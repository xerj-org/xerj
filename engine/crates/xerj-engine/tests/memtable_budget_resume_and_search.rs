//! Issue #2321: a summed memtable at the process budget must flush and then
//! accept writes, and a search after that must not scan the full buffer.
//!
//! rc.98 rejected those writes with HTTP 429 and did not flush, so a client
//! that retried crawled and then gave up. Searching the unflushed memtable
//! took about a minute; a restart (flush to segments) did not.
//!
//! This binary owns the process governor. One test, so a second engine cannot
//! install a different budget first.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::governor;
use xerj_engine::Engine;
use xerj_query::parse_request;

const BUDGET: u64 = 1024 * 1024;

fn make_engine(dir: &TempDir) -> Arc<Engine> {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    // 1 MiB stands in for the auto tier's 2 GiB. RSS watermark off, so only
    // the memtable latch can reject. Each index's own flush line is far
    // above what this test writes.
    config.limits.max_total_memtable_mb = 1;
    config.limits.memory_watermark_percent = 0;
    config.storage.flush_size_mb = 64;
    config.storage.flush_idle_secs = 0;
    config.storage.flush_interval_secs = 3600;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    Arc::new(Engine::new(config).expect("engine"))
}

fn text_schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
}

async fn fill_until_budget(engine: &Engine, names: &[&str], body: &str, tag: &str) -> usize {
    let mut accepted = 0usize;
    for n in 0..800 {
        let name = names[n % names.len()];
        engine
            .get_index(name)
            .unwrap()
            .index_document(Some(format!("{tag}-{name}-{n}")), json!({ "body": body }))
            .await
            .unwrap_or_else(|error| panic!("{tag} write {n} before the budget: {error}"));
        accepted += 1;
        if engine.total_memtable_bytes() >= BUDGET {
            break;
        }
    }
    accepted
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_resume_after_the_memtable_budget_fills_and_search_is_bounded() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    // The sampler stays off during the fill. Thread B would otherwise flush
    // as soon as the sum crossed the budget, and the rejection below would
    // depend on winning that race.
    let names = ["a", "b", "c", "d"];
    for name in names {
        engine
            .create_index(name, text_schema())
            .expect("create index");
    }
    let body = format!("budgettoken {}", "a".repeat(4096));
    let accepted = fill_until_budget(&engine, &names, &body, "fill").await;
    let summed = engine.total_memtable_bytes();
    assert!(
        summed >= BUDGET,
        "four indices must cross the 1 MiB budget (accepted {accepted}, summed {summed})"
    );
    for name in names {
        let idx = engine.get_index(name).unwrap();
        assert!(
            idx.memtable_bytes() < idx.flush_threshold(),
            "index {name} must still be under its own flush line"
        );
    }

    let gov = governor::global().expect("governor");
    gov.refresh_memtable(summed);
    assert!(
        gov.memtable_breaker_engaged(),
        "crossing the budget must engage the memtable breaker"
    );
    let rejected = engine
        .get_index("a")
        .unwrap()
        .index_document(Some("rejected".into()), json!({ "body": "no" }))
        .await
        .expect_err("a write at the budget must be rejected");
    let rejected = rejected.to_string();
    assert!(rejected.contains("memtable byte budget"), "{rejected}");

    // What `search_impl` does before it scans: publish while the breaker is
    // on. No sampler yet, so this is the only flush that can free the budget.
    let published = engine.publish_memtables_if_budget_engaged().await;
    assert!(published > 0, "the engaged breaker must flush a memtable");
    gov.refresh_memtable(engine.total_memtable_bytes());
    assert!(
        engine.total_memtable_bytes() < BUDGET,
        "publishing must leave the summed memtable under the budget"
    );
    assert!(!gov.memtable_breaker_engaged());

    let infos = engine.list_indices().await;
    let segments: usize = infos.iter().map(|info| info.segment_count).sum();
    assert!(
        segments > 0,
        "the pressure flush must publish a segment: {infos:?}"
    );

    let started = Instant::now();
    let req = parse_request(&json!({
        "query": { "match": { "body": "budgettoken" } },
        "size": 5,
        "from": 0
    }))
    .expect("parse");
    let mut hits = 0u64;
    for name in names {
        let page = engine
            .get_index(name)
            .unwrap()
            .search(&req)
            .await
            .expect("search");
        hits += page.total.value;
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "search after the flush took {elapsed:?} (the unflushed-memtable stall)"
    );
    assert!(hits > 0, "budgettoken must be searchable after the flush");
    assert!(
        hits as usize <= accepted,
        "search total {hits} exceeds accepted writes {accepted}"
    );

    // Second crossing, with the sampler running. Writes are rejected until
    // its drain publishes; the test does not call flush itself.
    engine.spawn_resource_sampler();
    let mut resumed = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    while resumed < accepted && Instant::now() < deadline {
        let name = names[resumed % names.len()];
        match engine
            .get_index(name)
            .unwrap()
            .index_document(
                Some(format!("again-{name}-{resumed}")),
                json!({ "body": body }),
            )
            .await
        {
            Ok(_) => resumed += 1,
            Err(error) => {
                let rendered = error.to_string();
                assert!(
                    rendered.contains("memtable byte budget"),
                    "while waiting for the sampler drain: {rendered}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    assert!(
        resumed == accepted,
        "the sampler drain must admit another {accepted} writes (got {resumed}, memtable {} bytes)",
        engine.total_memtable_bytes()
    );
    // The last admitted writes can sit over the budget until the sampler's
    // next drain. That interval is 2 s; give it a few windows.
    let settle = Instant::now() + Duration::from_secs(10);
    while engine.total_memtable_bytes() >= BUDGET && Instant::now() < settle {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    gov.refresh_memtable(engine.total_memtable_bytes());
    assert!(
        engine.total_memtable_bytes() < BUDGET,
        "the sampler drain must leave the summed memtable under the budget"
    );
    assert!(!gov.memtable_breaker_engaged());
}
