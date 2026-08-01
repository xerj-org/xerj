//! Regression tests for the field-less `query_string` projection.
//!
//! `query_string` with no `default_field` searches EVERY mapped text field
//! (ES/OpenSearch `index.query.default_field` defaults to `"*"`). Two hazards
//! come with that widening, and both are covered here:
//!
//!  1. HETEROGENEOUS SEGMENTS. Segments flushed before a field first appeared
//!     carry no FTS side-car for it. The per-segment "does the reader have
//!     every queried field?" gate then rejects the whole segment and the
//!     stored-doc fallback had no `query_string` matcher at all — so those
//!     segments silently contributed ZERO hits. Under-matching on a search
//!     path surfaces nowhere, which is worse than the single-field bug it
//!     replaced.
//!
//!  2. `tokens × fields` CROSS-PRODUCT. One `should` clause is built per
//!     (token, field) pair and the bool executor materialises a full hit
//!     vector per clause, so a wide mapping plus a long query string is an
//!     unbounded, uninterruptible unit of work inside the search hot path.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::ast::SearchRequest;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// One ingest shard and a flush threshold nothing in these tests can reach, so
/// an explicit `flush()` yields exactly ONE segment. The between-segment
/// deadline check is what bounds a multi-segment scan; a single segment is the
/// shape where the only thing standing between a clause storm and an
/// indefinitely-held worker is a poll *inside* the clause loop.
fn make_single_segment_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.storage.flush_size_mb = 8_192;
    config.engine.ingest_shards = 1;
    Engine::new(config).expect("engine::new")
}

fn search(query_json: Value) -> SearchRequest {
    parse_request(&json!({ "query": query_json, "size": 100 })).expect("parse_request")
}

/// A `query_string` whose text the Lucene lowerer cannot parse (here: an
/// unterminated quote) stays an opaque `QueryNode::QueryString` — the node
/// whose projection this PR changes. Anything that lowers becomes a
/// `Match`/`Bool` tree and never reaches that arm.
fn opaque_qs(text: &str) -> Value {
    json!({ "query_string": { "query": format!("\"{text}") } })
}

/// HIGH: heterogeneous segments must contribute the same hits as a
/// homogeneous corpus with the same documents.
///
/// `hetero` flushes a segment holding only `alpha` BEFORE any document
/// introduces `beta`, so that segment's FTS side-car has no `beta` field.
/// `homo` indexes the identical documents but flushes once, after `beta`
/// exists, so every segment carries both fields. The two must agree.
#[tokio::test]
async fn field_less_query_string_matches_equally_across_heterogeneous_segments() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Documents 0..5 only ever have `alpha`; 5..10 have `alpha` + `beta`.
    let docs: Vec<Value> = (0..10)
        .map(|i| {
            if i < 5 {
                json!({ "alpha": format!("needle alpha body {i}") })
            } else {
                json!({ "alpha": format!("filler body {i}"), "beta": "needle beta body" })
            }
        })
        .collect();

    engine.create_index("hetero", Schema::empty()).unwrap();
    let hetero = engine.get_index("hetero").unwrap();
    for (i, d) in docs.iter().enumerate() {
        hetero
            .index_document(Some(format!("d{i}")), d.clone())
            .await
            .unwrap();
        // Flush right after the alpha-only prefix: that segment is written
        // before `beta` is ever seen, so it has no `beta` FTS side-car.
        if i == 4 {
            hetero.flush().await.unwrap();
        }
    }
    hetero.flush().await.unwrap();

    engine.create_index("homo", Schema::empty()).unwrap();
    let homo = engine.get_index("homo").unwrap();
    // Seed `beta` first so the very first segment carries both fields.
    homo.index_document(
        Some("seed".to_string()),
        json!({ "alpha": "seed body", "beta": "seed body" }),
    )
    .await
    .unwrap();
    homo.flush().await.unwrap();
    for (i, d) in docs.iter().enumerate() {
        homo.index_document(Some(format!("d{i}")), d.clone())
            .await
            .unwrap();
    }
    homo.flush().await.unwrap();

    let q = opaque_qs("needle");
    let h = hetero.search(&search(q.clone())).await.unwrap();
    let m = homo.search(&search(q)).await.unwrap();

    assert_eq!(
        h.total.value,
        10,
        "every document holds `needle` in some text field; heterogeneous \
         segments dropped {} of them",
        10 - h.total.value
    );
    assert_eq!(
        h.total.value, m.total.value,
        "heterogeneous segments must match the same documents as a \
         homogeneous corpus (hetero={} homo={})",
        h.total.value, m.total.value
    );
}

