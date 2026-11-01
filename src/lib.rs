//! kongming as a library so integration tests (and other binaries) can drive
//! the daemon in-process. The binary in `main.rs` only wires these together.
//!
//! Features: `native` (default) is the daemon. Without it (`worker`) the
//! portable core builds for wasm32-unknown-unknown and the host provides SQL
//! (`db::SqlExecutor`), HTTP (`net::Http`) and background work (`host::Host`).

pub mod agent;
#[cfg(feature = "native")]
pub mod cdp;
pub mod config;
pub mod cron;
pub mod db;
pub mod earn;
#[cfg(feature = "native")]
pub mod fetch;
pub mod host;
pub mod llm;
pub mod mcp;
pub mod net;
pub mod platform;
pub mod proactive;
pub mod state;
pub mod telegram;
pub mod web;
