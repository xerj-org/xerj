//! Each index routes documents with its own shard count.
//!
//! A ring creates `old` with three primaries and writes 2,000 documents.
//! A later index `new` is created with a four-shard plan, which is what a
//! node started with four members proposes. Before the fix the router used
//! one hash width for the whole cluster, so creating `new` re-bucketed every
//! `old` id: about a quarter of the acknowledged ids answered 404 and new
//! writes landed on shard 3, which `old` does not have.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::watch;
use tower::ServiceExt;
use xerj_api::cluster_shard::ApiShardHandler;
use xerj_cluster::node::in_memory::{InMemoryBus, InMemoryTransport};
use xerj_cluster::raft::ClusterCommand;
use xerj_cluster::runner::ClusterRunner;
use xerj_cluster::{ClusterHandle, ClusterNode};
use xerj_engine::EngineMetadataApplier;

struct Member {
    app: Router,
    engine: std::sync::Arc<xerj_engine::Engine>,
    shutdown: watch::Sender<bool>,
    handle: ClusterHandle,
    _runner: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

async fn ring() -> Vec<Member> {
    let bus = InMemoryBus::new();
    let ids = ["n1", "n2", "n3"];
    let mut members = Vec::new();
    for id in ids {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        let state = xerj_api::state::AppState::new(config, engine, metrics);
        let peers: Vec<String> = ids
            .iter()
            .filter(|peer| **peer != id)
            .map(|peer| (*peer).to_string())
            .collect();
        let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
        let mut node = ClusterNode::new(id.to_string(), peers, Box::new(transport));
        node.set_applier(std::sync::Arc::new(EngineMetadataApplier {
            engine: std::sync::Arc::clone(&state.engine),
        }));
        let handle = node.handle();
        state.engine.attach_cluster(handle.clone());
        handle.set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(state.clone())));
        let engine = std::sync::Arc::clone(&state.engine);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
        let task = tokio::spawn(async move {
            runner.run().await;
        });
        let app = xerj_api::router::build_es_compat_router(state.clone());
        members.push(Member {
            app,
            engine,
            shutdown,
            handle,
            _runner: task,
            _dir: dir,
        });
    }
    members
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
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

async fn put_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::put(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    send(app, req).await
}

async fn post_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    send(app, req).await
}

async fn post_ndjson(app: &Router, path: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::post(path)
        .header("content-type", "application/x-ndjson")
        .body(Body::from(body.to_string()))
        .expect("request");
    send(app, req).await
}

async fn get(app: &Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn wait_leader(members: &[Member]) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let leaders: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(_, member)| member.handle.is_leader())
            .map(|(i, _)| i)
            .collect();
        if leaders.len() == 1 {
            return leaders[0];
        }
        assert!(Instant::now() < deadline, "no single raft leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_status(app: &Router, path: &str, want: StatusCode) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get(app, path).await;
        if status == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "GET {path} stayed {status} {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn bulk_old(app: &Router, from: usize, to: usize) {
    let mut body = String::new();
    for i in from..to {
        body.push_str(&json!({ "index": { "_index": "old", "_id": format!("d{i}") } }).to_string());
        body.push('\n');
        body.push_str(&json!({ "n": i }).to_string());
        body.push('\n');
    }
    let (status, resp) = post_ndjson(app, "/_bulk", &body).await;
    assert!(status.is_success(), "bulk {from}..{to}: {status} {resp}");
    assert_eq!(
        resp.get("errors"),
        Some(&Value::Bool(false)),
        "bulk {from}..{to} had item errors: {resp}"
    );
}

async fn missing_ids(members: &[Member], count: usize) -> Vec<(usize, usize)> {
    let mut missing = Vec::new();
    for (m, member) in members.iter().enumerate() {
        for i in 0..count {
            let (status, _) = get(&member.app, &format!("/old/_doc/d{i}")).await;
            if status != StatusCode::OK {
                missing.push((m, i));
            }
        }
    }
    missing
}

async fn totals(members: &[Member]) -> Vec<Option<u64>> {
    let mut out = Vec::new();
    for member in members {
        let (status, body) = post_json(
            &member.app,
            "/old/_search",
            json!({ "size": 0, "track_total_hits": true, "query": { "match_all": {} } }),
        )
        .await;
        assert!(status.is_success(), "search: {status} {body}");
        out.push(body.pointer("/hits/total/value").and_then(Value::as_u64));
    }
    out
}

fn four_shard_create(name: &str) -> ClusterCommand {
    create_with_plan(name, &[(0, "n1"), (1, "n2"), (2, "n3"), (3, "n1")])
}

