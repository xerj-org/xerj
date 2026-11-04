//! `Engine::create_*_template` is insert-if-absent as one step.
//!
//! A check in the handler followed by a separate put would let two concurrent
//! `?create=true` requests for the same name both see it free and both
//! succeed, the second silently overwriting the first. The engine checks and
//! inserts under the map's entry lock, so exactly one caller wins, the winner's
//! body is what is stored, and that result survives a reopen.

use std::sync::{Arc, Barrier};

use serde_json::{json, Value};
use xerj_common::config::{Config, WalSync};
use xerj_engine::engine::IndexTemplate;
use xerj_engine::Engine;

const RACERS: usize = 16;

fn config_for(path: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.server.data_dir = path.to_string_lossy().into_owned();
    config.storage.wal_sync = WalSync::Async;
    config
}

/// Run `RACERS` threads that all call `create(engine, i)` at once; return the
/// index of the single winner.
fn race(engine: &Arc<Engine>, create: fn(&Engine, usize) -> bool) -> usize {
    let barrier = Arc::new(Barrier::new(RACERS));
    let handles: Vec<_> = (0..RACERS)
        .map(|i| {
            let engine = Arc::clone(engine);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                create(&engine, i)
            })
        })
        .collect();
    let winners: Vec<usize> = handles
        .into_iter()
        .enumerate()
        .filter_map(|(i, h)| h.join().expect("racer").then_some(i))
        .collect();
    assert_eq!(winners.len(), 1, "exactly one create may win: {winners:?}");
    winners[0]
}

fn body(i: usize) -> Value {
    json!({ "index_patterns": [format!("racer-{i}-*")] })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_legacy_creates_have_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(Engine::new(config_for(dir.path())).expect("engine"));
    let winner = race(&engine, |e, i| {
        e.create_legacy_template("t".into(), body(i))
            .expect("create")
    });
    assert_eq!(
        *engine.legacy_templates.get("t").expect("stored"),
        body(winner)
    );
    drop(engine);

    let reopened = Engine::new(config_for(dir.path())).expect("reopen");
    assert_eq!(
        *reopened.legacy_templates.get("t").expect("persisted"),
        body(winner)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_component_creates_have_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(Engine::new(config_for(dir.path())).expect("engine"));
    let winner = race(&engine, |e, i| {
        e.create_component_template("t".into(), body(i))
            .expect("create")
    });
    assert_eq!(
        *engine.component_templates.get("t").expect("stored"),
        body(winner)
    );
    drop(engine);

    let reopened = Engine::new(config_for(dir.path())).expect("reopen");
    assert_eq!(
        *reopened.component_templates.get("t").expect("persisted"),
        body(winner)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_index_template_creates_have_one_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(Engine::new(config_for(dir.path())).expect("engine"));
    let winner = race(&engine, |e, i| {
        let t = IndexTemplate {
            index_patterns: vec![format!("racer-{i}-*")],
            settings: json!({}),
            mappings: json!({}),
            priority: i as i32,
        };
        e.create_index_template("t".into(), t).expect("create")
    });
    let stored = engine.templates.get("t").expect("stored").clone();
    assert_eq!(stored.index_patterns, vec![format!("racer-{winner}-*")]);
    assert_eq!(stored.priority, winner as i32);
    drop(engine);

    let reopened = Engine::new(config_for(dir.path())).expect("reopen");
    let persisted = reopened.templates.get("t").expect("persisted").clone();
    assert_eq!(persisted.priority, winner as i32);
}

/// A taken name returns `Ok(false)` and leaves the stored body alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_taken_name_is_reported_and_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new(config_for(dir.path())).expect("engine");
    assert!(engine
        .create_legacy_template("t".into(), body(0))
        .expect("first"));
    assert!(!engine
        .create_legacy_template("t".into(), body(1))
        .expect("second"));
    assert_eq!(*engine.legacy_templates.get("t").expect("stored"), body(0));
}
