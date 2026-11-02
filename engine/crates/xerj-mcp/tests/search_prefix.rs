//! A custom autoindex prefix must not fall back to the default corpus.
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefix_search_returns_only_its_corpus_for_plain_and_dsl_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = tmp.path().join("data").to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.auth.enabled = false;
    let metrics = xerj_common::metrics::Metrics::new().unwrap();
    let engine = xerj_engine::Engine::new(config.clone()).unwrap();
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state);
    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    for index in ["project-docs", "ax-other", "my-proj-docs"] {
        let response = client
            .put(format!("{url}/{index}"))
            .json(
                &json!({"settings": {"index": {"strict_unknown_fields": true}},
                "mappings": {"properties": {"text": {"type": "text"}}}}),
            )
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let response = client
            .put(format!("{url}/{index}/_doc/1?refresh=true"))
            .json(&json!({"text": "retry policy"}))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
    }
    for query in [json!("retry policy"), json!({"match": {"text": "retry"}})] {
        for (scope, expected_index) in [
            (json!({"prefix": "project"}), "project-docs"),
            (json!({"prefix": "My_Proj"}), "my-proj-docs"),
            (json!({"prefix": "project-*"}), "project-docs"),
            (json!({}), "ax-other"),
            (json!({"index": "project-docs"}), "project-docs"),
        ] {
            let mut arguments = scope;
            arguments["query"] = query.clone();
            let response = xerj_mcp::handle_message_for_test(
                &url,
                None,
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "xerj_search", "arguments": arguments}
                }),
            )
            .await
            .unwrap();
            assert_eq!(response["result"]["isError"], false, "{response}");
            let body: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(body["hits"]["total"]["value"], 1, "{body}");
            assert_eq!(body["hits"]["hits"][0]["_index"], expected_index, "{body}");
        }
    }
    server.abort();
    let _ = server.await;
}
