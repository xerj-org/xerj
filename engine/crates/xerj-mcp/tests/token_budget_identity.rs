//! Budgeting must not collapse independent records that share a relative path.
use std::sync::{Arc, Mutex};

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

#[tokio::test]
async fn json_budget_identity_survives_http_dispatch_and_source_projection() {
    for different_indices in [false, true] {
        let hits: Vec<Value> = (0..2)
            .map(|i| {
                json!({
                    "_index": if different_indices { format!("repo-{i}-code") } else { "ax-data".into() },
                    "_id": i.to_string(),
                    "_source": {
                        "ax_path": "shared.txt", "ax_file": "content-hash",
                        "ax_locator": if different_indices { "file".into() } else { format!("b{}", i * 100) }
                    },
                    "fields": {"_passage": [{"field":"body", "ordinal":0, "text":"independent evidence"}]}
                })
            })
            .collect();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = requests.clone();
        // `xerj_search` also POSTs `/autoindex-catalog/_search` after the
        // query (#2345). That probe is not the engine request the budget
        // must leave alone, and recording it in the same list pairs a search
        // body with a catalog body.
        let catalog = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen_catalog = catalog.clone();
        let app = Router::new()
            .route(
                "/:index/_mapping",
                get(|| async {
                    Json(json!({"ax-data":{"mappings":{"properties":{"body":{"type":"text"}}}}}))
                }),
            )
            .route(
                "/:index/_search",
                axum::routing::post(
                    move |axum::extract::Path(index): axum::extract::Path<String>,
                          Json(body): Json<Value>| {
                        let mut hits = hits.clone();
                        let catalog_probe = index == "autoindex-catalog";
                        if catalog_probe {
                            seen_catalog.lock().unwrap().push(body.clone());
                        } else {
                            seen.lock().unwrap().push(body.clone());
                        }
                        async move {
                            if catalog_probe {
                                return Json(json!({"hits":{"hits":[]}}));
                            }
                            // Model the engine's projection: the plain-query default
                            // must actually request identity fields, not just consume
                            // fixtures that always return them.
                            if let Some(fields) = body.get("_source").and_then(Value::as_array) {
                                for hit in &mut hits {
                                    hit["_source"].as_object_mut().unwrap().retain(|key, _| {
                                        fields.iter().any(|v| v.as_str() == Some(key.as_str()))
                                    });
                                }
                            }
                            Json(json!({"hits":{"total":{"value":2,"relation":"eq"},"hits":hits}}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (name, arguments) in [
            ("xerj_search", json!({"index":"ax-*","query":"independent"})),
            (
                "xerj_search",
                json!({"index":"ax-*","query":{"match_all":{}}}),
            ),
            (
                "xerj_semantic_search",
                json!({"index":"ax-*","field":"body","query":"independent"}),
            ),
            (
                "xerj_vector_search",
                json!({"index":"ax-*","field":"embedding","query_vector":[0.1]}),
            ),
            (
                "xerj_hybrid_search",
                json!({"index":"ax-*","queries":[{"query":{"match_all":{}}}]}),
            ),
        ] {
            let mut baseline = None;
            for budget in [None, Some(10_000)] {
                let mut args = arguments.clone();
                if let Some(budget) = budget {
                    args["max_tokens"] = json!(budget);
                }
                let response = xerj_mcp::handle_message_for_test(
                    &url,
                    None,
                    json!({
                        "jsonrpc":"2.0", "id":1, "method":"tools/call",
                        "params":{"name":name,"arguments":args}
                    }),
                )
                .await
                .unwrap();
                assert_eq!(response["result"]["isError"], false, "{response}");
                let text = response["result"]["content"][0]["text"].as_str().unwrap();
                let body: Value = serde_json::from_str(text).unwrap();
                assert_eq!(
                    body["hits"]["hits"].as_array().unwrap().len(),
                    2,
                    "{name}, different_indices={different_indices}, budget={budget:?}: {body}"
                );
                if let Some(baseline) = &baseline {
                    assert_eq!(
                        text, baseline,
                        "nonbinding budget preserves the exact response"
                    );
                } else {
                    baseline = Some(text.to_string());
                }
            }
        }
        {
            let requests = requests.lock().unwrap();
            assert!(
                requests.len() >= 2 && requests.len().is_multiple_of(2),
                "each call is made with and without a budget: {}",
                requests.len()
            );
            for pair in requests.chunks_exact(2) {
                assert_eq!(
                    pair[0], pair[1],
                    "the budget must not change the engine request"
                );
            }
        }
        {
            // Two `xerj_search` calls, each with and without a budget.
            let catalog = catalog.lock().unwrap();
            assert_eq!(catalog.len(), 4, "{catalog:?}");
            for body in catalog.iter() {
                assert_eq!(body["query"]["bool"]["must"][0]["term"]["doc_kind"], "run");
                assert_eq!(
                    body["query"]["bool"]["must"][1]["bool"]["should"][0]["term"]["prefix"],
                    "ax"
                );
            }
        }
        server.abort();
        let _ = server.await;
    }
}
