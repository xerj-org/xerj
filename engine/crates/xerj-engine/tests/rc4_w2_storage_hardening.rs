//! RC4 Wave-2 Stream D — storage/durability hardening regressions.
//!
//! Item 15: transient GET 404 under merge (version map repointed to the
//!          merged segment before the snapshot publish).
//! Item 14: delete tombstones must survive merges (carry-forward) now that
//!          the WAL pin is released once a tombstone is segment-resident.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// Item 15 — GET must never 404 an existing doc while a forcemerge
/// publishes.  Live repro pre-fix: 47/15072 GETs returned 404 in the
/// ~300 ms window between the merge task's version-map repoint and
/// `apply_merge`'s snapshot swap (8×25k docs, HTTP).  In-process the
/// hammer is much faster than HTTP, so even the smaller window here
/// catches the regression reliably.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_document_never_404s_during_forcemerge() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("m15", Schema::empty()).unwrap();
    let idx = engine.get_index("m15").unwrap();

    // 6 flushed batches → 6+ segments, one probe id per batch.
    let mut probe_ids = Vec::new();
    for b in 0..6 {
        for i in 0..1500 {
            let id = format!("d{b}_{i}");
            idx.index_document(
                Some(id),
                json!({"title": format!("doc {b}/{i}"), "body": "lorem ipsum dolor sit amet", "n": b * 1500 + i}),
            )
            .await
            .unwrap();
        }
        idx.flush().await.unwrap();
        probe_ids.push(format!("d{b}_7"));
    }

    // Sanity: every probe resolves before the merge.
    for id in &probe_ids {
        assert!(
            idx.get_document(id).await.unwrap().is_some(),
            "probe {id} must exist pre-merge"
        );
    }

    // Hammer GETs concurrently with the forcemerge.
    let stop = Arc::new(AtomicBool::new(false));
    let misses = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let hammer = {
        let idx = Arc::clone(&idx);
        let stop = Arc::clone(&stop);
        let misses = Arc::clone(&misses);
        let total = Arc::clone(&total);
        let ids = probe_ids.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                for id in &ids {
                    match idx.get_document(id).await {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            misses.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            misses.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    total.fetch_add(1, Ordering::Relaxed);
                }
                tokio::task::yield_now().await;
            }
        })
    };

    let merged = idx.force_merge(1).await.unwrap();
    assert!(merged > 0, "forcemerge must merge something");
    // Let the hammer overlap any post-merge publish work too.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    stop.store(true, Ordering::Relaxed);
    hammer.await.unwrap();

    let t = total.load(Ordering::Relaxed);
    let m = misses.load(Ordering::Relaxed);
    assert!(t > 0, "hammer must have run");
    assert_eq!(
        m, 0,
        "GET returned 404/error for existing docs during forcemerge ({m}/{t})"
    );

    // And everything still resolves after.
    for id in &probe_ids {
        assert!(
            idx.get_document(id).await.unwrap().is_some(),
            "probe {id} lost after merge"
        );
    }
}

/// Item 14 — with WAL pins released once tombstones are segment-resident,
/// a merge must CARRY the tombstones forward: the deleted doc stays dead
/// in live search AND across a restart, even after every pre-merge
/// segment (including the tombstone-only one) has been merged away and
/// the delete's WAL entry pruned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleted_doc_stays_dead_through_merge_and_restart() {
    let dir = TempDir::new().unwrap();
    {
        let engine = make_engine(&dir);
        engine.create_index("m14", Schema::empty()).unwrap();
        let idx = engine.get_index("m14").unwrap();

        for b in 0..3 {
            for i in 0..50 {
                idx.index_document(Some(format!("k{b}_{i}")), json!({"v": i, "b": b}))
                    .await
                    .unwrap();
            }
            idx.flush().await.unwrap();
        }

        // Plain delete, never re-indexed.
        assert!(idx.delete_document("k0_7").await.unwrap());
        // Flush → maintenance persists the tombstone (tombstone-only
        // segment) and releases the WAL pin.
        idx.flush().await.unwrap();
        assert!(idx.get_document("k0_7").await.unwrap().is_none());

        // Merge everything — inputs (including the tombstone-only
        // segment) are removed; the tombstone must ride along.
        idx.force_merge(1).await.unwrap();
        assert!(
            idx.get_document("k0_7").await.unwrap().is_none(),
            "deleted doc resurrected in live search after merge"
        );
        assert!(
            idx.get_document("k0_8").await.unwrap().is_some(),
            "sibling doc lost after merge"
        );
        idx.flush().await.unwrap();
        drop(idx);
        drop(engine);
    }

    // Restart — the WAL delete entry is pruned; only the merged
    // segment's carried tombstone can keep the doc dead.
    let engine2 = make_engine(&dir);
    let idx2 = engine2.get_index("m14").unwrap();
    assert!(
        idx2.get_document("k0_7").await.unwrap().is_none(),
        "deleted doc resurrected after merge + restart (tombstone not carried forward)"
    );
    for probe in ["k0_8", "k1_7", "k2_49"] {
        assert!(
            idx2.get_document(probe).await.unwrap().is_some(),
            "live doc {probe} lost after merge + restart"
        );
    }
}
