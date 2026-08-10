//! OpenSearch Index State Management (ISM) — the native REST surface over
//! `xerj_engine::lifecycle`'s execution engine. See that module's doc
//! comment for why ISM's state machine (not ES ILM's fixed phases) is the
//! internal model both `_plugins/_ism/*` (here) and `_ilm/*`
//! (`es_compat::put_ilm_policy` and friends) are built on.
//!
//! Endpoints:
//!   PUT/GET/DELETE /_plugins/_ism/policies/{policy_id}
//!   GET            /_plugins/_ism/policies            (list/search — added
//!                  after live OSD UI verification showed the "State
//!                  management policies" table 500ing without it)
//!   POST           /_plugins/_ism/add/{index}
//!   POST           /_plugins/_ism/remove/{index}
//!   POST           /_plugins/_ism/change_policy/{index}
//!   POST           /_plugins/_ism/retry/{index}
//!   GET            /_plugins/_ism/explain/{index}
//!   GET            /_plugins/_ism/explain             (list-all — same
//!                  reason, for the "Policy managed indexes" table)
//!
//! `remove`/`change_policy`/`retry` were added after a systematic pass over
//! OSD's Index Management UI actions (the "Remove policy" / "Change policy"
//! / "Retry policy" buttons on the Managed Indexes table) found they had no
//! backend at all — same discovery method as the list-form gaps above.

use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use xerj_engine::lifecycle::{self, LifecyclePolicy, ManagedIndexState};

use crate::{error::ApiError, extract::OptionalJson, state::AppState};

/// The `{"failures": bool, "updated_indices": N, "failed_indices": [...]}`
/// shape real ISM's bulk-style index-targeting endpoints (`add`, `remove`,
/// `change_policy`, `retry`) all share. A single-index failure here is a
/// normal, expected outcome (e.g. "not currently managed") — not an error
/// worth a non-200 status, matching how OSD's own client reads the body's
/// `failures` flag rather than the HTTP status to decide success.
fn not_managed_failure(index: &str, reason: &str) -> Value {
    json!({
        "failures": true,
        "updated_indices": 0,
        "failed_indices": [{
            "index_name": index,
            "index_uuid": "_na_",
            "reason": reason,
        }],
    })
}

fn single_index_success() -> Value {
    json!({ "failures": false, "updated_indices": 1, "failed_indices": [] })
}

// ─────────────────────────────────────────────────────────────────────────────
// PUT/GET/DELETE /_plugins/_ism/policies/{policy_id}
// ─────────────────────────────────────────────────────────────────────────────

/// Real ISM wraps the policy body as `{"policy": {...}}`; accept that shape
/// but also the bare policy object, since it's an easy, harmless mistake to
/// make by hand (and this parses unambiguously either way).
fn unwrap_policy_body(body: &Value) -> &Value {
    body.get("policy").unwrap_or(body)
}

