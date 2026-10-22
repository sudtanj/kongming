//! Telegram bot, over long polling or, when kongming has a public HTTPS URL, a
//! webhook. Each chat maps to a session with the lead agent (the rest of the
//! team takes work only from it); `/new`, `/agents`, `/skills`, `/status` are
//! the commands. Replies stream by editing one message at a throttled rate,
//! below a message that shows the model's reasoning and tool calls (unless
//! `showThinking` is off).

use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

use crate::agent::{self, TurnOptions};
use crate::state::{Event, Shared};

const EDIT_INTERVAL: Duration = Duration::from_millis(900);
const MAX_MESSAGE: usize = 4000;

#[derive(Clone)]
pub struct Telegram {
    http: reqwest::Client,
    api: String,
    token: String,
}

impl Telegram {
    /// `api` is the Bot API base URL, normally `https://api.telegram.org`.
    pub fn new(http: reqwest::Client, api: &str, token: String) -> Telegram {
        Telegram { http, api: api.trim_end_matches('/').to_string(), token }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api, self.token)
    }

    pub async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let r = self
            .http
            .post(self.url(method))
            .json(&body)
            .timeout(Duration::from_secs(70))
            .send()
            .await
            // The URL carries the bot token: keep it out of errors and logs.
            .map_err(|e| anyhow!("telegram {method} request failed: {}", e.without_url()))?;
        let v: Value =
            r.json().await.map_err(|e| anyhow!("telegram {method}: reply was not JSON: {}", e.without_url()))?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(anyhow!(
                "telegram {method}: {}",
                v.get("description").and_then(Value::as_str).unwrap_or("unknown error")
            ));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<i64> {
        self.send(chat_id, text, false).await
    }

    /// `silent`: delivered without a sound or vibration.
    pub async fn send(&self, chat_id: i64, text: &str, silent: bool) -> Result<i64> {
        let text = clamp(text);
        let r = match self
            .call(
                "sendMessage",
                json!({"chat_id": chat_id, "text": text, "parse_mode": "HTML", "disable_web_page_preview": true, "disable_notification": silent}),
            )
            .await
        {
            Ok(r) => r,
            // HTML parse failures: resend as plain text.
            Err(_) => {
                self.call(
                    "sendMessage",
                    json!({"chat_id": chat_id, "text": strip_html(&text), "disable_web_page_preview": true, "disable_notification": silent}),
                )
                .await?
            }
        };
        Ok(r.get("message_id").and_then(Value::as_i64).unwrap_or(0))
    }

    pub async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let text = clamp(text);
        if self.call("editMessageText", json!({"chat_id": chat_id, "message_id": message_id, "text": text, "parse_mode": "HTML", "disable_web_page_preview": true})).await.is_err() {
            self.call("editMessageText", json!({"chat_id": chat_id, "message_id": message_id, "text": strip_html(&text), "disable_web_page_preview": true})).await?;
        }
        Ok(())
    }

    pub async fn send_long(&self, chat_id: i64, text: &str) -> Result<()> {
        for chunk in split_chunks(text, MAX_MESSAGE) {
            self.send_message(chat_id, &chunk).await?;
        }
        Ok(())
    }

    pub async fn typing(&self, chat_id: i64) {
        let _ = self.call("sendChatAction", json!({"chat_id": chat_id, "action": "typing"})).await;
    }
}

fn clamp(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE {
        text.to_string()
    } else {
        text.chars().take(MAX_MESSAGE - 1).chain(std::iter::once('…')).collect()
    }
}

pub fn split_chunks(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if cur.chars().count() + line.chars().count() > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if line.chars().count() > max {
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(max) {
                out.push(piece.iter().collect());
            }
        } else {
            cur.push_str(line);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Minimal Markdown -> Telegram HTML: fenced code, inline code, bold, italic,
/// links, headings become bold lines, list markers kept.
pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                out.push_str("</code></pre>\n");
            } else {
                out.push_str("<pre><code>");
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push_str(&esc(line));
            out.push('\n');
            continue;
        }
        let l = line.trim_start_matches('#');
        let is_heading = l.len() != line.len() && l.starts_with(' ');
        let mut rendered = inline(l.trim());
        if is_heading {
            rendered = format!("<b>{rendered}</b>");
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        out.push_str(indent);
        out.push_str(&rendered);
        out.push('\n');
    }
    if in_code {
        out.push_str("</code></pre>\n");
    }
    out.trim_end().to_string()
}

