//! `_bulk` must apply items that share an `(index, _id)` in request order.
//!
//! The bulk executor partitions items into phases — plain `index` items run
//! as one turbo group, `delete` / `update` / `create` / CAS / routed items run
//! afterwards — which reorders items across groups. When one `_id` was touched
//! by both kinds in one request, its plain `index` items jumped ahead of an
//! earlier `delete` or `update`, so the final document did not match the
//! request: an `index`, `delete`, `index` sequence ended with the document
//! deleted, while every item reported success.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::bulk::process_bulk;
use xerj_engine::Engine;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn seed(engine: &Engine, name: &str) {
    let mut schema = Schema::empty();
    schema.fields.push(FieldConfig::new("v", FieldType::Long));
    engine.create_index(name, schema).expect("create index");
}

fn ndjson(lines: &[Value]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&l.to_string());
        out.push('\n');
    }
    out
}

async fn source(engine: &Engine, index: &str, id: &str) -> Option<Value> {
    engine
        .get_index(index)
        .expect("get index")
        .get_document(id)
        .await
        .expect("lookup")
}

/// FAIL-BEFORE: both `index` items ran in the turbo phase, then the `delete`
/// ran last and removed the document that the request's final item wrote.
#[tokio::test]
async fn index_delete_index_keeps_the_last_write() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "t");

    let body = ndjson(&[
        json!({"index": {"_index": "t", "_id": "1"}}),
        json!({"v": 1}),
        json!({"delete": {"_index": "t", "_id": "1"}}),
        json!({"index": {"_index": "t", "_id": "1"}}),
        json!({"v": 2}),
    ]);
    let result = process_bulk(&engine, None, &body).await;
    assert!(!result.errors, "bulk must not error: {:?}", result.items);
    let statuses: Vec<u16> = result.items.iter().map(|i| i.status).collect();
    assert_eq!(statuses, vec![201, 200, 201], "items: {:?}", result.items);

    let doc = source(&engine, "t", "1")
        .await
        .expect("the final `index` item must leave the document present");
    assert_eq!(doc.get("v"), Some(&json!(2)));
}

/// FAIL-BEFORE: the `index` ran first and the earlier `delete` removed it.
#[tokio::test]
async fn delete_then_index_leaves_the_document() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "t");
    process_bulk(
        &engine,
        None,
        &ndjson(&[
            json!({"index": {"_index": "t", "_id": "1"}}),
            json!({"v": 1}),
        ]),
    )
    .await;

    let body = ndjson(&[
        json!({"delete": {"_index": "t", "_id": "1"}}),
        json!({"index": {"_index": "t", "_id": "1"}}),
        json!({"v": 7}),
    ]);
    let result = process_bulk(&engine, None, &body).await;
    assert!(!result.errors, "bulk must not error: {:?}", result.items);
    assert_eq!(result.items[1].status, 201, "re-index after delete creates");

    let doc = source(&engine, "t", "1")
        .await
        .expect("document indexed after the delete must exist");
    assert_eq!(doc.get("v"), Some(&json!(7)));
}

/// FAIL-BEFORE: the `index` ran first, so `create` saw an existing document
/// and failed with 409 although it came first in the request.
#[tokio::test]
async fn create_then_index_runs_create_first() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "t");

    let body = ndjson(&[
        json!({"create": {"_index": "t", "_id": "1"}}),
        json!({"v": 1}),
        json!({"index": {"_index": "t", "_id": "1"}}),
        json!({"v": 2}),
    ]);
    let result = process_bulk(&engine, None, &body).await;
    assert!(!result.errors, "bulk must not error: {:?}", result.items);
    let statuses: Vec<u16> = result.items.iter().map(|i| i.status).collect();
    assert_eq!(statuses, vec![201, 200], "items: {:?}", result.items);
    assert_eq!(result.items[1].result.as_deref(), Some("updated"));

    let doc = source(&engine, "t", "1").await.expect("document");
    assert_eq!(doc.get("v"), Some(&json!(2)));
}

/// FAIL-BEFORE: the `index` ran first and the earlier upsert merged its
/// field into the replacement document, which a full `index` must not keep.
#[tokio::test]
async fn upsert_then_index_replaces_the_document() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "t");

    let body = ndjson(&[
        json!({"update": {"_index": "t", "_id": "1"}}),
        json!({"doc": {"u": 1}, "doc_as_upsert": true}),
        json!({"index": {"_index": "t", "_id": "1"}}),
        json!({"v": 3}),
    ]);
    let result = process_bulk(&engine, None, &body).await;
    assert!(!result.errors, "bulk must not error: {:?}", result.items);

    let doc = source(&engine, "t", "1").await.expect("document");
    assert_eq!(doc.get("v"), Some(&json!(3)));
    assert_eq!(
        doc.get("u"),
        None,
        "`index` replaces the whole source: {doc}"
    );
}

/// Plain `index` items on distinct ids keep using the turbo group and stay
/// unaffected by a `delete` of an unrelated id in the same request.
#[tokio::test]
async fn unrelated_ids_are_unaffected() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine, "t");

    let body = ndjson(&[
        json!({"index": {"_index": "t", "_id": "a"}}),
        json!({"v": 1}),
        json!({"delete": {"_index": "t", "_id": "zzz"}}),
        json!({"index": {"_index": "t", "_id": "b"}}),
        json!({"v": 2}),
    ]);
    let result = process_bulk(&engine, None, &body).await;
    assert_eq!(result.items[0].status, 201);
    assert_eq!(result.items[2].status, 201);
    assert!(source(&engine, "t", "a").await.is_some());
    assert!(source(&engine, "t", "b").await.is_some());
}
