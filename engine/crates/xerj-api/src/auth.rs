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

/// Probe endpoints exempted from authentication.
///
/// `/health/live` and `/health/ready` are the conventional Kubernetes liveness
/// and readiness paths that container `HEALTHCHECK`s (see `engine/Dockerfile`)
/// and Helm chart probes hit **without** credentials. Gating them behind auth
/// makes every hardened deployment crashloop: the moment `auth.enabled = true`,
/// the kubelet's probe (and Docker's `curl -fsS .../health/ready`) receives a
/// 401, is marked unhealthy, and the pod is restarted forever. Both handlers
/// are dependency-free and expose no index data (they answer a bare
/// `"live"`/`"ready"` string), so leaving them open is safe.
pub const AUTH_EXEMPT_PATHS: [&str; 2] = ["/health/live", "/health/ready"];

/// Axum middleware that enforces API key authentication.
///
/// Call via `middleware::from_fn_with_state(state, auth_middleware)` in the
/// router builders.
pub async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // Kubernetes / Docker probes must stay reachable without credentials, or a
    // hardened (auth-enabled) deployment crashloops. See `AUTH_EXEMPT_PATHS`.
    if AUTH_EXEMPT_PATHS.contains(&req.uri().path()) {
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

    if is_authorized(&state, auth_header) {
        next.run(req).await
    } else {
        unauthorized_response()
    }
}

/// The shared authorization decision, enforced identically by the HTTP
/// `auth_middleware` and the gRPC auth interceptor (`xerj-server::grpc`) so both
/// API surfaces require the same credentials.
///
/// Returns `true` when the request may proceed:
/// - auth is disabled, or no admin key is configured (open mode — matches the
///   `--insecure` / first-run posture); or
/// - the `Authorization` value carries the configured admin/superuser key; or
/// - it carries a key minted by `POST /_security/api_key` (presented as
///   `ApiKey <base64(id:api_key)>`) that is valid, not expired, and not
///   invalidated.
///
/// `auth_header` is the raw `Authorization` header / `authorization` metadata
/// value (e.g. `"ApiKey abc"` or `"Bearer abc"`), or `None` when absent.
pub fn is_authorized(state: &AppState, auth_header: Option<&str>) -> bool {
    let cfg = &state.config.auth;

    // Skip auth when disabled or no admin key is configured.
    if !cfg.enabled || cfg.admin_api_key.is_empty() {
        return true;
    }

    match auth_header.and_then(extract_key) {
        // The configured admin/superuser key. Compared in constant time
        // (item 7): a plain `==` short-circuits on the first mismatching byte,
        // leaking the shared-secret prefix length via response timing. The
        // created-key path already used `constant_time_eq`; the admin key —
        // the single most valuable credential in the system — must not be
        // weaker. `constant_time_eq` still returns early on a length mismatch,
        // which only reveals the key *length*, matching the created-key path.
        Some(key) if constant_time_eq(key.as_bytes(), cfg.admin_api_key.as_bytes()) => true,
        // A key minted by `POST /_security/api_key`.
        Some(key) if authenticate_api_key(state, key) => true,
        _ => false,
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
                resource_type: None,
                resource_id: None,
                index_uuid: None,
                index: None,
            }],
            error_type,
            reason,
            resource_type: None,
            resource_id: None,
            index_uuid: None,
            index: None,
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

    /// Item 7: the admin key is authenticated through the constant-time
    /// comparator. Functionally this must be indistinguishable from `==`: the
    /// exact key authenticates, and a same-length-but-wrong key (the case a
    /// naive early-exit `==` would leak byte-by-byte) is rejected.
    #[tokio::test]
    async fn admin_key_constant_time_accepts_exact_rejects_wrong() {
        let admin = "admin-secret-key-0123456789abcdef";
        let state = test_state(admin);

        // Exact key authenticates.
        assert!(is_authorized(&state, Some(&format!("ApiKey {admin}"))));
        assert!(is_authorized(&state, Some(&format!("Bearer {admin}"))));

        // Same length, differs only in the LAST byte — the worst case for an
        // early-exit `==` (it would compare all but one byte before failing).
        let mut wrong = admin.to_string();
        wrong.pop();
        wrong.push('X');
        assert_eq!(wrong.len(), admin.len(), "wrong key must match length");
        assert!(!is_authorized(&state, Some(&format!("ApiKey {wrong}"))));

        // Differs only in the FIRST byte, and a length mismatch, both rejected.
        assert!(!is_authorized(
            &state,
            Some("ApiKey Xdmin-secret-key-0123456789abcdef")
        ));
        assert!(!is_authorized(&state, Some("ApiKey short")));
        assert!(!is_authorized(&state, None));
    }

    /// Regression for the RC4 blocker: `/health/live` + `/health/ready` must
    /// stay reachable without credentials even when auth is enabled, so Docker
    /// HEALTHCHECKs and k8s probes don't crashloop on a hardened deployment.
    /// Every other route must still demand a key.
    #[tokio::test]
    async fn health_probes_bypass_auth_when_enabled() {
        let admin = "admin-secret-key";
        let state = test_state(admin);
        let app = Router::new()
            .route("/health/live", get(|| async { "live" }))
            .route("/health/ready", get(|| async { "ready" }))
            .route("/_cluster/health", get(|| async { "data" }))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        // Probes answer 200 with NO credentials, even though auth is on.
        let (status, _) = send(&app, "GET", "/health/live", None, "").await;
        assert_eq!(status, StatusCode::OK, "liveness must bypass auth");
        let (status, _) = send(&app, "GET", "/health/ready", None, "").await;
        assert_eq!(status, StatusCode::OK, "readiness must bypass auth");

        // A normal endpoint still requires the key.
        let (status, _) = send(&app, "GET", "/_cluster/health", None, "").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "data endpoint must still 401 without a key"
        );
        let admin_hdr = format!("ApiKey {admin}");
        let (status, _) = send(&app, "GET", "/_cluster/health", Some(&admin_hdr), "").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "data endpoint must pass with the admin key"
        );
    }
}
