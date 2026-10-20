//! `exists` inside a filter-context `bool` conjunction must answer from the
//! columnar filter executor — not by source-scanning every row.
//!
//! #1183's finalize-verify issues one `bool: [term: ax_file, exists: <field>]`
//! count per changed group. In scoring context (`must`) that query fell to
//! the brute path and source-scanned the whole index per call: measured
//! took=9638ms for 0 hits on a 91k-doc segment of the standing xerj-search
//! rebuild. The fix lowers filter-context `exists` to the columnar path —
//! keyword fields via the empty-prefix dictionary range, numeric/boolean via
//! the unbounded window, and column-less fields (text / semantic_text) via a
//! source-backed leaf that runs AFTER the cheap conjuncts in the same row
//! walk (so the term narrows first).
//!
//! Every count below is asserted against a hand-computed expectation from
//! the seed (not against the brute path), and the scoring-context `must`
//! variant — still served by the brute path — must report the SAME total:
//! cross-path agreement is the regression guard for drift between the
//! columnar leaf and `doc_matches_query`'s `exists` arm.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// 90 docs over three `ax_file` groups, exercising every `exists` edge in
/// `value_present`: absent key, explicit null, empty string (PRESENT), and
/// an array with a non-null element (PRESENT).  `note` is a second
/// column-less (text) field with a different presence pattern so a
/// conjunction can discriminate; `tag` (keyword) and `num` (long) carry
/// nulls to pin the column-backed lowers.
///
/// Flushed: the columnar executor serves fully-flushed data only — an
/// unflushed population is covered by `memtable_resident_docs_match`.
async fn seed(name: &str) -> (TempDir, std::sync::Arc<Index>) {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("ax_file", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("note", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    schema.fields.push(FieldConfig::new("num", FieldType::Long));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();

    let doc = |id: &str,
               body: Option<Value>,
               note: Option<Value>,
               tag: Option<&str>,
               num: Option<i64>| {
        let mut v = json!({ "ax_file": if id < "d030" { "f1" } else if id < "d064" { "f2" } else { "f3" } });
        if let Some(b) = body {
            v["body"] = b;
        }
        if let Some(n) = note {
            v["note"] = n;
        }
        if let Some(t) = tag {
            v["tag"] = json!(t);
        }
        if let Some(n) = num {
            v["num"] = json!(n);
        }
        v
    };

    // f1: d000..d029 — body everywhere, note nowhere; tag null on odd,
    // num null on every third doc.
    for i in 0..30 {
        idx.index_document(
            Some(format!("d{i:03}")),
            doc(
                &format!("d{i:03}"),
                Some(json!(format!("body text {i}"))),
                None,
                if i % 2 == 0 { Some("a") } else { None },
                if i % 3 == 0 { None } else { Some(i as i64) },
            ),
        )
        .await
        .unwrap();
    }
    // f2: d030..d063 — the edge-case population.
    for i in 30..45 {
        idx.index_document(
            Some(format!("d{i:03}")),
            doc(
                &format!("d{i:03}"),
                Some(json!(format!("body text {i}"))),
                Some(json!(format!("note {i}"))),
                None,
                None,
            ),
        )
        .await
        .unwrap();
    }
    for i in 45..60 {
        idx.index_document(
            Some(format!("d{i:03}")),
            doc(
                &format!("d{i:03}"),
                None,
                Some(json!(format!("note {i}"))),
                None,
                None,
            ),
        )
        .await
        .unwrap();
    }
    idx.index_document(
        Some("d060".to_string()),
        doc("d060", Some(json!("")), None, None, None),
    )
    .await
    .unwrap(); // empty string: PRESENT
    idx.index_document(
        Some("d061".to_string()),
        doc(
            "d061",
            Some(Value::Null),
            Some(json!("note 61")),
            None,
            None,
        ),
    )
    .await
    .unwrap(); // explicit null: ABSENT
    idx.index_document(
        Some("d062".to_string()),
        doc("d062", None, None, None, None),
    )
    .await
    .unwrap(); // key omitted: ABSENT
    idx.index_document(
        Some("d063".to_string()),
        doc("d063", Some(json!(["array body"])), None, None, None),
    )
    .await
    .unwrap(); // array w/ non-null element: PRESENT
               // f3: d064..d089 — no body, note on the first ten only.
    for i in 64..90 {
        idx.index_document(
            Some(format!("d{i:03}")),
            doc(
                &format!("d{i:03}"),
                None,
                if i < 74 {
                    Some(json!(format!("note {i}")))
                } else {
                    None
                },
                None,
                None,
            ),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    (dir, idx)
}

async fn total(idx: &Index, query: Value, size: usize) -> u64 {
    let req = parse_request(&json!({"query": query, "size": size, "track_total_hits": true}))
        .expect("parse_request");
    let res = idx.search(&req).await.expect("search");
    res.total.value
}

/// The production shape (#1183 finalize-verify): term + exists on a
/// column-less text field, filter context, count-only.
#[tokio::test]
async fn term_and_text_exists_count_matches_expectation() {
    let (_dir, idx) = seed("exists-term-text").await;
    // f2 body-present: d030..d044 (15) + d060 (empty string) + d063 (array)
    // = 17; the null (d061) and omitted (d062) bodies count ABSENT.
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f2"}},
                {"exists": {"field": "body"}}
            ]}}),
            0
        )
        .await,
        17
    );
    // Cross-path agreement: the scoring-context shape still rides the
    // brute path and must report the same total.
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"must": [
                {"term": {"ax_file": "f2"}},
                {"exists": {"field": "body"}}
            ]}}),
            0
        )
        .await,
        17
    );
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f1"}},
                {"exists": {"field": "body"}}
            ]}}),
            0
        )
        .await,
        30
    );
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f3"}},
                {"exists": {"field": "body"}}
            ]}}),
            0
        )
        .await,
        0
    );
}

