//! Range bucket aggregations must look at every value of a multi-valued
//! field, not just the first one.
//!
//! Found while verifying #1750 (`range` query on arrays): the query side is
//! fixed separately (PR #1776), but the three range **aggregation** types had
//! the same blind spot on the aggregation path:
//!
//! * `range` read `extract_numeric(doc, field)` — the first numeric element —
//!   so `[7, 8080]` counted in `[.., 10)` and vanished from `[8000, 9000)`;
//! * `date_range` handed the whole array to the per-value date parser, which
//!   returned `None` for an array, so every array-valued date document was
//!   missed entirely;
//! * `ip_range` read `Value::as_str()`, which is `None` for an array, so every
//!   array-valued address was missed.
//!
//! ES buckets a document into every range that ANY of its values falls into,
//! and counts it once per bucket (a document is one document). That is the
//! contract asserted here; the `[5, 7]` document pins the once-per-bucket
//! half.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn events_schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("ports", FieldType::Long));
    schema
        .fields
        .push(FieldConfig::new("seen", FieldType::Date));
    schema.fields.push(FieldConfig::new("ipf", FieldType::Ip));
    schema
}

async fn seed(engine: &Engine) -> std::sync::Arc<xerj_engine::Index> {
    engine.create_index("events", events_schema()).unwrap();
    let idx = engine.get_index("events").unwrap();
    for (id, doc) in [
        (
            "1",
            json!({
                "ports": [7, 8080],
                "seen": ["2019-01-01", "2022-06-01"],
                "ipf": ["192.168.0.1", "10.0.0.3"],
            }),
        ),
        (
            "2",
            json!({
                "ports": 443,
                "seen": "2024-01-01",
                "ipf": "172.16.0.1",
            }),
        ),
        (
            "3",
            json!({
                "ports": [5, 7],
                "seen": ["2021-06-01", "2030-01-01"],
                "ipf": ["10.0.0.9", "10.0.0.4"],
            }),
        ),
        // No value for any of the three fields: exercises `missing`.
        ("4", json!({"note": "no values"})),
    ] {
        idx.index_document(Some(id.into()), doc).await.unwrap();
    }
    idx.refresh().await.unwrap();
    idx
}

async fn aggs(idx: &xerj_engine::Index, aggs: Value) -> Value {
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": aggs,
    }))
    .expect("parse_request");
    idx.search(&req)
        .await
        .unwrap()
        .aggs
        .expect("aggs in the response")
}

fn counts(bucket: &Value) -> Vec<u64> {
    bucket["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("expected buckets, got {bucket}"))
        .iter()
        .map(|b| {
            b["doc_count"]
                .as_u64()
                .unwrap_or_else(|| panic!("no doc_count: {b}"))
        })
        .collect()
}

#[tokio::test]
async fn range_aggregations_bucket_every_value_of_a_multi_valued_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine).await;

    let body = aggs(
        &idx,
        json!({
            "ports": {
                "range": {
                    "field": "ports",
                    "ranges": [
                        { "from": 8000, "to": 9000 },
                        { "to": 10 },
                        { "from": 8000 }
                    ]
                }
            },
            "dates": {
                "date_range": {
                    "field": "seen",
                    "ranges": [{ "from": "2021-01-01", "to": "2023-01-01" }]
                }
            },
            "dates_via_range": {
                "range": {
                    "field": "seen",
                    "ranges": [{ "from": "2021-01-01", "to": "2023-01-01" }]
                }
            },
            "ips": {
                "ip_range": {
                    "field": "ipf",
                    "ranges": [{ "from": "10.0.0.1", "to": "10.0.0.5" }]
                }
            },
            "ips_mask": {
                "ip_range": {
                    "field": "ipf",
                    "ranges": [{ "mask": "10.0.0.0/24" }]
                }
            },
            "ports_missing": {
                "range": {
                    "field": "ports",
                    "missing": 6,
                    "ranges": [{ "to": 10 }]
                }
            }
        }),
    )
    .await;

    // d1's 8080 is its SECOND value; d3's 9000 is excluded by `to`.
    assert_eq!(
        counts(&body["ports"]),
        vec![1, 2, 1],
        "ports range must bucket every value: {body}"
    );
    // d1's 2022-06-01 and d3's 2021-06-01 are both second values.
    assert_eq!(counts(&body["dates"]), vec![2], "date_range: {body}");
    // Same data through `range` (string bounds select its date branch).
    assert_eq!(
        counts(&body["dates_via_range"]),
        vec![2],
        "range on dates: {body}"
    );
    // d1's 10.0.0.3 and d3's 10.0.0.4.
    assert_eq!(counts(&body["ips"]), vec![2], "ip_range: {body}");
    assert_eq!(counts(&body["ips_mask"]), vec![2], "ip_range mask: {body}");
    // `missing` applies only to d4 (no value at all), never to a doc whose
    // array merely has no element in range.
    assert_eq!(
        counts(&body["ports_missing"]),
        vec![3],
        "missing applies once, only to docs without a value: {body}"
    );
}

/// The same three aggregations must agree before and after a flush: the
/// aggregation path reads `_source` on both sides, so a value-drop here is a
/// data bug, not a timing one.
#[tokio::test]
async fn range_aggregations_are_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine).await;
    let aggs_def = json!({
        "ports": { "range": { "field": "ports", "ranges": [{ "from": 8000, "to": 9000 }] } },
        "dates": { "date_range": { "field": "seen", "ranges": [{ "from": "2021-01-01", "to": "2023-01-01" }] } },
        "ips": { "ip_range": { "field": "ipf", "ranges": [{ "from": "10.0.0.1", "to": "10.0.0.5" }] } }
    });
    let before = aggs(&idx, aggs_def.clone()).await;
    idx.flush().await.unwrap();
    let after = aggs(&idx, aggs_def).await;
    assert_eq!(before, after, "range aggs must not change at a flush");
}
