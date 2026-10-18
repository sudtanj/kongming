//! System prompt assembly. Stable sections come first (identity, team,
//! skills index) so provider-side prompt caching gets a long common prefix;
//! volatile bits (time, attached context) go last.

use crate::db::{Agent, Memory, Skill};

pub const DEFAULT_PERSONA: &str = "You are Kongming, a calm and resourceful assistant. You think before you act, \
prefer verified facts over guesses, and say plainly when you do not know something. You keep answers short and \
useful. When a task is multi-step you work through it with your tools instead of describing what could be done.";

const MAX_DESC: usize = 96;

fn short(s: &str, n: usize) -> String {
    let s = s.trim().replace('\n', " ");
    if s.chars().count() <= n {
        s
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

pub struct PromptInput<'a> {
    pub agent: &'a Agent,
    pub team: &'a [Agent],
    pub skills: &'a [Skill],
    pub memories: &'a [Memory],
    /// Full bodies to inline: pinned skills and `@skill:` mentions.
    pub inline_skills: &'a [Skill],
    pub mcp_servers: &'a [String],
    pub channel: &'a str,
    pub can_delegate: bool,
    pub extra_context: &'a str,
    pub now_utc: &'a str,
}

pub fn build(p: &PromptInput<'_>) -> String {
    let a = p.agent;
    let mut s = String::with_capacity(4096);

    // 1. Identity
    s.push_str("# Identity\n");
    s.push_str(&format!("You are **{}** (@{}).", a.name, a.handle));
    if !a.role.trim().is_empty() {
        s.push_str(&format!(" Role: {}.", a.role.trim()));
    }
    s.push('\n');
    if !a.description.trim().is_empty() {
        s.push_str(&format!("Responsibilities: {}\n", a.description.trim()));
    }
    s.push('\n');
    s.push_str(if a.system_prompt.trim().is_empty() { DEFAULT_PERSONA } else { a.system_prompt.trim() });
    s.push_str("\n\n");

    // 2. Team
    let reports: Vec<&Agent> = p.team.iter().filter(|t| t.manager_id.as_deref() == Some(&a.id)).collect();
    let manager = a.manager_id.as_ref().and_then(|m| p.team.iter().find(|t| &t.id == m));
    if p.team.len() > 1 {
        s.push_str("# Team\n");
        s.push_str("You are part of a team of agents. Users can address any agent with `@handle`.\n");
        if let Some(m) = manager {
            s.push_str(&format!("Your manager is **{}** (@{}) — {}.\n", m.name, m.handle, short(&m.role, 60)));
        }
        if !reports.is_empty() && p.can_delegate {
            s.push_str("Agents reporting to you (you may hand work to them with `delegate_task`):\n");
            for r in &reports {
                s.push_str(&format!("- @{} — {}: {}\n", r.handle, r.role.trim(), short(&r.description, 160)));
            }
            s.push_str(
                "Delegate when a task clearly belongs to a report's specialty or when independent pieces can run side by side. \
Give each delegate everything it needs in `task` and `context`; it cannot see this conversation. Treat results as self-reports \
and sanity-check them before presenting them as fact. Do simple things yourself.\n",
            );
        }
        let others: Vec<&Agent> = p
            .team
            .iter()
            .filter(|t| {
                t.id != a.id && !reports.iter().any(|r| r.id == t.id) && Some(t.id.as_str()) != a.manager_id.as_deref()
            })
            .collect();
        if !others.is_empty() {
            s.push_str("Other agents (for the user's reference; you cannot delegate to them):\n");
            for o in others {
                s.push_str(&format!("- @{} — {}\n", o.handle, short(&o.role, 60)));
            }
        }
        s.push('\n');
    }

    // 3. Skills index
    let enabled: Vec<&Skill> = p.skills.iter().filter(|k| k.enabled).collect();
    s.push_str("# Skills\n");
    if enabled.is_empty() {
        s.push_str("No skills saved yet.\n");
    } else {
        s.push_str("Reusable procedures. Load one with `skill_view` before relying on it; do not guess its contents.\n<available_skills>\n");
        for k in &enabled {
            s.push_str(&format!("- {}: {}\n", k.name, short(&k.description, MAX_DESC)));
        }
        s.push_str("</available_skills>\n");
    }
    s.push('\n');

    // 4. Self-improvement
    s.push_str("# Learning\n");
    if a.self_improve {
        s.push_str(
            "You improve over time. After finishing something non-trivial, ask yourself: would a future run benefit from a written \
procedure? If a workflow took several steps, needed a non-obvious fix, or the user corrected you, save or update a skill with \
`skill_manage` (clear name, when to use it, exact steps, pitfalls). Save durable facts about the user, their systems and their \
preferences with `memory`. Fix a skill that misled you instead of working around it. Do not save one-off trivia or secrets.\n",
        );
    } else {
        s.push_str("Skills and memory are managed by the user; `skill_manage` and `memory` are unavailable to you.\n");
    }
    s.push('\n');

    // 5. Memory
    if !p.memories.is_empty() {
        s.push_str("# Memory\n");
        for m in p.memories {
            s.push_str(&format!("- [{}] {}\n", m.kind, m.content.trim()));
        }
        s.push('\n');
    }

    // 6. Tools / MCP
    s.push_str("# Using tools\n");
    s.push_str(
        "When a tool call fails, fix it before anything else: read the error, then supply the missing arguments or correct \
the wrong ones and call again. Look up values you lack with other tools first (for example the account, owner or repository \
behind a request) or take them from the conversation; try another tool when one cannot do the job. Give up only after real \
attempts, then tell the user in plain words what failed and what you need. Never answer with raw tool output; summarize it.\n\n",
    );
    if !p.mcp_servers.is_empty() {
        s.push_str("# Connected services\n");
        s.push_str(&format!(
            "MCP tools are available from: {}. Their tool names start with `mcp__`.\n\n",
            p.mcp_servers.join(", ")
        ));
    }

    // 7. Channel conventions
    s.push_str("# Conversation\n");
    match p.channel {
        "telegram" => s.push_str(
            "This conversation is on Telegram. Keep replies compact (a few short paragraphs at most), use plain Markdown \
(bold, code, lists), no tables, no headings.\n",
        ),
        "delegation" => s.push_str(
            "You were given this task by another agent. Work it through completely, then end with a tight summary: outcome \
first, then key findings, then anything that blocked you. Your final message is all the requester sees.\n",
        ),
        "cron" => s.push_str("This is a scheduled run without a human present. Do the task and report concisely.\n"),
        _ => s.push_str("This conversation is in the kongming web app. Markdown is rendered, including code blocks and tables.\n"),
    }
    s.push_str(&format!("Current time: {} (UTC).\n", p.now_utc));

    // 8. Inline skills and extra context
    for k in p.inline_skills {
        s.push_str(&format!("\n<skill name=\"{}\">\n{}\n</skill>\n", k.name, k.body.trim()));
    }
    if !p.extra_context.trim().is_empty() {
        s.push_str("\n# Attached context\n");
        s.push_str(p.extra_context.trim());
        s.push('\n');
    }
    s
}

/// Prompt for the post-turn reflection pass (the "keeps improving itself"
/// part). It runs with only `skill_manage` and `memory` as tools.
pub fn reflection_prompt(agent: &Agent, skills: &[Skill]) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "You are the reflection step for the agent @{} ({}). You read a finished conversation and decide whether anything \
should be kept for future runs.\n\n",
        agent.handle, agent.role
    ));
    s.push_str("Save a SKILL when the conversation contains a reusable multi-step procedure, a non-obvious fix, or a workflow the \
user is likely to ask for again. A skill has a short slug name, a one-line description that says WHEN to use it, and a body \
that a future run follows without seeing this conversation: Markdown with the inputs it needs, numbered steps with the exact \
tools, commands, parameters, ids and urls that worked, how to check the result, and the pitfalls hit here. The body is the \
skill; never send a skill without one or with the description repeated. Prefer updating an existing skill over creating a \
near-duplicate: `skill_view` it first and send the merged body.\n\
Save a MEMORY for durable facts: who the user is, their systems, names, preferences, decisions, and corrections they made.\n\
Do NOTHING when the conversation was small talk, a one-off question, or already covered by existing skills and memory. \
Never store secrets (tokens, passwords).\n\n");
    if !skills.is_empty() {
        s.push_str("Existing skills:\n");
        for k in skills.iter().filter(|k| k.enabled) {
            // Older skills saved with no real body: worth an update when this conversation shows how.
            let thin = super::tools::thin_body(&k.body, &k.description).map_or("", |_| " [EMPTY BODY: rewrite it]");
            s.push_str(&format!("- {}: {}{thin}\n", k.name, short(&k.description, MAX_DESC)));
        }
        s.push('\n');
    }
    s.push_str("Use the tools if there is something worth keeping, then answer with one short line describing what you saved, \
or exactly `nothing to save`.");
    s
}

