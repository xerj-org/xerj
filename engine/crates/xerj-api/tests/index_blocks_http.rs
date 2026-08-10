//! Index blocks, from the outside — the three defects reported against the
//! `_block` / `index.blocks.*` surface, each pinned by a test that fails on the
//! pre-fix tree.
//!
//! 1. **A block could be set but never removed.** `PUT /{index}/_settings`
//!    wrote only the display-side `engine.index_settings` map, which nothing
//!    enforces; enforcement reads `Index::settings`. So the 200
//!    `{"acknowledged": true}` was a lie and the index stayed blocked. There was
//!    no `DELETE /{index}/_block/{block}` either — the route was PUT-only — so
//!    the block outlived every API on offer.
//!
//! 2. **`read_only_allow_delete` was inverted.** It set `blocks.read`, which
//!    denies *searches* and leaves *writes* running: precisely backwards.
//!    Elasticsearch's own definition is a WRITE-level block that answers 429
//!    (`IndexMetadata.INDEX_READ_ONLY_ALLOW_DELETE_BLOCK`), and its docs are
//!    explicit that document deletes are denied too — the "allow delete" is
//!    *index* deletion, which frees disk immediately, unlike deleting documents
//!    which costs disk first
//!    (`docs/reference/elasticsearch/index-settings/index-block.md:29-34`).
//!    `read_only` had the same inversion: it forced `blocks.read` on as well,
//!    when ES means "readable, not writable".
//!
//! 3. **A blocked bulk answered 500.** Three inline `match`es in `bulk.rs`
//!    listed the errors their authors had in mind and sent everything else to
//!    `_ => 500`, so a plain write block — a 403 in `XerjError::http_status()`
//!    and a `cluster_block_exception` in ES — was reported to clients as a
//!    server fault.
//!
//! Elasticsearch is referenced for semantics only. It is AGPL-3.0/SSPL-1.0/
//! Elastic-2.0 licensed and no code from it is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

