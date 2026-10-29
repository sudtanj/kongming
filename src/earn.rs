//! Earnings: Kongming looks for paid work for the user on agent marketplaces
//! and code-bounty boards, rates it, tells the user about the best fits on
//! Telegram, and with their go-ahead bids, does the work and delivers.
//!
//! A scan lists open work on every enabled provider (public listings; no key
//! needed), keeps the leads that pay at least `minUsd` and are not other agents
//! advertising themselves, and stores them (`earn_leads`, each lead once). The
//! lead agent then rates the new leads in an `earn` conversation, sends one
//! digest of the shortlist with `notify_user`, and follows up on work in
//! progress. The user answers with "take <ref>" / "skip <ref>" (Telegram or the
//! dashboard).
//!
//! The `earn` tool reaches a provider's API with the key saved in settings; the
//! key never enters the model's context. Reading is always allowed. Writing
//! (bid, deliver) needs a lead the user approved, or `auto` for marketplace
//! leads up to `autoMaxUsd`. Withdrawals, sign-ups and key management are never
//! done by the agent; payouts stay with the user.
//!
//! Settings (key `earn`): `enabled`, `scanMinutes` (0 = only on demand),
//! `minUsd`, `profile` (what to look for), `auto`, `autoMaxUsd`, `maxAlerts`,
//! `providers.<id>.{enabled, apiKey, baseUrl}`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::agent::tools::ToolCtx;
use crate::agent::{self, TurnOptions};
use crate::db::{now_ms, EarnLead};
use crate::llm::ToolSpec;
use crate::state::Shared;

const KEY: &str = "earn";
const STATUS_KEY: &str = "earn.status";
const LAST_SCAN: &str = "earn.lastScan";
/// Leads rated per scan; the rest wait for the next one.
const RATE_PER_SCAN: i64 = 15;
/// Biggest listing response read.
const MAX_BODY: usize = 4_000_000;
/// New Algora issues verified per scan (one GitHub call each; 60 an hour without a token).
const ALGORA_CHECKS: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Clients post work for agents; Kongming can bid and deliver through the API.
    Market,
    /// Open-source issues with money on them; claimed with a pull request from the user's GitHub account.
    Bounty,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Auth {
    Bearer,
    Header(&'static str),
}

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    /// API base; paths in `earn` requests are relative to it.
    pub base: &'static str,
    pub site: &'static str,
    /// What the agent reads before acting on this provider.
    pub docs: &'static str,
    /// Where the user gets an API key (shown in the dashboard).
    pub key_help: &'static str,
    pub payout: &'static str,
    pub auth: Auth,
    /// Kongming may bid and deliver through the API (with a key).
    pub acts: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "toku",
        name: "toku.agency",
        kind: Kind::Market,
        base: "https://toku.agency",
        site: "https://www.toku.agency",
        docs: "https://toku.agency/skill.md",
        key_help: "Create an agent at toku.agency/dashboard/agents/new and copy its API key. Payouts need Stripe Connect.",
        payout: "USD via Stripe, 85% to you",
        auth: Auth::Bearer,
        acts: true,
    },
    Provider {
        id: "dealwork",
        name: "dealwork.ai",
        kind: Kind::Market,
        base: "https://dealwork.ai/api/v1",
        site: "https://dealwork.ai",
        docs: "https://dealwork.ai/skill.md",
        key_help: "Connect an agent on dealwork.ai (owner link) and copy its ak_… key.",
        payout: "USD wallet, withdraw via Stripe or crypto",
        auth: Auth::Bearer,
        acts: true,
    },
    Provider {
        id: "opentask",
        name: "OpenTask",
        kind: Kind::Market,
        base: "https://opentask.ai/api",
        site: "https://opentask.ai",
        docs: "https://opentask.ai/skill.md",
        key_help: "Create a token in OpenTask → Developer Settings. Buyers pay your crypto wallet directly.",
        payout: "Crypto to your wallet (mostly USDC on Base)",
        auth: Auth::Bearer,
        acts: true,
    },
    Provider {
        id: "moltjobs",
        name: "MoltJobs",
        kind: Kind::Market,
        base: "https://api.moltjobs.io/v1",
        site: "https://moltjobs.io",
        docs: "https://moltjobs.io/skill.md",
        key_help: "Claim the agent at app.moltjobs.io and issue an mj_live_… key.",
        payout: "USDC on Base, 5% fee",
        auth: Auth::Bearer,
        acts: true,
    },
    Provider {
        id: "hyrve",
        name: "HYRVE AI",
        kind: Kind::Market,
        base: "https://api.hyrveai.com/v1",
        site: "https://hyrveai.com",
        docs: "https://hyrveai.com/skill.md",
        key_help: "Register an agent at app.hyrveai.com and copy its hyrve_ak_… key.",
        payout: "USD via Stripe or USDT, 85% to you",
        auth: Auth::Header("X-API-Key"),
        acts: true,
    },
    Provider {
        id: "clawearn",
        name: "Claw Earn (AI Agent Store)",
        kind: Kind::Market,
        base: "https://aiagentstore.ai",
        site: "https://aiagentstore.ai/claw-earn",
        docs: "https://aiagentstore.ai/llms.txt",
        key_help: "Taking a task needs a crypto wallet that stakes USDC on Base; Kongming only lists these, you take them yourself.",
        payout: "USDC on Base, needs a staked wallet",
        auth: Auth::Bearer,
        acts: false,
    },
    Provider {
        id: "opire",
        name: "Opire",
        kind: Kind::Bounty,
        base: "https://api.opire.dev",
        site: "https://opire.dev",
        docs: "https://opire.dev",
        key_help: "No key: claim by commenting /try on the issue and opening a pull request from your GitHub account.",
        payout: "USD via Stripe Connect",
        auth: Auth::Bearer,
        acts: false,
    },
    Provider {
        id: "algora",
        name: "Algora",
        kind: Kind::Bounty,
        base: "https://api.github.com",
        site: "https://algora.io/bounties",
        docs: "https://algora.io/docs",
        key_help: "Optional GitHub token (no scopes) for a higher search limit. Claim with /attempt and a pull request from your GitHub account.",
        payout: "USD via Stripe Connect (identity check)",
        auth: Auth::Bearer,
        acts: false,
    },
];

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

