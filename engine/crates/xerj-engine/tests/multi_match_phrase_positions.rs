//! Regression tests for issue #230: `multi_match` with `type: phrase` /
//! `phrase_prefix` must evaluate REAL positional phrase semantics — term
//! positions in the analyzed token stream — not whole-query lowercase
//! substring containment on the raw field text.
//!
//! Pre-fix both the memtable arm of `doc_matches_query` and the scoring
//! arm of `score_query_against_doc` tested `field_text.contains(query)`,
//! and the segment FTS projection DECLINED phrase types outright so the
//! stored-doc scan evaluated the same substring predicate. Substring
//! containment diverges from ES in both directions:
//!
//! * under-match — the analyzer strips punctuation, so ES's positional
//!   phrase matches `"merge policy"` against `merge, policy`; the raw
//!   text has no `"merge policy"` substring, so XERJ returned 0 hits;
//! * over-match — substring containment ignores token boundaries, so
//!   `"merge polic"` matched the doc `merge policy`, where ES's phrase
//!   terms (`merge`, `polic`) never line up;
//! * `slop` was parsed away entirely, so `{"type":"phrase","slop":2}`
//!   silently behaved as slop 0.
//!
//! Every case is asserted BOTH pre-flush (memtable) and post-flush
//! (segment), and cross-checked against the single-field `match_phrase` /
//! `match_phrase_prefix` queries, which have always been positional — ES
//! lowers `multi_match` phrase types to exactly a dis_max over per-field
//! `match_phrase` clauses.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::{Engine, Index};
use xerj_query::ast::QueryNode;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn req(q: Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request")
}

async fn ids(idx: &Index, q: &Value) -> BTreeSet<String> {
    idx.search(&req(q.clone()))
        .await
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id.clone())
        .collect()
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Run every case against the memtable, flush once, run every case again
/// against the segment, and assert the hit set is the expected one in both
/// states — semantics AND flush-invariance in one assertion.
async fn assert_both_states(idx: &std::sync::Arc<Index>, cases: &[(Value, &[&str], &str)]) {
    for (q, exp, label) in cases {
        let pre = ids(idx, q).await;
        assert_eq!(
            pre,
            set(exp),
            "{label}: PRE-flush (memtable) hit set for {q}"
        );
    }
    idx.flush().await.unwrap();
    for (q, exp, label) in cases {
        let post = ids(idx, q).await;
        assert_eq!(
            post,
            set(exp),
            "{label}: POST-flush (segment) hit set for {q}"
        );
    }
}

/// Doc 1 carries punctuation INSIDE the phrase (`merge, policy`) — the
/// standard analyzer drops it, so the phrase terms are adjacent even
/// though the raw text has no `"merge policy"` substring.
/// Doc 2 carries the bare phrase, so it exercises the over-match direction.
/// Doc 3 is a control that must never match.
async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    idx.index_document(
        Some("1".into()),
        json!({
            "body": "the log merge, policy groups segments into buckets",
            "title": "log structured merge"
        }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({"body": "a tiered merge policy compacts segments", "title": "merge policy"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("3".into()),
        json!({"body": "quick brown fox", "title": "animals"}),
    )
    .await
    .unwrap();
    idx
}

/// UNDER-MATCH: the analyzer strips the comma, so ES's positional phrase
/// matches doc 1. Pre-fix the substring test missed it in both states.
/// `match_phrase` on the same field is the in-repo oracle: it has always
/// been positional, and ES lowers `multi_match` phrase to exactly that.
#[tokio::test]
async fn phrase_matches_across_stripped_punctuation() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_punct").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"match_phrase": {"body": "merge policy"}}),
                &["1", "2"],
                "oracle match_phrase",
            ),
            (
                json!({"multi_match": {"query": "merge policy", "fields": ["body"],
                                       "type": "phrase"}}),
                &["1", "2"],
                "multi_match phrase, single field",
            ),
            (
                json!({"multi_match": {"query": "merge policy", "fields": ["body", "title"],
                                       "type": "phrase"}}),
                &["1", "2"],
                "multi_match phrase, two fields",
            ),
        ],
    )
    .await;
}

