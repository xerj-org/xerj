//! Regression tests for issue #97: a Painless resource limit must never be
//! absorbed into a plausible-looking value.
//!
//! PR #88 bounded closure recursion correctly — the process no longer aborts.
//! But the scoring paths (`apply_function_score`, `apply_rescore`, terms_set
//! `min_should_match`, script-bucketed aggs) return bare `f32`/`Value` with no
//! error channel, so they mapped the limit error to `0.0` / `[]`. A scoring
//! script that recursed past `MAX_CALL_DEPTH` therefore produced a WRONG SCORE
//! and reported success. These tests pin the loud behavior.
//!
//! Issue #353: this file never got the explicitly-sized-stack harness its
//! sibling `xerj-api/tests/script_limits_http.rs` already has, so every
//! `#[tokio::test]` here ran on the 2 MiB `std`/`libtest` default while
//! production runs on `RT_THREAD_STACK_SIZE`. A debug build's frames need more
//! than that and the whole binary aborted with a stack overflow. Measured on
//! this branch by bisecting `RUST_MIN_STACK` against the unfixed file: the dev
//! profile aborts at 2,359,296 bytes and passes at 2,621,440; the `ci-test`
//! profile (which inherits `release` codegen) is green on the stock 2 MiB,
//! which is why CI never saw it.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// Stack size for every thread these tests evaluate a script on, replacing the
/// 2 MiB `std`/`libtest` default that `#[tokio::test]` would have given them.
///
/// Equal to the *smallest* stack any production worker gets: `xerj-server`'s
/// `RT_THREAD_STACK_SIZE` pins 4 MiB in a release build (and more in a debug
/// one). `xerj_engine::painless::MAX_CALL_DEPTH` is a **stack** budget, and its
/// 1.20 MiB worst case is an optimized-build measurement, so what these tests
/// cost is a function of codegen — sizing the stack here keeps them measuring
/// that the depth guard trips and the fault reaches `SearchResult`, instead of
/// measuring how large `rustc -C opt-level=0` makes an async frame.
///
/// The budget assertions at the bottom of this file size their own threads and
/// are deliberately NOT routed through this: there the stack size is the thing
/// under test, not scaffolding.
const WORKER_STACK_BYTES: usize = 4 * 1024 * 1024;

/// Drive `fut` to completion on `builder`'s runtime with every stack involved
/// sized to [`WORKER_STACK_BYTES`].
///
/// Both stacks have to be sized. The runtime builder covers work that lands on
/// a worker thread; the outer `std` thread covers `block_on` itself, which runs
/// on the caller — and under `libtest` the caller is the per-test thread, whose
/// size comes from `RUST_MIN_STACK` (2 MiB by default), not from the builder.
fn block_on_sized<F>(mut builder: tokio::runtime::Builder, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || {
            builder
                .thread_stack_size(WORKER_STACK_BYTES)
                .enable_all()
                .build()
                .expect("test runtime")
                .block_on(fut)
        })
        .expect("spawn test thread")
        .join()
        .expect("test thread panicked");
}

/// `#[tokio::test]`'s default flavor, with sized stacks.
fn block_on_sized_current_thread<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    block_on_sized(tokio::runtime::Builder::new_current_thread(), fut);
}

/// `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, with sized
/// stacks. NOT interchangeable with the current-thread variant: `Index::search`
/// gates its `block_in_place` hand-off on an `is_multi_thread` check
/// (`index.rs`), so a current-thread runtime takes the other branch and the one
/// test that exists to cover that path would keep passing while covering
/// nothing.
fn block_on_sized_multi_thread<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.worker_threads(4);
    block_on_sized(builder, fut);
}

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
#[test]
fn script_score_over_call_depth_reports_a_failure_not_a_score() {
    block_on_sized_current_thread(
        script_score_over_call_depth_reports_a_failure_not_a_score_inner(),
    );
}

