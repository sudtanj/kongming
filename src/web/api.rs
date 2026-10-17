//! REST API. Every handler returns JSON; errors are `{ "error": "..." }` with
//! a 4xx/5xx status.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::{self, TurnOptions};
use crate::db::{new_id, now_ms, Agent, CronJob, McpServer, OAuthState};
use crate::llm::ProviderConfig;
use crate::state::Shared;

#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        let status = if msg.contains("not found") || msg.starts_with("no ") || msg.starts_with("unknown") {
            StatusCode::NOT_FOUND
        } else if msg.contains("already in progress") {
            StatusCode::CONFLICT
        } else if msg.contains("required") || msg.contains("invalid") || msg.contains("must") {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        ApiError(status, msg)
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}

type R = Result<Json<Value>, ApiError>;

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty())
}
fn strings(v: &Value, k: &str) -> Option<Vec<String>> {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect())
}

// ------------------------------------------------------------------------ auth

pub async fn auth_status(State(state): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let authed = state.cfg.password.is_empty()
        || headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|c| {
                c.split(';')
                    .map(str::trim)
                    .find_map(|p| p.strip_prefix(&format!("{}=", super::auth::COOKIE)).map(str::to_string))
            })
            .is_some_and(|t| super::auth::verify(&state.secret, &t));
    Json(json!({"authRequired": !state.cfg.password.is_empty(), "authenticated": authed}))
}

pub async fn login(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let pw = body.get("password").and_then(Value::as_str).unwrap_or("");
    if !super::auth::password_ok(&state, pw) {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        return Err(ApiError(StatusCode::UNAUTHORIZED, "wrong password".into()));
    }
    let token = super::auth::issue(&state.secret);
    let secure = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    Ok(([(header::SET_COOKIE, super::auth::cookie_header(&token, secure))], Json(json!({"ok": true, "token": token})))
        .into_response())
}

pub async fn logout() -> Response {
    ([(header::SET_COOKIE, super::auth::clear_cookie_header())], Json(json!({"ok": true}))).into_response()
}

// ------------------------------------------------------------------------ info

pub async fn info(State(state): State<Shared>) -> R {
    let pcfg = state.provider_config().await?;
    let agents = state.db.agents().await?;
    let day = now_ms() - 24 * 3600 * 1000;
    let usage = state.db.usage_since(day).await.unwrap_or_default();
    let turso_ms = state.db.turso.ping().await.ok();
    Ok(Json(json!({
        "name": "kongming",
        "version": env!("CARGO_PKG_VERSION"),
        "startedAt": state.started_at,
        "uptimeMs": now_ms() - state.started_at,
        "turso": {"host": state.db.turso.host(), "pingMs": turso_ms},
        "llm": pcfg.redacted(),
        "telegram": {"enabled": state.telegram_token().await.is_some()},
        "agents": agents.len(),
        "usage24h": usage,
        "authRequired": !state.cfg.password.is_empty(),
        "publicUrl": state.public_url().await,
    })))
}

// -------------------------------------------------------------------- sessions

#[derive(Deserialize)]
pub struct Limit {
    pub limit: Option<i64>,
}

pub async fn list_sessions(State(state): State<Shared>, Query(q): Query<Limit>) -> R {
    let list = state.db.sessions(q.limit.unwrap_or(100).clamp(1, 500)).await?;
    let agents = state.db.agents().await?;
    let out: Vec<Value> = list
        .into_iter()
        .map(|sess| {
            let mut v = serde_json::to_value(&sess).unwrap_or(Value::Null);
            v["agentHandle"] = json!(agents.iter().find(|a| a.id == sess.agent_id).map(|a| a.handle.clone()));
            v["running"] = json!(state.is_running(&sess.id));
            v
        })
        .collect();
    Ok(Json(json!(out)))
}

pub async fn create_session(State(state): State<Shared>, body: Option<Json<Value>>) -> R {
    let body = body.map(|b| b.0).unwrap_or(json!({}));
    let agent = match s(&body, "agentId") {
        Some(id) => state
            .db
            .agent(id)
            .await?
            .or(state.db.agent_by_handle(id).await?)
            .ok_or_else(|| not_found("no such agent"))?,
        None => agent::ensure_default_agent(&state).await?,
    };
    let sess = state.db.create_session(&agent.id, "web", None, None, s(&body, "title").unwrap_or("")).await?;
    Ok(Json(serde_json::to_value(sess).unwrap()))
}