// -------------------------------------------------------------------- settings

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderSettings {
    pub enabled: bool,
    pub api_key: Option<String>,
    /// Another API base (tests, mirrors).
    pub base_url: Option<String>,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        ProviderSettings { enabled: true, api_key: None, base_url: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    /// Minutes between scans; 0 = only when asked.
    pub scan_minutes: i64,
    pub min_usd: f64,
    /// What the user wants to take on: skills, kinds of work, things to avoid.
    pub profile: String,
    /// Bid and deliver on marketplace leads up to `auto_max_usd` without asking.
    pub auto: bool,
    pub auto_max_usd: f64,
    /// Leads in one digest.
    pub max_alerts: i64,
    pub providers: BTreeMap<String, ProviderSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            scan_minutes: 120,
            min_usd: 5.0,
            profile: String::new(),
            auto: false,
            auto_max_usd: 25.0,
            max_alerts: 3,
            providers: PROVIDERS.iter().map(|p| (p.id.to_string(), ProviderSettings::default())).collect(),
        }
    }
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn opt_text(v: &Value, k: &str) -> Option<String> {
    Some(text(v, k)).filter(|s| !s.is_empty())
}

impl Settings {
    pub fn from_value(v: Option<&Value>) -> Settings {
        let mut s = Settings::default();
        let Some(v) = v else { return s };
        s.enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(s.enabled);
        s.scan_minutes = v.get("scanMinutes").and_then(Value::as_i64).unwrap_or(s.scan_minutes).max(0);
        s.min_usd = num(v.get("minUsd")).unwrap_or(s.min_usd).max(0.0);
        s.profile = text(v, "profile");
        s.auto = v.get("auto").and_then(Value::as_bool).unwrap_or(false);
        s.auto_max_usd = num(v.get("autoMaxUsd")).unwrap_or(s.auto_max_usd).max(0.0);
        s.max_alerts = v.get("maxAlerts").and_then(Value::as_i64).unwrap_or(s.max_alerts).max(0);
        if let Some(ps) = v.get("providers").and_then(Value::as_object) {
            for (id, p) in ps {
                let Some(cur) = s.providers.get_mut(id) else { continue };
                cur.enabled = p.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                cur.api_key = opt_text(p, "apiKey");
                cur.base_url = opt_text(p, "baseUrl");
            }
        }
        s
    }

    /// As stored; `redact` (for the dashboard) replaces keys with `keySet`.
    pub fn to_json(&self, redact: bool) -> Value {
        let providers: serde_json::Map<String, Value> = self
            .providers
            .iter()
            .map(|(id, p)| {
                let mut o = json!({"enabled": p.enabled});
                if redact {
                    o["keySet"] = json!(p.api_key.is_some());
                } else if let Some(k) = &p.api_key {
                    o["apiKey"] = json!(k);
                }
                if let Some(b) = &p.base_url {
                    o["baseUrl"] = json!(b);
                }
                (id.clone(), o)
            })
            .collect();
        json!({
            "enabled": self.enabled,
            "scanMinutes": self.scan_minutes,
            "minUsd": self.min_usd,
            "profile": self.profile,
            "auto": self.auto,
            "autoMaxUsd": self.auto_max_usd,
            "maxAlerts": self.max_alerts,
            "providers": providers,
        })
    }

    /// Apply a change from the dashboard; errors name the bad field.
    pub fn merged(&self, body: &Value) -> Result<Settings, String> {
        let mut s = self.clone();
        let int = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|x| x.trim().parse().ok()));
        if let Some(v) = body.get("enabled") {
            s.enabled = v.as_bool().ok_or("enabled must be true or false")?;
        }
        if let Some(v) = body.get("scanMinutes") {
            s.scan_minutes = int(v)
                .filter(|n| *n == 0 || (30..=10_080).contains(n))
                .ok_or("scanMinutes must be 0 (only on demand) or 30 to 10080")?;
        }
        if let Some(v) = body.get("minUsd") {
            s.min_usd = num(Some(v)).filter(|n| (0.0..=100_000.0).contains(n)).ok_or("minUsd must be 0 to 100000")?;
        }
        if let Some(v) = body.get("profile") {
            let p = v.as_str().ok_or("profile must be text")?.trim();
            if p.chars().count() > 2000 {
                return Err("profile is too long (max 2000 characters)".into());
            }
            s.profile = p.to_string();
        }
        if let Some(v) = body.get("auto") {
            s.auto = v.as_bool().ok_or("auto must be true or false")?;
        }
        if let Some(v) = body.get("autoMaxUsd") {
            s.auto_max_usd =
                num(Some(v)).filter(|n| (0.0..=1000.0).contains(n)).ok_or("autoMaxUsd must be 0 to 1000")?;
        }
        if let Some(v) = body.get("maxAlerts") {
            s.max_alerts = int(v).filter(|n| (0..=20).contains(n)).ok_or("maxAlerts must be 0 to 20")?;
        }
        if let Some(ps) = body.get("providers") {
            let ps = ps.as_object().ok_or("providers must be an object")?;
            for (id, p) in ps {
                let cur = s.providers.get_mut(id).ok_or_else(|| format!("unknown provider {id}"))?;
                if let Some(v) = p.get("enabled") {
                    cur.enabled = v.as_bool().ok_or_else(|| format!("providers.{id}.enabled must be true or false"))?;
                }
                if let Some(k) = p.get("apiKey").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()) {
                    if k.len() > 500 || k.chars().any(char::is_whitespace) {
                        return Err(format!("providers.{id}.apiKey does not look like a key"));
                    }
                    cur.api_key = Some(k.to_string());
                }
                if p.get("clearApiKey").and_then(Value::as_bool) == Some(true) {
                    cur.api_key = None;
                }
                if let Some(b) = p.get("baseUrl") {
                    let b = b.as_str().unwrap_or("").trim().trim_end_matches('/');
                    if !b.is_empty() && url::Url::parse(b).is_err() {
                        return Err(format!("providers.{id}.baseUrl is not a URL"));
                    }
                    cur.base_url = Some(b.to_string()).filter(|b| !b.is_empty());
                }
            }
        }
        Ok(s)
    }

    fn base(&self, p: &Provider) -> String {
        self.providers.get(p.id).and_then(|x| x.base_url.clone()).unwrap_or_else(|| p.base.to_string())
    }

    fn key(&self, p: &Provider) -> Option<&str> {
        self.providers.get(p.id).and_then(|x| x.api_key.as_deref())
    }

    fn on(&self, p: &Provider) -> bool {
        self.providers.get(p.id).is_some_and(|x| x.enabled)
    }
}

pub async fn settings(state: &Shared) -> Settings {
    Settings::from_value(state.db.setting(KEY).await.ok().flatten().as_ref())
}

pub async fn save(state: &Shared, s: &Settings) -> Result<()> {
    state.db.set_setting(KEY, &s.to_json(false)).await
}

pub async fn set_enabled(state: &Shared, enabled: bool) -> Result<()> {
    save(state, &Settings { enabled, ..settings(state).await }).await
}

pub async fn last_scan(state: &Shared) -> Option<i64> {
    state.db.setting(LAST_SCAN).await.ok().flatten().and_then(|v| v.as_i64())
}

// -------------------------------------------------------------------- listings

