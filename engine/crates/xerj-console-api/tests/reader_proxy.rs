//! What the Console's Reader relies on from the session-authenticated
//! data-sources proxy.
//!
//! The Reader (`xerj-ux/src/ux/reader-view.js`) shows other people's documents.
//! A signed-in operator holds a Console session, not an engine API key, so on
//! an auth-enabled engine (the default) it can reach documents ONLY through
//! this proxy. Two behaviours it depends on are pinned here:
//!
//! 1. **Highlights pass through, delimited by the caller's tags, and the
//!    fragment text is NOT HTML-escaped.** The SPA asks for two private-use
//!    code points as delimiters and rebuilds each fragment from text nodes
//!    (`safe-dom.js#highlightChildren`); it never parses a fragment as markup.
//!    If the proxy dropped the tags the operator would read literal `<em>`,
//!    and if anyone ever "helpfully" pre-escaped fragments the reader would
//!    show `&lt;` — so both halves are asserted.
//! 2. **`/fields` says which field is `semantic_text`.** Its `type` reads
//!    `text`; the `semantic` flag is how the console picks the field that
//!    `semantic` / `hybrid` queries run against without reading `_mapping`.

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_common::types::{EmbeddingConfig, FieldConfig, FieldType, Schema};
use xerj_console_api::{
    auth::{sessions, store},
    state::ClusterMode,
    xerj_console_router, ConsoleState,
};
use xerj_engine::Engine;

const PRE: &str = "\u{e000}";
const POST: &str = "\u{e001}";
/// An email subject as hostile as they come. It must travel through the
/// proxy byte-for-byte: escaping is the renderer's job, exactly once.
const HOSTILE: &str = "invoice <img src=x onerror=alert(1)> overdue";

struct TestApp {
    router: Router,
    cookie: String,
    _dir: TempDir,
}

async fn boot() -> TestApp {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .unwrap();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );
    let user = store::User {
        id: "owner-test".to_string(),
        email: "owner@example.com".to_string(),
        display_name: "Owner".to_string(),
        role: "owner".to_string(),
        status: store::UserStatus::Active,
        created_at: xerj_console_api::time::now_iso(),
        last_seen_at: Some(xerj_console_api::time::now_iso()),
    };
    store::upsert_user(&engine, &user).await.unwrap();
    let (_session, signed) = sessions::mint_session(&state, &user.id, "passkey", None, None)
        .await
        .unwrap();

    // An inbox the way autoindex maps one: `body` is semantic_text (a text
    // field with an embedding config), the headers are keywords.
    let mut schema = Schema::empty();
    let mut body = FieldConfig::new("body", FieldType::Text);
    body.embedding = Some(EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("body_vector".to_string()),
    });
    schema.add_field(body).unwrap();
    schema
        .add_field(FieldConfig::new("email_subject", FieldType::Text))
        .unwrap();
    schema
        .add_field(FieldConfig::new("email_from", FieldType::Keyword))
        .unwrap();
    engine
        .create_index("ax-inbox", schema)
        .expect("create index");
    let idx = engine.get_index("ax-inbox").unwrap();
    idx.create_document(
        "m1".into(),
        json!({
            "email_subject": HOSTILE,
            "email_from": "mallory@example.com",
            "body": "please find the overdue invoice attached",
        }),
    )
    .await
    .expect("index doc");

    TestApp {
        router: xerj_console_router(state),
        cookie: format!("xerj_session={signed}"),
        _dir: dir,
    }
}

async fn call(app: &TestApp, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", &app.cookie)
        .header("content-type", "application/json")
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const SEARCH: &str =
    "/_xerj-console/api/v1/data-sources/connections/built-in/indices/ax-inbox/search";

/// Exactly the `highlight` block `safe-dom.js#highlightRequest` builds: the
/// ES plural-array spelling AND the singular one this proxy's parser reads.
fn reader_highlight() -> Value {
    json!({
        "pre_tags": [PRE], "post_tags": [POST],
        "pre_tag": PRE, "post_tag": POST,
        "fields": { "email_subject": { "fragment_size": 160, "number_of_fragments": 2 } }
    })
}

#[tokio::test]
async fn highlights_pass_through_with_the_callers_tags_and_unescaped_text() {
    let app = boot().await;
    let (status, body) = call(
        &app,
        "POST",
        SEARCH,
        Some(json!({
            "query": { "match": { "email_subject": "invoice" } },
            "highlight": reader_highlight(),
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let hit = &body["hits"]["hits"][0];
    assert_eq!(hit["_id"], "m1");
    assert_eq!(hit["_source"]["email_subject"], HOSTILE);

    let frags = hit["highlight"]["email_subject"]
        .as_array()
        .unwrap_or_else(|| panic!("the proxy dropped the highlight block: {hit}"));
    let frag = frags[0].as_str().unwrap();
    assert!(
        frag.contains(&format!("{PRE}invoice{POST}")),
        "match must be wrapped in the caller's tags, got {frag:?}"
    );
    assert!(
        !frag.contains("<em>"),
        "the proxy fell back to the default <em> tags: {frag:?}"
    );
    // Raw document text: the markup characters arrive as themselves. The SPA
    // turns a fragment into text nodes; a pre-escaped fragment would display
    // as `&lt;img…`, and a fragment treated as HTML would be an XSS.
    assert!(
        frag.contains("<img src=x onerror=alert(1)>"),
        "fragment text must not be HTML-escaped by the engine or the proxy: {frag:?}"
    );
}

#[tokio::test]
async fn no_highlight_block_unless_one_was_requested() {
    let app = boot().await;
    let (status, body) = call(
        &app,
        "POST",
        SEARCH,
        Some(json!({ "query": { "match": { "email_subject": "invoice" } } })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["hits"]["hits"][0].get("highlight").is_none(), "{body}");
}

#[tokio::test]
async fn fields_flag_the_semantic_text_field() {
    let app = boot().await;
    let (status, body) = call(
        &app,
        "GET",
        "/_xerj-console/api/v1/data-sources/connections/built-in/indices/ax-inbox/fields",
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let fields = body["data"]["fields"].as_array().expect("fields array");
    let by_name = |n: &str| -> &Value {
        fields
            .iter()
            .find(|f| f["name"] == n)
            .unwrap_or_else(|| panic!("no field {n} in {body}"))
    };
    assert_eq!(by_name("body")["type"], "text");
    assert_eq!(by_name("body")["semantic"], true);
    assert_eq!(by_name("email_subject")["semantic"], false);
    assert_eq!(by_name("email_from")["type"], "keyword");
    assert_eq!(by_name("email_from")["semantic"], false);
}

#[tokio::test]
async fn the_reader_proxy_needs_a_session() {
    let app = boot().await;
    let req = Request::builder()
        .method("POST")
        .uri(SEARCH)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "query": { "match_all": {} } }).to_string(),
        ))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        !String::from_utf8_lossy(&bytes).contains("mallory"),
        "a 401 must carry no document data"
    );
}
