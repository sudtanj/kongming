//! OAuth 2.1 for remote MCP servers: protected-resource and authorization-server
//! metadata discovery (RFC 9728 / RFC 8414), dynamic client registration
//! (RFC 7591), authorization code with PKCE, and refresh.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db::{now_ms, OAuthState};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    B64.encode(buf)
}

pub fn pkce_challenge(verifier: &str) -> String {
    B64.encode(Sha256::digest(verifier.as_bytes()))
}

fn origin(url: &str) -> Result<String> {
    let u = url::Url::parse(url).context("invalid server url")?;
    Ok(u.origin().ascii_serialization())
}

async fn get_json(http: &reqwest::Client, url: &str) -> Option<Value> {
    let r = http.get(url).header("accept", "application/json").send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

/// Find the authorization server for `server_url` and its endpoints. Falls back
/// to the conventional paths on the server's origin when no metadata is served.
pub async fn discover(http: &reqwest::Client, server_url: &str, scope_hint: Option<&str>) -> Result<OAuthState> {
    let server_origin = origin(server_url)?;
    let path = url::Url::parse(server_url).map(|u| u.path().trim_end_matches('/').to_string()).unwrap_or_default();

    // 1. Protected resource metadata (path-specific first, then origin-wide).
    let mut auth_server = server_origin.clone();
    let mut resource = Some(server_url.trim_end_matches('/').to_string());
    let mut scope = scope_hint.map(str::to_string);
    for candidate in [format!("{server_origin}/.well-known/oauth-protected-resource{path}"), format!("{server_origin}/.well-known/oauth-protected-resource")] {
        if let Some(meta) = get_json(http, &candidate).await {
            if let Some(a) = meta.get("authorization_servers").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str) {
                auth_server = a.trim_end_matches('/').to_string();
            }
            if let Some(r) = meta.get("resource").and_then(Value::as_str) {
                resource = Some(r.to_string());
            }
            if scope.is_none() {
                if let Some(s) = meta.get("scopes_supported").and_then(Value::as_array) {
                    let joined: Vec<&str> = s.iter().filter_map(Value::as_str).collect();
                    if !joined.is_empty() {
                        scope = Some(joined.join(" "));
                    }
                }
            }
            break;
        }
    }

    // 2. Authorization server metadata.
    let as_url = url::Url::parse(&auth_server).context("invalid authorization server url")?;
    let as_origin = as_url.origin().ascii_serialization();
    let as_path = as_url.path().trim_end_matches('/').to_string();
    let candidates = [
        format!("{as_origin}/.well-known/oauth-authorization-server{as_path}"),
        format!("{as_origin}/.well-known/openid-configuration{as_path}"),
        format!("{auth_server}/.well-known/openid-configuration"),
    ];
    let mut st = OAuthState { scope, resource, ..Default::default() };
    for c in candidates {
        if let Some(meta) = get_json(http, &c).await {
            st.authorization_endpoint = meta.get("authorization_endpoint").and_then(Value::as_str).map(str::to_string);
            st.token_endpoint = meta.get("token_endpoint").and_then(Value::as_str).map(str::to_string);
            st.registration_endpoint = meta.get("registration_endpoint").and_then(Value::as_str).map(str::to_string);
            if st.scope.is_none() {
                if let Some(s) = meta.get("scopes_supported").and_then(Value::as_array) {
                    let joined: Vec<&str> = s.iter().filter_map(Value::as_str).collect();
                    if !joined.is_empty() {
                        st.scope = Some(joined.join(" "));
                    }
                }
            }
            if st.authorization_endpoint.is_some() && st.token_endpoint.is_some() {
                break;
            }
        }
    }
    if st.authorization_endpoint.is_none() {
        st.authorization_endpoint = Some(format!("{auth_server}/authorize"));
    }
    if st.token_endpoint.is_none() {
        st.token_endpoint = Some(format!("{auth_server}/token"));
    }
    if st.registration_endpoint.is_none() {
        st.registration_endpoint = Some(format!("{auth_server}/register"));
    }
    Ok(st)
}