/// One open piece of work as a provider lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub external_id: String,
    pub title: String,
    pub description: String,
    pub budget_usd: Option<f64>,
    pub currency: String,
    pub url: String,
    pub bids: Option<i64>,
    pub deadline: Option<String>,
    pub tags: Vec<String>,
}

fn items<'a>(v: &'a Value, keys: &[&str]) -> &'a [Value] {
    if let Some(a) = v.as_array() {
        return a;
    }
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_array)).map(Vec::as_slice).unwrap_or(&[])
}

fn tags(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| match t {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(_) => opt_text(t, "name").or_else(|| opt_text(t, "slug")),
                    _ => None,
                })
                .take(12)
                .collect()
        })
        .unwrap_or_default()
}

/// Stablecoins count as dollars; other currencies have no USD value here.
fn usd(amount: Option<f64>, currency: &str) -> Option<f64> {
    match currency.to_ascii_uppercase().as_str() {
        "" | "USD" | "USDC" | "USDT" | "DAI" => amount,
        _ => None,
    }
}

/// The listing request for a provider.
pub fn list_url(id: &str, base: &str) -> String {
    match id {
        "toku" => format!("{base}/api/agents/jobs?status=OPEN&limit=100"),
        "dealwork" => format!("{base}/jobs?status=posted&sort=newest&per_page=50"),
        "opentask" => format!("{base}/tasks?sort=new&limit=50"),
        "moltjobs" => format!("{base}/jobs?status=OPEN&limit=50"),
        "hyrve" => format!("{base}/jobs?status=open&limit=50&sort=created_at&order=desc"),
        "clawearn" => format!("{base}/claw/tasks?limit=50&tab=available"),
        "opire" => format!("{base}/rewards"),
        // Open issues the Algora bot commented on, newest first. GitHub, not
        // algora.io: Algora's terms forbid automated access to its site.
        "algora" => format!(
            "{base}/search/issues?q={}&sort=created&order=desc&per_page=30",
            urlenc("is:issue is:open commenter:app/algora-pbc label:\"💎 Bounty\"")
        ),
        _ => base.to_string(),
    }
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Parse a provider's listing into leads. Entries without an id or title are dropped.
pub fn parse(id: &str, body: &Value) -> Vec<Found> {
    let list: Vec<Found> = match id {
        "toku" => items(body, &["jobPosts", "jobs", "data"])
            .iter()
            .map(|j| {
                let mut t = tags(j.get("tags"));
                t.extend(opt_text(j, "category"));
                Found {
                    external_id: text(j, "id"),
                    title: text(j, "title"),
                    description: text(j, "description"),
                    budget_usd: num(j.get("budgetCents")).map(|c| c / 100.0),
                    currency: "USD".into(),
                    url: format!("https://www.toku.agency/jobs/{}", text(j, "id")),
                    bids: j.get("bidCount").and_then(Value::as_i64),
                    deadline: opt_text(j, "deadline"),
                    tags: t,
                }
            })
            .collect(),
        "dealwork" => items(body, &["data", "jobs"])
            .iter()
            .map(|j| {
                let mut t = tags(j.get("tags"));
                t.extend(opt_text(j, "category"));
                Found {
                    external_id: text(j, "id"),
                    title: text(j, "title"),
                    description: text(j, "description"),
                    budget_usd: num(j.get("fixedPrice")).or(num(j.get("budgetMax"))).or(num(j.get("budgetMin"))),
                    currency: "USD".into(),
                    url: format!("https://dealwork.ai/api/v1/jobs/{}", text(j, "id")),
                    bids: j.get("bidCount").and_then(Value::as_i64),
                    deadline: opt_text(j, "deadline").or_else(|| opt_text(j, "biddingDeadline")),
                    tags: t,
                }
            })
            .collect(),
        "opentask" => items(body, &["tasks", "data"])
            .iter()
            .map(|j| {
                let currency = text(j, "budgetCurrency");
                let description = opt_text(j, "description").unwrap_or_else(|| {
                    [text(j, "budgetText"), text(j, "executionMode"), text(j, "deliveryStructure")]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ")
                });
                Found {
                    external_id: text(j, "id"),
                    title: text(j, "title"),
                    description,
                    budget_usd: usd(num(j.get("budgetAmount")), &currency),
                    currency: if currency.is_empty() { "USD?".into() } else { currency },
                    url: format!("https://opentask.ai/tasks/{}", text(j, "id")),
                    bids: j.get("bidCount").and_then(Value::as_i64),
                    deadline: opt_text(j, "deadline"),
                    tags: tags(j.get("skillsTags")),
                }
            })
            .collect(),
        "moltjobs" => items(body, &["data", "jobs"])
            .iter()
            .map(|j| {
                let description = j
                    .pointer("/inputData/generalDescription")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| text(j, "description"));
                Found {
                    external_id: text(j, "id"),
                    title: text(j, "title"),
                    description,
                    budget_usd: num(j.get("budgetUsdc")),
                    currency: "USDC".into(),
                    url: format!("https://api.moltjobs.io/v1/jobs/{}/public", text(j, "id")),
                    bids: j.get("bidCount").and_then(Value::as_i64),
                    deadline: opt_text(j, "deadlineAt"),
                    tags: tags(j.get("requiredSkills")),
                }
            })
            .collect(),
        "hyrve" => items(body, &["jobs", "data"])
            .iter()
            .map(|j| {
                let t: Vec<String> = opt_text(j, "category").into_iter().collect();
                Found {
                    external_id: text(j, "id"),
                    title: text(j, "title"),
                    description: text(j, "description"),
                    budget_usd: num(j.get("budget_usd")),
                    currency: "USD".into(),
                    url: format!("https://api.hyrveai.com/v1/jobs/{}", text(j, "id")),
                    bids: None,
                    deadline: opt_text(j, "deadline"),
                    tags: t,
                }
            })
            .collect(),
        "clawearn" => items(body, &["items", "tasks"])
            .iter()
            .map(|j| {
                let task = opt_text(j, "taskId").unwrap_or_else(|| text(j, "id"));
                let mut t = tags(j.get("tags"));
                t.extend(opt_text(j, "category"));
                Found {
                    external_id: text(j, "id"),
                    title: text(j, "title"),
                    description: text(j, "description"),
                    budget_usd: num(j.get("amountUsdc")),
                    currency: "USDC".into(),
                    url: format!("https://aiagentstore.ai/claw-earn/task/{task}"),
                    bids: None,
                    deadline: None,
                    tags: t,
                }
            })
            .collect(),
        "opire" => parse_opire(body),
        "algora" => parse_algora(body),
        _ => Vec::new(),
    };
    list.into_iter().filter(|f| !f.external_id.is_empty() && !f.title.is_empty()).collect()
}

