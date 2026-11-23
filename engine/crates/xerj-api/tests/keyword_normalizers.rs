//! Keyword normalizers (#3513).
//!
//! A keyword field with a normalizer indexes its normalized value (terms,
//! doc values, aggregation keys) and normalizes the values of the queries
//! that target it, while `_source` keeps the original. This build applies the
//! built-in `lowercase` normalizer and custom chains of `lowercase`,
//! `uppercase`, `asciifolding` and `trim`; anything else is refused at
//! mapping time. Every answer is checked with the documents in the memtable,
//! after `_flush`, and after a force-merge of two flushed segments.
//!
//! Without a normalizer, `match` on a keyword multi-field stays a
//! case-sensitive whole-value term, as in ES (#3495).

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

async fn ok(app: &axum::Router, method: &str, path: &str, body: Value) -> Value {
    let text = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let (status, out) = req(app, method, path, text).await;
    assert_eq!(status, StatusCode::OK, "{method} {path} {body}: {out}");
    out
}

async fn bulk(app: &axum::Router, index: &str, docs: &[(&str, Value)]) {
    let mut lines = String::new();
    for (id, doc) in docs {
        lines.push_str(&json!({"index": {"_id": id}}).to_string());
        lines.push('\n');
        lines.push_str(&doc.to_string());
        lines.push('\n');
    }
    let (status, body) = req(app, "POST", &format!("/{index}/_bulk"), lines).await;
    assert_eq!(status, StatusCode::OK, "bulk: {body}");
    assert_eq!(body["errors"], json!(false), "bulk: {body}");
}

/// Sorted ids of `query`, checked against `_count` for the same query.
async fn ids(app: &axum::Router, index: &str, query: &Value) -> Vec<String> {
    let body = ok(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"query": query, "size": 100}),
    )
    .await;
    let mut out: Vec<String> = body["hits"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .map(|h| h["_id"].as_str().expect("_id").to_string())
        .collect();
    out.sort_unstable();
    let count = ok(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({ "query": query }),
    )
    .await;
    assert_eq!(
        count["count"].as_u64(),
        Some(out.len() as u64),
        "_count vs _search for {query}: {count}"
    );
    out
}

