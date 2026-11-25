//! `xerj mcp --http` (#3066): auth, Origin, tool scope and parity with stdio,
//! against a fake node. Every listener here is loopback.
//!
//! Own test binary: the refusal test clears `XERJ_MCP_TOKEN`, which is
//! process-global.

use std::sync::{Arc, Mutex};

use axum::{routing::any, Json, Router};
use serde_json::{json, Value};

const TOKEN: &str = "0123456789abcdef-test-token";

/// A fake node: answers every `_search` with one fixed hit, every mapping
/// with a code index, and records the paths it was asked for.
async fn fake_node() -> (String, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let app = Router::new().fallback(any(
        move |uri: axum::http::Uri, method: axum::http::Method, body: String| {
            log.lock()
                .unwrap()
                .push(format!("{method} {}", uri.path()));
            async move {
                let path = uri.path().to_string();
                if path.ends_with("/_search") {
                    let hits = if body.contains("activeDialog") {
                        json!([{"_index":"p-docs","_id":"1","_source":{
                            "ax_path":"src/app.js","start_line":1,
                            "body":"function activeDialog() {}\nfunction open() {\n  ActiveUtils.activeDialog();\n}\n",
                            "symbols":[
                                {"name":"activeDialog","kind":"function","line":1,"end_line":1},
                                {"name":"open","kind":"function","line":2,"end_line":4}]}}])
                    } else {
                        json!([{"_index":"docs","_id":"1","_source":{"body":"retry policy"}}])
                    };
                    let n = hits.as_array().map_or(0, Vec::len);
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"took":1,"hits":{"total":{"value":n,"relation":"eq"},"hits":hits}})),
                    )
                } else if path.ends_with("/_mapping") {
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"p-docs":{"mappings":{"properties":{"body":{"type":"text"}}}}})),
                    )
                } else if path.ends_with("/_analyze") {
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"tokens":[{"token":"activedialog"}]})),
                    )
                } else if path.contains("/_brain/") {
                    (axum::http::StatusCode::OK, Json(json!({"acknowledged":true})))
                } else {
                    (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"not here"})))
                }
            }
        },
    ));
    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

/// Start `/mcp` over HTTP on an ephemeral loopback port.
async fn mcp(node: &str, opts: xerj_mcp::HttpOptions) -> String {
    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let node = node.to_string();
    tokio::spawn(async move {
        xerj_mcp::serve_http_for_test(listener, &node, None, opts)
            .await
            .unwrap()
    });
    url
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn with_token() -> xerj_mcp::HttpOptions {
    xerj_mcp::HttpOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    }
}

async fn post(
    url: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (u16, reqwest::header::HeaderMap, String) {
    let mut req = client()
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string());
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    (status, headers, resp.text().await.unwrap())
}

fn list() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
}

