//! Regression tests for #217 — `multi_match` naming an UNMAPPED field.
//!
//! ES ignores unmapped fields in `multi_match`: the query runs over the
//! mapped subset, and only when EVERY field is unmapped does it match
//! nothing (no error either way). XERJ instead built an FTS clause for the
//! unmapped field; the per-segment "reader has every queried field" gate
//! then refused the postings path for the WHOLE query, and the stored-doc
//! fallback — whose default multi_match arm requires the entire query
//! string as one contiguous substring — silently zeroed every multi-token
//! query. Single-token queries survived because the one token IS the whole
//! string, which is what kept the bug invisible to the ES-YAML suite.
//!
//! Every search here runs AFTER a flush: the memtable path has a separate,
//! still-open multi-token divergence (MULTIMATCH_DEFECT.md, defect 2) that
//! these tests deliberately do not encode expectations for.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn req(q: Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request")
}

/// One flushed doc with two mapped text fields (`body`, `title`); `ghost`
/// never appears anywhere, so dynamic mapping never creates it.
async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<xerj_engine::Index> {
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    idx.index_document(
        Some("1".to_string()),
        json!({
            "body": "the log merge policy groups segments into size buckets",
            "title": "merge policy"
        }),
    )
    .await
    .unwrap();
    idx.flush().await.unwrap();
    idx
}

/// #217 headline: an unmapped field must not change the result of a
/// multi-token query — same hits as the identical query without it.
#[tokio::test]
async fn unmapped_field_must_not_zero_a_multi_token_multi_match() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mm217").await;

    let control = idx
        .search(&req(json!({"multi_match": {
            "query": "merge zzzznotpresent", "fields": ["body"]}})))
        .await
        .unwrap();
    assert_eq!(control.total.value, 1, "control: one token present in body");

    let with_ghost = idx
        .search(&req(json!({"multi_match": {
            "query": "merge zzzznotpresent", "fields": ["body", "ghost"]}})))
        .await
        .unwrap();
    assert_eq!(
        with_ghost.total.value, 1,
        "an unmapped field removed a document the mapped subset matches (#217)"
    );
    assert_eq!(
        control.hits[0].id, with_ghost.hits[0].id,
        "same document either way"
    );
}

/// Single-token shape with the same unmapped field — the case that already
/// worked (via the stored scan) and must keep working on the postings path.
#[tokio::test]
async fn unmapped_field_single_token_still_matches() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mm217_single").await;

    let r = idx
        .search(&req(json!({"multi_match": {
            "query": "merge", "fields": ["body", "ghost"]}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1);
}

/// EVERY named field unmapped: ES semantics — match nothing, no error.
#[tokio::test]
async fn all_fields_unmapped_matches_nothing_without_error() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mm217_allghost").await;

    for query in ["merge zzzznotpresent", "merge"] {
        let r = idx
            .search(&req(json!({"multi_match": {
                "query": query, "fields": ["ghost1", "ghost2"]}})))
            .await
            .unwrap();
        assert_eq!(
            r.total.value, 0,
            "query {query:?} over only unmapped fields"
        );
    }
}

/// The same zeroing hit every field-centric type through the shared
/// lowering; `most_fields` is the non-dis_max shape.
#[tokio::test]
async fn most_fields_multi_token_with_unmapped_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mm217_most").await;

    let r = idx
        .search(&req(json!({"multi_match": {
            "query": "merge zzzznotpresent",
            "fields": ["body", "ghost"],
            "type": "most_fields"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1, "most_fields shares the lowering (#217)");
}

/// `operator: and` binds PER FIELD (ES best_fields/most_fields build one
/// match query per field with the operator): a document matches only when
/// ONE field holds every token. The pre-fix FTS lowering ignored the
/// operator and OR'd the tokens, over-matching whenever every named field
/// was mapped.
#[tokio::test]
async fn operator_and_requires_every_token_in_one_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mm217_and").await;

    // No single field holds both "merge" and "zzzznotpresent" → 0 hits.
    let r = idx
        .search(&req(json!({"multi_match": {
            "query": "merge zzzznotpresent",
            "fields": ["body", "title"],
            "operator": "and"}})))
        .await
        .unwrap();
    assert_eq!(
        r.total.value, 0,
        "operator:and must not degrade to OR over tokens"
    );

    // `body` holds both tokens; the unmapped field must not change that —
    // and in particular must not flip the query from AND onto an OR path.
    let r = idx
        .search(&req(json!({"multi_match": {
            "query": "merge policy",
            "fields": ["body", "ghost"],
            "operator": "and"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1, "mapped subset satisfies per-field AND");
}

/// `cross_fields` + `operator: and` is term-centric — every token must
/// appear in at least ONE of the (mapped) fields, NOT all in the same one.
/// Here `bravo` lives only in `alpha` and `charlie` only in `beta`, so a
/// per-field AND would find nothing while the combined view matches — with
/// or without an unmapped field in the list.
#[tokio::test]
async fn cross_fields_and_matches_across_fields() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mm217_cross", Schema::empty()).unwrap();
    let idx = engine.get_index("mm217_cross").unwrap();
    idx.index_document(
        Some("1".to_string()),
        json!({ "alpha": "delta bravo", "beta": "charlie echo" }),
    )
    .await
    .unwrap();
    idx.flush().await.unwrap();

    for fields in [json!(["alpha", "beta"]), json!(["alpha", "beta", "ghost"])] {
        let r = idx
            .search(&req(json!({"multi_match": {
                "query": "bravo charlie",
                "fields": fields,
                "type": "cross_fields",
                "operator": "and"}})))
            .await
            .unwrap();
        assert_eq!(
            r.total.value, 1,
            "combined-text AND across mapped fields ({fields})"
        );
    }
}