/// A second column-less field with a different presence pattern: the leaf
/// must consult the right field, and `must_not exists` must invert exactly.
#[tokio::test]
async fn note_exists_and_must_not_exist_count() {
    let (_dir, idx) = seed("exists-note").await;
    // f2 note-present: d030..d059 (30) + d061 = 31 of 34.
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f2"}},
                {"exists": {"field": "note"}}
            ]}}),
            0
        )
        .await,
        31
    );
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
            {"term": {"ax_file": "f2"}},
        ], "must_not": [{"exists": {"field": "note"}}]}}),
            0
        )
        .await,
        3
    );
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f3"}},
                {"exists": {"field": "note"}}
            ]}}),
            0
        )
        .await,
        10
    );
}

/// Column-backed lowers: `exists` on keyword and long fields with nulls.
#[tokio::test]
async fn keyword_and_numeric_exists_in_conjunction() {
    let (_dir, idx) = seed("exists-columns").await;
    // f1 tag present on even ids 0..=28 → 15.
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f1"}},
                {"exists": {"field": "tag"}}
            ]}}),
            0
        )
        .await,
        15
    );
    // f1 num present on every non-multiple of 3 → 20.
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "f1"}},
                {"exists": {"field": "num"}}
            ]}}),
            0
        )
        .await,
        20
    );
}

/// size>0 must return the right ids at score 0.0 (ES filter context).
#[tokio::test]
async fn paged_hits_are_the_matching_ids_at_zero_score() {
    let (_dir, idx) = seed("exists-page").await;
    let req = parse_request(&json!({
        "query": {"bool": {"filter": [
            {"term": {"ax_file": "f2"}},
            {"exists": {"field": "body"}}
        ]}},
        "size": 100,
        "track_total_hits": true
    }))
    .expect("parse_request");
    let res = idx.search(&req).await.unwrap();
    assert_eq!(res.total.value, 17);
    let mut ids: Vec<String> = res.hits.iter().map(|h| h.id.clone()).collect();
    ids.sort_unstable();
    let mut expected: Vec<String> = (30..45).map(|i| format!("d{i:03}")).collect();
    expected.push("d060".to_string());
    expected.push("d063".to_string());
    expected.sort_unstable();
    assert_eq!(ids, expected);
    assert!(
        res.hits.iter().all(|h| h.score == 0.0),
        "filter-context scores must be 0.0, got {:?}",
        res.hits.iter().map(|h| h.score).collect::<Vec<_>>()
    );
}

/// The columnar executor bails on memtable-resident data by design; the
/// brute path answers instead and must agree on the same edges.
#[tokio::test]
async fn memtable_resident_docs_match() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("ax_file", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("exists-memtable", schema).unwrap();
    let idx = engine.get_index("exists-memtable").unwrap();
    idx.index_document(
        Some("m1".to_string()),
        json!({"ax_file": "g", "body": "present"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("m2".to_string()),
        json!({"ax_file": "g", "body": Value::Null}),
    )
    .await
    .unwrap();
    idx.index_document(Some("m3".to_string()), json!({"ax_file": "g"}))
        .await
        .unwrap();
    // No flush: memtable-resident.
    assert_eq!(
        total(
            &idx,
            json!({"bool": {"filter": [
                {"term": {"ax_file": "g"}},
                {"exists": {"field": "body"}}
            ]}}),
            0
        )
        .await,
        1
    );
}