fn names(reply: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(reply).unwrap();
    v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

const BEARER: (&str, &str) = ("Authorization", "Bearer 0123456789abcdef-test-token");

#[tokio::test]
async fn missing_and_wrong_tokens_get_401_and_the_right_one_gets_through() {
    let (node, _) = fake_node().await;
    let url = mcp(&node, with_token()).await;

    let (status, headers, body) = post(&url, &[], list()).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(headers["www-authenticate"], "Bearer realm=\"xerj-mcp\"");
    assert!(!body.contains(TOKEN), "the token is never echoed: {body}");

    let (status, _, _) = post(
        &url,
        &[("Authorization", "Bearer wrong-token-of-any-len")],
        list(),
    )
    .await;
    assert_eq!(status, 401);
    let (status, _, _) = post(
        &url,
        &[("Authorization", &format!("ApiKey {TOKEN}"))],
        list(),
    )
    .await;
    assert_eq!(status, 401, "only the Bearer scheme carries the token");

    let (status, _, body) = post(&url, &[BEARER], list()).await;
    assert_eq!(status, 200, "{body}");
    assert!(names(&body).contains(&"xerj_search".to_string()));
}

#[tokio::test]
async fn a_foreign_origin_is_refused_and_loopback_or_listed_origins_pass() {
    let (node, _) = fake_node().await;
    let url = mcp(
        &node,
        xerj_mcp::HttpOptions {
            allowed_origins: vec!["https://chat.example.com".into()],
            ..with_token()
        },
    )
    .await;
    let (status, _, body) = post(&url, &[BEARER, ("Origin", "https://evil.example")], list()).await;
    assert_eq!(status, 403, "{body}");
    // Refused before auth: a rebinding page learns nothing about the token.
    let (status, _, _) = post(&url, &[("Origin", "https://evil.example")], list()).await;
    assert_eq!(status, 403);
    for ok in ["http://localhost:5173", "https://chat.example.com"] {
        let (status, _, body) = post(&url, &[BEARER, ("Origin", ok)], list()).await;
        assert_eq!(status, 200, "{ok}: {body}");
    }
}

#[tokio::test]
async fn without_a_token_a_tunnelled_request_is_refused() {
    let (node, _) = fake_node().await;
    let url = mcp(&node, xerj_mcp::HttpOptions::default()).await;
    let (status, _, _) = post(&url, &[], list()).await;
    assert_eq!(status, 200, "loopback, no token, no proxy header: served");
    for header in [
        "X-Forwarded-For",
        "Forwarded",
        "CF-Connecting-IP",
        "Tailscale-User-Login",
    ] {
        let (status, _, body) = post(&url, &[(header, "203.0.113.9")], list()).await;
        assert_eq!(status, 403, "{header}: {body}");
        assert!(body.contains("XERJ_MCP_TOKEN"), "{body}");
    }
}

#[tokio::test]
async fn http_lists_read_only_tools_by_default_and_every_tool_with_allow_writes() {
    let (node, _) = fake_node().await;
    let read_only = mcp(&node, with_token()).await;
    let (_, _, body) = post(&read_only, &[BEARER], list()).await;
    let served = names(&body);
    let full: Vec<String> = xerj_mcp::tool_specs()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let expected: Vec<String> = full
        .iter()
        .filter(|n| xerj_mcp::HTTP_READ_ONLY_TOOLS.contains(&n.as_str()))
        .cloned()
        .collect();
    assert_eq!(served, expected);
    for writer in [
        "xerj_memory_store",
        "xerj_brain_link",
        "xerj_autoindex_remove",
        "xerj_field_report",
    ] {
        assert!(!served.contains(&writer.to_string()), "{writer}");
    }
    assert!(served.contains(&"xerj_references".to_string()));

    let all = mcp(
        &node,
        xerj_mcp::HttpOptions {
            allow_writes: true,
            ..with_token()
        },
    )
    .await;
    let (_, _, body) = post(&all, &[BEARER], list()).await;
    assert_eq!(names(&body), full, "--allow-writes serves the stdio list");
}

#[tokio::test]
async fn a_write_tool_is_refused_over_http_unless_allowed() {
    let (node, seen) = fake_node().await;
    let call = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
        "name":"xerj_brain_link","arguments":{"brain":"b","from":"a","to":"c","relation":"cites","evidence":"e"}}});
    let url = mcp(&node, with_token()).await;
    let (status, _, body) = post(&url, &[BEARER], call.clone()).await;
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["result"]["isError"], true);
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("not served over HTTP") && text.contains("--allow-writes"),
        "{text}"
    );
    assert!(
        seen.lock().unwrap().iter().all(|p| !p.contains("_brain")),
        "the refused call never reached the node"
    );

    let open = mcp(
        &node,
        xerj_mcp::HttpOptions {
            allow_writes: true,
            ..with_token()
        },
    )
    .await;
    let (_, _, body) = post(&open, &[BEARER], call).await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_ne!(
        v["result"]["content"][0]["text"].as_str().unwrap_or(""),
        text,
        "with --allow-writes the call is dispatched: {body}"
    );
}

