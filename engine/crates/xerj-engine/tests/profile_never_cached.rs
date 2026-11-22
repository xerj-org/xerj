//! `profile: true` requests are never stored in or served from the
//! whole-response query cache (#3315 revert): a profile must time a fresh
//! execution. #3020's marker stays, and on a profile response it is always
//! `request_cache: "bypass"`.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

fn request_cache(profile: &Option<Value>) -> Option<String> {
    profile
        .as_ref()
        .and_then(|p| p.get("request_cache"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[tokio::test]
async fn profile_requests_bypass_the_whole_response_cache() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine::new");
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("prof", schema).unwrap();
    let idx = engine.get_index("prof").unwrap();
    for (id, body) in [("1", "the quick brown fox"), ("2", "quick start")] {
        idx.index_document(Some(id.into()), json!({ "body": body }))
            .await
            .unwrap();
    }
    idx.refresh().await.unwrap();

    let profiled = parse_request(&json!({
        "query": {"match": {"body": "quick"}},
        "profile": true
    }))
    .unwrap();
    assert!(profiled.profile);

    let hits_before = idx.query_cache_hit_count();
    for run in 1..=3 {
        let r = idx.search(&profiled).await.unwrap();
        assert_eq!(r.total.value, 2, "profile run {run}: hit count");
        assert_eq!(
            request_cache(&r.profile).as_deref(),
            Some("bypass"),
            "profile run {run} must report a fresh execution"
        );
    }
    assert_eq!(
        idx.query_cache_hit_count(),
        hits_before,
        "a repeated profile request was answered from the query cache"
    );

    // The same body without `profile` still uses the cache, and never grows
    // a profile key.
    let plain = parse_request(&json!({"query": {"match": {"body": "quick"}}})).unwrap();
    let first = idx.search(&plain).await.unwrap();
    let second = idx.search(&plain).await.unwrap();
    assert_eq!(first.total.value, 2);
    assert_eq!(second.total.value, 2);
    assert!(request_cache(&second.profile).is_none());
    let cache_disabled = matches!(
        std::env::var("XERJ_DISABLE_QUERY_CACHE").as_deref(),
        Ok("1") | Ok("true")
    );
    if !cache_disabled {
        assert_eq!(
            idx.query_cache_hit_count(),
            hits_before + 1,
            "the plain repeat should be a cache hit"
        );
    }

    // A profile request after the plain one is cached still executes fresh.
    let r = idx.search(&profiled).await.unwrap();
    assert_eq!(request_cache(&r.profile).as_deref(), Some("bypass"));
}
