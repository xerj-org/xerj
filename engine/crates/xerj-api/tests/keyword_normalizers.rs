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

#[tokio::test]
async fn fields_docvalue_fields_and_sort_emit_normalized_values_in_every_phase() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/out",
        json!({
            "settings": {"analysis": {"normalizer": {"fold_lower": {
                "type": "custom", "filter": ["lowercase", "asciifolding"]
            }}}},
            "mappings": {"properties": {
                "foo": {"type": "keyword", "normalizer": "fold_lower"},
                "tag": {"type": "keyword", "normalizer": "lowercase"},
                "tag_alias": {"type": "alias", "path": "tag"},
                "plain": {"type": "keyword"},
                "title": {"type": "text", "fields": {"raw": {"type": "keyword", "normalizer": "lowercase"}}},
                "meta": {"properties": {"owner": {"type": "keyword", "normalizer": "lowercase"}}},
                "items": {"type": "nested", "properties": {"code": {"type": "keyword", "normalizer": "fold_lower"}}}
            }}
        }),
    )
    .await;
    let docs = [
        (
            "1",
            json!({
                "foo": "BÀR",
                "tag": "Fruit",
                "plain": "Fruit",
                "title": "Fruit",
                "meta": {"owner": "Bob"},
                "items": [{"code": "BÀR"}]
            }),
        ),
        (
            "2",
            json!({
                "foo": "bap",
                "tag": "Banana",
                "plain": "Banana",
                "title": "Banana",
                "meta": {"owner": "alice"},
                "items": [{"code": "bap"}]
            }),
        ),
        (
            "3",
            json!({
                "foo": "BAS",
                "tag": ["Zebra", "apple"],
                "plain": "apple",
                "title": "apple",
                "meta": {"owner": "Cara"},
                "items": [{"code": "BAS"}]
            }),
        ),
    ];
    let app_ref = &app;
    in_every_phase(&app, "out", &docs, |phase| async move {
        // 1. `sort` on normalized keyword (including multi-valued array reduction after normalizing)
        // and `fields` on top-level, alias, multi-field, object leaf, nested sub-field, and plain.
        let body = ok(
            app_ref,
            "POST",
            "/out/_search",
            json!({
                "sort": [{"tag": "asc"}],
                "fields": ["foo", "tag", "tag_alias", "plain", "title.raw", "meta.owner", "items.code"]
            }),
        )
        .await;
        let hits = body["hits"]["hits"].as_array().expect("hits");
        let hit_ids: Vec<&str> = hits
            .iter()
            .map(|h| h["_id"].as_str().unwrap())
            .collect();
        assert_eq!(hit_ids, vec!["3", "2", "1"], "{phase} sort order: {body}");
        let sort_vals: Vec<&Value> = hits.iter().map(|h| &h["sort"][0]).collect();
        assert_eq!(
            sort_vals,
            vec![&json!("apple"), &json!("banana"), &json!("fruit")],
            "{phase} sort values: {body}"
        );
        // Hit 2 (`_id: 1`): normalized `fields`, untouched `_source` and `plain`.
        assert_eq!(hits[2]["_source"]["foo"], json!("BÀR"), "{phase}");
        assert_eq!(hits[2]["_source"]["tag"], json!("Fruit"), "{phase}");
        assert_eq!(hits[2]["fields"]["foo"], json!(["bar"]), "{phase}: {body}");
        assert_eq!(hits[2]["fields"]["tag"], json!(["fruit"]), "{phase}: {body}");
        assert_eq!(
            hits[2]["fields"]["tag_alias"],
            json!(["fruit"]),
            "{phase}: {body}"
        );
        assert_eq!(
            hits[2]["fields"]["plain"],
            json!(["Fruit"]),
            "{phase} plain must stay original: {body}"
        );
        assert_eq!(
            hits[2]["fields"]["title.raw"],
            json!(["fruit"]),
            "{phase}: {body}"
        );
        assert_eq!(
            hits[2]["fields"]["meta.owner"],
            json!(["bob"]),
            "{phase}: {body}"
        );
        assert_eq!(
            hits[2]["fields"]["items"][0]["code"],
            json!(["bar"]),
            "{phase}: {body}"
        );

        // 2. `docvalue_fields` + `sort` on custom normalizer (`foo`) and multi-field (`title.raw`).
        let body = ok(
            app_ref,
            "POST",
            "/out/_search",
            json!({
                "sort": [{"foo": "asc"}],
                "docvalue_fields": ["foo", "tag", "plain", "title.raw", "meta.owner"]
            }),
        )
        .await;
        let hits = body["hits"]["hits"].as_array().expect("hits");
        let hit_ids: Vec<&str> = hits
            .iter()
            .map(|h| h["_id"].as_str().unwrap())
            .collect();
        assert_eq!(hit_ids, vec!["2", "1", "3"], "{phase} foo sort: {body}");
        let sort_vals: Vec<&Value> = hits.iter().map(|h| &h["sort"][0]).collect();
        assert_eq!(
            sort_vals,
            vec![&json!("bap"), &json!("bar"), &json!("bas")],
            "{phase} foo sort values: {body}"
        );
        assert_eq!(hits[1]["fields"]["foo"], json!(["bar"]), "{phase}: {body}");
        assert_eq!(hits[1]["fields"]["tag"], json!(["fruit"]), "{phase}: {body}");
        assert_eq!(
            hits[1]["fields"]["plain"],
            json!(["Fruit"]),
            "{phase} plain dv must stay original: {body}"
        );
        assert_eq!(
            hits[1]["fields"]["title.raw"],
            json!(["fruit"]),
            "{phase}: {body}"
        );
        assert_eq!(
            hits[1]["fields"]["meta.owner"],
            json!(["bob"]),
            "{phase}: {body}"
        );

        // 3. `sort` on multi-field (`title.raw`) and object leaf (`meta.owner`).
        let body = ok(
            app_ref,
            "POST",
            "/out/_search",
            json!({"sort": [{"title.raw": "asc"}]}),
        )
        .await;
        let sort_vals: Vec<&Value> = body["hits"]["hits"]
            .as_array()
            .expect("hits")
            .iter()
            .map(|h| &h["sort"][0])
            .collect();
        assert_eq!(
            sort_vals,
            vec![&json!("apple"), &json!("banana"), &json!("fruit")],
            "{phase} title.raw sort: {body}"
        );
        let body = ok(
            app_ref,
            "POST",
            "/out/_search",
            json!({"sort": [{"meta.owner": "asc"}]}),
        )
        .await;
        let sort_vals: Vec<&Value> = body["hits"]["hits"]
            .as_array()
            .expect("hits")
            .iter()
            .map(|h| &h["sort"][0])
            .collect();
        assert_eq!(
            sort_vals,
            vec![&json!("alice"), &json!("bob"), &json!("cara")],
            "{phase} meta.owner sort: {body}"
        );
    })
    .await;
}

