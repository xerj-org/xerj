//! Local definition lookup must use the CLI's ranking and stay in its corpus.
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(String, String, Value)>>>;

async fn mock_node(
    mapping: Value,
    status: axum::http::StatusCode,
    response: String,
) -> (String, tokio::task::JoinHandle<()>, Requests) {
    let requests: Requests = Arc::default();
    let captured = requests.clone();
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let captured = captured.clone();
        let mapping = mapping.clone();
        let response = response.clone();
        async move {
            let path = request.uri().path().to_string();
            let auth = request
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            captured.lock().unwrap().push((path.clone(), auth, body));
            if path.ends_with("/_mapping") {
                (axum::http::StatusCode::OK, mapping.to_string())
            } else {
                (status, response)
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, server, requests)
}

#[tokio::test]
async fn local_def_mapping_guards_scope_auth_and_legacy_passages() {
    for (properties, numeric) in [
        (json!({"body":{"type":"text"}}), false),
        (
            json!({"body":{"type":"text"},"name":{"type":"long"},
            "kind":{"type":"integer"},"language":{"type":"long"}}),
            true,
        ),
    ] {
        let mut mapping = json!({"project-prose":{"mappings":{"properties":properties}}});
        if numeric {
            mapping["project-code"] = json!({"mappings":{"properties":{
                "name":{"type":"keyword"},"kind":{"type":"keyword"},
                "language":{"type":"keyword"},"code":{"type":"text"}}}});
        }
        let legacy = json!({"_id":"legacy","_score":1.0,"_source":{"ax_path":"guide.md"},
            "fields":{"_passage":[{"text":"Cache declaration", "start_line":12}]}});
        let (url, server, requests) = mock_node(
            mapping,
            axum::http::StatusCode::OK,
            json!({"hits":{"total":{"value":1},"hits":[legacy.clone()]}}).to_string(),
        )
        .await;
        let response = lookup(
            &url,
            json!({"symbol":"Cache", "prefix":" project ",
            "kind":"struct", "lang":"rust"}),
        )
        .await;
        assert_eq!(response["hits"]["hits"], json!([legacy]));
        let all = requests.lock().unwrap();
        // #2426: a definition lookup reads the autoindex catalog once for
        // the staleness hint, like search; the lookup itself is unchanged.
        let catalog = "/autoindex-catalog/_search";
        assert_eq!(all.iter().filter(|r| r.0 == catalog).count(), 1);
        let requests: Vec<_> = all.iter().filter(|r| r.0 != catalog).collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0, "/project-*/_mapping");
        assert_eq!(requests[1].0, "/project-*/_search");
        assert!(requests.iter().all(|r| r.1 == "ApiKey fixture"));
        let body = &requests[1].2;
        assert_eq!(body["size"], 45, "fetch a wider window before reranking");
        assert_eq!(body["query"]["bool"]["filter"], json!([]));
        let query = body["query"].to_string();
        assert!(!query.contains("\"name\""), "{query}");
        assert!(!query.contains("name^"), "{query}");
        assert!(
            query.contains(if numeric { "code^2" } else { "body" }),
            "{query}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn local_def_preserves_errors_and_refuses_malformed_success() {
    use axum::http::StatusCode;
    for (status, body, expected) in [
        (
            StatusCode::UNAUTHORIZED,
            "denied",
            "HTTP 401 Unauthorized: denied",
        ),
        (
            StatusCode::BAD_REQUEST,
            "strict field refusal",
            "HTTP 400 Bad Request: strict field refusal",
        ),
        (StatusCode::OK, "not json", "invalid JSON"),
        (StatusCode::OK, "{}", "without hits.hits"),
    ] {
        let (url, server, requests) = mock_node(json!({}), status, body.to_owned()).await;
        let response = xerj_mcp::handle_message_for_test(
            &url,
            Some("ApiKey fixture".into()),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"xerj_def","arguments":{"symbol":"Cache"}}}),
        )
        .await
        .unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{response}"
        );
        // Unreadable mappings keep the historical CLI query, not match_all.
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests[1].2["query"]["bool"]["should"][0]["term"]["name"]["value"],
            "Cache"
        );
        server.abort();
    }
}

#[tokio::test]
async fn local_def_applies_token_budget_after_ranking() {
    use unicode_segmentation::UnicodeSegmentation;
    let hit = json!({"_id":"real", "_score":1, "_source":{
        "ax_path":"src/cache.rs", "line":7, "kind":"struct",
        "code":format!("pub struct Cache {}", "many words ".repeat(500))}});
    let (url, server, _) = mock_node(
        json!({}),
        axum::http::StatusCode::OK,
        json!({"hits":{"hits":[hit]}}).to_string(),
    )
    .await;
    let response = xerj_mcp::handle_message_for_test(
        &url,
        Some("ApiKey fixture".into()),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call", "params":{
            "name":"xerj_def","arguments":{"symbol":"Cache","max_tokens":128}}}),
    )
    .await
    .unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.unicode_words().count() <= 128, "{text}");
    assert!(text.contains("src/cache.rs"), "{text}");
    server.abort();
}