/// Opire rewards: `pendingPrice {value, unit: USD_CENT}`, the issue in `url`.
fn parse_opire(body: &Value) -> Vec<Found> {
    items(body, &["rewards", "data", "items"])
        .iter()
        .map(|r| {
            let price = r.get("pendingPrice").or_else(|| r.get("price")).unwrap_or(&Value::Null);
            let unit = text(price, "unit").to_ascii_uppercase();
            let value = num(price.get("value"));
            let dollars = if unit.contains("CENT") { value.map(|v| v / 100.0) } else { value };
            let currency = unit.trim_end_matches("_CENTS").trim_end_matches("_CENT").to_string();
            let count = |k: &str| r.get(k).and_then(Value::as_array).map_or(0, |a| a.len() as i64);
            let mut t = tags(r.get("programmingLanguages"));
            if let Some(p) = r.get("project").and_then(|p| opt_text(p, "url")) {
                t.insert(0, p.trim_start_matches("https://github.com/").to_string());
            }
            Found {
                external_id: text(r, "id"),
                title: text(r, "title"),
                description: String::new(),
                budget_usd: usd(dollars, &currency),
                currency: "USD".into(),
                url: text(r, "url"),
                bids: Some(count("tryingUsers") + count("claimerUsers")),
                deadline: None,
                tags: t,
            }
        })
        .collect()
}

/// Algora bounties found through GitHub issue search. The amount comes from the
/// Algora bot's comment (see `algora_amount`); a label or title amount is a hint only.
fn parse_algora(body: &Value) -> Vec<Found> {
    items(body, &["items"])
        .iter()
        .map(|i| {
            let labels = tags(i.get("labels"));
            let amount = labels.iter().chain(std::iter::once(&text(i, "title"))).find_map(|l| dollar_amount(l));
            let repo = text(i, "repository_url").rsplit("/repos/").next().unwrap_or("").to_string();
            let mut t = labels.into_iter().filter(|l| !l.contains('$') && !l.contains("Bounty")).collect::<Vec<_>>();
            if !repo.is_empty() {
                t.insert(0, repo);
            }
            Found {
                external_id: text(i, "id"),
                title: text(i, "title"),
                description: text(i, "body"),
                budget_usd: amount,
                currency: "USD".into(),
                url: text(i, "html_url"),
                bids: i.get("comments").and_then(Value::as_i64),
                deadline: None,
                tags: t,
            }
        })
        .collect()
}

/// `$1,500` / `$50` in a label or title.
pub fn dollar_amount(s: &str) -> Option<f64> {
    let i = s.find('$')?;
    let digits: String = s[i + 1..].chars().take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.').collect();
    let n: f64 = digits.replace(',', "").trim_end_matches('.').parse().ok()?;
    let rest = s[i + 1 + digits.len()..].trim_start().to_ascii_lowercase();
    Some(if rest.starts_with('k') { n * 1000.0 } else { n })
}

/// Agents advertising their own services, which marketplaces list among open jobs.
pub fn is_ad(title: &str, description: &str) -> bool {
    let t: String = title.trim_start_matches(|c: char| !c.is_alphanumeric()).to_ascii_lowercase();
    const TITLE: &[&str] = &[
        "available",
        "for sale",
        "for hire",
        "hire me",
        "offering",
        "service:",
        "services:",
        "i will ",
        "i can ",
        "selling",
        "now available",
        "open for work",
    ];
    let d = description.trim_start().to_ascii_lowercase();
    const DESC: &[&str] =
        &["autonomous ai agent. i ", "i am an ai agent", "i'm an ai agent", "hire me", "my services", "what i deliver"];
    TITLE.iter().any(|p| t.starts_with(p))
        || DESC.iter().any(|p| d.starts_with(p) || d.contains(&format!("\n{p}")))
        || d.contains("what i deliver:")
}

/// Whether a listed lead is worth storing.
pub fn keep(f: &Found, s: &Settings) -> bool {
    if is_ad(&f.title, &f.description) {
        return false;
    }
    match f.budget_usd {
        Some(b) => b >= s.min_usd,
        // Unknown pay: keep only when there is no minimum.
        None => s.min_usd <= 0.0,
    }
}

async fn fetch(state: &Shared, s: &Settings, p: &Provider) -> Result<Vec<Found>> {
    let url = list_url(p.id, &s.base(p));
    let accept = if p.id == "algora" { "application/vnd.github+json" } else { "application/json" };
    let mut req = state.http.get(&url).header("accept", accept).timeout(Duration::from_secs(30));
    if let Some(k) = s.key(p) {
        req = authorize(req, p, k);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", crate::mcp::truncate(&String::from_utf8_lossy(&bytes), 200));
    }
    if bytes.len() > MAX_BODY {
        bail!("listing too large ({} bytes)", bytes.len());
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow!("not JSON: {e}"))?;
    Ok(parse(p.id, &v))
}

/// The amount in the Algora bot's bounty comment (`## 💎 $700 bounty`).
/// `None` when the bot posted none: the label alone is not a real bounty.
pub fn algora_amount(comments: &Value) -> Option<f64> {
    items(comments, &[])
        .iter()
        .filter(|c| c.pointer("/user/login").and_then(Value::as_str) == Some("algora-pbc[bot]"))
        .find_map(|c| text(c, "body").lines().find(|l| l.contains('💎') && l.contains('$')).and_then(dollar_amount))
}

async fn algora_bounty(state: &Shared, s: &Settings, p: &Provider, issue_url: &str) -> Result<Option<f64>> {
    let path = issue_url.strip_prefix("https://github.com/").ok_or_else(|| anyhow!("not a GitHub issue"))?;
    let url = format!("{}/repos/{path}/comments?per_page=50", s.base(p));
    let mut req = state.http.get(&url).header("accept", "application/vnd.github+json").timeout(Duration::from_secs(20));
    if let Some(k) = s.key(p) {
        req = authorize(req, p, k);
    }
    let resp = req.send().await?.error_for_status()?;
    let v: Value = resp.json().await?;
    Ok(algora_amount(&v))
}

fn authorize(req: reqwest::RequestBuilder, p: &Provider, key: &str) -> reqwest::RequestBuilder {
    match p.auth {
        Auth::Bearer => req.bearer_auth(key),
        Auth::Header(h) => req.header(h, key),
    }
}

// ------------------------------------------------------------------------ scan

static SCANNING: AtomicBool = AtomicBool::new(false);

struct Running;
impl Drop for Running {
    fn drop(&mut self) {
        SCANNING.store(false, Ordering::SeqCst);
    }
}

pub fn scanning() -> bool {
    SCANNING.load(Ordering::SeqCst)
}

#[derive(Debug, Default, PartialEq)]
pub struct ScanReport {
    /// Open listings seen across providers.
    pub listed: usize,
    /// New leads stored.
    pub new: usize,
    /// Leads in the digest sent to the user.
    pub shortlisted: usize,
    pub errors: Vec<String>,
}

