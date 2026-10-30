//! The `shell` tool: one POSIX shell command at a time in a small, read-only
//! sandbox. It is for interacting (hosts, URLs and services on the network,
//! commands such as `cdp`, quick text processing), not for keeping files.
//!
//! The Docker image ships a static BusyBox (`/opt/busybox`), so the agent gets
//! `sh` plus the usual applets without a distro; `cdp`, `curl` and `wget` are
//! links to the kongming binary (`curl`/`wget` in `fetch.rs`: rustls, output
//! always to stdout). Isolation comes from the container (read-only root, no
//! capabilities, pid and memory caps) plus what is set here: an empty
//! environment (no secrets), `/` as the working directory, a process group
//! killed on timeout, and no file writes: a
//! Landlock ruleset (when the kernel has it) refuses creating, changing or
//! deleting anything except writing to /dev/null, and a zero file-size limit
//! keeps any file that can still be created empty. The kernel's OOM killer
//! picks the command before the daemon. `main` marks the daemon non-dumpable
//! so a command cannot read its environment or memory through /proc.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
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

/// Runs before every command: stderr joins stdout, no input.
const PRELUDE: &str = "exec 2>&1 </dev/null\n";

#[derive(Debug, Clone)]
pub struct Shell {
    pub sh: PathBuf,
    path: String,
    /// Landlock ruleset denying file writes; None when the kernel lacks it.
    landlock: Option<Arc<OwnedFd>>,
}

impl Shell {
    /// `shell`: path to a POSIX sh, "off" to disable, empty for the bundled
    /// BusyBox when it exists.
    pub fn from_config(shell: &str) -> Option<Shell> {
        let sh = match shell.trim() {
            "off" | "none" | "false" | "0" => return None,
            "" if std::path::Path::new(BUSYBOX_SH).exists() => PathBuf::from(BUSYBOX_SH),
            "" => return None,
            p => PathBuf::from(p),
        };
        let bin = sh.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let path = format!("{bin}:/usr/local/bin:/usr/bin:/bin");
        let landlock = landlock::deny_writes().ok().map(Arc::new);
        Some(Shell { sh, path, landlock })
    }

