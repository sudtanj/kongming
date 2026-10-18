//! Process-wide shared state and the event bus that fans chat events out to
//! the web socket clients, Telegram and tests.

use std::sync::Arc;

use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::Config;
use crate::db::{Agent, Db, Message, Session};
use crate::llm::{self, Provider, ProviderConfig};
use crate::mcp::McpManager;

/// Everything a UI needs to render a live turn. `session_id` lets one bus
/// serve every session; subscribers filter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Event {
    UserMessage {
        session_id: String,
        message: Message,
    },
    TurnStart {
        session_id: String,
        turn_id: String,
        agent: Agent,
    },
    Delta {
        session_id: String,
        turn_id: String,
        text: String,
    },
    ReasoningDelta {
        session_id: String,
        turn_id: String,
        text: String,
    },
    ToolCall {
        session_id: String,
        turn_id: String,
        id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        session_id: String,
        turn_id: String,
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    /// A persisted assistant message (one per LLM round, so tool-call rounds
    /// also appear in history).
    Message {
        session_id: String,
        message: Message,
    },
    Status {
        session_id: String,
        turn_id: String,
        text: String,
    },
    TurnEnd {
        session_id: String,
        turn_id: String,
        stop_reason: String,
        error: Option<String>,
    },
    SessionUpdated {
        session: Session,
    },
    Improvement {
        kind: String,
        note: String,
        agent_id: Option<String>,
    },
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::UserMessage { session_id, .. }
            | Event::TurnStart { session_id, .. }
            | Event::Delta { session_id, .. }
            | Event::ReasoningDelta { session_id, .. }
            | Event::ToolCall { session_id, .. }
            | Event::ToolResult { session_id, .. }
            | Event::Message { session_id, .. }
            | Event::Status { session_id, .. }
            | Event::TurnEnd { session_id, .. } => Some(session_id),
            Event::SessionUpdated { session } => Some(&session.id),
            Event::Improvement { .. } => None,
        }
    }
}

pub struct AppState {
    pub cfg: Config,
    pub db: Db,
    pub http: reqwest::Client,
    pub bus: broadcast::Sender<Event>,
    pub mcp: McpManager,
    pub secret: Vec<u8>,
    pub started_at: i64,
    /// Wakes the Telegram loop when its settings change.
    pub telegram_reload: tokio::sync::Notify,
    /// Outcome of the last Telegram setup (bot, mode, error), shown in the dashboard.
    pub telegram_status: Mutex<Value>,
    /// Sessions with a turn in flight, with a cancel flag each.
    running: Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    provider_cache: RwLock<Option<(ProviderConfig, Arc<dyn Provider>)>>,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(cfg: Config, db: Db) -> Shared {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(2)
            .use_rustls_tls()
            .user_agent(concat!("kongming/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        let (bus, _) = broadcast::channel(256);
        let secret = match &cfg.secret {
            Some(s) if !s.is_empty() => s.as_bytes().to_vec(),
            _ => crate::mcp::oauth::random_token(32).into_bytes(),
        };
        Arc::new(AppState {
            mcp: McpManager::new(db.clone(), http.clone()),
            cfg,
            db,
            http,
            bus,
            secret,
            started_at: crate::db::now_ms(),
            running: Mutex::new(Default::default()),
            provider_cache: RwLock::new(None),
            telegram_reload: tokio::sync::Notify::new(),
            telegram_status: Mutex::new(Value::Null),
        })
    }

    pub fn emit(&self, ev: Event) {
        let _ = self.bus.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.subscribe()
    }

    /// Mark a session busy; returns `None` when a turn is already running.
    pub fn begin_turn(&self, session_id: &str) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        let mut r = self.running.lock();
        if r.contains_key(session_id) {
            return None;
        }
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        r.insert(session_id.to_string(), flag.clone());
        Some(flag)
    }

    pub fn end_turn(&self, session_id: &str) {
        self.running.lock().remove(session_id);
    }

    pub fn cancel_turn(&self, session_id: &str) -> bool {
        match self.running.lock().get(session_id) {
            Some(flag) => {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.running.lock().contains_key(session_id)
    }

    /// LLM configuration as saved from the dashboard. Nothing saved yet means
    /// OpenAI with no key, and `provider()` asks the user to open Settings.
    pub async fn provider_config(&self) -> Result<ProviderConfig> {
        let mut c = match self.db.setting("llm").await? {
            Some(v) => serde_json::from_value::<ProviderConfig>(v).unwrap_or_default(),
            None => ProviderConfig::default(),
        };
        if c.kind.is_empty() {
            c.kind = "openai".into();
        }
        if c.model.is_empty() {
            c.model = default_model(&c.kind);
        }
        Ok(c)
    }

    pub async fn provider(&self) -> Result<(ProviderConfig, Arc<dyn Provider>)> {
        let cfg = self.provider_config().await?;
        if cfg.api_key.as_deref().is_none_or(str::is_empty) && cfg.base_url.as_deref().is_none_or(str::is_empty) {
            anyhow::bail!("no LLM is set up yet: add a provider and API key in Settings → Models");
        }
        if let Some((c, p)) = self.provider_cache.read().as_ref() {
            if *c == cfg {
                return Ok((cfg, p.clone()));
            }
        }
        let p: Arc<dyn Provider> = Arc::from(llm::build(&cfg, self.http.clone())?);
        *self.provider_cache.write() = Some((cfg.clone(), p.clone()));
        Ok((cfg, p))
    }

    /// Telegram bot token saved from the dashboard.
    pub async fn telegram_token(&self) -> Option<String> {
        let v = self.db.setting("telegram").await.ok().flatten()?;
        v.get("token").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string)
    }

    pub async fn telegram_allowed(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = Vec::new();
        if let Ok(Some(v)) = self.db.setting("telegram").await {
            if let Some(list) = v.get("allowedUsers").and_then(Value::as_array) {
                ids.extend(
                    list.iter().filter_map(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))),
                );
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub async fn public_url(&self) -> Option<String> {
        if let Ok(Some(v)) = self.db.setting("publicUrl").await {
            if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                return Some(s.trim_end_matches('/').to_string());
            }
        }
        self.cfg.public_url.clone().map(|s| s.trim_end_matches('/').to_string())
    }
}

pub fn default_model(kind: &str) -> String {
    match kind {
        "anthropic" => "claude-opus-5-5".into(),
        _ => "gpt-4o-mini".into(),
    }
}