fn inline(s: &str) -> String {
    // Protect inline code first.
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('`') {
        let before = &rest[..start];
        out.push_str(&inline_fmt(before));
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                out.push_str(&format!("<code>{}</code>", esc(&after[..end])));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&esc(&rest[start..]));
                rest = "";
                break;
            }
        }
    }
    out.push_str(&inline_fmt(rest));
    out
}

fn inline_fmt(s: &str) -> String {
    let mut t = esc(s);
    t = replace_pairs(&t, "**", "<b>", "</b>");
    t = replace_pairs(&t, "__", "<b>", "</b>");
    t = replace_pairs(&t, "~~", "<s>", "</s>");
    // Links [text](url)
    let mut out = String::new();
    let mut rest = t.as_str();
    while let Some(ls) = rest.find('[') {
        if let Some(le) = rest[ls..].find("](") {
            let text = &rest[ls + 1..ls + le];
            let after = &rest[ls + le + 2..];
            if let Some(ue) = after.find(')') {
                let url = &after[..ue];
                if url.starts_with("http") && !text.contains('[') {
                    out.push_str(&rest[..ls]);
                    out.push_str(&format!("<a href=\"{}\">{}</a>", url.replace('"', "%22"), text));
                    rest = &after[ue + 1..];
                    continue;
                }
            }
        }
        out.push_str(&rest[..ls + 1]);
        rest = &rest[ls + 1..];
    }
    out.push_str(rest);
    out
}

fn replace_pairs(s: &str, marker: &str, open: &str, close: &str) -> String {
    let parts: Vec<&str> = s.split(marker).collect();
    if parts.len() < 3 {
        return s.to_string();
    }
    let mut out = String::new();
    let mut open_now = false;
    let last = parts.len() - 1;
    for (i, p) in parts.iter().enumerate() {
        out.push_str(p);
        if i == last {
            break;
        }
        if !open_now && i + 1 < last || (!open_now && i + 1 == last && false) {
            out.push_str(open);
            open_now = true;
        } else if open_now {
            out.push_str(close);
            open_now = false;
        } else {
            out.push_str(marker);
        }
    }
    if open_now {
        out.push_str(close);
    }
    out
}

/// Where Telegram posts updates in webhook mode, under the public URL.
pub const WEBHOOK_PATH: &str = "/api/telegram/webhook";

fn commands() -> Value {
    json!({"commands": [
        {"command": "new", "description": "Start a new conversation"},
        {"command": "agents", "description": "Show the team"},
        {"command": "skills", "description": "List skills"},
        {"command": "status", "description": "Show status"},
        {"command": "stop", "description": "Stop the current reply"},
        {"command": "proactive", "description": "Messages Kongming sends on its own: on, off, now"},
        {"command": "help", "description": "Help"}
    ]})
}

pub fn client(state: &Shared, token: String) -> Telegram {
    Telegram::new(state.http.clone(), &state.cfg.telegram_api, token)
}

/// The bot token, when Telegram is enabled.
pub async fn active_token(state: &Shared) -> Option<String> {
    let enabled = state
        .db
        .setting("telegram")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.get("enabled").and_then(Value::as_bool))
        .unwrap_or(true);
    if enabled {
        state.telegram_token().await
    } else {
        None
    }
}

/// The webhook URL, when the public URL is HTTPS (Telegram accepts nothing else).
pub async fn webhook_url(state: &Shared) -> Option<String> {
    state.public_url().await.filter(|u| u.starts_with("https://")).map(|u| format!("{u}{WEBHOOK_PATH}"))
}

async fn webhook_secret(state: &Shared) -> Result<String> {
    let saved = state.db.setting("telegram.webhookSecret").await?;
    if let Some(s) = saved.as_ref().and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return Ok(s.to_string());
    }
    let s = crate::mcp::oauth::random_token(32);
    state.db.set_setting("telegram.webhookSecret", &json!(s)).await?;
    Ok(s)
}

