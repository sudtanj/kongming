//! Password login with an HMAC-signed, expiring cookie token. No sessions are
//! stored anywhere; the secret alone validates tokens. A token carries the
//! revocation epoch (the `auth_epoch` setting): logging out bumps it, which
//! ends every session, and tokens from before the epoch (or without one) fail.

use std::collections::HashMap;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::state::Shared;

pub const COOKIE: &str = "kongming_session";
const TTL_MS: i64 = 30 * 24 * 3600 * 1000;
const EPOCH_KEY: &str = "auth_epoch";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Epoch per signing secret (one entry per daemon; tests run several apps in a process).
static EPOCHS: Mutex<Option<HashMap<Vec<u8>, u64>>> = Mutex::new(None);
/// Consecutive failed logins and the time of the last one.
static LOGIN_FAILS: Mutex<(u32, i64)> = Mutex::new((0, 0));

fn epoch(secret: &[u8]) -> u64 {
    EPOCHS.lock().as_ref().and_then(|m| m.get(secret).copied()).unwrap_or(0)
}

fn set_epoch(secret: &[u8], n: u64) {
    EPOCHS.lock().get_or_insert_with(HashMap::new).insert(secret.to_vec(), n);
}

/// Read the saved epoch at boot.
pub async fn load_epoch(state: &Shared) {
    if let Ok(Some(n)) = state.db.setting(EPOCH_KEY).await.map(|v| v.and_then(|v| v.as_u64())) {
        set_epoch(&state.secret, n);
    }
}

/// End every session: tokens issued so far stop verifying.
pub async fn revoke_all(state: &Shared) -> anyhow::Result<()> {
    let next = epoch(&state.secret) + 1;
    state.db.set_setting(EPOCH_KEY, &serde_json::json!(next)).await?;
    set_epoch(&state.secret, next);
    Ok(())
}

/// Milliseconds a login must wait after repeated failures: from the fifth
/// failure on, 30 s doubling per failure up to 15 minutes. Applies to every
/// caller, so guessing in parallel gains nothing.
pub fn login_wait_ms() -> i64 {
    let (fails, last) = *LOGIN_FAILS.lock();
    if fails < 5 {
        return 0;
    }
    let lock = (30_000i64 << (fails - 5).min(5)).min(15 * 60 * 1000);
    (last + lock - crate::db::now_ms()).max(0)
}

pub fn login_failed() {
    let mut g = LOGIN_FAILS.lock();
    let now = crate::db::now_ms();
    if now - g.1 > 15 * 60 * 1000 {
        g.0 = 0;
    }
    *g = (g.0.saturating_add(1), now);
}

pub fn login_succeeded() {
    *LOGIN_FAILS.lock() = (0, 0);
}

fn sign(secret: &[u8], payload: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

pub fn issue(secret: &[u8]) -> String {
    let exp = crate::db::now_ms() + TTL_MS;
    let payload = B64.encode(format!("{exp}:{}", epoch(secret)));
    format!("{payload}.{}", sign(secret, &payload))
}

pub fn verify(secret: &[u8], token: &str) -> bool {
    let Some((payload, sig)) = token.split_once('.') else { return false };
    let expected = sign(secret, payload);
    if expected.as_bytes().ct_eq(sig.as_bytes()).unwrap_u8() != 1 {
        return false;
    }
    let Ok(bytes) = B64.decode(payload) else { return false };
    let Ok(text) = String::from_utf8(bytes) else { return false };
    let Some((Ok(exp), Ok(ep))) = text.split_once(':').map(|(e, n)| (e.parse::<i64>(), n.parse::<u64>())) else {
        return false;
    };
    exp > crate::db::now_ms() && ep == epoch(secret)
}

pub fn password_ok(state: &Shared, candidate: &str) -> bool {
    let want = state.cfg.password.as_bytes();
    if want.is_empty() {
        return true;
    }
    want.ct_eq(candidate.as_bytes()).unwrap_u8() == 1
}

pub fn token_from_request(req: &Request) -> Option<String> {
    if let Some(v) = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_string());
        }
    }
    if let Some(c) = req.headers().get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for part in c.split(';') {
            let p = part.trim();
            if let Some(v) = p.strip_prefix(&format!("{COOKIE}=")) {
                return Some(v.to_string());
            }
        }
    }
    // A token in the address ends up in logs and history: only the WebSocket, which cannot set headers, may use it.
    if let Some(q) = req.uri().query().filter(|_| req.uri().path() == "/ws") {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("token=") {
                return Some(v.to_string());
            }
        }
    }
    None
}

pub fn is_authenticated(state: &Shared, req: &Request) -> bool {
    if state.cfg.password.is_empty() {
        return true;
    }
    token_from_request(req).is_some_and(|t| verify(&state.secret, &t))
}

pub async fn require_auth(State(state): State<Shared>, req: Request, next: Next) -> Response {
    if is_authenticated(&state, &req) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({"error": "unauthorized"}))).into_response()
    }
}

pub fn cookie_header(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        TTL_MS / 1000,
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie_header() -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_verify_only_with_same_secret() {
        let t = issue(b"secret");
        assert!(verify(b"secret", &t));
        assert!(!verify(b"other", &t));
        assert!(!verify(b"secret", "garbage"));
        assert!(!verify(b"secret", &format!("{t}x")));
    }

    #[test]
    fn expired_token_is_rejected() {
        let payload = B64.encode(format!("{}:0", crate::db::now_ms() - 1000));
        let t = format!("{payload}.{}", sign(b"s", &payload));
        assert!(!verify(b"s", &t));
    }

    #[test]
    fn tokens_from_another_epoch_or_without_one_are_rejected() {
        let t = issue(b"epoch-secret");
        assert!(verify(b"epoch-secret", &t));
        set_epoch(b"epoch-secret", 1);
        assert!(!verify(b"epoch-secret", &t));
        assert!(verify(b"epoch-secret", &issue(b"epoch-secret")));
        let old = B64.encode((crate::db::now_ms() + 1000).to_string());
        assert!(!verify(b"s", &format!("{old}.{}", sign(b"s", &old))));
    }
}
