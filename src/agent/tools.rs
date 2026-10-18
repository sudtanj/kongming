//! Built-in tools: skills, memory, delegation, cross-session reading, a small
//! web fetch and the clock. MCP tools are added by the runtime.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::shell;
use crate::db::{Agent, Session};
use crate::llm::ToolSpec;
use crate::state::{Event, Shared};

pub const MAX_DELEGATION_DEPTH: usize = 2;
const FETCH_LIMIT: usize = 12_000;

pub struct ToolCtx<'a> {
    pub state: &'a Shared,
    pub agent: &'a Agent,
    pub session: &'a Session,
    pub turn_id: &'a str,
    pub depth: usize,
    pub can_delegate: bool,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

pub fn builtin_specs(agent: &Agent, can_delegate: bool, sh: Option<&shell::Shell>) -> Vec<ToolSpec> {
    let mut v = vec![
        ToolSpec {
            name: "skill_view".into(),
            description: "Load the full text of a saved skill (procedure) by name. Always view a skill before following it.".into(),
            parameters: obj(json!({"name": {"type": "string", "description": "Skill name from the skills index"}}), &["name"]),
        },
        ToolSpec {
            name: "list_agents".into(),
            description: "List the agents in the team with their handles, roles and who they report to.".into(),
            parameters: obj(json!({}), &[]),
        },
        ToolSpec {
            name: "list_sessions".into(),
            description: "List recent conversations (sessions) so one can be read with read_session.".into(),
            parameters: obj(json!({"limit": {"type": "integer", "minimum": 1, "maximum": 50}}), &[]),
        },
        ToolSpec {
            name: "read_session".into(),
            description: "Read the recent transcript of another conversation by session id (the user may reference one with @session:<id>).".into(),
            parameters: obj(json!({"session_id": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 60}}), &["session_id"]),
        },
        ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a public http(s) URL and return its readable text (HTML is stripped). For JSON APIs the raw body is returned.".into(),
            parameters: obj(json!({"url": {"type": "string"}, "max_chars": {"type": "integer", "minimum": 500, "maximum": 20000}}), &["url"]),
        },
        ToolSpec {
            name: "current_time".into(),
            description: "Current date and time in UTC and as a unix timestamp.".into(),
            parameters: obj(json!({}), &[]),
        },
    ];
    if let Some(sh) = sh {
        let mut description =
            "Run a command in a local, isolated, read-only POSIX shell (BusyBox: sh, grep, sed, awk, \
sort, wc, wget, nc, nslookup, …) and get its output and exit code. It is for interacting, not for storage: nothing \
can be written to disk (files cannot be created, changed or deleted), so pass data along with pipes instead, e.g. \
`wget -qO- URL | grep …`. Nothing secret is reachable from it. Use it to talk to hosts, ports and URLs on this machine \
and the tailnet, and for quick calculations or text processing."
                .to_string();
        if sh.cdp_url.is_some() {
            description.push_str(
                " A real Chrome browser on the tailnet is connected over the Chrome DevTools Protocol (CDP): drive it \
with the `cdp` command, e.g. `cdp tabs`, `cdp goto URL`, `cdp text | head -c 4000`, `cdp click 'SELECTOR'`, \
`cdp type 'SELECTOR' 'TEXT'`, `cdp press Enter`, `cdp eval 'JS'`, `cdp call Domain.method '{json}'`; `cdp help` \
lists everything. Use it for pages that need JavaScript or a logged-in session.",
            );
        } else {
            description.push_str(
                " `cdp` drives a Chrome browser over the Chrome DevTools Protocol when you have its address (from a \
skill or the user): `cdp --url http://<ip>:9222 tabs`; `cdp help` lists everything.",
            );
        }
        v.push(ToolSpec {
            name: "shell".into(),
            description,
            parameters: obj(
                json!({
                    "command": {"type": "string", "description": "shell script to run with sh -c; stderr is merged into the output"},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": shell::MAX_TIMEOUT, "description": "default 30"}
                }),
                &["command"],
            ),
        });
    }
    if agent.self_improve {
        v.push(ToolSpec {
            name: "skill_manage".into(),
            description: "Create, update or delete saved skills. A skill is a reusable procedure: name (slug), one-line description saying when to use it, and a Markdown body with exact steps and pitfalls. Several operations may be sent at once.".into(),
            parameters: obj(
                json!({"operations": {"type": "array", "minItems": 1, "maxItems": 10, "items": obj(json!({
                    "op": {"type": "string", "enum": ["create", "update", "delete"]},
                    "name": {"type": "string"},
                    "description": {"type": "string", "description": "one line: when to use the skill"},
                    "body": {"type": "string", "description": "Markdown with the whole procedure: inputs, numbered steps with the exact tools, commands and parameters, how to check the result, pitfalls"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                }), &["op", "name"])}}),
                &["operations"],
            ),
        });
        v.push(ToolSpec {
            name: "memory".into(),
            description: "Long-term memory shared across conversations. add: store a durable fact/preference/lesson. search: find memories. remove: delete one by id.".into(),
            parameters: obj(
                json!({
                    "action": {"type": "string", "enum": ["add", "search", "remove"]},
                    "content": {"type": "string", "description": "add: the fact to remember (one or two sentences)"},
                    "kind": {"type": "string", "enum": ["fact", "preference", "lesson", "project"]},
                    "keywords": {"type": "array", "items": {"type": "string"}},
                    "importance": {"type": "integer", "minimum": 1, "maximum": 5},
                    "shared": {"type": "boolean", "description": "add: visible to every agent (default true)"},
                    "query": {"type": "string", "description": "search: words to look for"},
                    "id": {"type": "string", "description": "remove: memory id"}
                }),
                &["action"],
            ),
        });
    }
    if agent.is_default {
        v.push(ToolSpec {
            name: "agent_manage".into(),
            description: "Grow your team. create: a new agent that reports to you, with a handle, a role and a description \
of what it owns; hand it work with delegate_task (available as soon as it exists). update: change an agent that reports to \
you. Create an agent for a recurring specialty, not for a single task."
                .into(),
            parameters: obj(
                json!({
                    "op": {"type": "string", "enum": ["create", "update"]},
                    "handle": {"type": "string", "description": "short lowercase handle, e.g. researcher"},
                    "name": {"type": "string", "description": "display name, e.g. Researcher"},
                    "role": {"type": "string", "description": "job title, e.g. Research analyst"},
                    "description": {"type": "string", "description": "what it is responsible for; you see it when delegating"},
                    "system_prompt": {"type": "string", "description": "persona and working rules (optional)"},
                    "tools": {"type": "array", "items": {"type": "string"}, "description": "built-in tools it may use; omit for all"},
                    "model": {"type": "string", "description": "model override; omit to use the default model"}
                }),
                &["op", "handle"],
            ),
        });
    }
    if can_delegate {
        v.push(ToolSpec {
            name: "delegate_task".into(),
            description: "Hand a task to one of the agents that report to you. The delegate runs in its own conversation with its own tools and returns a summary. It cannot see this chat: put everything it needs in task and context.".into(),
            parameters: obj(
                json!({
                    "agent": {"type": "string", "description": "handle of a direct report, e.g. researcher"},
                    "task": {"type": "string", "description": "what to do and what to return"},
                    "context": {"type": "string", "description": "background, constraints, relevant facts"}
                }),
                &["agent", "task"],
            ),
        });
    }
    if let Some(allowed) = &agent.tools {
        v.retain(|t| allowed.iter().any(|a| a == &t.name));
    }
    v
}

