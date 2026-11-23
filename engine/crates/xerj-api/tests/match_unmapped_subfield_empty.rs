//! #3515: `match` (and other full-text queries) on an unmapped dotted sub-field
//! (`title.raw`, where `title` is a dynamic `text` field with only `.keyword`)
//! must return 0 hits on every path, before and after `_flush`, on `_search`
//! and `_count`, as in ES.
//!
//! Before the fix, `match title.raw "fruit"` returned `[]` before `_flush` and
//! `[1, 2]` after `_flush` (the segment FTS reader lacked `title.raw` and fell
//! back to the stored-doc scan, whose `get_field_value` stripped `.raw` and ran
//! the query against the analysed parent `title`), while `constant_score` over
//! `match title.raw` hit the parent on both sides of `_flush`. Mapped
//! multi-fields (`title.keyword`, or an explicit `title.raw`) keep their #3495
//! behaviour.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn req(app: &axum::Router, method: &str, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn ids(app: &axum::Router, index: &str, query: &Value) -> Vec<String> {
    let (status, body) = req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"query": query, "size": 10}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{query}: {body}");
    let mut out: Vec<String> = body["hits"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .map(|h| h["_id"].as_str().expect("_id").to_string())
        .collect();
    out.sort();
    let (status, count) = req(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({"query": query}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{query}: {count}");
    assert_eq!(
        count["count"].as_u64(),
        Some(out.len() as u64),
        "_count disagrees with _search for {query}"
    );
    out
}

async fn flush(app: &axum::Router, index: &str) {
    let (status, body) = req(app, "POST", &format!("/{index}/_flush"), String::new()).await;
    assert_eq!(status, StatusCode::OK, "flush {index}: {body}");
}

#[tokio::test]
async fn match_on_unmapped_subfield_returns_empty_before_and_after_flush() {
    let (app, _dir) = app().await;
    let bulk = concat!(
        "{\"index\":{\"_id\":\"1\"}}\n",
        "{\"title\":\"Fruit\",\"obj\":{\"name\":\"Fruit\"}}\n",
        "{\"index\":{\"_id\":\"2\"}}\n",
        "{\"title\":\"fruit salad\",\"obj\":{\"name\":\"fruit salad\"}}\n",
        "{\"index\":{\"_id\":\"3\"}}\n",
        "{\"title\":\"FRUiT\",\"obj\":{\"name\":\"FRUiT\"}}\n",
    );
    let (status, body) = req(&app, "POST", "/nonorm/_bulk?refresh=true", bulk.to_string()).await;
    assert_eq!(status, StatusCode::OK, "bulk: {body}");
    assert_eq!(body["errors"], json!(false), "bulk: {body}");

    let cases: Vec<(Value, Vec<&str>)> = vec![
        // Issue #3515 table: unmapped `.raw` sub-field matches nothing on every path.
        (json!({"match": {"title.raw": "fruit"}}), vec![]),
        (json!({"match": {"title.raw": "Fruit Salad"}}), vec![]),
        (
            json!({"constant_score": {"filter": {"match": {"title.raw": "fRuIt"}}}}),
            vec![],
        ),
        (
            json!({"bool": {"filter": [{"match": {"title.raw": "fruit"}}]}}),
            vec![],
        ),
        (
            json!({"bool": {"must": [{"match": {"title": "fruit"}}], "filter": [{"match": {"title.raw": "fruit"}}]}}),
            vec![],
        ),
        (
            json!({"bool": {"must": [{"match": {"title": "fruit"}}], "must_not": [{"match": {"title.raw": "fruit"}}]}}),
            vec!["1", "2", "3"],
        ),
        (
            json!({"bool": {"should": [{"match": {"title.raw": "fruit"}}, {"match": {"title": "salad"}}]}}),
            vec!["2"],
        ),
        (
            json!({"multi_match": {"query": "fruit", "fields": ["title.raw"]}}),
            vec![],
        ),
        (
            json!({"multi_match": {"query": "salad", "fields": ["title.raw", "title"]}}),
            vec!["2"],
        ),
        (
            json!({"match_phrase": {"title.raw": "fruit salad"}}),
            vec![],
        ),
        (
            json!({"match_phrase_prefix": {"title.raw": "fruit sal"}}),
            vec![],
        ),
        // Parser-lowered full-text shapes (`match_bool_prefix`, `bool_prefix`,
        // `query_string` wildcard/multi-field, `dis_max`, `intervals`).
        (json!({"match_bool_prefix": {"title.raw": "fru"}}), vec![]),
        (
            json!({"match_bool_prefix": {"title.raw": "fruit sal"}}),
            vec![],
        ),
        (
            json!({"multi_match": {"type": "bool_prefix", "query": "fru", "fields": ["title.raw"]}}),
            vec![],
        ),
        (json!({"query_string": {"query": "title.raw:fru*"}}), vec![]),
        (
            json!({"query_string": {"query": "salad", "fields": ["title.raw", "title"]}}),
            vec!["2"],
        ),
        (
            json!({"multi_match": {"query": "Fruit", "fields": ["title.keyword", "title.raw"]}}),
            vec!["1"],
        ),
        (
            json!({"dis_max": {"queries": [{"match": {"title.raw": "fruit"}}, {"match": {"obj.name.raw": "fruit"}}]}}),
            vec![],
        ),
        (
            json!({"intervals": {"title.raw": {"match": {"query": "fruit"}}}}),
            vec![],
        ),
        // Doubly-suffixed and object-leaf unmapped sub-fields.
        (json!({"match": {"title.raw.extra": "fruit"}}), vec![]),
        (json!({"match": {"title.keyword.raw": "Fruit"}}), vec![]),
        (json!({"match": {"obj.name.raw": "fruit"}}), vec![]),
        // Mapped multi-fields keep #3495 behaviour, and mapped parents stay unchanged.
        (json!({"match": {"title.keyword": "fruit"}}), vec![]),
        (json!({"match": {"title.keyword": "Fruit"}}), vec!["1"]),
        (json!({"match": {"obj.name.keyword": "Fruit"}}), vec!["1"]),
        (json!({"match": {"title": "fruit"}}), vec!["1", "2", "3"]),
        (json!({"match": {"obj.name": "salad"}}), vec!["2"]),
    ];

    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            flush(&app, "nonorm").await;
        }
        for (query, want) in &cases {
            assert_eq!(ids(&app, "nonorm", query).await, *want, "{phase}: {query}");
        }

        // Aggregation `filter` (both pure text and mixed `bool` falling back to
        // `doc_matches_filter`) and `adjacency_matrix` must reject unmapped
        // sub-fields while keeping mapped fields intact.
        let (status, agg_resp) = req(
            &app,
            "POST",
            "/nonorm/_search",
            json!({
                "size": 0,
                "aggs": {
                    "unmapped_raw": { "filter": { "match": { "title.raw": "fruit" } } },
                    "mixed_bool_must": {
                        "filter": {
                            "bool": {
                                "must": [
                                    { "exists": { "field": "title" } },
                                    { "match": { "title.raw": "fruit" } }
                                ]
                            }
                        }
                    },
                    "mixed_bool_must_not": {
                        "filter": {
                            "bool": {
                                "must": [{ "term": { "title.keyword": "Fruit" } }],
                                "must_not": [{ "match": { "title.raw": "fruit" } }]
                            }
                        }
                    },
                    "adj": {
                        "adjacency_matrix": {
                            "filters": { "raw": { "match": { "title.raw": "fruit" } } }
                        }
                    },
                    "mapped_text": { "filter": { "match": { "title": "salad" } } },
                    "mapped_kw_term": { "filter": { "term": { "title.keyword": "Fruit" } } }
                }
            })
            .to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{phase} agg: {agg_resp}");
        assert_eq!(
            agg_resp["aggregations"]["unmapped_raw"]["doc_count"],
            json!(0),
            "{phase}: agg filter on unmapped title.raw must match 0"
        );
        assert_eq!(
            agg_resp["aggregations"]["mixed_bool_must"]["doc_count"],
            json!(0),
            "{phase}: mixed bool agg filter on unmapped title.raw must match 0"
        );
        assert_eq!(
            agg_resp["aggregations"]["mixed_bool_must_not"]["doc_count"],
            json!(1),
            "{phase}: mixed bool must_not on unmapped title.raw must not exclude"
        );
        assert_eq!(
            agg_resp["aggregations"]["adj"]["buckets"]
                .as_array()
                .map(|b| b.len()),
            Some(0),
            "{phase}: adjacency_matrix on unmapped title.raw must return 0 buckets"
        );
        assert_eq!(
            agg_resp["aggregations"]["mapped_text"]["doc_count"],
            json!(1),
            "{phase}: agg filter on mapped title must keep matching"
        );
        assert_eq!(
            agg_resp["aggregations"]["mapped_kw_term"]["doc_count"],
            json!(1),
            "{phase}: agg filter on mapped title.keyword must keep matching"
        );

        // `function_score` function filter on unmapped `title.raw` must not fire.
        let (status, fs_resp) = req(
            &app,
            "POST",
            "/nonorm/_search",
            json!({
                "query": {
                    "function_score": {
                        "query": { "match_all": {} },
                        "functions": [
                            { "filter": { "match": { "title.raw": "fruit" } }, "weight": 100.0 }
                        ],
                        "boost_mode": "multiply"
                    }
                },
                "size": 10
            })
            .to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{phase} function_score: {fs_resp}");
        let hits = fs_resp["hits"]["hits"].as_array().expect("hits");
        assert_eq!(hits.len(), 3, "{phase} function_score hits");
        for hit in hits {
            let score = hit["_score"].as_f64().expect("_score");
            assert!(
                (score - 1.0).abs() < 1e-5,
                "{phase}: function_score filter on unmapped title.raw must not boost (got {score})"
            );
        }
    }
}

#[tokio::test]
async fn explicit_mapping_keeps_declared_multifield_and_rejects_undeclared_siblings() {
    let (app, _dir) = app().await;
    let mapping = json!({
        "mappings": {
            "properties": {
                "title": {
                    "type": "text",
                    "fields": { "raw": { "type": "keyword" } }
                },
                "items": {
                    "type": "nested",
                    "properties": {
                        "title": {
                            "type": "text",
                            "fields": { "keyword": { "type": "keyword" } }
                        }
                    }
                }
            }
        }
    });
    let (status, body) = req(&app, "PUT", "/explicit_mf", mapping.to_string()).await;
    assert_eq!(status, StatusCode::OK, "create: {body}");

    let bulk = concat!(
        "{\"index\":{\"_id\":\"1\"}}\n",
        "{\"title\":\"Fruit\",\"items\":[{\"title\":\"Fruit\"}]}\n",
        "{\"index\":{\"_id\":\"2\"}}\n",
        "{\"title\":\"fruit salad\",\"items\":[{\"title\":\"fruit salad\"}]}\n",
    );
    let (status, body) = req(
        &app,
        "POST",
        "/explicit_mf/_bulk?refresh=true",
        bulk.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bulk: {body}");
    assert_eq!(body["errors"], json!(false), "bulk: {body}");

    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            flush(&app, "explicit_mf").await;
        }
        // Declared `.raw` multi-field works as keyword (#3495).
        assert_eq!(
            ids(
                &app,
                "explicit_mf",
                &json!({"match": {"title.raw": "Fruit"}})
            )
            .await,
            vec!["1"],
            "{phase}: declared title.raw"
        );
        assert_eq!(
            ids(
                &app,
                "explicit_mf",
                &json!({"match": {"title.raw": "fruit"}})
            )
            .await,
            Vec::<String>::new(),
            "{phase}: declared title.raw case-sensitive"
        );
        // Undeclared sibling `.missing` (and `.keyword`, absent from explicit mapping) match nothing.
        assert_eq!(
            ids(
                &app,
                "explicit_mf",
                &json!({"match": {"title.missing": "fruit"}})
            )
            .await,
            Vec::<String>::new(),
            "{phase}: undeclared title.missing"
        );
        assert_eq!(
            ids(
                &app,
                "explicit_mf",
                &json!({"match": {"title.keyword": "fruit"}})
            )
            .await,
            Vec::<String>::new(),
            "{phase}: undeclared title.keyword"
        );
        // Inside `nested`: undeclared `items.title.raw` matches nothing; declared `items.title` works.
        assert_eq!(
            ids(
                &app,
                "explicit_mf",
                &json!({"nested": {"path": "items", "query": {"match": {"items.title.raw": "fruit"}}}})
            )
            .await,
            Vec::<String>::new(),
            "{phase}: nested unmapped items.title.raw"
        );
        assert_eq!(
            ids(
                &app,
                "explicit_mf",
                &json!({"nested": {"path": "items", "query": {"match": {"items.title": "salad"}}}})
            )
            .await,
            vec!["2"],
            "{phase}: nested mapped items.title"
        );
    }
}