/// Check the token, register the bot commands and tell Telegram where to send
/// updates: the webhook when kongming has a public HTTPS URL, otherwise no
/// webhook so long polling works (the usual case behind a firewall). The
/// outcome is kept for the dashboard.
pub async fn connect(state: &Shared, tg: &Telegram) -> Result<Value> {
    let res = setup(state, tg).await;
    *state.telegram_status.lock() = match &res {
        Ok(v) => v.clone(),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    };
    res
}

async fn setup(state: &Shared, tg: &Telegram) -> Result<Value> {
    let me = tg.call("getMe", json!({})).await?;
    let bot = me.get("username").and_then(Value::as_str).unwrap_or("").to_string();
    if let Err(e) = tg.call("setMyCommands", commands()).await {
        tracing::warn!("telegram setMyCommands failed: {e:#}");
    }
    let webhook = webhook_url(state).await;
    match &webhook {
        Some(url) => {
            let secret = webhook_secret(state).await?;
            tg.call("setWebhook", json!({"url": url, "secret_token": secret, "allowed_updates": ["message"]})).await?;
        }
        None => {
            tg.call("deleteWebhook", json!({})).await?;
        }
    }
    let mode = if webhook.is_some() { "webhook" } else { "polling" };
    tracing::info!(bot = %bot, mode, "telegram connected");
    Ok(json!({"ok": true, "bot": bot, "mode": mode, "webhookUrl": webhook}))
}

/// Remove the webhook when Telegram is switched off or the token cleared.
pub async fn disconnect(state: &Shared, token: String) {
    let _ = client(state, token).call("deleteWebhook", json!({})).await;
    *state.telegram_status.lock() = Value::Null;
}

async fn wait(state: &Shared, d: Duration) {
    tokio::select! {
        _ = state.telegram_reload.notified() => {}
        _ = tokio::time::sleep(d) => {}
    }
}

async fn unchanged(state: &Shared, token: &str, public: &Option<String>) -> bool {
    active_token(state).await.as_deref() == Some(token) && state.public_url().await == *public
}

fn dispatch(state: &Shared, tg: &Telegram, update: &Value) {
    if let Some(msg) = update.get("message") {
        let (st, tg, msg) = (state.clone(), tg.clone(), msg.clone());
        tokio::spawn(async move {
            if let Err(e) = handle_message(st, tg, msg).await {
                tracing::warn!("telegram message failed: {e:#}");
            }
        });
    }
}

/// Keep Telegram connected until the process stops. In polling mode this runs
/// getUpdates; in webhook mode updates arrive on `webhook` and this only
/// watches for settings changes. A change in the dashboard wakes it at once.
pub async fn run(state: Shared) {
    loop {
        let Some(token) = active_token(&state).await else {
            wait(&state, Duration::from_secs(60)).await;
            continue;
        };
        let tg = client(&state, token.clone());
        let public = state.public_url().await;
        let use_webhook = match connect(&state, &tg).await {
            Ok(v) => v.get("mode").and_then(Value::as_str) == Some("webhook"),
            Err(e) => {
                tracing::warn!("telegram setup failed: {e:#}");
                wait(&state, Duration::from_secs(30)).await;
                continue;
            }
        };
        if use_webhook {
            while unchanged(&state, &token, &public).await {
                wait(&state, Duration::from_secs(60)).await;
            }
            continue;
        }
        let mut offset: i64 =
            state.db.setting("telegram.offset").await.ok().flatten().and_then(|v| v.as_i64()).unwrap_or(0);
        while unchanged(&state, &token, &public).await {
            let poll = tg.call("getUpdates", json!({"offset": offset, "timeout": 50, "allowed_updates": ["message"]}));
            let updates = tokio::select! {
                u = poll => u,
                _ = state.telegram_reload.notified() => continue,
            };
            let updates = match updates {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("telegram poll failed: {e:#}");
                    wait(&state, Duration::from_secs(5)).await;
                    continue;
                }
            };
            let Some(list) = updates.as_array() else { continue };
            for u in list {
                let id = u.get("update_id").and_then(Value::as_i64).unwrap_or(0);
                offset = offset.max(id + 1);
                dispatch(&state, &tg, u);
            }
            if !list.is_empty() {
                let _ = state.db.set_setting("telegram.offset", &json!(offset)).await;
            }
        }
    }
}