/// HIGH, narrower: the same corpus queried for a token that lives ONLY in the
/// field the older segment lacks. The older segment legitimately contributes
/// nothing, but the newer one must still answer.
#[tokio::test]
async fn field_less_query_string_finds_new_field_added_after_a_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("late_field", Schema::empty()).unwrap();
    let idx = engine.get_index("late_field").unwrap();

    for i in 0..5 {
        idx.index_document(Some(format!("old{i}")), json!({ "alpha": "old body" }))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    for i in 0..3 {
        idx.index_document(
            Some(format!("new{i}")),
            json!({ "alpha": "new body", "beta": "zebrafish" }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();

    let r = idx.search(&search(opaque_qs("zebrafish"))).await.unwrap();
    assert_eq!(
        r.total.value, 3,
        "`zebrafish` lives only in `beta`, present in 3 documents"
    );
}

/// BLOCKER: a wide mapping crossed with a long query string must not turn one
/// request into an unbounded unit of work. The request carries a short
/// timeout; it must come back inside a small multiple of that budget rather
/// than grinding through `tokens × fields` clauses.
#[tokio::test]
async fn field_less_query_string_cross_product_respects_the_request_deadline() {
    let dir = TempDir::new().unwrap();
    let engine = make_single_segment_engine(&dir);
    engine.create_index("wide", Schema::empty()).unwrap();
    let idx = engine.get_index("wide").unwrap();

    // Every field of every document carries the SAME vocabulary, so every
    // (token, field) clause the projection can build is a clause that
    // actually returns the whole corpus — the worst case, and the one an
    // attacker picks.
    const FIELDS: usize = 120;
    const TOKENS: usize = 300;
    const DOCS: usize = 600;
    let body: String = (0..TOKENS).map(|t| format!("tok{t} ")).collect::<String>();
    for d in 0..DOCS {
        let mut doc = serde_json::Map::new();
        for f in 0..FIELDS {
            doc.insert(format!("f{f}"), json!(body.clone()));
        }
        idx.index_document(Some(format!("d{d}")), Value::Object(doc))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    // 300 tokens × 120 fields = 36 000 `should` clauses, each matching all
    // 600 documents — 21.6 M materialised hits before a single result is
    // returned, with nothing polling the request deadline in between.
    let req = parse_request(&json!({
        "query": { "query_string": { "query": format!("\"{body}") } },
        "size": 10,
        "timeout": "150ms",
    }))
    .expect("parse_request");

    let stats = idx.stats().await;
    assert_eq!(
        stats.segment_count, 1,
        "the test needs ONE segment: with several, the between-segment \
         deadline check masks an uninterruptible clause loop"
    );

    let t0 = std::time::Instant::now();
    let res = idx.search(&req).await.unwrap();
    let elapsed = t0.elapsed();
    eprintln!(
        "cross-product query: {elapsed:?}, total={} timed_out={}",
        res.total.value, res.timed_out
    );

    // Generous multiple of the 150ms budget so a loaded 2-core CI runner has
    // room, but far below the seconds an unbounded clause storm takes.
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "field-less query_string cross-product ran {elapsed:?} against a \
         150ms deadline — the projection is unbounded and the clause loop \
         never polls the deadline"
    );
    // Whatever comes back must be self-consistent: either it finished inside
    // the budget, or it says it did not. This assertion is load-normalising —
    // a slow machine trips the deadline poll and reports `timed_out`, so only
    // a run with NO poll at all can overshoot while claiming success.
    assert!(
        res.timed_out || elapsed < std::time::Duration::from_millis(1_500),
        "a run that overran its deadline must report timed_out"
    );

    // Load-independent restatement: a budget the work provably cannot meet
    // must come back saying so.
    let impossible = parse_request(&json!({
        "query": { "query_string": { "query": format!("\"{body}") } },
        "size": 10,
        "timeout": "1ms",
    }))
    .expect("parse_request");
    let starved = idx.search(&impossible).await.unwrap();
    assert!(
        starved.timed_out,
        "a 1ms budget against a 36 000-clause cross-product must report \
         timed_out, not silently run to completion"
    );
    // Bounded must not mean silently wrong: partial results are a SUBSET.
    assert!(
        res.total.value <= DOCS as u64,
        "over-counting: {} > {DOCS}",
        res.total.value
    );

    // Same query, ample budget: bounding the cross-product must not cost
    // correctness — every document really does hold every token.
    let generous = parse_request(&json!({
        "query": { "query_string": { "query": format!("\"{body}") } },
        "size": 10,
        "timeout": "60s",
    }))
    .expect("parse_request");
    let full = idx.search(&generous).await.unwrap();
    assert!(!full.timed_out, "60s is ample for this corpus");
    assert_eq!(
        full.total.value, DOCS as u64,
        "the capped path must still find every matching document"
    );
}

/// The cap must not change results for ordinary (within-cap) queries.
#[tokio::test]
async fn field_less_query_string_within_the_cap_is_unchanged() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("small", Schema::empty()).unwrap();
    let idx = engine.get_index("small").unwrap();

    idx.index_document(
        Some("a".into()),
        json!({"title": "alpha", "body": "needle"}),
    )
    .await
    .unwrap();
    idx.index_document(Some("b".into()), json!({"title": "needle", "body": "beta"}))
        .await
        .unwrap();
    idx.index_document(Some("c".into()), json!({"title": "gamma", "body": "delta"}))
        .await
        .unwrap();
    idx.flush().await.unwrap();

    let r = idx.search(&search(opaque_qs("needle"))).await.unwrap();
    assert_eq!(
        r.total.value, 2,
        "`needle` is in `body` of one doc and `title` of another"
    );
}
