//! Regression tests: `multi_match` must return non-zero, ES-shaped scores.
//!
//! Bug: every multi_match hit came back with `_score: 0.0` on the segment
//! FTS path because `BoolQuery::new()` defaulted `boost` to 0.0 and
//! `execute_bool` multiplies the combined score by it.  ES 8.x semantics:
//! `best_fields` (the default type) is a dis_max over per-field match
//! queries — the hit score is the MAX of the per-field scores; a
//! single-field multi_match scores identically to the equivalent `match`.

use std::collections::HashMap;

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

#[tokio::test]
async fn multi_match_scores_nonzero_and_best_fields_takes_field_max() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mm", Schema::empty()).unwrap();
    let idx = engine.get_index("mm").unwrap();

    // 20 docs: "golf" in msg for i % 3 == 0, in note for i % 5 == 0, so the
    // multi_match union (9 docs: i%3==0 or i%5==0) differs from either
    // single-field result (7 via msg, 4 via note, 2 overlap).
    for i in 0..20 {
        let msg = if i % 3 == 0 {
            format!("user played golf round {i}")
        } else {
            format!("user played tennis {i}")
        };
        let note = if i % 5 == 0 { "golf note" } else { "misc" };
        idx.index_document(Some(format!("d{i}")), json!({"msg": msg, "note": note}))
            .await
            .unwrap();
    }

    // ── Memtable state: scores must not be zero ──────────────────────────
    let r = idx
        .search(&req(
            json!({"multi_match": {"query": "golf", "fields": ["msg", "note"]}}),
        ))
        .await
        .unwrap();
    assert_eq!(r.total.value, 9, "memtable multi_match union");
    for h in &r.hits {
        assert!(
            h.score > 0.0,
            "memtable multi_match hit {} scored 0.0",
            h.id
        );
    }

    // ── Segment state: BM25 scores, ES best_fields semantics ─────────────
    idx.flush().await.unwrap();

    let match_msg = idx
        .search(&req(json!({"match": {"msg": "golf"}})))
        .await
        .unwrap();
    let match_note = idx
        .search(&req(json!({"match": {"note": "golf"}})))
        .await
        .unwrap();
    let msg_scores: HashMap<&str, f32> = match_msg
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.score))
        .collect();
    let note_scores: HashMap<&str, f32> = match_note
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.score))
        .collect();
    assert!(match_msg.hits.iter().all(|h| h.score > 0.0));

    // Single-field multi_match must score IDENTICALLY to the match query.
    let mm_single = idx
        .search(&req(
            json!({"multi_match": {"query": "golf", "fields": ["msg"]}}),
        ))
        .await
        .unwrap();
    assert_eq!(mm_single.total.value, match_msg.total.value);
    for h in &mm_single.hits {
        let expected = msg_scores.get(h.id.as_str()).copied().unwrap_or(0.0);
        assert!(
            h.score > 0.0,
            "single-field multi_match hit {} scored 0.0",
            h.id
        );
        assert!(
            (h.score - expected).abs() < 1e-5,
            "hit {}: multi_match(msg) score {} != match(msg) score {}",
            h.id,
            h.score,
            expected
        );
    }

    // Multi-field best_fields: score = max(per-field match scores).
    let mm = idx
        .search(&req(
            json!({"multi_match": {"query": "golf", "fields": ["msg", "note"]}}),
        ))
        .await
        .unwrap();
    assert_eq!(mm.total.value, 9, "segment multi_match union");
    for h in &mm.hits {
        let ms = msg_scores.get(h.id.as_str()).copied().unwrap_or(0.0);
        let ns = note_scores.get(h.id.as_str()).copied().unwrap_or(0.0);
        let expected = ms.max(ns);
        assert!(
            h.score > 0.0,
            "multi-field multi_match hit {} scored 0.0",
            h.id
        );
        assert!(
            (h.score - expected).abs() < 1e-5,
            "hit {}: best_fields score {} != max(msg {}, note {})",
            h.id,
            h.score,
            ms,
            ns
        );
    }

    // most_fields: score = sum of the per-field scores.
    let mm_most = idx
        .search(&req(json!({
            "multi_match": {"query": "golf", "fields": ["msg", "note"], "type": "most_fields"}
        })))
        .await
        .unwrap();
    assert_eq!(mm_most.total.value, 9);
    for h in &mm_most.hits {
        let ms = msg_scores.get(h.id.as_str()).copied().unwrap_or(0.0);
        let ns = note_scores.get(h.id.as_str()).copied().unwrap_or(0.0);
        let expected = ms + ns;
        assert!(
            (h.score - expected).abs() < 1e-5,
            "hit {}: most_fields score {} != sum(msg {}, note {})",
            h.id,
            h.score,
            ms,
            ns
        );
    }
}
