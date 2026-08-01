//! #76 S5-4 — `x-forwarded-for` must not be trusted for client identity.
//!
//! The per-IP auth rate limiter used to key on `x-forwarded-for`, a header the
//! caller writes. A rotating `x-forwarded-for: 1.2.3.$RANDOM` therefore reset
//! the quota on every request (unthrottled brute force of the magic-link /
//! bootstrap-claim endpoints) while honest clients, who send no such header,
//! stayed throttled.
//!
//! These drive the *real* router — `ConnectInfo` supplied exactly as the axum
//! listener supplies it — and assert both halves of the fix:
//!
//!  - an untrusted peer's forwarded header changes nothing;
//!  - a peer the operator declared in `server.trusted_proxies` has its
//!    forwarded address honoured, so real users behind a proxy still get one
//!    bucket each.

use std::net::SocketAddr;

use axum::{body::Body, extract::ConnectInfo, http::Request, Router};
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::{config::Config, net::TrustedProxies};
use xerj_console_api::{state::ClusterMode, xerj_console_router, ConsoleState};
use xerj_engine::Engine;

/// PER_MINUTE in `auth/rate_limit.rs`.
const PER_MINUTE: usize = 10;

const LOGIN_BEGIN: &str = "/_xerj-console/api/v1/auth/login/begin";

async fn boot(trusted: &[&str]) -> (Router, TempDir) {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .expect("bootstrap");
    let trusted =
        TrustedProxies::parse(&trusted.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    let state = ConsoleState::new(
        engine,
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    )
    .with_trusted_proxies(trusted);
    (xerj_console_router(state), dir)
}

/// One `POST /auth/login/begin` from `peer`, optionally carrying `xff`.
/// `ConnectInfo` is inserted the same way axum's
/// `into_make_service_with_connect_info` inserts it.
async fn login_begin(router: &Router, peer: &str, xff: Option<&str>) -> u16 {
    let peer: SocketAddr = peer.parse().expect("peer must be addr:port");
    let mut req = Request::builder()
        .method("POST")
        .uri(LOGIN_BEGIN)
        .header("content-type", "application/json");
    if let Some(xff) = xff {
        req = req.header("x-forwarded-for", xff);
    }
    let mut req = req
        .body(Body::from(json!({ "email": "x@y" }).to_string()))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(peer));

    router.clone().oneshot(req).await.unwrap().status().as_u16()
}

/// The exploit, verbatim: a rotating forwarded header from one untrusted
/// caller. Every request must land in the *same* bucket, so the limiter fires
/// on schedule instead of never.
#[tokio::test]
async fn rotating_forwarded_header_cannot_refresh_the_quota() {
    let (router, _dir) = boot(&[]).await;

    let mut statuses = Vec::new();
    for n in 0..PER_MINUTE + 5 {
        statuses.push(login_begin(&router, "203.0.113.9:44321", Some(&format!("1.2.3.{n}"))).await);
    }

    assert!(
        statuses[..PER_MINUTE].iter().all(|s| *s == 200),
        "first {PER_MINUTE} should pass: {statuses:?}"
    );
    assert!(
        statuses[PER_MINUTE..].iter().all(|s| *s == 429),
        "a rotating x-forwarded-for must NOT mint a fresh quota: {statuses:?}"
    );
}

/// …and the header cannot be used to frame someone else either: an exhausted
/// attacker cannot spend a victim's quota, and a victim's socket is unaffected
/// by whatever the attacker claimed to be.
#[tokio::test]
async fn untrusted_peer_cannot_move_its_bucket_onto_another_address() {
    let (router, _dir) = boot(&[]).await;

    for _ in 0..PER_MINUTE {
        assert_eq!(login_begin(&router, "203.0.113.9:1000", None).await, 200);
    }
    assert_eq!(
        login_begin(&router, "203.0.113.9:1000", Some("198.51.100.23")).await,
        429,
        "spoofing another address must not buy a fresh quota"
    );
    assert_eq!(
        login_begin(&router, "198.51.100.23:1000", None).await,
        200,
        "the spoofed victim's own bucket must be untouched"
    );
}

/// A different socket peer is a different bucket — the limiter still does its
/// job for distinct clients rather than collapsing everyone together.
#[tokio::test]
async fn distinct_peers_have_distinct_buckets() {
    let (router, _dir) = boot(&[]).await;

    for _ in 0..PER_MINUTE {
        assert_eq!(login_begin(&router, "203.0.113.9:1000", None).await, 200);
    }
    assert_eq!(login_begin(&router, "203.0.113.9:1000", None).await, 429);
    assert_eq!(
        login_begin(&router, "203.0.113.10:1000", None).await,
        200,
        "a different peer must not inherit the exhausted bucket"
    );
    // Ports are not identity: the same host on a new connection is the same
    // bucket, or one client could just reconnect for a fresh quota.
    assert_eq!(login_begin(&router, "203.0.113.9:2000", None).await, 429);
}

/// Behind a declared proxy the peer is always the proxy, so the forwarded
/// address must be honoured — otherwise every user collapses into one bucket
/// and ten logins company-wide per minute locks everybody out.
#[tokio::test]
async fn trusted_peer_has_its_forwarded_address_honoured() {
    let (router, _dir) = boot(&["10.0.0.7"]).await;

    for _ in 0..PER_MINUTE {
        assert_eq!(
            login_begin(&router, "10.0.0.7:1000", Some("198.51.100.23")).await,
            200
        );
    }
    assert_eq!(
        login_begin(&router, "10.0.0.7:1000", Some("198.51.100.23")).await,
        429,
        "the forwarded client must be throttled on its own bucket"
    );
    assert_eq!(
        login_begin(&router, "10.0.0.7:1000", Some("198.51.100.24")).await,
        200,
        "a different forwarded client must have its own bucket"
    );
}

/// The chain is appended left-to-right, so the left end is whatever the caller
/// sent. Only the right-most entry was written by our own proxy. A client that
/// pre-seeds `x-forwarded-for` must not be able to pick its own bucket.
#[tokio::test]
async fn attacker_prefix_in_the_chain_is_ignored() {
    let (router, _dir) = boot(&["10.0.0.7"]).await;

    for n in 0..PER_MINUTE {
        // Attacker rotates the part *they* control; the proxy appends the
        // truth on the right.
        assert_eq!(
            login_begin(
                &router,
                "10.0.0.7:1000",
                Some(&format!("9.9.9.{n}, 198.51.100.23"))
            )
            .await,
            200
        );
    }
    assert_eq!(
        login_begin(&router, "10.0.0.7:1000", Some("9.9.9.250, 198.51.100.23")).await,
        429,
        "the caller-authored left end must not select the bucket"
    );
}

/// Trusting one proxy does not make every peer trustworthy.
#[tokio::test]
async fn configuring_a_proxy_does_not_trust_other_peers() {
    let (router, _dir) = boot(&["10.0.0.7"]).await;

    for n in 0..PER_MINUTE {
        assert_eq!(
            login_begin(&router, "203.0.113.9:1000", Some(&format!("1.2.3.{n}"))).await,
            200
        );
    }
    assert_eq!(
        login_begin(&router, "203.0.113.9:1000", Some("1.2.3.99")).await,
        429,
        "a peer outside trusted_proxies is still keyed on its socket"
    );
}
