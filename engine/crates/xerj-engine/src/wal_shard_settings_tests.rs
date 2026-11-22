//! Regressions for the per-index WAL shard override (`index.xerj_ingest_shards`).
//!
//! The override exists because each index eagerly opens one WAL file descriptor
//! per ingest shard, and the ingest-shard count scales with CPU cores: a few
//! hundred autoindex datasets exhausted the macOS fd limit. Two properties have
//! to hold or the fix is worse than the bug — the value must be *ignored* unless
//! it is a sane count, and it must be honored identically at create and on
//! reopen, since the WAL write layout (`wal/*.wal` vs `wal/s{N}/*.wal`) and doc
//! routing both derive from it.

use std::path::Path;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;

use super::{store_config_from, wal_shards_override_from_settings};
use crate::Engine;

fn settings_with(shards: Value) -> Value {
    json!({ "index": { "xerj_ingest_shards": shards } })
}

/// The `s{N}` shard subdirectories under an index's WAL root. Zero of them means
/// the single-shard (legacy root) layout; `IndexStore::open` creates one per
/// shard whenever the count is > 1, so this is the on-disk witness of the shard
/// count the store was actually opened with.
fn wal_shard_dirs(index_dir: &Path) -> usize {
    std::fs::read_dir(index_dir.join("wal"))
        .expect("wal dir")
        .flatten()
        .filter(|e| {
            e.path().is_dir()
                && e.file_name()
                    .to_string_lossy()
                    .strip_prefix('s')
                    .is_some_and(|n| n.parse::<usize>().is_ok())
        })
        .count()
}

fn root_wal_files(index_dir: &Path) -> usize {
    std::fs::read_dir(index_dir.join("wal"))
        .expect("wal dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "wal"))
        .count()
}

/// `engine.ingest_shards` defaults to a fraction of the host's core count, so a
/// test that let it default would silently assert nothing on a 1- or 2-core
/// runner (the default would already be 1). Pin it well above 1.
fn config_for(dir: &TempDir) -> Config {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.engine.ingest_shards = 8;
    config
}

#[test]
fn a_positive_wal_shard_count_in_index_settings_is_used() {
    assert_eq!(
        wal_shards_override_from_settings(&settings_with(json!(1))),
        Some(1)
    );
    assert_eq!(
        wal_shards_override_from_settings(&settings_with(json!(4))),
        Some(4)
    );
    assert_eq!(
        wal_shards_override_from_settings(&settings_with(json!(256))),
        Some(256),
        "256 is the documented ceiling and must still be accepted"
    );
}

#[test]
fn an_absent_wal_shard_setting_leaves_the_engine_default_in_charge() {
    for absent in [
        Value::Null,
        json!({}),
        json!({ "index": {} }),
        json!({ "index": { "number_of_shards": 1 } }),
        json!({ "xerj_ingest_shards": 1 }),
        json!({ "index.xerj_ingest_shards": 1 }),
    ] {
        assert_eq!(
            wal_shards_override_from_settings(&absent),
            None,
            "no override should be read from {absent}"
        );
    }
}

#[test]
fn zero_and_negative_wal_shard_counts_are_ignored_never_used_as_a_count() {
    for bad in [json!(0), json!(-1), json!(-8)] {
        assert_eq!(
            wal_shards_override_from_settings(&settings_with(bad.clone())),
            None,
            "{bad} must not become a shard count"
        );
    }
}

#[test]
fn non_numeric_wal_shard_values_are_ignored_never_coerced() {
    for bad in [
        json!("4"),
        json!("many"),
        json!(4.5),
        json!(true),
        json!([4]),
        json!({ "value": 4 }),
        Value::Null,
    ] {
        assert_eq!(
            wal_shards_override_from_settings(&settings_with(bad.clone())),
            None,
            "{bad} must not become a shard count"
        );
    }
}

/// The value arrives straight off an index-create request body, and every shard
/// costs a directory plus a permanently open WAL file descriptor at
/// `IndexStore::open`. An out-of-range count is refused outright rather than
/// clamped, so a typo falls back to the engine default instead of quietly
/// running with a shard count nobody asked for.
#[test]
fn absurdly_large_wal_shard_counts_are_refused_and_never_opened() {
    for absurd in [json!(257), json!(100_000), json!(u64::MAX)] {
        assert_eq!(
            wal_shards_override_from_settings(&settings_with(absurd.clone())),
            None,
            "{absurd} exceeds the 256 ceiling and must be refused"
        );
    }
}

#[test]
fn the_store_config_takes_its_wal_shard_count_from_the_override_when_present() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_for(&dir);

    assert_eq!(
        store_config_from(&config, None, xerj_storage::StorageMode::Local).num_wal_shards,
        8
    );
    assert_eq!(
        store_config_from(&config, Some(1), xerj_storage::StorageMode::Local).num_wal_shards,
        1
    );
    assert_eq!(
        store_config_from(&config, Some(2), xerj_storage::StorageMode::Local).num_wal_shards,
        2
    );
}

