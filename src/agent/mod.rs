//! The agentic loop. One `run_turn` call takes a user message for a session,
//! assembles the prompt, streams the model, executes tool calls until the model
//! stops, persists every round, and (optionally) runs a reflection pass that
//! may save skills and memories. Events go to the bus for whoever is listening.

pub mod mention;
pub mod paste;
pub mod prompt;
pub mod setup;
#[cfg(feature = "native")]
pub mod shell;
/// The `shell` tool runs a local process: native builds only. Without a
/// `Shell` the tool is left out of every tool list.
#[cfg(not(feature = "native"))]
pub mod shell {
    pub const DEFAULT_TIMEOUT: u64 = 30;
    pub const MAX_TIMEOUT: u64 = 300;

    #[derive(Debug, Clone)]
    pub struct Shell(());

    impl Shell {
        pub fn from_config(_shell: &str) -> Option<Shell> {
            None
        }

        pub async fn run(&self, _command: &str, _timeout_secs: u64) -> anyhow::Result<(String, bool)> {
            anyhow::bail!("there is no local shell in this build")
        }
    }
}
pub mod tools;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::{new_id, Agent, Message, Part, Session, Skill};
use crate::llm::{self, ChatMessage, ChatRequest, LlmEvent, ToolSpec};
use crate::state::{Event, Shared};
use tools::ToolCtx;

const MAX_TOOL_RESULT_CHARS: usize = 16_000;
/// Appended to failed tool results: weaker models otherwise stop at the first
/// error and hand it to the user instead of fixing the call.
const RETRY_HINT: &str = "[kongming] This call failed. Work out why from the error above and fix it yourself: add \
missing arguments or correct wrong ones (find the values with another tool, such as a list or search call, or take them \
from the conversation), or use a different tool. Ask the user only for what you cannot find out yourself.";
/// Tool results from earlier step blocks, once a turn continues past the limit.
const OLD_TOOL_RESULT_CHARS: usize = 1_500;
const KEEP_RECENT_MESSAGES: usize = 12;
const LLM_RETRIES: usize = 3;

/// Outcome of a turn, for callers that need the text (Telegram, delegation, cron).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnResult {
    pub text: String,
    pub agent: Option<Agent>,
    pub stop_reason: String,
    pub error: Option<String>,
}

/// Options for a turn beyond the text itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnOptions {
    /// Delegation depth (0 for a human turn).
    pub depth: usize,
    /// Skip the reflection pass (delegations and compactions).
    pub no_reflect: bool,
    /// Force a specific agent regardless of mentions.
    pub agent_override: Option<Agent>,
}

/// The user talks only to the lead agent; the rest of the team takes work from
/// it. A person's message goes to Kongming whatever `@handle` it names, and the
/// conversations Kongming hands to its reports are read-only.
pub async fn user_agent(state: &Shared, session: &Session) -> Result<Agent> {
    if session.channel == "delegation" {
        return Err(anyhow!(
            "this conversation is a task handed to a team member and is read-only; talk to the lead agent instead"
        ));
    }
    ensure_default_agent(state).await
}

fn now_utc_string() -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    crate::platform::now_utc().format(&fmt).unwrap_or_default()
}

/// History as the model sees it: the rolling summary (if any) followed by the
/// messages after `summary_upto`.
fn history_to_chat(session: &Session, msgs: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    if let Some(sum) = session.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push(ChatMessage::text("user", format!("[Summary of the earlier part of this conversation]\n{sum}")));
        // Compaction inside a long task can leave an assistant message first.
        let first = msgs.iter().find(|m| m.seq > session.summary_upto && m.role != "system");
        if first.is_none_or(|m| m.role == "user") {
            out.push(ChatMessage::text("assistant", "Understood, I have the summary of our earlier conversation."));
        }
    }
    for m in msgs.iter().filter(|m| m.seq > session.summary_upto) {
        if m.role == "system" {
            continue;
        }
        out.push(ChatMessage { role: m.role.clone(), parts: m.parts.clone() });
    }
    out
}

/// The model's view of the session. The request being worked on keeps its
/// live form; once compaction has folded it into the summary, it is restated
/// after the summary so a long task does not lose what it was asked to do.
async fn load_chat(state: &Shared, session: &Session, request: (i64, &str)) -> Result<Vec<ChatMessage>> {
    let (seq, text) = request;
    let mut msgs = state.db.messages(&session.id, session.summary_upto).await?;
    msgs.retain(|m| m.seq > session.summary_upto);
    if let Some(m) = msgs.iter_mut().find(|m| m.seq == seq) {
        m.parts = vec![Part::Text { text: text.to_string() }];
    }
    let mut chat = history_to_chat(session, &msgs);
    if seq <= session.summary_upto {
        if let Some(Part::Text { text: sum }) = chat.first_mut().and_then(|m| m.parts.first_mut()) {
            sum.push_str(&format!("\n\n[The request you are working on now]\n{text}"));
        }
    }
    Ok(chat)
}

/// Rough size of a request: system prompt, tool schemas and history.
fn request_tokens(system: &str, tools: &[ToolSpec], chat: &[ChatMessage]) -> usize {
    let tools: usize = tools
        .iter()
        .map(|t| 8 + llm::estimate_tokens(&t.description) + llm::estimate_tokens(&t.parameters.to_string()))
        .sum();
    llm::estimate_tokens(system) + tools + chat.iter().map(llm::estimate_message_tokens).sum::<usize>()
}

