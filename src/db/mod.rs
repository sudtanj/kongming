//! The store: schema plus typed queries over Turso. Hot, rarely-changing rows
//! (settings, agents, skills index, MCP servers) are cached in memory so a
//! chat turn costs a handful of round trips, not dozens.

pub mod models;
pub mod turso;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use serde_json::{json, Value};

pub use models::*;
pub use turso::{Rows, Stmt, Turso, Val};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY,
  handle TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT '',
  system_prompt TEXT NOT NULL DEFAULT '',
  model TEXT,
  provider TEXT,
  manager_id TEXT,
  tools_json TEXT,
  mcp_json TEXT,
  skills_json TEXT NOT NULL DEFAULT '[]',
  self_improve INTEGER NOT NULL DEFAULT 1,
  is_default INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL DEFAULT '',
  agent_id TEXT NOT NULL,
  channel TEXT NOT NULL DEFAULT 'web',
  channel_ref TEXT,
  parent_session_id TEXT,
  summary TEXT,
  summary_upto INTEGER NOT NULL DEFAULT 0,
  archived INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(archived, updated_at DESC);
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  agent_id TEXT,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS messages_session_seq ON messages(session_id, seq);
CREATE TABLE IF NOT EXISTS skills (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '',
  body TEXT NOT NULL DEFAULT '',
  tags TEXT NOT NULL DEFAULT '[]',
  version INTEGER NOT NULL DEFAULT 1,
  source TEXT NOT NULL DEFAULT 'user',
  author_agent_id TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  uses INTEGER NOT NULL DEFAULT 0,
  last_used_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_versions (
  skill_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  description TEXT NOT NULL,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (skill_id, version)
);
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY,
  agent_id TEXT,
  kind TEXT NOT NULL DEFAULT 'fact',
  content TEXT NOT NULL,
  keywords TEXT NOT NULL DEFAULT '[]',
  importance INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_servers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  url TEXT NOT NULL,
  transport TEXT NOT NULL DEFAULT 'http',
  auth_type TEXT NOT NULL DEFAULT 'none',
  token TEXT,
  oauth_json TEXT NOT NULL DEFAULT '{}',
  headers_json TEXT NOT NULL DEFAULT '{}',
  enabled INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'unknown',
  last_error TEXT,
  tools_json TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS telegram_chats (
  chat_id INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  from_agent_id TEXT NOT NULL,
  to_agent_id TEXT NOT NULL,
  child_session_id TEXT,
  instruction TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'running',
  result TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE TABLE IF NOT EXISTS cron_jobs (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  schedule TEXT NOT NULL,
  prompt TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  deliver_to TEXT,
  last_run_at INTEGER,
  next_run_at INTEGER,
  last_status TEXT,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  session_id TEXT,
  agent_id TEXT,
  model TEXT,
  prompt_tokens INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS improvements (
  id TEXT PRIMARY KEY,
  agent_id TEXT,
  session_id TEXT,
  kind TEXT NOT NULL,
  ref_id TEXT,
  note TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
"#;

#[derive(Default)]
struct Cache {
    settings: Option<HashMap<String, Value>>,
    agents: Option<Vec<Agent>>,
    skills: Option<Vec<Skill>>,
    mcp: Option<Vec<McpServer>>,
}

#[derive(Clone)]
pub struct Db {
    pub turso: Turso,
    cache: Arc<RwLock<Cache>>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("host", &self.turso.host()).finish()
    }
}

fn j<T: serde::Serialize>(v: &T) -> Val {
    Val::Text(serde_json::to_string(v).unwrap_or_else(|_| "null".into()))
}

fn opt_j<T: serde::Serialize>(v: &Option<T>) -> Val {
    match v {
        Some(x) => j(x),
        None => Val::Null,
    }
}

impl Db {
    pub fn new(turso: Turso) -> Db {
        Db { turso, cache: Arc::new(RwLock::new(Cache::default())) }
    }

    pub async fn migrate(&self) -> Result<()> {
        self.turso.sequence(SCHEMA).await?;
        Ok(())
    }

    pub fn invalidate(&self) {
        *self.cache.write() = Cache::default();
    }

    // ---------------------------------------------------------------- settings

    pub async fn settings(&self) -> Result<HashMap<String, Value>> {
        if let Some(s) = &self.cache.read().settings {
            return Ok(s.clone());
        }
        let rows = self.turso.query("SELECT key, value FROM settings", &[]).await?;
        let map: HashMap<String, Value> = rows.iter().map(|r| (r.str("key"), r.json("value"))).collect();
        self.cache.write().settings = Some(map.clone());
        Ok(map)
    }

    pub async fn setting(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.settings().await?.get(key).cloned())
    }

    pub async fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO settings(key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                &[key.into(), j(value), now_ms().into()],
            )
            .await?;
        self.cache.write().settings = None;
        Ok(())
    }

    pub async fn delete_setting(&self, key: &str) -> Result<()> {
        self.turso.execute("DELETE FROM settings WHERE key = ?", &[key.into()]).await?;
        self.cache.write().settings = None;
        Ok(())
    }

    // ------------------------------------------------------------------ agents

    pub async fn agents(&self) -> Result<Vec<Agent>> {
        if let Some(a) = &self.cache.read().agents {
            return Ok(a.clone());
        }
        let rows = self.turso.query("SELECT * FROM agents ORDER BY is_default DESC, created_at", &[]).await?;
        let list: Vec<Agent> = rows.iter().map(|r| Agent::from_row(&r)).collect();
        self.cache.write().agents = Some(list.clone());
        Ok(list)
    }

    pub async fn agent(&self, id: &str) -> Result<Option<Agent>> {
        Ok(self.agents().await?.into_iter().find(|a| a.id == id))
    }

    pub async fn agent_by_handle(&self, handle: &str) -> Result<Option<Agent>> {
        let h = handle.trim_start_matches('@').to_ascii_lowercase();
        Ok(self.agents().await?.into_iter().find(|a| a.handle == h))
    }

    pub async fn default_agent(&self) -> Result<Agent> {
        let agents = self.agents().await?;
        agents.iter().find(|a| a.is_default).cloned().or_else(|| agents.first().cloned()).ok_or_else(|| anyhow!("no agents configured"))
    }

    pub async fn upsert_agent(&self, a: &Agent) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO agents(id, handle, name, role, description, system_prompt, model, provider, manager_id, tools_json, mcp_json, skills_json, self_improve, is_default, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET handle = excluded.handle, name = excluded.name, role = excluded.role, description = excluded.description,
                 system_prompt = excluded.system_prompt, model = excluded.model, provider = excluded.provider, manager_id = excluded.manager_id,
                 tools_json = excluded.tools_json, mcp_json = excluded.mcp_json, skills_json = excluded.skills_json, self_improve = excluded.self_improve,
                 is_default = excluded.is_default, updated_at = excluded.updated_at",
                &[
                    a.id.as_str().into(),
                    a.handle.as_str().into(),
                    a.name.as_str().into(),
                    a.role.as_str().into(),
                    a.description.as_str().into(),
                    a.system_prompt.as_str().into(),
                    a.model.clone().into(),
                    a.provider.clone().into(),
                    a.manager_id.clone().into(),
                    opt_j(&a.tools),
                    opt_j(&a.mcp_servers),
                    j(&a.pinned_skills),
                    a.self_improve.into(),
                    a.is_default.into(),
                    a.created_at.into(),
                    a.updated_at.into(),
                ],
            )
            .await?;
        if a.is_default {
            self.turso.execute("UPDATE agents SET is_default = 0 WHERE id != ?", &[a.id.as_str().into()]).await?;
        }
        self.cache.write().agents = None;
        Ok(())
    }

    pub async fn delete_agent(&self, id: &str) -> Result<()> {
        self.turso
            .batch(&[
                ("DELETE FROM agents WHERE id = ?".into(), vec![id.into()]),
                ("UPDATE agents SET manager_id = NULL WHERE manager_id = ?".into(), vec![id.into()]),
            ])
            .await?;
        self.cache.write().agents = None;
        Ok(())
    }

    // ---------------------------------------------------------------- sessions

    pub async fn sessions(&self, limit: i64) -> Result<Vec<Session>> {
        let rows = self
            .turso
            .query("SELECT * FROM sessions WHERE archived = 0 AND channel != 'delegation' ORDER BY updated_at DESC LIMIT ?", &[limit.into()])
            .await?;
        Ok(rows.iter().map(|r| Session::from_row(&r)).collect())
    }

    pub async fn session(&self, id: &str) -> Result<Option<Session>> {
        let rows = self.turso.query("SELECT * FROM sessions WHERE id = ?", &[id.into()]).await?;
        Ok(rows.first().map(|r| Session::from_row(&r)))
    }

    pub async fn create_session(&self, agent_id: &str, channel: &str, channel_ref: Option<&str>, parent: Option<&str>, title: &str) -> Result<Session> {
        let now = now_ms();
        let s = Session {
            id: new_id(),
            title: title.to_string(),
            agent_id: agent_id.to_string(),
            channel: channel.to_string(),
            channel_ref: channel_ref.map(str::to_string),
            parent_session_id: parent.map(str::to_string),
            summary: None,
            summary_upto: 0,
            archived: false,
            created_at: now,
            updated_at: now,
        };
        self.turso
            .execute(
                "INSERT INTO sessions(id, title, agent_id, channel, channel_ref, parent_session_id, summary, summary_upto, archived, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, NULL, 0, 0, ?, ?)",
                &[
                    s.id.as_str().into(),
                    s.title.as_str().into(),
                    s.agent_id.as_str().into(),
                    s.channel.as_str().into(),
                    s.channel_ref.clone().into(),
                    s.parent_session_id.clone().into(),
                    now.into(),
                    now.into(),
                ],
            )
            .await?;
        Ok(s)
    }

    pub async fn update_session_title(&self, id: &str, title: &str) -> Result<()> {
        self.turso.execute("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ?", &[title.into(), now_ms().into(), id.into()]).await?;
        Ok(())
    }

    pub async fn set_session_agent(&self, id: &str, agent_id: &str) -> Result<()> {
        self.turso.execute("UPDATE sessions SET agent_id = ?, updated_at = ? WHERE id = ?", &[agent_id.into(), now_ms().into(), id.into()]).await?;
        Ok(())
    }

    pub async fn touch_session(&self, id: &str) -> Result<()> {
        self.turso.execute("UPDATE sessions SET updated_at = ? WHERE id = ?", &[now_ms().into(), id.into()]).await?;
        Ok(())
    }

    pub async fn set_session_summary(&self, id: &str, summary: &str, upto: i64) -> Result<()> {
        self.turso
            .execute("UPDATE sessions SET summary = ?, summary_upto = ? WHERE id = ?", &[summary.into(), upto.into(), id.into()])
            .await?;
        Ok(())
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        self.turso
            .batch(&[
                ("DELETE FROM messages WHERE session_id = ?".into(), vec![id.into()]),
                ("DELETE FROM sessions WHERE id = ?".into(), vec![id.into()]),
                ("DELETE FROM telegram_chats WHERE session_id = ?".into(), vec![id.into()]),
            ])
            .await
    }

    pub async fn clear_session(&self, id: &str) -> Result<()> {
        self.turso
            .batch(&[
                ("DELETE FROM messages WHERE session_id = ?".into(), vec![id.into()]),
                ("UPDATE sessions SET summary = NULL, summary_upto = 0, updated_at = ? WHERE id = ?".into(), vec![now_ms().into(), id.into()]),
            ])
            .await
    }

    // ---------------------------------------------------------------- messages

    pub async fn messages(&self, session_id: &str, from_seq: i64) -> Result<Vec<Message>> {
        let rows = self
            .turso
            .query("SELECT * FROM messages WHERE session_id = ? AND seq >= ? ORDER BY seq", &[session_id.into(), from_seq.into()])
            .await?;
        Ok(rows.iter().map(|r| Message::from_row(&r)).collect())
    }

    pub async fn recent_messages(&self, session_id: &str, limit: i64) -> Result<Vec<Message>> {
        let rows = self
            .turso
            .query(
                "SELECT * FROM (SELECT * FROM messages WHERE session_id = ? ORDER BY seq DESC LIMIT ?) ORDER BY seq",
                &[session_id.into(), limit.into()],
            )
            .await?;
        Ok(rows.iter().map(|r| Message::from_row(&r)).collect())
    }

    /// Append a message; `seq` is assigned server-side in the same transaction-free
    /// statement so concurrent writers from one process stay ordered.
    pub async fn append_message(&self, session_id: &str, role: &str, parts: &[Part], agent_id: Option<&str>) -> Result<Message> {
        let id = new_id();
        let now = now_ms();
        self.turso
            .batch(&[
                (
                    "INSERT INTO messages(id, session_id, seq, role, content, agent_id, created_at)
                     SELECT ?, ?, COALESCE(MAX(seq), 0) + 1, ?, ?, ?, ? FROM messages WHERE session_id = ?"
                        .into(),
                    vec![
                        id.as_str().into(),
                        session_id.into(),
                        role.into(),
                        j(&parts),
                        agent_id.map(str::to_string).into(),
                        now.into(),
                        session_id.into(),
                    ],
                ),
                ("UPDATE sessions SET updated_at = ? WHERE id = ?".into(), vec![now.into(), session_id.into()]),
            ])
            .await?;
        let rows = self.turso.query("SELECT * FROM messages WHERE id = ?", &[id.as_str().into()]).await?;
        rows.first().map(|r| Message::from_row(&r)).ok_or_else(|| anyhow!("message vanished after insert"))
    }

    pub async fn message_count(&self, session_id: &str) -> Result<i64> {
        let rows = self.turso.query("SELECT COUNT(*) AS n FROM messages WHERE session_id = ?", &[session_id.into()]).await?;
        Ok(rows.first().map(|r| r.i64("n")).unwrap_or(0))
    }

    // ------------------------------------------------------------------ skills

    pub async fn skills(&self) -> Result<Vec<Skill>> {
        if let Some(s) = &self.cache.read().skills {
            return Ok(s.clone());
        }
        let rows = self.turso.query("SELECT * FROM skills ORDER BY name", &[]).await?;
        let list: Vec<Skill> = rows.iter().map(|r| Skill::from_row(&r)).collect();
        self.cache.write().skills = Some(list.clone());
        Ok(list)
    }

    pub async fn skill_by_name(&self, name: &str) -> Result<Option<Skill>> {
        let n = name.trim().to_ascii_lowercase();
        Ok(self.skills().await?.into_iter().find(|s| s.name == n))
    }

    /// Create or update a skill; every update keeps the previous version in
    /// `skill_versions` so an agent's edit can be reviewed or rolled back.
    pub async fn save_skill(&self, name: &str, description: &str, body: &str, tags: &[String], source: &str, author: Option<&str>) -> Result<Skill> {
        let name = normalize_skill_name(name)?;
        let now = now_ms();
        let existing = self.skill_by_name(&name).await?;
        match existing {
            Some(old) => {
                let version = old.version + 1;
                self.turso
                    .batch(&[
                        (
                            "INSERT OR REPLACE INTO skill_versions(skill_id, version, description, body, created_at) VALUES (?, ?, ?, ?, ?)".into(),
                            vec![old.id.as_str().into(), old.version.into(), old.description.as_str().into(), old.body.as_str().into(), old.updated_at.into()],
                        ),
                        (
                            "UPDATE skills SET description = ?, body = ?, tags = ?, version = ?, updated_at = ? WHERE id = ?".into(),
                            vec![description.into(), body.into(), j(&tags), version.into(), now.into(), old.id.as_str().into()],
                        ),
                    ])
                    .await?;
            }
            None => {
                self.turso
                    .execute(
                        "INSERT INTO skills(id, name, description, body, tags, version, source, author_agent_id, enabled, uses, last_used_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 1, ?, ?, 1, 0, NULL, ?, ?)",
                        &[new_id().into(), name.as_str().into(), description.into(), body.into(), j(&tags), source.into(), author.map(str::to_string).into(), now.into(), now.into()],
                    )
                    .await?;
            }
        }
        self.cache.write().skills = None;
        self.skill_by_name(&name).await?.ok_or_else(|| anyhow!("skill vanished after save"))
    }

    pub async fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.turso.execute("UPDATE skills SET enabled = ?, updated_at = ? WHERE name = ?", &[enabled.into(), now_ms().into(), name.into()]).await?;
        self.cache.write().skills = None;
        Ok(())
    }

    pub async fn delete_skill(&self, name: &str) -> Result<()> {
        if let Some(s) = self.skill_by_name(name).await? {
            self.turso
                .batch(&[
                    ("DELETE FROM skill_versions WHERE skill_id = ?".into(), vec![s.id.as_str().into()]),
                    ("DELETE FROM skills WHERE id = ?".into(), vec![s.id.as_str().into()]),
                ])
                .await?;
            self.cache.write().skills = None;
        }
        Ok(())
    }

    pub async fn mark_skill_used(&self, name: &str) -> Result<()> {
        self.turso.execute("UPDATE skills SET uses = uses + 1, last_used_at = ? WHERE name = ?", &[now_ms().into(), name.into()]).await?;
        self.cache.write().skills = None;
        Ok(())
    }

    pub async fn skill_versions(&self, name: &str) -> Result<Vec<Value>> {
        let Some(s) = self.skill_by_name(name).await? else { return Ok(vec![]) };
        let rows = self
            .turso
            .query("SELECT version, description, body, created_at FROM skill_versions WHERE skill_id = ? ORDER BY version DESC", &[s.id.as_str().into()])
            .await?;
        Ok(rows
            .iter()
            .map(|r| json!({"version": r.i64("version"), "description": r.str("description"), "body": r.str("body"), "createdAt": r.i64("created_at")}))
            .collect())
    }

    // ---------------------------------------------------------------- memories

    pub async fn memories(&self, agent_id: Option<&str>, limit: i64) -> Result<Vec<Memory>> {
        let rows = match agent_id {
            Some(a) => {
                self.turso
                    .query(
                        "SELECT * FROM memories WHERE agent_id IS NULL OR agent_id = ? ORDER BY importance DESC, updated_at DESC LIMIT ?",
                        &[a.into(), limit.into()],
                    )
                    .await?
            }
            None => self.turso.query("SELECT * FROM memories ORDER BY importance DESC, updated_at DESC LIMIT ?", &[limit.into()]).await?,
        };
        Ok(rows.iter().map(|r| Memory::from_row(&r)).collect())
    }

    pub async fn search_memories(&self, agent_id: Option<&str>, query: &str, limit: i64) -> Result<Vec<Memory>> {
        let all = self.memories(agent_id, 500).await?;
        let terms: Vec<String> = query.to_ascii_lowercase().split_whitespace().map(str::to_string).filter(|t| t.len() > 2).collect();
        let mut scored: Vec<(i64, Memory)> = all
            .into_iter()
            .map(|m| {
                let hay = format!("{} {}", m.content, m.keywords.join(" ")).to_ascii_lowercase();
                let score: i64 = terms.iter().map(|t| if hay.contains(t.as_str()) { 2 } else { 0 }).sum::<i64>() + m.importance;
                (score, m)
            })
            .filter(|(s, _)| terms.is_empty() || *s > 1)
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(scored.into_iter().take(limit as usize).map(|(_, m)| m).collect())
    }

    pub async fn save_memory(&self, agent_id: Option<&str>, kind: &str, content: &str, keywords: &[String], importance: i64) -> Result<Memory> {
        let now = now_ms();
        let m = Memory {
            id: new_id(),
            agent_id: agent_id.map(str::to_string),
            kind: kind.to_string(),
            content: content.trim().to_string(),
            keywords: keywords.to_vec(),
            importance: importance.clamp(1, 5),
            created_at: now,
            updated_at: now,
        };
        self.turso
            .execute(
                "INSERT INTO memories(id, agent_id, kind, content, keywords, importance, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                &[m.id.as_str().into(), m.agent_id.clone().into(), m.kind.as_str().into(), m.content.as_str().into(), j(&m.keywords), m.importance.into(), now.into(), now.into()],
            )
            .await?;
        Ok(m)
    }

    pub async fn delete_memory(&self, id: &str) -> Result<()> {
        self.turso.execute("DELETE FROM memories WHERE id = ?", &[id.into()]).await?;
        Ok(())
    }

    // --------------------------------------------------------------------- mcp

    pub async fn mcp_servers(&self) -> Result<Vec<McpServer>> {
        if let Some(m) = &self.cache.read().mcp {
            return Ok(m.clone());
        }
        let rows = self.turso.query("SELECT * FROM mcp_servers ORDER BY name", &[]).await?;
        let list: Vec<McpServer> = rows.iter().map(|r| McpServer::from_row(&r)).collect();
        self.cache.write().mcp = Some(list.clone());
        Ok(list)
    }

    pub async fn mcp_server(&self, id: &str) -> Result<Option<McpServer>> {
        Ok(self.mcp_servers().await?.into_iter().find(|s| s.id == id))
    }

    pub async fn upsert_mcp_server(&self, s: &McpServer) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO mcp_servers(id, name, url, transport, auth_type, token, oauth_json, headers_json, enabled, status, last_error, tools_json, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, url = excluded.url, transport = excluded.transport, auth_type = excluded.auth_type,
                 token = excluded.token, oauth_json = excluded.oauth_json, headers_json = excluded.headers_json, enabled = excluded.enabled,
                 status = excluded.status, last_error = excluded.last_error, tools_json = excluded.tools_json, updated_at = excluded.updated_at",
                &[
                    s.id.as_str().into(),
                    s.name.as_str().into(),
                    s.url.as_str().into(),
                    s.transport.as_str().into(),
                    s.auth_type.as_str().into(),
                    s.token.clone().into(),
                    j(&s.oauth),
                    j(&s.headers),
                    s.enabled.into(),
                    s.status.as_str().into(),
                    s.last_error.clone().into(),
                    j(&s.tools),
                    s.created_at.into(),
                    s.updated_at.into(),
                ],
            )
            .await?;
        self.cache.write().mcp = None;
        Ok(())
    }

    pub async fn set_mcp_status(&self, id: &str, status: &str, error: Option<&str>, tools: Option<&Value>) -> Result<()> {
        match tools {
            Some(t) => {
                self.turso
                    .execute(
                        "UPDATE mcp_servers SET status = ?, last_error = ?, tools_json = ?, updated_at = ? WHERE id = ?",
                        &[status.into(), error.map(str::to_string).into(), j(t), now_ms().into(), id.into()],
                    )
                    .await?
            }
            None => {
                self.turso
                    .execute(
                        "UPDATE mcp_servers SET status = ?, last_error = ?, updated_at = ? WHERE id = ?",
                        &[status.into(), error.map(str::to_string).into(), now_ms().into(), id.into()],
                    )
                    .await?
            }
        };
        self.cache.write().mcp = None;
        Ok(())
    }

    pub async fn set_mcp_oauth(&self, id: &str, oauth: &OAuthState) -> Result<()> {
        self.turso.execute("UPDATE mcp_servers SET oauth_json = ?, updated_at = ? WHERE id = ?", &[j(oauth), now_ms().into(), id.into()]).await?;
        self.cache.write().mcp = None;
        Ok(())
    }

    pub async fn delete_mcp_server(&self, id: &str) -> Result<()> {
        self.turso.execute("DELETE FROM mcp_servers WHERE id = ?", &[id.into()]).await?;
        self.cache.write().mcp = None;
        Ok(())
    }

    // ---------------------------------------------------------------- telegram

    pub async fn telegram_chat(&self, chat_id: i64) -> Result<Option<(String, String)>> {
        let rows = self.turso.query("SELECT session_id, agent_id FROM telegram_chats WHERE chat_id = ?", &[chat_id.into()]).await?;
        Ok(rows.first().map(|r| (r.str("session_id"), r.str("agent_id"))))
    }

    pub async fn set_telegram_chat(&self, chat_id: i64, session_id: &str, agent_id: &str) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO telegram_chats(chat_id, session_id, agent_id, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(chat_id) DO UPDATE SET session_id = excluded.session_id, agent_id = excluded.agent_id, updated_at = excluded.updated_at",
                &[chat_id.into(), session_id.into(), agent_id.into(), now_ms().into()],
            )
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------------- tasks

    pub async fn create_task(&self, session_id: &str, from: &str, to: &str, instruction: &str) -> Result<Task> {
        let t = Task {
            id: new_id(),
            session_id: session_id.into(),
            from_agent_id: from.into(),
            to_agent_id: to.into(),
            child_session_id: None,
            instruction: instruction.into(),
            status: "running".into(),
            result: None,
            created_at: now_ms(),
            finished_at: None,
        };
        self.turso
            .execute(
                "INSERT INTO tasks(id, session_id, from_agent_id, to_agent_id, child_session_id, instruction, status, result, created_at, finished_at) VALUES (?, ?, ?, ?, NULL, ?, 'running', NULL, ?, NULL)",
                &[t.id.as_str().into(), session_id.into(), from.into(), to.into(), instruction.into(), t.created_at.into()],
            )
            .await?;
        Ok(t)
    }

    pub async fn finish_task(&self, id: &str, child_session: Option<&str>, status: &str, result: &str) -> Result<()> {
        self.turso
            .execute(
                "UPDATE tasks SET child_session_id = COALESCE(?, child_session_id), status = ?, result = ?, finished_at = ? WHERE id = ?",
                &[child_session.map(str::to_string).into(), status.into(), result.into(), now_ms().into(), id.into()],
            )
            .await?;
        Ok(())
    }

    pub async fn tasks(&self, limit: i64) -> Result<Vec<Task>> {
        let rows = self.turso.query("SELECT * FROM tasks ORDER BY created_at DESC LIMIT ?", &[limit.into()]).await?;
        Ok(rows.iter().map(|r| Task::from_row(&r)).collect())
    }

    // -------------------------------------------------------------------- cron

    pub async fn cron_jobs(&self) -> Result<Vec<CronJob>> {
        let rows = self.turso.query("SELECT * FROM cron_jobs ORDER BY created_at", &[]).await?;
        Ok(rows.iter().map(|r| CronJob::from_row(&r)).collect())
    }

    pub async fn upsert_cron_job(&self, c: &CronJob) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO cron_jobs(id, name, agent_id, schedule, prompt, enabled, deliver_to, last_run_at, next_run_at, last_status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, agent_id = excluded.agent_id, schedule = excluded.schedule, prompt = excluded.prompt, enabled = excluded.enabled,
                 deliver_to = excluded.deliver_to, next_run_at = excluded.next_run_at",
                &[
                    c.id.as_str().into(),
                    c.name.as_str().into(),
                    c.agent_id.as_str().into(),
                    c.schedule.as_str().into(),
                    c.prompt.as_str().into(),
                    c.enabled.into(),
                    c.deliver_to.clone().into(),
                    c.last_run_at.into(),
                    c.next_run_at.into(),
                    c.last_status.clone().into(),
                    c.created_at.into(),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn mark_cron_run(&self, id: &str, next: Option<i64>, status: &str) -> Result<()> {
        self.turso
            .execute("UPDATE cron_jobs SET last_run_at = ?, next_run_at = ?, last_status = ? WHERE id = ?", &[now_ms().into(), next.into(), status.into(), id.into()])
            .await?;
        Ok(())
    }

    pub async fn delete_cron_job(&self, id: &str) -> Result<()> {
        self.turso.execute("DELETE FROM cron_jobs WHERE id = ?", &[id.into()]).await?;
        Ok(())
    }

    // ------------------------------------------------------------------- usage

    pub async fn record_usage(&self, session_id: Option<&str>, agent_id: Option<&str>, model: &str, prompt: i64, completion: i64) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO usage(ts, session_id, agent_id, model, prompt_tokens, completion_tokens) VALUES (?, ?, ?, ?, ?, ?)",
                &[now_ms().into(), session_id.map(str::to_string).into(), agent_id.map(str::to_string).into(), model.into(), prompt.into(), completion.into()],
            )
            .await?;
        Ok(())
    }

    pub async fn usage_since(&self, since_ms: i64) -> Result<UsageTotals> {
        let rows = self
            .turso
            .query(
                "SELECT COALESCE(SUM(prompt_tokens),0) AS p, COALESCE(SUM(completion_tokens),0) AS c, COUNT(*) AS n FROM usage WHERE ts >= ?",
                &[since_ms.into()],
            )
            .await?;
        Ok(rows
            .first()
            .map(|r| UsageTotals { prompt_tokens: r.i64("p"), completion_tokens: r.i64("c"), requests: r.i64("n") })
            .unwrap_or_default())
    }

    // ------------------------------------------------------------ improvements

    pub async fn log_improvement(&self, agent_id: Option<&str>, session_id: Option<&str>, kind: &str, ref_id: Option<&str>, note: &str) -> Result<()> {
        self.turso
            .execute(
                "INSERT INTO improvements(id, agent_id, session_id, kind, ref_id, note, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                &[new_id().into(), agent_id.map(str::to_string).into(), session_id.map(str::to_string).into(), kind.into(), ref_id.map(str::to_string).into(), note.into(), now_ms().into()],
            )
            .await?;
        Ok(())
    }

    pub async fn improvements(&self, limit: i64) -> Result<Vec<Improvement>> {
        let rows = self.turso.query("SELECT * FROM improvements ORDER BY created_at DESC LIMIT ?", &[limit.into()]).await?;
        Ok(rows.iter().map(|r| Improvement::from_row(&r)).collect())
    }
}

/// Skill names are lowercase slugs so they are stable as `@skill:name` mentions
/// and as tool arguments.
pub fn normalize_skill_name(name: &str) -> Result<String> {
    let n: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let n = n.trim_matches('-').to_string();
    if n.is_empty() || n.len() > 64 {
        return Err(anyhow!("skill name must be 1-64 characters of letters, digits, '-' or '_'"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_names_are_slugs() {
        assert_eq!(normalize_skill_name(" Deploy To Prod ").unwrap(), "deploy-to-prod");
        assert_eq!(normalize_skill_name("a_b-c").unwrap(), "a_b-c");
        assert!(normalize_skill_name("   ").is_err());
        assert!(normalize_skill_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn schema_is_idempotent_sql() {
        // Every statement uses IF NOT EXISTS so re-running on boot is safe.
        for stmt in SCHEMA.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            assert!(stmt.contains("IF NOT EXISTS"), "statement lacks IF NOT EXISTS: {stmt}");
        }
    }
}
