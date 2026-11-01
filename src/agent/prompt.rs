//! System prompt assembly. Stable sections come first (identity, team,
//! skills index, rules) so provider-side prompt caching gets a long common
//! prefix; volatile bits (memory recalled for this message, recent
//! conversations, time, attached context) go last.

use crate::db::{Agent, Memory, Skill};

pub const DEFAULT_PERSONA: &str = "You are Kongming, a calm and resourceful assistant. You think before you act, \
prefer verified facts over guesses, and say plainly when you do not know something. You keep answers short and \
useful. When a task is multi-step you work through it with your tools instead of describing what could be done.";

/// When to ask before acting and when to decide (Hermes' clarify rule).
pub const ASK_FIRST: &str = "When the goal, the target, or a step that can't be undone (sending, paying, deleting, \
publishing) is unclear, ask one short question first, offering your recommended option. Low-stakes details: decide \
yourself and say what you assumed.";

/// Hermes' tool-use enforcement, for models that narrate instead of acting.
pub const TOOL_RULES: &str = "Every reply either calls a tool or gives the user a final answer. Don't describe what \
you would do — do it. Never invent tool output.";

pub const RECALL_HINT: &str =
    "If the user refers to something earlier, check these before asking them to repeat themselves.";

const MAX_DESC: usize = 96;
/// The Memory section's size, in characters.
const MEMORY_CHARS: usize = 8000;
const MEMORY_CHARS_SMALL: usize = 2500;
/// One line of the Recent conversations section.
const RECENT_LINE: usize = 150;

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
    /// The user's local time with its offset, when one is set (Settings → Channels).
    pub local_time: &'a str,
    /// Small-model mode: shorter sections.
    pub small: bool,
    /// Add the tool-use enforcement rules (small and loose open models).
    pub tool_rules: bool,
    /// Whether `setup` is offered this turn.
    pub setup: bool,
    /// MCP servers and their tool counts when the tools are behind
    /// `tool_search` / `tool_call`; empty when they are offered directly.
    pub mcp_catalog: &'a [(String, usize)],
    /// Lines of the Recent conversations section (see `recent_line`).
    pub recent: &'a [String],
}

/// One line of the Recent conversations section: the title and what it was
/// about, with the session id when the model can read it.
pub fn recent_line(title: &str, gist: &str, session_id: Option<&str>) -> String {
    let title = if title.trim().is_empty() { "(untitled)" } else { title.trim() };
    let gist = gist.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = if gist.is_empty() || gist == title { title.to_string() } else { format!("{title}: {gist}") };
    let line = short(&line, RECENT_LINE);
    match session_id {
        Some(id) => format!("- {line} (session {id})"),
        None => format!("- {line}"),
    }
}

