//! Remote MCP client: Streamable HTTP (2025-03-26+) and the legacy HTTP+SSE
//! transport, with bearer-token or OAuth authentication. Local (stdio) servers
//! are deliberately unsupported: nothing may run beside the daemon on the board.

pub mod oauth;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::db::{now_ms, Db, McpServer, OAuthState};
use crate::llm::sse::SseParser;
use crate::llm::ToolSpec;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RESULT_CHARS: usize = 24_000;

/// Legacy SSE transport connection: one long GET, a POST endpoint, replies
/// matched by JSON-RPC id.
struct SseConn {
    post_url: Mutex<String>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    alive: std::sync::atomic::AtomicBool,
}

pub struct McpManager {
    db: Db,
    http: reqwest::Client,
    next_id: AtomicI64,
    /// server id -> Mcp-Session-Id (streamable HTTP)
    sessions: Mutex<HashMap<String, String>>,
    sse: Mutex<HashMap<String, Arc<SseConn>>>,
    /// OAuth state param -> (server id, PKCE verifier, redirect uri)
    pending_oauth: Mutex<HashMap<String, (String, String, String)>>,
}

/// A tool exposed to the model, with where it lives.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub spec: ToolSpec,
    pub server_id: String,
    pub server_name: String,
    pub tool: String,
}

pub fn slug(name: &str) -> String {
    let s: String =
        name.trim().to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    s.trim_matches('_').to_string()
}

pub fn tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", slug(server), tool)
}

/// Split `mcp__server__tool` back into (server slug, tool).
pub fn parse_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    rest.split_once("__")
}

impl McpManager {
    pub fn new(db: Db, http: reqwest::Client) -> McpManager {
        McpManager {
            db,
            http,
            next_id: AtomicI64::new(1),
            sessions: Mutex::new(HashMap::new()),
            sse: Mutex::new(HashMap::new()),
            pending_oauth: Mutex::new(HashMap::new()),
        }
    }