#[tokio::test]
async fn query_string_and_simple_query_string_normalize_terms_in_every_phase() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/qs",
        json!({
            "settings": {"analysis": {"normalizer": {"fold_lower": {
                "type": "custom", "filter": ["lowercase", "asciifolding"]
            }}}},
            "mappings": {"properties": {
                "foo": {"type": "keyword", "normalizer": "fold_lower"},
                "tag": {"type": "keyword", "normalizer": "lowercase"},
                "plain": {"type": "keyword"},
                "title": {"type": "text", "fields": {"raw": {"type": "keyword", "normalizer": "lowercase"}}},
                "meta": {"properties": {"owner": {"type": "keyword", "normalizer": "lowercase"}}}
            }}
        }),
    )
    .await;
    let docs = [
        (
            "1",
            json!({"foo": "BÀR", "tag": "Fruit", "plain": "Fruit", "title": "Fruit", "meta": {"owner": "Alice"}}),
        ),
        (
            "2",
            json!({"foo": "bar", "tag": "fruit salad", "plain": "fruit", "title": "fruit salad", "meta": {"owner": "Bob"}}),
        ),
        (
            "3",
            json!({"foo": "baz", "tag": "FRUIT", "plain": "Other", "title": "FRUIT", "meta": {"owner": "ALICE"}}),
        ),
    ];
    let app_ref = &app;
    in_every_phase(&app, "qs", &docs, |phase| async move {
        let cases = [
            // query_string explicit, default_field, fields, phrase, wildcard, boolean
            (json!({"query_string": {"query": "foo:BAR"}}), s(&["1", "2"])),
            (
                json!({"query_string": {"query": "BAR", "default_field": "foo"}}),
                s(&["1", "2"]),
            ),
            (
                json!({"query_string": {"query": "tag:\"FRUIT SALAD\""}}),
                s(&["2"]),
            ),
            (
                json!({"query_string": {"query": "\"FRUIT SALAD\"", "default_field": "title.raw"}}),
                s(&["2"]),
            ),
            (
                json!({"query_string": {"query": "foo:BÀ*"}}),
                s(&["1", "2", "3"]),
            ),
            (
                json!({"query_string": {"query": "BÀ*"}}),
                s(&["1", "2", "3"]),
            ),
            (
                json!({"query_string": {"query": "foo:BAR AND meta.owner:ALICE"}}),
                s(&["1"]),
            ),
            // simple_query_string fieldless, ["*"], fields, boosted, phrase, prefix
            (
                json!({"simple_query_string": {"query": "BAR"}}),
                s(&["1", "2"]),
            ),
            (
                json!({"simple_query_string": {"query": "BAR", "fields": ["*"]}}),
                s(&["1", "2"]),
            ),
            (
                json!({"simple_query_string": {"query": "BAR", "fields": ["foo"]}}),
                s(&["1", "2"]),
            ),
            (
                json!({"simple_query_string": {"query": "BAR", "fields": ["foo^2"]}}),
                s(&["1", "2"]),
            ),
            (
                json!({"simple_query_string": {"query": "\"FRUIT SALAD\"", "fields": ["tag"]}}),
                s(&["2"]),
            ),
            (
                json!({"simple_query_string": {"query": "\"FRUIT SALAD\""}}),
                s(&["2"]),
            ),
            (
                json!({"simple_query_string": {"query": "FRU*", "fields": ["tag"]}}),
                s(&["1", "2", "3"]),
            ),
            (
                json!({"simple_query_string": {"query": "BÀ*", "fields": ["foo"]}}),
                s(&["1", "2", "3"]),
            ),
            (
                json!({"simple_query_string": {"query": "BAR + ALICE", "fields": ["foo", "meta.owner"]}}),
                s(&["1"]),
            ),
            // Negative controls: unnormalized `plain` stays case-sensitive
            (json!({"query_string": {"query": "plain:FRUIT"}}), s(&[])),
            (
                json!({"simple_query_string": {"query": "FRUIT", "fields": ["plain"]}}),
                s(&[]),
            ),
        ];
        for (query, want) in cases {
            assert_eq!(ids(app_ref, "qs", &query).await, want, "{phase} {query}");
        }
    })
    .await;
}

