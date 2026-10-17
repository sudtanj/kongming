//! LLM providers behind one streaming interface. Two wire formats are
//! supported: OpenAI chat completions (covers OpenAI, OpenRouter, zilong-router,
//! Ollama, vLLM, ...) and the Anthropic Messages API.

pub mod anthropic;
pub mod openai;
pub mod sse;

use std::pin::Pin;

use anyhow::{bail, Result};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::Part;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON schema for the arguments object.
    pub parameters: Value,
}

/// A chat message in provider-neutral form. `parts` uses the same enum the
/// store persists, so history round-trips without conversion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    /// "system" | "user" | "assistant" | "tool"
    pub role: String,
    pub parts: Vec<Part>,
}

impl ChatMessage {
    pub fn text(role: &str, text: impl Into<String>) -> ChatMessage {
        ChatMessage { role: role.into(), parts: vec![Part::Text { text: text.into() }] }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    /// Anthropic effort / reasoning depth hint: "low" | "medium" | "high".
    pub effort: Option<String>,
}

/// Streaming events, normalized across providers.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// A complete tool call (arguments fully assembled).
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    Usage {
        prompt_tokens: i64,
        completion_tokens: i64,
    },
    /// "stop" | "tool_use" | "length" | "refusal" | other
    Done {
        stop_reason: String,
    },
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<LlmEvent>> + Send>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn stream(&self, req: ChatRequest) -> Result<EventStream>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// "openai" | "anthropic"
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub effort: Option<String>,
}

impl ProviderConfig {
    pub fn redacted(&self) -> Value {
        serde_json::json!({
            "kind": self.kind,
            "baseUrl": self.base_url,
            "apiKeySet": self.api_key.as_deref().is_some_and(|k| !k.is_empty()),
            "model": self.model,
            "maxTokens": self.max_tokens,
            "temperature": self.temperature,
            "effort": self.effort,
        })
    }
}

pub fn build(cfg: &ProviderConfig, http: reqwest::Client) -> Result<Box<dyn Provider>> {
    match cfg.kind.as_str() {
        "openai" | "" => Ok(Box::new(openai::OpenAi::new(http, cfg.base_url.clone(), cfg.api_key.clone()))),
        "anthropic" => Ok(Box::new(anthropic::Anthropic::new(http, cfg.base_url.clone(), cfg.api_key.clone()))),
        other => bail!("unknown provider kind '{other}' (use openai or anthropic)"),
    }
}

/// Rough token estimate (4 chars per token) used for context budgeting; cheap
/// and good enough to decide when to compact.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

pub fn estimate_message_tokens(m: &ChatMessage) -> usize {
    let mut n = 4;
    for p in &m.parts {
        n += match p {
            Part::Text { text } | Part::Thinking { text } => estimate_tokens(text),
            Part::ToolCall { name, args, .. } => estimate_tokens(name) + estimate_tokens(&args.to_string()),
            Part::ToolResult { content, .. } => estimate_tokens(content),
        };
    }
    n
}

/// Tool calls from an assistant message, in order.
pub fn tool_calls(m: &ChatMessage) -> Vec<(String, String, Value)> {
    m.parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolCall { id, name, args } => Some((id.clone(), name.clone(), args.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_is_monotonic() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        let m = ChatMessage::text("user", "hello world");
        assert!(estimate_message_tokens(&m) > 4);
    }

    #[test]
    fn build_rejects_unknown_kind() {
        let cfg = ProviderConfig { kind: "nope".into(), ..Default::default() };
        assert!(build(&cfg, reqwest::Client::new()).is_err());
        let cfg = ProviderConfig { kind: "openai".into(), ..Default::default() };
        assert_eq!(build(&cfg, reqwest::Client::new()).unwrap().name(), "openai");
    }

    #[test]
    fn redacted_hides_key() {
        let cfg = ProviderConfig { kind: "openai".into(), api_key: Some("sk-secret".into()), ..Default::default() };
        let v = cfg.redacted();
        assert_eq!(v["apiKeySet"], true);
        assert!(!v.to_string().contains("sk-secret"));
    }
}
