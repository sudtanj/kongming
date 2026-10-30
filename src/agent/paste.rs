//! Keys and tokens the user pastes into a chat. Before a person's message is
//! stored or shown to the model, every credential-shaped word in it moves to
//! the vault (the `vault` setting) and the text keeps a reference instead:
//! `[secret:<id> <kind> …<last 4>]`. The `setup` tool resolves those
//! references, so a pasted key can be configured without its value ever
//! reaching the model, the message history or the logs.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::db::{new_id, now_ms};
use crate::state::Shared;

const VAULT_KEY: &str = "vault";
/// Pasted secrets are kept this long, and at most this many, for `setup`.
const VAULT_TTL_MS: i64 = 24 * 3_600_000;
const VAULT_MAX: usize = 20;

/// A credential found in a text: its kind and byte range.
#[derive(Debug, Clone, PartialEq)]
pub struct Secret {
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
}

fn is_sep(c: char) -> bool {
    c.is_whitespace() || "\"'`,()[]{}<>|;".contains(c)
}

fn token_chars(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The kind of a word that has a known credential shape.
pub fn shape(w: &str) -> Option<&'static str> {
    if let Some((bot, secret)) = w.split_once(':') {
        if (6..=12).contains(&bot.len())
            && bot.chars().all(|c| c.is_ascii_digit())
            && secret.len() >= 30
            && token_chars(secret)
        {
            return Some("telegram");
        }
        return None;
    }
    if !token_chars(w) {
        return None;
    }
    let prefixed = |p: &str, min: usize| w.strip_prefix(p).is_some_and(|rest| rest.len() >= min);
    if prefixed("sk-ant-", 24) {
        return Some("anthropic");
    }
    if prefixed("sk-or-", 24) {
        return Some("openrouter");
    }
    if prefixed("sk-", 20) {
        return Some("openai");
    }
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"].iter().any(|p| prefixed(p, 30)) || prefixed("github_pat_", 40) {
        return Some("github");
    }
    if prefixed("xoxb-", 20) || prefixed("xoxp-", 20) {
        return Some("slack");
    }
    if w.len() == 39 && w.starts_with("AIza") {
        return Some("google");
    }
    None
}

/// A name that announces a secret: `apiKey`, `TOKEN`, `Bearer`, `password`…
fn announces(name: &str) -> bool {
    let n: String = name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    ["key", "token", "secret", "bearer", "password", "passwd"].iter().any(|k| n.ends_with(k))
}

/// A value that looks like a credential once something announced one.
fn generic(v: &str) -> bool {
    (16..=512).contains(&v.len())
        && !v.contains("://")
        && !v.starts_with(['/', '.', '~', '['])
        && v.chars().all(|c| c.is_ascii_alphanumeric() || "-_.+/=~:".contains(c))
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
}

/// Every credential in `text`, in order, not overlapping.
pub fn detect(text: &str) -> Vec<Secret> {
    let mut out = Vec::new();
    let mut prev: Option<&str> = None;
    let mut i = 0;
    let bytes_len = text.len();
    while i < bytes_len {
        let rest = &text[i..];
        let Some(c) = rest.chars().next() else { break };
        if is_sep(c) {
            i += c.len_utf8();
            continue;
        }
        let len = rest.find(is_sep).unwrap_or(rest.len());
        let raw = &rest[..len];
        // Sentence punctuation and a `name:` colon are not part of the word.
        let lead = raw.len() - raw.trim_start_matches([':', '=']).len();
        let word = raw[lead..].trim_end_matches(['.', ':', '!', '?', '=']);
        let start = i + lead;
        i += len;
        if word.is_empty() {
            continue;
        }
        if let Some(kind) = shape(word) {
            out.push(Secret { kind, start, end: start + word.len() });
        } else if word.contains("://") {
            // A key in a URL's query: https://host/mcp?api_key=…
            if let Some((_, query)) = word.split_once('?') {
                let mut at = start + word.len() - query.len();
                for pair in query.split(['&', '#']) {
                    if let Some((name, value)) = pair.split_once('=') {
                        if announces(name) && (generic(value) || shape(value).is_some()) {
                            let s = at + name.len() + 1;
                            out.push(Secret { kind: shape(value).unwrap_or("key"), start: s, end: s + value.len() });
                        }
                    }
                    at += pair.len() + 1;
                }
            }
        } else if let Some(p) = word.find(['=', ':']).filter(|p| *p > 0) {
            // NAME=value and name:value in one word.
            let (name, value) = (&word[..p], &word[p + 1..]);
            let value_start = start + p + 1;
            if let Some(kind) = shape(value) {
                out.push(Secret { kind, start: value_start, end: value_start + value.len() });
            } else if announces(name) && generic(value) {
                out.push(Secret { kind: "key", start: value_start, end: value_start + value.len() });
            }
        } else if prev.is_some_and(announces) && generic(word) {
            out.push(Secret { kind: "key", start, end: start + word.len() });
        }
        prev = Some(word);
    }
    out
}