/// OVER-MATCH: `"merge polic"` is a substring of `merge policy` but the
/// analyzed terms never line up, so ES matches nothing. Pre-fix XERJ
/// returned doc 2 (and doc 1 for the `body` variant is impossible either
/// way — its raw text has no such substring).
#[tokio::test]
async fn phrase_does_not_match_partial_trailing_token() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_partial").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"match_phrase": {"title": "merge polic"}}),
                &[],
                "oracle match_phrase partial token",
            ),
            (
                json!({"multi_match": {"query": "merge polic", "fields": ["title"],
                                       "type": "phrase"}}),
                &[],
                "multi_match phrase partial token",
            ),
            // Reversed phrase must not match either (positional order).
            (
                json!({"multi_match": {"query": "policy merge", "fields": ["title", "body"],
                                       "type": "phrase"}}),
                &[],
                "multi_match phrase reversed",
            ),
        ],
    )
    .await;
}

/// `operator` is meaningless for a phrase in ES — its phrase parser never
/// consults it. Pre-fix `{"type":"phrase","operator":"and"}` fell into the
/// memtable's token-AND branch (tested before the phrase branch) and
/// silently stopped being a phrase: the reversed, non-adjacent query below
/// matched because both tokens were merely present in the same field.
#[tokio::test]
async fn operator_does_not_override_phrase_semantics() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_operator").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "policy merge", "fields": ["title", "body"],
                                       "type": "phrase", "operator": "and"}}),
                &[],
                "phrase + operator:and stays a phrase (reversed → no hit)",
            ),
            (
                json!({"multi_match": {"query": "merge policy", "fields": ["body"],
                                       "type": "phrase", "operator": "and"}}),
                &["1", "2"],
                "phrase + operator:and stays a phrase (in order → hit)",
            ),
        ],
    )
    .await;
}

/// `phrase_prefix`: head terms form an ordered phrase, the LAST term is a
/// prefix over the analyzed term dictionary. Pre-fix the memtable arm used
/// substring containment, so the punctuated doc was missed.
#[tokio::test]
async fn phrase_prefix_is_positional() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_prefix").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "merge poli", "fields": ["body"],
                                       "type": "phrase_prefix"}}),
                &["1", "2"],
                "multi_match phrase_prefix across punctuation",
            ),
            (
                json!({"match_phrase_prefix": {"body": "merge poli"}}),
                &["1", "2"],
                "oracle match_phrase_prefix across punctuation",
            ),
            // The head phrase still has to be an ordered adjacent phrase.
            (
                json!({"multi_match": {"query": "policy mer", "fields": ["body", "title"],
                                       "type": "phrase_prefix"}}),
                &[],
                "multi_match phrase_prefix reversed head",
            ),
        ],
    )
    .await;
}

/// `slop` was parsed away by `parse_multi_match`, so a sloppy phrase
/// silently behaved as an exact one. Doc 1's `body` analyses to
/// [the, log, merge, policy, …]: `"log policy"` needs slop >= 1.
#[tokio::test]
async fn phrase_slop_is_honoured() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_slop").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "log policy", "fields": ["body"],
                                       "type": "phrase"}}),
                &[],
                "slop 0 (default): one intervening token → no match",
            ),
            (
                json!({"multi_match": {"query": "log policy", "fields": ["body"],
                                       "type": "phrase", "slop": 1}}),
                &["1"],
                "slop 1: one intervening token → match",
            ),
            (
                json!({"multi_match": {"query": "log policy", "fields": ["body"],
                                       "type": "phrase", "slop": 5}}),
                &["1"],
                "slop 5: match",
            ),
        ],
    )
    .await;
}