    /// Tools of every enabled server with a cached tool list, optionally
    /// restricted to `allowed` server ids.
    pub async fn tools(&self, allowed: Option<&[String]>) -> Result<Vec<McpTool>> {
        let servers = self.db.mcp_servers().await?;
        let mut out = Vec::new();
        for s in servers.into_iter().filter(|s| s.enabled) {
            if let Some(a) = allowed {
                if !a.iter().any(|x| x == &s.id || x == &s.name) {
                    continue;
                }
            }
            if let Some(list) = s.tools.as_array() {
                for t in list {
                    let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
                    let desc = t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .take(1024)
                        .collect::<String>();
                    let mut schema =
                        t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                    if !schema.is_object() {
                        schema = json!({"type": "object", "properties": {}});
                    }
                    out.push(McpTool {
                        spec: ToolSpec {
                            name: tool_name(&s.name, name),
                            description: format!("[{}] {}", s.name, desc),
                            parameters: schema,
                        },
                        server_id: s.id.clone(),
                        server_name: s.name.clone(),
                        tool: name.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }

    pub async fn find_tool(&self, qualified: &str) -> Result<Option<McpTool>> {
        let Some((srv, tool)) = parse_tool_name(qualified) else { return Ok(None) };
        Ok(self.tools(None).await?.into_iter().find(|t| slug(&t.server_name) == srv && t.tool == tool))
    }

    fn rpc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Resolve the Authorization header, refreshing an OAuth token when needed.
    async fn auth_header(&self, s: &McpServer) -> Result<Option<String>> {
        match s.auth_type.as_str() {
            "token" => Ok(s.token.as_ref().filter(|t| !t.is_empty()).map(|t| format!("Bearer {t}"))),
            "oauth" => {
                let mut st = s.oauth.clone();
                if oauth::needs_refresh(&st) {
                    if st.refresh_token.is_some() {
                        oauth::refresh(&self.http, &mut st).await.context("refreshing OAuth token")?;
                        self.db.set_mcp_oauth(&s.id, &st).await?;
                    } else if st.access_token.is_none() {
                        bail!("not authorized: connect this server with OAuth first");
                    }
                }
                Ok(st.access_token.map(|t| format!("Bearer {t}")))
            }
            _ => Ok(None),
        }
    }

    fn apply_headers(
        &self,
        mut req: reqwest::RequestBuilder,
        s: &McpServer,
        auth: &Option<String>,
    ) -> reqwest::RequestBuilder {
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        if let Some(obj) = s.headers.as_object() {
            for (k, v) in obj {
                if let Some(val) = v.as_str() {
                    req = req.header(k.as_str(), val);
                }
            }
        }
        req.header("mcp-protocol-version", PROTOCOL_VERSION)
    }

    /// One JSON-RPC request over Streamable HTTP. Handles JSON and SSE replies
    /// and records the session id.
    async fn http_rpc(&self, s: &McpServer, auth: &Option<String>, body: Value, expect_reply: bool) -> Result<Value> {
        let id = body.get("id").cloned();
        let session = self.sessions.lock().get(&s.id).cloned();
        let mut req = self
            .http
            .post(&s.url)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(sid) = &session {
            req = req.header("mcp-session-id", sid);
        }
        let req = self.apply_headers(req, s, auth).json(&body);
        let resp = tokio::time::timeout(CALL_TIMEOUT, req.send())
            .await
            .map_err(|_| anyhow!("mcp request timed out"))?
            .context("mcp request failed")?;
        let status = resp.status();
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            self.sessions.lock().insert(s.id.clone(), sid.to_string());
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            bail!("unauthorized (HTTP {status})");
        }
        if status.as_u16() == 404 && session.is_some() {
            self.sessions.lock().remove(&s.id);
            bail!("session expired");
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("mcp HTTP {status}: {}", text.chars().take(300).collect::<String>());
        }
        if !expect_reply {
            return Ok(Value::Null);
        }
        let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if ct.starts_with("text/event-stream") {
            let mut parser = SseParser::new();
            let mut stream = resp.bytes_stream();
            let deadline = tokio::time::Instant::now() + CALL_TIMEOUT;
            loop {
                let chunk = tokio::time::timeout_at(deadline, stream.next())
                    .await
                    .map_err(|_| anyhow!("mcp stream timed out"))?;
                let Some(chunk) = chunk else { bail!("mcp stream ended without a reply") };
                let chunk = chunk.context("mcp stream read failed")?;
                for ev in parser.feed(&chunk) {
                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                        if v.get("id").is_some()
                            && v.get("id") == id.as_ref()
                            && (v.get("result").is_some() || v.get("error").is_some())
                        {
                            return Ok(v);
                        }
                    }
                }
            }
        }
        let text = resp.text().await.context("reading mcp reply")?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).context("mcp reply was not JSON")
    }

    /// Open (or reuse) a legacy SSE connection and return it.
    async fn sse_conn(&self, s: &McpServer, auth: &Option<String>) -> Result<Arc<SseConn>> {
        if let Some(c) = self.sse.lock().get(&s.id) {
            if c.alive.load(Ordering::Relaxed) {
                return Ok(c.clone());
            }
        }
        let req = self.apply_headers(self.http.get(&s.url).header("accept", "text/event-stream"), s, auth);
        let resp = tokio::time::timeout(Duration::from_secs(20), req.send())
            .await
            .map_err(|_| anyhow!("sse connect timed out"))?
            .context("sse connect failed")?;
        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            bail!("unauthorized (HTTP {status})");
        }
        if !status.is_success() {
            bail!("sse HTTP {status}");
        }
        let base = url::Url::parse(&s.url).context("invalid server url")?;
        let (tx, rx) = oneshot::channel::<String>();
        let conn = Arc::new(SseConn {
            post_url: Mutex::new(String::new()),
            pending: Mutex::new(HashMap::new()),
            alive: std::sync::atomic::AtomicBool::new(true),
        });
        // The POST endpoint arrives as the first event; the reader task owns the
        // stream and resolves replies by JSON-RPC id.
        let reader = conn.clone();
        tokio::spawn(async move {
            let mut parser = SseParser::new();
            let mut stream = resp.bytes_stream();
            let mut tx = Some(tx);
            while let Some(Ok(chunk)) = stream.next().await {
                for ev in parser.feed(&chunk) {
                    if ev.event == "endpoint" {
                        let url = base
                            .join(ev.data.trim())
                            .map(|u| u.to_string())
                            .unwrap_or_else(|_| ev.data.trim().to_string());
                        *reader.post_url.lock() = url.clone();
                        if let Some(t) = tx.take() {
                            let _ = t.send(url);
                        }
                    } else if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                        if let Some(id) = v.get("id").and_then(Value::as_i64) {
                            if let Some(w) = reader.pending.lock().remove(&id) {
                                let _ = w.send(v);
                            }
                        }
                    }
                }
            }
            reader.alive.store(false, Ordering::Relaxed);
            reader.pending.lock().clear();
        });
        tokio::time::timeout(Duration::from_secs(20), rx)
            .await
            .map_err(|_| anyhow!("sse: no endpoint event"))?
            .map_err(|_| anyhow!("sse: stream closed"))?;
        self.sse.lock().insert(s.id.clone(), conn.clone());
        Ok(conn)
    }

