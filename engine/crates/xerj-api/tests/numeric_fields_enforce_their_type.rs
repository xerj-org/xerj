//! Numeric and boolean fields enforce their declared type, from the outside —
//! issue #781.
//!
//! A field mapped `integer` used to accept and keep literally anything: `1.9`
//! stayed `1.9`, `9999999999` stayed exact, `"abc"` was stored as a string,
//! and even `{"bad": "x"}` was indexed. Every one of those was a `201`.
//!
//! The worst of it is not leniency, it is wrong hits. With `1.9` sitting in an
//! `integer` field, `range {"i": {"gte": 1.5}}` MATCHED in xerj and would not
//! in ES (which indexes the truncated `1`), while `term {"i": 1}` matched in
//! ES and returned nothing here. Same mapping, same document, same query,
//! different answers — `wrong_results_the_declared_type_used_to_produce`
//! below is that case, pinned.
//!
//! ES 8.x semantics, with `coerce` defaulting to `true`:
//!
//! * `1.9` → `1` (truncated toward zero), `"5"` → `5`, `"1.9"` → `1`
//! * `"true"`/`"false"` → the booleans
//! * out of range for the declared width → **400**
//! * unparseable string, object, array, `"yes"` in a boolean → **400**
//! * `"coerce": false` additionally refuses the decimal part and the string
//!
//! Every write path that knows the typed schema is covered here: `PUT
//! /{index}/_doc/{id}`, `POST /{index}/_doc` (auto-id), `PUT
//! /{index}/_create/{id}`, and `_bulk` — including the turbo batch path
//! auto-id bulk takes, which never parses the document body and so had to be
//! reached by a partition-time check rather than the per-item loop.
//!
//! Elasticsearch is referenced for semantics only. It is AGPL-3.0/SSPL-1.0/
//! Elastic-2.0 licensed and no code from it is reproduced here.

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

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn json_req(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn bulk(app: &axum::Router, path: &str, ndjson: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::post(path)
            .header("content-type", "application/x-ndjson")
            .body(Body::from(ndjson.to_owned()))
            .expect("request"),
    )
    .await
}

