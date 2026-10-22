//! Proactive messages: Kongming reaching the user on Telegram without being
//! asked in that moment.
//!
//! Two parts. `notify` delivers one message (the `notify_user` tool): to every
//! allowed Telegram user, copied into that chat's conversation so a reply
//! continues from it, logged so the same topic is not sent twice in a row, at
//! most `maxPerDay` a day, and silently during quiet hours. `run` is the
//! check-in loop: every `intervalMinutes` outside quiet hours, the lead agent
//! looks at what the user watches and cares about and at recent conversations,
//! checks with its tools, and messages only when something is worth it. A
//! check-in that sent nothing leaves no conversation behind.
//!
//! Settings (key `proactive`): `enabled`, `intervalMinutes` (0 = no check-ins),
//! `maxPerDay`, `quietHours` ("22-7", local), `utcOffset` ("+07:00").

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::agent::{self, TurnOptions};
use crate::db::{now_ms, Agent, Part, Session};
use crate::state::{Event, Shared};

const KEY: &str = "proactive";
const LAST_RUN: &str = "proactive.lastRun";
const HOUR_MS: i64 = 3_600_000;
/// The same topic is not sent again within this window.
const REPEAT_WINDOW_MS: i64 = 20 * HOUR_MS;
/// How far back a check-in looks at conversations and sent messages.
const LOOKBACK_MS: i64 = 72 * HOUR_MS;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    /// Minutes between check-ins; 0 = none (messages from tasks still go out).
    pub interval_minutes: i64,
    pub max_per_day: i64,
    /// Local hours `[start, end)` with no check-ins and silent messages.
    pub quiet: Option<(u32, u32)>,
    pub utc_offset_minutes: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, interval_minutes: 180, max_per_day: 5, quiet: Some((22, 7)), utc_offset_minutes: 0 }
    }
}

/// `[+-]HH[:MM]`, optionally after `UTC`; empty = UTC.
pub fn parse_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let s = s.strip_prefix("UTC").or_else(|| s.strip_prefix("utc")).unwrap_or(s).trim();
    if s.is_empty() || s == "Z" {
        return Some(0);
    }
    let (sign, rest) = match s.as_bytes()[0] {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 14 && (0..60).contains(&m)).then_some(sign * (h * 60 + m))
}

pub fn format_offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60)
}

/// `"22-7"` → (22, 7); empty or `off` = no quiet hours.
pub fn parse_quiet(s: &str) -> Option<Option<(u32, u32)>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("off") {
        return Some(None);
    }
    let (a, b) = s.split_once('-')?;
    let hour = |x: &str| x.trim().split(':').next()?.trim().parse::<u32>().ok().filter(|h| *h < 24);
    let (a, b) = (hour(a)?, hour(b)?);
    Some((a != b).then_some((a, b)))
}