/// Memory lines up to `cap` characters, in the order given; one that does
/// not fit is skipped and a shorter one after it may still go in. `ids`
/// adds each memory's id, for a model that may update them.
fn memory_lines(memories: &[Memory], cap: usize, ids: bool) -> String {
    let mut out = String::new();
    for m in memories {
        let line = if ids {
            format!("- ({}) [{}] {}\n", m.id, m.kind, m.content.trim())
        } else {
            format!("- [{}] {}\n", m.kind, m.content.trim())
        };
        if out.len() + line.len() <= cap {
            out.push_str(&line);
        }
    }
    out
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
        if a.is_default {
            s.push_str(
                "You lead a team of agents and are the only one the user talks to. The others take work only from their \
manager; the user can watch their conversations but not message them, so everything goes through you and you answer for \
the team's work. When the user names an agent with `@handle`, they mean that agent's work: hand it on, follow up or fix it \
yourself. When something went wrong, own it: correct the result, adjust the report with `agent_manage` so it does not \
happen again, or retire it.\n",
            );
        } else {
            s.push_str("You are part of a team of agents and take work only from your manager; the user does not talk to you directly.\n");
        }
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
            s.push_str("Other agents (you cannot delegate to them):\n");
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

    // 4b. Reaching the user between conversations (the lead only)
    if a.is_default && p.small {
        s.push_str("# Reaching the user\n");
        s.push_str(
            "`notify_user` messages the user on Telegram; `schedule` plans reminders and checks for later. Message \
only what is new and useful to them.\n\n",
        );
    } else if a.is_default {
        s.push_str("# Reaching the user\n");
        s.push_str(
            "You look out for the user between conversations, not only when spoken to. `notify_user` messages them on \
Telegram; `schedule` plans work for later. When they ask to be reminded at a time, schedule it with notify `always`. When \
they ask to be told when something happens (a price, a release, a reply, a page change, an event), schedule a check with \
notify `if_needed` at a sensible interval and save a `watch` memory saying what and why. When you start long work they \
are waiting for outside a live chat, tell them the result with `notify_user` when it is done. Save the topics, products, \
people and projects they keep coming back to as `interest` memories: periodic check-ins use them to spot news worth \
sending. Message only what is new, timely and clearly useful to them; never small talk, never the same thing twice.\n\n",
        );
    }

    // 4c. Setting things up from what the user pastes (the lead, with the user)
    let setup = p.setup && a.is_default && matches!(p.channel, "web" | "telegram");
    if setup && p.small {
        s.push_str("# Setup\n");
        s.push_str(
            "Configure what the user pastes with `setup`. Keys reach you as `[secret:<id> <kind> …1234]`: pass that \
reference as the value, never repeat a key, and confirm in one line what changed.\n\n",
        );
    } else if setup {
        s.push_str("# Setup\n");
        s.push_str(
            "You set kongming up for the user with `setup`; they should not need the Settings pages. When they paste \
setup material, work out what it is for and configure it right away, then confirm in a line or two what changed. Keys and \
tokens they paste reach you as `[secret:<id> <kind> …1234]`: the value is in the vault, so pass the reference itself to \
`setup` and never ask them to paste it again unless setup says it expired. Recognize: an LLM API key (with its provider, \
base URL or model) → `llm`; a Telegram bot token (digits:letters) or their Telegram user id → `telegram`; an MCP config \
JSON or an MCP server URL → `mcp_add`; a paid-work site key → `earn` with its provider; a SKILL.md or a link to one → \
`skill_import`; the address kongming runs at → `public_url`. Check `setup` `show` first when it is unclear what a key is \
for, and ask only when it is still ambiguous (an `sk-` key fits several OpenAI-compatible providers). Never repeat a key \
back. The `setup-from-paste` skill has the details.\n\n",
        );
    }

    // 5. Tools / MCP
    s.push_str("# Using tools\n");
    s.push_str(
        "When a tool call fails, fix it before anything else: read the error, then supply the missing arguments or correct \
the wrong ones and call again. Look up values you lack with other tools first (for example the account, owner or repository \
behind a request) or take them from the conversation; try another tool when one cannot do the job. Give up only after real \
attempts, then tell the user in plain words what failed and what you need. Never answer with raw tool output; summarize it.\n",
    );
    s.push_str(ASK_FIRST);
    s.push('\n');
    if p.tool_rules {
        s.push_str(TOOL_RULES);
        s.push('\n');
    }
    s.push('\n');
    if !p.mcp_catalog.is_empty() {
        s.push_str("# Connected services\n");
        s.push_str(&format!(
            "MCP tools from: {}. They are not listed one by one: find one with `tool_search` (say what you want to \
do) and run it with `tool_call`.\n\n",
            super::bridge::catalog(p.mcp_catalog)
        ));
    } else if !p.mcp_servers.is_empty() {
        s.push_str("# Connected services\n");
        s.push_str(&format!(
            "MCP tools are available from: {}. Their tool names start with `mcp__`.\n\n",
            p.mcp_servers.join(", ")
        ));
    }

    // 6. Channel conventions
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
        "proactive" => s.push_str(
            "This is a proactive check-in without the user present. The only way to reach them is `notify_user`; your \
final answer is a one-line log entry nobody is notified about.\n",
        ),
        _ => s.push_str("This conversation is in the kongming web app. Markdown is rendered, including code blocks and tables.\n"),
    }

    // 7. What changes from message to message: memory recalled for this one,
    // the other recent conversations, the time.
    let memory = memory_lines(p.memories, if p.small { MEMORY_CHARS_SMALL } else { MEMORY_CHARS }, false);
    if !memory.is_empty() {
        s.push_str("\n# Memory\n");
        s.push_str(&memory);
    }
    if !p.recent.is_empty() {
        s.push_str("\n# Recent conversations\n");
        for line in p.recent {
            s.push_str(line);
            s.push('\n');
        }
        s.push_str(RECALL_HINT);
        s.push('\n');
    }
    if !memory.is_empty() || !p.recent.is_empty() {
        s.push('\n');
    }
    s.push_str(&format!("Current time: {} (UTC).", p.now_utc));
    if !p.local_time.is_empty() {
        s.push_str(&format!(" The user's local time: {}.", p.local_time));
    }
    s.push('\n');

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

