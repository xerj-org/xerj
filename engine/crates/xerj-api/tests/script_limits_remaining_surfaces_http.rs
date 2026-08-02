//! Issue #123: the surfaces PR #116 (issue #97) did not reach.
//!
//! #116 split Painless failures into "cannot evaluate" (degrade quietly, the
//! ES-compat contract) and "refused because a resource limit tripped" (must
//! surface), and wired the second class through ten request surfaces via
//! `SearchResult::script_failure`. `_rank_eval`, `_explain` and the pivot
//! transform runner were left reading the same field's absence as success.
//!
//! `_rank_eval` is the worst of the three: it exists to *measure relevance
//! quality*, so a silently degraded score does not merely return a bad answer,
//! it corrupts the number you would use to notice bad answers.
//!
//! The two `*_by_query` tests cover the second defect in the same issue: those
//! handlers refused correctly but shipped the refusal as `200 OK` with a
//! `{"error": …, "status": 400}` body, so a client branching on the HTTP status
//! saw a success and then found no `deleted` / `updated` key.
//!
//! As in #116, the last test is the guard rail in the other direction: a
//! script merely outside our Painless subset must keep scoring neutrally on a
//! 200. A "fix" that turns every unparseable script into a 400 passes every
//! loudness assertion here and breaks the compat surface.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Self-application recursion that runs past `MAX_CALL_DEPTH` (32), with the
/// recursive call wrapped in nested blocks. Short enough to clear the static
/// source-size and parse-depth guards, so the limit can only trip
/// mid-evaluation — the case with no error channel of its own.
fn over_deep_script() -> String {
    let open = "if(true){".repeat(10);
    let close = "}".repeat(10);
    format!("def f = (g, n) -> {{ {open} return g(g, n); {close} return 0; }}; return f(f, 1);")
}

/// A `terms_set` whose `minimum_should_match_script` trips the call-depth
/// limit. Unlike `script_score`, this one fails *matching* closed, so the
/// affected surfaces under-select rather than mis-score.
fn over_deep_matching_query() -> Value {
    json!({
        "terms_set": {
            "tags": {
                "terms": ["a", "b"],
                "minimum_should_match_script": { "source": over_deep_script() }
            }
        }
    })
}

/// One index, two documents, ready to search.
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
    idx.index_document(
        Some("1".into()),
        json!({ "rank": 7, "tags": ["a", "b"], "team": "red" }),
    )
    .await
    .expect("index_document");
    idx.index_document(
        Some("2".into()),
        json!({ "rank": 3, "tags": ["a"], "team": "blue" }),
    )
    .await
    .expect("index_document");
    idx.refresh().await.expect("refresh");
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(app, Request::post(path), body).await
}

async fn send(
    app: &axum::Router,
    builder: axum::http::request::Builder,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            builder
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

/// The headline of #123. `_rank_eval` scores each request with the same
/// `Index::search` the other surfaces use, so a `script_score` past the
/// call-depth limit degrades every hit to 0.0 — and then `_rank_eval`
/// publishes a `metric_score` computed over that degraded ranking as a
/// successful measurement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rank_eval_past_the_call_depth_limit_is_an_error_not_a_metric_score() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_rank_eval",
        json!({
            "requests": [{
                "id": "q1",
                "request": {
                    "query": {
                        "function_score": {
                            "query": { "match_all": {} },
                            "functions": [
                                { "script_score": { "script": { "source": over_deep_script() } } }
                            ]
                        }
                    }
                },
                "ratings": [{ "_index": "scripts", "_id": "1", "rating": 3 }]
            }],
            "metric": { "precision": { "k": 2 } }
        }),
    )
    .await;

    // Reported per request, not by failing the batch: `failures` is the channel
    // ES provides for exactly this, and a body of many requests must not lose
    // the good ones because of one bad script.
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["failures"]["q1"]["type"], "script_exception",
        "_rank_eval published a relevance measurement taken from scores a \
         resource-limited script never produced, with no failure recorded: {body}"
    );
    assert!(
        body["failures"]["q1"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("closure call depth"),
        "the reason must name the limit that tripped: {body}"
    );
    // The faulted request contributes nothing, so the published number is
    // computed only over requests that produced trustworthy scores.
    assert_eq!(
        body["metric_score"].as_f64(),
        Some(0.0),
        "a faulted request must not contribute to metric_score: {body}"
    );
    assert!(
        body["details"].get("q1").is_none(),
        "a faulted request must not appear in details as if it had been measured: {body}"
    );
}

