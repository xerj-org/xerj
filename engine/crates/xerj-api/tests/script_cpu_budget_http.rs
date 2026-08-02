//! Issue #122, from the outside: a script that is *legal* by every structural
//! measure but arbitrarily expensive per document must reach the HTTP caller
//! as a 400, not as a search that runs for as long as the script wants.
//!
//! The script below is flat, closure-free, 44 KiB (under the 64 KiB source
//! limit), nests nothing, and calls nothing — so `MAX_PARSE_DEPTH`,
//! `MAX_EVAL_DEPTH`, `MAX_CALL_DEPTH`, `MAX_CALL_COUNT` and `MAX_SCRIPT_LEN`
//! all pass it. Measured on the pre-fix tree in release it cost 2.80 s of CPU
//! **per document**, and a request `timeout` could not interrupt it, because
//! the doc-scan poll only checks a document *boundary*.
//!
//! The counterpart test matters as much: an ordinary script must still return
//! 200. A budget that answers 400 too eagerly would pass the loudness test and
//! break every real client.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

/// A flat script whose cost is quadratic in its own statement count: each
/// statement concatenates a 1 KiB chunk onto an accumulator the previous
/// statement just grew, so 4,000 statements copy ~8 GB. Nothing about its
/// *shape* is unusual.
fn quadratic_concat_script(statements: usize) -> String {
    let mut src = String::with_capacity(1024 + statements * 10 + 64);
    src.push_str("def c = \"");
    src.push_str(&"x".repeat(1024));
    src.push_str("\";\ndef s = c;\n");
    for _ in 0..statements {
        src.push_str("s = s + c;\n");
    }
    src.push_str("return s.length();");
    src
}

async fn seeded_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);

    state
        .engine
        .create_index("scripts", xerj_common::types::Schema::empty())
        .expect("create_index");
    let idx = state.engine.get_index("scripts").expect("get_index");
    for id in 1..=8 {
        idx.index_document(Some(id.to_string()), json!({ "rank": id }))
            .await
            .expect("index_document");
    }
    idx.refresh().await.expect("refresh");
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn script_score_body(source: String) -> Value {
    json!({
        "query": {
            "function_score": {
                "query": { "match_all": {} },
                "functions": [ { "script_score": { "script": { "source": source } } } ]
            }
        }
    })
}

/// The headline. Pre-fix this returned `200 OK` after burning 2.80 s of CPU
/// *per matching document* — eight documents, so ~22 s of CPU for one
/// unauthenticated request that no `timeout` could shorten.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legal_but_arbitrarily_expensive_script_is_a_400_not_a_long_wait() {
    let (app, _dir) = seeded_app().await;
    let source = quadratic_concat_script(4000);
    assert!(
        source.len() < 64 * 1024,
        "the repro must clear the source-size limit, got {} bytes",
        source.len()
    );

    let started = std::time::Instant::now();
    let (status, body) = post(&app, "/scripts/_search", script_score_body(source)).await;
    let elapsed = started.elapsed();

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unbudgeted script must not answer 200; body {body}"
    );
    assert_eq!(
        body["error"]["type"], "script_exception",
        "the work budget must surface through the resource-limit class, got {body}"
    );
    let reason = body["error"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("budget"),
        "the error must name the budget so the caller knows the remedy, got {reason:?}"
    );
    // Generous by two orders of magnitude against the 22 s of CPU this used to
    // cost, so a slow CI box cannot make it flaky while it still fails loudly
    // if the budget stops bounding anything.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the request was still unbounded: {elapsed:?}"
    );
}

/// The other half of the contract: an ordinary `script_score` must be entirely
/// unaffected, 200 with real scores.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ordinary_script_score_is_untouched_by_the_budget() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_search",
        script_score_body("doc['rank'].value * 2 + _score".to_string()),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body {body}");
    let hits = body["hits"]["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 8, "body {body}");
    assert!(
        hits.iter()
            .all(|hit| hit["_score"].as_f64().unwrap_or(0.0) > 0.0),
        "the budget degraded ordinary scores to zero: {body}"
    );
}

/// A script that is merely *outside* the supported Painless subset must keep
/// degrading quietly — the fault sink is for resource limits only, and the
/// ES-compat surface depends on that distinction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unsupported_script_still_degrades_instead_of_400ing() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_search",
        script_score_body("someUnsupportedThing(1,2,3)".to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an out-of-subset script must not become a 400; body {body}"
    );
}