/// The reflection's answer when nothing is worth keeping.
pub const NOTHING_TO_SAVE: &str = "Nothing to save";

/// Prompt for the post-turn reflection pass (the "keeps improving itself"
/// part). It runs with only `skill_manage` and `memory` as tools, and sees
/// the saved memories (up to `memory_chars`) so it updates them instead of
/// saving them again.
pub fn reflection_prompt(agent: &Agent, skills: &[Skill], memories: &[Memory], memory_chars: usize) -> String {
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
Save an INTEREST memory (kind `interest`) for a topic, product, person, place or project the user clearly follows or keeps \
asking about, and a WATCH memory (kind `watch`) when they asked to be told about something later; check-ins use both to \
decide what is worth messaging them about.\n\
Write memories as facts (\"User prefers metric units\"), not as instructions to yourself.\n\
Do NOTHING when the conversation was small talk, a one-off question, or already covered by existing skills and memory. \
Never store secrets (tokens, passwords). Do not capture: failures of the environment (a server down, a timeout, a \
missing key), negative claims about tools (\"tool X does not work\"), dead ends that were never resolved, or the details \
of a one-off task.\n\n");
    let saved = memory_lines(memories, memory_chars, true);
    if !saved.is_empty() {
        s.push_str("Saved memories (update one with its id instead of adding it again):\n");
        s.push_str(&saved);
        s.push('\n');
    }
    if !skills.is_empty() {
        s.push_str("Existing skills:\n");
        for k in skills.iter().filter(|k| k.enabled) {
            // Older skills saved with no real body: worth an update when this conversation shows how.
            let thin = super::tools::thin_body(&k.body, &k.description).map_or("", |_| " [EMPTY BODY: rewrite it]");
            s.push_str(&format!("- {}: {}{thin}\n", k.name, short(&k.description, MAX_DESC)));
        }
        s.push('\n');
    }
    s.push_str(&format!(
        "Use the tools if there is something worth keeping, then answer with one short line describing what you saved. \
When nothing is worth saving, answer exactly: {NOTHING_TO_SAVE}"
    ));
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
        let boss = Agent { is_default: true, ..agent("1", "boss", None) };
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
            local_time: "",
            small: false,
            tool_rules: false,
            setup: true,
            mcp_catalog: &[],
            recent: &[],
        };
        let s = build(&p);
        assert!(s.contains("@dev"));
        assert!(s.contains("delegate_task"));
        assert!(s.contains(DEFAULT_PERSONA));
        assert!(s.contains("the only one the user talks to"), "the lead answers to the user: {s}");
        assert!(s.contains("# Reaching the user") && s.contains("notify_user"), "{s}");
        let p2 = PromptInput { agent: &dev, can_delegate: false, ..p };
        let s2 = build(&p2);
        assert!(s2.contains("Your manager is **BOSS**"));
        assert!(!s2.contains("delegate_task"));
        assert!(s2.contains("the user does not talk to you directly"), "{s2}");
        assert!(!s2.contains("# Reaching the user"), "only the lead reaches the user");
        assert!(s.contains("# Setup") && !s2.contains("# Setup"), "only the lead sets things up");
        let s3 = build(&PromptInput { channel: "delegation", ..p });
        assert!(!s3.contains("# Setup"));
        assert!(s.contains(ASK_FIRST) && !s.contains(TOOL_RULES), "{s}");
        assert!(!build(&PromptInput { setup: false, ..p }).contains("# Setup"), "only while setup is offered");
    }

    fn memory(kind: &str, content: &str) -> Memory {
        Memory {
            id: "m1".into(),
            agent_id: None,
            kind: kind.into(),
            content: content.into(),
            keywords: vec![],
            importance: 3,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn small_mode_is_shorter_and_volatile_sections_come_last() {
        let lead = Agent { is_default: true, ..agent("1", "kongming", None) };
        let team = vec![lead.clone()];
        let memories = vec![memory("preference", "User likes tea"), memory("fact", &"x".repeat(3000))];
        let recent = vec![recent_line("Trip plan", "Book a hotel in Kyoto for May", None)];
        let catalog = vec![("GitHub".to_string(), 40)];
        let p = PromptInput {
            agent: &lead,
            team: &team,
            skills: &[],
            memories: &memories,
            inline_skills: &[],
            mcp_servers: &[],
            channel: "telegram",
            can_delegate: false,
            extra_context: "",
            now_utc: "2026-01-01 00:00",
            local_time: "",
            small: false,
            tool_rules: false,
            setup: true,
            mcp_catalog: &catalog,
            recent: &recent,
        };
        let normal = build(&p);
        let small = build(&PromptInput { small: true, tool_rules: true, ..p });
        assert!(small.len() + 1500 < normal.len(), "{} vs {}", small.len(), normal.len());
        assert!(small.contains("# Setup") && small.contains("# Reaching the user") && small.contains(TOOL_RULES));
        // The long memory fits the normal cap only.
        assert!(normal.contains(&"x".repeat(3000)) && !small.contains(&"x".repeat(3000)));
        assert!(small.contains("- [preference] User likes tea"), "{small}");
        // Memory, recent conversations and the time come after every stable section.
        for s in [&normal, &small] {
            let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("no {needle} in {s}"));
            assert!(at("# Conversation") < at("# Memory"));
            assert!(at("# Memory") < at("# Recent conversations"));
            assert!(at("- Trip plan: Book a hotel in Kyoto for May") < at(RECALL_HINT));
            assert!(at(RECALL_HINT) < at("Current time:"));
            assert!(at("GitHub (40 tools)") < at("# Conversation") && at("`tool_search`") > 0);
        }
    }

    #[test]
    fn recent_lines_are_short() {
        let line = recent_line("Trip", &"word ".repeat(100), Some("s1"));
        assert!(line.starts_with("- Trip: word word") && line.ends_with("… (session s1)"), "{line}");
        assert_eq!(recent_line("", "", None), "- (untitled)");
        assert_eq!(recent_line("Hi", "hi", None), "- Hi: hi");
    }

    #[test]
    fn reflection_sees_saved_memories_and_the_do_not_capture_list() {
        let a = agent("1", "kongming", None);
        let s = reflection_prompt(&a, &[], &[memory("fact", "User lives in Jakarta")], 1000);
        assert!(s.contains("- (m1) [fact] User lives in Jakarta"), "{s}");
        assert!(s.contains("Do not capture") && s.ends_with(NOTHING_TO_SAVE), "{s}");
    }

    #[test]
    fn short_truncates_with_ellipsis() {
        assert_eq!(short("abc", 10), "abc");
        assert_eq!(short("abcdefghij", 5), "abcd…");
    }
}
