//! Loopback MCP calls ignore `ALL_PROXY` / `HTTP_PROXY` / `HTTPS_PROXY` (#2379).
//!
//! reqwest honours those variables for `127.0.0.1`. A socks or HTTP proxy then
//! answers 503 (or refuses the connection) while the node is healthy, and the
//! tool used to report that as `XERJ returned HTTP 503`. `curl --noproxy`
//! reached the same node.
//!
//! This is its own test binary because the proxy variables are process-global:
//! set inside the library tests they would redirect every other HTTP client.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

const PROXY_VARS: &[&str] = &[
    "ALL_PROXY",
    "all_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
];

struct EnvGuard(Vec<(String, Option<std::ffi::OsString>)>);

impl EnvGuard {
    fn capture() -> Self {
        let saved = PROXY_VARS
            .iter()
            .map(|key| ((*key).to_string(), std::env::var_os(key)))
            .collect();
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for key in PROXY_VARS {
            std::env::remove_var(key);
        }
        for (key, value) in &self.0 {
            if let Some(value) = value {
                std::env::set_var(key, value);
            }
        }
    }
}

fn clear_proxy_env() {
    for key in PROXY_VARS {
        std::env::remove_var(key);
    }
}

fn set_proxy(value: &str) {
    clear_proxy_env();
    for key in [
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
    ] {
        std::env::set_var(key, value);
    }
}

/// A server on an ephemeral port that answers every request with `status`/`body`
/// and counts the requests it received. `bind` is `127.0.0.1:0` or `[::1]:0`.
/// `None` when that address family is not available.
fn serve(
    bind: &str,
    status: &'static str,
    body: &'static str,
    hits: Arc<AtomicUsize>,
) -> Option<u16> {
    let listener = TcpListener::bind(bind).ok()?;
    let port = listener.local_addr().ok()?.port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let hits = hits.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                if conn.read(&mut buf).unwrap_or(0) == 0 {
                    return;
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
    Some(port)
}

fn closed_port() -> u16 {
    let listener = xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"));
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn search_call() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "xerj_search",
            "arguments": { "index": "docs", "query": { "match_all": {} } }
        }
    })
}

fn map_call() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "xerj_map", "arguments": {} }
    })
}

async fn tool(url: &str, msg: Value) -> (bool, String) {
    let resp = xerj_mcp::handle_message_for_test(url, None, msg)
        .await
        .expect("tools/call returns a response");
    let is_error = resp["result"]["isError"].as_bool().unwrap();
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    (is_error, text)
}