/// Prompt used to compact old history into a running summary.
pub fn compaction_prompt() -> &'static str {
    "Summarize the conversation so far for your own future reference. Keep: the user's goals and constraints, decisions made, \
facts discovered (names, ids, urls, paths, numbers), what was tried and what failed, open questions, and the current state of \
any task (for a task still in progress: what is done and what remains). Use terse bullet points. Omit pleasantries. Do not \
invent anything."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, handle: &str, manager: Option<&str>) -> Agent {
        Agent {
            id: id.into(),
            handle: handle.into(),
            name: handle.to_uppercase(),
            role: "Role".into(),
            description: "desc".into(),
            system_prompt: String::new(),
            model: None,
            provider: None,
            manager_id: manager.map(str::to_string),
            tools: None,
            mcp_servers: None,
            pinned_skills: vec![],
            self_improve: true,
            is_default: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn manager_sees_reports_and_delegation_rules() {
        let boss = agent("1", "boss", None);
        let dev = agent("2", "dev", Some("1"));
        let team = vec![boss.clone(), dev.clone()];
        let p = PromptInput {
            agent: &boss,
            team: &team,
            skills: &[],
            memories: &[],
            inline_skills: &[],
            mcp_servers: &[],
            channel: "web",
            can_delegate: true,
            extra_context: "",
            now_utc: "2026-01-01 00:00",
        };
        let s = build(&p);
        assert!(s.contains("@dev"));
        assert!(s.contains("delegate_task"));
        assert!(s.contains(DEFAULT_PERSONA));
        let p2 = PromptInput { agent: &dev, can_delegate: false, ..p };
        let s2 = build(&p2);
        assert!(s2.contains("Your manager is **BOSS**"));
        assert!(!s2.contains("delegate_task"));
    }

    #[test]
    fn short_truncates_with_ellipsis() {
        assert_eq!(short("abc", 10), "abc");
        assert_eq!(short("abcdefghij", 5), "abcd…");
    }
}
