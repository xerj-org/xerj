//! The term-level family obeys ONE case rule, and `_count` agrees with
//! `_search` — #362, from the outside.
//!
//! The report was "`term` normalises case but `prefix`/`wildcard` do not": a
//! document you can find with `term title=TestSegmentReader.java` is invisible
//! to `prefix title=Test`. The attribution turned out to be backwards in a way
//! that made it worse. `prefix`/`wildcard` are the ES-correct half — a
//! multi-term query is not analysed, Lucene builds the automaton straight off
//! the query bytes (`PrefixQuery.toAutomaton`, PrefixQuery.java:44) — and
//! `term` was not normalising at all. What normalised was the `_count`
//! shortcut, which retried the FTS term-dictionary lookup with a lowercased
//! spelling of a term the caller never wrote, while the hit-materialising path
//! stayed on the raw one. So `_count` reported a document that `_search` could
//! not produce, and it did so for every `term` on a `text` field, cased or not.
//!
//! Two invariants are pinned here.
//!
//! 1. **count == hits**, Lucene's own `QueryUtils.checkCount`
//!    (QueryUtils.java:680) invariant, asserted across the whole term-level
//!    family in both cases on both a `text` and a `keyword` field. Lucene gets
//!    this for free because `IndexSearcher.count` (IndexSearcher.java:495)
//!    derives its answer from the SAME rewritten query and Weight `search()`
//!    uses; its only shortcuts are algebraic rewrites, never a re-spelling of
//!    the term. A count no `_search` can reproduce is worse than a slow one.
//!
//! 2. **`case_insensitive` is honoured**, not accepted and dropped. It is the
//!    documented escape hatch for exactly the reporter's case — the ES
//!    parameter backed in Lucene by `Automata.makeCaseInsensitiveString`
//!    (Automata.java:573), an opt-in rather than a default — and the parser
//!    used to parse it and throw it away, so the one correct answer to the
//!    problem silently returned zero.
//!
//! Elasticsearch and Lucene are referenced for semantics only.

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

