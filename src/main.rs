//! kongming: a small agentic AI daemon for 1 GB boards. Web UI (nanobot-style),
//! Telegram, remote MCP tools, agent roles with delegation, skills and memory
//! that the agents improve while running. All state lives on Turso.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use kongming::{agent, config, cron, db, state, telegram, web};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // `kongming cdp ...` (or the `cdp` link in the image) is the browser client for the shell tool.
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(|a| a.rsplit('/').next().unwrap_or(a).to_string()).unwrap_or_default();
    if prog == "cdp" {
        std::process::exit(kongming::cdp::main(args[1..].to_vec()));
    }
    if args.get(1).map(String::as_str) == Some("cdp") {
        std::process::exit(kongming::cdp::main(args[2..].to_vec()));
    }
    let cfg = config::Config::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cfg.log_level).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .compact()
        .init();
    let workers = cfg.workers.max(1);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(4)
        .thread_stack_size(512 * 1024)
        .enable_all()
        .build()
        .context("tokio runtime")?
        .block_on(run(cfg))
}

async fn run(cfg: config::Config) -> Result<()> {
    let turso = db::Turso::new(&cfg.turso_url, &cfg.turso_token)?;
    let database = db::Db::new(turso);
    database.migrate().await.context("applying schema on Turso")?;
    tracing::info!(host = %database.turso.host(), "turso ready");

    if cfg.password.is_empty() {
        tracing::warn!("KONGMING_PASSWORD is empty: the web UI is open to anyone who can reach the port");
    }

    let state = state::AppState::new(cfg.clone(), database);
    if let Some(sh) = &state.shell {
        // Shell commands run as the same user: keep them out of /proc/<pid>/environ
        // and /proc/<pid>/mem, which hold the Turso token and the password.
        // SAFETY: plain prctl call with constant arguments.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            tracing::warn!("could not mark the process non-dumpable");
        }
        tracing::info!(sh = %sh.sh.display(), confinement = sh.confinement(), "shell tool enabled");
    }
    agent::ensure_default_agent(&state).await?;
    if let Err(e) = agent::seed_skills(&state).await {
        tracing::warn!("seeding skills failed: {e:#}");
    }

    // Background work: MCP tool discovery, Telegram, cron.
    {
        let st = state.clone();
        tokio::spawn(async move {
            st.mcp.refresh_all().await;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1800));
            tick.tick().await;
            loop {
                tick.tick().await;
                st.mcp.refresh_all().await;
            }
        });
    }
    tokio::spawn(telegram::run(state.clone()));
    tokio::spawn(cron::run(state.clone()));

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse().context("invalid host/port")?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "kongming listening");
    axum::serve(listener, web::router(state.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
        .context("server error")?;
    Ok(())
}