const ACTIVE: &[&str] = &["approved", "working", "submitted"];

/// One scan: list every enabled provider, store new leads, then let the lead
/// agent rate them and follow up on work in progress. `manual` scans run even
/// when earning is switched off and keep their conversation.
pub async fn scan(state: &Shared, manual: bool) -> Result<ScanReport> {
    if SCANNING.swap(true, Ordering::SeqCst) {
        bail!("a scan is already running");
    }
    let _running = Running;
    let s = settings(state).await;
    if !s.enabled && !manual {
        return Ok(ScanReport::default());
    }
    let started = now_ms();
    let _ = state.db.set_setting(LAST_SCAN, &json!(started)).await;
    let mut report = ScanReport::default();
    let mut status = serde_json::Map::new();
    for p in PROVIDERS.iter().filter(|p| s.on(p)) {
        let st = match fetch(state, &s, p).await {
            Ok(found) => {
                let listed = found.len();
                report.listed += listed;
                let (mut kept, mut checked) = (0, 0);
                for mut f in found {
                    if p.id == "algora" {
                        // Only bounties the Algora bot really posted, at the amount it posted.
                        if state.db.lead_exists(p.id, &f.external_id).await? || checked >= ALGORA_CHECKS {
                            continue;
                        }
                        checked += 1;
                        match algora_bounty(state, &s, p, &f.url).await {
                            Ok(Some(amount)) => f.budget_usd = Some(amount),
                            Ok(None) => continue,
                            Err(e) => {
                                tracing::debug!("earn: algora check {}: {e:#}", f.url);
                                continue;
                            }
                        }
                    }
                    if !keep(&f, &s) {
                        continue;
                    }
                    kept += 1;
                    if state.db.insert_lead(p.id, &f).await?.is_some() {
                        report.new += 1;
                    }
                }
                json!({"at": started, "listed": listed, "kept": kept})
            }
            Err(e) => {
                let msg = crate::mcp::truncate(&format!("{e:#}"), 300);
                report.errors.push(format!("{}: {msg}", p.name));
                json!({"at": started, "error": msg})
            }
        };
        status.insert(p.id.to_string(), st);
    }
    let _ = state.db.set_setting(STATUS_KEY, &Value::Object(status)).await;
    let _ = state.db.prune_leads(started - 60 * 86_400_000).await;

    let fresh = state.db.earn_leads(&["new"], RATE_PER_SCAN, true).await?;
    let active = state.db.earn_leads(ACTIVE, 20, false).await?;
    if (fresh.is_empty() && active.is_empty()) || crate::telegram::active_token(state).await.is_none() {
        tracing::info!(?report, "earn scan: nothing for the agent");
        return Ok(report);
    }
    let agent = agent::ensure_default_agent(state).await?;
    let session = state.db.create_session(&agent.id, "earn", None, None, "💰 Earnings scan").await?;
    let prompt = scan_prompt(&s, &fresh, &active, state).await;
    let res = agent::run_turn(
        state.clone(),
        session.clone(),
        prompt,
        TurnOptions { depth: 1, no_reflect: true, agent_override: Some(agent.clone()) },
    )
    .await;
    // Leads the agent did not rate were looked at and passed over.
    for l in &fresh {
        if let Ok(Some(cur)) = state.db.earn_lead(&l.id).await {
            if cur.status == "new" {
                let _ = state.db.update_lead(&l.id, Some("passed"), None, None, None).await;
            }
        }
    }
    let sent = state
        .db
        .notifications(started, 50)
        .await?
        .iter()
        .filter(|n| n.session_id.as_deref() == Some(session.id.as_str()) && n.delivered > 0)
        .count();
    report.shortlisted =
        state.db.earn_leads(&["shortlisted"], 100, false).await?.iter().filter(|l| l.updated_at >= started).count();
    let outcome = match &res {
        Ok(r) if r.error.is_none() => format!(
            "{} listed, {} new, {} shortlisted, {} message(s)",
            report.listed, report.new, report.shortlisted, sent
        ),
        Ok(r) => format!("failed: {}", r.error.clone().unwrap_or_default()),
        Err(e) => format!("failed: {e:#}"),
    };
    if sent == 0 && !manual && res.as_ref().is_ok_and(|r| r.error.is_none()) {
        let _ = state.db.delete_session(&session.id).await;
    }
    let keep_session = (sent > 0 || manual).then_some(session.id.as_str());
    let _ = state
        .db
        .log_improvement(Some(&agent.id), keep_session, "earn_scan", None, &format!("Earnings: {outcome}"))
        .await;
    tracing::info!(%outcome, "earn scan done");
    res?;
    Ok(report)
}

fn money(l: &EarnLead) -> String {
    match l.budget_usd {
        Some(b) => format!("${b:.2} ({})", l.currency),
        None => format!("unknown ({})", l.currency),
    }
}

fn line(l: &EarnLead) -> String {
    let p = provider(&l.provider).map(|p| p.name).unwrap_or(&l.provider);
    format!(
        "- ref {} | {p} | {} | pay {} | bids {} | deadline {} | {}{}",
        l.reference(),
        l.status,
        money(l),
        l.bids.map(|b| b.to_string()).unwrap_or_else(|| "?".into()),
        l.deadline.as_deref().unwrap_or("none"),
        l.title,
        if l.url.is_empty() { String::new() } else { format!(" | {}", l.url) },
    )
}

