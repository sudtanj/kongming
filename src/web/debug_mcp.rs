//! MCP server at `POST /mcp` so a developer's agent can inspect this daemon:
//! status, settings with secrets redacted, recent logs and chats. Only
//! `send_message` changes anything, and only when the `debug_mcp_chat` setting is
//! on (off by default, switched in the dashboard). Authenticated by its own
//! bearer token (only the SHA-256 is stored, in the `debug_mcp_token_hash`
//! setting); without a token the endpoint does not exist. JSON responses only, no SSE.

use anyhow::{anyhow, bail, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::Level;

use crate::db::now_ms;
use crate::mcp::truncate;
use crate::state::Shared;

pub const HASH_KEY: &str = "debug_mcp_token_hash";
pub const CHAT_KEY: &str = "debug_mcp_chat";
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_TEXT: usize = 60_000;
const LOG_BUDGET: usize = 50_000;
const SECRET_WORDS: [&str; 9] = ["key", "token", "secret", "password", "vault", "auth", "cookie", "hash", "credential"];

pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn bearer_ok(headers: &HeaderMap, want_hash: &str) -> bool {
    let got = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    got.is_some_and(|t| hash_token(t).as_bytes().ct_eq(want_hash.as_bytes()).unwrap_u8() == 1)
}

fn rpc_error(id: Value, code: i32, message: &str) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})).into_response()
}

pub async fn handle(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let want = match state.db.setting(HASH_KEY).await {
        Ok(Some(Value::String(h))) => h,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!("debug mcp: reading the token failed: {e:#}");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if !bearer_ok(&headers, &want) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "parse error");
    };
    if !msg.is_object() {
        return rpc_error(Value::Null, -32600, "invalid request (batches are not supported)");
    }
    // Notifications and client responses carry no id and get no reply.
    let Some(id) = msg.get("id").cloned() else { return StatusCode::ACCEPTED.into_response() };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_list()})),
        "tools/call" => call_tool(&state, &params).await,
        m => Err((-32601, format!("method not found: {m}"))),
    };
    match result {
        Ok(r) => Json(json!({"jsonrpc": "2.0", "id": id, "result": r})).into_response(),
        Err((code, m)) => rpc_error(id, code, &m),
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "kongming", "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tool_list() -> Value {
    json!([
        {
            "name": "get_status",
            "description": "Version, uptime, memory, model and provider (no keys), Telegram, MCP servers and their status, cron and earn state.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "get_config",
            "description": "Every saved setting and MCP server with secrets redacted.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "get_logs",
            "description": "Recent log lines from the in-memory buffer (since boot, newest last). level=warn shows warnings and errors.",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "description": "Newest N lines, default 200, max 2000"},
                "level": {"type": "string", "enum": ["error", "warn", "info", "debug"], "description": "Least severe level to include"},
                "contains": {"type": "string", "description": "Case-insensitive text to look for"},
                "since_seconds": {"type": "number", "description": "Only lines from the last N seconds"},
            }},
        },
        {
            "name": "get_sessions",
            "description": "Without session_id: recent chats (id, title, channel). With it: that chat's newest messages including tool calls and results, fields named like secrets redacted.",
            "inputSchema": {"type": "object", "properties": {
                "session_id": {"type": "string"},
                "limit": {"type": "integer", "description": "Chats to list (default 20) or newest messages to show (default 30), max 100"},
            }},
        },
        {
            "name": "send_message",
            "description": "Send a message to Kongming as the user, in a chat on the \"debug\" channel (a new one unless session_id names an earlier debug chat). Returns the session id at once; the reply runs in the background, read it with get_sessions. Works only when the dashboard's \"Chat from debug MCP\" switch is on.",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"},
                "session_id": {"type": "string", "description": "A debug chat from an earlier send_message"},
            }, "required": ["text"]},
        },
    ])
}

pub async fn chat_enabled(state: &Shared) -> Result<bool> {
    Ok(state.db.setting(CHAT_KEY).await? == Some(Value::Bool(true)))
}