/// The whole point of persisting the setting: create and reopen must agree on
/// the shard count. If reopen fell back to `engine.ingest_shards`, the reopened
/// store would write into `wal/s{N}/` while the create-time entries live in
/// `wal/`, splitting the layout under a single index.
#[tokio::test]
async fn a_pinned_wal_shard_count_survives_reopen_with_every_document_intact() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_for(&dir);
    let pinned_dir = dir.path().join("pinned");
    let default_dir = dir.path().join("spread");

    // Deliberately below the flush thresholds: the documents stay in the
    // memtable + WAL, so surviving a reopen means the WAL was replayed out of
    // the layout the persisted shard count selected, not read back from a
    // segment that would have survived either way.
    async fn write_docs(idx: &crate::Index, range: std::ops::Range<u32>) {
        for i in range {
            idx.index_document(
                Some(format!("doc-{i}")),
                json!({ "body": format!("payload {i}") }),
            )
            .await
            .expect("index document");
        }
    }

    async fn assert_docs(idx: &crate::Index, count: u32) {
        for i in 0..count {
            let doc = idx
                .get_document(&format!("doc-{i}"))
                .await
                .expect("get document")
                .unwrap_or_else(|| panic!("doc-{i} was lost"));
            assert_eq!(doc["body"], json!(format!("payload {i}")));
        }
        let all = idx
            .search(
                &xerj_query::parse_request(&json!({ "query": { "match_all": {} }, "size": 0 }))
                    .unwrap(),
            )
            .await
            .expect("search");
        assert_eq!(
            all.total.value, count as u64,
            "every document must be searchable after reopen"
        );
    }

    {
        let engine = Engine::new(config.clone()).expect("engine");
        engine
            .create_index_with_settings("pinned", Schema::empty(), settings_with(json!(1)))
            .expect("create pinned index");
        engine
            .create_index("spread", Schema::empty())
            .expect("create default index");

        let idx = engine.get_index("pinned").expect("pinned index");
        write_docs(&idx, 0..25).await;
        write_docs(&engine.get_index("spread").expect("spread index"), 0..1).await;

        assert_eq!(
            wal_shard_dirs(&pinned_dir),
            0,
            "one WAL shard means no s{{N}} subdirectories"
        );
        assert_eq!(
            root_wal_files(&pinned_dir),
            1,
            "one WAL shard means exactly one WAL file"
        );
        // Control: without the setting the same engine config opens the
        // core-scaled layout, so the assertions above are driven by the
        // setting and not by an engine that only ever opens one shard.
        assert_eq!(wal_shard_dirs(&default_dir), 8);
    }

    {
        let engine = Engine::new(config.clone()).expect("reopen engine");
        let idx = engine
            .get_index("pinned")
            .expect("pinned index after reopen");
        assert_docs(&idx, 25).await;

        assert_eq!(
            wal_shard_dirs(&pinned_dir),
            0,
            "reopen must honor the persisted shard count, not engine.ingest_shards"
        );
        assert_eq!(
            wal_shard_dirs(&default_dir),
            8,
            "an index without the setting keeps the default"
        );

        // Writes taken by the *reopened* store must land in the same layout —
        // this is where a create/open mismatch would split the WAL.
        write_docs(&idx, 25..50).await;
        assert_eq!(wal_shard_dirs(&pinned_dir), 0);
        assert_eq!(root_wal_files(&pinned_dir), 1);
    }

    {
        let engine = Engine::new(config).expect("second reopen engine");
        let idx = engine
            .get_index("pinned")
            .expect("pinned index after second reopen");
        assert_docs(&idx, 50).await;
        assert_eq!(wal_shard_dirs(&pinned_dir), 0);
    }
}

/// #2929 — the memtable follows the same per-index override. A 5-document
/// write to an index pinned to one shard refreshes into ONE segment; before,
/// the memtable kept the host-sized default and the refresh wrote one segment
/// per non-empty shard (each a full per-field file set, every file fsynced).
#[tokio::test]
async fn a_pinned_index_refreshes_a_small_write_into_one_segment() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_for(&dir);
    {
        let engine = Engine::new(config.clone()).expect("engine");
        engine
            .create_index_with_settings("pinned", Schema::empty(), settings_with(json!(1)))
            .expect("create pinned index");
        engine
            .create_index("spread", Schema::empty())
            .expect("create default index");
        for name in ["pinned", "spread"] {
            let idx = engine.get_index(name).expect("index");
            for i in 0..5 {
                idx.index_document(Some(format!("catalog-{i}")), json!({ "n": i }))
                    .await
                    .expect("index document");
            }
            idx.refresh().await.expect("refresh");
        }
        let pinned = engine.get_index("pinned").expect("pinned");
        let spread = engine.get_index("spread").expect("spread");
        assert_eq!(pinned.memtable.shard_count(), 1);
        assert_eq!(pinned.store.snapshot().segments.len(), 1);
        // Control: the same documents on a default index spread over the
        // engine's 8 shards, so the assertion above is the setting's doing.
        assert_eq!(spread.memtable.shard_count(), 8);
        assert!(spread.store.snapshot().segments.len() > 1);
    }
    let engine = Engine::new(config).expect("reopen engine");
    let pinned = engine.get_index("pinned").expect("pinned after reopen");
    assert_eq!(
        pinned.memtable.shard_count(),
        1,
        "reopen must size the memtable from the persisted setting"
    );
    for i in 0..5 {
        assert!(pinned
            .get_document(&format!("catalog-{i}"))
            .await
            .expect("get")
            .is_some());
    }
}
