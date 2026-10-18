//! kongming as a library so integration tests (and other binaries) can drive
//! the daemon in-process. The binary in `main.rs` only wires these together.

pub mod agent;
pub mod cdp;
pub mod config;
pub mod cron;
pub mod db;
pub mod llm;
pub mod mcp;
pub mod state;
pub mod telegram;
pub mod web;
