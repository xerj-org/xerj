//! Issue #948: a memory-pressure drain must flush every loaded index and
//! release its rebuildable caches WITHOUT closing anything — the parent RSS
//! breaker now requests this drain when it engages, because a breaker that
//! only rejects (429) frees nothing and a cache-heavy ingest never finishes
//! under a memory cap.
//!
//! The drain is the `close_index` (#463) flush+release pair applied to every
//! loaded index at once, minus the closed flag. These tests pin the two
//! properties that pair must keep: data safety (flush-first, so
//! memtable-resident docs survive) and service continuity (the index stays
//! loaded and queryable, reads re-hydrate from disk).

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

/// The drain releases every loaded index's caches and keeps the index in
/// service: loaded, not closed, queryable, lossless. FAIL-BEFORE: with the
/// `idx.release_memory()` call inside `drain_rebuildable_memory` reverted,
/// `total_cache_entries == 0` fails.
#[tokio::test]
async fn drain_releases_caches_and_keeps_indexes_in_service() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "docs", 50).await;
    engine.get_index("docs").unwrap().refresh().await.unwrap();

    hydrate(&engine, "docs").await;
    let cached_before = engine.get_index("docs").unwrap().total_cache_entries();
    assert!(
        cached_before > 0,
        "a query should have hydrated the caches, got {cached_before}"
    );

    let drained = engine.drain_rebuildable_memory().await;
    assert_eq!(drained, 1, "one loaded index must be drained");
    assert!(
        engine.is_index_loaded("docs"),
        "a drain never closes or unloads an index"
    );
    assert!(
        !engine.closed_indices.contains_key("docs"),
        "a drain must not mark the index closed"
    );
    let cached_after = engine.get_index("docs").unwrap().total_cache_entries();
    assert_eq!(
        cached_after, 0,
        "#948: the drain must release resident caches (was {cached_before}, now {cached_after})"
    );
    // Reads still work — caches re-hydrate from disk on demand.
    assert_eq!(count_all(&engine, "docs").await, 50);
}

/// Data-safety: documents still in the memtable (never refreshed/flushed)
/// must survive the drain — it flushes FIRST, exactly like `close_index`
/// (#463). Guards against a memory win that silently drops unflushed writes.
#[tokio::test]
async fn drain_flushes_so_unrefreshed_docs_survive() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "docs", 30).await; // NO refresh — docs sit in the memtable/WAL

    let drained = engine.drain_rebuildable_memory().await;
    assert_eq!(drained, 1);
    assert_eq!(
        engine.get_index("docs").unwrap().memtable_bytes(),
        0,
        "the drain must leave the memtable empty"
    );
    assert_eq!(
        count_all(&engine, "docs").await,
        30,
        "#948: drain must flush before releasing — no doc may be lost"
    );
}

/// A closed index is skipped (its caches were already released by
/// `close_index`), and its existence must not abort the drain for the rest.
#[tokio::test]
async fn drain_skips_closed_indexes_and_drains_the_rest() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "open-docs", 10).await;
    seed(&engine, "closed-docs", 10).await;
    engine.close_index("closed-docs").await.expect("close");

    let drained = engine.drain_rebuildable_memory().await;
    assert_eq!(
        drained, 1,
        "only the loaded-and-open index counts; closed caches were freed at close"
    );
    assert_eq!(count_all(&engine, "open-docs").await, 10);
}

/// #948 — the storage memtable must not retain one parsed `Arc<Value>` per
/// explicit-id write forever.
///
/// The engine flush path drains the FTS memtable and builds its
/// `DrainedMemtable` from THAT; `IndexStore::memtable_shards` (pushed by
/// every `store.index()` / `index_batch()` — the per-doc route explicit-id
/// bulk takes) was never drained on that path, so every flush left the full
/// parsed document trees resident for the life of the process.  Measured as
/// the dominant un-attributed at-rest RSS on mailbox-shaped bulk loads
/// (~2.4-3.1x source bytes).  `prune_published_memtable`, run at the end of
/// every successful `finalize_flush_with_publisher`, drops them once the
/// version map repoints at the published segment.
///
/// FAIL-BEFORE: with the `prune_published_memtable()` call in
/// `finalize_flush_with_publisher` reverted (but this test and the accessor
/// kept), `resident == 0` fails — entries survive the flush.
#[tokio::test]
async fn flush_prunes_published_storage_memtable_entries() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    // seed() writes with an explicit id → the per-doc `store.index()` path
    // that pushes a MemEntry per doc.
    seed(&engine, "docs", 40).await;
    let idx = engine.get_index("docs").unwrap();
    let before = idx.resident_memtable_entries();
    assert!(
        before >= 40,
        "explicit-id writes must land in the storage memtable, got {before}"
    );

    idx.refresh().await.expect("refresh");

    // refresh → flush is async (spawned finalize); poll for completion.
    let mut resident = before;
    for _ in 0..200 {
        resident = idx.resident_memtable_entries();
        if resident == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        resident, 0,
        "#948: a completed flush must prune the storage memtable entries it \
         published (started at {before})"
    );

    // And the prune dropped nothing the index still needs: all docs
    // readable, all durable.
    assert_eq!(count_all(&engine, "docs").await, 40);
}

/// #2427/#1122: the breaker drain between cache releases flushes the
/// memtable (data safety, memory relief) but keeps the warm caches, so a
/// read-heavy node is not forced to re-hydrate cold on every query while the
/// breaker stays engaged — the thrash loop the gate showed.
#[tokio::test]
async fn pressure_drain_without_release_flushes_but_keeps_caches() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "docs", 50).await;
    engine.get_index("docs").unwrap().refresh().await.unwrap();
    hydrate(&engine, "docs").await;
    let cached_before = engine.get_index("docs").unwrap().total_cache_entries();
    assert!(cached_before > 0, "precondition: hydrated caches");

    // Unflushed docs must still be flushed by the keep-caches drain.
    let idx = engine.get_index("docs").unwrap();
    for i in 50..60 {
        idx.index_document(
            Some(format!("d{i}")),
            json!({ "body": format!("document number {i}") }),
        )
        .await
        .unwrap();
    }
    assert!(idx.memtable_bytes() > 0, "precondition: live memtable");

    let drained = engine.pressure_drain(false).await;
    assert_eq!(drained, 1);
    assert_eq!(idx.memtable_bytes(), 0, "the drain still flushes");
    assert!(
        idx.total_cache_entries() >= cached_before,
        "caches must survive a drain between releases (was {cached_before}, now {})",
        idx.total_cache_entries()
    );

    engine.pressure_drain(true).await;
    assert_eq!(idx.total_cache_entries(), 0, "a due release still releases");
    assert_eq!(count_all(&engine, "docs").await, 60, "lossless");
}
