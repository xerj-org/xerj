//! RC4 blocker 11 regression: scroll + async-search contexts are TTL-swept.
//!
//! Before the fix, `Engine.scrolls` (each entry pinning a fully-hydrated
//! `Vec<Hit>`) and `Engine.async_searches` (each entry pinning a stored
//! response JSON) were only ever removed by an explicit client DELETE —
//! normal client behavior (open scrolls, never clear) grew both maps, and
//! therefore process RSS, without bound. The engine now mirrors the PIT
//! lifecycle: TTLs on every context, sweep helpers (run opportunistically
//! on open + by a background task), expiry enforced on access, and a hard
//! open-context cap at the API layer.

use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_engine::engine::ScrollContext;
use xerj_engine::Engine;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn scroll_ctx(expires_in: i64) -> ScrollContext {
    let now = Instant::now();
    let keep_alive = Duration::from_secs(60);
    let expires_at = if expires_in >= 0 {
        now + Duration::from_secs(expires_in as u64)
    } else {
        now - Duration::from_secs((-expires_in) as u64)
    };
    ScrollContext {
        index: "idx".to_string(),
        hits: Vec::new(),
        position: 0,
        page_size: 10,
        created: now,
        keep_alive,
        expires_at,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_scroll_contexts_are_swept() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.scrolls.insert("dead".into(), scroll_ctx(-5));
    engine.scrolls.insert("live".into(), scroll_ctx(3600));

    let swept = engine.sweep_expired_scrolls();
    assert_eq!(swept, 1, "exactly the expired context is swept");
    assert!(engine.scrolls.get("dead").is_none(), "expired ctx freed");
    assert!(engine.scrolls.get("live").is_some(), "live ctx kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_async_search_results_are_swept() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    engine.async_searches.insert(
        "dead".into(),
        json!({ "id": "dead", "expiration_time_in_millis": now_ms - 1000 }),
    );
    engine.async_searches.insert(
        "live".into(),
        json!({ "id": "live", "expiration_time_in_millis": now_ms + 3_600_000 }),
    );
    // Defensive arm: an entry with no expiry field counts as expired —
    // every writer sets the field, so this only fires on corruption, and
    // "reclaim" beats "pin forever" there.
    engine
        .async_searches
        .insert("malformed".into(), json!({ "id": "malformed" }));

    let swept = engine.sweep_expired_async_searches();
    assert_eq!(swept, 2, "expired + malformed swept, live kept");
    assert!(engine.async_searches.get("dead").is_none());
    assert!(engine.async_searches.get("malformed").is_none());
    assert!(engine.async_searches.get("live").is_some());
}
