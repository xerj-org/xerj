//! Issue #97, from the outside: a Painless **resource limit** must reach the
//! HTTP caller instead of being absorbed into a plausible-looking answer.
//!
//! These tests deliberately touch nothing but the wire. They compile against
//! the pre-fix engine (no `SearchResult::script_failure`, no
//! `is_resource_limit_error`), so running them on the unfixed tree shows the
//! real defect: `200 OK` with `_score: 0.0` from a scoring script that never
//! ran, and a `script_fields` entry that silently vanished.
//!
//! The counterpart matters just as much: a script that is merely *outside* our
//! Painless subset must keep degrading quietly, because the ES-compat surface
//! depends on it. A fix that turns every unparseable script into a 400 would
//! pass the loudness tests and break real clients.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Stack size for every thread these tests evaluate a script on, replacing the
/// 2 MiB `std`/`libtest` default that `#[tokio::test]` would have given them.
///
/// `xerj_engine::painless::MAX_CALL_DEPTH` is a **stack** budget: 32 is
/// documented as measured in a *release* build, where the adversarial worst
/// case costs 1.20 MiB of a 2 MiB worker stack and the rest is left for the
/// axum/search frames underneath. A debug build's frames are several times
/// larger, so that same recursion lands much closer to the edge here than the
/// constant's own arithmetic implies — and `_delete_by_query` is the deepest
/// of these paths, since it runs a search inside the by-query wrapper.
///
/// Adding the issue-#79 authorization layer to the router pushed the debug
/// profile of that one path over 2 MiB. The cost is the *layer*, not the
/// authorization: these tests configure no admin key, so the caller is a
/// superuser and `authz_middleware` returns on its second statement. Measured
/// on this branch by bisecting `RUST_MIN_STACK`,
/// `delete_by_query_refuses_a_selection_a_script_limit_truncated` aborts at
/// 2,097,152 bytes and passes at 2,359,296 — roughly 200 KiB of debug frames
/// for one more `tower` layer. The same test passes on `main` at the default,
/// so the margin it had there was under 200 KiB.
///
/// The release binary was checked directly rather than inferred: the same
/// recursion with 10, 50 and 90 nested blocks in the closure body (90 being
/// the documented worst case) answers `400 script_exception` on a running
/// release node, which stays up. So the production margin still holds and the
/// budget in `painless.rs` is unchanged.
///
/// Sizing the stack here keeps these tests measuring what they are for — that
/// the depth guard trips and the refusal reaches the caller — instead of
/// measuring how large `rustc -C opt-level=0` makes an async frame.
const WORKER_STACK_BYTES: usize = 4 * 1024 * 1024;

/// Drive `fut` to completion with every stack involved sized to
/// [`WORKER_STACK_BYTES`].
///
/// Both threads have to be sized, because the two shapes here evaluate the
/// script in different places: `_search` hands the work to a task
/// (`search_impl` spawns it) so it lands on a **runtime worker**, while
/// `_delete_by_query` with the default `wait_for_completion=true` awaits
/// `run_delete_by_query` inline, so it lands on the thread that called
/// `block_on` — which under `libtest` is the per-test thread, and that one
/// takes its size from `RUST_MIN_STACK` (2 MiB by default), not from the
/// runtime builder.
fn block_on_sized<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
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

/// Self-application recursion that runs past `MAX_CALL_DEPTH` (32), with the
/// recursive call wrapped in nested blocks — the shape from the PR #88
/// process-abort repro. Short enough to clear the static source-size and
/// parse-depth guards, so the limit can only trip mid-evaluation, which is
/// exactly the case that had no error channel.
fn over_deep_script() -> String {
    let open = "if(true){".repeat(10);
    let close = "}".repeat(10);
    format!("def f = (g, n) -> {{ {open} return g(g, n); {close} return 0; }}; return f(f, 1);")
}

/// One index, one document, ready to search.
///
/// The `TempDir` is returned, not `keep()`-ed: each test gets its own data
/// directory and gives it back when it ends.
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
    idx.index_document(Some("1".into()), json!({ "rank": 7, "tags": ["a", "b"] }))
        .await
        .expect("index_document");
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

/// The headline of #97. `apply_function_score` returns a bare `f32`, so it
/// mapped the call-depth error to `0.0` and the request succeeded with a score
/// that no script produced.
#[test]
fn script_score_past_the_call_depth_limit_is_an_error_not_a_zero_score() {
    block_on_sized(script_score_past_the_call_depth_limit_is_an_error_not_a_zero_score_inner());
}

async fn script_score_past_the_call_depth_limit_is_an_error_not_a_zero_score_inner() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_search",
        json!({
            "query": {
                "function_score": {
                    "query": { "match_all": {} },
                    "functions": [
                        { "script_score": { "script": { "source": over_deep_script() } } }
                    ]
                }
            }
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a scoring script that hit the closure call-depth limit was served as a \
         successful search: {body}"
    );
    assert_eq!(body["error"]["type"], "script_exception", "body: {body}");
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("closure call depth"),
        "the reason must name the limit that tripped: {body}"
    );
}

