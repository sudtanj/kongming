//! Anthropic Messages API (raw HTTP; there is no official Rust SDK). Uses
//! adaptive thinking with summarized display, so the UI can show reasoning
//! summaries on current models.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::{sse, ChatRequest, EventStream, LlmEvent, Provider};
use crate::db::Part;

pub struct Anthropic {
    http: reqwest::Client,
    base: String,
    key: Option<String>,
}

impl Anthropic {
    pub fn new(http: reqwest::Client, base: Option<String>, key: Option<String>) -> Anthropic {
        let base = base.filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "https://api.anthropic.com".into());
        Anthropic { http, base: base.trim_end_matches('/').trim_end_matches("/v1").to_string(), key: key.filter(|k| !k.trim().is_empty()) }
    }

    /// Neutral history to Anthropic blocks. Tool results must come back in a
    /// `user` message directly after the assistant's `tool_use` blocks.
    pub fn messages(req: &ChatRequest) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for m in &req.messages {
            let (role, blocks): (&str, Vec<Value>) = match m.role.as_str() {
                "assistant" => (
                    "assistant",
                    m.parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } if !text.trim().is_empty() => Some(json!({"type": "text", "text": text})),
                            Part::ToolCall { id, name, args } => {
                                Some(json!({"type": "tool_use", "id": id, "name": name, "input": if args.is_object() { args.clone() } else { json!({}) }}))
                            }
                            _ => None,
                        })
                        .collect(),
                ),
                "tool" => (
                    "user",
                    m.parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::ToolResult { id, content, is_error, .. } => {
                                Some(json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error}))
                            }
                            _ => None,
                        })
                        .collect(),
                ),
                _ => {
                    let text: String = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    ("user", vec![json!({"type": "text", "text": if text.is_empty() { "(empty)".to_string() } else { text }})])
                }
            };
            if blocks.is_empty() {
                continue;
            }
            // Merge consecutive same-role messages (two tool results, or a tool
            // result followed by the next user text).
            if let Some(last) = out.last_mut() {
                if last["role"] == role {
                    if let Some(arr) = last["content"].as_array_mut() {
                        arr.extend(blocks);
                        continue;
                    }
                }
            }
            out.push(json!({"role": role, "content": blocks}));
        }
        // The API requires the first message to be from the user.
        if out.first().is_none_or(|m| m["role"] != "user") {
            out.insert(0, json!({"role": "user", "content": [{"type": "text", "text": "(conversation start)"}]}));
        }
        out
    }

    pub fn body(req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "max_tokens": if req.max_tokens > 0 { req.max_tokens } else { 16000 },
            "stream": true,
            "messages": Self::messages(req),
            "thinking": {"type": "adaptive", "display": "summarized"},
        });
        if !req.system.trim().is_empty() {
            body["system"] = json!([{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}]);
        }
        if let Some(e) = &req.effort {
            body["output_config"] = json!({"effort": e});
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters})).collect(),
            );
        }
        body
    }
}

#[derive(Default)]
struct State {
    /// index -> (id, name, partial json)
    calls: std::collections::BTreeMap<u64, (String, String, String)>,
    stop: String,
    prompt_tokens: i64,
}

