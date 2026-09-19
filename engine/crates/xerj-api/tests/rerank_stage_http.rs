//! End-to-end checks for the `rerank` stage of `_search`, over HTTP, against an
//! in-process stub that speaks TypeSafe's documented System One wire format:
//!
//! ```text
//! POST /v1/systemone
//! request  { state, model, questions: { id: { type:"noul", instructions, criteria } } }
//! response { model, answers: { id: { type:"noul", noul: 0..1 } }, usage }
//! ```
//!
//! THE STUB IS A TEST DOUBLE, NOT A JUDGE. It scores by a table the test hands
//! it (or by word overlap), so ordering is checkable. Nothing here says
//! anything about ranking quality with the real model — that has not been
//! verified by this project; no provider key was available.
//!
//! # Why there is no `set_var` in this file
//!
//! The provider used to read `TYPESAFE_API_KEY` / `TYPESAFE_ENDPOINT` from the
//! process environment on every request. Cargo runs the tests of one binary as
//! parallel threads of one process, so a test that sets those variables races
//! every other test that reads them. Each test here builds its own stub on an
//! ephemeral port and its own node, and points the node at the stub through
//! `AppState::rerank` — the injection seam — so nothing process-wide is touched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_rerank::ProviderSettings;

// ─────────────────────────────────────────────────────────────────────────────
// The stub provider
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
enum Verdicts {
    /// Fraction of the query's words found in the document (what the scratch
    /// `stub.py` did).
    Overlap,
    /// Probability by document title; titles not listed score 0.0.
    ByTitle(HashMap<String, f64>),
}

#[derive(Clone)]
enum Fault {
    None,
    /// Answer every call with this status and an error body.
    Status(u16),
    /// Answer the first `n` calls with `status`, then behave.
    StatusThenOk {
        n: usize,
        status: u16,
    },
    /// 200 with a body that is not JSON.
    Garbage,
    /// Sleep this long before answering correctly.
    Delay(Duration),
    /// 401 with a body that echoes the bearer token back, as some APIs do.
    EchoKey,
    /// Answer correctly, and ALSO return a 0.99 verdict for `d0`..`d9` whether
    /// or not those documents were sent — a provider that misbehaves.
    AnswerUnsent,
    /// Answer every other document in each call and stay silent on the rest.
    Partial,
    /// Answer ONLY for keys nobody sent (`d999`, `x`): a provider that parses
    /// but says nothing about the documents it was asked about.
    WrongKeys,
    /// Answer correctly, and ALSO echo a 0.5 verdict for `d0`..`d59` on every
    /// call — a provider that leaks other batches' keys back.
    EchoOtherBatches,
    /// Answer every document correctly, under a `usage` block whose numbers
    /// are not the unsigned integers the documentation promises.
    OddUsage,
    /// 200 with a valid JSON body of several megabytes: a provider (or
    /// something answering in its place) that does not know when to stop.
    HugeBody,
}

#[derive(Clone)]
struct Captured {
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct Stub {
    verdicts: Arc<Mutex<Verdicts>>,
    fault: Arc<Mutex<Fault>>,
    seen: Arc<Mutex<Vec<Captured>>>,
    endpoint: String,
}

impl Stub {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let stub = Stub {
            verdicts: Arc::new(Mutex::new(Verdicts::Overlap)),
            fault: Arc::new(Mutex::new(Fault::None)),
            seen: Arc::new(Mutex::new(Vec::new())),
            endpoint: format!("http://{addr}/v1/systemone"),
        };
        let app = axum::Router::new()
            .route("/v1/systemone", post(systemone))
            .with_state(stub.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        stub
    }

    fn by_title(&self, table: &[(&str, f64)]) {
        *self.verdicts.lock().unwrap() =
            Verdicts::ByTitle(table.iter().map(|(t, p)| (t.to_string(), *p)).collect());
    }
    fn fault(&self, f: Fault) {
        *self.fault.lock().unwrap() = f;
    }
    fn calls(&self) -> Vec<Captured> {
        self.seen.lock().unwrap().clone()
    }
    /// Everything that left the node, as one string — for "this text was never
    /// sent" assertions.
    fn everything_sent(&self) -> String {
        self.calls()
            .iter()
            .map(|c| c.body.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn words(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

async fn systemone(
    axum::extract::State(stub): axum::extract::State<Stub>,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let nth = {
        let mut seen = stub.seen.lock().unwrap();
        seen.push(Captured {
            authorization: headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            body: parsed.clone(),
        });
        seen.len()
    };
    let fault = stub.fault.lock().unwrap().clone();
    match fault {
        Fault::Status(code) => {
            return (
                StatusCode::from_u16(code).unwrap(),
                axum::Json(json!({"error": "stub fault"})),
            )
                .into_response();
        }
        Fault::StatusThenOk { n, status } if nth <= n => {
            return (
                StatusCode::from_u16(status).unwrap(),
                axum::Json(json!({"error": "stub fault"})),
            )
                .into_response();
        }
        Fault::EchoKey => {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": format!("invalid api key: {auth}")})),
            )
                .into_response();
        }
        Fault::Garbage => return (StatusCode::OK, "not json").into_response(),
        Fault::HugeBody => {
            let padding = "x".repeat(3 * 1024 * 1024);
            return axum::Json(json!({"model": "m", "answers": {}, "padding": padding}))
                .into_response();
        }
        Fault::Delay(d) => tokio::time::sleep(d).await,
        _ => {}
    }

    let query = words(parsed["state"]["query"].as_str().unwrap_or_default());
    let verdicts = stub.verdicts.lock().unwrap().clone();
    let mut answers = serde_json::Map::new();
    if matches!(fault, Fault::WrongKeys) {
        answers.insert("d999".into(), json!({"type": "noul", "noul": 0.9}));
        answers.insert("x".into(), json!({"type": "noul", "noul": 0.9}));
        return axum::Json(json!({
            "model": parsed["model"],
            "answers": answers,
            "usage": {"input_tokens": 100, "output_tokens": 10},
        }))
        .into_response();
    }
    if let Some(docs) = parsed["state"]["documents"].as_object() {
        for (i, (key, doc)) in docs.iter().enumerate() {
            if matches!(fault, Fault::Partial) && i % 2 == 1 {
                continue;
            }
            let title = doc["title"].as_str().unwrap_or_default();
            let text = doc["text"].as_str().unwrap_or_default();
            let p = match &verdicts {
                Verdicts::Overlap => {
                    let have = words(&format!("{title} {text}"));
                    query.intersection(&have).count() as f64 / query.len().max(1) as f64
                }
                Verdicts::ByTitle(table) => table.get(title).copied().unwrap_or(0.0),
            };
            answers.insert(key.clone(), json!({"type": "noul", "noul": p}));
        }
    }
    if matches!(fault, Fault::AnswerUnsent) {
        for i in 0..10 {
            answers
                .entry(format!("d{i}"))
                .or_insert_with(|| json!({"type": "noul", "noul": 0.99}));
        }
    }
    if matches!(fault, Fault::EchoOtherBatches) {
        for i in 0..60 {
            answers
                .entry(format!("d{i}"))
                .or_insert_with(|| json!({"type": "noul", "noul": 0.5}));
        }
    }
    let usage = if matches!(fault, Fault::OddUsage) {
        json!({"input_tokens": 9_223_372_036_854_775_808u64, "output_tokens": -5})
    } else {
        json!({"input_tokens": 100, "output_tokens": 10})
    };
    axum::Json(json!({
        "model": parsed["model"],
        "answers": answers,
        "usage": usage,
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
    // The seam. Whatever TYPESAFE_* the developer's shell happens to hold was
    // read into `state.rerank` by `AppState::new`; it is replaced wholesale
    // here, so no test can reach the real provider or depend on the ambient
    // environment.
    state.rerank = Arc::new(settings);
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

async fn node(stub: &Stub) -> Node {
    node_with(ProviderSettings::with_key_and_endpoint(
        "test-key",
        &stub.endpoint,
    ))
    .await
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

    /// The four documents the scratch script used.
    async fn seed_kb(&self) {
        self.seed(
            "kb",
            &[
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
            ],
        )
        .await;
    }

    async fn seed(&self, index: &str, docs: &[(&str, &str, &str, &str, i64)]) {
        let (st, b) = self
            .call(
                "PUT",
                &format!("/{index}"),
                json!({"mappings": {"properties": {
                    "title": {"type": "text"},
                    "body": {"type": "text"},
                    "cat": {"type": "keyword"},
                    "n": {"type": "integer"}
                }}}),
            )
            .await;
        assert!(st.is_success(), "create {index}: {st} {b}");
        for (id, title, body, cat, n) in docs {
            let (st, b) = self
                .call(
                    "PUT",
                    &format!("/{index}/_doc/{id}"),
                    json!({"title": title, "body": body, "cat": cat, "n": n}),
                )
                .await;
            assert!(st.is_success(), "index {index}/{id}: {st} {b}");
        }
        let (st, b) = self
            .call("POST", &format!("/{index}/_refresh"), json!({}))
            .await;
        assert!(st.is_success(), "refresh {index}: {st} {b}");
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

fn reason(r: &Value) -> String {
    r["error"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A judge that inverts whatever the engine said about the four `kb` docs.
fn inverted(stub: &Stub) {
    stub.by_title(&[
        ("Cooking", 0.9),
        ("Bone health basics", 0.7),
        ("Density of materials", 0.3),
        ("Trial results", 0.1),
    ]);
}
const INVERTED_IDS: [&str; 4] = ["3", "1", "4", "2"];

// ─────────────────────────────────────────────────────────────────────────────
// 1. The seventeen checks the scratch script made, plus the wire check
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_rerank_block_leaves_the_response_untouched_and_calls_nobody() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "size": 4}))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r.get("_rerank").is_none(), "{r}");
    assert_eq!(ids(&r).len(), 4);
    assert!(
        stub.calls().is_empty(),
        "a search without `rerank` must not contact the provider"
    );
}

#[tokio::test]
async fn applied_order_follows_the_judge_not_the_engine() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, base) = node
        .search("/kb/_search", json!({"query": match_q(), "size": 4}))
        .await;
    assert_ne!(
        ids(&base),
        INVERTED_IDS,
        "the fixture is only a test if the judge disagrees with the engine"
    );

    let (st, text) = node
        .raw(
            "POST",
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"provider": "jev"}}).to_string(),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(ids(&r), INVERTED_IDS, "{r}");

    let rr = &r["_rerank"];
    assert_eq!(rr["applied"], true, "{rr}");
    assert_eq!(rr["provider"], "jev");
    assert_eq!(rr["model"], "jev-latest");
    assert_eq!(rr["score_kind"], "probability");
    assert_eq!(rr["window"], 4);
    assert_eq!(rr["judged"], 4);
    assert_eq!(rr["skipped_no_text"], 0);
    assert_eq!(rr["partial_failures"], 0);
    assert_eq!(rr["query"], Q, "the inferred question is reported back");
    assert_eq!(rr["usage"]["input_tokens"], 100);
    assert_eq!(rr["usage"]["output_tokens"], 10);
    assert!(rr["took_ms"].is_u64(), "{rr}");

    // `_rerank` precedes `hits` on the wire: a reader that truncates long
    // output from the bottom must still see which ranking it is holding.
    let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
    assert!(at("\"_rerank\"") < at("\"hits\""), "{text}");
}

#[tokio::test]
async fn scores_are_probabilities_and_max_score_tracks_the_top_hit() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {}}),
        )
        .await;
    let scores: Vec<f64> = r["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["_score"].as_f64().unwrap())
        .collect();
    // To the digit: the provider said 0.9 and the wire says 0.9, not the
    // 0.8999999761581421 an f32 in the middle produced. A caller who re-applies
    // a threshold client-side must agree with `rerank.min_score`.
    assert_eq!(scores, vec![0.9, 0.7, 0.3, 0.1]);
    assert_eq!(r["hits"]["max_score"].as_f64(), Some(scores[0]));
}