/// Provider errors that mean the request no longer fits the model's context.
fn is_context_overflow(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "context length",
        "context window",
        "context size",
        "maximum context",
        "exceeds the available context",
        "prompt is too long",
        "input is too long",
        "too many tokens",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// Fold older history into the session summary. Keeps the last `keep`
/// messages verbatim and may cut inside a turn (a long task has one user
/// message and many tool rounds), but never between a tool call and its
/// results. Returns whether anything was folded.
async fn compact(state: &Shared, session: &mut Session, turn_id: &str, model: &str, keep: usize) -> Result<bool> {
    let msgs = state.db.messages(&session.id, session.summary_upto).await?;
    let live: Vec<&Message> = msgs.iter().filter(|m| m.seq > session.summary_upto && m.role != "system").collect();
    if live.len() <= keep {
        return Ok(false);
    }
    let mut cut = live.len() - keep;
    while cut > 0 && live[cut].role == "tool" {
        cut -= 1;
    }
    if cut == 0 {
        return Ok(false);
    }
    let upto = live[cut - 1].seq;
    state.emit(Event::Status {
        session_id: session.id.clone(),
        turn_id: turn_id.into(),
        text: "Summarizing older messages to stay within the context budget".into(),
    });
    let mut transcript = String::new();
    if let Some(s) = &session.summary {
        transcript.push_str("Previous summary:\n");
        transcript.push_str(s);
        transcript.push_str("\n\n");
    }
    for m in &live[..cut] {
        for p in &m.parts {
            match p {
                Part::Text { text } => transcript.push_str(&format!("[{}] {}\n", m.role, text)),
                Part::ToolCall { name, args, .. } => transcript
                    .push_str(&format!("[tool call] {name} {}\n", crate::mcp::truncate(&args.to_string(), 300))),
                Part::ToolResult { name, content, .. } => {
                    transcript.push_str(&format!("[tool result {name}] {}\n", crate::mcp::truncate(content, 600)))
                }
                Part::Thinking { .. } => {}
            }
        }
    }
    let (_, provider) = state.provider().await?;
    let req = ChatRequest {
        model: model.to_string(),
        system: prompt::compaction_prompt().to_string(),
        // The previous summary leads and the newest messages close it: keep both.
        messages: vec![ChatMessage::text("user", head_tail(&transcript, 60_000))],
        tools: vec![],
        max_tokens: 1500,
        temperature: Some(0.2),
        effort: Some("low".into()),
    };
    let summary = collect_text(provider.stream(req).await?).await?;
    if summary.trim().is_empty() {
        return Ok(false);
    }
    state.db.set_session_summary(&session.id, &summary, upto).await?;
    tracing::info!(session = %session.id, upto, "compacted older messages into the summary");
    session.summary = Some(summary);
    session.summary_upto = upto;
    Ok(true)
}

/// `compact`, then reload the history if it changed. Failures are logged:
/// the turn carries on with the history it has.
async fn compact_chat(
    state: &Shared,
    session: &mut Session,
    turn_id: &str,
    model: &str,
    keep: usize,
    request: (i64, &str),
    chat: &mut Vec<ChatMessage>,
) -> bool {
    match compact(state, session, turn_id, model, keep).await {
        Ok(true) => match load_chat(state, session, request).await {
            Ok(c) => {
                *chat = c;
                true
            }
            Err(e) => {
                tracing::warn!("reloading history after compaction failed: {e:#}");
                false
            }
        },
        Ok(false) => false,
        Err(e) => {
            tracing::warn!("compaction failed: {e:#}");
            false
        }
    }
}

/// Drain a stream into its text (for compaction, reflection, titles).
pub async fn collect_text(mut stream: llm::EventStream) -> Result<String> {
    let mut out = String::new();
    while let Some(ev) = stream.next().await {
        match ev? {
            LlmEvent::TextDelta(t) => out.push_str(&t),
            LlmEvent::Done { .. } => break,
            _ => {}
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnPlan {
    agent: Agent,
    system: String,
    tools: Vec<ToolSpec>,
    mcp_names: Vec<String>,
    model: String,
    can_delegate: bool,
}

async fn plan(state: &Shared, session: &Session, agent: Agent, mentions: &mention::Mentions) -> Result<TurnPlan> {
    let team = state.db.agents().await?;
    let skills = state.db.skills().await?;
    let memories = state.db.memories(Some(&agent.id), 40).await?;
    let can_delegate = team.iter().any(|t| t.manager_id.as_deref() == Some(agent.id.as_str()));

    // Inline skills: pinned + mentioned.
    let mut inline: Vec<Skill> = Vec::new();
    for name in agent.pinned_skills.iter().chain(mentions.skills.iter()) {
        if let Some(k) = skills.iter().find(|k| &k.name == name && k.enabled) {
            if !inline.iter().any(|x| x.id == k.id) {
                inline.push(k.clone());
                let _ = state.db.mark_skill_used(&k.name).await;
            }
        }
    }
    // Context from other sessions.
    let mut extra = String::new();
    for sid in &mentions.sessions {
        if let Ok(t) = tools::session_transcript(state, sid, 20).await {
            extra.push_str(&crate::mcp::truncate(&t, 6000));
            extra.push_str("\n\n");
        }
    }

    // Tools: built-ins + MCP (restricted per agent, plus @mcp: mentions).
    let mut tool_specs = tools::builtin_specs(&agent, can_delegate, state.shell.as_ref());
    // Earnings: the lead agent, while switched on.
    if agent.is_default
        && agent.tools.as_ref().is_none_or(|t| t.iter().any(|n| n == "earn"))
        && crate::earn::settings(state).await.enabled
    {
        tool_specs.push(crate::earn::tool_spec());
    }
    // Setup: the lead agent, in a conversation with the user.
    if setup::offered(&agent, &session.channel) {
        tool_specs.push(setup::spec());
    }
    let mut allowed = agent.mcp_servers.clone();
    if let Some(a) = &mut allowed {
        a.extend(mentions.mcp.iter().cloned());
    }
    let mcp_tools = state.mcp.tools(allowed.as_deref()).await.unwrap_or_default();
    let mut mcp_names: Vec<String> = mcp_tools.iter().map(|t| t.server_name.clone()).collect();
    mcp_names.sort();
    mcp_names.dedup();
    tool_specs.extend(mcp_tools.into_iter().map(|t| t.spec));

    let (pcfg, _) = state.provider().await?;
    let model = agent.model.clone().filter(|m| !m.is_empty()).unwrap_or(pcfg.model.clone());
    let proactive = crate::proactive::settings(state).await;
    let local_time = match proactive.utc_offset_minutes {
        0 => String::new(),
        _ => proactive.local_time(crate::db::now_ms()),
    };
    let system = prompt::build(&prompt::PromptInput {
        agent: &agent,
        team: &team,
        skills: &skills,
        memories: &memories,
        inline_skills: &inline,
        mcp_servers: &mcp_names,
        channel: &session.channel,
        can_delegate,
        extra_context: &extra,
        now_utc: &now_utc_string(),
        local_time: &local_time,
    });
    Ok(TurnPlan { agent, system, tools: tool_specs, mcp_names, model, can_delegate })
}

/// Call the model with retries on transport/5xx errors. Returns the assistant
/// parts, the stop reason and usage.
async fn model_round(
    state: &Shared,
    session_id: &str,
    turn_id: &str,
    req: ChatRequest,
    cancel: &AtomicBool,
) -> Result<(Vec<Part>, String, (i64, i64))> {
    let (_, provider) = state.provider().await?;
    let mut last_err = None;
    for attempt in 0..LLM_RETRIES {
        if attempt > 0 {
            let wait = Duration::from_millis(800 * (1 << attempt));
            state.emit(Event::Status {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                text: format!("Model request failed, retrying ({}/{})", attempt + 1, LLM_RETRIES),
            });
            crate::platform::sleep(wait).await;
        }
        let stream = match provider.stream(req.clone()).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("HTTP 4") && !msg.contains("HTTP 429") && !msg.contains("HTTP 408") {
                    return Err(e);
                }
                last_err = Some(e);
                continue;
            }
        };
        match consume(state, session_id, turn_id, stream, cancel).await {
            Ok(r) => return Ok(r),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("cancelled") {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("model unavailable")))
}

async fn consume(
    state: &Shared,
    session_id: &str,
    turn_id: &str,
    mut stream: llm::EventStream,
    cancel: &AtomicBool,
) -> Result<(Vec<Part>, String, (i64, i64))> {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut parts: Vec<Part> = Vec::new();
    let mut stop = String::from("stop");
    let mut usage = (0i64, 0i64);
    let mut streamed_any = false;
    loop {
        let item = crate::platform::timeout(Duration::from_secs(180), stream.next())
            .await
            .map_err(|_| anyhow!("model stream stalled"))?;
        let Some(item) = item else { break };
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        match item? {
            LlmEvent::TextDelta(t) => {
                streamed_any = true;
                text.push_str(&t);
                state.emit(Event::Delta { session_id: session_id.into(), turn_id: turn_id.into(), text: t });
            }
            LlmEvent::ThinkingDelta(t) => {
                thinking.push_str(&t);
                state.emit(Event::ReasoningDelta { session_id: session_id.into(), turn_id: turn_id.into(), text: t });
            }
            LlmEvent::ToolCall { id, name, args } => {
                parts.push(Part::ToolCall { id, name, args });
            }
            LlmEvent::Usage { prompt_tokens, completion_tokens } => usage = (prompt_tokens, completion_tokens),
            LlmEvent::Done { stop_reason } => {
                stop = stop_reason;
                break;
            }
        }
    }
    let _ = streamed_any;
    let mut out = Vec::new();
    if !thinking.trim().is_empty() {
        out.push(Part::Thinking { text: thinking });
    }
    if !text.is_empty() {
        out.push(Part::Text { text });
    }
    out.extend(parts);
    Ok((out, stop, usage))
}

/// Execute one tool call (built-in or MCP) and return the result part.
async fn execute_tool(ctx: &ToolCtx<'_>, id: &str, name: &str, args: &Value) -> Part {
    let result: Result<(String, bool)> = match tools::run_builtin(ctx, name, args).await {
        Some(r) => r,
        None => match ctx.state.mcp.find_tool(name).await {
            Ok(Some(t)) => ctx.state.mcp.call(&t.server_id, &t.tool, args.clone()).await,
            Ok(None) => Err(anyhow!("unknown tool '{name}'")),
            Err(e) => Err(e),
        },
    };
    result_part(id, name, result)
}

/// A tool's outcome as the result part the model sees.
fn result_part(id: &str, name: &str, result: Result<(String, bool)>) -> Part {
    let (content, is_error) = match result {
        Ok((c, e)) => (c, e),
        Err(e) => (format!("error: {e:#}"), true),
    };
    let content = crate::mcp::truncate(&content, MAX_TOOL_RESULT_CHARS);
    let content = if is_error { format!("{content}\n\n{RETRY_HINT}") } else { content };
    Part::ToolResult { id: id.to_string(), name: name.to_string(), content, is_error }
}

/// Catch malformed calls before they reach the tool and answer with an error
/// the model can act on: weaker models send invalid JSON, drop required
/// arguments, or wrap another tool's call inside the arguments.
pub fn check_args(spec: Option<&ToolSpec>, args: &Value) -> Option<String> {
    if let Some(raw) = args.get("_raw") {
        return Some(format!(
            "the arguments were not valid JSON ({}); call the tool again with a JSON object",
            crate::mcp::truncate(raw.as_str().unwrap_or(""), 200)
        ));
    }
    let spec = spec?;
    let schema = &spec.parameters;
    let props = schema.get("properties").and_then(Value::as_object);
    let given: Vec<&str> = args.as_object().map(|a| a.keys().map(String::as_str).collect()).unwrap_or_default();
    let missing: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).filter(|k| !given.contains(k)).collect())
        .unwrap_or_default();
    let unknown: Vec<&str> = match props {
        Some(p) if !p.is_empty() => given.iter().copied().filter(|k| !p.contains_key(*k)).collect(),
        _ => vec![],
    };
    let strict = schema.get("additionalProperties") == Some(&Value::Bool(false));
    let all_unknown = !given.is_empty() && unknown.len() == given.len();
    if missing.is_empty() && (unknown.is_empty() || (!strict && !all_unknown)) {
        return None;
    }
    let mut msg = format!("invalid arguments for {}", spec.name);
    if !missing.is_empty() {
        msg.push_str(&format!("; missing required: {}", missing.join(", ")));
    }
    if !unknown.is_empty() {
        msg.push_str(&format!("; unknown: {}", unknown.join(", ")));
    }
    if let Some(other) = args.get("name").and_then(Value::as_str) {
        if ["name", "parameters", "arguments"].iter().any(|k| unknown.contains(k)) {
            msg.push_str(&format!(
                ". These arguments look like a call to '{other}' put inside {}: to use '{other}', call it directly as its own tool",
                spec.name
            ));
        }
    }
    msg.push_str(&format!(". Expected arguments (JSON schema): {}", crate::mcp::truncate(&schema.to_string(), 2000)));
    Some(msg)
}

/// Tool output the model has already acted on, cut down before a turn carries
/// on past its step limit so the context does not keep growing at full size.
fn shorten_tool_results(chat: &mut [ChatMessage]) {
    for part in chat.iter_mut().flat_map(|m| m.parts.iter_mut()) {
        if let Part::ToolResult { content, .. } = part {
            if content.chars().count() > OLD_TOOL_RESULT_CHARS {
                *content = crate::mcp::truncate(content, OLD_TOOL_RESULT_CHARS);
            }
        }
    }
}

/// Model-round settings from the provider config when the turn started. The
/// API key is not kept: a host may store the turn between steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoundParams {
    max_tokens: u32,
    temperature: Option<f32>,
    effort: Option<String>,
}