/// The former in-order-only divergence pin, FLIPPED by the #830 fix — its
/// own doc said «when the walk learns transpositions, both evaluators
/// change together and this test flips».  XERJ's sloppy phrase now follows
/// Lucene `SloppyPhraseMatcher` move-distance: `SloppyPhraseMatcher`'s
/// class javadoc — «for query "a b"~2, a document "x a b a y" can be
/// matched twice: once for "a b" (distance=0), and once for "b a"
/// (distance=2)» — so ES answers `{"query": "policy merge", "slop": 2}`
/// with docs 1 and 2 (both read `merge policy`), and XERJ now gives the
/// same answer, at every entry point and in both states.  The dedicated
/// slop-1 negative cases live in `match_phrase_slop_transposition.rs`.
#[tokio::test]
async fn sloppy_phrase_admits_transpositions_like_lucene() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_inorder").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "policy merge", "fields": ["body"],
                                       "type": "phrase", "slop": 2}}),
                &["1", "2"],
                "transposed phrase, slop 2: matches like ES (cost 2)",
            ),
            (
                json!({"match_phrase": {"body": {"query": "policy merge", "slop": 3}}}),
                &["1", "2"],
                "transposed phrase, slop 3, single-field spelling: same answer",
            ),
            // The in-order control at the same slop, so the cases above
            // cannot pass by slop being ignored altogether.
            (
                json!({"multi_match": {"query": "log policy", "fields": ["body"],
                                       "type": "phrase", "slop": 2}}),
                &["1"],
                "in-order control at the same slop: matches",
            ),
        ],
    )
    .await;
}

/// A memtable phrase hit must also SCORE above zero — `score_query_against_doc`
/// carried its own copy of the substring predicate, so a doc admitted by the
/// (fixed) membership test would still score 0.0 and be dropped by scored paths.
#[tokio::test]
async fn memtable_phrase_hit_scores_nonzero() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_score").await;

    let r = idx
        .search(&req(json!({
            "multi_match": {"query": "merge policy", "fields": ["body", "title"],
                            "type": "phrase"}
        })))
        .await
        .unwrap();
    assert_eq!(r.total.value, 2, "memtable phrase hit count");
    for h in &r.hits {
        assert!(
            h.score > 0.0,
            "memtable multi_match phrase hit {} scored {} (expected > 0)",
            h.id,
            h.score
        );
    }
}

/// `slop` on a `phrase_prefix` is HONOURED, at both entry points and in
/// both states. ES honours it (`TextFieldMapper.createPhrasePrefixQuery`
/// builds a `MultiPhrasePrefixQuery` and calls `setSlop`), so refusing it
/// would break clients, and dropping it — which `parse_match_phrase_prefix`
/// did — is the accept-and-ignore defect class of #204: the `multi_match`
/// and `match_phrase_prefix` spellings of the same query must not disagree.
///
/// Doc 1's `body` analyses to [the, log, merge, policy, …]: the phrase
/// `log poli*` needs one intervening position, so slop 0 misses and slop 1
/// hits.
#[tokio::test]
async fn phrase_prefix_slop_is_honoured_at_both_entry_points() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmpp_prefix_slop").await;

    assert_both_states(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "log poli", "fields": ["body"],
                                       "type": "phrase_prefix"}}),
                &[],
                "phrase_prefix slop 0 (default): one intervening token → no match",
            ),
            (
                json!({"multi_match": {"query": "log poli", "fields": ["body"],
                                       "type": "phrase_prefix", "slop": 1}}),
                &["1"],
                "phrase_prefix slop 1: one intervening token → match",
            ),
            (
                json!({"match_phrase_prefix": {"body": {"query": "log poli", "slop": 1}}}),
                &["1"],
                "oracle match_phrase_prefix slop 1 answers the same query",
            ),
            (
                json!({"match_phrase_prefix": {"body": {"query": "log poli"}}}),
                &[],
                "oracle match_phrase_prefix slop 0",
            ),
        ],
    )
    .await;
}

