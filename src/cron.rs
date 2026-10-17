//! Scheduled prompts. Schedules are `every:<minutes>` or a 5-field cron
//! expression in UTC (`min hour dom month dow`, with `*`, lists, ranges and
//! `*/n`). A ticker checks every 30 s; each due job runs one turn in its own
//! session and can deliver the answer to a Telegram chat.

use std::time::Duration;

use serde_json::Value;

use crate::agent::{self, TurnOptions};
use crate::db::CronJob;
use crate::state::Shared;

fn field_matches(field: &str, value: u32, min: u32, max: u32) -> Option<bool> {
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0)?),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let v: u32 = range.parse().ok()?;
            if step == 1 {
                if v == value {
                    return Some(true);
                }
                continue;
            }
            (v, max)
        };
        if value >= lo && value <= hi && (value - lo) % step == 0 {
            return Some(true);
        }
    }
    Some(false)
}

/// Next run time (unix ms) strictly after `after_ms`, or `None` if the
/// schedule is invalid.
pub fn next_run(schedule: &str, after_ms: i64) -> Option<i64> {
    let s = schedule.trim();
    if let Some(m) = s.strip_prefix("every:") {
        let mins: i64 = m.trim().parse().ok().filter(|m| *m >= 1)?;
        return Some(after_ms + mins * 60_000);
    }
    let fields: Vec<&str> = s.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    // Validate once with a dummy value so bad expressions are rejected up front.
    for (f, (lo, hi)) in fields.iter().zip([(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)]) {
        field_matches(f, lo, lo, hi)?;
    }
    let start = time::OffsetDateTime::from_unix_timestamp(after_ms / 1000).ok()?.replace_second(0).ok()?.replace_nanosecond(0).ok()? + time::Duration::minutes(1);
    let mut t = start;
    for _ in 0..(366 * 24 * 60) {
        let dow = t.weekday().number_days_from_sunday() as u32;
        let ok = field_matches(fields[0], t.minute() as u32, 0, 59)?
            && field_matches(fields[1], t.hour() as u32, 0, 23)?
            && field_matches(fields[2], t.day() as u32, 1, 31)?
            && field_matches(fields[3], t.month() as u8 as u32, 1, 12)?
            && (field_matches(fields[4], dow, 0, 6)? || (dow == 0 && field_matches(fields[4], 7, 0, 7)?));
        if ok {
            return Some(t.unix_timestamp() * 1000);
        }
        t += time::Duration::minutes(1);
    }
    None
}

pub async fn run_job(state: Shared, job: CronJob) {
    tracing::info!(job = %job.name, "cron job start");
    let agent = match state.db.agent(&job.agent_id).await {
        Ok(Some(a)) => a,
        _ => match agent::ensure_default_agent(&state).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("cron: no agent: {e:#}");
                return;
            }
        },
    };
    let title = format!("⏰ {}", job.name);
    let session = match state.db.create_session(&agent.id, "cron", Some(&job.id), None, &title).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cron: session: {e:#}");
            return;
        }
    };
    let res = agent::run_turn(state.clone(), session, job.prompt.clone(), TurnOptions { depth: 1, no_reflect: false, agent_override: Some(agent) }).await;
    let status = match &res {
        Ok(r) if r.error.is_none() => "ok".to_string(),
        Ok(r) => format!("error: {}", r.error.clone().unwrap_or_default()),
        Err(e) => format!("error: {e:#}"),
    };
    let next = if job.enabled { next_run(&job.schedule, crate::db::now_ms()) } else { None };
    let _ = state.db.mark_cron_run(&job.id, next, &status).await;
    if let (Some(chat), Ok(r)) = (job.deliver_to.as_deref(), &res) {
        if let (Ok(chat_id), Some(token)) = (chat.trim().parse::<i64>(), state.telegram_token().await) {
            let tg = crate::telegram::Telegram::new(state.http.clone(), token);
            let text = if r.text.trim().is_empty() { status.clone() } else { r.text.clone() };
            let body = format!("<b>⏰ {}</b>\n{}", job.name, crate::telegram::markdown_to_html(&text));
            if let Err(e) = tg.send_long(chat_id, &body).await {
                tracing::warn!("cron: telegram delivery failed: {e:#}");
            }
        }
    }
    tracing::info!(job = %job.name, %status, "cron job done");
}

pub async fn run(state: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        let jobs = match state.db.cron_jobs().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("cron: list failed: {e:#}");
                continue;
            }
        };
        let now = crate::db::now_ms();
        for job in jobs.into_iter().filter(|j| j.enabled) {
            let due = match job.next_run_at {
                Some(n) => n <= now,
                None => {
                    // Never scheduled: compute and store, don't run now.
                    let _ = state.db.mark_cron_run(&job.id, next_run(&job.schedule, now), job.last_status.as_deref().unwrap_or("scheduled")).await;
                    false
                }
            };
            if due {
                // Claim it first so a long run isn't started twice.
                let _ = state.db.mark_cron_run(&job.id, next_run(&job.schedule, now), "running").await;
                let st = state.clone();
                tokio::spawn(async move { run_job(st, job).await });
            }
        }
        let _: Option<Value> = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_minutes() {
        assert_eq!(next_run("every:5", 1000), Some(1000 + 5 * 60_000));
        assert_eq!(next_run("every:0", 0), None);
        assert_eq!(next_run("every:x", 0), None);
    }

    #[test]
    fn cron_expressions() {
        // 2026-01-01 00:00:00 UTC
        let base = 1_767_225_600_000;
        assert_eq!(next_run("*/15 * * * *", base), Some(base + 15 * 60_000));
        assert_eq!(next_run("0 9 * * *", base), Some(base + 9 * 3_600_000));
        // 2026-01-01 is a Thursday (4); next Monday (1) is Jan 5 at 08:30.
        assert_eq!(next_run("30 8 * * 1", base), Some(base + 4 * 86_400_000 + 8 * 3_600_000 + 30 * 60_000));
        assert_eq!(next_run("0 0 1 2 *", base), Some(base + 31 * 86_400_000));
        assert!(next_run("bad", base).is_none());
        assert!(next_run("61 * * * *", base).is_none());
    }

    #[test]
    fn field_matching() {
        assert_eq!(field_matches("1,3,5-7", 6, 0, 59), Some(true));
        assert_eq!(field_matches("1,3,5-7", 4, 0, 59), Some(false));
        assert_eq!(field_matches("*/20", 40, 0, 59), Some(true));
        assert_eq!(field_matches("*/20", 30, 0, 59), Some(false));
        assert_eq!(field_matches("a", 1, 0, 59), None);
    }
}
