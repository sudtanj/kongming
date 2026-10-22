//! Row types shared by the store, the agent runtime and the web API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::turso::Row;

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..20].to_string()
}

fn json_list(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
}

/// An agent: a persona with a role in the team. `manager_id` makes the structure:
/// a manager may delegate to the agents that report to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    /// `@handle` used in mentions, lowercase, `[a-z0-9_-]`.
    pub handle: String,
    pub name: String,
    /// Job label, e.g. "Chief of staff", "Researcher" (grokbot's "role").
    pub role: String,
    /// What the agent is responsible for; shown to other agents so they can route work.
    pub description: String,
    pub system_prompt: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub manager_id: Option<String>,
    /// Allowed built-in tool names; `None` = all.
    pub tools: Option<Vec<String>>,
    /// Allowed MCP server ids; `None` = all enabled servers.
    pub mcp_servers: Option<Vec<String>>,
    /// Skills always loaded into this agent's prompt.
    pub pinned_skills: Vec<String>,
    /// Whether this agent may create/update skills and memories on its own.
    pub self_improve: bool,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Agent {
    pub fn from_row(r: &Row<'_>) -> Agent {
        Agent {
            id: r.str("id"),
            handle: r.str("handle"),
            name: r.str("name"),
            role: r.str("role"),
            description: r.str("description"),
            system_prompt: r.str("system_prompt"),
            model: r.opt_str("model"),
            provider: r.opt_str("provider"),
            manager_id: r.opt_str("manager_id"),
            tools: json_list(&r.json("tools_json")),
            mcp_servers: json_list(&r.json("mcp_json")),
            pinned_skills: json_list(&r.json("skills_json")).unwrap_or_default(),
            self_improve: r.bool("self_improve"),
            is_default: r.bool("is_default"),
            created_at: r.i64("created_at"),
            updated_at: r.i64("updated_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub agent_id: String,
    /// "web" | "telegram" | "delegation" | "cron"
    pub channel: String,
    pub channel_ref: Option<String>,
    pub parent_session_id: Option<String>,
    /// Rolling summary of compacted history, and the seq it covers (exclusive).
    pub summary: Option<String>,
    pub summary_upto: i64,
    pub archived: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Session {
    pub fn from_row(r: &Row<'_>) -> Session {
        Session {
            id: r.str("id"),
            title: r.str("title"),
            agent_id: r.str("agent_id"),
            channel: r.str("channel"),
            channel_ref: r.opt_str("channel_ref"),
            parent_session_id: r.opt_str("parent_session_id"),
            summary: r.opt_str("summary"),
            summary_upto: r.i64("summary_upto"),
            archived: r.bool("archived"),
            created_at: r.i64("created_at"),
            updated_at: r.i64("updated_at"),
        }
    }
}

/// One part of a message. Text, a tool call the assistant made, the result the
/// runtime produced, or (summarized) reasoning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Part {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        args: Value,
    },
    ToolResult {
        id: String,
        name: String,
        content: String,
        #[serde(default, alias = "is_error")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub seq: i64,
    /// "user" | "assistant" | "tool" | "system"
    pub role: String,
    pub parts: Vec<Part>,
    pub agent_id: Option<String>,
    pub created_at: i64,
}

impl Message {
    pub fn from_row(r: &Row<'_>) -> Message {
        Message {
            id: r.str("id"),
            session_id: r.str("session_id"),
            seq: r.i64("seq"),
            role: r.str("role"),
            parts: serde_json::from_value(r.json("content")).unwrap_or_default(),
            agent_id: r.opt_str("agent_id"),
            created_at: r.i64("created_at"),
        }
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text { text } = p {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Markdown body (SKILL.md without the frontmatter).
    pub body: String,
    pub tags: Vec<String>,
    pub version: i64,
    /// "user" | "agent" | "seed"
    pub source: String,
    pub author_agent_id: Option<String>,
    pub enabled: bool,
    pub uses: i64,
    pub last_used_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Skill {
    pub fn from_row(r: &Row<'_>) -> Skill {
        Skill {
            id: r.str("id"),
            name: r.str("name"),
            description: r.str("description"),
            body: r.str("body"),
            tags: json_list(&r.json("tags")).unwrap_or_default(),
            version: r.i64("version"),
            source: r.str("source"),
            author_agent_id: r.opt_str("author_agent_id"),
            enabled: r.bool("enabled"),
            uses: r.i64("uses"),
            last_used_at: r.opt_i64("last_used_at"),
            created_at: r.i64("created_at"),
            updated_at: r.i64("updated_at"),
        }
    }

    /// Render as a SKILL.md document (hermes/agentskills format).
    pub fn to_markdown(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\nversion: {}\ntags: [{}]\n---\n\n{}",
            self.name,
            self.description.replace('\n', " "),
            self.version,
            self.tags.join(", "),
            self.body
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    /// `None` = shared by every agent.
    pub agent_id: Option<String>,
    /// "fact" | "preference" | "lesson" | "project" | "interest" | "watch"
    pub kind: String,
    pub content: String,
    pub keywords: Vec<String>,
    pub importance: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Memory {
    pub fn from_row(r: &Row<'_>) -> Memory {
        Memory {
            id: r.str("id"),
            agent_id: r.opt_str("agent_id"),
            kind: r.str("kind"),
            content: r.str("content"),
            keywords: json_list(&r.json("keywords")).unwrap_or_default(),
            importance: r.i64("importance"),
            created_at: r.i64("created_at"),
            updated_at: r.i64("updated_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct OAuthState {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub registration_endpoint: Option<String>,
    pub scope: Option<String>,
    pub resource: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    /// Unix ms.
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub url: String,
    /// "http" (streamable HTTP) | "sse" (legacy HTTP+SSE)
    pub transport: String,
    /// "none" | "token" | "oauth"
    pub auth_type: String,
    #[serde(skip_serializing)]
    pub token: Option<String>,
    #[serde(skip_serializing)]
    pub oauth: OAuthState,
    pub headers: Value,
    pub enabled: bool,
    /// "connected" | "error" | "unauthorized" | "disabled" | "unknown"
    pub status: String,
    pub last_error: Option<String>,
    pub tools: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

impl McpServer {
    pub fn from_row(r: &Row<'_>) -> McpServer {
        McpServer {
            id: r.str("id"),
            name: r.str("name"),
            url: r.str("url"),
            transport: r.str("transport"),
            auth_type: r.str("auth_type"),
            token: r.opt_str("token"),
            oauth: serde_json::from_value(r.json("oauth_json")).unwrap_or_default(),
            headers: {
                let h = r.json("headers_json");
                if h.is_object() {
                    h
                } else {
                    Value::Object(Default::default())
                }
            },
            enabled: r.bool("enabled"),
            status: r.str("status"),
            last_error: r.opt_str("last_error"),
            tools: {
                let t = r.json("tools_json");
                if t.is_array() {
                    t
                } else {
                    Value::Array(vec![])
                }
            },
            created_at: r.i64("created_at"),
            updated_at: r.i64("updated_at"),
        }
    }

    pub fn has_token(&self) -> bool {
        self.token.as_deref().is_some_and(|t| !t.is_empty())
    }
    pub fn has_oauth_token(&self) -> bool {
        self.oauth.access_token.as_deref().is_some_and(|t| !t.is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub session_id: String,
    pub from_agent_id: String,
    pub to_agent_id: String,
    pub child_session_id: Option<String>,
    pub instruction: String,
    /// "running" | "done" | "failed"
    pub status: String,
    pub result: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

impl Task {
    pub fn from_row(r: &Row<'_>) -> Task {
        Task {
            id: r.str("id"),
            session_id: r.str("session_id"),
            from_agent_id: r.str("from_agent_id"),
            to_agent_id: r.str("to_agent_id"),
            child_session_id: r.opt_str("child_session_id"),
            instruction: r.str("instruction"),
            status: r.str("status"),
            result: r.opt_str("result"),
            created_at: r.i64("created_at"),
            finished_at: r.opt_i64("finished_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    /// `every:<minutes>`, `at:<unix ms>` (once) or a 5-field cron expression (minute hour dom month dow, UTC).
    pub schedule: String,
    pub prompt: String,
    pub enabled: bool,
    /// Telegram chat id to deliver the answer to, if any.
    pub deliver_to: Option<String>,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub last_status: Option<String>,
    pub created_at: i64,
}

impl CronJob {
    pub fn from_row(r: &Row<'_>) -> CronJob {
        CronJob {
            id: r.str("id"),
            name: r.str("name"),
            agent_id: r.str("agent_id"),
            schedule: r.str("schedule"),
            prompt: r.str("prompt"),
            enabled: r.bool("enabled"),
            deliver_to: r.opt_str("deliver_to"),
            last_run_at: r.opt_i64("last_run_at"),
            next_run_at: r.opt_i64("next_run_at"),
            last_status: r.opt_str("last_status"),
            created_at: r.i64("created_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Improvement {
    pub id: String,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    /// "skill_created" | "skill_updated" | "skill_deleted" | "memory_saved" | "reflection" | "agent_created" | "agent_updated"
    pub kind: String,
    pub ref_id: Option<String>,
    pub note: String,
    pub created_at: i64,
}

impl Improvement {
    pub fn from_row(r: &Row<'_>) -> Improvement {
        Improvement {
            id: r.str("id"),
            agent_id: r.opt_str("agent_id"),
            session_id: r.opt_str("session_id"),
            kind: r.str("kind"),
            ref_id: r.opt_str("ref_id"),
            note: r.str("note"),
            created_at: r.i64("created_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub requests: i64,
}

/// A message an agent sent the user on its own initiative (`notify_user`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: String,
    pub agent_id: Option<String>,
    /// The conversation it came from.
    pub session_id: Option<String>,
    /// Short label of what it is about; the same topic is not sent twice in a row.
    pub topic: String,
    pub text: String,
    /// Telegram chats it reached.
    pub delivered: i64,
    pub created_at: i64,
}

impl Notification {
    pub fn from_row(r: &Row<'_>) -> Notification {
        Notification {
            id: r.str("id"),
            agent_id: r.opt_str("agent_id"),
            session_id: r.opt_str("session_id"),
            topic: r.str("topic"),
            text: r.str("text"),
            delivered: r.i64("delivered"),
            created_at: r.i64("created_at"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_serialize_with_type_tag() {
        let p = Part::ToolCall { id: "1".into(), name: "x".into(), args: serde_json::json!({"a": 1}) };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"type\":\"tool_call\""));
        let back: Part = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn skill_markdown_has_frontmatter() {
        let s = Skill {
            id: "a".into(),
            name: "deploy".into(),
            description: "How to deploy".into(),
            body: "# Steps".into(),
            tags: vec!["ops".into()],
            version: 2,
            source: "user".into(),
            author_agent_id: None,
            enabled: true,
            uses: 0,
            last_used_at: None,
            created_at: 0,
            updated_at: 0,
        };
        let md = s.to_markdown();
        assert!(md.starts_with("---\nname: deploy\n"));
        assert!(md.ends_with("# Steps"));
    }

    #[test]
    fn ids_are_short_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 20);
        assert_ne!(a, b);
    }
}