async fn script_score_over_call_depth_reports_a_failure_not_a_score_inner() {
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
#[test]
fn ordinary_script_error_still_degrades_gracefully() {
    block_on_sized_current_thread(ordinary_script_error_still_degrades_gracefully_inner());
}

async fn ordinary_script_error_still_degrades_gracefully_inner() {
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
#[test]
fn call_depth_limit_survives_the_multi_thread_block_in_place_path() {
    // `block_on_sized_multi_thread`, not the current-thread variant: the
    // `is_multi_thread` check is what selects the path this test is named for.
    block_on_sized_multi_thread(
        call_depth_limit_survives_the_multi_thread_block_in_place_path_inner(),
    );
}

async fn call_depth_limit_survives_the_multi_thread_block_in_place_path_inner() {
    // Nothing below would notice if this ran on the other flavor — the fault is
    // reported either way — so the coverage this test exists for could be lost
    // silently by picking the wrong helper. Pin the flavor, not just the
    // outcome.
    assert_eq!(
        tokio::runtime::Handle::current().runtime_flavor(),
        tokio::runtime::RuntimeFlavor::MultiThread,
        "must run on the multi-thread flavor, or `Index::search`'s \
         `is_multi_thread` check takes the branch without `block_in_place`"
    );

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
#[test]
fn terms_set_min_should_match_script_over_call_depth_reports_a_failure() {
    block_on_sized_current_thread(
        terms_set_min_should_match_script_over_call_depth_reports_a_failure_inner(),
    );
}

async fn terms_set_min_should_match_script_over_call_depth_reports_a_failure_inner() {
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
#[test]
fn rescore_script_over_call_depth_reports_a_failure() {
    block_on_sized_current_thread(rescore_script_over_call_depth_reports_a_failure_inner());
}

async fn rescore_script_over_call_depth_reports_a_failure_inner() {
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
#[test]
fn script_bucketed_terms_agg_over_call_depth_reports_a_failure() {
    block_on_sized_current_thread(
        script_bucketed_terms_agg_over_call_depth_reports_a_failure_inner(),
    );
}

async fn script_bucketed_terms_agg_over_call_depth_reports_a_failure_inner() {
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
#[test]
fn script_failure_is_not_cached_or_replayed() {
    block_on_sized_current_thread(script_failure_is_not_cached_or_replayed_inner());
}

async fn script_failure_is_not_cached_or_replayed_inner() {
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

/// Run `src` to completion on a thread with exactly `stack_bytes` of stack and
/// return the error. If the call-depth guard regresses, the recursion overflows
/// that stack and the whole test process aborts — this cannot fail quietly.
fn eval_on_a_stack(stack_bytes: usize, src: String) -> String {
    std::thread::Builder::new()
        .stack_size(stack_bytes)
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

/// The 2 MiB a tokio worker gets when nobody sets `thread_stack_size` — the
/// floor the budget assertions below are written against.
fn eval_on_a_2mib_stack(src: String) -> String {
    eval_on_a_stack(2 * 1024 * 1024, src)
}

/// The adversarial worst case: the closure body nested as deep as the parser
/// allows, so every call level also pays ~90 `exec_stmt` frames.
fn worst_case_nested_script() -> String {
    let nest = 90;
    format!(
        "def f = (g, n) -> {{ {} return g(g, n); {} return 0; }}; return f(f, 1);",
        "if(true){".repeat(nest),
        "}".repeat(nest),
    )
}

/// PR #88's process-abort repro must stay dead: the shape from the issue —
/// closure self-application with 10 nested `if(true){}` blocks in the body.
/// Measured at ~186 KiB of the 2 MiB stack in release.
#[test]
fn nested_block_closure_recursion_does_not_abort_on_a_2mib_stack() {
    let err = eval_on_a_2mib_stack(over_deep_script());
    assert!(err.contains("closure call depth"), "got {err:?}");
}

/// [`worst_case_nested_script`] measured at 40,720 bytes/level → 1.20 MiB at
/// `MAX_CALL_DEPTH` = 32, which is why that ceiling cannot be raised. Release
/// only — debug frames are several times larger and would legitimately need
/// more than 2 MiB; see the debug counterpart below for how much more.
#[cfg(not(debug_assertions))]
#[test]
fn worst_case_nested_block_recursion_still_fits_a_2mib_stack() {
    let err = eval_on_a_2mib_stack(worst_case_nested_script());
    assert!(err.contains("closure call depth"), "got {err:?}");
}

/// The debug half of the same budget, and the measurement behind #353's
/// profile-aware `RT_THREAD_STACK_SIZE` in `xerj-server`.
///
/// `MAX_CALL_DEPTH` is a stack budget, so its cost is a function of codegen:
/// the *same* worst case that fits 1.20 MiB at `opt-level=3` needs a measured
/// 10,223,616 bytes at `opt-level=0` (bisected on a 256 KiB grid on
/// `aarch64-unknown-linux-gnu`; 50 nested blocks already needs 5,767,168). That
/// is why a flat 4 MiB pin was a release-only number and why a `cargo run`
/// server aborted on a 50-block script. Runs on the debug branch of
/// `RT_THREAD_STACK_SIZE` (32 MiB), so this also checks that the pin's margin
/// really covers the evaluator on whatever architecture is running it; a
/// regression aborts the process rather than failing quietly.
#[cfg(debug_assertions)]
#[test]
fn worst_case_nested_block_recursion_fits_the_debug_worker_stack() {
    let err = eval_on_a_stack(32 * 1024 * 1024, worst_case_nested_script());
    assert!(err.contains("closure call depth"), "got {err:?}");
}