#[tokio::test]
async fn loopback_ignores_an_unreachable_proxy_and_a_proxied_503_names_the_proxy() {
    // Phase 3 connects to 127.0.0.2 directly. Stock macOS has no such alias,
    // and the connection error alone does not say so (#2903).
    xerj_common::loopback_test::require_loopback_alias(std::net::Ipv4Addr::new(127, 0, 0, 2));
    let _guard = EnvGuard::capture();

    let node_hits = Arc::new(AtomicUsize::new(0));
    let node_body = r#"{"took":1,"hits":{"total":{"value":0,"relation":"eq"},"hits":[]}}"#;
    let node_port =
        serve("127.0.0.1:0", "200 OK", node_body, node_hits.clone()).expect("bind 127.0.0.1");
    let v6_port = serve("[::1]:0", "200 OK", node_body, node_hits.clone());

    // Phase 1: ALL_PROXY is an unreachable socks5 proxy, and the HTTP(S) proxy
    // variables point at a closed port. A loopback tool call still succeeds.
    let closed = closed_port();
    set_proxy(&format!("socks5://127.0.0.1:{closed}"));
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{closed}"));
    std::env::set_var("http_proxy", format!("http://127.0.0.1:{closed}"));
    std::env::set_var("HTTPS_PROXY", format!("http://127.0.0.1:{closed}"));
    std::env::set_var("https_proxy", format!("http://127.0.0.1:{closed}"));

    let node = format!("http://127.0.0.1:{node_port}");
    let (err, text) = tool(&node, search_call()).await;
    assert!(!err, "loopback search went through the proxy: {text}");
    let (err, text) = tool(&node, map_call()).await;
    assert!(!err, "loopback map went through the proxy: {text}");
    assert!(
        node_hits.load(Ordering::SeqCst) >= 2,
        "the node itself must have answered"
    );
    if let Some(port) = v6_port {
        let (err, text) = tool(&format!("http://[::1]:{port}"), search_call()).await;
        assert!(!err, "::1 search went through the proxy: {text}");
    }

    // Phase 2: a live HTTP proxy returns 503. Loopback still skips it. A
    // non-loopback host is sent there, and the tool error names the proxy
    // instead of `XERJ returned HTTP 503`.
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let proxy_port = serve(
        "127.0.0.1:0",
        "503 Service Unavailable",
        r#"{"error":"bad gateway from the corporate proxy"}"#,
        proxy_hits.clone(),
    )
    .expect("bind proxy");
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    set_proxy(&proxy_url);

    let before = node_hits.load(Ordering::SeqCst);
    let (err, text) = tool(&node, search_call()).await;
    assert!(!err, "loopback search used the live proxy: {text}");
    let (err, text) = tool(&node, map_call()).await;
    assert!(!err, "loopback map used the live proxy: {text}");
    assert!(node_hits.load(Ordering::SeqCst) > before);
    assert_eq!(
        proxy_hits.load(Ordering::SeqCst),
        0,
        "the proxy must not see a loopback node"
    );

    // 127.0.0.2 is this machine's loopback range, but it is not one of the
    // hosts `url_is_loopback` treats as the local node, so the proxy applies.
    let remote = "http://127.0.0.2:9";
    let (err, text) = tool(remote, search_call()).await;
    assert!(err, "{text}");
    assert!(
        text.contains(&proxy_url),
        "the 503 must name the proxy: {text}"
    );
    assert!(text.contains("HTTP 503"), "{text}");
    assert!(text.contains("went through proxy"), "{text}");
    assert!(
        !text.contains("XERJ returned HTTP"),
        "a proxy 503 is not the node's status: {text}"
    );
    let (err, text) = tool(remote, map_call()).await;
    assert!(err, "{text}");
    assert!(text.contains(&proxy_url), "{text}");
    assert!(text.contains("went through proxy"), "{text}");
    assert!(
        proxy_hits.load(Ordering::SeqCst) >= 2,
        "the non-loopback calls must have reached the proxy"
    );

    // Phase 3: NO_PROXY still excludes a non-loopback host. The closed port
    // refuses immediately, and the error is the node-down sentence, not the proxy.
    let proxy_before = proxy_hits.load(Ordering::SeqCst);
    std::env::set_var("NO_PROXY", "127.0.0.2");
    std::env::set_var("no_proxy", "127.0.0.2");
    let (err, text) = tool(remote, search_call()).await;
    assert!(err, "{text}");
    assert!(
        !text.contains("went through proxy"),
        "NO_PROXY must bypass the proxy: {text}"
    );
    assert!(
        text.contains("no XERJ node is listening") || text.contains("Connection refused"),
        "{text}"
    );
    assert_eq!(
        proxy_hits.load(Ordering::SeqCst),
        proxy_before,
        "NO_PROXY must not contact the proxy"
    );

    // Phase 4: a proxy that is not listening is named on the transport error,
    // rather than reported as the node being down.
    clear_proxy_env();
    let dead = closed_port();
    let dead_url = format!("http://127.0.0.1:{dead}");
    set_proxy(&dead_url);
    let (err, text) = tool(remote, search_call()).await;
    assert!(err, "{text}");
    assert!(
        text.contains(&dead_url),
        "a refused proxy connection must name the proxy: {text}"
    );
    assert!(text.contains("via proxy"), "{text}");
    assert!(
        !text.contains("no XERJ node is listening"),
        "the node was not the thing that refused: {text}"
    );
}
