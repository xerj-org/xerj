//! Real installed-shape CLI and MCP must return the same batch contract.
use std::process::Stdio;

use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn cli_stdin_file_and_mcp_agree_and_item_errors_have_exit_three() {
    let app = Router::new().route(
        "/_msearch",
        post(|| async {
            Json(json!({"responses":[
                {"hits":{"hits":[{"_index":"docs","_id":"1","_source":{"body":"evidence"}}]}},
                {"error":{"reason":"bad query"},"status":400}
            ]}))
        }),
    );
    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let input = json!({"index":"docs","queries":[{"query":{"match_all":{}}},{"query":{"bad":{}}}]});
    let mcp = xerj_mcp::handle_message_for_test(
        &url,
        None,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"xerj_batch_search","arguments":input}}),
    )
    .await
    .unwrap();
    let expected: Value =
        serde_json::from_str(mcp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queries.json");
    std::fs::write(&path, input.to_string()).unwrap();
    for file in ["-".to_string(), path.to_string_lossy().into_owned()] {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_xerj"))
            .args([
                "--url",
                &url,
                "batch-search",
                "--input",
                &file,
                "--disable-feedback",
            ])
            .env("XERJ_DISABLE_FEEDBACK", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        if file == "-" {
            stdin.write_all(input.to_string().as_bytes()).await.unwrap();
        }
        drop(stdin);
        let out = child.wait_with_output().await.unwrap();
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap(),
            expected
        );
    }
    server.abort();
    let _ = server.await;
}

#[test]
fn cli_help_and_malformed_input_never_start_a_node() {
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["batch-search", "--help", "--disable-feedback"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("1..50"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.json");
    std::fs::write(&path, r#"{"queries":[]}"#).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args([
            "batch-search",
            "--input",
            path.to_str().unwrap(),
            "--url",
            "http://127.0.0.1:1",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("1..50"));
}