fn s<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// Run a built-in tool. `None` means the name is not a built-in.
pub async fn run_builtin(ctx: &ToolCtx<'_>, name: &str, args: &Value) -> Option<Result<(String, bool)>> {
    let r = match name {
        "skill_view" => skill_view(ctx, args).await,
        "skill_manage" => skill_manage(ctx, args).await,
        "memory" => memory(ctx, args).await,
        "list_agents" => list_agents(ctx).await,
        "list_sessions" => list_sessions(ctx, args).await,
        "read_session" => read_session(ctx, args).await,
        "web_fetch" => web_fetch(ctx, args).await,
        "current_time" => Ok(current_time()),
        "delegate_task" => delegate(ctx, args).await,
        "agent_manage" => agent_manage(ctx, args).await,
        "shell" => return Some(run_shell(ctx, args).await),
        _ => return None,
    };
    Some(r.map(|t| (t, false)))
}

async fn skill_view(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    let name = s(args, "name");
    let skill = ctx.state.db.skill_by_name(name).await?.ok_or_else(|| anyhow!("no skill named '{name}'"))?;
    let _ = ctx.state.db.mark_skill_used(&skill.name).await;
    Ok(skill.to_markdown())
}

const BODY_HELP: &str = "Send `body` as Markdown with the whole procedure: when it applies, numbered steps with the exact \
tools, commands and parameters, how to check the result, and pitfalls. The description only says when to use the skill; \
the body is what a future run follows.";

