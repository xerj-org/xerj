//! Issue #1947: a `semantic` query on a `semantic_text` field returned hits
//! and never a `highlight`. The semantic highlighter (the default for that
//! field, and `"type": "semantic"`) returns the field's best-scoring chunks
//! for the query embedding. A lexical `match` on the same field still wraps
//! query terms.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
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
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn short_sentence() -> &'static str {
    "The walrus eats clams on the ice floe."
}

/// Two ingest chunks: the first is only the walrus sentence, the second
/// overlaps it and then talks about railways. ASCII, so char offsets and
/// byte offsets match the semantic chunker (512 chars, 64 overlap, break on
/// the sentence ending at char 507).
fn two_chunk_text() -> String {
    let walrus = "The walrus eats clams on the ice floe. ".repeat(13);
    let railway = "The railway timetable lists every locomotive. ".repeat(8);
    format!("{walrus}{railway}")
}

async fn highlight_of(app: &axum::Router, body: Value) -> Value {
    let (status, response) = send(app, "POST", "/hl/_search", body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["hits"]["total"]["value"], 1, "{response}");
    response["hits"]["hits"][0]["highlight"].clone()
}

#[tokio::test]
async fn semantic_query_returns_chunk_highlights_and_lexical_match_still_tags() {
    let (app, _dir) = app().await;
    let (status, response) = send(
        &app,
        "PUT",
        "/hl",
        json!({"mappings": {"properties": {
            "body": {"type": "text"},
            "sem": {"type": "semantic_text"}
        }}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {response}");
    let (status, response) = send(
        &app,
        "PUT",
        "/hl/_doc/1?refresh=true",
        json!({
            "body": "Pricing is per-node for Professional.",
            "sem": short_sentence(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "index: {response}");

    let sentence = json!([short_sentence()]);

    let default_form = highlight_of(
        &app,
        json!({
            "query": {"semantic": {"field": "sem", "query": "walrus clams"}},
            "highlight": {"fields": {"sem": {}}}
        }),
    )
    .await;
    assert_eq!(
        default_form,
        json!({"sem": sentence}),
        "default highlighter"
    );
    assert!(
        !default_form["sem"][0].as_str().unwrap().contains("<em>"),
        "semantic chunks are not term-tagged: {default_form}"
    );

    // A paraphrase shares no query terms with the field. Term tagging would
    // return nothing; the chunk the semantic score came from is still the hit.
    let paraphrase = highlight_of(
        &app,
        json!({
            "query": {"semantic": {"field": "sem", "query": "marine mammal molluscs"}},
            "highlight": {"fields": {"sem": {}}}
        }),
    )
    .await;
    assert_eq!(paraphrase, json!({"sem": sentence}), "paraphrase");

    let explicit = highlight_of(
        &app,
        json!({
            "query": {"semantic": {"field": "sem", "query": "walrus clams"}},
            "highlight": {"fields": {"sem": {"type": "semantic", "number_of_fragments": 1}}}
        }),
    )
    .await;
    assert_eq!(explicit, json!({"sem": sentence}), "type semantic");

    let whole_field = highlight_of(
        &app,
        json!({
            "query": {"semantic": {"field": "sem", "query": "walrus clams"}},
            "highlight": {"fields": {"sem": {"number_of_fragments": 0}}}
        }),
    )
    .await;
    assert_eq!(
        whole_field,
        json!({"sem": sentence}),
        "number_of_fragments 0"
    );

    let hidden_source = highlight_of(
        &app,
        json!({
            "_source": false,
            "query": {"semantic": {"field": "sem", "query": "walrus clams"}},
            "highlight": {"fields": {"sem": {}}}
        }),
    )
    .await;
    assert_eq!(hidden_source, json!({"sem": sentence}), "_source false");

    let lexical = highlight_of(
        &app,
        json!({
            "query": {"match": {"sem": "walrus"}},
            "highlight": {"fields": {"sem": {}}}
        }),
    )
    .await;
    assert_eq!(
        lexical,
        json!({"sem": ["The <em>walrus</em> eats clams on the ice floe."]}),
        "lexical match still tags terms"
    );
}

#[tokio::test]
async fn semantic_highlight_honours_fragment_count_and_order() {
    let (app, _dir) = app().await;
    let (status, response) = send(
        &app,
        "PUT",
        "/hl",
        json!({"mappings": {"properties": {"sem": {"type": "semantic_text"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {response}");
    let (status, response) = send(
        &app,
        "PUT",
        "/hl/_doc/2?refresh=true",
        json!({"sem": two_chunk_text()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "index: {response}");

    let railway = json!({
        "semantic": {
            "field": "sem",
            "query": "railway timetable locomotive",
            "filter": {"ids": {"values": ["2"]}}
        }
    });

    let one = highlight_of(
        &app,
        json!({
            "query": railway,
            "highlight": {"fields": {"sem": {"type": "semantic", "number_of_fragments": 1}}}
        }),
    )
    .await;
    let one = one["sem"].as_array().expect("fragments");
    assert_eq!(one.len(), 1, "{one:?}");
    assert!(
        one[0].as_str().unwrap().contains("locomotive"),
        "best chunk is the railway passage: {one:?}"
    );

    let by_score = highlight_of(
        &app,
        json!({
            "query": railway,
            "highlight": {"fields": {"sem": {"number_of_fragments": 2, "order": "score"}}}
        }),
    )
    .await;
    let by_score = by_score["sem"].as_array().expect("fragments");
    assert_eq!(by_score.len(), 2, "{by_score:?}");
    assert!(
        by_score[0].as_str().unwrap().contains("locomotive"),
        "{by_score:?}"
    );
    assert!(
        !by_score[1].as_str().unwrap().contains("locomotive"),
        "the lower-scoring chunk is the walrus passage: {by_score:?}"
    );

    let in_order = highlight_of(
        &app,
        json!({
            "query": railway,
            "highlight": {"fields": {"sem": {"number_of_fragments": 2, "order": "none"}}}
        }),
    )
    .await;
    let in_order = in_order["sem"].as_array().expect("fragments");
    assert_eq!(in_order.len(), 2, "{in_order:?}");
    assert!(
        !in_order[0].as_str().unwrap().contains("locomotive"),
        "document order leads with the earlier chunk: {in_order:?}"
    );
    assert!(
        in_order[1].as_str().unwrap().contains("locomotive"),
        "{in_order:?}"
    );

    let walrus = highlight_of(
        &app,
        json!({
            "query": {
                "semantic": {
                    "field": "sem",
                    "query": "walrus clams ice floe",
                    "filter": {"ids": {"values": ["2"]}}
                }
            },
            "highlight": {"fields": {"sem": {"number_of_fragments": 1}}}
        }),
    )
    .await;
    let walrus = walrus["sem"][0].as_str().unwrap();
    assert!(walrus.contains("walrus"), "{walrus}");
    assert!(
        !walrus.contains("locomotive"),
        "walrus query must not surface the railway chunk: {walrus}"
    );
}