#[tokio::test]
async fn dynamic_templates_apply_to_object_leaves_nested_paths_and_other_mappings() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/dyn2",
        json!({
            "settings": {"analysis": {"normalizer": {"fold_lower": {
                "type": "custom", "filter": ["lowercase", "asciifolding"]
            }}}},
            "mappings": {
                "properties": {
                    "items": {"type": "nested"}
                },
                "dynamic_templates": [
                    {"meta_lower": {
                        "path_match": "meta.*",
                        "path_unmatch": "*.raw_id",
                        "match_mapping_type": "string",
                        "mapping": {"type": "keyword", "normalizer": "lowercase"}
                    }},
                    {"nested_fold": {
                        "path_match": "items.*",
                        "match_mapping_type": "string",
                        "mapping": {"type": "keyword", "normalizer": "fold_lower"}
                    }},
                    {"text_with_raw": {
                        "match": "*_text",
                        "match_mapping_type": "string",
                        "mapping": {
                            "type": "text",
                            "fields": {"raw": {"type": "keyword", "normalizer": "lowercase"}}
                        }
                    }},
                    {"plain_ids": {
                        "match": "*_id",
                        "mapping": {"type": "keyword"}
                    }},
                    {"no_dv_longs": {
                        "match_mapping_type": "long",
                        "mapping": {"type": "{dynamic_type}", "doc_values": false}
                    }}
                ]
            }
        }),
    )
    .await;

    // First doc establishes `meta` (with `owner` and `raw_id`), `items.code`, `note_text`, `user_id`, `count`.
    // Second doc adds a brand-new leaf `meta.region` into the already-existing `meta` object!
    let docs = [
        (
            "1",
            json!({
                "meta": {"owner": "Alice", "raw_id": "ID-1"},
                "items": [{"code": "BÀR"}],
                "note_text": "Fruit Salad",
                "user_id": "AB",
                "count": 7
            }),
        ),
        (
            "2",
            json!({
                "meta": {"owner": "ALICE", "region": "North", "raw_id": "id-1"},
                "items": [{"code": "bar"}],
                "note_text": "fruit salad",
                "user_id": "ab",
                "count": 9
            }),
        ),
    ];
    let app_ref = &app;
    in_every_phase(&app, "dyn2", &docs, |phase| async move {
        assert_eq!(
            ids(app_ref, "dyn2", &json!({"term": {"meta.owner": "alice"}})).await,
            s(&["1", "2"]),
            "{phase} meta.owner"
        );
        assert_eq!(
            ids(app_ref, "dyn2", &json!({"term": {"meta.region": "NORTH"}})).await,
            s(&["2"]),
            "{phase} meta.region"
        );
        assert_eq!(
            ids(app_ref, "dyn2", &json!({"term": {"meta.raw_id": "ID-1"}})).await,
            s(&["1"]),
            "{phase} meta.raw_id (excluded by path_unmatch)"
        );
        assert_eq!(
            ids(
                app_ref,
                "dyn2",
                &json!({"term": {"note_text.raw": "FRUIT SALAD"}})
            )
            .await,
            s(&["1", "2"]),
            "{phase} note_text.raw"
        );
        assert_eq!(
            ids(
                app_ref,
                "dyn2",
                &json!({"nested": {"path": "items", "query": {"term": {"items.code": "BAR"}}}})
            )
            .await,
            s(&["1", "2"]),
            "{phase} items.code"
        );
    })
    .await;

    let mapping = ok(&app, "GET", "/dyn2/_mapping", json!(null)).await;
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/meta/properties/owner/normalizer"),
        Some(&json!("lowercase")),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/meta/properties/region/normalizer"),
        Some(&json!("lowercase")),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/meta/properties/raw_id/type"),
        Some(&json!("keyword")),
        "{mapping}"
    );
    assert!(
        mapping
            .pointer("/dyn2/mappings/properties/meta/properties/raw_id/normalizer")
            .is_none(),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/items/properties/code/normalizer"),
        Some(&json!("fold_lower")),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/note_text/fields/raw/normalizer"),
        Some(&json!("lowercase")),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/user_id/type"),
        Some(&json!("keyword")),
        "{mapping}"
    );
    assert!(
        mapping
            .pointer("/dyn2/mappings/properties/user_id/fields")
            .is_none(),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn2/mappings/properties/count/doc_values"),
        Some(&json!(false)),
        "{mapping}"
    );

    // Invalid normalizer inside a dynamic template's multi-field must be refused with 400.
    let (status, out) = req(
        &app,
        "PUT",
        "/dyn_bad_mf",
        json!({"mappings": {"dynamic_templates": [{"t": {
            "match_mapping_type": "string",
            "mapping": {"type": "text", "fields": {"raw": {"type": "keyword", "normalizer": "nope"}}}
        }}]}})
        .to_string(),
    )
    .await;
    assert_mapper_refusal(status, &out, "normalizer [nope] not found");
}

