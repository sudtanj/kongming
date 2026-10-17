# kongming

A small, self-improving agentic AI daemon that fits on an Orange Pi Zero 3 (1 GB RAM). One static Rust binary, a nanobot-style web UI, Telegram, remote MCP tools, a team of agents with roles, and skills + memory that the agents refine while they run. **No local storage**: everything lives in a remote [Turso](https://turso.tech) SQLite database, so the container runs read-only and a wiped SD card loses nothing.

Inspired by [nanobot](https://github.com/HKUDS/nanobot) (UI, `@` mentions), [hermes-agent](https://github.com/NousResearch/hermes-agent) (skills, reflection, compaction, delegation) and the role-based agent teams of Grok Bot and OpenAI Dots.

## Features

- **Agents with roles.** Each agent has a handle, name, job title, description, persona prompt, optional model override, tool/MCP/skill allow-lists and a manager. A manager can `delegate_task` to its direct reports; the report works in its own session and returns a summary. Talk to any agent directly with `@handle` (sticky per session) or pick it in the UI/Telegram.
- **Skills.** Hermes-style `SKILL.md` documents (name, when-to-use description, Markdown body) stored in Turso with version history. The index is in every prompt; the agent loads a skill with `skill_view` and creates/updates them with `skill_manage`. Import/export SKILL.md, pin skills to agents, attach one to a turn with `@skill:name`.
- **Self-improvement loop.** After turns that used tools or contained a correction, a cheap reflection pass reviews the conversation with only `skill_manage`/`memory` available and saves procedures and durable facts. Everything it does is logged under Activity and shown as toasts.
- **Memory.** Shared or per-agent facts, preferences, lessons; searchable; injected into prompts.
- **Context compaction.** Older turns are summarized into a rolling session summary when the estimated context exceeds the budget.
- **MCP.** Remote servers only (Streamable HTTP or legacy SSE) with no auth, bearer token, or OAuth 2.1 (metadata discovery, dynamic client registration, PKCE, refresh). Tools are exposed as `mcp__<server>__<tool>`. `@mcp:name` in a message makes sure a server's tools are offered.
- **Telegram.** Long polling, user-id allow-list, streamed replies via message edits, `/new`, `/agents`, `/agent <handle>`, `/skills`, `/status`, `/stop`, `@handle` switching. Cron jobs can deliver to a chat.
- **Automations.** `every:<minutes>` or 5-field cron (UTC) prompts run by an agent of your choice.
- **Web UI.** Vanilla HTML/CSS/JS port of the nanobot UI (sidebar sessions, streaming thread, reasoning and tool activity cards, `@`/`/` palettes, settings pages for agents, skills, MCP, channels, models, automations, memory, activity). Password login, cookie session, WebSocket streaming.
- **Providers.** Any OpenAI-compatible chat-completions endpoint (OpenAI, OpenRouter, zilong-router, Ollama, vLLM) or the Anthropic Messages API (adaptive thinking, prompt caching).

## Run

```bash
docker run -d --name kongming --read-only --cap-drop ALL \
  -p 127.0.0.1:18800:18800 \
  -e TURSO_DATABASE_URL=libsql://<db>-<org>.turso.io \
  -e TURSO_AUTH_TOKEN=<token> \
  -e KONGMING_PASSWORD=<ui password> \
  -e KONGMING_SECRET=<random string> \
  -e LLM_PROVIDER=openai -e LLM_BASE_URL=https://api.openai.com/v1 -e LLM_API_KEY=sk-... -e LLM_MODEL=gpt-4o-mini \
  -e TELEGRAM_BOT_TOKEN=... -e TELEGRAM_ALLOWED_USERS=123456789 \
  --memory 96m --cpus 1 sudtanj/kongming:latest
```

Open `http://127.0.0.1:18800`. Everything except the Turso credentials, port and UI password can also be configured in the UI (Settings) and is stored in Turso; environment values are defaults.

| Variable | Meaning | Default |
|---|---|---|
| `TURSO_DATABASE_URL`, `TURSO_AUTH_TOKEN` | Turso database (required) | |
| `PORT`, `KONGMING_HOST` | listen address | `18800`, `0.0.0.0` |
| `KONGMING_PASSWORD` | UI password (empty = no auth; only on a private network) | empty |
| `KONGMING_SECRET` | cookie signing secret (random per boot if unset) | |
| `KONGMING_PUBLIC_URL` | public base URL for MCP OAuth redirects | |
| `LLM_PROVIDER` | `openai` or `anthropic` | `openai` |
| `LLM_BASE_URL`, `LLM_API_KEY`, `LLM_MODEL` | provider settings | |
| `TELEGRAM_BOT_TOKEN`, `TELEGRAM_ALLOWED_USERS` | Telegram bot and comma-separated user ids | |
| `KONGMING_WORKERS` | tokio workers | `1` |
| `KONGMING_MAX_ITERATIONS` | tool-call rounds per turn | `24` |
| `KONGMING_CONTEXT_TOKENS` | compaction threshold | `48000` |
| `LOG_LEVEL` | tracing filter | `info` |

The Orange Pi deployment script lives in [`sudtanj/infrastructure-tools`](https://github.com/sudtanj/infrastructure-tools) (`bash-scripts/init-kongming.sh`, image from `builder-tools/kongming`).

## API

REST under `/api` (cookie or `Authorization: Bearer <token>` from `/api/auth/login`), WebSocket at `/ws`. Main resources: `sessions`, `sessions/:id/messages` (`?wait=true` returns the final text), `agents`, `skills`, `memories`, `mcp`, `settings`, `cron`, `tasks`, `improvements`, `usage`, `info`. See `src/web/api.rs`.

## Development

```bash
cargo test                 # unit + integration tests (mocks for Turso, LLM, MCP)
cargo clippy --all-targets -- -D warnings
cross build --release --target aarch64-unknown-linux-musl
```

CI builds static musl binaries for `aarch64` and `x86_64` and attaches them to GitHub releases on `v*` tags.

## License

MIT