/// Dynamic client registration. On failure the state is returned unchanged so a
/// pre-configured client id can still be used.
pub async fn register(http: &reqwest::Client, st: &mut OAuthState, redirect_uri: &str) -> Result<()> {
    if st.client_id.as_deref().map_or(false, |c| !c.is_empty()) {
        return Ok(());
    }
    let Some(reg) = st.registration_endpoint.clone() else { bail!("no registration endpoint and no client id configured") };
    let body = serde_json::json!({
        "client_name": "kongming",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let r = http.post(&reg).json(&body).send().await.context("registration request failed")?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("client registration failed (HTTP {status}): {}", v.get("error_description").or(v.get("error")).map(|e| e.to_string()).unwrap_or_default());
    }
    st.client_id = v.get("client_id").and_then(Value::as_str).map(str::to_string);
    st.client_secret = v.get("client_secret").and_then(Value::as_str).map(str::to_string);
    if st.client_id.is_none() {
        bail!("registration reply had no client_id");
    }
    Ok(())
}

pub fn authorize_url(st: &OAuthState, redirect_uri: &str, state: &str, verifier: &str) -> Result<String> {
    let endpoint = st.authorization_endpoint.as_deref().ok_or_else(|| anyhow!("no authorization endpoint"))?;
    let client_id = st.client_id.as_deref().ok_or_else(|| anyhow!("no client id"))?;
    let mut u = url::Url::parse(endpoint).context("invalid authorization endpoint")?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("state", state);
        q.append_pair("code_challenge", &pkce_challenge(verifier));
        q.append_pair("code_challenge_method", "S256");
        if let Some(s) = &st.scope {
            if !s.is_empty() {
                q.append_pair("scope", s);
            }
        }
        if let Some(r) = &st.resource {
            q.append_pair("resource", r);
        }
    }
    Ok(u.to_string())
}

async fn token_request(http: &reqwest::Client, st: &mut OAuthState, form: &HashMap<&str, String>) -> Result<()> {
    let endpoint = st.token_endpoint.clone().ok_or_else(|| anyhow!("no token endpoint"))?;
    let mut req = http.post(&endpoint).header("accept", "application/json");
    if let (Some(id), Some(secret)) = (&st.client_id, &st.client_secret) {
        req = req.basic_auth(id, Some(secret));
    }
    let r = req.form(form).send().await.context("token request failed")?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!(
            "token endpoint HTTP {status}: {}",
            v.get("error_description").or(v.get("error")).and_then(Value::as_str).unwrap_or("no detail")
        );
    }
    let access = v.get("access_token").and_then(Value::as_str).ok_or_else(|| anyhow!("token reply had no access_token"))?;
    st.access_token = Some(access.to_string());
    if let Some(rt) = v.get("refresh_token").and_then(Value::as_str) {
        st.refresh_token = Some(rt.to_string());
    }
    st.expires_at = v.get("expires_in").and_then(Value::as_i64).map(|s| now_ms() + s * 1000);
    if let Some(s) = v.get("scope").and_then(Value::as_str) {
        st.scope = Some(s.to_string());
    }
    Ok(())
}

pub async fn exchange_code(http: &reqwest::Client, st: &mut OAuthState, code: &str, redirect_uri: &str, verifier: &str) -> Result<()> {
    let mut form: HashMap<&str, String> = HashMap::new();
    form.insert("grant_type", "authorization_code".into());
    form.insert("code", code.into());
    form.insert("redirect_uri", redirect_uri.into());
    form.insert("code_verifier", verifier.into());
    if let Some(id) = &st.client_id {
        form.insert("client_id", id.clone());
    }
    if let Some(r) = &st.resource {
        form.insert("resource", r.clone());
    }
    token_request(http, st, &form).await
}

pub async fn refresh(http: &reqwest::Client, st: &mut OAuthState) -> Result<()> {
    let rt = st.refresh_token.clone().ok_or_else(|| anyhow!("no refresh token"))?;
    let mut form: HashMap<&str, String> = HashMap::new();
    form.insert("grant_type", "refresh_token".into());
    form.insert("refresh_token", rt);
    if let Some(id) = &st.client_id {
        form.insert("client_id", id.clone());
    }
    if let Some(r) = &st.resource {
        form.insert("resource", r.clone());
    }
    token_request(http, st, &form).await
}

/// True when the token is missing or within 60 s of expiry.
pub fn needs_refresh(st: &OAuthState) -> bool {
    match (&st.access_token, st.expires_at) {
        (None, _) => true,
        (Some(t), _) if t.is_empty() => true,
        (Some(_), Some(exp)) => exp - 60_000 <= now_ms(),
        (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_example() {
        // RFC 7636 appendix B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn authorize_url_has_pkce_and_state() {
        let st = OAuthState {
            client_id: Some("cid".into()),
            authorization_endpoint: Some("https://as.example/authorize".into()),
            scope: Some("read write".into()),
            resource: Some("https://mcp.example/mcp".into()),
            ..Default::default()
        };
        let u = authorize_url(&st, "https://me/cb", "st8", "ver").unwrap();
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains("state=st8"));
        assert!(u.contains("scope=read+write"));
        assert!(u.contains("resource=https%3A%2F%2Fmcp.example%2Fmcp"));
    }

    #[test]
    fn refresh_logic() {
        assert!(needs_refresh(&OAuthState::default()));
        let fresh = OAuthState { access_token: Some("t".into()), expires_at: Some(now_ms() + 3_600_000), ..Default::default() };
        assert!(!needs_refresh(&fresh));
        let stale = OAuthState { access_token: Some("t".into()), expires_at: Some(now_ms() + 10_000), ..Default::default() };
        assert!(needs_refresh(&stale));
    }

    #[test]
    fn random_tokens_differ() {
        assert_ne!(random_token(32), random_token(32));
    }
}