/// `script_fields` are evaluated by the `_search` handler itself, during
/// response assembly and outside the engine's search task. The old
/// `if let Ok(pv)` dropped a limit trip on the floor and the field just wasn't
/// in the response — indistinguishable from a script that legitimately
/// returned nothing.
#[test]
fn script_field_past_the_call_depth_limit_is_an_error_not_a_missing_field() {
    block_on_sized(script_field_past_the_call_depth_limit_is_an_error_not_a_missing_field_inner());
}

async fn script_field_past_the_call_depth_limit_is_an_error_not_a_missing_field_inner() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_search",
        json!({
            "query": { "match_all": {} },
            "script_fields": {
                "computed": { "script": { "source": over_deep_script() } }
            }
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a script_field that hit the closure call-depth limit vanished from an \
         otherwise successful response: {body}"
    );
    assert_eq!(body["error"]["type"], "script_exception", "body: {body}");
}

/// A script-bucketed terms agg mapped a limit trip to "no buckets" — an empty
/// aggregation that reads as a legitimate answer.
#[test]
fn script_bucketed_agg_past_the_call_depth_limit_is_an_error_not_empty_buckets() {
    block_on_sized(
        script_bucketed_agg_past_the_call_depth_limit_is_an_error_not_empty_buckets_inner(),
    );
}

async fn script_bucketed_agg_past_the_call_depth_limit_is_an_error_not_empty_buckets_inner() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_search",
        json!({
            "size": 0,
            "query": { "match_all": {} },
            "aggs": { "by_script": { "terms": { "script": { "source": over_deep_script() } } } }
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a script-bucketed agg that hit the call-depth limit returned buckets as \
         a complete answer: {body}"
    );
    assert_eq!(body["error"]["type"], "script_exception", "body: {body}");
}

/// `_delete_by_query` selects with the same matching machinery. A fail-closed
/// script trip there means deleting an arbitrary subset and reporting a
/// completed run — destructive as well as wrong.
#[test]
fn delete_by_query_refuses_a_selection_a_script_limit_truncated() {
    block_on_sized(delete_by_query_refuses_a_selection_a_script_limit_truncated_inner());
}

async fn delete_by_query_refuses_a_selection_a_script_limit_truncated_inner() {
    let (app, _dir) = seeded_app().await;
    let (_, body) = post(
        &app,
        "/scripts/_delete_by_query",
        json!({
            "query": {
                "terms_set": {
                    "tags": {
                        "terms": ["a", "b"],
                        "minimum_should_match_script": { "source": over_deep_script() }
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(
        body["error"]["type"], "script_exception",
        "delete_by_query ran against a silently truncated selection: {body}"
    );
    assert!(
        body.get("deleted").is_none(),
        "a refused delete must not also report a deletion count: {body}"
    );
}

/// The other half of the contract, and the reason the error type has to
/// distinguish the two classes: a script our interpreter simply doesn't
/// support is NOT a resource limit. It must keep degrading to a neutral score
/// on a 200, exactly as before, or every out-of-subset script in the ES-compat
/// surface becomes a spurious 400.
#[test]
fn unsupported_script_syntax_still_degrades_quietly() {
    block_on_sized(unsupported_script_syntax_still_degrades_quietly_inner());
}

async fn unsupported_script_syntax_still_degrades_quietly_inner() {
    let (app, _dir) = seeded_app().await;
    for source in [
        "someUnsupportedThing(1,2,3)",
        "unknown_identifier_here",
        "doc['missing'].value.someMethodWeDoNotHave()",
    ] {
        let (status, body) = post(
            &app,
            "/scripts/_search",
            json!({
                "query": {
                    "function_score": {
                        "query": { "match_all": {} },
                        "functions": [ { "script_score": { "script": { "source": source } } } ]
                    }
                }
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an out-of-subset script must not become a 400 ({source}): {body}"
        );
        assert_eq!(body["hits"]["total"]["value"], 1, "body: {body}");
    }
}

/// A search that trips a limit must not leave the response cache poisoned with
/// its degraded scores, and the fault must not be re-reported against the next
/// caller of a different query.
#[test]
fn a_faulted_search_is_neither_cached_nor_replayed() {
    block_on_sized(a_faulted_search_is_neither_cached_nor_replayed_inner());
}

async fn a_faulted_search_is_neither_cached_nor_replayed_inner() {
    let (app, _dir) = seeded_app().await;
    let bad = json!({
        "query": {
            "function_score": {
                "query": { "match_all": {} },
                "functions": [
                    { "script_score": { "script": { "source": over_deep_script() } } }
                ]
            }
        }
    });

    let (first, _) = post(&app, "/scripts/_search", bad.clone()).await;
    assert_eq!(first, StatusCode::BAD_REQUEST);
    let (second, body) = post(&app, "/scripts/_search", bad).await;
    assert_eq!(
        second,
        StatusCode::BAD_REQUEST,
        "the repeat request was served the cached degraded result: {body}"
    );

    let (clean, body) = post(
        &app,
        "/scripts/_search",
        json!({ "query": { "match_all": {} } }),
    )
    .await;
    assert_eq!(
        clean,
        StatusCode::OK,
        "an unrelated request inherited another one's script fault: {body}"
    );
}
