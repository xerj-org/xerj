//! `sort: [{"_doc": ...}]` is physical index (arrival) order, not `_id` order.
//!
//! ES defines `_doc` as index order — the order docs were written (Lucene
//! internal doc-id order), "no real use-case besides being the most efficient
//! sort order" (`sort-search-results`, ES 8.13 docs). `compute_sort_values`
//! (`engine/crates/xerj-engine/src/index.rs`) projected the `_id` STRING as
//! the `_doc` sort value, so `compare_sort_keys` ranked lexicographically by
//! id: `sort:["_doc"]` returned `alpha, mike, zeta` for docs inserted as
//! `zeta, alpha, mike` — matching neither ES nor xerj's own stored order
//! (tracked in `demo/playbooks/LOSS_BATTLE_PLAN.md` as the `sort:["_doc"]`
//! `_id`-string defect, explicitly split out as its own ticket).
//!
//! The fix projects the doc's `seq_no` (arrival sequence) as the `_doc` sort
//! value — a Number, so ordering is numeric arrival order, ties are
//! impossible (seq is unique per live doc), and the emitted `sort` array
//! doubles as a working `search_after` cursor through the existing numeric
//! cursor path (`normalize_search_after_value` passes numbers through
//! unchanged). `_id` sort keeps echoing the id string; only `_doc` changes.

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

fn doc_req(order: &str, size: usize) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({
        "query": {"match_all": {}},
        "size": size,
        "sort": [{"_doc": order}],
    }))
    .expect("parse_request")
}

/// Ids chosen so lexicographic order differs from insertion order: inserted
/// `zeta, alpha, mike`, lexicographic ascending would be `alpha, mike, zeta`.
/// FAIL-BEFORE: the page comes back in `_id` string order.
#[tokio::test]
async fn doc_sort_is_arrival_order_not_id_order() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("doc_sort", Schema::empty()).unwrap();
    let idx = engine.get_index("doc_sort").unwrap();

    for id in ["zeta", "alpha", "mike"] {
        idx.index_document(Some(id.to_string()), json!({ "n": id }))
            .await
            .unwrap();
    }

    let res = idx.search(&doc_req("asc", 10)).await.unwrap();
    let ids: Vec<&str> = res.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(
        ids,
        ["zeta", "alpha", "mike"],
        "_doc asc must be insertion order, not lexicographic _id order"
    );

    // The emitted sort value is the doc's arrival sequence number — a Number
    // that agrees with `lookup_seq_no`, so it pages as a cursor.
    for hit in &res.hits {
        assert_eq!(
            hit.sort.first().and_then(Value::as_u64),
            idx.lookup_seq_no(&hit.id),
            "hit {} must carry its seq_no as the _doc sort value, got {:?}",
            hit.id,
            hit.sort
        );
    }

    // Desc is the exact reverse.
    let res = idx.search(&doc_req("desc", 10)).await.unwrap();
    let ids: Vec<&str> = res.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, ["mike", "alpha", "zeta"], "_doc desc page");

    // Keyset paging across the whole corpus: no dupes, no drops.
    let mut collected: Vec<String> = Vec::new();
    let mut after: Option<Vec<Value>> = None;
    for _ in 0..5 {
        let mut body = json!({
            "query": {"match_all": {}},
            "size": 2,
            "sort": [{"_doc": "asc"}],
        });
        if let Some(ref a) = after {
            body["search_after"] = Value::Array(a.clone());
        }
        let page = idx.search(&parse_request(&body).unwrap()).await.unwrap();
        if page.hits.is_empty() {
            break;
        }
        after = page.hits.last().map(|h| h.sort.clone());
        collected.extend(page.hits.iter().map(|h| h.id.clone()));
    }
    assert_eq!(
        collected,
        ["zeta", "alpha", "mike"],
        "search_after on _doc must walk insertion order exactly once"
    );
}

/// Same guarantee once the docs live in a segment: the segment scan path
/// resolves `_doc` through the version map too, not from `_source`.
#[tokio::test]
async fn doc_sort_is_arrival_order_after_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("doc_sort_seg", Schema::empty())
        .unwrap();
    let idx = engine.get_index("doc_sort_seg").unwrap();

    for id in ["doc-9", "doc-10", "doc-2"] {
        idx.index_document(Some(id.to_string()), json!({ "n": id }))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let res = idx.search(&doc_req("asc", 10)).await.unwrap();
    let ids: Vec<&str> = res.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(
        ids,
        ["doc-9", "doc-10", "doc-2"],
        "flushed _doc asc must be insertion order (lexicographic would be doc-10, doc-2, doc-9)"
    );
    for hit in &res.hits {
        assert_eq!(
            hit.sort.first().and_then(Value::as_u64),
            idx.lookup_seq_no(&hit.id),
            "flushed hit {} must carry its seq_no, got {:?}",
            hit.id,
            hit.sort
        );
    }
}