#[tokio::test]
async fn size_and_from_page_inside_the_reranked_window() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, first) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 2, "rerank": {}}),
        )
        .await;
    let (_, second) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "from": 2, "size": 2, "rerank": {}}),
        )
        .await;
    assert_eq!(ids(&first), &INVERTED_IDS[..2], "{first}");
    assert_eq!(
        ids(&second),
        &INVERTED_IDS[2..],
        "page two continues the JUDGE's order, not the engine's: {second}"
    );
    // The whole window was judged both times, even though two hits came back.
    assert_eq!(first["_rerank"]["judged"], 4);
    assert_eq!(second["_rerank"]["judged"], 4);
    // `?size=` / `?from=` are the same request spelled in the URL.
    let (_, url_page) = node
        .search(
            "/kb/_search?from=2&size=2",
            json!({"query": match_q(), "rerank": {}}),
        )
        .await;
    assert_eq!(ids(&url_page), &INVERTED_IDS[2..], "{url_page}");
}

#[tokio::test]
async fn min_score_prunes_by_probability_and_the_total_stays_the_engines() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"min_score": 0.5}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(ids(&r), ["3", "1"], "{r}");
    assert_eq!(r["_rerank"]["pruned_below_min_score"], 2);
    assert_eq!(r["_rerank"]["dropped_unjudged"], 0);
    // Four documents matched. Two were judged irrelevant. Both facts are true
    // and the response says both: the total is the engine's.
    assert_eq!(r["hits"]["total"]["value"], 4, "{r}");
    assert_eq!(r["hits"]["total"]["relation"], "eq");
}

#[tokio::test]
async fn every_refusal_is_a_400_that_names_the_problem_and_sends_nothing() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let q = match_q();
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        (
            "sort",
            "/kb/_search",
            json!({"query": q, "sort": [{"n": "asc"}], "rerank": {}}),
            "sort",
        ),
        (
            "?sort= in the URL",
            "/kb/_search?sort=n:asc",
            json!({"query": q, "rerank": {}}),
            "sort",
        ),
        (
            "misspelt field",
            "/kb/_search",
            json!({"query": q, "rerank": {"treshold": 0.5}}),
            "treshold",
        ),
        (
            "min_score outside 0..1",
            "/kb/_search",
            json!({"query": q, "rerank": {"min_score": 7}}),
            "probability",
        ),
        (
            "bool without rerank.query",
            "/kb/_search",
            json!({"query": {"bool": {"must": [{"match": {"body": "vitamin"}}]}}, "rerank": {}}),
            "rerank.query",
        ),
        (
            "no query at all",
            "/kb/_search",
            json!({"rerank": {}}),
            "rerank.query",
        ),
        (
            "paging past the window",
            "/kb/_search",
            json!({"query": q, "from": 25, "size": 10, "rerank": {}}),
            "window",
        ),
        (
            "?from= past the window",
            "/kb/_search?from=25&size=10",
            json!({"query": q, "rerank": {}}),
            "window",
        ),
        (
            // `from + size` used to be an unchecked add: u64::MAX + 1 wrapped
            // to 0, passed the window check, paid for a full window of
            // judgements and returned an empty page under a 200 — while the
            // same request without `rerank` is a 400.
            "from + size overflowing",
            "/kb/_search",
            json!({"query": q, "from": u64::MAX, "size": 1, "rerank": {}}),
            "window",
        ),
        (
            "?from= overflowing in the URL",
            "/kb/_search?from=18446744073709551615&size=1",
            json!({"query": q, "rerank": {}}),
            "window",
        ),
        (
            "_source:false",
            "/kb/_search",
            json!({"query": q, "_source": false, "rerank": {}}),
            "_source",
        ),
        (
            "unknown provider",
            "/kb/_search",
            json!({"query": q, "rerank": {"provider": "voyage"}}),
            "voyage",
        ),
        (
            "search_after",
            "/kb/_search",
            json!({"query": q, "search_after": [1], "rerank": {}}),
            "s",
        ),
        (
            "collapse",
            "/kb/_search",
            json!({"query": q, "collapse": {"field": "cat"}, "rerank": {}}),
            "collapse",
        ),
        (
            "scroll",
            "/kb/_search?scroll=1m",
            json!({"query": q, "rerank": {}}),
            "scroll",
        ),
        (
            "size 0",
            "/kb/_search",
            json!({"query": q, "size": 0, "rerank": {}}),
            "size: 0",
        ),
        (
            "window over the ceiling",
            "/kb/_search",
            json!({"query": q, "rerank": {"window": 301}}),
            "rerank.window",
        ),
        (
            "window zero",
            "/kb/_search",
            json!({"query": q, "rerank": {"window": 0}}),
            "rerank.window",
        ),
        (
            "concurrency over the ceiling",
            "/kb/_search",
            json!({"query": q, "rerank": {"max_concurrency": 17}}),
            "max_concurrency",
        ),
        (
            "timeout over the ceiling",
            "/kb/_search",
            json!({"query": q, "rerank": {"timeout_ms": 60001}}),
            "timeout_ms",
        ),
        (
            "rerank is not an object",
            "/kb/_search",
            json!({"query": q, "rerank": true}),
            "object",
        ),
        (
            "empty rerank.query",
            "/kb/_search",
            json!({"query": q, "rerank": {"query": "  "}}),
            "rerank.query",
        ),
        (
            "empty rerank.fields",
            "/kb/_search",
            json!({"query": q, "rerank": {"fields": []}}),
            "rerank.fields",
        ),
        (
            "judged field excluded from _source",
            "/kb/_search",
            json!({"query": q, "_source": {"excludes": ["body"]}, "rerank": {"fields": ["body"]}}),
            "body",
        ),
        (
            "judged field not in _source includes",
            "/kb/_search",
            json!({"query": q, "_source": ["title"], "rerank": {"fields": ["body"]}}),
            "body",
        ),
        (
            "judged field excluded in the URL",
            "/kb/_search?_source_excludes=body",
            json!({"query": q, "rerank": {"fields": ["body"]}}),
            "body",
        ),
    ];
    for (name, path, body, needle) in cases {
        let (st, r) = node.search(path, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}: {r}");
        assert_eq!(r["status"], 400, "{name}: {r}");
        assert!(
            r["error"]["type"].is_string(),
            "{name}: an ES-shaped error envelope, not a bare string: {r}"
        );
        assert!(
            reason(&r).contains(needle),
            "{name}: the reason must name `{needle}`, got: {}",
            reason(&r)
        );
    }
    assert!(
        stub.calls().is_empty(),
        "a refused request must not have sent a single document to the provider"
    );
}

#[tokio::test]
async fn a_bool_query_works_once_the_question_is_spelled_out() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": {"bool": {"must": [{"match": {"body": "vitamin"}}]}},
                "size": 4,
                "rerank": {"query": "bone density trial"}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true);
    assert_eq!(r["_rerank"]["query"], "bone density trial");
    assert_eq!(
        stub.calls()[0].body["state"]["query"],
        "bone density trial",
        "the judge is asked the caller's question, not a guess at the bool tree"
    );
}

#[tokio::test]
async fn provider_401_surfaces_as_502_rerank_exception_not_as_lexical_order() {
    let stub = Stub::start().await;
    stub.fault(Fault::Status(401));
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{r}");
    assert_eq!(r["status"], 502);
    assert_eq!(r["error"]["type"], "rerank_exception");
    assert!(reason(&r).contains("401"), "{r}");
    assert!(
        r.get("hits").is_none(),
        "a contract fault must not hand back the engine's order as if nothing happened: {r}"
    );
    assert_eq!(stub.calls().len(), 1, "a 401 is not retried");
}

#[tokio::test]
async fn malformed_provider_body_surfaces_as_502() {
    let stub = Stub::start().await;
    stub.fault(Fault::Garbage);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{r}");
    assert_eq!(r["error"]["type"], "rerank_exception");
    assert!(reason(&r).contains("malformed"), "{r}");
}

#[tokio::test]
async fn an_unreachable_provider_surfaces_as_502() {
    // Port 1 refuses immediately; no stub involved.
    let node = node_with(ProviderSettings::with_key_and_endpoint(
        "test-key",
        "http://127.0.0.1:1/v1/systemone",
    ))
    .await;
    node.seed_kb().await;
    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{r}");
    assert_eq!(r["error"]["type"], "rerank_exception");
}