/// `(key, doc_count)` of a terms aggregation, sorted by key.
async fn terms(app: &axum::Router, index: &str, field: &str) -> Vec<(String, u64)> {
    let body = ok(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"size": 0, "aggs": {"t": {"terms": {"field": field, "size": 50}}}}),
    )
    .await;
    let mut out: Vec<(String, u64)> = body["aggregations"]["t"]["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("buckets for {field}: {body}"))
        .iter()
        .map(|b| {
            (
                b["key"].as_str().expect("key").to_string(),
                b["doc_count"].as_u64().expect("doc_count"),
            )
        })
        .collect();
    out.sort();
    out
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn kv(v: &[(&str, u64)]) -> Vec<(String, u64)> {
    v.iter().map(|(k, n)| (k.to_string(), *n)).collect()
}

/// Run `check` with the documents buffered, flushed, and force-merged. The
/// documents are written in two batches with a flush between them, so the
/// merge has two segments to combine.
async fn in_every_phase<F, Fut>(app: &axum::Router, index: &str, docs: &[(&str, Value)], check: F)
where
    F: Fn(&'static str) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (first, second) = docs.split_at(docs.len() / 2);
    bulk(app, index, first).await;
    bulk(app, index, second).await;
    check("memtable").await;
    ok(app, "POST", &format!("/{index}/_flush"), json!({})).await;
    check("flushed").await;
    // Rewrite every document so the second write lands in its own segment,
    // then merge the two.
    bulk(app, index, docs).await;
    ok(app, "POST", &format!("/{index}/_flush"), json!({})).await;
    ok(
        app,
        "POST",
        &format!("/{index}/_forcemerge?max_num_segments=1"),
        json!({}),
    )
    .await;
    check("merged").await;
}

#[tokio::test]
async fn builtin_lowercase_normalizes_index_query_and_aggs() {
    let (app, _dir) = app().await;
    let kw = json!({"type": "keyword", "normalizer": "lowercase"});
    ok(
        &app,
        "PUT",
        "/lc",
        json!({"mappings": {"properties": {
            "tag": kw,
            "title": {"type": "text", "fields": {"raw": kw}},
            "meta": {"properties": {"owner": kw}},
        }}}),
    )
    .await;
    let doc = |v: &str| json!({"tag": v, "title": v, "meta": {"owner": v}});
    let docs = [
        ("1", doc("Fruit")),
        ("2", doc("fruit salad")),
        ("3", doc("FRUIT")),
        ("4", doc("Apple")),
    ];
    let app_ref = &app;
    in_every_phase(&app, "lc", &docs, |phase| async move {
        for field in ["tag", "title.raw", "meta.owner"] {
            let cases = [
                (json!({"term": {field: "fruit"}}), s(&["1", "3"])),
                (json!({"term": {field: "FrUiT"}}), s(&["1", "3"])),
                (json!({"match": {field: "FRUIT"}}), s(&["1", "3"])),
                (
                    json!({"terms": {field: ["APPLE", "Fruit Salad"]}}),
                    s(&["2", "4"]),
                ),
                (json!({"prefix": {field: "FRU"}}), s(&["1", "2", "3"])),
                (json!({"wildcard": {field: "F*T"}}), s(&["1", "3"])),
                (
                    json!({"range": {field: {"gte": "B", "lt": "G"}}}),
                    s(&["1", "2", "3"]),
                ),
                (
                    json!({"bool": {"filter": [{"term": {field: "FRUIT"}}]}}),
                    s(&["1", "3"]),
                ),
                (
                    json!({"constant_score": {"filter": {"term": {field: "Fruit"}}}}),
                    s(&["1", "3"]),
                ),
            ];
            for (query, want) in cases {
                assert_eq!(ids(app_ref, "lc", &query).await, want, "{phase} {query}");
            }
        }
        for field in ["tag", "title.raw"] {
            assert_eq!(
                terms(app_ref, "lc", field).await,
                kv(&[("apple", 1), ("fruit", 2), ("fruit salad", 1)]),
                "{phase} terms on {field}"
            );
        }
        // `_source` keeps the original value.
        let body = ok(app_ref, "GET", "/lc/_doc/3", json!(null)).await;
        assert_eq!(body["_source"]["tag"], json!("FRUIT"), "{phase}: {body}");
    })
    .await;
}

#[tokio::test]
async fn custom_lowercase_asciifolding_normalizer() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/custom",
        json!({
            "settings": {"analysis": {"normalizer": {"my_normalizer": {
                "type": "custom", "char_filter": [], "filter": ["lowercase", "asciifolding"]
            }}}},
            "mappings": {"properties": {"foo": {"type": "keyword", "normalizer": "my_normalizer"}}}
        }),
    )
    .await;
    let docs = [
        ("1", json!({"foo": "BÀR"})),
        ("2", json!({"foo": "bar"})),
        ("3", json!({"foo": "baz"})),
        ("4", json!({"foo": "Crème"})),
    ];
    let app_ref = &app;
    in_every_phase(&app, "custom", &docs, |phase| async move {
        for (query, want) in [
            (json!({"term": {"foo": "BAR"}}), s(&["1", "2"])),
            (json!({"match": {"foo": "BAR"}}), s(&["1", "2"])),
            (json!({"term": {"foo": "creme"}}), s(&["4"])),
            (json!({"term": {"foo": "CRÈME"}}), s(&["4"])),
        ] {
            assert_eq!(
                ids(app_ref, "custom", &query).await,
                want,
                "{phase} {query}"
            );
        }
        assert_eq!(
            terms(app_ref, "custom", "foo").await,
            kv(&[("bar", 2), ("baz", 1), ("creme", 1)]),
            "{phase}"
        );
    })
    .await;
    let body = ok(
        &app,
        "POST",
        "/custom/_analyze",
        json!({"normalizer": "my_normalizer", "text": "BÀR"}),
    )
    .await;
    assert_eq!(body["tokens"][0]["token"], json!("bar"), "{body}");
    let body = ok(
        &app,
        "POST",
        "/custom/_analyze",
        json!({"field": "foo", "text": "Crème"}),
    )
    .await;
    assert_eq!(body["tokens"][0]["token"], json!("creme"), "{body}");
    let body = ok(
        &app,
        "POST",
        "/_analyze",
        json!({"normalizer": "lowercase", "text": "ABc"}),
    )
    .await;
    assert_eq!(body["tokens"][0]["token"], json!("abc"), "{body}");
}

