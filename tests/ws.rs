//! WebSocket: authentication, the ready frame, a streamed turn and stop.

mod support;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use support::{spawn_app, text, TestApp};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(app: &TestApp) -> Ws {
    let mut req = app.ws_url().into_client_request().expect("ws request");
    let cookie = format!("{}={}", kongming::web::auth::COOKIE, app.token);
    req.headers_mut().insert("cookie", HeaderValue::from_str(&cookie).expect("cookie value"));
    let (ws, _resp) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(req))
        .await
        .expect("ws connect timed out")
        .expect("ws connect");
    ws
}

/// Next text frame as JSON (control frames are skipped).
async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("socket closed")
            .expect("socket error");
        match frame {
            Message::Text(t) => return serde_json::from_str(&t).expect("json frame"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn send_json(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string())).await.expect("send frame");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_unauthenticated_connections() {
    let app = spawn_app(vec![]).await;
    let req = app.ws_url().into_client_request().expect("ws request");
    let res = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(req)).await.expect("timeout");
    assert!(res.is_err(), "connection without a cookie must be refused");
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_a_turn_over_the_socket() {
    let app = spawn_app(vec![text("streamed reply")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let mut ws = connect(&app).await;

    let ready = next_json(&mut ws).await;
    assert_eq!(ready["type"], json!("ready"));
    assert_eq!(ready["version"], json!(env!("CARGO_PKG_VERSION")));

    // Ping/pong and stop on an idle session.
    send_json(&mut ws, json!({"type": "ping", "requestId": "p1"})).await;
    let pong = next_json(&mut ws).await;
    assert_eq!(pong["type"], json!("pong"));
    assert_eq!(pong["requestId"], json!("p1"));
    send_json(&mut ws, json!({"type": "stop", "sessionId": sid, "requestId": "s1"})).await;
    let stopped = next_json(&mut ws).await;
    assert_eq!(stopped["type"], json!("stopped"));
    assert_eq!(stopped["stopped"], json!(false));
    assert_eq!(stopped["requestId"], json!("s1"));

    // Validation.
    send_json(&mut ws, json!({"type": "send", "sessionId": sid, "text": "   ", "requestId": "e1"})).await;
    let err = next_json(&mut ws).await;
    assert_eq!(err["type"], json!("error"));
    assert_eq!(err["requestId"], json!("e1"));
    send_json(&mut ws, json!({"type": "send", "sessionId": "nope", "text": "hi", "requestId": "e2"})).await;
    let err = next_json(&mut ws).await;
    assert_eq!(err["type"], json!("error"));
    assert_eq!(err["message"], json!("session not found"));
    send_json(&mut ws, json!({"type": "bogus"})).await;
    assert_eq!(next_json(&mut ws).await["type"], json!("error"));

    // A real turn.
    send_json(&mut ws, json!({"type": "send", "sessionId": sid, "text": "hi", "requestId": "1"})).await;
    let mut frames: Vec<Value> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no turn_end; frames so far: {frames:?}");
        let f = next_json(&mut ws).await;
        let done = f["type"] == json!("turn_end") && f["sessionId"] == json!(sid);
        frames.push(f);
        if done {
            break;
        }
    }
    let types: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap_or("")).collect();

    let accepted = frames.iter().find(|f| f["type"] == json!("accepted")).expect("accepted frame");
    assert_eq!(accepted["requestId"], json!("1"));
    assert_eq!(accepted["sessionId"], json!(sid));

    let user = frames.iter().find(|f| f["type"] == json!("user_message")).expect("user_message");
    assert_eq!(user["message"]["parts"][0]["text"], json!("hi"));
    let start = frames.iter().find(|f| f["type"] == json!("turn_start")).expect("turn_start");
    assert_eq!(start["agent"]["handle"], json!("kongming"));
    let turn_id = start["turnId"].as_str().unwrap().to_string();

    let deltas: Vec<&Value> = frames.iter().filter(|f| f["type"] == json!("delta")).collect();
    assert!(!deltas.is_empty(), "no delta frames in {types:?}");
    assert!(deltas.iter().all(|d| d["turnId"] == json!(turn_id)));
    let streamed: String = deltas.iter().map(|d| d["text"].as_str().unwrap_or("")).collect();
    assert_eq!(streamed, "streamed reply");

    let message = frames
        .iter()
        .find(|f| f["type"] == json!("message") && f["message"]["role"] == json!("assistant"))
        .expect("assistant message frame");
    assert_eq!(message["message"]["parts"][0]["text"], json!("streamed reply"));
    assert_eq!(message["sessionId"], json!(sid));

    let session_updated = frames.iter().find(|f| f["type"] == json!("session_updated")).expect("session_updated");
    assert_eq!(session_updated["session"]["title"], json!("hi"));

    let end = frames.last().unwrap();
    assert_eq!(end["type"], json!("turn_end"));
    assert_eq!(end["stopReason"], json!("stop"));
    assert!(end["error"].is_null());
    assert_eq!(end["turnId"], json!(turn_id));

    // Order sanity: accepted before the deltas, deltas before the persisted message.
    let pos = |t: &str| types.iter().position(|x| *x == t).unwrap();
    assert!(pos("accepted") < pos("delta"));
    assert!(pos("delta") < pos("message"));
    assert!(pos("turn_start") < pos("delta"));

    // History matches what was streamed.
    let msgs = app.messages(&sid).await;
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1]["parts"][0]["text"], json!("streamed reply"));

    // Sending while the session is idle again works; a second socket sees the same bus.
    let mut ws2 = connect(&app).await;
    assert_eq!(next_json(&mut ws2).await["type"], json!("ready"));
    app.llm.push(vec![text("second")]);
    send_json(&mut ws, json!({"type": "send", "sessionId": sid, "text": "again", "requestId": "2"})).await;
    let mut saw_end = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let f = next_json(&mut ws2).await;
        if f["type"] == json!("turn_end") && f["sessionId"] == json!(sid) {
            saw_end = true;
            break;
        }
    }
    assert!(saw_end, "the second socket did not observe the turn");
    ws.close(None).await.ok();
    ws2.close(None).await.ok();
}
