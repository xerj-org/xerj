//! Issue #203: index templates, legacy templates, component templates,
//! ingest pipelines, data streams and ILM policies must survive a restart.
//!
//! Before the fix each of these lived only in an in-memory `DashMap` on
//! `Engine`. `Engine::new` restored index directories, `es_mapping.json`,
//! API keys and aliases — nothing else — so a `PUT _index_template/logs`
//! answered `200 {"acknowledged": true}` and then evaporated on the next
//! boot. The operator got no error, and the next index that should have
//! matched the template was created without it.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn config_for(dir: &std::path::Path) -> xerj_common::config::Config {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config
}

/// Build a fresh app over `dir`. Calling this twice (with the first engine
/// dropped in between) is a restart: nothing is handed over in memory, the
/// second boot sees only what reached the disk.
fn app_over(dir: &std::path::Path) -> axum::Router {
    let config = config_for(dir);
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    xerj_api::router::build_es_compat_router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
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

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
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

/// Write one of each managed object. Kept separate from the assertions so
/// the same fixture can be replayed after a restart.
async fn write_all_the_config(app: &axum::Router) {
    let (st, _) = put_json(
        app,
        "/_index_template/logs",
        json!({
            "index_patterns": ["logs-*"],
            "priority": 200,
            "template": {
                "settings": { "number_of_replicas": 0 },
                "mappings": { "properties": { "host": { "type": "keyword" } } }
            }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "PUT _index_template");

    let (st, _) = put_json(
        app,
        "/_template/legacy-logs",
        json!({
            "index_patterns": ["legacy-*"],
            "settings": { "number_of_shards": 1 },
            "mappings": { "properties": { "msg": { "type": "text" } } }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "PUT _template");

    let (st, _) = put_json(
        app,
        "/_component_template/base-settings",
        json!({ "template": { "settings": { "number_of_replicas": 1 } } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "PUT _component_template");

    let (st, _) = put_json(
        app,
        "/_ingest/pipeline/tagger",
        json!({
            "description": "tag every doc",
            "processors": [ { "set": { "field": "tagged", "value": "yes" } } ]
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "PUT _ingest/pipeline");

    let (st, _) = put_json(
        app,
        "/_ilm/policy/hot-warm",
        json!({ "policy": { "phases": { "hot": { "actions": {} } } } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "PUT _ilm/policy");

    let (st, _) = put_json(app, "/_data_stream/metrics-app", json!({})).await;
    assert_eq!(st, StatusCode::OK, "PUT _data_stream");
}

/// The read side of each managed object.
const READ_BACK: [&str; 6] = [
    "/_index_template/logs",
    "/_template/legacy-logs",
    "/_component_template/base-settings",
    "/_ingest/pipeline/tagger",
    "/_ilm/policy/hot-warm",
    "/_data_stream/metrics-app",
];

/// Read every managed object back, asserting each is present, and return the
/// exact responses so a later boot can be compared against them verbatim.
///
/// Comparing whole bodies is deliberate: it catches a partial restore (a
/// template that comes back without its mappings, a data stream that comes
/// back at generation 1) that a spot-check on one field would wave through.
async fn read_back_everything(app: &axum::Router, when: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for path in READ_BACK {
        let (st, body) = get(app, path).await;
        assert_eq!(st, StatusCode::OK, "GET {path} {when}: {body}");
        out.push(body);
    }

    // Enough content assertions to prove the fixture really was written, so
    // an all-empty "equal" comparison cannot pass for a restore.
    let tmpl = &out[0]["index_templates"][0]["index_template"];
    assert_eq!(tmpl["index_patterns"][0], "logs-*", "{when}: {}", out[0]);
    assert_eq!(tmpl["priority"], 200, "{when}: {}", out[0]);
    assert_eq!(
        tmpl["template"]["mappings"]["properties"]["host"]["type"], "keyword",
        "{when}: {}",
        out[0]
    );
    assert_eq!(
        out[1]["legacy-logs"]["index_patterns"][0], "legacy-*",
        "{when}: {}",
        out[1]
    );
    assert_eq!(
        out[2]["component_templates"][0]["component_template"]["template"]["settings"]
            ["number_of_replicas"],
        1,
        "{when}: {}",
        out[2]
    );
    assert_eq!(
        out[5]["data_streams"][0]["name"], "metrics-app",
        "{when}: {}",
        out[5]
    );
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn templates_pipelines_streams_and_policies_survive_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");

    let before = {
        let app = app_over(dir.path());
        write_all_the_config(&app).await;
        // Sanity: it is all readable *before* the restart, so a failure
        // below is about durability and not about the write path.
        read_back_everything(&app, "before restart").await
    }; // engine dropped — the node.lock is released, nothing is handed over

    let app = app_over(dir.path());
    let after = read_back_everything(&app, "after restart").await;

    for (path, (b, a)) in READ_BACK.iter().zip(before.iter().zip(after.iter())) {
        assert_eq!(
            b, a,
            "GET {path} answered differently after a restart\nbefore: {b}\nafter:  {a}"
        );
    }
}

/// A restored ingest pipeline must actually *run*, not merely round-trip
/// through `GET /_ingest/pipeline`. Storing the raw JSON without
/// recompiling it would leave `?pipeline=tagger` accepted and ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_pipeline_still_transforms_documents() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let app = app_over(dir.path());
        write_all_the_config(&app).await;
    }

    let app = app_over(dir.path());
    let (st, body) = send(
        &app,
        Request::post("/_ingest/pipeline/tagger/_simulate")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "docs": [ { "_source": { "host": "a" } } ] }).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "simulate after restart: {body}");
    assert_eq!(
        body["docs"][0]["doc"]["_source"]["tagged"], "yes",
        "the restored pipeline must still execute its processors: {body}"
    );
}

/// Deletes must be durable too — a deleted template that comes back on the
/// next boot is the same class of surprise as one that disappears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_template_stays_deleted_across_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let app = app_over(dir.path());
        write_all_the_config(&app).await;
        let (st, _) = delete(&app, "/_index_template/logs").await;
        assert_eq!(st, StatusCode::OK, "DELETE _index_template");
        let (st, _) = delete(&app, "/_ingest/pipeline/tagger").await;
        assert_eq!(st, StatusCode::OK, "DELETE _ingest/pipeline");
    }

    let app = app_over(dir.path());
    let (st, _) = get(&app, "/_index_template/logs").await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a deleted index template must not resurrect on restart"
    );
    let (st, _) = get(&app, "/_ingest/pipeline/tagger").await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a deleted pipeline must not resurrect on restart"
    );
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

/// A rollover interrupted between "backing index created" and "generation
/// written" must not wedge the stream.
///
/// `rollover_data_stream` creates `.ds-<name>-00000N` on disk first and only
/// then flushes the new generation, so a `kill -9` in that window leaves the
/// index present and the document still saying N-1. Restoring N-1 verbatim
/// makes the next rollover recompute the *same* name, which `create_index`
/// refuses as `index_already_exists` — and it does so on every subsequent
/// attempt, so the stream can never roll again.
///
/// The crash window is reproduced exactly rather than approximated: the
/// backing index is left on disk and the persisted generation is rewound by
/// hand, which is byte-for-byte the state a crash at that instant leaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollover_interrupted_before_its_flush_does_not_wedge_the_stream() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let app = app_over(dir.path());
        let (st, _) = put_json(&app, "/_data_stream/metrics-app", json!({})).await;
        assert_eq!(st, StatusCode::OK, "PUT _data_stream");
        let (st, body) = post(&app, "/metrics-app/_rollover", json!({})).await;
        assert_eq!(st, StatusCode::OK, "rollover: {body}");
    }

    // Rewind only the generation, exactly as a crash between `create_index`
    // and `flush_cluster_state` would have left it: `.ds-metrics-app-000002`
    // is on disk, the document still reads generation 1.
    let state_path = dir.path().join("cluster_state.json");
    let mut state: Value =
        serde_json::from_slice(&std::fs::read(&state_path).expect("read state")).expect("parse");
    let ds = &mut state["data_streams"]["metrics-app"];
    ds["generation"] = json!(1);
    ds["backing_indices"] = json!([".ds-metrics-app-000001"]);
    std::fs::write(&state_path, serde_json::to_vec(&state).expect("serialize")).expect("write");
    assert!(
        dir.path().join(".ds-metrics-app-000002").is_dir(),
        "fixture is wrong: the generation-2 backing index should still be on disk"
    );

    let app = app_over(dir.path());
    let (st, body) = post(&app, "/metrics-app/_rollover", json!({})).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "a stream recovered from an interrupted rollover must still roll: {body}"
    );
    assert!(
        body.to_string().contains(".ds-metrics-app-000003"),
        "the next rollover must skip past the backing index the crash left \
         behind, not reissue it: {body}"
    );
}

/// A corrupt `cluster_state.json` must not be destroyed by the next write.
///
/// The node boots with empty maps and logs the failure, so without this the
/// first `PUT /_index_template/...` rewrote the file — taking whatever was
/// still legible in it along. Hand-recovery out of a damaged document is the
/// operator's last option; it should not be silently removed. The write is
/// refused (the original stays where it is) *and* a copy is kept, because
/// un-wedging the node means moving the original aside by hand.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corrupt_cluster_state_is_preserved_before_it_is_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let app = app_over(dir.path());
        write_all_the_config(&app).await;
    }

    // Truncate mid-document — the shape a torn write would have had, if the
    // rewrite were not atomic, and the shape media damage leaves.
    let state_path = dir.path().join("cluster_state.json");
    let original = std::fs::read_to_string(&state_path).expect("read state");
    let truncated = &original[..original.len() / 2];
    std::fs::write(&state_path, truncated).expect("write truncated");

    let app = app_over(dir.path());
    let (st, _) = get(&app, "/_index_template/logs").await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a corrupt document cannot be restored — this is the honest outcome"
    );

    // The write that would have destroyed the evidence.
    let (st, body) = put_json(
        &app,
        "/_index_template/fresh",
        json!({ "index_patterns": ["fresh-*"] }),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a management write on top of a state that did not load must be \
         refused, not acknowledged: {body}"
    );

    let salvage = dir.path().join("cluster_state.corrupt.json");
    assert!(
        salvage.is_file(),
        "the corrupt document must be kept for recovery"
    );
    assert_eq!(
        std::fs::read_to_string(&salvage).expect("read salvage"),
        truncated,
        "the preserved copy must be the bytes that were found, unmodified"
    );
    assert_eq!(
        std::fs::read_to_string(&state_path).expect("read state"),
        truncated,
        "the live document must be left exactly as it was found — the copy is \
         a convenience, not a licence to overwrite the original"
    );

    // Moving the damaged file aside is the documented recovery, and it must
    // actually work: the next boot loads cleanly and writes are accepted again.
    drop(app);
    std::fs::remove_file(&state_path).expect("operator moves the file aside");
    let app = app_over(dir.path());
    let (st, body) = put_json(
        &app,
        "/_index_template/fresh",
        json!({ "index_patterns": ["fresh-*"] }),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "PUT after the operator recovered: {body}"
    );
    assert!(
        std::fs::read_to_string(&state_path)
            .expect("read state")
            .contains("fresh-*"),
        "the live document must have moved on once the node loaded cleanly"
    );
}