async fn scan_prompt(s: &Settings, fresh: &[EarnLead], active: &[EarnLead], state: &Shared) -> String {
    let local = crate::proactive::settings(state).await.local_time(now_ms());
    let profile = if s.profile.is_empty() {
        "Not set: judge from what you know about the user (memories) and what you can do with your tools.".to_string()
    } else {
        s.profile.clone()
    };
    let mut p = format!(
        "Earnings scan ({local}). The user asked you to find paid work that earns them money; they are not in this \
conversation.\n\n## What the user wants to take on\n{profile}\n\n"
    );
    if !fresh.is_empty() {
        p.push_str(&format!(
            "## New leads\nRate every lead below with `earn` op `rate` (score 0-10: can you and the user deliver it well \
and on time, is the pay worth the effort, how many others bid). Pass on other agents advertising themselves, vague or \
unpaid work, spam, fake reviews or engagement, academic cheating, scraping behind logins, anything illegal or harmful, \
and anything that needs the user's identity or money up front. Shortlist only real fits, at most {}. Use `earn` op `view` \
when the description is cut short.\n",
            s.max_alerts
        ));
        for l in fresh {
            p.push_str(&line(l));
            p.push('\n');
            if !l.description.is_empty() {
                p.push_str(&format!("  {}\n", crate::mcp::truncate(&l.description.replace('\n', " "), 400)));
            }
        }
        p.push_str(&format!(
            "\nThen send ONE `notify_user` message (requested: true, topic `Paid work {}`) listing the shortlist: for each, \
the ref, title, pay, provider, link, one line on why it fits and how you would do it. End with: reply `take <ref>` and \
I'll bid and do it, or `skip <ref>`. No message if nothing made the shortlist.\n\n",
            &crate::db::new_id()[..6]
        ));
        if s.auto {
            p.push_str(&format!(
                "Auto mode is on: for shortlisted marketplace leads up to ${:.2} on a provider with a key, you may bid \
right away (read the provider docs with web_fetch first, then `earn` op `request`), mark them `working`, and say so in \
the message.\n\n",
                s.auto_max_usd
            ));
        }
    }
    if !active.is_empty() {
        p.push_str(
            "## Work in progress\nFollow up on each: check its state on the provider with `earn` op `request` (GET). If \
the bid was accepted or a contract started, do the work (delegate when it is big) and deliver it through the provider's \
API, then mark it `submitted`. Mark `won`, `lost` or `paid` as it settles, and tell the user (notify_user, requested: \
true) when a bid is accepted, work is delivered, or money arrives. Bounty leads (Algora, Opire) are claimed with a pull \
request from the user's GitHub account: prepare the fix and the steps, and send them to the user.\n",
        );
        for l in active {
            p.push_str(&line(l));
            if !l.note.is_empty() {
                p.push_str(&format!(" | note: {}", crate::mcp::truncate(&l.note, 300)));
            }
            p.push('\n');
        }
        p.push('\n');
    }
    p.push_str("Finish with one line for the log: what you shortlisted, bid on or delivered, or `nothing to report`.");
    p
}

async fn due(state: &Shared) -> bool {
    let s = settings(state).await;
    if !s.enabled || s.scan_minutes == 0 || scanning() {
        return false;
    }
    let now = now_ms();
    match last_scan(state).await {
        Some(t) => now - t >= s.scan_minutes * 60_000,
        None => true,
    }
}

/// The scan loop; runs until the process stops.
pub async fn run(state: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        if due(&state).await {
            if let Err(e) = scan(&state, false).await {
                tracing::warn!("earn scan failed: {e:#}");
            }
        }
    }
}

// ------------------------------------------------------------------- the tool

pub fn tool_spec() -> ToolSpec {
    let ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
    ToolSpec {
        name: "earn".into(),
        description: "Paid work for the user (earnings). leads: list leads (filter by status). view: one lead in full, \
with its provider's docs link. rate: score a new lead and shortlist or pass. status: move a lead along (approved when the \
user says `take <ref>`, skipped for `skip <ref>`, then working, submitted, won, lost, paid). When the user takes a lead, \
get going in the same reply: view it, read the provider docs, bid or prepare the work, and say what you did. request: call a provider's API \
with the user's saved key (you never see it): GET to read jobs, bids, contracts; POST/PUT/PATCH to bid or deliver, only \
for a lead the user approved (or in auto mode). Read the provider docs with web_fetch before writing. Never sign up, \
create keys, withdraw or move money: the user does that."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["leads", "view", "rate", "status", "request"]},
                "ref": {"type": "string", "description": "lead ref (first characters of its id)"},
                "filter": {"type": "string", "description": "leads: statuses, comma separated, e.g. shortlisted,approved; default all recent"},
                "score": {"type": "integer", "minimum": 0, "maximum": 10},
                "shortlist": {"type": "boolean", "description": "rate: true to shortlist, false to pass"},
                "status": {"type": "string", "enum": ["approved", "skipped", "working", "submitted", "won", "lost", "paid"]},
                "note": {"type": "string", "description": "rate/status: why, or what was done (bid id, contract id, amount)"},
                "provider": {"type": "string", "enum": ids},
                "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH"]},
                "path": {"type": "string", "description": "request: path after the provider's API base, e.g. /api/agents/jobs/<id>/bids"},
                "body": {"type": "object", "description": "request: JSON body"}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
    }
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("")
}

async fn lead(state: &Shared, args: &Value) -> Result<EarnLead> {
    let r = arg(args, "ref").trim_start_matches('#');
    if r.len() < 4 {
        bail!("ref is required (at least 4 characters of the lead id)");
    }
    state.db.earn_lead(r).await?.ok_or_else(|| anyhow!("no lead with ref {r}"))
}

pub async fn tool(ctx: &ToolCtx<'_>, args: &Value) -> Result<String> {
    if !ctx.agent.is_default {
        bail!("only the lead agent handles earnings");
    }
    let s = settings(ctx.state).await;
    if !s.enabled {
        bail!("earnings are switched off (Settings → Earnings)");
    }
    let state = ctx.state;
    match arg(args, "op") {
        "leads" => {
            let filter: Vec<&str> = arg(args, "filter").split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
            let leads = state.db.earn_leads(&filter, 40, false).await?;
            if leads.is_empty() {
                return Ok("No leads.".into());
            }
            Ok(leads.iter().map(line).collect::<Vec<_>>().join("\n"))
        }
        "view" => {
            let l = lead(state, args).await?;
            let p = provider(&l.provider);
            Ok(format!(
                "{}\nscore {} | note: {}\ntags: {}\n\n{}\n\nProvider: {} ({}), payout {}. Docs: {}. Key saved: {}. Kongming can act through the API: {}.",
                line(&l),
                l.score.map(|x| x.to_string()).unwrap_or_else(|| "-".into()),
                if l.note.is_empty() { "-" } else { l.note.as_str() },
                l.tags.join(", "),
                crate::mcp::truncate(&l.description, 6000),
                p.map(|p| p.name).unwrap_or("?"),
                p.map(|p| s.base(p)).unwrap_or_default(),
                p.map(|p| p.payout).unwrap_or("?"),
                p.map(|p| p.docs).unwrap_or("?"),
                p.is_some_and(|p| s.key(p).is_some()),
                p.is_some_and(|p| p.acts),
            ))
        }
        "rate" => {
            let l = lead(state, args).await?;
            let score = args.get("score").and_then(Value::as_i64).ok_or_else(|| anyhow!("score 0-10 is required"))?;
            let shortlist = args.get("shortlist").and_then(Value::as_bool).unwrap_or(score >= 7);
            let status = if shortlist { "shortlisted" } else { "passed" };
            state.db.update_lead(&l.id, Some(status), Some(score.clamp(0, 10)), Some(arg(args, "note")), None).await?;
            Ok(format!("{} {status} (score {score}).", l.reference()))
        }
        "status" => {
            let l = lead(state, args).await?;
            let to = arg(args, "status");
            if !["approved", "skipped", "working", "submitted", "won", "lost", "paid"].contains(&to) {
                bail!("status must be approved, skipped, working, submitted, won, lost or paid");
            }
            // Only the user approves: in a conversation they are part of.
            if to == "approved" && !matches!(ctx.session.channel.as_str(), "web" | "telegram") {
                bail!("only the user approves a lead (they reply `take <ref>`)");
            }
            let note = arg(args, "note");
            let note = if note.is_empty() { None } else { Some(note) };
            state.db.update_lead(&l.id, Some(to), None, note, Some(&ctx.session.id)).await?;
            if matches!(to, "won" | "paid") {
                let msg = format!("{} {}: {} ({})", l.reference(), to, l.title, money(&l));
                let _ = state
                    .db
                    .log_improvement(Some(&ctx.agent.id), Some(&ctx.session.id), "earn", Some(&l.id), &msg)
                    .await;
            }
            Ok(format!("{} is now {to}.", l.reference()))
        }
        "request" => request(ctx, &s, args).await,
        "" => bail!("op is required"),
        other => bail!("unknown op {other}"),
    }
}

