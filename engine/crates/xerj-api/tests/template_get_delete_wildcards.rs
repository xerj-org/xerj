//! Template GET with no name, and template names that are patterns or lists.
//!
//! Elasticsearch 8.13 serves `GET /_template`, `GET /_index_template` and
//! `GET /_component_template` with no name: every template, always 200, even
//! when there are none. XERJ had no such route, so the request fell through
//! to `GET /{index}` and was refused as an invalid index name.
//!
//! A name may also select several templates. ES `Regex.simpleMatch`es a
//! name containing `*` on GET and DELETE for all three kinds; legacy GET
//! splits a comma list, and composable and component DELETE take a comma
//! list of exact names that must all exist. A pattern GET that matches
//! nothing is a 404, and a DELETE of `*` with nothing to delete is a no-op
//! ack. XERJ only special-cased an exact `*` or `_all` on GET (200 even when
//! empty) and only exact names on DELETE.
//!
//! Sources: `RestGetIndexTemplateAction`, `RestGetComposableIndexTemplateAction`,
//! `RestGetComponentTemplateAction`, `TransportGet{Composable,Component}*Action`
//! and `MetadataIndexTemplateService.{removeTemplates,innerRemoveIndexTemplateV2,
//! innerRemoveComponentTemplate}` at the v8.13.4 tag.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app_at(dir: &std::path::Path) -> axum::Router {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    xerj_api::router::build_es_compat_router(state)
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// One template API: its base path, a body it accepts, and how to read the
/// template names out of a GET response.
struct Kind {
    base: &'static str,
    body: Value,
    names: fn(&Value) -> Vec<String>,
}