impl Settings {
    pub fn from_value(v: Option<&Value>) -> Settings {
        let d = Settings::default();
        let Some(v) = v else { return d };
        Settings {
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(d.enabled),
            interval_minutes: v.get("intervalMinutes").and_then(Value::as_i64).unwrap_or(d.interval_minutes).max(0),
            max_per_day: v.get("maxPerDay").and_then(Value::as_i64).unwrap_or(d.max_per_day).max(0),
            quiet: v.get("quietHours").and_then(Value::as_str).and_then(parse_quiet).unwrap_or(d.quiet),
            utc_offset_minutes: v.get("utcOffset").and_then(Value::as_str).and_then(parse_offset).unwrap_or(0),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "intervalMinutes": self.interval_minutes,
            "maxPerDay": self.max_per_day,
            "quietHours": self.quiet.map(|(a, b)| format!("{a}-{b}")).unwrap_or_default(),
            "utcOffset": format_offset(self.utc_offset_minutes),
        })
    }

    /// Apply a change from the dashboard; errors name the bad field.
    pub fn merged(&self, body: &Value) -> Result<Settings, String> {
        let mut s = self.clone();
        if let Some(v) = body.get("enabled") {
            s.enabled = v.as_bool().ok_or("enabled must be true or false")?;
        }
        if let Some(v) = body.get("intervalMinutes") {
            s.interval_minutes = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|x| x.trim().parse().ok()))
                .filter(|n| *n == 0 || (15..=10_080).contains(n))
                .ok_or("intervalMinutes must be 0 (no check-ins) or 15 to 10080")?;
        }
        if let Some(v) = body.get("maxPerDay") {
            s.max_per_day = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|x| x.trim().parse().ok()))
                .filter(|n| (0..=50).contains(n))
                .ok_or("maxPerDay must be 0 to 50")?;
        }
        if let Some(v) = body.get("quietHours") {
            s.quiet = parse_quiet(v.as_str().unwrap_or(""))
                .ok_or("quietHours must look like 22-7 (local hours), or be empty")?;
        }
        if let Some(v) = body.get("utcOffset") {
            s.utc_offset_minutes =
                parse_offset(v.as_str().unwrap_or("")).ok_or("utcOffset must look like +07:00 or -05:30")?;
        }
        Ok(s)
    }

    pub fn local(&self, ms: i64) -> time::OffsetDateTime {
        let offset = time::UtcOffset::from_whole_seconds(self.utc_offset_minutes * 60).unwrap_or(time::UtcOffset::UTC);
        time::OffsetDateTime::from_unix_timestamp(ms / 1000)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(offset)
    }

    pub fn is_quiet(&self, ms: i64) -> bool {
        let Some((start, end)) = self.quiet else { return false };
        let h = self.local(ms).hour() as u32;
        if start < end {
            (start..end).contains(&h)
        } else {
            h >= start || h < end
        }
    }

    pub fn local_time(&self, ms: i64) -> String {
        let fmt = time::macros::format_description!("[weekday repr:short] [year]-[month]-[day] [hour]:[minute]");
        format!("{} (UTC{})", self.local(ms).format(&fmt).unwrap_or_default(), format_offset(self.utc_offset_minutes))
    }
}

pub async fn settings(state: &Shared) -> Settings {
    Settings::from_value(state.db.setting(KEY).await.ok().flatten().as_ref())
}

pub async fn save(state: &Shared, s: &Settings) -> Result<()> {
    state.db.set_setting(KEY, &s.to_json()).await
}

pub async fn set_enabled(state: &Shared, enabled: bool) -> Result<()> {
    save(state, &Settings { enabled, ..settings(state).await }).await
}

pub async fn last_run(state: &Shared) -> Option<i64> {
    state.db.setting(LAST_RUN).await.ok().flatten().and_then(|v| v.as_i64())
}