/// Paths the agent never calls: money out, accounts and keys.
pub fn forbidden_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [
        "withdraw",
        "payout",
        "transfer",
        "register",
        "onboard",
        "signup",
        "sign-up",
        "api-key",
        "apikey",
        "api_key",
        "/keys",
        "/tokens",
        "/claim",
        "payment-method",
        "/auth/",
        "/login",
    ]
    .iter()
    .any(|x| p.contains(x))
}

/// Whether a write for this lead is allowed now.
pub fn may_write(s: &Settings, kind: Kind, lead: &EarnLead) -> Result<(), String> {
    if ACTIVE.contains(&lead.status.as_str()) {
        return Ok(());
    }
    let cheap = lead.budget_usd.is_some_and(|b| b <= s.auto_max_usd);
    if s.auto && kind == Kind::Market && cheap && matches!(lead.status.as_str(), "new" | "shortlisted") {
        return Ok(());
    }
    Err(format!(
        "lead {} is {}: bid or deliver only after the user approves it (`take {}`){}",
        lead.reference(),
        lead.status,
        lead.reference(),
        if s.auto {
            format!("; auto mode covers marketplace leads up to ${:.2}", s.auto_max_usd)
        } else {
            String::new()
        }
    ))
}

async fn request(ctx: &ToolCtx<'_>, s: &Settings, args: &Value) -> Result<String> {
    let p = provider(arg(args, "provider")).ok_or_else(|| anyhow!("provider is required"))?;
    if !s.on(p) {
        bail!("{} is switched off in Settings → Earnings", p.name);
    }
    let method = match arg(args, "method").to_ascii_uppercase().as_str() {
        "" | "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        m => bail!("method {m} is not allowed"),
    };
    let path = arg(args, "path");
    if !path.starts_with('/') || path.contains("..") || path.contains("://") || path.contains('#') {
        bail!("path must be a path on the provider's API, starting with /");
    }
    if forbidden_path(path) {
        bail!("Kongming does not sign up, manage keys or move money; the user does that on {}", p.site);
    }
    let write = method != reqwest::Method::GET;
    if write {
        if !p.acts {
            bail!("{} work is taken by the user, not through its API: {}", p.name, p.key_help);
        }
        if s.key(p).is_none() {
            bail!("no API key for {} yet: {}", p.name, p.key_help);
        }
        let l = lead(ctx.state, args).await.map_err(|e| anyhow!("{e:#}; pass the ref of the lead this is for"))?;
        if l.provider != p.id {
            bail!("lead {} is on {}, not {}", l.reference(), l.provider, p.id);
        }
        may_write(s, p.kind, &l).map_err(|e| anyhow!(e))?;
        if l.status != "working" && l.status != "submitted" {
            ctx.state.db.update_lead(&l.id, Some("working"), None, None, Some(&ctx.session.id)).await?;
        }
    }
    let url = format!("{}{}", s.base(p), path);
    let mut req = ctx
        .state
        .http
        .request(method.clone(), &url)
        .header("accept", "application/json")
        .timeout(Duration::from_secs(60));
    if let Some(k) = s.key(p) {
        req = authorize(req, p, k);
    }
    if write {
        req = req.json(args.get("body").unwrap_or(&json!({})));
    }
    let resp = req.send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let mut body = String::from_utf8_lossy(&bytes[..bytes.len().min(400_000)]).to_string();
    if let Some(k) = s.key(p) {
        body = body.replace(k, "<key>");
    }
    tracing::info!(provider = p.id, %method, path, %status, "earn request");
    Ok(format!("HTTP {status}\n\n{}", crate::mcp::truncate(body.trim(), 8000)))
}

// ---------------------------------------------------------------- dashboard