/// The tool calls of the current model round, while they run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolRound {
    calls: Vec<(String, String, Value)>,
    next: usize,
    results: Vec<Part>,
    used_tokens: usize,
}

/// A `delegate_task` call whose delegate works as a turn of its own, one step
/// at a time (turns started with `begin_turn`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Delegation {
    call_id: String,
    to: Agent,
    task_id: String,
    child: TurnState,
}

/// A turn between two steps. It serializes, so a host that cannot run a
/// whole turn at once (a Worker on the free plan) can store it and call
/// `step_turn` again from a later invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnState {
    session: Session,
    turn_id: String,
    opts: TurnOptions,
    /// Delegations run to the end inside their tool call (`run_turn`)
    /// instead of as steps of their own (`begin_turn`).
    inline_delegation: bool,
    plan: TurnPlan,
    params: RoundParams,
    request_seq: i64,
    request_text: String,
    stored_text: String,
    chat: Vec<ChatMessage>,
    iteration: usize,
    rounds_since_compact: usize,
    overflow_retried: bool,
    final_text: String,
    stop_reason: String,
    error: Option<String>,
    used_tools: bool,
    retried_empty: bool,
    seen: std::collections::HashSet<String>,
    block_calls: usize,
    repeats: usize,
    round: Option<ToolRound>,
    delegation: Option<Box<Delegation>>,
}

impl TurnState {
    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// The agent answering this turn.
    pub fn agent(&self) -> &Agent {
        &self.plan.agent
    }

    /// The session of the delegated task this turn is waiting on, if any.
    pub fn waiting_on(&self) -> Option<&str> {
        self.delegation.as_ref().map(|d| d.child.session.id.as_str())
    }
}

/// What `step_turn` did. Short-lived, so the size difference of the
/// variants does not matter.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Step {
    /// More to do: call `step_turn` again with this state.
    Continue(Box<TurnState>),
    /// The turn is over; `TurnEnd` was emitted.
    Done(TurnResult),
}

/// Run one user turn end to end. The user message is persisted here.
pub async fn run_turn(state: Shared, session: Session, user_text: String, opts: TurnOptions) -> Result<TurnResult> {
    let Some(cancel) = state.begin_turn(&session.id) else {
        return Err(anyhow!("a reply is already in progress for this conversation"));
    };
    let session_id = session.id.clone();
    let result = drive(&state, session, user_text, opts, &cancel).await;
    state.end_turn(&session_id);
    result
}

async fn drive(
    state: &Shared,
    session: Session,
    user_text: String,
    opts: TurnOptions,
    cancel: &AtomicBool,
) -> Result<TurnResult> {
    let mut ts = begin(state, session, user_text, opts, true).await?;
    loop {
        match step_turn(state, ts, cancel).await? {
            Step::Continue(next) => ts = *next,
            Step::Done(r) => return Ok(r),
        }
    }
}

/// Start a turn that the host drives with `step_turn`: persists the user
/// message, emits `UserMessage`/`TurnStart` and plans the turn (no model
/// round yet, but a compaction when the history is over budget). Delegated
/// tasks become steps of their own. The host keeps track of running turns
/// and cancellation itself (`AppState::begin_turn` is not used).
pub async fn begin_turn(state: &Shared, session: Session, user_text: String, opts: TurnOptions) -> Result<TurnState> {
    begin(state, session, user_text, opts, false).await
}