#[async_trait::async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn stream(&self, req: ChatRequest) -> Result<EventStream> {
        let mut r = self
            .http
            .post(format!("{}/v1/messages", self.base))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&Self::body(&req));
        if let Some(k) = &self.key {
            r = r.header("x-api-key", k);
        }
        let resp = r.send().await.context("llm request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("llm HTTP {status}: {}", super::openai::short_error(&text));
        }
        let events = sse::events(resp.bytes_stream());
        let out = futures_util::stream::unfold((events.boxed(), State::default(), false), |(mut src, mut st, mut finished)| async move {
            loop {
                if finished {
                    return None;
                }
                let Some(item) = src.next().await else {
                    finished = true;
                    let reason = if st.stop.is_empty() { "stop".to_string() } else { st.stop.clone() };
                    return Some((vec![Ok(LlmEvent::Done { stop_reason: reason })], (src, st, finished)));
                };
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        finished = true;
                        return Some((vec![Err(e)], (src, st, finished)));
                    }
                };
                let v: Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
                let mut batch = Vec::new();
                match kind {
                    "message_start" => {
                        st.prompt_tokens = v.pointer("/message/usage/input_tokens").and_then(Value::as_i64).unwrap_or(0)
                            + v.pointer("/message/usage/cache_read_input_tokens").and_then(Value::as_i64).unwrap_or(0)
                            + v.pointer("/message/usage/cache_creation_input_tokens").and_then(Value::as_i64).unwrap_or(0);
                    }
                    "content_block_start" => {
                        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(b) = v.get("content_block") {
                            if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                                st.calls.insert(
                                    idx,
                                    (
                                        b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                                        b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                                        String::new(),
                                    ),
                                );
                            }
                        }
                    }
                    "content_block_delta" => {
                        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(d) = v.get("delta") {
                            match d.get("type").and_then(Value::as_str) {
                                Some("text_delta") => {
                                    if let Some(t) = d.get("text").and_then(Value::as_str) {
                                        batch.push(LlmEvent::TextDelta(t.to_string()));
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(t) = d.get("thinking").and_then(Value::as_str) {
                                        if !t.is_empty() {
                                            batch.push(LlmEvent::ThinkingDelta(t.to_string()));
                                        }
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let Some(p) = d.get("partial_json").and_then(Value::as_str) {
                                        if let Some(c) = st.calls.get_mut(&idx) {
                                            c.2.push_str(p);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "content_block_stop" => {
                        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some((id, name, raw)) = st.calls.remove(&idx) {
                            let args = if raw.trim().is_empty() { json!({}) } else { serde_json::from_str(&raw).unwrap_or_else(|_| json!({"_raw": raw})) };
                            batch.push(LlmEvent::ToolCall { id, name, args });
                        }
                    }
                    "message_delta" => {
                        if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                            st.stop = match r {
                                "tool_use" => "tool_use".into(),
                                "max_tokens" => "length".into(),
                                "refusal" => "refusal".into(),
                                _ => "stop".into(),
                            };
                        }
                        if let Some(o) = v.pointer("/usage/output_tokens").and_then(Value::as_i64) {
                            batch.push(LlmEvent::Usage { prompt_tokens: st.prompt_tokens, completion_tokens: o });
                        }
                    }
                    "message_stop" => {
                        finished = true;
                        let reason = if st.stop.is_empty() { "stop".to_string() } else { st.stop.clone() };
                        batch.push(LlmEvent::Done { stop_reason: reason });
                    }
                    "error" => {
                        finished = true;
                        let m = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error");
                        return Some((vec![Err(anyhow!("llm error: {m}"))], (src, st, finished)));
                    }
                    _ => {}
                }
                if batch.is_empty() {
                    continue;
                }
                return Some((batch.into_iter().map(Ok).collect::<Vec<_>>(), (src, st, finished)));
            }
        })
        .flat_map(futures_util::stream::iter);
        Ok(Box::pin(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;

    #[test]
    fn tool_results_become_user_blocks_and_merge() {
        let req = ChatRequest {
            model: "claude-opus-5-5".into(),
            system: "s".into(),
            messages: vec![
                ChatMessage::text("user", "hi"),
                ChatMessage { role: "assistant".into(), parts: vec![Part::ToolCall { id: "t1".into(), name: "f".into(), args: json!({}) }] },
                ChatMessage { role: "tool".into(), parts: vec![Part::ToolResult { id: "t1".into(), name: "f".into(), content: "r".into(), is_error: false }] },
                ChatMessage::text("user", "next"),
            ],
            tools: vec![],
            max_tokens: 0,
            temperature: None,
            effort: Some("high".into()),
        };
        let body = Anthropic::body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(body["max_tokens"], 16000);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn first_message_is_forced_to_user() {
        let req = ChatRequest { messages: vec![ChatMessage::text("assistant", "hello")], ..Default::default() };
        let msgs = Anthropic::messages(&req);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn base_url_strips_v1() {
        let a = Anthropic::new(reqwest::Client::new(), Some("https://proxy.example/v1/".into()), None);
        assert_eq!(a.base, "https://proxy.example");
    }
}
