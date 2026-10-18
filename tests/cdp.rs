//! The `cdp` browser client against a mock Chrome: `/json/*` over HTTP and a
//! DevTools WebSocket (tungstenite as the server checks the client's framing).

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

mod support;

type Calls = Arc<Mutex<Vec<Value>>>;

async fn mock_chrome() -> (String, Calls) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls: Calls = Arc::default();
    let seen = calls.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, seen.clone()));
        }
    });
    (url, calls)
}

async fn serve(mut stream: TcpStream, calls: Calls) {
    let mut head = [0u8; 16];
    loop {
        let n = stream.peek(&mut head).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        if n >= 10 {
            break;
        }
    }
    if head.starts_with(b"GET /json") || head.starts_with(b"PUT /json") {
        let mut req = Vec::new();
        let mut chunk = [0u8; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await.unwrap();
            if n == 0 {
                return;
            }
            req.extend_from_slice(&chunk[..n]);
        }
        let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
        let body = if line.contains("/json/list") {
            json!([
                {"id": "STOP", "type": "page", "title": "stop", "url": "about:blank#stop-session"},
                {"id": "W1", "type": "service_worker", "title": "sw", "url": "https://example.com/sw.js"},
                {"id": "T1", "type": "page", "title": "Mock", "url": "https://example.com/"}
            ])
        } else if line.contains("/json/new") {
            json!({"id": "T2", "type": "page", "url": line.split('?').nth(1).unwrap_or("")})
        } else {
            json!({"Browser": "Chrome/141.0 (mock)"})
        }
        .to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        return;
    }
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
    while let Some(Ok(msg)) = ws.next().await {
        let Message::Text(text) = msg else { continue };
        let call: Value = serde_json::from_str(&text).unwrap();
        calls.lock().unwrap().push(call.clone());
        // An event and a ping before every answer, as a busy page would send.
        let _ = ws.send(Message::Text(json!({"method": "Page.frameNavigated", "params": {}}).to_string())).await;
        let _ = ws.send(Message::Ping(b"hi".to_vec())).await;
        let expr = call.pointer("/params/expression").and_then(Value::as_str).unwrap_or("");
        let result = match call["method"].as_str().unwrap_or("") {
            "Page.navigate" => json!({"frameId": "F1"}),
            "Runtime.evaluate" if expr == "document.readyState" => json!({"result": {"type": "string", "value": "complete"}}),
            "Runtime.evaluate" if expr.contains("getBoundingClientRect") => {
                json!({"result": {"type": "object", "value": {"x": 10.5, "y": 20}}})
            }
            // Big enough for a 64-bit frame length.
            "Runtime.evaluate" if expr.contains("innerText") => {
                json!({"result": {"type": "string", "value": "x".repeat(100_000)}})
            }
            "Runtime.evaluate" if expr == "boom()" => {
                json!({"result": {"type": "object"}, "exceptionDetails": {"text": "Uncaught", "exception": {"description": "ReferenceError: boom is not defined"}}})
            }
            "Runtime.evaluate" => json!({"result": {"type": "string", "value": format!("eval:{expr}")}}),
            "Nope.method" => {
                let _ = ws.send(Message::Text(json!({"id": call["id"], "error": {"message": "'Nope.method' wasn't found"}}).to_string())).await;
                continue;
            }
            _ => json!({}),
        };
        let _ = ws.send(Message::Text(json!({"id": call["id"], "result": result}).to_string())).await;
    }
}

async fn cdp(url: &str, args: &[&str]) -> anyhow::Result<String> {
    let mut v = vec!["--url".to_string(), url.to_string()];
    v.extend(args.iter().map(|s| s.to_string()));
    tokio::time::timeout(support::timeout(), kongming::cdp::run(v)).await.expect("cdp timed out")
}

fn methods(calls: &Calls) -> Vec<String> {
    calls.lock().unwrap().iter().map(|c| c["method"].as_str().unwrap_or("").to_string()).collect()
}

#[tokio::test]
async fn browser_endpoints_over_http() {
    let (url, _) = mock_chrome().await;
    assert!(cdp(&url, &["version"]).await.unwrap().contains("Chrome/141.0"));
    let tabs = cdp(&url, &["tabs"]).await.unwrap();
    assert!(tabs.contains("T1\tMock\thttps://example.com/"), "{tabs}");
    assert!(!tabs.contains("W1"), "only pages are listed: {tabs}");
    assert_eq!(cdp(&url, &["open", "https://example.org/"]).await.unwrap(), "T2");
}

#[tokio::test]
async fn page_commands_drive_the_first_real_tab() {
    let (url, calls) = mock_chrome().await;
    let out = cdp(&url, &["goto", "https://example.com/a"]).await.unwrap();
    assert!(out.starts_with("eval:document.title"), "{out}");
    let first = calls.lock().unwrap()[0].clone();
    assert_eq!(first["method"], json!("Page.navigate"));
    assert_eq!(first["params"]["url"], json!("https://example.com/a"));

    assert_eq!(cdp(&url, &["text"]).await.unwrap().len(), 100_000);
    assert_eq!(cdp(&url, &["eval", "1", "+", "1"]).await.unwrap(), "eval:1 + 1");
    let err = cdp(&url, &["eval", "boom()"]).await.unwrap_err().to_string();
    assert!(err.contains("boom is not defined"), "{err}");
    let err = cdp(&url, &["call", "Nope.method"]).await.unwrap_err().to_string();
    assert!(err.contains("wasn't found"), "{err}");

    calls.lock().unwrap().clear();
    assert_eq!(cdp(&url, &["type", "#q", "kongming 孔明"]).await.unwrap(), "typed 11 characters into #q");
    let m = methods(&calls);
    assert_eq!(m.iter().filter(|x| *x == "Input.dispatchMouseEvent").count(), 3, "{m:?}");
    let typed = calls.lock().unwrap().iter().find(|c| c["method"] == json!("Input.insertText")).cloned().unwrap();
    assert_eq!(typed["params"]["text"], json!("kongming 孔明"));
    let click = calls.lock().unwrap().iter().find(|c| c["params"]["type"] == json!("mousePressed")).cloned().unwrap();
    assert_eq!((click["params"]["x"].clone(), click["params"]["y"].clone()), (json!(10.5), json!(20.0)));

    calls.lock().unwrap().clear();
    cdp(&url, &["press", "Enter"]).await.unwrap();
    let keys = calls.lock().unwrap().clone();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0]["params"]["text"], json!("\r"));
    assert!(cdp(&url, &["press", "F13"]).await.is_err());
}

#[tokio::test]
async fn an_unreachable_browser_is_explained() {
    let err = cdp("http://127.0.0.1:9", &["tabs"]).await.unwrap_err();
    assert!(format!("{err:#}").contains("browser session running"), "{err:#}");
}