#[tokio::test]
async fn uppercase_and_trim_filters() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/up",
        json!({
            "settings": {"analysis": {"normalizer": {"up": {"filter": ["trim", "uppercase"]}}}},
            "mappings": {"properties": {"code": {"type": "keyword", "normalizer": "up"}}}
        }),
    )
    .await;
    let docs = [
        ("1", json!({"code": " ab-1 "})),
        ("2", json!({"code": "AB-1"})),
    ];
    let app_ref = &app;
    in_every_phase(&app, "up", &docs, |phase| async move {
        let query = json!({"term": {"code": "ab-1"}});
        assert_eq!(ids(app_ref, "up", &query).await, s(&["1", "2"]), "{phase}");
        assert_eq!(
            terms(app_ref, "up", "code").await,
            kv(&[("AB-1", 2)]),
            "{phase}"
        );
    })
    .await;
}

fn assert_mapper_refusal(status: StatusCode, body: &Value, want: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{want}: {body}");
    let reason = body
        .pointer("/error/reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(reason.contains(want), "want [{want}] in: {body}");
}

#[tokio::test]
async fn normalizers_this_build_cannot_apply_are_refused() {
    let (app, _dir) = app().await;
    let cases = [
        (
            json!({"mappings": {"properties": {"tag": {"type": "keyword", "normalizer": "nope"}}}}),
            "normalizer [nope] not found for field [tag]",
        ),
        (
            json!({"mappings": {"properties": {"t": {"type": "text", "normalizer": "lowercase"}}}}),
            "unknown parameter [normalizer] on mapper [t] of type [text]",
        ),
        (
            json!({
                "settings": {"analysis": {"normalizer": {"n": {"filter": ["stemmer"]}}}},
                "mappings": {"properties": {"tag": {"type": "keyword", "normalizer": "n"}}}
            }),
            "may not use filter [stemmer]",
        ),
        (
            json!({
                "settings": {"analysis": {"normalizer": {"n": {"filter": ["cjk_width"]}}}},
                "mappings": {"properties": {"tag": {"type": "keyword", "normalizer": "n"}}}
            }),
            "not supported by xerj",
        ),
        (
            json!({"mappings": {"dynamic_templates": [{"s": {
                "match_mapping_type": "string",
                "mapping": {"type": "keyword", "normalizer": "nope"}
            }}]}}),
            "dynamic template [s]: normalizer [nope] not found",
        ),
    ];
    for (i, (body, want)) in cases.into_iter().enumerate() {
        let index = format!("refused_{i}");
        let (status, out) = req(&app, "PUT", &format!("/{index}"), body.to_string()).await;
        assert_mapper_refusal(status, &out, want);
        let (status, out) = req(&app, "GET", &format!("/{index}"), String::new()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{index} exists: {out}");
    }
}

#[tokio::test]
async fn put_mapping_adds_a_normalized_field_but_cannot_change_one() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/pm",
        json!({"mappings": {"properties": {"tag": {"type": "keyword"}}}}),
    )
    .await;
    let (status, body) = req(
        &app,
        "PUT",
        "/pm/_mapping",
        json!({"properties": {"tag": {"type": "keyword", "normalizer": "lowercase"}}}).to_string(),
    )
    .await;
    assert_mapper_refusal(status, &body, "Cannot update parameter [normalizer]");
    let (status, body) = req(
        &app,
        "PUT",
        "/pm/_mapping",
        json!({"properties": {"label": {"type": "keyword", "normalizer": "nope"}}}).to_string(),
    )
    .await;
    assert_mapper_refusal(
        status,
        &body,
        "normalizer [nope] not found for field [label]",
    );
    ok(
        &app,
        "PUT",
        "/pm/_mapping",
        json!({"properties": {"label": {"type": "keyword", "normalizer": "lowercase"}}}),
    )
    .await;
    let docs = [
        ("1", json!({"tag": "A", "label": "Red"})),
        ("2", json!({"tag": "a", "label": "RED"})),
    ];
    let app_ref = &app;
    in_every_phase(&app, "pm", &docs, |phase| async move {
        let query = json!({"term": {"label": "red"}});
        assert_eq!(ids(app_ref, "pm", &query).await, s(&["1", "2"]), "{phase}");
        // `tag` has no normalizer: still case-sensitive.
        let query = json!({"term": {"tag": "a"}});
        assert_eq!(ids(app_ref, "pm", &query).await, s(&["2"]), "{phase}");
    })
    .await;
    let body = ok(&app, "GET", "/pm/_mapping", json!(null)).await;
    assert_eq!(
        body.pointer("/pm/mappings/properties/label/normalizer"),
        Some(&json!("lowercase")),
        "{body}"
    );
}

