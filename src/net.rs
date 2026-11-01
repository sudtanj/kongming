//! Outbound HTTP behind one small trait, so the host picks the client:
//! reqwest with rustls in the native daemon, `fetch` in a Cloudflare Worker.
//! `Client` gives the rest of the code a reqwest-like builder on top of it.
//!
//! The futures are `Send` on every target: a Worker host wraps its
//! JavaScript futures (e.g. `worker::send::SendFuture`), which is sound on
//! single-threaded wasm32.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
pub use http::{HeaderMap, Method, StatusCode};
use http::{header, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// A response body as it arrives.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// One request, fully built.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Limit for the whole exchange; `None` keeps the host's default.
    pub timeout: Option<Duration>,
}

/// Status and headers as soon as they arrive; the body streams.
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    /// Final URL, after redirects.
    pub url: String,
    pub body: ByteStream,
}

/// What the host implements. Redirects are followed by the host.
#[async_trait::async_trait]
pub trait Http: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse>;
}

/// Cheap to clone handle on the host's HTTP client.
#[derive(Clone)]
pub struct Client {
    inner: Arc<dyn Http>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

impl Client {
    pub fn new(http: impl Http + 'static) -> Client {
        Client { inner: Arc::new(http) }
    }

    pub fn from_arc(http: Arc<dyn Http>) -> Client {
        Client { inner: http }
    }

    pub fn get(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn request(&self, method: Method, url: impl AsRef<str>) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            req: HttpRequest {
                method,
                url: url.as_ref().to_string(),
                headers: HeaderMap::new(),
                body: None,
                timeout: None,
            },
            err: None,
        }
    }
}

#[must_use]
pub struct RequestBuilder {
    client: Client,
    req: HttpRequest,
    err: Option<anyhow::Error>,
}

impl RequestBuilder {
    /// Add a header (repeats are kept, as with reqwest).
    pub fn header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = name.as_ref();
        match (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value.as_ref())) {
            (Ok(n), Ok(v)) => {
                self.req.headers.append(n, v);
            }
            _ => {
                if self.err.is_none() {
                    self.err = Some(anyhow!("invalid HTTP header {name}"));
                }
            }
        }
        self
    }

    pub fn bearer_auth(self, token: impl std::fmt::Display) -> Self {
        self.header("authorization", format!("Bearer {token}"))
    }

    pub fn basic_auth(self, user: impl std::fmt::Display, password: Option<impl std::fmt::Display>) -> Self {
        let raw = match password {
            Some(p) => format!("{user}:{p}"),
            None => format!("{user}:"),
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        self.header("authorization", format!("Basic {encoded}"))
    }

    /// JSON body; sets `content-type: application/json` unless already set.
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        match serde_json::to_vec(body) {
            Ok(b) => {
                if !self.req.headers.contains_key(header::CONTENT_TYPE) {
                    self.req.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                }
                self.req.body = Some(Bytes::from(b));
            }
            Err(e) => {
                if self.err.is_none() {
                    self.err = Some(anyhow!(e).context("encoding the request body"));
                }
            }
        }
        self
    }

    /// `application/x-www-form-urlencoded` body.
    pub fn form<I, K, V>(mut self, pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in pairs {
            ser.append_pair(k.as_ref(), v.as_ref());
        }
        self.req
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-www-form-urlencoded"));
        self.req.body = Some(Bytes::from(ser.finish()));
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.req.body = Some(body.into());
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.req.timeout = Some(d);
        self
    }

    pub async fn send(self) -> Result<Response> {
        if let Some(e) = self.err {
            return Err(e);
        }
        let inner = self.client.inner.send(self.req).await?;
        Ok(Response { inner })
    }
}

pub struct Response {
    inner: HttpResponse,
}

impl Response {
    pub fn status(&self) -> StatusCode {
        self.inner.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.inner.headers
    }

    pub fn url(&self) -> &str {
        &self.inner.url
    }

    pub fn bytes_stream(self) -> ByteStream {
        self.inner.body
    }

    pub async fn bytes(self) -> Result<Bytes> {
        let mut body = self.inner.body;
        let mut out: Vec<u8> = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(out))
    }

    /// The body as text (invalid UTF-8 is replaced).
    pub async fn text(self) -> Result<String> {
        let b = self.bytes().await?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    pub async fn json<T: DeserializeOwned>(self) -> Result<T> {
        let b = self.bytes().await?;
        Ok(serde_json::from_slice(&b)?)
    }

    /// An error for 4xx and 5xx statuses, worded like reqwest's.
    pub fn error_for_status(self) -> Result<Self> {
        let status = self.inner.status;
        if status.is_client_error() || status.is_server_error() {
            let kind = if status.is_client_error() { "client" } else { "server" };
            bail!("HTTP status {kind} error ({status}) for url ({})", self.inner.url);
        }
        Ok(self)
    }
}

/// The native client: reqwest with rustls.
#[cfg(feature = "native")]
#[derive(Clone, Debug)]
pub struct Reqwest(pub reqwest::Client);

#[cfg(feature = "native")]
#[async_trait::async_trait]
impl Http for Reqwest {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse> {
        let mut b = self.0.request(req.method, &req.url).headers(req.headers);
        if let Some(body) = req.body {
            b = b.body(body);
        }
        if let Some(t) = req.timeout {
            b = b.timeout(t);
        }
        let resp = b.send().await?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let url = resp.url().to_string();
        let body = resp.bytes_stream().map(|c| c.map_err(anyhow::Error::from)).boxed();
        Ok(HttpResponse { status, headers, url, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait::async_trait]
    impl Http for Echo {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse> {
            let mut headers = HeaderMap::new();
            for (k, v) in req.headers.iter() {
                headers.append(k.clone(), v.clone());
            }
            let body = req.body.unwrap_or_default();
            let status = if req.url.ends_with("/missing") { StatusCode::NOT_FOUND } else { StatusCode::OK };
            Ok(HttpResponse {
                status,
                headers,
                url: req.url,
                body: futures_util::stream::iter(vec![Ok(body)]).boxed(),
            })
        }
    }

    #[tokio::test]
    async fn builder_sets_headers_and_bodies() {
        let c = Client::new(Echo);
        let r = c.post("https://x.test/a").bearer_auth("t").json(&serde_json::json!({"a": 1})).send().await.unwrap();
        assert_eq!(r.headers().get("authorization").unwrap(), "Bearer t");
        assert_eq!(r.headers().get("content-type").unwrap(), "application/json");
        assert_eq!(r.text().await.unwrap(), r#"{"a":1}"#);

        let r = c.post("https://x.test/a").basic_auth("id", Some("secret")).form([("a", "b c")]).send().await.unwrap();
        assert_eq!(r.headers().get("authorization").unwrap(), "Basic aWQ6c2VjcmV0");
        assert_eq!(r.text().await.unwrap(), "a=b+c");

        assert!(c.get("https://x.test/a").header("bad header", "v").send().await.is_err());
        let e = c.get("https://x.test/missing").send().await.unwrap().error_for_status().err().unwrap();
        assert!(e.to_string().contains("404 Not Found"), "{e}");
    }
}
