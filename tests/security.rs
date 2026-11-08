//! Pure-logic checks for the outbound address guard, the cross-site guard,
//! the login lockout and the lead approval rule.

use axum::http::{HeaderMap, HeaderValue};
use kongming::db::EarnLead;
use kongming::earn::{may_write, Settings};
use kongming::net::{check_url, is_public_ip};
use kongming::web::{names_this_host, origin_authority};

fn public(ip: &str) -> bool {
    is_public_ip(&ip.parse().unwrap())
}

#[test]
fn private_and_mapped_addresses_are_not_public() {
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "100.64.0.1",
        "169.254.169.254",
        "0.0.0.0",
        "255.255.255.255",
        "192.0.2.1",
        "::1",
        "::",
        "fe80::1",
        "fd00::1",
        "::ffff:169.254.169.254",
        "::ffff:10.0.0.1",
        "64:ff9b::a00:1",
    ] {
        assert!(!public(ip), "{ip}");
    }
    for ip in ["1.1.1.1", "8.8.8.8", "100.128.0.1", "2606:4700:4700::1111", "::ffff:1.1.1.1"] {
        assert!(public(ip), "{ip}");
    }
}

#[test]
fn urls_are_checked_by_host_not_by_text() {
    for u in [
        "http://[::1]/",
        "http://[::ffff:169.254.169.254]/latest",
        "http://127.1/",
        "http://2130706433/",
        "http://localhost./",
        "http://printer.local/",
        "ftp://example.com/",
    ] {
        assert!(check_url(&u.parse().unwrap()).is_err(), "{u}");
    }
    assert!(check_url(&"https://example.com/a".parse().unwrap()).is_ok());
}

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, HeaderValue::from_static(v));
    }
    h
}

#[test]
fn origin_must_name_the_host_it_was_sent_to() {
    let same = headers(&[("host", "127.0.0.1:18800"), ("origin", "http://127.0.0.1:18800")]);
    let origin = origin_authority(&same).unwrap();
    assert!(names_this_host(&same, &origin));

    let other = headers(&[("host", "127.0.0.1:18800"), ("origin", "https://evil.example")]);
    assert!(!names_this_host(&other, &origin_authority(&other).unwrap()));

    let referer = headers(&[("host", "kong.example"), ("referer", "https://kong.example/#/chat")]);
    assert!(names_this_host(&referer, &origin_authority(&referer).unwrap()));

    let null = headers(&[("host", "kong.example"), ("origin", "null")]);
    assert!(!names_this_host(&null, &origin_authority(&null).unwrap()));

    let proxied = headers(&[("host", "127.0.0.1:18800"), ("x-forwarded-host", "kong.example")]);
    assert!(names_this_host(&proxied, "kong.example"));

    assert!(origin_authority(&headers(&[("host", "x")])).is_none(), "no Origin or Referer: not a browser page");
}

#[test]
fn working_and_submitted_need_the_same_approval_as_a_write() {
    let mut lead = EarnLead { id: "abcdef123".into(), status: "shortlisted".into(), ..Default::default() };
    let s = Settings::default();
    assert!(may_write(&s, &lead).is_err());
    lead.status = "approved".into();
    assert!(may_write(&s, &lead).is_ok());
}

#[test]
fn logins_lock_out_after_repeated_failures() {
    use kongming::web::auth::{login_failed, login_succeeded, login_wait_ms};
    login_succeeded();
    for _ in 0..4 {
        login_failed();
    }
    assert_eq!(login_wait_ms(), 0);
    login_failed();
    assert!(login_wait_ms() > 0);
    login_succeeded();
    assert_eq!(login_wait_ms(), 0);
}