/// A body where only ONE of several requests carries a limit-tripping script
/// must still measure the others. Failing all of them was the first attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rank_eval_failure_is_scoped_to_the_offending_request() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_rank_eval",
        json!({
            "requests": [
                {
                    "id": "good",
                    "request": { "query": { "match_all": {} } },
                    "ratings": [{ "_index": "scripts", "_id": "1", "rating": 3 }]
                },
                {
                    "id": "bad",
                    "request": { "query": over_deep_matching_query() },
                    "ratings": [{ "_index": "scripts", "_id": "1", "rating": 3 }]
                }
            ],
            "metric": { "precision": { "k": 2 } }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["failures"]["bad"]["type"], "script_exception",
        "the offending request must be recorded: {body}"
    );
    assert!(
        body["failures"].get("good").is_none(),
        "a healthy request must not be marked failed: {body}"
    );
    assert!(
        body["details"].get("good").is_some(),
        "a healthy request must still be measured when a sibling fails: {body}"
    );
}

/// A limit trip in the *matching* path truncates the ranking instead of
/// skewing it: fewer retrieved ids, so precision/recall are computed over a
/// selection the script silently cut short.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rank_eval_refuses_a_ranking_a_script_limit_truncated() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_rank_eval",
        json!({
            "requests": [{
                "id": "q1",
                "request": { "query": over_deep_matching_query() },
                "ratings": [{ "_index": "scripts", "_id": "1", "rating": 3 }]
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["failures"]["q1"]["type"], "script_exception",
        "_rank_eval measured a ranking that a fail-closed script truncated, with \
         no failure recorded: {body}"
    );
    assert!(
        body["details"].get("q1").is_none(),
        "a truncated ranking must not be published as a measurement: {body}"
    );
}

/// `_explain` answers a yes/no question — did this document match? — by
/// running the query with an `ids` filter. A fail-closed script makes the
/// answer `matched: false`, which is indistinguishable from a document that
/// genuinely does not match.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explain_past_the_call_depth_limit_is_an_error_not_an_unmatched_doc() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_explain/1",
        json!({ "query": over_deep_matching_query() }),
    )
    .await;

    // `_explain` is a diagnostic endpoint, so the fault is REPORTED rather than
    // refused: most script faults here come from scoring, which does not decide
    // `matched`, and refusing turned a correct 200 into a 400 for exactly the
    // person debugging the script. The caller still gets the verdict and never
    // gets it without being told a limit tripped.
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.get("matched").is_some(),
        "_explain must still answer the question it was asked: {body}"
    );
    let details = body["explanation"]["details"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        details.iter().any(|d| d["description"]
            .as_str()
            .unwrap_or_default()
            .contains("resource limit")),
        "_explain reported a match verdict decided by a script that was refused \
         mid-evaluation, without disclosing it: {body}"
    );
    assert!(
        details.iter().any(|d| d["description"]
            .as_str()
            .unwrap_or_default()
            .contains("closure call depth")),
        "the disclosed fault must name the limit that tripped: {body}"
    );
}

