//! The ML datafeed is not a way around the per-index boundary (issue #79).
//!
//! This is the exploit that got past the first two-layer cut, run end to end
//! against a real auth-enforced router, and it is deliberately written as the
//! attack rather than as a unit test of either half of the fix.
//!
//! ## What it was
//!
//! The engine-side guard is a `tokio` task-local, so it lives on the task that
//! is handling the request. `POST /_ml/datafeeds/{id}/_start` did two things:
//! one synchronous scoring pass, on the request's own task, and then a
//! detached `tokio::spawn` that re-scored every `frequency` seconds. The
//! synchronous pass was inside the guard and was correctly denied — which is
//! what made this so quiet. The caller saw an empty result set and could
//! reasonably conclude it had been stopped. A couple of seconds later the
//! background tick ran with no guard installed at all, read the brain, and
//! appended the field values it found to the job's results, where the same
//! caller read them straight back out of
//! `GET /_ml/anomaly_detectors/{job}/results/records`.
//!
//! The credential used is an **unscoped** key: minted with no
//! `role_descriptors`, the shape every key had before per-brain authorization
//! existed. The design guarantees it holds *nothing* on the reserved
//! namespace, and `a_direct_read_of_the_brain_is_still_denied` re-proves that
//! for this exact key inside every test below, so a passing run cannot be an
//! artifact of a credential that was allowed all along.
//!
//! ## What is asserted
//!
//! Both halves, separately, because neither alone closes it:
//!
//! - **config time** — an ML job or datafeed naming an index the caller cannot
//!   read is refused, instead of being accepted with a 200;
//! - **run time** — even when the configuration was planted by the superuser,
//!   so there was nothing to refuse, the detached scorer runs under the
//!   *starting* principal's visibility rule and finds nothing.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use xerj_api::{router::build_es_compat_router, state::AppState};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const ADMIN_KEY: &str = "admin-secret-key-for-ml-datafeed-test";
const BRAIN: &str = ".xerj-memory-bob-edges";

/// The value planted on bob's TOPSECRET edge. Distinctive enough that a
/// substring search over the whole response body is a sound leak test.
const SECRET: i64 = 987_654_321;

/// An auth-enabled node over a fresh data directory. The `TempDir` must be
/// held for the whole test — the Engine keeps the directory open.
fn auth_enabled_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.auth.enabled = true;
    config.auth.admin_api_key = ADMIN_KEY.to_string();
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    (AppState::new(config, engine, metrics), dir)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
}

/// Mint a key with **no** `role_descriptors` — `Principal::Unscoped`.
async fn mint_unscoped(app: &axum::Router) -> String {
    let (status, resp) = send(
        app,
        "POST",
        "/_security/api_key",
        &admin(),
        r#"{"name":"leak-key"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "minting: {resp}");
    format!("ApiKey {}", resp["encoded"].as_str().expect("encoded key"))
}

/// Plant bob's brain as the superuser: four ordinary edges, then the TOPSECRET
/// one. The detector needs four normal buckets of baseline before it will
/// score anything, so the fifth bucket is the anomaly and the record's
/// `actual` is the exact planted `valid_at`.
async fn plant_the_brain(app: &axum::Router) {
    for i in 1..=4 {
        let (status, body) = send(
            app,
            "PUT",
            &format!("/{BRAIN}/_doc/e{i}"),
            &admin(),
            &format!(
                r#"{{"from":"bob","to":"n{i}","type":"knows",
                     "created_at":"2026-01-01T00:00:0{i}Z","valid_at":100{i}}}"#
            ),
        )
        .await;
        assert!(status.is_success(), "seeding e{i}: {body}");
    }
    let (status, body) = send(
        app,
        "PUT",
        &format!("/{BRAIN}/_doc/secret?refresh=true"),
        &admin(),
        &format!(
            r#"{{"from":"bob","to":"TOPSECRET","type":"knows",
                 "created_at":"2026-01-01T00:00:09Z","valid_at":{SECRET}}}"#
        ),
    )
    .await;
    assert!(status.is_success(), "seeding the secret edge: {body}");
}

/// The detector and datafeed configs exactly as the reported exploit spells
/// them.
const DETECTOR_BODY: &str = r#"{"source_index":".xerj-memory-bob-edges","time_field":"created_at",
                                "function":"max","field":"valid_at","bucket_span":"1s"}"#;
const FEED_BODY: &str = r#"{"job_id":"leak2","indices":[".xerj-memory-bob-edges"],
                            "frequency":"1s"}"#;

/// The control every assertion in this file rests on: the key really is
/// denied the brain by the front door, so anything it learns by another route
/// is a bypass and not a grant.
async fn a_direct_read_of_the_brain_is_still_denied(app: &axum::Router, key: &str) {
    let (status, body) = send(
        app,
        "POST",
        &format!("/{BRAIN}/_search"),
        key,
        r#"{"query":{"match_all":{}}}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the credential under test must be denied the brain directly: {body}"
    );
}