/// An **intact but unreadable** `cluster_state.json` must survive untouched,
/// and the node must refuse management writes until a boot can load it.
///
/// This is the worse half of the corrupt case and it was missed on the first
/// pass: the bytes are perfectly good, only the `read(2)` failed — a uid
/// change on a container volume, a backup tool's chmod, `EIO`, `EMFILE` at
/// boot. The maps come up empty, and `write_file_atomic` renames
/// `cluster_state.tmp` over the target; `rename(2)` needs write permission on
/// the *directory*, not on the file, so a `PUT` answering
/// `{"acknowledged": true}` unlinked a fully recoverable document. Measured
/// on this test before the fix, with `chmod 000` for the boot only:
///
/// ```text
/// PUT /_index_template/fresh -> 200 {"acknowledged":true}
/// cluster_state.corrupt.json exists: false
/// live file still contains "logs-*": false     <-- the operator's templates
/// ```
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_cluster_state_is_never_overwritten() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let app = app_over(dir.path());
        write_all_the_config(&app).await;
    }

    let state_path = dir.path().join("cluster_state.json");
    let original = std::fs::read_to_string(&state_path).expect("read state");
    assert!(original.contains("logs-*"), "fixture must be on disk");

    // Unreadable for the boot only; restored immediately afterwards so the
    // assertions below read the file the node was left holding, and so a
    // failure cannot be blamed on the test's own permissions.
    std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000");
    let app = app_over(dir.path());
    std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o644))
        .expect("chmod 644");

    let (st, _) = get(&app, "/_index_template/logs").await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a document that could not be read cannot be restored — that part is \
         honest already"
    );

    // The write that used to destroy it.
    let (st, body) = put_json(
        &app,
        "/_index_template/fresh",
        json!({ "index_patterns": ["fresh-*"] }),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a management write must be refused while the persisted state is \
         unloaded, not acknowledged: {body}"
    );
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be loaded at boot"),
        "the 500 must say why, so the operator can act on it: {body}"
    );

    assert_eq!(
        std::fs::read_to_string(&state_path).expect("read state"),
        original,
        "the intact document must still be on disk, byte for byte"
    );
    assert!(
        !dir.path().join("cluster_state.tmp").exists(),
        "a refused write must not even stage a temp file"
    );

    // And the refusal is not a one-off: the in-memory map must not be left
    // holding a change that was reported as failed.
    let (st, _) = get(&app, "/_index_template/fresh").await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a refused PUT must roll its in-memory change back"
    );
    let (st, _) = delete(&app, "/_ilm/policy/hot-warm").await;
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "deletes are writes too — they must be refused as well"
    );

    // Recovery path: the file was readable all along, so a plain restart is
    // the whole fix.
    drop(app);
    let app = app_over(dir.path());
    let (st, body) = get(&app, "/_index_template/logs").await;
    assert_eq!(
        st,
        StatusCode::OK,
        "a restart once the file is readable must restore everything: {body}"
    );
    let (st, _) = put_json(
        &app,
        "/_index_template/fresh",
        json!({ "index_patterns": ["fresh-*"] }),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "and writes must work again — the refusal must not be sticky across a \
         clean boot"
    );
}

