//! Regression tests for issue #97: a Painless resource limit must never be
//! absorbed into a plausible-looking value.
//!
//! PR #88 bounded closure recursion correctly — the process no longer aborts.
//! But the scoring paths (`apply_function_score`, `apply_rescore`, terms_set
//! `min_should_match`, script-bucketed aggs) return bare `f32`/`Value` with no
//! error channel, so they mapped the limit error to `0.0` / `[]`. A scoring
//! script that recursed past `MAX_CALL_DEPTH` therefore produced a WRONG SCORE
//! and reported success. These tests pin the loud behavior.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

fn test_engine() -> (Engine, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.storage.flush_size_mb = 4096;
    config.storage.flush_interval_secs = 3600;
    let engine = Engine::new(config).expect("engine::new");
    (engine, dir)
}

/// Self-application recursion that runs past `MAX_CALL_DEPTH` (32). The
/// closure body wraps the recursive call in nested blocks, the shape from the
/// original process-abort repro.
fn over_deep_script() -> String {
    let nested_open = "if(true){".repeat(10);
    let nested_close = "}".repeat(10);
    format!(
        "def f = (g, n) -> {{ {nested_open} return g(g, n); {nested_close} return 0; }}; \
         return f(f, 1);"
    )
}

async fn seed(engine: &Engine) -> std::sync::Arc<xerj_engine::index::Index> {
    engine.create_index("scripts", Schema::empty()).unwrap();
    let idx = engine.get_index("scripts").unwrap();
    idx.index_document(Some("1".into()), json!({ "rank": 7, "tags": ["a", "b"] }))
        .await
        .expect("index");
    idx.refresh().await.expect("refresh");
    idx
}

fn script_score_request(source: String) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({
        "query": {
            "function_score": {
                "query": { "match_all": {} },
                "functions": [ { "script_score": { "script": { "source": source } } } ]
            }
        }
    }))
    .expect("parse_request")
}

/// The headline of #97: a `script_score` that trips the call-depth limit used
/// to return `_score: 0.0` and a 200-shaped success. It must now surface the
/// limit on the result so the API layer can fail the request.
#[tokio::test]
async fn script_score_over_call_depth_reports_a_failure_not_a_score() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    let req = script_score_request(over_deep_script());
    let result = idx.search(&req).await.expect("search");

    let failure = result.script_failure.as_deref().unwrap_or_else(|| {
        panic!(
            "call-depth limit was swallowed: search succeeded with scores {:?} \
             and no script_failure — this is the silent wrong-score bug",
            result.hits.iter().map(|h| h.score).collect::<Vec<_>>()
        )
    });
    assert!(
        failure.contains("closure call depth"),
        "expected the closure-call-depth limit to be named, got {failure:?}"
    );
}

/// A script the interpreter simply doesn't support must keep degrading
/// gracefully — the fault sink is for *resource limits* only. Without this the
/// fix would turn every out-of-subset script into a failed search.
#[tokio::test]
async fn ordinary_script_error_still_degrades_gracefully() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    let req = script_score_request("someUnsupportedThing(1,2,3)".to_string());
    let result = idx.search(&req).await.expect("search");
    assert!(
        result.script_failure.is_none(),
        "an unsupported-syntax script must not be reported as a resource-limit \
         failure, got {:?}",
        result.script_failure
    );
}

/// The production runtime is multi-threaded, and `Index::search` then runs the
/// whole search body inside `block_in_place` + `Handle::block_on`. The fault
/// sink is a *task*-local, so this pins that it survives that hand-off — a
/// current-thread-only test would pass while production stayed silently wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_depth_limit_survives_the_multi_thread_block_in_place_path() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    let req = script_score_request(over_deep_script());
    let result = idx.search(&req).await.expect("search");
    assert!(
        result.script_failure.is_some(),
        "the multi-thread block_in_place path lost the fault; scores {:?}",
        result.hits.iter().map(|h| h.score).collect::<Vec<_>>()
    );
}

/// `terms_set`'s `minimum_should_match_script` is a *matching* path, not a
/// scoring one: a script error makes the doc unsatisfiable (fail-closed), so a
/// limit trip silently removes documents from the result set.
#[tokio::test]
async fn terms_set_min_should_match_script_over_call_depth_reports_a_failure() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    let req = parse_request(&json!({
        "query": {
            "terms_set": {
                "tags": {
                    "terms": ["a", "b"],
                    "minimum_should_match_script": { "source": over_deep_script() }
                }
            }
        }
    }))
    .expect("parse_request");

    let result = idx.search(&req).await.expect("search");
    assert!(
        result.script_failure.is_some(),
        "terms_set min_should_match swallowed the call-depth limit and reported \
         {} hits as a complete answer",
        result.total.value
    );
}

/// The `Rc<Vec<Stmt>>` follow-up note on #97: a shared closure body was the
/// only thing keeping `PainlessValue` off a thread boundary. This fails to
/// compile if it regresses to `Rc`.
#[test]
fn painless_values_can_cross_a_thread_boundary() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<xerj_engine::painless::PainlessValue>();
}

