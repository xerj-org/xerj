//! End-to-end checks for the decision cache flywheel (issue #1061): every
//! tier-2+ answer written back to the `[decisions]` index, every answer
//! naming its tier in a `source` field, and human corrections weighted
//! `decisions.human_weight`× (default 2.0) in the vote.
//!
//! Three of the contracts here hold in ANY build (they are the history
//! tier's, and the default node is history-only), so they are not
//! feature-gated:
//!
//! - the `source` field on both surfaces (`/_decide` top level,
//!   `/v1/systemone` per-question in `decisions.evidence.*.source`), without
//!   touching the strictly-parsed answer objects — the wire compatibility
//!   #1072 established;
//! - a history document carrying `human: true`, indexed through the ordinary
//!   `PUT /{index}/_doc` path, is weighted by the configured `human_weight`
//!   in the vote — at 1.0 the plain vote wins, at the default 2.0 the
//!   corrections flip it;
//! - history-tier answers are never written back (the write-back tests below
//!   assert the negative half of that on the same node).
//!
//! The write-back itself needs an answer produced by tier 2, so those tests
//! are gated on `decide-local` and arm the same tiny untrained fixture as
//! `systemone_local_decide.rs` — the probabilities are reproducible
//! arithmetic, not judgement quality, and nothing here measures accuracy.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use xerj_api::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// The node
// ─────────────────────────────────────────────────────────────────────────────

struct Node {
    native: axum::Router,
    app: axum::Router,
    _dir: tempfile::TempDir,
}

/// A node whose `[decisions]` block names `index`, with `human_weight`
/// overrideable (the flywheel's one new setting). No local tier: the
/// ungated tests are the history tier's contract.
async fn node_with_decisions(index: &str, human_weight: f64, min_confidence: f64) -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.decisions.index = index.to_string();
    config.decisions.human_weight = human_weight;
    config.decisions.min_confidence = min_confidence;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = AppState::new(config, engine, metrics);
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        app: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