/// Starts a turn in a "debug" chat and returns its id. That channel gets no `setup` tool and cannot approve
/// earn leads, and the user's own chats are never written to.
async fn send_message(state: &Shared, args: &Value) -> Result<String> {
    if !chat_enabled(state).await? {
        bail!("chat is off: turn on \"Chat from debug MCP\" under Settings > Models in the dashboard");
    }
    let text = args.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
    let text = text.ok_or_else(|| anyhow!("text is required"))?.to_string();
    let session = match args.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(id) => {
            let s = state.db.session(id).await?.ok_or_else(|| anyhow!("no session {id}"))?;
            if s.channel != "debug" {
                bail!("session {id} is not a debug chat");
            }
            s
        }
        None => {
            let agent = state.db.default_agent().await?;
            state.db.create_session(&agent.id, "debug", None, None, "Debug MCP").await?
        }
    };
    if state.is_running(&session.id) {
        bail!("a reply is already in progress in {}", session.id);
    }
    let id = session.id.clone();
    tracing::info!(session = %id, "debug mcp message");
    let st = state.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::agent::run_turn(st, session, text, crate::agent::TurnOptions::default()).await {
            tracing::warn!("debug mcp turn failed: {e:#}");
        }
    });
    Ok(format!("started in session {id}; read the reply with get_sessions session_id={id}"))
}

async fn sessions(state: &Shared, args: &Value) -> Result<String> {
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(0).clamp(0, 100);
    let Some(id) = args.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        let list = state.db.sessions(if limit == 0 { 20 } else { limit }).await?;
        let out: Vec<Value> = list
            .iter()
            .map(|s| json!({"id": s.id, "title": s.title, "channel": s.channel, "updatedAt": s.updated_at}))
            .collect();
        return Ok(serde_json::to_string_pretty(&out)?);
    };
    let session = state.db.session(id).await?.ok_or_else(|| anyhow!("no session {id}"))?;
    let mut out = format!("# {} ({})\n", session.title, session.channel);
    for m in state.db.recent_messages(id, if limit == 0 { 30 } else { limit }).await? {
        let parts = redact(&serde_json::to_value(&m.parts)?).to_string();
        out.push_str(&format!("[{} {} {}] {}\n", m.seq, m.created_at, m.role, truncate(&parts, 3000)));
    }
    Ok(out)
}

async fn call_tool(state: &Shared, params: &Value) -> Result<Value, (i32, String)> {
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let out = match params.get("name").and_then(Value::as_str) {
        Some("get_status") => status(state).await,
        Some("get_config") => config(state).await,
        Some("get_logs") => logs(&args),
        Some("get_sessions") => sessions(state, &args).await,
        Some("send_message") => send_message(state, &args).await,
        other => return Err((-32602, format!("unknown tool: {}", other.unwrap_or("")))),
    };
    Ok(match out {
        Ok(text) => json!({"content": [{"type": "text", "text": truncate(&text, MAX_TEXT)}]}),
        Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
    })
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// Resident set size in kB (Linux only).
fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?;
    line.split_whitespace().next()?.parse().ok()
}

async fn status(state: &Shared) -> Result<String> {
    let llm = state.provider_config().await?;
    let profile = crate::llm::profile::profile(&llm);
    let servers: Vec<Value> = state
        .db
        .mcp_servers()
        .await?
        .iter()
        .map(|s| {
            json!({
                "name": s.name, "host": host_of(&s.url), "transport": s.transport, "authType": s.auth_type,
                "enabled": s.enabled, "status": s.status, "lastError": s.last_error.as_deref().map(crate::logbuf::scrub),
                "tools": s.tools.as_array().map_or(0, Vec::len),
            })
        })
        .collect();
    let cron = state.db.cron_jobs().await?;
    let earn = state.db.setting("earn").await?;
    let telegram_enabled = state.telegram_token().await.is_some();
    let telegram_status = state.telegram_status.lock().clone();
    let out = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": (now_ms() - state.started_at) / 1000,
        "rssKb": rss_kb(),
        "listen": format!("{}:{}", state.cfg.host, state.cfg.port),
        "publicUrl": state.public_url().await,
        "turso": state.db.turso.host(),
        "llm": {
            "kind": llm.kind, "model": llm.model, "baseHost": llm.base_url.as_deref().and_then(host_of),
            "apiKeySet": llm.api_key.as_deref().is_some_and(|k| !k.is_empty()),
            "smallModel": profile.small, "contextWindow": profile.context_window,
        },
        "telegram": {"enabled": telegram_enabled, "status": telegram_status},
        "mcpServers": servers,
        "cron": {"jobs": cron.len(), "enabled": cron.iter().filter(|c| c.enabled).count()},
        "earnScanEnabled": earn.and_then(|e| e.get("enabled").and_then(Value::as_bool)).unwrap_or(false),
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|w| key.contains(w))
}