#[tokio::test]
async fn nested_query_string_match_phrase_prefix_and_unmapped_simple_prefix_in_every_phase() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/nest_qs",
        json!({
            "settings": {
                "analysis": {
                    "normalizer": {
                        "fold_lower": {"type": "custom", "filter": ["lowercase", "asciifolding"]}
                    }
                }
            },
            "mappings": {
                "properties": {
                    "foo": {"type": "keyword", "normalizer": "fold_lower"},
                    "raw_kw": {"type": "keyword"},
                    "title": {
                        "type": "text",
                        "fields": {
                            "raw": {"type": "keyword", "normalizer": "fold_lower"}
                        }
                    },
                    "body": {"type": "text"},
                    "items": {
                        "type": "nested",
                        "properties": {
                            "code": {"type": "keyword", "normalizer": "fold_lower"}
                        }
                    }
                }
            }
        }),
    )
    .await;

    let docs = [
        (
            "1",
            json!({
                "foo": "BÀR Baz",
                "raw_kw": "Fruit",
                "title": "BÀR",
                "body": "alpha beta",
                "dyn_txt": "Fruit Salad",
                "items": [{"code": "BÀR"}, {"code": "Fruit Salad"}]
            }),
        ),
        (
            "2",
            json!({
                "foo": "qux",
                "raw_kw": "Other",
                "title": "Other",
                "body": "alpha gamma",
                "dyn_txt": "Other Item",
                "items": [{"code": "QUX"}]
            }),
        ),
    ];

    let app_ref = &app;
    in_every_phase(app_ref, "nest_qs", &docs, |phase| async move {
        // P1 #3: nested query containing query_string & simple_query_string on a
        // normalized nested keyword leaf.
        for (label, query) in [
            (
                "nested query_string term",
                json!({"nested": {"path": "items", "query": {"query_string": {"query": "items.code:BAR"}}}}),
            ),
            (
                "nested query_string quoted phrase",
                json!({"nested": {"path": "items", "query": {"query_string": {"query": "items.code:\"FRUIT SALAD\""}}}}),
            ),
            (
                "nested simple_query_string",
                json!({"nested": {"path": "items", "query": {"simple_query_string": {"query": "BAR", "fields": ["items.code"]}}}}),
            ),
            // P1 #4: match_phrase_prefix on normalized keyword (pre-flush & post-flush parity).
            (
                "match_phrase_prefix fold_lower",
                json!({"match_phrase_prefix": {"foo": "BÀR B"}}),
            ),
            // P1 #4: default_operator AND preserved on field-less query_string.
            (
                "query_string default_operator AND",
                json!({"query_string": {"query": "alpha beta", "default_operator": "AND"}}),
            ),
            // P1 #4: wildcard pattern matching normalized multi-field `title.raw`.
            (
                "query_string title.* multi-field",
                json!({"query_string": {"query": "bar", "fields": ["title.*"]}}),
            ),
            // P1 #11: simple_query_string prefix on analyzed text & dynamic text folds case.
            (
                "simple_query_string text prefix",
                json!({"simple_query_string": {"query": "ALP*", "fields": ["body"]}}),
            ),
            (
                "simple_query_string dynamic text prefix",
                json!({"simple_query_string": {"query": "FRU*", "fields": ["dyn_txt"]}}),
            ),
        ] {
            let expected = if label == "simple_query_string text prefix" {
                s(&["1", "2"])
            } else {
                s(&["1"])
            };
            assert_eq!(
                ids(app_ref, "nest_qs", &query).await,
                expected,
                "{phase} {label}: {query}"
            );
        }
        // Negative control: unnormalized keyword stays case-sensitive for simple_query_string prefix.
        assert_eq!(
            ids(
                app_ref,
                "nest_qs",
                &json!({"simple_query_string": {"query": "FRU*", "fields": ["raw_kw"]}})
            )
            .await,
            s(&[]),
            "{phase} unnormalized keyword prefix stays case-sensitive"
        );
    })
    .await;
}