/// A skill body as models send it: Markdown, or a list of steps. A whole
/// SKILL.md loses its frontmatter, which would otherwise be all the body shows.
fn skill_body(v: Option<&Value>) -> String {
    let text = match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(steps)) => steps
            .iter()
            .filter_map(Value::as_str)
            .enumerate()
            .map(|(i, step)| format!("{}. {}", i + 1, step.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    match crate::web::api::parse_skill_markdown(&text) {
        Some((_, _, _, body)) => body.trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// Why `body` would not be worth saving as a skill, if it would not.
pub(crate) fn thin_body(body: &str, description: &str) -> Option<&'static str> {
    let flat = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    if flat(description).contains(&flat(body)) {
        return Some("only repeats the description");
    }
    let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
    if body.chars().count() < 80 || lines < 2 {
        return Some("is too short to be a procedure");
    }
    None
}

async fn skill_manage(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    if !ctx.agent.self_improve {
        bail!("this agent may not edit skills");
    }
    let ops = args.get("operations").and_then(Value::as_array).ok_or_else(|| anyhow!("operations must be an array"))?;
    let mut report = Vec::new();
    for op in ops {
        let kind = s(op, "op");
        let name = s(op, "name");
        match kind {
            "create" | "update" => {
                let existing = ctx.state.db.skill_by_name(name).await?;
                let description = match (s(op, "description"), &existing) {
                    ("", Some(e)) => e.description.clone(),
                    ("", None) => bail!("{kind} {name}: description is required"),
                    (d, _) => d.to_string(),
                };
                let body = match (skill_body(op.get("body")), &existing) {
                    (b, Some(e)) if b.is_empty() => e.body.clone(),
                    (b, None) if b.is_empty() => bail!("{kind} {name}: body is required. {BODY_HELP}"),
                    (b, _) => b,
                };
                if let Some(why) = thin_body(&body, &description) {
                    bail!("{kind} {name}: the body {why}, so the skill would be useless. {BODY_HELP}");
                }
                if body.len() > 40_000 {
                    bail!("{kind} {name}: body too long (max 40000 chars)");
                }
                let tags = {
                    let t = strings(op.get("tags"));
                    if t.is_empty() {
                        existing.as_ref().map(|e| e.tags.clone()).unwrap_or_default()
                    } else {
                        t
                    }
                };
                let was_new = existing.is_none();
                let saved =
                    ctx.state.db.save_skill(name, &description, &body, &tags, "agent", Some(&ctx.agent.id)).await?;
                let ikind = if was_new { "skill_created" } else { "skill_updated" };
                let note = format!(
                    "{} skill `{}` (v{}): {}",
                    if was_new { "Created" } else { "Updated" },
                    saved.name,
                    saved.version,
                    description
                );
                ctx.state
                    .db
                    .log_improvement(Some(&ctx.agent.id), Some(&ctx.session.id), ikind, Some(&saved.id), &note)
                    .await?;
                ctx.state.emit(Event::Improvement {
                    kind: ikind.into(),
                    note: note.clone(),
                    agent_id: Some(ctx.agent.id.clone()),
                });
                report.push(note);
            }
            "delete" => {
                ctx.state.db.delete_skill(name).await?;
                let note = format!("Deleted skill `{name}`");
                ctx.state
                    .db
                    .log_improvement(Some(&ctx.agent.id), Some(&ctx.session.id), "skill_deleted", None, &note)
                    .await?;
                report.push(note);
            }
            other => bail!("unknown op '{other}' (create|update|delete)"),
        }
    }
    Ok(report.join("\n"))
}

async fn memory(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    if !ctx.agent.self_improve {
        bail!("this agent may not edit memory");
    }
    match s(args, "action") {
        "add" => {
            let content = s(args, "content").trim();
            if content.is_empty() {
                bail!("content is required");
            }
            if content.len() > 2000 {
                bail!("memory content too long (max 2000 chars)");
            }
            let shared = args.get("shared").and_then(Value::as_bool).unwrap_or(true);
            let kind = match s(args, "kind") {
                "" => "fact",
                k => k,
            };
            let m = ctx
                .state
                .db
                .save_memory(
                    if shared { None } else { Some(&ctx.agent.id) },
                    kind,
                    content,
                    &strings(args.get("keywords")),
                    args.get("importance").and_then(Value::as_i64).unwrap_or(2),
                )
                .await?;
            let note = format!("Remembered [{}]: {}", m.kind, m.content);
            ctx.state
                .db
                .log_improvement(Some(&ctx.agent.id), Some(&ctx.session.id), "memory_saved", Some(&m.id), &note)
                .await?;
            ctx.state.emit(Event::Improvement {
                kind: "memory_saved".into(),
                note,
                agent_id: Some(ctx.agent.id.clone()),
            });
            Ok(format!("saved memory {}", m.id))
        }
        "search" => {
            let found = ctx.state.db.search_memories(Some(&ctx.agent.id), s(args, "query"), 12).await?;
            if found.is_empty() {
                return Ok("no matching memories".into());
            }
            Ok(found.iter().map(|m| format!("- ({}) [{}] {}", m.id, m.kind, m.content)).collect::<Vec<_>>().join("\n"))
        }
        "remove" => {
            let id = s(args, "id");
            if id.is_empty() {
                bail!("id is required");
            }
            ctx.state.db.delete_memory(id).await?;
            Ok(format!("removed memory {id}"))
        }
        other => bail!("unknown action '{other}' (add|search|remove)"),
    }
}

async fn list_agents(ctx: &ToolCtx<'_>) -> Result<String> {
    let agents = ctx.state.db.agents().await?;
    let out: Vec<Value> = agents
        .iter()
        .map(|a| {
            json!({
                "handle": a.handle, "name": a.name, "role": a.role, "description": a.description,
                "reportsTo": a.manager_id.as_ref().and_then(|m| agents.iter().find(|x| &x.id == m)).map(|m| m.handle.clone()),
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&out)?)
}

async fn list_sessions(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(15).clamp(1, 50);
    let sessions = ctx.state.db.sessions(limit).await?;
    let out: Vec<Value> = sessions
        .iter()
        .map(|x| json!({"id": x.id, "title": x.title, "channel": x.channel, "updatedAt": x.updated_at, "current": x.id == ctx.session.id}))
        .collect();
    Ok(serde_json::to_string_pretty(&out)?)
}

pub async fn session_transcript(state: &Shared, session_id: &str, limit: i64) -> Result<String> {
    let session = state.db.session(session_id).await?.ok_or_else(|| anyhow!("no session {session_id}"))?;
    let msgs = state.db.recent_messages(session_id, limit).await?;
    let mut out = format!("# Session {} — {}\n", session.id, session.title);
    if let Some(sum) = &session.summary {
        out.push_str(&format!("Summary of earlier turns:\n{sum}\n\n"));
    }
    for m in msgs {
        let text = m.text();
        if text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("[{}] {}\n", m.role, crate::mcp::truncate(&text, 1500)));
    }
    Ok(out)
}

async fn read_session(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    let id = s(args, "session_id");
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(30).clamp(1, 60);
    session_transcript(ctx.state, id, limit).await
}

async fn run_shell(ctx: &ToolCtx<'_>, args: &Value) -> Result<(String, bool)> {
    let sh = ctx.state.shell.as_ref().ok_or_else(|| anyhow!("the local shell is turned off (KONGMING_SHELL)"))?;
    let command = s(args, "command");
    if command.trim().is_empty() {
        bail!("command is empty");
    }
    let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(shell::DEFAULT_TIMEOUT);
    sh.run(command, timeout).await
}

async fn web_fetch(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    let url = s(args, "url").trim().to_string();
    let parsed = url::Url::parse(&url).map_err(|_| anyhow!("invalid url"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("only http(s) urls are allowed");
    }
    if let Some(host) = parsed.host_str() {
        if host == "localhost"
            || host.ends_with(".local")
            || host.parse::<std::net::IpAddr>().is_ok_and(|ip| !is_public_ip(&ip))
        {
            bail!("refusing to fetch a private address");
        }
    }
    let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(FETCH_LIMIT as u64).clamp(500, 20_000) as usize;
    let resp = ctx
        .state
        .http
        .get(parsed)
        .header("accept", "text/html, application/json, text/plain, */*")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    let status = resp.status();
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = resp.bytes().await?;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(1_500_000)]).to_string();
    let text = if ct.contains("html") { html_to_text(&body) } else { body };
    Ok(format!("HTTP {status} {ct}\n\n{}", crate::mcp::truncate(text.trim(), max)))
}

fn is_public_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        std::net::IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// Crude but dependency-free HTML to text: drops script/style, tags, decodes
/// common entities and collapses whitespace.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            let skip_to = if rest.starts_with("<script") {
                rest.find("</script>").map(|p| i + p + 9)
            } else if rest.starts_with("<style") {
                rest.find("</style>").map(|p| i + p + 8)
            } else if rest.starts_with("<!--") {
                rest.find("-->").map(|p| i + p + 3)
            } else {
                None
            };
            if let Some(end) = skip_to {
                i = end;
                continue;
            }
            let end = html[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
            let tag = &lower[i..end];
            if tag.starts_with("<br")
                || tag.starts_with("<p")
                || tag.starts_with("</p")
                || tag.starts_with("<div")
                || tag.starts_with("</div")
                || tag.starts_with("<li")
                || tag.starts_with("<h")
                || tag.starts_with("</h")
                || tag.starts_with("<tr")
            {
                out.push('\n');
            } else if tag.starts_with("<td") || tag.starts_with("<th") {
                out.push(' ');
            }
            i = end;
            continue;
        }
        // Copy a whole UTF-8 character.
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&html[i..(i + ch_len).min(bytes.len())]);
        i += ch_len;
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let mut result = String::with_capacity(decoded.len());
    let mut blank = 0;
    for line in decoded.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            blank += 1;
            if blank <= 1 {
                result.push('\n');
            }
        } else {
            blank = 0;
            result.push_str(&l);
            result.push('\n');
        }
    }
    result.trim().to_string()
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn current_time() -> String {
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    format!("{} UTC (unix {})", now.format(&fmt).unwrap_or_default(), now.unix_timestamp())
}

