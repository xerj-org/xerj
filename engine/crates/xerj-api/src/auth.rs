//! API key authentication middleware.
//!
//! Checks the `Authorization` header for either:
//! - `Authorization: ApiKey <key>`
//! - `Authorization: Bearer <key>`
//!
//! When `config.auth.enabled` is `false` (or `--insecure` mode was set by
//! clearing the key), the check is skipped entirely.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::error::{EsErrorBody, EsErrorResponse, EsRootCause};
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────

/// Axum middleware that enforces API key authentication.
///
/// Call via `middleware::from_fn_with_state(state, auth_middleware)` in the
/// router builders.
pub async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let cfg = &state.config.auth;

    // Skip auth when disabled or no admin key is configured.
    if !cfg.enabled || cfg.admin_api_key.is_empty() {
        return next.run(req).await;
    }
    // Xerj Console lives in a peer router (mounted by `xerj-server` via
    // `Router::merge`) and runs its own session-cookie auth, so this
    // middleware never sees `/_xerj-console/*` requests — no path-prefix
    // bypass needed.

    // Extract the Authorization header.
    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let provided_key = match auth_header {
        Some(h) => extract_key(h),
        None => None,
    };

    match provided_key {
        // Fast path: the configured admin/superuser key.
        Some(key) if key == cfg.admin_api_key => next.run(req).await,
        // A key minted by `POST /_security/api_key`, presented as
        // `Authorization: ApiKey <base64(id:api_key)>`, that is valid, not
        // expired, and not invalidated.
        Some(key) if authenticate_api_key(&state, key) => next.run(req).await,
        _ => unauthorized_response(),
    }
}

/// Re-authenticate a created API key credential.
///
/// The presented value is `base64("id:api_key")`. Decode it, split on the
/// first `':'`, look the id up in the engine's key store, and constant-time
/// compare the secret. All keys authenticate as the single superuser
/// (role_descriptors are accepted at creation but not enforced).
fn authenticate_api_key(state: &AppState, presented: &str) -> bool {
    let decoded = match crate::es_compat::base64_decode(presented) {
        Some(d) => d,
        None => return false,
    };
    let decoded = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let (id, secret) = match decoded.split_once(':') {
        Some(parts) => parts,
        None => return false,
    };
    let record = match state.engine.api_keys.get(id) {
        Some(r) => r,
        None => return false,
    };
    if record.invalidated {
        return false;
    }
    if let Some(exp) = record.expiration_ms {
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        if now_ms >= exp {
            return false;
        }
    }
    constant_time_eq(record.secret.as_bytes(), secret.as_bytes())
}

/// Length-independent-only constant-time byte comparison. Avoids leaking the
/// secret via early-exit timing once lengths match.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the raw key from an `Authorization` header value.
///
/// Accepts both `ApiKey <key>` and `Bearer <key>` schemes (case-insensitive
/// scheme prefix).
fn extract_key(header: &str) -> Option<&str> {
    let lower = header.to_ascii_lowercase();
    if lower.starts_with("apikey ") {
        Some(header["ApiKey ".len()..].trim())
    } else if lower.starts_with("bearer ") {
        Some(header["Bearer ".len()..].trim())
    } else {
        None
    }
}

/// Build an ES-compatible 401 Unauthorized response.
fn unauthorized_response() -> Response {
    let reason = "missing or invalid API key in Authorization header".to_string();
    let error_type = "security_exception".to_string();

    let body = EsErrorResponse {
        error: EsErrorBody {
            root_cause: vec![EsRootCause {
                error_type: error_type.clone(),
                reason: reason.clone(),
                index: None,
            }],
            error_type,
            reason,
            request_id: None,
        },
        status: 401,
    };

    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::Request,
        middleware,
        routing::{get, post},
        Router,
    };
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt; // oneshot
    use xerj_common::{config::Config, metrics::Metrics};
    use xerj_engine::Engine;

    fn test_state(admin_key: &str) -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let mut config = Config::default();
        config.server.data_dir = path.to_str().unwrap().to_string();
        config.auth.enabled = true;
        config.auth.admin_api_key = admin_key.to_string();
        let metrics = Metrics::new().expect("metrics");
        let engine = Engine::new(config.clone()).expect("engine");
        AppState::new(config, engine, metrics)
    }

    fn app(state: AppState) -> Router {
        Router::new()
            .route(
                "/_security/api_key",
                post(crate::es_compat::security_create_api_key),
            )
            .route(
                "/_security/_authenticate",
                get(crate::es_compat::security_authenticate),
            )
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state)
    }

    async fn send(
        app: &Router,
        method: &str,
        uri: &str,
        auth: Option<&str>,
        body: &str,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        let req = b.body(Body::from(body.to_string())).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn created_api_key_is_reauthenticatable() {
        let admin = "admin-secret-key";
        let state = test_state(admin);
        let app = app(state.clone());
        let admin_hdr = format!("ApiKey {admin}");

        // Mint a key (authenticated as admin).
        let (status, body) = send(
            &app,
            "POST",
            "/_security/api_key",
            Some(&admin_hdr),
            r#"{"name":"kibana"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create returned {status}");
        assert_eq!(body["name"], "kibana");
        let encoded = body["encoded"].as_str().expect("encoded").to_string();
        let id = body["id"].as_str().expect("id").to_string();

        // The minted key re-authenticates.
        let key_hdr = format!("ApiKey {encoded}");
        let (status, _) = send(&app, "GET", "/_security/_authenticate", Some(&key_hdr), "").await;
        assert_eq!(status, StatusCode::OK, "valid key should authenticate");

        // The admin key still works.
        let (status, _) = send(
            &app,
            "GET",
            "/_security/_authenticate",
            Some(&admin_hdr),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "admin key should authenticate");

        // A bogus key is rejected.
        let (status, _) = send(
            &app,
            "GET",
            "/_security/_authenticate",
            Some("ApiKey bm90LWEta2V5"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "bogus key should 401");

        // No auth header at all is rejected.
        let (status, _) = send(&app, "GET", "/_security/_authenticate", None, "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "missing auth should 401");

        // Expired key is rejected.
        state
            .engine
            .api_keys
            .get_mut(&id)
            .expect("record")
            .expiration_ms = Some(1); // epoch ms 1 = long past
        let (status, _) = send(&app, "GET", "/_security/_authenticate", Some(&key_hdr), "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "expired key should 401");

        // Invalidated key is rejected (even after clearing expiry).
        {
            let mut rec = state.engine.api_keys.get_mut(&id).expect("record");
            rec.expiration_ms = None;
            rec.invalidated = true;
        }
        let (status, _) = send(&app, "GET", "/_security/_authenticate", Some(&key_hdr), "").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "invalidated key should 401"
        );
    }
}