/// A one-index app with a single document already visible.
async fn app_with_index(name: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);

    state
        .engine
        .create_index(name, xerj_common::types::Schema::empty())
        .expect("create_index");
    let idx = state.engine.get_index(name).expect("get_index");
    idx.index_document(Some("seed".into()), json!({ "value": 1 }))
        .await
        .expect("index_document");
    idx.refresh().await.expect("refresh");

    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn put_json(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Request::put(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn put_empty(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::put(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn delete(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::delete(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn bulk(app: &axum::Router, path: &str, ndjson: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::post(path)
            .header("content-type", "application/x-ndjson")
            .body(Body::from(ndjson.to_owned()))
            .expect("request"),
    )
    .await
}

/// Index one document; returns the status the write got.
async fn write_doc(app: &axum::Router, index: &str, id: &str) -> StatusCode {
    put_json(app, &format!("/{index}/_doc/{id}"), json!({ "value": 2 }))
        .await
        .0
}

// ── 1. A block must be removable ─────────────────────────────────────────────

#[tokio::test]
async fn a_write_block_can_be_cleared_through_the_settings_api() {
    let (app, _dir) = app_with_index("clearsettings").await;

    let (status, _) = put_empty(&app, "/clearsettings/_block/write").await;
    assert_eq!(status, StatusCode::OK, "setting the block should succeed");
    assert_eq!(
        write_doc(&app, "clearsettings", "1").await,
        StatusCode::FORBIDDEN,
        "a write block must deny writes with 403"
    );

    let (status, body) = put_json(
        &app,
        "/clearsettings/_settings",
        json!({ "index": { "blocks": { "write": false } } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings update: {body}");

    assert_eq!(
        write_doc(&app, "clearsettings", "1").await,
        StatusCode::CREATED,
        "PUT _settings answered 'acknowledged' but the block was still enforced — \
         the settings write never reached the engine state the write guard reads"
    );
}

#[tokio::test]
async fn a_write_block_can_be_cleared_through_the_dotted_settings_form() {
    let (app, _dir) = app_with_index("cleardotted").await;

    put_empty(&app, "/cleardotted/_block/write").await;
    assert_eq!(
        write_doc(&app, "cleardotted", "1").await,
        StatusCode::FORBIDDEN
    );

    // ES clients send index settings in dotted form at least as often as nested.
    let (status, body) = put_json(
        &app,
        "/cleardotted/_settings",
        json!({ "index.blocks.write": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings update: {body}");

    assert_eq!(
        write_doc(&app, "cleardotted", "1").await,
        StatusCode::CREATED,
        "the dotted spelling of index.blocks.write must clear the block too"
    );
}

#[tokio::test]
async fn a_write_block_can_be_set_through_the_settings_api() {
    let (app, _dir) = app_with_index("setsettings").await;

    // The inverse direction of the same defect: setting a block through
    // _settings also has to reach the engine, not just the display map.
    let (status, body) = put_json(
        &app,
        "/setsettings/_settings",
        json!({ "index": { "blocks": { "write": true } } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings update: {body}");

    assert_eq!(
        write_doc(&app, "setsettings", "1").await,
        StatusCode::FORBIDDEN,
        "index.blocks.write set through _settings must actually deny writes"
    );
}

/// A block declared in the **create** body has to be enforced too, in any
/// spelling. `PUT /{index}` forwards its `settings` blob to the engine
/// verbatim, so an index created with the dotted (and string-valued) form used
/// to store a flag that the write guard's nested-only lookup never saw.
#[tokio::test]
async fn a_block_declared_at_create_time_is_enforced_in_every_spelling() {
    for (n, settings) in [
        json!({ "index": { "blocks": { "write": true } } }),
        json!({ "index.blocks.write": true }),
        json!({ "index.blocks.write": "true" }),
        json!({ "index": { "blocks.write": "true" } }),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();
        config.storage.wal_sync = xerj_common::config::WalSync::Async;
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        let state = xerj_api::state::AppState::new(config, engine, metrics);
        let app = xerj_api::router::build_es_compat_router(state);

        let name = format!("created{n}");
        let (status, body) =
            put_json(&app, &format!("/{name}"), json!({ "settings": settings })).await;
        assert_eq!(status, StatusCode::OK, "create {settings}: {body}");

        assert_eq!(
            write_doc(&app, &name, "1").await,
            StatusCode::FORBIDDEN,
            "a create-time block written as {settings} must deny writes"
        );

        // …and it must still be removable, which means the stray spelling has
        // to be cleared alongside the canonical one.
        let (status, _) = delete(&app, &format!("/{name}/_block/write")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            write_doc(&app, &name, "1").await,
            StatusCode::CREATED,
            "a create-time block written as {settings} must be removable"
        );
    }
}

#[tokio::test]
async fn the_block_api_has_a_delete_verb() {
    let (app, _dir) = app_with_index("blockdelete").await;

    put_empty(&app, "/blockdelete/_block/write").await;
    assert_eq!(
        write_doc(&app, "blockdelete", "1").await,
        StatusCode::FORBIDDEN
    );

    let (status, body) = delete(&app, "/blockdelete/_block/write").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "DELETE /{{index}}/_block/{{block}} must exist — the route was PUT-only, \
         so this answered 405 and the block had no removal path: {body}"
    );
    assert_eq!(body["acknowledged"], json!(true), "body: {body}");
    assert_eq!(body["indices"][0]["blocked"], json!(false), "body: {body}");

    assert_eq!(
        write_doc(&app, "blockdelete", "1").await,
        StatusCode::CREATED,
        "writes must be admitted again once the block is deleted"
    );
}

#[tokio::test]
async fn deleting_an_unknown_block_name_is_a_client_error() {
    let (app, _dir) = app_with_index("blockvalidate").await;

    let (status, _) = delete(&app, "/blockvalidate/_block/not_a_block").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the DELETE verb must validate the block name like the PUT verb does"
    );
}

#[tokio::test]
async fn a_block_is_visible_in_the_settings_it_is_cleared_through() {
    let (app, _dir) = app_with_index("blockvisible").await;

    put_empty(&app, "/blockvisible/_block/write").await;
    let (_, body) = get(&app, "/blockvisible/_settings").await;
    assert_eq!(
        body["blockvisible"]["settings"]["index"]["blocks"]["write"],
        json!(true),
        "a block set through _block must show up in GET _settings — otherwise the \
         only API that reports blocks disagrees with the one that enforces them: \
         {body}"
    );

    delete(&app, "/blockvisible/_block/write").await;
    let (_, body) = get(&app, "/blockvisible/_settings").await;
    assert!(
        body["blockvisible"]["settings"]["index"]["blocks"]["write"].is_null(),
        "a cleared block must disappear from GET _settings: {body}"
    );
}

// ── 2. read_only_allow_delete / read_only were inverted ──────────────────────

#[tokio::test]
async fn read_only_allow_delete_blocks_writes_and_not_reads() {
    let (app, _dir) = app_with_index("floodstage").await;

    let (status, _) = put_empty(&app, "/floodstage/_block/read_only_allow_delete").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get(&app, "/floodstage/_search").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "read_only_allow_delete is a WRITE-level block; searches stay served. \
         The pre-fix code set index.blocks.read instead: {body}"
    );

    assert_eq!(
        write_doc(&app, "floodstage", "1").await,
        StatusCode::TOO_MANY_REQUESTS,
        "read_only_allow_delete must deny writes, with the 429 ES answers for the \
         flood-stage block (not the 403 the explicit blocks get)"
    );
}

#[tokio::test]
async fn read_only_allow_delete_denies_document_deletes() {
    let (app, _dir) = app_with_index("floodstagedel").await;

    put_empty(&app, "/floodstagedel/_block/read_only_allow_delete").await;

    let (status, body) = delete(&app, "/floodstagedel/_doc/seed").await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "deleting *documents* under this block is denied — it grows the index \
         before it shrinks it, which is the opposite of what a node out of disk \
         needs. Only deleting the index itself stays allowed: {body}"
    );
}

#[tokio::test]
async fn read_only_allow_delete_still_allows_deleting_the_index() {
    let (app, _dir) = app_with_index("floodstagedrop").await;

    put_empty(&app, "/floodstagedrop/_block/read_only_allow_delete").await;

    let (status, body) = delete(&app, "/floodstagedrop").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "dropping the index is the one write this block exists to permit: {body}"
    );
}

#[tokio::test]
async fn read_only_blocks_writes_and_not_reads() {
    let (app, _dir) = app_with_index("readonly").await;

    put_empty(&app, "/readonly/_block/read_only").await;

    let (status, body) = get(&app, "/readonly/_search").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "read_only means readable-but-not-writable; the pre-fix alias expansion \
         forced index.blocks.read on as well: {body}"
    );
    assert_eq!(
        write_doc(&app, "readonly", "1").await,
        StatusCode::FORBIDDEN,
        "read_only must still deny writes, with 403"
    );
}

#[tokio::test]
async fn read_only_is_cleared_by_clearing_read_only() {
    let (app, _dir) = app_with_index("readonlyclear").await;

    put_empty(&app, "/readonlyclear/_block/read_only").await;
    assert_eq!(
        write_doc(&app, "readonlyclear", "1").await,
        StatusCode::FORBIDDEN
    );

    delete(&app, "/readonlyclear/_block/read_only").await;

    assert_eq!(
        write_doc(&app, "readonlyclear", "1").await,
        StatusCode::CREATED,
        "clearing read_only must lift the write denial — with the old alias \
         expansion it left an independent blocks.write behind that nothing \
         addressed by name could reach"
    );
}

#[tokio::test]
async fn an_explicit_read_block_still_denies_reads() {
    let (app, _dir) = app_with_index("readblocked").await;

    put_empty(&app, "/readblocked/_block/read").await;

    let (status, _) = get(&app, "/readblocked/_search").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "index.blocks.read is the block that denies reads, and it must keep doing so"
    );
}

// ── 3. A blocked bulk is 403, not 500 ────────────────────────────────────────

#[tokio::test]
async fn bulk_auto_id_against_a_write_blocked_index_is_403_per_item() {
    let (app, _dir) = app_with_index("bulkauto").await;

    put_empty(&app, "/bulkauto/_block/write").await;

    // No `_id` — this is the auto-ID path, which batches through
    // `index_batch_turbo_raw` and fails as a whole batch.
    let (status, body) = bulk(&app, "/bulkauto/_bulk", "{\"index\":{}}\n{\"value\":2}\n").await;
    assert_eq!(status, StatusCode::OK, "the bulk envelope itself is 200");
    assert_eq!(body["errors"], json!(true), "body: {body}");
    assert_eq!(
        body["items"][0]["index"]["status"],
        json!(403),
        "a write block is a client error (cluster_block_exception, 403), but the \
         inline whole-batch match in bulk.rs sent it to `_ => 500`: {body}"
    );
}

#[tokio::test]
async fn bulk_with_an_explicit_id_against_a_write_blocked_index_is_403_per_item() {
    let (app, _dir) = app_with_index("bulkid").await;

    put_empty(&app, "/bulkid/_block/write").await;

    let (status, body) = bulk(
        &app,
        "/bulkid/_bulk",
        "{\"index\":{\"_id\":\"9\"}}\n{\"value\":2}\n",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["errors"], json!(true), "body: {body}");
    assert_eq!(
        body["items"][0]["index"]["status"],
        json!(403),
        "body: {body}"
    );
}

#[tokio::test]
async fn bulk_against_a_flood_stage_blocked_index_stays_429_per_item() {
    let (app, _dir) = app_with_index("bulkflood").await;

    put_empty(&app, "/bulkflood/_block/read_only_allow_delete").await;

    let (_, body) = bulk(&app, "/bulkflood/_bulk", "{\"index\":{}}\n{\"value\":2}\n").await;
    assert_eq!(
        body["items"][0]["index"]["status"],
        json!(429),
        "the flood-stage block keeps its retryable 429 — routing every bulk error \
         through one mapper must not flatten that into the 403 the other blocks \
         get: {body}"
    );
}

#[tokio::test]
async fn a_bulk_delete_against_a_write_blocked_index_is_403_per_item() {
    let (app, _dir) = app_with_index("bulkdel").await;

    put_empty(&app, "/bulkdel/_block/write").await;

    let (_, body) = bulk(&app, "/bulkdel/_bulk", "{\"delete\":{\"_id\":\"seed\"}}\n").await;
    assert_eq!(
        body["items"][0]["delete"]["status"],
        json!(403),
        "body: {body}"
    );
}

// ── Round trip ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn block_set_and_clear_round_trips_across_every_route() {
    let (app, _dir) = app_with_index("roundtrip").await;

    for (n, (path, expected_denied)) in [
        ("/roundtrip/_block/write", StatusCode::FORBIDDEN),
        ("/roundtrip/_block/read_only", StatusCode::FORBIDDEN),
        (
            "/roundtrip/_block/read_only_allow_delete",
            StatusCode::TOO_MANY_REQUESTS,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        // A fresh id each pass, so the admitted write is unambiguously a 201
        // create rather than a 200 update of the previous pass's document.
        let id = format!("rt{n}");
        put_empty(&app, path).await;
        assert_eq!(
            write_doc(&app, "roundtrip", &id).await,
            expected_denied,
            "{path} should deny the write"
        );
        delete(&app, path).await;
        assert_eq!(
            write_doc(&app, "roundtrip", &id).await,
            StatusCode::CREATED,
            "{path} should be fully removable"
        );
    }
}