fn listed(v: &Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = v[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` is not a list in {v}"))
        .iter()
        .map(|t| t["name"].as_str().expect("name").to_string())
        .collect();
    out.sort();
    out
}

fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            base: "/_template",
            body: json!({ "index_patterns": ["x-*"] }),
            names: |v| {
                let mut out: Vec<String> = v
                    .as_object()
                    .unwrap_or_else(|| panic!("legacy GET is not an object: {v}"))
                    .keys()
                    .cloned()
                    .collect();
                out.sort();
                out
            },
        },
        Kind {
            base: "/_index_template",
            body: json!({ "index_patterns": ["x-*"] }),
            names: |v| listed(v, "index_templates"),
        },
        Kind {
            base: "/_component_template",
            body: json!({ "template": {} }),
            names: |v| listed(v, "component_templates"),
        },
    ]
}

/// Store `logs-a`, `logs-b` and `metrics` under `kind`.
async fn seed(app: &axum::Router, kind: &Kind) {
    for name in ["logs-a", "logs-b", "metrics"] {
        // Distinct patterns, so composable templates do not collide on priority.
        let mut body = kind.body.clone();
        if body.get("index_patterns").is_some() {
            body["index_patterns"] = json!([format!("{name}-*")]);
        }
        let (status, got) = send(app, "PUT", &format!("{}/{name}", kind.base), Some(&body)).await;
        assert_eq!(status, StatusCode::OK, "PUT {}/{name}: {got}", kind.base);
    }
}

fn s(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[tokio::test]
async fn a_get_with_no_name_lists_every_template_and_is_200_even_when_empty() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());

        let (status, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "GET {} on an empty store: {got}",
            kind.base
        );
        assert!((kind.names)(&got).is_empty(), "GET {}: {got}", kind.base);

        seed(&app, &kind).await;
        let (status, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(status, StatusCode::OK, "GET {}: {got}", kind.base);
        assert_eq!(
            (kind.names)(&got),
            s(&["logs-a", "logs-b", "metrics"]),
            "GET {}",
            kind.base
        );
    }
}

#[tokio::test]
async fn a_pattern_get_returns_only_the_matches() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, &kind).await;

        let path = format!("{}/logs-*", kind.base);
        let (status, got) = send(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK, "GET {path}: {got}");
        assert_eq!((kind.names)(&got), s(&["logs-a", "logs-b"]), "GET {path}");

        let path = format!("{}/*-b", kind.base);
        let (status, got) = send(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK, "GET {path}: {got}");
        assert_eq!((kind.names)(&got), s(&["logs-b"]), "GET {path}");
    }
}

#[tokio::test]
async fn a_pattern_get_that_matches_nothing_is_404() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());

        let path = format!("{}/*", kind.base);
        let (status, got) = send(&app, "GET", &path, None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "GET {path} on an empty store: {got}"
        );

        seed(&app, &kind).await;
        let path = format!("{}/nomatch*", kind.base);
        let (status, got) = send(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {path}: {got}");
    }
}

#[tokio::test]
async fn a_legacy_get_takes_a_comma_list_and_skips_missing_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_at(dir.path());
    let legacy = &kinds()[0];
    seed(&app, legacy).await;

    let (status, got) = send(&app, "GET", "/_template/logs-a,metrics", None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!((legacy.names)(&got), s(&["logs-a", "metrics"]));

    let (status, got) = send(&app, "GET", "/_template/logs-*,nope", None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!((legacy.names)(&got), s(&["logs-a", "logs-b"]));

    let (status, got) = send(&app, "GET", "/_template/nope,nomatch*", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{got}");
}

#[tokio::test]
async fn a_pattern_delete_removes_only_the_matches() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, &kind).await;

        let path = format!("{}/logs-*", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::OK, "DELETE {path}: {got}");
        assert_eq!(got, json!({ "acknowledged": true }), "DELETE {path}");

        let (status, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(status, StatusCode::OK, "GET {}: {got}", kind.base);
        assert_eq!((kind.names)(&got), s(&["metrics"]), "after DELETE {path}");
    }
}

#[tokio::test]
async fn a_pattern_delete_that_matches_nothing_is_404_and_removes_nothing() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, &kind).await;

        let path = format!("{}/nomatch*", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "DELETE {path}: {got}");

        let (_, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(
            (kind.names)(&got),
            s(&["logs-a", "logs-b", "metrics"]),
            "after DELETE {path}"
        );
    }
}

#[tokio::test]
async fn deleting_star_from_an_empty_store_is_acknowledged() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());

        let path = format!("{}/*", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "DELETE {path} on an empty store: {got}"
        );
        assert_eq!(got, json!({ "acknowledged": true }), "DELETE {path}");
    }
}

#[tokio::test]
async fn composable_and_component_deletes_take_an_all_or_nothing_comma_list() {
    for kind in &kinds()[1..] {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, kind).await;

        // One name is missing: nothing is removed.
        let path = format!("{}/logs-a,nope", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "DELETE {path}: {got}");
        let (_, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(
            (kind.names)(&got),
            s(&["logs-a", "logs-b", "metrics"]),
            "after DELETE {path}"
        );

        // A list does not expand patterns: ES treats `logs-*` here as a name.
        let path = format!("{}/logs-*,metrics", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "DELETE {path}: {got}");

        let path = format!("{}/logs-a,metrics", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::OK, "DELETE {path}: {got}");
        let (_, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!((kind.names)(&got), s(&["logs-b"]), "after DELETE {path}");
    }
}

#[tokio::test]
async fn a_pattern_delete_is_still_gone_after_a_restart() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let app = app_at(dir.path());
            seed(&app, &kind).await;
            let path = format!("{}/logs-*", kind.base);
            let (status, got) = send(&app, "DELETE", &path, None).await;
            assert_eq!(status, StatusCode::OK, "DELETE {path}: {got}");
        }
        let app = app_at(dir.path());
        let (status, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "GET {} after restart: {got}",
            kind.base
        );
        assert_eq!((kind.names)(&got), s(&["metrics"]), "after restart");
    }
}

#[tokio::test]
async fn an_exact_name_still_behaves_as_before() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, &kind).await;

        let path = format!("{}/logs-a", kind.base);
        let (status, got) = send(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK, "GET {path}: {got}");
        assert_eq!((kind.names)(&got), s(&["logs-a"]), "GET {path}");

        let path = format!("{}/nope", kind.base);
        let (status, got) = send(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {path}: {got}");
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "DELETE {path}: {got}");

        let path = format!("{}/logs-a", kind.base);
        let (status, got) = send(&app, "DELETE", &path, None).await;
        assert_eq!(status, StatusCode::OK, "DELETE {path}: {got}");
    }
}

#[tokio::test]
async fn a_listing_that_finds_nothing_keeps_its_empty_body_shape() {
    for (base, empty) in [
        ("/_template", json!({})),
        ("/_index_template", json!({ "index_templates": [] })),
        ("/_component_template", json!({ "component_templates": [] })),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        let (status, got) = send(&app, "GET", &format!("{base}/nomatch-*"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {base}/nomatch-*: {got}");
        assert_eq!(got, empty, "GET {base}/nomatch-*");
    }
}

#[tokio::test]
async fn all_lists_everything_on_get_but_is_a_literal_name_on_delete() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, &kind).await;

        let (status, got) = send(&app, "GET", &format!("{}/_all", kind.base), None).await;
        assert_eq!(status, StatusCode::OK, "GET {}/_all: {got}", kind.base);
        assert_eq!((kind.names)(&got), s(&["logs-a", "logs-b", "metrics"]));

        let (status, got) = send(&app, "DELETE", &format!("{}/_all", kind.base), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "DELETE {}/_all: {got}",
            kind.base
        );
        let (_, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!((kind.names)(&got), s(&["logs-a", "logs-b", "metrics"]));
    }
}

#[tokio::test]
async fn deleting_star_removes_every_template() {
    for kind in kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = app_at(dir.path());
        seed(&app, &kind).await;

        let (status, got) = send(&app, "DELETE", &format!("{}/*", kind.base), None).await;
        assert_eq!(status, StatusCode::OK, "DELETE {}/*: {got}", kind.base);
        let (status, got) = send(&app, "GET", kind.base, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!((kind.names)(&got).is_empty(), "left behind: {got}");
    }
}

/// ES `removeTemplates` matches the whole name, so a comma is part of it.
#[tokio::test]
async fn a_legacy_delete_does_not_split_a_comma_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_at(dir.path());
    let kind = kinds().remove(0);
    assert_eq!(kind.base, "/_template");
    seed(&app, &kind).await;

    let (status, got) = send(&app, "DELETE", "/_template/logs-a,metrics", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{got}");
    let (_, got) = send(&app, "GET", "/_template", None).await;
    assert_eq!((kind.names)(&got), s(&["logs-a", "logs-b", "metrics"]));
}

/// The bare paths serve GET only. Any other method still reaches the
/// `/{index}` handler, exactly as `/_bogus` does, rather than axum's empty 405.
#[tokio::test]
async fn other_methods_on_a_bare_template_path_answer_as_an_index_path_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_at(dir.path());
    for base in ["/_template", "/_index_template", "/_component_template"] {
        for method in ["PUT", "DELETE", "HEAD"] {
            let body = (method == "PUT").then(|| json!({}));
            let (status, got) = send(&app, method, base, body.as_ref()).await;
            let (want_status, want) = send(&app, method, "/_bogus", body.as_ref()).await;
            assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {base}");
            assert_eq!(status, want_status, "{method} {base}: {got}");
            assert_eq!(
                got["error"]["type"], want["error"]["type"],
                "{method} {base}: {got}"
            );
        }
    }
    let (status, got) = send(&app, "PUT", "/_template", Some(&json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    assert_eq!(got["error"]["type"], "invalid_index_name_exception");
}
