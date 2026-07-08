//! Public router — Xerj Console exposes a *complete* HTTP surface that any
//! container can mount as a peer to other axum routers.  In the v1.0
//! bundled binary, `xerj-server` brings up this router alongside the
//! engine's ES-compat / native routers via `Router::merge` so they
//! share one TCP listener.  When Xerj Console is extracted post-v1.0 it ships
//! with this same router as the binary's main entry point.
//!
//! Surface owned by this crate:
//!
//! ```text
//! /_xerj-console                 redirect → /_xerj-console/
//! /_xerj-console/                bundled SPA index.html
//! /_xerj-console/setup           bundled setup.html (magic-link claim)
//! /_xerj-console/login           bundled login.html (returning user)
//! /_xerj-console/{path*}         bundled SPA assets (CSS, JS, images, fonts)
//! /_xerj-console/api/v1/...      API endpoints (auth, prefs, dashboards, …)
//! ```
//!
//! ## Why one router, not two?
//!
//! Mounting the SPA assets and the API as one Router ensures Xerj Console
//! owns the full `/_xerj-console/*` namespace exclusively — no risk that the
//! engine's ES-compat router accidentally serves a 404 page over an
//! asset request, or vice versa.  The engine layer enforces its own
//! `Authorization: Bearer` policy on its routes only; this router
//! enforces its session-cookie policy on its routes only.

use axum::{
    routing::{delete, get, patch, post},
    Router,
};

use crate::{auth, cluster, dashboards, data_sources, prefs, spa, state::ConsoleState, views};

/// Build the full Xerj Console router. Mount at the root of an axum Router
/// (it owns the `/_xerj-console/*` prefix internally):
///
/// ```ignore
/// let merged = engine_router.merge(xerj_console_api::xerj_console_router(state));
/// axum::serve(listener, merged).await?;
/// ```
pub fn xerj_console_router(state: ConsoleState) -> Router {
    Router::new()
        // ── Bundled SPA assets (index, setup, login, css, js, fonts) ───────
        .route("/_xerj-console", get(spa::xerj_console_redirect))
        .route("/_xerj-console/", get(spa::xerj_console_index))
        .route("/_xerj-console/*rest", get(spa::xerj_console_asset))
        // ── API: cluster awareness ─────────────────────────────────────────
        .route("/_xerj-console/api/v1/cluster/info", get(cluster::info))
        // ── API: auth bootstrap (unauthenticated) ──────────────────────────
        .route(
            "/_xerj-console/api/v1/auth/magic/redeem",
            post(auth::magic::redeem),
        )
        // ── API: invite issue (owner/admin session required) ───────────────
        .route(
            "/_xerj-console/api/v1/auth/magic/issue",
            post(auth::magic::issue),
        )
        .route(
            "/_xerj-console/api/v1/auth/passkey/begin",
            post(auth::passkey::begin),
        )
        .route(
            "/_xerj-console/api/v1/auth/passkey/finish",
            post(auth::passkey::finish),
        )
        .route(
            "/_xerj-console/api/v1/auth/login/begin",
            post(auth::login::begin),
        )
        .route(
            "/_xerj-console/api/v1/auth/login/finish",
            post(auth::login::finish),
        )
        // ── API: session-protected ─────────────────────────────────────────
        .route(
            "/_xerj-console/api/v1/auth/logout",
            post(auth::login::logout),
        )
        .route("/_xerj-console/api/v1/me", get(auth::me::me))
        .route(
            "/_xerj-console/api/v1/auth/passkeys",
            get(auth::me::list_passkeys),
        )
        .route(
            "/_xerj-console/api/v1/auth/passkeys/:id",
            delete(auth::me::delete_passkey),
        )
        .route(
            "/_xerj-console/api/v1/auth/api-tokens",
            get(auth::tokens::list).post(auth::tokens::create),
        )
        .route(
            "/_xerj-console/api/v1/auth/api-tokens/:id",
            delete(auth::tokens::revoke),
        )
        .route(
            "/_xerj-console/api/v1/prefs",
            get(prefs::get).put(prefs::put),
        )
        .route(
            "/_xerj-console/api/v1/dashboards",
            get(dashboards::list).post(dashboards::create),
        )
        .route(
            "/_xerj-console/api/v1/dashboards/:id",
            get(dashboards::get_one)
                .put(dashboards::replace)
                .delete(dashboards::delete),
        )
        .route(
            "/_xerj-console/api/v1/dashboards/:id",
            patch(dashboards::patch),
        )
        .route(
            "/_xerj-console/api/v1/views",
            get(views::list).post(views::create),
        )
        .route(
            "/_xerj-console/api/v1/views/:id",
            get(views::get_one).delete(views::delete),
        )
        .route(
            "/_xerj-console/api/v1/data-sources/connections",
            get(data_sources::list_connections),
        )
        .route(
            "/_xerj-console/api/v1/data-sources/connections/:id/indices",
            get(data_sources::list_indices),
        )
        .route(
            "/_xerj-console/api/v1/data-sources/connections/:id/indices/:name/fields",
            get(data_sources::list_fields),
        )
        .with_state(state)
}

/// Routes this crate currently exposes. Used by tests so an accidental
/// drop is caught at CI.
#[doc(hidden)]
pub fn known_routes() -> &'static [&'static str] {
    &[
        "/_xerj-console",
        "/_xerj-console/",
        "/_xerj-console/*rest",
        "/_xerj-console/api/v1/cluster/info",
        "/_xerj-console/api/v1/auth/magic/redeem",
        "/_xerj-console/api/v1/auth/magic/issue",
        "/_xerj-console/api/v1/auth/passkey/begin",
        "/_xerj-console/api/v1/auth/passkey/finish",
        "/_xerj-console/api/v1/auth/login/begin",
        "/_xerj-console/api/v1/auth/login/finish",
        "/_xerj-console/api/v1/auth/logout",
        "/_xerj-console/api/v1/me",
        "/_xerj-console/api/v1/auth/passkeys",
        "/_xerj-console/api/v1/auth/passkeys/:id",
        "/_xerj-console/api/v1/auth/api-tokens",
        "/_xerj-console/api/v1/auth/api-tokens/:id",
        "/_xerj-console/api/v1/prefs",
        "/_xerj-console/api/v1/dashboards",
        "/_xerj-console/api/v1/dashboards/:id",
        "/_xerj-console/api/v1/views",
        "/_xerj-console/api/v1/views/:id",
        "/_xerj-console/api/v1/data-sources/connections",
        "/_xerj-console/api/v1/data-sources/connections/:id/indices",
        "/_xerj-console/api/v1/data-sources/connections/:id/indices/:name/fields",
    ]
}
