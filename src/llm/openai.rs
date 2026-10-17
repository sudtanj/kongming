//! OpenAI chat-completions wire format with streaming tool calls. Works with
//! any compatible endpoint (OpenAI, OpenRouter, zilong-router, Ollama, vLLM).

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::{sse, ChatRequest, EventStream, LlmEvent, Provider};
use crate::db::Part;

pub struct OpenAi {
    http: reqwest::Client,
    base: String,
    key: Option<String>,
}

impl OpenAi {
    pub fn new(http: reqwest::Client, base: Option<String>, key: Option<String>) -> OpenAi {
        let base = base.filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "https://api.openai.com/v1".into());
        OpenAi { http, base: base.trim_end_matches('/').to_string(), key: key.filter(|k| !k.trim().is_empty()) }
    }

    /// Convert neutral messages into the OpenAI shape. Tool results become
    /// `role: tool` messages; assistant tool calls carry `tool_calls`.
    pub fn messages(req: &ChatRequest) -> Vec<Value> {
        let mut out = Vec::new();
        if !req.system.trim().is_empty() {
            out.push(json!({"role": "system", "content": req.system}));
        }
        for m in &req.messages {
            match m.role.as_str() {
                "assistant" => {
                    let mut text = String::new();
                    let mut calls = Vec::new();
                    for p in &m.parts {
                        match p {
                            Part::Text { text: t } => text.push_str(t),
                            Part::ToolCall { id, name, args } => calls.push(json!({
                                "id": id, "type": "function",
                                "function": {"name": name, "arguments": args.to_string()}
                            })),
                            _ => {}
                        }
                    }
                    let mut msg = json!({"role": "assistant"});
                    msg["content"] = if text.is_empty() { Value::Null } else { Value::String(text) };
                    if !calls.is_empty() {
                        msg["tool_calls"] = Value::Array(calls);
                    }
                    out.push(msg);
                }
                "tool" => {
                    for p in &m.parts {
                        if let Part::ToolResult { id, content, .. } = p {
                            out.push(json!({"role": "tool", "tool_call_id": id, "content": content}));
                        }
                    }
                }
                role => {
                    let text: String = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push(json!({"role": role, "content": text}));
                }
            }
        }
        out
    }

    pub fn body(req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "messages": Self::messages(req),
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if req.max_tokens > 0 {
            body["max_tokens"] = json!(req.max_tokens);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
                    .collect(),
            );
            body["tool_choice"] = json!("auto");
        }
        body
    }
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    args: String,
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn stream(&self, req: ChatRequest) -> Result<EventStream> {
        let mut r = self.http.post(format!("{}/chat/completions", self.base)).json(&Self::body(&req));
        if let Some(k) = &self.key {
            r = r.bearer_auth(k);
        }
        let resp = r.send().await.context("llm request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("llm HTTP {status}: {}", short_error(&text));
        }
        let events = sse::events(resp.bytes_stream());
        let state = (BTreeMap::<u64, PendingCall>::new(), String::new(), false);
        let out = futures_util::stream::unfold((events.boxed(), state, false), |(mut src, mut state, mut finished)| async move {
            loop {
                if finished {
                    return None;
                }
                let Some(item) = src.next().await else {
                    finished = true;
                    // Stream ended without a finish marker: still flush tool calls and a Done.
                    let mut batch = flush_calls(&mut state.0);
                    batch.push(LlmEvent::Done { stop_reason: if state.1.is_empty() { "stop".into() } else { state.1.clone() } });
                    return Some((batch.into_iter().map(Ok).collect::<Vec<_>>(), (src, state, finished)));
                };
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        finished = true;
                        return Some((vec![Err(e)], (src, state, finished)));
                    }
                };
                if ev.data.trim() == "[DONE]" {
                    finished = true;
                    let mut batch = flush_calls(&mut state.0);
                    let reason = if state.1.is_empty() { "stop".to_string() } else { state.1.clone() };
                    batch.push(LlmEvent::Done { stop_reason: reason });
                    return Some((batch.into_iter().map(Ok).collect(), (src, state, finished)));
                }
                let v: Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(err) = v.get("error") {
                    finished = true;
                    return Some((vec![Err(anyhow!("llm error: {}", short_error(&err.to_string())))], (src, state, finished)));
                }
                let mut batch = Vec::new();
                if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                    batch.push(LlmEvent::Usage {
                        prompt_tokens: u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
                        completion_tokens: u.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
                    });
                }
                if let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| c.first()) {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(t) = delta.get("content").and_then(Value::as_str) {
                            if !t.is_empty() {
                                batch.push(LlmEvent::TextDelta(t.to_string()));
                            }
                        }
                        for key in ["reasoning_content", "reasoning"] {
                            if let Some(t) = delta.get(key).and_then(Value::as_str) {
                                if !t.is_empty() {
                                    batch.push(LlmEvent::ThinkingDelta(t.to_string()));
                                }
                            }
                        }
                        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                            for c in calls {
                                let idx = c.get("index").and_then(Value::as_u64).unwrap_or(0);
                                let slot = state.0.entry(idx).or_default();
                                if let Some(id) = c.get("id").and_then(Value::as_str) {
                                    slot.id = id.to_string();
                                }
                                if let Some(f) = c.get("function") {
                                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                                        slot.name.push_str(n);
                                    }
                                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                                        slot.args.push_str(a);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        state.1 = match reason {
                            "tool_calls" | "function_call" => "tool_use".into(),
                            "length" => "length".into(),
                            "content_filter" => "refusal".into(),
                            _ => "stop".into(),
                        };
                        if state.1 == "tool_use" || !state.0.is_empty() {
                            batch.extend(flush_calls(&mut state.0));
                        }
                    }
                }
                if batch.is_empty() {
                    continue;
                }
                return Some((batch.into_iter().map(Ok).collect(), (src, state, finished)));
            }
        })
        .flat_map(futures_util::stream::iter);
        Ok(Box::pin(out))
    }
}

