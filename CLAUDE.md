# kongming — notes for coding agents

Single Rust binary (`src/main.rs` + `src/lib.rs`). No local storage: every row is on Turso over its HTTP pipeline API (`src/db/turso.rs`); the Docker image runs read-only. Target is an Orange Pi Zero 3 (1 GB RAM), so: one tokio worker, no C SQLite in the release binary, small dependencies, nothing spawned beside the daemon (local/stdio MCP servers are refused on purpose).

Layout:
- `db/` store + schema (`SCHEMA` is idempotent, re-applied at boot) + row models (camelCase JSON).
- `llm/` provider trait with OpenAI-compatible and Anthropic streaming implementations; `sse.rs` is the shared SSE parser.
- `agent/` the loop (`run_turn`), prompt assembly, `@` mentions, built-in tools, reflection pass (self-improvement) and delegation (`run_delegation`).
- `mcp/` remote MCP client (streamable HTTP + legacy SSE) and OAuth (discovery, dynamic registration, PKCE, refresh).
- `telegram/` long-polling bot with streamed edits. `cron.rs` scheduled prompts.
- `web/` axum router, cookie auth, REST (`api.rs`), WebSocket (`ws.rs`); the UI in `web/` is embedded with `include_str!`.
- `tests/` integration tests against mocks (Hrana/Turso over rusqlite, OpenAI-style LLM, MCP).

Conventions: errors are `anyhow`, API errors become `{ "error": ... }`; events on the bus are the `state::Event` enum (serialized `type` snake_case, fields camelCase); never log secrets; keep the system prompt's stable sections first for prompt caching.

Development machines may be tiny: don't run the app, docker or the full test suite locally unless you know the machine can take it; CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, tests and cross builds for aarch64/x86_64 musl. The Docker image is built from the `builder-tools/kongming` folder of `sudtanj/infrastructure-tools`.
