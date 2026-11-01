//! OpenAI chat-completions wire format with streaming tool calls. Works with
//! any compatible endpoint (OpenAI, OpenRouter, zilong-router, Ollama, vLLM).

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::think::{Piece, ThinkScrubber};
use super::{repair, sse, ChatRequest, EventStream, LlmEvent, Provider};
use crate::db::Part;

pub struct OpenAi {
    http: reqwest::Client,
    base: String,
    key: Option<String>,
    /// The server refused `tool_choice` once (vLLM started without a tool
    /// parser): leave it out for as long as this provider lives.
    no_tool_choice: AtomicBool,
    /// Ask for one tool call per reply (small models), until the server
    /// refuses `parallel_tool_calls`.
    serial_tools: AtomicBool,
    /// The context window to ask an Ollama server for.
    num_ctx: Option<u32>,
}

impl OpenAi {
    pub fn new(http: reqwest::Client, base: Option<String>, key: Option<String>) -> OpenAi {
        let base = base.filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "https://api.openai.com/v1".into());
        OpenAi {
            http,
            base: base.trim_end_matches('/').to_string(),
            key: key.filter(|k| !k.trim().is_empty()),
            no_tool_choice: AtomicBool::new(false),
            serial_tools: AtomicBool::new(false),
            num_ctx: None,
        }
    }

    /// Settings from the model profile: one tool call per reply for a small
    /// model, and the context window for Ollama.
    pub fn tuned(self, serial_tools: bool, num_ctx: Option<u32>) -> OpenAi {
        OpenAi { serial_tools: AtomicBool::new(serial_tools), num_ctx, ..self }
    }

    /// `body` with what this server takes beyond the standard: no parallel
    /// tool calls, Ollama's `num_ctx` (top level and in `options`), and no
    /// `tool_choice` once refused.
    pub fn request_body(&self, req: &ChatRequest) -> Value {
        let mut body = Self::body(req);
        if body.get("tools").is_some() {
            if self.serial_tools.load(Ordering::Relaxed) {
                body["parallel_tool_calls"] = json!(false);
            }
            if self.no_tool_choice.load(Ordering::Relaxed) {
                if let Some(o) = body.as_object_mut() {
                    o.remove("tool_choice");
                }
            }
        }
        if let Some(n) = self.num_ctx {
            body["num_ctx"] = json!(n);
            body["options"] = json!({"num_ctx": n});
        }
        body
    }

    /// Drop the field a 400 blames, when it is one this server can do without
    /// (`parallel_tool_calls`, `tool_choice`), and remember that. Whether a
    /// field was dropped and the request is worth sending again.
    fn drop_refused(&self, body: &mut Value, error: &str) -> bool {
        let (field, flag, on) = if body.get("parallel_tool_calls").is_some() && rejects_parallel_calls(error) {
            ("parallel_tool_calls", &self.serial_tools, false)
        } else if body.get("tool_choice").is_some() && rejects_tool_choice(error) {
            ("tool_choice", &self.no_tool_choice, true)
        } else {
            return false;
        };
        tracing::info!("the model server refused {field}; sending requests without it");
        flag.store(on, Ordering::Relaxed);
        if let Some(o) = body.as_object_mut() {
            o.remove(field);
        }
        true
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

    async fn send(&self, body: &Value) -> Result<reqwest::Response> {
        let mut r = self.http.post(format!("{}/chat/completions", self.base)).json(body);
        if let Some(k) = &self.key {
            r = r.bearer_auth(k);
        }
        r.send().await.context("llm request failed")
    }
}

/// A 400 that blames `tool_choice`, e.g. vLLM's "\"auto\" tool choice
/// requires --enable-auto-tool-choice and --tool-call-parser to be set".
fn rejects_tool_choice(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    ["tool_choice", "tool choice", "tool-call-parser", "tool parser", "tool_call_parser"].iter().any(|p| b.contains(p))
}

/// A 400 that blames `parallel_tool_calls` (servers that do not know it).
fn rejects_parallel_calls(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("parallel_tool_calls") || b.contains("parallel tool calls")
}

#[derive(Default)]
struct PendingCall {
    index: u64,
    id: String,
    name: String,
    args: String,
}

/// What a stream has received and not handed on yet.
#[derive(Default)]
struct StreamState {
    /// Tool calls being assembled, in the order they started.
    calls: Vec<PendingCall>,
    stop: String,
    think: ThinkScrubber,
}