pub async fn get_session(State(state): State<Shared>, Path(id): Path<String>) -> R {
    let sess = state.db.session(&id).await?.ok_or_else(|| not_found("session not found"))?;
    let msgs = state.db.messages(&id, 0).await?;
    let agent = state.db.agent(&sess.agent_id).await?;
    Ok(Json(json!({"session": sess, "messages": msgs, "agent": agent, "running": state.is_running(&id)})))
}

pub async fn patch_session(State(state): State<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> R {
    let mut sess = state.db.session(&id).await?.ok_or_else(|| not_found("session not found"))?;
    if let Some(t) = s(&body, "title") {
        state.db.update_session_title(&id, t).await?;
        sess.title = t.to_string();
    }
    if let Some(a) = s(&body, "agentId") {
        let agent = state
            .db
            .agent(a)
            .await?
            .or(state.db.agent_by_handle(a).await?)
            .ok_or_else(|| not_found("no such agent"))?;
        state.db.set_session_agent(&id, &agent.id).await?;
        sess.agent_id = agent.id;
    }
    state.emit(crate::state::Event::SessionUpdated { session: sess.clone() });
    Ok(Json(serde_json::to_value(sess).unwrap()))
}

pub async fn delete_session(State(state): State<Shared>, Path(id): Path<String>) -> R {
    state.cancel_turn(&id);
    state.db.delete_session(&id).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn clear_session(State(state): State<Shared>, Path(id): Path<String>) -> R {
    state.db.clear_session(&id).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn stop_session(State(state): State<Shared>, Path(id): Path<String>) -> R {
    Ok(Json(json!({"stopped": state.cancel_turn(&id)})))
}

/// Send a message over HTTP (Telegram-less clients, scripts, tests). The reply
/// streams on the WebSocket; with `wait=true` the final text is returned.
#[derive(Deserialize)]
pub struct SendQuery {
    pub wait: Option<bool>,
}

pub async fn send_message(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<SendQuery>,
    Json(body): Json<Value>,
) -> R {
    let text = s(&body, "text").ok_or_else(|| bad("text is required"))?.to_string();
    let sess = state.db.session(&id).await?.ok_or_else(|| not_found("session not found"))?;
    if state.is_running(&id) {
        return Err(ApiError(StatusCode::CONFLICT, "a reply is already in progress".into()));
    }
    if q.wait.unwrap_or(false) {
        let r = agent::run_turn(state.clone(), sess, text, TurnOptions::default()).await?;
        return Ok(Json(json!({"text": r.text, "stopReason": r.stop_reason, "error": r.error, "agent": r.agent})));
    }
    let st = state.clone();
    tokio::spawn(async move {
        if let Err(e) = agent::run_turn(st, sess, text, TurnOptions::default()).await {
            tracing::warn!("turn failed: {e:#}");
        }
    });
    Ok(Json(json!({"accepted": true})))
}

// ---------------------------------------------------------------------- agents

pub async fn list_agents(State(state): State<Shared>) -> R {
    agent::ensure_default_agent(&state).await?;
    Ok(Json(json!(state.db.agents().await?)))
}

fn normalize_handle(h: &str) -> Result<String, ApiError> {
    let h: String = h
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    if h.is_empty() || h.len() > 32 {
        return Err(bad("handle must be 1-32 letters, digits, '-' or '_'"));
    }
    if matches!(h.as_str(), "skill" | "session" | "mcp" | "all" | "everyone") {
        return Err(bad("that handle is reserved"));
    }
    Ok(h)
}

async fn agent_from_body(state: &Shared, existing: Option<Agent>, body: &Value) -> Result<Agent, ApiError> {
    let now = now_ms();
    let mut a = existing.unwrap_or(Agent {
        id: new_id(),
        handle: String::new(),
        name: String::new(),
        role: String::new(),
        description: String::new(),
        system_prompt: String::new(),
        model: None,
        provider: None,
        manager_id: None,
        tools: None,
        mcp_servers: None,
        pinned_skills: vec![],
        self_improve: true,
        is_default: false,
        created_at: now,
        updated_at: now,
    });
    if let Some(h) = s(body, "handle") {
        a.handle = normalize_handle(h)?;
    }
    if a.handle.is_empty() {
        return Err(bad("handle is required"));
    }
    if let Some(n) = s(body, "name") {
        a.name = n.to_string();
    }
    if a.name.is_empty() {
        a.name = a.handle.clone();
    }
    if let Some(v) = body.get("role").and_then(Value::as_str) {
        a.role = v.trim().to_string();
    }
    if let Some(v) = body.get("description").and_then(Value::as_str) {
        a.description = v.trim().to_string();
    }
    if let Some(v) = body.get("systemPrompt").and_then(Value::as_str) {
        a.system_prompt = v.to_string();
    }
    if body.get("model").is_some() {
        a.model = s(body, "model").map(str::to_string);
    }
    if body.get("managerId").is_some() {
        a.manager_id = match s(body, "managerId") {
            Some(m) => {
                let mgr = state
                    .db
                    .agent(m)
                    .await?
                    .or(state.db.agent_by_handle(m).await?)
                    .ok_or_else(|| bad("manager not found"))?;
                if mgr.id == a.id {
                    return Err(bad("an agent cannot manage itself"));
                }
                // No cycles: walk up from the manager.
                let team = state.db.agents().await?;
                let mut cur = mgr.manager_id.clone();
                let mut hops = 0;
                while let Some(c) = cur {
                    if c == a.id {
                        return Err(bad("that would create a management cycle"));
                    }
                    cur = team.iter().find(|t| t.id == c).and_then(|t| t.manager_id.clone());
                    hops += 1;
                    if hops > 32 {
                        break;
                    }
                }
                Some(mgr.id)
            }
            None => None,
        };
    }
    if body.get("tools").is_some() {
        a.tools = strings(body, "tools");
    }
    if body.get("mcpServers").is_some() {
        a.mcp_servers = strings(body, "mcpServers");
    }
    if body.get("pinnedSkills").is_some() {
        a.pinned_skills = strings(body, "pinnedSkills").unwrap_or_default();
    }
    if let Some(b) = body.get("selfImprove").and_then(Value::as_bool) {
        a.self_improve = b;
    }
    if let Some(b) = body.get("isDefault").and_then(Value::as_bool) {
        a.is_default = b;
    }
    a.updated_at = now;
    Ok(a)
}

pub async fn create_agent(State(state): State<Shared>, Json(body): Json<Value>) -> R {
    let a = agent_from_body(&state, None, &body).await?;
    if state.db.agent_by_handle(&a.handle).await?.is_some() {
        return Err(bad("handle already in use"));
    }
    state.db.upsert_agent(&a).await?;
    Ok(Json(json!(a)))
}

pub async fn get_agent(State(state): State<Shared>, Path(id): Path<String>) -> R {
    let a = state
        .db
        .agent(&id)
        .await?
        .or(state.db.agent_by_handle(&id).await?)
        .ok_or_else(|| not_found("agent not found"))?;
    Ok(Json(json!(a)))
}

pub async fn update_agent(State(state): State<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> R {
    let existing = state.db.agent(&id).await?.ok_or_else(|| not_found("agent not found"))?;
    let a = agent_from_body(&state, Some(existing), &body).await?;
    if let Some(other) = state.db.agent_by_handle(&a.handle).await? {
        if other.id != a.id {
            return Err(bad("handle already in use"));
        }
    }
    state.db.upsert_agent(&a).await?;
    Ok(Json(json!(a)))
}

pub async fn delete_agent(State(state): State<Shared>, Path(id): Path<String>) -> R {
    let agents = state.db.agents().await?;
    if agents.len() <= 1 {
        return Err(bad("cannot delete the last agent"));
    }
    let a = agents.iter().find(|a| a.id == id).ok_or_else(|| not_found("agent not found"))?.clone();
    state.db.delete_agent(&id).await?;
    if a.is_default {
        if let Some(mut next) = state.db.agents().await?.into_iter().next() {
            next.is_default = true;
            state.db.upsert_agent(&next).await?;
        }
    }
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------- skills

pub async fn list_skills(State(state): State<Shared>) -> R {
    let skills = state.db.skills().await?;
    let out: Vec<Value> = skills
        .iter()
        .map(|k| {
            json!({"id": k.id, "name": k.name, "description": k.description, "tags": k.tags, "version": k.version, "source": k.source,
                   "authorAgentId": k.author_agent_id, "enabled": k.enabled, "uses": k.uses, "lastUsedAt": k.last_used_at,
                   "createdAt": k.created_at, "updatedAt": k.updated_at, "bodyChars": k.body.chars().count()})
        })
        .collect();
    Ok(Json(json!(out)))
}

pub async fn get_skill(State(state): State<Shared>, Path(name): Path<String>) -> R {
    let k = state.db.skill_by_name(&name).await?.ok_or_else(|| not_found("skill not found"))?;
    let versions = state.db.skill_versions(&name).await?;
    let mut v = json!(k);
    v["markdown"] = json!(k.to_markdown());
    v["versions"] = json!(versions);
    Ok(Json(v))
}

pub async fn put_skill(State(state): State<Shared>, Path(name): Path<String>, Json(body): Json<Value>) -> R {
    let existing = state.db.skill_by_name(&name).await?;
    if let Some(enabled) = body.get("enabled").and_then(Value::as_bool) {
        if body.get("body").is_none() && body.get("description").is_none() {
            let k = existing.ok_or_else(|| not_found("skill not found"))?;
            state.db.set_skill_enabled(&k.name, enabled).await?;
            return Ok(Json(json!(state.db.skill_by_name(&k.name).await?)));
        }
    }
    let description = s(&body, "description")
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|e| e.description.clone()))
        .ok_or_else(|| bad("description is required"))?;
    let skill_body = body
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| existing.as_ref().map(|e| e.body.clone()))
        .ok_or_else(|| bad("body is required"))?;
    let tags = strings(&body, "tags").or_else(|| existing.as_ref().map(|e| e.tags.clone())).unwrap_or_default();
    let saved = state.db.save_skill(&name, &description, &skill_body, &tags, "user", None).await?;
    if let Some(enabled) = body.get("enabled").and_then(Value::as_bool) {
        state.db.set_skill_enabled(&saved.name, enabled).await?;
    }
    Ok(Json(json!(state.db.skill_by_name(&saved.name).await?)))
}

pub async fn delete_skill(State(state): State<Shared>, Path(name): Path<String>) -> R {
    state.db.delete_skill(&name).await?;
    Ok(Json(json!({"ok": true})))
}

/// Import a SKILL.md document (frontmatter `name`, `description`, `tags`).
pub async fn import_skill(State(state): State<Shared>, Json(body): Json<Value>) -> R {
    let md = s(&body, "markdown").ok_or_else(|| bad("markdown is required"))?;
    let (name, description, tags, skill_body) =
        parse_skill_markdown(md).ok_or_else(|| bad("expected YAML frontmatter with name and description"))?;
    let saved = state.db.save_skill(&name, &description, &skill_body, &tags, "user", None).await?;
    Ok(Json(json!(saved)))
}

pub fn parse_skill_markdown(md: &str) -> Option<(String, String, Vec<String>, String)> {
    let md = md.trim_start_matches('\u{feff}').trim_start();
    let rest = md.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('\r').trim_start_matches('\n').to_string();
    let mut name = None;
    let mut description = None;
    let mut tags = Vec::new();
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        match k.trim() {
            "name" => name = Some(v.to_string()),
            "description" => description = Some(v.to_string()),
            "tags" => {
                tags = v
                    .trim_matches(['[', ']'])
                    .split(',')
                    .map(|t| t.trim().trim_matches('"').to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            }
            _ => {}
        }
    }
    Some((name?, description?, tags, body))
}

// -------------------------------------------------------------------- memories

pub async fn list_memories(State(state): State<Shared>, Query(q): Query<Limit>) -> R {
    Ok(Json(json!(state.db.memories(None, q.limit.unwrap_or(200).clamp(1, 1000)).await?)))
}

pub async fn create_memory(State(state): State<Shared>, Json(body): Json<Value>) -> R {
    let content = s(&body, "content").ok_or_else(|| bad("content is required"))?;
    let m = state
        .db
        .save_memory(
            s(&body, "agentId"),
            s(&body, "kind").unwrap_or("fact"),
            content,
            &strings(&body, "keywords").unwrap_or_default(),
            body.get("importance").and_then(Value::as_i64).unwrap_or(2),
        )
        .await?;
    Ok(Json(json!(m)))
}

pub async fn delete_memory(State(state): State<Shared>, Path(id): Path<String>) -> R {
    state.db.delete_memory(&id).await?;
    Ok(Json(json!({"ok": true})))
}

// ------------------------------------------------------------------------- mcp

pub async fn list_mcp(State(state): State<Shared>) -> R {
    Ok(Json(json!(state.db.mcp_servers().await?.iter().map(crate::mcp::server_public).collect::<Vec<_>>())))
}

async fn mcp_from_body(existing: Option<McpServer>, body: &Value) -> Result<McpServer, ApiError> {
    let now = now_ms();
    let mut m = existing.unwrap_or(McpServer {
        id: new_id(),
        name: String::new(),
        url: String::new(),
        transport: "http".into(),
        auth_type: "none".into(),
        token: None,
        oauth: OAuthState::default(),
        headers: json!({}),
        enabled: true,
        status: "unknown".into(),
        last_error: None,
        tools: json!([]),
        created_at: now,
        updated_at: now,
    });
    if let Some(n) = s(body, "name") {
        m.name = n.to_string();
    }
    if let Some(u) = s(body, "url") {
        let parsed = url::Url::parse(u).map_err(|_| bad("invalid url"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(bad("only http(s) MCP servers are supported; local stdio servers are not allowed"));
        }
        m.url = u.to_string();
    }
    if m.name.is_empty() || m.url.is_empty() {
        return Err(bad("name and url are required"));
    }
    if let Some(t) = s(body, "transport") {
        if !matches!(t, "http" | "sse") {
            return Err(bad("transport must be http or sse"));
        }
        m.transport = t.to_string();
    }
    if let Some(a) = s(body, "authType") {
        if !matches!(a, "none" | "token" | "oauth") {
            return Err(bad("authType must be none, token or oauth"));
        }
        m.auth_type = a.to_string();
    }
    if let Some(t) = body.get("token").and_then(Value::as_str) {
        if !t.is_empty() {
            m.token = Some(t.to_string());
        }
    }
    if body.get("clearToken").and_then(Value::as_bool) == Some(true) {
        m.token = None;
    }
    if body.get("oauthClientId").is_some() {
        m.oauth.client_id = s(body, "oauthClientId").map(str::to_string);
    }
    if let Some(sec) = s(body, "oauthClientSecret") {
        m.oauth.client_secret = Some(sec.to_string());
    }
    if body.get("oauthScope").is_some() {
        m.oauth.scope = s(body, "oauthScope").map(str::to_string);
    }
    if let Some(h) = body.get("headers") {
        if h.is_object() {
            m.headers = h.clone();
        }
    }
    if let Some(e) = body.get("enabled").and_then(Value::as_bool) {
        m.enabled = e;
    }
    m.updated_at = now;
    Ok(m)
}

pub async fn create_mcp(State(state): State<Shared>, Json(body): Json<Value>) -> R {
    let m = mcp_from_body(None, &body).await?;
    if state.db.mcp_servers().await?.iter().any(|x| x.name == m.name) {
        return Err(bad("an MCP server with that name exists"));
    }
    state.db.upsert_mcp_server(&m).await?;
    let st = state.clone();
    let id = m.id.clone();
    tokio::spawn(async move {
        let _ = st.mcp.refresh(&id).await;
    });
    Ok(Json(crate::mcp::server_public(&m)))
}

pub async fn update_mcp(State(state): State<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> R {
    let existing = state.db.mcp_server(&id).await?.ok_or_else(|| not_found("server not found"))?;
    let m = mcp_from_body(Some(existing), &body).await?;
    state.db.upsert_mcp_server(&m).await?;
    state.mcp.forget(&id);
    let st = state.clone();
    tokio::spawn(async move {
        let _ = st.mcp.refresh(&id).await;
    });
    Ok(Json(crate::mcp::server_public(&m)))
}

pub async fn delete_mcp(State(state): State<Shared>, Path(id): Path<String>) -> R {
    state.mcp.forget(&id);
    state.db.delete_mcp_server(&id).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn refresh_mcp(State(state): State<Shared>, Path(id): Path<String>) -> R {
    let res = state.mcp.refresh(&id).await;
    let m = state.db.mcp_server(&id).await?.ok_or_else(|| not_found("server not found"))?;
    let mut v = crate::mcp::server_public(&m);
    if let Err(e) = res {
        v["error"] = json!(format!("{e:#}"));
    }
    Ok(Json(v))
}

pub async fn call_mcp_tool(State(state): State<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> R {
    let tool = s(&body, "tool").ok_or_else(|| bad("tool is required"))?;
    let args = body.get("args").cloned().unwrap_or(json!({}));
    let (text, is_error) = state.mcp.call(&id, tool, args).await?;
    Ok(Json(json!({"content": text, "isError": is_error})))
}

async fn redirect_uri(state: &Shared, headers: &HeaderMap) -> String {
    let base = match state.public_url().await {
        Some(u) => u,
        None => {
            let proto = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
            let host = headers
                .get("x-forwarded-host")
                .or(headers.get(header::HOST))
                .and_then(|v| v.to_str().ok())
                .unwrap_or("localhost");
            format!("{proto}://{host}")
        }
    };
    format!("{base}/api/mcp/oauth/callback")
}

pub async fn mcp_oauth_start(State(state): State<Shared>, Path(id): Path<String>, headers: HeaderMap) -> R {
    let redirect = redirect_uri(&state, &headers).await;
    let url = state.mcp.start_oauth(&id, &redirect).await?;
    Ok(Json(json!({"url": url, "redirectUri": redirect})))
}

#[derive(Deserialize)]
pub struct OAuthCallback {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Public (unauthenticated) endpoint: the browser lands here from the
/// authorization server. The `state` parameter is the only credential.
pub async fn mcp_oauth_callback(State(state): State<Shared>, Query(q): Query<OAuthCallback>) -> Response {
    if let Some(err) = q.error {
        let msg = q.error_description.unwrap_or(err);
        return Redirect::to(&format!("/#/settings/mcp?oauth=error&message={}", urlenc(&msg))).into_response();
    }
    match (q.code, q.state) {
        (Some(code), Some(st)) => match state.mcp.finish_oauth(&st, &code).await {
            Ok(id) => Redirect::to(&format!("/#/settings/mcp?oauth=ok&server={id}")).into_response(),
            Err(e) => Redirect::to(&format!("/#/settings/mcp?oauth=error&message={}", urlenc(&format!("{e:#}"))))
                .into_response(),
        },
        _ => (StatusCode::BAD_REQUEST, "missing code or state").into_response(),
    }
}

pub async fn mcp_oauth_disconnect(State(state): State<Shared>, Path(id): Path<String>) -> R {
    state.mcp.clear_oauth(&id).await?;
    state.mcp.forget(&id);
    Ok(Json(json!({"ok": true})))
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// -------------------------------------------------------------------- settings

pub async fn get_settings(State(state): State<Shared>) -> R {
    let all = state.db.settings().await?;
    let llm = state.provider_config().await?;
    let mut telegram = all.get("telegram").cloned().unwrap_or(json!({}));
    let has_token = state.telegram_token().await.is_some();
    if let Some(o) = telegram.as_object_mut() {
        o.remove("token");
        o.insert("tokenSet".into(), json!(has_token));
        o.insert("allowedUsers".into(), json!(state.telegram_allowed().await));
    }
    Ok(Json(json!({
        "llm": llm.redacted(),
        "telegram": telegram,
        "reflection": all.get("reflection").cloned().unwrap_or(json!({"enabled": true})),
        "publicUrl": state.public_url().await,
        "ui": all.get("ui").cloned().unwrap_or(json!({})),
        "env": {
            "llmFromEnv": state.cfg.llm_api_key.is_some(),
            "telegramFromEnv": state.cfg.telegram_token.is_some(),
        }
    })))
}

pub async fn put_settings(State(state): State<Shared>, Json(body): Json<Value>) -> R {
    if let Some(llm) = body.get("llm") {
        let mut cfg: ProviderConfig = state.provider_config().await.unwrap_or_default();
        if let Some(k) = s(llm, "kind") {
            cfg.kind = k.to_string();
        }
        if llm.get("baseUrl").is_some() {
            cfg.base_url = s(llm, "baseUrl").map(str::to_string);
        }
        if let Some(k) = s(llm, "apiKey") {
            cfg.api_key = Some(k.to_string());
        }
        if llm.get("clearApiKey").and_then(Value::as_bool) == Some(true) {
            cfg.api_key = None;
        }
        if let Some(m) = s(llm, "model") {
            cfg.model = m.to_string();
        }
        if llm.get("maxTokens").is_some() {
            cfg.max_tokens = llm.get("maxTokens").and_then(Value::as_u64).map(|v| v as u32);
        }
        if llm.get("temperature").is_some() {
            cfg.temperature = llm.get("temperature").and_then(Value::as_f64).map(|v| v as f32);
        }
        if llm.get("effort").is_some() {
            cfg.effort = s(llm, "effort").map(str::to_string);
        }
        if !matches!(cfg.kind.as_str(), "openai" | "anthropic") {
            return Err(bad("llm.kind must be openai or anthropic"));
        }
        state.db.set_setting("llm", &serde_json::to_value(&cfg).unwrap()).await?;
    }
    if let Some(tg) = body.get("telegram") {
        let mut cur = state.db.setting("telegram").await?.unwrap_or(json!({}));
        if !cur.is_object() {
            cur = json!({});
        }
        if let Some(t) = s(tg, "token") {
            cur["token"] = json!(t);
        }
        if tg.get("clearToken").and_then(Value::as_bool) == Some(true) {
            cur["token"] = json!("");
        }
        if let Some(a) = tg.get("allowedUsers") {
            let ids: Vec<i64> = match a {
                Value::Array(arr) => arr
                    .iter()
                    .filter_map(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.trim().parse().ok())))
                    .collect(),
                Value::String(st) => st.split([',', ' ', '\n']).filter_map(|x| x.trim().parse().ok()).collect(),
                _ => vec![],
            };
            cur["allowedUsers"] = json!(ids);
        }
        if let Some(e) = tg.get("enabled").and_then(Value::as_bool) {
            cur["enabled"] = json!(e);
        }
        state.db.set_setting("telegram", &cur).await?;
    }
    if let Some(r) = body.get("reflection") {
        state.db.set_setting("reflection", r).await?;
    }
    if let Some(p) = body.get("publicUrl") {
        state.db.set_setting("publicUrl", &json!(p.as_str().unwrap_or("").trim().trim_end_matches('/'))).await?;
    }
    if let Some(u) = body.get("ui") {
        state.db.set_setting("ui", u).await?;
    }
    get_settings(State(state)).await
}

pub async fn test_llm(State(state): State<Shared>) -> R {
    let (cfg, provider) = state.provider().await?;
    let req = crate::llm::ChatRequest {
        model: cfg.model.clone(),
        system: "Reply with the single word: pong".into(),
        messages: vec![crate::llm::ChatMessage::text("user", "ping")],
        tools: vec![],
        max_tokens: 20,
        temperature: Some(0.0),
        effort: Some("low".into()),
    };
    let started = std::time::Instant::now();
    let text = agent::collect_text(provider.stream(req).await?).await?;
    Ok(Json(json!({"ok": true, "reply": text.trim(), "ms": started.elapsed().as_millis() as u64, "model": cfg.model})))
}

// ------------------------------------------------------------------------ cron

pub async fn list_cron(State(state): State<Shared>) -> R {
    Ok(Json(json!(state.db.cron_jobs().await?)))
}

async fn cron_from_body(state: &Shared, existing: Option<CronJob>, body: &Value) -> Result<CronJob, ApiError> {
    let mut c = existing.unwrap_or(CronJob {
        id: new_id(),
        name: String::new(),
        agent_id: String::new(),
        schedule: String::new(),
        prompt: String::new(),
        enabled: true,
        deliver_to: None,
        last_run_at: None,
        next_run_at: None,
        last_status: None,
        created_at: now_ms(),
    });
    if let Some(n) = s(body, "name") {
        c.name = n.to_string();
    }
    if let Some(a) = s(body, "agentId") {
        c.agent_id =
            state.db.agent(a).await?.or(state.db.agent_by_handle(a).await?).ok_or_else(|| bad("agent not found"))?.id;
    }
    if c.agent_id.is_empty() {
        c.agent_id = agent::ensure_default_agent(state).await?.id;
    }
    if let Some(sch) = s(body, "schedule") {
        crate::cron::next_run(sch, now_ms())
            .ok_or_else(|| bad("schedule must be 'every:<minutes>' or a 5-field cron expression"))?;
        c.schedule = sch.to_string();
    }
    if let Some(p) = s(body, "prompt") {
        c.prompt = p.to_string();
    }
    if c.name.is_empty() || c.schedule.is_empty() || c.prompt.is_empty() {
        return Err(bad("name, schedule and prompt are required"));
    }
    if body.get("deliverTo").is_some() {
        c.deliver_to = s(body, "deliverTo").map(str::to_string);
    }
    if let Some(e) = body.get("enabled").and_then(Value::as_bool) {
        c.enabled = e;
    }
    c.next_run_at = if c.enabled { crate::cron::next_run(&c.schedule, now_ms()) } else { None };
    Ok(c)
}

pub async fn create_cron(State(state): State<Shared>, Json(body): Json<Value>) -> R {
    let c = cron_from_body(&state, None, &body).await?;
    state.db.upsert_cron_job(&c).await?;
    Ok(Json(json!(c)))
}

pub async fn update_cron(State(state): State<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> R {
    let existing =
        state.db.cron_jobs().await?.into_iter().find(|c| c.id == id).ok_or_else(|| not_found("job not found"))?;
    let c = cron_from_body(&state, Some(existing), &body).await?;
    state.db.upsert_cron_job(&c).await?;
    Ok(Json(json!(c)))
}

pub async fn delete_cron(State(state): State<Shared>, Path(id): Path<String>) -> R {
    state.db.delete_cron_job(&id).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn run_cron(State(state): State<Shared>, Path(id): Path<String>) -> R {
    let job = state.db.cron_jobs().await?.into_iter().find(|c| c.id == id).ok_or_else(|| not_found("job not found"))?;
    let st = state.clone();
    tokio::spawn(async move {
        crate::cron::run_job(st, job).await;
    });
    Ok(Json(json!({"started": true})))
}

// ------------------------------------------------------------- misc listings

pub async fn list_tasks(State(state): State<Shared>, Query(q): Query<Limit>) -> R {
    Ok(Json(json!(state.db.tasks(q.limit.unwrap_or(100).clamp(1, 500)).await?)))
}

pub async fn list_improvements(State(state): State<Shared>, Query(q): Query<Limit>) -> R {
    Ok(Json(json!(state.db.improvements(q.limit.unwrap_or(100).clamp(1, 500)).await?)))
}

pub async fn usage(State(state): State<Shared>) -> R {
    let now = now_ms();
    Ok(Json(json!({
        "day": state.db.usage_since(now - 86_400_000).await?,
        "week": state.db.usage_since(now - 7 * 86_400_000).await?,
        "month": state.db.usage_since(now - 30 * 86_400_000).await?,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_skill_markdown() {
        let md = "---\nname: Deploy App\ndescription: \"How to deploy\"\ntags: [ops, deploy]\n---\n\n# Steps\n1. go";
        let (n, d, t, b) = parse_skill_markdown(md).unwrap();
        assert_eq!(n, "Deploy App");
        assert_eq!(d, "How to deploy");
        assert_eq!(t, vec!["ops", "deploy"]);
        assert_eq!(b, "# Steps\n1. go");
        assert!(parse_skill_markdown("no frontmatter").is_none());
    }

    #[test]
    fn handles_are_normalized() {
        assert_eq!(normalize_handle("@Dev Ops").unwrap(), "dev-ops");
        assert!(normalize_handle("skill").is_err());
        assert!(normalize_handle("").is_err());
    }
}
