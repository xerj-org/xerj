//! API key authentication middleware — *who* the caller is.
//!
//! Checks the `Authorization` header for any of:
//! - `Authorization: ApiKey <key>`
//! - `Authorization: Bearer <key>`
//! - `Authorization: Basic <base64(user:pass)>`
//!
//! When `config.auth.enabled` is `false` (or `--insecure` mode was set by
//! clearing the key), the check is skipped entirely.
//!
//! Authentication answers *who*; [`crate::authz`] answers *what they may
//! touch*. The bridge between them is [`Principal`], produced here by
//! [`authenticate`] and consulted there. `is_authorized` — the boolean the
//! gRPC interceptor and the HTTP middleware share — is now a thin wrapper over
//! `authenticate`, so the two surfaces cannot drift.

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{request::Parts, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use xerj_engine::rbac::{Privilege, Role};

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

// ─────────────────────────────────────────────────────────────────────────────
// Principal
// ─────────────────────────────────────────────────────────────────────────────

/// The authenticated identity behind a request.
///
/// Authentication used to collapse to a bool, which is exactly why a brain was
/// not a security boundary: with no principal on the request there was nothing
/// for an authorization check to consult (issue #79). Every variant below is a
/// *capability statement*, and the ordering is deliberate — the further down
/// the list, the less a caller may reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// Full access. Two ways to be one, and only two:
    /// - **open mode** — `auth.enabled = false` or no admin key configured.
    ///   This is `xerj --insecure` and the point-at-a-folder first run, where
    ///   there is exactly one user and no configuration; it must keep working
    ///   with zero setup, so it is deliberately unrestricted.
    /// - the configured **admin/superuser key** (`auth.admin_api_key`).
    Superuser,
    /// A key minted by `POST /_security/api_key` **with** usable
    /// `role_descriptors`. Confined: it may touch only the indices its roles
    /// name, and nothing that is not index-scoped.
    Scoped {
        /// The minted key's id (for audit/log lines, never compared).
        key_id: String,
        /// Index grants parsed from `role_descriptors`. Never empty — an
        /// empty grant list is [`Principal::Unscoped`].
        roles: Vec<Role>,
    },
    /// A key minted **without** `role_descriptors` — the shape every key had
    /// before per-brain authorization existed.
    ///
    /// It keeps its historical reach over the general ES-compat surface (so
    /// hardened Kibana/Beats deployments do not break), but it holds **no**
    /// privilege on the reserved `.xerj-memory-*` namespace. That is the
    /// fail-closed half: an unconfigured principal is denied the brain, not
    /// granted it.
    Unscoped {
        /// The minted key's id.
        key_id: String,
    },
    /// No valid credential. Normally unreachable inside a handler — the auth
    /// middleware answers 401 first — but it is what the [`Principal`]
    /// extractor yields if a handler is ever mounted without that middleware,
    /// so the default is "deny", never "allow".
    Denied,
}

impl Principal {
    /// Does this principal hold `privilege` on `index`?
    ///
    /// This is the single authorization predicate; [`crate::authz`] routes
    /// every enforcement point through it. `Unscoped` answers `true` for
    /// ordinary indices (its historical reach) and `false` for the reserved
    /// namespace, which [`crate::authz::is_reserved_index`] defines.
    pub fn allows_index(&self, index: &str, privilege: Privilege) -> bool {
        match self {
            Principal::Superuser => true,
            Principal::Scoped { roles, .. } => roles.iter().any(|r| r.allows(index, privilege)),
            Principal::Unscoped { .. } => !crate::authz::is_reserved_index(index),
            Principal::Denied => false,
        }
    }

    /// True for the one principal that is allowed to see and do everything —
    /// used to skip the authorization work entirely on the local-dev path.
    pub fn is_superuser(&self) -> bool {
        matches!(self, Principal::Superuser)
    }

