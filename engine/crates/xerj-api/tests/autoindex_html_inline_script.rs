//! Issue #2356: a JavaScript function defined in an inline `<script>` of an
//! HTML file must be findable by `xerj def` and `xerj search` after
//! `xerj autoindex`, at the line it occupies in that file.
//!
//! Python in the same folder is the control: it already indexed. The page's
//! visible text stays searchable, and script source stays out of that body.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_autoindex::cli::IndexCfg;

const PAGE_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><title>Drawer</title></head>
<body>
<p>The drawer holds the menu.</p>
<script>
function openDrawer(id) {
  document.getElementById(id).hidden = false;
}
</script>
<script type="module">
export function openPanel() {
  return 1;
}
</script>
<script type="application/ld+json">
function ignoredJson() {}
</script>
<script src="app.js">
function fromSrc() {}
</script>
<p>After the scripts.</p>
</body>
</html>
"#;

const PAGE_HTM: &str = r#"<!DOCTYPE html>
<html>
<body>
<p>Secondary page.</p>
<script>
function closeDrawer() {
  return false;
}
</script>
</body>
</html>
"#;

const NOTES_PY: &str = "\
def load_notes():\n\
    return \"notes\"\n\
";

fn source_line(text: &str, needle: &str) -> u64 {
    text.lines()
        .position(|line| line.contains(needle))
        .map(|i| (i + 1) as u64)
        .unwrap_or_else(|| panic!("{needle} missing from fixture"))
}

async fn app() -> axum::Router {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.auth.enabled = false;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    // Leak the temp dir for the process lifetime: the router keeps the data
    // dir path, and dropping `TempDir` at the end of this function would
    // delete it out from under the engine.
    std::mem::forget(dir);
    xerj_api::router::build_es_compat_router(state)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        Body::empty()
    } else {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn index_cfg(root: &Path, state_dir: &Path, url: &str, prefix: &str) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        endpoint_url: None,
        stub_globs: Vec::new(),
        url: url.to_owned(),
        api_key: None,
        api_key_file: None,
        workers: 1,
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
        xerj_url_note: None,
        pdf_timeout_secs: 30,
        bulk_mb: 1,
        bulk_timeout_secs: 30,
        snapshot_max_bytes: 64 << 30,
        prefix: prefix.to_owned(),
        replace_prefix: false,
        prefix_note: None,
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: xerj_autoindex::ignore_rules::IgnoreOptions::default(),
        max_file_bytes: 1 << 30,
        sample: 50,
        no_semantic: false,
        code_analyzer: Default::default(),
        brain: Some(format!("brain-{prefix}")),
        no_graph: true,
        max_minutes: 0,
        approve: None,
        max_index_bytes: None,
        budget_plan: None,
        drop_indexed: false,
        dry_run: false,
        walk_only: false,
        json: false,
        quiet: true,
        progress: xerj_autoindex::progress::ProgressMode::None,
        progress_interval: None,
        watch: false,
        label: None,
        debounce: Duration::from_millis(0),
        exclude_rels: Vec::new(),
        skip_corpus_data: false,
    }
}

async fn run_autoindex(root: &Path, state_dir: &Path, url: &str, prefix: &str) -> i32 {
    let root = root.to_owned();
    let state_dir = state_dir.to_owned();
    let url = url.to_owned();
    let prefix = prefix.to_owned();
    tokio::task::spawn_blocking(move || {
        let cfg = index_cfg(&root, &state_dir, &url, &prefix);
        match xerj_autoindex::run_index_report(cfg) {
            Ok((code, _)) => code,
            Err(error) => panic!("autoindex {prefix} failed: {error:#}"),
        }
    })
    .await
    .expect("autoindex task")
}

