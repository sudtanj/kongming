//! Process configuration. Only what is needed to reach Turso and to protect the
//! UI comes from the environment; everything else (LLM provider, Telegram,
//! agents, skills, MCP servers) lives in the database and is edited in the UI.

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "kongming", version, about = "kongming: small agentic AI daemon, all state on Turso")]
pub struct Config {
    /// Address to listen on.
    #[arg(long, env = "KONGMING_HOST", default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long, env = "PORT", default_value_t = 18800)]
    pub port: u16,

    /// Turso database URL (libsql://... or https://...).
    #[arg(long, env = "KONGMING_TURSO_DATABASE_URL")]
    pub turso_url: String,

    #[arg(long, env = "KONGMING_TURSO_AUTH_TOKEN", hide_env_values = true)]
    pub turso_token: String,

    /// Password for the web UI. Empty disables authentication (loopback / tailnet only!).
    #[arg(long, env = "KONGMING_PASSWORD", hide_env_values = true, default_value = "")]
    pub password: String,

    /// Secret for signing session cookies. Random per boot when unset (users must log in again after a restart).
    #[arg(long, env = "KONGMING_SECRET", hide_env_values = true)]
    pub secret: Option<String>,

    /// Public base URL (for MCP OAuth redirects), e.g. https://kongming.tailnet.ts.net
    #[arg(long, env = "KONGMING_PUBLIC_URL")]
    pub public_url: Option<String>,

    /// Telegram Bot API base URL (a self-hosted Bot API server, or a mock in tests).
    #[arg(long, env = "KONGMING_TELEGRAM_API", default_value = "https://api.telegram.org", hide = true)]
    pub telegram_api: String,

    /// Tokio worker threads. 1 is right for a 1 GB board.
    #[arg(long, env = "KONGMING_WORKERS", default_value_t = 1)]
    pub workers: usize,

    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    /// Agent loop iterations (model rounds) per step block of a turn.
    #[arg(long, env = "KONGMING_MAX_ITERATIONS", default_value_t = 24)]
    pub max_iterations: usize,

    /// Step blocks a turn carries on into by itself when it hits
    /// `max_iterations` (0: stop and wait for "continue").
    #[arg(long, env = "KONGMING_AUTO_CONTINUE", default_value_t = 4)]
    pub auto_continue: usize,

    /// Approximate context budget in tokens before old turns are compacted.
    #[arg(long, env = "KONGMING_CONTEXT_TOKENS", default_value_t = 48_000)]
    pub context_tokens: usize,
}

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
    }
}
