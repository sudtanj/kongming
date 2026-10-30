//! The `setup` tool: the lead agent configures kongming from what the user
//! pastes or says (model provider keys, the Telegram bot, MCP servers,
//! earnings keys, skills). It goes through the same functions as the
//! dashboard, so validation is identical. Keys come in as `[secret:…]`
//! references to the vault (see `paste`) and never go back to the model.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use super::paste;
use super::tools::ToolCtx;
use crate::db::Agent;
use crate::llm::{ChatMessage, ChatRequest, ToolSpec};
use crate::state::{Event, Shared};
use crate::web::api;

const SKILL_FETCH_LIMIT: usize = 200_000;

/// Only the lead agent sets things up, and only while talking to the user.
pub fn offered(agent: &Agent, channel: &str) -> bool {
    agent.is_default
        && matches!(channel, "web" | "telegram")
        && agent.tools.as_ref().is_none_or(|t| t.iter().any(|n| n == "setup"))
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "setup".into(),
        description: "Configure kongming from what the user pastes or asks for. Keys and tokens the user pasted appear as \
`[secret:<id> <kind> …1234]`: pass that reference (or `secret:<id>`) wherever a key or token goes; the value is filled in \
when saving and never shown to you. show: the current configuration (keys only as set or not). llm: the model provider: \
kind `openai` for OpenAI and any OpenAI-compatible API (with base_url) or `anthropic`, plus model and api_key; it is tested \
before it is saved. telegram: the bot token and allowed_users (added to the allowed list). public_url: the address \
kongming is reached at. mcp_add: a remote MCP server from url (with name, transport, token, headers) or from a pasted MCP \
config JSON in `config`; a server with the same name is replaced. earn: a paid-work provider's api_key (with provider), \
and the earnings options. skill_import: a SKILL.md from a url or pasted as markdown."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["show", "llm", "telegram", "public_url", "mcp_add", "earn", "skill_import"]},
                "kind": {"type": "string", "enum": ["openai", "anthropic"], "description": "llm: the API flavour"},
                "base_url": {"type": "string", "description": "llm: API base URL, e.g. https://openrouter.ai/api/v1"},
                "model": {"type": "string", "description": "llm: model id"},
                "api_key": {"type": "string", "description": "llm, earn: the key (a secret reference)"},
                "token": {"type": "string", "description": "telegram: bot token; mcp_add: bearer token (a secret reference)"},
                "allowed_users": {"type": "string", "description": "telegram: Telegram user ids, comma separated"},
                "url": {"type": "string", "description": "public_url, mcp_add, skill_import: the address"},
                "name": {"type": "string", "description": "mcp_add: short server name"},
                "transport": {"type": "string", "enum": ["http", "sse"], "description": "mcp_add: default http (sse for urls ending in /sse)"},
                "headers": {"type": "object", "description": "mcp_add: extra HTTP headers; values may hold secret references"},
                "config": {"type": "string", "description": "mcp_add: the pasted MCP config JSON, e.g. {\"mcpServers\": {...}}"},
                "provider": {"type": "string", "description": "earn: provider id (toku, dealwork, opentask, moltjobs, hyrve, clawearn, opire, algora)"},
                "enabled": {"type": "boolean", "description": "earn: switch earnings on or off, or with provider that provider"},
                "auto": {"type": "boolean", "description": "earn: bid on small marketplace jobs without asking"},
                "auto_max_usd": {"type": "number", "description": "earn: the auto mode limit"},
                "min_usd": {"type": "number", "description": "earn: ignore work paying less"},
                "scan_minutes": {"type": "integer", "description": "earn: minutes between scans (0 = on demand)"},
                "profile": {"type": "string", "description": "earn: what work the user wants to take on"},
                "markdown": {"type": "string", "description": "skill_import: the SKILL.md text"}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
    }
}

fn opt<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

/// The secret-bearing arguments as given, to scrub from the output when the
/// model passed a raw value.
fn raw_secrets(args: &Value) -> Vec<String> {
    ["api_key", "token"]
        .iter()
        .filter_map(|k| opt(args, k))
        .filter(|v| !v.contains("secret:"))
        .map(str::to_string)
        .collect()
}

