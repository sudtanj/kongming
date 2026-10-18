//! The agentic loop. One `run_turn` call takes a user message for a session,
//! assembles the prompt, streams the model, executes tool calls until the model
//! stops, persists every round, and (optionally) runs a reflection pass that
//! may save skills and memories. Events go to the bus for whoever is listening.

pub mod mention;
pub mod prompt;
pub mod shell;
pub mod tools;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
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
#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub text: String,
    pub agent: Option<Agent>,
    pub stop_reason: String,
    pub error: Option<String>,
}

/// Options for a turn beyond the text itself.
#[derive(Debug, Clone, Default)]
pub struct TurnOptions {
    /// Delegation depth (0 for a human turn).
    pub depth: usize,
    /// Skip the reflection pass (delegations and compactions).
    pub no_reflect: bool,
    /// Force a specific agent regardless of mentions.
    pub agent_override: Option<Agent>,
}

/// Resolve which agent answers: an `@handle` mention switches the session's
/// agent (sticky, nanobot-style), otherwise the session's current agent.
pub async fn resolve_agent(state: &Shared, session: &Session, handles: &[String]) -> Result<(Agent, bool)> {
    for h in handles {
        if let Some(a) = state.db.agent_by_handle(h).await? {
            let switched = a.id != session.agent_id;
            return Ok((a, switched));
        }
    }
    match state.db.agent(&session.agent_id).await? {
        Some(a) => Ok((a, false)),
        None => Ok((state.db.default_agent().await?, true)),
    }
}

fn now_utc_string() -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    time::OffsetDateTime::now_utc().format(&fmt).unwrap_or_default()
}

/// History as the model sees it: the rolling summary (if any) followed by the
/// messages after `summary_upto`.
fn history_to_chat(session: &Session, msgs: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    if let Some(sum) = session.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push(ChatMessage::text("user", format!("[Summary of the earlier part of this conversation]\n{sum}")));
        out.push(ChatMessage::text("assistant", "Understood, I have the summary of our earlier conversation."));
    }
    for m in msgs.iter().filter(|m| m.seq > session.summary_upto) {
        if m.role == "system" {
            continue;
        }
        out.push(ChatMessage { role: m.role.clone(), parts: m.parts.clone() });
    }
    out
}

/// Compact older history into the session summary when the estimate exceeds
/// the budget. Keeps the most recent messages verbatim.
async fn maybe_compact(
    state: &Shared,
    session: &mut Session,
    msgs: &[Message],
    provider_cfg: &llm::ProviderConfig,
    model: &str,
) -> Result<()> {
    let chat = history_to_chat(session, msgs);
    let total: usize = chat.iter().map(llm::estimate_message_tokens).sum();
    if total < state.cfg.context_tokens {
        return Ok(());
    }
    let live: Vec<&Message> = msgs.iter().filter(|m| m.seq > session.summary_upto).collect();
    if live.len() <= KEEP_RECENT_MESSAGES {
        return Ok(());
    }
    // Cut on a user message boundary so tool call/result pairs stay together.
    let mut cut = live.len() - KEEP_RECENT_MESSAGES;
    while cut > 0 && live[cut].role != "user" {
        cut -= 1;
    }
    if cut == 0 {
        return Ok(());
    }
    let upto = live[cut - 1].seq;
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
        messages: vec![ChatMessage::text("user", crate::mcp::truncate(&transcript, 60_000))],
        tools: vec![],
        max_tokens: 1500,
        temperature: Some(0.2),
        effort: Some("low".into()),
    };
    let summary = collect_text(provider.stream(req).await?).await?;
    if summary.trim().is_empty() {
        return Ok(());
    }
    state.db.set_session_summary(&session.id, &summary, upto).await?;
    session.summary = Some(summary);
    session.summary_upto = upto;
    let _ = provider_cfg;
    Ok(())
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
    let mut tool_specs = tools::builtin_specs(&agent, can_delegate, state.shell.is_some());
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
            tokio::time::sleep(wait).await;
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
        let item = tokio::time::timeout(Duration::from_secs(180), stream.next())
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

/// Run one user turn end to end. The user message is persisted here.
pub async fn run_turn(state: Shared, mut session: Session, user_text: String, opts: TurnOptions) -> Result<TurnResult> {
    let Some(cancel) = state.begin_turn(&session.id) else {
        return Err(anyhow!("a reply is already in progress for this conversation"));
    };
    let result = run_turn_inner(&state, &mut session, user_text, &opts, &cancel).await;
    state.end_turn(&session.id);
    result
}