/// Concurrent writers must not trip over each other.
///
/// The atomic rewrite stages through one fixed temp path, so two flushes
/// running at once create, truncate and rename the *same* file: one loses
/// its bytes to the other's truncate, or finds the temp file already renamed
/// away. A provisioning script that fires its `PUT`s in parallel is all it
/// takes — no stress needed. Measured without the engine's write lock, 32
/// requests over a multi-threaded runtime fail immediately:
///
/// ```text
/// PUT template t2: {"error":{"type":"store_exception",
///                            "reason":"storage error: IO error"},"status":500}
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_writes_do_not_corrupt_the_store() {
    const N: usize = 32;
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let app = app_over(dir.path());
        let mut writes = Vec::with_capacity(N);
        for i in 0..N {
            let app = app.clone();
            writes.push(tokio::spawn(async move {
                put_json(
                    &app,
                    &format!("/_index_template/t{i}"),
                    json!({ "index_patterns": [format!("t{i}-*")], "priority": i }),
                )
                .await
            }));
        }
        for (i, w) in writes.into_iter().enumerate() {
            let (st, body) = w.await.expect("join");
            assert_eq!(st, StatusCode::OK, "PUT template t{i}: {body}");
        }
    }

    let app = app_over(dir.path());
    for i in 0..N {
        let (st, body) = get(&app, &format!("/_index_template/t{i}")).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "template t{i} did not survive concurrent writes + restart: {body}"
        );
        assert_eq!(
            body["index_templates"][0]["index_template"]["priority"], i,
            "template t{i} came back with the wrong contents: {body}"
        );
    }
}

