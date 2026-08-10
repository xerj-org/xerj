//! `engine.{flush,merge,search}_workers` must reach the pools they name.
//!
//! Before #240 all three were declared, defaulted, documented and validated in
//! `xerj-common::config` and read by **nothing**. Setting
//! `engine.search_workers = 2` to keep XERJ off a laptop's cores changed no
//! pool and produced no warning — the accepted-and-ignored class from #204.
//!
//! Search segment fan-out runs on rayon's *global* pool, which rayon sizes to
//! every core on first use unless something builds it first. That makes the
//! global pool's width the observable for `search_workers`, and it is
//! process-global state — hence a test file of its own, since each Rust
//! integration-test file is its own binary.

use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_engine::pools;
use xerj_engine::Engine;

#[tokio::test]
async fn search_workers_sizes_the_pool_search_actually_runs_on() {
    let requested = 2;
    let machine = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap();
    assert!(
        machine > requested,
        "this assertion is only meaningful on a machine with more cores than we ask for \
         (machine={machine})"
    );

    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.engine.search_workers = requested;
    config.engine.merge_workers = 3;
    config.engine.flush_workers = 5;
    let _engine = Engine::new(config).expect("engine::new");

    // Pre-fix this reported `machine` (32 on the dev box): the config value was
    // never read, so search fan-out took every core no matter what was asked.
    assert_eq!(
        rayon::current_num_threads(),
        requested,
        "engine.search_workers must size the global pool that search fans out on"
    );

    let sizing = pools::sizing();
    assert_eq!(sizing.search, requested);
    assert_eq!(
        sizing.merge, 3,
        "engine.merge_workers must reach merge_pool"
    );
    assert_eq!(
        sizing.flush_finalize, 5,
        "engine.flush_workers must reach the flush finalize gate"
    );
}

#[test]
fn out_of_range_worker_counts_are_refused_not_clamped() {
    for name in [
        "engine.flush_workers",
        "engine.merge_workers",
        "engine.search_workers",
    ] {
        for bad in [0usize, usize::MAX] {
            let mut engine = Config::default().engine;
            match name {
                "engine.flush_workers" => engine.flush_workers = bad,
                "engine.merge_workers" => engine.merge_workers = bad,
                _ => engine.search_workers = bad,
            }
            let err = engine
                .validate()
                .expect_err("an unusable worker count must be refused, never clamped");
            assert!(
                err.to_string().contains(name),
                "the error must name the setting, got: {err}"
            );
        }
    }
    // merge_workers = 0 used to pass validation entirely: only flush_workers
    // was checked, and nothing read any of the three anyway.
    assert!(Config::default().engine.validate().is_ok());
}