pub async fn put_ism_policy(
    State(state): State<AppState>,
    Path(policy_id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let policy_value = unwrap_policy_body(&body).clone();
    let policy: LifecyclePolicy = match serde_json::from_value(policy_value) {
        Ok(p) => p,
        Err(e) => {
            let err =
                xerj_common::XerjError::invalid_query(format!("invalid ISM policy body: {e}"));
            return ApiError::new(err).into_response();
        }
    };
    if let Err(reason) = policy.validate() {
        let err = xerj_common::XerjError::invalid_query(format!("invalid ISM policy: {reason}"));
        return ApiError::new(err).into_response();
    }
    state
        .engine
        .put_ism_policy(policy_id.clone(), policy.clone());
    Json(json!({
        "_id": policy_id,
        "_version": 1,
        "_seq_no": 0,
        "_primary_term": 1,
        "policy": policy,
    }))
    .into_response()
}

pub async fn get_ism_policy(
    State(state): State<AppState>,
    Path(policy_id): Path<String>,
) -> impl IntoResponse {
    match state.engine.ism_policies.get(&policy_id) {
        Some(policy) => Json(json!({
            "_id": policy_id,
            "_version": 1,
            "_seq_no": 0,
            "_primary_term": 1,
            "policy": policy.value(),
        }))
        .into_response(),
        None => {
            let e =
                xerj_common::XerjError::index_not_found(format!("policy [{policy_id}] not found"));
            ApiError::new(e).into_response()
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    #[serde(default)]
    pub from: usize,
    #[serde(default = "default_size")]
    pub size: usize,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub sort_direction: Option<String>,
}
fn default_size() -> usize {
    20
}

/// `GET /_plugins/_ism/policies` — the list/search form OSD's own Index
/// Management UI uses to populate the "State management policies" table
/// (its backend proxies `/api/ism/policies` straight through to this
/// endpoint). Found missing while verifying the real UI: only the
/// single-policy `GET .../policies/{id}` was implemented, so the list page
/// rendered "no existing policies" and a raw Internal Server Error toast
/// even immediately after a successful create.
pub async fn list_ism_policies(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let mut all: Vec<(String, LifecyclePolicy)> = state
        .engine
        .ism_policies
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .filter(|(id, _)| params.search.is_empty() || id.contains(&params.search))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    if params.sort_direction.as_deref() == Some("desc") {
        all.reverse();
    }
    let total = all.len();
    let page: Vec<Value> = all
        .into_iter()
        .skip(params.from)
        .take(params.size.max(1))
        .map(|(id, policy)| {
            json!({
                "_id": id,
                "_version": 1,
                "_seq_no": 0,
                "_primary_term": 1,
                "policy": policy,
            })
        })
        .collect();
    Json(json!({ "totalPolicies": total, "policies": page })).into_response()
}

pub async fn delete_ism_policy(
    State(state): State<AppState>,
    Path(policy_id): Path<String>,
) -> impl IntoResponse {
    if state.engine.remove_ism_policy(&policy_id) {
        Json(json!({ "_id": policy_id, "result": "deleted" })).into_response()
    } else {
        let e = xerj_common::XerjError::index_not_found(format!("policy [{policy_id}] not found"));
        ApiError::new(e).into_response()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_plugins/_ism/add/{index}
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddPolicyBody {
    pub policy_id: String,
}

pub async fn add_ism_policy(
    State(state): State<AppState>,
    Path(index): Path<String>,
    Json(body): Json<AddPolicyBody>,
) -> impl IntoResponse {
    attach_policy(&state, &index, &body.policy_id).await
}

/// Shared attach implementation: validates the target index and policy both
/// exist, then creates (or resets) that index's managed-index execution
/// cursor at the policy's `default_state`. Used by the native ISM endpoint
/// above and by the ES-shape `index.lifecycle.name` auto-attach in
/// `es_compat` (a single index↔policy association model, per the design —
/// not two).
pub async fn attach_policy(state: &AppState, index: &str, policy_id: &str) -> Response {
    if state.engine.get_index(index).is_err() {
        let e = xerj_common::XerjError::index_not_found(index);
        return ApiError::new(e).into_response();
    }
    let Some(policy) = state
        .engine
        .ism_policies
        .get(policy_id)
        .map(|e| e.value().clone())
    else {
        let e = xerj_common::XerjError::index_not_found(format!("policy [{policy_id}] not found"));
        return ApiError::new(e).into_response();
    };
    let managed = ManagedIndexState::new(
        policy_id.to_string(),
        policy.default_state.clone(),
        lifecycle::now_ms(),
    );
    state
        .engine
        .managed_indices
        .insert(index.to_string(), managed);
    state.engine.persist_managed_indices();

    Json(json!({
        "failures": false,
        "updated_indices": 1,
        "failed_indices": [],
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_plugins/_ism/remove/{index}
// ─────────────────────────────────────────────────────────────────────────────

pub async fn remove_ism_policy(
    State(state): State<AppState>,
    Path(index): Path<String>,
) -> impl IntoResponse {
    if state.engine.managed_indices.remove(&index).is_some() {
        state.engine.persist_managed_indices();
        Json(single_index_success()).into_response()
    } else {
        Json(not_managed_failure(
            &index,
            "This index is not being managed.",
        ))
        .into_response()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_plugins/_ism/change_policy/{index}
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IncludeState {
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct ChangePolicyBody {
    pub policy_id: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub include: Vec<IncludeState>,
}

/// Real ISM's `change_policy` doesn't apply immediately mid-step; xerj has
/// no concept of a mid-step (states execute all their actions, then
/// transition), so this applies as soon as the request lands. `include` is
/// still honored as the safety check it's documented as: if given, the
/// index's *current* state name must be one of the listed states, or the
/// change is refused — this is what stops a well-intentioned bulk
/// `change_policy` call from silently reassigning indices that already
/// moved on to a state the caller didn't expect.
pub async fn change_ism_policy(
    State(state): State<AppState>,
    Path(index): Path<String>,
    Json(body): Json<ChangePolicyBody>,
) -> impl IntoResponse {
    let Some(mut managed) = state
        .engine
        .managed_indices
        .get(&index)
        .map(|e| e.value().clone())
    else {
        return Json(not_managed_failure(
            &index,
            "This index is not being managed.",
        ))
        .into_response();
    };

    if !body.include.is_empty()
        && !body
            .include
            .iter()
            .any(|i| i.state == managed.current_state)
    {
        return Json(not_managed_failure(
            &index,
            &format!(
                "Index is currently in state '{}', which does not match the provided 'include' safety condition.",
                managed.current_state
            ),
        ))
        .into_response();
    }

    let Some(new_policy) = state
        .engine
        .ism_policies
        .get(&body.policy_id)
        .map(|e| e.value().clone())
    else {
        let e = xerj_common::XerjError::index_not_found(format!(
            "policy [{}] not found",
            body.policy_id
        ));
        return ApiError::new(e).into_response();
    };

    let target_state = match &body.state {
        Some(s) if new_policy.state(s).is_some() => s.clone(),
        Some(s) => {
            let e = xerj_common::XerjError::invalid_query(format!(
                "state '{s}' does not exist in policy '{}'",
                body.policy_id
            ));
            return ApiError::new(e).into_response();
        }
        None if new_policy.state(&managed.current_state).is_some() => managed.current_state.clone(),
        None => new_policy.default_state.clone(),
    };

    managed.policy_id = body.policy_id.clone();
    managed.current_state = target_state;
    managed.state_entered_at_ms = lifecycle::now_ms();
    managed.next_action_index = 0;
    managed.failed = false;
    managed.info_message = format!("policy changed to '{}'", body.policy_id);
    managed.last_updated_ms = lifecycle::now_ms();
    state.engine.managed_indices.insert(index, managed);
    state.engine.persist_managed_indices();

    Json(single_index_success()).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_plugins/_ism/retry/{index}
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct RetryBody {
    #[serde(default)]
    pub state: Option<String>,
}

/// xerj's engine already retries a failing action on every tick without
/// operator intervention (see `lifecycle::ManagedIndexState::failed`'s doc
/// comment — `failed` is visibility, not a halt), so this doesn't need to
/// "unstick" anything internally. It still needs to exist: OSD's Managed
/// Indexes table renders a "Retry policy" button whenever `explain` reports
/// `failed: true`, and that button 404ing is a real, visible break even
/// though the underlying index was never actually stuck. Real ISM also
/// rejects retrying an index that isn't currently failed, so this mirrors
/// that rather than silently succeeding as a no-op.
pub async fn retry_ism_index(
    State(state): State<AppState>,
    Path(index): Path<String>,
    body: OptionalJson<RetryBody>,
) -> impl IntoResponse {
    let body = body.0.unwrap_or_default();
    let Some(mut managed) = state
        .engine
        .managed_indices
        .get(&index)
        .map(|e| e.value().clone())
    else {
        return Json(not_managed_failure(
            &index,
            "This index is not being managed.",
        ))
        .into_response();
    };

    if !managed.failed {
        return Json(not_managed_failure(
            &index,
            "This index is not in a failed state.",
        ))
        .into_response();
    }

    if let Some(s) = &body.state {
        let Some(policy) = state
            .engine
            .ism_policies
            .get(&managed.policy_id)
            .map(|e| e.value().clone())
        else {
            let e = xerj_common::XerjError::index_not_found(format!(
                "policy [{}] not found",
                managed.policy_id
            ));
            return ApiError::new(e).into_response();
        };
        if policy.state(s).is_none() {
            let e = xerj_common::XerjError::invalid_query(format!(
                "state '{s}' does not exist in policy '{}'",
                managed.policy_id
            ));
            return ApiError::new(e).into_response();
        }
        managed.current_state = s.clone();
        managed.state_entered_at_ms = lifecycle::now_ms();
    }
    managed.next_action_index = 0;
    managed.failed = false;
    managed.info_message = "retrying".to_string();
    managed.last_updated_ms = lifecycle::now_ms();
    state.engine.managed_indices.insert(index, managed);
    state.engine.persist_managed_indices();

    Json(single_index_success()).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /_plugins/_ism/explain/{index}
// ─────────────────────────────────────────────────────────────────────────────

/// `GET /_plugins/_ism/explain` — the list-all form OSD's "Policy managed
/// indexes" table uses (its backend's `/api/ism/managedIndices` proxies
/// straight through to this). Same gap as `list_ism_policies`: only the
/// single-index `.../explain/{index}` was implemented at first.
pub async fn list_managed_indices(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let mut all: Vec<(String, ManagedIndexState)> = state
        .engine
        .managed_indices
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .filter(|(name, _)| params.search.is_empty() || name.contains(&params.search))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    if params.sort_direction.as_deref() == Some("desc") {
        all.reverse();
    }
    let total = all.len();
    let mut body = serde_json::Map::new();
    for (name, managed) in all.into_iter().skip(params.from).take(params.size.max(1)) {
        body.insert(name.clone(), lifecycle::explain_json(&name, &managed));
    }
    body.insert("total_managed_indices".to_string(), json!(total));
    Json(Value::Object(body)).into_response()
}

pub async fn explain_ism_index(
    State(state): State<AppState>,
    Path(index): Path<String>,
) -> impl IntoResponse {
    match state.engine.managed_indices.get(&index) {
        Some(managed) => {
            let mut body = serde_json::Map::new();
            body.insert(
                index.clone(),
                lifecycle::explain_json(&index, managed.value()),
            );
            body.insert("total_managed_indices".to_string(), json!(1));
            Json(Value::Object(body)).into_response()
        }
        None => {
            // Real ISM answers 200 with a "not managed" note rather than
            // 404 — `explain` on an unmanaged (or nonexistent) index is a
            // normal, expected call from Dashboards' Managed Indexes view.
            Json(json!({
                index.clone(): { "index": index, "enabled": false },
                "total_managed_indices": 0,
            }))
            .into_response()
        }
    }
}

#[cfg(test)]
mod managed_index_action_tests {
    use super::*;
    use axum::{body::Body, http::Request, http::StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.to_string_lossy().into_owned();
        config.storage.wal_sync = xerj_common::config::WalSync::Async;
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        AppState::new(config, engine, metrics)
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        let body = match body {
            Some(v) => {
                builder = builder.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    fn simple_policy(default_state: &str) -> Value {
        json!({
            "policy": {
                "default_state": default_state,
                "states": [
                    {"name": default_state, "actions": [], "transitions": []}
                ]
            }
        })
    }

    #[tokio::test]
    async fn remove_detaches_a_managed_index_and_is_idempotent_about_reporting_it() {
        let state = test_state();
        let app = crate::router::build_es_compat_router(state);

        let (status, _) = call(&app, "PUT", "/remove-test-idx", Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(
            &app,
            "PUT",
            "/_plugins/_ism/policies/remove-test-policy",
            Some(simple_policy("only")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(
            &app,
            "POST",
            "/_plugins/_ism/add/remove-test-idx",
            Some(json!({"policy_id": "remove-test-policy"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) =
            call(&app, "POST", "/_plugins/_ism/remove/remove-test-idx", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["failures"], false);
        assert_eq!(body["updated_indices"], 1);

        // Removing again reports the (now expected) "not managed" failure
        // rather than pretending success or 404ing.
        let (status, body) =
            call(&app, "POST", "/_plugins/_ism/remove/remove-test-idx", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["failures"], true);
        assert_eq!(body["failed_indices"][0]["index_name"], "remove-test-idx");
    }

    #[tokio::test]
    async fn change_policy_switches_policy_and_honors_include_safety_check() {
        let state = test_state();
        let app = crate::router::build_es_compat_router(state);

        call(&app, "PUT", "/change-policy-idx", Some(json!({}))).await;
        call(
            &app,
            "PUT",
            "/_plugins/_ism/policies/policy-a",
            Some(simple_policy("state-a")),
        )
        .await;
        call(
            &app,
            "PUT",
            "/_plugins/_ism/policies/policy-b",
            Some(simple_policy("state-b")),
        )
        .await;
        call(
            &app,
            "POST",
            "/_plugins/_ism/add/change-policy-idx",
            Some(json!({"policy_id": "policy-a"})),
        )
        .await;

        // Wrong `include` safety condition (index is in "state-a", not
        // "some-other-state") must refuse the change.
        let (status, body) = call(
            &app,
            "POST",
            "/_plugins/_ism/change_policy/change-policy-idx",
            Some(json!({
                "policy_id": "policy-b",
                "include": [{"state": "some-other-state"}]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["failures"], true);

        let (status, body) = call(
            &app,
            "POST",
            "/_plugins/_ism/change_policy/change-policy-idx",
            Some(json!({"policy_id": "policy-b"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["failures"], false);

        let (_, explain) = call(
            &app,
            "GET",
            "/_plugins/_ism/explain/change-policy-idx",
            None,
        )
        .await;
        assert_eq!(
            explain["change-policy-idx"]["policy_id"], "policy-b",
            "explain: {explain}"
        );
        assert_eq!(
            explain["change-policy-idx"]["state"]["name"], "state-b",
            "explain: {explain}"
        );
    }

    #[tokio::test]
    async fn retry_clears_failed_and_rejects_a_non_failed_index() {
        let state = test_state();
        // `AppState`'s `engine` field is an `Arc`, so this handle keeps
        // reaching the same `managed_indices` map the router (built from
        // `state` below, consumed by value) operates on — the same
        // technique `passage_scroll_tests` uses to seed engine state
        // directly instead of only driving it through HTTP.
        let state_handle = state.clone();
        let app = crate::router::build_es_compat_router(state);

        call(&app, "PUT", "/retry-idx", Some(json!({}))).await;
        call(
            &app,
            "PUT",
            "/_plugins/_ism/policies/retry-policy",
            Some(simple_policy("only")),
        )
        .await;
        call(
            &app,
            "POST",
            "/_plugins/_ism/add/retry-idx",
            Some(json!({"policy_id": "retry-policy"})),
        )
        .await;

        // Not failed yet — retry must refuse, matching real ISM.
        let (status, body) = call(&app, "POST", "/_plugins/_ism/retry/retry-idx", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["failures"], true);

        // Force the managed index into a failed state directly (this is
        // what `lifecycle::tick` does internally on a real action error —
        // simulated here rather than driving a real failing action).
        {
            let mut managed = state_handle
                .engine
                .managed_indices
                .get_mut("retry-idx")
                .expect("managed index exists");
            managed.failed = true;
            managed.info_message = "simulated failure for test".to_string();
        }

        let (status, body) = call(&app, "POST", "/_plugins/_ism/retry/retry-idx", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["failures"], false, "body: {body}");
        assert!(
            !state_handle
                .engine
                .managed_indices
                .get("retry-idx")
                .unwrap()
                .failed
        );
    }
}