/// The rescore path has the same shape (`Err(_) => 0.0`) and the same bug.
#[tokio::test]
async fn rescore_script_over_call_depth_reports_a_failure() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    // `parse_request` doesn't read `rescore` (the API layer builds it), so
    // attach the stage directly.
    let mut req = parse_request(&json!({ "query": { "match_all": {} } })).expect("parse_request");
    req.rescore = vec![xerj_query::ast::RescoreQuery {
        window_size: 10,
        query: None,
        script: Some(xerj_query::ast::ScriptRescore {
            source: over_deep_script(),
            params: json!({}),
            query_weight: 1.0,
            rescore_query_weight: 1.0,
            score_mode: None,
        }),
    }];

    let result = idx.search(&req).await.expect("search");
    assert!(
        result.script_failure.is_some(),
        "rescore swallowed the call-depth limit; scores {:?}",
        result.hits.iter().map(|h| h.score).collect::<Vec<_>>()
    );
}

/// Script-bucketed aggregations run inside the search too, and mapped a failed
/// script to "no buckets" — an empty aggregation that looks like a legitimate
/// answer.
#[tokio::test]
async fn script_bucketed_terms_agg_over_call_depth_reports_a_failure() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    let req = parse_request(&json!({
        "size": 0,
        "query": { "match_all": {} },
        "aggs": {
            "by_script": { "terms": { "script": { "source": over_deep_script() } } }
        }
    }))
    .expect("parse_request");

    let result = idx.search(&req).await.expect("search");
    assert!(
        result.script_failure.is_some(),
        "the terms-agg script path swallowed the call-depth limit; aggs {:?}",
        result.aggs
    );
}

/// A failed search must not poison the response cache, and the fault must not
/// be replayed against the next (innocent) caller of the same query.
#[tokio::test]
async fn script_failure_is_not_cached_or_replayed() {
    let (engine, _dir) = test_engine();
    let idx = seed(&engine).await;

    let bad = parse_request(&json!({
        "query": {
            "function_score": {
                "query": { "match_all": {} },
                "functions": [ { "script_score": { "script": { "source": over_deep_script() } } } ]
            }
        }
    }))
    .expect("parse_request");
    assert!(idx
        .search(&bad)
        .await
        .expect("search")
        .script_failure
        .is_some());
    assert!(
        idx.search(&bad)
            .await
            .expect("search")
            .script_failure
            .is_some(),
        "the second identical request must fail too, not be served a cached \
         result whose scores are the degraded ones"
    );

    let good: xerj_query::ast::SearchRequest =
        parse_request(&json!({ "query": { "match_all": {} } })).expect("parse_request");
    let clean = idx.search(&good).await.expect("search");
    assert!(
        clean.script_failure.is_none(),
        "an unrelated request inherited another request's script fault: {:?}",
        clean.script_failure
    );
}

/// Run `src` to completion on a thread with exactly the 2 MiB stack a tokio
/// worker gets, and return the error. If the call-depth guard regresses, the
/// recursion overflows that stack and the whole test process aborts — this
/// cannot fail quietly.
fn eval_on_a_2mib_stack(src: String) -> String {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let doc = Value::Object(serde_json::Map::new());
            let params = Value::Object(serde_json::Map::new());
            let ctx = xerj_engine::painless::PainlessCtx::new(&doc, &params, 0.0);
            let err = xerj_engine::painless::eval_painless(&src, &ctx)
                .expect_err("the call-depth guard must trip");
            assert!(
                xerj_engine::painless::is_resource_limit_error(&err),
                "call-depth trip must classify as a resource limit, got {err:?}"
            );
            err
        })
        .expect("spawn")
        .join()
        .expect("the closure-recursion guard let the stack overflow")
}

/// PR #88's process-abort repro must stay dead: the shape from the issue —
/// closure self-application with 10 nested `if(true){}` blocks in the body.
/// Measured at ~186 KiB of the 2 MiB stack in release.
#[test]
fn nested_block_closure_recursion_does_not_abort_on_a_2mib_stack() {
    let err = eval_on_a_2mib_stack(over_deep_script());
    assert!(err.contains("closure call depth"), "got {err:?}");
}

/// The adversarial worst case for the same repro: the body nested as deep as
/// the parser allows, so every call level also pays ~90 `exec_stmt` frames.
/// Measured at 40,720 bytes/level → 1.20 MiB at `MAX_CALL_DEPTH` = 32, which
/// is why that ceiling cannot be raised. Release only — debug frames are
/// several times larger and would legitimately need more than 2 MiB.
#[cfg(not(debug_assertions))]
#[test]
fn worst_case_nested_block_recursion_still_fits_a_2mib_stack() {
    let nest = 90;
    let src = format!(
        "def f = (g, n) -> {{ {} return g(g, n); {} return 0; }}; return f(f, 1);",
        "if(true){".repeat(nest),
        "}".repeat(nest),
    );
    let err = eval_on_a_2mib_stack(src);
    assert!(err.contains("closure call depth"), "got {err:?}");
}
