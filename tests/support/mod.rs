//! Shared test harness: an in-memory Hrana (Turso) server, an OpenAI-style LLM
//! mock, a Streamable HTTP MCP mock, and `spawn_app` which boots the real
//! router against all three.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use kongming::config::Config;
use kongming::db::{Db, Turso};
use kongming::state::{AppState, Shared};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};

/// How long a test waits for anything: 10 s, or `KONGMING_TEST_TIMEOUT_SECS`
/// (the Orange Pi simulation in CI runs under QEMU and needs longer).
pub fn timeout() -> Duration {
    let secs = std::env::var("KONGMING_TEST_TIMEOUT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    Duration::from_secs(secs)
}

/// Bind a router on an ephemeral loopback port and return its base URL.
pub async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    format!("http://{addr}")
}

/// Await a future with the standard test timeout.
pub async fn within<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(timeout(), fut).await.expect("timed out waiting")
}

// ------------------------------------------------------------------ Hrana mock

struct HranaInner {
    conn: Mutex<Connection>,
}

#[derive(Clone)]
pub struct HranaMock {
    pub url: String,
    inner: Arc<HranaInner>,
}

fn hrana_to_sql(v: &Value) -> rusqlite::types::Value {
    match v.get("type").and_then(Value::as_str) {
        Some("integer") => {
            let n = v
                .get("value")
                .and_then(|x| x.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| x.as_i64()))
                .unwrap_or(0);
            rusqlite::types::Value::Integer(n)
        }
        Some("float") => rusqlite::types::Value::Real(v.get("value").and_then(Value::as_f64).unwrap_or(0.0)),
        Some("text") => rusqlite::types::Value::Text(v.get("value").and_then(Value::as_str).unwrap_or("").to_string()),
        Some("blob") => {
            let bytes = v
                .get("base64")
                .and_then(Value::as_str)
                .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
                .unwrap_or_default();
            rusqlite::types::Value::Blob(bytes)
        }
        _ => rusqlite::types::Value::Null,
    }
}

fn sql_to_hrana(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => json!({"type": "null"}),
        ValueRef::Integer(i) => json!({"type": "integer", "value": i.to_string()}),
        ValueRef::Real(f) => json!({"type": "float", "value": f}),
        ValueRef::Text(t) => json!({"type": "text", "value": String::from_utf8_lossy(t)}),
        ValueRef::Blob(b) => json!({"type": "blob", "base64": base64::engine::general_purpose::STANDARD.encode(b)}),
    }
}

fn run_execute(conn: &Connection, stmt: &Value) -> Result<Value, String> {
    let sql = stmt.get("sql").and_then(Value::as_str).unwrap_or("");
    let args: Vec<rusqlite::types::Value> =
        stmt.get("args").and_then(Value::as_array).map(|a| a.iter().map(hrana_to_sql).collect()).unwrap_or_default();
    let mut prepared = conn.prepare(sql).map_err(|e| e.to_string())?;
    let cols: Vec<String> = prepared.column_names().iter().map(|c| c.to_string()).collect();
    let ncols = cols.len();
    let mut rows_out: Vec<Value> = Vec::new();
    {
        let mut rows = prepared.query(rusqlite::params_from_iter(args.iter())).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let mut cells = Vec::with_capacity(ncols);
            for i in 0..ncols {
                let cell = row.get_ref(i).map_err(|e| e.to_string())?;
                cells.push(sql_to_hrana(cell));
            }
            rows_out.push(Value::Array(cells));
        }
    }
    let affected = conn.changes();
    let last_insert_rowid = conn.last_insert_rowid();
    let cols_json: Vec<Value> = cols.iter().map(|c| json!({"name": c, "decltype": null})).collect();
    Ok(json!({
        "cols": cols_json,
        "rows": rows_out,
        "affected_row_count": affected,
        "last_insert_rowid": last_insert_rowid.to_string(),
    }))
}

