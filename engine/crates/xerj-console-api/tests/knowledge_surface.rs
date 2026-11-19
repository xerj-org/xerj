//! The knowledge surface (`GET /_xerj-console/api/v1/knowledge`) serves the
//! catalog's own facts (issue: "one knowledge surface after indexing").
//!
//! What a person must see the moment `xerj brain` finishes — how large the
//! corpus is, which data is in it, what they can do with it — is computed
//! server-side from the autoindex catalog and the live engine. These tests
//! pin the PAYLOAD (never the styling):
//!
//! - catalog facts survive the round trip: record/file/byte counts, the
//!   full field list with types + null ratios + example values, the
//!   semantic field flag, the sample queries;
//! - the engine's LIVE numbers are joined on: doc counts come from
//!   `Index::stats`, store bytes from the index's data dir (nonzero after a
//!   real write), an index the catalog does not describe still shows up
//!   under `others` (an engine filled another way must not read as empty);
//! - relations are exactly what autoindex inferred — key overlaps with
//!   their overlap/grade, time alignments with their Pearson r — and a
//!   catalog with none yields `relations: []`, not a guess;
//! - the capability strip is grounded: `map`/`ask` appear only when a
//!   catalog exists, the graph entry only for a role that may read brains
//!   AND only when a brain exists, and every entry names a real console
//!   route, CLI command, or endpoint;
//! - the `semantic` entry (#1099) is the node's own posture — dims,
//!   similarity and the embedder's label from the live schema — and never
//!   a vector count (the engine exposes none) nor a READY claim; an index
//!   that lost its embedding says so in as many words;
//! - a node with nothing indexed is the ordinary empty state (`catalog:
//!   false`), not an error;
//! - the endpoint is session-authorized: no cookie is a 401.

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_common::types::{EmbeddingConfig, FieldConfig, FieldType, Schema};
use xerj_console_api::{
    auth::{sessions, store},
    state::ClusterMode,
    xerj_console_router, ConsoleState,
};
use xerj_engine::Engine;

const KNOWLEDGE_PATH: &str = "/_xerj-console/api/v1/knowledge";

struct TestApp {
    router: Router,
    owner_cookie: String,
    viewer_cookie: String,
    _dir: TempDir,
}

async fn add_session_user(state: &ConsoleState, engine: &Engine, id: &str, role: &str) -> String {
    let user = store::User {
        id: id.to_string(),
        email: format!("{id}@example.com"),
        display_name: id.to_string(),
        role: role.to_string(),
        status: store::UserStatus::Active,
        created_at: xerj_console_api::time::now_iso(),
        last_seen_at: Some(xerj_console_api::time::now_iso()),
    };
    store::upsert_user(engine, &user).await.unwrap();
    let (_s, signed) = sessions::mint_session(state, &user.id, "passkey", None, None)
        .await
        .unwrap();
    format!("xerj_session={signed}")
}

fn schema(pairs: &[(&str, FieldType)]) -> Schema {
    let mut s = Schema::empty();
    for (name, t) in pairs {
        s.add_field(FieldConfig::new(*name, *t)).expect("add field");
    }
    s
}

/// The catalog index's declared subset these tests read (the frozen mapping
/// in `xerj_autoindex::catalog::catalog_mapping` has ~40 properties; the
/// console only term-queries `doc_kind` and reads `_source`).
fn catalog_schema() -> Schema {
    schema(&[
        ("doc_kind", FieldType::Keyword),
        ("index_name", FieldType::Keyword),
        ("slug", FieldType::Keyword),
        ("formats", FieldType::Keyword),
        ("record_count", FieldType::Long),
        ("junk_records", FieldType::Long),
        ("bytes", FieldType::Long),
        ("file_count", FieldType::Long),
        ("time_field", FieldType::Keyword),
        ("time_min", FieldType::Date),
        ("time_max", FieldType::Date),
        ("semantic_field", FieldType::Keyword),
        ("fields_json", FieldType::Text),
        ("sample_queries_json", FieldType::Text),
        ("run_id", FieldType::Keyword),
        ("corr_kind", FieldType::Keyword),
        ("a_dataset", FieldType::Keyword),
        ("b_dataset", FieldType::Keyword),
        ("a_index", FieldType::Keyword),
        ("b_index", FieldType::Keyword),
        ("a_field", FieldType::Keyword),
        ("b_field", FieldType::Keyword),
        ("grade", FieldType::Keyword),
        ("overlap", FieldType::Long),
        ("containment", FieldType::Double),
        ("examples", FieldType::Keyword),
    ])
}