/// Read the job's anomaly records and assert the planted value is not in them,
/// whatever shape the response takes. Returns the body so a failure can print
/// what actually came back.
async fn records_must_stay_clean(app: &axum::Router, key: &str, when: &str) -> Value {
    let (status, body) = send(
        app,
        "GET",
        "/_ml/anomaly_detectors/leak2/results/records",
        key,
        "",
    )
    .await;
    let text = body.to_string();
    assert!(
        !text.contains(&SECRET.to_string()),
        "{when}: the reserved field value {SECRET} reached a credential that \
         cannot read {BRAIN} — HTTP {status}, body {text}"
    );
    assert_eq!(
        body["count"].as_i64().unwrap_or(0),
        0,
        "{when}: the datafeed produced records off a brain it may not read: {text}"
    );
    body
}

/// Wait out several datafeed ticks. The feed's `frequency` is 1s and the tick
/// that leaked fired on the second one, so four seconds is three chances to
/// fail — real elapsed time, because the scorer runs on a real interval.
async fn let_the_background_scorer_tick() {
    tokio::time::sleep(std::time::Duration::from_millis(4_200)).await;
}

// ─────────────────────────────────────────────────────────────────────────────

/// FINDING A, both halves, as the reported exploit runs it.
///
/// Steps 1-3 are the config; step 4 is the `_start`; step 5 is the tell (empty
/// immediately, because the synchronous pass is inside the guard); step 6 is
/// where it used to leak.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reported_ml_datafeed_exploit_end_to_end() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    plant_the_brain(&app).await;
    let attacker = mint_unscoped(&app).await;
    a_direct_read_of_the_brain_is_still_denied(&app, &attacker).await;

    // 1. The superuser can score it — so the secret really is reachable by
    //    this configuration, and an empty result later is the boundary
    //    working rather than the fixture being wrong.
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/anomaly_detectors/control",
        &admin(),
        DETECTOR_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin detector: {body}");
    let (status, scored) = send(
        &app,
        "POST",
        "/_ml/anomaly_detectors/control/_score",
        &admin(),
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin _score: {scored}");
    assert!(
        scored.to_string().contains(&SECRET.to_string()),
        "fixture check: the superuser must be able to score the planted value, \
         otherwise this test proves nothing. Got: {scored}"
    );

    // 2. PUT /_ml/anomaly_detectors/leak2 — refused at config time now.
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/anomaly_detectors/leak2",
        &attacker,
        DETECTOR_BODY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an ML job may not name an index its creator cannot read: {body}"
    );

    // 3. PUT /_ml/datafeeds/leak2-feed — likewise.
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/datafeeds/leak2-feed",
        &attacker,
        FEED_BODY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a datafeed may not name an index its creator cannot read: {body}"
    );

    // 4. …so there is nothing to start, and nothing to read.
    let (status, _) = send(
        &app,
        "POST",
        "/_ml/datafeeds/leak2-feed/_start",
        &attacker,
        "{}",
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a datafeed that was never created must not start"
    );

    // 5 & 6. Empty immediately, and still empty after the ticks that used to
    // carry the value out.
    records_must_stay_clean(&app, &attacker, "immediately after _start").await;
    let_the_background_scorer_tick().await;
    records_must_stay_clean(&app, &attacker, "after the background ticks").await;
}

/// FINDING A, run-time half **in isolation**.
///
/// The config-time check cannot help here: the superuser plants the job and
/// the datafeed, so both already exist and name the brain legitimately. All
/// the attacker does is press `_start`. That is enough, because `_start` is
/// what spawns the detached scorer — and the scorer must therefore run under
/// the rule of whoever started it.
///
/// This is the assertion that fails if only the `BodyShape` half is applied.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_superuser_planted_datafeed_started_by_an_unscoped_key_reads_nothing() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    plant_the_brain(&app).await;
    let attacker = mint_unscoped(&app).await;
    a_direct_read_of_the_brain_is_still_denied(&app, &attacker).await;

    // The superuser configures everything. Nothing here is refusable.
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/anomaly_detectors/leak2",
        &admin(),
        DETECTOR_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin detector: {body}");
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/datafeeds/leak2-feed",
        &admin(),
        FEED_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin datafeed: {body}");

    // The attacker only presses start.
    let (status, body) = send(
        &app,
        "POST",
        "/_ml/datafeeds/leak2-feed/_start",
        &attacker,
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_start under the attacker: {body}");

    // The tell: empty right away, because the synchronous pass ran inside the
    // request's guarded scope. This was true before the fix too.
    records_must_stay_clean(&app, &attacker, "immediately after _start").await;

    // The bug: the detached tick. Before the fix this came back with
    // `actual: 987654321.0`.
    let_the_background_scorer_tick().await;
    records_must_stay_clean(&app, &attacker, "after the background ticks").await;

    // And the brain is still not readable by the front door afterwards, so
    // the empty results are the boundary and not a broken datafeed.
    a_direct_read_of_the_brain_is_still_denied(&app, &attacker).await;
}