    async fn sse_rpc(&self, s: &McpServer, auth: &Option<String>, body: Value, expect_reply: bool) -> Result<Value> {
        let conn = self.sse_conn(s, auth).await?;
        let id = body.get("id").and_then(Value::as_i64);
        let rx = match (expect_reply, id) {
            (true, Some(id)) => {
                let (tx, rx) = oneshot::channel();
                conn.pending.lock().insert(id, tx);
                Some(rx)
            }
            _ => None,
        };
        let req = self
            .apply_headers(
                self.http.post(conn.post_url.lock().clone()).header("content-type", "application/json"),
                s,
                auth,
            )
            .json(&body);
        let resp = req.send().await.context("sse post failed")?;
        if !resp.status().is_success() {
            if let Some(id) = id {
                conn.pending.lock().remove(&id);
            }
            bail!("sse post HTTP {}", resp.status());
        }
        match rx {
            Some(rx) => tokio::time::timeout(CALL_TIMEOUT, rx)
                .await
                .map_err(|_| anyhow!("mcp call timed out"))?
                .map_err(|_| anyhow!("sse connection closed")),
            None => Ok(Value::Null),
        }
    }

    async fn rpc(&self, s: &McpServer, method: &str, params: Value) -> Result<Value> {
        let auth = self.auth_header(s).await?;
        let id = self.rpc_id();
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = if s.transport == "sse" {
            self.sse_rpc(s, &auth, body, true).await?
        } else {
            self.http_rpc(s, &auth, body, true).await?
        };
        if let Some(err) = reply.get("error") {
            bail!("{} failed: {}", method, err.get("message").and_then(Value::as_str).unwrap_or("unknown error"));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, s: &McpServer, method: &str) -> Result<()> {
        let auth = self.auth_header(s).await?;
        let body = json!({"jsonrpc": "2.0", "method": method});
        if s.transport == "sse" {
            self.sse_rpc(s, &auth, body, false).await?;
        } else {
            self.http_rpc(s, &auth, body, false).await?;
        }
        Ok(())
    }

    async fn initialize(&self, s: &McpServer) -> Result<Value> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "kongming", "version": env!("CARGO_PKG_VERSION")},
        });
        let info = self.rpc(s, "initialize", params).await?;
        let _ = self.notify(s, "notifications/initialized").await;
        Ok(info)
    }

    /// Connect, initialize and refresh the tool list; persists status.
    pub async fn refresh(&self, server_id: &str) -> Result<Vec<Value>> {
        let s = self.db.mcp_server(server_id).await?.ok_or_else(|| anyhow!("unknown MCP server"))?;
        if !s.enabled {
            self.db.set_mcp_status(server_id, "disabled", None, None).await?;
            return Ok(vec![]);
        }
        self.sessions.lock().remove(server_id);
        let res: Result<Vec<Value>> = async {
            self.initialize(&s).await?;
            let mut tools = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let params = match &cursor {
                    Some(c) => json!({"cursor": c}),
                    None => json!({}),
                };
                let r = self.rpc(&s, "tools/list", params).await?;
                if let Some(list) = r.get("tools").and_then(Value::as_array) {
                    tools.extend(list.iter().cloned());
                }
                cursor = r.get("nextCursor").and_then(Value::as_str).map(str::to_string);
                if cursor.is_none() || tools.len() > 500 {
                    break;
                }
            }
            Ok(tools)
        }
        .await;
        match res {
            Ok(tools) => {
                self.db.set_mcp_status(server_id, "connected", None, Some(&Value::Array(tools.clone()))).await?;
                Ok(tools)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let status = if msg.contains("unauthorized") || msg.contains("not authorized") {
                    "unauthorized"
                } else {
                    "error"
                };
                self.db.set_mcp_status(server_id, status, Some(&msg), None).await?;
                Err(e)
            }
        }
    }

    /// Call a tool; returns (text, is_error). Re-initializes once if the
    /// session was lost.
    pub async fn call(&self, server_id: &str, tool: &str, args: Value) -> Result<(String, bool)> {
        let s = self.db.mcp_server(server_id).await?.ok_or_else(|| anyhow!("unknown MCP server"))?;
        if !s.enabled {
            bail!("MCP server {} is disabled", s.name);
        }
        let args = if args.is_object() { args } else { json!({}) };
        let params = json!({"name": tool, "arguments": args});
        let mut result = self.rpc(&s, "tools/call", params.clone()).await;
        if let Err(e) = &result {
            let msg = format!("{e:#}");
            if msg.contains("session expired")
                || (s.transport != "sse" && !self.sessions.lock().contains_key(server_id))
            {
                self.initialize(&s).await?;
                result = self.rpc(&s, "tools/call", params).await;
            }
        }
        let r = result?;
        Ok(render_result(&r))
    }

    // ------------------------------------------------------------------ OAuth

    pub async fn start_oauth(&self, server_id: &str, redirect_uri: &str) -> Result<String> {
        let s = self.db.mcp_server(server_id).await?.ok_or_else(|| anyhow!("unknown MCP server"))?;
        let mut st = oauth::discover(&self.http, &s.url, s.oauth.scope.as_deref()).await?;
        st.client_id = s.oauth.client_id.clone().filter(|c| !c.is_empty());
        st.client_secret = s.oauth.client_secret.clone();
        oauth::register(&self.http, &mut st, redirect_uri).await?;
        let state = oauth::random_token(24);
        let verifier = oauth::random_token(48);
        let url = oauth::authorize_url(&st, redirect_uri, &state, &verifier)?;
        self.db.set_mcp_oauth(server_id, &st).await?;
        let mut pending = self.pending_oauth.lock();
        // One sign-in per server at a time: a new Connect replaces the old one.
        pending.retain(|_, (sid, _, _)| sid != server_id);
        pending.insert(state, (server_id.to_string(), verifier, redirect_uri.to_string()));
        drop(pending);
        Ok(url)
    }

    pub async fn finish_oauth(&self, state: &str, code: &str) -> Result<String> {
        let (server_id, verifier, redirect_uri) =
            self.pending_oauth.lock().remove(state).ok_or_else(|| anyhow!("unknown or expired OAuth state"))?;
        let s = self.db.mcp_server(&server_id).await?.ok_or_else(|| anyhow!("unknown MCP server"))?;
        let mut st: OAuthState = s.oauth.clone();
        oauth::exchange_code(&self.http, &mut st, code, &redirect_uri, &verifier).await?;
        self.db.set_mcp_oauth(&server_id, &st).await?;
        let _ = self.refresh(&server_id).await;
        Ok(server_id)
    }

    /// Finish a sign-in from what the user pasted (the loopback redirect URL,
    /// its query, or the bare code). Used when the browser cannot reach
    /// kongming, e.g. behind a firewall.
    pub async fn finish_oauth_pasted(&self, server_id: &str, pasted: &str) -> Result<String> {
        let (code, state) = oauth::parse_redirect(pasted)?;
        let state = {
            let pending = self.pending_oauth.lock();
            match state {
                Some(st) => match pending.get(&st) {
                    Some((sid, _, _)) if sid == server_id => st,
                    Some(_) => bail!("that sign-in belongs to a different MCP server"),
                    None => bail!("unknown or expired sign-in; press Connect again"),
                },
                None => pending
                    .iter()
                    .find(|(_, (sid, _, _))| sid == server_id)
                    .map(|(st, _)| st.clone())
                    .ok_or_else(|| anyhow!("no sign-in in progress for this server; press Connect first"))?,
            }
        };
        self.finish_oauth(&state, &code).await
    }

    pub async fn clear_oauth(&self, server_id: &str) -> Result<()> {
        let s = self.db.mcp_server(server_id).await?.ok_or_else(|| anyhow!("unknown MCP server"))?;
        let st = OAuthState {
            client_id: s.oauth.client_id.clone(),
            client_secret: s.oauth.client_secret.clone(),
            scope: s.oauth.scope.clone(),
            ..Default::default()
        };
        self.db.set_mcp_oauth(server_id, &st).await?;
        self.db.set_mcp_status(server_id, "unauthorized", None, None).await?;
        Ok(())
    }

    pub fn forget(&self, server_id: &str) {
        self.sessions.lock().remove(server_id);
        self.sse.lock().remove(server_id);
    }

    /// Refresh every enabled server at boot and periodically.
    pub async fn refresh_all(&self) {
        if let Ok(servers) = self.db.mcp_servers().await {
            for s in servers.into_iter().filter(|s| s.enabled) {
                if let Err(e) = self.refresh(&s.id).await {
                    tracing::warn!(server = %s.name, "mcp refresh failed: {e:#}");
                }
            }
        }
    }
}

