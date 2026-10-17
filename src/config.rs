//! Process configuration. Only what is needed to reach Turso and to protect the
//! UI comes from the environment; everything else (LLM provider, Telegram,
//! agents, skills, MCP servers) lives in the database and is edited in the UI.
//! Environment values act as defaults until a setting is saved.

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
    #[arg(long, env = "TURSO_DATABASE_URL")]
    pub turso_url: String,

    #[arg(long, env = "TURSO_AUTH_TOKEN", hide_env_values = true)]
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

    /// Default LLM provider: "openai" (any OpenAI-compatible endpoint) or "anthropic".
    #[arg(long, env = "LLM_PROVIDER", default_value = "openai")]
    pub llm_provider: String,

    #[arg(long, env = "LLM_BASE_URL")]
    pub llm_base_url: Option<String>,

    #[arg(long, env = "LLM_API_KEY", hide_env_values = true)]
    pub llm_api_key: Option<String>,

    #[arg(long, env = "LLM_MODEL")]
    pub llm_model: Option<String>,

    #[arg(long, env = "TELEGRAM_BOT_TOKEN", hide_env_values = true)]
    pub telegram_token: Option<String>,

    /// Comma-separated Telegram user ids allowed to talk to the bot. Empty = nobody.
    #[arg(long, env = "TELEGRAM_ALLOWED_USERS", default_value = "")]
    pub telegram_allowed_users: String,

    /// Tokio worker threads. 1 is right for a 1 GB board.
    #[arg(long, env = "KONGMING_WORKERS", default_value_t = 1)]
    pub workers: usize,

    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    /// Hard cap on agent loop iterations per turn.
    #[arg(long, env = "KONGMING_MAX_ITERATIONS", default_value_t = 24)]
    pub max_iterations: usize,

    /// Approximate context budget in tokens before old turns are compacted.
    #[arg(long, env = "KONGMING_CONTEXT_TOKENS", default_value_t = 48_000)]
    pub context_tokens: usize,
}

impl Config {
    pub fn telegram_allowed(&self) -> Vec<i64> {
        self.telegram_allowed_users.split(',').filter_map(|s| s.trim().parse().ok()).collect()
    }

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
        let c = Config::from_args(["kongming", "--turso-url", "libsql://x.turso.io", "--turso-token", "t", "--telegram-allowed-users", "1, 22,x"]).unwrap();
        assert_eq!(c.port, 18800);
        assert_eq!(c.telegram_allowed(), vec![1, 22]);
        assert_eq!(c.workers, 1);
    }
}