/// The second defect in #123. `run_delete_by_query` refuses correctly, but the
/// handler wrapped the refusal in `Json(..)` alone — HTTP 200 carrying
/// `{"error": …, "status": 400}`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_by_query_script_refusal_carries_its_own_http_status() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_delete_by_query",
        json!({ "query": over_deep_matching_query() }),
    )
    .await;

    assert_eq!(body["error"]["type"], "script_exception", "body: {body}");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the refusal body said 400 while the HTTP status said OK, so a client \
         branching on the status sees a successful delete: {body}"
    );
    assert!(
        body.get("deleted").is_none(),
        "a refused delete must not also report a deletion count: {body}"
    );

    // The refusal has to be a refusal: nothing may have been deleted.
    let (count_status, count_body) = post(&app, "/scripts/_count", json!({})).await;
    assert_eq!(count_status, StatusCode::OK, "body: {count_body}");
    assert_eq!(count_body["count"], 2, "body: {count_body}");
}

/// Same defect on `_update_by_query`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_by_query_script_refusal_carries_its_own_http_status() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_update_by_query",
        json!({ "query": over_deep_matching_query() }),
    )
    .await;

    assert_eq!(body["error"]["type"], "script_exception", "body: {body}");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the refusal body said 400 while the HTTP status said OK: {body}"
    );
    assert!(
        body.get("updated").is_none(),
        "a refused update must not also report an update count: {body}"
    );
}

/// A successful `_delete_by_query` must stay a 200 — the status now comes from
/// the body, so this pins the non-error half of that mapping.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_by_query_without_a_script_is_still_a_200() {
    let (app, _dir) = seeded_app().await;
    let (status, body) = post(
        &app,
        "/scripts/_delete_by_query",
        json!({ "query": { "term": { "team": "blue" } } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["deleted"], 1, "body: {body}");
}

/// A pivot transform runs a composite aggregation over `source.query` and
/// writes one document per bucket into `dest`. A fail-closed script in that
/// query silently shrinks the source set, so the transform materialises a
/// wrong summary and reports `acknowledged: true` over it — the same defect as
/// an under-delete, except the wrong numbers are now persisted in an index
/// that later queries will read as fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pivot_transform_refuses_a_source_a_script_limit_truncated() {
    let (app, _dir) = seeded_app().await;

    let (put_status, put_body) = send(
        &app,
        Request::put("/_transform/t1"),
        json!({
            "source": { "index": "scripts", "query": over_deep_matching_query() },
            "dest": { "index": "scripts_pivot" },
            "pivot": {
                "group_by": { "team": { "terms": { "field": "team" } } },
                "aggregations": { "max_rank": { "max": { "field": "rank" } } }
            }
        }),
    )
    .await;
    assert_eq!(put_status, StatusCode::OK, "body: {put_body}");

    let (status, body) = post(&app, "/_transform/t1/_start", json!({})).await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the transform wrote a summary of a source set a refused script had \
         truncated, and acknowledged it: {body}"
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

/// The counterpart, restated for the new surfaces: a script our interpreter
/// does not support is NOT a resource limit and must keep degrading quietly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsupported_script_syntax_still_degrades_quietly_on_the_new_surfaces() {
    let (app, _dir) = seeded_app().await;
    let unsupported = json!({
        "function_score": {
            "query": { "match_all": {} },
            "functions": [
                { "script_score": { "script": { "source": "someUnsupportedThing(1,2,3)" } } }
            ]
        }
    });

    let (status, body) = post(
        &app,
        "/scripts/_rank_eval",
        json!({
            "requests": [{
                "id": "q1",
                "request": { "query": unsupported.clone() },
                "ratings": [{ "_index": "scripts", "_id": "1", "rating": 3 }]
            }]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an out-of-subset script must not turn _rank_eval into a 400: {body}"
    );
    assert!(body["metric_score"].is_number(), "body: {body}");

    let (status, body) = post(&app, "/scripts/_explain/1", json!({ "query": unsupported })).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an out-of-subset script must not turn _explain into a 400: {body}"
    );
    assert_eq!(body["matched"], true, "body: {body}");
}