async fn begin(
    state: &Shared,
    mut session: Session,
    user_text: String,
    opts: TurnOptions,
    inline_delegation: bool,
) -> Result<TurnState> {
    let turn_id = new_id();
    let agent = match &opts.agent_override {
        Some(a) => a.clone(),
        None => user_agent(state, &session).await?,
    };
    // A person's message: pasted keys go to the vault before anything sees them.
    let user_text = match &opts.agent_override {
        None => paste::stash(state, &user_text).await,
        Some(_) => user_text,
    };
    let mentions = mention::parse(&user_text);
    if agent.id != session.agent_id {
        state.db.set_session_agent(&session.id, &agent.id).await?;
        session.agent_id = agent.id.clone();
    }

    // Persist the user message (cleaned of structured mentions).
    let stored_text = if mentions.text.is_empty() { user_text.clone() } else { mentions.text.clone() };
    let user_msg =
        state.db.append_message(&session.id, "user", &[Part::Text { text: stored_text.clone() }], None).await?;
    state.emit(Event::UserMessage { session_id: session.id.clone(), message: user_msg.clone() });
    if session.title.trim().is_empty() {
        let title = auto_title(&stored_text);
        state.db.update_session_title(&session.id, &title).await?;
        session.title = title;
    }
    if let Ok(Some(s)) = state.db.session(&session.id).await {
        state.emit(Event::SessionUpdated { session: s });
    }
    state.emit(Event::TurnStart { session_id: session.id.clone(), turn_id: turn_id.clone(), agent: agent.clone() });

    let plan = match plan(state, &session, agent, &mentions).await {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("{e:#}");
            state.emit(Event::TurnEnd {
                session_id: session.id.clone(),
                turn_id,
                stop_reason: "error".into(),
                error: Some(msg.clone()),
            });
            return Err(e);
        }
    };
    let (pcfg, _) = state.provider().await?;

    // Context: load, and fold older messages into the summary when the request
    // would not fit the budget. The agent is addressed as "@handle: text"; the
    // live copy of the request drops the prefix.
    let request_text = mention::strip_leading_handle(&stored_text, &plan.agent.handle);
    let request = (user_msg.seq, request_text.as_str());
    let mut chat = load_chat(state, &session, request).await?;
    if request_tokens(&plan.system, &plan.tools, &chat) >= state.cfg.context_tokens {
        compact_chat(state, &mut session, &turn_id, &plan.model, KEEP_RECENT_MESSAGES, request, &mut chat).await;
    }
    Ok(TurnState {
        session,
        turn_id,
        opts,
        inline_delegation,
        plan,
        params: RoundParams {
            max_tokens: pcfg.max_tokens.unwrap_or(8192),
            temperature: pcfg.temperature,
            effort: pcfg.effort.clone(),
        },
        request_seq: user_msg.seq,
        request_text,
        stored_text,
        chat,
        iteration: 0,
        // Inside the turn: compact again once enough new rounds have piled up;
        // in between, cut old tool output down instead.
        rounds_since_compact: KEEP_RECENT_MESSAGES / 2,
        overflow_retried: false,
        final_text: String::new(),
        stop_reason: String::from("stop"),
        error: None,
        used_tools: false,
        retried_empty: false,
        seen: Default::default(),
        block_calls: 0,
        repeats: 0,
        round: None,
        delegation: None,
    })
}

/// Advance a turn by one step: one model round and the tool calls it asks
/// for. While a delegated task runs (turns from `begin_turn`), a step is one
/// step of the delegate's turn instead; when that ends, the rest of the
/// manager's tool calls run. `cancel` stops the turn at the next check.
pub fn step_turn<'a>(state: &'a Shared, ts: TurnState, cancel: &'a AtomicBool) -> BoxFuture<'a, Result<Step>> {
    Box::pin(async move {
        let mut ts = ts;
        if let Some(d) = ts.delegation.take() {
            let child_id = d.child.session.id.clone();
            let mut d = *d;
            let res = match step_turn(state, d.child, cancel).await {
                Ok(Step::Continue(child)) => {
                    d.child = *child;
                    ts.delegation = Some(Box::new(d));
                    return Ok(Step::Continue(Box::new(ts)));
                }
                Ok(Step::Done(r)) => Ok(r),
                Err(e) => Err(e),
            };
            let summary = close_delegation(state, &d.to, &d.task_id, &child_id, res).await;
            let part = result_part(&d.call_id, "delegate_task", summary.map(|t| (t, false)));
            emit_result(state, &ts, &part);
            ts.round.get_or_insert_with(ToolRound::default).results.push(part);
            return run_calls(state, ts, cancel).await;
        }
        if ts.round.is_some() {
            return run_calls(state, ts, cancel).await;
        }
        model_step(state, ts, cancel).await
    })
}

fn block_and_limit(state: &Shared) -> (usize, usize) {
    // The step limit is a checkpoint, not the end: a long task carries on by
    // itself for `auto_continue` more blocks unless it is going in circles.
    let block = state.cfg.max_iterations.max(1);
    (block, block * (1 + state.cfg.auto_continue))
}