/// Flatten an MCP `tools/call` result into text for the model.
pub fn render_result(r: &Value) -> (String, bool) {
    let is_error = r.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let mut out = String::new();
    if let Some(items) = r.get("content").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => out.push_str(item.get("text").and_then(Value::as_str).unwrap_or("")),
                Some("image") => {
                    out.push_str(&format!("[image {}]", item.get("mimeType").and_then(Value::as_str).unwrap_or("")))
                }
                Some("audio") => out.push_str("[audio]"),
                Some("resource") => {
                    let res = item.get("resource").cloned().unwrap_or(Value::Null);
                    out.push_str(res.get("text").and_then(Value::as_str).unwrap_or("[resource]"));
                }
                Some("resource_link") => {
                    out.push_str(&format!("[link {}]", item.get("uri").and_then(Value::as_str).unwrap_or("")))
                }
                _ => out.push_str(&item.to_string()),
            }
            out.push('\n');
        }
    } else if let Some(sc) = r.get("structuredContent") {
        out.push_str(&sc.to_string());
    } else {
        out.push_str(&r.to_string());
    }
    let out = out.trim_end().to_string();
    let is_error = is_error || reports_failure(&out);
    (truncate(&out, MAX_RESULT_CHARS), is_error)
}

/// Servers such as Composio answer a failed call as a normal result whose JSON
/// says `"successful": false`; treat that as the error it is.
fn reports_failure(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return false };
    let flag = |k: &str| v.get(k).and_then(Value::as_bool);
    flag("successful") == Some(false) || flag("success") == Some(false) || flag("isError") == Some(true)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n…[truncated, {} more characters]", s.chars().count() - max)
}