impl StreamState {
    /// One entry of a `tool_calls` delta.
    fn tool_delta(&mut self, c: &Value) {
        let index = c.get("index").and_then(Value::as_u64).unwrap_or(0);
        let id = match c.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        // A new id at an index already in use starts another call: Ollama
        // sends every call at index 0.
        let i = match self.calls.iter().rposition(|p| p.index == index) {
            Some(i) if id.is_empty() || self.calls[i].id.is_empty() || self.calls[i].id == id => i,
            _ => {
                self.calls.push(PendingCall { index, ..Default::default() });
                self.calls.len() - 1
            }
        };
        let slot = &mut self.calls[i];
        if !id.is_empty() {
            slot.id = id;
        }
        let Some(f) = c.get("function") else { return };
        // Some servers repeat the whole name in every chunk.
        if let Some(n) = f.get("name").and_then(Value::as_str).filter(|n| slot.name != *n) {
            slot.name.push_str(n);
        }
        match f.get("arguments") {
            Some(Value::String(a)) => slot.args.push_str(a),
            // Sent already parsed, as some Ollama versions do.
            Some(v @ (Value::Object(_) | Value::Array(_))) => slot.args = v.to_string(),
            _ => {}
        }
    }

    /// End of the stream: held-back text, the pending calls and the Done.
    fn finish(&mut self) -> Vec<LlmEvent> {
        let mut batch: Vec<LlmEvent> = self.think.finish().into_iter().map(piece_event).collect();
        batch.extend(flush_calls(&mut self.calls));
        let stop_reason = if self.stop.is_empty() { "stop".to_string() } else { self.stop.clone() };
        batch.push(LlmEvent::Done { stop_reason });
        batch
    }
}

fn piece_event(p: Piece) -> LlmEvent {
    match p {
        Piece::Text(t) => LlmEvent::TextDelta(t),
        Piece::Thinking(t) => LlmEvent::ThinkingDelta(t),
    }
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn stream(&self, req: ChatRequest) -> Result<EventStream> {
        let mut body = self.request_body(&req);
        let mut resp = self.send(&body).await?;
        // A field the server does not take: drop it and ask again (at most
        // once per field).
        for _ in 0..2 {
            let status = resp.status();
            if status != reqwest::StatusCode::BAD_REQUEST {
                break;
            }
            let text = resp.text().await.unwrap_or_default();
            if !self.drop_refused(&mut body, &text) {
                bail!("llm HTTP {status}: {}", short_error(&text));
            }
            resp = self.send(&body).await?;
        }
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("llm HTTP {status}: {}", short_error(&text));
        }
        let events = sse::events(resp.bytes_stream());
        let out = futures_util::stream::unfold(
            (events.boxed(), StreamState::default(), false),
            |(mut src, mut state, mut finished)| async move {
                loop {
                    if finished {
                        return None;
                    }
                    let Some(item) = src.next().await else {
                        finished = true;
                        // Stream ended without a finish marker: still flush tool calls and a Done.
                        let batch = state.finish();
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
                        let batch = state.finish();
                        return Some((batch.into_iter().map(Ok).collect(), (src, state, finished)));
                    }
                    let v: Value = match serde_json::from_str(&ev.data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Some(err) = v.get("error") {
                        finished = true;
                        return Some((
                            vec![Err(anyhow!("llm error: {}", short_error(&err.to_string())))],
                            (src, state, finished),
                        ));
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
                                // Inline <think> blocks become reasoning.
                                batch.extend(state.think.feed(t).into_iter().map(piece_event));
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
                                    state.tool_delta(c);
                                }
                            }
                        }
                        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                            state.stop = match reason {
                                "tool_calls" | "function_call" => "tool_use".into(),
                                "length" => "length".into(),
                                "content_filter" => "refusal".into(),
                                _ => "stop".into(),
                            };
                            if state.stop == "tool_use" || !state.calls.is_empty() {
                                batch.extend(flush_calls(&mut state.calls));
                            }
                        }
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    return Some((batch.into_iter().map(Ok).collect(), (src, state, finished)));
                }
            },
        )
        .flat_map(futures_util::stream::iter);
        Ok(Box::pin(out))
    }
}

fn flush_calls(calls: &mut Vec<PendingCall>) -> Vec<LlmEvent> {
    std::mem::take(calls)
        .into_iter()
        .filter(|c| !c.name.is_empty())
        .map(|c| {
            let id = if c.id.is_empty() { format!("call_{}", crate::db::new_id()) } else { c.id };
            LlmEvent::ToolCall { id, name: c.name, args: repair::parse_args(&c.args) }
        })
        .collect()
}