async fn model_step(state: &Shared, mut ts: TurnState, cancel: &AtomicBool) -> Result<Step> {
    let (_, limit) = block_and_limit(state);
    if ts.iteration >= limit {
        return finish(state, ts).await;
    }
    if cancel.load(Ordering::Relaxed) {
        ts.stop_reason = "cancelled".into();
        return finish(state, ts).await;
    }
    let req = ChatRequest {
        model: ts.plan.model.clone(),
        system: ts.plan.system.clone(),
        messages: ts.chat.clone(),
        tools: ts.plan.tools.clone(),
        max_tokens: ts.params.max_tokens,
        temperature: ts.params.temperature,
        effort: ts.params.effort.clone(),
    };
    let (parts, stop, usage) = match model_round(state, &ts.session.id, &ts.turn_id, req, cancel).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("{e:#}");
            // Too long for the model after all: fold harder and try once more.
            if is_context_overflow(&msg) && !ts.overflow_retried {
                ts.overflow_retried = true;
                tracing::warn!(session = %ts.session.id, "the model rejected the request as too long; compacting");
                let keep = KEEP_RECENT_MESSAGES / 3;
                let request = (ts.request_seq, ts.request_text.as_str());
                compact_chat(state, &mut ts.session, &ts.turn_id, &ts.plan.model, keep, request, &mut ts.chat).await;
                let fresh = ts.chat.len().saturating_sub(1);
                shorten_tool_results(&mut ts.chat[..fresh]);
                ts.rounds_since_compact = 0;
                ts.iteration += 1;
                return Ok(Step::Continue(Box::new(ts)));
            }
            if msg.contains("cancelled") {
                ts.stop_reason = "cancelled".into();
            } else {
                ts.stop_reason = "error".into();
                ts.error = Some(msg);
            }
            return finish(state, ts).await;
        }
    };
    if usage.0 > 0 || usage.1 > 0 {
        let _ =
            state.db.record_usage(Some(&ts.session.id), Some(&ts.plan.agent.id), &ts.plan.model, usage.0, usage.1).await;
    }
    let used_tokens = usize::try_from(usage.0 + usage.1).unwrap_or(0);
    let calls = parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolCall { id, name, args } => Some((id.clone(), name.clone(), args.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let has_text = parts.iter().any(|p| matches!(p, Part::Text { text } if !text.trim().is_empty()));
    if !has_text && calls.is_empty() {
        // No answer (only thinking, or nothing at all): ask once more, then
        // say so instead of ending the turn in silence.
        if !ts.retried_empty {
            ts.retried_empty = true;
            ts.iteration += 1;
            return Ok(Step::Continue(Box::new(ts)));
        }
        ts.stop_reason = "error".into();
        ts.error = Some(format!(
            "the model ({}) returned an empty reply{}; try again, or pick a stronger model in Settings → Models",
            ts.plan.model,
            if ts.used_tools { " after the tool results" } else { "" }
        ));
        return finish(state, ts).await;
    }
    let assistant_msg = state.db.append_message(&ts.session.id, "assistant", &parts, Some(&ts.plan.agent.id)).await?;
    state.emit(Event::Message { session_id: ts.session.id.clone(), message: assistant_msg });
    for p in &parts {
        if let Part::Text { text } = p {
            ts.final_text = text.clone();
        }
    }
    ts.chat.push(ChatMessage {
        role: "assistant".into(),
        parts: parts.iter().filter(|p| !matches!(p, Part::Thinking { .. })).cloned().collect(),
    });

    if calls.is_empty() {
        ts.stop_reason = stop;
        return finish(state, ts).await;
    }
    ts.used_tools = true;
    let n = calls.len();
    ts.round = Some(ToolRound { calls, next: 0, results: Vec::with_capacity(n), used_tokens });
    run_calls(state, ts, cancel).await
}

fn emit_result(state: &Shared, ts: &TurnState, part: &Part) {
    if let Part::ToolResult { id, name, content, is_error } = part {
        state.emit(Event::ToolResult {
            session_id: ts.session.id.clone(),
            turn_id: ts.turn_id.clone(),
            id: id.clone(),
            name: name.clone(),
            content: content.clone(),
            is_error: *is_error,
        });
    }
}

/// Run the round's remaining tool calls, then close the round.
async fn run_calls(state: &Shared, mut ts: TurnState, cancel: &AtomicBool) -> Result<Step> {
    let mut round = ts.round.take().unwrap_or_default();
    while round.next < round.calls.len() {
        let (id, name, args) = round.calls[round.next].clone();
        round.next += 1;
        ts.block_calls += 1;
        if !ts.seen.insert(format!("{name} {args}")) {
            ts.repeats += 1;
        }
        state.emit(Event::ToolCall {
            session_id: ts.session.id.clone(),
            turn_id: ts.turn_id.clone(),
            id: id.clone(),
            name: name.clone(),
            args: args.clone(),
        });
        let part = if cancel.load(Ordering::Relaxed) {
            Part::ToolResult { id: id.clone(), name: name.clone(), content: "cancelled by user".into(), is_error: true }
        } else if !ts.plan.tools.iter().any(|t| t.name == *name) {
            Part::ToolResult {
                id: id.clone(),
                name: name.clone(),
                content: format!("error: unknown tool {name}: use one of the tools you were given"),
                is_error: true,
            }
        } else if let Some(problem) = check_args(ts.plan.tools.iter().find(|t| t.name == *name), &args) {
            Part::ToolResult { id: id.clone(), name: name.clone(), content: format!("error: {problem}"), is_error: true }
        } else {
            let ctx = ToolCtx {
                state,
                agent: &ts.plan.agent,
                session: &ts.session,
                turn_id: &ts.turn_id,
                depth: ts.opts.depth,
                can_delegate: ts.plan.can_delegate,
            };
            if name == "delegate_task" && !ts.inline_delegation {
                match start_delegation(&ctx, &id, &args).await {
                    Ok(d) => {
                        ts.delegation = Some(Box::new(d));
                        ts.round = Some(round);
                        return Ok(Step::Continue(Box::new(ts)));
                    }
                    Err(e) => result_part(&id, &name, Err(e)),
                }
            } else {
                execute_tool(&ctx, &id, &name, &args).await
            }
        };
        emit_result(state, &ts, &part);
        round.results.push(part);
    }
    after_round(state, ts, round).await
}

/// Persist the tool results, keep the context in budget and apply the step
/// limit.
async fn after_round(state: &Shared, mut ts: TurnState, round: ToolRound) -> Result<Step> {
    let used_tokens = round.used_tokens;
    let results = round.results;
    // A report hired this turn can take work right away.
    let hired =
        results.iter().any(|p| matches!(p, Part::ToolResult { name, is_error: false, .. } if name == "agent_manage"));
    if hired && !ts.plan.can_delegate {
        ts.plan.can_delegate = true;
        ts.plan.tools.extend(
            tools::builtin_specs(&ts.plan.agent, true, None).into_iter().filter(|t| t.name == "delegate_task"),
        );
    }
    let tool_msg = state.db.append_message(&ts.session.id, "tool", &results, Some(&ts.plan.agent.id)).await?;
    state.emit(Event::Message { session_id: ts.session.id.clone(), message: tool_msg });
    ts.chat.push(ChatMessage { role: "tool".into(), parts: results });
    ts.rounds_since_compact += 1;
    let need = request_tokens(&ts.plan.system, &ts.plan.tools, &ts.chat).max(used_tokens);
    if need >= state.cfg.context_tokens {
        let compacted = ts.rounds_since_compact >= KEEP_RECENT_MESSAGES / 2 && {
            let request = (ts.request_seq, ts.request_text.as_str());
            compact_chat(state, &mut ts.session, &ts.turn_id, &ts.plan.model, KEEP_RECENT_MESSAGES, request, &mut ts.chat)
                .await
        };
        if compacted {
            ts.rounds_since_compact = 0;
        } else {
            let fresh = ts.chat.len().saturating_sub(1);
            shorten_tool_results(&mut ts.chat[..fresh]);
        }
    }
    let (block, limit) = block_and_limit(state);
    let step = ts.iteration + 1;
    ts.iteration = step;
    if step % block != 0 {
        return Ok(Step::Continue(Box::new(ts)));
    }
    // Half the calls of the block were exact repeats: the model is stuck.
    let looping = ts.repeats * 2 >= ts.block_calls;
    if step < limit && !looping {
        tracing::info!(session = %ts.session.id, step, "step limit reached; continuing automatically");
        shorten_tool_results(&mut ts.chat);
        ts.seen.clear();
        (ts.block_calls, ts.repeats) = (0, 0);
        return Ok(Step::Continue(Box::new(ts)));
    }
    ts.stop_reason = "max_iterations".into();
    let note = if looping {
        format!(
            "I stopped after {step} steps because I kept repeating the same tool calls. Tell me how to proceed, \
or to continue."
        )
    } else {
        format!("I used all {step} steps allowed for one turn. Tell me to continue and I will pick up where I stopped.")
    };
    let m = state
        .db
        .append_message(&ts.session.id, "assistant", &[Part::Text { text: note.clone() }], Some(&ts.plan.agent.id))
        .await?;
    state.emit(Event::Message { session_id: ts.session.id.clone(), message: m });
    ts.final_text = note;
    finish(state, ts).await
}

/// End the turn: `TurnEnd`, and a reflection pass in the background when the
/// turn did real work.
async fn finish(state: &Shared, ts: TurnState) -> Result<Step> {
    state.emit(Event::TurnEnd {
        session_id: ts.session.id.clone(),
        turn_id: ts.turn_id.clone(),
        stop_reason: ts.stop_reason.clone(),
        error: ts.error.clone(),
    });
    let _ = state.db.touch_session(&ts.session.id).await;

    // Reflection: learn from turns that did real work.
    if !ts.opts.no_reflect
        && ts.plan.agent.self_improve
        && ts.error.is_none()
        && ts.stop_reason != "cancelled"
        && should_reflect(state, &ts.session.id, ts.used_tools, &ts.stored_text).await
    {
        let st = state.clone();
        let agent = ts.plan.agent.clone();
        let sess = ts.session.clone();
        let model = ts.plan.model.clone();
        state.spawn(async move {
            if let Err(e) = reflect(st, agent, sess, model).await {
                tracing::warn!("reflection failed: {e:#}");
            }
        });
    }

    Ok(Step::Done(TurnResult {
        text: ts.final_text,
        agent: Some(ts.plan.agent),
        stop_reason: ts.stop_reason,
        error: ts.error,
    }))
}

/// A `delegate_task` call in a stepped turn: check it, open the task and
/// start the delegate's turn.
async fn start_delegation(ctx: &ToolCtx<'_>, call_id: &str, args: &Value) -> Result<Delegation> {
    let d = tools::check_delegate(ctx, args).await?;
    let (task_id, child, text) =
        open_delegation(ctx.state, ctx.agent, &d.to, ctx.session, &d.task, &d.context).await?;
    let child_id = child.id.clone();
    let opts = TurnOptions { depth: ctx.depth + 1, no_reflect: true, agent_override: Some(d.to.clone()) };
    match begin(ctx.state, child, text, opts, false).await {
        Ok(child) => Ok(Delegation { call_id: call_id.to_string(), to: d.to, task_id, child }),
        Err(e) => Err(close_delegation(ctx.state, &d.to, &task_id, &child_id, Err(e))
            .await
            .err()
            .unwrap_or_else(|| anyhow!("delegation to @{} failed", d.to.handle))),
    }
}

/// Reflect after tool-using turns, after user corrections, and every few
/// plain turns, without burning tokens on every message.
async fn should_reflect(state: &Shared, session_id: &str, used_tools: bool, user_text: &str) -> bool {
    let enabled = state
        .db
        .setting("reflection")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.get("enabled").and_then(Value::as_bool))
        .unwrap_or(true);
    if !enabled {
        return false;
    }
    let lower = user_text.to_ascii_lowercase();
    let correction =
        ["wrong", "not that", "actually", "instead", "no,", "remember", "always", "never", "from now on", "prefer"]
            .iter()
            .any(|k| lower.contains(k));
    if used_tools || correction {
        return true;
    }
    let n = state.db.message_count(session_id).await.unwrap_or(0);
    n > 0 && n % 16 == 0
}