#[tokio::test]
async fn collapse_fields_wildcards_and_date_shaped_sort_in_every_phase() {
    let (app, _dir) = app().await;
    ok(
        &app,
        "PUT",
        "/col_a",
        json!({
            "settings": {
                "analysis": {
                    "normalizer": {
                        "fold_lower": {"type": "custom", "filter": ["lowercase", "asciifolding"]}
                    }
                }
            },
            "mappings": {
                "properties": {
                    "tag": {"type": "keyword", "normalizer": "fold_lower"},
                    "ts_kw": {"type": "keyword", "normalizer": "lowercase"},
                    "title": {
                        "type": "text",
                        "fields": {
                            "raw": {"type": "keyword", "normalizer": "fold_lower"}
                        }
                    },
                    "meta": {
                        "properties": {
                            "owner": {"type": "keyword", "normalizer": "fold_lower"}
                        }
                    }
                }
            }
        }),
    )
    .await;

    let docs = [
        (
            "1",
            json!({"tag": "BÀR", "ts_kw": "2024-01-02T00:00:00Z", "title": "Fruit", "meta": {"owner": "BÀR"}}),
        ),
        (
            "2",
            json!({"tag": "bar", "ts_kw": "2024-01-01T00:00:00Z", "title": "FRUIT", "meta": {"owner": "bar"}}),
        ),
        (
            "3",
            json!({"tag": "Baz", "ts_kw": "2024-01-03T00:00:00Z", "title": "Apple", "meta": {"owner": "Baz"}}),
        ),
    ];

    let app_ref = &app;
    in_every_phase(app_ref, "col_a", &docs, |phase| async move {
        // P1 #5: collapse on dotted normalized keyword (`meta.owner`) with inner_hits.
        let body = ok(
            app_ref,
            "POST",
            "/col_a/_search",
            json!({
                "sort": [{"_id": "asc"}],
                "collapse": {
                    "field": "meta.owner",
                    "inner_hits": {"name": "by_owner", "size": 5, "sort": [{"title.raw": "asc"}]}
                }
            }),
        )
        .await;
        let hits = body["hits"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "{phase} dotted collapse groups: {body}");
        assert_eq!(hits[0]["fields"]["meta.owner"], json!(["bar"]), "{phase}");
        assert_eq!(hits[1]["fields"]["meta.owner"], json!(["baz"]), "{phase}");
        assert_eq!(
            hits[0]["inner_hits"]["by_owner"]["hits"]["total"]["value"],
            json!(2),
            "{phase} inner_hits group size: {body}"
        );

        // P1 #5: collapse on normalized multi-field (`title.raw`).
        let body = ok(
            app_ref,
            "POST",
            "/col_a/_search",
            json!({
                "sort": [{"_id": "asc"}],
                "collapse": {"field": "title.raw"}
            }),
        )
        .await;
        let hits = body["hits"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "{phase} multi-field collapse groups: {body}");
        assert_eq!(hits[0]["fields"]["title.raw"], json!(["fruit"]), "{phase}");
        assert_eq!(hits[1]["fields"]["title.raw"], json!(["apple"]), "{phase}");

        // P1 #6: `fields: ["title.*"]` and `fields: ["*.raw"]` return normalized `title.raw`.
        for pat in ["title.*", "*.raw"] {
            let body = ok(
                app_ref,
                "POST",
                "/col_a/_search",
                json!({"query": {"ids": {"values": ["2"]}}, "fields": [pat]}),
            )
            .await;
            assert_eq!(
                body["hits"]["hits"][0]["fields"]["title.raw"],
                json!(["fruit"]),
                "{phase} fields {pat}: {body}"
            );
        }

        // P1 #7: date-shaped strings in a normalized keyword field sort and
        // paginate via `search_after` as normalized strings before and after flush.
        let page1 = ok(
            app_ref,
            "POST",
            "/col_a/_search",
            json!({"size": 1, "sort": [{"ts_kw": "asc"}, {"_id": "asc"}]}),
        )
        .await;
        assert_eq!(page1["hits"]["hits"][0]["_id"], json!("2"), "{phase}");
        assert_eq!(
            page1["hits"]["hits"][0]["sort"][0],
            json!("2024-01-01t00:00:00z"),
            "{phase} date-like normalized keyword sort must stay string: {page1}"
        );
        let cursor = page1["hits"]["hits"][0]["sort"].clone();
        let page2 = ok(
            app_ref,
            "POST",
            "/col_a/_search",
            json!({
                "size": 2,
                "sort": [{"ts_kw": "asc"}, {"_id": "asc"}],
                "search_after": cursor
            }),
        )
        .await;
        let p2_ids: Vec<&str> = page2["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            p2_ids,
            vec!["1", "3"],
            "{phase} search_after page2: {page2}"
        );
    })
    .await;

    // P1 #6: pure dynamic-template index (`fields: ["*"]`) must NOT leak a fake
    // unnormalized `color.keyword` sibling alongside the normalized keyword `color`.
    ok(
        &app,
        "PUT",
        "/dyn_star",
        json!({
            "mappings": {
                "dynamic_templates": [{
                    "all_lower": {
                        "match_mapping_type": "string",
                        "mapping": {"type": "keyword", "normalizer": "lowercase"}
                    }
                }]
            }
        }),
    )
    .await;
    bulk(&app, "dyn_star", &[("1", json!({"color": "Red"}))]).await;
    ok(&app, "POST", "/dyn_star/_refresh", json!(null)).await;
    let star_resp = ok(&app, "POST", "/dyn_star/_search", json!({"fields": ["*"]})).await;
    let fmap = star_resp["hits"]["hits"][0]["fields"].as_object().unwrap();
    assert_eq!(fmap.get("color"), Some(&json!(["red"])), "{star_resp}");
    assert!(
        !fmap.contains_key("color.keyword"),
        "dynamic-template keyword must not leak synthetic color.keyword: {star_resp}"
    );
    ok(&app, "POST", "/dyn_star/_flush", json!(null)).await;
    let star_flushed = ok(&app, "POST", "/dyn_star/_search", json!({"fields": ["*"]})).await;
    let fmap_flushed = star_flushed["hits"]["hits"][0]["fields"]
        .as_object()
        .unwrap();
    assert_eq!(
        fmap_flushed.get("color"),
        Some(&json!(["red"])),
        "{star_flushed}"
    );
    assert!(
        !fmap_flushed.contains_key("color.keyword"),
        "flushed dynamic-template keyword must not leak color.keyword: {star_flushed}"
    );

    // P1 #5: cross-index collapse deduplicates normalized keys across indices.
    ok(
        &app,
        "PUT",
        "/col_b",
        json!({
            "settings": {
                "analysis": {
                    "normalizer": {
                        "fold_lower": {"type": "custom", "filter": ["lowercase", "asciifolding"]}
                    }
                }
            },
            "mappings": {
                "properties": {
                    "meta": {
                        "properties": {
                            "owner": {"type": "keyword", "normalizer": "fold_lower"}
                        }
                    }
                }
            }
        }),
    )
    .await;
    bulk(&app, "col_b", &[("10", json!({"meta": {"owner": "BAR"}}))]).await;
    ok(&app, "POST", "/col_b/_refresh", json!(null)).await;
    let cross = ok(
        &app,
        "POST",
        "/col_a,col_b/_search",
        json!({
            "sort": [{"_id": "asc"}],
            "collapse": {
                "field": "meta.owner",
                "inner_hits": {"name": "all_owners", "size": 5}
            }
        }),
    )
    .await;
    let cross_hits = cross["hits"]["hits"].as_array().unwrap();
    assert_eq!(
        cross_hits.len(),
        2,
        "cross-index collapse must merge BAR/BÀR/bar: {cross}"
    );
    assert_eq!(cross_hits[0]["fields"]["meta.owner"], json!(["bar"]));
    assert_eq!(
        cross_hits[0]["inner_hits"]["all_owners"]["hits"]["total"]["value"],
        json!(3),
        "cross-index inner_hits must absorb sibling index member: {cross}"
    );
}