/// `DELETE /_data_stream/<name>` must not leave a backing index behind.
///
/// Now that the stream is persisted, the *order* of the two destructive steps
/// decides whether a crash mid-delete is recoverable. Recording the removal
/// first and destroying the backing indices second leaves `.ds-<name>-00000N`
/// directories that no data-stream API can reach: GET and DELETE answer 404
/// while `PUT /_data_stream/<name>` answers `409
/// resource_already_exists_exception` — permanently, which is the same wedge
/// the interrupted-rollover test above exists to prevent.
///
/// This test pins the observable end state of a clean delete: nothing left on
/// disk, nothing left in the document, and the name immediately reusable
/// after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_data_stream_leaves_no_backing_index_behind() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let app = app_over(dir.path());
        let (st, body) = put_json(&app, "/_data_stream/events", json!({})).await;
        assert_eq!(st, StatusCode::OK, "PUT _data_stream: {body}");
        for _ in 0..2 {
            let (st, body) = post(&app, "/events/_rollover", json!({})).await;
            assert_eq!(st, StatusCode::OK, "rollover: {body}");
        }
        assert!(
            dir.path().join(".ds-events-000003").is_dir(),
            "fixture is wrong: three generations should exist"
        );

        let (st, body) = delete(&app, "/_data_stream/events").await;
        assert_eq!(st, StatusCode::OK, "DELETE _data_stream: {body}");
    }

    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".ds-events-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a delete that answered 200 left backing indices on disk: {leftovers:?}"
    );

    let state: Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("cluster_state.json")).expect("read"),
    )
    .expect("parse");
    assert!(
        state["data_streams"]["events"].is_null(),
        "the removal did not reach disk: {state}"
    );

    // The name must be reusable — the wedge shows up here as a 409 naming a
    // backing index the operator can no longer see.
    let app = app_over(dir.path());
    let (st, body) = put_json(&app, "/_data_stream/events", json!({})).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "the stream name must be free after a delete + restart: {body}"
    );
}

