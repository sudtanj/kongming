//! Telegram bot over long polling. Each chat maps to a session; `@handle`
//! switches agents; `/agent`, `/new`, `/agents`, `/skills`, `/status` are the
//! commands. Replies stream by editing one message at a throttled rate.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::agent::{self, TurnOptions};
use crate::state::{Event, Shared};

const EDIT_INTERVAL: Duration = Duration::from_millis(900);
const MAX_MESSAGE: usize = 4000;

#[derive(Clone)]
pub struct Telegram {
    http: reqwest::Client,
    token: String,
}

impl Telegram {
    pub fn new(http: reqwest::Client, token: String) -> Telegram {
        Telegram { http, token }
    }

    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    pub async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let r = self.http.post(self.url(method)).json(&body).timeout(Duration::from_secs(70)).send().await.context("telegram request failed")?;
        let v: Value = r.json().await.context("telegram reply was not JSON")?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(anyhow!("telegram {method}: {}", v.get("description").and_then(Value::as_str).unwrap_or("unknown error")));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<i64> {
        let text = clamp(text);
        let r = match self.call("sendMessage", json!({"chat_id": chat_id, "text": text, "parse_mode": "HTML", "disable_web_page_preview": true})).await {
            Ok(r) => r,
            // HTML parse failures: resend as plain text.
            Err(_) => self.call("sendMessage", json!({"chat_id": chat_id, "text": strip_html(&text), "disable_web_page_preview": true})).await?,
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

/// Run the polling loop until the process stops. Reconnects on errors.
pub async fn run(state: Shared) {
    let mut announced = false;
    loop {
        let Some(token) = state.telegram_token().await else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        let enabled = state.db.setting("telegram").await.ok().flatten().and_then(|v| v.get("enabled").and_then(Value::as_bool)).unwrap_or(true);
        if !enabled {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        }
        let tg = Telegram::new(state.http.clone(), token.clone());
        if !announced {
            match tg.call("getMe", json!({})).await {
                Ok(me) => {
                    tracing::info!(bot = %me.get("username").and_then(serde_json::Value::as_str).unwrap_or("?"), "telegram connected");
                    announced = true;
                    let _ = tg
                        .call(
                            "setMyCommands",
                            json!({"commands": [
                                {"command": "new", "description": "Start a new conversation"},
                                {"command": "agents", "description": "List agents"},
                                {"command": "agent", "description": "Switch agent: /agent <handle>"},
                                {"command": "skills", "description": "List skills"},
                                {"command": "status", "description": "Show status"},
                                {"command": "stop", "description": "Stop the current reply"},
                                {"command": "help", "description": "Help"}
                            ]}),
                        )
                        .await;
                }
                Err(e) => {
                    tracing::warn!("telegram getMe failed: {e:#}");
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
            }
        }
        let mut offset: i64 = state.db.setting("telegram.offset").await.ok().flatten().and_then(|v| v.as_i64()).unwrap_or(0);
        loop {
            // Token changed in settings: restart the outer loop.
            if state.telegram_token().await.as_deref() != Some(token.as_str()) {
                announced = false;
                break;
            }
            let updates = tg.call("getUpdates", json!({"offset": offset, "timeout": 50, "allowed_updates": ["message"]})).await;
            let updates = match updates {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("telegram poll failed: {e:#}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let Some(list) = updates.as_array() else { continue };
            for u in list {
                let id = u.get("update_id").and_then(Value::as_i64).unwrap_or(0);
                offset = offset.max(id + 1);
                if let Some(msg) = u.get("message") {
                    let st = state.clone();
                    let tg2 = tg.clone();
                    let msg = msg.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_message(st, tg2, msg).await {
                            tracing::warn!("telegram message failed: {e:#}");
                        }
                    });
                }
            }
            if !list.is_empty() {
                let _ = state.db.set_setting("telegram.offset", &json!(offset)).await;
            }
        }
    }
}

async fn session_for_chat(state: &Shared, chat_id: i64, fresh: bool) -> Result<crate::db::Session> {
    if !fresh {
        if let Some((sid, _)) = state.db.telegram_chat(chat_id).await? {
            if let Some(s) = state.db.session(&sid).await? {
                return Ok(s);
            }
        }
    }
    let agent = match state.db.telegram_chat(chat_id).await? {
        Some((_, aid)) => state.db.agent(&aid).await?.unwrap_or(agent::ensure_default_agent(state).await?),
        None => agent::ensure_default_agent(state).await?,
    };
    let s = state.db.create_session(&agent.id, "telegram", Some(&chat_id.to_string()), None, "").await?;
    state.db.set_telegram_chat(chat_id, &s.id, &agent.id).await?;
    Ok(s)
}

async fn handle_message(state: Shared, tg: Telegram, msg: Value) -> Result<()> {
    let chat_id = msg.pointer("/chat/id").and_then(Value::as_i64).ok_or_else(|| anyhow!("no chat id"))?;
    let user_id = msg.pointer("/from/id").and_then(Value::as_i64).unwrap_or(0);
    let allowed = state.telegram_allowed().await;
    if !allowed.contains(&user_id) {
        tracing::info!(user_id, "telegram message from a user not in the allowlist");
        tg.send_message(chat_id, &format!("This bot is private. Your Telegram user id is <code>{user_id}</code>; ask the owner to allow it.")).await?;
        return Ok(());
    }
    let text = msg.get("text").and_then(Value::as_str).or_else(|| msg.get("caption").and_then(Value::as_str)).unwrap_or("").trim().to_string();
    if text.is_empty() {
        tg.send_message(chat_id, "I can only read text for now.").await?;
        return Ok(());
    }

    if let Some(cmd) = text.strip_prefix('/') {
        let (name, arg) = cmd.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
        let name = name.split('@').next().unwrap_or(name);
        match name {
            "start" | "help" => {
                tg.send_message(chat_id, "Hi! Just type to talk to the current agent.\nMention <b>@handle</b> to talk to another agent.\n/new – new conversation\n/agents – list agents\n/agent &lt;handle&gt; – switch agent\n/skills – list skills\n/status – status\n/stop – stop the current reply").await?;
            }
            "new" => {
                let s = session_for_chat(&state, chat_id, true).await?;
                let a = state.db.agent(&s.agent_id).await?;
                tg.send_message(chat_id, &format!("New conversation with <b>@{}</b>.", a.map(|a| a.handle).unwrap_or_default())).await?;
            }
            "agents" => {
                let agents = state.db.agents().await?;
                let cur = session_for_chat(&state, chat_id, false).await?.agent_id;
                let lines: Vec<String> = agents
                    .iter()
                    .map(|a| format!("{}<b>@{}</b> – {}{}", if a.id == cur { "▶ " } else { "" }, a.handle, esc(&a.role), a.manager_id.as_ref().and_then(|m| agents.iter().find(|x| &x.id == m)).map(|m| format!(" (reports to @{})", m.handle)).unwrap_or_default()))
                    .collect();
                tg.send_message(chat_id, &lines.join("\n")).await?;
            }
            "agent" => {
                let Some(a) = state.db.agent_by_handle(arg).await? else {
                    tg.send_message(chat_id, &format!("No agent @{}", esc(arg))).await?;
                    return Ok(());
                };
                let s = session_for_chat(&state, chat_id, false).await?;
                state.db.set_session_agent(&s.id, &a.id).await?;
                state.db.set_telegram_chat(chat_id, &s.id, &a.id).await?;
                tg.send_message(chat_id, &format!("Now talking to <b>@{}</b> ({}).", a.handle, esc(&a.role))).await?;
            }
            "skills" => {
                let skills = state.db.skills().await?;
                if skills.is_empty() {
                    tg.send_message(chat_id, "No skills yet.").await?;
                } else {
                    let lines: Vec<String> = skills.iter().map(|k| format!("{} <b>{}</b> – {}", if k.enabled { "•" } else { "○" }, k.name, esc(&k.description))).collect();
                    tg.send_long(chat_id, &lines.join("\n")).await?;
                }
            }
            "status" => {
                let s = session_for_chat(&state, chat_id, false).await?;
                let a = state.db.agent(&s.agent_id).await?;
                let n = state.db.message_count(&s.id).await.unwrap_or(0);
                let cfg = state.provider_config().await?;
                tg.send_message(chat_id, &format!("Agent: <b>@{}</b>\nSession: <code>{}</code> ({n} messages)\nModel: {} via {}\nBusy: {}", a.map(|a| a.handle).unwrap_or_default(), s.id, esc(&cfg.model), cfg.kind, state.is_running(&s.id))).await?;
            }
            "stop" => {
                let s = session_for_chat(&state, chat_id, false).await?;
                tg.send_message(chat_id, if state.cancel_turn(&s.id) { "Stopping." } else { "Nothing is running." }).await?;
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
    // Remember a sticky agent switch for this chat.
    let mentions = agent::mention::parse(&text);
    if let Some(h) = mentions.handles.first() {
        if let Some(a) = state.db.agent_by_handle(h).await? {
            state.db.set_telegram_chat(chat_id, &session.id, &a.id).await?;
        }
    }

    tg.typing(chat_id).await;
    let session_id = session.id.clone();
    let mut events = state.subscribe();
    let streamer = {
        let tg = tg.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut message_id: Option<i64> = None;
            let mut last_edit = tokio::time::Instant::now() - EDIT_INTERVAL;
            let mut last_sent = String::new();
            let mut ticker = tokio::time::interval(Duration::from_millis(300));
            loop {
                tokio::select! {
                    ev = events.recv() => {
                        match ev {
                            Ok(Event::Delta { session_id, text, .. }) if session_id == sid => buf.push_str(&text),
                            Ok(Event::Message { session_id, message }) if session_id == sid && message.role == "assistant" && buf.is_empty() => {
                                // A text-only round that we did not stream (e.g. no deltas): mirror it.
                                let t = message.text();
                                if !t.is_empty() { buf = t; }
                            }
                            Ok(Event::ToolCall { session_id, name, .. }) if session_id == sid => {
                                let _ = tg.call("sendChatAction", json!({"chat_id": chat_id, "action": "typing"})).await;
                                tracing::debug!(tool = %name, "telegram turn tool call");
                                // Tool rounds start a fresh message so earlier text stays intact.
                                if message_id.is_some() && !buf.is_empty() { buf.clear(); message_id = None; last_sent.clear(); }
                            }
                            Ok(Event::TurnEnd { session_id, .. }) if session_id == sid => break,
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(_) => break,
                        }
                    }
                    _ = ticker.tick() => {}
                }
                if !buf.is_empty() && last_edit.elapsed() >= EDIT_INTERVAL && buf != last_sent {
                    let html = markdown_to_html(&buf);
                    let shown = if buf.chars().count() > MAX_MESSAGE - 50 { clamp(&html) } else { format!("{html} ▍") };
                    match message_id {
                        None => {
                            if let Ok(id) = tg.send_message(chat_id, &shown).await { message_id = Some(id); }
                        }
                        Some(id) => { let _ = tg.edit_message(chat_id, id, &shown).await; }
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
            let final_text = if r.text.trim().is_empty() { r.error.clone().unwrap_or_else(|| "(no answer)".into()) } else { r.text.clone() };
            let html = markdown_to_html(&final_text);
            match message_id {
                Some(id) if streamed == final_text || final_text.starts_with(&streamed) || streamed.starts_with(&final_text) => {
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
        let html = markdown_to_html("# Title\nHello **bold** and `code` <tag>\n```\nlet x = 1 < 2;\n```\n- item [link](https://x.y)");
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
}
