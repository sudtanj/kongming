//! The `shell` tool: one POSIX shell command at a time in a small sandbox.
//!
//! The Docker image ships a static BusyBox (`/opt/busybox`), so the agent gets
//! `sh` plus the usual applets without a distro. Isolation comes from the
//! container (read-only root, no capabilities, pid and memory caps) plus what
//! is set here: an empty environment (no secrets), a scratch working
//! directory (a tmpfs in the container), a process group killed on timeout,
//! ulimits, and an OOM score that makes the kernel kill the command before the
//! daemon. `main` marks the daemon non-dumpable so a command cannot read its
//! environment or memory through /proc.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Where the Docker image puts BusyBox and its applet links.
pub const BUSYBOX_SH: &str = "/opt/busybox/bin/sh";
pub const DEFAULT_TIMEOUT: u64 = 30;
pub const MAX_TIMEOUT: u64 = 300;
const OUTPUT_LIMIT: usize = 12_000;
const READ_LIMIT: usize = 1 << 20;

/// Runs before every command: stderr joins stdout, the kernel's OOM killer
/// picks the command first, and limits that cannot be raised again (no
/// capabilities): 64 MiB files, no core dumps. Memory is capped by the
/// container; the OOM score keeps the daemon alive when a command hits it.
const PRELUDE: &str = "exec 2>&1 </dev/null
echo 1000 >/proc/self/oom_score_adj 2>/dev/null
ulimit -f 131072 2>/dev/null
ulimit -c 0 2>/dev/null
";

#[derive(Debug, Clone)]
pub struct Shell {
    pub sh: PathBuf,
    pub workdir: PathBuf,
    path: String,
}

impl Shell {
    /// `shell`: path to a POSIX sh, "off" to disable, empty for the bundled
    /// BusyBox when it exists. `workdir`: empty for `<tmp>/kongming-work`.
    pub fn from_config(shell: &str, workdir: &str) -> Option<Shell> {
        let sh = match shell.trim() {
            "off" | "none" | "false" | "0" => return None,
            "" if std::path::Path::new(BUSYBOX_SH).exists() => PathBuf::from(BUSYBOX_SH),
            "" => return None,
            p => PathBuf::from(p),
        };
        let bin = sh.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let path = format!("{bin}:/usr/local/bin:/usr/bin:/bin");
        let workdir = match workdir.trim() {
            "" => std::env::temp_dir().join("kongming-work"),
            w => PathBuf::from(w),
        };
        Some(Shell { sh, workdir, path })
    }

    /// Run `command`; returns the output with its exit status and whether it failed.
    pub async fn run(&self, command: &str, timeout_secs: u64) -> Result<(String, bool)> {
        tokio::fs::create_dir_all(&self.workdir).await.context("creating the shell working directory")?;
        let secs = timeout_secs.clamp(1, MAX_TIMEOUT);
        let mut child = Command::new(&self.sh)
            .arg("-c")
            .arg(format!("{PRELUDE}{command}"))
            .current_dir(&self.workdir)
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", &self.workdir)
            .env("TMPDIR", &self.workdir)
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", self.sh.display()))?;
        let pgid = child.id().map(|p| p as i32);
        let mut out = child.stdout.take().context("shell stdout")?;

        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut eof = false;
        let mut status = None;
        let mut lingering = false;
        let mut timed_out = false;
        let deadline = tokio::time::sleep(Duration::from_secs(secs));
        tokio::pin!(deadline);
        while !(eof && status.is_some()) {
            tokio::select! {
                r = out.read(&mut chunk), if !eof => match r {
                    Ok(0) | Err(_) => eof = true,
                    // Past the limit keep draining, so the command never blocks on a full pipe.
                    Ok(n) => if buf.len() < READ_LIMIT { buf.extend_from_slice(&chunk[..n]) },
                },
                s = child.wait(), if status.is_none() => {
                    status = Some(s?);
                    // Something started in the background still holds the output: give it a moment.
                    if !eof {
                        lingering = true;
                        deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(1));
                    }
                },
                _ = &mut deadline => {
                    timed_out = !lingering;
                    break;
                },
            }
        }
        // Whatever is left in the group (background jobs, a timed-out command) goes.
        if let Some(pgid) = pgid {
            // SAFETY: plain syscall; a group that is already gone just returns ESRCH.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        if status.is_none() {
            status = child.wait().await.ok();
        }

        let text = String::from_utf8_lossy(&buf);
        let text = head_tail(text.trim_end(), OUTPUT_LIMIT);
        let (head, failed) = if timed_out {
            (format!("timed out after {secs}s; the command was killed"), true)
        } else {
            match status.and_then(|s| s.code()) {
                Some(0) => ("exit 0".to_string(), false),
                Some(c) => (format!("exit {c}"), true),
                None => ("killed by a signal (out of memory or a limit?)".to_string(), true),
            }
        };
        let body = if text.is_empty() { "(no output)".to_string() } else { text };
        Ok((format!("{head}\n{body}"), failed))
    }
}

/// Keep the start and the end of long output; the end usually holds the error.
fn head_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max / 3).collect();
    let tail: String = s.chars().skip(n - (max - max / 3)).collect();
    format!("{head}\n…[{} characters cut]…\n{tail}", n - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh() -> Shell {
        let dir = std::env::temp_dir().join(format!("kongming-shell-test-{}", uuid::Uuid::new_v4()));
        Shell::from_config("/bin/sh", &dir.display().to_string()).unwrap()
    }

    #[test]
    fn config_picks_the_shell() {
        assert!(Shell::from_config("off", "").is_none());
        let s = Shell::from_config("/bin/sh", "/work").unwrap();
        assert_eq!(s.workdir, PathBuf::from("/work"));
        assert!(s.path.starts_with("/bin:"));
    }

    #[test]
    fn long_output_keeps_both_ends() {
        let s: String = (0..5000).map(|i| format!("{i}\n")).collect();
        let out = head_tail(&s, 300);
        assert!(out.starts_with("0\n1\n"));
        assert!(out.trim_end().ends_with("4999"));
        assert!(out.contains("characters cut"));
    }

    #[tokio::test]
    async fn runs_in_a_clean_scratch_directory() {
        let s = sh();
        let (out, failed) = s.run("echo hi; echo oops >&2; pwd; echo \"[$KONGMING_TURSO_AUTH_TOKEN]\"", 10).await.unwrap();
        assert!(!failed, "{out}");
        assert!(out.starts_with("exit 0\nhi\noops\n"), "{out}");
        assert!(out.contains(&s.workdir.display().to_string()), "{out}");
        assert!(out.contains("[]"), "{out}");
        let (out, failed) = s.run("echo left > note.txt; cat note.txt; exit 3", 10).await.unwrap();
        assert!(failed);
        assert!(out.starts_with("exit 3\nleft"), "{out}");
    }

    #[tokio::test]
    async fn a_command_that_hangs_is_killed() {
        let (out, failed) = sh().run("echo started; sleep 30", 3).await.unwrap();
        assert!(failed);
        assert!(out.starts_with("timed out after 3s"), "{out}");
        assert!(out.contains("started"), "{out}");
        // A background job holding the output does not keep the call open.
        let started = std::time::Instant::now();
        let (out, failed) = sh().run("sleep 30 & echo done", 20).await.unwrap();
        assert!(!failed, "{out}");
        assert!(started.elapsed() < Duration::from_secs(10), "{out}");
    }
}
