//! Xerj Console UX — embedded UI bundle served at `/_xerj-console`.
//!
//! `build.rs` walks the xerj-ux source tree at compile time and
//! generates `OUT_DIR/xerj_console_assets.rs` with a sorted slice of
//! `(url_path, bytes, content_type)` tuples. This module includes
//! that slice and exposes one Axum handler that does an O(log n)
//! lookup by path.
//!
//! ## URL shape
//!
//! ```text
//! GET /_xerj-console             → Console index.html (302 to /_xerj-console/)
//! GET /_xerj-console/            → Console index.html (200, text/html)
//! GET /_xerj-console/{path*}     → static asset by path, 404 if missing
//! ```
//!
//! Cache-Control is set to `public, max-age=300` for static assets;
//! the index.html gets `no-cache` so a deploy of a new binary surfaces
//! the new UI on next page load without ctrl-F5.
//!
//! ## Why not include_dir / rust-embed
//!
//! Both pull in proc-macro deps. The build.rs approach is ~80 LOC,
//! depends only on std, and produces a slice of `include_bytes!()`
//! references that the linker resolves directly.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
};

include!(concat!(env!("OUT_DIR"), "/xerj_console_assets.rs"));

/// Find an asset by its url path. Linear scan is fine for ~50 files;
/// the slice is sorted so we could binary-search if the bundle grows.
fn find_asset(path: &str) -> Option<&'static (&'static str, &'static [u8], &'static str)> {
    XERJ_CONSOLE_ASSETS.iter().find(|(p, _, _)| *p == path)
}

/// Serve the Console's index.html for `/_xerj-console/`.
pub async fn xerj_console_index() -> impl IntoResponse {
    serve("index.html", true)
}

/// Serve a named asset under `/_xerj-console/{path*}`.
pub async fn xerj_console_asset(Path(rest): Path<String>) -> impl IntoResponse {
    // Empty rest path = same as `/_xerj-console/` — serve index.
    let path = if rest.is_empty() { "index.html".to_string() } else { rest };
    serve(&path, false)
}

/// Bare `/_xerj-console` (no trailing slash) — relative `<script src="src/app.js">`
/// style tags resolve correctly only with the trailing slash, so redirect.
pub async fn xerj_console_redirect() -> impl IntoResponse {
    Redirect::permanent("/_xerj-console/")
}

fn serve(path: &str, no_cache: bool) -> Response {
    // Defence-in-depth: no .. components allowed in a relative URL,
    // even though the asset table is statically built and cannot
    // contain them.
    if path.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }
    let asset = match find_asset(path) {
        Some(a) => a,
        None => {
            // Fall back to `<name>.html` when the path has no
            // extension — gives us nice URLs like `/_xerj-console/setup` and
            // `/_xerj-console/login` without an explicit `.html`. We never
            // append `.html` to a path with a dot already in the last
            // segment so a typo on a real asset still 404s instead of
            // serving the wrong page.
            if !path.rsplit('/').next().unwrap_or("").contains('.') {
                let html = format!("{path}.html");
                if let Some(a) = find_asset(&html) {
                    return serve_asset(a, no_cache);
                }
            }
            return (
                StatusCode::NOT_FOUND,
                format!("xerj-console asset not found: {path}"),
            ).into_response();
        }
    };
    serve_asset(asset, no_cache)
}

fn serve_asset(
    asset: &(&'static str, &'static [u8], &'static str),
    no_cache: bool,
) -> Response {
    let cache = if no_cache {
        "no-cache"
    } else {
        "public, max-age=300"
    };
    (
        [
            (header::CONTENT_TYPE, asset.2),
            (header::CACHE_CONTROL, cache),
        ],
        asset.1,
    )
        .into_response()
}

/// Number of bundled assets — exposed for the startup banner so
/// operators see at-a-glance whether the Console UX was bundled.
pub fn asset_count() -> usize {
    XERJ_CONSOLE_ASSETS.len()
}
