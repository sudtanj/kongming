# kongming — notes for coding agents

Single Rust binary (`src/main.rs` + `src/lib.rs`). No local storage: every row is on Turso over its HTTP pipeline API (`src/db/turso.rs`); the Docker image runs read-only. Target is an Orange Pi Zero 3 (1 GB RAM), so: one tokio worker, no C SQLite in the release binary, small dependencies, nothing spawned beside the daemon except the `shell` tool's BusyBox `sh` (`agent/shell.rs`: empty env, tmpfs workdir, process group killed on timeout; the daemon is non-dumpable so `/proc` does not leak its env). `cdp.rs` is a tiny CDP/WebSocket client run as `kongming cdp …` (or the `cdp` link) from that shell. Local/stdio MCP servers are still refused on purpose.

Layout:
- `db/` store + schema (`SCHEMA` is idempotent, re-applied at boot) + row models (camelCase JSON).
- `llm/` provider trait with OpenAI-compatible and Anthropic streaming implementations; `sse.rs` is the shared SSE parser.
- `agent/` the loop (`run_turn`), prompt assembly, `@` mentions, built-in tools, reflection pass (self-improvement) and delegation (`run_delegation`).
- `mcp/` remote MCP client (streamable HTTP + legacy SSE) and OAuth (discovery, dynamic registration, PKCE, refresh).
- `telegram/` long-polling bot with streamed edits. `cron.rs` scheduled prompts.
- `web/` axum router, cookie auth, REST (`api.rs`), WebSocket (`ws.rs`); the UI in `web/` is embedded with `include_str!`.
- `tests/` integration tests against mocks (Hrana/Turso over rusqlite, OpenAI-style LLM, MCP).

Conventions: errors are `anyhow`, API errors become `{ "error": ... }`; events on the bus are the `state::Event` enum (serialized `type` snake_case, fields camelCase); never log secrets; keep the system prompt's stable sections first for prompt caching.

Development machines may be tiny: don't run the app, docker or the full test suite locally unless you know the machine can take it; CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, tests and cross builds for aarch64/x86_64 musl, plus an Orange Pi Zero 3 simulation (`ci/orangepi-sim.sh`: aarch64 tests and a daemon smoke test against a real libSQL server in a 4-CPU, 1 GB, read-only container, under QEMU Cortex-A53 and natively on arm64). Test waits use `support::timeout()` (`KONGMING_TEST_TIMEOUT_SECS`), never a fixed duration. The Docker image is built from the `builder-tools/kongming` folder of `sudtanj/infrastructure-tools`.
