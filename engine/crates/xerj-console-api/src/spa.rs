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
//! GET /_xerj-console/share       → the share-link guest page (share/index.html)
//! ```
//!
//! Cache-Control is set to `public, max-age=300` for static assets;
//! the index.html gets `no-cache` so a deploy of a new binary surfaces
//! the new UI on next page load without ctrl-F5.
//!
//! ## The share-link guest page
//!
//! `share/` is the one part of the bundle meant for someone who is **not** the
//! operator: the page a guest opens from a `xerj share` link, possibly through
//! a public tunnel. Like every other asset it is served without a session —
//! the guest has no credential until the page has exchanged a passcode for
//! one — so serving it makes nothing new reachable: it is three static files
//! that were already in the asset table. What it adds is a directory index
//! (`share` and `share/` both answer with `share/index.html`; the link the CLI
//! prints has no trailing slash) and a header set no other asset gets, see
//! [`GUEST_CSP`]. The page renders text from other people's documents, so the
//! policy is written to make a markup-injection bug inert rather than to rely
//! on there being none.
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

/// Everything under this asset directory is the share-link guest page.
const GUEST_DIR: &str = "share";

/// The guest page's Content-Security-Policy.
///
/// `default-src 'none'` and then only what the page uses, all same-origin: its
/// one script, one stylesheet, one icon, and `fetch` back to this node. No
/// inline script or style is permitted — so text that somehow became markup
/// could not run — and no third-party origin is reachable, so nothing a guest
/// reads can be beaconed elsewhere. `form-action 'none'` matters for a
/// specific failure: the passcode form has no `action`, and if the script
/// failed to load a native submit would put the passcode in the query string
/// of a GET, i.e. in the address bar and the access log. `frame-ancestors
/// 'none'` (with `X-Frame-Options` for browsers that predate it) keeps the
/// passcode prompt out of anyone else's frame.
pub const GUEST_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; \
     frame-ancestors 'none'";