/// A backing index that cannot be destroyed must abort the delete, not be
/// acknowledged.
///
/// This is the ordering above stated as a property rather than an outcome: the
/// removal is only recorded once every backing index is really gone. The
/// failure is injected where it actually happens in the field — the directory
/// cannot be unlinked (read-only mount, EACCES after a uid change, EIO) —
/// and the assertions are that the operator is told, that the stream is still
/// fully addressable, that the document on disk still names it, and that the
/// retry after the cause is fixed completes the job.
///
/// Unix-only, and skipped when the process can write through a `chmod 555`
/// (i.e. running as root), where the failure cannot be injected this way.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backing_index_that_cannot_be_deleted_aborts_the_delete() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    let (st, body) = put_json(&app, "/_data_stream/orders", json!({})).await;
    assert_eq!(st, StatusCode::OK, "PUT _data_stream: {body}");

    let backing = dir.path().join(".ds-orders-000001");
    assert!(backing.is_dir(), "fixture is wrong: no backing index");
    let before = std::fs::read_to_string(dir.path().join("cluster_state.json")).expect("read");

    // Make the backing index's own contents un-unlinkable. Nothing inside is
    // destroyed, and the data dir stays writable — so a flush would succeed
    // if the code reached one, which is what makes the assertion below about
    // the document meaningful.
    std::fs::set_permissions(&backing, std::fs::Permissions::from_mode(0o555)).expect("chmod 555");
    if std::fs::File::create(backing.join("root-probe")).is_ok() {
        let _ = std::fs::remove_file(backing.join("root-probe"));
        std::fs::set_permissions(&backing, std::fs::Permissions::from_mode(0o755))
            .expect("chmod 755");
        eprintln!("skipped: permissions do not deny this process (running as root?)");
        return;
    }

    let (st, body) = delete(&app, "/_data_stream/orders").await;
    std::fs::set_permissions(&backing, std::fs::Permissions::from_mode(0o755)).expect("chmod 755");
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a delete that could not destroy a backing index must report it, not \
         answer acknowledged: {body}"
    );

    let (st, body) = get(&app, "/_data_stream/orders").await;
    assert_eq!(
        st,
        StatusCode::OK,
        "the stream must stay addressable after a failed delete, or the \
         operator has no way to retry it: {body}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cluster_state.json")).expect("read"),
        before,
        "the removal must not have been recorded while the data is still there"
    );

    // The cause is fixed; the retry finishes the job.
    let (st, body) = delete(&app, "/_data_stream/orders").await;
    assert_eq!(st, StatusCode::OK, "retry after the cause is fixed: {body}");
    assert!(
        !backing.exists(),
        "the retry left the backing index on disk"
    );
    let state: Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("cluster_state.json")).expect("read"),
    )
    .expect("parse");
    assert!(
        state["data_streams"]["orders"].is_null(),
        "the retry did not record the removal: {state}"
    );
}

