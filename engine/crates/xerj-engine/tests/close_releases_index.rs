//! Issue #463: `_close` must actually reclaim RAM, not just flip a flag.
//!
//! Before the fix, `close_index` only set the `closed_indices` gate; the
//! `Arc<Index>` (memtable, per-segment caches, hydration budget) stayed in
//! `Engine::indices`, so `_close` freed ~0.03% of an index's memory. The fix
//! releases the in-memory handle on close and reconstructs it from disk on
//! reopen — and must flush first so no not-yet-published document is lost.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

async fn count_all(engine: &Engine, name: &str) -> u64 {
    let idx = engine.get_index(name).expect("get index");
    let req = parse_request(&json!({ "query": { "match_all": {} }, "size": 0, "from": 0 }))
        .expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

async fn seed(engine: &Engine, name: &str, n: usize) {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index(name, schema).expect("create");
    let idx = engine.get_index(name).expect("get");
    for i in 0..n {
        idx.index_document(
            Some(format!("d{i}")),
            json!({ "body": format!("document number {i}") }),
        )
        .await
        .expect("index");
    }
}

/// Run a size>0 search so the per-segment caches actually hydrate.
async fn hydrate(engine: &Engine, name: &str) -> u64 {
    let idx = engine.get_index(name).expect("get index");
    let req = parse_request(&json!({ "query": { "match": { "body": "document" } }, "size": 25 }))
        .expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

/// `close_index` releases the resident caches while keeping the `Arc<Index>`
/// shell loaded (so `_cat`/`_cluster` views are unchanged); reads re-hydrate on
/// reopen with no data loss. FAIL-BEFORE: with the `idx.release_memory()` call
/// reverted, the caches stay populated so `total_cache_entries == 0` fails.
#[tokio::test]
async fn close_releases_caches_and_reopen_is_lossless() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "docs", 50).await;
    engine.get_index("docs").unwrap().refresh().await.unwrap();

    assert!(engine.is_index_loaded("docs"), "loaded before close");
    hydrate(&engine, "docs").await;
    let cached_before = engine.get_index("docs").unwrap().total_cache_entries();
    assert!(
        cached_before > 0,
        "a query should have hydrated the caches, got {cached_before}"
    );

    engine.close_index("docs").await.expect("close");
    // The shell stays loaded (loaded-index views unchanged), but caches are freed.
    assert!(
        engine.is_index_loaded("docs"),
        "a closed index stays loaded (shell) — matches _cat/_cluster's model"
    );
    assert!(
        engine.closed_indices.contains_key("docs"),
        "close marks the index closed"
    );
    let cached_after = engine.get_index("docs").unwrap().total_cache_entries();
    assert_eq!(
        cached_after, 0,
        "#463: _close must release the resident caches (was {cached_before}, now {cached_after})"
    );

    engine.reopen_index("docs").expect("reopen");
    assert!(
        !engine.closed_indices.contains_key("docs"),
        "reopen clears the closed flag"
    );
    assert_eq!(
        count_all(&engine, "docs").await,
        50,
        "#463: reopen re-hydrates losslessly"
    );
}

/// Data-safety: documents indexed but NOT refreshed/flushed before close must
/// survive close→reopen (close flushes the memtable BEFORE releasing caches).
/// Guards against a memory win that silently drops the memtable.
#[tokio::test]
async fn close_flushes_so_unrefreshed_docs_survive_reopen() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "docs", 30).await; // NO refresh — docs sit in the memtable/WAL

    engine.close_index("docs").await.expect("close");
    engine.reopen_index("docs").expect("reopen");
    assert_eq!(
        count_all(&engine, "docs").await,
        30,
        "#463: close must flush before releasing — no doc may be lost"
    );
}
