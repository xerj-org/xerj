//! Smoke tests for the bundled Xerj Console SPA assets.
//!
//! `build.rs` walks the xerj-ux/ source tree at compile time and
//! emits a static `(url_path, bytes, content_type)` slice. These tests
//! confirm the runtime serves the files the demo flow depends on:
//! setup.html, login.html, the auth + sync JS modules, and the
//! console page with its auth guard, policy headers and guest modules.

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_console_api::{state::ClusterMode, xerj_console_router, ConsoleState};
use xerj_engine::Engine;

fn boot() -> (axum::Router, TempDir) {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    xerj_console_api::indices::ensure_all(&engine).unwrap();
    let state = ConsoleState::new(
        engine,
        "local".to_string(),
        [0u8; 32],
        ClusterMode::Standalone,
    );
    (xerj_console_router(state), dir)
}

async fn fetch(router: axum::Router, path: &str) -> (u16, String) {
    let resp = router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn setup_html_is_bundled_and_served() {
    let (router, _dir) = boot();
    // /_xerj-console/setup (no extension) should fall back to setup.html.
    let (status, body) = fetch(router, "/_xerj-console/setup").await;
    assert_eq!(
        status, 200,
        "expected 200 for /_xerj-console/setup, body={body}"
    );
    assert!(
        body.contains("XERJ CONSOLE") && body.contains("setup"),
        "setup page must contain XERJ CONSOLE banner — got: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("token") && body.contains("redeemMagic"),
        "setup page must include the magic-link consumption logic"
    );
}

#[tokio::test]
async fn login_html_is_bundled_and_served() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/login").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("XERJ CONSOLE") && body.contains("Sign in"),
        "login page must contain XERJ CONSOLE + sign-in copy"
    );
    assert!(
        body.contains("beginLogin"),
        "login page must include the WebAuthn login logic"
    );
}

#[tokio::test]
async fn xerj_console_auth_module_is_bundled() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/src/xerj-console-auth.js").await;
    assert_eq!(status, 200);
    assert!(body.contains("redeemMagic"));
    assert!(body.contains("beginEnrol"));
    assert!(body.contains("finishEnrol"));
    assert!(body.contains("beginLogin"));
    assert!(body.contains("finishLogin"));
    assert!(body.contains("b64uToBuf"));
    assert!(body.contains("bufToB64u"));
}

#[tokio::test]
async fn xerj_console_sync_module_is_bundled() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/src/xerj-console-sync.js").await;
    assert_eq!(status, 200);
    assert!(body.contains("pullAll") && body.contains("startPush"));
}

#[tokio::test]
async fn console_page_boots_through_the_auth_guard_with_no_inline_script() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router.clone(), "/_xerj-console/").await;
    assert_eq!(status, 200);
    assert!(
        body.contains(r#"<script type="module" src="src/boot.js"></script>"#),
        "index.html must boot through src/boot.js"
    );
    // The page is served with `script-src 'self'`, so an inline script would
    // simply not run — and the auth guard used to be one. Every <script> tag
    // must carry a src.
    for tag in body.match_indices("<script").map(|(i, _)| &body[i..]) {
        let open = &tag[..tag.find('>').expect("unterminated <script")];
        assert!(
            open.contains(" src="),
            "inline script on the console page: {open}"
        );
    }

    // The guard itself now lives in boot.js.
    let (status, boot_js) = fetch(router, "/_xerj-console/src/boot.js").await;
    assert_eq!(status, 200);
    assert!(
        boot_js.contains("/_xerj-console/api/v1/me"),
        "boot.js must call /me as the auth guard"
    );
    assert!(
        boot_js.contains("/_xerj-console/login"),
        "boot.js must redirect to /login on 401"
    );
    assert!(
        boot_js.contains("guest-app.js"),
        "boot.js must hand a share record to the guest shell"
    );
}

async fn headers_of(router: axum::Router, path: &str) -> axum::http::HeaderMap {
    router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .headers()
        .clone()
}

#[tokio::test]
async fn console_page_is_served_with_the_content_security_policy() {
    let (router, _dir) = boot();
    for path in ["/_xerj-console/", "/_xerj-console/index.html"] {
        let h = headers_of(router.clone(), path).await;
        let csp = h
            .get("content-security-policy")
            .unwrap_or_else(|| panic!("{path}: no Content-Security-Policy"))
            .to_str()
            .unwrap();
        assert_eq!(csp, xerj_console_api::spa::CONSOLE_CSP);
        assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
        assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(h.get("cache-control").unwrap(), "no-cache");
    }

    // What the policy has to say, whatever else it grows: scripts and
    // connections from this origin only, and nothing that re-opens inline
    // script or eval.
    let csp = xerj_console_api::spa::CONSOLE_CSP;
    let directive = |name: &str| -> Vec<&str> {
        csp.split(';')
            .map(str::trim)
            .find(|d| d.split(' ').next() == Some(name))
            .unwrap_or_else(|| panic!("CSP has no {name} directive"))
            .split(' ')
            .skip(1)
            .collect()
    };
    assert_eq!(directive("script-src"), ["'self'"]);
    assert_eq!(directive("connect-src"), ["'self'"]);
    assert_eq!(directive("default-src"), ["'none'"]);
    assert_eq!(directive("object-src"), ["'none'"]);
    assert_eq!(directive("base-uri"), ["'none'"]);
    assert_eq!(directive("frame-ancestors"), ["'none'"]);
    assert!(!csp.contains("unsafe-eval"));
    assert!(!directive("script-src").contains(&"'unsafe-inline'"));

    // login / setup carry an inline module script and show no document data:
    // they are served without the policy (it would stop them working).
    for path in ["/_xerj-console/login", "/_xerj-console/setup"] {
        let h = headers_of(router.clone(), path).await;
        assert!(h.get("content-security-policy").is_none(), "{path}");
        assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    }
}

#[tokio::test]
async fn guest_and_reader_modules_are_bundled() {
    // A share link's guest page probes `src/data/guest.js` to decide whether
    // this console can take a guest; the rest are what the guest shell imports.
    let (router, _dir) = boot();
    for path in [
        "src/data/guest.js",
        "src/guest-app.js",
        "src/theme-boot.js",
        "src/data/transport-guest.js",
        "src/data/reader-api.js",
        "src/ux/safe-dom.js",
        "src/ux/reader-render.js",
        "src/ux/reader-view.js",
        "src/ux/corpus-render.js",
    ] {
        let (status, _) = fetch(router.clone(), &format!("/_xerj-console/{path}")).await;
        assert_eq!(status, 200, "{path} must be bundled");
    }
}

#[tokio::test]
async fn root_redirect_works() {
    // /_xerj-console (no trailing slash) → redirect to /_xerj-console/.
    let (router, _dir) = boot();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/_xerj-console")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "/_xerj-console must redirect, got {}",
        resp.status()
    );
}
