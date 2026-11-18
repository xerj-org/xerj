//! Issue #2238: on the 8 GiB auto memory tier the summed-memtable budget is
//! 2 GiB while each index flushes at `storage.flush_size_mb` (512 MiB). A
//! corpus spread across dozens of indices crosses the process budget with
//! every index still under its own flush line. The parent breaker then
//! answers every write HTTP 429, and until this change that latch did not
//! request a drain — so nothing flushed, the client retried, and the indexed
//! count stayed at 0.
//!
//! This test is its own binary so `governor::init` sees this config (the
//! first `Engine::new` in a process wins). One test, so a second engine
//! cannot install a different governor first.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::governor;
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    // 1 MiB process budget. The auto tier's 2 GiB is the same shape at a
    // size a test can fill. RSS watermark off, so only the memtable latch
    // can reject.
    config.limits.max_total_memtable_mb = 1;
    config.limits.memory_watermark_percent = 0;
    // Keep every index under its own flush line. Idle flush disabled, and
    // the periodic flusher pushed far past this test.
    config.storage.flush_size_mb = 64;
    config.storage.flush_idle_secs = 0;
    config.storage.flush_interval_secs = 3600;
    Engine::new(config).expect("engine::new")
}

fn text_schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
}

async fn count_all(engine: &Engine, name: &str) -> u64 {
    let idx = engine.get_index(name).expect("get index");
    let req = parse_request(&json!({ "query": { "match_all": {} }, "size": 0, "from": 0 }))
        .expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

/// Fill four indices past the 1 MiB summed budget without any one of them
/// reaching its flush threshold, watch the next writes get rejected with the
/// indexed bytes unchanged, then drain — the request the sampler now makes
/// on this latch — and accept one more document.
#[tokio::test]
async fn summed_memtable_pressure_rejects_until_a_drain_frees_the_budget() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let names = ["a", "b", "c", "d"];
    for name in names {
        engine
            .create_index(name, text_schema())
            .expect("create index");
    }
    // One long token. The memtable charges ~3× the source bytes, so a few
    // dozen of these cross 1 MiB without a per-index flush.
    let body = format!("n{}", "a".repeat(4096));
    let mut accepted = 0usize;
    for n in 0..400 {
        let name = names[n % names.len()];
        let idx = engine.get_index(name).unwrap();
        idx.index_document(Some(format!("{name}-{n}")), json!({ "body": body }))
            .await
            .unwrap_or_else(|error| panic!("write {n} before the budget: {error}"));
        accepted += 1;
        if engine.total_memtable_bytes() >= 1024 * 1024 {
            break;
        }
    }
    let summed = engine.total_memtable_bytes();
    assert!(
        summed >= 1024 * 1024,
        "the four indices must cross the 1 MiB budget (accepted {accepted}, summed {summed} bytes)"
    );
    for name in names {
        let idx = engine.get_index(name).unwrap();
        let held = idx.memtable_bytes();
        let line = idx.flush_threshold();
        assert!(
            held < line,
            "index {name} holds {held} bytes, flush line is {line}: this must be the multi-index shape, not a per-index flush"
        );
        assert!(held > 0, "index {name} took none of the writes");
    }

    let governor = governor::global().expect("Engine::new installs the governor");
    governor.refresh_memtable(summed);
    assert!(
        !governor.memory_breaker_engaged(),
        "RSS watermark is off; only the memtable latch may engage"
    );
    assert!(governor.memtable_breaker_engaged());
    assert!(
        governor.ingest_pressure_needs_drain(),
        "memtable pressure must request the drain the sampler was not asking for"
    );
    let admission = governor.check_ingest_admission().unwrap_err().to_string();
    assert!(
        admission.contains("limits.max_process_memory_mb"),
        "{admission}"
    );

    let stuck = engine.total_memtable_bytes();
    for attempt in 0..2 {
        let err = engine
            .get_index("a")
            .unwrap()
            .index_document(Some(format!("rejected-{attempt}")), json!({ "body": "no" }))
            .await
            .expect_err("a write over the summed budget must be rejected");
        let rendered = err.to_string();
        assert!(
            rendered.contains("limits.max_process_memory_mb"),
            "attempt {attempt}: {rendered}"
        );
        assert_eq!(
            engine.total_memtable_bytes(),
            stuck,
            "attempt {attempt}: a rejected write must not move the indexed bytes"
        );
    }
    let visible_while_stuck: u64 = {
        let mut total = 0;
        for name in names {
            total += count_all(&engine, name).await;
        }
        total
    };
    assert_eq!(
        visible_while_stuck, accepted as u64,
        "the two rejected writes must not increase the searchable count (the no-progress loop)"
    );

    let drained = engine.drain_rebuildable_memory().await;
    assert_eq!(drained, names.len(), "every loaded index is drained");
    let after = engine.total_memtable_bytes();
    assert!(
        after < 1024 * 1024,
        "the drain must flush the memtables that were holding the budget (still {after} bytes)"
    );
    governor.refresh_memtable(after);
    assert!(!governor.memtable_breaker_engaged());
    assert!(!governor.ingest_pressure_needs_drain());
    assert!(governor.check_ingest_admission().is_ok());

    engine
        .get_index("a")
        .unwrap()
        .index_document(Some("after-drain".into()), json!({ "body": "progress" }))
        .await
        .expect("a write after the drain must be accepted");
    engine
        .get_index("a")
        .unwrap()
        .refresh()
        .await
        .expect("refresh");
    let mut visible = 0u64;
    for name in names {
        visible += count_all(&engine, name).await;
    }
    assert_eq!(
        visible,
        (accepted + 1) as u64,
        "the docs the breaker was sitting on, plus the one write the drain made possible"
    );
}