pub async fn run(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    if !offered(ctx.agent, &ctx.session.channel) {
        bail!("setup is only for the lead agent, in a conversation with the user");
    }
    let state = ctx.state;
    let op = opt(args, "op").unwrap_or("");
    let result = match op {
        "show" => show(state).await,
        "llm" => llm(state, args).await,
        "telegram" => telegram(state, args).await,
        "public_url" => public_url(state, args).await,
        "mcp_add" => mcp_add(state, args).await,
        "earn" => earn(state, args).await,
        "skill_import" => skill_import(state, args).await,
        other => Err(anyhow!("unknown op '{other}' (show|llm|telegram|public_url|mcp_add|earn|skill_import)")),
    };
    let also = raw_secrets(args);
    match result {
        Ok(text) => {
            let text = paste::scrub(state, &text, &also).await;
            if op != "show" {
                let _ = state.db.log_improvement(Some(&ctx.agent.id), Some(&ctx.session.id), "setup", None, &text).await;
                state.emit(Event::Improvement {
                    kind: "setup".into(),
                    note: text.clone(),
                    agent_id: Some(ctx.agent.id.clone()),
                });
            }
            Ok(text)
        }
        Err(e) => Err(anyhow!("{}", paste::scrub(state, &format!("{e:#}"), &also).await)),
    }
}

async fn secret(state: &Shared, args: &Value, k: &str) -> Result<Option<String>> {
    match opt(args, k) {
        Some(v) => Ok(Some(paste::resolve(state, v).await?.trim().to_string())),
        None => Ok(None),
    }
}

fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

// ----------------------------------------------------------------------- show

