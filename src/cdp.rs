//! `cdp`: a small Chrome DevTools Protocol client for the `shell` tool.
//!
//! BusyBox has no WebSocket client and Node + Playwright do not fit on the
//! board, so the kongming binary doubles as a command (`kongming cdp ...`, or
//! the `cdp` link in the Docker image) that drives a Chrome over CDP, the
//! endpoint Playwright's `connectOverCDP` uses, e.g. the `chrome-cdp` session
//! on the tailnet. The endpoint comes from `CDP_URL` (set by the daemon from
//! `KONGMING_CDP_URL`) or `--url`.
//!
//! Page commands run `Runtime.evaluate` and `Input.*` without enabling any
//! domain (no `Runtime.enable`, which anti-bot scripts look for), and clicks
//! are real mouse events at the element's centre.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const USAGE: &str = "cdp: drive a Chrome browser over the DevTools protocol (the endpoint Playwright's connectOverCDP uses).
Browser: $CDP_URL, or --url http://<tailnet ip>:9222. Page commands act on the first open tab, or on --tab ID.

  cdp version                  browser version
  cdp tabs                     open tabs: id, title, url
  cdp open URL                 open a new tab, prints its id
  cdp close ID                 close a tab
  cdp goto URL                 load URL in the tab and wait until it is loaded
  cdp url                      title and URL of the tab
  cdp text                     visible text of the page (pipe it through grep, head, ...)
  cdp html                     HTML of the page
  cdp eval 'JS'                run JavaScript in the page, prints the result
  cdp click 'SELECTOR'         click an element (real mouse events)
  cdp type 'SELECTOR' 'TEXT'   click an element and type text into it
  cdp press KEY                Enter, Tab, Escape, Backspace, Space, ArrowUp/Down/Left/Right, PageUp/Down
  cdp wait 'SELECTOR' [SECS]   wait until an element exists (default 10 s)
  cdp scroll [PIXELS]          scroll down (negative: up), default one screen
  cdp back                     go back one page
  cdp call METHOD ['JSON']     any CDP command on the tab, prints its result

Never open a URL ending in #stop-session: that ends the browser session.";

const MAX_MESSAGE: usize = 64 << 20;
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Entry point of `kongming cdp ...`: prints the result, returns the exit code.
pub fn main(args: Vec<String>) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cdp: {e}");
            return 1;
        }
    };
    match rt.block_on(run(args)) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            0
        }
        Err(e) => {
            eprintln!("cdp: {e:#}");
            1
        }
    }
}

/// Run one `cdp` command line (without the program name) and return its output.
pub async fn run(args: Vec<String>) -> Result<String> {
    let mut url = std::env::var("CDP_URL").unwrap_or_default();
    let mut tab = None;
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--url" => url = it.next().context("--url needs a value")?,
            "--tab" => tab = Some(it.next().context("--tab needs a value")?),
            _ => rest.push(a),
        }
    }
    let cmd = rest.first().cloned().unwrap_or_else(|| "help".into());
    if matches!(cmd.as_str(), "help" | "-h" | "--help") {
        return Ok(USAGE.into());
    }
    if url.trim().is_empty() {
        bail!("no browser address: set CDP_URL or pass --url http://<tailnet ip>:9222");
    }
    let base = Base::parse(&url)?;
    let arg = |i: usize, what: &str| rest.get(i).cloned().with_context(|| format!("`cdp {cmd}` needs {what}"));
    match cmd.as_str() {
        "version" => Ok(pretty(&base.get("/json/version").await?)),
        "tabs" => {
            let pages = base.pages().await?;
            if pages.is_empty() {
                return Ok("(no open tabs)".into());
            }
            Ok(pages.iter().map(|t| format!("{}\t{}\t{}", s(t, "id"), s(t, "title"), s(t, "url"))).collect::<Vec<_>>().join("\n"))
        }
        "open" => {
            let target = base.put(&format!("/json/new?{}", arg(1, "a URL")?)).await?;
            Ok(s(&target, "id").to_string())
        }
        "close" => base.text(&format!("/json/close/{}", arg(1, "a tab id")?)).await,
        _ => {
            let id = match tab {
                Some(t) => t,
                None => base.default_tab().await?,
            };
            let mut page = Page { ws: Ws::connect(&base.host_port, &format!("/devtools/page/{id}")).await?, next: 0 };
            page_command(&mut page, &cmd, &rest, arg).await
        }
    }
}