#[tokio::test]
async fn a_slow_provider_degrades_and_the_engine_order_stands() {
    let stub = Stub::start().await;
    inverted(&stub);
    stub.fault(Fault::Delay(Duration::from_secs(3)));
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, base) = node
        .search("/kb/_search", json!({"query": match_q(), "size": 4}))
        .await;

    let started = Instant::now();
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"timeout_ms": 300}}),
        )
        .await;
    let waited = started.elapsed();

    assert_eq!(st, StatusCode::OK, "degrade on deadline is a 200: {r}");
    assert_eq!(r["_rerank"]["applied"], false, "{r}");
    assert_eq!(r["_rerank"]["score_kind"], "engine");
    assert!(
        r["_rerank"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("deadline"),
        "{r}"
    );
    assert_eq!(ids(&r), ids(&base), "the engine's order, untouched");
    assert_eq!(
        r["hits"]["hits"][0]["_score"], base["hits"]["hits"][0]["_score"],
        "and the engine's scores — not a probability nobody computed"
    );
    assert!(
        waited < Duration::from_millis(2500),
        "the caller waited {waited:?} on a 300 ms budget — the deadline did not bound the stage"
    );
}

#[tokio::test]
async fn a_rate_limit_that_clears_is_retried_inside_the_budget() {
    let stub = Stub::start().await;
    inverted(&stub);
    stub.fault(Fault::StatusThenOk { n: 1, status: 429 });
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(ids(&r), INVERTED_IDS);
    assert_eq!(stub.calls().len(), 2, "one 429, one retry");
}

#[tokio::test]
async fn a_server_with_no_key_answers_503_and_sends_nothing() {
    let stub = Stub::start().await;
    // Resolved with no config key and NO environment — deterministic whatever
    // the developer's shell exports.
    let node = node_with(ProviderSettings::resolve(
        true,
        "",
        &stub.endpoint,
        None,
        None,
    ))
    .await;
    node.seed_kb().await;

    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["status"], 503);
    assert_eq!(r["error"]["type"], "rerank_exception");
    assert!(reason(&r).contains("TYPESAFE_API_KEY"), "{r}");
    assert!(
        reason(&r).contains("[rerank]"),
        "names the config section too: {r}"
    );
    assert!(stub.calls().is_empty());

    // Without a `rerank` block the same node searches normally.
    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q()}))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
}

#[tokio::test]
async fn an_operator_kill_switch_is_a_403_and_sends_nothing() {
    let stub = Stub::start().await;
    let node = node_with(ProviderSettings::resolve(
        false,
        "test-key",
        &stub.endpoint,
        None,
        None,
    ))
    .await;
    node.seed_kb().await;

    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{r}");
    assert_eq!(r["error"]["type"], "rerank_exception");
    assert!(reason(&r).contains("disabled"), "{r}");
    assert!(
        stub.calls().is_empty(),
        "enabled = false means nothing leaves the host"
    );
}

#[tokio::test]
async fn wire_shape_bearer_auth_model_and_one_noul_per_document_keyed_identically() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"model": "jev-2026-09"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");

    let calls = stub.calls();
    assert_eq!(calls.len(), 1, "four documents fit one call");
    let call = &calls[0];
    assert_eq!(call.authorization.as_deref(), Some("Bearer test-key"));
    assert_eq!(call.body["model"], "jev-2026-09");
    assert_eq!(call.body["state"]["query"], Q);

    let documents = call.body["state"]["documents"].as_object().unwrap();
    let questions = call.body["questions"].as_object().unwrap();
    assert_eq!(documents.len(), 4);
    let mut dk: Vec<&String> = documents.keys().collect();
    let mut qk: Vec<&String> = questions.keys().collect();
    dk.sort();
    qk.sort();
    assert_eq!(dk, qk, "one question per document, keyed identically");
    for (key, question) in questions {
        assert_eq!(question["type"], "noul", "{key}");
        assert!(question["instructions"].is_string(), "{key}");
        assert!(question["criteria"]["true"].is_string(), "{key}");
        assert!(question["criteria"]["false"].is_string(), "{key}");
    }
    for (key, doc) in documents {
        assert!(doc["text"].is_string(), "{key}");
        assert!(doc["title"].is_string(), "{key}");
    }
    // The key is a bearer token and nothing else: never in the body.
    assert!(!call.body.to_string().contains("test-key"));
}

#[tokio::test]
async fn custom_instructions_reach_every_question() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;
    let (st, _) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "rerank": {"instructions": "Is this a clinical result?"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    for (_, q) in stub.calls()[0].body["questions"].as_object().unwrap() {
        assert_eq!(q["instructions"], "Is this a clinical result?");
    }
}

#[tokio::test]
async fn a_window_larger_than_thirty_is_split_and_ordinals_survive() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    let docs: Vec<(String, String)> = (0..35)
        .map(|i| (format!("{i}"), format!("doc number {i}")))
        .collect();
    let seeded: Vec<(&str, &str, &str, &str, i64)> = docs
        .iter()
        .map(|(id, title)| (id.as_str(), title.as_str(), "common term here", "c", 1))
        .collect();
    node.seed("many", &seeded).await;
    // Only one document is relevant, and it sits wherever the engine put it.
    stub.by_title(&[("doc number 33", 0.95)]);

    let (st, r) = node
        .search(
            "/many/_search",
            json!({"query": {"match": {"body": "common"}}, "size": 35, "rerank": {"window": 35}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["judged"], 35, "{r}");
    assert_eq!(
        ids(&r)[0],
        "33",
        "the one relevant document wins across batches"
    );

    let calls = stub.calls();
    assert_eq!(calls.len(), 2, "35 documents = 30 + 5");
    let mut keys: Vec<String> = Vec::new();
    for c in &calls {
        let docs = c.body["state"]["documents"].as_object().unwrap();
        assert!(docs.len() <= 30, "the provider ceiling is 30 per call");
        keys.extend(docs.keys().cloned());
    }
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 35, "every document is judged exactly once");
    // Usage is summed across the calls that answered.
    assert_eq!(r["_rerank"]["usage"]["input_tokens"], 200);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Interactions with the rest of `_search`
// ─────────────────────────────────────────────────────────────────────────────

/// Twelve matching documents in three categories, a window of three.
async fn seed_twelve(node: &Node) {
    let docs: Vec<(String, String, &str)> = (0..12)
        .map(|i| {
            (
                format!("{i}"),
                format!("note {i}"),
                ["red", "green", "blue"][i % 3],
            )
        })
        .collect();
    let seeded: Vec<(&str, &str, &str, &str, i64)> = docs
        .iter()
        .map(|(id, title, cat)| (id.as_str(), title.as_str(), "shared keyword", *cat, 1))
        .collect();
    node.seed("twelve", &seeded).await;
}

#[tokio::test]
async fn aggregations_describe_the_full_match_set_not_the_window() {
    let stub = Stub::start().await;
    stub.by_title(&[("note 7", 0.9)]);
    let node = node(&stub).await;
    seed_twelve(&node).await;

    let body = |rerank: bool| {
        let mut b = json!({
            "query": {"match": {"body": "shared"}},
            "size": 2,
            "aggs": {"by_cat": {"terms": {"field": "cat"}}, "n_sum": {"sum": {"field": "n"}}}
        });
        if rerank {
            b["rerank"] = json!({"window": 3, "min_score": 0.5});
        }
        b
    };
    let (_, plain) = node.search("/twelve/_search", body(false)).await;
    let (st, r) = node.search("/twelve/_search", body(true)).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["_rerank"]["window"], 3);

    // The window was 3, the page 2, and pruning left at most one hit — and
    // none of that is allowed to leak into the facets or the total.
    assert_eq!(
        r["aggregations"], plain["aggregations"],
        "aggregations must be byte-identical with and without `rerank`"
    );
    let bucket_sum: u64 = r["aggregations"]["by_cat"]["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["doc_count"].as_u64().unwrap())
        .sum();
    assert_eq!(bucket_sum, 12, "{r}");
    assert_eq!(r["aggregations"]["n_sum"]["value"].as_f64(), Some(12.0));
    assert_eq!(r["hits"]["total"]["value"], 12, "{r}");
    assert!(ids(&r).len() <= 2);
}

#[tokio::test]
async fn track_total_hits_keeps_its_meaning() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    seed_twelve(&node).await;
    let q = json!({"match": {"body": "shared"}});

    let (_, exact) = node
        .search(
            "/twelve/_search",
            json!({"query": q, "size": 2, "track_total_hits": true, "rerank": {"window": 5}}),
        )
        .await;
    assert_eq!(
        exact["hits"]["total"],
        json!({"value": 12, "relation": "eq"}),
        "{exact}"
    );

    let (_, capped) = node
        .search(
            "/twelve/_search",
            json!({"query": q, "size": 2, "track_total_hits": 5, "rerank": {"window": 5}}),
        )
        .await;
    let (_, capped_plain) = node
        .search(
            "/twelve/_search",
            json!({"query": q, "size": 2, "track_total_hits": 5}),
        )
        .await;
    assert_eq!(
        capped["hits"]["total"], capped_plain["hits"]["total"],
        "an integer cap reports what it reports without `rerank`"
    );

    let (st, off) = node
        .search(
            "/twelve/_search",
            json!({"query": q, "size": 2, "track_total_hits": false, "rerank": {"window": 5}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{off}");
    assert!(off["hits"].get("total").is_none(), "{off}");
    assert_eq!(off["_rerank"]["applied"], true);
    assert_eq!(ids(&off).len(), 2);
}

#[tokio::test]
async fn highlight_and_fields_travel_with_their_hit_through_the_reorder() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let body = |rerank: bool| {
        let mut b = json!({
            "query": match_q(),
            "size": 4,
            "highlight": {"fields": {"body": {}}},
            "fields": ["cat", "n"]
        });
        if rerank {
            b["rerank"] = json!({});
        }
        b
    };
    let (_, plain) = node.search("/kb/_search", body(false)).await;
    let (_, r) = node.search("/kb/_search", body(true)).await;
    assert_eq!(ids(&r), INVERTED_IDS, "{r}");

    let by_id = |resp: &Value| -> HashMap<String, (Value, Value)> {
        resp["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["_id"].as_str().unwrap().to_string(),
                    (h["highlight"].clone(), h["fields"].clone()),
                )
            })
            .collect()
    };
    let (before, after) = (by_id(&plain), by_id(&r));
    for (id, (hl, fields)) in &after {
        assert!(!hl.is_null(), "hit {id} lost its highlight: {r}");
        assert!(!fields.is_null(), "hit {id} lost its fields: {r}");
        assert_eq!(
            &before[id].0, hl,
            "hit {id} carries another hit's highlight"
        );
        assert_eq!(
            &before[id].1, fields,
            "hit {id} carries another hit's fields"
        );
    }
}

