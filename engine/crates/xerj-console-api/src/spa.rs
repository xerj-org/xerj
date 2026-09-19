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
//! ## Content-Security-Policy on the console page
//!
//! `index.html` is the page that shows other people's documents (the Reader)
//! and, in guest mode, holds a share's API key in `sessionStorage`. It is
//! served with [`CONSOLE_CSP`]: scripts from this origin only — no inline
//! script, no `eval` — and no connection to any other origin. The SPA builds
//! document-derived DOM without an HTML parser (`xerj-ux/src/ux/safe-dom.js`);
//! the policy is the second wall behind that one, so a missed escape in some
//! other view cannot become script execution or send a key off-origin.
//! `xerj-ux/test/browser/` loads the console under this exact string (it is
//! read out of this file) and fails on any violation report, so the policy
//! and the SPA cannot drift apart silently.
//!
//! `login.html` / `setup.html` carry an inline module script and show no
//! document data, so they are served without it.
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

/// The policy `index.html` is served with. One line, no line breaks: the
/// browser tests parse it out of this file verbatim.
///
/// * `script-src 'self'` — no inline script, no `eval`, nothing off-origin.
/// * `connect-src 'self'` — `fetch` reaches this engine and nothing else.
/// * `style-src … 'unsafe-inline'` — the shell sets `style=""` attributes on
///   the markup it builds; styles cannot run script or read storage.
/// * fonts come from Google Fonts, as `index.html` has always linked them.
/// * `img-src data: blob:` — the inline favicon and the PNG chart export.
/// * `frame-ancestors 'none'` — the console cannot be framed (clickjacking).
pub const CONSOLE_CSP: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src 'self' https://fonts.gstatic.com; img-src 'self' data: blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; object-src 'none'";

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
    let path = if rest.is_empty() {
        "index.html".to_string()
    } else {
        rest
    };
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
            )
                .into_response();
        }
    };
    serve_asset(asset, no_cache)
}

fn serve_asset(asset: &(&'static str, &'static [u8], &'static str), no_cache: bool) -> Response {
    // The console page is always revalidated — also when it is requested by
    // name (`/_xerj-console/index.html`) rather than as the directory index —
    // so a new binary's UI and policy arrive together.
    let is_console_page = asset.0 == "index.html";
    let cache = if no_cache || is_console_page {
        "no-cache"
    } else {
        "public, max-age=300"
    };
    let mut resp = (
        [
            (header::CONTENT_TYPE, asset.2),
            (header::CACHE_CONTROL, cache),
            // Every asset declares its real type (build.rs); never let a
            // browser second-guess it.
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        asset.1,
    )
        .into_response();
    if is_console_page {
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static(CONSOLE_CSP),
        );
        // A guest's tab holds a key; never send this page's URL anywhere.
        h.insert(
            header::REFERRER_POLICY,
            header::HeaderValue::from_static("no-referrer"),
        );
    }
    resp
}

/// Number of bundled assets — exposed for the startup banner so
/// operators see at-a-glance whether the Console UX was bundled.
pub fn asset_count() -> usize {
    XERJ_CONSOLE_ASSETS.len()
}
