//! `explain: true` on the native search API (`POST /v1/indices/{name}/search`)
//! adds a per-field `_score_breakdown` to every hit (issue #1798).
//!
//! The field numbers are checked against independent runs: a field's score
//! must equal the score the engine gives that same hit for the single-field
//! clause on its own, and for an additive query the fields must add up to
//! `_score`. Each check runs on the memtable and again after `_flush`, since
//! the two paths score through different code. The ES `explain` on `_search`
//! must not change.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Node {
    es: axum::Router,
    native: axum::Router,
    _dir: tempfile::TempDir,
}

async fn node() -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    Node {
        native: xerj_api::router::build_native_router(state.clone()),
        es: xerj_api::router::build_es_compat_router(state),
        _dir: dir,
    }
}

async fn send(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
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

impl Node {
    async fn es(&self, method: &str, path: &str, body: Value) -> Value {
        let (st, b) = send(&self.es, method, path, body).await;
        assert!(st.is_success(), "{method} {path}: {st} {b}");
        b
    }

    async fn native_search(&self, body: Value) -> Vec<Value> {
        let (st, b) = send(&self.native, "POST", "/v1/indices/kb/search", body.clone()).await;
        assert_eq!(st, StatusCode::OK, "{body} -> {b}");
        b["data"]["hits"].as_array().cloned().expect("hits")
    }

    async fn seed(&self) {
        self.es(
            "PUT",
            "/kb",
            json!({"mappings": {"properties": {
                "title": {"type": "text"},
                "body": {"type": "text"},
                "cat": {"type": "keyword"}
            }}}),
        )
        .await;
        let docs = [
            (
                "1",
                "Bone health basics",
                "vitamin supplements are popular. vitamin vitamin.",
                "health",
            ),
            (
                "2",
                "Trial results",
                "vitamin d supplementation improved bone density",
                "trial",
            ),
            (
                "3",
                "Cooking with vitamin greens",
                "rich vegetables for dinner",
                "food",
            ),
            (
                "4",
                "Density of materials",
                "bone china has high density",
                "materials",
            ),
            (
                "5",
                "Vitamin bone density review",
                "a review of bone density and vitamin intake",
                "trial",
            ),
        ];
        for (id, title, body, cat) in docs {
            self.es(
                "PUT",
                &format!("/kb/_doc/{id}"),
                json!({"title": title, "body": body, "cat": cat}),
            )
            .await;
        }
        self.es("POST", "/kb/_refresh", json!({})).await;
    }
}

fn score(hit: &Value) -> f64 {
    hit["_score"].as_f64().expect("_score")
}

fn field_score(hit: &Value, field: &str) -> f64 {
    hit["_score_breakdown"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .find(|f| f["field"] == field)
        .and_then(|f| f["score"].as_f64())
        .unwrap_or(0.0)
}

fn field_sum(hit: &Value) -> f64 {
    hit["_score_breakdown"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|f| f["score"].as_f64().unwrap())
        .sum()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-4 * a.abs().max(b.abs()).max(1.0)
}

/// `_score` of every hit of a native search, by id.
async fn scores_by_id(node: &Node, query: Value) -> std::collections::HashMap<String, f64> {
    node.native_search(json!({"query": query, "size": 50}))
        .await
        .iter()
        .map(|h| (h["_id"].as_str().unwrap().to_string(), score(h)))
        .collect()
}

async fn check_all(node: &Node, phase: &str) {
    // 1. A bool over two fields plus a keyword filter, combined by sum. The
    // filter scores nothing and is not a field in the breakdown.
    let title_clause = json!({"match": {"title": {"query": "bone density", "boost": 2.0}}});
    let body_clause = json!({"match": {"body": "vitamin"}});
    let q = json!({"bool": {
        "should": [title_clause, body_clause],
        "filter": [{"term": {"cat": "trial"}}]
    }});
    let hits = node
        .native_search(json!({"query": q, "explain": true}))
        .await;
    assert_eq!(hits.len(), 2, "{phase}: {hits:?}");
    let title_only = scores_by_id(node, title_clause.clone()).await;
    let body_only = scores_by_id(node, body_clause.clone()).await;
    for hit in &hits {
        let id = hit["_id"].as_str().unwrap();
        let b = &hit["_score_breakdown"];
        assert_eq!(b["combine"], "sum", "{phase}: {hit}");
        assert!(
            close(field_sum(hit), score(hit)),
            "{phase}: fields must add up to _score: {hit}"
        );
        assert!(
            b["unattributed"].as_f64().unwrap().abs() < 1e-3,
            "{phase}: {hit}"
        );
        assert!(
            close(field_score(hit, "title"), *title_only.get(id).unwrap_or(&0.0)),
            "{phase}: boosted title share of {id} must equal the title clause alone: {hit} vs {title_only:?}"
        );
        assert!(
            close(field_score(hit, "body"), *body_only.get(id).unwrap_or(&0.0)),
            "{phase}: body share of {id} must equal the body clause alone: {hit} vs {body_only:?}"
        );
        let names: Vec<&str> = b["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["field"].as_str().unwrap())
            .collect();
        assert!(
            !names.contains(&"cat"),
            "{phase}: a filter is not a scoring field: {hit}"
        );
    }
    // Doc 5 matches in both fields, doc 2 has "bone density" in its body only.
    let doc5 = hits.iter().find(|h| h["_id"] == "5").expect("doc 5");
    assert!(
        field_score(doc5, "title") > 0.0 && field_score(doc5, "body") > 0.0,
        "{doc5}"
    );
    let doc2 = hits.iter().find(|h| h["_id"] == "2").expect("doc 2");
    assert_eq!(field_score(doc2, "title"), 0.0, "{doc2}");

    // 2. most_fields multi_match sums its fields; the `^3` boost stays on
    // the field it was written on.
    let hits = node
        .native_search(json!({
            "query": {"multi_match": {
                "query": "vitamin bone",
                "fields": ["title^3", "body"],
                "type": "most_fields"
            }},
            "explain": true
        }))
        .await;
    assert!(hits.len() >= 4, "{phase}: {hits:?}");
    let title_only = scores_by_id(
        node,
        json!({"multi_match": {"query": "vitamin bone", "fields": ["title^3"], "type": "most_fields"}}),
    )
    .await;
    for hit in &hits {
        let id = hit["_id"].as_str().unwrap();
        assert_eq!(hit["_score_breakdown"]["combine"], "sum", "{phase}: {hit}");
        assert!(close(field_sum(hit), score(hit)), "{phase}: {hit}");
        assert!(
            close(
                field_score(hit, "title"),
                *title_only.get(id).unwrap_or(&0.0)
            ),
            "{phase}: {hit} vs {title_only:?}"
        );
    }

    // 3. best_fields multi_match takes the max, so it is reported as `other`
    // and `_score` is the largest field score.
    let hits = node
        .native_search(json!({
            "query": {"multi_match": {"query": "bone density", "fields": ["title", "body"]}},
            "explain": true
        }))
        .await;
    assert!(!hits.is_empty(), "{phase}");
    for hit in &hits {
        let b = &hit["_score_breakdown"];
        assert_eq!(b["combine"], "other", "{phase}: {hit}");
        assert!(b.get("unattributed").is_none(), "{phase}: {hit}");
        let max = field_score(hit, "title").max(field_score(hit, "body"));
        assert!(close(max, score(hit)), "{phase}: best_fields = max: {hit}");
    }

    // 4. A field-less query_string is broken down over every text field. The
    // engine scores it as one clause, not as a sum of per-field BM25, so it
    // is reported as `other`.
    let hits = node
        .native_search(json!({
            "query": {"query_string": {"query": "vitamin density"}},
            "explain": true
        }))
        .await;
    assert!(!hits.is_empty(), "{phase}");
    let doc5 = hits.iter().find(|h| h["_id"] == "5").expect("doc 5");
    assert!(
        field_score(doc5, "title") > 0.0 && field_score(doc5, "body") > 0.0,
        "{doc5}"
    );
    assert_eq!(doc5["_score_breakdown"]["combine"], "other", "{doc5}");
    let title_only = scores_by_id(node, json!({"match": {"title": "vitamin density"}})).await;
    assert!(
        close(field_score(doc5, "title"), title_only["5"]),
        "{doc5} vs {title_only:?}"
    );

    // 5. Off by default.
    let hits = node
        .native_search(json!({"query": {"match": {"body": "vitamin"}}}))
        .await;
    assert!(!hits.is_empty(), "{phase}");
    assert!(
        hits.iter().all(|h| h.get("_score_breakdown").is_none()),
        "{phase}"
    );

    // 6. The ES-compatible `explain` is a separate path and is unchanged.
    let es = node
        .es(
            "POST",
            "/kb/_search",
            json!({"query": {"match": {"title": "bone"}}, "explain": true}),
        )
        .await;
    let es_hits = es["hits"]["hits"].as_array().unwrap();
    assert!(!es_hits.is_empty(), "{phase}");
    for hit in es_hits {
        assert!(hit.get("_explanation").is_some(), "{phase}: {hit}");
        assert!(hit.get("_score_breakdown").is_none(), "{phase}: {hit}");
    }
}

#[tokio::test]
async fn native_explain_breaks_the_score_down_per_field() {
    let node = node().await;
    node.seed().await;
    check_all(&node, "memtable").await;
    node.es("POST", "/kb/_flush", json!({})).await;
    check_all(&node, "flushed").await;
}