fn topic_key(topic: &str) -> String {
    topic.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Send one message to the user. `requested`: the user asked to be told
/// about this, so it goes out even with proactive messages off or past the
/// daily limit. `urgent`: not silenced by quiet hours. Returns what happened,
/// for the agent; errors say why nothing was sent.
pub async fn notify(
    state: &Shared,
    agent: &Agent,
    session: Option<&Session>,
    topic: &str,
    text: &str,
    requested: bool,
    urgent: bool,
) -> Result<String> {
    let (topic, text) = (topic.trim(), text.trim());
    if topic.is_empty() || text.is_empty() {
        bail!("topic and message are required");
    }
    if text.chars().count() > 3500 {
        bail!("message too long (max 3500 characters); keep it short and link to details");
    }
    let s = settings(state).await;
    if !s.enabled && !requested {
        bail!("the user switched proactive messages off; mention it in your next reply instead");
    }
    let token = crate::telegram::active_token(state)
        .await
        .ok_or_else(|| anyhow!("Telegram is not set up, so the user cannot be reached outside this app"))?;
    let chats = state.telegram_allowed().await;
    if chats.is_empty() {
        bail!("no Telegram user is allowed yet (Settings → Channels), so there is nobody to message");
    }
    let now = now_ms();
    let recent = state.db.notifications(now - 24 * HOUR_MS, 200).await?;
    let key = topic_key(topic);
    if let Some(prev) = recent.iter().find(|n| topic_key(&n.topic) == key && n.created_at >= now - REPEAT_WINDOW_MS) {
        bail!(
            "the user was already told about \"{}\" {} min ago; send again only when something changed, under a topic that says what changed",
            prev.topic,
            (now - prev.created_at) / 60_000
        );
    }
    let sent_today = recent.iter().filter(|n| n.delivered > 0).count() as i64;
    if !requested && sent_today >= s.max_per_day {
        bail!("daily limit reached ({sent_today} messages in 24 h); keep it for the next conversation");
    }

    let silent = s.is_quiet(now) && !urgent;
    let tg = crate::telegram::client(state, token);
    let html = format!("💡 {}", crate::telegram::markdown_to_html(text));
    let mut delivered = 0;
    let mut last_err = None;
    for chat in &chats {
        match tg.send(*chat, &html, silent).await {
            Ok(_) => {
                delivered += 1;
                // Into the chat's conversation, so a reply has the context.
                match crate::telegram::session_for_chat(state, *chat, false).await {
                    Ok(cs) => {
                        let parts = [Part::Text { text: format!("💡 {text}") }];
                        if let Ok(m) = state.db.append_message(&cs.id, "assistant", &parts, Some(&agent.id)).await {
                            state.emit(Event::Message { session_id: cs.id.clone(), message: m });
                        }
                    }
                    Err(e) => tracing::warn!("proactive: chat session: {e:#}"),
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    let n = state.db.log_notification(Some(&agent.id), session.map(|s| s.id.as_str()), topic, text, delivered).await?;
    if delivered == 0 {
        let e = last_err.map(|e| format!("{e:#}")).unwrap_or_default();
        bail!("Telegram did not deliver the message: {e} (the user may need to open the bot and press Start)");
    }
    let note = format!("Told the user: {topic}");
    let _ = state
        .db
        .log_improvement(Some(&agent.id), session.map(|s| s.id.as_str()), "notification", Some(&n.id), &note)
        .await;
    state.emit(Event::Improvement { kind: "notification".into(), note, agent_id: Some(agent.id.clone()) });
    tracing::info!(topic, delivered, silent, "proactive message sent");
    Ok(format!("Sent to {delivered} Telegram chat(s){}.", if silent { " silently (quiet hours)" } else { "" }))
}

static CHECKING: AtomicBool = AtomicBool::new(false);

struct Running;
impl Drop for Running {
    fn drop(&mut self) {
        CHECKING.store(false, Ordering::SeqCst);
    }
}

/// One check-in. Returns how many messages it sent. `manual` (asked for from
/// Telegram or the dashboard) runs even with nothing obvious to look at and
/// keeps its conversation when it sent nothing.
pub async fn check_in(state: &Shared, manual: bool) -> Result<usize> {
    if CHECKING.swap(true, Ordering::SeqCst) {
        bail!("a check-in is already running");
    }
    let _running = Running;
    let s = settings(state).await;
    let started = now_ms();
    let _ = state.db.set_setting(LAST_RUN, &json!(started)).await;
    if crate::telegram::active_token(state).await.is_none() {
        bail!("Telegram is not set up, so there is no way to reach you");
    }
    let sent_today =
        state.db.notifications(started - 24 * HOUR_MS, 200).await?.iter().filter(|n| n.delivered > 0).count();
    let left = (s.max_per_day - sent_today as i64).max(0);
    if left == 0 && !manual {
        return Ok(0);
    }
    let Some(prompt) = check_in_prompt(state, &s, started, left.max(1), manual).await? else {
        return Ok(0);
    };
    let agent = agent::ensure_default_agent(state).await?;
    let session = state.db.create_session(&agent.id, "proactive", None, None, "💡 Check-in").await?;
    let res = agent::run_turn(
        state.clone(),
        session.clone(),
        prompt,
        TurnOptions { depth: 1, no_reflect: true, agent_override: Some(agent.clone()) },
    )
    .await;
    let sent = state
        .db
        .notifications(started, 50)
        .await?
        .iter()
        .filter(|n| n.session_id.as_deref() == Some(session.id.as_str()) && n.delivered > 0)
        .count();
    let outcome = match &res {
        Ok(r) if r.error.is_none() => format!("sent {sent} message(s)"),
        Ok(r) => format!("failed: {}", r.error.clone().unwrap_or_default()),
        Err(e) => format!("failed: {e:#}"),
    };
    if sent == 0 && !manual && res.as_ref().is_ok_and(|r| r.error.is_none()) {
        let _ = state.db.delete_session(&session.id).await;
    }
    let keep = (sent > 0 || manual).then_some(session.id.as_str());
    let _ = state.db.log_improvement(Some(&agent.id), keep, "check_in", None, &format!("Check-in: {outcome}")).await;
    tracing::info!(%outcome, "proactive check-in done");
    res?;
    Ok(sent)
}

fn bullet(text: &str, max: usize) -> String {
    crate::mcp::truncate(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

/// What a check-in looks at. `None` when there is nothing to base one on.
async fn check_in_prompt(state: &Shared, s: &Settings, now: i64, left: i64, manual: bool) -> Result<Option<String>> {
    let memories = state.db.memories(None, 300).await?;
    let watches: Vec<String> = memories
        .iter()
        .filter(|m| m.kind == "watch")
        .map(|m| format!("- ({}) {}", m.id, bullet(&m.content, 300)))
        .collect();
    let interests: Vec<String> = memories
        .iter()
        .filter(|m| matches!(m.kind.as_str(), "interest" | "project"))
        .map(|m| format!("- [{}] {}", m.kind, bullet(&m.content, 200)))
        .collect();
    let conversations: Vec<String> = state
        .db
        .sessions(40)
        .await?
        .into_iter()
        .filter(|x| x.updated_at >= now - LOOKBACK_MS && matches!(x.channel.as_str(), "web" | "telegram"))
        .take(12)
        .map(|x| {
            format!(
                "- {} (session {}, {}, {} h ago)",
                bullet(&x.title, 80),
                x.id,
                x.channel,
                (now - x.updated_at) / HOUR_MS
            )
        })
        .collect();
    if !manual && watches.is_empty() && interests.is_empty() && conversations.is_empty() {
        return Ok(None);
    }
    let sent: Vec<String> = state
        .db
        .notifications(now - LOOKBACK_MS, 20)
        .await?
        .iter()
        .map(|n| format!("- {} h ago, [{}] {}", (now - n.created_at) / HOUR_MS, n.topic, bullet(&n.text, 200)))
        .collect();
    let section = |title: &str, lines: &[String], none: &str| {
        format!("## {title}\n{}\n\n", if lines.is_empty() { none.to_string() } else { lines.join("\n") })
    };
    let mut p = format!(
        "Proactive check-in. The user's local time is {}. The user is not in this conversation.\n\n\
Decide whether anything is worth telling the user right now, and if so tell them with `notify_user`. Good reasons: \
something they asked to be told about (watch list) happened or changed; a deadline, event or plan they mentioned is \
coming up soon; new information that clearly matters for one of their interests or projects; work you left unfinished \
in a recent conversation that you can now finish, or that needs their answer. Check with your tools (web_fetch, shell, \
MCP, read_session) before claiming anything; never message from memory alone.\n\
Do not message about small talk, things they already know, anything in the already-sent list unless it changed, or \
more than {left} thing(s) now. One message per topic: short, the key fact first, a link when there is one. Remove \
watch memories that are done or expired, and save new interests you notice. If nothing qualifies, send nothing.\n\
Finish with one line for the log: what you sent, or `nothing to report`.\n\n",
        s.local_time(now)
    );
    p.push_str(&section("Watch list (the user asked to be told)", &watches, "Nothing on the watch list."));
    p.push_str(&section("Interests and projects", &interests, "None saved yet."));
    p.push_str(&section("Recent conversations", &conversations, "None in the last 3 days."));
    p.push_str(&section("Already sent (last 3 days)", &sent, "Nothing yet."));
    Ok(Some(p))
}

async fn due(state: &Shared) -> bool {
    let s = settings(state).await;
    if !s.enabled || s.interval_minutes == 0 || CHECKING.load(Ordering::SeqCst) {
        return false;
    }
    let now = now_ms();
    if s.is_quiet(now) || crate::telegram::active_token(state).await.is_none() {
        return false;
    }
    match last_run(state).await {
        Some(t) => now - t >= s.interval_minutes * 60_000,
        None => {
            // First start: wait one interval instead of checking in at boot.
            let _ = state.db.set_setting(LAST_RUN, &json!(now)).await;
            false
        }
    }
}

/// The check-in loop; runs until the process stops.
pub async fn run(state: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        if due(&state).await {
            if let Err(e) = check_in(&state, false).await {
                tracing::warn!("proactive check-in failed: {e:#}");
            }
        }
    }
}

/// Status line for `/proactive` and the dashboard.
pub async fn status_html(state: &Shared) -> Result<String> {
    let s = settings(state).await;
    let now = now_ms();
    let sent = state.db.notifications(now - 24 * HOUR_MS, 200).await?.iter().filter(|n| n.delivered > 0).count();
    let checks = if !s.enabled {
        "off".to_string()
    } else if s.interval_minutes == 0 {
        "off (messages from tasks and watches still come)".to_string()
    } else {
        let next = last_run(state).await.map(|t| t + s.interval_minutes * 60_000 - now).unwrap_or(0).max(0);
        format!("every {} min, next in about {} min", s.interval_minutes, next / 60_000)
    };
    Ok(format!(
        "Proactive messages: <b>{}</b>\nCheck-ins: {checks}\nQuiet hours: {}\nSent in the last 24 h: {sent} of {}\n\n/proactive on · /proactive off · /proactive now",
        if s.enabled { "on" } else { "off" },
        s.quiet
            .map(|(a, b)| format!("{a:02}:00–{b:02}:00 (UTC{})", format_offset(s.utc_offset_minutes)))
            .unwrap_or_else(|| "none".into()),
        s.max_per_day
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+07:00"), Some(420));
        assert_eq!(parse_offset("UTC+7"), Some(420));
        assert_eq!(parse_offset("-05:30"), Some(-330));
        assert_eq!(parse_offset(""), Some(0));
        assert_eq!(parse_offset("7"), None);
        assert_eq!(parse_offset("+15:00"), None);
        assert_eq!(format_offset(-330), "-05:30");
        assert_eq!(format_offset(420), "+07:00");
    }

    #[test]
    fn quiet_hours_wrap_midnight_in_local_time() {
        assert_eq!(parse_quiet("22-7"), Some(Some((22, 7))));
        assert_eq!(parse_quiet("22:00-07:00"), Some(Some((22, 7))));
        assert_eq!(parse_quiet(""), Some(None));
        assert_eq!(parse_quiet("off"), Some(None));
        assert_eq!(parse_quiet("5-5"), Some(None));
        assert_eq!(parse_quiet("25-7"), None);
        let s = Settings { utc_offset_minutes: 420, ..Settings::default() };
        // 2026-01-01 00:00 UTC = 07:00 at +07:00: just past the end.
        let base = 1_767_225_600_000;
        assert!(!s.is_quiet(base));
        assert!(s.is_quiet(base - 60_000), "06:59 local");
        assert!(s.is_quiet(base + 15 * HOUR_MS), "22:00 local");
        assert!(!s.is_quiet(base + 14 * HOUR_MS), "21:00 local");
        let day = Settings { quiet: Some((1, 6)), ..Settings::default() };
        assert!(day.is_quiet(base + 3 * HOUR_MS));
        assert!(!day.is_quiet(base + 6 * HOUR_MS));
        assert!(!Settings { quiet: None, ..s }.is_quiet(base - 60_000));
    }

    #[test]
    fn settings_round_trip_and_validate() {
        let d = Settings::default();
        assert_eq!(Settings::from_value(Some(&d.to_json())), d);
        assert_eq!(Settings::from_value(None), d);
        let s = d
            .merged(&json!({"intervalMinutes": "60", "maxPerDay": 3, "quietHours": "23-6", "utcOffset": "+07:00"}))
            .unwrap();
        assert_eq!((s.interval_minutes, s.max_per_day, s.quiet, s.utc_offset_minutes), (60, 3, Some((23, 6)), 420));
        assert_eq!(Settings::from_value(Some(&s.to_json())), s);
        assert!(d.merged(&json!({"intervalMinutes": 5})).unwrap_err().contains("intervalMinutes"));
        assert_eq!(d.merged(&json!({"intervalMinutes": 0})).unwrap().interval_minutes, 0);
        assert!(d.merged(&json!({"quietHours": "late"})).is_err());
        assert!(d.merged(&json!({"utcOffset": "Jakarta"})).is_err());
        assert_eq!(d.merged(&json!({"quietHours": ""})).unwrap().quiet, None);
        assert!(!d.merged(&json!({"enabled": false})).unwrap().enabled);
    }

    #[test]
    fn local_time_names_the_offset() {
        let s = Settings { utc_offset_minutes: 420, ..Settings::default() };
        assert_eq!(s.local_time(1_767_225_600_000), "Thu 2026-01-01 07:00 (UTC+07:00)");
    }

    #[test]
    fn topics_compare_loosely() {
        assert_eq!(topic_key("  GPU  price\nDrop "), "gpu price drop");
    }
}