#[tokio::test]
async fn dynamic_template_maps_new_strings_to_a_normalized_keyword() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/dyn",
        json!({"mappings": {"dynamic_templates": [
            {"ids": {"match": "*_id", "mapping": {"type": "keyword"}}},
            {"strings": {
                "match_mapping_type": "string",
                "mapping": {"type": "keyword", "normalizer": "lowercase"}
            }}
        ]}}),
    )
    .await;
    let docs = [
        ("1", json!({"color": "Red", "user_id": "AB"})),
        ("2", json!({"color": "RED", "user_id": "ab"})),
        ("3", json!({"color": "blue", "user_id": "cd"})),
        ("4", json!({"color": "Blue", "user_id": "CD"})),
    ];
    let app_ref = &app;
    in_every_phase(&app, "dyn", &docs, |phase| async move {
        let query = json!({"term": {"color": "RED"}});
        assert_eq!(ids(app_ref, "dyn", &query).await, s(&["1", "2"]), "{phase}");
        assert_eq!(
            terms(app_ref, "dyn", "color").await,
            kv(&[("blue", 2), ("red", 2)]),
            "{phase}"
        );
    })
    .await;
    let body = ok(&app, "GET", "/dyn/_mapping", json!(null)).await;
    assert_eq!(
        body.pointer("/dyn/mappings/properties/color"),
        Some(&json!({"type": "keyword", "normalizer": "lowercase"})),
        "{body}"
    );
    // The first matching template (`ids`, no normalizer) wins for `user_id`.
    assert!(
        body.pointer("/dyn/mappings/properties/user_id/normalizer")
            .is_none(),
        "{body}"
    );
}

#[tokio::test]
async fn keyword_multi_field_without_normalizer_is_case_sensitive() {
    let (app, _dir) = app().await;
    let raw = json!({"type": "text", "fields": {"raw": {"type": "keyword"}}});
    ok(
        &app,
        "PUT",
        "/cs",
        json!({"mappings": {"properties": {"title": raw}}}),
    )
    .await;
    let docs = [
        ("1", json!({"title": "Fruit"})),
        ("2", json!({"title": "fruit"})),
        ("3", json!({"title": "fruit salad"})),
    ];
    let app_ref = &app;
    in_every_phase(&app, "cs", &docs, |phase| async move {
        for (q, want) in [
            ("Fruit", s(&["1"])),
            ("fruit", s(&["2"])),
            ("FRUIT", s(&[])),
            ("salad", s(&[])),
        ] {
            let query = json!({"match": {"title.raw": q}});
            assert_eq!(ids(app_ref, "cs", &query).await, want, "{phase} {query}");
        }
    })
    .await;
}
