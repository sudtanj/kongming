<p align="center"><img src="web/logo.svg" width="112" height="112" alt="kongming logo: a feather fan (羽扇)"></p>

<h1 align="center">kongming 孔明</h1>

<p align="center"><b>非淡泊无以明志，非宁静无以致远</b><br><i>Without calm and simplicity one cannot see one's purpose or reach far.</i> (诸葛亮《诫子书》)</p>

A small, self-improving agentic AI daemon that fits on an Orange Pi Zero 3 (1 GB RAM). One small Rust binary, a nanobot-style web UI, Telegram, remote MCP tools, a team of agents with roles, and skills + memory that the agents refine while they run. **No local storage**: everything lives in a remote [Turso](https://turso.tech) SQLite database, so the container runs read-only and a wiped SD card loses nothing.

Inspired by [nanobot](https://github.com/HKUDS/nanobot) (UI, `@` mentions), [hermes-agent](https://github.com/NousResearch/hermes-agent) (skills, reflection, compaction, delegation) and the role-based agent teams of Grok Bot and OpenAI Dots.

## Features

- **Kongming runs the team.** You talk only to Kongming (the default agent); it hires, adjusts and retires its own reports with `agent_manage` (handle, role, description, persona prompt, optional model override, tool/MCP/skill allow-lists) and hands them work with `delegate_task`. A report works in its own session and returns a summary, and Kongming answers for the result. The rest of the team is read-only to you: the dashboard shows each agent's profile and every delegated task (Activity → Delegations), but you cannot message, edit or remove a report; when something goes wrong you tell Kongming, which fixes the work or the report. Naming a teammate with `@handle` tells Kongming who you mean.
- **Skills.** Hermes-style `SKILL.md` documents (name, when-to-use description, Markdown body) stored in Turso with version history. The index is in every prompt; the agent loads a skill with `skill_view` and creates/updates them with `skill_manage`. Import/export SKILL.md, pin skills to agents, attach one to a turn with `@skill:name`. Built-in starter skills (writing skills, delegation, memory hygiene, and `shell-and-browser` for the BusyBox shell and `cdp`) are added once each, so new ones reach existing databases and deleted ones stay deleted.
- **Self-improvement loop.** After turns that used tools or contained a correction, a cheap reflection pass reviews the conversation with only `skill_manage`/`memory` available and saves procedures and durable facts. Everything it does is logged under Activity and shown as toasts.
- **Local shell, read-only.** The `shell` tool runs commands in a static BusyBox `sh` bundled in the image (`grep`, `sed`, `awk`, `wget`, `nc`, … about 1 MB). It is for interacting (the browser, hosts and URLs on the tailnet, quick text processing), not for storage: nothing can be written to disk. A Landlock ruleset refuses creating, changing or deleting files (only `/dev/null` is writable), and where the kernel has no Landlock a zero file-size limit keeps anything that can still be created empty; data moves through pipes. Each command also gets an empty environment (no tokens or passwords), `/` as its working directory, a time limit (30 s by default, at most 5 min), and its whole process group is killed when it ends. The daemon marks itself non-dumpable so a command cannot read its secrets through `/proc`, and the kernel kills a runaway command before the daemon when memory runs out. It is still a shell inside the container: anyone you let talk to an agent (the web login, the Telegram allow-list) can run commands through it. Turn it off with `KONGMING_SHELL=off` or leave `shell` out of an agent's tools.
- **Browser over the tailnet.** BusyBox has no WebSocket client, so the kongming binary doubles as a small Chrome DevTools Protocol (CDP) client: `cdp` in the shell drives a real Chrome on port 9222 (for example the `chrome-cdp` session in infrastructure-tools, reached over Tailscale). `cdp tabs`, `cdp goto URL`, `cdp text`, `cdp click SELECTOR`, `cdp type SELECTOR TEXT`, `cdp press Enter`, `cdp eval JS`, `cdp call Method JSON`; clicks are real mouse events and no `Runtime.enable` is sent. The browser address is not configured in kongming: give it to the agents in a skill (`cdp --url http://<ip>:9222 …`); the built-in `shell-and-browser` skill explains the commands.
- **Memory.** Shared or per-agent facts, preferences, lessons; searchable; injected into prompts.
- **Automatic context compaction.** When the next request (system prompt, tool schemas and history) would pass the budget, older messages are folded into a rolling session summary and the latest ones stay verbatim. This also happens in the middle of a long task, between tool rounds, without splitting a tool call from its results; the request being worked on is restated after the summary. If the model still rejects a request as too long, kongming compacts harder and retries once.
- **MCP.** Remote servers only (Streamable HTTP or legacy SSE) with no auth, bearer token, or OAuth 2.1 (metadata discovery, dynamic client registration, PKCE, refresh; works behind a firewall by pasting the redirect URL). Tools are exposed as `mcp__<server>__<tool>`. `@mcp:name` in a message makes sure a server's tools are offered.
- **Telegram.** Long polling, user-id allow-list, streamed replies via message edits, a live *thinking* message above each answer with the model's reasoning (in a collapsed quote) and the tools it calls (Settings → Channels → Show thinking), `/new`, `/agents` (the team), `/skills`, `/status`, `/stop`, `/proactive` (on, off, now), `/earn` (on, off, scan); every message goes to Kongming. Cron jobs can deliver to a chat. Conversations that came in from Telegram carry a Telegram badge in the dashboard (sidebar and chat header).
- **Proactive.** Kongming looks out for you between conversations and messages you on Telegram when something is worth it. `notify_user` sends one message (copied into your Telegram conversation, so a reply continues from it); `schedule` turns "remind me at 9" into a one-off job and "tell me when the price drops" into a recurring check that messages you only when the condition is met. The reflection pass saves the topics you follow as `interest` memories and what you asked to be told about as `watch` memories, and a periodic check-in (every 3 h by default, never in quiet hours) looks at both and at recent conversations, checks with its tools, and sends only what is new and useful. Limits: the same topic is not sent twice in a row, at most 5 unrequested messages a day, silent during quiet hours unless urgent. Settings → Channels → Proactive messages (interval, daily limit, quiet hours, your UTC offset, "Check in now"), or `/proactive` in Telegram. A check-in that finds nothing leaves no conversation behind; everything sent is listed under Activity.
- **Earnings.** Kongming looks for paid work for you and, with your go-ahead, does it. Every 2 h (Settings → Earnings) it lists open work on agent marketplaces (toku.agency, dealwork.ai, OpenTask, MoltJobs, HYRVE AI, Claw Earn) and code-bounty boards (Opire; Algora through GitHub search, counting only bounties the Algora bot really posted), drops other agents advertising themselves and anything below your minimum pay, rates the rest against what you want to take on, and sends one Telegram digest of the best fits. Reply `take <ref>` (or press Take in the dashboard) and it bids, does the work and delivers through the provider's API with the key you saved there; the key never reaches the model. `skip <ref>` passes. Optional auto mode bids on small marketplace jobs without asking (up to a limit you set). Signing up, API keys, withdrawals and payouts are always yours: the agent cannot call those paths. Claw Earn (needs a staked crypto wallet) and code bounties (claimed with a pull request from your GitHub account) are listed for you to take yourself; Kongming prepares the work.
- **Paste it, Kongming sets it up.** Paste a model provider key, a Telegram bot token, an MCP config (`{"mcpServers": …}`) or server URL, a paid-work site key, or a SKILL.md (or a link to one) into a chat, and Kongming configures it with its `setup` tool through the same code as the Settings pages, then says what changed. Keys and tokens (Anthropic, OpenAI-style `sk-…`, OpenRouter, Google, GitHub, Slack, Telegram bot tokens, and anything after `key:`, `token=`, `Bearer`, `password` …) are cut out of your message before it is stored or sent to the model: the text keeps `[secret:<id> <kind> …1234]` (a lock chip in the dashboard) and the value waits in the vault for a day for `setup` to use. On Telegram the message holding the key is deleted from the chat. A new model setting is tested before it is saved, so a typo cannot cut you off; local (stdio) MCP servers are still refused.
- **Automations.** `every:<minutes>`, `at:<time>` (once) or 5-field cron (UTC) prompts run by an agent of your choice.
- **Web UI.** Vanilla HTML/CSS/JS port of the nanobot UI (sidebar sessions, streaming thread, reasoning and tool activity cards, `@`/`/` palettes, settings pages for agents, skills, MCP, channels, models, automations, memory, activity). Password login, cookie session, WebSocket streaming.
- **Providers.** Any OpenAI-compatible chat-completions endpoint (OpenAI, OpenRouter, zilong-router, Ollama, vLLM) or the Anthropic Messages API (adaptive thinking, prompt caching).

## The name: 孔明

**诸葛亮 Zhuge Liang** (181–234), courtesy name **孔明 Kongming**, was the chancellor (丞相) of 蜀汉 Shu Han in the Three Kingdoms period (三国). Before he took office he lived as a farmer-scholar at 隆中 Longzhong and was known as **卧龙**, the *Sleeping Dragon*. 刘备 Liu Bei called on his thatched cottage three times (三顾茅庐) before Zhuge Liang agreed to serve him, and on that visit he laid out the strategy that split the empire in three (隆中对). As chancellor he ran the state, drafted its laws and led its campaigns. His memorial to the throne (出师表) is still read in Chinese schools.

The novel 《三国演义》 *Romance of the Three Kingdoms* made him the archetype of the strategist who wins by planning rather than by force: borrowing a hundred thousand arrows with straw boats (草船借箭) and holding off an army with an open gate and a zither (空城计). These episodes are fiction, but they are the Kongming most people know. On stage and in paintings he always carries a **feather fan (羽扇)** and wears a silk scarf (纶巾). The fan is the emblem of calm command, and it is kongming's logo ([`web/logo.svg`](web/logo.svg)). Sky lanterns are called 孔明灯 *Kongming lanterns* after a legend that he invented them to send signals.

That is the idea behind this project. kongming does not do everything itself: it plans, keeps notes (skills and memory), and directs a team of agents, each with its own role, the way the chancellor directed his generals. Its companion router is named after one of them, **赵云 Zhao Yun**, courtesy name **子龙 Zilong** ([zilong-router](https://github.com/sudtanj/zilong-router)). He was the general who carried Liu Bei's infant son through the enemy lines at 长坂坡 Changban, and the router likewise gets every request through to a model that answers. The motto at the top comes from Zhuge Liang's letter of advice to his son (诫子书): a small, quiet daemon on a 1 GB board, reaching far.

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
docker run -d --name kongming --read-only --ipc none --cap-drop ALL \
  -p 18800:18800 \
  -e KONGMING_TURSO_DATABASE_URL=libsql://<db>-<org>.turso.io \
  -e KONGMING_TURSO_AUTH_TOKEN=<token> \
  -e KONGMING_PASSWORD=<ui password> \
  -e KONGMING_SECRET=<random string> \
  --memory 96m --cpus 1 sudtanj/kongming:latest
```

Open `http://<board-ip>:18800` from any machine on your network (the port is published on every interface, so keep `KONGMING_PASSWORD` set), log in and set up the rest in the dashboard; it is stored in Turso:

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
| `KONGMING_MAX_ITERATIONS` | tool-call rounds per step block of a turn | `24` |
| `KONGMING_AUTO_CONTINUE` | extra step blocks a long turn runs on its own before asking you to continue (older tool output is shortened; a turn repeating the same calls stops early; `0` = always ask) | `4` |
| `KONGMING_CONTEXT_TOKENS` | approximate request size (tokens) that triggers compaction | `48000` |
| `KONGMING_SHELL` | shell behind the `shell` tool: a path, or `off`; empty uses the bundled BusyBox when it is there (the Docker image) | empty |
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

**Source-available: free for personal use, paid for commercial use.**

- kongming is under the [PolyForm Noncommercial License 1.0.0](LICENSE-POLYFORM-NONCOMMERCIAL.md), Copyright (c) 2026-present Sudono Tanjung. You can run, read and modify it for personal and other noncommercial use at no cost.
- **Commercial use** (in or for a business, as a hosted service, or in a commercial product) needs a paid commercial license: contact [Sudono Tanjung on LinkedIn](https://www.linkedin.com/in/sudtanj/) or [@sudtanj](https://github.com/sudtanj).
- Versions published before the license change were MIT and remain available under it. See [LICENSE](LICENSE).