fn run_pipeline(conn: &Connection, body: &Value) -> Value {
    let mut results = Vec::new();
    let empty = Vec::new();
    let reqs = body.get("requests").and_then(Value::as_array).unwrap_or(&empty);
    for req in reqs {
        let kind = req.get("type").and_then(Value::as_str).unwrap_or("");
        let res: Result<Value, String> = match kind {
            "execute" => run_execute(conn, req.get("stmt").unwrap_or(&Value::Null))
                .map(|result| json!({"type": "execute", "result": result})),
            "sequence" => {
                let sql = req.get("sql").and_then(Value::as_str).unwrap_or("");
                conn.execute_batch(sql).map(|_| json!({"type": "sequence"})).map_err(|e| e.to_string())
            }
            "close" => Ok(json!({"type": "close"})),
            other => Err(format!("unsupported request type '{other}'")),
        };
        match res {
            Ok(response) => results.push(json!({"type": "ok", "response": response})),
            Err(message) => {
                // Turso rolls an aborted pipeline back when the connection closes.
                if !conn.is_autocommit() {
                    let _ = conn.execute_batch("ROLLBACK");
                }
                results.push(json!({"type": "error", "error": {"message": message}}));
            }
        }
    }
    json!({"results": results, "baton": null, "base_url": null})
}

async fn hrana_handler(State(st): State<Arc<HranaInner>>, Json(body): Json<Value>) -> Json<Value> {
    let out = {
        let conn = st.conn.lock().expect("hrana lock");
        run_pipeline(&conn, &body)
    };
    Json(out)
}

impl HranaMock {
    pub async fn start() -> HranaMock {
        let conn = Connection::open_in_memory().expect("sqlite in memory");
        let inner = Arc::new(HranaInner { conn: Mutex::new(conn) });
        let router = Router::new().route("/v2/pipeline", post(hrana_handler)).with_state(inner.clone());
        let url = serve(router).await;
        HranaMock { url, inner }
    }

    /// Run a raw query against the backing SQLite (for assertions).
    pub fn query_i64(&self, sql: &str) -> i64 {
        let conn = self.inner.conn.lock().expect("hrana lock");
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).expect("query_i64")
    }

    pub fn turso(&self) -> Turso {
        Turso::new(&self.url, "test-token").expect("turso client")
    }

    pub async fn db(&self) -> Db {
        let db = Db::new(self.turso());
        db.migrate().await.expect("migrate");
        db
    }
}

// -------------------------------------------------------------------- LLM mock

#[derive(Debug, Clone)]
pub enum ScriptedReply {
    Text(String),
    ToolCalls(Vec<(String, Value)>),
}

pub fn text(s: &str) -> ScriptedReply {
    ScriptedReply::Text(s.to_string())
}

pub fn tool_call(name: &str, args: Value) -> ScriptedReply {
    ScriptedReply::ToolCalls(vec![(name.to_string(), args)])
}

pub type Script = Arc<Mutex<VecDeque<ScriptedReply>>>;

struct LlmInner {
    script: Script,
    requests: Mutex<Vec<Value>>,
}

#[derive(Clone)]
pub struct LlmMock {
    pub url: String,
    inner: Arc<LlmInner>,
}

fn split_chars(s: &str, pieces: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let per = chars.len().div_ceil(pieces).max(1);
    chars.chunks(per).map(|c| c.iter().collect()).collect()
}

fn sse_line(v: &Value) -> String {
    format!("data: {v}\n\n")
}