#[tokio::test]
async fn a_tool_call_over_http_equals_the_same_call_over_stdio() {
    let (node, _) = fake_node().await;
    let url = mcp(&node, with_token()).await;
    for call in [
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"xerj_search","arguments":{"index":"docs","query":{"match":{"body":"retry"}}}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"xerj_references","arguments":{"index":"p-docs","symbol":"activeDialog"}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"ping"}),
    ] {
        let stdio = xerj_mcp::handle_message_for_test(&node, None, call.clone())
            .await
            .unwrap();
        let (status, _, body) = post(&url, &[BEARER], call).await;
        assert_eq!(status, 200, "{body}");
        let http: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(http, stdio);
    }
    // The references result itself: the call site, not the definition.
    let stdio = xerj_mcp::handle_message_for_test(
        &node,
        None,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"xerj_references","arguments":{"index":"p-docs","symbol":"activeDialog"}}}),
    )
    .await
    .unwrap();
    let out: Value =
        serde_json::from_str(stdio["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(out["total"], 1, "{out}");
    assert_eq!(out["references"][0]["path"], "src/app.js");
    assert_eq!(out["references"][0]["line"], 3);
    assert_eq!(out["references"][0]["enclosing"]["name"], "open");
    assert_eq!(out["definitions"][0]["line"], 1);
}

#[tokio::test]
async fn notifications_get_202_batches_an_array_and_get_405() {
    let (node, _) = fake_node().await;
    let url = mcp(&node, with_token()).await;
    let (status, _, body) = post(
        &url,
        &[BEARER],
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!((status, body.as_str()), (202, ""));
    let (status, _, body) = post(
        &url,
        &[BEARER],
        json!([{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"}]),
    )
    .await;
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    let resp = client()
        .get(&url)
        .header(BEARER.0, BEARER.1)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 405);
    assert_eq!(resp.headers()["allow"], "POST");
    let resp = client()
        .post(&url)
        .header(BEARER.0, BEARER.1)
        .header("Content-Type", "text/plain")
        .body(list().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415);
    let (status, _, body) = post(&url, &[BEARER], json!("not json-rpc")).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("-32600"), "{body}");
    let resp = client()
        .post(&url)
        .header(BEARER.0, BEARER.1)
        .header("Content-Type", "application/json")
        .body("{broken")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], -32700);
}

#[tokio::test]
async fn a_non_loopback_bind_without_a_token_refuses_to_start() {
    std::env::remove_var("XERJ_MCP_TOKEN");
    let args: Vec<String> = ["--http", "--bind", "0.0.0.0:0", "--no-autostart"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), xerj_mcp::run(&args))
        .await
        .expect("run() must refuse at once, not start serving");
    let err = outcome.expect_err("a 0.0.0.0 bind without a token must not start");
    let msg = err.to_string();
    assert!(msg.contains("without a bearer token"), "{msg}");
    assert!(msg.contains("XERJ_MCP_TOKEN"), "{msg}");

    // An empty token file is an error, not "no auth" on loopback.
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("token");
    std::fs::write(&empty, "  \n").unwrap();
    let args: Vec<String> = vec![
        "--http".into(),
        "--bind".into(),
        "127.0.0.1:0".into(),
        "--token-file".into(),
        empty.display().to_string(),
    ];
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), xerj_mcp::run(&args))
        .await
        .expect("run() must refuse at once");
    let msg = outcome.expect_err("empty token file").to_string();
    assert!(msg.contains("is empty"), "{msg}");

    for flag in [
        &["--allow-writes"][..],
        &["--bind", "127.0.0.1:0"],
        &["--token-file", "x"],
    ] {
        let args: Vec<String> = flag.iter().map(|s| s.to_string()).collect();
        let err = xerj_mcp::run(&args)
            .await
            .expect_err("HTTP flag without --http");
        assert!(err.to_string().contains("add --http"), "{err}");
    }
}