    /// Short identity string for error/log lines. Never contains key material.
    pub fn label(&self) -> &str {
        match self {
            Principal::Superuser => "superuser",
            Principal::Scoped { key_id, .. } | Principal::Unscoped { key_id } => key_id.as_str(),
            Principal::Denied => "unauthenticated",
        }
    }
}

/// Extract the principal straight from the request's `Authorization` header.
///
/// Deliberately *re-derives* rather than reading a request extension planted
/// by the middleware: a handler that authorizes must be safe by construction
/// even if it is mounted on a router where the middleware was forgotten. The
/// cost is one map lookup plus a constant-time compare. When no credential
/// resolves, the result is [`Principal::Denied`] — never an error that a
/// caller could turn into a bypass.
#[axum::async_trait]
impl FromRequestParts<AppState> for Principal {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        Ok(authenticate(state, header))
    }
}

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

    // RC4-W4 item 4: optional read-only metrics token. When configured, a
    // caller presenting the exact token may scrape `/v1/metrics` — and ONLY
    // that endpoint — without the admin key, so Prometheus can be handed a
    // low-privilege scrape credential that cannot read index data. The admin
    // key still works for metrics via the normal path below.
    if req.uri().path() == "/v1/metrics" && metrics_token_authorized(&state, &req) {
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
/// - the `Authorization` value carries the configured admin/superuser key
///   (as `ApiKey <key>`/`Bearer <key>`, or as the password half of
///   `Basic <base64(user:pass)>`); or
/// - it carries a key minted by `POST /_security/api_key`, as either
///   `ApiKey <base64(id:api_key)>` or `Basic <base64(id:api_key)>` — Kibana's
///   interactive "basic" realm login sends the latter, and a minted key's
///   `id:secret` shape is already exactly `user:pass`.
///
/// `auth_header` is the raw `Authorization` header / `authorization` metadata
/// value (e.g. `"ApiKey abc"`, `"Bearer abc"`, or `"Basic base64(...)"`), or
/// `None` when absent.
pub fn is_authorized(state: &AppState, auth_header: Option<&str>) -> bool {
    !matches!(authenticate(state, auth_header), Principal::Denied)
}

/// Resolve the request's credential to a [`Principal`].
///
/// Same acceptance rules as [`is_authorized`] (which is now defined in terms of
/// this), but it reports *which* identity authenticated so authorization has
/// something to decide against. Returns [`Principal::Denied`] when nothing
/// authenticates.
pub fn authenticate(state: &AppState, auth_header: Option<&str>) -> Principal {
    let cfg = &state.config.auth;

    // Open mode: auth disabled or no admin key configured. This is the
    // `--insecure` / point-at-a-folder posture — one user, no configuration —
    // and it stays fully unrestricted on purpose.
    if !cfg.enabled || cfg.admin_api_key.is_empty() {
        return Principal::Superuser;
    }

    let Some(header) = auth_header else {
        return Principal::Denied;
    };

    if let Some(key) = extract_key(header) {
        // The configured admin/superuser key. Compared in constant time
        // (item 7): a plain `==` short-circuits on the first mismatching byte,
        // leaking the shared-secret prefix length via response timing. The
        // created-key path already used `constant_time_eq`; the admin key —
        // the single most valuable credential in the system — must not be
        // weaker. `constant_time_eq` still returns early on a length mismatch,
        // which only reveals the key *length*, matching the created-key path.
        if constant_time_eq(key.as_bytes(), cfg.admin_api_key.as_bytes()) {
            return Principal::Superuser;
        }
        // A key minted by `POST /_security/api_key`.
        return authenticate_api_key(state, key);
    }

    basic_principal(state, header)
}

/// Validate `Authorization: Basic <base64(user:pass)>`.
///
/// xerj has no per-username credential store — same single-owner identity
/// model `security_authenticate` already uses — so this doesn't check the
/// username against anything. Two things are accepted as the *password*
/// half, both already valid credentials via the `ApiKey`/`Bearer` schemes:
/// - the admin/superuser key (any username — mirrors ES's own "elastic"
///   bootstrap superuser, where the secret is what's checked, not the name);
/// - or, treating the whole decoded `user:pass` as `id:secret`, a key minted
///   by `POST /_security/api_key` (its `id:secret` shape already matches
///   `user:pass` exactly, so no separate encoding is needed here).
fn basic_principal(state: &AppState, header: &str) -> Principal {
    let Some(prefix) = header.get(..6) else {
        return Principal::Denied;
    };
    if !prefix.eq_ignore_ascii_case("basic ") {
        return Principal::Denied;
    }
    let encoded = header[6..].trim();
    let decoded = match crate::es_compat::base64_decode(encoded) {
        Some(d) => d,
        None => return Principal::Denied,
    };
    let decoded = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return Principal::Denied,
    };
    let Some((user, pass)) = decoded.split_once(':') else {
        return Principal::Denied;
    };

    let cfg = &state.config.auth;
    if constant_time_eq(pass.as_bytes(), cfg.admin_api_key.as_bytes()) {
        return Principal::Superuser;
    }
    check_minted_key(state, user, pass)
}