/// A negative `slop` is a client error in ES from all three *phrase* builders
/// (`MatchPhraseQueryBuilder` / `MatchPhrasePrefixQueryBuilder:103` /
/// `MultiMatchQueryBuilder:350`, all "No negative slop allowed"), so all three
/// must answer alike here — a 400 from one spelling and a silently coerced 0
/// from the other is the same accept-and-ignore defect wearing a different hat.
///
/// SCOPE, stated so the name cannot be read as more than it is: these three
/// query types are the whole of it. `span_near` also takes a `slop` and is
/// **not** covered — it keeps `as_u64().unwrap_or(0)`, so `span_near` with
/// `slop: -1` returns 200 with the value silently 0. That is a pre-existing
/// gap, recorded at `parse_span_near`, and it is left alone because ES applies
/// no validation to `span_near.slop` either
/// (`SpanNearQueryBuilder.java:62`) — rejecting it would break a query ES
/// answers.
#[test]
fn negative_slop_is_rejected_at_the_three_phrase_entry_points() {
    for bad in [
        json!({"multi_match": {"query": "merge policy", "fields": ["body"],
                               "type": "phrase", "slop": -1}}),
        json!({"match_phrase": {"body": {"query": "merge policy", "slop": -1}}}),
        json!({"match_phrase_prefix": {"body": {"query": "merge poli", "slop": -1}}}),
    ] {
        let err = parse_request(&json!({"query": bad}))
            .expect_err("negative slop must be rejected, not coerced to 0");
        assert!(
            err.to_string().contains("slop"),
            "error should name the offending parameter, got: {err}"
        );
    }
}

/// A float-encoded `slop` (`2.0`) must be honoured as 2, not refused and not
/// dropped. ES reads slop with `XContentParser.intValue()` under its default
/// coercing policy, which truncates a float token and narrows a numeric string
/// (`AbstractXContentParser.java:171` `intValue(coerce)` → `:162` `parseInt`,
/// with the truncation note at `:70`), so `{"slop": 2.0}` is a
/// query ES answers with slop 2.
///
/// This test exists because an earlier revision of this branch tightened the
/// parse to `as_i64()` and turned `2.0` into a 400 — a new refusal on a
/// request `main` answered, which is exactly the wire break this PR declined
/// to make elsewhere. The two wrong answers are symmetrical: refusing the
/// value, or silently substituting 0 as `main` did.
#[test]
fn float_and_string_encoded_slop_are_coerced_like_es() {
    for (body, want) in [
        (json!({"query": "merge policy", "slop": 2.0}), 2u32),
        (json!({"query": "merge policy", "slop": 2.7}), 2),
        (json!({"query": "merge policy", "slop": "2"}), 2),
    ] {
        let q = json!({"match_phrase": {"body": body.clone()}});
        let parsed = parse_request(&json!({ "query": q }))
            .unwrap_or_else(|e| panic!("ES coerces this slop, so it must parse: {body} -> {e}"));
        match parsed.query {
            QueryNode::MatchPhrase { slop, .. } => {
                assert_eq!(slop, want, "expected slop {want} from {body}, got {slop}")
            }
            other => panic!("expected a MatchPhrase node, got {other:?}"),
        }
    }

    // The same coercion, and the same refusal to ignore, on `max_expansions`:
    // a float is read, and a value that cannot be read is a 400 rather than
    // a silent fall back to 50 (which is what `main` did here).
    let parsed = parse_request(&json!({"query": {"match_phrase_prefix": {
        "body": {"query": "merge poli", "max_expansions": 7.0}}}}))
    .expect("float max_expansions is coerced, not refused");
    match parsed.query {
        QueryNode::MatchPhrasePrefix { max_expansions, .. } => assert_eq!(max_expansions, 7),
        other => panic!("expected a MatchPhrasePrefix node, got {other:?}"),
    }
    for bad in [
        json!({"match_phrase_prefix": {"body": {"query": "merge poli", "max_expansions": -1}}}),
        json!({"match_phrase_prefix": {"body": {"query": "merge poli",
                                                "max_expansions": "seven"}}}),
    ] {
        parse_request(&json!({ "query": bad.clone() })).unwrap_err();
    }
}