fn last4(v: &str) -> String {
    let chars: Vec<char> = v.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

/// The reference a pasted secret is replaced with.
pub fn reference(id: &str, kind: &str, value: &str) -> String {
    format!("[secret:{id} {kind} …{}]", last4(value))
}

async fn vault(state: &Shared) -> serde_json::Map<String, Value> {
    match state.db.setting(VAULT_KEY).await.ok().flatten() {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    }
}

/// Move the credentials in a person's message to the vault and return the
/// text with references in their place. On a storage error the secrets are
/// still cut out (the user is asked to paste them again), never kept.
pub async fn stash(state: &Shared, text: &str) -> String {
    let found = detect(text);
    if found.is_empty() {
        return text.to_string();
    }
    let now = now_ms();
    let mut v = vault(state).await;
    v.retain(|_, e| e.get("at").and_then(Value::as_i64).is_some_and(|at| now - at < VAULT_TTL_MS));
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for s in &found {
        let value = &text[s.start..s.end];
        let id = new_id()[..8].to_string();
        v.insert(id.clone(), json!({"kind": s.kind, "value": value, "at": now}));
        out.push_str(&text[last..s.start]);
        out.push_str(&reference(&id, s.kind, value));
        last = s.end;
    }
    out.push_str(&text[last..]);
    while v.len() > VAULT_MAX {
        let oldest =
            v.iter().min_by_key(|(_, e)| e.get("at").and_then(Value::as_i64).unwrap_or(0)).map(|(k, _)| k.clone());
        let Some(k) = oldest else { break };
        v.remove(&k);
    }
    if let Err(e) = state.db.set_setting(VAULT_KEY, &Value::Object(v)).await {
        tracing::warn!("could not keep {} pasted secret(s): {e:#}", found.len());
    }
    tracing::info!(count = found.len(), "pasted secrets moved to the vault");
    out
}

/// The id in `secret:<id>` or `[secret:<id> …]` at the start of `s`.
fn ref_id(s: &str) -> Option<&str> {
    let s = s.trim().trim_start_matches('[');
    let rest = s.strip_prefix("secret:")?;
    let end = rest.find(|c: char| c.is_whitespace() || c == ']').unwrap_or(rest.len());
    Some(&rest[..end]).filter(|id| !id.is_empty())
}

/// A value for `setup`: a `secret:<id>` reference becomes the pasted value,
/// references inside a longer text (`Bearer [secret:…]`) are filled in, and
/// anything else is taken as it is.
pub async fn resolve(state: &Shared, s: &str) -> Result<String> {
    if !s.contains("secret:") {
        return Ok(s.to_string());
    }
    let v = vault(state).await;
    let lookup = |id: &str| -> Result<String> {
        v.get(id).and_then(|e| e.get("value")).and_then(Value::as_str).map(str::to_string).ok_or_else(|| {
            anyhow!("secret {id} is no longer kept (pasted secrets are kept for a day); ask the user to paste it again")
        })
    };
    let t = s.trim();
    if t.starts_with("secret:") || (t.starts_with("[secret:") && t.ends_with(']') && t.matches('[').count() == 1) {
        let id = ref_id(t).ok_or_else(|| anyhow!("malformed secret reference"))?;
        return lookup(id);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("[secret:") {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        let end = tail.find(']').ok_or_else(|| anyhow!("malformed secret reference"))?;
        let id = ref_id(&tail[..end]).ok_or_else(|| anyhow!("malformed secret reference"))?;
        out.push_str(&lookup(id)?);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `text` with every kept secret, and each of `also`, replaced by its
/// reference: tool output that might echo a value (an error with the URL,
/// say) goes back to the model clean.
pub async fn scrub(state: &Shared, text: &str, also: &[String]) -> String {
    let mut out = text.to_string();
    for (id, e) in vault(state).await {
        if let Some(v) = e.get("value").and_then(Value::as_str).filter(|v| v.len() >= 8) {
            let kind = e.get("kind").and_then(Value::as_str).unwrap_or("key");
            out = out.replace(v, &reference(&id, kind, v));
        }
    }
    for v in also.iter().filter(|v| v.len() >= 8) {
        out = out.replace(v.as_str(), &format!("…{}", last4(v)));
    }
    out
}

/// The kind recorded for a reference, to pick a sensible target.
pub async fn kind_of(state: &Shared, s: &str) -> Option<String> {
    let id = ref_id(s)?;
    vault(state).await.get(id)?.get("kind")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(t: &str) -> Vec<(&'static str, String)> {
        detect(t).into_iter().map(|s| (s.kind, t[s.start..s.end].to_string())).collect()
    }

    const ANT: &str = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
    const TG: &str = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";

    #[test]
    fn known_shapes_are_found() {
        assert_eq!(kinds(&format!("here is my key {ANT} thanks")), vec![("anthropic", ANT.to_string())]);
        assert_eq!(kinds(&format!("bot token: {TG}.")), vec![("telegram", TG.to_string())]);
        let or = "sk-or-v1-0123456789abcdef0123456789abcdef";
        assert_eq!(kinds(or), vec![("openrouter", or.to_string())]);
        let oa = "sk-proj-AbCdEf0123456789GhIjKl";
        assert_eq!(kinds(&format!("`{oa}`")), vec![("openai", oa.to_string())]);
        let gh = "ghp_0123456789abcdefABCDEF0123456789abcd";
        assert_eq!(kinds(&format!("GITHUB_TOKEN={gh}")), vec![("github", gh.to_string())]);
        let g = "AIzaSyA-0123456789abcdefghijklmnopqrstu";
        assert_eq!(g.len(), 39);
        assert_eq!(kinds(g), vec![("google", g.to_string())]);
    }

    #[test]
    fn announced_values_are_found() {
        assert_eq!(kinds("toku api key: tk_live_8f3a9c2e1b7d"), vec![("key", "tk_live_8f3a9c2e1b7d".to_string())]);
        assert_eq!(kinds("API_KEY=abc123def456ghi789"), vec![("key", "abc123def456ghi789".to_string())]);
        assert_eq!(kinds(r#"{"apiKey": "abc123def456ghi789"}"#), vec![("key", "abc123def456ghi789".to_string())]);
        assert_eq!(
            kinds(r#""Authorization": "Bearer eyJhbGciOi.J9abc123def""#),
            vec![("key", "eyJhbGciOi.J9abc123def".to_string())]
        );
        assert_eq!(
            kinds("connect https://mcp.example.com/sse?api_key=abcd1234efgh5678&x=1 please"),
            vec![("key", "abcd1234efgh5678".to_string())]
        );
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for t in [
            "Please look at https://example.com/a/b?token=x and tell me",
            "the uuid 550e8400-e29b-41d4-a716-446655440000 is the id",
            "commit 3f5a1c9e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39 fixed it",
            "internationalization and sk-learn are words",
            "the key thing is to start",
            "token: /etc/ssl/private/server2.key",
            "my telegram id is 123456789 and time is 12:30",
            "risk-adjusted-return-calculation-model-v2",
        ] {
            assert!(detect(t).is_empty(), "{t}: {:?}", kinds(t));
        }
    }

    #[test]
    fn references_name_the_kind_and_the_end() {
        assert_eq!(reference("ab12cd34", "openai", "sk-abcdefgh1234"), "[secret:ab12cd34 openai …1234]");
        assert_eq!(ref_id("secret:ab12cd34"), Some("ab12cd34"));
        assert_eq!(ref_id("[secret:ab12cd34 openai …1234]"), Some("ab12cd34"));
        assert_eq!(ref_id("ab12"), None);
    }
}