fn create_with_plan(name: &str, assignments: &[(u32, &str)]) -> ClusterCommand {
    let plan: Vec<Value> = assignments
        .iter()
        .map(|(shard, node)| json!({ "shard": shard, "node_id": node, "replicas": [] }))
        .collect();
    let envelope = json!({
        "v": 1,
        "kind": "index_create",
        "schema": xerj_common::types::Schema::empty(),
        "settings": {},
        "mappings": null,
        "index_settings": null,
        "aliases": null,
        "shard_plan": plan,
    });
    ClusterCommand::CreateIndex {
        name: name.to_string(),
        schema_json: envelope.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wider_index_does_not_rebucket_an_older_index() {
    const FIRST: usize = 2_000;
    const MORE: usize = 500;
    let members = ring().await;
    let leader = wait_leader(&members).await;

    let (status, body) = put_json(
        &members[leader].app,
        "/old",
        json!({ "settings": { "index": { "number_of_replicas": 1 } } }),
    )
    .await;
    assert!(status.is_success(), "create old: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/old", StatusCode::OK).await;
        assert_eq!(member.engine.shard_search_targets("old").len(), 3);
    }

    for (chunk, start) in (0..FIRST).step_by(500).enumerate() {
        bulk_old(&members[chunk % 3].app, start, start + 500).await;
    }
    assert!(missing_ids(&members, FIRST).await.is_empty(), "baseline");
    assert_eq!(totals(&members).await, vec![Some(FIRST as u64); 3]);

    members[leader]
        .handle
        .propose(four_shard_create("new"))
        .await
        .expect("create new");
    for member in &members {
        wait_status(&member.app, "/new", StatusCode::OK).await;
    }

    let missing = missing_ids(&members, FIRST).await;
    assert!(
        missing.is_empty(),
        "{} of {} (member, id) GETs missed after a 4-shard index was created; first: {:?}",
        missing.len(),
        FIRST * 3,
        &missing[..missing.len().min(5)]
    );
    assert_eq!(totals(&members).await, vec![Some(FIRST as u64); 3]);

    for (chunk, start) in (FIRST..FIRST + MORE).step_by(100).enumerate() {
        bulk_old(&members[chunk % 3].app, start, start + 100).await;
    }
    let missing = missing_ids(&members, FIRST + MORE).await;
    assert!(
        missing.is_empty(),
        "{} GETs missed after more writes",
        missing.len()
    );
    assert_eq!(totals(&members).await, vec![Some((FIRST + MORE) as u64); 3]);

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

/// A plan with no primary for shard 2: a write that hashes there is refused
/// with 503 `shard_not_assigned_exception` on every member, single-doc and
/// bulk, and nothing is stored on the member that received it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_to_an_unassigned_shard_is_refused_not_stored_locally() {
    let members = ring().await;
    let leader = wait_leader(&members).await;
    members[leader]
        .handle
        .propose(create_with_plan(
            "holey",
            &[(0, "n1"), (1, "n2"), (3, "n3")],
        ))
        .await
        .expect("create holey");
    for member in &members {
        wait_status(&member.app, "/holey", StatusCode::OK).await;
    }
    let id = (0..10_000)
        .map(|i| format!("h{i}"))
        .find(|id| members[0].engine.primary_of("holey", id).is_err())
        .expect("an id on the unassigned shard");

    for member in &members {
        let (status, body) =
            put_json(&member.app, &format!("/holey/_doc/{id}"), json!({ "x": 1 })).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            body.pointer("/error/type").and_then(Value::as_str),
            Some("shard_not_assigned_exception"),
            "{body}"
        );
        let bulk = format!(
            "{}\n{}\n",
            json!({ "index": { "_index": "holey", "_id": id } }),
            json!({ "x": 2 })
        );
        let (status, body) = post_ndjson(&member.app, "/_bulk", &bulk).await;
        assert!(status.is_success(), "bulk envelope: {status} {body}");
        assert_eq!(body.get("errors"), Some(&Value::Bool(true)), "{body}");
        assert_eq!(
            body.pointer("/items/0/index/status")
                .and_then(Value::as_u64),
            Some(503),
            "{body}"
        );
    }
    for member in &members {
        let local = member
            .engine
            .get_index("holey")
            .expect("index")
            .get_document(&id)
            .await
            .expect("local get");
        assert!(local.is_none(), "refused write must not be stored locally");
    }
    for member in &members {
        let _ = member.shutdown.send(true);
    }
}