fn flush_calls(calls: &mut BTreeMap<u64, PendingCall>) -> Vec<LlmEvent> {
    let drained: Vec<PendingCall> = std::mem::take(calls).into_values().collect();
    drained
        .into_iter()
        .filter(|c| !c.name.is_empty())
        .map(|c| {
            let args = if c.args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&c.args).unwrap_or_else(|_| json!({"_raw": c.args}))
            };
            let id = if c.id.is_empty() { format!("call_{}", crate::db::new_id()) } else { c.id };
            LlmEvent::ToolCall { id, name: c.name, args }
        })
        .collect()
}

pub fn short_error(text: &str) -> String {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        if let Some(m) = v.pointer("/error/message").and_then(Value::as_str).or_else(|| v.get("message").and_then(Value::as_str)) {
            return m.chars().take(400).collect();
        }
    }
    t.chars().take(400).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ToolSpec};

    #[test]
    fn converts_history_with_tool_calls() {
        let req = ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![
                ChatMessage::text("user", "hi"),
                ChatMessage {
                    role: "assistant".into(),
                    parts: vec![Part::ToolCall { id: "c1".into(), name: "f".into(), args: json!({"x": 1}) }],
                },
                ChatMessage {
                    role: "tool".into(),
                    parts: vec![Part::ToolResult { id: "c1".into(), name: "f".into(), content: "ok".into(), is_error: false }],
                },
            ],
            tools: vec![ToolSpec { name: "f".into(), description: "d".into(), parameters: json!({"type": "object"}) }],
            max_tokens: 100,
            temperature: None,
            effort: None,
        };
        let body = OpenAi::body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["arguments"], "{\"x\":1}");
        assert_eq!(msgs[3]["tool_call_id"], "c1");
        assert_eq!(body["tools"][0]["function"]["name"], "f");
        assert_eq!(body["max_tokens"], 100);
    }

    #[test]
    fn flushes_calls_with_invalid_json_as_raw() {
        let mut m = BTreeMap::new();
        m.insert(0, PendingCall { id: "a".into(), name: "f".into(), args: "{bad".into() });
        let evs = flush_calls(&mut m);
        match &evs[0] {
            LlmEvent::ToolCall { args, .. } => assert_eq!(args["_raw"], "{bad"),
            _ => panic!(),
        }
        assert!(m.is_empty());
    }

    #[test]
    fn short_error_extracts_message() {
        assert_eq!(short_error(r#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(short_error("plain"), "plain");
    }
}