/// The reflection pass: a short, tool-only call that may save skills/memory.
async fn reflect(state: Shared, agent: Agent, session: Session, model: String) -> Result<()> {
    let skills = state.db.skills().await?;
    let msgs = state.db.recent_messages(&session.id, 24).await?;
    let mut transcript = String::new();
    for m in &msgs {
        for p in &m.parts {
            match p {
                Part::Text { text } => {
                    transcript.push_str(&format!("[{}] {}\n", m.role, crate::mcp::truncate(text, 2000)))
                }
                Part::ToolCall { name, args, .. } => transcript
                    .push_str(&format!("[tool call] {name} {}\n", crate::mcp::truncate(&args.to_string(), 1000))),
                Part::ToolResult { name, content, is_error, .. } => transcript.push_str(&format!(
                    "[tool result {name}{}] {}\n",
                    if *is_error { " ERROR" } else { "" },
                    crate::mcp::truncate(content, 1500)
                )),
                Part::Thinking { .. } => {}
            }
        }
    }
    let system = prompt::reflection_prompt(&agent, &skills);
    let specs: Vec<ToolSpec> = tools::builtin_specs(&agent, false, None)
        .into_iter()
        .filter(|t| t.name == "skill_manage" || t.name == "memory" || t.name == "skill_view")
        .collect();
    let (pcfg, provider) = state.provider().await?;
    let mut chat = vec![ChatMessage::text(
        "user",
        format!("Conversation to review:\n\n{}", crate::mcp::truncate(&transcript, 30_000)),
    )];
    let ctx = ToolCtx {
        state: &state,
        agent: &agent,
        session: &session,
        turn_id: "reflection",
        depth: 1,
        can_delegate: false,
    };
    for _ in 0..4 {
        let req = ChatRequest {
            model: model.clone(),
            system: system.clone(),
            messages: chat.clone(),
            tools: specs.clone(),
            // Room for a whole skill body: a cut-off call arrives as invalid JSON.
            max_tokens: 8000,
            temperature: pcfg.temperature,
            effort: Some("low".into()),
        };
        let mut stream = provider.stream(req).await?;
        let mut text = String::new();
        let mut calls = Vec::new();
        while let Some(ev) = stream.next().await {
            match ev? {
                LlmEvent::TextDelta(t) => text.push_str(&t),
                LlmEvent::ToolCall { id, name, args } => calls.push((id, name, args)),
                LlmEvent::Done { .. } => break,
                _ => {}
            }
        }
        if calls.is_empty() {
            let note = text.trim();
            if !note.is_empty() && !note.eq_ignore_ascii_case("nothing to save") {
                let _ = state.db.log_improvement(Some(&agent.id), Some(&session.id), "reflection", None, note).await;
            }
            return Ok(());
        }
        let mut parts = Vec::new();
        let mut results = Vec::new();
        for (id, name, args) in calls {
            parts.push(Part::ToolCall { id: id.clone(), name: name.clone(), args: args.clone() });
            if specs.iter().any(|t| t.name == name) {
                results.push(execute_tool(&ctx, &id, &name, &args).await);
            } else {
                results.push(Part::ToolResult {
                    id,
                    name,
                    content: "error: not available here".into(),
                    is_error: true,
                });
            }
        }
        chat.push(ChatMessage { role: "assistant".into(), parts });
        chat.push(ChatMessage { role: "tool".into(), parts: results });
    }
    Ok(())
}

/// A manager hands work to a report: new child session, one turn, summary back.
pub fn run_delegation(
    state: Shared,
    from: Agent,
    to: Agent,
    parent: Session,
    task: String,
    context: String,
    depth: usize,
) -> futures_util::future::BoxFuture<'static, Result<String>> {
    Box::pin(async move {
        let (task_id, child, text) = open_delegation(&state, &from, &to, &parent, &task, &context).await?;
        let child_id = child.id.clone();
        let res = run_turn(
            state.clone(),
            child,
            text,
            TurnOptions { depth, no_reflect: true, agent_override: Some(to.clone()) },
        )
        .await;
        close_delegation(&state, &to, &task_id, &child_id, res).await
    })
}

/// Record the task and open the delegate's conversation. Returns the task id,
/// the new session and the delegate's first message.
async fn open_delegation(
    state: &Shared,
    from: &Agent,
    to: &Agent,
    parent: &Session,
    task: &str,
    context: &str,
) -> Result<(String, Session, String)> {
    let task_row = state.db.create_task(&parent.id, &from.id, &to.id, task).await?;
    let title = format!("@{} → @{}: {}", from.handle, to.handle, auto_title(task));
    let child = state.db.create_session(&to.id, "delegation", None, Some(&parent.id), &title).await?;
    let mut text = format!("Task from @{} ({}):\n{}", from.handle, from.role, task);
    if !context.trim().is_empty() {
        text.push_str(&format!("\n\nContext:\n{}", context.trim()));
    }
    Ok((task_row.id, child, text))
}

/// Close the task with the delegate's outcome; the text is the manager's
/// tool result.
async fn close_delegation(
    state: &Shared,
    to: &Agent,
    task_id: &str,
    child_id: &str,
    res: Result<TurnResult>,
) -> Result<String> {
    match res {
        Ok(r) => {
            let status = if r.error.is_some() { "failed" } else { "done" };
            let summary = if r.text.trim().is_empty() {
                r.error.clone().unwrap_or_else(|| "(no answer)".into())
            } else {
                r.text.clone()
            };
            state.db.finish_task(task_id, Some(child_id), status, &summary).await?;
            Ok(format!("Result from @{} (session {}):\n{}", to.handle, child_id, summary))
        }
        Err(e) => {
            let msg = format!("{e:#}");
            state.db.finish_task(task_id, Some(child_id), "failed", &msg).await?;
            Err(anyhow!("delegation to @{} failed: {msg}", to.handle))
        }
    }
}

/// Keep the start and the end of long output; the end usually holds the error.
pub(crate) fn head_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max / 3).collect();
    let tail: String = s.chars().skip(n - (max - max / 3)).collect();
    format!("{head}\n…[{} characters cut]…\n{tail}", n - max)
}

/// First line of the message, trimmed, as a session title.
pub fn auto_title(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("New chat");
    let line = line.trim_start_matches('@');
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    t
}

/// Create a session and ensure the agent exists; used by every channel.
pub async fn ensure_default_agent(state: &Shared) -> Result<Agent> {
    if let Ok(a) = state.db.default_agent().await {
        return Ok(a);
    }
    let now = crate::db::now_ms();
    let a = Agent {
        id: new_id(),
        handle: "kongming".into(),
        name: "Kongming".into(),
        role: "Chief of staff".into(),
        description: "General assistant and coordinator. Talks to the user, handles everyday requests, and routes specialist work to other agents when they exist.".into(),
        system_prompt: String::new(),
        model: None,
        provider: None,
        manager_id: None,
        tools: None,
        mcp_servers: None,
        pinned_skills: vec![],
        self_improve: true,
        is_default: true,
        created_at: now,
        updated_at: now,
    };
    state.db.upsert_agent(&a).await.context("creating default agent")?;
    Ok(a)
}

