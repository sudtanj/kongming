//! The daemon's timers as plain functions. Natively each loop (`cron::run`,
//! `proactive::run`, `earn::run`, the MCP refresh in `main.rs`) calls its own
//! part of this on its interval. A host without long-lived tasks (a Worker)
//! calls `tick` from an alarm, then `start` for each item, steps the returned
//! turn with `agent::step_turn` across invocations, and calls `finish` with
//! the turn's outcome.

use std::sync::atomic::AtomicBool;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::{self, Step, TurnResult, TurnState};
use crate::db::CronJob;
use crate::earn::{ScanStart, ScanTurn};
use crate::proactive::CheckInTurn;
use crate::state::Shared;

/// Something a timer found due.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum Due {
    /// A scheduled prompt, already claimed (marked running).
    Cron { job: CronJob },
    /// A proactive check-in.
    CheckIn,
    /// A scheduled earnings scan.
    EarnScan,
    /// The dealwork.ai heartbeat (one request, no turn).
    EarnHeartbeat,
    /// Re-read the MCP servers' tool lists (one or more requests per server, no turn).
    McpRefresh,
}

/// What is due at `now`. Cheap: settings and the cron table, no outside
/// requests. Due cron jobs are claimed here.
pub async fn tick(state: &Shared, now: i64) -> Vec<Due> {
    let mut due: Vec<Due> = crate::cron::tick(state, now).await.into_iter().map(|job| Due::Cron { job }).collect();
    if crate::proactive::tick(state, now).await {
        due.push(Due::CheckIn);
    }
    if crate::earn::tick(state, now).await {
        due.push(Due::EarnScan);
    }
    if crate::earn::heartbeat_due(state, now).await {
        due.push(Due::EarnHeartbeat);
    }
    if state.mcp.refresh_due(now) {
        due.push(Due::McpRefresh);
    }
    due
}

/// What runs after a background turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum Followup {
    Cron { job: CronJob },
    CheckIn { check_in: CheckInTurn },
    EarnScan { scan: ScanTurn },
}

/// A background turn for the host to step, and what to do once it ends.
/// Serializable, like `TurnState`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobTurn {
    pub turn: TurnState,
    pub followup: Followup,
}

/// Start a due item. Work without a turn (heartbeat, MCP refresh, a scan
/// with nothing for the agent) is done when this returns `None`.
pub async fn start(state: &Shared, due: Due) -> Result<Option<JobTurn>> {
    match due {
        Due::Cron { job } => {
            let (session, opts) = match crate::cron::start_job(state, &job).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("cron: {e:#}");
                    return Ok(None);
                }
            };
            let prompt = job.prompt.clone();
            begin(state, session, prompt, opts, Followup::Cron { job }).await
        }
        Due::CheckIn => {
            if !crate::proactive::claim_check_in(state) {
                return Ok(None);
            }
            match crate::proactive::prepare_check_in(state, false).await {
                Ok(Some(t)) => {
                    let (session, prompt, opts) = (t.session.clone(), t.prompt.clone(), t.turn_options());
                    begin(state, session, prompt, opts, Followup::CheckIn { check_in: t }).await
                }
                Ok(None) => {
                    crate::proactive::release_check_in(state);
                    Ok(None)
                }
                Err(e) => {
                    crate::proactive::release_check_in(state);
                    Err(e)
                }
            }
        }
        Due::EarnScan => {
            if !crate::earn::claim_scan(state) {
                return Ok(None);
            }
            match crate::earn::prepare_scan(state, false).await {
                Ok(ScanStart::Turn(t)) => {
                    let (session, prompt, opts) = (t.session.clone(), t.prompt.clone(), t.turn_options());
                    begin(state, session, prompt, opts, Followup::EarnScan { scan: *t }).await
                }
                Ok(ScanStart::Done(_)) => {
                    crate::earn::release_scan(state);
                    Ok(None)
                }
                Err(e) => {
                    crate::earn::release_scan(state);
                    Err(e)
                }
            }
        }
        Due::EarnHeartbeat => {
            crate::earn::heartbeat(state).await;
            Ok(None)
        }
        Due::McpRefresh => {
            state.mcp.refresh_all().await;
            Ok(None)
        }
    }
}

async fn begin(
    state: &Shared,
    session: crate::db::Session,
    prompt: String,
    opts: agent::TurnOptions,
    followup: Followup,
) -> Result<Option<JobTurn>> {
    match agent::begin_turn(state, session, prompt, opts).await {
        Ok(turn) => Ok(Some(JobTurn { turn, followup })),
        Err(e) => {
            finish(state, followup, Err(e)).await?;
            Ok(None)
        }
    }
}

/// After a background turn ended (or failed): deliveries, bookkeeping and
/// cleanup. Call it exactly once per `JobTurn`, also when stepping failed.
pub async fn finish(state: &Shared, followup: Followup, res: Result<TurnResult>) -> Result<()> {
    match followup {
        Followup::Cron { job } => {
            crate::cron::finish_job(state, &job, res).await;
            Ok(())
        }
        Followup::CheckIn { check_in } => {
            let r = crate::proactive::finish_check_in(state, check_in, res).await;
            crate::proactive::release_check_in(state);
            r.map(|_| ())
        }
        Followup::EarnScan { scan } => {
            let r = crate::earn::finish_scan(state, scan, res).await;
            crate::earn::release_scan(state);
            r.map(|_| ())
        }
    }
}

/// Run one due item to the end in this invocation: for a host with the
/// budget for a whole turn.
pub async fn run(state: &Shared, due: Due) -> Result<()> {
    let Some(job) = start(state, due).await? else {
        return Ok(());
    };
    let cancel = AtomicBool::new(false);
    let mut ts = job.turn;
    let res = loop {
        match agent::step_turn(state, ts, &cancel).await {
            Ok(Step::Continue(next)) => ts = *next,
            Ok(Step::Done(r)) => break Ok(r),
            Err(e) => break Err(e),
        }
    };
    finish(state, job.followup, res).await
}