fn sse_body(reply: &ScriptedReply) -> String {
    let mut out = String::new();
    let finish = match reply {
        ScriptedReply::Text(t) => {
            for piece in split_chars(t, 3) {
                out.push_str(&sse_line(&json!({
                    "id": "chatcmpl-test", "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {"role": "assistant", "content": piece}, "finish_reason": null}]
                })));
            }
            "stop"
        }
        ScriptedReply::ToolCalls(calls) => {
            for (i, (name, args)) in calls.iter().enumerate() {
                let parts = split_chars(&args.to_string(), 2);
                let (head, tail) = (parts[0].clone(), parts.get(1).cloned().unwrap_or_default());
                out.push_str(&sse_line(&json!({
                    "id": "chatcmpl-test", "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {"role": "assistant", "content": null, "tool_calls": [
                        {"index": i, "id": format!("call_{i}"), "type": "function", "function": {"name": name, "arguments": head}}
                    ]}, "finish_reason": null}]
                })));
                out.push_str(&sse_line(&json!({
                    "id": "chatcmpl-test", "object": "chat.completion.chunk",
                    "choices": [{"index": 0, "delta": {"tool_calls": [
                        {"index": i, "function": {"arguments": tail}}
                    ]}, "finish_reason": null}]
                })));
            }
            "tool_calls"
        }
    };
    out.push_str(&sse_line(&json!({
        "id": "chatcmpl-test", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]
    })));
    out.push_str(&sse_line(&json!({
        "id": "chatcmpl-test", "object": "chat.completion.chunk",
        "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    })));
    out.push_str("data: [DONE]\n\n");
    out
}

async fn llm_handler(State(st): State<Arc<LlmInner>>, Json(body): Json<Value>) -> Response {
    st.requests.lock().expect("llm lock").push(body);
    let reply = st.script.lock().expect("script lock").pop_front().unwrap_or_else(|| text("(no script)"));
    let body = sse_body(&reply);
    (StatusCode::OK, [("content-type", "text/event-stream"), ("cache-control", "no-cache")], body).into_response()
}

impl LlmMock {
    pub async fn start(replies: Vec<ScriptedReply>) -> LlmMock {
        let inner = Arc::new(LlmInner {
            script: Arc::new(Mutex::new(replies.into_iter().collect())),
            requests: Mutex::new(Vec::new()),
        });
        let router = Router::new().route("/v1/chat/completions", post(llm_handler)).with_state(inner.clone());
        let url = serve(router).await;
        LlmMock { url, inner }
    }

    pub fn script(&self) -> Script {
        self.inner.script.clone()
    }

    /// Append replies to the script.
    pub fn push(&self, replies: Vec<ScriptedReply>) {
        self.inner.script.lock().expect("script lock").extend(replies);
    }

    /// Replace the whole script.
    pub fn set(&self, replies: Vec<ScriptedReply>) {
        *self.inner.script.lock().expect("script lock") = replies.into_iter().collect();
    }

    pub fn remaining(&self) -> usize {
        self.inner.script.lock().expect("script lock").len()
    }

    /// Every request body received so far, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.inner.requests.lock().expect("llm lock").clone()
    }

    pub fn request_count(&self) -> usize {
        self.inner.requests.lock().expect("llm lock").len()
    }
}

/// System prompt of a recorded chat-completions request (empty if none).
pub fn system_prompt(req: &Value) -> String {
    req.get("messages")
        .and_then(Value::as_array)
        .and_then(|m| m.iter().find(|x| x.get("role").and_then(Value::as_str) == Some("system")))
        .and_then(|m| m.get("content").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

/// Function names of the tools offered in a recorded request.
pub fn tool_names(req: &Value) -> Vec<String> {
    req.get("tools")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter().filter_map(|x| x.pointer("/function/name").and_then(Value::as_str).map(str::to_string)).collect()
        })
        .unwrap_or_default()
}

// -------------------------------------------------------------------- MCP mock

struct McpInner {
    require_token: bool,
    calls: Mutex<Vec<Value>>,
}

#[derive(Clone)]
pub struct McpMock {
    pub url: String,
    inner: Arc<McpInner>,
}

fn mcp_json(v: Value) -> Response {
    (StatusCode::OK, [("mcp-session-id", "test-session"), ("content-type", "application/json")], v.to_string())
        .into_response()
}

async fn mcp_handler(State(st): State<Arc<McpInner>>, headers: HeaderMap, body: String) -> Response {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    if st.require_token && auth != "Bearer good" {
        return (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer")], "unauthorized").into_response();
    }
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad json").into_response(),
    };
    st.calls.lock().expect("mcp lock").push(v.clone());
    let method = v.get("method").and_then(Value::as_str).unwrap_or("");
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => mcp_json(json!({"jsonrpc": "2.0", "id": id, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo-mock", "version": "0.1.0"}
        }})),
        "notifications/initialized" => (StatusCode::ACCEPTED, [("mcp-session-id", "test-session")]).into_response(),
        "tools/list" => mcp_json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{
            "name": "echo",
            "description": "Echo the text back",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}
        }]}})),
        "tools/call" => {
            let name = v.pointer("/params/name").and_then(Value::as_str).unwrap_or("");
            if name != "echo" {
                return mcp_json(
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": format!("unknown tool {name}")}}),
                );
            }
            let text = v.pointer("/params/arguments/text").and_then(Value::as_str).unwrap_or("");
            mcp_json(json!({"jsonrpc": "2.0", "id": id, "result": {
                "content": [{"type": "text", "text": format!("echo: {text}")}],
                "isError": false
            }}))
        }
        other => mcp_json(
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}}),
        ),
    }
}

