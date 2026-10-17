//! Password login with an HMAC-signed, expiring cookie token. No sessions are
//! stored anywhere; the secret alone validates tokens.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::state::Shared;

pub const COOKIE: &str = "kongming_session";
const TTL_MS: i64 = 30 * 24 * 3600 * 1000;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

fn sign(secret: &[u8], payload: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

pub fn issue(secret: &[u8]) -> String {
    let exp = crate::db::now_ms() + TTL_MS;
    let payload = B64.encode(exp.to_string());
    format!("{payload}.{}", sign(secret, &payload))
}

pub fn verify(secret: &[u8], token: &str) -> bool {
    let Some((payload, sig)) = token.split_once('.') else { return false };
    let expected = sign(secret, payload);
    if expected.as_bytes().ct_eq(sig.as_bytes()).unwrap_u8() != 1 {
        return false;
    }
    let Ok(bytes) = B64.decode(payload) else { return false };
    let Ok(exp) = String::from_utf8(bytes).map_err(|_| ()).and_then(|s| s.parse::<i64>().map_err(|_| ())) else {
        return false;
    };
    exp > crate::db::now_ms()
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
    if let Some(q) = req.uri().query() {
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
        let payload = B64.encode((crate::db::now_ms() - 1000).to_string());
        let t = format!("{payload}.{}", sign(b"s", &payload));
        assert!(!verify(b"s", &t));
    }
}