/// Copy of `v` where the values under secret-looking keys, at any depth, are
/// replaced by their length or `[set]`. Booleans and numbers stay visible.
pub fn redact(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let shown = match x {
                        Value::String(s) if is_secret_key(k) => {
                            json!(format!("[redacted, {} chars]", s.chars().count()))
                        }
                        Value::Array(_) | Value::Object(_) if is_secret_key(k) => json!("[set]"),
                        _ => redact(x),
                    };
                    (k.clone(), shown)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        Value::String(s) => json!(crate::logbuf::scrub(s)),
        _ => v.clone(),
    }
}

async fn config(state: &Shared) -> Result<String> {
    let settings: Map<String, Value> = state.db.settings().await?.into_iter().collect();
    let servers: Vec<Value> = state
        .db
        .mcp_servers()
        .await?
        .iter()
        .map(|s| {
            // The token and OAuth state are never serialized. Header values and the URL path
            // can hold credentials whatever they are called: show names and the host only.
            let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
            v["url"] = json!(host_of(&s.url));
            if let Some(h) = v.get_mut("headers").and_then(Value::as_object_mut) {
                h.values_mut().for_each(|x| *x = json!("[set]"));
            }
            v["tokenSet"] = json!(s.token.as_deref().is_some_and(|t| !t.is_empty()));
            v
        })
        .collect();
    let out = redact(&json!({"settings": settings, "mcpServers": servers}));
    Ok(serde_json::to_string_pretty(&out)?)
}

fn logs(args: &Value) -> Result<String> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(200).clamp(1, 2000) as usize;
    let level = match args.get("level").and_then(Value::as_str) {
        Some(l) => l.parse::<Level>().map_err(|_| anyhow!("level must be error, warn, info or debug"))?,
        None => Level::TRACE,
    };
    let contains = args.get("contains").and_then(Value::as_str).filter(|c| !c.is_empty());
    let since_ms = args
        .get("since_seconds")
        .and_then(Value::as_f64)
        .map_or(0, |s| now_ms().saturating_sub((s.max(0.0) * 1000.0) as i64));
    let entries = crate::logbuf::buffer().query(level, contains, since_ms, limit);
    // Keep the newest lines when the budget runs out.
    let mut used = 0;
    let mut lines: Vec<String> = Vec::new();
    for e in entries.iter().rev() {
        let line = e.line();
        used += line.len() + 1;
        if used > LOG_BUDGET {
            break;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return Ok("no matching log lines".into());
    }
    lines.reverse();
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_keys_at_any_depth() {
        let v = json!({
            "llm": {"apiKey": "sk-123456", "model": "m", "apiKeySet": true},
            "mcpServers": [{"headers": {"Authorization": "Bearer abc", "X-Other": "ok"}}],
            "vault": {"a": {"value": "x"}},
            "auth_epoch": 3,
            "note": "plain",
        });
        let r = redact(&v);
        assert_eq!(r["llm"]["apiKey"], json!("[redacted, 9 chars]"));
        assert_eq!(r["llm"]["model"], json!("m"));
        assert_eq!(r["llm"]["apiKeySet"], json!(true));
        assert_eq!(r["mcpServers"][0]["headers"]["Authorization"], json!("[redacted, 10 chars]"));
        assert_eq!(r["mcpServers"][0]["headers"]["X-Other"], json!("ok"));
        assert_eq!(r["vault"], json!("[set]"));
        assert_eq!(r["auth_epoch"], json!(3));
        assert_eq!(r["note"], json!("plain"));
        assert!(!r.to_string().contains("sk-123456"));
    }
}