impl McpMock {
    pub async fn start(require_token: bool) -> McpMock {
        let inner = Arc::new(McpInner { require_token, calls: Mutex::new(Vec::new()) });
        let router = Router::new().route("/mcp", post(mcp_handler)).with_state(inner.clone());
        let url = serve(router).await;
        McpMock { url, inner }
    }

    /// Endpoint URL to register in kongming.
    pub fn endpoint(&self) -> String {
        format!("{}/mcp", self.url)
    }

    pub fn calls(&self) -> Vec<Value> {
        self.inner.calls.lock().expect("mcp lock").clone()
    }
}

// ------------------------------------------------------------------- the app

pub struct TestApp {
    pub base_url: String,
    pub state: Shared,
    pub llm: LlmMock,
    pub hrana: HranaMock,
    /// Authenticated client (session cookie attached to every request).
    pub client: reqwest::Client,
    /// Client without credentials.
    pub plain: reqwest::Client,
    /// The session token returned by login.
    pub token: String,
}

pub const PASSWORD: &str = "pw";

pub async fn spawn_app(replies: Vec<ScriptedReply>) -> TestApp {
    spawn_app_with(replies, &[]).await
}

/// `spawn_app` with extra command-line flags, e.g. `--telegram-api`.
pub async fn spawn_app_with(replies: Vec<ScriptedReply>, extra_args: &[&str]) -> TestApp {
    let hrana = HranaMock::start().await;
    let llm = LlmMock::start(replies).await;
    let mut args: Vec<String> = vec![
        "kongming".into(),
        "--turso-url".into(),
        hrana.url.clone(),
        "--turso-token".into(),
        "test-token".into(),
        "--password".into(),
        PASSWORD.into(),
        "--secret".into(),
        "test-secret-test-secret".into(),
        "--host".into(),
        "127.0.0.1".into(),
    ];
    args.extend(extra_args.iter().map(|a| a.to_string()));
    let cfg = Config::from_args(args).expect("config");
    let turso = Turso::new(&cfg.turso_url, &cfg.turso_token).expect("turso");
    let db = Db::new(turso);
    db.migrate().await.expect("migrate");
    // The LLM is configured from the dashboard, i.e. a saved setting.
    let llm_setting = json!({"kind": "openai", "baseUrl": format!("{}/v1", llm.url), "apiKey": "test", "model": "test-model"});
    db.set_setting("llm", &llm_setting).await.expect("llm setting");
    let state = AppState::new(cfg, db);
    kongming::agent::ensure_default_agent(&state).await.expect("default agent");
    let base_url = serve(kongming::web::router(state.clone())).await;

    let plain = reqwest::Client::builder().build().expect("client");
    let login = within(plain.post(format!("{base_url}/api/auth/login")).json(&json!({"password": PASSWORD})).send())
        .await
        .expect("login request");
    assert_eq!(login.status(), reqwest::StatusCode::OK, "login failed");
    let body: Value = login.json().await.expect("login json");
    let token = body.get("token").and_then(Value::as_str).expect("token in login reply").to_string();

    let mut headers = reqwest::header::HeaderMap::new();
    let cookie = format!("{}={}", kongming::web::auth::COOKIE, token);
    headers.insert(reqwest::header::COOKIE, reqwest::header::HeaderValue::from_str(&cookie).expect("cookie header"));
    let client = reqwest::Client::builder().default_headers(headers).build().expect("client");

    TestApp { base_url, state, llm, hrana, client, plain, token }
}

