//! End-to-end contract checks for the local `judge` stage of `_search` (issue
//! #1060), over HTTP against an in-process node.
//!
//! The judge needs no provider — that is its whole point — so most tests here
//! run with reranking DISABLED (`state.rerank` replaced wholesale, as
//! `rerank_stage_http.rs` does) and nothing ambient can be reached. The one
//! composition test brings up the same System One stub the rerank suite uses,
//! reduced to its overlap scoring, because the contract it pins is the ORDER
//! the two stages run in when both blocks ride one request.
//!
//! The lexical judge is deterministic, so these tests pin exact behaviour —
//! but they pin CONTRACT properties (presence, order, counts, refusals), not
//! ranking quality: nothing here says anything about which documents a real
//! model would judge relevant. That is the BEIR gate's job at release.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_rerank::ProviderSettings;

// ─────────────────────────────────────────────────────────────────────────────
// A minimal System One stub — for the composition tests only
// ─────────────────────────────────────────────────────────────────────────────

fn words(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Scores by the fraction of the query's words found in the document — the
/// same shape `rerank_stage_http.rs`'s stub uses. A test double, not a judge.
///
/// Every request increments `calls`: the zero-token claim of the judge stage
/// is asserted against that counter, so the assertion observes real egress,
/// not the absence of a crash.
async fn systemone(
    State(calls): State<Arc<AtomicU64>>,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    let _ = headers;
    calls.fetch_add(1, Ordering::SeqCst);
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let query = words(parsed["state"].as_str().unwrap_or_default());
    let mut answers = serde_json::Map::new();
    if let Some(qs) = parsed["questions"].as_object() {
        for (key, q) in qs {
            let doc = q["instructions"]["document"].as_str().unwrap_or_default();
            let have = words(doc);
            let p = query.intersection(&have).count() as f64 / query.len().max(1) as f64;
            answers.insert(key.clone(), json!({"type": "noul", "noul": p}));
        }
    }
    axum::Json(json!({
        "model": parsed["model"],
        "answers": answers,
        "usage": {"input_tokens": 100, "output_tokens": 10},
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// The node
// ─────────────────────────────────────────────────────────────────────────────

struct Node {
    app: axum::Router,
    /// The XERJ-native REST API (`/v1/...`), over the SAME state as `app`.
    native: axum::Router,
    _dir: tempfile::TempDir,
}

async fn node_with(settings: ProviderSettings) -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let mut state = xerj_api::state::AppState::new(config, engine, metrics);
    // The seam, same as the rerank suite: whatever TYPESAFE_* the developer's
    // shell holds was read into `state.rerank` by `AppState::new` and is
    // replaced wholesale here. The judge stage never consults it; only the
    // composition test arms it.
    state.rerank = Arc::new(settings);
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

/// A node with reranking disabled: the judge is the only second stage that
/// can run, and no ambient provider key can change that.
async fn node() -> Node {
    node_with(ProviderSettings::resolve(false, "", "", None, None)).await
}

/// A node with a live overlap stub, for the composition tests. Returns the
/// node plus the counter of requests the stub received — the observed egress.
async fn node_with_stub() -> (Node, Arc<AtomicU64>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let endpoint = format!("http://{addr}/v1/systemone");
    let calls = Arc::new(AtomicU64::new(0));
    let app = axum::Router::new()
        .route("/v1/systemone", post(systemone))
        .with_state(calls.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (
        node_with(ProviderSettings::with_key_and_endpoint(
            "test-key", &endpoint,
        ))
        .await,
        calls,
    )
}

impl Node {
    async fn raw(&self, method: &str, path: &str, body: String) -> (StatusCode, String) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let (status, text) = self.raw(method, path, body.to_string()).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn search(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call("POST", path, body).await
    }

    /// The four documents the rerank suite seeds: doc 2 is the only one that
    /// carries every word of Q, which is what makes the judge's job checkable.
    async fn seed_kb(&self) {
        let (st, b) = self
            .call(
                "PUT",
                "/kb",
                json!({"mappings": {"properties": {
                    "title": {"type": "text"},
                    "body": {"type": "text"},
                    "cat": {"type": "keyword"},
                    "n": {"type": "integer"}
                }}}),
            )
            .await;
        assert!(st.is_success(), "create kb: {st} {b}");
        for (id, title, body, cat, n) in [
            (
                "1",
                "Bone health basics",
                "vitamin supplements are popular. vitamin vitamin vitamin vitamin.",
                "health",
                1,
            ),
            (
                "2",
                "Trial results",
                "vitamin d supplementation improved bone density in the treatment group",
                "trial",
                2,
            ),
            (
                "3",
                "Cooking",
                "vitamin rich vegetables for dinner",
                "food",
                3,
            ),
            (
                "4",
                "Density of materials",
                "bone china has high density",
                "materials",
                4,
            ),
        ] {
            let (st, b) = self
                .call(
                    "PUT",
                    &format!("/kb/_doc/{id}"),
                    json!({"title": title, "body": body, "cat": cat, "n": n}),
                )
                .await;
            assert!(st.is_success(), "index kb/{id}: {st} {b}");
        }
        let (st, b) = self.call("POST", "/kb/_refresh", json!({})).await;
        assert!(st.is_success(), "refresh kb: {st} {b}");
    }
}

const Q: &str = "vitamin d supplementation bone density";

fn match_q() -> Value {
    json!({"match": {"body": Q}})
}

fn ids(r: &Value) -> Vec<String> {
    r["hits"]["hits"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|h| h["_id"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn ps(r: &Value) -> Vec<f64> {
    r["hits"]["hits"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|h| h["_p_relevant"].as_f64().unwrap_or(f64::NAN))
                .collect()
        })
        .unwrap_or_default()
}

fn reason(r: &Value) -> String {
    r["error"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Opt-in: absent by default, shape unchanged
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_judge_block_leaves_the_response_untouched() {
    let node = node().await;
    node.seed_kb().await;

    for body in [
        json!({"query": match_q(), "size": 4}),
        // `"judge": null` is the same as no `judge` key everywhere.
        json!({"query": match_q(), "size": 4, "judge": null}),
    ] {
        let (st, r) = node.search("/kb/_search", body).await;
        assert_eq!(st, StatusCode::OK, "{r}");
        assert!(r.get("judged").is_none(), "opt-in: {r}");
        assert_eq!(ids(&r).len(), 4);
        for h in r["hits"]["hits"].as_array().unwrap() {
            assert!(
                h.get("_p_relevant").is_none(),
                "no judge, no probability on the wire: {h}"
            );
            assert!(
                h["_score"].is_number(),
                "and the engine's score stands alone: {h}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Applied: _p_relevant per hit, reorder, the judged block
// ─────────────────────────────────────────────────────────────────────────────

/// #2580: use match_all to hold the candidate set fixed. This isolates the
/// judge's tokenization from the separate first-stage CJK recall question.
#[tokio::test]
async fn cjk_judge_finds_embedded_terms_and_thresholds_without_changing_engine_scores() {
    let node = node().await;
    let (st, body) = node
        .call(
            "PUT",
            "/cjk",
            json!({"mappings":{"properties":{
                "body":{"type":"text"}
            }}}),
        )
        .await;
    assert!(st.is_success(), "{st} {body}");
    for (id, text) in [
        ("exact", "这里发现雷同内容雷同内容"),
        ("partial", "部分雷声同时出现"),
        ("unrelated", "今天阳光明媚风景宜人"),
    ] {
        let (st, body) = node
            .call(
                "PUT",
                &format!("/cjk/_doc/{id}?refresh=true"),
                json!({"body":text}),
            )
            .await;
        assert!(st.is_success(), "{st} {body}");
    }
    for flushed in [false, true] {
        if flushed {
            let (st, body) = node.call("POST", "/cjk/_flush", json!({})).await;
            assert!(st.is_success(), "{st} {body}");
        }
        let (_, base) = node
            .search("/cjk/_search", json!({"query":{"match_all":{}},"size":3}))
            .await;
        assert!(base.get("judged").is_none(), "{base}");
        let (st, judged) = node
            .search(
                "/cjk/_search",
                json!({"query":{"match_all":{}},"size":3,"judge":{"query":"雷同"}}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{judged}");
        assert_eq!(ids(&judged)[0], "exact", "{judged}");
        let p = |id| {
            judged["hits"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["_id"] == id)
                .unwrap()["_p_relevant"]
                .as_f64()
                .unwrap()
        };
        assert!(p("exact") > p("partial"), "{judged}");
        assert!(p("partial") > 0.0, "{judged}");
        assert_eq!(p("unrelated"), 0.0, "{judged}");
        for hit in judged["hits"]["hits"].as_array().unwrap() {
            let original = base["hits"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["_id"] == hit["_id"])
                .unwrap();
            assert_eq!(hit["_score"], original["_score"], "{hit}");
        }
        let cut = (p("exact") + p("partial")) / 2.0;
        let (st, kept) = node
            .search(
                "/cjk/_search",
                json!({"query":{"match_all":{}},"size":3,"judge":{"query":"雷同","min_p":cut}}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{kept}");
        assert_eq!(ids(&kept), ["exact"], "{kept}");
        assert_eq!(kept["hits"]["total"], base["hits"]["total"]);
        assert_eq!(kept["judged"]["scorer"], "lexical");
        assert_eq!(kept["judged"]["judged"], 3);
        assert_eq!(kept["judged"]["kept"], 1);
        assert_eq!(kept["judged"]["dropped"], 2);
    }
}

#[tokio::test]
async fn judged_hits_carry_a_probability_and_the_block_reports_the_pass() {
    let node = node().await;
    node.seed_kb().await;

    let (st, text) = node
        .raw(
            "POST",
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {"local": true}}).to_string(),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();

    // Every hit was judged (all four carry text) and reordered by the
    // probability, descending.
    let probabilities = ps(&r);
    assert_eq!(probabilities.len(), 4, "{r}");
    assert!(
        probabilities.windows(2).all(|w| w[0] >= w[1]),
        "descending: {probabilities:?} in {:?}",
        ids(&r)
    );
    // Doc 2 carries every word of the question — the one hit the lexical
    // judge is certainst about sits first.
    assert_eq!(ids(&r)[0], "2", "{r}");

    let judged = &r["judged"];
    assert_eq!(judged["applied"], true, "{judged}");
    assert_eq!(judged["scorer"], "lexical", "{judged}");
    assert_eq!(judged["judged"], 4, "{judged}");
    assert_eq!(judged["kept"], 4, "{judged}");
    assert_eq!(judged["dropped"], 0, "{judged}");
    assert_eq!(judged["query"], Q, "the inferred question is reported back");
    assert!(judged["took_ms"].is_u64(), "{judged}");
    assert!(
        judged["min_p"].is_null(),
        "no threshold asked for, none reported: {judged}"
    );
    assert!(
        judged["unjudged"].is_null(),
        "everything was judged: {judged}"
    );
    // The lexical arm is arithmetic: naming a "model" beside it would be the
    // silent fake this stage exists to avoid.
    assert!(judged["model"].is_null(), "{judged}");

    // `judged` precedes `hits` on the wire: a reader that truncates long
    // output from the bottom must still see which pass it is holding.
    let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
    assert!(at("\"judged\"") < at("\"hits\""), "{text}");
}

#[tokio::test]
async fn the_engine_score_stands_beside_the_probability() {
    let node = node().await;
    node.seed_kb().await;

    let (_, base) = node
        .search("/kb/_search", json!({"query": match_q(), "size": 4}))
        .await;
    let (_, judged) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {}}),
        )
        .await;

    // Same hits, same `_score` per hit — the judge adds a scale, it does not
    // replace the engine's (the one deliberate difference from `rerank`).
    let mut base_scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for h in base["hits"]["hits"].as_array().unwrap() {
        base_scores.insert(
            h["_id"].as_str().unwrap().to_string(),
            h["_score"].as_f64().unwrap(),
        );
    }
    for h in judged["hits"]["hits"].as_array().unwrap() {
        let id = h["_id"].as_str().unwrap();
        assert_eq!(
            h["_score"].as_f64(),
            base_scores.get(id).copied(),
            "`_score` is the engine's own, untouched: {h}"
        );
        assert!(h["_p_relevant"].is_number(), "{h}");
    }
    // `max_score` describes `_score`, not `_p_relevant`.
    let emitted_max = judged["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["_score"].as_f64())
        .fold(None::<f64>, |acc: Option<f64>, s| match acc {
            Some(m) if m >= s => Some(m),
            _ => Some(s),
        });
    assert_eq!(
        judged["hits"]["max_score"].as_f64(),
        emitted_max,
        "{}",
        judged
    );
}

#[tokio::test]
async fn an_explicit_judge_query_wins_over_inference() {
    let node = node().await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "judge": {"query": "bone density"}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["judged"]["query"], "bone density", "{r}");
}

#[tokio::test]
async fn text_returned_through_fields_is_judgeable() {
    let node = node().await;
    node.seed_kb().await;

    // `_source: false` alone is refused; beside a `fields` clause it is the
    // documented way to judge without returning the whole source.
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "_source": false,
                "fields": ["body"],
                "judge": {}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["judged"]["applied"], true, "{r}");
    assert_eq!(r["judged"]["judged"], 4, "{r}");
    assert!(ps(&r).iter().all(|p| p.is_finite()), "{r}");
}

// ─────────────────────────────────────────────────────────────────────────────
// min_p: drops are counted, the total stays the engine's
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn min_p_drops_below_threshold_hits_and_counts_them() {
    let node = node().await;
    node.seed_kb().await;

    // Read the page's probabilities first, then cut inside them: the cut is
    // self-adjusting, so the test pins the CONTRACT (drop below, keep at or
    // above, count both) rather than one scorer's arithmetic.
    let (_, base) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {}}),
        )
        .await;
    let base_ps = ps(&base);
    assert!(base_ps.len() == 4, "{base}");
    // A threshold at the third hit's probability keeps the top three (>=
    // keeps it) and drops the rest.
    let cut = base_ps[2];
    assert!(cut > 0.0, "the fixture needs spread: {base_ps:?}");
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {"min_p": cut}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let kept = ps(&r);
    assert!(
        kept.iter().all(|p| *p >= cut),
        "nothing below the cut survives: {kept:?}"
    );
    assert_eq!(
        kept.len(),
        base_ps.iter().filter(|p| **p >= cut).count(),
        "{r}"
    );
    assert_eq!(r["judged"]["judged"], 4, "{r}");
    assert_eq!(r["judged"]["kept"], kept.len() as u64, "{r}");
    assert_eq!(
        r["judged"]["dropped"],
        (4 - kept.len()) as u64,
        "dropped is counted in the response: {r}"
    );
    assert_eq!(r["judged"]["min_p"], cut, "{r}");
    // `hits.total` describes the full match set, not the pruned page.
    assert_eq!(
        r["hits"]["total"]["value"], base["hits"]["total"]["value"],
        "{r}"
    );
}

#[tokio::test]
async fn a_threshold_of_one_drops_everything_and_says_so() {
    let node = node().await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {"min_p": 1.0}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // Lexical saturation approaches 1.0 only as tf grows without bound, so a
    // finite page never clears a cut of exactly 1.0 — an honest, counted
    // empty page rather than a silent unjudged one.
    assert!(ids(&r).is_empty(), "{r}");
    assert_eq!(r["judged"]["kept"], 0, "{r}");
    assert_eq!(r["judged"]["dropped"], 4, "{r}");
    assert!(r["hits"]["max_score"].is_null(), "{r}");
}

#[tokio::test]
async fn a_threshold_of_zero_keeps_everything() {
    let node = node().await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {"min_p": 0.0}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(ids(&r).len(), 4, "{r}");
    assert_eq!(r["judged"]["dropped"], 0, "{r}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Refusals: 400, by name, before the search runs
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_refusal_is_a_400_that_names_the_problem() {
    let node = node().await;
    node.seed_kb().await;

    let q = match_q();
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        (
            "judge not an object",
            "/kb/_search",
            json!({"query": q, "judge": "yes"}),
            "must be an object",
        ),
        (
            "sort",
            "/kb/_search",
            json!({"query": q, "sort": [{"n": "asc"}], "judge": {}}),
            "sort",
        ),
        (
            "?sort= in the URL",
            "/kb/_search?sort=n:asc",
            json!({"query": q, "judge": {}}),
            "sort",
        ),
        (
            "local false",
            "/kb/_search",
            json!({"query": q, "judge": {"local": false}}),
            "cannot be false",
        ),
        (
            "misspelt field",
            "/kb/_search",
            json!({"query": q, "judge": {"treshold": 0.5}}),
            "treshold",
        ),
        (
            "min_p outside 0..1",
            "/kb/_search",
            json!({"query": q, "judge": {"min_p": 7}}),
            "between 0 and 1",
        ),
        (
            "bool without judge.query",
            "/kb/_search",
            json!({"query": {"bool": {"must": [{"match": {"body": "vitamin"}}]}}, "judge": {}}),
            "judge.query",
        ),
        (
            "no query at all",
            "/kb/_search",
            json!({"judge": {}}),
            "judge.query",
        ),
        (
            "_source:false",
            "/kb/_search",
            json!({"query": q, "_source": false, "judge": {}}),
            "_source",
        ),
        // ES's own search_after-without-sort validation (which runs before
        // the stage's) fires first here — both are 400s, and the
        // judge-specific refusal text is pinned by the unit tests on a body
        // built directly. What this case pins over HTTP: the combination
        // never 200s with unjudged hits.
        (
            "search_after",
            "/kb/_search",
            json!({"query": q, "search_after": [1], "judge": {}}),
            "at least one field",
        ),
        (
            "collapse",
            "/kb/_search",
            json!({"query": q, "collapse": {"field": "cat"}, "judge": {}}),
            "collapse",
        ),
        (
            "scroll",
            "/kb/_search?scroll=1m",
            json!({"query": q, "judge": {}}),
            "scroll",
        ),
        (
            "size 0",
            "/kb/_search",
            json!({"query": q, "size": 0, "judge": {}}),
            "size: 0",
        ),
    ];
    for (name, path, body, needle) in cases {
        let (st, r) = node.search(path, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}: {r}");
        // Matched against the whole error object: some refusals ride inside
        // ES's "all shards failed" wrapper (root_cause / caused_by), and the
        // contract is that the caller can find the named cause, not that it
        // sits in one specific slot.
        let why = r["error"].to_string();
        assert!(why.contains(needle), "{name}: refusal must name it: {why}");
        // The judge's own refusals are `illegal_argument_exception`s; the
        // search_after case rides ES's `search_phase_execution_exception`
        // wrapper because the shard-level validation fires first. Both are
        // 400s that name the cause — that is the contract; the type taxonomy
        // is the engine's own, already covered by its suites.
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Other surfaces refuse the block by name instead of dropping it
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn msearch_items_carrying_judge_are_refused_per_item() {
    let node = node().await;
    node.seed_kb().await;

    let ndjson =
        "{\"index\":\"kb\"}\n{\"query\":{\"match\":{\"body\":\"vitamin\"}},\"judge\":{}}\n";
    let (st, text) = node.raw("POST", "/_msearch", ndjson.to_string()).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "the rest of the batch still runs: {text}"
    );
    let r: Value = serde_json::from_str(&text).unwrap();
    let items = r["responses"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{r}");
    assert_eq!(items[0]["status"], 400, "{r}");
    assert!(
        reason(&items[0]).contains("`judge` is not supported on _msearch"),
        "{r}"
    );
    // ...and a body carrying `rerank` keeps the rerank refusal, not the
    // judge's.
    let ndjson =
        "{\"index\":\"kb\"}\n{\"query\":{\"match\":{\"body\":\"vitamin\"}},\"rerank\":{}}\n";
    let (_, text) = node.raw("POST", "/_msearch", ndjson.to_string()).await;
    let r: Value = serde_json::from_str(&text).unwrap();
    assert!(
        reason(&r["responses"][0]).contains("`rerank` is not supported on _msearch"),
        "{r}"
    );
}

#[tokio::test]
async fn a_scroll_continuation_refuses_judge_instead_of_dropping_it() {
    let node = node().await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search?scroll=1m",
            json!({"query": match_q(), "size": 2}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let scroll_id = r["_scroll_id"].as_str().expect("scroll id").to_string();

    let (st, r) = node
        .search(
            "/_search/scroll",
            json!({"scroll": "1m", "scroll_id": scroll_id, "judge": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    assert!(reason(&r).contains("_search/scroll"), "{r}");
}

#[tokio::test]
async fn the_native_search_api_refuses_judge_instead_of_dropping_it() {
    let node = node().await;
    node.seed_kb().await;

    let send = |body: Value| {
        let app = node.native.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/indices/kb/search")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };

    // Its request struct ignores unknown keys, so without this a `judge`
    // block vanished and the caller got unjudged hits under a 200.
    let (st, text) = send(json!({"q": "vitamin", "judge": {}})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("`judge`"), "{text}");
    assert!(text.contains("_search"), "says where to send it: {text}");

    // Without the block the native search is untouched.
    let (st, text) = send(json!({"q": "vitamin"})).await;
    assert_eq!(st, StatusCode::OK, "{text}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Composition with the hosted stage
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn judge_runs_after_the_hosted_rerank_on_the_same_search() {
    let (node, calls) = node_with_stub().await;
    node.seed_kb().await;

    let (st, text) = node
        .raw(
            "POST",
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "rerank": {},
                "judge": {"min_p": 0.1}
            })
            .to_string(),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();

    // Both blocks are on the wire, in the order the stages ran: hosted first
    // (it widened and reordered its window), local judge second (it re-judged
    // and pruned the emitted page).
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["judged"]["applied"], true, "{r}");
    let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
    assert!(
        at("\"_rerank\"") < at("\"judged\""),
        "wire order is stage order: {text}"
    );

    // The hosted stage replaced `_score` with its probability; the judge left
    // that alone and added `_p_relevant`. Two scales, both named.
    for h in r["hits"]["hits"].as_array().unwrap() {
        assert!(h["_score"].is_number(), "the provider's verdict: {h}");
        assert!(h["_p_relevant"].is_number(), "the judge's verdict: {h}");
    }
    // And the judge's prune counted against its own pass.
    assert_eq!(
        r["judged"]["judged"].as_u64().unwrap(),
        r["judged"]["kept"].as_u64().unwrap() + r["judged"]["dropped"].as_u64().unwrap(),
        "kept + dropped accounting holds: {r}"
    );
    // Positive control for the zero-token test below: the hosted stage DID
    // leave the node, the stub answered.
    assert!(calls.load(Ordering::SeqCst) >= 1, "the stub was called");
}

#[tokio::test]
async fn a_judged_search_never_contacts_the_hosted_provider() {
    let (node, calls) = node_with_stub().await;
    node.seed_kb().await;

    // The stub is running and reachable, the node holds a live key and
    // endpoint for it — and a judged search must not touch it. Zero egress is
    // the stage's whole claim; it is asserted against the stub's own request
    // counter, not inferred from the search succeeding.
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "judge": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["judged"]["scorer"], "lexical", "{r}");
    assert_eq!(r["judged"]["judged"], 4, "{r}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the judge is local: the hosted endpoint must not be called"
    );
}
