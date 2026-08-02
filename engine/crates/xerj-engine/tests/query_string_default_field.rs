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
use xerj_query::ast::{QueryNode, SearchRequest};
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

/// Floor cost of `req` against `idx`, query cache cleared before every run.
///
/// `query_cache` is keyed on `(query_body_hash, dataset_version)` and holds
/// the finished `SearchResult`, so re-running an identical request otherwise
/// measures a DashMap lookup and nothing else. Without the clear an earlier
/// version of this measurement reported ~5 µs for every shape on BOTH sides of
/// the fix — one real execution followed by four cache hits.
///
/// Best-of-5: the floor is the shape's real cost, and a scheduler blip can
/// only ever make a run look SLOWER, never faster.
async fn floor_ns(idx: &std::sync::Arc<xerj_engine::Index>, req: &SearchRequest) -> (u128, u64) {
    let warm = idx.search(req).await.unwrap();
    let mut best = u128::MAX;
    for _ in 0..5 {
        idx.query_cache.clear();
        let t = std::time::Instant::now();
        let r = idx.search(req).await.unwrap();
        best = best.min(t.elapsed().as_nanos());
        assert_eq!(r.total.value, warm.total.value, "runs must agree");
    }
    (best, warm.total.value)
}

/// A `query_string` whose text the standard analyzer reduces to NOTHING —
/// pure punctuation — can match no document, on any path: the projection is a
/// disjunction with zero clauses and the stored-doc scan's `QueryString` arm
/// returns `false` the instant its token set is empty.
///
/// Routing that shape to the stored-doc scan (because "the projection
/// declined") made the query that matches nothing pay for a full O(corpus)
/// walk.
///
/// The comparator is a query on the SAME index that provably DOES take that
/// walk — `match` with `operator: and`, which `is_doc_scan_query` routes to
/// the stored section — so the assertion is machine-relative and needs no
/// absolute budget. `match_all` is deliberately NOT the comparator: it has its
/// own bounded-scan fast path and is cheaper than one full walk, which is what
/// made an earlier version of this test fail against the FIXED code.
///
/// MEASURED, release, 20 000 documents in ONE segment, best of 5, query cache
/// cleared before every run — see the commit message for the numbers.
#[tokio::test]
async fn zero_token_query_string_does_not_scan_the_corpus() {
    // `+++` survives the Lucene lowerer as an opaque `QueryString` (the
    // unterminated quote is what stops it becoming a `Match`/`Bool` tree), and
    // the standard analyzer reduces it to zero tokens. Pin the shape: if the
    // parser ever lowers it, this test stops covering the arm it names.
    let zero_token = search(opaque_qs("+++"));
    assert!(
        matches!(zero_token.query, QueryNode::QueryString { .. }),
        "the test needs an opaque QueryString node; got {:?}",
        zero_token.query
    );
    // Known stored-doc scan over the same corpus: per-token AND semantics
    // exist only on the doc-scan path, so `is_doc_scan_query` sends it there.
    let known_scan = search(json!({
        "match": { "alpha": { "query": "body text", "operator": "and" } }
    }));

    let dir = TempDir::new().unwrap();
    let engine = make_single_segment_engine(&dir);
    engine.create_index("punct", Schema::empty()).unwrap();
    let idx = engine.get_index("punct").unwrap();

    const DOCS: usize = 20_000;
    for d in 0..DOCS {
        idx.index_document(
            Some(format!("d{d}")),
            json!({ "alpha": format!("body text {d}"), "beta": "more words here" }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    assert_eq!(
        idx.stats().await.segment_count,
        1,
        "one segment, so the comparator walks exactly the documents the \
         zero-token query would have walked"
    );

    let (zero_ns, zero_total) = floor_ns(&idx, &zero_token).await;
    let (scan_ns, scan_total) = floor_ns(&idx, &known_scan).await;

    assert_eq!(zero_total, 0, "zero tokens can match nothing");
    assert_eq!(
        scan_total, DOCS as u64,
        "the comparator must really walk the corpus"
    );

    eprintln!(
        "zero-token query_string: {:.3}ms   full stored scan: {:.3}ms   ({DOCS} docs, 1 segment)",
        zero_ns as f64 / 1e6,
        scan_ns as f64 / 1e6
    );

    // An order of magnitude of headroom on this machine. Demanding only 4×
    // keeps the guard meaningful on a runner where the fixed per-request
    // overhead is a larger share of the total.
    assert!(
        zero_ns * 4 < scan_ns,
        "a `query_string` that matches NOTHING cost {:.3}ms against {:.3}ms \
         for a query that really does walk all {DOCS} documents — it is doing \
         the full stored-doc scan instead of returning empty",
        zero_ns as f64 / 1e6,
        scan_ns as f64 / 1e6
    );
}

/// The zero-token split must not weaken the cross-product fallback: a
/// `query_string` that declines for the OTHER reason — too many
/// `tokens × fields` clauses — still has to be answered by the stored-doc
/// scan, or every one of its matches disappears.
#[tokio::test]
async fn over_cap_query_string_still_reaches_the_stored_doc_scan() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("overcap", Schema::empty()).unwrap();
    let idx = engine.get_index("overcap").unwrap();

    // 40 fields × 200 tokens = 8 000 clauses, past MAX_QS_CROSS_PRODUCT
    // (4 096), so the projection returns `None` — the same `None` the
    // zero-token shape returns, and the one that still needs the scan.
    const FIELDS: usize = 40;
    const TOKENS: usize = 200;
    const DOCS: usize = 12;
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

    let r = idx.search(&search(opaque_qs(body.trim()))).await.unwrap();
    assert_eq!(
        r.total.value,
        DOCS as u64,
        "an over-cap `query_string` declines the postings path and MUST fall \
         through to the stored-doc scan; {} of {DOCS} documents were dropped",
        DOCS as u64 - r.total.value
    );
}

/// Undeclared response-value change, now pinned.
///
/// `prune_missing_should_fields` fires for ANY should-only bool of plain
/// terms, which includes an ordinary two-clause `match` disjunction — a shape
/// the field-less `query_string` work never mentions. On a heterogeneous
/// corpus that moved `max_score` (2.0986123 -> 1.3996884 as first measured);
/// `hits.total` and every per-hit `_score` were byte-identical.
///
/// The new value is the correct one, and this test is the definition rather
/// than a pinned constant: ES documents `max_score` as the highest `_score`
/// among the matching documents, so a `max_score` no returned hit can reach is
/// wrong by construction. Pre-change it came from `scored_max_bits` — the
/// brute stored-doc scorer's population max — while the hits themselves were
/// scored by the FTS postings path. Two scorers, two scales, and `max_score`
/// took the larger: the field-lacking segment was rejected by the
/// all-fields-present gate and re-scored by the brute scan, which is exactly
/// what the prune stops happening.
#[tokio::test]
async fn should_only_bool_max_score_never_exceeds_the_top_hit() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("scores", Schema::empty()).unwrap();
    let idx = engine.get_index("scores").unwrap();

    // Heterogeneous by construction: the first four documents are flushed
    // before `gamma` exists, so that segment has no `gamma` FTS side-car and
    // the disjunction's `gamma` clause is the one the prune drops.
    for i in 0..4 {
        idx.index_document(
            Some(format!("old{i}")),
            json!({ "alpha": format!("apple pie {i}") }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    for i in 0..3 {
        idx.index_document(
            Some(format!("new{i}")),
            json!({ "alpha": "apple tart", "gamma": format!("apple sauce {i}") }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();

    let req = search(json!({
        "bool": { "should": [
            { "match": { "alpha": "apple" } },
            { "match": { "gamma": "apple" } },
        ]}
    }));
    let r = idx.search(&req).await.unwrap();

    assert_eq!(r.total.value, 7, "every document holds `apple` somewhere");
    assert_eq!(r.hits.len(), 7, "size:100 — every hit is on the page");

    let top = r
        .hits
        .iter()
        .map(|h| h.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_score = r.max_score.expect("a scored query reports max_score");
    let mut scores: Vec<f32> = r.hits.iter().map(|h| h.score).collect();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
    eprintln!(
        "total={} max_score={max_score} top_hit={top} scores={scores:?}",
        r.total.value
    );
    assert!(
        (max_score - top).abs() <= f32::EPSILON * top.abs().max(1.0),
        "max_score {max_score} is not the highest returned _score {top} — ES \
         defines max_score as the maximum `_score` of the matching documents, \
         so a value no hit can reach is unreachable by definition"
    );
    // Guard the other direction too: a max_score BELOW the top hit would be
    // just as wrong, and would be the natural regression if the population
    // max were ever narrowed to a page.
    assert!(max_score >= top, "max_score {max_score} < top hit {top}");
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
