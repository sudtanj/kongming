//! What the process around the core decides: where background work runs,
//! who else sees events, and how a person's message becomes a running turn.
//! The native daemon uses `NativeHost` (tokio tasks, the broadcast bus only);
//! a Worker implements `Host` to persist turns and drive them from alarms
//! with `agent::step_turn`, and to push events to its WebSockets.

use futures_util::future::BoxFuture;

use crate::agent::TurnOptions;
use crate::db::Session;
use crate::state::{Event, Shared};

pub trait Host: Send + Sync + 'static {
    /// Background work that outlives the request that started it.
    fn spawn(&self, task: BoxFuture<'static, ()>) {
        crate::platform::spawn(task);
    }

    /// Every event, in order, as it is emitted (the broadcast bus gets it too).
    fn emit(&self, _ev: &Event) {}

    /// A message from the dashboard should start a turn without waiting for
    /// it. Natively the whole turn runs in a background task.
    fn start_turn(&self, state: Shared, session: Session, text: String, opts: TurnOptions) {
        self.spawn(Box::pin(async move {
            if let Err(e) = crate::agent::run_turn(state, session, text, opts).await {
                tracing::warn!("turn failed: {e:#}");
            }
        }));
    }
}

/// The daemon's host: tokio tasks and the broadcast bus.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeHost;

impl Host for NativeHost {}
