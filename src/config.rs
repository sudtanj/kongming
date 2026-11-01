//! Process configuration. Only what is needed to reach Turso and to protect the
//! UI comes from the environment; everything else (LLM provider, Telegram,
//! agents, skills, MCP servers) lives in the database and is edited in the UI.

#[cfg(feature = "native")]
use clap::Parser;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "native", derive(Parser))]
#[cfg_attr(feature = "native", command(name = "kongming", version, about = "kongming: small agentic AI daemon, all state on Turso"))]
pub struct Config {
    /// Address to listen on.
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_HOST", default_value = "0.0.0.0"))]
    pub host: String,

    #[cfg_attr(feature = "native", arg(long, env = "PORT", default_value_t = 18800))]
    pub port: u16,

    /// Turso database URL (libsql://... or https://...).
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_TURSO_DATABASE_URL"))]
    pub turso_url: String,

    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_TURSO_AUTH_TOKEN", hide_env_values = true))]
    pub turso_token: String,

    /// Password for the web UI. Empty disables authentication (loopback / tailnet only!).
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_PASSWORD", hide_env_values = true, default_value = ""))]
    pub password: String,

    /// Secret for signing session cookies. Random per boot when unset (users must log in again after a restart).
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_SECRET", hide_env_values = true))]
    pub secret: Option<String>,

    /// Public base URL (for MCP OAuth redirects), e.g. https://kongming.tailnet.ts.net
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_PUBLIC_URL"))]
    pub public_url: Option<String>,

    /// Telegram Bot API base URL (a self-hosted Bot API server, or a mock in tests).
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_TELEGRAM_API", default_value = "https://api.telegram.org", hide = true))]
    pub telegram_api: String,

    /// Tokio worker threads. 1 is right for a 1 GB board.
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_WORKERS", default_value_t = 1))]
    pub workers: usize,

    #[cfg_attr(feature = "native", arg(long, env = "LOG_LEVEL", default_value = "info"))]
    pub log_level: String,

    /// Agent loop iterations (model rounds) per step block of a turn.
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_MAX_ITERATIONS", default_value_t = 24))]
    pub max_iterations: usize,

    /// Step blocks a turn carries on into by itself when it hits
    /// `max_iterations` (0: stop and wait for "continue").
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_AUTO_CONTINUE", default_value_t = 4))]
    pub auto_continue: usize,

    /// Approximate request size in tokens (system prompt, tools, history)
    /// past which older messages are compacted into the session summary, also
    /// between the tool rounds of a long turn.
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_CONTEXT_TOKENS", default_value_t = 48_000))]
    pub context_tokens: usize,

    /// POSIX shell behind the `shell` tool: a path, or "off". Empty uses the
    /// BusyBox bundled in the Docker image (/opt/busybox/bin/sh) when present.
    #[cfg_attr(feature = "native", arg(long, env = "KONGMING_SHELL", default_value = ""))]
    pub shell: String,
}

/// The same defaults as the command line, for hosts without one (a Worker
/// fills in what it needs, e.g. `password` and `secret`).
impl Default for Config {
    fn default() -> Self {
        Config {
            host: "0.0.0.0".into(),
            port: 18800,
            turso_url: String::new(),
            turso_token: String::new(),
            password: String::new(),
            secret: None,
            public_url: None,
            telegram_api: "https://api.telegram.org".into(),
            workers: 1,
            log_level: "info".into(),
            max_iterations: 24,
            auto_continue: 4,
            context_tokens: 48_000,
            shell: "off".into(),
        }
    }
}

#[cfg(feature = "native")]
impl Config {
    /// Parse from an explicit argument list (tests) with no environment lookup
    /// beyond what clap does by itself.
    pub fn from_args<I, T>(args: I) -> Result<Config, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Config::try_parse_from(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_args() {
        let c = Config::from_args(["kongming", "--turso-url", "libsql://x.turso.io", "--turso-token", "t"]).unwrap();
        assert_eq!(c.port, 18800);
        assert_eq!(c.telegram_api, "https://api.telegram.org");
        assert_eq!(c.workers, 1);
        let d = Config { turso_url: c.turso_url.clone(), turso_token: c.turso_token.clone(), shell: c.shell.clone(), ..Config::default() };
        assert_eq!(c, d, "Default must match the command-line defaults");
    }
}