pub fn server_public(s: &McpServer) -> Value {
    json!({
        "id": s.id, "name": s.name, "url": s.url, "transport": s.transport, "authType": s.auth_type,
        "hasToken": s.has_token(), "oauthConnected": s.has_oauth_token(), "oauthClientId": s.oauth.client_id,
        "oauthScope": s.oauth.scope, "headers": s.headers, "enabled": s.enabled, "status": s.status,
        "lastError": s.last_error, "tools": s.tools, "toolCount": s.tools.as_array().map_or(0, |a| a.len()),
        "createdAt": s.created_at, "updatedAt": s.updated_at, "now": now_ms(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_round_trip() {
        let n = tool_name("My Server!", "do_thing");
        assert_eq!(n, "mcp__my_server__do_thing");
        assert_eq!(parse_tool_name(&n), Some(("my_server", "do_thing")));
        assert_eq!(parse_tool_name("web_fetch"), None);
    }

    #[test]
    fn renders_text_and_error_results() {
        let (t, e) = render_result(
            &json!({"content": [{"type": "text", "text": "a"}, {"type": "image", "mimeType": "image/png"}], "isError": true}),
        );
        assert_eq!(t, "a\n[image image/png]");
        assert!(e);
        let (t, e) = render_result(&json!({"structuredContent": {"x": 1}}));
        assert_eq!(t, "{\"x\":1}");
        assert!(!e);
        // Composio reports failures in the text of a normal result.
        let failed = r#"{"successful":false,"data":{},"error":"Following fields are missing: {'owner', 'repo'}"}"#;
        assert!(render_result(&json!({"content": [{"type": "text", "text": failed}]})).1);
        let fine = r#"{"successful":true,"data":{"items":[]},"error":null}"#;
        assert!(!render_result(&json!({"content": [{"type": "text", "text": fine}]})).1);
    }

    #[test]
    fn truncates_long_output() {
        let s = "x".repeat(50);
        let t = truncate(&s, 10);
        assert!(t.starts_with("xxxxxxxxxx\n…[truncated, 40"));
        assert_eq!(truncate("short", 10), "short");
    }
}
