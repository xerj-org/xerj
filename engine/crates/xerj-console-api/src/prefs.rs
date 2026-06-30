//! Per-user UI preferences — `GET /prefs` and `PUT /prefs`.
//!
//! Replaces every `localStorage["xerj.*"]` write the playground does
//! today: theme, default cluster, time range, mobile flag, search-box
//! contents.  We don't constrain the schema — the doc is whatever
//! key/value pairs the SPA asks us to round-trip — but `_id` is always
//! the user_id, so reads are by-id and idempotent.

use axum::{extract::State, response::Response, Json};
use serde_json::{json, Value};

use crate::auth::sessions::AuthSession;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::ok;
use crate::state::ConsoleState;
use crate::time::now_iso;

const FALLBACK_PREFS: &str = r#"{
  "theme": "dark",
  "time": "24H",
  "cluster": "LOCAL",
  "mobile": false
}"#;

pub async fn get(
    State(state): State<ConsoleState>,
    sess: AuthSession,
) -> ConsoleResult<Response> {
    let idx = state.engine.get_index(indices::PREFS)?;
    let body = json!({
        "query": { "ids": { "values": [&sess.user.id] } },
        "size": 1
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    let prefs = match r.hits.into_iter().next() {
        Some(h) => h.source,
        None => serde_json::from_str(FALLBACK_PREFS)
            .expect("fallback prefs are valid json"),
    };
    Ok(ok(prefs, None))
}

pub async fn put(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Json(mut body): Json<Value>,
) -> ConsoleResult<Response> {
    if !body.is_object() {
        return Err(ConsoleApiError::BadRequest(
            "prefs body must be a JSON object".into(),
        ));
    }
    // Strip any client-set "updated_at" so the server is authoritative.
    body.as_object_mut().unwrap().insert(
        "updated_at".into(),
        Value::String(now_iso()),
    );

    let idx = state.engine.get_index(indices::PREFS)?;
    let _ = idx.delete_document(&sess.user.id).await;
    idx.create_document(sess.user.id.clone(), body.clone()).await?;
    Ok(ok(body, None))
}