/// Webhook delivery from Telegram. The secret header, handed to Telegram in
/// setWebhook, proves the call is genuine.
pub async fn webhook(State(state): State<Shared>, headers: HeaderMap, Json(update): Json<Value>) -> StatusCode {
    let Some(token) = active_token(&state).await else {
        return StatusCode::NOT_FOUND;
    };
    let saved = state.db.setting("telegram.webhookSecret").await.ok().flatten();
    let expected = saved.as_ref().and_then(Value::as_str).unwrap_or("");
    let given = headers.get("x-telegram-bot-api-secret-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if expected.is_empty() || !same(expected.as_bytes(), given.as_bytes()) {
        return StatusCode::UNAUTHORIZED;
    }
    dispatch(&state, &client(&state, token), &update);
    StatusCode::OK
}

/// Constant-time comparison.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The chat's current conversation (a new one when `fresh` or none yet).
pub async fn session_for_chat(state: &Shared, chat_id: i64, fresh: bool) -> Result<crate::db::Session> {
    if !fresh {
        if let Some((sid, _)) = state.db.telegram_chat(chat_id).await? {
            if let Some(s) = state.db.session(&sid).await? {
                return Ok(s);
            }
        }
    }
    let agent = agent::ensure_default_agent(state).await?;
    let s = state.db.create_session(&agent.id, "telegram", Some(&chat_id.to_string()), None, "").await?;
    state.db.set_telegram_chat(chat_id, &s.id, &agent.id).await?;
    Ok(s)
}

/// Reasoning shown in the thinking message: the latest part, so it fits.
const THINKING_CHARS: usize = 2800;
/// Tool steps listed under it.
const MAX_STEPS: usize = 12;

/// The message above an answer that shows the model's reasoning and the tool
/// calls it makes, edited live like the answer.
#[derive(Default)]
struct Progress {
    thinking: String,
    /// (call id, tool name, outcome once known)
    steps: Vec<(String, String, Option<bool>)>,
    message_id: Option<i64>,
    sent: String,
    last_edit: Option<tokio::time::Instant>,
    /// An answer was posted below it: new activity starts a fresh message.
    closed: bool,
}

impl Progress {
    fn html(&self) -> String {
        let mut out = String::new();
        let thinking = self.thinking.trim();
        if !thinking.is_empty() {
            let n = thinking.chars().count();
            let tail: String = thinking.chars().skip(n.saturating_sub(THINKING_CHARS)).collect();
            out.push_str("💭 <b>Thinking</b>\n<blockquote expandable>");
            if n > THINKING_CHARS {
                out.push('…');
            }
            out.push_str(&esc(tail.trim()));
            out.push_str("</blockquote>");
        }
        let skip = self.steps.len().saturating_sub(MAX_STEPS);
        for (_, name, outcome) in self.steps.iter().skip(skip) {
            let mark = match outcome {
                None => "…",
                Some(true) => "✓",
                Some(false) => "✗",
            };
            out.push_str(&format!("\n🔧 <code>{}</code> {mark}", esc(name)));
        }
        out.trim_start().to_string()
    }

    /// Post or edit the message when it changed, at most once per interval
    /// unless `now` (the turn is over).
    async fn flush(&mut self, tg: &Telegram, chat_id: i64, now: bool) {
        let html = self.html();
        if html.is_empty() || html == self.sent {
            return;
        }
        if !now && self.last_edit.is_some_and(|t| t.elapsed() < EDIT_INTERVAL) {
            return;
        }
        match self.message_id {
            None => {
                if let Ok(id) = tg.send_message(chat_id, &html).await {
                    self.message_id = Some(id);
                }
            }
            Some(id) => {
                let _ = tg.edit_message(chat_id, id, &html).await;
            }
        }
        self.sent = html;
        self.last_edit = Some(tokio::time::Instant::now());
    }

    /// Activity after an answer was posted goes into a new message below it.
    fn reopen(&mut self) {
        if self.closed {
            *self = Progress::default();
        }
    }
}