async fn run_turn_inner(
    state: &Shared,
    session: &mut Session,
    user_text: String,
    opts: &TurnOptions,
    cancel: &AtomicBool,
) -> Result<TurnResult> {
    let turn_id = new_id();
    let mentions = mention::parse(&user_text);
    let (agent, switched) = match &opts.agent_override {
        Some(a) => (a.clone(), a.id != session.agent_id),
        None => resolve_agent(state, session, &mentions.handles).await?,
    };
    if switched {
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

    let mut plan = match plan(state, session, agent, &mentions).await {
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

    // Context: compact if needed, then load.
    let mut msgs = state.db.messages(&session.id, session.summary_upto).await?;
    if let Err(e) = maybe_compact(state, session, &msgs, &pcfg, &plan.model).await {
        tracing::warn!("compaction failed: {e:#}");
    }
    msgs.retain(|m| m.seq > session.summary_upto);
    let mut chat = history_to_chat(session, &msgs);
    // The agent is addressed as "@handle: text"; drop the prefix in the live copy.
    if let Some(last) = chat.last_mut() {
        if last.role == "user" {
            last.parts = vec![Part::Text { text: mention::strip_leading_handle(&stored_text, &plan.agent.handle) }];
        }
    }

    let mut final_text = String::new();
    let mut stop_reason = String::from("stop");
    let mut error: Option<String> = None;
    let mut used_tools = false;
    let mut retried_empty = false;

    // The step limit is a checkpoint, not the end: a long task carries on by
    // itself for `auto_continue` more blocks unless it is going in circles.
    let block = state.cfg.max_iterations.max(1);
    let limit = block * (1 + state.cfg.auto_continue);
    let mut seen = std::collections::HashSet::new();
    let (mut block_calls, mut repeats) = (0usize, 0usize);

    for iteration in 0..limit {
        if cancel.load(Ordering::Relaxed) {
            stop_reason = "cancelled".into();
            break;
        }
        let req = ChatRequest {
            model: plan.model.clone(),
            system: plan.system.clone(),
            messages: chat.clone(),
            tools: plan.tools.clone(),
            max_tokens: pcfg.max_tokens.unwrap_or(8192),
            temperature: pcfg.temperature,
            effort: pcfg.effort.clone(),
        };
        let (parts, stop, usage) = match model_round(state, &session.id, &turn_id, req, cancel).await {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("cancelled") {
                    stop_reason = "cancelled".into();
                } else {
                    stop_reason = "error".into();
                    error = Some(msg);
                }
                break;
            }
        };
        if usage.0 > 0 || usage.1 > 0 {
            let _ = state.db.record_usage(Some(&session.id), Some(&plan.agent.id), &plan.model, usage.0, usage.1).await;
        }
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
            if !retried_empty {
                retried_empty = true;
                continue;
            }
            stop_reason = "error".into();
            error = Some(format!(
                "the model ({}) returned an empty reply{}; try again, or pick a stronger model in Settings → Models",
                plan.model,
                if used_tools { " after the tool results" } else { "" }
            ));
            break;
        }
        let assistant_msg = state.db.append_message(&session.id, "assistant", &parts, Some(&plan.agent.id)).await?;
        state.emit(Event::Message { session_id: session.id.clone(), message: assistant_msg });
        for p in &parts {
            if let Part::Text { text } = p {
                final_text = text.clone();
            }
        }
        chat.push(ChatMessage {
            role: "assistant".into(),
            parts: parts.iter().filter(|p| !matches!(p, Part::Thinking { .. })).cloned().collect(),
        });

        if calls.is_empty() {
            stop_reason = stop;
            break;
        }
        used_tools = true;
        let ctx = ToolCtx {
            state,
            agent: &plan.agent,
            session,
            turn_id: &turn_id,
            depth: opts.depth,
            can_delegate: plan.can_delegate,
        };
        let mut results = Vec::with_capacity(calls.len());
        for (id, name, args) in &calls {
            block_calls += 1;
            if !seen.insert(format!("{name} {args}")) {
                repeats += 1;
            }
            state.emit(Event::ToolCall {
                session_id: session.id.clone(),
                turn_id: turn_id.clone(),
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
            });
            let part = if cancel.load(Ordering::Relaxed) {
                Part::ToolResult {
                    id: id.clone(),
                    name: name.clone(),
                    content: "cancelled by user".into(),
                    is_error: true,
                }
            } else if !plan.tools.iter().any(|t| t.name == *name) {
                Part::ToolResult {
                    id: id.clone(),
                    name: name.clone(),
                    content: format!("error: unknown tool {name}: use one of the tools you were given"),
                    is_error: true,
                }
            } else if let Some(problem) = check_args(plan.tools.iter().find(|t| t.name == *name), args) {
                Part::ToolResult {
                    id: id.clone(),
                    name: name.clone(),
                    content: format!("error: {problem}"),
                    is_error: true,
                }
            } else {
                execute_tool(&ctx, id, name, args).await
            };
            if let Part::ToolResult { content, is_error, .. } = &part {
                state.emit(Event::ToolResult {
                    session_id: session.id.clone(),
                    turn_id: turn_id.clone(),
                    id: id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                });
            }
            results.push(part);
        }
        // A report hired this turn can take work right away.
        let hired = results
            .iter()
            .any(|p| matches!(p, Part::ToolResult { name, is_error: false, .. } if name == "agent_manage"));
        if hired && !plan.can_delegate {
            plan.can_delegate = true;
            plan.tools.extend(
                tools::builtin_specs(&plan.agent, true, false).into_iter().filter(|t| t.name == "delegate_task"),
            );
        }
        let tool_msg = state.db.append_message(&session.id, "tool", &results, Some(&plan.agent.id)).await?;
        state.emit(Event::Message { session_id: session.id.clone(), message: tool_msg });
        chat.push(ChatMessage { role: "tool".into(), parts: results });
        let step = iteration + 1;
        if step % block != 0 {
            continue;
        }
        // Half the calls of the block were exact repeats: the model is stuck.
        let looping = repeats * 2 >= block_calls;
        if step < limit && !looping {
            tracing::info!(session = %session.id, step, "step limit reached; continuing automatically");
            shorten_tool_results(&mut chat);
            seen.clear();
            (block_calls, repeats) = (0, 0);
            continue;
        }
        stop_reason = "max_iterations".into();
        let note = if looping {
            format!(
                "I stopped after {step} steps because I kept repeating the same tool calls. Tell me how to proceed, \
or to continue."
            )
        } else {
            format!(
                "I used all {step} steps allowed for one turn. Tell me to continue and I will pick up where I stopped."
            )
        };
        let m = state
            .db
            .append_message(&session.id, "assistant", &[Part::Text { text: note.clone() }], Some(&plan.agent.id))
            .await?;
        state.emit(Event::Message { session_id: session.id.clone(), message: m });
        final_text = note;
        break;
    }

    state.emit(Event::TurnEnd {
        session_id: session.id.clone(),
        turn_id: turn_id.clone(),
        stop_reason: stop_reason.clone(),
        error: error.clone(),
    });
    let _ = state.db.touch_session(&session.id).await;

    // Reflection: learn from turns that did real work.
    if !opts.no_reflect
        && plan.agent.self_improve
        && error.is_none()
        && stop_reason != "cancelled"
        && should_reflect(state, &session.id, used_tools, &stored_text).await
    {
        let st = state.clone();
        let agent = plan.agent.clone();
        let sess = session.clone();
        let model = plan.model.clone();
        tokio::spawn(async move {
            if let Err(e) = reflect(st, agent, sess, model).await {
                tracing::warn!("reflection failed: {e:#}");
            }
        });
    }

    let _ = plan.mcp_names;
    Ok(TurnResult { text: final_text, agent: Some(plan.agent), stop_reason, error })
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
    let specs: Vec<ToolSpec> = tools::builtin_specs(&agent, false, false)
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
        let task_row = state.db.create_task(&parent.id, &from.id, &to.id, &task).await?;
        let title = format!("@{} → @{}: {}", from.handle, to.handle, auto_title(&task));
        let child = state.db.create_session(&to.id, "delegation", None, Some(&parent.id), &title).await?;
        let mut text = format!("Task from @{} ({}):\n{}", from.handle, from.role, task);
        if !context.trim().is_empty() {
            text.push_str(&format!("\n\nContext:\n{}", context.trim()));
        }
        let child_id = child.id.clone();
        let res = run_turn(
            state.clone(),
            child,
            text,
            TurnOptions { depth, no_reflect: true, agent_override: Some(to.clone()) },
        )
        .await;
        match res {
            Ok(r) => {
                let status = if r.error.is_some() { "failed" } else { "done" };
                let summary = if r.text.trim().is_empty() {
                    r.error.clone().unwrap_or_else(|| "(no answer)".into())
                } else {
                    r.text.clone()
                };
                state.db.finish_task(&task_row.id, Some(&child_id), status, &summary).await?;
                Ok(format!("Result from @{} (session {}):\n{}", to.handle, child_id, summary))
            }
            Err(e) => {
                let msg = format!("{e:#}");
                state.db.finish_task(&task_row.id, Some(&child_id), "failed", &msg).await?;
                Err(anyhow!("delegation to @{} failed: {msg}", to.handle))
            }
        }
    })
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

