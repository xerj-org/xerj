//! The batch adapter preserves item boundaries across errors, misses and budgets.
use std::sync::{Arc, Mutex};

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

async fn call(url: &str, args: Value) -> Value {
    let response = xerj_mcp::handle_message_for_test(
        url,
        Some("ApiKey batch-test".into()),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"xerj_batch_search","arguments":args}}),
    )
    .await
    .unwrap();
    response["result"].clone()
}

fn body(result: &Value) -> Value {
    assert_eq!(result["isError"], false, "{result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn batch_preserves_positions_errors_duplicates_and_exact_ids() {
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    let app = Router::new()
        .route(
            "/:index/_mapping",
            get(|| async {
                Json(json!({"docs":{"mappings":{"properties":{
                    "body":{"type":"text"},"advisory":{"type":"keyword"}
                }}}}))
            }),
        )
        .route(
            "/_msearch",
            axum::routing::post(move |headers: axum::http::HeaderMap, text: String| {
                assert_eq!(headers["authorization"], "ApiKey batch-test");
                assert_eq!(headers["content-type"], "application/x-ndjson");
                seen.lock().unwrap().push(text.clone());
                async move {
                    assert!(text.ends_with('\n'));
                    let lines: Vec<Value> = text
                        .lines()
                        .map(|s| serde_json::from_str(s).unwrap())
                        .collect();
                    let responses: Vec<Value> = lines.chunks_exact(2).map(|pair| {
                    assert_eq!(pair[0]["index"], "docs");
                    let q = pair[1]["query"].to_string();
                    if q.contains("missing") {
                        json!({"hits":{"total":{"value":0,"relation":"eq"},"hits":[]}})
                    } else if q.contains("broken") {
                        json!({"error":{"type":"test_error","reason":"invalid query"},"status":400})
                    } else {
                        json!({"hits":{"total":{"value":1,"relation":"eq"},"hits":[{
                            "_index":"docs","_id":"record-1","_source":{
                                "ax_path":"advisories.jsonl","ax_locator":"b100",
                                "body":"CVE-2026-12345 is the evidence"}}]}})
                    }
                }).collect();
                    Json(json!({"responses":responses}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = body(
        &call(
            &url,
            json!({"index":"docs","queries":[
                {"key":"same","identifier":"CVE-2026-12345","field":"advisory"},
                {"query":{"match":{"body":"missing"}}},
                {"query":{"match":{"body":"broken"}}},
                {"query":17},
                {"key":"same","identifier":"CVE-2026-12345","field":"advisory"}
            ]}),
        )
        .await,
    );
    let items = result["results"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item["position"], i);
    }
    assert_eq!(items[0]["status"], "hits");
    assert_eq!(items[0]["abstain"], false);
    assert_eq!(items[0]["hits"][0]["citation"]["ax_locator"], "b100");
    assert_eq!(items[1]["status"], "abstain");
    assert_eq!(items[1]["reason"], "no_hits");
    assert_eq!(items[2]["status"], "error");
    assert_eq!(items[3]["status"], "error");
    assert_eq!(items[4]["hits"], items[0]["hits"]);
    {
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "one HTTP search round trip for the batch"
        );
        let lines: Vec<Value> = requests[0]
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines.len(), 8, "invalid item never reaches the node");
        assert_eq!(
            lines[1]["query"],
            json!({"term":{"advisory":"CVE-2026-12345"}})
        );
    }
    server.abort();
    let _ = server.await;
}

/// #2497: a plain string item is shorthand for `{"query": <string>}`, not an
/// invalid item; a blank one still fails alone with the query error.
#[tokio::test]
async fn batch_accepts_plain_string_items_as_queries() {
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    let app = Router::new().route(
        "/_msearch",
        axum::routing::post(move |text: String| {
            seen.lock().unwrap().push(text.clone());
            let hit = json!({"hits":{"total":{"value":1,"relation":"eq"},"hits":[{
                "_index":"docs","_id":"record-1","_source":{"body":"rotate the WAL"}}]}});
            async move {
                let searches = text.lines().count() / 2;
                Json(json!({"responses":vec![hit; searches]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = body(
        &call(
            &url,
            json!({"index":"docs","queries":[
                "rotate the WAL",
                {"key":"object","query":"rotate the WAL"},
                "   "
            ]}),
        )
        .await,
    );
    let items = result["results"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{result}");
    assert_eq!(items[0]["position"], 0);
    assert_eq!(items[0]["status"], "hits", "{result}");
    assert!(items[0].get("key").is_none(), "{result}");
    assert_eq!(items[0]["hits"], items[1]["hits"]);
    assert_eq!(items[2]["status"], "error", "{result}");
    assert!(
        items[2]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("non-blank"),
        "{result}"
    );
    {
        let requests = requests.lock().unwrap();
        let lines: Vec<Value> = requests[0]
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines.len(), 4, "the blank item never reaches the node");
        assert_eq!(
            lines[1], lines[3],
            "a string item searches exactly as its object form"
        );
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn batch_rejects_invalid_envelopes_before_network() {
    for args in [
        json!({"queries":[]}),
        json!({"queries":"wrong"}),
        json!({"queries":vec![json!({"query":"x"});51]}),
        json!({"queries":[{"query":"x"}],"size":0}),
        json!({"queries":[{"query":"x"}],"prefix":"x","index":"y"}),
    ] {
        let result = call("http://127.0.0.1:1", args).await;
        assert_eq!(result["isError"], true, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("unknown tool"), "{text}");
        assert!(
            !text.contains("connect"),
            "local validation must precede network: {text}"
        );
    }
}

async fn fixture(response: Value, args: Value) -> Value {
    let app = Router::new().route(
        "/_msearch",
        axum::routing::post(move || {
            let response = response.clone();
            async move { Json(response) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = call(&url, args).await;
    server.abort();
    let _ = server.await;
    result
}

#[tokio::test]
async fn batch_never_turns_failed_or_partial_searches_into_abstentions() {
    let empty = json!({"hits":{"hits":[]}});
    for response in [
        json!({"responses":[{"status":503,"hits":{"hits":[]}},empty]}),
        json!({"responses":[{"hits":{"total":{"value":1},"hits":[]}},empty]}),
        json!({"responses":[{"timed_out":true,"hits":{"hits":[]}},empty]}),
        json!({"responses":[{"_shards":{"failed":1},"hits":{"hits":[]}},empty]}),
        json!({"responses":[{"unexpected":"shape"},empty]}),
        json!({"responses":[{"hits":{"hits":[{"_source":{"body":"no identity"}}]}},empty]}),
    ] {
        let result = body(
            &fixture(
                response,
                json!({"queries":[
                    {"query":{"match_all":{}}},{"query":{"match_all":{}}}
                ]}),
            )
            .await,
        );
        assert_eq!(result["results"][0]["status"], "error", "{result}");
        assert!(result["results"][0].get("abstain").is_none());
        assert_eq!(result["results"][1]["status"], "abstain", "{result}");
    }
    for response in [
        json!({"responses":[empty]}),
        json!({"responses":[]}),
        json!({"wrong":[]}),
    ] {
        let result = body(
            &fixture(
                response,
                json!({"queries":[
                    {"query":{"match_all":{}}},{"query":{"match_all":{}}}
                ]}),
            )
            .await,
        );
        assert!(result["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "error"));
    }
}

#[tokio::test]
async fn batch_accepts_fifty_and_budget_never_loses_an_item() {
    let hit = json!({"_index":"docs","_id":"1","_source":{"body":"word ".repeat(400)}});
    let response = json!({"hits":{"hits":[hit]}});
    let args = json!({"queries":vec![json!({"query":{"match_all":{}}});50],"max_tokens":3000});
    let result = fixture(json!({"responses":vec![response;50]}), args).await;
    let value = body(&result);
    assert_eq!(value["results"].as_array().unwrap().len(), 50);
    assert!(value["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["status"] == "hits"));
    use unicode_segmentation::UnicodeSegmentation;
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .unicode_words()
            .count()
            <= 3000
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_exact_identifier_is_not_a_prefix_or_token_match_on_a_real_node() {
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
            .unwrap()
    });
    let client = reqwest::Client::new();
    let response = client
        .put(format!("{url}/advisories"))
        .json(&json!({
            "mappings":{"properties":{"id":{"type":"keyword"},"body":{"type":"text"},
            "ax_path":{"type":"keyword"},"ax_locator":{"type":"keyword"}}}
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    for id in ["CVE-2026-12345", "CVE-2026-123456", "CVE-2025-12345"] {
        let response = client.put(format!("{url}/advisories/_doc/{id}?refresh=true"))
            .json(&json!({"id":id,"body":"advisory evidence","ax_path":"advisories.jsonl","ax_locator":id}))
            .send().await.unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
    }
    let result = body(
        &call(
            &url,
            json!({"index":"advisories","queries":[
                {"identifier":"CVE-2026-12345","field":"id"},
                {"identifier":"CVE-2026-1234","field":"id"},
                {"identifier":"CVE-2026-12345","field":"body"},
                {"query":"advisory"},
                {"identifier":"CVE-2026-12345","field":"absent"}
            ]}),
        )
        .await,
    );
    assert_eq!(
        result["results"][0]["hits"].as_array().unwrap().len(),
        1,
        "{result}"
    );
    assert_eq!(
        result["results"][0]["hits"][0]["hit"]["_source"]["id"],
        "CVE-2026-12345"
    );
    assert_eq!(result["results"][1]["status"], "abstain");
    assert_eq!(result["results"][2]["status"], "error");
    assert_eq!(result["results"][3]["status"], "hits", "{result}");
    assert_eq!(result["results"][4]["status"], "error");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn binding_budget_counts_omitted_hits_and_keeps_the_first_citation() {
    use unicode_segmentation::UnicodeSegmentation;
    let response = json!({"responses":[{"hits":{"hits":[
        {"_index":"docs","_id":"first","_source":{"body":"evidence ".repeat(400)}},
        {"_index":"docs","_id":"second","_source":{"body":"evidence ".repeat(400)}}
    ]}}]});
    let mut args = json!({"queries":[{"query":{"match_all":{}}}]});
    let mut skeleton = body(&fixture(response.clone(), args.clone()).await);
    for hit in skeleton["results"][0]["hits"].as_array_mut().unwrap() {
        hit.as_object_mut().unwrap().remove("hit");
    }
    skeleton["_token_budget"] = json!({"tokenizer":"standard-uax29-words","max_tokens":0});
    // Add an immutable error outcome so the envelope is above the 32-word
    // floor; choose a budget that fits all outcomes but not both citations.
    args["queries"]
        .as_array_mut()
        .unwrap()
        .push(json!({"query":17}));
    let invalid = body(&fixture(json!({"responses":[]}), json!({"queries":[{"query":17}]})).await);
    let mut error = invalid["results"][0].clone();
    error["position"] = json!(1);
    skeleton["results"].as_array_mut().unwrap().push(error);
    let limit = skeleton.to_string().unicode_words().count() - 1;
    assert!(limit >= 32);
    args["max_tokens"] = json!(limit);
    let result = fixture(response, args).await;
    let value = body(&result);
    assert_eq!(
        value["results"][0]["hits"].as_array().unwrap().len(),
        1,
        "{value}"
    );
    assert_eq!(value["results"][0]["hits_omitted"], 1);
    assert_eq!(value["results"][0]["hits"][0]["citation"]["_id"], "first");
    assert_eq!(value["results"][1]["status"], "error");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .unicode_words()
            .count()
            <= limit
    );
}

/// #2694: a batch whose combined `_msearch` body passes the 32 MiB cap is
/// retried in halves, so the small items still answer and only an item whose
/// own response is over the cap fails, alone.
#[tokio::test]
async fn an_oversized_msearch_is_split_and_only_the_oversized_item_fails() {
    let requests = Arc::new(Mutex::new(Vec::<usize>::new()));
    let seen = requests.clone();
    let app = Router::new().route(
        "/_msearch",
        axum::routing::post(move |text: String| {
            let lines: Vec<&str> = text.lines().collect();
            seen.lock().unwrap().push(lines.len() / 2);
            let responses: Vec<Value> = lines
                .chunks(2)
                .map(|pair| {
                    // 20 MiB fits alone and two do not; 40 MiB never fits.
                    let mib = if pair[1].contains("huge") {
                        40
                    } else if pair[1].contains("big") {
                        20
                    } else {
                        0
                    };
                    json!({"hits":{"total":{"value":1,"relation":"eq"},"hits":[{
                        "_index":"docs","_id":format!("r{}", pair[1].len()),
                        "_source":{"ax_path":"src/a.rs","body":"x".repeat(mib << 20)}}]}})
                })
                .collect();
            async move { Json(json!({ "responses": responses })) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = body(
        &call(
            &url,
            json!({"index":"docs","queries":[
                {"query":{"match":{"body":"small one"}}},
                {"query":{"match":{"body":"big one"}}},
                {"query":{"match":{"body":"big two"}}},
                {"query":{"match":{"body":"huge"}}},
                {"query":{"match":{"body":"small two"}}}
            ]}),
        )
        .await,
    );
    assert!(result.get("batch_error").is_none(), "{result}");
    let items = result["results"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item["position"], i);
        if i == 3 {
            assert_eq!(item["status"], "error");
            assert_eq!(item["error"]["reason"], "response_too_large");
        } else {
            assert_eq!(item["status"], "hits", "item {i}");
            assert_eq!(item["hits"][0]["citation"]["ax_path"], "src/a.rs");
        }
    }
    // One whole try, then halves until each request fits.
    let sizes = requests.lock().unwrap().clone();
    assert_eq!(sizes[0], 5, "{sizes:?}");
    assert!(sizes.len() > 1, "{sizes:?}");
    server.abort();
    let _ = server.await;
}