impl TestApp {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn ws_url(&self) -> String {
        format!("{}/ws", self.base_url.replacen("http://", "ws://", 1))
    }

    pub async fn disable_reflection(&self) {
        self.state.db.set_setting("reflection", &json!({"enabled": false})).await.expect("set reflection");
    }

    pub async fn enable_reflection(&self) {
        self.state.db.set_setting("reflection", &json!({"enabled": true})).await.expect("set reflection");
    }

    /// GET and parse JSON; panics on non-2xx.
    pub async fn get(&self, path: &str) -> Value {
        let resp = within(self.client.get(self.url(path)).send()).await.expect("get");
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "GET {path} -> {status}: {body}");
        body
    }

    /// POST JSON and return (status, body).
    pub async fn post_raw(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let resp = within(self.client.post(self.url(path)).json(&body).send()).await.expect("post");
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    /// POST JSON; panics on non-2xx.
    pub async fn post(&self, path: &str, body: Value) -> Value {
        let (status, out) = self.post_raw(path, body).await;
        assert!(status.is_success(), "POST {path} -> {status}: {out}");
        out
    }

    pub async fn put_raw(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let resp = within(self.client.put(self.url(path)).json(&body).send()).await.expect("put");
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    pub async fn put(&self, path: &str, body: Value) -> Value {
        let (status, out) = self.put_raw(path, body).await;
        assert!(status.is_success(), "PUT {path} -> {status}: {out}");
        out
    }

    pub async fn delete(&self, path: &str) -> Value {
        let resp = within(self.client.delete(self.url(path)).send()).await.expect("delete");
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "DELETE {path} -> {status}: {body}");
        body
    }

    /// Create a web session for the default agent and return its id.
    pub async fn create_session(&self) -> String {
        let v = self.post("/api/sessions", json!({})).await;
        v.get("id").and_then(Value::as_str).expect("session id").to_string()
    }

    /// Send a message and wait for the full turn; returns the reply JSON.
    pub async fn send_wait(&self, session_id: &str, text: &str) -> Value {
        let path = format!("/api/sessions/{session_id}/messages?wait=true");
        let resp = within(self.client.post(self.url(&path)).json(&json!({"text": text})).send()).await.expect("send");
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "send -> {status}: {body}");
        body
    }

    /// Messages of a session as stored (GET /api/sessions/:id).
    pub async fn messages(&self, session_id: &str) -> Vec<Value> {
        let v = self.get(&format!("/api/sessions/{session_id}")).await;
        v.get("messages").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    pub async fn default_agent(&self) -> kongming::db::Agent {
        self.state.db.default_agent().await.expect("default agent")
    }
}

/// Poll `check` every 50 ms until it returns `Some`, or panic after `timeout()`.
pub async fn poll_until<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout();
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Parts of a stored message, as `(type, json)` pairs.
pub fn part_types(msg: &Value) -> Vec<String> {
    msg.get("parts")
        .and_then(Value::as_array)
        .map(|p| p.iter().filter_map(|x| x.get("type").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn role(msg: &Value) -> &str {
    msg.get("role").and_then(Value::as_str).unwrap_or("")
}

pub fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
