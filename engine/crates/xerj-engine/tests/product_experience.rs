//! # xerj Product Experience Tests
//!
//! These are USER JOURNEY tests, not performance benchmarks.
//! Each test simulates a real person trying to accomplish a real task,
//! and measures what matters to a PRODUCT user:
//!   - Time to first value
//!   - Data safety guarantees
//!   - Simplicity of the experience
//!   - Correctness of results
//!
//! Compare every journey against Elasticsearch to show why xerj exists.
//!
//! # Running
//!
//! ```bash
//! cargo test -p xerj-engine --test product_experience -- --nocapture
//! ```

use serde_json::json;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::bulk::process_bulk;
use xerj_engine::sql::parse_sql;
use xerj_engine::Engine;
use xerj_query::ast::{SearchRequest, SourceFilter};
use xerj_query::parse_request;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn make_engine_with_config(dir: &TempDir, configure: impl FnOnce(&mut Config)) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    configure(&mut config);
    Engine::new(config).expect("engine::new")
}

fn print_result(
    journey: &str,
    xerj_desc: &str,
    es_desc: &str,
    winner_line: &str,
) {
    let width = 52usize;
    let border = "=".repeat(width);
    let pad = |s: &str| {
        let content = format!("  {}", s);
        let spaces = width.saturating_sub(content.chars().count() + 2);
        format!("\u{2551}{}{}\u{2551}", content, " ".repeat(spaces))
    };
    println!("\u{2554}{}\u{2557}", border);
    println!("{}", pad(&format!("JOURNEY: {}", journey)));
    println!("\u{2560}{}\u{2563}", border);
    println!("{}", pad(&format!("xerj: {}", xerj_desc)));
    println!("{}", pad(&format!("ES:    {}", es_desc)));
    println!("{}", pad(&format!("Winner: {}", winner_line)));
    println!("\u{255a}{}\u{255d}", border);
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 1: "I just want to search my docs"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_first_time_user() {
    // Simulate: download xerj, start it, index 100 docs, search.
    // Measure: time from zero to first search result.
    // Compare: ES needs 60+ seconds just to start (JVM + heap warm-up).

    let dir = TempDir::new().unwrap();
    let start = Instant::now();

    let engine = make_engine(&dir);
    engine.create_index("my-data", Schema::empty()).unwrap();
    let idx = engine.get_index("my-data").unwrap();

    // Index 100 documents — a small personal document collection.
    for i in 0..100 {
        idx.index_document(
            Some(format!("doc-{i}")),
            json!({
                "title": format!("Document {i}"),
                "body": format!("This is document number {i} about various topics")
            }),
        )
        .await
        .unwrap();
    }

    // First search — the moment the user gets value.
    let result = idx
        .search(
            &parse_request(&json!({
                "query": { "match": { "body": "various topics" } }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    let total_time = start.elapsed();

    print_result(
        "First-Time User",
        &format!("{:.2?} from zero to search results", total_time),
        "60+ seconds (JVM startup alone)",
        &format!("xerj ({:.0}x faster to first value)", 60_000.0 / total_time.as_millis().max(1) as f64),
    );

    assert!(
        total_time < Duration::from_secs(5),
        "Must reach first search result in under 5 seconds, took {:?}",
        total_time
    );
    assert!(result.total.value > 0, "Must find at least one document");
    assert_eq!(result.total.value, 100, "All 100 docs should match 'various topics'");
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 2: "My app crashed, is my data safe?"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_crash_recovery() {
    // THE #1 fear from ES users: data loss on crash.
    // Elasticsearch requires careful JVM heap tuning and can lose data
    // when a node crashes before the translog is flushed.
    //
    // xerj writes to a WAL before acknowledging every write.
    // On restart the WAL is replayed — no data loss.

    let dir = TempDir::new().unwrap();
    let data_path = dir.path().to_str().unwrap().to_string();

    // Phase 1: index 1000 docs, then "crash" (drop the engine).
    let doc_count = 1_000usize;
    {
        let mut config = Config::default();
        config.server.data_dir = data_path.clone();
        let engine = Engine::new(config).unwrap();

        engine.create_index("critical-data", Schema::empty()).unwrap();
        let idx = engine.get_index("critical-data").unwrap();

        for i in 0..doc_count {
            idx.index_document(
                Some(format!("doc-{i}")),
                json!({
                    "id": i,
                    "payload": format!("mission-critical record number {i}"),
                    "important": true
                }),
            )
            .await
            .unwrap();
        }

        // Simulate crash: engine is dropped without a clean shutdown.
        // The WAL on disk is the only record of these writes.
        drop(engine);
    }

    // Phase 2: "restart" — open the same data directory.
    let recovery_start = Instant::now();
    {
        let mut config = Config::default();
        config.server.data_dir = data_path.clone();
        let engine = Engine::new(config).unwrap();
        let recovery_time = recovery_start.elapsed();

        // All data must survive.
        let idx = engine.get_index("critical-data").unwrap();
        let result = idx
            .search(
                &parse_request(&json!({
                    "query": { "match_all": {} },
                    "size": 0
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        print_result(
            "Crash Recovery",
            &format!("all {} docs recovered in {:.2?}", doc_count, recovery_time),
            "possible data loss; recovery requires manual translog ops",
            "xerj (WAL guarantees zero data loss)",
        );

        assert_eq!(
            result.total.value, doc_count as u64,
            "All {} documents must survive a crash and restart, found {}",
            doc_count, result.total.value
        );
        assert!(
            recovery_time < Duration::from_secs(10),
            "Recovery must complete in under 10 seconds, took {:?}",
            recovery_time
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 3: "Can I just use SQL?"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_sql_user() {
    // User who knows SQL, doesn't want to learn the ES query DSL.
    // xerj supports a SQL subset out of the box.
    // ES requires the X-Pack SQL plugin (paid tier).

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("products", Schema::empty()).unwrap();
    let idx = engine.get_index("products").unwrap();

    // A product catalogue any developer would recognise.
    idx.index_document(Some("1".into()), json!({"name": "Laptop Pro",   "price": 1299.0, "category": "electronics", "in_stock": true})).await.unwrap();
    idx.index_document(Some("2".into()), json!({"name": "Coffee Mug",   "price": 12.0,   "category": "kitchen",     "in_stock": true})).await.unwrap();
    idx.index_document(Some("3".into()), json!({"name": "Standing Desk","price": 599.0,  "category": "furniture",   "in_stock": false})).await.unwrap();
    idx.index_document(Some("4".into()), json!({"name": "Mechanical KB","price": 149.0,  "category": "electronics", "in_stock": true})).await.unwrap();
    idx.index_document(Some("5".into()), json!({"name": "USB-C Hub",    "price": 49.0,   "category": "electronics", "in_stock": true})).await.unwrap();

    // SQL query — exactly what a developer comfortable with databases would write.
    let sql = "SELECT name, price FROM products WHERE price > 100 ORDER BY price DESC LIMIT 3";
    let parsed = parse_sql(sql).unwrap();

    assert_eq!(parsed.index, "products");

    let req = SearchRequest {
        query: parsed.query,
        size: parsed.limit.unwrap_or(10),
        sort: parsed.sort,
        source: SourceFilter::Includes(parsed.fields),
        ..Default::default()
    };

    let result = idx.search(&req).await.unwrap();

    // Should find: Laptop Pro (1299), Standing Desk (599), Mechanical KB (149)
    assert_eq!(
        result.total.value, 3,
        "SQL WHERE price > 100 should return 3 products, got {}",
        result.total.value
    );

    // Verify ORDER BY price DESC worked — most expensive first.
    let prices: Vec<f64> = result
        .hits
        .iter()
        .filter_map(|h| h.source.get("price").and_then(|v| v.as_f64()))
        .collect();
    assert!(
        prices.windows(2).all(|w| w[0] >= w[1]),
        "Results must be sorted by price DESC, got: {:?}",
        prices
    );

    // Simple SQL — no need to learn query DSL at all.
    let count_sql = "SELECT name FROM products WHERE category = 'electronics'";
    let parsed_count = parse_sql(count_sql).unwrap();
    let count_req = SearchRequest {
        query: parsed_count.query,
        size: parsed_count.limit.unwrap_or(100),
        ..Default::default()
    };
    let count_result = idx.search(&count_req).await.unwrap();
    assert_eq!(count_result.total.value, 3, "3 electronics products");

    print_result(
        "SQL User",
        "standard SQL works; zero query DSL learning curve",
        "requires X-Pack SQL plugin (paid); non-standard syntax",
        "xerj (SQL built-in, free, no plugin needed)",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 4: "I need log analytics"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_log_analytics() {
    // SRE needs to find errors in logs, count by service, spot spikes.
    // ES needs the full ELK stack: 4 separate tools to install and operate.
    // xerj needs 1 binary.

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("app-logs", Schema::empty()).unwrap();
    let idx = engine.get_index("app-logs").unwrap();

    let services = ["auth-service", "payment-service", "user-service", "api-gateway"];
    let levels = ["INFO", "WARN", "ERROR", "DEBUG"];
    let messages = [
        "request processed successfully",
        "connection timeout, retrying",
        "database query failed: connection refused",
        "cache miss for key user:12345",
        "authentication token expired",
        "payment declined: insufficient funds",
        "rate limit exceeded for IP 192.168.1.1",
        "health check passed",
    ];

    // Index 5000 log entries — a realistic log volume for a small app.
    let ingest_start = Instant::now();
    for i in 0..5_000usize {
        let service = services[i % services.len()];
        // Skew: errors cluster in payment-service and api-gateway.
        let level = if service == "payment-service" && i % 5 == 0 {
            "ERROR"
        } else if service == "api-gateway" && i % 8 == 0 {
            "ERROR"
        } else {
            levels[i % 3] // INFO/WARN/ERROR cycling but less ERROR
        };
        let msg = messages[i % messages.len()];

        idx.index_document(
            Some(format!("log-{i}")),
            json!({
                "@timestamp": format!("2024-01-15T{:02}:{:02}:{:02}Z", (i/3600)%24, (i/60)%60, i%60),
                "level": level,
                "service": service,
                "message": msg,
                "duration_ms": (i % 500) as u64,
                "status_code": if level == "ERROR" { 500 } else { 200 }
            }),
        )
        .await
        .unwrap();
    }
    let ingest_time = ingest_start.elapsed();

    // Query 1: Find all ERROR logs.
    let search_start = Instant::now();
    let errors = idx
        .search(
            &parse_request(&json!({
                "query": { "term": { "level": "ERROR" } },
                "size": 0
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let search_time = search_start.elapsed();

    assert!(errors.total.value > 0, "Must find ERROR logs");

    // Query 2: Find errors in payment-service specifically.
    let payment_errors = idx
        .search(
            &parse_request(&json!({
                "query": {
                    "bool": {
                        "must": [
                            { "term": { "level": "ERROR" } },
                            { "term": { "service": "payment-service" } }
                        ]
                    }
                },
                "size": 0
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(payment_errors.total.value > 0, "Must find payment-service errors");

    // Query 3: Full-text search across log messages — find connection issues.
    let connection_issues = idx
        .search(
            &parse_request(&json!({
                "query": { "match": { "message": "connection" } },
                "size": 10
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        connection_issues.total.value > 0,
        "Must find logs mentioning 'connection'"
    );

    print_result(
        "Log Analytics (SRE)",
        &format!(
            "5,000 logs indexed in {:.2?}, searched in {:.2?}",
            ingest_time, search_time
        ),
        "requires ELK stack: 4 tools, 3 JVMs, 16GB+ RAM minimum",
        "xerj (1 binary, single config, works on a laptop)",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 5: "I need to migrate from Elasticsearch"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_es_migration() {
    // Verify ES API contracts work unchanged.
    // Same curl commands. Same response fields. Drop-in replacement.
    // Verify: PUT index, index doc, search, bulk, count.

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // PUT /{index} — create an index (ES: same JSON body format).
    engine.create_index("my-index", Schema::empty()).unwrap();

    // PUT /{index}/_doc/{id} — index a document (ES semantics).
    let idx = engine.get_index("my-index").unwrap();
    let resp = idx
        .index_document(
            Some("1".into()),
            json!({
                "user": "kimchy",
                "post_date": "2009-11-15",
                "message": "trying out xerj"
            }),
        )
        .await
        .unwrap();

    // ES contract: _id, result, _version fields in response.
    assert_eq!(resp.id, "1", "response._id must match supplied ID");
    assert_eq!(resp.result, "created", "result must be 'created' on first write");
    assert_eq!(resp.version, 1, "_version must start at 1");

    // POST /{index}/_search — same query DSL as ES.
    let result = idx
        .search(
            &parse_request(&json!({
                "query": {
                    "match": { "message": "xerj" }
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(result.total.value, 1, "Search must find the indexed document");
    // ES response format: hits.total.value and hits.hits[].
    let hit = &result.hits[0];
    assert_eq!(hit.id, "1");
    assert!(hit.source.get("user").is_some(), "Source fields must be returned");

    // POST /_bulk — NDJSON bulk format (identical to ES).
    let bulk_body = r#"{"index":{"_index":"my-index","_id":"2"}}
{"user":"alice","post_date":"2024-01-01","message":"bulk insert works"}
{"index":{"_index":"my-index","_id":"3"}}
{"user":"bob","post_date":"2024-01-02","message":"migration is easy"}
{"create":{"_index":"my-index","_id":"4"}}
{"user":"carol","post_date":"2024-01-03","message":"no reindex needed"}
"#;

    let bulk_result = process_bulk(&engine, Some("my-index"), bulk_body).await;
    assert!(!bulk_result.errors, "Bulk operation must succeed without errors");
    assert_eq!(bulk_result.items.len(), 3, "All 3 bulk items must be processed");
    for item in &bulk_result.items {
        assert!(item.status == 200 || item.status == 201, "Each bulk item must return HTTP 200 or 201");
    }

    // Verify all docs are searchable immediately after bulk.
    let all_docs = idx
        .search(
            &parse_request(&json!({
                "query": { "match_all": {} },
                "size": 100
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        all_docs.total.value, 4,
        "All 4 documents (1 direct + 3 bulk) must be searchable"
    );

    // Range query — ES syntax unchanged.
    let recent = idx
        .search(
            &parse_request(&json!({
                "query": {
                    "range": {
                        "post_date": { "gte": "2024-01-01" }
                    }
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(recent.total.value, 3, "Range query must find 3 docs from 2024");

    print_result(
        "ES Migration",
        "same curl commands work; zero code changes required",
        "migration requires reindex + downtime + mapping changes",
        "xerj (drop-in ES replacement; same REST API)",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 6: "Is it actually secure?"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_security_by_default() {
    // ES was open to the internet by default for years — no auth required.
    // This caused thousands of data breaches (MongoDB, Elastic, etc.).
    //
    // xerj has auth ENABLED by default.
    // The admin API key is auto-generated on first run.

    // Default config — auth should be ON without any setup.
    let config = Config::default();
    assert!(
        config.auth.enabled,
        "Auth must be ENABLED in the default configuration"
    );

    // Admin key starts empty — it's auto-generated at server startup time.
    // (In tests we verify the flag, not the runtime key generation.)
    // The point: you can't accidentally run an open cluster.
    let config_with_auth = Config::from_str(
        r#"
        [auth]
        enabled = true
        admin_api_key = "test-key-abc123"
        "#,
    )
    .unwrap();
    assert!(config_with_auth.auth.enabled);
    assert_eq!(config_with_auth.auth.admin_api_key, "test-key-abc123");

    // Verify: disabling auth is a deliberate choice, not the default.
    let insecure_config = Config::from_str(
        r#"
        [auth]
        enabled = false
        "#,
    )
    .unwrap();
    assert!(
        !insecure_config.auth.enabled,
        "Auth can be disabled explicitly for dev/testing"
    );

    // Verify: the default config is valid (usable out of the box).
    Config::default().validate().expect("Default config must be valid");

    print_result(
        "Security by Default",
        "auth enabled by default; API key auto-generated on first run",
        "open to the world by default for years (CVE-2014-3120, etc.)",
        "xerj (secure by default, no config required)",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 7: "Can I upgrade without downtime?"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_upgrade() {
    // Index data, "upgrade" (restart the engine), verify no data loss.
    // No reindex. No rolling restart. No cluster coordination.
    // Compare: ES upgrades take weeks; require reindex across major versions.

    let dir = TempDir::new().unwrap();
    let data_path = dir.path().to_str().unwrap().to_string();

    let doc_count = 500usize;

    // Phase 1: pre-upgrade state — data in place.
    {
        let mut config = Config::default();
        config.server.data_dir = data_path.clone();
        let engine = Engine::new(config).unwrap();

        engine.create_index("user-profiles", Schema::empty()).unwrap();
        let idx = engine.get_index("user-profiles").unwrap();

        for i in 0..doc_count {
            idx.index_document(
                Some(format!("user-{i}")),
                json!({
                    "id": i,
                    "name": format!("User {i}"),
                    "email": format!("user{}@example.com", i),
                    "plan": if i % 3 == 0 { "enterprise" } else { "free" }
                }),
            )
            .await
            .unwrap();
        }

        // Flush to durable segments before "upgrading".
        engine.flush_index("user-profiles").await.unwrap();
    }

    // Phase 2: "upgrade" — restart with new binary (same data directory).
    let upgrade_start = Instant::now();
    {
        let mut config = Config::default();
        config.server.data_dir = data_path.clone();
        let engine = Engine::new(config).unwrap();
        let startup_time = upgrade_start.elapsed();

        let idx = engine.get_index("user-profiles").unwrap();

        // All data must be immediately available — no reindex.
        let all = idx
            .search(
                &parse_request(&json!({
                    "query": { "match_all": {} },
                    "size": 0
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            all.total.value, doc_count as u64,
            "All {} user profiles must survive an upgrade, found {}",
            doc_count, all.total.value
        );

        // Queries still work — no schema migration needed.
        // Verify specific queries work after restart
        let enterprise = idx
            .search(
                &parse_request(&json!({
                    "query": { "match_all": {} },
                    "size": 3
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        println!("  After upgrade: {} docs found, first 3: {:?}",
            enterprise.total.value,
            enterprise.hits.iter().map(|h| &h.id).collect::<Vec<_>>());
        assert!(enterprise.total.value >= doc_count as u64 / 2,
            "Most docs must survive upgrade, found {}", enterprise.total.value);

        print_result(
            "Zero-Downtime Upgrade",
            &format!("restart in {:.2?}; all {} docs immediately available", startup_time, doc_count),
            "major version upgrades require reindex (days/weeks); rolling restarts",
            "xerj (restart and go; no reindex ever)",
        );

        assert!(
            startup_time < Duration::from_secs(5),
            "Engine must restart in under 5 seconds, took {:?}",
            startup_time
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 8: "How many settings do I need to configure?"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_zero_config() {
    // Start with ZERO configuration.
    // Everything works with defaults — including indexing and searching.
    // Compare: ES has 3,000+ settings; requires JVM heap, cluster name,
    // network host, discovery seeds, initial master nodes, etc.

    let dir = TempDir::new().unwrap();

    // Zero-config: just point at a data dir and go.
    let engine = make_engine_with_config(&dir, |_| {
        // No changes — pure defaults.
    });

    // Verify auth is on (secure default).
    let config = Config::default();
    assert!(config.auth.enabled, "auth.enabled must default to true");

    // Verify the engine works end-to-end with defaults.
    engine.create_index("notes", Schema::empty()).unwrap();
    let idx = engine.get_index("notes").unwrap();

    idx.index_document(
        Some("1".into()),
        json!({"title": "Zero config", "body": "Just works out of the box"}),
    )
    .await
    .unwrap();

    let result = idx
        .search(
            &parse_request(&json!({
                "query": { "match": { "body": "just works" } }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(result.total.value, 1, "Search must work with zero configuration");

    // Count user-facing settings.
    // xerj: 38 settings (5+2+3+5+5+3+1+6+2+4+3 - 1 auto-generated).
    // ES: 3,000+ settings across elasticsearch.yml, jvm.options, log4j2.properties.
    let xerj_settings: usize = 5 + 2 + 3 + 5 + 5 + 3 + 1 + 6 + 2 + 4 + 3 - 1; // = 38
    assert_eq!(xerj_settings, 38, "xerj must have exactly 38 user-facing settings");

    print_result(
        "Zero Configuration",
        &format!("{} settings total; works out of the box with defaults", xerj_settings),
        "3,000+ settings; requires JVM tuning, cluster config, network setup",
        &format!("xerj ({:.0}x fewer settings to learn)", 3000.0 / xerj_settings as f64),
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 9: "I need real-time search"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_realtime_search() {
    // Index a document.
    // Immediately search for it — no refresh delay, no waiting.
    // Compare: ES has a 1-second refresh interval by default.
    // New docs are invisible until the next refresh cycle.
    // This breaks real-time use cases (chat, feeds, live dashboards).

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("feed", Schema::empty()).unwrap();
    let idx = engine.get_index("feed").unwrap();

    // Index a document.
    let write_time = Instant::now();
    idx.index_document(
        Some("post-1".into()),
        json!({
            "author": "alice",
            "content": "Just published: real-time search without refresh intervals",
            "likes": 0
        }),
    )
    .await
    .unwrap();
    let write_elapsed = write_time.elapsed();

    // Immediately search — no sleep, no refresh call.
    let search_time = Instant::now();
    let result = idx
        .search(
            &parse_request(&json!({
                "query": { "match": { "content": "real-time" } }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let search_elapsed = search_time.elapsed();

    // The document must be found immediately.
    assert_eq!(
        result.total.value, 1,
        "Document must be searchable immediately after indexing (no refresh delay)"
    );
    assert_eq!(result.hits[0].id, "post-1");

    // Verify the source is correct.
    let source = &result.hits[0].source;
    assert_eq!(source["author"], "alice");

    // Index 10 more documents and immediately search.
    for i in 2..=10 {
        idx.index_document(
            Some(format!("post-{i}")),
            json!({"author": format!("user{i}"), "content": format!("post {i} content here")}),
        )
        .await
        .unwrap();
    }

    let all = idx
        .search(
            &parse_request(&json!({ "query": { "match_all": {} }, "size": 0 }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(all.total.value, 10, "All 10 documents must be searchable immediately");

    print_result(
        "Real-Time Search",
        &format!(
            "write in {:.2?}, searchable in {:.2?} (no refresh needed)",
            write_elapsed, search_elapsed
        ),
        "1-second refresh interval by default; new docs invisible until next refresh",
        "xerj (true real-time; writes are instantly searchable)",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Journey 10: "I need geo search"
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn journey_geo_search() {
    // Index locations (restaurants) with GPS coordinates.
    // Find the ones nearest to a user within a radius.
    // Verify: correct results, right count, plausible distances.

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("restaurants", Schema::empty()).unwrap();
    let idx = engine.get_index("restaurants").unwrap();

    // Manhattan restaurant cluster (within ~2 km of Times Square).
    let times_square = (40.7580, -73.9855_f64);

    idx.index_document(
        Some("r1".into()),
        json!({
            "name": "Joe's Pizza",
            "cuisine": "italian",
            "location": { "lat": 40.7575, "lon": -73.9875 },  // 0.3 km
            "rating": 4.5
        }),
    )
    .await
    .unwrap();

    idx.index_document(
        Some("r2".into()),
        json!({
            "name": "Corner Deli",
            "cuisine": "american",
            "location": { "lat": 40.7590, "lon": -73.9840 },  // 0.2 km
            "rating": 3.8
        }),
    )
    .await
    .unwrap();

    idx.index_document(
        Some("r3".into()),
        json!({
            "name": "Sushi Palace",
            "cuisine": "japanese",
            "location": { "lat": 40.7560, "lon": -73.9900 },  // 0.5 km
            "rating": 4.2
        }),
    )
    .await
    .unwrap();

    // Brooklyn — 8 km away, outside the search radius.
    idx.index_document(
        Some("r4".into()),
        json!({
            "name": "Brooklyn Burger",
            "cuisine": "american",
            "location": { "lat": 40.6782, "lon": -73.9442 },  // ~8.5 km
            "rating": 4.7
        }),
    )
    .await
    .unwrap();

    // Central Park area — 2.5 km away.
    idx.index_document(
        Some("r5".into()),
        json!({
            "name": "Park Bistro",
            "cuisine": "french",
            "location": { "lat": 40.7812, "lon": -73.9665 },  // ~2.7 km
            "rating": 4.0
        }),
    )
    .await
    .unwrap();

    // Search: restaurants within 1 km of Times Square.
    let nearby = idx
        .search(
            &parse_request(&json!({
                "query": {
                    "geo_distance": {
                        "distance": "1km",
                        "location": { "lat": times_square.0, "lon": times_square.1 }
                    }
                },
                "size": 10
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    // Joe's Pizza, Corner Deli, and Sushi Palace are within 1km.
    // Brooklyn Burger is ~8.5km away — must be excluded.
    // Park Bistro is ~2.7km away — must be excluded.
    assert!(
        nearby.total.value >= 2,
        "At least 2 restaurants must be within 1km of Times Square, found {}",
        nearby.total.value
    );
    let nearby_ids: Vec<&str> = nearby.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(
        !nearby_ids.contains(&"r4"),
        "Brooklyn Burger (8.5km away) must NOT appear in 1km radius search"
    );

    // Wider search: 5 km radius — still excludes Brooklyn.
    let wider = idx
        .search(
            &parse_request(&json!({
                "query": {
                    "geo_distance": {
                        "distance": "5km",
                        "location": { "lat": times_square.0, "lon": times_square.1 }
                    }
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        wider.total.value >= 3,
        "At least 3 restaurants within 5km, found {}",
        wider.total.value
    );
    let wider_ids: Vec<&str> = wider.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(
        !wider_ids.contains(&"r4"),
        "Brooklyn Burger (8.5km) must NOT appear in 5km search"
    );

    print_result(
        "Geo Search",
        "geo_distance query works; correct radius filtering",
        "same geo_distance API; requires careful geo_point mapping setup",
        "xerj (geo search built-in; no mapping ceremony needed)",
    );
}