pub fn can_delegate(agent: &Agent, team: &[Agent]) -> bool {
    team.iter().any(|t| t.manager_id.as_deref() == Some(agent.id.as_str()))
}

/// Starter skills, each inserted once (see `seed_skills`). They teach
/// the agents how to write good skills, work as a team and use the shell and browser.
pub const SEED_SKILLS: &[(&str, &str, &str, &[&str])] = &[
    (
        "writing-skills",
        "Use when creating or updating a skill with skill_manage: what makes a skill reusable and what to leave out.",
        "# Writing a good skill\n\n1. **Name**: a short slug that says what it does (`deploy-blog`, `weekly-report`).\n2. **Description**: one line that starts with *Use when…* so the index tells future runs when to load it.\n3. **Body**: the exact procedure.\n   - Preconditions and inputs needed.\n   - Numbered steps with the real commands, URLs, tool names and parameters.\n   - What a correct result looks like, and how to verify it.\n   - Pitfalls you hit and how to avoid them.\n4. **Keep it tight**: no transcript, no secrets, no one-off values that will not repeat.\n5. **Update, don't duplicate**: if a similar skill exists, `skill_view` it and send an `update` with the merged body.\n6. After a run that followed a skill and hit a problem, fix the skill the same turn.",
        &["meta"],
    ),
    (
        "team-delegation",
        "Use when you manage other agents and a request spans several specialties or can run in parallel.",
        "# Delegating inside the team\n\n- Check `list_agents` (or the Team section of your prompt) for who reports to you and what they own.\n- Split the request into tasks with a clear deliverable each. One `delegate_task` call per task.\n- The delegate cannot see this chat: put the goal, constraints, relevant facts, names/ids/urls and the expected output format into `task` and `context`.\n- Treat the returned summary as a self-report. Spot-check anything that matters (ids, urls, numbers) before presenting it.\n- Combine the results yourself and answer the user in your own words; mention which agent did what only if it helps.\n- Do small or single-step things yourself; delegation costs a full turn.",
        &["team"],
    ),
    (
        "memory-hygiene",
        "Use when deciding what to store with the memory tool and what to leave out.",
        "# Memory hygiene\n\nStore (importance 3-5): who the user is, their timezone and language, systems and accounts they mention by name, standing preferences (format, tone, tools), decisions and corrections (\"never deploy on Fridays\").\n\nStore (importance 1-2): project facts likely to come up again in the next weeks.\n\nDo not store: secrets or tokens, anything already in a skill, one-off numbers, speculation. Prefer `kind: preference` for how the user likes things, `lesson` for what went wrong and the fix, `project` for ongoing work, `fact` for the rest. Keep each memory to one or two sentences and add 2-4 keywords for search.",
        &["meta"],
    ),
    (
        "shell-and-browser",
        "Use when a task needs the local `shell` tool (BusyBox) or the Chrome browser through `cdp`: checking hosts, URLs and ports, quick text or number crunching, reading or operating web pages.",
        r##"# The local shell (BusyBox) and the browser (cdp)

The `shell` tool runs one `sh -c` script in a read-only BusyBox sandbox on the kongming board. It is for interacting, not for storage.

## Rules of the sandbox
- Nothing can be written to disk: `> file`, `tee`, `curl -o file`, `wget -O file`, `mkdir`, `touch`, `rm` fail ("Permission denied", "Read-only file system" or "File too large"). Pass data along with pipes and do everything a step needs in one command; nothing carries over between calls, so keep what matters in your own reply.
- The environment is empty: no tokens or passwords. Never paste secrets into a command.
- Time limit 30 s (`timeout_secs` up to 300). Background jobs are killed when the command ends.
- The output starts with `exit N`; anything but `exit 0` failed, so read the message and fix the command instead of repeating it. Output over ~12,000 characters is cut in the middle: narrow it with `grep`, `sed -n`, `head -c`, `tail`.

## Handy commands
- Web: `curl` and `wget` use real TLS and always write to stdout. `curl -sS URL | head -c 3000`, `curl -sSI URL` (status and headers), `curl -sS -o /dev/null -w '%{http_code}\n' URL`, `curl -sS -X POST -H 'Content-Type: application/json' -d '{"k":"v"}' URL`, `curl -sSL` to follow redirects, `-f` to fail on HTTP errors; `wget -qO- URL` works too.
- Network: `nc -z -w 3 HOST PORT && echo open`, `nslookup HOST`.
- Text: `grep -E -i`, `sed -n '1,40p'`, `awk -F, '{print $2}'`, `sort | uniq -c | sort -rn`, `wc -l`, `cut`, `tr`.
- Numbers and dates: `echo $((6*7))`, `awk 'BEGIN{printf "%.2f\n", 10/3}'`, `date -u`, `date -u -d @1700000000`.
- JSON has no jq: pull fields with `grep -o '"name":"[^"]*"'`, or parse it in the browser with `cdp eval`.
- `busybox --list` shows every applet.

## Driving Chrome with cdp
The browser address (`http://<ip>:9222`) comes from another skill or from the user. Give it in every command, since nothing is kept between calls: `cdp --url http://<ip>:9222 tabs`, or `export CDP_URL=http://<ip>:9222; cdp tabs && cdp text`. `cdp help` lists everything; page commands act on the first open tab unless you pass `--tab ID`.
1. `cdp tabs` lists the tabs (id, title, url).
2. `cdp goto URL` loads a page and waits for it.
3. Read it: `cdp text | head -c 4000`, or search it: `cdp text | grep -i -n -A2 'price'`.
4. Act: `cdp click 'CSS selector'`, `cdp type 'CSS selector' 'text'`, `cdp press Enter`, `cdp wait 'CSS selector'`, `cdp scroll`, `cdp back`. Check the result with `cdp url` and `cdp text` afterwards.
5. Extract precisely with JavaScript: `cdp eval '[...document.querySelectorAll("a")].slice(0,30).map(a => a.innerText.trim() + " " + a.href).join("\n")'`.
6. Anything else is a raw CDP command: `cdp call Page.reload`, `cdp call Emulation.setGeolocationOverride '{"latitude":1.3,"longitude":103.8,"accuracy":10}'`.
- Chain steps in one call with `&&`, e.g. `cdp goto https://example.com && cdp click 'a.more' && cdp text | head -c 2000`.
- Selectors: prefer ids, names and visible attributes (`input[name=q]`, `button[type=submit]`); find them with `cdp eval 'document.querySelector("form").outerHTML.slice(0,2000)'`.
- The browser is the user's real profile and may be logged in: never buy, post, send, delete or change settings without the user's explicit OK, and never open a URL ending in `#stop-session` (it ends the browser session).
- If `cdp` says the browser cannot be reached, the chrome-cdp session is probably not running, or the address is wrong: tell the user instead of retrying."##,
        &["tools", "browser"],
    ),
    (
        "setup-from-paste",
        "Use when the user pastes a key, token, config, URL or skill file: recognize what it is and configure it with setup.",
        r##"# Setting kongming up from a paste

The user should never need the Settings pages: what they paste, you configure with `setup`, then confirm in one or two lines.

## Keys never pass through you
Pasted keys are cut out of the message before you see it and replaced by `[secret:<id> <kind> …1234]`. Pass that reference (or `secret:<id>`) as the value; `setup` fills it in. A reference expires after a day: only then ask for the key again. Never write a key into memory, a skill or a reply.

## What it is → what to call
| Pasted | Looks like | Call |
|---|---|---|
| Anthropic key | `[secret:… anthropic …]` (sk-ant-…) | `setup` op `llm`, api_key (kind is set to anthropic) |
| OpenRouter key | `[secret:… openrouter …]` (sk-or-…) | op `llm`, api_key; base_url is set to OpenRouter; give a model like `anthropic/claude-sonnet-4.5` |
| OpenAI-style key | `[secret:… openai …]` (sk-…) | op `llm`, api_key, plus kind/base_url/model when the user named the provider (DeepSeek, Moonshot, Groq…); ask which provider if nothing says |
| Google AI key | `[secret:… google …]` (AIza…) | op `llm`, api_key (Gemini's OpenAI-compatible address is set), model e.g. `gemini-2.5-flash` |
| Telegram bot token | `[secret:… telegram …]` (digits:letters) | op `telegram`, token; add their numeric user id with allowed_users when known |
| Telegram user id | a bare 6–12 digit number after setting up the bot | op `telegram`, allowed_users |
| MCP config | JSON with `mcpServers` / `servers`, or one server with `url` | op `mcp_add`, config = the pasted JSON as it is |
| MCP server URL | https://…/mcp or …/sse, maybe with a token | op `mcp_add`, url (+ name, token) |
| Earnings key | a key named for toku, dealwork, OpenTask, MoltJobs, HYRVE or Claw Earn | op `earn`, provider, api_key |
| dealwork.ai agent | `agentAccountId` (often with an `ak_…` apiKey, e.g. its onboarding JSON) | op `earn`, provider `dealwork`, agent_id (+ api_key); without it dealwork shows the agent as pending |
| Skill | a SKILL.md (frontmatter with name and description) or a link to one | op `skill_import`, markdown or url |
| Public address | "kongming is at https://…" | op `public_url` |

## Steps
1. If it is unclear what a key is for, call `setup` op `show` and look at what is missing or broken.
2. Make the call. For several items in one paste, one call each.
3. Read the result and tell the user what changed. When `llm` fails the old settings stay: say why (wrong model id, wrong address, bad key).
4. An MCP server that needs sign-in: pass on the link setup gives, or point to Settings → MCP → Sign in.

## Pitfalls
- Local MCP servers (`command`, stdio) are refused on purpose: say so, and suggest the server's hosted (http) version if it has one.
- A key with no hint of its provider: ask, do not guess between providers.
- Earnings needs to be switched on separately (`/earn on`, or `setup` op `earn` enabled true when the user asks for it).
"##,
        &["setup", "settings"],
    ),
];

pub async fn seed_skills(state: &Shared) -> Result<()> {
    // Which starter skills were ever added: a new one reaches existing
    // databases, one the user deleted stays deleted.
    let mut seeded: Vec<String> = match state.db.setting(SEEDED_SKILLS_KEY).await? {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        // Before this was tracked the first three went in with an empty table.
        None if !state.db.skills().await?.is_empty() => {
            SEED_SKILLS.iter().take(3).map(|(name, ..)| name.to_string()).collect()
        }
        None => vec![],
    };
    // The text each one was last written with: a starter nobody has edited
    // follows the built-in text when it changes, an edited one is left alone.
    let mut bodies: std::collections::HashMap<String, String> = match state.db.setting(SEED_BODIES_KEY).await? {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let (seeded_before, bodies_before) = (seeded.clone(), bodies.clone());
    for (name, description, body, tags) in SEED_SKILLS {
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        let existing = state.db.skill_by_name(name).await?;
        if !seeded.iter().any(|s| s == name) {
            if existing.is_none() {
                state.db.save_skill(name, description, body, &tags, "seed", None).await?;
                bodies.insert(name.to_string(), body.to_string());
            }
            seeded.push(name.to_string());
            continue;
        }
        let Some(skill) = existing else { continue };
        if skill.body == *body {
            bodies.insert(name.to_string(), body.to_string());
            continue;
        }
        let unedited = match bodies.get(*name) {
            Some(last) => *last == skill.body,
            None => skill.source == "seed" && skill.version == 1,
        };
        if unedited {
            state.db.save_skill(name, description, body, &tags, "seed", None).await?;
            bodies.insert(name.to_string(), body.to_string());
        }
    }
    if seeded != seeded_before || state.db.setting(SEEDED_SKILLS_KEY).await?.is_none() {
        state.db.set_setting(SEEDED_SKILLS_KEY, &serde_json::json!(seeded)).await?;
    }
    if bodies != bodies_before {
        state.db.set_setting(SEED_BODIES_KEY, &serde_json::json!(bodies)).await?;
    }
    Ok(())
}

const SEEDED_SKILLS_KEY: &str = "seeded_skills";
const SEED_BODIES_KEY: &str = "seeded_skill_bodies";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_title_uses_first_line() {
        assert_eq!(auto_title("\n\n  hello world \nmore"), "hello world");
        assert_eq!(auto_title(""), "New chat");
        let long = "x".repeat(100);
        assert_eq!(auto_title(&long).chars().count(), 61);
    }

    #[test]
    fn check_args_lets_good_calls_through_and_explains_bad_ones() {
        let spec = ToolSpec {
            name: "mcp__composio__SEARCH".into(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object", "properties": {"queries": {"type": "array"}}, "required": ["queries"]}),
        };
        assert_eq!(check_args(Some(&spec), &serde_json::json!({"queries": []})), None);
        // Extra keys are fine on a loose schema as long as something matches.
        assert_eq!(check_args(Some(&spec), &serde_json::json!({"queries": [], "x": 1})), None);
        // Unknown tools are left to the executor.
        assert_eq!(check_args(None, &serde_json::json!({"anything": 1})), None);

        let wrapped = serde_json::json!({"name": "read session", "parameters": {"session_id": "x"}});
        let msg = check_args(Some(&spec), &wrapped).expect("rejected");
        assert!(msg.contains("missing required: queries"), "{msg}");
        assert!(msg.contains("unknown: name, parameters"), "{msg}");
        assert!(msg.contains("call it directly"), "{msg}");
        assert!(msg.contains("\"queries\""), "{msg}");

        let raw = serde_json::json!({"_raw": "{bad"});
        assert!(check_args(None, &raw).expect("rejected").contains("not valid JSON"));
    }

    fn session(summary_upto: i64) -> Session {
        Session {
            id: "s".into(),
            title: String::new(),
            agent_id: "a".into(),
            channel: "web".into(),
            channel_ref: None,
            parent_session_id: None,
            summary: Some("earlier".into()),
            summary_upto,
            archived: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn msg(seq: i64, role: &str) -> Message {
        Message {
            id: seq.to_string(),
            session_id: "s".into(),
            seq,
            role: role.into(),
            parts: vec![Part::Text { text: "t".into() }],
            agent_id: None,
            created_at: 0,
        }
    }

    #[test]
    fn history_includes_summary_and_skips_system() {
        let chat =
            history_to_chat(&session(2), &[msg(1, "user"), msg(2, "assistant"), msg(3, "system"), msg(4, "user")]);
        assert_eq!(chat.len(), 3);
        assert!(chat[0].parts.iter().any(|p| matches!(p, Part::Text { text } if text.contains("earlier"))));
        assert_eq!(chat[2].role, "user");
    }

    #[test]
    fn summary_before_an_assistant_message_needs_no_reply() {
        let chat = history_to_chat(&session(1), &[msg(1, "user"), msg(2, "assistant"), msg(3, "tool")]);
        let roles: Vec<&str> = chat.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
    }

    #[test]
    fn context_overflow_errors_are_recognized() {
        assert!(is_context_overflow("llm HTTP 400 Bad Request: prompt is too long: 210000 tokens > 200000 maximum"));
        assert!(is_context_overflow("llm HTTP 400: This model's maximum context length is 128000 tokens"));
        assert!(is_context_overflow("llm HTTP 400: context_length_exceeded"));
        assert!(!is_context_overflow("llm HTTP 401 Unauthorized: invalid api key"));
    }
}