async fn prefix_docs(app: &axum::Router, prefix: &str) -> Vec<Value> {
    let (status, body) = call(
        app,
        "POST",
        &format!("/{prefix}-*/_search"),
        json!({
            "size": 200,
            "query": {"match_all": {}},
            "sort": [{"_id": "asc"}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search {prefix}-*: {body}");
    body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits array: {body}"))
        .iter()
        .map(|hit| hit["_source"].clone())
        .collect()
}

fn assert_symbol(docs: &[Value], name: &str, file: &str, line: u64) {
    let hit = docs.iter().find(|doc| {
        doc.get("name").and_then(Value::as_str) == Some(name)
            && doc
                .get("ax_path")
                .and_then(Value::as_str)
                .is_some_and(|path| path.ends_with(file))
    });
    let Some(doc) = hit else {
        panic!(
            "{file} has no symbol {name}: {}",
            serde_json::to_string(docs).unwrap()
        );
    };
    assert_eq!(doc["line"], json!(line), "{name} line: {doc}");
    assert_eq!(doc["kind"], json!("function"), "{name} kind: {doc}");
    assert_eq!(
        doc["language"],
        json!("javascript"),
        "{name} language: {doc}"
    );
    let code = doc["code"].as_str().unwrap_or("");
    assert!(code.contains(name), "{name} declaration: {code}");
    assert_eq!(
        doc["ax_locator"],
        json!(format!("code:{line}:{name}")),
        "{name} locator: {doc}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autoindex_finds_inline_script_functions_by_def_and_search() {
    let app = app().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, served).await.unwrap();
    });
    let url = format!("http://{addr}");

    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("page.html"), PAGE_HTML).unwrap();
    std::fs::write(root.path().join("drawer.htm"), PAGE_HTM).unwrap();
    std::fs::write(root.path().join("notes.py"), NOTES_PY).unwrap();

    let prefix = "htmljs";
    let code = run_autoindex(root.path(), state.path(), &url, prefix).await;
    assert_eq!(code, 0, "autoindex exit {code}");

    let docs = prefix_docs(&app, prefix).await;
    let open_line = source_line(PAGE_HTML, "function openDrawer");
    let panel_line = source_line(PAGE_HTML, "export function openPanel");
    let close_line = source_line(PAGE_HTM, "function closeDrawer");
    assert_symbol(&docs, "openDrawer", "page.html", open_line);
    assert_symbol(&docs, "openPanel", "page.html", panel_line);
    assert_symbol(&docs, "closeDrawer", "drawer.htm", close_line);

    let rendered = serde_json::to_string(&docs).unwrap();
    for absent in ["ignoredJson", "fromSrc"] {
        assert!(
            !rendered.contains(absent),
            "{absent} was indexed from a non-JS script: {rendered}"
        );
    }
    let prose = docs.iter().find(|doc| {
        doc.get("ax_path").and_then(Value::as_str) == Some("page.html")
            && doc
                .get("body")
                .and_then(Value::as_str)
                .is_some_and(|body| body.contains("The drawer holds the menu."))
    });
    let Some(prose) = prose else {
        panic!("page prose was not indexed: {rendered}");
    };
    let prose_body = prose["body"].as_str().unwrap();
    assert!(prose_body.contains("After the scripts."), "{prose_body}");
    assert!(
        !prose_body.contains("openDrawer") && !prose_body.contains("getElementById"),
        "script source was copied into the page body: {prose_body}"
    );

    let (status, mapping) = call(&app, "GET", &format!("/{prefix}-*/_mapping"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "mapping: {mapping}");
    let mapped = xerj_common::code_query::mapped_property_names(&mapping);
    let numeric = mapped
        .as_ref()
        .map(|_| xerj_common::code_query::numeric_property_names(&mapping));

    let def_body =
        xerj_autoindex::def::definition_query("openDrawer", None, None, 5, Some(&mapping));
    let (status, mut def_resp) =
        call(&app, "POST", &format!("/{prefix}-*/_search"), def_body).await;
    assert_eq!(status, StatusCode::OK, "def query: {def_resp}");
    let def_hits =
        xerj_autoindex::def::finalize_definition_response(&mut def_resp, "openDrawer", 5);
    assert!(
        !def_hits.is_empty(),
        "xerj def openDrawer returned nothing: {def_resp}"
    );
    let top = &def_hits[0];
    let top_src = &top["_source"];
    assert_eq!(top_src["name"], json!("openDrawer"), "def top hit: {top}");
    assert_eq!(top_src["ax_path"], json!("page.html"), "def top hit: {top}");
    assert_eq!(top_src["line"], json!(open_line), "def top hit: {top}");
    assert!(
        top_src["code"]
            .as_str()
            .is_some_and(|code| code.contains("openDrawer")),
        "def declaration: {top}"
    );

    let search_body = xerj_common::code_query::search_query_body_for_mapping(
        "openDrawer",
        5,
        mapped.as_ref(),
        numeric.as_ref(),
    );
    let (status, search_resp) =
        call(&app, "POST", &format!("/{prefix}-*/_search"), search_body).await;
    assert_eq!(status, StatusCode::OK, "search query: {search_resp}");
    let search_hits = search_resp["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no search hits: {search_resp}"));
    // `xerj search` does not request `name` in `_source`. The hit is the
    // declaration: title, kind, code, and the file line.
    assert!(
        search_hits.iter().any(|hit| {
            let src = &hit["_source"];
            src["title"] == json!("openDrawer")
                && src["kind"] == json!("function")
                && src["ax_path"] == json!("page.html")
                && src["line"] == json!(open_line)
                && src["code"]
                    .as_str()
                    .is_some_and(|code| code.contains("function openDrawer"))
        }),
        "xerj search did not return openDrawer at page.html:{open_line}: {search_resp}"
    );

    let prose_body = xerj_common::code_query::search_query_body_for_mapping(
        "drawer holds the menu",
        5,
        mapped.as_ref(),
        numeric.as_ref(),
    );
    let (status, prose_resp) =
        call(&app, "POST", &format!("/{prefix}-*/_search"), prose_body).await;
    assert_eq!(status, StatusCode::OK, "prose search: {prose_resp}");
    let prose_hits = prose_resp["hits"]["hits"].as_array().unwrap();
    assert!(
        prose_hits
            .iter()
            .any(|hit| hit["_source"]["ax_path"] == json!("page.html")),
        "page text was not searchable: {prose_resp}"
    );

    let py_body =
        xerj_autoindex::def::definition_query("load_notes", None, None, 5, Some(&mapping));
    let (status, mut py_resp) = call(&app, "POST", &format!("/{prefix}-*/_search"), py_body).await;
    assert_eq!(status, StatusCode::OK, "python def: {py_resp}");
    let py_hits = xerj_autoindex::def::finalize_definition_response(&mut py_resp, "load_notes", 5);
    assert!(
        py_hits.iter().any(|hit| {
            hit["_source"]["name"] == json!("load_notes")
                && hit["_source"]["ax_path"] == json!("notes.py")
        }),
        "python def regressed: {py_resp}"
    );
}