/// The other side of the same change: a datafeed the **superuser** starts must
/// keep working exactly as before. A superuser has no visibility rule
/// installed, so `None` is what gets carried across the spawn, and the scorer
/// stays unrestricted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_superuser_started_datafeed_still_scores() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    plant_the_brain(&app).await;

    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/anomaly_detectors/leak2",
        &admin(),
        DETECTOR_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detector: {body}");
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/datafeeds/leak2-feed",
        &admin(),
        FEED_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "datafeed: {body}");
    let (status, body) = send(
        &app,
        "POST",
        "/_ml/datafeeds/leak2-feed/_start",
        &admin(),
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_start: {body}");

    // The synchronous start pass alone should already have produced it.
    let (status, records) = send(
        &app,
        "GET",
        "/_ml/anomaly_detectors/leak2/results/records",
        &admin(),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        records.to_string().contains(&SECRET.to_string()),
        "the superuser's own datafeed must still score its index: {records}"
    );

    // …and the background ticks must not break it either.
    let_the_background_scorer_tick().await;
    let (_, records) = send(
        &app,
        "GET",
        "/_ml/anomaly_detectors/leak2/results/records",
        &admin(),
        "",
    )
    .await;
    assert!(
        records.to_string().contains(&SECRET.to_string()),
        "the superuser's datafeed stopped scoring after a background tick: {records}"
    );
}

/// A scoped key configuring ML over an index it **holds** must not be caught
/// by the new config-time check — the point is to bound the datafeed to its
/// creator, not to refuse everyone.
///
/// It also pins what this change deliberately did **not** touch. The ML
/// *verbs* (`_score`, `_start`, `_stop`) name no index of their own, so they
/// fall to the pre-existing cluster rule that a scoped key must name what it
/// mutates, and are refused — as they were before this change. That is a
/// usability gap in the scoped-key surface, not a hole: it fails closed, and
/// widening it is a change to the cluster arm rather than to anything issue
/// #79 is about. Pinned here so the difference is visible and deliberate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_key_still_configures_ml_over_its_own_index() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    plant_the_brain(&app).await;

    // Same fixture shape, under a name the scoped key will be granted.
    for (i, (ts, val)) in [
        ("2026-01-01T00:00:01Z", 1001),
        ("2026-01-01T00:00:02Z", 1002),
        ("2026-01-01T00:00:03Z", 1003),
        ("2026-01-01T00:00:04Z", 1004),
        ("2026-01-01T00:00:09Z", SECRET),
    ]
    .iter()
    .enumerate()
    {
        let refresh = if i == 4 { "?refresh=true" } else { "" };
        let (status, body) = send(
            &app,
            "PUT",
            &format!("/logs-2026/_doc/d{i}{refresh}"),
            &admin(),
            &format!(r#"{{"created_at":"{ts}","valid_at":{val}}}"#),
        )
        .await;
        assert!(status.is_success(), "seeding logs-2026 d{i}: {body}");
    }

    let (status, minted) = send(
        &app,
        "POST",
        "/_security/api_key",
        &admin(),
        r#"{"name":"logs-agent","role_descriptors":{"logs":{"indices":[
             {"names":["logs-2026"],"privileges":["read","write"]}]}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "minting a scoped key: {minted}");
    let agent = format!("ApiKey {}", minted["encoded"].as_str().expect("encoded"));

    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/anomaly_detectors/mine",
        &agent,
        r#"{"source_index":"logs-2026","time_field":"created_at",
            "function":"max","field":"valid_at","bucket_span":"1s"}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a scoped key must still configure ML over an index it holds: {body}"
    );
    // …but it still cannot point one at someone else's brain.
    let (status, body) = send(
        &app,
        "PUT",
        "/_ml/anomaly_detectors/theirs",
        &agent,
        DETECTOR_BODY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "cross-brain ML config: {body}"
    );

    // The verbs stay where the pre-existing cluster rule leaves them: refused
    // for a scoped key, because they name nothing. Unchanged by this commit.
    for (method, uri) in [
        ("POST", "/_ml/anomaly_detectors/mine/_score"),
        ("POST", "/_ml/datafeeds/mine-feed/_start"),
    ] {
        let (status, body) = send(&app, method, uri, &agent, "{}").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} is expected to stay refused for a scoped key \
             (pre-existing cluster-mutation rule, fails closed): {body}"
        );
    }

    // The superuser can still drive the very same job, so the detector the
    // scoped key configured is real and usable — the gap is the verb, not the
    // config.
    let (status, scored) = send(
        &app,
        "POST",
        "/_ml/anomaly_detectors/mine/_score",
        &admin(),
        "{}",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin _score of the scoped job: {scored}"
    );
    assert!(
        scored.to_string().contains(&SECRET.to_string()),
        "the job the scoped key configured must score its own index: {scored}"
    );
}