/// `title` — text (analyzed, so the term dictionary is lowercased).
/// `code`  — keyword (whole-value, stored as written).
///
/// The corpus is the reporter's: CamelCase source-file names, plus one
/// ordinary prose document to cover the all-lowercase `term` case where the
/// old count shortcut's lowercase retry was a no-op and the dictionary hit was
/// real — that one lied too.
///
/// **The flush is load-bearing.** Both halves of #362 live on the segment
/// side: the count shortcut only reaches its FTS term-dictionary fallback for
/// a field with no doc-values column (a `text` field), and the multi-term
/// queries only reach the FST expansion once there is a segment to expand
/// against. A memtable-resident corpus answers every one of these from the
/// stored-document scan and reproduces neither. The reporter had 11,450
/// flushed documents; this has four.
async fn app_with_corpus() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (status, body) = json_req(
        &app,
        "PUT",
        "/refs",
        json!({
            "mappings": {
                "properties": {
                    "title": { "type": "text" },
                    "code":  { "type": "keyword" }
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create index failed: {body}");

    for (id, title, code) in [
        (1, "TestSegmentReader.java", "TestSegmentReader"),
        (2, "TestIndexWriter.java", "TestIndexWriter"),
        (3, "HnswGraphBuilder.java", "HnswGraphBuilder"),
        (4, "the quick brown fox", "quick-brown-fox"),
    ] {
        let (status, body) = json_req(
            &app,
            "PUT",
            &format!("/refs/_doc/{id}?refresh=true"),
            json!({ "title": title, "code": code }),
        )
        .await;
        assert!(
            status.is_success(),
            "index doc {id} failed: {status} {body}"
        );
    }

    let (status, body) = json_req(&app, "POST", "/refs/_flush", json!({})).await;
    assert!(status.is_success(), "flush failed: {status} {body}");
    let (status, body) = json_req(&app, "POST", "/refs/_refresh", json!({})).await;
    assert!(status.is_success(), "refresh failed: {status} {body}");

    // Guard the guard: if the flush stopped producing a segment this suite
    // would keep passing while testing nothing, because the stored-doc scan
    // answers everything and is not where either bug lives.
    let (status, stats) = send(
        &app,
        Request::get("/refs/_stats")
            .body(Body::empty())
            .expect("req"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_stats failed: {stats}");
    let segments = stats["_all"]["primaries"]["segments"]["count"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        segments > 0,
        "corpus must be segment-resident for this suite to test anything: {stats}"
    );

    (app, dir)
}

/// Run one query through `_count` and `_search` and return
/// `(count, search_total, returned_hits)`.
async fn count_and_search(app: &axum::Router, query: &Value) -> (u64, u64, usize) {
    let (status, counted) = json_req(app, "POST", "/refs/_count", json!({ "query": query })).await;
    assert_eq!(status, StatusCode::OK, "_count failed: {counted}");
    let count = counted["count"].as_u64().expect("count field");

    let (status, found) = json_req(
        app,
        "POST",
        "/refs/_search",
        json!({ "query": query, "size": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_search failed: {found}");
    let total = found["hits"]["total"]["value"]
        .as_u64()
        .expect("hits.total.value");
    let returned = found["hits"]["hits"].as_array().expect("hits array").len();

    (count, total, returned)
}

/// `_count` must never claim a document `_search` cannot hand back.
///
/// Before the fix the shortcut retried the term-dictionary lookup lowercased,
/// so `{"term":{"title":"testsegmentreader.java"}}` counted 1 against a
/// `_search` total of 0 — and so did the already-lowercase
/// `{"term":{"title":"quick"}}`, where the retry changes nothing and the
/// dictionary hit is genuine but the raw-`_source` comparison that
/// materialises hits still cannot see it.
#[tokio::test]
async fn count_never_exceeds_the_hits_search_can_return() {
    let (app, _dir) = app_with_corpus().await;

    let queries = [
        json!({"term": {"title": "TestSegmentReader.java"}}),
        json!({"term": {"title": "testsegmentreader.java"}}),
        json!({"term": {"title": "quick"}}),
        json!({"term": {"title": "Quick"}}),
        json!({"term": {"code": "TestSegmentReader"}}),
        json!({"term": {"code": "testsegmentreader"}}),
        json!({"prefix": {"title": "Test"}}),
        json!({"prefix": {"title": "test"}}),
        json!({"prefix": {"title": {"value": "Test", "case_insensitive": true}}}),
        json!({"prefix": {"code": "Test"}}),
        json!({"prefix": {"code": {"value": "test", "case_insensitive": true}}}),
        json!({"wildcard": {"title": "Hnsw*"}}),
        json!({"wildcard": {"title": "hnsw*"}}),
        json!({"wildcard": {"title": {"value": "Hnsw*", "case_insensitive": true}}}),
        json!({"wildcard": {"code": "Hnsw*"}}),
        json!({"wildcard": {"code": {"value": "hnsw*", "case_insensitive": false}}}),
    ];

    for query in &queries {
        let (count, total, returned) = count_and_search(&app, query).await;
        assert_eq!(
            count, total,
            "_count and _search disagree for {query}: count={count} total={total}"
        );
        assert_eq!(
            total as usize, returned,
            "hits.total is not the number of hits returned for {query}"
        );
    }
}

/// The reporter's exact sequence, and the answer to it.
///
/// `prefix title=Test` returning 0 stays correct — ES does not analyse a
/// multi-term query, so an uppercase prefix against a lowercased text
/// dictionary matches nothing. What changes is that the documented way to say
/// "I meant either case" now works instead of silently returning the same
/// zero.
#[tokio::test]
async fn case_insensitive_is_honoured_on_prefix_and_wildcard() {
    let (app, _dir) = app_with_corpus().await;

    // Case-sensitive is the default, on both field types: unchanged, ES-correct.
    let (_, cased, _) = count_and_search(&app, &json!({"prefix": {"title": "Test"}})).await;
    assert_eq!(cased, 0, "a cased prefix on text must stay case-sensitive");
    let (_, lowered, _) = count_and_search(&app, &json!({"prefix": {"title": "test"}})).await;
    assert_eq!(lowered, 2, "`test` matches both Test*.java titles");

    // ... and `case_insensitive: true` is the escape hatch out of it.
    let (_, opt_in, _) = count_and_search(
        &app,
        &json!({"prefix": {"title": {"value": "Test", "case_insensitive": true}}}),
    )
    .await;
    assert_eq!(
        opt_in, lowered,
        "`case_insensitive: true` must find what the lowercase prefix finds"
    );

    let (_, cased, _) = count_and_search(&app, &json!({"wildcard": {"title": "Hnsw*"}})).await;
    assert_eq!(
        cased, 0,
        "a cased wildcard on text must stay case-sensitive"
    );
    let (_, opt_in, _) = count_and_search(
        &app,
        &json!({"wildcard": {"title": {"value": "Hnsw*", "case_insensitive": true}}}),
    )
    .await;
    assert_eq!(opt_in, 1, "`case_insensitive: true` finds HnswGraphBuilder");

    // Keyword fields take the same parameter. The default there is XERJ's
    // historical case-INsensitive one rather than ES's, and #362 does not flip
    // it — but an explicit value now wins in both directions.
    let (_, folded, _) = count_and_search(
        &app,
        &json!({"prefix": {"code": {"value": "test", "case_insensitive": true}}}),
    )
    .await;
    assert_eq!(folded, 2, "folded keyword prefix matches both Test* codes");
    let (_, raw, _) = count_and_search(&app, &json!({"prefix": {"code": "test"}})).await;
    assert_eq!(raw, 0, "keyword prefix is case-sensitive by default");

    let (_, strict, _) = count_and_search(
        &app,
        &json!({"wildcard": {"code": {"value": "hnsw*", "case_insensitive": false}}}),
    )
    .await;
    assert_eq!(
        strict, 0,
        "an explicit `case_insensitive: false` must not be dropped on a keyword wildcard"
    );
}