/// The lead agent hires (or adjusts) its own direct reports.
async fn agent_manage(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    if !ctx.agent.is_default {
        bail!("only the lead agent may add agents to the team");
    }
    let handle = crate::db::normalize_handle(s(args, "handle"))?;
    let op = s(args, "op");
    let mut a = match (op, ctx.state.db.agent_by_handle(&handle).await?) {
        ("create", Some(_)) => bail!("@{handle} already exists; use op update, or pick another handle"),
        ("create", None) => {
            let now = crate::db::now_ms();
            Agent {
                id: crate::db::new_id(),
                handle: handle.clone(),
                name: handle.clone(),
                role: String::new(),
                description: String::new(),
                system_prompt: String::new(),
                model: None,
                provider: None,
                manager_id: Some(ctx.agent.id.clone()),
                tools: None,
                mcp_servers: None,
                pinned_skills: vec![],
                self_improve: true,
                is_default: false,
                created_at: now,
                updated_at: now,
            }
        }
        ("update", Some(a)) if a.manager_id.as_deref() == Some(ctx.agent.id.as_str()) => a,
        ("update", Some(_)) => bail!("@{handle} does not report to you; you can only change your direct reports"),
        ("update", None) => bail!("no agent @{handle}; use op create"),
        (other, _) => bail!("unknown op '{other}' (create|update)"),
    };
    let text = |k: &str| args.get(k).and_then(Value::as_str).map(|v| v.trim().to_string());
    if let Some(v) = text("name").filter(|v| !v.is_empty()) {
        a.name = v;
    }
    if let Some(v) = text("role") {
        a.role = v;
    }
    if let Some(v) = text("description") {
        a.description = v;
    }
    if let Some(v) = args.get("system_prompt").and_then(Value::as_str) {
        a.system_prompt = v.to_string();
    }
    if let Some(v) = text("model") {
        a.model = Some(v).filter(|m| !m.is_empty());
    }
    if args.get("tools").is_some() {
        let t = strings(args.get("tools"));
        a.tools = if t.is_empty() { None } else { Some(t) };
    }
    if op == "create" && (a.role.is_empty() || a.description.is_empty()) {
        bail!("create {handle}: role and description are required, so you know what to delegate to it");
    }
    a.updated_at = crate::db::now_ms();
    ctx.state.db.upsert_agent(&a).await?;
    let (kind, verb) = if op == "create" { ("agent_created", "Created") } else { ("agent_updated", "Updated") };
    let note = format!("{verb} agent @{} ({}), reporting to @{}", a.handle, a.role, ctx.agent.handle);
    ctx.state.db.log_improvement(Some(&ctx.agent.id), Some(&ctx.session.id), kind, Some(&a.id), &note).await?;
    ctx.state.emit(Event::Improvement { kind: kind.into(), note: note.clone(), agent_id: Some(ctx.agent.id.clone()) });
    Ok(format!("{note}. Hand it work with delegate_task (agent: \"{}\").", a.handle))
}

