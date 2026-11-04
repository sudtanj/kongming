//! Outbound HTTP that must stay off private networks: the model-driven
//! `web_fetch`, skill imports and the shell's `curl`/`wget`. A request is
//! checked twice: the URL itself (IP literals, `localhost`) and, for names,
//! every address the resolver returns at connect time, so a name pointing at a
//! private address, a rebinding answer and a redirect hop are all covered.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use url::{Host, Url};

/// Part of every error this module raises, so callers can recognise a refusal.
pub const BLOCKED: &str = "non-public address";
const MAX_REDIRECTS: usize = 5;

fn public_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_documentation()
        || o[0] == 0
        || o[0] >= 224
        || (o[0] == 100 && (64..128).contains(&o[1])))
}

/// Whether `ip` is a routable public address. IPv4 inside IPv6 (`::ffff:a.b.c.d`,
/// NAT64) is judged as the IPv4 address.
pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return public_v4(&v4);
            }
            let s = v6.segments();
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return public_v4(&Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

/// The part of the check that needs no lookup: scheme, IP literals and
/// names that are local by definition.
pub fn check_url(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http(s) urls are allowed");
    }
    let blocked = match url.host() {
        None => bail!("the url has no host"),
        Some(Host::Ipv4(ip)) => !is_public_ip(&ip.into()),
        Some(Host::Ipv6(ip)) => !is_public_ip(&ip.into()),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local")
        }
    };
    if blocked {
        bail!("refusing to connect to a {BLOCKED}");
    }
    Ok(())
}

/// Resolves names like the system does but fails when any answer is not public.
struct PublicOnly;

async fn public_addrs(name: &str) -> Result<Addrs, Box<dyn std::error::Error + Send + Sync>> {
    let found = tokio::time::timeout(Duration::from_secs(10), tokio::net::lookup_host((name, 0))).await??;
    let addrs: Vec<SocketAddr> = found.collect();
    if addrs.is_empty() || addrs.iter().any(|a| !is_public_ip(&a.ip())) {
        return Err(format!("{name} resolves to a {BLOCKED}").into());
    }
    Ok(Box::new(addrs.into_iter()))
}

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move { public_addrs(name.as_str()).await })
    }
}

/// Follow at most `max` redirects, each to a URL that passes `check_url`.
pub fn redirect_policy(max: usize) -> Policy {
    Policy::custom(move |attempt| {
        if attempt.previous().len() > max {
            return attempt.error("too many redirects");
        }
        match check_url(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(e.to_string()),
        }
    })
}

/// The builder with name resolution restricted to public addresses and no
/// proxy (a proxy would resolve names where this cannot see them).
pub fn guard(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    b.dns_resolver(Arc::new(PublicOnly)).no_proxy()
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        guard(reqwest::Client::builder())
            .redirect(redirect_policy(MAX_REDIRECTS))
            .connect_timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(0)
            .use_rustls_tls()
            .user_agent(concat!("kongming/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client")
    })
}

/// Whether a request error was this module refusing an address.
pub fn refused(e: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = source {
        if s.to_string().contains(BLOCKED) {
            return true;
        }
        source = s.source();
    }
    false
}

/// GET a public URL, following up to five redirects that are each checked.
pub async fn get(url: Url, accept: Option<&str>, timeout: Duration) -> Result<reqwest::Response> {
    check_url(&url)?;
    let mut req = client().get(url).timeout(timeout);
    if let Some(a) = accept {
        req = req.header("accept", a);
    }
    req.send().await.map_err(|e| if refused(&e) { anyhow!("refusing to fetch a private address") } else { e.into() })
}