#[tokio::test]
async fn the_judge_sees_exactly_what_the_response_returns() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    // `_source: ["title"]` — the body is not in the response, so it is not in
    // the request to the provider either. This is the privacy contract.
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "_source": ["title"], "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true);
    let sent = stub.everything_sent();
    assert!(
        sent.contains("Trial results"),
        "titles were returned, so titles were judged"
    );
    assert!(
        !sent.contains("treatment group") && !sent.contains("bone china"),
        "body text the caller excluded from the response must not leave the machine: {sent}"
    );
    for h in r["hits"]["hits"].as_array().unwrap() {
        assert!(h["_source"].get("body").is_none(), "{h}");
    }

    // A projection that leaves no prose at all: refused, nothing sent.
    let before = stub.calls().len();
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "_source": ["n"], "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    assert_eq!(r["error"]["type"], "illegal_argument_exception");
    assert!(reason(&r).contains("nothing to judge"), "{r}");
    assert_eq!(stub.calls().len(), before, "blank documents are never sent");

    // `fields` is part of the response too: text returned that way is judgeable
    // even with `_source` switched off entirely.
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "_source": false, "fields": ["body"],
                "rerank": {"fields": ["body"]}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    let last = stub.calls().last().unwrap().body.clone();
    assert!(last.to_string().contains("treatment group"), "{last}");
    assert!(
        !last.to_string().contains("Trial results"),
        "titles were neither returned nor named, so they were not sent: {last}"
    );
}

/// The invariant behind the privacy contract's other half: a caller who names
/// the field to judge on is NEVER handed an `applied: true` that was judged on
/// blank text.
///
/// Written against the invariant rather than one outcome on purpose. Today the
/// engine drops a `fields` entry that an explicit `_source` includes list
/// filtered away, so this request returns no `body` and must be refused. If the
/// engine starts returning it, as Elasticsearch does, the same request becomes
/// a legitimate rerank on the body. Both are correct; judging blind is not.
#[tokio::test]
async fn a_named_field_the_response_does_not_return_is_never_judged_blind() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "_source": ["title"], "fields": ["body"],
                "rerank": {"fields": ["body"]}
            }),
        )
        .await;
    if st == StatusCode::BAD_REQUEST {
        assert_eq!(r["error"]["type"], "illegal_argument_exception", "{r}");
        assert!(reason(&r).contains("nothing to judge"), "{r}");
        assert!(reason(&r).contains("body"), "names the field: {r}");
        assert!(stub.calls().is_empty(), "blank documents are never sent");
    } else {
        assert_eq!(st, StatusCode::OK, "{r}");
        assert_eq!(r["_rerank"]["applied"], true, "{r}");
        for c in stub.calls() {
            for (key, doc) in c.body["state"]["documents"].as_object().unwrap() {
                assert!(
                    !doc["text"].as_str().unwrap_or_default().trim().is_empty(),
                    "document {key} was judged on blank text: {doc}"
                );
            }
        }
    }
}

/// A named field that contributes nothing is reported, not swallowed: a typo in
/// `rerank.fields` must not pass for a judgement on that field.
#[tokio::test]
async fn a_named_field_with_no_text_anywhere_in_the_window_is_reported() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"fields": ["body", "bdoy"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["_rerank"]["fields_without_text"], json!(["bdoy"]), "{r}");

    // Nothing to report means the key is absent, not an empty array.
    let (_, clean) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"fields": ["body"]}}),
        )
        .await;
    assert!(
        clean["_rerank"].get("fields_without_text").is_none(),
        "{clean}"
    );

    // Every named field empty: that is nothing to judge, and nothing is sent.
    let before = stub.calls().len();
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"fields": ["bdoy"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    assert!(reason(&r).contains("bdoy"), "{r}");
    assert_eq!(stub.calls().len(), before);
}

#[tokio::test]
async fn rerank_fields_is_exhaustive_about_what_is_sent() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"fields": ["body"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");

    // Asserted on the structure, not by substring: the title "Density of
    // materials" ends in the `cat` value "materials", and a substring check
    // reported a leak that was not there.
    let bodies = [
        "vitamin supplements are popular. vitamin vitamin vitamin vitamin.",
        "vitamin d supplementation improved bone density in the treatment group",
        "vitamin rich vegetables for dinner",
        "bone china has high density",
    ];
    let calls = stub.calls();
    let documents = calls[0].body["state"]["documents"].as_object().unwrap();
    assert_eq!(documents.len(), 4);
    for (key, doc) in documents {
        let keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["text"],
            "{key}: `rerank.fields` named only `body`, so not even the title may be sent"
        );
        let text = doc["text"].as_str().unwrap();
        assert!(
            bodies.contains(&text),
            "{key}: the text is exactly the body — no `cat`, no `n`, nothing appended: {text:?}"
        );
    }

    // Naming the title brings it back, in its own slot.
    let (_, _) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {"fields": ["title", "body"]}}),
        )
        .await;
    let last = stub.calls().last().unwrap().body.clone();
    for (key, doc) in last["state"]["documents"].as_object().unwrap() {
        assert!(doc["title"].is_string(), "{key}: {doc}");
        assert!(
            bodies.contains(&doc["text"].as_str().unwrap()),
            "{key}: {doc}"
        );
    }
}

#[tokio::test]
async fn hits_with_no_prose_are_skipped_not_sent_blank() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;
    let (st, b) = node.call("PUT", "/kb/_doc/9", json!({"n": 9})).await;
    assert!(st.is_success(), "{b}");
    node.call("POST", "/kb/_refresh", json!({})).await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": {"match_all": {}}, "size": 5, "rerank": {"query": "bone density"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["window"], 5);
    assert_eq!(r["_rerank"]["judged"], 4);
    assert_eq!(r["_rerank"]["skipped_no_text"], 1);
    assert_eq!(
        stub.calls()[0].body["state"]["documents"]
            .as_object()
            .unwrap()
            .len(),
        4
    );
    // Unjudged is not a verdict of irrelevant: it sorts after the judged hits.
    // Its `_score` is null — never the engine's BM25 value beside
    // probabilities — and the block counts it.
    assert_eq!(ids(&r).last().map(String::as_str), Some("9"), "{r}");
    assert!(r["hits"]["hits"][4]["_score"].is_null(), "{r}");
    assert_eq!(r["_rerank"]["unjudged"], 1, "{r}");

    // With a threshold it cannot be shown to clear the bar, so it goes — and
    // the response says that is why.
    let (_, r) = node
        .search(
            "/kb/_search",
            json!({"query": {"match_all": {}}, "size": 5,
                   "rerank": {"query": "bone density", "min_score": 0.0}}),
        )
        .await;
    assert!(!ids(&r).contains(&"9".to_string()), "{r}");
    assert_eq!(r["_rerank"]["dropped_unjudged"], 1, "{r}");
}

#[tokio::test]
async fn explain_is_wrapped_so_it_never_contradicts_the_score() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, plain) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "explain": true}),
        )
        .await;
    let engine: HashMap<String, Value> = plain["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["_id"].as_str().unwrap().to_string(),
                h["_explanation"].clone(),
            )
        })
        .collect();

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "explain": true, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    for h in r["hits"]["hits"].as_array().unwrap() {
        let id = h["_id"].as_str().unwrap();
        let ex = &h["_explanation"];
        assert_eq!(
            ex["value"], h["_score"],
            "hit {id}: `_explanation.value` must agree with `_score`"
        );
        assert!(
            ex["description"]
                .as_str()
                .unwrap_or_default()
                .contains("rerank"),
            "{ex}"
        );
        assert_eq!(
            ex["details"][0], engine[id],
            "hit {id}: the engine's own explanation is kept underneath, unaltered"
        );
    }
}

#[tokio::test]
async fn profile_survives_and_took_covers_the_provider_call() {
    let stub = Stub::start().await;
    stub.fault(Fault::Delay(Duration::from_millis(200)));
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "profile": true, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(
        r["profile"]["shards"].is_array(),
        "profile is the engine's and still there: {r}"
    );
    assert_eq!(r["_rerank"]["applied"], true);
    let stage = r["_rerank"]["took_ms"].as_u64().unwrap();
    let took = r["took"].as_u64().unwrap();
    assert!(stage >= 200, "the stage reports its own time: {stage} ms");
    assert!(
        took >= stage,
        "`took` ({took} ms) is what the caller waited, so it includes the stage ({stage} ms)"
    );
}

#[tokio::test]
async fn top_level_min_score_cuts_engine_scores_and_rerank_min_score_cuts_probabilities() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, plain) = node
        .search("/kb/_search", json!({"query": match_q(), "size": 4}))
        .await;
    let hits = plain["hits"]["hits"].as_array().unwrap();
    // A bar between the second and third engine scores.
    let bar = (hits[1]["_score"].as_f64().unwrap() + hits[2]["_score"].as_f64().unwrap()) / 2.0;
    let survivors: Vec<String> = ids(&plain)[..2].to_vec();

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "min_score": bar, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // Top-level `min_score` is the engine's, applied to ENGINE scores before
    // the window is cut — so it shapes the match set and the total, exactly as
    // it does without `rerank`…
    assert_eq!(r["hits"]["total"]["value"], 2, "{r}");
    assert_eq!(
        r["_rerank"]["judged"], 2,
        "only survivors are sent to the judge"
    );
    let mut got = ids(&r);
    got.sort();
    let mut want = survivors.clone();
    want.sort();
    assert_eq!(got, want);
    // …and it is NOT compared against the probabilities that replace `_score`:
    // both survivors score below `bar` as probabilities and are still here.
    for h in r["hits"]["hits"].as_array().unwrap() {
        assert!(h["_score"].as_f64().unwrap() <= 1.0);
    }

    // The two thresholds compose: engine bar first, probability bar second.
    let (_, both) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "min_score": bar,
                   "rerank": {"min_score": 0.2}}),
        )
        .await;
    for h in both["hits"]["hits"].as_array().unwrap() {
        assert!(h["_score"].as_f64().unwrap() >= 0.2, "{both}");
    }
    assert_eq!(both["hits"]["total"]["value"], 2);
}