/// `i` integer, `l` long, `f` float, `b` boolean, `strict` integer with
/// `coerce: false`, `lenient` integer with `ignore_malformed: true`, plus a
/// nested object and a `keyword` this check must not touch.
async fn typed_index(name: &str) -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (status, _) = json_req(
        &app,
        "PUT",
        &format!("/{name}"),
        json!({
            "mappings": {
                "properties": {
                    "i": { "type": "integer" },
                    "l": { "type": "long" },
                    "f": { "type": "float" },
                    "b": { "type": "boolean" },
                    "strict": { "type": "integer", "coerce": false },
                    "lenient": { "type": "integer", "ignore_malformed": true },
                    "k": { "type": "keyword" },
                    "inner": { "properties": { "n": { "type": "integer" } } }
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mapping should be accepted");
    (app, dir)
}

async fn put_doc(app: &axum::Router, index: &str, id: &str, doc: Value) -> (StatusCode, Value) {
    json_req(app, "PUT", &format!("/{index}/_doc/{id}"), doc).await
}

/// Flush the memtable into a segment.
///
/// This matters for the query-side tests at the bottom of this file: while a
/// document is memtable-resident, `term`/`terms` are answered by the doc-values
/// fast path, which compares STRINGIFIED values and so is blind to the
/// boolean-versus-`"boolean"` spelling. It is the flushed segment path — a
/// doc-values prefilter refined by an exact re-test of `_source` — that
/// distinguishes them, which is why the ES-YAML case that caught this
/// (`search/390_doc_values_search.yml`) does `indices.refresh` before it
/// queries. A test that skips the refresh passes on the broken code.
async fn refresh(app: &axum::Router, index: &str) {
    let (status, body) = json_req(app, "POST", &format!("/{index}/_refresh"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "refresh should succeed: {body}");
}

async fn search(app: &axum::Router, index: &str, query: Value) -> Value {
    let (status, body) = json_req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({ "query": query }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search should succeed: {body}");
    body
}

fn hit_count(body: &Value) -> u64 {
    body["hits"]["total"]["value"].as_u64().unwrap_or(0)
}

fn is_parsing_rejection(status: StatusCode, body: &Value, field: &str) -> bool {
    status == StatusCode::BAD_REQUEST
        && body["error"]["type"] == "document_parsing_exception"
        && body["error"]["reason"]
            .as_str()
            .map(|r| r.contains(&format!("failed to parse field [{field}]")))
            .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// The wrong-results case that makes this a bug and not a style preference.
// ─────────────────────────────────────────────────────────────────────────────

/// `1.9` into an `integer` field. ES truncates it to `1` at ingest, so
/// `range {gte: 1.5}` misses and `term {i: 1}` hits. Before the fix xerj
/// stored `1.9` verbatim and answered both queries the other way round.
#[tokio::test]
async fn wrong_results_the_declared_type_used_to_produce() {
    let (app, _dir) = typed_index("t").await;

    let (status, _) = put_doc(&app, "t", "1", json!({ "i": 1.9 })).await;
    assert_eq!(status, StatusCode::CREATED);

    // The stored value is the integer ES would have indexed.
    let (_, doc) = get(&app, "/t/_doc/1").await;
    assert_eq!(doc["_source"]["i"], json!(1), "1.9 should truncate to 1");

    let gte = search(&app, "t", json!({ "range": { "i": { "gte": 1.5 } } })).await;
    assert_eq!(
        hit_count(&gte),
        0,
        "an integer field holding ES's 1 must not match gte 1.5: {gte}"
    );

    let term = search(&app, "t", json!({ "term": { "i": 1 } })).await;
    assert_eq!(
        hit_count(&term),
        1,
        "term 1 must find the truncated value: {term}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Coercion: the values ES accepts and rewrites.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn numeric_strings_and_boolean_spellings_are_coerced() {
    let (app, _dir) = typed_index("t").await;

    let (status, _) = put_doc(
        &app,
        "t",
        "1",
        json!({ "i": "5", "l": "-7", "f": "2.5", "b": "true", "inner": { "n": "3" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, doc) = get(&app, "/t/_doc/1").await;
    assert_eq!(doc["_source"]["i"], json!(5));
    assert_eq!(doc["_source"]["l"], json!(-7));
    assert_eq!(doc["_source"]["f"], json!(2.5));
    assert_eq!(doc["_source"]["b"], json!(true));
    assert_eq!(doc["_source"]["inner"]["n"], json!(3), "nested too");

    // Coerced, therefore queryable as the number it now is.
    let term = search(&app, "t", json!({ "term": { "i": 5 } })).await;
    assert_eq!(hit_count(&term), 1, "coerced string is numerically indexed");
}

#[tokio::test]
async fn arrays_are_coerced_element_wise() {
    let (app, _dir) = typed_index("t").await;

    let (status, _) = put_doc(&app, "t", "1", json!({ "i": [1.9, "3", 4] })).await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, doc) = get(&app, "/t/_doc/1").await;
    assert_eq!(doc["_source"]["i"], json!([1, 3, 4]));
}

#[tokio::test]
async fn values_that_already_match_the_type_are_left_alone() {
    let (app, _dir) = typed_index("t").await;

    // `2.0` is already the integer ES indexes — no `_source` churn — and a
    // `keyword` field is not this check's business at all.
    let (status, _) = put_doc(
        &app,
        "t",
        "1",
        json!({ "i": 2.0, "l": 42, "b": false, "k": 7, "unmapped": { "any": "shape" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, doc) = get(&app, "/t/_doc/1").await;
    assert_eq!(doc["_source"]["l"], json!(42));
    assert_eq!(doc["_source"]["b"], json!(false));
    assert_eq!(doc["_source"]["k"], json!(7), "keyword is not coerced");
    assert_eq!(doc["_source"]["unmapped"], json!({ "any": "shape" }));
}

// ─────────────────────────────────────────────────────────────────────────────
// Rejection: the values ES refuses outright.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_non_numeric_string_is_refused() {
    let (app, _dir) = typed_index("t").await;
    let (status, body) = put_doc(&app, "t", "1", json!({ "i": "abc" })).await;
    assert!(
        is_parsing_rejection(status, &body, "i"),
        "expected 400 document_parsing_exception, got {status}: {body}"
    );
    assert!(
        body["error"]["caused_by"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("abc"),
        "the caused_by should name the offending input: {body}"
    );
    // Nothing was written.
    assert_eq!(get(&app, "/t/_doc/1").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_out_of_range_value_is_refused_even_though_it_is_an_integer() {
    let (app, _dir) = typed_index("t").await;

    let (status, body) = put_doc(&app, "t", "1", json!({ "i": 9999999999i64 })).await;
    assert!(
        is_parsing_rejection(status, &body, "i"),
        "9999999999 does not fit an integer: {status} {body}"
    );

    // The same number in a `long` is perfectly fine.
    let (status, _) = put_doc(&app, "t", "2", json!({ "l": 9999999999i64 })).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_boolean_takes_only_true_and_false() {
    let (app, _dir) = typed_index("t").await;

    let (status, body) = put_doc(&app, "t", "1", json!({ "b": "yes" })).await;
    assert!(
        is_parsing_rejection(status, &body, "b"),
        "\"yes\" is not a boolean: {status} {body}"
    );

    let (status, body) = put_doc(&app, "t", "2", json!({ "b": 1 })).await;
    assert!(
        is_parsing_rejection(status, &body, "b"),
        "1 is not a boolean in ES 8.x: {status} {body}"
    );
}

/// The comment on the issue: an OBJECT went into an integer field and the
/// document was indexed, so the declared type enforced nothing at all.
#[tokio::test]
async fn an_object_is_not_a_number() {
    let (app, _dir) = typed_index("t").await;
    let (status, body) = put_doc(&app, "t", "1", json!({ "i": { "bad": "x" } })).await;
    assert!(
        is_parsing_rejection(status, &body, "i"),
        "an object in an integer field must be refused: {status} {body}"
    );
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("bad=x"),
        "the preview should render the object ES-style: {body}"
    );
}

#[tokio::test]
async fn a_bad_nested_field_is_named_by_its_dotted_path() {
    let (app, _dir) = typed_index("t").await;
    let (status, body) = put_doc(&app, "t", "1", json!({ "inner": { "n": "abc" } })).await;
    assert!(
        is_parsing_rejection(status, &body, "inner.n"),
        "expected the dotted path in the reason: {status} {body}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The two mapping parameters that change the answer.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn coerce_false_refuses_what_the_default_would_fix() {
    let (app, _dir) = typed_index("t").await;

    for bad in [json!(1.9), json!("5")] {
        let (status, body) = put_doc(&app, "t", "1", json!({ "strict": bad })).await;
        assert!(
            is_parsing_rejection(status, &body, "strict"),
            "coerce:false should refuse {bad}: {status} {body}"
        );
    }

    // An in-range integer still goes in.
    let (status, _) = put_doc(&app, "t", "1", json!({ "strict": 3 })).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn ignore_malformed_still_wins_over_rejection() {
    let (app, _dir) = typed_index("t").await;

    // The field asked for the bad value to be dropped into `_ignored`, not for
    // the document to be refused — that contract must survive this change.
    let (status, _) = put_doc(&app, "t", "1", json!({ "lenient": "abc", "i": 4 })).await;
    assert_eq!(status, StatusCode::CREATED, "ignore_malformed must not 400");

    let (_, doc) = get(&app, "/t/_doc/1").await;
    assert_eq!(doc["_source"]["i"], json!(4));
    assert!(
        doc["_source"].get("lenient").is_none(),
        "the malformed value should have been dropped: {doc}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Every other write path agrees with `PUT /_doc/{id}`.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_auto_id_and_create_endpoints_enforce_the_same_rules() {
    let (app, _dir) = typed_index("t").await;

    let (status, body) = json_req(&app, "POST", "/t/_doc", json!({ "i": "abc" })).await;
    assert!(
        is_parsing_rejection(status, &body, "i"),
        "POST /_doc must refuse it too: {status} {body}"
    );

    let (status, body) = json_req(&app, "PUT", "/t/_create/9", json!({ "b": "yes" })).await;
    assert!(
        is_parsing_rejection(status, &body, "b"),
        "PUT /_create/{{id}} must refuse it too: {status} {body}"
    );

    // …and coerce on the happy path.
    let (status, _) = json_req(&app, "PUT", "/t/_create/10", json!({ "i": 1.9 })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, doc) = get(&app, "/t/_doc/10").await;
    assert_eq!(doc["_source"]["i"], json!(1));
}

/// `_bulk` reports per item and isolates failures — including on the auto-id
/// turbo batch path, which never parses the document body.
#[tokio::test]
async fn bulk_rejects_per_item_and_keeps_the_good_ones() {
    let (app, _dir) = typed_index("t").await;

    let ndjson = concat!(
        "{\"index\":{\"_id\":\"ok\"}}\n{\"i\":1.9}\n",
        "{\"index\":{\"_id\":\"bad-str\"}}\n{\"i\":\"abc\"}\n",
        "{\"index\":{\"_id\":\"bad-range\"}}\n{\"i\":9999999999}\n",
        "{\"index\":{\"_id\":\"bad-obj\"}}\n{\"i\":{\"bad\":\"x\"}}\n",
        "{\"index\":{\"_id\":\"bad-bool\"}}\n{\"b\":\"yes\"}\n",
        "{\"create\":{\"_id\":\"ok2\"}}\n{\"i\":\"7\"}\n",
    );
    let (status, body) = bulk(&app, "/t/_bulk", ndjson).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["errors"], json!(true), "{body}");

    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 6);
    let code = |n: usize| -> u64 {
        items[n]
            .as_object()
            .and_then(|o| o.values().next())
            .and_then(|v| v["status"].as_u64())
            .unwrap_or(0)
    };
    assert_eq!(code(0), 201, "the coercible one lands: {body}");
    for (n, what) in [(1, "string"), (2, "range"), (3, "object"), (4, "boolean")] {
        assert_eq!(code(n), 400, "bad {what} should be a per-item 400: {body}");
    }
    assert_eq!(code(5), 201, "create coerces too: {body}");

    // Only the two good documents exist, with ES's values.
    let (_, ok) = get(&app, "/t/_doc/ok").await;
    assert_eq!(ok["_source"]["i"], json!(1));
    let (_, ok2) = get(&app, "/t/_doc/ok2").await;
    assert_eq!(ok2["_source"]["i"], json!(7));
    assert_eq!(get(&app, "/t/_doc/bad-str").await.0, StatusCode::NOT_FOUND);

    let (_, count) = get(&app, "/t/_count").await;
    assert_eq!(count["count"], json!(2), "{count}");
}

/// Auto-id bulk is the turbo batch path: the doc body is never parsed and the
/// whole group is appended in one shot, so it needed its own check. Without
/// it, every bad value in this batch was a silent 201.
#[tokio::test]
async fn the_bulk_turbo_path_is_not_a_way_around_the_check() {
    let (app, _dir) = typed_index("t").await;

    let ndjson = concat!(
        "{\"index\":{}}\n{\"i\":1.9}\n",
        "{\"index\":{}}\n{\"i\":\"abc\"}\n",
        "{\"index\":{}}\n{\"i\":\"6\"}\n",
    );
    let (status, body) = bulk(&app, "/t/_bulk", ndjson).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["errors"], json!(true), "{body}");

    let (_, count) = get(&app, "/t/_count").await;
    assert_eq!(count["count"], json!(2), "only the two valid docs: {count}");

    // Both survivors were coerced, so both are numerically queryable.
    let one = search(&app, "t", json!({ "term": { "i": 1 } })).await;
    assert_eq!(hit_count(&one), 1, "{one}");
    let six = search(&app, "t", json!({ "term": { "i": 6 } })).await;
    assert_eq!(hit_count(&six), 1, "{six}");
}

/// An index with no numeric or boolean field skips the check entirely — the
/// gate that keeps the turbo path free must not change behaviour for it.
#[tokio::test]
async fn an_index_with_no_numeric_mapping_is_untouched() {
    let (app, _dir) = app().await;
    let (status, _) = json_req(
        &app,
        "PUT",
        "/plain",
        json!({ "mappings": { "properties": { "msg": { "type": "text" } } } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = put_doc(&app, "plain", "1", json!({ "msg": "hi", "n": "abc" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, doc) = get(&app, "/plain/_doc/1").await;
    assert_eq!(doc["_source"]["n"], json!("abc"), "unmapped stays verbatim");
}

// ─────────────────────────────────────────────────────────────────────────────
// The query side of the same coercion. Rewriting `_source` moves the STORED
// spelling, so a query operand written the pre-coercion way has to keep
// matching — this is the case CI caught in
// `tests/es-compat-yaml/yaml/search/390_doc_values_search.yml`, and the reason
// the first cut of this change regressed conformance from 0 failed to 1.
// ─────────────────────────────────────────────────────────────────────────────

/// A document written `{"b": "true"}` is now STORED as `{"b": true}`. ES accepts
/// either spelling in a `term`/`terms` operand against a `boolean` field
/// (`terms {b: ["true"]}` is literally what the ES YAML suite asserts), so both
/// must still find it. Pre-fix, `terms` with the string operand found nothing:
/// `terms` verifies each admitted doc with exact JSON equality, and `true` is
/// not `"true"`.
#[tokio::test]
async fn a_query_in_the_pre_coercion_spelling_still_matches_the_coerced_document() {
    let (app, _dir) = typed_index("t").await;

    let (status, _) = put_doc(&app, "t", "1", json!({ "b": "true", "i": "5" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, doc) = get(&app, "/t/_doc/1").await;
    assert_eq!(
        doc["_source"]["b"],
        json!(true),
        "ingest coerced the spelling"
    );
    assert_eq!(doc["_source"]["i"], json!(5));
    refresh(&app, "t").await;

    for query in [
        json!({ "terms": { "b": ["true"] } }),
        json!({ "terms": { "b": [true] } }),
        json!({ "term": { "b": "true" } }),
        json!({ "term": { "b": true } }),
        json!({ "terms": { "b": ["false", "true"] } }),
        json!({ "terms": { "i": ["5"] } }),
        json!({ "terms": { "i": [5] } }),
    ] {
        let body = search(&app, "t", query.clone()).await;
        assert_eq!(
            hit_count(&body),
            1,
            "both spellings of {query} must find the coerced document: {body}"
        );
    }

    // Tolerance, not blindness: the other boolean still must not match.
    for query in [
        json!({ "terms": { "b": ["false"] } }),
        json!({ "term": { "b": false } }),
        json!({ "terms": { "b": ["yes"] } }),
        json!({ "terms": { "i": ["6"] } }),
    ] {
        let body = search(&app, "t", query.clone()).await;
        assert_eq!(hit_count(&body), 0, "{query} must not match: {body}");
    }
}

/// An index that spans the upgrade holds BOTH representations of the same
/// logical value: documents written before this change keep `"true"` / `"5"`,
/// documents written after hold `true` / `5`. One query must not silently see
/// half the index. `_update` is the shortest way to plant the old spelling —
/// it merges into a document that was already validated on write and is
/// deliberately not re-validated, which is exactly the shape a pre-upgrade
/// document has.
#[tokio::test]
async fn an_index_holding_both_spellings_answers_every_query_with_both_docs() {
    let (app, _dir) = typed_index("t").await;

    // Post-upgrade document: coerced on the way in.
    let (status, _) = put_doc(&app, "t", "new", json!({ "b": "true", "i": "5" })).await;
    assert_eq!(status, StatusCode::CREATED);

    // Pre-upgrade document: the raw spelling, planted past the check.
    let (status, _) = put_doc(&app, "t", "legacy", json!({ "k": "a" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = json_req(
        &app,
        "POST",
        "/t/_update/legacy",
        json!({ "doc": { "b": "true", "i": "5" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update should apply: {body}");

    refresh(&app, "t").await;

    let (_, legacy) = get(&app, "/t/_doc/legacy").await;
    let (_, fresh) = get(&app, "/t/_doc/new").await;
    assert_eq!(
        legacy["_source"]["b"],
        json!("true"),
        "the legacy spelling is what makes this test meaningful"
    );
    assert_eq!(fresh["_source"]["b"], json!(true));

    for query in [
        json!({ "terms": { "b": ["true"] } }),
        json!({ "terms": { "b": [true] } }),
        json!({ "term": { "b": "true" } }),
        json!({ "term": { "b": true } }),
        json!({ "terms": { "i": ["5"] } }),
        json!({ "terms": { "i": [5] } }),
        json!({ "term": { "i": 5 } }),
    ] {
        let body = search(&app, "t", query.clone()).await;
        assert_eq!(
            hit_count(&body),
            2,
            "{query} must see both spellings in one index: {body}"
        );
    }
}