    /// How file writes are refused, for the startup log.
    pub fn confinement(&self) -> &'static str {
        if self.landlock.is_some() {
            "landlock: no file writes"
        } else {
            "no landlock in this kernel: zero file-size limit only"
        }
    }

    /// Run `command`; returns the output with its exit status and whether it failed.
    pub async fn run(&self, command: &str, timeout_secs: u64) -> Result<(String, bool)> {
        let secs = timeout_secs.clamp(1, MAX_TIMEOUT);
        let mut cmd = Command::new(&self.sh);
        cmd.arg("-c")
            .arg(format!("{PRELUDE}{command}"))
            .current_dir("/")
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", "/")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let ruleset = self.landlock.as_ref().map(|fd| fd.as_raw_fd());
        // SAFETY: `confine` only makes async-signal-safe syscalls.
        unsafe { cmd.pre_exec(move || confine(ruleset)) };
        let mut child = cmd.spawn().with_context(|| format!("starting {}", self.sh.display()))?;
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

/// In the child between fork and exec. The OOM score goes first, while
/// /proc can still be written.
fn confine(ruleset: Option<RawFd>) -> std::io::Result<()> {
    // SAFETY: plain syscalls on constant or stack arguments.
    unsafe {
        let fd = libc::open(c"/proc/self/oom_score_adj".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd >= 0 {
            libc::write(fd, b"1000".as_ptr().cast(), 4);
            libc::close(fd);
        }
        let zero = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_FSIZE, &zero);
        libc::setrlimit(libc::RLIMIT_CORE, &zero);
        // A write past the limit fails with "File too large" instead of killing the command.
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        if let Some(fd) = ruleset {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 || landlock::restrict_self(fd) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Just enough Landlock (Linux 5.13+, unprivileged) to refuse every file
/// write; reading and running programs stay allowed. Raw syscalls: their
/// numbers are the same on aarch64 and x86_64.
mod landlock {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    const CREATE_RULESET: libc::c_long = 444;
    const ADD_RULE: libc::c_long = 445;
    const RESTRICT_SELF: libc::c_long = 446;
    const CREATE_RULESET_VERSION: libc::c_uint = 1;
    const RULE_PATH_BENEATH: libc::c_uint = 1;

    const WRITE_FILE: u64 = 1 << 1;
    /// REMOVE_DIR, REMOVE_FILE and every MAKE_* (char, dir, reg, sock, fifo, block, sym).
    const REMOVE_AND_MAKE: u64 = 0x1ff0;
    const REFER: u64 = 1 << 13; // ABI 2
    const TRUNCATE: u64 = 1 << 14; // ABI 3

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneath {
        allowed_access: u64,
        parent_fd: i32,
    }

    pub fn deny_writes() -> std::io::Result<OwnedFd> {
        let err = std::io::Error::last_os_error;
        // SAFETY: the Landlock syscalls with valid pointers and sizes; the
        // returned descriptor is owned (and close-on-exec) from here on.
        unsafe {
            let abi = libc::syscall(CREATE_RULESET, std::ptr::null::<RulesetAttr>(), 0usize, CREATE_RULESET_VERSION);
            if abi < 1 {
                return Err(err());
            }
            let mut handled = WRITE_FILE | REMOVE_AND_MAKE;
            let mut null_access = WRITE_FILE;
            if abi >= 2 {
                handled |= REFER;
            }
            if abi >= 3 {
                handled |= TRUNCATE;
                null_access |= TRUNCATE;
            }
            let attr = RulesetAttr { handled_access_fs: handled };
            let fd =
                libc::syscall(CREATE_RULESET, &attr as *const RulesetAttr, std::mem::size_of::<RulesetAttr>(), 0u32);
            if fd < 0 {
                return Err(err());
            }
            let ruleset = OwnedFd::from_raw_fd(fd as RawFd);
            // `2>/dev/null` keeps working.
            let null = std::fs::File::open("/dev/null")?;
            let rule = PathBeneath { allowed_access: null_access, parent_fd: null.as_raw_fd() };
            if libc::syscall(ADD_RULE, ruleset.as_raw_fd(), RULE_PATH_BENEATH, &rule as *const PathBeneath, 0u32) != 0 {
                return Err(err());
            }
            Ok(ruleset)
        }
    }

    /// In the child, after PR_SET_NO_NEW_PRIVS.
    pub fn restrict_self(ruleset: RawFd) -> libc::c_long {
        // SAFETY: plain syscall; a bad descriptor just fails.
        unsafe { libc::syscall(RESTRICT_SELF, ruleset, 0u32) }
    }
}

/// Keep the start and the end of long output; the end usually holds the error.
pub(crate) fn head_tail(s: &str, max: usize) -> String {
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
        Shell::from_config("/bin/sh").unwrap()
    }

    #[test]
    fn config_picks_the_shell() {
        assert!(Shell::from_config("off").is_none());
        assert!(Shell::from_config(" none ").is_none());
        assert!(sh().path.starts_with("/bin:"));
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
    async fn runs_without_secrets_in_the_root() {
        let (out, failed) = sh()
            .run(
                "echo hi; echo oops >&2; pwd; echo \"[$KONGMING_TURSO_AUTH_TOKEN]\"; echo x >/dev/null && echo null-ok",
                10,
            )
            .await
            .unwrap();
        assert!(!failed, "{out}");
        assert!(out.starts_with("exit 0\nhi\noops\n/\n[]\nnull-ok"), "{out}");
        let (out, failed) = sh().run("echo out | tr a-z A-Z; exit 3", 10).await.unwrap();
        assert!(failed);
        assert!(out.starts_with("exit 3\nOUT"), "{out}");
    }

    #[tokio::test]
    async fn nothing_is_stored_on_disk() {
        let f = std::env::temp_dir().join(format!("kongming-shell-test-{}", uuid::Uuid::new_v4()));
        let s = sh();
        let (out, _) =
            s.run(&format!("echo stored > {0}; echo rc=$?; cat {0}; echo end", f.display()), 10).await.unwrap();
        let kept = std::fs::read_to_string(&f).unwrap_or_default();
        let _ = std::fs::remove_file(&f);
        assert!(!out.contains("rc=0"), "{} {out}", s.confinement());
        assert!(!out.contains("stored\n"), "{out}");
        assert!(kept.is_empty(), "{kept}");
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