/// Starter skills, inserted once when the skills table is empty. They teach
/// the agents how to write good skills and how to work as a team.
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
];

pub async fn seed_skills(state: &Shared) -> Result<()> {
    if !state.db.skills().await?.is_empty() {
        return Ok(());
    }
    for (name, description, body, tags) in SEED_SKILLS {
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        state.db.save_skill(name, description, body, &tags, "seed", None).await?;
    }
    Ok(())
}

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

    #[test]
    fn history_includes_summary_and_skips_system() {
        let s = Session {
            id: "s".into(),
            title: String::new(),
            agent_id: "a".into(),
            channel: "web".into(),
            channel_ref: None,
            parent_session_id: None,
            summary: Some("earlier".into()),
            summary_upto: 2,
            archived: false,
            created_at: 0,
            updated_at: 0,
        };
        let m = |seq: i64, role: &str| Message {
            id: seq.to_string(),
            session_id: "s".into(),
            seq,
            role: role.into(),
            parts: vec![Part::Text { text: "t".into() }],
            agent_id: None,
            created_at: 0,
        };
        let chat = history_to_chat(&s, &[m(1, "user"), m(2, "assistant"), m(3, "system"), m(4, "user")]);
        assert_eq!(chat.len(), 3);
        assert!(chat[0].parts.iter().any(|p| matches!(p, Part::Text { text } if text.contains("earlier"))));
        assert_eq!(chat[2].role, "user");
    }
}