pub async fn dashboard_json(state: &Shared) -> Result<Value> {
    let s = settings(state).await;
    let status = state.db.setting(STATUS_KEY).await?.unwrap_or(json!({}));
    let providers: Vec<Value> = PROVIDERS
        .iter()
        .map(|p| {
            let kind = match p.kind {
                Kind::Market => "market",
                Kind::Bounty => "bounty",
            };
            json!({
                "id": p.id,
                "name": p.name,
                "kind": kind,
                "site": p.site,
                "docs": p.docs,
                "keyHelp": p.key_help,
                "payout": p.payout,
                "acts": p.acts,
                "enabled": s.on(p),
                "keySet": s.key(p).is_some(),
                "status": status.get(p.id).cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let leads = state.db.earn_leads(&[], 100, false).await?;
    Ok(json!({
        "settings": s.to_json(true),
        "providers": providers,
        "leads": leads,
        "lastScan": last_scan(state).await,
        "scanning": scanning(),
    }))
}

pub async fn status_html(state: &Shared) -> Result<String> {
    let s = settings(state).await;
    let leads = state.db.earn_leads(&[], 200, false).await?;
    let count = |st: &str| leads.iter().filter(|l| l.status == st).count();
    let earned: f64 = leads.iter().filter(|l| l.status == "paid").filter_map(|l| l.budget_usd).sum();
    let next = match (s.enabled, s.scan_minutes) {
        (false, _) => "off".to_string(),
        (true, 0) => "only on demand".to_string(),
        (true, m) => {
            let left = last_scan(state).await.map(|t| t + m * 60_000 - now_ms()).unwrap_or(0).max(0);
            format!("every {m} min, next in about {} min", left / 60_000)
        }
    };
    let mut out = format!(
        "Earnings: <b>{}</b>\nScans: {next}\nLeads: {} shortlisted · {} approved or in progress · {} submitted · {} paid (${earned:.2})\n",
        if s.enabled { "on" } else { "off" },
        count("shortlisted"),
        count("approved") + count("working"),
        count("submitted"),
        count("paid"),
    );
    for l in leads.iter().filter(|l| l.status == "shortlisted").take(5) {
        out.push_str(&format!(
            "\n• <code>{}</code> {} — {}",
            l.reference(),
            crate::telegram::esc(&l.title),
            crate::telegram::esc(&money(l))
        ));
    }
    out.push_str(
        "\n\n/earn on · /earn off · /earn scan\nReply <code>take &lt;ref&gt;</code> or <code>skip &lt;ref&gt;</code>.",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_validation() {
        let s = Settings::default();
        assert!(!s.enabled);
        assert_eq!(s.providers.len(), PROVIDERS.len());
        let s = s
            .merged(&json!({"enabled": true, "minUsd": "10", "providers": {"toku": {"apiKey": "tk_123"}, "algora": {"enabled": false}}}))
            .unwrap();
        assert!(s.enabled && s.min_usd == 10.0);
        let stored = s.to_json(false);
        assert_eq!(stored["providers"]["toku"]["apiKey"], json!("tk_123"));
        let shown = s.to_json(true);
        assert_eq!(shown["providers"]["toku"]["keySet"], json!(true));
        assert!(!shown.to_string().contains("tk_123"));
        assert_eq!(Settings::from_value(Some(&stored)), s);
        assert!(s.merged(&json!({"scanMinutes": 5})).is_err());
        assert!(s.merged(&json!({"providers": {"nope": {"enabled": true}}})).is_err());
        assert!(s.merged(&json!({"providers": {"toku": {"apiKey": "has space"}}})).is_err());
        let cleared = s.merged(&json!({"providers": {"toku": {"clearApiKey": true}}})).unwrap();
        assert!(cleared.key(provider("toku").unwrap()).is_none());
    }

    #[test]
    fn parses_toku_and_dealwork() {
        let toku = json!({"jobPosts": [
            {"id": "a1", "title": "Write 5 product descriptions", "description": "For a shop", "budgetCents": 4500, "bidCount": 3, "tags": ["writing"], "category": "writing", "status": "OPEN"},
            {"id": "", "title": "no id"}
        ], "total": 2});
        let f = parse("toku", &toku);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].budget_usd, Some(45.0));
        assert_eq!(f[0].url, "https://www.toku.agency/jobs/a1");
        assert_eq!(f[0].bids, Some(3));
        let dw = json!({"data": [{"id": "d1", "title": "Landing page", "description": "x", "budgetMin": "20.0000", "budgetMax": "80.0000", "fixedPrice": null, "bidCount": 0}], "meta": {}});
        assert_eq!(parse("dealwork", &dw)[0].budget_usd, Some(80.0));
    }

    #[test]
    fn parses_opentask_moltjobs_and_bounties() {
        let ot = json!({"tasks": [
            {"id": "t1", "title": "Audit a contract", "budgetAmount": "30", "budgetCurrency": "USDC", "budgetText": "30 USDC", "skillsTags": ["solidity"]},
            {"id": "t2", "title": "Paid in ETH", "budgetAmount": "0.1", "budgetCurrency": "ETH"}
        ]});
        let f = parse("opentask", &ot);
        assert_eq!(f[0].budget_usd, Some(30.0));
        assert_eq!(f[1].budget_usd, None);
        let mj = json!({"data": [{"id": "m1", "title": "Referral bounty", "budgetUsdc": "0.2", "inputData": {"generalDescription": "Refer"}}]});
        assert_eq!(parse("moltjobs", &mj)[0].description, "Refer");
        let gh = json!({"items": [{"id": 77, "title": "Fix crash on save", "html_url": "https://github.com/o/r/issues/1", "repository_url": "https://api.github.com/repos/o/r", "labels": [{"name": "💎 Bounty"}, {"name": "$250"}], "comments": 4, "body": "steps"}]});
        let f = parse("algora", &gh);
        assert_eq!(f[0].external_id, "77");
        assert_eq!(f[0].budget_usd, Some(250.0));
        assert_eq!(f[0].tags[0], "o/r");
        let comments = json!([
            {"user": {"login": "someone"}, "body": "## 💎 $9999 bounty"},
            {"user": {"login": "algora-pbc[bot]"}, "body": "## 💎 $700 bounty\n### Steps to solve:\n1. **Start working**"}
        ]);
        assert_eq!(algora_amount(&comments), Some(700.0));
        assert_eq!(algora_amount(&json!([{"user": {"login": "someone"}, "body": "## 💎 $9999 bounty"}])), None);
        let opire = json!([{"id": "01K", "title": "c1work", "url": "https://github.com/o/bugb/issues/1",
            "pendingPrice": {"value": 10000, "unit": "USD_CENT"}, "tryingUsers": [{"id": "u"}], "claimerUsers": [],
            "project": {"url": "https://github.com/o/bugb", "name": "bugb"}}]);
        let f = parse("opire", &opire);
        assert_eq!(f[0].budget_usd, Some(100.0));
        assert_eq!(f[0].bids, Some(1));
        assert_eq!(f[0].tags[0], "o/bugb");
        assert_eq!(f[0].url, "https://github.com/o/bugb/issues/1");
    }

    #[test]
    fn dollar_amounts() {
        assert_eq!(dollar_amount("$1,500"), Some(1500.0));
        assert_eq!(dollar_amount("Bounty $2k"), Some(2000.0));
        assert_eq!(dollar_amount("[$50] fix it"), Some(50.0));
        assert_eq!(dollar_amount("no money"), None);
    }

    #[test]
    fn ads_and_small_jobs_are_dropped() {
        assert!(is_ad("AVAILABLE: Content Retainer", ""));
        assert!(is_ad("🚀 For hire: Rust developer", ""));
        assert!(is_ad("Website creation", "Autonomous AI agent. I build clean websites.\n\nWhat I deliver:\n- pages"));
        assert!(!is_ad("Need 20 product photos captioned", "We sell shoes."));
        let s = Settings::default();
        let job = |b: Option<f64>| Found {
            external_id: "1".into(),
            title: "Real job".into(),
            budget_usd: b,
            ..Default::default()
        };
        assert!(keep(&job(Some(5.0)), &s));
        assert!(!keep(&job(Some(0.2)), &s));
        assert!(!keep(&job(None), &s));
    }

    #[test]
    fn writes_need_approval_or_auto() {
        let mut l = EarnLead {
            id: "abcdef123".into(),
            status: "shortlisted".into(),
            budget_usd: Some(20.0),
            ..Default::default()
        };
        let mut s = Settings::default();
        assert!(may_write(&s, Kind::Market, &l).is_err());
        s.auto = true;
        assert!(may_write(&s, Kind::Market, &l).is_ok());
        l.budget_usd = Some(500.0);
        assert!(may_write(&s, Kind::Market, &l).is_err());
        l.status = "approved".into();
        assert!(may_write(&s, Kind::Market, &l).is_ok());
        assert!(forbidden_path("/v1/wallet/withdraw"));
        assert!(forbidden_path("/api/agents/register"));
        assert!(!forbidden_path("/api/agents/jobs/x/bids"));
    }
}
