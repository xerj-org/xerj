//! End-to-end checks for rerank provider `local`, over HTTP.
//!
//! Two groups. The first needs no model and runs everywhere: what the provider
//! refuses, which status each refusal carries, and that the egress switch
//! (`[rerank] enabled`) does not govern a provider with no egress. Every node
//! in it is OFFLINE (`download = false`, an empty cache directory), so no test
//! in this file can reach the network by accident.
//!
//! The second group runs the real MiniLM cross-encoder and is `#[ignore]`d,
//! because it downloads ~92 MB on a cold cache:
//!
//! ```text
//! cargo test -p xerj-api --features neural --test rerank_local_http -- --ignored
//! ```
//!
//! Unlike `rerank_stage_http.rs` there is no stub here: the judge is a function
//! call, so the thing under test is the thing that ships.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_rerank::local::{LocalJudge, LocalJudgeConfig};
use xerj_rerank::ProviderSettings;

struct Node {
    app: axum::Router,
    _dir: tempfile::TempDir,
}

/// A node whose hosted provider is `hosted_enabled` and whose local judge is
/// built from `judge`.
async fn node_with(hosted_enabled: bool, judge: LocalJudgeConfig) -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let mut state = xerj_api::state::AppState::new(config, engine, metrics);
    // No key, no endpoint: nothing here can call a hosted provider.
    state.rerank = Arc::new(
        ProviderSettings::resolve(hosted_enabled, "", "", None, None)
            .with_local(LocalJudge::new(judge)),
    );
    Node {
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

/// Offline: downloads forbidden and a cache directory with nothing in it.
fn offline() -> LocalJudgeConfig {
    LocalJudgeConfig {
        download: false,
        cache_dir: Some(std::env::temp_dir().join(format!(
            "xerj-rerank-local-empty-cache-{}",
            std::process::id()
        ))),
        ..Default::default()
    }
}

impl Node {
    async fn call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
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

    async fn seed(&self) {
        let (st, b) = self
            .call(
                "PUT",
                "/kb",
                json!({"mappings": {"properties": {
                    "title": {"type": "text"}, "body": {"type": "text"}}}}),
            )
            .await;
        assert!(st.is_success(), "{st} {b}");
        for (id, title, body) in [
            // Keyword-dense and answers nothing: what BM25 likes best.
            ("1", "Popular searches", "vitamin d supplementation bone density vitamin d supplementation bone density: popular search terms this month."),
            // Answers the question: what a reader wants first. Fewer query
            // terms than document 1, so BM25 puts it second.
            ("2", "Trial results", "A randomised controlled trial found that vitamin D3 supplements increased hip bone mineral density in older adults over two years."),
            ("3", "Cooking", "vitamin rich vegetables for dinner, with a density of flavour"),
            ("4", "Density of materials", "bone china has a high density compared with earthenware"),
        ] {
            let (st, b) = self
                .call("PUT", &format!("/kb/_doc/{id}"), json!({"title": title, "body": body}))
                .await;
            assert!(st.is_success(), "{st} {b}");
        }
        let (st, _) = self.call("POST", "/kb/_refresh", json!({})).await;
        assert!(st.is_success());
    }
}

const Q: &str = "does vitamin d supplementation improve bone density";

fn search(rerank: Value) -> Value {
    json!({"query": {"match": {"body": Q}}, "rerank": rerank})
}

fn reason(r: &Value) -> String {
    r["error"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn compiled_in() -> bool {
    LocalJudge::compiled_in()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. No model needed
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn options_that_mean_nothing_to_a_cross_encoder_are_a_400_by_name() {
    let node = node_with(true, offline()).await;
    node.seed().await;
    for (block, needle) in [
        (
            json!({"provider": "local", "instructions": "be strict"}),
            "`rerank.instructions` does not apply",
        ),
        (
            json!({"provider": "local", "batch": 10}),
            "`rerank.batch` does not apply",
        ),
        (
            json!({"provider": "local", "max_concurrency": 4}),
            "`rerank.max_concurrency` does not apply",
        ),
    ] {
        let (st, r) = node.call("POST", "/kb/_search", search(block)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
        assert_eq!(r["error"]["type"], "illegal_argument_exception", "{r}");
        assert!(reason(&r).contains(needle), "{r}");
    }
}

#[tokio::test]
async fn a_request_picks_a_tier_by_name_and_cannot_name_a_repository() {
    if !compiled_in() {
        return;
    }
    let node = node_with(true, offline()).await;
    node.seed().await;
    for model in ["BAAI/bge-reranker-large", "jev-latest", "../../etc/passwd"] {
        let (st, r) = node
            .call(
                "POST",
                "/kb/_search",
                search(json!({"provider": "local", "model": model})),
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{model}: {r}");
        assert!(reason(&r).contains("available: small, base, large"), "{r}");
    }
}

#[tokio::test]
async fn the_judge_switch_is_a_403_and_the_egress_switch_does_not_apply() {
    if !compiled_in() {
        return;
    }
    // `[judge] enabled = false`: refused, whatever `[rerank] enabled` says.
    let off = node_with(
        true,
        LocalJudgeConfig {
            enabled: false,
            ..offline()
        },
    )
    .await;
    off.seed().await;
    let (st, r) = off
        .call("POST", "/kb/_search", search(json!({"provider": "local"})))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{r}");
    assert_eq!(r["error"]["type"], "rerank_exception");
    assert!(reason(&r).contains("[judge] enabled = false"), "{r}");

    // `[rerank] enabled = false` forbids sending text to a third party. The
    // local provider sends none, so it is NOT refused by that switch: the
    // request gets as far as looking for the model (503 here, because this
    // node is offline with an empty cache) — and the hosted provider on the
    // same node still answers 403.
    let no_egress = node_with(false, offline()).await;
    no_egress.seed().await;
    let (st, r) = no_egress
        .call("POST", "/kb/_search", search(json!({"provider": "local"})))
        .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    let (st, r) = no_egress
        .call("POST", "/kb/_search", search(json!({"provider": "jev"})))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{r}");
    assert!(reason(&r).contains("[rerank] enabled = false"), "{r}");
}

#[tokio::test]
async fn an_offline_node_without_the_model_is_a_503_that_says_how_to_fix_it() {
    if !compiled_in() {
        return;
    }
    let node = node_with(true, offline()).await;
    node.seed().await;
    let (st, r) = node
        .call("POST", "/kb/_search", search(json!({"provider": "local"})))
        .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["error"]["type"], "rerank_exception", "{r}");
    let why = reason(&r);
    assert!(why.contains("downloads are disabled"), "{why}");
    assert!(why.contains("cross-encoder/ms-marco-MiniLM-L6-v2"), "{why}");
    // Not a silent fallback to lexical order under a 200.
    assert!(r["hits"].is_null(), "{r}");
}

#[tokio::test]
async fn a_build_without_the_feature_answers_501_not_lexical_order() {
    if compiled_in() {
        return;
    }
    let node = node_with(true, offline()).await;
    node.seed().await;
    let (st, r) = node
        .call("POST", "/kb/_search", search(json!({"provider": "local"})))
        .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED, "{r}");
    assert!(reason(&r).contains("`neural` feature"), "{r}");
}

#[tokio::test]
async fn the_status_endpoint_lists_both_providers_and_every_tier_with_its_licence() {
    let node = node_with(true, offline()).await;
    let (st, r) = node.call("GET", "/_xerj/rerank", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["providers"], json!(["jev", "local"]));
    assert_eq!(r["local"]["compiled_in"], compiled_in());
    assert_eq!(r["local"]["enabled"], true);
    assert_eq!(r["local"]["download"], false);
    assert_eq!(r["local"]["default_model"], "small");
    assert!(r["local"]["data_egress"]
        .as_str()
        .unwrap()
        .starts_with("none"));
    if compiled_in() {
        let models = r["local"]["models"].as_array().unwrap();
        assert_eq!(
            models
                .iter()
                .map(|m| m["model"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["small", "base", "large"]
        );
        for m in models {
            assert_eq!(m["state"], "not_downloaded", "{m}");
            assert!(
                m["licence"].is_string() && m["training_data"].is_string(),
                "{m}"
            );
            assert!(m["download_mb"].as_u64().unwrap() > 50, "{m}");
        }
    }
}

/// `xerj-common` validates `judge.rerank_model` at boot and cannot link
/// `xerj-ai`, so it carries its own copy of the tier names.
#[cfg(feature = "neural")]
#[test]
fn the_tier_names_are_the_same_in_the_config_and_in_the_model_table() {
    // Read through the status document: it is built from the model table.
    let doc = LocalJudge::new(offline()).status();
    let tiers: Vec<&str> = doc["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["model"].as_str().unwrap())
        .collect();
    assert_eq!(tiers, xerj_common::config::JudgeConfig::RERANK_MODELS);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. The real model (downloads ~92 MB on a cold cache)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn the_real_cross_encoder_puts_the_answer_first_and_scores_are_probabilities() {
    let node = node_with(false, LocalJudgeConfig::default()).await;
    node.seed().await;

    // The engine's own order first, so the test shows a change and not a tie.
    let (_, plain) = node
        .call(
            "POST",
            "/kb/_search",
            json!({"query": {"match": {"body": Q}}}),
        )
        .await;
    let engine_top = plain["hits"]["hits"][0]["_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st, r) = node
        .call(
            "POST",
            "/kb/_search",
            search(json!({"provider": "local", "timeout_ms": 60000})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let info = &r["_rerank"];
    assert_eq!(info["applied"], true, "{r}");
    assert_eq!(info["provider"], "local");
    assert_eq!(
        info["model"], "small",
        "an absent model resolves to the server default"
    );
    assert_eq!(info["score_kind"], "probability");
    assert_eq!(info["judged"], 4);
    assert_eq!(
        info["usage"],
        json!({"input_tokens": 0, "output_tokens": 0})
    );
    assert_eq!(info["local"]["data_egress"], "none");
    assert_eq!(
        info["local"]["repository"],
        "cross-encoder/ms-marco-MiniLM-L6-v2"
    );
    assert_eq!(info["local"]["truncated"], 0);
    assert!(info["local"]["tokens"].as_u64().unwrap() > 40);

    let hits = r["hits"]["hits"].as_array().unwrap();
    assert_eq!(
        engine_top, "1",
        "BM25 leads with the keyword-dense document"
    );
    assert_eq!(
        hits[0]["_id"], "2",
        "the trial result answers the question: {r}"
    );
    let scores: Vec<f64> = hits.iter().map(|h| h["_score"].as_f64().unwrap()).collect();
    assert!(scores.iter().all(|s| (0.0..=1.0).contains(s)), "{scores:?}");
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    assert_eq!(r["hits"]["max_score"].as_f64().unwrap(), scores[0]);
    eprintln!(
        "engine top = {engine_top}, reranked = {:?}, scores = {scores:?}",
        hits.iter()
            .map(|h| h["_id"].as_str().unwrap())
            .collect::<Vec<_>>()
    );

    // `min_score` is an absolute cut on that probability.
    let (_, pruned) = node
        .call(
            "POST",
            "/kb/_search",
            search(
                json!({"provider": "local", "min_score": scores[0] - 1e-9, "timeout_ms": 60000}),
            ),
        )
        .await;
    assert_eq!(
        pruned["hits"]["hits"].as_array().unwrap().len(),
        1,
        "{pruned}"
    );
}

#[tokio::test]
#[ignore]
async fn a_deadline_that_cannot_be_met_degrades_to_the_engine_order() {
    let node = node_with(false, LocalJudgeConfig::default()).await;
    node.seed().await;
    // Warm the model first so the 1 ms budget is spent on scoring, not loading.
    let _ = node
        .call(
            "POST",
            "/kb/_search",
            search(json!({"provider": "local", "timeout_ms": 60000})),
        )
        .await;
    let (st, r) = node
        .call(
            "POST",
            "/kb/_search",
            search(json!({"provider": "local", "timeout_ms": 1})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (_, plain) = node
        .call(
            "POST",
            "/kb/_search",
            json!({"query": {"match": {"body": Q}}}),
        )
        .await;
    // Either nothing was scored in 1 ms (degraded, engine order and scores),
    // or the first pass finished and the rest is reported unjudged. Both are
    // honest; silently claiming a full rerank is the failure.
    if r["_rerank"]["applied"] == false {
        assert_eq!(r["_rerank"]["score_kind"], "engine", "{r}");
        assert_eq!(r["hits"]["hits"], plain["hits"]["hits"], "{r}");
    } else {
        assert_eq!(r["_rerank"]["judged"], 4, "{r}");
    }
}