/// The exact `FieldSpec` JSON autoindex serializes into `fields_json` (see
/// `infer::FieldSpec` — `semantic` is the embedder's label and is skipped
/// when absent, `null_ratio`/`coverage` are always present).
fn fields_json() -> String {
    json!([
        {
            "name": "email_from",
            "es_type": "keyword",
            "cardinality_est": 12,
            "cardinality_overflow": false,
            "null_ratio": 0.0,
            "avg_len": 0.0,
            "coverage": 1.0,
            "examples": ["sam@acme.example", "jo@grid.example"]
        },
        {
            "name": "body",
            "es_type": "semantic_text",
            "semantic": "lexical-hash-384",
            "cardinality_est": 91,
            "cardinality_overflow": false,
            "null_ratio": 0.0,
            "avg_len": 412.5,
            "coverage": 1.0,
            "examples": ["term sheet attached"]
        },
        {
            "name": "page",
            "es_type": "long",
            "cardinality_est": 4,
            "cardinality_overflow": false,
            "null_ratio": 0.75,
            "avg_len": 0.0,
            "coverage": 0.25,
            "examples": ["1", "2"]
        }
    ])
    .to_string()
}

/// Boot the console over a node `xerj brain` could have left behind:
/// a catalog describing two datasets (one larger, one smaller), one
/// key-overlap correlation, a brain over the first dataset, and one user
/// index the catalog does NOT describe.
///
/// `embed_semantic` decides whether ax-mail's `body` carries the embedding
/// config an autoindex run installs (the `semantic_text` mapping): `true` is
/// the ordinary post-`xerj brain` node, `false` is an index that was rebuilt
/// or mapped another way — the catalog still ELECTS `body`, but the index
/// has nothing to embed it with (#1099's "no vectors on this node" case).
async fn boot_with(embed_semantic: bool) -> TestApp {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .unwrap();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );
    let owner_cookie = add_session_user(&state, &engine, "owner-test", "owner").await;
    let viewer_cookie = add_session_user(&state, &engine, "viewer-test", "viewer").await;

    // The dataset indices the catalog describes — 3 real documents in
    // ax-mail (the LIVE count the endpoint must join on), 1 in ax-pdfs.
    // ax-mail's `body` carries the embedding config (dims/similarity) the
    // es-compat layer installs for autoindex's `semantic_text` mapping.
    let mut mail_schema = Schema::empty();
    let mut body = FieldConfig::new("body", FieldType::Text);
    if embed_semantic {
        body.options.dimensions = Some(384);
        body.options.similarity = Some("cosine".into());
        body.embedding = Some(EmbeddingConfig {
            endpoint: None,
            model: None,
            target_field: None,
        });
    }
    mail_schema.add_field(body).expect("add body");
    engine
        .create_index("ax-mail", mail_schema)
        .expect("create ax-mail");
    engine
        .get_index("ax-mail")
        .unwrap()
        .index_documents_batched(vec![
            (Some("m1".into()), json!({"body": "term sheet attached"})),
            (Some("m2".into()), json!({"body": "lunch on friday"})),
            (Some("m3".into()), json!({"body": "re: contract"})),
        ])
        .await;
    engine
        .create_index("ax-pdfs", schema(&[("text", FieldType::Text)]))
        .expect("create ax-pdfs");
    engine
        .get_index("ax-pdfs")
        .unwrap()
        .index_documents_batched(vec![(
            Some("p1".into()),
            json!({"text": "invoice 2026-09"}),
        )])
        .await;

    // An index the catalog does not describe — filled some other way, and
    // still part of "what is on this node".
    engine
        .create_index("weblogs", schema(&[("message", FieldType::Text)]))
        .expect("create weblogs");
    engine
        .get_index("weblogs")
        .unwrap()
        .index_documents_batched(vec![(
            Some("w1".into()),
            json!({"message": "timeout on shard 2"}),
        )])
        .await;

    // The catalog: two dataset docs + one key-overlap correlation.
    engine
        .create_index("autoindex-catalog", catalog_schema())
        .expect("create catalog");
    let catalog = engine.get_index("autoindex-catalog").unwrap();
    catalog
        .index_documents_batched(vec![
            (
                Some("ds:home:ax-mail".into()),
                json!({
                    "doc_kind": "dataset",
                    "index_name": "ax-mail",
                    "slug": "mail",
                    "formats": ["eml"],
                    "record_count": 91,
                    "junk_records": 2,
                    "bytes": 482_133,
                    "file_count": 14,
                    "time_field": "email_date",
                    "time_min": "2026-01-05T00:00:00.000Z",
                    "time_max": "2026-09-19T00:00:00.000Z",
                    "semantic_field": "body",
                    "fields_json": fields_json(),
                    "sample_queries_json": [
                        r#"{"query":{"match":{"body":"term sheet"}},"size":3}"#
                    ],
                    "run_id": "run-1",
                }),
            ),
            (
                Some("ds:home:ax-pdfs".into()),
                json!({
                    "doc_kind": "dataset",
                    "index_name": "ax-pdfs",
                    "slug": "pdfs",
                    "formats": ["pdf"],
                    "record_count": 14,
                    "junk_records": 0,
                    "bytes": 3_111_999,
                    "file_count": 4,
                    "semantic_field": "text",
                    "fields_json": json!([
                        {"name": "text", "es_type": "semantic_text", "semantic": "lexical-hash-384",
                         "cardinality_est": 14, "cardinality_overflow": false, "null_ratio": 0.0,
                         "avg_len": 1200.0, "coverage": 1.0, "examples": ["invoice 2026-09"]},
                        {"name": "page", "es_type": "long",
                         "cardinality_est": 9, "cardinality_overflow": false, "null_ratio": 0.1,
                         "avg_len": 0.0, "coverage": 0.9, "examples": ["1"]}
                    ]).to_string(),
                    "run_id": "run-1",
                }),
            ),
            (
                Some("corr:home:mail:pdfs:email_from:title".into()),
                json!({
                    "doc_kind": "correlation",
                    "corr_kind": "key_overlap",
                    "a_dataset": "mail", "a_index": "ax-mail", "a_field": "email_from",
                    "b_dataset": "pdfs", "b_index": "ax-pdfs", "b_field": "title",
                    "overlap": 7, "containment": 0.58, "grade": "likely",
                    "examples": ["sam@acme.example"],
                    "confirmed_values": 7, "tested_values": 12,
                }),
            ),
        ])
        .await;

    // One brain over ax-mail, with a meta doc naming its nodes index and
    // two edges (the §2.5 shape `graph::brains` reads).
    engine
        .create_index(
            ".xerj-memory-casefile-edges",
            schema(&[
                ("edge_id", FieldType::Keyword),
                ("src", FieldType::Keyword),
                ("dst", FieldType::Keyword),
                ("type", FieldType::Keyword),
            ]),
        )
        .expect("create edges index");
    let edges = engine.get_index(".xerj-memory-casefile-edges").unwrap();
    edges
        .index_documents_batched(vec![
            (
                Some("__xerj-brain-meta".into()),
                json!({ "meta_version": 1, "brain": "casefile", "nodes_index": "ax-mail" }),
            ),
            (
                Some("e1".into()),
                json!({ "edge_id": "e1", "src": "m1", "dst": "m2", "type": "same_dir" }),
            ),
            (
                Some("e2".into()),
                json!({ "edge_id": "e2", "src": "m1", "dst": "m3", "type": "mdlink" }),
            ),
        ])
        .await;

    let router = xerj_console_router(state);
    TestApp {
        router,
        owner_cookie,
        viewer_cookie,
        _dir: dir,
    }
}