async fn page_command(
    page: &mut Page,
    cmd: &str,
    rest: &[String],
    arg: impl Fn(usize, &str) -> Result<String>,
) -> Result<String> {
    match cmd {
        "goto" => {
            let r = page.call("Page.navigate", json!({"url": arg(1, "a URL")?})).await?;
            if let Some(err) = r.get("errorText").and_then(Value::as_str).filter(|e| !e.is_empty()) {
                bail!("navigation failed: {err}");
            }
            page.wait_loaded().await;
            page.location().await
        }
        "url" => page.location().await,
        "back" => {
            page.eval("history.back()").await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            page.wait_loaded().await;
            page.location().await
        }
        "text" => Ok(show(page.eval("document.body ? document.body.innerText : ''").await?)),
        "html" => Ok(show(page.eval("document.documentElement.outerHTML").await?)),
        "eval" => {
            let js = rest.get(1..).map(|r| r.join(" ")).unwrap_or_default();
            if js.trim().is_empty() {
                bail!("`cdp eval` needs JavaScript");
            }
            Ok(show(page.eval(&js).await?))
        }
        "click" => {
            let sel = arg(1, "a CSS selector")?;
            page.click(&sel).await?;
            Ok(format!("clicked {sel}"))
        }
        "type" => {
            let sel = arg(1, "a CSS selector")?;
            let text = arg(2, "the text to type")?;
            page.click(&sel).await?;
            page.call("Input.insertText", json!({"text": text})).await?;
            Ok(format!("typed {} characters into {sel}", text.chars().count()))
        }
        "press" => {
            let key = arg(1, "a key name")?;
            page.press(&key).await?;
            Ok(format!("pressed {key}"))
        }
        "wait" => {
            let sel = arg(1, "a CSS selector")?;
            let secs = rest.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(10);
            let js = format!("!!document.querySelector({})", quote(&sel));
            let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
            loop {
                if page.eval(&js).await.ok() == Some(Value::Bool(true)) {
                    return Ok(format!("found {sel}"));
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("{sel} did not appear within {secs}s");
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        "scroll" => {
            let px = rest.get(1).and_then(|s| s.parse::<i64>().ok());
            let by = px.map(|p| p.to_string()).unwrap_or_else(|| "window.innerHeight".into());
            Ok(show(page.eval(&format!("window.scrollBy(0, {by}); Math.round(window.scrollY)")).await?))
        }
        "call" => {
            let method = arg(1, "a CDP method, e.g. Page.reload")?;
            let params = match rest.get(2) {
                Some(p) => serde_json::from_str(p).context("the params are not valid JSON")?,
                None => json!({}),
            };
            Ok(pretty(&page.call(&method, params).await?))
        }
        other => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Strings print as they are, everything else as JSON.
fn show(v: Value) -> String {
    match v {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => pretty(&v),
    }
}

/// A Rust string as a JavaScript string literal.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// The browser's HTTP endpoint (`/json/...`).
struct Base {
    http: String,
    host_port: String,
    client: reqwest::Client,
}

impl Base {
    fn parse(url: &str) -> Result<Base> {
        let url = url.trim().trim_end_matches('/');
        let full = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
        let u = url::Url::parse(&full).with_context(|| format!("invalid browser address {url}"))?;
        let host = u.host_str().context("the browser address has no host")?;
        let host_port = format!("{host}:{}", u.port().unwrap_or(9222));
        let client = reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?;
        Ok(Base { http: format!("http://{host_port}"), host_port, client })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = req.send().await.with_context(|| {
            format!(
                "cannot reach the browser at {} (is the browser session running, and may this machine reach it on the tailnet?)",
                self.http
            )
        })?;
        if !resp.status().is_success() {
            let status = resp.status();
            bail!("{status}: {}", resp.text().await.unwrap_or_default().trim());
        }
        Ok(resp)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        Ok(self.send(self.client.get(format!("{}{path}", self.http))).await?.json().await?)
    }

    async fn put(&self, path: &str) -> Result<Value> {
        Ok(self.send(self.client.put(format!("{}{path}", self.http))).await?.json().await?)
    }

    async fn text(&self, path: &str) -> Result<String> {
        Ok(self.send(self.client.get(format!("{}{path}", self.http))).await?.text().await?.trim().to_string())
    }

    async fn pages(&self) -> Result<Vec<Value>> {
        let list = self.get("/json/list").await?;
        Ok(list.as_array().cloned().unwrap_or_default().into_iter().filter(|t| s(t, "type") == "page").collect())
    }

    async fn default_tab(&self) -> Result<String> {
        self.pages()
            .await?
            .iter()
            .find(|t| !s(t, "url").ends_with("#stop-session"))
            .map(|t| s(t, "id").to_string())
            .ok_or_else(|| anyhow!("no open tab: open one with `cdp open URL`"))
    }
}

/// The reply to call `id`, skipping the events in between.
async fn answer(ws: &mut Ws, id: u64, method: &str) -> Result<Value> {
    loop {
        let v: Value = serde_json::from_str(&ws.recv_text().await?).unwrap_or(Value::Null);
        if v.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(e) = v.get("error") {
            bail!("{method}: {}", e.get("message").and_then(Value::as_str).unwrap_or("error"));
        }
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// One tab over its DevTools WebSocket.
struct Page {
    ws: Ws,
    next: u64,
}

impl Page {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.ws.send_text(&json!({"id": id, "method": method, "params": params}).to_string()).await?;
        tokio::time::timeout(CALL_TIMEOUT, answer(&mut self.ws, id, method))
            .await
            .map_err(|_| anyhow!("{method}: no answer from the browser within {}s", CALL_TIMEOUT.as_secs()))?
    }

    async fn eval(&mut self, js: &str) -> Result<Value> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true, "awaitPromise": true, "userGesture": true}),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("exception");
            bail!("{msg}");
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Wait for `document.readyState == "complete"`; a slow page is not an error.
    async fn wait_loaded(&mut self) {
        let deadline = tokio::time::Instant::now() + LOAD_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Value::String(s)) = self.eval("document.readyState").await {
                if s == "complete" {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn location(&mut self) -> Result<String> {
        Ok(show(self.eval("document.title + '\\n' + location.href").await?))
    }

    async fn click(&mut self, sel: &str) -> Result<()> {
        let js = format!(
            "(() => {{ const e = document.querySelector({}); if (!e) return null; \
             e.scrollIntoView({{block: 'center', inline: 'center'}}); const r = e.getBoundingClientRect(); \
             return {{x: r.left + r.width / 2, y: r.top + r.height / 2}}; }})()",
            quote(sel)
        );
        let at = self.eval(&js).await?;
        let (Some(x), Some(y)) = (at.get("x").and_then(Value::as_f64), at.get("y").and_then(Value::as_f64)) else {
            bail!("no element matches {sel}");
        };
        self.call("Input.dispatchMouseEvent", json!({"type": "mouseMoved", "x": x, "y": y})).await?;
        for kind in ["mousePressed", "mouseReleased"] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .await?;
        }
        Ok(())
    }

    async fn press(&mut self, key: &str) -> Result<()> {
        let (name, code, vk, text) = match key.to_ascii_lowercase().as_str() {
            "enter" | "return" => ("Enter", "Enter", 13, Some("\r")),
            "tab" => ("Tab", "Tab", 9, None),
            "escape" | "esc" => ("Escape", "Escape", 27, None),
            "backspace" => ("Backspace", "Backspace", 8, None),
            "space" => (" ", "Space", 32, Some(" ")),
            "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, None),
            "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, None),
            "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, None),
            "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, None),
            "pageup" => ("PageUp", "PageUp", 33, None),
            "pagedown" => ("PageDown", "PageDown", 34, None),
            _ => bail!("unknown key {key}; use Enter, Tab, Escape, Backspace, Space, ArrowUp/Down/Left/Right, PageUp/Down"),
        };
        let mut down = json!({"type": "keyDown", "key": name, "code": code, "windowsVirtualKeyCode": vk});
        if let Some(t) = text {
            down["text"] = json!(t);
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        self.call("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": name, "code": code, "windowsVirtualKeyCode": vk}))
            .await?;
        Ok(())
    }
}

/// Minimal WebSocket client (RFC 6455) over plain TCP: CDP on the tailnet is
/// `ws://`, text frames only, and a dependency would cost more than this.
pub struct Ws {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Ws {
    pub async fn connect(host_port: &str, path: &str) -> Result<Ws> {
        let mut stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(host_port))
            .await
            .map_err(|_| anyhow!("connecting to {host_port} timed out (is the browser session running?)"))?
            .with_context(|| format!("connecting to {host_port}"))?;
        let key = base64::engine::general_purpose::STANDARD.encode(rand::random::<[u8; 16]>());
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let end = loop {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            if buf.len() > 16_384 {
                bail!("invalid WebSocket handshake from {host_port}");
            }
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                bail!("{host_port} closed the connection during the WebSocket handshake");
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        let status = head.lines().next().unwrap_or("").to_string();
        if !status.contains(" 101") {
            bail!("the browser refused the WebSocket: {status}");
        }
        buf.drain(..end);
        Ok(Ws { stream, buf })
    }

    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        self.send_frame(0x1, text.as_bytes()).await
    }

    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let len = payload.len();
        let mut f = Vec::with_capacity(len + 14);
        f.push(0x80 | opcode);
        if len < 126 {
            f.push(0x80 | len as u8);
        } else if len <= 0xffff {
            f.push(0x80 | 126);
            f.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask: [u8; 4] = rand::random();
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&f).await?;
        Ok(())
    }

    async fn fill(&mut self, n: usize) -> Result<()> {
        let mut chunk = [0u8; 16_384];
        while self.buf.len() < n {
            let r = self.stream.read(&mut chunk).await?;
            if r == 0 {
                bail!("the browser closed the connection");
            }
            self.buf.extend_from_slice(&chunk[..r]);
        }
        Ok(())
    }

    /// Next text message; answers pings and joins fragments.
    pub async fn recv_text(&mut self) -> Result<String> {
        let mut msg = Vec::new();
        loop {
            self.fill(2).await?;
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let (fin, opcode, masked) = (b0 & 0x80 != 0, b0 & 0x0f, b1 & 0x80 != 0);
            let mut len = (b1 & 0x7f) as usize;
            let mut off = 2;
            if len == 126 {
                self.fill(4).await?;
                len = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
                off = 4;
            } else if len == 127 {
                self.fill(10).await?;
                let mut b = [0u8; 8];
                b.copy_from_slice(&self.buf[2..10]);
                len = u64::from_be_bytes(b) as usize;
                off = 10;
            }
            if len > MAX_MESSAGE || msg.len() + len > MAX_MESSAGE {
                bail!("message from the browser is too large");
            }
            let mask_at = off;
            if masked {
                off += 4;
            }
            self.fill(off + len).await?;
            let mut payload = self.buf[off..off + len].to_vec();
            if masked {
                let m = [self.buf[mask_at], self.buf[mask_at + 1], self.buf[mask_at + 2], self.buf[mask_at + 3]];
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= m[i % 4];
                }
            }
            self.buf.drain(..off + len);
            match opcode {
                0x8 => bail!("the browser closed the connection"),
                0x9 => self.send_frame(0xA, &payload).await?,
                0xA => {}
                _ => {
                    msg.extend_from_slice(&payload);
                    if fin {
                        return Ok(String::from_utf8_lossy(&msg).into_owned());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalised() {
        assert_eq!(Base::parse("100.64.1.2").unwrap().host_port, "100.64.1.2:9222");
        assert_eq!(Base::parse("http://100.64.1.2:9333/").unwrap().http, "http://100.64.1.2:9333");
        assert_eq!(Base::parse("ws://chrome:9222").unwrap().host_port, "chrome:9222");
    }

    #[test]
    fn selectors_are_quoted_for_javascript() {
        assert_eq!(quote(r#"a[href="x"]"#), r#""a[href=\"x\"]""#);
    }

    #[tokio::test]
    async fn help_needs_no_browser() {
        assert!(run(vec!["help".into()]).await.unwrap().contains("cdp goto URL"));
        assert!(run(vec!["--url".into(), "".into(), "text".into()]).await.is_err());
    }
}
