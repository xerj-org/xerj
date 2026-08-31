//! Regression test for issue #751 — the CI hang.
//!
//! `Index::search` runs the whole search body inside
//! `block_in_place(|| Handle::current().block_on(search_fut))` on a
//! multi-thread runtime (the M5.21 read-throughput fix). `block_in_place`
//! hands the worker's core to a **tokio blocking-pool** thread and keeps the
//! calling thread parked for the entire search — await points included.
//!
//! An aggregation that has no columnar fast path then assembles the full
//! corpus, and a cold flushed segment is hydrated by
//! `Index::stored_values_for_async`. Before the fix that hydration queued its
//! decode with `tokio::task::spawn_blocking` — i.e. **onto the same blocking
//! pool the search is already holding a thread of**. tokio only grows that
//! pool when it observes zero idle threads at push time; when it cannot grow
//! (or when the core hand-off has just consumed the one thread it thought was
//! idle) the decode task is queued behind threads that are all running worker
//! cores and never return to the pool. The search then waits for a task that
//! can never be scheduled — forever. That is the CI hang: `cargo test` at
//! default parallelism sat in
//! `script_bucketed_agg_past_the_call_depth_limit_is_an_error_not_empty_buckets`
//! until the 12-minute step timeout killed the job.
//!
//! The runtime below makes the starvation deterministic rather than a race:
//! one worker, and a blocking pool that may hold exactly one extra thread —
//! which `block_in_place`'s core hand-off takes. On the unfixed engine the
//! search never returns; the watchdog turns that into a named failure instead
//! of a hang, because a regression test for a hang must never hang.

use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// Long enough that a slow machine is never mistaken for the deadlock (the
/// search itself is single-digit milliseconds), short enough that a regression
/// fails a CI step instead of hanging it.
const WATCHDOG: Duration = Duration::from_secs(60);

fn test_config(dir: &TempDir) -> Config {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    // Keep every flush explicit: this test is about the *cold read* of an
    // already-published segment, not about flush scheduling.
    config.storage.flush_size_mb = 4096;
    config.storage.flush_interval_secs = 3600;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config
}

/// A `terms` agg keyed by a Painless script. Scripts carry no field mapping,
/// so no columnar/doc-values fast path can serve this — it forces the
/// full-corpus assembly that hydrates every flushed segment's stored section,
/// which is the path that deadlocked.
fn scripted_terms_agg() -> xerj_query::ast::SearchRequest {
    parse_request(&json!({
        "size": 0,
        "query": { "match_all": {} },
        "aggs": {
            "by_script": { "terms": { "script": { "source": "return doc['rank'];" } } }
        }
    }))
    .expect("parse_request")
}

#[test]
fn agg_corpus_hydration_completes_when_the_blocking_pool_cannot_grow() {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                // One worker + one spare blocking thread. `block_in_place`
                // inside `Index::search` hands the worker's core to that one
                // spare, so any *further* `spawn_blocking` submitted while the
                // search is in flight has no thread that will ever run it.
                .worker_threads(1)
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .expect("build test runtime");
            runtime.block_on(async move {
                let dir = TempDir::new().expect("tempdir");
                let engine = Engine::new(test_config(&dir)).expect("engine::new");
                engine
                    .create_index("aggs", Schema::empty())
                    .expect("create");
                let idx = engine.get_index("aggs").expect("get_index");
                idx.index_document(Some("1".into()), json!({ "rank": 7 }))
                    .await
                    .expect("index_document");
                // Publishes a segment; the stored section stays cold, so the
                // agg corpus below has to hydrate it.
                idx.refresh().await.expect("refresh");

                // The search must run ON A WORKER for `block_in_place` to take
                // its core — `Runtime::block_on`'s own thread holds no core and
                // would run the closure inline, hiding the bug.
                let searcher = std::sync::Arc::clone(&idx);
                tokio::spawn(async move { searcher.search(&scripted_terms_agg()).await })
                    .await
                    .expect("search task")
                    .expect("search");
                // Keep the data dir alive until the search is done.
                drop(dir);
            });
            let _ = tx.send(());
        })
        .expect("spawn test thread");

    match rx.recv_timeout(WATCHDOG) {
        Ok(()) => {
            worker.join().expect("test thread panicked");
        }
        Err(_) => panic!(
            "issue #751: the aggregation-corpus segment hydration never completed in {WATCHDOG:?}. \
             The cold-segment decode is waiting on the tokio blocking pool while the search that \
             needs it is itself parked on a blocking-pool thread inside `block_in_place` — a \
             self-deadlock, not a slow test."
        ),
    }
}
