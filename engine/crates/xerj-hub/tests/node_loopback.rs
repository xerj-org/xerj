//! A loopback node is never reached through a proxy (review finding E8).
//!
//! `HTTP_PROXY` is how a corporate machine gets *out*. The health probe used a
//! default `reqwest` client, which honours it for `127.0.0.1` too: the probe of the
//! silent loopback URL went to the proxy, the proxy's `502 Bad Gateway` page counted
//! as "an HTTP answer, so a node is running", and `xerj ask` declined to start the
//! node it needed — or, with a healthy node, talked to the proxy instead of it.
//!
//! This is its own test binary because `HTTP_PROXY` is process-global: set in the
//! unit-test binary it would redirect every other test's HTTP client.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use xerj_hub::node::{self, HubNode};

/// A server on an ephemeral loopback port that answers every request with
/// `status`/`body` and counts the requests it received.
fn serve(status: &'static str, body: &'static str, hits: Arc<AtomicUsize>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let hits = hits.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                if conn.read(&mut buf).unwrap_or(0) == 0 {
                    return; // a connect with no request (a port probe)
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(reply.as_bytes());
            });
        }
    });
    port
}

#[test]
fn a_loopback_node_is_reached_directly_and_a_remote_one_still_goes_through_the_proxy() {
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let node_hits = Arc::new(AtomicUsize::new(0));
    let proxy = serve(
        "502 Bad Gateway",
        r#"{"error":"bad gateway from the corporate proxy"}"#,
        proxy_hits.clone(),
    );
    let node_port = serve("200 OK", r#"{"status":"green"}"#, node_hits.clone());

    // A corporate box: a proxy for everything, and no NO_PROXY that covers loopback.
    for var in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(var);
    }
    for var in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        std::env::set_var(var, format!("http://127.0.0.1:{proxy}"));
    }

    // The node is probed directly.
    let url = format!("http://127.0.0.1:{node_port}");
    let health = node::probe(&url, Duration::from_secs(3)).expect("the node answers");
    assert_eq!(health.http_status, 200);
    assert_eq!(health.status.as_deref(), Some("green"));
    assert_eq!(
        proxy_hits.load(Ordering::SeqCst),
        0,
        "the proxy must never be asked about a loopback node"
    );
    assert!(node_hits.load(Ordering::SeqCst) >= 1);

    // So is every request the client makes to it.
    let client = HubNode::connect_insecure(&url).unwrap();
    client
        .wait_until_reachable(Duration::from_secs(3))
        .expect("reachable");
    client.request_ok("GET", "/_cluster/health", None).unwrap();
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 0);

    // A loopback port with NOTHING on it is silent — it is not "the proxy said 502
    // so a node answers", which is what made the hub decline to start its own.
    let silent = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(
        node::probe(
            &format!("http://127.0.0.1:{silent}"),
            Duration::from_secs(2)
        )
        .is_none(),
        "nothing listens there, so nothing answers"
    );
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 0);

    // A remote node keeps the system proxy settings: someone behind a corporate
    // proxy reaching a node elsewhere still works.
    let remote = node::probe("http://node.example.invalid:9200", Duration::from_secs(3))
        .expect("the proxy answered for it");
    assert_eq!(remote.http_status, 502);
    assert!(proxy_hits.load(Ordering::SeqCst) >= 1);
}
