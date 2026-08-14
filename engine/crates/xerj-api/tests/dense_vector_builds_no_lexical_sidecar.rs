//! A user-mapped `dense_vector` must build no lexical sidecar — issue #328,
//! the still-live half of the invariant RFC #148 proposed as "do not build
//! lexical FSTs for generated vectors". The *generated* half was fixed in #12;
//! a field a user explicitly maps as `dense_vector` was still getting one.
//!
//! Before this test's fix, `"index": true` on a `dense_vector` — which is how
//! ES asks for kNN — set `FieldOptions::indexed`, so the field walked straight
//! past the `index: false` arm of `memtable::fts_excluded_fields` and into
//! `build_fts_field_configs`, which handed it the `keyword` analyzer. The term
//! dictionary then held one whole-value token per document that was a verbatim
//! decimal re-rendering of the entire vector. Measured on a 3,000-doc / 128-dim
//! force-merged index: `<seg>.emb.fst` was 7,758,140 B against a
//! `<seg>.emb.post` of 6,244 B — 3,000 terms carrying one posting each, 78.6%
//! of the whole index, and no query path can read a byte of it.
//!
//! The behavioural change is four lines in one shared exclusion set, so the
//! risk is not in the bytes it removes but in the surfaces it might have
//! removed an answer from. This file pins both halves:
//!
//!  * the sidecar is gone from disk, after flush AND after merge (the merge
//!    call sites read the same set, but that is proved here rather than
//!    trusted), and
//!  * every read surface that names the field still answers — `knn`, `term`,
//!    `match`, `match_phrase`, `query_string`, `exists`, `_field_caps` and
//!    `highlight` — with a `200`, and with the same values it gave before,
//!    with one measured exception recorded on the test below: `match` /
//!    `match_phrase` handed the *exact* whole-vector decimal rendering used to
//!    return the document and now returns nothing.
//!
//! Lucene makes the defect unreachable rather than filtering it out —
//! `KnnFloatVectorField.createType` never touches `indexOptions`, which
//! `FieldType` defaults to `NONE`, and `IndexingChain.invertAndStore` gates
//! inversion on it. See the doc comment on `memtable::fts_excluded_fields` for
//! the citations. Approach only, no code taken.
//!
//! Elasticsearch is referenced for wire semantics only. It is AGPL-3.0/
//! SSPL-1.0/Elastic-2.0 licensed and no code from it is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use std::path::{Path as FsPath, PathBuf};
use tower::ServiceExt;

const DIMS: usize = 16;
const DOCS: u32 = 40;

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

async fn search(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    json_req(app, "POST", "/probe/_search", body).await
}

/// The vector for document `i`: deterministic, distinct per document, and
/// never all-zero (cosine similarity refuses a zero vector).
fn vector(i: u32) -> Vec<f64> {
    (0..DIMS)
        .map(|d| ((i as f64) + 1.0) * 0.01 + (d as f64) * 0.001)
        .collect()
}

/// `emb` — `dense_vector`, `"index": true`: kNN, and before #328 also postings.
/// `body` — a plain indexed text field, the control that must keep its FST.
async fn app_with_corpus() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (status, body) = json_req(
        &app,
        "PUT",
        "/probe",
        json!({
            "mappings": { "properties": {
                "body": { "type": "text" },
                "cat":  { "type": "keyword" },
                "emb":  {
                    "type": "dense_vector",
                    "dims": DIMS,
                    "index": true,
                    "similarity": "cosine"
                }
            }}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create index failed: {body}");

    // Two refreshes with documents in between, so the corpus lands in more
    // than one segment and the later force-merge has real work to do. A
    // force-merge of an index already at one segment is a no-op and would
    // prove nothing about the merge path.
    for half in 0..2u32 {
        for i in 0..DOCS / 2 {
            let id = half * (DOCS / 2) + i + 1;
            let (status, body) = json_req(
                &app,
                "PUT",
                &format!("/probe/_doc/{id}"),
                json!({
                    "body": format!("pelican memo number {id}"),
                    "cat": "memo",
                    "emb": vector(id)
                }),
            )
            .await;
            assert!(status.is_success(), "index doc {id}: {status} {body}");
        }
        let (status, body) = json_req(&app, "POST", "/probe/_refresh", json!({})).await;
        assert!(status.is_success(), "refresh: {status} {body}");
    }
    (app, dir)
}

/// Every segment artifact under `dir` whose name ends `.<field>.<ext>`.
///
/// Walked recursively off the data dir rather than reaching for the segment
/// directory layout, so the assertion cannot be quietly defeated by the files
/// moving. Naming is `<seg-id>.<field>.<ext>` (`xerj-fts/src/index.rs:8`).
fn sidecars_for(dir: &FsPath, field: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if ["fst", "post", "norms", "meta"]
                .iter()
                .any(|ext| name.ends_with(&format!(".{field}.{ext}")))
            {
                found.push(path);
            }
        }
    }
    found
}