#[tokio::test]
async fn local_def_rejects_invalid_arguments_before_http() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    for (args, field) in [
        (json!({}), "symbol"),
        (json!({"symbol":"  "}), "symbol"),
        (json!({"symbol":{}}), "symbol"),
        (json!({"symbol":"Cache","prefix":" "}), "prefix"),
        (json!({"symbol":"Cache","index":42}), "index"),
        (
            json!({"symbol":"Cache","prefix":"project","index":"ax-*"}),
            "prefix",
        ),
        (json!({"symbol":"Cache","kind":42}), "kind"),
        (json!({"symbol":"Cache","lang":" "}), "lang"),
        (json!({"symbol":"Cache","k":0}), "k"),
        (json!({"symbol":"Cache","k":51}), "k"),
        (json!({"symbol":"Cache","k":1.5}), "k"),
        (json!({"symbol":"Cache","max_tokens":0}), "max_tokens"),
    ] {
        let response = xerj_mcp::handle_message_for_test(
            &url,
            Some("ApiKey fixture".into()),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"xerj_def","arguments":args}}),
        )
        .await
        .unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(&format!("`{field}`")), "{text}");
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    }
}

async fn lookup(url: &str, arguments: Value) -> Value {
    let response = xerj_mcp::handle_message_for_test(
        url,
        Some("ApiKey fixture".into()),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"xerj_def","arguments":arguments}}),
    )
    .await
    .unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_def_returns_ranked_unique_declarations_in_the_requested_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = tmp.path().join("data").to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.auth.enabled = false;
    let metrics = xerj_common::metrics::Metrics::new().unwrap();
    let engine = xerj_engine::Engine::new(config.clone()).unwrap();
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    for index in ["project-code", "ax-code"] {
        let response = client
            .put(format!("{url}/{index}"))
            .json(&json!({
                "settings":{"index":{"strict_unknown_fields":true}},
                "mappings":{"properties":{
                    "name":{"type":"keyword"}, "kind":{"type":"keyword"},
                    "language":{"type":"keyword"}, "code":{"type":"text"},
                    "ax_path":{"type":"keyword"}, "line":{"type":"integer"}
                }}
            }))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        for (id, path, kind, language, code) in [
            ("test", "tests/cache.rs", "struct", "rust", "struct Cache"),
            (
                "alias",
                "src/alias.rs",
                "type",
                "rust",
                "type Cache = Store;",
            ),
            ("real", "src/cache.rs", "struct", "rust", "pub struct Cache"),
            (
                "duplicate",
                "src/cache.rs",
                "struct",
                "rust",
                "pub struct Cache",
            ),
            ("python", "src/cache.py", "class", "python", "class Cache:"),
        ] {
            let response = client
                .put(format!("{url}/{index}/_doc/{id}?refresh=true"))
                .json(&json!({"name":"Cache","kind":kind,"language":language,
                    "code":code,"ax_path":path,"line":7}))
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_success(),
                "{}",
                response.text().await.unwrap()
            );
        }
    }
    for (scope, index) in [
        (json!({"prefix":"project"}), "project-code"),
        (json!({"index":"project-code"}), "project-code"),
        (json!({}), "ax-code"),
    ] {
        let mut args = scope;
        args["symbol"] = json!("Cache");
        let result = lookup(&url, args.clone()).await;
        let hits = result["hits"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 4, "{result}");
        assert!(hits.iter().all(|h| h["_index"] == index), "{result}");
        assert_eq!(hits[0]["_source"]["ax_path"], "src/cache.rs");
        assert_eq!(hits[0]["_source"]["line"], 7);
        assert_eq!(hits[0]["_source"]["code"], "pub struct Cache");
        args["k"] = json!(1);
        assert_eq!(
            lookup(&url, args.clone()).await["hits"]["hits"],
            json!([hits[0]])
        );
        args["kind"] = json!("class");
        args["lang"] = json!("python");
        let filtered = lookup(&url, args.clone()).await;
        assert_eq!(
            filtered["hits"]["hits"][0]["_source"]["ax_path"],
            "src/cache.py"
        );
        args["symbol"] = json!("MissingIdentifier");
        assert_eq!(lookup(&url, args).await["hits"]["hits"], json!([]));
    }
    // Numeric homonyms in another dataset must not poison a wildcard lookup.
    let response = client
        .put(format!("{url}/project-metrics"))
        .json(&json!({
            "settings":{"index":{"strict_unknown_fields":true}},
            "mappings":{"properties":{
                "name":{"type":"long"},"kind":{"type":"integer"},
                "language":{"type":"long"},"body":{"type":"text"}
            }}
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let response = lookup(
        &url,
        json!({"symbol":"Cache", "prefix":"project",
        "kind":"struct", "lang":"rust"}),
    )
    .await;
    let hits = response["hits"]["hits"].as_array().unwrap();
    assert_eq!(
        hits.len(),
        4,
        "numeric kind/language filters are ignored: {response}"
    );
    assert_eq!(hits[0]["_source"]["ax_path"], "src/cache.rs");
    server.abort();
    let _ = server.await;
}