/// Decide whether a request to `/v1/metrics` may proceed on the strength of
/// the read-only `auth.metrics_token` alone (RC4-W4 item 4).
///
/// Returns `false` (falls through to normal auth) when the token is unset or
/// the presented credential doesn't match it. The comparison is constant-time,
/// matching the admin-key path — a scrape token is still a shared secret.
fn metrics_token_authorized(state: &AppState, req: &Request) -> bool {
    let token = &state.config.auth.metrics_token;
    if token.is_empty() {
        return false;
    }
    req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(extract_key)
        .map(|k| constant_time_eq(k.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
}

/// Re-authenticate a created API key credential.
///
/// The presented value is `base64("id:api_key")`. Decode it, split on the
/// first `':'`, look the id up in the engine's key store, and constant-time
/// compare the secret. The resulting principal carries the key's stored
/// grants: [`Principal::Scoped`] when it was minted with usable
/// `role_descriptors`, [`Principal::Unscoped`] otherwise.
fn authenticate_api_key(state: &AppState, presented: &str) -> Principal {
    let decoded = match crate::es_compat::base64_decode(presented) {
        Some(d) => d,
        None => return Principal::Denied,
    };
    let decoded = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return Principal::Denied,
    };
    let (id, secret) = match decoded.split_once(':') {
        Some(parts) => parts,
        None => return Principal::Denied,
    };
    check_minted_key(state, id, secret)
}

/// Look up a minted `POST /_security/api_key` credential by `(id, secret)`.
/// Shared by the `ApiKey <base64(id:secret)>` and `Basic <base64(id:secret)>`
/// paths — both decode to the same `id:secret` shape.
///
/// A minted key is **never** a superuser: the strongest thing it can be is
/// `Unscoped`, which has no reach into the reserved `.xerj-memory-*`
/// namespace. That is what stops "any authenticated caller reads any brain".
fn check_minted_key(state: &AppState, id: &str, secret: &str) -> Principal {
    let record = match state.engine.api_keys.get(id) {
        Some(r) => r,
        None => return Principal::Denied,
    };
    if record.invalidated {
        return Principal::Denied;
    }
    if let Some(exp) = record.expiration_ms {
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        if now_ms >= exp {
            return Principal::Denied;
        }
    }
    if !constant_time_eq(record.secret.as_bytes(), secret.as_bytes()) {
        return Principal::Denied;
    }
    if record.roles.is_empty() {
        Principal::Unscoped {
            key_id: id.to_string(),
        }
    } else {
        Principal::Scoped {
            key_id: id.to_string(),
            roles: record.roles.clone(),
        }
    }
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

    /// Kibana's interactive "basic" realm login authenticates to ES via
    /// `Authorization: Basic base64(user:pass)`, not `ApiKey`/`Bearer` — this
    /// is what let the login form work at all once xerj started requiring
    /// auth. Any username is accepted as long as the password is the admin
    /// key (mirrors ES's own "elastic" bootstrap superuser), and a minted
    /// `POST /_security/api_key` credential also works via Basic since its
    /// `id:secret` shape already IS `user:pass`.
    #[tokio::test]
    async fn basic_auth_accepts_admin_password_and_minted_key() {
        let admin = "admin-secret-key-0123456789abcdef";
        let state = test_state(admin);
        let app = app(state.clone());

        // Any username, password = admin key.
        let hdr = format!(
            "Basic {}",
            crate::es_compat::base64_encode(&format!("kibana_system:{admin}"))
        );
        assert!(is_authorized(&state, Some(&hdr)));

        // Wrong password is rejected.
        let bad_hdr = format!(
            "Basic {}",
            crate::es_compat::base64_encode("kibana_system:wrong-password")
        );
        assert!(!is_authorized(&state, Some(&bad_hdr)));

        // Malformed input never panics: bad base64, no colon, too-short
        // header, and an empty credential all just fail authorization.
        assert!(!is_authorized(&state, Some("Basic %%%not-base64%%%")));
        assert!(!is_authorized(
            &state,
            Some(&format!(
                "Basic {}",
                crate::es_compat::base64_encode("no-colon-here")
            ))
        ));
        assert!(!is_authorized(&state, Some("Bas")));
        assert!(!is_authorized(&state, Some("Basic ")));

        // A key minted via POST /_security/api_key also works presented as
        // Basic `id:secret` (exercised through the real handler, matching
        // how `created_api_key_is_reauthenticatable` tests the ApiKey path).
        let admin_hdr = format!("ApiKey {admin}");
        let (status, body) = send(
            &app,
            "POST",
            "/_security/api_key",
            Some(&admin_hdr),
            r#"{"name":"kibana"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = body["id"].as_str().unwrap().to_string();
        let secret = body["api_key"].as_str().unwrap().to_string();
        let minted_basic_hdr = format!(
            "Basic {}",
            crate::es_compat::base64_encode(&format!("{id}:{secret}"))
        );
        let (status, _) = send(
            &app,
            "GET",
            "/_security/_authenticate",
            Some(&minted_basic_hdr),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "minted key via Basic should work");
    }

    /// RC4-W4 item 4: the read-only `metrics_token` scrapes `/v1/metrics` and
    /// ONLY `/v1/metrics`. It is not a blanket bypass — a data endpoint still
    /// 401s with it, and metrics still 401 with no credential at all.
    #[tokio::test]
    async fn metrics_token_scopes_to_metrics_only() {
        let admin = "admin-secret-key";
        let metrics_tok = "scrape-only-token";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let mut config = Config::default();
        config.server.data_dir = path.to_str().unwrap().to_string();
        config.auth.enabled = true;
        config.auth.admin_api_key = admin.to_string();
        config.auth.metrics_token = metrics_tok.to_string();
        let metrics = Metrics::new().expect("metrics");
        let engine = Engine::new(config.clone()).expect("engine");
        let state = AppState::new(config, engine, metrics);

        let app = Router::new()
            .route("/v1/metrics", get(|| async { "metrics" }))
            .route("/_cluster/health", get(|| async { "data" }))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        let tok_hdr = format!("Bearer {metrics_tok}");
        // The scrape token reads metrics …
        let (status, _) = send(&app, "GET", "/v1/metrics", Some(&tok_hdr), "").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "metrics token must scrape /v1/metrics"
        );
        // … but NOTHING else.
        let (status, _) = send(&app, "GET", "/_cluster/health", Some(&tok_hdr), "").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "metrics token must not read index data"
        );
        // The admin key still works for metrics.
        let admin_hdr = format!("ApiKey {admin}");
        let (status, _) = send(&app, "GET", "/v1/metrics", Some(&admin_hdr), "").await;
        assert_eq!(status, StatusCode::OK, "admin key still scrapes metrics");
        // A missing credential still 401s — the token is opt-in, not open.
        let (status, _) = send(&app, "GET", "/v1/metrics", None, "").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "metrics still demands a credential"
        );
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