pub fn short_error(text: &str) -> String {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        if let Some(m) =
            v.pointer("/error/message").and_then(Value::as_str).or_else(|| v.get("message").and_then(Value::as_str))
        {
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
                    parts: vec![Part::ToolResult {
                        id: "c1".into(),
                        name: "f".into(),
                        content: "ok".into(),
                        is_error: false,
                    }],
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

    fn call(state: &mut StreamState, delta: Value) {
        state.tool_delta(&delta);
    }

    fn flushed(state: &mut StreamState) -> Vec<(String, String, Value)> {
        flush_calls(&mut state.calls)
            .into_iter()
            .filter_map(|e| match e {
                LlmEvent::ToolCall { id, name, args } => Some((id, name, args)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn flushes_calls_with_invalid_json_as_raw() {
        let mut calls = vec![PendingCall { index: 0, id: "a".into(), name: "f".into(), args: "{bad".into() }];
        let evs = flush_calls(&mut calls);
        match &evs[0] {
            LlmEvent::ToolCall { args, .. } => assert_eq!(args["_raw"], "{bad"),
            _ => panic!(),
        }
        assert!(calls.is_empty());
    }

    #[test]
    fn tool_call_deltas_from_loose_servers_are_assembled() {
        let mut st = StreamState::default();
        // Streamed in pieces, with trailing damage the repair fixes.
        call(&mut st, json!({"index": 0, "id": "a", "function": {"name": "f", "arguments": "{\"x\": "}}));
        call(&mut st, json!({"index": 0, "function": {"arguments": "1,"}}));
        // The whole name again in a later chunk, arguments as an object, a numeric id.
        call(&mut st, json!({"index": 1, "id": 7, "function": {"name": "g", "arguments": {"y": 2}}}));
        call(&mut st, json!({"index": 1, "function": {"name": "g"}}));
        // Ollama: a second call at index 0 with its own id.
        call(&mut st, json!({"index": 0, "id": "b", "function": {"name": "h", "arguments": "{}"}}));
        let calls = flushed(&mut st);
        assert_eq!(
            calls,
            vec![
                ("a".to_string(), "f".to_string(), json!({"x": 1})),
                ("7".to_string(), "g".to_string(), json!({"y": 2})),
                ("b".to_string(), "h".to_string(), json!({})),
            ]
        );
    }

    #[test]
    fn inline_think_blocks_become_reasoning() {
        let mut st = StreamState::default();
        let mut evs: Vec<LlmEvent> = Vec::new();
        for chunk in ["<think>plan", "</think>", "Hi"] {
            evs.extend(st.think.feed(chunk).into_iter().map(piece_event));
        }
        evs.extend(st.finish());
        assert_eq!(
            evs,
            vec![
                LlmEvent::ThinkingDelta("plan".into()),
                LlmEvent::TextDelta("Hi".into()),
                LlmEvent::Done { stop_reason: "stop".into() },
            ]
        );
    }

    #[test]
    fn a_refused_tool_choice_is_recognized() {
        let vllm = r#""auto" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set"#;
        assert!(rejects_tool_choice(vllm));
        assert!(!rejects_tool_choice(r#"{"error": {"message": "model not found"}}"#));
    }

    #[test]
    fn small_model_requests_ask_for_serial_calls_and_the_window() {
        let tool = ToolSpec { name: "f".into(), description: "d".into(), parameters: json!({"type": "object"}) };
        let req = ChatRequest { model: "m".into(), tools: vec![tool], ..Default::default() };
        let api = OpenAi::new(reqwest::Client::new(), None, None).tuned(true, Some(8192));
        let mut body = api.request_body(&req);
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(body["num_ctx"], json!(8192));
        assert_eq!(body["options"]["num_ctx"], json!(8192));
        // Refused once: dropped from then on; other 400s are left alone.
        assert!(!api.drop_refused(&mut body, "model not found"));
        assert!(api.drop_refused(&mut body, "Unrecognized request argument supplied: parallel_tool_calls"));
        assert!(body.get("parallel_tool_calls").is_none() && body.get("tool_choice").is_some());
        assert!(api.request_body(&req).get("parallel_tool_calls").is_none());
        // No tools, no tool fields; a normal provider adds nothing.
        let plain = ChatRequest { model: "m".into(), ..Default::default() };
        assert!(api.request_body(&plain).get("parallel_tool_calls").is_none());
        let normal = OpenAi::new(reqwest::Client::new(), None, None).request_body(&req);
        assert!(normal.get("parallel_tool_calls").is_none() && normal.get("num_ctx").is_none());
    }

    #[test]
    fn short_error_extracts_message() {
        assert_eq!(short_error(r#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(short_error("plain"), "plain");
    }
}