#[tokio::test]
async fn a_knn_section_needs_the_question_spelled_out_unless_a_text_query_carries_it() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    let (st, b) = node
        .call(
            "PUT",
            "/vec",
            json!({"mappings": {"properties": {
                "title": {"type": "text"}, "body": {"type": "text"},
                "v": {"type": "dense_vector", "dims": 3}
            }}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    for (id, title, body, v) in [
        ("1", "alpha one", "alpha beta", [0.1, 0.2, 0.3]),
        ("2", "alpha two", "alpha gamma", [0.2, 0.1, 0.4]),
        ("3", "only by vector", "zzz qqq", [0.1, 0.2, 0.3]),
    ] {
        let (st, b) = node
            .call(
                "PUT",
                &format!("/vec/_doc/{id}"),
                json!({"title": title, "body": body, "v": v}),
            )
            .await;
        assert!(st.is_success(), "{b}");
    }
    node.call("POST", "/vec/_refresh", json!({})).await;
    let knn = json!({"field": "v", "query_vector": [0.1, 0.2, 0.3], "k": 3, "num_candidates": 10});

    // A vector is not a question.
    let (st, r) = node
        .search("/vec/_search", json!({"knn": knn, "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    assert!(reason(&r).contains("rerank.query"), "{r}");
    assert!(
        reason(&r).contains("knn"),
        "the reason names the knn case: {r}"
    );

    // Spelled out, a pure-kNN search reranks.
    stub.by_title(&[
        ("only by vector", 0.8),
        ("alpha two", 0.6),
        ("alpha one", 0.1),
    ]);
    let (st, r) = node
        .search(
            "/vec/_search",
            json!({"knn": knn, "size": 3, "_source": ["title", "body"],
                   "rerank": {"query": "which one is reachable only by vector?"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(ids(&r), ["3", "2", "1"], "{r}");

    // knn beside a text query: the question is read from the text query, and
    // the vector-only document is inside the window the judge sees.
    let (st, r) = node
        .search(
            "/vec/_search",
            json!({"query": {"match": {"body": "alpha"}}, "knn": knn, "size": 3,
                   "_source": ["title", "body"], "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["query"], "alpha");
    assert_eq!(r["_rerank"]["judged"], 3, "{r}");
    assert_eq!(ids(&r)[0], "3", "{r}");
    // No vector was sent to a text judge.
    assert!(
        !stub.everything_sent().contains("0.2"),
        "{}",
        stub.everything_sent()
    );
}

#[tokio::test]
async fn msearch_items_carrying_rerank_are_refused_per_item() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let ndjson = format!(
        "{}\n{}\n{}\n{}\n",
        json!({"index": "kb"}),
        json!({"query": match_q(), "rerank": {}}),
        json!({"index": "kb"}),
        json!({"query": match_q(), "size": 2}),
    );
    let (st, text) = node.raw("POST", "/_msearch", ndjson).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    let items = r["responses"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{r}");
    assert_eq!(items[0]["status"], 400, "{r}");
    assert!(reason(&items[0]).contains("_msearch"), "{r}");
    assert!(
        reason(&items[0]).contains("_search"),
        "says where to send it: {r}"
    );
    assert_eq!(
        items[1]["hits"]["hits"].as_array().map(Vec::len),
        Some(2),
        "the rest of the batch still runs: {r}"
    );
    assert!(stub.calls().is_empty());
}

#[tokio::test]
async fn templates_async_search_and_scroll_refuse_rerank_instead_of_dropping_it() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search/template",
            json!({"source": {"query": {"match": {"body": "{{q}}"}}, "rerank": {}},
                   "params": {"q": "vitamin"}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "_search/template: {r}");
    assert!(reason(&r).contains("_search/template"), "{r}");

    let ndjson = format!(
        "{}\n{}\n",
        json!({"index": "kb"}),
        json!({"source": {"query": {"match": {"body": "{{q}}"}}, "rerank": {}}, "params": {"q": "vitamin"}}),
    );
    let (st, text) = node.raw("POST", "/_msearch/template", ndjson).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["responses"][0]["status"], 400, "{r}");
    assert!(
        reason(&r["responses"][0]).contains("_msearch/template"),
        "{r}"
    );

    let (st, r) = node
        .search(
            "/kb/_async_search",
            json!({"query": match_q(), "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "_async_search: {r}");
    assert!(reason(&r).contains("_async_search"), "{r}");

    let (st, r) = node
        .search(
            "/kb/_search_scroll?scroll=1m",
            json!({"query": match_q(), "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "_search_scroll: {r}");

    // A template WITHOUT the block is unaffected.
    let (st, r) = node
        .search(
            "/kb/_search/template",
            json!({"source": {"query": {"match": {"body": "{{q}}"}}}, "params": {"q": "vitamin"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(stub.calls().is_empty());
}

#[tokio::test]
async fn a_multi_index_search_is_reranked_across_its_indices() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;
    node.seed(
        "kb2",
        &[
            (
                "a",
                "Second index winner",
                "vitamin d and bone density, again",
                "trial",
                1,
            ),
            ("b", "Second index loser", "vitamin gummies", "food", 2),
        ],
    )
    .await;
    stub.by_title(&[
        ("Second index winner", 0.99),
        ("Cooking", 0.80),
        ("Trial results", 0.60),
        ("Second index loser", 0.05),
    ]);

    let (st, r) = node
        .search(
            "/kb,kb2/_search",
            json!({"query": match_q(), "size": 6, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["judged"], 6, "{r}");
    assert_eq!(r["hits"]["total"]["value"], 6);
    let got: Vec<(String, String)> = r["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["_index"].as_str().unwrap().to_string(),
                h["_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(got[0], ("kb2".to_string(), "a".to_string()), "{r}");
    assert_eq!(got[1], ("kb".to_string(), "3".to_string()), "{r}");
    assert_eq!(got[2], ("kb".to_string(), "2".to_string()), "{r}");
    // `_index` still belongs to its `_id` after the reorder.
    for (index, id) in &got {
        let want = if id == "a" || id == "b" { "kb2" } else { "kb" };
        assert_eq!(index, want, "hit {id} claims index {index}");
    }

    // Same through a wildcard.
    let (st, r) = node
        .search(
            "/kb*/_search",
            json!({"query": match_q(), "size": 6, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["hits"]["hits"][0]["_id"], "a");
}

#[tokio::test]
async fn rescore_runs_first_and_rerank_has_the_last_word() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "rescore": {"window_size": 4, "query": {"rescore_query": {"match": {"body": "china"}}}},
                "rerank": {}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(ids(&r), INVERTED_IDS, "{r}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Visibility without exposure
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_status_endpoint_says_whether_it_is_configured_and_never_the_key() {
    let stub = Stub::start().await;
    let node = node_with(ProviderSettings::with_key_and_endpoint(
        "sk-this-must-never-be-printed",
        &stub.endpoint,
    ))
    .await;
    let (st, text) = node.raw("GET", "/_xerj/rerank", String::new()).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert!(!text.contains("sk-this-must-never-be-printed"), "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["enabled"], true);
    assert_eq!(r["configured"], true);
    assert_eq!(r["api_key"], json!({"set": true, "source": "config"}));
    assert_eq!(r["endpoint"]["url"], stub.endpoint);
    assert_eq!(r["limits"]["max_docs_per_call"], 30);
    // The string ceilings are limits an operator (and an agent) can read,
    // like the numeric ones — and they are the crate's constants, not copies.
    assert_eq!(
        r["limits"]["max_instructions_chars"],
        xerj_rerank::MAX_INSTRUCTIONS_CHARS
    );
    assert_eq!(r["limits"]["max_query_chars"], xerj_rerank::MAX_QUERY_CHARS);
    assert_eq!(r["limits"]["max_model_chars"], xerj_rerank::MAX_MODEL_CHARS);
    assert_eq!(r["limits"]["max_fields"], xerj_rerank::MAX_FIELDS);
    assert_eq!(r["defaults"]["max_concurrency"], 8);
    assert!(r["data_egress"]
        .as_str()
        .unwrap()
        .contains("sends the text"));

    let bare = node_with(ProviderSettings::resolve(true, "", "", None, None)).await;
    let (_, r) = bare.call("GET", "/_xerj/rerank", Value::Null).await;
    assert_eq!(r["configured"], false, "{r}");
    assert_eq!(r["api_key"]["set"], false);
    assert_eq!(r["endpoint"]["source"], "default");
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. The surfaces that do not run the stage, and the operator's meter
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_native_search_api_refuses_rerank_instead_of_dropping_it() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
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

    // Its request struct ignores unknown keys, so this used to be a 200 in the
    // engine's order for a caller who believed it had been reranked.
    let (st, text) = send(json!({"q": "vitamin", "rerank": {}})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("rerank"), "{text}");
    assert!(text.contains("_search"), "says where to send it: {text}");
    assert!(stub.calls().is_empty());

    // Without the block the native search is untouched.
    let (st, text) = send(json!({"q": "vitamin"})).await;
    assert_eq!(st, StatusCode::OK, "{text}");
}

/// The operator pays per judged document, on a volume the caller chooses.
#[tokio::test]
async fn the_operator_can_meter_rerank_from_prometheus() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    // applied: 4 judged, the stub reports 100 in / 10 out.
    let (st, _) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    // refused before the search ran: nothing sent, nothing billed.
    let (st, _) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "rerank": {"treshold": 1}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // failed: the provider was called and answered 401.
    stub.fault(Fault::Status(401));
    let (st, _) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": {}}))
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    // degraded: the deadline ran out.
    stub.fault(Fault::Delay(Duration::from_secs(2)));
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "rerank": {"timeout_ms": 100}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], false);
    // A search with no `rerank` block is not counted at all.
    let (st, _) = node
        .search("/kb/_search", json!({"query": match_q()}))
        .await;
    assert_eq!(st, StatusCode::OK);

    let (st, text) = node.raw("GET", "/v1/metrics", String::new()).await;
    assert_eq!(st, StatusCode::OK);
    let value = |needle: &str| -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(needle))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    };
    for (series, want) in [
        (r#"xerj_rerank_requests_total{outcome="applied"}"#, 1.0),
        (r#"xerj_rerank_requests_total{outcome="refused"}"#, 1.0),
        (r#"xerj_rerank_requests_total{outcome="failed"}"#, 1.0),
        (r#"xerj_rerank_requests_total{outcome="degraded"}"#, 1.0),
        ("xerj_rerank_documents_judged_total", 4.0),
        (r#"xerj_rerank_provider_tokens_total{kind="input"}"#, 100.0),
        (r#"xerj_rerank_provider_tokens_total{kind="output"}"#, 10.0),
    ] {
        assert_eq!(value(series), Some(want), "{series}\n{text}");
    }
}

/// `docvalue_fields` also puts values on a hit, so the pre-flight prediction
/// must stand aside for it rather than refuse a request that works.
#[tokio::test]
async fn text_returned_through_docvalue_fields_is_judgeable() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "_source": false, "docvalue_fields": ["cat"],
                "rerank": {"fields": ["cat"], "query": "which category is a trial?"}
            }),
        )
        .await;
    // The engine returns `cat` under `fields`, so it is judged — on exactly the
    // text that came back. The first draft of `prepare` gated on `fields`
    // alone and refused this pre-flight, with a message naming `fields` as the
    // only way; this test accepted that refusal and so passed vacuously.
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["_rerank"]["judged"], 4, "{r}");
    let sent = stub.everything_sent();
    assert!(sent.contains("trial"), "{sent}");
    assert!(
        !sent.contains("supplementation"),
        "`_source: false` returned no body, so no body was sent: {sent}"
    );
}

/// The provider is a third party. A verdict it returns for a document XERJ
/// never sent must not score that document, and must not move the meter.
#[tokio::test]
async fn a_verdict_for_a_document_that_was_never_sent_is_ignored() {
    let stub = Stub::start().await;
    stub.fault(Fault::AnswerUnsent);
    let node = node(&stub).await;
    node.seed_kb().await;
    // A fifth hit with no prose: skipped, so never sent.
    let (st, b) = node.call("PUT", "/kb/_doc/9", json!({"n": 9})).await;
    assert!(st.is_success(), "{b}");
    node.call("POST", "/kb/_refresh", json!({})).await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": {"match_all": {}}, "size": 5, "rerank": {"query": "bone density"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["skipped_no_text"], 1, "{r}");
    assert_eq!(
        r["_rerank"]["judged"], 4,
        "four documents were sent; the stub answered for ten: {r}"
    );
    let nine = r["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["_id"] == "9")
        .expect("unjudged, not dropped")
        .clone();
    assert!(
        nine["_score"].is_null(),
        "doc 9 was never sent, so the stub's 0.99 for it is not its score: {r}"
    );
    assert_eq!(ids(&r).last().map(String::as_str), Some("9"), "{r}");
    assert_eq!(r["_rerank"]["unjudged"], 1, "{r}");
}

/// `fields: ["_passage"]` returns the matching slice of a document. It is an
/// array of OBJECTS, so it needs reading deliberately — and it is the best thing
/// to send: the part that matched, rather than the first `max_doc_chars` of a
/// long document, and less text leaving the machine.
#[tokio::test]
async fn the_matching_passage_is_judgeable_and_goes_first() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;
    let bodies = [
        "vitamin supplements are popular. vitamin vitamin vitamin vitamin.",
        "vitamin d supplementation improved bone density in the treatment group",
        "vitamin rich vegetables for dinner",
        "bone china has high density",
    ];

    // Named explicitly: the passage and nothing else.
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "_source": ["title"], "fields": ["_passage"],
                "rerank": {"fields": ["_passage"]}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert!(r["_rerank"].get("fields_without_text").is_none(), "{r}");
    let sent = stub.calls().last().unwrap().body.clone();
    for (key, doc) in sent["state"]["documents"].as_object().unwrap() {
        assert!(
            doc.get("title").is_none(),
            "{key}: the title was not named: {doc}"
        );
        let text = doc["text"].as_str().unwrap();
        assert!(!text.trim().is_empty(), "{key}: {doc}");
        assert!(
            bodies.iter().any(|b| b.contains(text.trim())),
            "{key}: a passage is a slice of the body it came from: {text:?}"
        );
    }

    // Not named: the passage still leads, ahead of the rest of `_source`, so a
    // clip at `max_doc_chars` cuts the tail and never the part that matched.
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({
                "query": match_q(), "size": 4,
                "_source": ["title", "cat"], "fields": ["_passage"],
                "rerank": {}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let sent = stub.calls().last().unwrap().body.clone();
    for (key, doc) in sent["state"]["documents"].as_object().unwrap() {
        let text = doc["text"].as_str().unwrap();
        let first_line = text.lines().next().unwrap_or_default();
        assert!(
            bodies.iter().any(|b| b.contains(first_line.trim())) && !first_line.trim().is_empty(),
            "{key}: the passage comes first: {text:?}"
        );
        assert!(
            ["health", "trial", "food", "materials"].contains(&text.lines().last().unwrap()),
            "{key}: then the returned `_source` strings: {text:?}"
        );
    }
}

/// The large-response hint offers a "ready-to-send" corrected request. For a
/// caller that asked for `rerank`, a suggestion without it is a different
/// search: following it would silently return the engine's order.
#[tokio::test]
async fn the_response_hint_keeps_the_rerank_block() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    let filler = "lorem ipsum dolor sit amet ".repeat(120); // ~3.2 KB each
    let big: Vec<(String, String)> = (0..3)
        .map(|i| (format!("{i}"), format!("shared keyword {filler} {i}")))
        .collect();
    let seeded: Vec<(&str, &str, &str, &str, i64)> = big
        .iter()
        .map(|(id, body)| (id.as_str(), "a long one", body.as_str(), "c", 1))
        .collect();
    node.seed("big", &seeded).await;

    let rerank = json!({"min_score": 0.0, "query": "which one is shared?"});
    let (st, r) = node
        .search(
            "/big/_search",
            json!({"query": {"match": {"body": "shared"}}, "size": 3, "rerank": rerank}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // Several diagnostics can share the `hints` array; find the payload one.
    let payload_hint = |resp: &Value| -> Option<Value> {
        resp["_xerj"]["hints"]
            .as_array()?
            .iter()
            .find(|h| h["try"]["body"]["fields"] == json!(["_passage"]))
            .cloned()
    };
    let hint = payload_hint(&r)
        .unwrap_or_else(|| panic!("an unprojected ~10 KB response carries the hint: {r}"));
    assert_eq!(
        hint["try"]["body"]["rerank"], rerank,
        "the suggested request is still the caller's search: {hint}"
    );

    // Without `rerank` the suggestion does not grow one.
    let (_, plain) = node
        .search(
            "/big/_search",
            json!({"query": {"match": {"body": "shared"}}, "size": 3}),
        )
        .await;
    let hint = payload_hint(&plain).expect("same payload, same hint");
    assert!(hint["try"]["body"].get("rerank").is_none(), "{hint}");
}

/// The 502's `reason` carries the provider's error body to whoever ran the
/// search — not necessarily the operator who owns the key. A provider that
/// echoes the credential in its auth error must not turn every search caller
/// into someone who can read it.
#[tokio::test]
async fn a_provider_error_that_echoes_the_key_never_reaches_the_caller() {
    let stub = Stub::start().await;
    stub.fault(Fault::EchoKey);
    let node = node_with(ProviderSettings::with_key_and_endpoint(
        "sk-operator-secret-do-not-leak",
        &stub.endpoint,
    ))
    .await;
    node.seed_kb().await;

    let (st, text) = node
        .raw(
            "POST",
            "/kb/_search",
            json!({"query": match_q(), "rerank": {}}).to_string(),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{text}");
    assert!(
        stub.calls()[0]
            .authorization
            .as_deref()
            .is_some_and(|a| a.contains("sk-operator-secret-do-not-leak")),
        "the fixture is only a test if the stub really received the key"
    );
    assert!(
        !text.contains("sk-operator-secret-do-not-leak"),
        "the operator's key leaked to the search caller: {text}"
    );
    assert!(text.contains("<redacted>"), "{text}");
    assert!(
        text.contains("401"),
        "the status still gets through: {text}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Review remediation (PR #946): provider answers that are partial, wrong or
//    duplicated must never produce a response that contradicts itself.
// ─────────────────────────────────────────────────────────────────────────────

/// Twelve documents with distinct titles and one common body term, so the
/// engine returns all of them and the judge can tell them apart.
async fn seed_dozen(node: &Node) {
    let docs: Vec<(String, String)> = (0..12)
        .map(|i| (format!("{i}"), format!("doc number {i}")))
        .collect();
    let seeded: Vec<(&str, &str, &str, &str, i64)> = docs
        .iter()
        .map(|(id, title)| (id.as_str(), title.as_str(), "common term here", "c", 1))
        .collect();
    node.seed("dozen", &seeded).await;
}

fn scores_of(r: &Value) -> Vec<Option<f64>> {
    r["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["_score"].as_f64())
        .collect()
}

/// The provider answers half the window. The response must still be one a
/// client can read without contradiction: every `_score` is a probability or
/// `null`, never the engine's BM25 next to probabilities; `max_score` is the
/// maximum of every score on the page; scores descend; unjudged hits are
/// counted and sort last.
#[tokio::test]
async fn a_partial_verdict_set_keeps_score_single_kind_on_the_wire() {
    let stub = Stub::start().await;
    stub.fault(Fault::Partial);
    stub.by_title(&[
        ("doc number 0", 0.9),
        ("doc number 1", 0.8),
        ("doc number 2", 0.7),
        ("doc number 3", 0.6),
        ("doc number 4", 0.5),
        ("doc number 5", 0.4),
        ("doc number 6", 0.3),
        ("doc number 7", 0.2),
        ("doc number 8", 0.1),
    ]);
    let node = node(&stub).await;
    seed_dozen(&node).await;

    let (st, r) = node
        .search(
            "/dozen/_search",
            json!({"query": {"match": {"body": "common"}}, "size": 12, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let rr = &r["_rerank"];
    assert_eq!(rr["applied"], true, "{rr}");
    assert_eq!(rr["score_kind"], "probability", "{rr}");
    assert_eq!(rr["window"], 12, "{rr}");
    assert_eq!(rr["judged"], 6, "{rr}");
    assert_eq!(rr["unjudged"], 6, "{rr}");
    assert_eq!(rr["skipped_no_text"], 0, "{rr}");

    let scores = scores_of(&r);
    assert_eq!(scores.len(), 12, "{r}");
    let judged: Vec<f64> = scores.iter().flatten().copied().collect();
    assert_eq!(judged.len(), 6, "six probabilities, six nulls: {scores:?}");
    assert!(
        scores[..6].iter().all(Option::is_some) && scores[6..].iter().all(Option::is_none),
        "judged hits first, unjudged after, nothing interleaved: {scores:?}"
    );
    for p in &judged {
        assert!(
            (0.0..=1.0).contains(p),
            "a BM25 value leaked into _score: {scores:?}"
        );
    }
    assert!(
        judged.windows(2).all(|w| w[0] >= w[1]),
        "_score must not rise down the page: {scores:?}"
    );
    let max = r["hits"]["max_score"]
        .as_f64()
        .expect("max_score is a number");
    assert!(
        judged.iter().all(|p| *p <= max),
        "max_score {max} is below a later hit's _score: {scores:?}"
    );
    assert_eq!(
        Some(max),
        scores[0],
        "max_score is the top hit's score: {r}"
    );
    // `hits.total` is still the engine's: twelve matched, whatever was judged.
    assert_eq!(r["hits"]["total"]["value"], 12, "{r}");
}

/// The provider answers, but only about documents nobody sent. That is not a
/// reranked order, and the response must not say it is — nor may the
/// operator's meter count it as applied.
#[tokio::test]
async fn verdicts_only_for_unknown_keys_degrade_instead_of_claiming_the_judges_order() {
    let stub = Stub::start().await;
    stub.fault(Fault::WrongKeys);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (_, base) = node
        .search("/kb/_search", json!({"query": match_q(), "size": 3}))
        .await;
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 3, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let rr = &r["_rerank"];
    assert_eq!(
        rr["applied"], false,
        "verdicts for d999 and x are not a reranking: {rr}"
    );
    assert_eq!(rr["score_kind"], "engine", "{rr}");
    let why = rr["reason"].as_str().unwrap_or_default();
    assert!(
        why.contains("no scores") || why.contains("no verdict"),
        "the reason says nothing came back for what was sent: {rr}"
    );
    assert!(
        rr.get("judged").is_none(),
        "a degraded block has no `judged`: {rr}"
    );
    assert_eq!(ids(&r), ids(&base), "the engine's order, untouched: {r}");
    assert_eq!(
        scores_of(&r),
        scores_of(&base),
        "and the engine's scores: {r}"
    );
    assert_eq!(stub.calls().len(), 1);

    let (_, text) = node.raw("GET", "/v1/metrics", String::new()).await;
    let value = |needle: &str| -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(needle))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    };
    assert_eq!(
        value(r#"xerj_rerank_requests_total{outcome="degraded"}"#),
        Some(1.0),
        "{text}"
    );
    assert_ne!(
        value(r#"xerj_rerank_requests_total{outcome="applied"}"#),
        Some(1.0),
        "must not be counted as applied: {text}"
    );
    assert_eq!(
        value("xerj_rerank_documents_judged_total").unwrap_or(0.0),
        0.0,
        "nothing was judged, nothing is billed: {text}"
    );
}

/// A provider that echoes `d0..d59` back on every call must not turn a
/// 35-document window into 105 judged documents (three verdicts per hit, the
/// last one winning) or triple the billing meter. One verdict per document,
/// from the batch that sent it.
#[tokio::test]
async fn a_verdict_echoed_from_another_batch_is_not_counted_twice() {
    let stub = Stub::start().await;
    stub.fault(Fault::EchoOtherBatches);
    let node = node(&stub).await;
    let docs: Vec<(String, String)> = (0..35)
        .map(|i| (format!("{i}"), format!("doc number {i}")))
        .collect();
    let seeded: Vec<(&str, &str, &str, &str, i64)> = docs
        .iter()
        .map(|(id, title)| (id.as_str(), title.as_str(), "common term here", "c", 1))
        .collect();
    node.seed("many", &seeded).await;
    stub.by_title(&[("doc number 33", 0.95)]);

    let (st, r) = node
        .search(
            "/many/_search",
            json!({"query": {"match": {"body": "common"}}, "size": 35,
                   "rerank": {"window": 35, "batch": 10}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(stub.calls().len(), 4, "35 documents in batches of 10");
    let rr = &r["_rerank"];
    assert_eq!(rr["applied"], true, "{rr}");
    assert_eq!(rr["window"], 35, "{rr}");
    assert_eq!(
        rr["judged"], 35,
        "one verdict per document, not per echo: {rr}"
    );
    assert_eq!(rr["unjudged"], 0, "{rr}");
    assert_eq!(
        ids(&r)[0],
        "33",
        "the real verdict wins over the echoed 0.5: {r}"
    );
    let scores = scores_of(&r);
    assert!(scores.iter().all(Option::is_some), "{scores:?}");
    assert_eq!(scores[0], Some(0.95), "{scores:?}");

    let (_, text) = node.raw("GET", "/v1/metrics", String::new()).await;
    let judged_total = text
        .lines()
        .find(|l| l.starts_with("xerj_rerank_documents_judged_total"))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse::<f64>().ok());
    assert_eq!(
        judged_total,
        Some(35.0),
        "the meter counts documents: {text}"
    );
}

/// The deadline stops batches from ever being sent. Those documents are as
/// unjudged as the ones in a batch that failed, and the block must account
/// for the whole window: `judged` plus `partial_failures` batches.
#[tokio::test]
async fn batches_the_deadline_never_dispatched_count_as_partial_failures() {
    let stub = Stub::start().await;
    stub.fault(Fault::Delay(Duration::from_millis(1000)));
    let node = node(&stub).await;
    let docs: Vec<(String, String)> = (0..35)
        .map(|i| (format!("{i}"), format!("doc number {i}")))
        .collect();
    let seeded: Vec<(&str, &str, &str, &str, i64)> = docs
        .iter()
        .map(|(id, title)| (id.as_str(), title.as_str(), "common term here", "c", 1))
        .collect();
    node.seed("many", &seeded).await;

    // Batch 1 answers at ~1.0 s; batch 2 is sent with ~0.8 s left and times
    // out; batches 3 and 4 are never sent.
    let (st, r) = node
        .search(
            "/many/_search",
            json!({"query": {"match": {"body": "common"}}, "size": 5,
                   "rerank": {"window": 35, "batch": 10, "max_concurrency": 1,
                              "timeout_ms": 1800}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let rr = &r["_rerank"];
    assert_eq!(
        rr["applied"], true,
        "one batch answered, so its order stands: {rr}"
    );
    assert_eq!(rr["window"], 35, "{rr}");
    assert_eq!(rr["judged"], 10, "{rr}");
    assert_eq!(rr["unjudged"], 25, "{rr}");
    assert_eq!(
        rr["partial_failures"], 3,
        "one batch timed out and two were never dispatched: {rr}"
    );
    assert_eq!(stub.calls().len(), 2, "batches 3 and 4 were never sent");
}

/// A scroll continuation that carries `rerank` is refused like the `?scroll=`
/// open is; and `"rerank": null` means "no rerank" on every surface, so an
/// `_msearch` item and a `_search` body with it behave the same way.
#[tokio::test]
async fn scroll_continuation_refuses_rerank_and_null_is_absent_everywhere() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, opened) = node
        .search(
            "/kb/_search?scroll=1m",
            json!({"query": match_q(), "size": 2}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{opened}");
    let scroll_id = opened["_scroll_id"]
        .as_str()
        .expect("scroll id")
        .to_string();
    let (st, r) = node
        .search(
            "/_search/scroll",
            json!({"scroll": "1m", "scroll_id": scroll_id, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "_search/scroll: {r}");
    assert!(reason(&r).contains("_search/scroll"), "{r}");
    let (st, r) = node
        .search(
            "/_search/scroll",
            json!({"scroll": "1m", "scroll_id": scroll_id}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "the continuation itself still works: {r}"
    );

    let (st, r) = node
        .search("/kb/_search", json!({"query": match_q(), "rerank": null}))
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r.get("_rerank").is_none(), "null is absent: {r}");

    let ndjson = format!(
        "{}\n{}\n",
        json!({"index": "kb"}),
        json!({"query": match_q(), "size": 2, "rerank": null}),
    );
    let (st, text) = node.raw("POST", "/_msearch", ndjson).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        r["responses"][0]["hits"]["hits"].as_array().map(Vec::len),
        Some(2),
        "null is absent on _msearch too, not a refused block: {r}"
    );
    assert!(stub.calls().is_empty());
}

/// A point-in-time search has no `sort` or `search_after` of its own, so the
/// stage runs on it; paging it with `search_after` is refused like any other.
#[tokio::test]
async fn a_pit_search_runs_the_stage() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, pit) = node.call("POST", "/kb/_pit?keep_alive=1m", json!({})).await;
    assert_eq!(st, StatusCode::OK, "{pit}");
    let id = pit["id"].as_str().expect("pit id").to_string();

    let (st, r) = node
        .search(
            "/_search",
            json!({"pit": {"id": id, "keep_alive": "1m"}, "query": match_q(), "size": 4,
                   "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(ids(&r), INVERTED_IDS, "{r}");
}

/// The stage widens the engine's page to `rerank.window`. On an index whose
/// `index.max_result_window` is smaller, the engine's own check fired with
/// "from + size > max_result_window" — numbers the caller never sent. The
/// refusal is now in the caller's terms, per index, and a wildcard search that
/// touches such an index names it.
#[tokio::test]
async fn a_small_max_result_window_is_refused_in_the_callers_terms() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;
    let (st, b) = node
        .call(
            "PUT",
            "/kbsmall",
            json!({"settings": {"index": {"max_result_window": 20}},
                   "mappings": {"properties": {"title": {"type": "text"}, "body": {"type": "text"}}}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    node.call(
        "PUT",
        "/kbsmall/_doc/1",
        json!({"title": "t", "body": "vitamin d"}),
    )
    .await;
    node.call("POST", "/kbsmall/_refresh", json!({})).await;

    let (st, r) = node
        .search(
            "/kbsmall/_search",
            json!({"query": match_q(), "size": 5, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let why = reason(&r);
    assert!(why.contains("`rerank.window` (30)"), "{why}");
    assert!(why.contains("`index.max_result_window` (20)"), "{why}");
    assert!(why.contains("kbsmall"), "{why}");
    assert!(
        !why.contains("from + size"),
        "not the engine's page arithmetic: {why}"
    );

    // The wildcard search names the index that cannot serve the window.
    let (st, r) = node
        .search(
            "/kb*/_search",
            json!({"query": match_q(), "size": 5, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    assert!(reason(&r).contains("kbsmall"), "{r}");

    // A window that fits works, on the small index and across the wildcard.
    let (st, r) = node
        .search(
            "/kb*/_search",
            json!({"query": match_q(), "size": 5, "rerank": {"window": 20}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert!(
        stub.calls().len() == 1,
        "only the fitting request reached the provider"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. Second review round (PR #946 at 2cc53f60)
// ─────────────────────────────────────────────────────────────────────────────

/// Forty matching documents, for windows wider than one provider call.
async fn seed_forty(node: &Node) {
    let docs: Vec<(String, String, String)> = (0..40)
        .map(|i| {
            (
                i.to_string(),
                format!("Doc {i}"),
                format!("vitamin d supplementation bone density note {i}"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str, &str, &str, i64)> = docs
        .iter()
        .enumerate()
        .map(|(i, (id, t, b))| (id.as_str(), t.as_str(), b.as_str(), "c", i as i64))
        .collect();
    node.seed("kb40", &refs).await;
}

/// MAJOR (review round 2): `rerank.instructions` had no ceiling, and the wire
/// format repeats it inside every per-document question. One ~1 MB string at
/// `window: 40`, `max_doc_chars: 1` put 40 MB on the wire to the provider for a
/// `size: 1` search. It is a caller-chosen cost knob like the window, so it
/// gets a server-side ceiling like the window: a 400 that names the limit, and
/// nothing sent.
#[tokio::test]
async fn oversized_instructions_query_and_model_are_refused_and_send_nothing() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    seed_forty(&node).await;

    let big = "y".repeat(1_000_000);
    for (name, body, needle) in [
        (
            "instructions",
            json!({"query": match_q(), "size": 1,
                   "rerank": {"window": 40, "max_doc_chars": 1, "instructions": big}}),
            "rerank.instructions",
        ),
        (
            "rerank.query",
            json!({"query": match_q(), "size": 1, "rerank": {"query": big}}),
            "rerank.query",
        ),
        (
            "model",
            json!({"query": match_q(), "size": 1, "rerank": {"model": big}}),
            "rerank.model",
        ),
        (
            // The question read out of the search query is the same string on
            // the same wire, so it has the same ceiling — and the refusal says
            // what to do about it.
            "inferred question",
            json!({"query": {"match": {"body": big}}, "size": 1, "rerank": {}}),
            "rerank.query",
        ),
    ] {
        let (st, r) = node.search("/kb40/_search", body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}: {}", reason(&r));
        let why = reason(&r);
        assert!(why.contains(needle), "{name}: {why}");
        assert!(
            why.contains("characters"),
            "{name}: the refusal names the limit: {why}"
        );
        assert!(
            why.len() < 600,
            "{name}: the refusal must not echo the oversized value ({} bytes)",
            why.len()
        );
    }
    assert!(
        stub.calls().is_empty(),
        "a refused request must not have sent a byte to the provider"
    );

    // At the ceiling it runs, and what one search can put on the wire is
    // bounded by the documented ceilings alone: 40 documents of 1 character
    // each, under instructions at the limit.
    let (st, r) = node
        .search(
            "/kb40/_search",
            json!({"query": match_q(), "size": 1,
                   "rerank": {"window": 40, "max_doc_chars": 1,
                              "instructions": "y".repeat(xerj_rerank::MAX_INSTRUCTIONS_CHARS)}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["_rerank"]["judged"], 40, "{r}");
    let sent: usize = stub.calls().iter().map(|c| c.body.to_string().len()).sum();
    let bound = 40 * (xerj_rerank::MAX_INSTRUCTIONS_CHARS + 2 + 512) + 2 * 1_024;
    assert!(
        sent <= bound,
        "{sent} bytes left the node for a 40-document window of 1-character documents; \
         the ceilings allow {bound}"
    );
}

/// `_rank_eval` builds its own search from `requests[].request` and the parser
/// ignores keys it does not know, so a `rerank` block vanished and the metric
/// was computed over the engine's order — "reranking does not help", under a
/// 200, on the one endpoint whose job is measuring ranking quality. It is
/// recorded per request in `failures` (the channel `_rank_eval` has for a
/// request it cannot run), the rest of the batch is evaluated, and nothing is
/// sent to the provider.
#[tokio::test]
async fn rank_eval_refuses_a_rerank_block_instead_of_scoring_the_engines_order() {
    let stub = Stub::start().await;
    inverted(&stub);
    let node = node(&stub).await;
    node.seed_kb().await;

    let ratings = json!([{"_index": "kb", "_id": "3", "rating": 1}]);
    let (st, r) = node
        .call(
            "POST",
            "/kb/_rank_eval",
            json!({
                "requests": [
                    {"id": "with_rerank",
                     "request": {"query": match_q(), "rerank": {"window": 30}},
                     "ratings": ratings},
                    {"id": "plain", "request": {"query": match_q()}, "ratings": ratings},
                    {"id": "null_is_absent",
                     "request": {"query": match_q(), "rerank": null},
                     "ratings": ratings}
                ],
                "metric": {"precision": {"k": 1}}
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let failure = &r["failures"]["with_rerank"];
    assert!(
        failure.is_object(),
        "a request carrying `rerank` must be reported in `failures`, not scored: {r}"
    );
    let why = failure["reason"].as_str().unwrap_or_default();
    assert!(why.contains("rerank"), "{r}");
    assert!(why.contains("_rank_eval"), "{r}");
    assert_eq!(
        failure["type"], "illegal_argument_exception",
        "the same refusal every other surface gives, not a failed search: {r}"
    );
    assert!(
        r["details"]["with_rerank"].is_null(),
        "no metric may be published for it: {r}"
    );
    // The siblings ran, on the engine's order, and say so by being in details.
    assert!(r["details"]["plain"].is_object(), "{r}");
    assert!(r["details"]["null_is_absent"].is_object(), "{r}");
    assert!(r["failures"]["plain"].is_null(), "{r}");
    assert!(r["failures"]["null_is_absent"].is_null(), "{r}");
    assert!(
        stub.calls().is_empty(),
        "_rank_eval does not run the stage, so nothing may reach the provider"
    );
}

/// The interaction table says the judge reads what the response returns, and
/// `prepare`'s own refusal says "return the text through `fields`". In the
/// default mode (no `rerank.fields`) the stage read `_source` and `_passage`
/// only, so following that advice produced a second 400.
#[tokio::test]
async fn text_returned_only_through_fields_is_judged_without_naming_it() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    node.seed_kb().await;

    // `fields`
    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "_source": false,
                   "fields": ["body"], "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["_rerank"]["judged"], 4, "{r}");
    let sent = stub.everything_sent();
    assert!(sent.contains("supplementation"), "{sent}");
    assert!(
        !sent.contains("Trial results"),
        "the title was not returned, so it was not sent: {sent}"
    );

    // `docvalue_fields`: the keyword value is the only text on the hit.
    let stub2 = Stub::start().await;
    let node2 = crate::node(&stub2).await;
    node2.seed_kb().await;
    let (st, r) = node2
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "_source": false,
                   "docvalue_fields": ["cat"],
                   "rerank": {"query": "which category is a trial?"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["judged"], 4, "{r}");
    let sent = stub2.everything_sent();
    assert!(sent.contains("trial"), "{sent}");
    assert!(!sent.contains("supplementation"), "{sent}");

    // A field that is in BOTH `_source` and `fields` is sent once, not twice.
    let stub3 = Stub::start().await;
    let node3 = crate::node(&stub3).await;
    node3.seed_kb().await;
    let (st, r) = node3
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "fields": ["body"], "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    for call in stub3.calls() {
        for (_, doc) in call.body["state"]["documents"].as_object().unwrap() {
            let text = doc["text"].as_str().unwrap_or_default();
            assert!(
                text.matches("vitamin rich vegetables").count() <= 1
                    && text.matches("bone china").count() <= 1,
                "a value present in `_source` and `fields` was sent twice: {text}"
            );
        }
    }
}

/// `usage` is advisory metering. A provider that reports a negative or
/// otherwise odd token count must not cost the caller a ranking whose verdicts
/// were all valid (it used to be a 502 with no hits).
#[tokio::test]
async fn odd_provider_usage_does_not_veto_a_valid_ranking() {
    let stub = Stub::start().await;
    inverted(&stub);
    stub.fault(Fault::OddUsage);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["_rerank"]["applied"], true, "{r}");
    assert_eq!(r["_rerank"]["judged"], 4, "{r}");
    assert_eq!(ids(&r), INVERTED_IDS, "{r}");
    // Neither count is one a call can produce (`-5`; 2^63, far past what 30
    // documents at the ceilings hold), so both meter as zero — and the
    // operator's Prometheus counter is not poisoned by one bad response.
    assert_eq!(r["_rerank"]["usage"]["output_tokens"], 0, "{r}");
    assert_eq!(r["_rerank"]["usage"]["input_tokens"], 0, "{r}");
    let (_, metrics) = node.raw("GET", "/v1/metrics", String::new()).await;
    let input_line = metrics
        .lines()
        .find(|l| l.starts_with("xerj_rerank_provider_tokens_total{kind=\"input\"}"))
        .unwrap_or_default()
        .to_string();
    assert!(
        input_line.is_empty() || input_line.ends_with(" 0"),
        "an implausible token count reached the operator's meter: {input_line}"
    );
}

/// The provider is a third party. A legitimate answer for 30 documents is a
/// few kilobytes; a response of megabytes is not one, and reading it whole
/// would let whatever answers at the endpoint choose how much memory a search
/// allocates. It is a contract break — 502, no hits — not an allocation.
#[tokio::test]
async fn an_oversized_provider_response_is_a_502_not_an_allocation() {
    let stub = Stub::start().await;
    stub.fault(Fault::HugeBody);
    let node = node(&stub).await;
    node.seed_kb().await;

    let (st, r) = node
        .search(
            "/kb/_search",
            json!({"query": match_q(), "size": 4, "rerank": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{r}");
    assert_eq!(r["error"]["type"], "rerank_exception", "{r}");
    let why = reason(&r);
    assert!(why.contains("larger than"), "{why}");
    assert!(r["hits"].is_null(), "a surfaced fault returns no hits: {r}");
}

/// Each page request judges the window again — there is no verdict cache. The
/// docs say so; this pins the fact they state, so a future cache changes both.
#[tokio::test]
async fn every_page_request_judges_the_whole_window_again() {
    let stub = Stub::start().await;
    let node = node(&stub).await;
    seed_forty(&node).await;

    let mut judged = 0u64;
    for from in [0, 10, 20] {
        let (st, r) = node
            .search(
                "/kb40/_search",
                json!({"query": match_q(), "size": 10, "from": from, "rerank": {"window": 30}}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        judged += r["_rerank"]["judged"].as_u64().unwrap_or(0);
    }
    assert_eq!(stub.calls().len(), 3, "one provider call per page request");
    assert_eq!(
        judged, 90,
        "three pages of one 30-document window pay for 90"
    );
}
