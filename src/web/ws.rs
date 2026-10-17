//! WebSocket: one connection receives every bus event (the client filters by
//! session) and can send `send` / `stop` frames.

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use crate::agent::{self, TurnOptions};
use crate::state::Shared;

pub async fn handler(State(state): State<Shared>, req: Request) -> Response {
    if !super::auth::is_authenticated(&state, &req) {
        return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let (mut parts, body) = req.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    drop(body);
    ws.on_upgrade(move |socket| run(state, socket))
}

use axum::extract::FromRequestParts;

async fn run(state: Shared, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let mut events = state.subscribe();
    let _ = tx.send(WsMessage::Text(json!({"type": "ready", "version": env!("CARGO_PKG_VERSION")}).to_string())).await;
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(25));
    loop {
        tokio::select! {
            ev = events.recv() => {
                match ev {
                    Ok(ev) => {
                        let text = serde_json::to_string(&ev).unwrap_or_default();
                        if tx.send(WsMessage::Text(text)).await.is_err() { break; }
                    }
                    Err(RecvError::Lagged(n)) => {
                        let _ = tx.send(WsMessage::Text(json!({"type": "lagged", "skipped": n}).to_string())).await;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            msg = rx.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(t))) => {
                        if let Some(reply) = handle_frame(&state, &t).await {
                            if tx.send(WsMessage::Text(reply.to_string())).await.is_err() { break; }
                        }
                    }
                    Some(Ok(WsMessage::Ping(p))) => { let _ = tx.send(WsMessage::Pong(p)).await; }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            _ = ping.tick() => {
                if tx.send(WsMessage::Ping(vec![])).await.is_err() { break; }
            }
        }
    }
}

/// Handle a client frame; returns an immediate reply frame if any.
pub async fn handle_frame(state: &Shared, text: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(text).ok()?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let request_id = v.get("requestId").cloned().unwrap_or(Value::Null);
    match kind {
        "send" => {
            let session_id = v.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
            let text = v.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if text.is_empty() {
                return Some(json!({"type": "error", "requestId": request_id, "message": "empty message"}));
            }
            let sess = match state.db.session(&session_id).await {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return Some(json!({"type": "error", "requestId": request_id, "message": "session not found"}))
                }
                Err(e) => return Some(json!({"type": "error", "requestId": request_id, "message": format!("{e:#}")})),
            };
            if state.is_running(&session_id) {
                return Some(
                    json!({"type": "error", "requestId": request_id, "sessionId": session_id, "message": "a reply is already in progress"}),
                );
            }
            let st = state.clone();
            tokio::spawn(async move {
                if let Err(e) = agent::run_turn(st, sess, text, TurnOptions::default()).await {
                    tracing::warn!("turn failed: {e:#}");
                }
            });
            Some(json!({"type": "accepted", "requestId": request_id, "sessionId": session_id}))
        }
        "stop" => {
            let session_id = v.get("sessionId").and_then(Value::as_str).unwrap_or("");
            Some(
                json!({"type": "stopped", "requestId": request_id, "sessionId": session_id, "stopped": state.cancel_turn(session_id)}),
            )
        }
        "ping" => Some(json!({"type": "pong", "requestId": request_id})),
        _ => Some(json!({"type": "error", "requestId": request_id, "message": format!("unknown frame type '{kind}'")})),
    }
}