fn describe(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            format!("{} ({size} B)", p.display())
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. the bytes are gone — at flush and at merge
// ─────────────────────────────────────────────────────────────────────────────

/// The whole point of #328. Asserted at both write paths because they are
/// separate call sites into the one exclusion set (`index.rs` flush at
/// `:7925`, merge at `:8176`, `:8312`, `:8552`, `:17771`, `:42429`); the set
/// being shared is the reason one fix suffices, and this is what proves it.
#[tokio::test]
async fn no_lexical_sidecar_survives_flush_or_merge() {
    let (app, dir) = app_with_corpus().await;

    let after_flush = sidecars_for(dir.path(), "emb");
    assert!(
        after_flush.is_empty(),
        "a dense_vector must build no term dictionary, postings or norms at \
         flush — found {:?}",
        describe(&after_flush)
    );
    // The control: the exclusion is per-field, not a blanket switch-off of the
    // FTS writer. If `body` lost its FST too, this test would pass for the
    // wrong reason.
    assert!(
        !sidecars_for(dir.path(), "body").is_empty(),
        "the indexed text control field must still have its lexical artifacts"
    );

    let (status, body) = json_req(
        &app,
        "POST",
        "/probe/_forcemerge?max_num_segments=1",
        json!({}),
    )
    .await;
    assert!(status.is_success(), "forcemerge: {status} {body}");

    let after_merge = sidecars_for(dir.path(), "emb");
    assert!(
        after_merge.is_empty(),
        "merge must not resurrect what flush correctly skipped — found {:?}",
        describe(&after_merge)
    );
    assert!(
        !sidecars_for(dir.path(), "body").is_empty(),
        "the control field must survive the merge with its lexical artifacts"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. nothing observable moved
// ─────────────────────────────────────────────────────────────────────────────

/// kNN is served from the HNSW graph, never from postings, so dropping the
/// postings must leave the ids AND the scores untouched.
#[tokio::test]
async fn knn_still_returns_the_same_ids_and_scores() {
    let (app, _dir) = app_with_corpus().await;

    let (status, body) = search(
        &app,
        json!({
            "knn": {
                "field": "emb",
                "query_vector": vector(7),
                "k": 5,
                "num_candidates": 100
            },
            "_source": false
        }),
    )
    .await;
    assert!(status.is_success(), "knn: {status} {body}");

    let hits = body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits: {body}"));
    assert_eq!(hits.len(), 5, "expected k=5 back: {body}");
    // Document 7 is its own nearest neighbour, at the top, at cosine 1.0.
    assert_eq!(
        hits[0]["_id"],
        json!("7"),
        "nearest neighbour wrong: {body}"
    );
    let top = hits[0]["_score"].as_f64().expect("score");
    assert!(
        (top - 1.0).abs() < 1e-4,
        "a vector against itself must score ~1.0 under cosine, got {top}: {body}"
    );
    // Scores must be a real ranking, not a constant handed out by a fallback.
    let scores: Vec<f64> = hits
        .iter()
        .map(|h| h["_score"].as_f64().expect("score"))
        .collect();
    assert!(
        scores.windows(2).all(|w| w[0] >= w[1]),
        "kNN scores must be non-increasing: {scores:?}"
    );
}

/// The stored-doc-scan-fallback regression, and the one place this fix is
/// observable at all.
///
/// Measured on `main` before the fix: `term` returned `hits.total: 0` for
/// every probe, and so did a single vector component under `term`, `match`,
/// `match_phrase` and `query_string`. The ONE form that matched was `match` /
/// `match_phrase` handed the exact whole-vector rendering — every component
/// `f64`-to-string, joined by single spaces — because the `keyword` analyzer
/// produced that identical token on both the index and the query side. That
/// form now returns 0, which is the only behaviour change in #328 and is not
/// reachable without reconstructing the engine's own float formatting.
///
/// The status must stay `200`. Dropping the postings must not route any of
/// these onto a stored-doc scan that *does* match the digits, and must not
/// turn a query naming the field into an error — that would be a wire-contract
/// change this fix does not make.
#[tokio::test]
async fn lexical_queries_on_the_vector_field_stay_a_200_with_zero_hits() {
    let (app, _dir) = app_with_corpus().await;

    // A single component of document 7's vector, and the whole vector rendered
    // exactly as the sidecar used to render it.
    let component = format!("{}", vector(7)[0]);
    let whole = vector(7)
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join(" ");

    for probe in [component.as_str(), whole.as_str()] {
        for query in [
            json!({ "term":         { "emb": probe } }),
            json!({ "match":        { "emb": probe } }),
            json!({ "match_phrase": { "emb": probe } }),
            json!({ "query_string": { "query": probe, "default_field": "emb" } }),
        ] {
            let (status, body) = search(&app, json!({ "query": query })).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "a lexical query on a dense_vector must not error: {query} -> {body}"
            );
            assert_eq!(
                body["hits"]["total"]["value"], 0,
                "a dense_vector has no lexical semantics: {query} -> {body}"
            );
        }
    }
}

/// `exists` is answered from `_source` / doc values, not from postings
/// (`xerj-engine/src/index.rs:30355-30359`), so the count is unaffected.
#[tokio::test]
async fn exists_still_counts_every_document() {
    let (app, _dir) = app_with_corpus().await;
    let (status, body) = search(
        &app,
        json!({ "query": { "exists": { "field": "emb" } }, "track_total_hits": true }),
    )
    .await;
    assert!(status.is_success(), "exists: {status} {body}");
    assert_eq!(
        body["hits"]["total"]["value"], DOCS,
        "every document has an `emb`: {body}"
    );
}

/// `_field_caps` is derived from the mapping, not from what is on disk, so the
/// field stays `searchable` and `aggregatable` exactly as before. This is the
/// "did the fix change the advertised contract" check.
#[tokio::test]
async fn field_caps_for_the_vector_field_are_unchanged() {
    let (app, _dir) = app_with_corpus().await;
    let (status, body) = get(&app, "/probe/_field_caps?fields=emb").await;
    assert!(status.is_success(), "field_caps: {status} {body}");
    let caps = &body["fields"]["emb"]["dense_vector"];
    assert_eq!(caps["type"], json!("dense_vector"), "{body}");
    assert_eq!(caps["searchable"], json!(true), "{body}");
    assert_eq!(caps["aggregatable"], json!(true), "{body}");
}

/// Highlighting is the one remaining read surface that could in principle want
/// a term dictionary for the field. It has no defensible reading of a decimal
/// vector, so the requirement is only that naming the field is still a `200`
/// and still returns the matching document. (`_termvectors` is deliberately
/// not covered: XERJ exposes no such route — it appears only in the authz
/// path list at `xerj-api/src/authz.rs:358` — so there is nothing to pin.)
#[tokio::test]
async fn highlighting_over_the_vector_field_is_still_a_200() {
    let (app, _dir) = app_with_corpus().await;
    let (status, body) = search(
        &app,
        json!({
            "query": { "match": { "body": "pelican" } },
            "highlight": { "fields": { "emb": {}, "body": {} } }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "highlighting a dense_vector must not error: {body}"
    );
    assert!(
        body["hits"]["total"]["value"].as_u64().unwrap_or(0) > 0,
        "the text query must still match: {body}"
    );
}
