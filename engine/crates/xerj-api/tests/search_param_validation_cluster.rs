//! A search on a multi-node index takes the shard fan-out, which rebuilt
//! `size` and `from` from the body and never read the URL: `?size=-5` ran
//! there while a single node refused it, and `?size=1` returned every hit.

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
    engine: std::sync::Arc<xerj_engine::Engine>,
    handle: ClusterHandle,
    _shutdown: watch::Sender<bool>,
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
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
        let task = tokio::spawn(async move {
            runner.run().await;
        });
        members.push(Member {
            app: xerj_api::router::build_es_compat_router(state.clone()),
            engine: std::sync::Arc::clone(&state.engine),
            handle,
            _shutdown: shutdown,
            _runner: task,
            _dir: dir,
        });
    }
    members
}

async fn send(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(req.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cluster_search_refuses_what_a_single_node_refuses() {
    let members = ring().await;
    wait_for("one raft leader", || {
        members.iter().filter(|m| m.handle.is_leader()).count() == 1
    })
    .await;
    let (status, body) = send(&members[0].app, "PUT", "/orders", Some(json!({}))).await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_for("the index on every member", || {
            // The shard plan is installed before the index is created, so
            // wait for both.
            member.engine.shard_search_targets("orders").len() == 3
                && member.engine.get_index("orders").is_ok()
        })
        .await;
    }
    let (status, body) = send(
        &members[0].app,
        "PUT",
        "/orders/_doc/a",
        Some(json!({"t": 1})),
    )
    .await;
    assert!(status.is_success(), "write: {status} {body}");

    let app = &members[1].app;
    let mut wrong = Vec::new();
    for (path, reason) in [
        (
            "/orders/_search?size=-5",
            "[size] parameter cannot be negative, found [-5]",
        ),
        (
            "/orders/_search?from=-1",
            "[from] parameter cannot be negative but was [-1]",
        ),
        ("/orders/_search?terminate_after=-1", "terminateAfter must be > 0"),
        (
            "/orders/_search?track_total_hits=-2",
            "[track_total_hits] parameter must be positive or equals to -1, got -2",
        ),
        (
            "/orders/_search?rest_total_hits_as_int=true&track_total_hits=2",
            "[rest_total_hits_as_int] cannot be used if the tracking of total hits is not accurate, got 2",
        ),
    ] {
        let (status, body) = send(app, "GET", path, None).await;
        if status != StatusCode::BAD_REQUEST
            || body["error"]["type"] != "illegal_argument_exception"
            || body["error"]["reason"] != reason
        {
            wrong.push(format!("{path}: {status} {body}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "not refused as ES does:\n{}",
        wrong.join("\n")
    );

    // The fan-out still runs a search it accepts.
    let (status, body) = send(app, "GET", "/orders/_search?track_total_hits=-1", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cluster_search_takes_size_and_from_from_the_url() {
    let members = ring().await;
    wait_for("one raft leader", || {
        members.iter().filter(|m| m.handle.is_leader()).count() == 1
    })
    .await;
    let (status, body) = send(&members[0].app, "PUT", "/orders", Some(json!({}))).await;
    assert!(status.is_success(), "create: {status} {body}");
    for member in &members {
        wait_for("the index on every member", || {
            // The shard plan is installed before the index is created, so
            // wait for both.
            member.engine.shard_search_targets("orders").len() == 3
                && member.engine.get_index("orders").is_ok()
        })
        .await;
    }
    for i in 0..6 {
        let (status, body) = send(
            &members[0].app,
            "PUT",
            &format!("/orders/_doc/d{i}?refresh=true"),
            Some(json!({"t": i})),
        )
        .await;
        assert!(status.is_success(), "write: {status} {body}");
    }

    let app = &members[1].app;
    let hits = |body: &Value| body["hits"]["hits"].as_array().map(Vec::len);
    let mut wrong = Vec::new();
    for (method, path, request, expected) in [
        ("GET", "/orders/_search", None, 6),
        ("GET", "/orders/_search?size=1", None, 1),
        ("GET", "/orders/_search?from=5", None, 1),
        ("GET", "/orders/_search?from=2&size=3", None, 3),
        // The URL wins over the body, as on a single node.
        (
            "POST",
            "/orders/_search?size=2",
            Some(json!({"size": 4})),
            2,
        ),
        ("POST", "/orders/_search", Some(json!({"size": 4})), 4),
    ] {
        let (status, body) = send(app, method, path, request.clone()).await;
        if status != StatusCode::OK || hits(&body) != Some(expected) {
            wrong.push(format!(
                "{method} {path} {request:?}: want {expected} hits, got {status} {:?}",
                hits(&body)
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cluster_search_over_the_result_window_gets_es_reason() {
    let members = ring().await;
    wait_for("one raft leader", || {
        members.iter().filter(|m| m.handle.is_leader()).count() == 1
    })
    .await;
    for (index, settings) in [
        ("orders", json!({})),
        (
            "wide",
            json!({"settings": {"index": {"max_result_window": 20000}}}),
        ),
    ] {
        let (status, body) =
            send(&members[0].app, "PUT", &format!("/{index}"), Some(settings)).await;
        assert!(status.is_success(), "create {index}: {status} {body}");
        for member in &members {
            wait_for("the index on every member", || {
                // The shard plan is installed before the index is created, so
                // wait for both.
                // The settings land after the index itself does.
                member.engine.shard_search_targets(index).len() == 3
                    && member.engine.get_index(index).is_ok()
                    && (index != "wide" || member.engine.index_settings.contains_key(index))
            })
            .await;
        }
    }

    let too_large = |limit: usize, window: usize| {
        format!(
            "Result window is too large, from + size must be less than or equal to: [{limit}] \
             but was [{window}]. See the scroll api for a more efficient way to request large \
             data sets. This limit can be set by changing the [index.max_result_window] index \
             level setting."
        )
    };
    let app = &members[1].app;
    let mut wrong = Vec::new();
    for (method, path, request, reason) in [
        (
            "GET",
            "/orders/_search?size=10001",
            None,
            too_large(10000, 10001),
        ),
        (
            "GET",
            "/orders/_search?from=9995&size=10",
            None,
            too_large(10000, 10005),
        ),
        (
            "POST",
            "/orders/_search",
            Some(json!({"from": 10000, "size": 1})),
            too_large(10000, 10001),
        ),
        // The window is refused before a feature the fan-out cannot merge,
        // as a single node refuses it before running anything.
        (
            "POST",
            "/orders/_search?size=10001",
            Some(json!({"aggs": {"n": {"value_count": {"field": "t"}}}})),
            too_large(10000, 10001),
        ),
        (
            "GET",
            "/wide/_search?size=20001",
            None,
            too_large(20000, 20001),
        ),
        // Inside a raised limit the fan-out still cannot merge the window,
        // and says so rather than quoting a limit the index does not have.
        (
            "GET",
            "/wide/_search?size=15000",
            None,
            "cluster search does not merge a from+size window above 10000 across shards yet; \
             the request was not executed"
                .to_string(),
        ),
    ] {
        let (status, body) = send(app, method, path, request.clone()).await;
        let es_shaped = !reason.starts_with("Result window")
            || (body["error"]["type"] == "search_phase_execution_exception"
                && body["error"]["root_cause"][0]["reason"] == reason.as_str());
        if status != StatusCode::BAD_REQUEST
            || body["error"]["reason"] != reason.as_str()
            || !es_shaped
        {
            wrong.push(format!("{method} {path} {request:?}: {status} {body}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));

    // A window inside both limits still runs.
    let (status, body) = send(app, "GET", "/orders/_search?from=9990&size=10", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
