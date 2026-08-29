//! #860 / #846: semantically different `minimum_should_match` specs on an
//! otherwise identical query must NOT collide in the query-result cache.
//!
//! The cache key hashes the serialized request. `MinShouldMatch` is an untagged
//! enum, so a derived `Serialize` emitted `Negative(2)` and `Percentage(2)` as
//! the bare number `2` — identical to `Fixed(2)` — so a `msm:-2` query and a
//! `msm:2` query hashed the same and the second returned the first's cached
//! count. A hand-written `Serialize` gives every spec a distinct wire form.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn hits(engine: &Engine, q: &serde_json::Value) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 0 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

#[tokio::test]
async fn msm_specs_do_not_collide_in_the_query_cache() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    for f in ["fa", "fb", "fc"] {
        schema.fields.push(FieldConfig::new(f, FieldType::Keyword));
    }
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    for (id, doc) in [
        ("d0", json!({ "fa": "y", "fb": "y", "fc": "y" })), // 3 should match
        ("d1", json!({ "fa": "y", "fb": "y" })),            // 2
        ("d2", json!({ "fa": "y" })),                       // 1
    ] {
        idx.index_document(Some(id.to_string()), doc)
            .await
            .expect("index");
    }

    let boolq = |msm: serde_json::Value| {
        json!({ "bool": {
            "should": [
                {"term": {"fa": "y"}}, {"term": {"fb": "y"}}, {"term": {"fc": "y"}}
            ],
            "minimum_should_match": msm
        }})
    };

    // Run the LOW-required spec first so it would poison the cache for the
    // high-required spec if their keys collided.
    // Negative(2): required 3-2 = 1 → d0,d1,d2 = 3.
    assert_eq!(
        hits(&engine, &boolq(json!(-2))).await,
        3,
        "msm:-2 → required 1"
    );
    // Fixed(2): required 2 → d0,d1 = 2. MUST NOT read the msm:-2 entry.
    assert_eq!(
        hits(&engine, &boolq(json!(2))).await,
        2,
        "#860: Fixed msm:2 must not collide with the earlier Negative(2) cache entry"
    );

    // Percentage(2) required floor(3*.02).max(1) = 1 → 3 hits; must also not
    // collide with Fixed(2) (the pre-existing bare-number collision).
    assert_eq!(
        hits(&engine, &boolq(json!("2%"))).await,
        3,
        "msm:2% → required 1"
    );
    assert_eq!(
        hits(&engine, &boolq(json!(2))).await,
        2,
        "#860: Fixed msm:2 must not collide with Percentage(2) either"
    );
}