#[tokio::test]
async fn dynamic_templates_heterogeneous_nested_put_mapping_and_spec_edge_cases() {
    let (app, _dir) = app().await;
    // P1 #8: dynamic template specifying `normalizer` without `type: keyword`
    // (omitted or `{dynamic_type}`) must be refused cleanly at index creation.
    for (idx_name, mapping) in [
        ("bad_omit_type", json!({"normalizer": "lowercase"})),
        (
            "bad_dyn_type",
            json!({"type": "{dynamic_type}", "normalizer": "lowercase"}),
        ),
    ] {
        let (status, out) = req(
            &app,
            "PUT",
            &format!("/{idx_name}"),
            json!({"mappings": {"dynamic_templates": [{"t": {
                "match_mapping_type": "string",
                "mapping": mapping
            }}]}})
            .to_string(),
        )
        .await;
        assert_mapper_refusal(status, &out, "unknown parameter [normalizer]");
    }

    // P1 #10: PUT _mapping adds/merges dynamic_templates into es_mapping.json.
    ok(
        &app,
        "PUT",
        "/dyn_spec",
        json!({
            "settings": {
                "analysis": {
                    "normalizer": {
                        "fold_lower": {"type": "custom", "filter": ["lowercase", "asciifolding"]}
                    }
                }
            },
            "mappings": {
                "properties": {
                    "items": {"type": "nested"},
                    "locked": {"type": "object", "dynamic": false}
                }
            }
        }),
    )
    .await;
    ok(
        &app,
        "PUT",
        "/dyn_spec/_mapping",
        json!({
            "dynamic_templates": [
                {
                    "nested_codes": {
                        "match_mapping_type": "string",
                        "path_match": "items.*",
                        "mapping": {"type": "keyword", "normalizer": "fold_lower"}
                    }
                },
                {
                    "key_placeholder": {
                        "match_mapping_type": "string",
                        "match": "label_*",
                        "mapping": {
                            "type": "text",
                            "fields": {
                                "{name}_norm": {"type": "keyword", "normalizer": "lowercase"}
                            }
                        }
                    }
                },
                {
                    "flat_bag": {
                        "match_mapping_type": "object",
                        "match": "bag_*",
                        "mapping": {"type": "flattened"}
                    }
                },
                {
                    "anchored_regex": {
                        "match_pattern": "regex",
                        "match": "exact",
                        "mapping": {"type": "keyword", "normalizer": "lowercase"}
                    }
                },
                {
                    "literal_qmark": {
                        "match": "a?b",
                        "mapping": {"type": "keyword", "normalizer": "lowercase"}
                    }
                }
            ]
        }),
    )
    .await;

    // P1 #9: heterogeneous nested array where `code` appears only on element 1!
    let docs = [(
        "1",
        json!({
            "items": [{"id": 1}, {"id": 2, "code": "BÀR"}],
            "locked": {"hidden": "ShouldNotBeMapped"},
            "label_a": "Hello",
            "bag_extra": {"k": "v"},
            "exact": "MATCH",
            "not_exact": "KEEP_TEXT",
            "axb": "NOT_QMARK"
        }),
    )];

    let app_ref = &app;
    in_every_phase(app_ref, "dyn_spec", &docs, |phase| async move {
        assert_eq!(
            ids(
                app_ref,
                "dyn_spec",
                &json!({"nested": {"path": "items", "query": {"term": {"items.code": "BAR"}}}})
            )
            .await,
            s(&["1"]),
            "{phase} heterogeneous nested array element 1 normalized"
        );
        assert_eq!(
            ids(
                app_ref,
                "dyn_spec",
                &json!({"term": {"label_a.label_a_norm": "HELLO"}})
            )
            .await,
            s(&["1"]),
            "{phase} placeholder in multi-field key"
        );
        assert_eq!(
            ids(app_ref, "dyn_spec", &json!({"term": {"exact": "match"}})).await,
            s(&["1"]),
            "{phase} anchored regex matches exact"
        );
    })
    .await;

    let mapping = ok(&app, "GET", "/dyn_spec/_mapping", json!(null)).await;
    assert_eq!(
        mapping.pointer("/dyn_spec/mappings/properties/items/properties/code/normalizer"),
        Some(&json!("fold_lower")),
        "{mapping}"
    );
    assert!(
        mapping
            .pointer("/dyn_spec/mappings/properties/locked/properties")
            .is_none(),
        "static dynamic:false must prevent dynamic children: {mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn_spec/mappings/properties/bag_extra/type"),
        Some(&json!("flattened")),
        "{mapping}"
    );
    assert!(
        mapping
            .pointer("/dyn_spec/mappings/properties/bag_extra/properties")
            .is_none(),
        "flattened template must not emit sub-properties: {mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn_spec/mappings/properties/exact/normalizer"),
        Some(&json!("lowercase")),
        "{mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn_spec/mappings/properties/not_exact/type"),
        Some(&json!("text")),
        "anchored regex `exact` must not match `not_exact`: {mapping}"
    );
    assert_eq!(
        mapping.pointer("/dyn_spec/mappings/properties/axb/type"),
        Some(&json!("text")),
        "simple match `a?b` must treat `?` literally and not match `axb`: {mapping}"
    );
}