async fn show(state: &Shared) -> Result<String> {
    let llm = state.provider_config().await?.redacted();
    let mcp: Vec<Value> = state
        .db
        .mcp_servers()
        .await?
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "host": host(&m.url),
                "transport": m.transport,
                "auth": m.auth_type,
                "status": m.status,
                "tools": m.tools.as_array().map_or(0, |a| a.len()),
                "headers": m.headers.as_object().map(|h| h.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                "enabled": m.enabled,
            })
        })
        .collect();
    let earn = crate::earn::settings(state).await.to_json(true);
    let tg_status = state.telegram_status.lock().clone();
    let out = json!({
        "llm": llm,
        "telegram": {
            "tokenSet": state.telegram_token().await.is_some(),
            "allowedUsers": state.telegram_allowed().await,
            "status": tg_status,
        },
        "publicUrl": state.public_url().await,
        "mcpServers": mcp,
        "earn": earn,
        "proactive": crate::proactive::settings(state).await.to_json(),
        "skills": state.db.skills().await?.iter().map(|k| k.name.clone()).collect::<Vec<_>>(),
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

// ------------------------------------------------------------------------ llm

async fn llm(state: &Shared, args: &Value) -> Result<String> {
    let current = state.provider_config().await?;
    let key = secret(state, args, "api_key").await?;
    let key_kind = match opt(args, "api_key") {
        Some(k) => paste::kind_of(state, k).await,
        None => None,
    };
    let mut kind = opt(args, "kind").map(str::to_string);
    let mut base = opt(args, "base_url").map(str::to_string);
    // What the key's shape says, unless the model said otherwise.
    match key_kind.as_deref() {
        Some("anthropic") if kind.is_none() => kind = Some("anthropic".into()),
        Some("openrouter") if base.is_none() => {
            kind.get_or_insert_with(|| "openai".into());
            base = Some("https://openrouter.ai/api/v1".into());
        }
        Some("google") if base.is_none() => {
            kind.get_or_insert_with(|| "openai".into());
            base = Some("https://generativelanguage.googleapis.com/v1beta/openai".into());
        }
        _ => {}
    }
    let mut body = Map::new();
    let switching = kind.as_deref().is_some_and(|k| k != current.kind);
    if let Some(k) = &kind {
        body.insert("kind".into(), json!(k));
    }
    match base {
        Some(b) => {
            body.insert("baseUrl".into(), json!(b.trim_end_matches('/')));
        }
        // Another provider's address would not work for this one.
        None if switching => {
            body.insert("baseUrl".into(), json!(""));
        }
        None => {}
    }
    match opt(args, "model") {
        Some(m) => {
            body.insert("model".into(), json!(m));
        }
        None if switching => {
            body.insert("model".into(), json!(crate::state::default_model(kind.as_deref().unwrap_or("openai"))));
        }
        None => {}
    }
    if let Some(k) = &key {
        body.insert("apiKey".into(), json!(k));
    }
    if body.is_empty() {
        bail!("nothing to change: give api_key, kind, base_url or model");
    }
    let llm = Value::Object(body);
    let cfg = api::merge_llm(state, &llm).await?;
    // Test first: a broken model setting would cut the user off from you.
    let provider = crate::llm::build(&cfg, state.http.clone())?;
    let req = ChatRequest {
        model: cfg.model.clone(),
        system: "Reply with the single word: pong".into(),
        messages: vec![ChatMessage::text("user", "ping")],
        tools: vec![],
        max_tokens: 20,
        temperature: Some(0.0),
        effort: Some("low".into()),
    };
    let ping = async { super::collect_text(provider.stream(req).await?).await };
    match tokio::time::timeout(Duration::from_secs(60), ping).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => bail!("the new model settings did not work, so nothing was changed: {e:#}"),
        Err(_) => bail!("the new model settings did not answer within a minute, so nothing was changed"),
    }
    api::apply_settings(state, &json!({ "llm": llm })).await?;
    Ok(format!(
        "Model provider saved and tested: {} {}, model {}{}.",
        cfg.kind,
        cfg.base_url.as_deref().map(host).unwrap_or_else(|| "(default address)".into()),
        cfg.model,
        if key.is_some() { ", new API key" } else { "" }
    ))
}

// ------------------------------------------------------------------- telegram

async fn telegram(state: &Shared, args: &Value) -> Result<String> {
    let mut tg = Map::new();
    let token = secret(state, args, "token").await?;
    if let Some(t) = &token {
        tg.insert("token".into(), json!(t));
    }
    if let Some(u) = opt(args, "allowed_users") {
        // Added to the list, so nobody is locked out by a partial answer.
        let mut ids = state.telegram_allowed().await;
        for id in u.split([',', ' ', '\n', ';']).filter_map(|x| x.trim().parse::<i64>().ok()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        tg.insert("allowedUsers".into(), json!(ids));
    }
    if tg.is_empty() {
        bail!("give token and/or allowed_users");
    }
    api::apply_settings(state, &json!({ "telegram": tg })).await?;
    let allowed = state.telegram_allowed().await;
    let mut out = String::from("Telegram: ");
    if token.is_some() {
        out.push_str("bot token accepted and saved. ");
    }
    if allowed.is_empty() {
        out.push_str("No one is allowed to talk to the bot yet: the bot tells a new user their id, which goes in allowed_users.");
    } else {
        out.push_str(&format!(
            "allowed users: {}.",
            allowed.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(out)
}

async fn public_url(state: &Shared, args: &Value) -> Result<String> {
    let u = opt(args, "url").ok_or_else(|| anyhow!("url is required"))?;
    let parsed = url::Url::parse(u).map_err(|_| anyhow!("url must be a full address, e.g. https://kongming.example.com"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("url must start with http:// or https://");
    }
    api::apply_settings(state, &json!({ "publicUrl": u })).await?;
    Ok(format!("Public address set to {}.", u.trim_end_matches('/')))
}

// ------------------------------------------------------------------------ mcp

/// A short name for a server from its address: mcp.notion.com → notion.
fn name_from_url(url: &str) -> String {
    let h = host(url);
    let h = h.trim_start_matches("www.").trim_start_matches("mcp.").trim_start_matches("api.");
    h.split('.').next().filter(|s| !s.is_empty()).unwrap_or("mcp").to_string()
}

fn transport_for(kind: Option<&str>, url: &str) -> &'static str {
    match kind.map(str::to_ascii_lowercase).as_deref() {
        Some("sse") => "sse",
        Some(_) => "http",
        None if url.trim_end_matches('/').ends_with("/sse") => "sse",
        None => "http",
    }
}

/// A dashboard-style server body from one entry of a pasted config, or why
/// it cannot be used.
async fn entry_body(state: &Shared, name: &str, e: &Value) -> Result<Value> {
    let url = ["url", "serverUrl", "httpUrl", "endpoint"].iter().find_map(|k| e.get(*k).and_then(Value::as_str));
    let Some(url) = url else {
        if e.get("command").is_some() {
            bail!(
                "{name}: only remote (http/https) MCP servers are supported; local stdio servers (`command`) are not allowed"
            );
        }
        bail!("{name}: no url in this entry");
    };
    let url = paste::resolve(state, url).await?;
    let kind = e.get("type").or(e.get("transport")).and_then(Value::as_str);
    let mut body = json!({"name": name, "url": url, "transport": transport_for(kind, &url)});
    let mut headers = Map::new();
    if let Some(h) = e.get("headers").and_then(Value::as_object) {
        for (k, v) in h {
            let v = paste::resolve(state, v.as_str().unwrap_or("")).await?;
            // A bearer token is kept as the server's token, not as a header.
            match v.strip_prefix("Bearer ").filter(|_| k.eq_ignore_ascii_case("authorization")) {
                Some(t) => {
                    body["authType"] = json!("token");
                    body["token"] = json!(t.trim());
                }
                None => {
                    headers.insert(k.clone(), json!(v));
                }
            }
        }
    }
    if !headers.is_empty() {
        body["headers"] = Value::Object(headers);
    }
    Ok(body)
}

/// Strip a ```json fence around a pasted config.
fn unfence(s: &str) -> &str {
    let t = s.trim();
    match t.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.trim_end().trim_end_matches("```")
        }
        None => t,
    }
}

async fn mcp_bodies(state: &Shared, args: &Value) -> Result<Vec<Result<Value>>> {
    if let Some(c) = opt(args, "config") {
        let v: Value = serde_json::from_str(unfence(c))
            .map_err(|e| anyhow!("config is not valid JSON ({e}); pass the pasted text as it is"))?;
        let servers = v.get("mcpServers").or(v.get("servers")).or(v.pointer("/mcp/servers")).and_then(Value::as_object);
        let mut out = Vec::new();
        match servers {
            Some(map) => {
                for (name, e) in map {
                    out.push(entry_body(state, name, e).await);
                }
            }
            None => {
                let url = v.get("url").or(v.get("serverUrl")).and_then(Value::as_str).unwrap_or("");
                let name = opt(args, "name")
                    .map(str::to_string)
                    .or_else(|| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| name_from_url(url));
                out.push(entry_body(state, &name, &v).await);
            }
        }
        if out.is_empty() {
            bail!("no servers in this config");
        }
        return Ok(out);
    }
    let url = paste::resolve(state, opt(args, "url").ok_or_else(|| anyhow!("give url (or the pasted config)"))?).await?;
    let name = opt(args, "name").map(str::to_string).unwrap_or_else(|| name_from_url(&url));
    let mut body = json!({"name": name, "url": url, "transport": transport_for(opt(args, "transport"), &url)});
    if let Some(t) = secret(state, args, "token").await? {
        body["authType"] = json!("token");
        body["token"] = json!(t.trim_start_matches("Bearer ").trim());
    }
    if let Some(h) = args.get("headers").and_then(Value::as_object) {
        let mut headers = Map::new();
        for (k, v) in h {
            headers.insert(k.clone(), json!(paste::resolve(state, v.as_str().unwrap_or("")).await?));
        }
        body["headers"] = Value::Object(headers);
    }
    Ok(vec![Ok(body)])
}

async fn mcp_add(state: &Shared, args: &Value) -> Result<String> {
    let mut lines = Vec::new();
    for body in mcp_bodies(state, args).await? {
        let body = match body {
            Ok(b) => b,
            Err(e) => {
                lines.push(format!("- skipped {e:#}"));
                continue;
            }
        };
        let name = body["name"].as_str().unwrap_or("").to_string();
        let existing = state.db.mcp_servers().await?.into_iter().find(|m| m.name == name);
        let replaced = existing.is_some();
        let saved = match existing {
            Some(m) => api::change_mcp(state, m, &body).await,
            None => api::add_mcp(state, &body).await,
        };
        let m = match saved {
            Ok(m) => m,
            Err(e) => {
                lines.push(format!("- {name}: not saved: {e}"));
                continue;
            }
        };
        let verb = if replaced { "updated" } else { "added" };
        match tokio::time::timeout(Duration::from_secs(45), state.mcp.refresh(&m.id)).await {
            Ok(Ok(tools)) => {
                let names: Vec<&str> = tools.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).take(12).collect();
                lines.push(format!("- {name} ({}) {verb} and connected: {} tools ({})", host(&m.url), tools.len(), names.join(", ")));
            }
            Ok(Err(e)) if format!("{e:#}").contains("authorized") && m.token.is_none() => {
                lines.push(sign_in(state, &m, verb).await);
            }
            Ok(Err(e)) => lines.push(format!("- {name} {verb}, but connecting failed: {e:#}")),
            Err(_) => lines.push(format!("- {name} {verb}; it is still connecting (check it with show)")),
        }
    }
    Ok(lines.join("\n"))
}

/// A server that wants OAuth: a sign-in link when kongming has a public
/// address, otherwise the way through the dashboard.
async fn sign_in(state: &Shared, m: &crate::db::McpServer, verb: &str) -> String {
    let (redirect, manual) = api::redirect_uri(state).await;
    if !manual {
        let oauth = json!({"authType": "oauth"});
        if let Ok(m) = api::change_mcp(state, m.clone(), &oauth).await {
            if let Ok(url) = state.mcp.start_oauth(&m.id, &redirect).await {
                return format!(
                    "- {} {verb}; it needs the user to sign in: send them this link: {url}",
                    m.name
                );
            }
        }
    }
    format!(
        "- {} {verb}; it needs the user to sign in (or a token): Settings → MCP → {} → Sign in",
        m.name, m.name
    )
}

// ----------------------------------------------------------------------- earn

async fn earn(state: &Shared, args: &Value) -> Result<String> {
    let mut body = Map::new();
    let enabled = args.get("enabled").and_then(Value::as_bool);
    let mut provider_name = None;
    if let Some(p) = opt(args, "provider") {
        let prov = crate::earn::provider(&p.to_ascii_lowercase()).ok_or_else(|| {
            anyhow!(
                "unknown provider {p}; one of: {}",
                crate::earn::PROVIDERS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
            )
        })?;
        let mut pv = Map::new();
        if let Some(k) = secret(state, args, "api_key").await? {
            pv.insert("apiKey".into(), json!(k));
            pv.insert("enabled".into(), json!(true));
        }
        if let Some(e) = enabled {
            pv.insert("enabled".into(), json!(e));
        }
        let mut providers = Map::new();
        providers.insert(prov.id.to_string(), Value::Object(pv));
        body.insert("providers".into(), Value::Object(providers));
        provider_name = Some(prov);
    } else if let Some(e) = enabled {
        body.insert("enabled".into(), json!(e));
    } else if opt(args, "api_key").is_some() {
        bail!("say which provider the key is for (provider)");
    }
    for (arg, field) in
        [("auto", "auto"), ("auto_max_usd", "autoMaxUsd"), ("min_usd", "minUsd"), ("scan_minutes", "scanMinutes"), ("profile", "profile")]
    {
        if let Some(v) = args.get(arg) {
            body.insert(field.into(), v.clone());
        }
    }
    if body.is_empty() {
        bail!("nothing to change");
    }
    api::save_earn(state, &Value::Object(body)).await?;
    let s = crate::earn::settings(state).await;
    let mut out = String::from("Earnings settings saved. ");
    if let Some(p) = provider_name {
        let on = s.to_json(true)["providers"][p.id].clone();
        out.push_str(&format!(
            "{}: key {}, {}. ",
            p.name,
            if on["keySet"] == json!(true) { "set" } else { "not set" },
            if on["enabled"] == json!(true) { "on" } else { "off" }
        ));
    }
    out.push_str(if s.enabled {
        "Earnings is on."
    } else {
        "Earnings itself is off: the user turns it on with /earn on (or ask them whether to)."
    });
    Ok(out)
}

// --------------------------------------------------------------------- skills

/// The raw file behind a GitHub page: blob → raw, a folder → its SKILL.md.
fn raw_url(u: &str) -> String {
    let Some(rest) = u.strip_prefix("https://github.com/") else { return u.to_string() };
    let parts: Vec<&str> = rest.splitn(5, '/').collect();
    match parts.as_slice() {
        [owner, repo, "blob", branch, path] => {
            format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{path}")
        }
        [owner, repo, "tree", branch, path] => {
            format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}/SKILL.md", path.trim_end_matches('/'))
        }
        _ => u.to_string(),
    }
}

async fn fetch_skill(state: &Shared, u: &str) -> Result<String> {
    let url = raw_url(u);
    let parsed = url::Url::parse(&url).map_err(|_| anyhow!("invalid url"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("only http(s) urls are allowed");
    }
    if let Some(h) = parsed.host_str() {
        if h == "localhost"
            || h.ends_with(".local")
            || h.parse::<std::net::IpAddr>().is_ok_and(|ip| !super::tools::is_public_ip(&ip))
        {
            bail!("refusing to fetch a private address");
        }
    }
    let resp = state.http.get(parsed).timeout(Duration::from_secs(30)).send().await?;
    let status = resp.status();
    if !status.is_success() {
        bail!("fetching {url} gave HTTP {status}");
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > SKILL_FETCH_LIMIT {
        bail!("that file is too large for a skill");
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

async fn skill_import(state: &Shared, args: &Value) -> Result<String> {
    let md = match (opt(args, "markdown"), opt(args, "url")) {
        (Some(m), _) => m.to_string(),
        (None, Some(u)) => fetch_skill(state, u).await?,
        (None, None) => bail!("give markdown or url"),
    };
    let k = api::import_skill_md(state, &md).await.map_err(|e| {
        anyhow!("{e}; a text without that frontmatter can still be saved with skill_manage (name, description, body)")
    })?;
    Ok(format!("Skill `{}` imported (v{}): {}", k.name, k.version, k.description))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_transports_come_from_the_url() {
        assert_eq!(name_from_url("https://mcp.notion.com/mcp"), "notion");
        assert_eq!(name_from_url("https://api.example.org/v1/sse"), "example");
        assert_eq!(transport_for(None, "https://x.dev/sse"), "sse");
        assert_eq!(transport_for(Some("streamable-http"), "https://x.dev/sse"), "http");
        assert_eq!(transport_for(None, "https://x.dev/mcp"), "http");
    }

    #[test]
    fn github_pages_become_raw_files() {
        assert_eq!(
            raw_url("https://github.com/a/b/blob/main/skills/x/SKILL.md"),
            "https://raw.githubusercontent.com/a/b/main/skills/x/SKILL.md"
        );
        assert_eq!(
            raw_url("https://github.com/a/b/tree/main/skills/x/"),
            "https://raw.githubusercontent.com/a/b/main/skills/x/SKILL.md"
        );
        assert_eq!(raw_url("https://example.com/SKILL.md"), "https://example.com/SKILL.md");
    }

    #[test]
    fn fenced_configs_are_unwrapped() {
        assert_eq!(unfence("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(unfence(" {\"a\": 1} "), "{\"a\": 1}");
    }
}
