//! Index create, mapping, settings, and delete on one member of a 3-node
//! ring show up on the others. A document written on one member is readable
//! from the others because it is stored on the shard owner. A ring that is
//! not running refuses the create instead of applying it locally.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::watch;
use tower::ServiceExt;
use xerj_api::cluster_shard::ApiShardHandler;
use xerj_cluster::node::in_memory::{InMemoryBus, InMemoryTransport};
use xerj_cluster::runner::ClusterRunner;
use xerj_cluster::{ClusterHandle, ClusterNode};
use xerj_engine::EngineMetadataApplier;

struct Member {
    app: Router,
    state: xerj_api::state::AppState,
    engine: std::sync::Arc<xerj_engine::Engine>,
    shutdown: watch::Sender<bool>,
    handle: ClusterHandle,
    /// Held when the runner is not started, so the proposal channel stays open.
    _node: Option<ClusterNode>,
    runner: Option<tokio::task::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

async fn ring(start_runners: bool) -> Vec<Member> {
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
        let (kept, runner) = if start_runners {
            let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
            let task = tokio::spawn(async move {
                runner.run().await;
            });
            (None, Some(task))
        } else {
            handle.set_timeout(Duration::from_millis(400));
            let _ = shutdown_rx;
            (Some(node), None)
        };
        let app = xerj_api::router::build_es_compat_router(state.clone());
        members.push(Member {
            app,
            state,
            engine,
            shutdown,
            handle,
            _node: kept,
            runner,
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
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn put_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Request::put(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn post_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn post_ndjson(app: &Router, path: &str, body: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::post(path)
            .header("content-type", "application/x-ndjson")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn get(app: &Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn delete(app: &Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::delete(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn wait_leader(members: &[Member], dead: &[bool]) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let leaders: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(i, member)| !dead[*i] && member.handle.is_leader())
            .map(|(i, _)| i)
            .collect();
        if leaders.len() == 1 {
            return leaders[0];
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for one raft leader");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_status(app: &Router, path: &str, want: StatusCode) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get(app, path).await;
        if status == want {
            return body;
        }
        if Instant::now() > deadline {
            panic!("GET {path} stayed {status} body={body} (wanted {want})");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_body_contains(app: &Router, path: &str, needle: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get(app, path).await;
        if status.is_success() && body.to_string().contains(needle) {
            return body;
        }
        if Instant::now() > deadline {
            panic!("GET {path} never contained {needle}: {status} {body}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_metadata_propagates_and_a_new_leader_still_commits() {
    let members = ring(true).await;
    let mut dead = [false, false, false];
    let leader = wait_leader(&members, &dead).await;
    let follower = (0..3).find(|i| *i != leader).expect("a follower");

    let (status, body) = put_json(
        &members[follower].app,
        "/orders",
        json!({
            "mappings": { "properties": { "title": { "type": "text" } } }
        }),
    )
    .await;
    assert!(status.is_success(), "create from follower: {status} {body}");

    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
        let mapping = wait_status(&member.app, "/orders/_mapping", StatusCode::OK).await;
        assert!(
            mapping.to_string().contains("title"),
            "mapping did not propagate: {mapping}"
        );
    }

    let (status, body) = put_json(
        &members[follower].app,
        "/orders/_settings",
        json!({ "index": { "number_of_replicas": "2" } }),
    )
    .await;
    assert!(status.is_success(), "settings: {status} {body}");
    for member in &members {
        wait_body_contains(&member.app, "/orders/_settings", "number_of_replicas").await;
    }

    let (status, body) = put_json(
        &members[follower].app,
        "/orders/_doc/1",
        json!({ "title": "routed" }),
    )
    .await;
    assert!(status.is_success(), "document write: {status} {body}");
    for member in &members {
        let (status, body) = get(&member.app, "/orders/_doc/1").await;
        assert!(
            status.is_success(),
            "document is readable on every member: {status} {body}"
        );
    }
    let owners = members
        .iter()
        .filter(|member| member.engine.owns_shard("orders", "1"))
        .count();
    assert_eq!(owners, 1, "the document has one primary owner");

    let (status, body) = delete(&members[follower].app, "/orders").await;
    assert!(status.is_success(), "delete: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::NOT_FOUND).await;
    }

    let _ = members[leader].shutdown.send(true);
    dead[leader] = true;
    let next = wait_leader(&members, &dead).await;
    assert_ne!(next, leader);
    let proposer = (0..3).find(|i| !dead[*i] && *i != next).unwrap_or(next);
    let (status, body) = put_json(&members[proposer].app, "/tickets", json!({})).await;
    assert!(
        status.is_success(),
        "create after leader change: {status} {body}"
    );
    for (i, member) in members.iter().enumerate() {
        if dead[i] {
            continue;
        }
        wait_status(&member.app, "/tickets", StatusCode::OK).await;
    }

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ring_with_no_leader_does_not_create_the_index() {
    let members = ring(false).await;
    let (status, body) = put_json(&members[0].app, "/orders", json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, _) = get(&members[0].app, "/orders").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = put_json(
        &members[0].app,
        "/orders/_doc/1",
        json!({ "title": "no leader" }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, _) = get(&members[0].app, "/orders/_doc/1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&members[1].app, "/orders").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_disabled_create_stays_on_the_node_that_received_it() {
    async fn lone() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        assert!(!engine.has_cluster());
        let state = xerj_api::state::AppState::new(config, engine, metrics);
        (xerj_api::router::build_es_compat_router(state), dir)
    }

    let (a, _dir_a) = lone().await;
    let (b, _dir_b) = lone().await;
    let (status, body) = put_json(&a, "/local", json!({})).await;
    assert!(status.is_success(), "{status} {body}");
    let (status, _) = get(&a, "/local").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&b, "/local").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = put_json(&a, "/local/_doc/1", json!({ "title": "here" })).await;
    assert!(
        status.is_success(),
        "single-node document write: {status} {body}"
    );
    let (status, _) = get(&a, "/local/_doc/1").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&b, "/local/_doc/1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = post_json(
        &a,
        "/local/_search",
        json!({ "query": { "match_all": {} }, "size": 10 }),
    )
    .await;
    assert!(status.is_success(), "single-node search: {status} {body}");
    assert_eq!(
        body.pointer("/hits/total/value").and_then(|v| v.as_u64()),
        Some(1),
        "single-node search total: {body}"
    );

    let (status, body) = put_json(
        &a,
        "/solo",
        json!({ "settings": { "index": { "number_of_replicas": 1 } } }),
    )
    .await;
    assert!(status.is_success(), "single-node create: {status} {body}");
    let (status, body) = put_json(&a, "/solo/_doc/1", json!({ "title": "solo" })).await;
    assert!(status.is_success(), "single-node write: {status} {body}");
    let (status, _) = get(&b, "/solo/_doc/1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&a, "/.xerj_replica_solo_0/_doc/1").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "single-node mode does not open a replica store"
    );
    let (status, body) = post_json(
        &a,
        "/solo/_search",
        json!({ "query": { "match_all": {} }, "size": 10 }),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(
        body.pointer("/hits/total/value").and_then(|v| v.as_u64()),
        Some(1),
        "single-node search stays one document: {body}"
    );

    let (status, body) = post_ndjson(
        &a,
        "/local/_bulk",
        "{\"index\":{\"_id\":\"b1\"}}\n{\"title\":\"bulk\"}\n",
    )
    .await;
    assert!(status.is_success(), "single-node bulk: {status} {body}");
    assert_eq!(
        body.pointer("/errors").and_then(|v| v.as_bool()),
        Some(false),
        "{body}"
    );
    let (status, got) = get(&a, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (status, _) = get(&b, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = post_json(
        &a,
        "/local/_update/b1",
        json!({ "doc": { "title": "patched" } }),
    )
    .await;
    assert!(status.is_success(), "single-node update: {status} {body}");
    let (status, got) = get(&a, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("patched")
    );
    let (status, _) = get(&b, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&a, "/.xerj_replica_local_0/_doc/b1").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "single-node bulk and update do not open a replica store"
    );

    let (status, body) = post_json(
        &a,
        "/local/_update_by_query",
        json!({
            "query": { "ids": { "values": ["b1"] } },
            "script": { "source": "ctx._source.title = 'ubq'" }
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "single-node update_by_query: {status} {body}"
    );
    assert_eq!(
        body.get("updated").and_then(|v| v.as_u64()),
        Some(1),
        "{body}"
    );
    let (status, got) = get(&a, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("ubq")
    );
    let (status, got) = get(&a, "/local/_doc/1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("here"),
        "an ids query on one node still leaves the other document: {got}"
    );
    let (status, _) = get(&b, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&a, "/.xerj_replica_local_0/_doc/b1").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "single-node update_by_query does not open a replica store"
    );
}

/// `_delete_by_query` with no cluster handle stays on the node that accepted
/// it. The runner does not fit a 2 MiB debug worker once it is under the
/// router, so this test sizes its own stacks. The HTTP result is the
/// single-node runner: one document deleted, the other left in place, and
/// no replica store.
#[test]
fn cluster_disabled_delete_by_query_stays_on_the_node_that_received_it() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(
                    cluster_disabled_delete_by_query_stays_on_the_node_that_received_it_inner(),
                )
        })
        .expect("spawn")
        .join()
        .expect("test thread panicked");
}

async fn cluster_disabled_delete_by_query_stays_on_the_node_that_received_it_inner() {
    async fn lone() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        assert!(!engine.has_cluster());
        let state = xerj_api::state::AppState::new(config, engine, metrics);
        (xerj_api::router::build_es_compat_router(state), dir)
    }

    let (a, _dir_a) = lone().await;
    let (b, _dir_b) = lone().await;
    let (status, body) = put_json(&a, "/local", json!({})).await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = put_json(&a, "/local/_doc/1", json!({ "title": "here" })).await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = put_json(&a, "/local/_doc/b1", json!({ "title": "bulk" })).await;
    assert!(status.is_success(), "{status} {body}");

    let (status, body) = post_json(
        &a,
        "/local/_delete_by_query",
        json!({ "query": { "ids": { "values": ["b1"] } } }),
    )
    .await;
    assert!(
        status.is_success(),
        "single-node delete_by_query: {status} {body}"
    );
    assert_eq!(
        body.get("deleted").and_then(|v| v.as_u64()),
        Some(1),
        "{body}"
    );
    let (status, _) = get(&a, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, got) = get(&a, "/local/_doc/1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("here"),
        "an ids query on one node still leaves the other document: {got}"
    );
    let (status, _) = get(&b, "/local/_doc/1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&b, "/local/_doc/b1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&a, "/.xerj_replica_local_0/_doc/b1").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "single-node delete_by_query does not open a replica store"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acknowledged_write_is_on_the_replica_and_readable_after_the_primary_stops() {
    let members = ring(true).await;
    let dead = [false, false, false];
    let _leader = wait_leader(&members, &dead).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({ "settings": { "index": { "number_of_replicas": 1 } } }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut placed = None;
    for n in 0..64 {
        let id = format!("r{n}");
        let primary = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id));
        let Some(primary) = primary else {
            continue;
        };
        let (shard, _) = members[primary]
            .engine
            .primary_of("orders", &id)
            .expect("primary placement");
        let replicas = members[primary].engine.replica_nodes("orders", shard);
        assert_eq!(replicas.len(), 1, "one replica for shard {shard}");
        let replica = members
            .iter()
            .position(|member| member.handle.node_id() == replicas[0])
            .expect("replica is a member");
        assert_ne!(replica, primary);
        placed = Some((id, primary, replica, shard));
        break;
    }
    let (id, primary, replica, shard) = placed.expect("an id with a primary and a replica");
    let writer = (0..3)
        .find(|n| *n != primary)
        .expect("a non-primary writer");
    let other = (0..3)
        .find(|n| *n != primary && *n != replica)
        .expect("a third member");

    let (status, body) = put_json(
        &members[writer].app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": "copied" }),
    )
    .await;
    assert!(status.is_success(), "write: {status} {body}");

    let on_primary = members[primary]
        .engine
        .get_index("orders")
        .expect("primary index")
        .get_document(&id)
        .await
        .expect("primary get");
    assert!(on_primary.is_some(), "primary stores the document");

    let store = xerj_cluster::replica_index_name("orders", shard).expect("store name");
    let on_replica = members[replica]
        .engine
        .get_index(&store)
        .expect("replica store")
        .get_document(&id)
        .await
        .expect("replica get");
    assert_eq!(
        on_replica
            .as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("copied"),
        "replica stores the acknowledged write: {on_replica:?}"
    );
    let mixed = members[replica]
        .engine
        .get_index("orders")
        .expect("user index")
        .get_document(&id)
        .await
        .expect("user get");
    assert!(
        mixed.is_none(),
        "the replica copy is not written into the user index"
    );
    assert!(
        members[other].engine.get_index(&store).is_err(),
        "a node that is not the replica does not open {store}"
    );

    let (status, body) = post_json(
        &members[other].app,
        "/orders/_search",
        json!({ "query": { "match_all": {} }, "size": 10 }),
    )
    .await;
    assert!(status.is_success(), "search: {status} {body}");
    assert_eq!(
        body.pointer("/hits/total/value").and_then(|v| v.as_u64()),
        Some(1),
        "a live primary search must not count the replica: {body}"
    );

    members[primary].handle.clear_shard_handler();

    let (status, body) = get(&members[other].app, &format!("/orders/_doc/{id}")).await;
    assert!(
        status.is_success(),
        "GET after the primary stops: {status} {body}"
    );
    assert_eq!(
        body.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("copied"),
        "{body}"
    );

    let (status, body) = post_json(
        &members[other].app,
        "/orders/_search",
        json!({ "query": { "match_all": {} }, "size": 10 }),
    )
    .await;
    assert!(
        status.is_success(),
        "search after the primary stops: {status} {body}"
    );
    assert_eq!(
        body.pointer("/hits/total/value").and_then(|v| v.as_u64()),
        Some(1),
        "{body}"
    );
    let hit_index = body.pointer("/hits/hits/0/_index").and_then(|v| v.as_str());
    assert_eq!(
        hit_index,
        Some("orders"),
        "hits keep the user index name: {body}"
    );

    let (status, body) = put_json(
        &members[other].app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": "again" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a write still needs the primary: {status} {body}"
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

async fn wait_in_sync(members: &[Member], shard: u32, want: &[String]) {
    let mut want = want.to_vec();
    want.sort();
    want.dedup();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ready = members.iter().all(|member| {
            let mut got = member.engine.replica_nodes("orders", shard);
            got.sort();
            got == want
        });
        if ready {
            return;
        }
        if Instant::now() > deadline {
            let observed: Vec<Vec<String>> = members
                .iter()
                .map(|member| member.engine.replica_nodes("orders", shard))
                .collect();
            panic!("in-sync for shard {shard} stayed {observed:?}, wanted {want:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quorum_of_copies_acks_when_one_of_two_replicas_does_not() {
    let members = ring(true).await;
    let dead = [false, false, false];
    let _leader = wait_leader(&members, &dead).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({ "settings": { "index": { "number_of_replicas": 2 } } }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut placed = None;
    for n in 0..64 {
        let id = format!("q{n}");
        let primary = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id));
        let Some(primary) = primary else {
            continue;
        };
        let (shard, _) = members[primary]
            .engine
            .primary_of("orders", &id)
            .expect("primary placement");
        let replicas = members[primary]
            .engine
            .placed_replica_nodes("orders", shard);
        if replicas.len() != 2 {
            continue;
        }
        let replica_at = |id: &str| {
            members
                .iter()
                .position(|member| member.handle.node_id() == id)
                .expect("replica is a member")
        };
        let replica_a = replica_at(&replicas[0]);
        let replica_b = replica_at(&replicas[1]);
        assert_ne!(replica_a, primary);
        assert_ne!(replica_b, primary);
        assert_ne!(replica_a, replica_b);
        placed = Some((id, primary, replica_a, replica_b, shard));
        break;
    }
    let (id, primary, replica_a, replica_b, shard) =
        placed.expect("an id whose shard has two replicas");
    let writer = (0..3)
        .find(|n| *n != primary)
        .expect("a non-primary writer");

    members[replica_a].handle.clear_shard_handler();
    members[replica_b].handle.clear_shard_handler();
    let (status, body) = put_json(
        &members[writer].app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": "refused" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the primary alone is not a quorum of three copies: {status} {body}"
    );
    assert_eq!(
        body.pointer("/error/type").and_then(|v| v.as_str()),
        Some("replica_not_acknowledged"),
        "{body}"
    );

    members[replica_b]
        .handle
        .set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(
            members[replica_b].state.clone(),
        )));

    let (status, body) = put_json(
        &members[writer].app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": "copied" }),
    )
    .await;
    assert!(
        status.is_success(),
        "primary plus one replica is a quorum of three copies: {status} {body}"
    );

    let on_primary = members[primary]
        .engine
        .get_index("orders")
        .expect("primary index")
        .get_document(&id)
        .await
        .expect("primary get");
    assert_eq!(
        on_primary
            .as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("copied")
    );

    let store = xerj_cluster::replica_index_name("orders", shard).expect("store name");
    let on_live = members[replica_b]
        .engine
        .get_index(&store)
        .expect("live replica store")
        .get_document(&id)
        .await
        .expect("live replica get");
    assert_eq!(
        on_live
            .as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("copied"),
        "the replica that confirmed has the write: {on_live:?}"
    );
    // The handler on this replica stays cleared. Restoring it would let
    // document backfill copy the write and put the replica back in the
    // in-sync set. That path is covered by
    // `a_replica_dropped_from_the_in_sync_set_copies_the_missed_write_and_rejoins`.
    let on_stale = members[replica_a]
        .engine
        .get_index(&store)
        .expect("stale replica store")
        .get_document(&id)
        .await
        .expect("stale replica get");
    assert!(
        on_stale.is_none(),
        "the replica that did not confirm does not have the document: {on_stale:?}"
    );

    let live_id = members[replica_b].handle.node_id().to_string();
    wait_in_sync(&members, shard, &[live_id]).await;
    assert_eq!(
        members[primary]
            .engine
            .placed_replica_nodes("orders", shard)
            .len(),
        2,
        "placement stays at the create-time replica count"
    );

    members[primary].handle.clear_shard_handler();
    let (status, body) = get(&members[replica_a].app, &format!("/orders/_doc/{id}")).await;
    assert!(
        status.is_success(),
        "GET uses the in-sync replica, not the stale one: {status} {body}"
    );
    assert_eq!(
        body.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("copied"),
        "a read after the primary stops uses the replica that confirmed: {body}"
    );

    let (status, body) = post_json(
        &members[replica_a].app,
        "/orders/_search",
        json!({ "query": { "match_all": {} }, "size": 10 }),
    )
    .await;
    assert!(status.is_success(), "search: {status} {body}");
    assert_eq!(
        body.pointer("/hits/total/value").and_then(|v| v.as_u64()),
        Some(1),
        "{body}"
    );
    assert_eq!(
        body.pointer("/hits/hits/0/_source/title")
            .and_then(|v| v.as_str()),
        Some("copied"),
        "{body}"
    );
    assert_eq!(
        body.pointer("/hits/hits/0/_index").and_then(|v| v.as_str()),
        Some("orders"),
        "{body}"
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_document_is_stored_on_its_shard_owner_and_search_sees_it_from_any_member() {
    let members = ring(true).await;
    let dead = [false, false, false];
    let _leader = wait_leader(&members, &dead).await;

    let (status, body) = put_json(&members[0].app, "/orders", json!({})).await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
        let targets = member.engine.shard_search_targets("orders");
        assert_eq!(
            targets.len(),
            3,
            "create installs one primary on each member"
        );
        for shard in 0..3 {
            let store = xerj_cluster::replica_index_name("orders", shard).expect("name");
            assert!(
                member.engine.get_index(&store).is_err(),
                "number_of_replicas defaults to 0, so {store} is not created"
            );
        }
    }

    let mut owner_of: Vec<(String, usize)> = Vec::new();
    for i in 0..48 {
        let id = format!("d{i}");
        if let Some(owner) = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id))
        {
            if !owner_of.iter().any(|(_, seen)| *seen == owner) {
                owner_of.push((id, owner));
            }
        }
        if owner_of.len() == 3 {
            break;
        }
    }
    assert_eq!(
        owner_of.len(),
        3,
        "expected the plan to place some id on each member"
    );

    for (id, _) in &owner_of {
        let writer = (owner_of[0].1 + 1) % 3;
        let (status, body) = put_json(
            &members[writer].app,
            &format!("/orders/_doc/{id}"),
            json!({ "title": id }),
        )
        .await;
        assert!(status.is_success(), "write {id}: {status} {body}");
    }

    for (id, owner) in &owner_of {
        for (i, member) in members.iter().enumerate() {
            let (status, body) = get(&member.app, &format!("/orders/_doc/{id}")).await;
            assert!(
                status.is_success(),
                "GET {id} on member {i}: {status} {body}"
            );
            let local = member
                .engine
                .get_index("orders")
                .expect("index")
                .get_document(id)
                .await
                .expect("local get");
            if i == *owner {
                assert!(local.is_some(), "owner stores {id}");
            } else {
                assert!(local.is_none(), "non-owner does not store {id}");
            }
        }
    }

    let reader = (owner_of[0].1 + 2) % 3;
    let (status, body) = post_json(
        &members[reader].app,
        "/orders/_search",
        json!({ "query": { "match_all": {} }, "size": 20 }),
    )
    .await;
    assert!(status.is_success(), "search: {status} {body}");
    let hits = body
        .pointer("/hits/hits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let ids: Vec<&str> = hits
        .iter()
        .filter_map(|hit| hit.get("_id").and_then(|v| v.as_str()))
        .collect();
    for (id, _) in &owner_of {
        assert!(ids.contains(&id.as_str()), "search missed {id} in {ids:?}");
    }
    let total = body.pointer("/hits/total/value").and_then(|v| v.as_u64());
    assert_eq!(total, Some(owner_of.len() as u64), "merged total: {body}");

    let (status, body) = post_json(
        &members[reader].app,
        "/orders/_search",
        json!({ "size": 0, "aggs": { "titles": { "terms": { "field": "title" } } } }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "aggregations are not merged: {body}"
    );

    let (id, owner) = &owner_of[0];
    let deleter = (*owner + 1) % 3;
    let (status, body) = delete(&members[deleter].app, &format!("/orders/_doc/{id}")).await;
    assert!(status.is_success(), "delete: {status} {body}");
    for member in &members {
        let (status, _) = get(&member.app, &format!("/orders/_doc/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let still = members[*owner]
        .engine
        .get_index("orders")
        .expect("index")
        .get_document(id)
        .await
        .expect("local get");
    assert!(still.is_none(), "delete reached the owner");

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

/// Stop one member and release its data directory.
///
/// The shard handler is cleared first, so a document write during shutdown
/// cannot land on this replica. The runner is joined before the engine is
/// dropped: the engine holds `node.lock`, and a later `Engine::new` on the
/// same directory refuses to open while that lock is held.
async fn shutdown_member(mut member: Member) -> tempfile::TempDir {
    member.handle.clear_shard_handler();
    let _ = member.shutdown.send(true);
    if let Some(task) = member.runner.take() {
        match tokio::time::timeout(Duration::from_secs(5), task).await {
            Ok(Ok(())) => {}
            other => panic!("cluster runner did not stop: {other:?}"),
        }
    }
    drop(member.app);
    drop(member.state);
    drop(member.engine);
    drop(member.handle);
    member._dir
}

async fn shutdown_and_keep(members: Vec<Member>) -> Vec<tempfile::TempDir> {
    let mut dirs = Vec::with_capacity(members.len());
    for mut member in members {
        let _ = member.shutdown.send(true);
        if let Some(task) = member.runner.take() {
            match tokio::time::timeout(Duration::from_secs(5), task).await {
                Ok(Ok(())) => {}
                other => panic!("cluster runner did not stop: {other:?}"),
            }
        }
        // ApiShardHandler holds AppState, which holds Engine, which holds
        // this handle. The engine's data-dir node.lock stays taken until
        // that cycle breaks, so a later Engine::new on the same directory
        // sees this process as another running node.
        member.handle.clear_shard_handler();
        drop(member.app);
        drop(member.state);
        drop(member.engine);
        drop(member.handle);
        dirs.push(member._dir);
    }
    dirs
}

/// Same ring as [`ring`], but each member opens `{data_dir}/_raft` and
/// replays the committed prefix before the runner starts.
async fn durable_ring(dirs: Vec<tempfile::TempDir>) -> Vec<Member> {
    let bus = InMemoryBus::new();
    let ids = ["n1", "n2", "n3"];
    let mut members = Vec::new();
    for (id, dir) in ids.into_iter().zip(dirs) {
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
        let raft_dir = xerj_cluster::raft_log::raft_storage_dir(dir.path());
        let mut node =
            ClusterNode::with_storage(id.to_string(), peers, Box::new(transport), &raft_dir)
                .expect("open raft log");
        node.set_applier(std::sync::Arc::new(EngineMetadataApplier {
            engine: std::sync::Arc::clone(&state.engine),
        }));
        let handle = node.handle();
        state.engine.attach_cluster(handle.clone());
        handle.set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(state.clone())));
        node.replay_committed().await;
        let engine = std::sync::Arc::clone(&state.engine);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
        let task = tokio::spawn(async move {
            runner.run().await;
        });
        let app = xerj_api::router::build_es_compat_router(state.clone());
        members.push(Member {
            app,
            state,
            engine,
            shutdown,
            handle,
            _node: None,
            runner: Some(task),
            _dir: dir,
        });
    }
    members
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_reloads_the_shard_plan_and_still_routes_the_document() {
    let dirs = (0..3)
        .map(|_| tempfile::tempdir().expect("tempdir"))
        .collect();
    let members = durable_ring(dirs).await;
    let dead = [false, false, false];
    let _leader = wait_leader(&members, &dead).await;

    let (status, body) = put_json(&members[0].app, "/orders", json!({})).await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }
    let (status, body) = put_json(
        &members[0].app,
        "/orders/_doc/1",
        json!({ "title": "kept" }),
    )
    .await;
    assert!(status.is_success(), "write: {status} {body}");

    let owner = members
        .iter()
        .position(|member| member.engine.owns_shard("orders", "1"))
        .expect("one primary");
    let owner_id = members[owner].handle.node_id().to_string();
    let other = (0..3).find(|i| *i != owner).expect("a non-owner");
    assert_eq!(
        members[other].engine.route_write("orders", "1").as_deref(),
        Some(owner_id.as_str()),
        "before restart the non-owner forwards"
    );
    let (status, body) = post_json(&members[owner].app, "/orders/_refresh", json!({})).await;
    assert!(
        status.is_success(),
        "refresh before restart: {status} {body}"
    );

    let dirs = shutdown_and_keep(members).await;
    let members = durable_ring(dirs).await;

    assert!(
        members[owner].engine.owns_shard("orders", "1"),
        "replay must put the primary back on {owner_id}"
    );
    assert_eq!(
        members[other].engine.route_write("orders", "1").as_deref(),
        Some(owner_id.as_str()),
        "restart must not treat the index as unassigned"
    );
    let (status, body) = get(&members[other].app, "/orders/_doc/1").await;
    assert!(
        status.is_success(),
        "GET from a non-owner after restart: {status} {body}"
    );

    let _leader = wait_leader(&members, &dead).await;
    let (status, body) = put_json(
        &members[other].app,
        "/orders/_doc/2",
        json!({ "title": "after" }),
    )
    .await;
    assert!(status.is_success(), "write after restart: {status} {body}");
    for member in &members {
        let (status, body) = get(&member.app, "/orders/_doc/2").await;
        assert!(
            status.is_success(),
            "doc 2 readable after restart: {status} {body}"
        );
    }

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

async fn open_durable_member(
    id: &str,
    ids: &[&str],
    bus: &InMemoryBus,
    dir: tempfile::TempDir,
    snapshot_every: u64,
) -> Member {
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
    let raft_dir = xerj_cluster::raft_log::raft_storage_dir(dir.path());
    let mut node = ClusterNode::with_storage(id.to_string(), peers, Box::new(transport), &raft_dir)
        .expect("open raft log");
    node.set_snapshot_every(snapshot_every);
    node.set_applier(std::sync::Arc::new(EngineMetadataApplier {
        engine: std::sync::Arc::clone(&state.engine),
    }));
    let handle = node.handle();
    state.engine.attach_cluster(handle.clone());
    handle.set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(state.clone())));
    node.replay_committed().await;
    let engine = std::sync::Arc::clone(&state.engine);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
    let task = tokio::spawn(async move {
        runner.run().await;
    });
    let app = xerj_api::router::build_es_compat_router(state.clone());
    Member {
        app,
        state,
        engine,
        shutdown,
        handle,
        _node: None,
        runner: Some(task),
        _dir: dir,
    }
}

async fn wait_compacted(member: &Member) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if member.handle.snapshot_index() >= 1 && member.handle.log_len() == 0 {
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "leader log was not compacted: snapshot={} log_len={}",
                member.handle.snapshot_index(),
                member.handle.log_len()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The create is compacted out of the leader's log before the third member
/// starts. That member still gets the index, and a document written afterwards
/// is readable from a member that was already up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_member_installs_a_compacted_create_and_routes_a_later_write() {
    let ids = ["n1", "n2", "n3"];
    let bus = InMemoryBus::new();
    let mut members = Vec::new();
    for id in ["n1", "n2"] {
        let dir = tempfile::tempdir().expect("tempdir");
        members.push(open_durable_member(id, &ids, &bus, dir, 1).await);
    }
    let dead = [false, false];
    let leader = wait_leader(&members, &dead).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({ "mappings": { "properties": { "title": { "type": "text" } } } }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    wait_compacted(&members[leader]).await;
    assert_eq!(
        members[leader].handle.log_len(),
        0,
        "the create must not still be an AppendEntries payload"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    members.push(open_durable_member("n3", &ids, &bus, dir, 64).await);
    let mapping = wait_body_contains(&members[2].app, "/orders/_mapping", "title").await;
    assert!(
        mapping.to_string().contains("title"),
        "n3 must apply the compacted create: {mapping}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if members[2].handle.snapshot_index() >= 1 {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "n3 did not install the snapshot: snapshot={} log_len={}",
                members[2].handle.snapshot_index(),
                members[2].handle.log_len()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (status, body) = put_json(
        &members[2].app,
        "/orders/_mapping",
        json!({ "properties": { "note": { "type": "keyword" } } }),
    )
    .await;
    assert!(
        status.is_success(),
        "mapping after snapshot: {status} {body}"
    );
    for member in &members {
        wait_body_contains(&member.app, "/orders/_mapping", "note").await;
    }

    let (status, body) = put_json(
        &members[2].app,
        "/orders/_doc/1",
        json!({ "title": "after" }),
    )
    .await;
    assert!(
        status.is_success(),
        "write from the late member: {status} {body}"
    );
    for member in &members {
        let (status, body) = get(&member.app, "/orders/_doc/1").await;
        assert!(
            status.is_success(),
            "doc readable on {}: {status} {body}",
            member.handle.node_id()
        );
    }

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

async fn replica_title(member: &Member, shard: u32, id: &str) -> Option<String> {
    let store = xerj_cluster::replica_index_name("orders", shard)?;
    let Ok(idx) = member.engine.get_index(&store) else {
        return None;
    };
    idx.get_document(id)
        .await
        .expect("replica get")
        .and_then(|doc| {
            doc.get("title")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
}

async fn wait_replica_title(member: &Member, shard: u32, id: &str, want: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let got = replica_title(member, shard, id).await;
        if got.as_deref() == want {
            return;
        }
        if Instant::now() > deadline {
            panic!("replica title of {id} on shard {shard} stayed {got:?}, wanted {want:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// n3 is in the configured membership and is not started until after the
/// documents exist. Its replica store is created empty by the create entry.
/// Backfill then copies the primary's live set, including a delete that
/// landed before n3 started, and publishes n3 into the in-sync set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_member_copies_documents_written_before_it_joined() {
    let ids = ["n1", "n2", "n3"];
    let bus = InMemoryBus::new();
    let mut members = Vec::new();
    for id in ["n1", "n2"] {
        let dir = tempfile::tempdir().expect("tempdir");
        members.push(open_durable_member(id, &ids, &bus, dir, 64).await);
    }
    let _leader = wait_leader(&members, &[false, false]).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({
            "settings": { "index": { "number_of_replicas": 2 } },
            "mappings": { "properties": { "title": { "type": "text" } } }
        }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut keep: Option<(String, u32, String, Vec<String>)> = None;
    let mut gone_id = None;
    for n in 0..256 {
        let id = format!("b{n}");
        let Some((shard, owner)) = members[0].engine.primary_of("orders", &id) else {
            continue;
        };
        if owner == "n3" {
            continue;
        }
        let replicas = members[0].engine.placed_replica_nodes("orders", shard);
        if !replicas.iter().any(|replica| replica == "n3") {
            continue;
        }
        if keep.is_none() {
            keep = Some((id, shard, owner, replicas));
            continue;
        }
        if keep
            .as_ref()
            .is_some_and(|(_, shard0, _, _)| *shard0 == shard)
        {
            gone_id = Some(id);
            break;
        }
    }
    let (keep_id, shard, owner, replicas) = keep.expect("a shard whose replica list includes n3");
    let gone_id = gone_id.expect("a second id on that shard");
    let live: Vec<String> = replicas.into_iter().filter(|id| id != "n3").collect();
    assert_eq!(live.len(), 1, "one replica is up while n3 is stopped");

    let (status, body) = put_json(
        &members[0].app,
        &format!("/orders/_doc/{keep_id}"),
        json!({ "title": "kept" }),
    )
    .await;
    assert!(status.is_success(), "write kept: {status} {body}");
    let (status, body) = put_json(
        &members[0].app,
        &format!("/orders/_doc/{gone_id}"),
        json!({ "title": "gone" }),
    )
    .await;
    assert!(status.is_success(), "write gone: {status} {body}");
    let (status, body) = delete(&members[0].app, &format!("/orders/_doc/{gone_id}")).await;
    assert!(status.is_success(), "delete gone: {status} {body}");
    wait_in_sync(&members, shard, &live).await;

    let live_member = members
        .iter()
        .find(|member| member.handle.node_id() == live[0])
        .expect("live replica");
    assert_eq!(
        replica_title(live_member, shard, &keep_id).await.as_deref(),
        Some("kept"),
        "the replica that acked the write has the document before n3 starts"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    members.push(open_durable_member("n3", &ids, &bus, dir, 64).await);
    let late = members.len() - 1;
    wait_replica_title(&members[late], shard, &keep_id, Some("kept")).await;
    let gone = replica_title(&members[late], shard, &gone_id).await;
    assert!(
        gone.is_none(),
        "a document deleted before n3 joined is not copied: {gone:?}"
    );
    let on_user = members[late]
        .engine
        .get_index("orders")
        .expect("user index")
        .get_document(&keep_id)
        .await
        .expect("user get");
    assert!(
        on_user.is_none(),
        "the backfill writes the replica store, not the user index: {on_user:?}"
    );
    let mut want = live.clone();
    want.push("n3".to_string());
    wait_in_sync(&members, shard, &want).await;

    let primary_at = members
        .iter()
        .position(|member| member.handle.node_id() == owner)
        .expect("primary");
    let live_at = members
        .iter()
        .position(|member| member.handle.node_id() == live[0])
        .expect("live replica");
    members[primary_at].handle.clear_shard_handler();
    members[live_at].handle.clear_shard_handler();
    let (status, body) = get(&members[late].app, &format!("/orders/_doc/{keep_id}")).await;
    assert!(
        status.is_success(),
        "GET uses the copy backfill put on n3: {status} {body}"
    );
    assert_eq!(
        body.pointer("/_source/title")
            .and_then(|value| value.as_str()),
        Some("kept"),
        "{body}"
    );

    members[primary_at]
        .handle
        .set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(
            members[primary_at].state.clone(),
        )));
    members[live_at]
        .handle
        .set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(
            members[live_at].state.clone(),
        )));
    let _leader = wait_leader(&members, &[false, false, false]).await;
    let (status, body) = put_json(
        &members[late].app,
        &format!("/orders/_doc/{keep_id}"),
        json!({ "title": "after" }),
    )
    .await;
    assert!(
        status.is_success(),
        "write after n3 is in sync: {status} {body}"
    );
    assert_eq!(
        replica_title(&members[late], shard, &keep_id)
            .await
            .as_deref(),
        Some("after"),
        "the in-sync replica receives the next write"
    );
    assert!(
        members[primary_at]
            .engine
            .replica_nodes("orders", shard)
            .iter()
            .any(|id| id == "n3"),
        "the write did not drop n3 from the in-sync set"
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

/// A replica that missed an acknowledged write is out of the in-sync set.
/// When it answers again it copies the primary's live document and rejoins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replica_dropped_from_the_in_sync_set_copies_the_missed_write_and_rejoins() {
    let members = ring(true).await;
    let _leader = wait_leader(&members, &[false, false, false]).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({ "settings": { "index": { "number_of_replicas": 2 } } }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut placed = None;
    for n in 0..64 {
        let id = format!("f{n}");
        let Some(primary) = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id))
        else {
            continue;
        };
        let (shard, _) = members[primary]
            .engine
            .primary_of("orders", &id)
            .expect("placement");
        let replicas = members[primary]
            .engine
            .placed_replica_nodes("orders", shard);
        if replicas.len() != 2 {
            continue;
        }
        let at = |node: &str| {
            members
                .iter()
                .position(|member| member.handle.node_id() == node)
                .expect("replica is a member")
        };
        placed = Some((id, primary, at(&replicas[0]), at(&replicas[1]), shard));
        break;
    }
    let (id, primary, replica_a, replica_b, shard) = placed.expect("a shard with two replicas");

    let (status, body) = put_json(
        &members[primary].app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": "first" }),
    )
    .await;
    assert!(status.is_success(), "first write: {status} {body}");
    assert_eq!(
        replica_title(&members[replica_a], shard, &id)
            .await
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        replica_title(&members[replica_b], shard, &id)
            .await
            .as_deref(),
        Some("first")
    );

    members[replica_a].handle.clear_shard_handler();
    let (status, body) = put_json(
        &members[primary].app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": "second" }),
    )
    .await;
    assert!(
        status.is_success(),
        "primary plus the live replica is a quorum: {status} {body}"
    );
    assert_eq!(
        replica_title(&members[replica_a], shard, &id)
            .await
            .as_deref(),
        Some("first"),
        "the replica that missed the write still has the previous copy"
    );
    let live_id = members[replica_b].handle.node_id().to_string();
    wait_in_sync(&members, shard, std::slice::from_ref(&live_id)).await;

    members[replica_a]
        .handle
        .set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(
            members[replica_a].state.clone(),
        )));
    wait_replica_title(&members[replica_a], shard, &id, Some("second")).await;
    let mut want = vec![live_id, members[replica_a].handle.node_id().to_string()];
    wait_in_sync(&members, shard, &want).await;
    want.sort();
    assert_eq!(
        {
            let mut got = members[primary].engine.replica_nodes("orders", shard);
            got.sort();
            got
        },
        want
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

fn position_of(members: &[Member], id: &str) -> usize {
    members
        .iter()
        .position(|member| member.handle.node_id() == id)
        .unwrap_or_else(|| panic!("member {id} is not in this ring"))
}

async fn index_sources(member: &Member, index: &str) -> BTreeMap<String, Value> {
    let Ok(idx) = member.engine.get_index(index) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for id in idx.live_document_ids() {
        match idx.get_document(&id).await {
            Ok(Some(doc)) => {
                out.insert(id, doc);
            }
            Ok(None) => {}
            Err(err) => panic!("get {index}/{id}: {err}"),
        }
    }
    out
}

/// Live documents on the primary for one shard, keyed by id.
async fn primary_shard_sources(primary: &Member, shard: u32) -> BTreeMap<String, Value> {
    let mut out = index_sources(primary, "orders").await;
    out.retain(|id, _| {
        primary
            .engine
            .primary_of("orders", id)
            .is_some_and(|(doc_shard, _)| doc_shard == shard)
    });
    out
}

async fn replica_sources(replica: &Member, shard: u32) -> BTreeMap<String, Value> {
    let Some(store) = xerj_cluster::replica_index_name("orders", shard) else {
        return BTreeMap::new();
    };
    index_sources(replica, &store).await
}

/// Wait until `replica_id` holds the primary's live documents for `shard`
/// and the in-sync set includes it.
///
/// The in-sync bit is read before the stores. Publishing the replica while
/// its copy still differs fails this wait instead of being papered over by
/// a later copy.
async fn wait_refilled(primary: &Member, replica: &Member, shard: u32, replica_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let in_sync = primary
            .engine
            .replica_nodes("orders", shard)
            .iter()
            .any(|id| id == replica_id);
        let want = primary_shard_sources(primary, shard).await;
        let got = replica_sources(replica, shard).await;
        if in_sync && got != want {
            panic!(
                "replica {replica_id} is in the in-sync set for shard {shard} before its documents match the primary\n primary={want:?}\n replica={got:?}"
            );
        }
        if in_sync && got == want {
            assert!(
                !want.is_empty(),
                "refill compared two empty stores on shard {shard}"
            );
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "replica {replica_id} shard {shard} was not refilled and published\n in_sync={in_sync}\n primary={want:?}\n replica={got:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct ShardCase {
    primary_id: String,
    replica_a: String,
    replica_b: String,
    shard: u32,
    keep: String,
    change: String,
    drop_id: String,
    fresh: String,
}

/// Four document ids on one shard that has two replicas. Nothing is written.
fn shard_case(members: &[Member]) -> ShardCase {
    let mut found: Option<(String, u32)> = None;
    let mut ids = Vec::new();
    for n in 0..512 {
        let id = format!("r{n}");
        let Some((shard, owner)) = members[0].engine.primary_of("orders", &id) else {
            continue;
        };
        let replicas = members[0].engine.placed_replica_nodes("orders", shard);
        if replicas.len() != 2 {
            continue;
        }
        match &found {
            None => {
                found = Some((owner, shard));
                ids.push((id, replicas));
            }
            Some((owner0, shard0)) if owner == *owner0 && shard == *shard0 => {
                ids.push((id, replicas));
            }
            _ => continue,
        }
        if ids.len() == 4 {
            break;
        }
    }
    assert_eq!(ids.len(), 4, "need four ids on one replicated shard");
    let replicas = ids[0].1.clone();
    let (primary_id, shard) = found.expect("shard");
    ShardCase {
        primary_id,
        replica_a: replicas[0].clone(),
        replica_b: replicas[1].clone(),
        shard,
        keep: ids[0].0.clone(),
        change: ids[1].0.clone(),
        drop_id: ids[2].0.clone(),
        fresh: ids[3].0.clone(),
    }
}

async fn create_replicated_orders(members: &[Member]) {
    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({
            "settings": { "index": { "number_of_replicas": 2 } },
            "mappings": { "properties": { "title": { "type": "text" } } }
        }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }
}

async fn put_title(app: &Router, id: &str, title: &str) {
    let (status, body) = put_json(
        app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": title }),
    )
    .await;
    assert!(status.is_success(), "write {id}: {status} {body}");
}

fn restore_handler(member: &Member) {
    member
        .handle
        .set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(
            member.state.clone(),
        )));
}

/// The replica misses an insert, an update, and a delete. When it can answer
/// again its store matches the primary, and only then is it in the in-sync set.
///
/// `bus` set drops the replica process and opens the same data directory
/// after the missed writes. `None` clears the shard handler and puts it
/// back, the in-process stand-in for a member that stopped answering.
async fn refill_after_missed_writes(mut members: Vec<Member>, bus: Option<&InMemoryBus>) {
    let case = shard_case(&members);
    put_title(&members[0].app, &case.keep, "kept").await;
    put_title(&members[0].app, &case.change, "v1").await;
    put_title(&members[0].app, &case.drop_id, "gone").await;

    let replica_a_at = position_of(&members, &case.replica_a);
    let replica_b_at = position_of(&members, &case.replica_b);
    for (id, title) in [
        (case.keep.as_str(), "kept"),
        (case.change.as_str(), "v1"),
        (case.drop_id.as_str(), "gone"),
    ] {
        assert_eq!(
            replica_title(&members[replica_a_at], case.shard, id)
                .await
                .as_deref(),
            Some(title),
            "{id} should be on the replica before it drops out"
        );
        assert_eq!(
            replica_title(&members[replica_b_at], case.shard, id)
                .await
                .as_deref(),
            Some(title)
        );
    }

    let held = if let Some(bus) = bus {
        let dir = shutdown_member(members.remove(replica_a_at)).await;
        let _leader = wait_leader(&members, &[false, false]).await;
        Some((bus, dir))
    } else {
        members[replica_a_at].handle.clear_shard_handler();
        None
    };

    put_title(&members[0].app, &case.change, "v2").await;
    let (status, body) = delete(&members[0].app, &format!("/orders/_doc/{}", case.drop_id)).await;
    assert!(status.is_success(), "delete: {status} {body}");
    put_title(&members[0].app, &case.fresh, "new").await;

    let replica_b_at = position_of(&members, &case.replica_b);
    wait_in_sync(&members, case.shard, std::slice::from_ref(&case.replica_b)).await;
    assert_eq!(
        replica_title(&members[replica_b_at], case.shard, &case.change)
            .await
            .as_deref(),
        Some("v2"),
        "the replica that stayed up has the update"
    );
    assert!(
        replica_title(&members[replica_b_at], case.shard, &case.drop_id)
            .await
            .is_none(),
        "the replica that stayed up dropped the deleted document"
    );
    assert_eq!(
        replica_title(&members[replica_b_at], case.shard, &case.fresh)
            .await
            .as_deref(),
        Some("new")
    );
    if held.is_none() {
        let stale = position_of(&members, &case.replica_a);
        assert_eq!(
            replica_title(&members[stale], case.shard, &case.change)
                .await
                .as_deref(),
            Some("v1"),
            "the replica that missed the write still has the previous copy"
        );
        assert_eq!(
            replica_title(&members[stale], case.shard, &case.drop_id)
                .await
                .as_deref(),
            Some("gone"),
            "the deleted document is still on the replica that missed it"
        );
        assert!(
            replica_title(&members[stale], case.shard, &case.fresh)
                .await
                .is_none(),
            "the inserted document is not on the replica that missed it"
        );
    }

    if let Some((bus, dir)) = held {
        // The aborted backfill task drops its engine Arc at the next await.
        // Opening the same directory before that releases `node.lock` fails.
        tokio::time::sleep(Duration::from_millis(200)).await;
        members.push(open_durable_member(&case.replica_a, &["n1", "n2", "n3"], bus, dir, 64).await);
    } else {
        restore_handler(&members[position_of(&members, &case.replica_a)]);
    }

    let primary_at = position_of(&members, &case.primary_id);
    let replica_a_at = position_of(&members, &case.replica_a);
    wait_refilled(
        &members[primary_at],
        &members[replica_a_at],
        case.shard,
        &case.replica_a,
    )
    .await;
    let on_user = members[replica_a_at]
        .engine
        .get_index("orders")
        .expect("user index")
        .get_document(&case.fresh)
        .await
        .expect("user get");
    assert!(
        on_user.is_none(),
        "refill writes the replica store, not the user index: {on_user:?}"
    );

    members[primary_at].handle.clear_shard_handler();
    let replica_b_at = position_of(&members, &case.replica_b);
    members[replica_b_at].handle.clear_shard_handler();
    for (id, title) in [
        (case.keep.as_str(), "kept"),
        (case.change.as_str(), "v2"),
        (case.fresh.as_str(), "new"),
    ] {
        let (status, body) = get(&members[replica_a_at].app, &format!("/orders/_doc/{id}")).await;
        assert!(
            status.is_success(),
            "GET {id} uses the refilled replica: {status} {body}"
        );
        assert_eq!(
            body.pointer("/_source/title")
                .and_then(|value| value.as_str()),
            Some(title),
            "{body}"
        );
    }
    let (status, body) = get(
        &members[replica_a_at].app,
        &format!("/orders/_doc/{}", case.drop_id),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the document deleted while the replica was out is not served from it: {body}"
    );

    restore_handler(&members[primary_at]);
    restore_handler(&members[replica_b_at]);
    let dead = vec![false; members.len()];
    let _leader = wait_leader(&members, &dead).await;
    put_title(&members[replica_a_at].app, &case.change, "v3").await;
    assert_eq!(
        replica_title(&members[replica_a_at], case.shard, &case.change)
            .await
            .as_deref(),
        Some("v3"),
        "once the replica is in sync it receives the next write"
    );
    assert!(
        members[primary_at]
            .engine
            .replica_nodes("orders", case.shard)
            .iter()
            .any(|id| id == &case.replica_a),
        "the write did not drop the refilled replica from the in-sync set"
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_replica_is_refilled_to_the_primary_live_set_before_it_rejoins() {
    let members = ring(true).await;
    let _leader = wait_leader(&members, &[false, false, false]).await;
    create_replicated_orders(&members).await;
    refill_after_missed_writes(members, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_replica_is_refilled_to_the_primary_live_set_before_it_rejoins() {
    let ids = ["n1", "n2", "n3"];
    let bus = InMemoryBus::new();
    let mut members = Vec::new();
    for id in ids {
        let dir = tempfile::tempdir().expect("tempdir");
        members.push(open_durable_member(id, &ids, &bus, dir, 64).await);
    }
    let _leader = wait_leader(&members, &[false, false, false]).await;
    create_replicated_orders(&members).await;
    refill_after_missed_writes(members, Some(&bus)).await;
}

fn bulk_item(body: &Value, at: usize) -> &Value {
    let item = body
        .pointer(&format!("/items/{at}"))
        .unwrap_or_else(|| panic!("bulk response has no item {at}: {body}"));
    item.as_object()
        .and_then(|obj| obj.values().next())
        .unwrap_or_else(|| panic!("bulk item {at} has no action: {item}"))
}

async fn user_source(member: &Member, id: &str) -> Option<Value> {
    member
        .engine
        .get_index("orders")
        .expect("orders is open")
        .get_document(id)
        .await
        .expect("get")
}

async fn replica_source(member: &Member, shard: u32, id: &str) -> Option<Value> {
    let store = xerj_cluster::replica_index_name("orders", shard).expect("store name");
    member
        .engine
        .get_index(&store)
        .unwrap_or_else(|_| panic!("replica store {store} is not open"))
        .get_document(id)
        .await
        .expect("replica get")
}

/// `_bulk` and `_update` accepted by a node that does not own the id are
/// applied on the primary and copied onto the in-sync replica. The receiving
/// node's user index does not keep them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_and_update_from_a_non_owner_write_the_primary_and_the_replica() {
    let members = ring(true).await;
    let _leader = wait_leader(&members, &[false, false, false]).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({
            "settings": { "index": { "number_of_replicas": 1 } },
            "mappings": { "properties": { "title": { "type": "text" } } }
        }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut placed: Vec<(String, usize, usize, u32)> = Vec::new();
    for n in 0..128 {
        let id = format!("b{n}");
        let Some(primary) = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id))
        else {
            continue;
        };
        let (shard, _) = members[primary]
            .engine
            .primary_of("orders", &id)
            .expect("placement");
        let replicas = members[primary].engine.replica_nodes("orders", shard);
        if replicas.len() != 1 {
            continue;
        }
        let Some(replica) = members
            .iter()
            .position(|member| member.handle.node_id() == replicas[0])
        else {
            continue;
        };
        if placed.iter().any(|(_, owner, _, _)| *owner == primary) {
            continue;
        }
        placed.push((id, primary, replica, shard));
        if placed.len() == 2 {
            break;
        }
    }
    assert_eq!(placed.len(), 2, "two primaries: {placed:?}");
    let (id_a, primary_a, replica_a, shard_a) = placed[0].clone();
    let (id_b, primary_b, replica_b, shard_b) = placed[1].clone();
    assert_ne!(primary_a, primary_b);
    let writer = (0..3)
        .find(|n| *n != primary_a && *n != primary_b)
        .expect("a node that owns neither id");

    let bulk = format!(
        "{{\"index\":{{\"_index\":\"orders\",\"_id\":\"{id_a}\"}}}}\n\
         {{\"title\":\"bulk\",\"n\":1}}\n\
         {{\"create\":{{\"_index\":\"orders\",\"_id\":\"{id_b}\"}}}}\n\
         {{\"title\":\"other\"}}\n\
         {{\"update\":{{\"_index\":\"orders\",\"_id\":\"{id_a}\"}}}}\n\
         {{\"doc\":{{\"via\":\"bulk-update\"}}}}\n\
         {{\"index\":{{\"_index\":\"orders\"}}}}\n\
         {{\"title\":\"auto\"}}\n"
    );
    let (status, body) = post_ndjson(&members[writer].app, "/_bulk", &bulk).await;
    assert!(status.is_success(), "bulk from non-owner: {status} {body}");
    assert_eq!(
        body.pointer("/errors").and_then(|v| v.as_bool()),
        Some(false),
        "{body}"
    );
    assert_eq!(
        bulk_item(&body, 0).get("result").and_then(|v| v.as_str()),
        Some("created"),
        "{body}"
    );
    assert_eq!(
        bulk_item(&body, 1).get("result").and_then(|v| v.as_str()),
        Some("created"),
        "{body}"
    );
    assert_eq!(
        bulk_item(&body, 2).get("result").and_then(|v| v.as_str()),
        Some("updated"),
        "bulk update merged on the primary: {body}"
    );
    let auto_id = bulk_item(&body, 3)
        .get("_id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("auto id: {body}"))
        .to_string();
    assert!(!auto_id.is_empty());

    let on_primary = user_source(&members[primary_a], &id_a).await;
    assert_eq!(
        on_primary
            .as_ref()
            .and_then(|doc| doc.get("via"))
            .and_then(|v| v.as_str()),
        Some("bulk-update"),
        "primary has the bulk update: {on_primary:?}"
    );
    let on_replica = replica_source(&members[replica_a], shard_a, &id_a).await;
    assert_eq!(
        on_replica
            .as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("bulk"),
        "replica copied the bulk update: {on_replica:?}"
    );
    assert_eq!(
        on_replica
            .as_ref()
            .and_then(|doc| doc.get("via"))
            .and_then(|v| v.as_str()),
        Some("bulk-update")
    );
    for (at, member) in members.iter().enumerate() {
        if at == primary_a {
            continue;
        }
        assert!(
            user_source(member, &id_a).await.is_none(),
            "node {at} user index stored {id_a}"
        );
    }
    let on_b = replica_source(&members[replica_b], shard_b, &id_b).await;
    assert_eq!(
        on_b.as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("other"),
        "create in the same bulk reached the other primary's replica: {on_b:?}"
    );
    assert!(
        user_source(&members[writer], &id_b).await.is_none(),
        "the receiver stored {id_b}"
    );

    let mut holders = Vec::new();
    for (at, member) in members.iter().enumerate() {
        if user_source(member, &auto_id).await.is_some() {
            holders.push(at);
        }
    }
    assert_eq!(
        holders,
        vec![members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &auto_id))
            .expect("auto-id has a primary")],
        "an id assigned before routing has one user-index copy"
    );
    let auto_primary = holders[0];
    let (auto_shard, _) = members[auto_primary]
        .engine
        .primary_of("orders", &auto_id)
        .expect("auto placement");
    let auto_replicas = members[auto_primary]
        .engine
        .replica_nodes("orders", auto_shard);
    assert_eq!(auto_replicas.len(), 1);
    let auto_replica = members
        .iter()
        .position(|member| member.handle.node_id() == auto_replicas[0])
        .expect("auto replica");
    let auto_copy = replica_source(&members[auto_replica], auto_shard, &auto_id).await;
    assert_eq!(
        auto_copy
            .as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("auto"),
        "{auto_copy:?}"
    );

    let overwrite = format!(
        "{{\"index\":{{\"_index\":\"orders\",\"_id\":\"{id_a}\"}}}}\n\
         {{\"title\":\"from-primary\",\"n\":1}}\n"
    );
    let (status, body) = post_ndjson(&members[primary_a].app, "/orders/_bulk", &overwrite).await;
    assert!(status.is_success(), "bulk on the primary: {status} {body}");
    assert_eq!(
        body.pointer("/errors").and_then(|v| v.as_bool()),
        Some(false),
        "{body}"
    );
    let on_replica = replica_source(&members[replica_a], shard_a, &id_a).await;
    assert_eq!(
        on_replica
            .as_ref()
            .and_then(|doc| doc.get("title"))
            .and_then(|v| v.as_str()),
        Some("from-primary"),
        "a bulk the primary itself accepts is still copied: {on_replica:?}"
    );

    let (status, body) = post_json(
        &members[writer].app,
        &format!("/orders/_update/{id_a}"),
        json!({ "doc": { "extra": "yes", "n": 2 } }),
    )
    .await;
    assert!(
        status.is_success(),
        "update from non-owner: {status} {body}"
    );
    assert_eq!(
        body.get("result").and_then(|v| v.as_str()),
        Some("updated"),
        "{body}"
    );
    let on_primary = user_source(&members[primary_a], &id_a)
        .await
        .expect("primary");
    assert_eq!(
        on_primary.get("extra").and_then(|v| v.as_str()),
        Some("yes"),
        "{on_primary}"
    );
    assert_eq!(on_primary.get("n").and_then(|v| v.as_u64()), Some(2));
    let on_replica = replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .expect("replica");
    assert_eq!(
        on_replica.get("extra").and_then(|v| v.as_str()),
        Some("yes")
    );
    assert_eq!(
        on_replica.get("title").and_then(|v| v.as_str()),
        Some("from-primary")
    );
    assert!(user_source(&members[writer], &id_a).await.is_none());

    let (status, body) = post_json(
        &members[writer].app,
        &format!("/orders/_update/{id_a}"),
        json!({ "script": { "source": "ctx._source.title = 'scripted'" } }),
    )
    .await;
    assert!(
        status.is_success(),
        "scripted update from non-owner: {status} {body}"
    );
    let on_primary = user_source(&members[primary_a], &id_a)
        .await
        .expect("primary");
    assert_eq!(
        on_primary.get("title").and_then(|v| v.as_str()),
        Some("scripted"),
        "the script ran where the document is: {on_primary}"
    );
    assert_eq!(
        on_primary.get("extra").and_then(|v| v.as_str()),
        Some("yes")
    );
    let on_replica = replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .expect("replica");
    assert_eq!(
        on_replica.get("title").and_then(|v| v.as_str()),
        Some("scripted"),
        "{on_replica}"
    );

    let deletion = format!("{{\"delete\":{{\"_index\":\"orders\",\"_id\":\"{id_b}\"}}}}\n");
    let (status, body) = post_ndjson(&members[writer].app, "/_bulk", &deletion).await;
    assert!(status.is_success(), "bulk delete: {status} {body}");
    assert_eq!(
        body.pointer("/errors").and_then(|v| v.as_bool()),
        Some(false),
        "{body}"
    );
    assert!(user_source(&members[primary_b], &id_b).await.is_none());
    assert!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .is_none(),
        "the replica still has the deleted document"
    );
    let (status, body) = get(&members[writer].app, &format!("/orders/_doc/{id_b}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    members[primary_a].handle.clear_shard_handler();
    let (status, body) = get(&members[writer].app, &format!("/orders/_doc/{id_a}")).await;
    assert!(
        status.is_success(),
        "GET after the primary stops reads the copy: {status} {body}"
    );
    assert_eq!(
        body.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("scripted"),
        "{body}"
    );
    let again = format!(
        "{{\"index\":{{\"_index\":\"orders\",\"_id\":\"{id_a}\"}}}}\n\
         {{\"title\":\"no-primary\"}}\n"
    );
    let (status, body) = post_ndjson(&members[writer].app, "/_bulk", &again).await;
    assert!(status.is_success(), "bulk HTTP status: {status} {body}");
    assert_eq!(
        body.pointer("/errors").and_then(|v| v.as_bool()),
        Some(true),
        "{body}"
    );
    assert_eq!(
        bulk_item(&body, 0).get("status").and_then(|v| v.as_u64()),
        Some(503),
        "a bulk write still needs the primary: {body}"
    );
    let on_replica = replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .expect("replica");
    assert_eq!(
        on_replica.get("title").and_then(|v| v.as_str()),
        Some("scripted"),
        "the refused bulk did not change the copy: {on_replica}"
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

/// `_update_by_query` accepted by a node that owns neither document updates
/// every primary and copies the new source onto the in-sync replica.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_by_query_from_a_non_owner_updates_every_primary_and_replica() {
    let members = ring(true).await;
    let _leader = wait_leader(&members, &[false, false, false]).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({
            "settings": { "index": { "number_of_replicas": 1 } },
            "mappings": { "properties": { "title": { "type": "text" } } }
        }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut placed: Vec<(String, usize, usize, u32)> = Vec::new();
    for n in 0..128 {
        let id = format!("u{n}");
        let Some(primary) = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id))
        else {
            continue;
        };
        let (shard, _) = members[primary]
            .engine
            .primary_of("orders", &id)
            .expect("placement");
        let replicas = members[primary].engine.replica_nodes("orders", shard);
        if replicas.len() != 1 {
            continue;
        }
        let Some(replica) = members
            .iter()
            .position(|member| member.handle.node_id() == replicas[0])
        else {
            continue;
        };
        if placed.iter().any(|(_, owner, _, _)| *owner == primary) {
            continue;
        }
        placed.push((id, primary, replica, shard));
        if placed.len() == 2 {
            break;
        }
    }
    assert_eq!(placed.len(), 2, "two primaries: {placed:?}");
    let (id_a, primary_a, replica_a, shard_a) = placed[0].clone();
    let (id_b, primary_b, replica_b, shard_b) = placed[1].clone();
    assert_ne!(primary_a, primary_b);
    assert_ne!(replica_a, replica_b);
    let writer = (0..3)
        .find(|n| *n != primary_a && *n != primary_b)
        .expect("a node that owns neither id");

    let (status, body) = put_json(
        &members[writer].app,
        &format!("/orders/_doc/{id_a}"),
        json!({ "title": "alpha" }),
    )
    .await;
    assert!(status.is_success(), "index {id_a}: {status} {body}");
    let (status, body) = put_json(
        &members[writer].app,
        &format!("/orders/_doc/{id_b}"),
        json!({ "title": "beta" }),
    )
    .await;
    assert!(status.is_success(), "index {id_b}: {status} {body}");
    assert_eq!(
        user_source(&members[primary_a], &id_a)
            .await
            .expect("primary a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("alpha")
    );
    assert!(
        user_source(&members[writer], &id_a).await.is_none(),
        "the receiver stored {id_a}"
    );

    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_update_by_query",
        json!({
            "query": { "match_all": {} },
            "script": { "source": "ctx._source.title = 'all'" }
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "update_by_query from non-owner: {status} {body}"
    );
    assert_eq!(
        body.get("updated").and_then(|v| v.as_u64()),
        Some(2),
        "{body}"
    );
    assert_eq!(
        body.get("failures")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(0),
        "{body}"
    );
    assert_eq!(
        user_source(&members[primary_a], &id_a)
            .await
            .expect("primary a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("all")
    );
    assert_eq!(
        user_source(&members[primary_b], &id_b)
            .await
            .expect("primary b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("all")
    );
    assert_eq!(
        replica_source(&members[replica_a], shard_a, &id_a)
            .await
            .expect("replica a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("all")
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("all")
    );
    assert!(user_source(&members[writer], &id_a).await.is_none());
    assert!(user_source(&members[writer], &id_b).await.is_none());
    let (status, got) = get(&members[writer].app, &format!("/orders/_doc/{id_a}")).await;
    assert!(
        status.is_success(),
        "GET after update_by_query: {status} {got}"
    );
    assert_eq!(
        got.pointer("/_source/title").and_then(|v| v.as_str()),
        Some("all"),
        "{got}"
    );

    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_update_by_query",
        json!({
            "query": { "ids": { "values": [id_a] } },
            "script": { "source": "ctx._source.title = 'only-a'" }
        }),
    )
    .await;
    assert!(status.is_success(), "ids update_by_query: {status} {body}");
    assert_eq!(
        body.get("updated").and_then(|v| v.as_u64()),
        Some(1),
        "{body}"
    );
    assert_eq!(
        user_source(&members[primary_a], &id_a)
            .await
            .expect("primary a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("only-a")
    );
    assert_eq!(
        replica_source(&members[replica_a], shard_a, &id_a)
            .await
            .expect("replica a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("only-a")
    );
    assert_eq!(
        user_source(&members[primary_b], &id_b)
            .await
            .expect("primary b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("all"),
        "the other shard stays as it was"
    );

    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_update_by_query",
        json!({
            "query": { "match_all": {} },
            "max_docs": 1,
            "script": { "source": "ctx._source.title = 'capped'" }
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "max_docs update_by_query: {status} {body}"
    );
    assert_eq!(
        body.get("updated").and_then(|v| v.as_u64()),
        Some(1),
        "max_docs is one budget for the index, not one per shard: {body}"
    );
    let title_a = user_source(&members[primary_a], &id_a)
        .await
        .expect("primary a")
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title_b = user_source(&members[primary_b], &id_b)
        .await
        .expect("primary b")
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let capped = [&title_a, &title_b]
        .iter()
        .filter(|title| title.as_str() == "capped")
        .count();
    assert_eq!(capped, 1, "titles {title_a} {title_b}");
    assert_eq!(
        replica_source(&members[replica_a], shard_a, &id_a)
            .await
            .expect("replica a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some(title_a.as_str())
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some(title_b.as_str())
    );

    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_update_by_query?wait_for_completion=false",
        json!({
            "query": { "match_all": {} },
            "script": { "source": "ctx._source.title = 'async'" }
        }),
    )
    .await;
    assert!(
        status.is_success(),
        "async update_by_query: {status} {body}"
    );
    let task = body
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("task id: {body}"))
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        let (status, got) = get(&members[writer].app, &format!("/_tasks/{task}")).await;
        if status.is_success() && got.get("completed").and_then(|v| v.as_bool()) == Some(true) {
            break got;
        }
        if Instant::now() > deadline {
            panic!("task {task} did not finish: {status} {got}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        completed
            .pointer("/response/updated")
            .and_then(|v| v.as_u64()),
        Some(2),
        "{completed}"
    );
    assert_eq!(
        user_source(&members[primary_a], &id_a)
            .await
            .expect("primary a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("async")
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("async")
    );

    members[primary_a].handle.clear_shard_handler();
    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_update_by_query",
        json!({
            "query": { "match_all": {} },
            "script": { "source": "ctx._source.title = 'down'" }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a primary that does not answer refuses before writing: {body}"
    );
    assert_eq!(
        user_source(&members[primary_a], &id_a)
            .await
            .expect("primary a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("async"),
        "the down primary was not written"
    );
    assert_eq!(
        replica_source(&members[replica_a], shard_a, &id_a)
            .await
            .expect("replica a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("async"),
        "the refused update did not change the copy"
    );
    assert_eq!(
        user_source(&members[primary_b], &id_b)
            .await
            .expect("primary b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("async"),
        "a later primary is not written once an earlier one cannot be checked"
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("async")
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}

fn restore_shard_handler(member: &Member) {
    member
        .handle
        .set_shard_handler(std::sync::Arc::new(ApiShardHandler::new(
            member.state.clone(),
        )));
}

async fn index_titled(app: &Router, id: &str, title: &str) {
    let (status, body) = put_json(
        app,
        &format!("/orders/_doc/{id}"),
        json!({ "title": title }),
    )
    .await;
    assert!(status.is_success(), "index {id}: {status} {body}");
}

/// `_delete_by_query` accepted by a node that owns neither document deletes
/// on every primary and copies each delete onto the in-sync replica.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_by_query_from_a_non_owner_deletes_on_every_primary_and_replica() {
    let members = ring(true).await;
    let _leader = wait_leader(&members, &[false, false, false]).await;

    let (status, body) = put_json(
        &members[0].app,
        "/orders",
        json!({
            "settings": { "index": { "number_of_replicas": 1 } },
            "mappings": { "properties": { "title": { "type": "text" } } }
        }),
    )
    .await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_status(&member.app, "/orders", StatusCode::OK).await;
    }

    let mut placed: Vec<(String, usize, usize, u32)> = Vec::new();
    for n in 0..128 {
        let id = format!("d{n}");
        let Some(primary) = members
            .iter()
            .position(|member| member.engine.owns_shard("orders", &id))
        else {
            continue;
        };
        let (shard, _) = members[primary]
            .engine
            .primary_of("orders", &id)
            .expect("placement");
        let replicas = members[primary].engine.replica_nodes("orders", shard);
        if replicas.len() != 1 {
            continue;
        }
        let Some(replica) = members
            .iter()
            .position(|member| member.handle.node_id() == replicas[0])
        else {
            continue;
        };
        if placed.iter().any(|(_, owner, _, _)| *owner == primary) {
            continue;
        }
        placed.push((id, primary, replica, shard));
        if placed.len() == 2 {
            break;
        }
    }
    assert_eq!(placed.len(), 2, "two primaries: {placed:?}");
    let (id_a, primary_a, replica_a, shard_a) = placed[0].clone();
    let (id_b, primary_b, replica_b, shard_b) = placed[1].clone();
    assert_ne!(primary_a, primary_b);
    assert_ne!(replica_a, replica_b);
    let writer = (0..3)
        .find(|n| *n != primary_a && *n != primary_b)
        .expect("a node that owns neither id");

    index_titled(&members[writer].app, &id_a, "alpha").await;
    index_titled(&members[writer].app, &id_b, "beta").await;
    assert!(
        user_source(&members[writer], &id_a).await.is_none(),
        "the receiver stored {id_a}"
    );

    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_delete_by_query",
        json!({ "query": { "match_all": {} } }),
    )
    .await;
    assert!(
        status.is_success(),
        "delete_by_query from non-owner: {status} {body}"
    );
    assert_eq!(
        body.get("deleted").and_then(|v| v.as_u64()),
        Some(2),
        "{body}"
    );
    assert_eq!(
        body.get("failures")
            .and_then(|v| v.as_array())
            .map(|items| items.len()),
        Some(0),
        "{body}"
    );
    assert!(user_source(&members[primary_a], &id_a).await.is_none());
    assert!(user_source(&members[primary_b], &id_b).await.is_none());
    assert!(replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .is_none());
    assert!(replica_source(&members[replica_b], shard_b, &id_b)
        .await
        .is_none());
    assert!(user_source(&members[writer], &id_a).await.is_none());
    assert!(user_source(&members[writer], &id_b).await.is_none());
    let (status, got) = get(&members[writer].app, &format!("/orders/_doc/{id_a}")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "GET after delete_by_query: {status} {got}"
    );

    members[primary_a].handle.clear_shard_handler();
    let (status, got) = get(&members[writer].app, &format!("/orders/_doc/{id_a}")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a primary that is down still does not serve a copy the delete removed: {status} {got}"
    );
    restore_shard_handler(&members[primary_a]);

    index_titled(&members[writer].app, &id_a, "alpha").await;
    index_titled(&members[writer].app, &id_b, "beta").await;
    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_delete_by_query",
        json!({ "query": { "ids": { "values": [id_a] } } }),
    )
    .await;
    assert!(status.is_success(), "ids delete_by_query: {status} {body}");
    assert_eq!(
        body.get("deleted").and_then(|v| v.as_u64()),
        Some(1),
        "{body}"
    );
    assert!(user_source(&members[primary_a], &id_a).await.is_none());
    assert!(replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .is_none());
    assert_eq!(
        user_source(&members[primary_b], &id_b)
            .await
            .expect("primary b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("beta"),
        "the other shard stays"
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("beta")
    );

    index_titled(&members[writer].app, &id_a, "alpha").await;
    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_delete_by_query",
        json!({ "query": { "match": { "title": "alpha" } } }),
    )
    .await;
    assert!(
        status.is_success(),
        "match delete_by_query: {status} {body}"
    );
    assert_eq!(
        body.get("deleted").and_then(|v| v.as_u64()),
        Some(1),
        "a non-ids query still deletes on the owning primary: {body}"
    );
    assert!(user_source(&members[primary_a], &id_a).await.is_none());
    assert!(replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .is_none());
    assert_eq!(
        user_source(&members[primary_b], &id_b)
            .await
            .expect("primary b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("beta")
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("beta")
    );

    index_titled(&members[writer].app, &id_a, "alpha").await;
    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_delete_by_query",
        json!({ "query": { "match_all": {} }, "max_docs": 1 }),
    )
    .await;
    assert!(
        status.is_success(),
        "max_docs delete_by_query: {status} {body}"
    );
    assert_eq!(
        body.get("deleted").and_then(|v| v.as_u64()),
        Some(1),
        "max_docs is one budget for the index, not one per shard: {body}"
    );
    let left_a = user_source(&members[primary_a], &id_a).await.is_some();
    let left_b = user_source(&members[primary_b], &id_b).await.is_some();
    assert_eq!(
        left_a as u8 + left_b as u8,
        1,
        "exactly one primary still has its document"
    );
    assert_eq!(
        replica_source(&members[replica_a], shard_a, &id_a)
            .await
            .is_some(),
        left_a,
        "replica a matches the primary"
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .is_some(),
        left_b,
        "replica b matches the primary"
    );

    index_titled(&members[writer].app, &id_a, "async").await;
    index_titled(&members[writer].app, &id_b, "async").await;
    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_delete_by_query?wait_for_completion=false",
        json!({ "query": { "match_all": {} } }),
    )
    .await;
    assert!(
        status.is_success(),
        "async delete_by_query: {status} {body}"
    );
    let task = body
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("task id: {body}"))
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        let (status, got) = get(&members[writer].app, &format!("/_tasks/{task}")).await;
        if status.is_success() && got.get("completed").and_then(|v| v.as_bool()) == Some(true) {
            break got;
        }
        if Instant::now() > deadline {
            panic!("task {task} did not finish: {status} {got}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        completed
            .pointer("/response/deleted")
            .and_then(|v| v.as_u64()),
        Some(2),
        "{completed}"
    );
    assert!(user_source(&members[primary_a], &id_a).await.is_none());
    assert!(user_source(&members[primary_b], &id_b).await.is_none());
    assert!(replica_source(&members[replica_a], shard_a, &id_a)
        .await
        .is_none());
    assert!(replica_source(&members[replica_b], shard_b, &id_b)
        .await
        .is_none());

    index_titled(&members[writer].app, &id_a, "stay").await;
    index_titled(&members[writer].app, &id_b, "stay").await;
    members[primary_a].handle.clear_shard_handler();
    let (status, body) = post_json(
        &members[writer].app,
        "/orders/_delete_by_query",
        json!({ "query": { "match_all": {} } }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a primary that does not answer refuses before writing: {body}"
    );
    assert_eq!(
        user_source(&members[primary_a], &id_a)
            .await
            .expect("primary a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("stay"),
        "the down primary was not deleted"
    );
    assert_eq!(
        replica_source(&members[replica_a], shard_a, &id_a)
            .await
            .expect("replica a")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("stay")
    );
    assert_eq!(
        user_source(&members[primary_b], &id_b)
            .await
            .expect("primary b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("stay"),
        "a later primary is not deleted once an earlier one cannot be checked"
    );
    assert_eq!(
        replica_source(&members[replica_b], shard_b, &id_b)
            .await
            .expect("replica b")
            .get("title")
            .and_then(|v| v.as_str()),
        Some("stay")
    );

    for member in &members {
        let _ = member.shutdown.send(true);
    }
}