/// Settings → Channels → "Show thinking" (on unless turned off).
async fn show_thinking(state: &Shared) -> bool {
    let tg = state.db.setting("telegram").await.ok().flatten();
    tg.and_then(|v| v.get("showThinking").and_then(Value::as_bool)).unwrap_or(true)
}

async fn handle_message(state: Shared, tg: Telegram, msg: Value) -> Result<()> {
    let chat_id = msg.pointer("/chat/id").and_then(Value::as_i64).ok_or_else(|| anyhow!("no chat id"))?;
    let user_id = msg.pointer("/from/id").and_then(Value::as_i64).unwrap_or(0);
    let allowed = state.telegram_allowed().await;
    if !allowed.contains(&user_id) {
        tracing::info!(user_id, "telegram message from a user not in the allowlist");
        tg.send_message(
            chat_id,
            &format!(
                "This bot is private. Your Telegram user id is <code>{user_id}</code>; ask the owner to allow it."
            ),
        )
        .await?;
        return Ok(());
    }
    let text = msg
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| msg.get("caption").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        tg.send_message(chat_id, "I can only read text for now.").await?;
        return Ok(());
    }

    if let Some(cmd) = text.strip_prefix('/') {
        let name = cmd.split(char::is_whitespace).next().unwrap_or(cmd);
        let name = name.split('@').next().unwrap_or(name);
        match name {
            "start" | "help" => {
                tg.send_message(chat_id, "Hi! Just type to talk to the lead agent; it runs the team and hands work to the others.\n/new – new conversation\n/agents – show the team\n/skills – list skills\n/status – status\n/stop – stop the current reply\n/proactive – messages Kongming sends on its own (on, off, now)").await?;
            }
            "new" => {
                let s = session_for_chat(&state, chat_id, true).await?;
                let a = state.db.agent(&s.agent_id).await?;
                tg.send_message(
                    chat_id,
                    &format!("New conversation with <b>@{}</b>.", a.map(|a| a.handle).unwrap_or_default()),
                )
                .await?;
            }
            "agents" => {
                let agents = state.db.agents().await?;
                let cur = session_for_chat(&state, chat_id, false).await?.agent_id;
                let lines: Vec<String> = agents
                    .iter()
                    .map(|a| {
                        format!(
                            "{}<b>@{}</b> – {}{}",
                            if a.id == cur { "▶ " } else { "" },
                            a.handle,
                            esc(&a.role),
                            a.manager_id
                                .as_ref()
                                .and_then(|m| agents.iter().find(|x| &x.id == m))
                                .map(|m| format!(" (reports to @{})", m.handle))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                tg.send_message(chat_id, &lines.join("\n")).await?;
            }
            "skills" => {
                let skills = state.db.skills().await?;
                if skills.is_empty() {
                    tg.send_message(chat_id, "No skills yet.").await?;
                } else {
                    let lines: Vec<String> = skills
                        .iter()
                        .map(|k| {
                            format!("{} <b>{}</b> – {}", if k.enabled { "•" } else { "○" }, k.name, esc(&k.description))
                        })
                        .collect();
                    tg.send_long(chat_id, &lines.join("\n")).await?;
                }
            }
            "status" => {
                let s = session_for_chat(&state, chat_id, false).await?;
                let a = state.db.agent(&s.agent_id).await?;
                let n = state.db.message_count(&s.id).await.unwrap_or(0);
                let cfg = state.provider_config().await?;
                tg.send_message(
                    chat_id,
                    &format!(
                        "Agent: <b>@{}</b>\nSession: <code>{}</code> ({n} messages)\nModel: {} via {}\nBusy: {}",
                        a.map(|a| a.handle).unwrap_or_default(),
                        s.id,
                        esc(&cfg.model),
                        cfg.kind,
                        state.is_running(&s.id)
                    ),
                )
                .await?;
            }
            "stop" => {
                let s = session_for_chat(&state, chat_id, false).await?;
                tg.send_message(chat_id, if state.cancel_turn(&s.id) { "Stopping." } else { "Nothing is running." })
                    .await?;
            }
            "proactive" => {
                let arg = cmd.split_whitespace().nth(1).unwrap_or("").to_ascii_lowercase();
                match arg.as_str() {
                    "on" | "off" => {
                        crate::proactive::set_enabled(&state, arg == "on").await?;
                        tg.send_message(chat_id, &crate::proactive::status_html(&state).await?).await?;
                    }
                    "now" => {
                        tg.send_message(chat_id, "Checking whether there is anything worth telling you…").await?;
                        let (st, tg) = (state.clone(), tg.clone());
                        tokio::spawn(async move {
                            let reply = match crate::proactive::check_in(&st, true).await {
                                Ok(0) => "Nothing worth telling you right now.".to_string(),
                                Ok(_) => return,
                                Err(e) => format!("⚠️ {}", esc(&format!("{e:#}"))),
                            };
                            let _ = tg.send_message(chat_id, &reply).await;
                        });
                    }
                    _ => {
                        tg.send_message(chat_id, &crate::proactive::status_html(&state).await?).await?;
                    }
                }
            }
            _ => {
                tg.send_message(chat_id, "Unknown command. /help").await?;
            }
        }
        return Ok(());
    }

    let session = session_for_chat(&state, chat_id, false).await?;
    if state.is_running(&session.id) {
        tg.send_message(chat_id, "Still working on the previous message. /stop to cancel it.").await?;
        return Ok(());
    }
    tg.typing(chat_id).await;
    let session_id = session.id.clone();
    let mut events = state.subscribe();
    let thinking = show_thinking(&state).await;
    let streamer = {
        let tg = tg.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut message_id: Option<i64> = None;
            let mut last_edit = tokio::time::Instant::now() - EDIT_INTERVAL;
            let mut last_sent = String::new();
            let mut progress = Progress::default();
            let mut ticker = tokio::time::interval(Duration::from_millis(300));
            loop {
                let mut finished = false;
                tokio::select! {
                    ev = events.recv() => {
                        match ev {
                            Ok(Event::Delta { session_id, text, .. }) if session_id == sid => buf.push_str(&text),
                            Ok(Event::ReasoningDelta { session_id, text, .. }) if session_id == sid && thinking => {
                                progress.reopen();
                                progress.thinking.push_str(&text);
                            }
                            Ok(Event::ToolResult { session_id, id, is_error, .. }) if session_id == sid && thinking => {
                                if let Some(step) = progress.steps.iter_mut().find(|s| s.0 == id) {
                                    step.2 = Some(!is_error);
                                }
                            }
                            Ok(Event::Message { session_id, message }) if session_id == sid && message.role == "assistant" && buf.is_empty() => {
                                // A text-only round that we did not stream (e.g. no deltas): mirror it.
                                let t = message.text();
                                if !t.is_empty() { buf = t; }
                            }
                            Ok(Event::ToolCall { session_id, id, name, .. }) if session_id == sid => {
                                let _ = tg.call("sendChatAction", json!({"chat_id": chat_id, "action": "typing"})).await;
                                tracing::debug!(tool = %name, "telegram turn tool call");
                                // Tool rounds start a fresh message so earlier text stays intact.
                                if message_id.is_some() && !buf.is_empty() { buf.clear(); message_id = None; last_sent.clear(); }
                                if thinking {
                                    progress.reopen();
                                    progress.steps.push((id, name, None));
                                }
                            }
                            Ok(Event::TurnEnd { session_id, .. }) if session_id == sid => finished = true,
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(_) => finished = true,
                        }
                    }
                    _ = ticker.tick() => {}
                }
                // The thinking message goes first, so it sits above the answer.
                if thinking {
                    progress.flush(&tg, chat_id, finished).await;
                }
                if finished {
                    break;
                }
                if !buf.is_empty() && last_edit.elapsed() >= EDIT_INTERVAL && buf != last_sent {
                    // An answer below the thinking message closes it.
                    progress.closed = progress.message_id.is_some();
                    let html = markdown_to_html(&buf);
                    let shown =
                        if buf.chars().count() > MAX_MESSAGE - 50 { clamp(&html) } else { format!("{html} ▍") };
                    match message_id {
                        None => {
                            if let Ok(id) = tg.send_message(chat_id, &shown).await {
                                message_id = Some(id);
                            }
                        }
                        Some(id) => {
                            let _ = tg.edit_message(chat_id, id, &shown).await;
                        }
                    }
                    last_sent = buf.clone();
                    last_edit = tokio::time::Instant::now();
                }
            }
            (buf, message_id)
        })
    };
    let result = agent::run_turn(state.clone(), session, text, TurnOptions::default()).await;
    let (streamed, message_id) = streamer.await.unwrap_or_default();
    match result {
        Ok(r) => {
            let final_text = if r.text.trim().is_empty() {
                r.error.clone().unwrap_or_else(|| "(no answer)".into())
            } else {
                r.text.clone()
            };
            let html = markdown_to_html(&final_text);
            match message_id {
                Some(id)
                    if streamed == final_text
                        || final_text.starts_with(&streamed)
                        || streamed.starts_with(&final_text) =>
                {
                    if html.chars().count() <= MAX_MESSAGE {
                        let _ = tg.edit_message(chat_id, id, &html).await;
                    } else {
                        let _ = tg.edit_message(chat_id, id, &clamp(&html)).await;
                        for chunk in split_chunks(&html, MAX_MESSAGE).into_iter().skip(1) {
                            let _ = tg.send_message(chat_id, &chunk).await;
                        }
                    }
                }
                Some(id) => {
                    let _ = tg.edit_message(chat_id, id, &markdown_to_html(&streamed)).await;
                    tg.send_long(chat_id, &html).await?;
                }
                None => tg.send_long(chat_id, &html).await?,
            }
            if let Some(e) = r.error {
                tg.send_message(chat_id, &format!("⚠️ {}", esc(&e))).await?;
            }
        }
        Err(e) => {
            tg.send_message(chat_id, &format!("⚠️ {}", esc(&format!("{e:#}")))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_renders_telegram_html() {
        let html = markdown_to_html(
            "# Title\nHello **bold** and `code` <tag>\n```\nlet x = 1 < 2;\n```\n- item [link](https://x.y)",
        );
        assert!(html.contains("<b>Title</b>"));
        assert!(html.contains("<b>bold</b>"));
        assert!(html.contains("<code>code</code>"));
        assert!(html.contains("&lt;tag&gt;"));
        assert!(html.contains("<pre><code>let x = 1 &lt; 2;\n</code></pre>"));
        assert!(html.contains("<a href=\"https://x.y\">link</a>"));
    }

    #[test]
    fn chunks_respect_limit_and_lines() {
        let text = "a\n".repeat(10);
        let chunks = split_chunks(&text, 6);
        assert!(chunks.iter().all(|c| c.chars().count() <= 6));
        assert_eq!(chunks.concat(), text);
        let long = "x".repeat(15);
        assert_eq!(split_chunks(&long, 4).len(), 4);
    }

    #[test]
    fn strip_html_roundtrip() {
        assert_eq!(strip_html("<b>a &lt; b</b>"), "a < b");
    }

    #[test]
    fn thinking_message_shows_reasoning_and_steps() {
        let mut p = Progress::default();
        assert_eq!(p.html(), "");
        p.thinking = "  The user wants <b> & a list.  ".into();
        p.steps.push(("c1".into(), "mcp__github__list".into(), Some(true)));
        p.steps.push(("c2".into(), "web_fetch".into(), None));
        assert_eq!(
            p.html(),
            "💭 <b>Thinking</b>\n<blockquote expandable>The user wants &lt;b&gt; &amp; a list.</blockquote>\n\
🔧 <code>mcp__github__list</code> ✓\n🔧 <code>web_fetch</code> …"
        );
        // Long reasoning keeps its latest part; only tool steps: no quote.
        p.thinking = "x".repeat(THINKING_CHARS + 10);
        assert!(p.html().contains("<blockquote expandable>…xxx"));
        assert!(p.html().chars().count() < MAX_MESSAGE);
        p.thinking.clear();
        assert!(p.html().starts_with("🔧 <code>mcp__github__list</code> ✓"));
        p.closed = true;
        p.reopen();
        assert_eq!(p.html(), "");
    }
}