/// A DELETE that fails part-way still destroys every backing index it can —
/// and the release note says so, so pin it.
///
/// The loop deliberately does not stop at the first error: the caller asked
/// for the whole stream to go, so a generation that *can* be destroyed is
/// destroyed and only the ones that could not are left. The 500 therefore
/// means "did not finish", never "nothing happened", and this test exists so
/// that sentence cannot quietly stop being true — a `break` added to that
/// loop would leave `-000002` and `-000003` on disk and fail here.
///
/// Unix-only, and skipped when the process can write through a `chmod 555`
/// (i.e. running as root), where the failure cannot be injected this way.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partly_failed_delete_still_removes_what_it_could() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    let (st, body) = put_json(&app, "/_data_stream/audit", json!({})).await;
    assert_eq!(st, StatusCode::OK, "PUT _data_stream: {body}");
    for _ in 0..2 {
        let (st, body) = post(&app, "/audit/_rollover", json!({})).await;
        assert_eq!(st, StatusCode::OK, "rollover: {body}");
    }

    let first = dir.path().join(".ds-audit-000001");
    let second = dir.path().join(".ds-audit-000002");
    let third = dir.path().join(".ds-audit-000003");
    for p in [&first, &second, &third] {
        assert!(p.is_dir(), "fixture is wrong: {} missing", p.display());
    }

    // Only the FIRST generation is un-unlinkable. It is also the first one
    // the loop reaches, so a loop that stopped on error would leave the other
    // two behind.
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o555)).expect("chmod 555");
    if std::fs::File::create(first.join("root-probe")).is_ok() {
        let _ = std::fs::remove_file(first.join("root-probe"));
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        eprintln!("skipped: permissions do not deny this process (running as root?)");
        return;
    }

    let (st, body) = delete(&app, "/_data_stream/audit").await;
    std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o755)).expect("chmod 755");
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a delete that could not destroy every backing index must report it: {body}"
    );

    assert!(
        first.exists(),
        "the generation that could not be deleted must still be there"
    );
    assert!(
        !second.exists() && !third.exists(),
        "the delete stopped at the first failure — the generations it could \
         have destroyed are still on disk, which contradicts the release note"
    );

    // The stream is still addressable and still recorded, so the retry has
    // something to finish.
    let (st, body) = get(&app, "/_data_stream/audit").await;
    assert_eq!(st, StatusCode::OK, "stream must stay addressable: {body}");
    let state: Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("cluster_state.json")).expect("read"),
    )
    .expect("parse");
    assert!(
        !state["data_streams"]["audit"].is_null(),
        "the removal must not have been recorded while a backing index remains: {state}"
    );

    let (st, body) = delete(&app, "/_data_stream/audit").await;
    assert_eq!(st, StatusCode::OK, "retry after the cause is fixed: {body}");
    assert!(!first.exists(), "the retry left the backing index on disk");
}
