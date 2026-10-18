# kongming

A small, self-improving agentic AI daemon that fits on an Orange Pi Zero 3 (1 GB RAM). One small Rust binary, a nanobot-style web UI, Telegram, remote MCP tools, a team of agents with roles, and skills + memory that the agents refine while they run. **No local storage**: everything lives in a remote [Turso](https://turso.tech) SQLite database, so the container runs read-only and a wiped SD card loses nothing.

Inspired by [nanobot](https://github.com/HKUDS/nanobot) (UI, `@` mentions), [hermes-agent](https://github.com/NousResearch/hermes-agent) (skills, reflection, compaction, delegation) and the role-based agent teams of Grok Bot and OpenAI Dots.

## Features

- **Agents with roles.** Each agent has a handle, name, job title, description, persona prompt, optional model override, tool/MCP/skill allow-lists and a manager. A manager can `delegate_task` to its direct reports; the report works in its own session and returns a summary. Talk to any agent directly with `@handle` (sticky per session) or pick it in the UI/Telegram.
- **Skills.** Hermes-style `SKILL.md` documents (name, when-to-use description, Markdown body) stored in Turso with version history. The index is in every prompt; the agent loads a skill with `skill_view` and creates/updates them with `skill_manage`. Import/export SKILL.md, pin skills to agents, attach one to a turn with `@skill:name`.
- **Self-improvement loop.** After turns that used tools or contained a correction, a cheap reflection pass reviews the conversation with only `skill_manage`/`memory` available and saves procedures and durable facts. Everything it does is logged under Activity and shown as toasts.
- **Memory.** Shared or per-agent facts, preferences, lessons; searchable; injected into prompts.
- **Context compaction.** Older turns are summarized into a rolling session summary when the estimated context exceeds the budget.
- **MCP.** Remote servers only (Streamable HTTP or legacy SSE) with no auth, bearer token, or OAuth 2.1 (metadata discovery, dynamic client registration, PKCE, refresh; works behind a firewall by pasting the redirect URL). Tools are exposed as `mcp__<server>__<tool>`. `@mcp:name` in a message makes sure a server's tools are offered.
- **Telegram.** Long polling, user-id allow-list, streamed replies via message edits, `/new`, `/agents`, `/agent <handle>`, `/skills`, `/status`, `/stop`, `@handle` switching. Cron jobs can deliver to a chat.
- **Automations.** `every:<minutes>` or 5-field cron (UTC) prompts run by an agent of your choice.
- **Web UI.** Vanilla HTML/CSS/JS port of the nanobot UI (sidebar sessions, streaming thread, reasoning and tool activity cards, `@`/`/` palettes, settings pages for agents, skills, MCP, channels, models, automations, memory, activity). Password login, cookie session, WebSocket streaming.
- **Providers.** Any OpenAI-compatible chat-completions endpoint (OpenAI, OpenRouter, zilong-router, Ollama, vLLM) or the Anthropic Messages API (adaptive thinking, prompt caching).

## System requirements

kongming runs no model locally; it is a small HTTP client and server, so the device mostly waits on the network.

| | Minimum | Recommended |
|---|---|---|
| CPU | 64-bit ARM (aarch64, e.g. Cortex-A53) or x86_64 | Orange Pi Zero 3 (4× Cortex-A53) or better |
| RAM free for kongming | 64 MB | 128 MB (the deploy script caps the container at 96 MB) |
| Disk | about 12 MB for the image, or 5 MB for the bare binary | same; no data directory, the root filesystem can be read-only |
| OS | Linux with Docker 20.10+, or any Linux for the static release binary | Armbian / Debian 12 |
| Clock | NTP-synced (TLS, OAuth token expiry and cron need correct time) | |

32-bit ARM (armv7, e.g. the original Orange Pi Zero) is not built. Memory figures are estimates for one worker thread with a few concurrent chats; measure on your device with `docker stats kongming`.

External services:

- **Turso database** (required). The free tier is enough; every read and write goes over HTTPS, so pick a Turso region close to the device.
- **An LLM endpoint** (required): OpenAI-compatible or Anthropic.
- **Telegram bot token** (optional).
- **Remote MCP servers** (optional). Local/stdio MCP servers are not supported by design.

Network: outbound HTTPS only (Turso, the LLM, `api.telegram.org`, MCP servers). Telegram uses long polling and MCP OAuth can finish by pasting the redirect URL, so nothing has to be reachable from the internet. The web UI port only needs to be reachable from your LAN or tailnet.

Browser: a current Chrome, Firefox or Safari for the web UI.

## Run

```bash
docker run -d --name kongming --read-only --cap-drop ALL \
  -p 127.0.0.1:18800:18800 \
  -e KONGMING_TURSO_DATABASE_URL=libsql://<db>-<org>.turso.io \
  -e KONGMING_TURSO_AUTH_TOKEN=<token> \
  -e KONGMING_PASSWORD=<ui password> \
  -e KONGMING_SECRET=<random string> \
  --memory 96m --cpus 1 sudtanj/kongming:latest
```

Open `http://127.0.0.1:18800`, log in and set up the rest in the dashboard; it is stored in Turso:

- **Settings → Models**: LLM provider (OpenAI-compatible or Anthropic), base URL, API key and model.
- **Settings → Channels**: Telegram bot token from @BotFather and the allowed user ids. Saving the token checks it with Telegram and connects the bot right away (see below).

Only the Turso credentials and the process settings come from the environment:

| Variable | Meaning | Default |
|---|---|---|
| `KONGMING_TURSO_DATABASE_URL`, `KONGMING_TURSO_AUTH_TOKEN` | Turso database (required) | |
| `PORT`, `KONGMING_HOST` | listen address | `18800`, `0.0.0.0` |
| `KONGMING_PASSWORD` | UI password (empty = no auth; only on a private network) | empty |
| `KONGMING_SECRET` | cookie signing secret (random per boot if unset) | |
| `KONGMING_PUBLIC_URL` | public HTTPS base URL; only set it if kongming is reachable from the internet (MCP OAuth redirects, Telegram webhook). Can also be set in Settings | empty |
| `KONGMING_WORKERS` | tokio workers | `1` |
| `KONGMING_MAX_ITERATIONS` | tool-call rounds per turn | `24` |
| `KONGMING_CONTEXT_TOKENS` | compaction threshold | `48000` |
| `LOG_LEVEL` | tracing filter | `info` |

The Orange Pi deployment script lives in [`sudtanj/infrastructure-tools`](https://github.com/sudtanj/infrastructure-tools) (`bash-scripts/init-kongming.sh`, image from `builder-tools/kongming`).

### Telegram

Paste the bot token in Settings → Channels and press Save. kongming checks the token with `getMe` (a wrong token is refused and not stored), registers the bot commands and picks how updates arrive:

- **No public URL (behind a firewall, the default):** long polling. kongming removes any webhook left on the bot and fetches updates itself, so nothing has to reach the machine.
- **Public HTTPS URL set:** webhook. kongming calls `setWebhook` with `<public URL>/api/telegram/webhook` and a random secret, and only accepts deliveries that carry that secret.

The page shows the bot name, the mode in use, or the error Telegram returned. Turning Telegram off or clearing the token removes the webhook. Only the user ids in the allowlist can talk to the bot; anyone else is told their id so you can add them.

### MCP OAuth behind a firewall

When `KONGMING_PUBLIC_URL` is empty (the default), OAuth uses the loopback redirect for native apps (RFC 8252): `http://localhost:<PORT>/api/mcp/oauth/callback`.

1. In Settings → MCP, add the server with auth type OAuth 2.1 and press **Connect with OAuth**.
2. Press **Open sign-in page** (or copy the link to any browser) and approve access.
3. The provider sends the browser to the localhost address. That page usually fails to load, which is expected. Copy the full URL from the address bar.
4. Paste it into the dialog and press **Complete sign-in**. kongming exchanges the code (with PKCE) and stores the tokens in Turso; refreshes happen on their own.

If the browser runs on the same machine as kongming, the localhost page loads and the sign-in completes by itself. If kongming is reachable at a public HTTPS address, set `KONGMING_PUBLIC_URL` (or Settings → Models → Public URL) and the provider redirects straight back. Providers without dynamic client registration need a client ID (and secret) registered with the redirect URI shown in the dialog.

## API

REST under `/api` (cookie or `Authorization: Bearer <token>` from `/api/auth/login`), WebSocket at `/ws`. Main resources: `sessions`, `sessions/:id/messages` (`?wait=true` returns the final text), `agents`, `skills`, `memories`, `mcp`, `settings`, `cron`, `tasks`, `improvements`, `usage`, `info`. See `src/web/api.rs`.

## Development

```bash
cargo test                 # unit + integration tests (mocks for Turso, LLM, MCP)
cargo clippy --all-targets -- -D warnings
cross build --release --target aarch64-unknown-linux-musl
```

CI builds static musl binaries for `aarch64` and `x86_64` and attaches them to GitHub releases on `v*` tags. The Docker image is a glibc build on `distroless/cc`; see `builder-tools/kongming` in infrastructure-tools.

Every push also runs an Orange Pi Zero 3 simulation (`ci/orangepi-sim.sh`). The aarch64 test binaries and the release binary run in an arm64 container cut to the board: 4 CPUs, 1 GB RAM with no swap, and a read-only root filesystem. One run is on an x86 runner under QEMU pinned to the board's Cortex-A53. This catches CPU features the H618 lacks, and the slowdown shakes out timing bugs. The other runs natively on an arm64 runner with the daemon held to 128 MB. Both boot the daemon against a real libSQL server, push a burst of API traffic, restart it and check that everything comes back from the database. Memory figures land in the job summary. A release is only published when the simulation passes.

## License

MIT