impl Node {
    async fn call(
        &self,
        router: &axum::Router,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = router
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
            serde_json::from_str(&String::from_utf8_lossy(&bytes)).unwrap_or(Value::Null),
        )
    }

    async fn systemone(&self, body: Value) -> (StatusCode, Value) {
        self.call(&self.native.clone(), "POST", "/v1/systemone", body)
            .await
    }
    async fn decide(&self, body: Value) -> (StatusCode, Value) {
        self.call(&self.app.clone(), "POST", "/_decide", body).await
    }
    async fn put_doc(&self, index: &str, id: &str, doc: Value) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "PUT",
                &format!("/{index}/_doc/{id}"),
                doc,
            )
            .await;
        assert!(st.is_success(), "index {index}/{id}: {st} {b}");
    }
    async fn create_index(&self, index: &str) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "PUT",
                &format!("/{index}"),
                json!({"mappings": {"properties": {
                    "text": {"type": "text"},
                    "label": {"type": "keyword"}
                }}}),
            )
            .await;
        assert!(st.is_success(), "create {index}: {st} {b}");
    }
    async fn refresh(&self, index: &str) {
        let (st, b) = self
            .call(
                &self.app.clone(),
                "POST",
                &format!("/{index}/_refresh"),
                json!({}),
            )
            .await;
        assert!(st.is_success(), "refresh {index}: {st} {b}");
    }
    /// Every document's `_source` in the index, refreshed first.
    #[cfg_attr(not(feature = "decide-local"), allow(dead_code))]
    async fn docs(&self, index: &str) -> Vec<Value> {
        self.refresh(index).await;
        self.search_all(index).await
    }
    /// `_source` of every document, no refresh.
    #[cfg_attr(not(feature = "decide-local"), allow(dead_code))]
    async fn search_all(&self, index: &str) -> Vec<Value> {
        let (st, r) = self
            .call(
                &self.app.clone(),
                "POST",
                &format!("/{index}/_search"),
                json!({"query": {"match_all": {}}, "size": 100}),
            )
            .await;
        assert!(st.is_success(), "search {index}: {st} {r}");
        r["hits"]["hits"]
            .as_array()
            .map(|hits| hits.iter().map(|h| h["_source"].clone()).collect())
            .unwrap_or_default()
    }
    /// Poll until the index holds at least `n` documents — the write-back is
    /// a spawned task, so the test yields until it lands (and says so, with
    /// what it saw, if it never does). Tolerates the index not existing YET:
    /// in the bootstrap case the write-back itself creates it.
    #[cfg_attr(not(feature = "decide-local"), allow(dead_code))]
    async fn wait_for_docs(&self, index: &str, n: usize) -> Vec<Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let (rst, _) = self
                .call(
                    &self.app.clone(),
                    "POST",
                    &format!("/{index}/_refresh"),
                    json!({}),
                )
                .await;
            if rst.is_success() {
                let docs = self.search_all(index).await;
                if docs.len() >= n {
                    return docs;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("write-back did not land: wanted {n} docs in `{index}`");
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The source field — every build, both surfaces, wire-compatible
// ─────────────────────────────────────────────────────────────────────────────

/// Every answer names the tier that produced it: `/_decide` at the top
/// level, `/v1/systemone` per-question in the evidence — and the strictly
/// parsed answer objects are untouched, exactly as #1072 left them (extras
/// ride in `decisions`, which every verified client ignores).
#[tokio::test]
async fn every_answer_names_its_source_on_both_surfaces() {
    let node = node_with_decisions("history", 2.0, 0.0).await;
    node.create_index("history").await;
    node.put_doc(
        "history",
        "h1",
        json!({"text": "refund refund refund asked for money back", "label": "true"}),
    )
    .await;
    node.put_doc(
        "history",
        "h2",
        json!({"text": "refund policy says money back", "label": "true"}),
    )
    .await;
    node.refresh("history").await;

    // /_decide: the audit surface, top level.
    let (st, r) = node
        .decide(json!({"index": "history", "question": "refund asked for money back"}))
        .await;
    assert!(st.is_success(), "{st} {r}");
    assert_eq!(r["tier"], "history", "{r}");
    assert_eq!(r["source"], "history", "source names the tier: {r}");

    // /v1/systemone: per question, in the evidence — and the answer object
    // itself still carries ONLY its documented fields.
    let (st, r) = node
        .systemone(json!({
            "state": {"documents": {"doc_0": "refund asked for money back"}},
            "questions": {"d0": {"type": "noul", "instructions": "Is `documents.doc_0` about refunds?"}}
        }))
        .await;
    assert!(st.is_success(), "{st} {r}");
    assert_eq!(r["decisions"]["evidence"]["d0"]["tier"], "history", "{r}");
    assert_eq!(
        r["decisions"]["evidence"]["d0"]["source"], "history",
        "source names the tier: {r}"
    );
    let answer = r["answers"]["d0"].as_object().expect("answer: {r}");
    let mut keys = answer.keys().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        ["noul", "type"],
        "the answer object is unchanged — extras stay in `decisions`: {r}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Human corrections — ordinary write path, weighted ≥ 2x in the vote
// ─────────────────────────────────────────────────────────────────────────────

/// The flywheel's correction arithmetic over the real vote. One history
/// document says `billing` and ranks first (it matches every query term);
/// two HUMAN corrections say `tech` and rank below it. At `human_weight = 1.0`
/// (corrections are ordinary neighbours) billing wins; at the DEFAULT 2.0 the
/// corrections outweigh it — the same documents, the same ranks, only the
/// weight differs, so the flip is the weight's and nothing else's.
#[tokio::test]
async fn a_two_x_human_correction_flips_the_vote_the_plain_arithmetic_settles() {
    for (human_weight, expected) in [(1.0, "billing"), (2.0, "tech")] {
        let node = node_with_decisions("history", human_weight, 0.0).await;
        node.create_index("history").await;
        node.put_doc(
            "history",
            "plain",
            json!({"text": "refund money back guarantee", "label": "billing"}),
        )
        .await;
        // The corrections are written through the ORDINARY indexing path —
        // `human: true` is a field on a document, not a new API.
        node.put_doc(
            "history",
            "human1",
            json!({"text": "refund money", "label": "tech", "human": true}),
        )
        .await;
        node.put_doc(
            "history",
            "human2",
            json!({"text": "refund policy", "label": "tech", "human": true}),
        )
        .await;
        node.refresh("history").await;

        let (st, r) = node
            .decide(json!({"index": "history", "question": "refund money back guarantee"}))
            .await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(r["label"], expected, "human_weight {human_weight}: {r}");
        assert!(!r["abstain"].as_bool().unwrap_or(true), "{r}");
        assert_eq!(r["source"], "history", "{r}");
    }
}

/// The boosted weight is the weight the vote USED, so the audit surface must
/// show it: every neighbour's `weight` is its reciprocal rank, ×
/// `human_weight` exactly when the document is a correction. Asserted
/// per-position in the returned (rank-ordered) neighbour list, so the test
/// does not depend on predicting BM25's ordering — only on reading it.
#[tokio::test]
async fn the_audit_surface_shows_the_boosted_weight() {
    let human_weight = 3.0;
    let node = node_with_decisions("history", human_weight, 0.0).await;
    node.create_index("history").await;
    node.put_doc(
        "history",
        "plain",
        json!({"text": "refund money back guarantee", "label": "billing"}),
    )
    .await;
    node.put_doc(
        "history",
        "human1",
        json!({"text": "refund money", "label": "tech", "human": true}),
    )
    .await;
    node.refresh("history").await;

    let (st, r) = node
        .decide(json!({"index": "history", "question": "refund money back guarantee"}))
        .await;
    assert!(st.is_success(), "{st} {r}");
    let neighbours = r["neighbours"].as_array().expect("neighbours: {r}");
    for (rank, n) in neighbours.iter().enumerate() {
        let id = n["_id"].as_str().expect("id: {r}");
        let expected = if id == "human1" {
            human_weight / (rank as f64 + 1.0)
        } else {
            1.0 / (rank as f64 + 1.0)
        };
        let got = n["weight"].as_f64().expect("weight: {r}");
        assert!(
            (got - expected).abs() < 1e-9,
            "{id} at rank {rank}: weight {got}, expected {expected}: {r}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The write-back — needs tier 2, so `decide-local`
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "decide-local")]
mod write_back {
    use super::*;

    struct LocalNode {
        node: Node,
        _model_dir: tempfile::TempDir,
    }

    /// The same armed node as `systemone_local_decide.rs`: a tiny untrained
    /// fixture model, `[decisions] index` set to `index`.
    async fn local_node(index: &str, min_confidence: f64) -> LocalNode {
        local_node_escalating(index, min_confidence, 0.8).await
    }

    /// [`local_node`] with `[decisions] escalate_below` set (#1094).
    async fn local_node_escalating(
        index: &str,
        min_confidence: f64,
        escalate_below: f64,
    ) -> LocalNode {
        let dir = tempfile::tempdir().expect("tempdir");
        let model_dir = tempfile::tempdir().expect("model tempdir");
        xerj_ai::decide::testing::write_fixture(model_dir.path())
            .unwrap_or_else(|e| panic!("write decide fixture: {e:#}"));

        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();
        config.storage.wal_sync = xerj_common::config::WalSync::Async;
        config.decisions.index = index.to_string();
        config.decisions.min_confidence = min_confidence;
        config.decisions.escalate_below = escalate_below;
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        let mut state = AppState::new(config, engine, metrics);
        // The injection seam, not the environment.
        state.decide = std::sync::Arc::new(xerj_api::systemone_api::DecideSettings::local(
            model_dir.path().to_path_buf(),
        ));
        LocalNode {
            node: Node {
                native: xerj_api::router::build_native_router(state.clone()),
                app: xerj_api::router::build_es_compat_router(state),
                _dir: dir,
            },
            _model_dir: model_dir,
        }
    }

    /// THE flywheel test. One request, two questions: history answers d0
    /// (seeded refund neighbours), the local head answers d1 (payload unlike
    /// anything seeded). Exactly ONE document is written back — d1's, with
    /// the text that was decided, the label and probability served, the
    /// source that produced them, and a timestamp — and d0's history answer
    /// is NOT written again. Then the same request again: d1 is now answered
    /// by HISTORY, from the cached answer, which is the entire point.
    #[tokio::test]
    async fn local_answers_are_cached_once_and_then_answer_the_next_request() {
        let ln = local_node("history", 0.0).await;
        let node = &ln.node;
        node.create_index("history").await;
        node.put_doc(
            "history",
            "h1",
            json!({"text": "refund refund refund asked for money back", "label": "true"}),
        )
        .await;
        node.put_doc(
            "history",
            "h2",
            json!({"text": "refund policy says money back", "label": "true"}),
        )
        .await;
        node.refresh("history").await;
        assert_eq!(node.docs("history").await.len(), 2, "the seeded history");

        let request = json!({
            "state": {"documents": {
                "doc_0": "refund asked for money back",
                "doc_1": "zzzqqq nothing like the history"
            }},
            "questions": {
                "d0": {"type": "noul", "instructions": "Is `documents.doc_0` about refunds?"},
                "d1": {"type": "noul", "instructions": "Is `documents.doc_1` about refunds?"}
            }
        });
        let (st, r) = node.systemone(request.clone()).await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(r["decisions"]["evidence"]["d0"]["tier"], "history", "{r}");
        assert_eq!(r["decisions"]["evidence"]["d1"]["tier"], "local", "{r}");
        let served_p = r["decisions"]["evidence"]["d1"]["support"]
            .as_f64()
            .expect("p: {r}");

        // The write-back lands: exactly one new document.
        let docs = node.wait_for_docs("history", 3).await;
        assert_eq!(
            docs.len(),
            3,
            "history answers are not double-written: {docs:?}"
        );
        let cached: Vec<&Value> = docs.iter().filter(|d| d.get("source").is_some()).collect();
        assert_eq!(cached.len(), 1, "exactly one cached answer: {docs:?}");
        let cached = cached[0];
        assert_eq!(
            cached["text"], "zzzqqq nothing like the history",
            "{cached}"
        );
        assert!(
            cached["label"].is_string(),
            "the label the head served: {cached}"
        );
        assert_eq!(cached["source"], "local", "{cached}");
        assert!(
            (cached["p"].as_f64().unwrap_or(-1.0) - served_p).abs() < 1e-9,
            "p is the probability that was served ({served_p}): {cached}"
        );
        let ts = cached["ts"].as_str().expect("ts: {cached}");
        assert!(
            ts.ends_with('Z') && ts.len() == 24,
            "an RFC 3339 millisecond timestamp: {cached}"
        );
        // The flywheel closes: the SAME request again has d1 answered by
        // history — the cached answer is the labelled neighbour now.
        let (st, r2) = node.systemone(request).await;
        assert!(st.is_success(), "{st} {r2}");
        assert_eq!(
            r2["decisions"]["evidence"]["d1"]["tier"], "history",
            "the cached answer answered it: {r2}"
        );
        assert_eq!(
            r2["decisions"]["evidence"]["d1"]["source"], "history",
            "{r2}"
        );
        // And nothing was written back for it: still three documents.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            node.docs("history").await.len(),
            3,
            "a history-tier answer is never cached again"
        );
    }

    /// `/_decide` caches its local answers too — and an abstain is NOT an
    /// answer, so it is never cached. Two nodes, the same fixture and
    /// question, differing only in `min_confidence`: one answers and writes,
    /// the other abstains and writes nothing (not even the index).
    #[tokio::test]
    async fn decide_caches_answered_local_answers_but_never_an_abstain() {
        // Answers: min_confidence 0 → the answer stands, the write-back
        // creates the configured-but-never-created index itself.
        let answered = local_node("history", 0.0).await;
        let (st, r) = answered
            .node
            .decide(json!({"index": "history", "question": "urgent prize claim call now"}))
            .await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(r["tier"], "local", "{r}");
        assert_eq!(r["source"], "local", "{r}");
        let docs = answered.node.wait_for_docs("history", 1).await;
        assert_eq!(docs.len(), 1, "{docs:?}");
        assert_eq!(docs[0]["text"], "urgent prize claim call now", "{docs:?}");
        assert_eq!(docs[0]["source"], "local", "{docs:?}");
        let confidence = r["confidence"].as_f64().expect("confidence: {r}");
        assert!(
            (docs[0]["p"].as_f64().unwrap_or(-1.0) - confidence).abs() < 1e-9,
            "p is the served confidence ({confidence}): {docs:?}"
        );

        // Abstains: min_confidence 2.0 is above any probability, so the same
        // question abstains — and nothing is written, not even the index.
        let abstained = local_node("history", 2.0).await;
        let (st, r) = abstained
            .node
            .decide(json!({"index": "history", "question": "urgent prize claim call now"}))
            .await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(r["tier"], "local", "{r}");
        assert!(r["abstain"].as_bool().unwrap_or(false), "{r}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let (st, r) = abstained
            .node
            .call(
                &abstained.node.app.clone(),
                "GET",
                "/history/_count",
                json!({}),
            )
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "no index was created: {st} {r}");
    }

    /// Issue #1094: once the history index has BM25 support for common stop
    /// words, split votes whose winning share is below `escalate_below` (default
    /// 0.8) MUST escalate to `local` on both `/_decide` and `/v1/systemone` and
    /// write the local judgement back into `[decisions] index` — unfreezing the
    /// flywheel instead of locking the cache at 12 documents.
    #[tokio::test]
    async fn split_history_below_escalate_below_escalates_to_local_and_writes_back() {
        let ln = local_node("history", 0.0).await;
        let node = &ln.node;
        node.create_index("history").await;
        // Two seeded neighbours sharing the token `refund` with opposite labels:
        // rank 0 gets weight 1.0, rank 1 gets weight 0.5 → winning share 2/3 ≈ 0.6667 < 0.8.
        node.put_doc(
            "history",
            "h_true",
            json!({"text": "refund money back request", "label": "true"}),
        )
        .await;
        node.put_doc(
            "history",
            "h_false",
            json!({"text": "refund inquiry status", "label": "not true"}),
        )
        .await;
        node.refresh("history").await;
        assert_eq!(node.docs("history").await.len(), 2);

        // 1. `/_decide` escalates the split 2/3 vote (< 0.8) to `local` and caches the judgement.
        let (st, r) = node
            .decide(json!({"index": "history", "question": "refund money back check"}))
            .await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(
            r["tier"], "local",
            "split history vote (2/3 < 0.8) must escalate to local: {r}"
        );
        assert_eq!(r["source"], "local", "{r}");
        let docs = node.wait_for_docs("history", 3).await;
        assert_eq!(
            docs.len(),
            3,
            "local escalation wrote back to history: {docs:?}"
        );

        // 2. `/v1/systemone` on another split index also escalates and writes back.
        let ln2 = local_node("history_s1", 0.0).await;
        let node2 = &ln2.node;
        node2.create_index("history_s1").await;
        node2
            .put_doc(
                "history_s1",
                "s_true",
                json!({"text": "invoice payment dispute", "label": "true"}),
            )
            .await;
        node2
            .put_doc(
                "history_s1",
                "s_false",
                json!({"text": "invoice summary receipt", "label": "not true"}),
            )
            .await;
        node2.refresh("history_s1").await;

        let (st, r2) = node2
            .systemone(json!({
                "state": {"documents": {"doc_0": "invoice payment question"}},
                "questions": {"d0": {"type": "noul", "instructions": "Is `documents.doc_0` a dispute?"}}
            }))
            .await;
        assert!(st.is_success(), "{st} {r2}");
        assert_eq!(
            r2["decisions"]["evidence"]["d0"]["tier"], "local",
            "split history vote on /v1/systemone escalates to local: {r2}"
        );
        assert_eq!(r2["decisions"]["evidence"]["d0"]["source"], "local", "{r2}");
        let docs2 = node2.wait_for_docs("history_s1", 3).await;
        assert_eq!(
            docs2.len(),
            3,
            "/v1/systemone escalation wrote back to history_s1: {docs2:?}"
        );
    }

    /// #1094 follow-up: a question that keeps escalating (its own cached
    /// answer is one neighbour among split ones) must not append a fresh
    /// cached document every time — repetition alone would then let the
    /// local head out-vote the history. Re-deciding the same text overwrites
    /// its one cached answer. `escalate_below = 1.0` makes every split vote
    /// escalate, so the second request is guaranteed to reach the local head.
    #[tokio::test]
    async fn re_escalating_the_same_question_keeps_one_cached_answer() {
        let ln = local_node_escalating("history", 0.0, 1.0).await;
        let node = &ln.node;
        node.create_index("history").await;
        node.put_doc(
            "history",
            "h_true",
            json!({"text": "refund money back request", "label": "true"}),
        )
        .await;
        node.put_doc(
            "history",
            "h_false",
            json!({"text": "refund inquiry status", "label": "not true"}),
        )
        .await;
        node.refresh("history").await;

        let question = json!({"index": "history", "question": "refund money back check"});
        let (st, r) = node.decide(question.clone()).await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(r["tier"], "local", "{r}");
        let docs = node.wait_for_docs("history", 3).await;
        let cached_ts = |docs: &[Value]| -> Vec<String> {
            docs.iter()
                .filter(|d| d["source"] == "local")
                .map(|d| d["ts"].as_str().unwrap_or_default().to_string())
                .collect()
        };
        let first = cached_ts(&docs);
        assert_eq!(first.len(), 1, "one cached answer: {docs:?}");

        // Same question again: still a split vote, so it escalates again.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let (st, r) = node.decide(question).await;
        assert!(st.is_success(), "{st} {r}");
        assert_eq!(r["tier"], "local", "the re-asked split vote escalates: {r}");
        // Wait for the second write-back to land (its ts differs), then the
        // index must still hold exactly one cached answer for that text.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let docs = node.docs("history").await;
            let ts = cached_ts(&docs);
            assert!(
                ts.len() <= 1,
                "a repeat escalation appended a duplicate: {docs:?}"
            );
            if ts.len() == 1 && ts[0] != first[0] {
                assert_eq!(docs.len(), 3, "{docs:?}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the second write-back never landed: {docs:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}