/// The ordinary post-`xerj brain` node: the elected semantic field carries
/// its embedding config.
async fn boot() -> TestApp {
    boot_with(true).await
}

/// A node with NOTHING indexed — no catalog, no user index.
async fn boot_empty() -> TestApp {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .unwrap();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );
    let owner_cookie = add_session_user(&state, &engine, "owner-test", "owner").await;
    let viewer_cookie = add_session_user(&state, &engine, "viewer-test", "viewer").await;
    let router = xerj_console_router(state);
    TestApp {
        router,
        owner_cookie,
        viewer_cookie,
        _dir: dir,
    }
}

async fn get(app: &TestApp, cookie: Option<&str>) -> (axum::http::StatusCode, Value) {
    let mut b = Request::builder().method("GET").uri(KNOWLEDGE_PATH);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    let resp = app
        .router
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).expect("json body");
    (status, v)
}

fn data(v: &Value) -> &Value {
    v.get("data").expect("console envelope {data}")
}

#[tokio::test]
async fn no_session_is_a_401() {
    let app = boot().await;
    let (status, _) = get(&app, None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_payload_carries_the_catalogs_own_facts() {
    let app = boot().await;
    let (status, v) = get(&app, Some(&app.owner_cookie)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let d = data(&v);
    // KNOWLEDGE_PRINT=1 cargo test knowledge_surface -- --nocapture
    // prints the exact payload the handler served (the same fixture the
    // node-test SPA suite renders) — the run book for PR bodies and for
    // eyeballing a change, without booting a node and enrolling a passkey.
    if std::env::var("KNOWLEDGE_PRINT").is_ok() {
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    }

    // Gate 1 — how large. Totals are the catalog's own numbers…
    let totals = d.get("totals").unwrap();
    assert_eq!(totals["datasets"].as_u64(), Some(2));
    assert_eq!(totals["records"].as_u64(), Some(91 + 14));
    assert_eq!(totals["files"].as_u64(), Some(14 + 4));
    assert_eq!(totals["bytes"].as_u64(), Some(482_133 + 3_111_999));
    assert_eq!(totals["relations"].as_u64(), Some(1));

    // …and the LIVE doc counts are joined on (3 real docs, not the catalog's 91).
    let datasets = d["datasets"].as_array().unwrap();
    assert_eq!(datasets.len(), 2);
    assert_eq!(
        datasets[0]["index"].as_str(),
        Some("ax-mail"),
        "largest first"
    );
    assert_eq!(datasets[0]["live_docs"].as_u64(), Some(3));
    assert_eq!(datasets[0]["records"].as_u64(), Some(91));
    assert_eq!(datasets[1]["index"].as_str(), Some("ax-pdfs"));
    assert_eq!(datasets[1]["live_docs"].as_u64(), Some(1));
    // Store bytes are measured from the index's own data dir — real writes
    // happened, so this is nonzero.
    assert!(datasets[0]["store_bytes"].as_u64().unwrap_or(0) > 0);

    // Gate 2 — which data. The FULL field list with the spec's facts.
    let fields = datasets[0]["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 3, "no 8-field cap on the payload");
    let by_name = |n: &str| {
        fields
            .iter()
            .find(|f| f["name"].as_str() == Some(n))
            .unwrap_or_else(|| panic!("field {n} missing: {fields:#?}"))
    };
    let from = by_name("email_from");
    assert_eq!(from["type"].as_str(), Some("keyword"));
    assert_eq!(from["null_ratio"].as_f64(), Some(0.0));
    assert_eq!(from["cardinality"].as_u64(), Some(12));
    assert_eq!(
        from["examples"].as_array().unwrap()[0].as_str(),
        Some("sam@acme.example")
    );
    let body = by_name("body");
    assert_eq!(
        body["semantic"].as_bool(),
        Some(true),
        "the semantic_text field is flagged"
    );
    assert_eq!(body["type"].as_str(), Some("semantic_text"));
    // the spec's measured average value length round-trips — the SPA ranks
    // the card's fields by coverage × avg_len, so the corpus's DOMINANT
    // fields lead the table (#1098)
    assert_eq!(body["avg_len"].as_f64(), Some(412.5));
    let page = by_name("page");
    assert_eq!(page["null_ratio"].as_f64(), Some(0.75));
    assert_eq!(page["coverage"].as_f64(), Some(0.25));
    // the semantic field the run elected is surfaced as a dataset fact
    assert_eq!(datasets[0]["semantic_field"].as_str(), Some("body"));
    // the catalog's ready-to-send sample query round-trips as JSON
    let sq = datasets[0]["sample_queries"].as_array().unwrap();
    assert_eq!(sq[0]["query"]["match"]["body"].as_str(), Some("term sheet"));

    // …and an index the catalog does not describe is still listed.
    let others = d["others"].as_array().unwrap();
    assert_eq!(others.len(), 1);
    assert_eq!(others[0]["index"].as_str(), Some("weblogs"));
    assert_eq!(others[0]["docs"].as_u64(), Some(1));
}

#[tokio::test]
async fn relations_are_exactly_what_autoindex_inferred() {
    let app = boot().await;
    let (_, v) = get(&app, Some(&app.owner_cookie)).await;
    let rels = data(&v)["relations"].as_array().unwrap();
    assert_eq!(rels.len(), 1);
    let r = &rels[0];
    assert_eq!(r["kind"].as_str(), Some("key_overlap"));
    assert_eq!(r["a_index"].as_str(), Some("ax-mail"));
    assert_eq!(r["a_field"].as_str(), Some("email_from"));
    assert_eq!(r["b_index"].as_str(), Some("ax-pdfs"));
    assert_eq!(r["b_field"].as_str(), Some("title"));
    assert_eq!(r["overlap"].as_u64(), Some(7));
    assert_eq!(r["grade"].as_str(), Some("likely"));
    assert_eq!(r["confirmed_values"].as_u64(), Some(7));
    assert_eq!(r["tested_values"].as_u64(), Some(12));
}

#[tokio::test]
async fn the_capability_strip_is_grounded_in_facts() {
    let app = boot().await;
    let (_, v) = get(&app, Some(&app.owner_cookie)).await;
    let d = data(&v);
    let caps = d["capabilities"].as_array().unwrap();
    let ids: Vec<&str> = caps.iter().filter_map(|c| c["id"].as_str()).collect();
    for must in [
        "search", "read", "graph", "ask", "map", "decide", "watch", "share",
    ] {
        assert!(ids.contains(&must), "missing capability {must}: {ids:?}");
    }
    // every entry names its real surface
    for c in caps {
        let named = ["href", "command", "endpoint"]
            .iter()
            .any(|k| c[*k].is_string());
        assert!(named, "capability without a real surface: {c:#?}");
    }
    // a console route is an in-app hash route; nothing else can be an href
    for c in caps {
        if let Some(href) = c["href"].as_str() {
            assert!(href.starts_with("#/"), "non-app href {href}");
        }
    }
    // the owner's brain shows up, with its real link count
    let brains = d["brains"].as_array().unwrap();
    assert_eq!(brains.len(), 1);
    assert_eq!(brains[0]["name"].as_str(), Some("casefile"));
    assert_eq!(brains[0]["links"].as_u64(), Some(2));

    // …and a role that may not read brains gets neither the brain nor the
    // graph capability (no existence oracle, same rule as /graph/brains).
    let (_, vv) = get(&app, Some(&app.viewer_cookie)).await;
    let dv = data(&vv);
    assert_eq!(dv["brains"].as_array().unwrap().len(), 0);
    let vids: Vec<&str> = dv["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert!(
        !vids.contains(&"graph"),
        "viewer must not see the graph capability"
    );
}

/// #1099 — the semantic capability is the NODE's own posture, computed from
/// the live schema and the embedder, and it never claims a vector count the
/// engine cannot back (the screenshot defect: "6,546,091 BODY VECTORS ·
/// 384-D · COSINE — READY" on a node reporting no such number anywhere).
#[tokio::test]
async fn the_semantic_capability_is_the_nodes_own_facts_with_no_invented_count() {
    let app = boot().await;
    let (_, v) = get(&app, Some(&app.owner_cookie)).await;
    let caps = data(&v)["capabilities"].as_array().unwrap();
    let sem = caps
        .iter()
        .find(|c| c["id"].as_str() == Some("semantic"))
        .expect("a semantic_text field with an embedding config earns the semantic entry");
    // the facts are the schema's and the embedder's own: field, companion,
    // dims (from the mapping here), similarity, and the embedder's label
    let blurb = sem["blurb"].as_str().unwrap();
    assert!(blurb.contains("`body`"), "names the elected field: {blurb}");
    assert!(
        blurb.contains("`body_vector`"),
        "names the companion: {blurb}"
    );
    assert!(blurb.contains("384-D"), "dims from the mapping: {blurb}");
    assert!(
        blurb.contains("cosine"),
        "similarity from the mapping: {blurb}"
    );
    assert!(
        blurb.contains("lexical feature-hash (built-in, 384-dim, non-neural)"),
        "the embedder's own honesty label, verbatim: {blurb}"
    );
    assert!(
        blurb.contains("NOT neural"),
        "the lexical case says so plainly: {blurb}"
    );
    assert!(
        blurb.contains("--embed-mode neural"),
        "and how to get neural: {blurb}"
    );
    // the honest-claims line: no vector count and no READY claim — the node
    // exposes no per-field vector count on any stats surface, so none may
    // appear here, whatever the client does with the payload
    assert!(!blurb.contains("VECTOR"), "no vector count: {blurb}");
    assert!(!blurb.contains("READY"), "no readiness claim: {blurb}");
    for key in sem.as_object().unwrap().keys() {
        assert!(
            !key.contains("count"),
            "no count key may ride the entry: {key}"
        );
    }
    // it is not brain-gated — it describes the corpus, not the operator's role
    let (_, vv) = get(&app, Some(&app.viewer_cookie)).await;
    let viewer_has = data(&vv)["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"].as_str() == Some("semantic"));
    assert!(
        viewer_has,
        "the viewer sees the node's semantic posture too"
    );
}

/// #1099's other shape — the catalog's last run ELECTED a semantic field,
/// but the live index carries no embedding for it (rebuilt / mapped another
/// way): the entry must say plainly that there are no vectors to match,
/// never a derived count and never READY.
#[tokio::test]
async fn an_elected_semantic_field_the_index_does_not_carry_is_stated_plainly() {
    let app = boot_with(false).await;
    let (_, v) = get(&app, Some(&app.owner_cookie)).await;
    let caps = data(&v)["capabilities"].as_array().unwrap();
    let sem = caps
        .iter()
        .find(|c| c["id"].as_str() == Some("semantic"))
        .expect("the catalog elected body; the mismatch must be visible, not omitted");
    let blurb = sem["blurb"].as_str().unwrap();
    assert!(blurb.contains("`body`"), "names the elected field: {blurb}");
    assert!(blurb.contains("no embedding"), "states the gap: {blurb}");
    assert!(
        blurb.contains("no vectors to match"),
        "states it in those words: {blurb}"
    );
    assert!(!blurb.contains("READY"), "no readiness claim: {blurb}");
    assert!(!blurb.contains("VECTOR"), "no vector count: {blurb}");
}

#[tokio::test]
async fn nothing_indexed_is_the_empty_state_not_an_error() {
    let app = boot_empty().await;
    let (status, v) = get(&app, Some(&app.owner_cookie)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let d = data(&v);
    assert_eq!(d["catalog"].as_bool(), Some(false));
    assert_eq!(d["datasets"].as_array().unwrap().len(), 0);
    assert_eq!(d["others"].as_array().unwrap().len(), 0);
    assert_eq!(d["totals"]["records"].as_u64(), Some(0));
    let ids: Vec<&str> = d["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert!(!ids.contains(&"map"), "no catalog → no map/ask capability");
    assert!(!ids.contains(&"ask"), "no catalog → no ask capability");
    assert!(
        !ids.contains(&"semantic"),
        "no dataset elected a semantic field → no semantic claim"
    );
    assert!(ids.contains(&"search"), "search is always real");
}

/// #2409: a fresh data directory reads as "my data is gone" unless the page
/// says which directory this node serves. The owner gets the path and the
/// same --data-dir/--config hint `xerj autoindex map` prints; a viewer gets
/// the hint without the server path.
#[tokio::test]
async fn the_empty_state_names_the_served_data_directory() {
    let app = boot_empty().await;
    let served = std::fs::canonicalize(app._dir.path())
        .unwrap()
        .display()
        .to_string();

    let (_, v) = get(&app, Some(&app.owner_cookie)).await;
    let d = data(&v);
    assert_eq!(d["data_dir"].as_str(), Some(served.as_str()));
    let hint = d["data_dir_hint"].as_str().expect("hint");
    assert!(
        hint.contains(&format!("serves data directory {served}")),
        "{hint}"
    );
    assert!(
        hint.contains("--data-dir") && hint.contains("--config"),
        "{hint}"
    );
    assert!(hint.contains("does not look for others"), "{hint}");

    let (_, vv) = get(&app, Some(&app.viewer_cookie)).await;
    let dv = data(&vv);
    assert!(dv["data_dir"].is_null(), "viewer must not get the path");
    let hint = dv["data_dir_hint"].as_str().expect("hint");
    assert!(!hint.contains(&served), "{hint}");
    assert!(
        hint.contains("--data-dir") && hint.contains("--config"),
        "{hint}"
    );
    assert!(hint.contains("server data directory data_dir="), "{hint}");
}