/// Is `path` (an asset-table path) part of the guest page?
fn is_guest_asset(path: &str) -> bool {
    path.strip_prefix(GUEST_DIR)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Map a request path onto the asset table: the guest page's directory index,
/// with or without the trailing slash. Everything else is served as asked.
fn resolve_request_path(rest: &str) -> &str {
    match rest {
        "share" | "share/" => "share/index.html",
        other => other,
    }
}

/// Serve the Console's index.html for `/_xerj-console/`.
pub async fn xerj_console_index() -> impl IntoResponse {
    serve("index.html", true)
}

/// Serve a named asset under `/_xerj-console/{path*}`.
pub async fn xerj_console_asset(Path(rest): Path<String>) -> impl IntoResponse {
    // Empty rest path = same as `/_xerj-console/` — serve index.
    let path = if rest.is_empty() {
        "index.html"
    } else {
        resolve_request_path(&rest)
    };
    serve(path, false)
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
    if is_guest_asset(asset.0) {
        return serve_guest_asset(asset);
    }
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

/// The guest page and its assets: never stored, never framed, never sniffed,
/// never a `Referer` source, and confined by [`GUEST_CSP`].
///
/// `no-store` rather than the console's five-minute cache: a guest may be on
/// a borrowed or shared browser, and a page fetched through a quick tunnel
/// must not be replayed from a cache after the tunnel — and its hostname — is
/// gone. The files are a few kilobytes; the cost is nothing.
fn serve_guest_asset(asset: &(&'static str, &'static [u8], &'static str)) -> Response {
    (
        [
            (header::CONTENT_TYPE, asset.2),
            (header::CACHE_CONTROL, "no-store, max-age=0"),
            (header::PRAGMA, "no-cache"),
            (header::CONTENT_SECURITY_POLICY, GUEST_CSP),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (
                header::HeaderName::from_static("cross-origin-opener-policy"),
                "same-origin",
            ),
            (
                header::HeaderName::from_static("cross-origin-resource-policy"),
                "same-origin",
            ),
            (
                header::HeaderName::from_static("permissions-policy"),
                "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
            ),
            (
                header::HeaderName::from_static("x-robots-tag"),
                "noindex, nofollow, noarchive",
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A release tarball may omit `xerj-ux/` (build.rs then emits an empty
    /// table). With nothing bundled there is nothing to assert about.
    fn bundled() -> bool {
        asset_count() > 0
    }

    fn asset_text(path: &str) -> String {
        let asset = find_asset(path).unwrap_or_else(|| panic!("{path} is not bundled"));
        String::from_utf8(asset.1.to_vec()).expect("utf-8 asset")
    }

    fn header<'a>(resp: &'a Response, name: &str) -> &'a str {
        resp.headers()
            .get(name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .unwrap()
    }

    /// Drop `/* … */` blocks and whole-line `//` comments, so a scan for a
    /// forbidden API does not trip over the comment that forbids it.
    fn strip_js_comments(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut rest = src;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start..].find("*/") {
                Some(end) => rest = &rest[start + end + 2..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        out.push_str(rest);
        out.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_guest_page_answers_with_and_without_the_trailing_slash() {
        if !bundled() {
            return;
        }
        // The CLI prints `/_xerj-console/share#<id>`; a mail client or a
        // person may add the slash. Both are the page, not a 404 or a 400.
        for rest in ["share", "share/", "share/index.html"] {
            let resp = serve(resolve_request_path(rest), false);
            assert_eq!(resp.status(), StatusCode::OK, "{rest}");
            assert_eq!(header(&resp, "content-type"), "text/html; charset=utf-8");
            assert_eq!(header(&resp, "content-security-policy"), GUEST_CSP);
        }
    }

    #[test]
    fn guest_assets_carry_the_strict_header_set() {
        if !bundled() {
            return;
        }
        for path in ["share/index.html", "share/share.js", "share/share.css"] {
            let resp = serve(path, false);
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            assert_eq!(header(&resp, "cache-control"), "no-store, max-age=0");
            assert_eq!(header(&resp, "referrer-policy"), "no-referrer");
            assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
            assert_eq!(header(&resp, "x-frame-options"), "DENY");
            assert_eq!(header(&resp, "cross-origin-opener-policy"), "same-origin");
            let csp = header(&resp, "content-security-policy");
            for directive in [
                "default-src 'none'",
                "script-src 'self'",
                "style-src 'self'",
                "connect-src 'self'",
                "base-uri 'none'",
                "form-action 'none'",
                "frame-ancestors 'none'",
            ] {
                assert!(csp.contains(directive), "{path}: CSP lacks {directive}");
            }
            // Nothing that would let injected markup run or phone home.
            for forbidden in [
                "unsafe-inline",
                "unsafe-eval",
                "data:",
                "blob:",
                "*",
                "http",
            ] {
                assert!(!csp.contains(forbidden), "{path}: CSP contains {forbidden}");
            }
        }
    }

    #[test]
    fn the_console_itself_is_served_exactly_as_before() {
        if !bundled() {
            return;
        }
        let resp = serve("index.html", true);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header(&resp, "cache-control"), "no-cache");
        assert!(resp.headers().get("content-security-policy").is_none());
        assert!(!is_guest_asset("index.html"));
        assert!(!is_guest_asset("shared.js"));
        assert!(!is_guest_asset("share"));
        assert!(is_guest_asset("share/share.js"));
    }

    #[test]
    fn the_directory_index_opens_nothing_else() {
        // Only the two spellings of the directory map anywhere.
        assert_eq!(resolve_request_path("share"), "share/index.html");
        assert_eq!(resolve_request_path("share/"), "share/index.html");
        for other in [
            "shared",
            "share/x",
            "share//",
            "api/v1/me",
            "src/",
            "share/..",
        ] {
            assert_eq!(resolve_request_path(other), other);
        }
        for bad in [
            "share/..",
            "share/../index.html",
            "share//share.js",
            "../share",
        ] {
            assert_eq!(serve(bad, false).status(), StatusCode::BAD_REQUEST, "{bad}");
        }
        for missing in ["share/nope.js", "shared", "share/share", "share/index.htm"] {
            assert_eq!(
                serve(missing, false).status(),
                StatusCode::NOT_FOUND,
                "{missing}"
            );
        }
        // The console's extensionless fallback (`setup` → `setup.html`) also
        // makes `share/index` the page. It is the same file, and it gets the
        // same policy — the guest header set is keyed on the asset served,
        // not on the spelling of the request.
        if bundled() {
            let resp = serve("share/index", false);
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(header(&resp, "content-security-policy"), GUEST_CSP);
        }
    }

    #[test]
    fn the_guest_page_has_no_inline_code_and_loads_nothing_from_elsewhere() {
        if !bundled() {
            return;
        }
        let html = asset_text("share/index.html");
        let lower = html.to_ascii_lowercase();
        // Every <script> is external; the CSP would refuse an inline one, and
        // this fails the build before a browser has to.
        for (at, _) in lower.match_indices("<script") {
            let tag_end = lower[at..].find('>').map(|e| at + e).unwrap_or(lower.len());
            assert!(
                lower[at..tag_end].contains("src="),
                "inline <script> at byte {at}"
            );
        }
        // (`javascript:` is matched as an attribute value: the <noscript> text
        // legitimately says "This page needs JavaScript: …".)
        for forbidden in [
            "<style",
            " style=",
            "=\"javascript:",
            "='javascript:",
            "=javascript:",
            "http://",
            "https://",
            "src=\"//",
            "href=\"//",
            "<iframe",
            "<object",
            "<embed",
            "<base",
        ] {
            assert!(
                !lower.contains(forbidden),
                "guest page contains {forbidden}"
            );
        }
        // No inline event handlers (` onclick=`, ` onerror=`, …).
        for (at, _) in lower.match_indices(" on") {
            let word: String = lower[at + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            let after = lower[at + 1 + word.len()..].chars().next();
            assert!(
                after != Some('='),
                "inline event handler `{word}=` in the guest page"
            );
        }
        // What the page tells a guest has to be true through a tunnel too
        // (review of PR #947): it used to say the browser "talks to their
        // machine" and that nothing goes "to a third party", on a page served
        // through Cloudflare. The Cloudflare notice ships hidden; share.js
        // shows it on a quick-tunnel hostname.
        for untrue in ["third party", "talks to their machine"] {
            assert!(!lower.contains(untrue), "guest page still says `{untrue}`");
        }
        for id in ["id=\"via-tunnel\" hidden", "id=\"room-via-tunnel\" hidden"] {
            assert!(html.contains(id), "guest page lacks {id}");
        }
        assert!(lower.contains("cloudflare carries the connection and can read"));
        // Every asset it names is one the bundle actually carries.
        for asset in ["share/share.js", "share/share.css", "share/icon.svg"] {
            assert!(find_asset(asset).is_some(), "{asset} is not bundled");
            assert!(
                html.contains(&format!("/_xerj-console/{asset}")),
                "page does not reference {asset}"
            );
        }
    }

    #[test]
    fn the_guest_script_never_parses_document_text_as_markup() {
        if !bundled() {
            return;
        }
        // Text from the shared index is attacker-controlled (it is other
        // people's email). The page may only ever place it as text.
        let code = strip_js_comments(&asset_text("share/share.js"));
        for sink in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "DOMParser",
            "createContextualFragment",
            "srcdoc",
            "eval(",
            "new Function",
            "setTimeout('",
            "setTimeout(\"",
            "localStorage",
            "document.cookie",
        ] {
            assert!(!code.contains(sink), "share.js uses `{sink}`");
        }
        // The one storage contract the console's guest mode depends on.
        assert!(code.contains("'xerj.share'"));
        assert!(code.contains("sessionStorage.setItem(SHARE_KEY"));
        // The share id goes to the node in the claim BODY. A path built from
        // it is what put the id into access logs (review of PR #947).
        assert!(code.contains("'/_share/claim'"), "claim route");
        assert!(
            !code.contains("/_share/${"),
            "share.js builds a /_share path from a variable — the id must stay out of URLs"
        );
        assert!(code.contains("{ id: state.shareId, passcode }"));
        // A link pasted over another differs only in its fragment.
        assert!(code.contains("'hashchange'"));
        // The highlight delimiters are private-use code points, which are
        // invisible in an editor and in review: they must be written as
        // escapes, never as the characters themselves.
        assert!(
            !asset_text("share/share.js")
                .chars()
                .any(|c| ('\u{E000}'..='\u{F8FF}').contains(&c)),
            "share.js contains a raw private-use character; write it as \\uE000"
        );
        assert!(code.contains("'\\uE000'") && code.contains("'\\uE001'"));
        // No request leaves the origin: every fetch target is a path.
        assert!(!code.contains("http://") && !code.contains("https://"));
    }
}