async fn delegate(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    if !ctx.can_delegate {
        bail!("this agent has no reports to delegate to");
    }
    if ctx.depth >= MAX_DELEGATION_DEPTH {
        bail!("delegation depth limit reached; do the work yourself");
    }
    let handle = s(args, "agent").trim().trim_start_matches('@').to_ascii_lowercase();
    let task = s(args, "task").trim().to_string();
    if task.is_empty() {
        bail!("task is required");
    }
    let target = ctx.state.db.agent_by_handle(&handle).await?.ok_or_else(|| anyhow!("no agent @{handle}"))?;
    if target.manager_id.as_deref() != Some(ctx.agent.id.as_str()) {
        bail!("@{handle} does not report to you; you can only delegate to your direct reports");
    }
    let context = s(args, "context").trim().to_string();
    ctx.state.emit(Event::Status {
        session_id: ctx.session.id.clone(),
        turn_id: ctx.turn_id.to_string(),
        text: format!("Delegating to @{}", target.handle),
    });
    crate::agent::run_delegation(
        ctx.state.clone(),
        ctx.agent.clone(),
        target,
        ctx.session.clone(),
        task,
        context,
        ctx.depth + 1,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(self_improve: bool, tools: Option<Vec<String>>) -> Agent {
        Agent {
            id: "a".into(),
            handle: "a".into(),
            name: "A".into(),
            role: String::new(),
            description: String::new(),
            system_prompt: String::new(),
            model: None,
            provider: None,
            manager_id: None,
            tools,
            mcp_servers: None,
            pinned_skills: vec![],
            self_improve,
            is_default: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn specs_follow_agent_permissions() {
        let sh = shell::Shell::from_config("/bin/sh", "").unwrap();
        let names = |a: &Agent, d: bool| builtin_specs(a, d, Some(&sh)).into_iter().map(|t| t.name).collect::<Vec<_>>();
        let full = names(&agent(true, None), true);
        assert!(full.contains(&"skill_manage".to_string()));
        assert!(full.contains(&"delegate_task".to_string()));
        assert!(full.contains(&"shell".to_string()));
        let worker = names(&agent(false, None), false);
        assert!(!worker.contains(&"skill_manage".to_string()));
        assert!(!worker.contains(&"delegate_task".to_string()));
        let limited = names(&agent(true, Some(vec!["current_time".into()])), true);
        assert_eq!(limited, vec!["current_time".to_string()]);
    }

    #[test]
    fn html_to_text_strips_markup() {
        let html = "<html><head><style>a{}</style><script>x()</script></head><body><h1>Title</h1><p>Hello &amp; <b>world</b></p><!-- c --><ul><li>one</li><li>two</li></ul></body></html>";
        let t = html_to_text(html);
        assert_eq!(t, "Title\n\nHello & world\n\none\ntwo");
    }

    #[test]
    fn private_ips_are_rejected() {
        assert!(!is_public_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip(&"10.1.2.3".parse().unwrap()));
        assert!(!is_public_ip(&"100.100.1.1".parse().unwrap()));
        assert!(is_public_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_public_ip(&"fe80::1".parse().unwrap()));
    }

    #[test]
    fn current_time_mentions_utc() {
        assert!(current_time().contains("UTC (unix "));
    }

    #[test]
    fn skill_bodies_must_hold_a_procedure() {
        let steps = "# Deploy\n\n1. Run `make release` on the build host.\n2. Copy the tarball to the board and restart the service.";
        assert_eq!(skill_body(Some(&json!(steps))), steps);
        let md = format!("---\nname: deploy\ndescription: Use when deploying\n---\n\n{steps}\n");
        assert_eq!(skill_body(Some(&json!(md))), steps, "frontmatter is dropped");
        assert_eq!(skill_body(Some(&json!(["Build it", "Ship it"]))), "1. Build it\n2. Ship it");
        assert_eq!(skill_body(None), "");

        assert_eq!(thin_body(steps, "Use when deploying"), None);
        assert!(thin_body("Use when deploying the app.", "Use when deploying the app").unwrap().contains("repeats"));
        assert!(thin_body("# Steps", "Use when deploying").unwrap().contains("too short"));
        let one_line = "Deploy by running make release and copying the tarball to the board, then restart it.";
        assert!(thin_body(one_line, "Use when deploying").unwrap().contains("too short"));
    }
}