/// The stored-scan/memtable evaluator and the positional segment clause
/// must tokenize with the SAME analyzer, or they answer different queries
/// and the hit set changes at `_flush` — the regression class #218/#222
/// removed and that #230 names as the standing invariant.
///
/// An earlier revision of this fix tokenized the memtable side by splitting
/// on `!char::is_alphanumeric()` while the segment clause was built from the
/// `standard` analyzer (UAX#29 `unicode_words()`). The two disagree on
/// intra-word `.`, `'` and `_`: `3.14`, `don't` and `foo_bar` are ONE
/// analyzed term each and two-or-three split ones. Each case below returned
/// a hit pre-flush and none post-flush at that revision; the analyzed answer
/// (no hit — the terms never line up) is also the one ES gives.
#[tokio::test]
async fn phrase_tokenizer_matches_the_segment_analyzer() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mmpp_tok", Schema::empty()).unwrap();
    let idx = engine.get_index("mmpp_tok").unwrap();
    for (id, body) in [
        ("n1", "release 3.14 notes here"),
        ("d1", "we don't stop now"),
        ("u1", "the foo_bar baz"),
    ] {
        idx.index_document(Some(id.into()), json!({ "body": body }))
            .await
            .unwrap();
    }

    assert_both_states(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "release 3", "fields": ["body"],
                                       "type": "phrase"}}),
                &[],
                "`3.14` is ONE analyzed term, so `release 3` is not a phrase in it",
            ),
            (
                json!({"match_phrase": {"body": "release 3"}}),
                &[],
                "oracle match_phrase agrees",
            ),
            (
                json!({"multi_match": {"query": "don t", "fields": ["body"],
                                       "type": "phrase"}}),
                &[],
                "`don't` is ONE analyzed term, so `don t` is not a phrase in it",
            ),
            (
                json!({"multi_match": {"query": "foo bar", "fields": ["body"],
                                       "type": "phrase"}}),
                &[],
                "`foo_bar` is ONE analyzed term, so `foo bar` is not a phrase in it",
            ),
            // The whole analyzed term still matches, in both states.
            (
                json!({"multi_match": {"query": "release 3.14", "fields": ["body"],
                                       "type": "phrase"}}),
                &["n1"],
                "the analyzed term itself matches",
            ),
            (
                json!({"multi_match": {"query": "we don't", "fields": ["body"],
                                       "type": "phrase"}}),
                &["d1"],
                "the analyzed term itself matches (apostrophe)",
            ),
        ],
    )
    .await;
}

/// `max_expansions` is why `phrase_prefix` keeps the declined/stored-scan
/// routing on `multi_match`: the segment clause bounds the trailing prefix
/// to `max_expansions` terms from the field's term dictionary, and the
/// stored-doc walk — which has no term dictionary — cannot. Projecting it
/// made the hit set shrink at `_flush`.
///
/// This asserts the INVARIANT (same answer either side of the flush), not
/// ES parity: XERJ does not bind `max_expansions` on `multi_match`, so both
/// states return the unbounded answer. Stated plainly rather than papered
/// over — flush invariance is the property #230 asks for.
///
/// HONEST LIMIT of this test: it also passes against the revision that DID
/// project `phrase_prefix` (checked out and run — 7 passed, 3 failed, and
/// this was not one of the three), so it is an invariant assertion, not a
/// regression catcher. This in-process fixture does not reliably route the
/// query through the bounded segment clause the way a mapped index on a
/// live server does, which is where the review measured the shrink
/// (pre {p1,p2,p3,p4,s1} / post {p1,p2,p3,p4}). The discriminating guard
/// for the routing decision is the unit test
/// `index::fts_projection_tests::multi_match_phrase_projects_positional_dis_max`,
/// which asserts `query_node_to_fts` returns `None` for `phrase_prefix`.
#[tokio::test]
async fn phrase_prefix_max_expansions_does_not_change_the_hit_set_at_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mmpp_maxexp", Schema::empty()).unwrap();
    let idx = engine.get_index("mmpp_maxexp").unwrap();
    // `pol*` expands to two dictionary terms — `polar` sorts first, so a
    // max_expansions of 1 would keep only it on a bounded segment clause.
    for (id, body) in [
        ("p1", "a tiered merge policy compacts segments"),
        ("p2", "the merge polar route"),
    ] {
        idx.index_document(Some(id.into()), json!({ "body": body }))
            .await
            .unwrap();
    }

    assert_both_states(
        &idx,
        &[(
            json!({"multi_match": {"query": "merge pol", "fields": ["body"],
                                   "type": "phrase_prefix", "max_expansions": 1}}),
            &["p1", "p2"],
            "multi_match phrase_prefix hit set is the same before and after _flush",
        )],
    )
    .await;
}
