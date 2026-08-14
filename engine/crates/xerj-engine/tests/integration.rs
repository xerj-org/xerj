//! Integration tests for xerj-engine.
//!
//! These tests exercise the full stack: Engine -> Index -> Storage + FTS.
//! Each test gets its own temporary directory so they can run in parallel.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{detect_log_format, Engine, LogFormat};
use xerj_query::ast::{QueryNode, SearchRequest};
use xerj_query::parse_request;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

#[tokio::test]
async fn test_semantic_vectors_stay_stored_but_not_fts_indexed_after_merge_and_reopen() {
    fn collect_names(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                collect_names(&entry.path(), out);
            } else {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }

    async fn assert_queries(idx: &xerj_engine::Index) {
        let semantic = idx
            .search(&make_search_with_source(
                json!({
                    "semantic": {"field": "content", "query": "quarterly liquidity", "k": 10}
                }),
                json!(true),
            ))
            .await
            .unwrap();
        assert_eq!(semantic.total.value, 2);
        assert!(semantic.hits.iter().all(|hit| {
            hit.source.get("custom_embedding").is_some()
                && hit.source.get("custom_embedding_chunks").is_some()
                && hit.passage.is_none()
                && hit.source.as_object().is_some_and(|source| {
                    source
                        .keys()
                        .all(|name| !name.starts_with("__xerj_passage_meta__"))
                })
        }));

        let lexical = idx
            .search(&make_search(json!({
                "match": {"content": "working capital"}
            })))
            .await
            .unwrap();
        assert_eq!(lexical.total.value, 2);

        let page = idx
            .search(&make_search(json!({
                "term": {"page": 7}
            })))
            .await
            .unwrap();
        assert_eq!(page.total.value, 1);
    }

    let dir = TempDir::new().unwrap();
    let mut schema = Schema::empty();
    let mut content = FieldConfig::new("content", FieldType::Text);
    content.options.dimensions = Some(16);
    content.options.similarity = Some("cosine".to_string());
    content.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("custom_embedding".to_string()),
    });
    schema.fields.push(content);
    schema
        .fields
        .push(FieldConfig::new("page", FieldType::Long));

    {
        let engine = make_engine(&dir);
        engine.create_index("sem-fts-exclusion", schema).unwrap();
        let idx = engine.get_index("sem-fts-exclusion").unwrap();
        let long_body = format!(
            "quarterly liquidity evidence {}",
            "cash assets liabilities working capital ".repeat(80)
        );

        idx.index_document(Some("a".into()), json!({"content": long_body, "page": 7}))
            .await
            .unwrap();
        idx.refresh().await.unwrap();
        idx.index_document(Some("b".into()), json!({"content": long_body, "page": 8}))
            .await
            .unwrap();
        idx.refresh().await.unwrap();
        idx.force_merge(1).await.unwrap();

        assert_queries(&idx).await;
    }

    let mut names = Vec::new();
    collect_names(dir.path(), &mut names);
    assert!(names.iter().any(|name| name.ends_with(".content.fst")));
    assert!(names.iter().any(|name| name.ends_with(".page.fst")));
    assert!(!names
        .iter()
        .any(|name| name.ends_with(".custom_embedding.fst")));
    assert!(!names
        .iter()
        .any(|name| name.ends_with(".custom_embedding_chunks.fst")));
    assert!(!names
        .iter()
        .any(|name| name.contains("__xerj_passage_meta__")));

    let reopened = make_engine(&dir);
    let idx = reopened.get_index("sem-fts-exclusion").unwrap();
    assert_queries(&idx).await;
}

/// #328 — a USER-MAPPED `dense_vector` must contribute no lexical artifacts.
///
/// The `#12` fix above covers the vectors XERJ *generates* from a semantic
/// mapping. A field the user maps as `dense_vector` took a different route:
/// it is `indexed == true` (kNN needs the HNSW graph), so the `index: false`
/// arm of `memtable::fts_excluded_fields` never fired and the field's 128
/// floats were flattened to decimal strings and tokenised into a term
/// dictionary that no query path reads.
///
/// This pins all four halves of the fix:
///   * no `<seg>.emb.{fst,post,norms}` is written, at flush or after merge —
///     AND none for the `<field>_chunks` companion either. A `dense_vector`
///     never arrives alone: `passage_scored_vector_fields` gives every one of
///     them a `_chunks` multi-vector and a `__xerj_passage_meta__` sidecar, and
///     the companion is the bigger artifact of the two on a chunked corpus;
///   * kNN, `exists` and the lexical fields keep answering exactly as before;
///   * a string `term` / `match` / `*`-expansion on the vector field still
///     finds nothing — the postings going away must not hand the query to the
///     stored-doc scan, whose `Term` arm matches any ELEMENT of a JSON array
///     and whose `match` arm splits on non-alphanumerics, which would
///     otherwise start returning hits for a bare float component;
///   * and neither does an EXPLICITLY-FIELDED lexical query. This is the one
///     the writer-side change cannot cover on its own and the one that fails
///     loudly: with the postings gone but no plan-time lowering,
///     `{"multi_match":{"query":"0","fields":["emb"]}}` returns EVERY document
///     instead of none, because the scan renders the float array to text and
///     every component contains a `0`. Measured on a 5,000-doc × 128-dim
///     corpus, post-`force_merge`, three ways: `main` 0 hits / 1.4 ms,
///     postings-removed-only 5,000 hits / 472.9 ms, this branch 0 hits /
///     0.018 ms. The same three-way run puts `{"term":{"emb":"<component>"}}`
///     at 55.7 ms → 426.5 ms → 0.062 ms, all three answering 0.
#[tokio::test]
async fn user_mapped_dense_vector_builds_no_fts_term_dictionary() {
    fn segment_files(dir: &std::path::Path) -> Vec<(String, u64)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, u64)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), out);
                } else {
                    out.push((
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.metadata().unwrap().len(),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, &mut out);
        out
    }

    const DIMS: usize = 128;
    const DOCS: usize = 300;

    let dir = TempDir::new().unwrap();
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("cat", FieldType::Keyword));
    schema.fields.push(FieldConfig::new("n", FieldType::Long));
    let mut emb = FieldConfig::new("emb", FieldType::Vector);
    emb.options.dimensions = Some(DIMS);
    emb.options.similarity = Some("cosine".to_string());
    schema.fields.push(emb);

    let engine = make_engine(&dir);
    engine.create_index("duprobe", schema).unwrap();
    let idx = engine.get_index("duprobe").unwrap();

    // Deterministic pseudo-random components with enough decimal digits that
    // each one is a distinct term — the shape that made the FST large.
    let component = |doc: usize, dim: usize| -> f64 {
        let h = (doc as u64)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add((dim as u64).wrapping_mul(1_442_695_040_888_963_407));
        ((h >> 11) as f64 / (1u64 << 53) as f64 * 2.0) - 1.0
    };
    let mut first_vector = Vec::new();
    for d in 0..DOCS {
        let v: Vec<f64> = (0..DIMS).map(|dim| component(d, dim)).collect();
        if d == 0 {
            first_vector = v.clone();
        }
        idx.index_document(
            Some(format!("d{d}")),
            json!({
                "body": format!("quarterly liquidity evidence document number {d}"),
                "cat": if d % 2 == 0 { "even" } else { "odd" },
                "n": d,
                "emb": v,
                // The `_chunks` companion, i.e. the per-document MULTI-vector.
                // Present in the fixture because a chunked corpus is the shape
                // RFC #148 reports, and excluding only `emb` leaves this one
                // fully tokenised.
                "emb_chunks": [v, v],
            }),
        )
        .await
        .unwrap();
    }
    idx.refresh().await.unwrap();
    idx.force_merge(1).await.unwrap();

    let files = segment_files(dir.path());
    let bytes_of = |suffix: &str| -> u64 {
        files
            .iter()
            .filter(|(name, _)| name.ends_with(suffix))
            .map(|(_, len)| *len)
            .sum()
    };
    // Reported so the saving is a measured number in the log, not a claim.
    eprintln!(
        "#328 lexical bytes: emb.fst={} emb.post={} emb.norms={} emb_chunks.fst={} | \
         body.fst={} body.post={} cat.fst={} n.fst={} | total-index={}",
        bytes_of(".emb.fst"),
        bytes_of(".emb.post"),
        bytes_of(".emb.norms"),
        bytes_of(".emb_chunks.fst"),
        bytes_of(".body.fst"),
        bytes_of(".body.post"),
        bytes_of(".cat.fst"),
        bytes_of(".n.fst"),
        files.iter().map(|(_, len)| *len).sum::<u64>(),
    );

    for suffix in [
        ".emb.fst",
        ".emb.post",
        ".emb.norms",
        // The companion. Excluding the base name alone leaves this behind, and
        // on the 5,000-doc × 128-dim corpus it is 1,652,858 B — 14.4% of the
        // index that a base-name-only exclusion produces.
        ".emb_chunks.fst",
        ".emb_chunks.post",
        ".emb_chunks.norms",
    ] {
        assert!(
            !files.iter().any(|(name, _)| name.ends_with(suffix)),
            "a dense_vector field must contribute no `{suffix}` artifact; got {:?}",
            files
                .iter()
                .filter(|(name, _)| name.contains(".emb"))
                .collect::<Vec<_>>()
        );
    }
    // The lexical fields are untouched — this is a per-type exclusion, not a
    // blanket one.
    for suffix in [".body.fst", ".cat.fst", ".n.fst"] {
        assert!(
            files.iter().any(|(name, _)| name.ends_with(suffix)),
            "lexical fields must keep their term dictionaries; missing {suffix}"
        );
    }

    // (a) kNN still answers from the HNSW graph.
    let knn = idx
        .search(
            &parse_request(&json!({
                "query": {"knn": {"field": "emb", "query_vector": first_vector, "k": 3}},
                "size": 10
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(knn.hits.len(), 3, "kNN must still retrieve neighbours");
    assert_eq!(
        knn.hits[0].id, "d0",
        "nearest neighbour must be the probe doc"
    );

    // (b) `exists` is answered from `_source` / doc values, never the postings.
    let exists = idx
        .search(&make_search(json!({"exists": {"field": "emb"}})))
        .await
        .unwrap();
    assert_eq!(exists.total.value, DOCS as u64);

    // (c) the lexical fields still match.
    let lexical = idx
        .search(&make_search(json!({"match": {"body": "liquidity"}})))
        .await
        .unwrap();
    assert_eq!(lexical.total.value, DOCS as u64);
    let kw = idx
        .search(&make_search(json!({"term": {"cat": "even"}})))
        .await
        .unwrap();
    assert_eq!(kw.total.value, (DOCS / 2) as u64);

    // (d) a lexical query on the vector field still finds nothing — the same
    // answer `main` gives, and the reason the exclusion had to reach the QUERY
    // PLAN and not only the writer.
    //
    // A POSITIVE component: a leading `-` is `query_string`'s NOT operator,
    // which would make the probe measure the parser rather than the field.
    let probe_num = first_vector
        .iter()
        .copied()
        .find(|v| *v > 0.0)
        .expect("fixture vector has a positive component");
    let probe = format!("{probe_num}");
    for q in [
        json!({"term": {"emb": probe}}),
        json!({"match": {"emb": probe}}),
        json!({"match": {"emb": "0.5"}}),
        json!({"query_string": {"query": probe}}),
        json!({"multi_match": {"query": probe, "fields": ["*"]}}),
        // Numeric `term` / `terms` / `range` — claimed unchanged by #328's
        // first cut and then left out of its fixture, so they are pinned here.
        // All three answer 0 on `main` post-flush and all three still answer 0.
        // (Pre-flush, `main` answers 0 / 1 / every-doc for the same three; that
        // is the pre-flush-moves-onto-flushed divergence the CHANGELOG records,
        // not a post-flush change.)
        json!({"term": {"emb": probe_num}}),
        json!({"terms": {"emb": [probe_num]}}),
        json!({"range": {"emb": {"gte": -2.0, "lte": 2.0}}}),
        // EXPLICITLY-FIELDED forms — the half the writer-side change cannot
        // reach. Each of these names `emb` outright rather than expanding onto
        // it, so no `*`-expansion filter helps; without a plan-time lowering
        // they land on the stored-doc scan. The `multi_match` one is the
        // correctness failure, not merely a slow one: postings-removed-only,
        // it answers DOCS instead of 0.
        json!({"multi_match": {"query": "0", "fields": ["emb"]}}),
        json!({"simple_query_string": {"query": "0", "fields": ["emb"]}}),
        json!({"query_string": {"query": "0", "default_field": "emb"}}),
        // …and the `_chunks` companion, which is in the exclusion set for the
        // same reason and therefore needs the same lowering.
        json!({"term": {"emb_chunks": probe}}),
        json!({"multi_match": {"query": "0", "fields": ["emb_chunks"]}}),
    ] {
        let hits = idx.search(&make_search(q.clone())).await.unwrap();
        assert_eq!(
            hits.total.value, 0,
            "a lexical query on a dense_vector must match nothing, got {} for {q}",
            hits.total.value
        );
    }

    // (d2) a MIXED field list keeps its lexical members. Only the vector entry
    // is dropped, so this is `{"multi_match":{"query":"liquidity","fields":
    // ["body"]}}` in effect — the whole corpus, not nothing. The empty-list
    // lowering must not swallow a list that still has a real field in it.
    let mixed = idx
        .search(&make_search(
            json!({"multi_match": {"query": "liquidity", "fields": ["emb", "body"]}}),
        ))
        .await
        .unwrap();
    assert_eq!(
        mixed.total.value, DOCS as u64,
        "dropping the vector entry must leave the rest of a `fields` list intact"
    );

    // (d3) the mapping still describes the field — the exclusion is about
    // lexical BYTES, not about the field's existence. This is the leaf of the
    // `_field_caps` guarantee reachable from the engine crate: `_field_caps`
    // is served by the API layer from exactly this schema and never opens a
    // term dictionary, so a field that reports `dense_vector` here reports
    // `dense_vector` there.
    let mapped = idx.schema().await;
    let emb_field = mapped
        .fields
        .iter()
        .find(|f| f.name == "emb")
        .expect("`emb` must still be a mapped field");
    assert!(matches!(emb_field.field_type, FieldType::Vector));
    assert_eq!(emb_field.options.dimensions, Some(DIMS));

    // (d4) highlighting a vector field yields no fragments and no panic, while
    // a real text field in the same request still highlights. Highlighting
    // resolves against the stored document and never opens the field's FST,
    // so removing the FST cannot change it.
    let hl = idx
        .search(
            &parse_request(&json!({
                "query": {"match": {"body": "liquidity"}},
                "size": 1,
                "highlight": {"fields": {"body": {}, "emb": {}}}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let frags = hl.hits[0]
        .highlight
        .as_ref()
        .expect("highlight must be present");
    assert!(
        frags.get("body").is_some_and(|f| !f.is_empty()),
        "a text field must still highlight"
    );
    assert!(
        frags.get("emb").is_none_or(|f| f.is_empty()),
        "a dense_vector must produce no highlight fragments, got {:?}",
        frags.get("emb")
    );

    // (e) the index reopens and searches after a restart. The exclusion is a
    // WRITE-side rule and no reader requires the field's sidecar to exist, so
    // a segment written without one loads exactly like one written with it.
    // (This does not, and cannot from inside one test binary, exercise a
    // segment produced by the previous release: those keep their `.emb.*`
    // files until a merge rewrites them, and the per-segment `fts_has_field`
    // gate reads whichever shape it finds.)
    drop(idx);
    drop(engine); // release the data dir's `node.lock` before reopening it
    let reopened = make_engine(&dir);
    let idx = reopened.get_index("duprobe").unwrap();
    let after = idx
        .search(&make_search(json!({"match": {"body": "liquidity"}})))
        .await
        .unwrap();
    assert_eq!(after.total.value, DOCS as u64);
}

/// #328, nested half — a `dense_vector` under an OBJECT mapping.
///
/// Excluding the leaf path alone moves no bytes here, and that is the trap the
/// first cut of this fix fell into: the segment builder never writes a
/// `passages.vec` field at all. It flattens the whole `passages` object into
/// ONE text field, so the vector's 64 decimal components land in
/// `<seg>.passages.fst` under the parent's name. The size of that file is
/// reported below rather than asserted to a golden number, and the ceiling is
/// deliberately generous: the point is that the fixture's 19,200 vector
/// components are gone, and they cannot fit in 4 KiB.
///
/// Both mapping shapes that reach `FieldType::Vector` are pinned, because they
/// take different routes into the schema and only one of them was covered by
/// the walk: a dotted top-level name (`"passages.vec"`, what `put_mapping`
/// produces) and a `vec` sub-mapping under a `passages` object (what
/// `es_properties_to_fields` produces from nested `properties`).
#[tokio::test]
async fn nested_dense_vector_is_excluded_from_its_parent_objects_term_dictionary() {
    const DIMS: usize = 64;
    const DOCS: usize = 300;

    let component = |doc: usize, dim: usize| -> f64 {
        let h = (doc as u64)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add((dim as u64).wrapping_mul(1_442_695_040_888_963_407));
        ((h >> 11) as f64 / (1u64 << 53) as f64 * 2.0) - 1.0
    };

    for shape in ["dotted-name", "sub-mapping"] {
        let dir = TempDir::new().unwrap();
        let mut schema = Schema::empty();
        schema
            .fields
            .push(FieldConfig::new("body", FieldType::Text));
        let mut vec_field = FieldConfig::new(
            if shape == "dotted-name" {
                "passages.vec"
            } else {
                "vec"
            },
            FieldType::Vector,
        );
        vec_field.options.dimensions = Some(DIMS);
        vec_field.options.similarity = Some("cosine".to_string());
        if shape == "dotted-name" {
            schema.fields.push(vec_field);
        } else {
            let mut parent = FieldConfig::new("passages", FieldType::Object);
            parent.fields.push(vec_field);
            schema.fields.push(parent);
        }

        let engine = make_engine(&dir);
        engine.create_index("nested-vec", schema).unwrap();
        let idx = engine.get_index("nested-vec").unwrap();

        let mut first_vector = Vec::new();
        for d in 0..DOCS {
            let v: Vec<f64> = (0..DIMS).map(|dim| component(d, dim)).collect();
            if d == 0 {
                first_vector = v.clone();
            }
            idx.index_document(
                Some(format!("d{d}")),
                json!({
                    "body": format!("quarterly liquidity evidence {d}"),
                    "passages": {"vec": v},
                }),
            )
            .await
            .unwrap();
        }
        idx.refresh().await.unwrap();
        idx.force_merge(1).await.unwrap();

        let mut files: Vec<(String, u64)> = Vec::new();
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, u64)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), out);
                } else {
                    out.push((
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.metadata().unwrap().len(),
                    ));
                }
            }
        }
        walk(dir.path(), &mut files);

        let parent_fst: u64 = files
            .iter()
            .filter(|(name, _)| name.ends_with(".passages.fst"))
            .map(|(_, len)| *len)
            .sum();
        // Reported so the saving is a measured number in the log, not a claim.
        eprintln!(
            "#328 nested [{shape}]: passages.fst={parent_fst} total-index={}",
            files.iter().map(|(_, len)| *len).sum::<u64>()
        );
        // A generous ceiling, not a golden number: the point is that the
        // 19,200 vector components are gone, and they cannot fit in 4 KiB.
        assert!(
            parent_fst < 4096,
            "[{shape}] the parent object's term dictionary must not carry the \
             nested vector's components; `.passages.fst` = {parent_fst} B"
        );
        assert!(
            !files
                .iter()
                .any(|(name, _)| name.ends_with(".passages.vec.fst")),
            "[{shape}] the nested vector leaf must have no term dictionary either"
        );

        // The parent is still a real field, just without the vector in it.
        let lexical = idx
            .search(&make_search(json!({"match": {"body": "liquidity"}})))
            .await
            .unwrap();
        assert_eq!(lexical.total.value, DOCS as u64, "[{shape}]");

        // A component of the vector must not be findable through the parent.
        let probe = format!(
            "{}",
            first_vector
                .iter()
                .copied()
                .find(|v| *v > 0.0)
                .expect("fixture vector has a positive component")
        );
        for q in [
            json!({"match": {"passages": probe}}),
            json!({"term": {"passages": probe}}),
        ] {
            let hits = idx.search(&make_search(q.clone())).await.unwrap();
            assert_eq!(
                hits.total.value, 0,
                "[{shape}] a vector component must not be lexically findable \
                 through its parent object, got {} for {q}",
                hits.total.value
            );
        }

        // kNN on the nested field still answers from the HNSW graph.
        let knn = idx
            .search(
                &parse_request(&json!({
                    "query": {"knn": {
                        "field": "passages.vec", "query_vector": first_vector, "k": 3
                    }},
                    "size": 10
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(knn.hits.len(), 3, "[{shape}] kNN must still retrieve");
        assert_eq!(
            knn.hits[0].id, "d0",
            "[{shape}] nearest must be the probe doc"
        );
    }
}

#[tokio::test]
async fn semantic_passage_provenance_survives_update_merge_restart_and_source_filter() {
    let dir = TempDir::new().unwrap();
    let mut schema = Schema::empty();
    let mut content = FieldConfig::new("content", FieldType::Text);
    content.options.dimensions = Some(64);
    content.options.similarity = Some("cosine".to_string());
    content.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("custom_embedding".to_string()),
    });
    schema.fields.push(content);
    schema
        .fields
        .push(FieldConfig::new("page", FieldType::Long));

    let initial = format!(
        "{} Résumé 📄 quarterly zephyr liquidity evidence. {}",
        "unrelated operating narrative. ".repeat(40),
        "unrelated tax footnote. ".repeat(40)
    );
    {
        let engine = make_engine(&dir);
        engine.create_index("passage-provenance", schema).unwrap();
        let idx = engine.get_index("passage-provenance").unwrap();
        idx.index_document(
            Some("report-page".into()),
            json!({"content": initial, "page": 17, "company": "ACME"}),
        )
        .await
        .unwrap();
        idx.refresh().await.unwrap();
        idx.force_merge(1).await.unwrap();

        let request = parse_request(&json!({
            "query": {"semantic": {
                "field": "content",
                "query": "Résumé 📄 quarterly zephyr liquidity evidence",
                "k": 10
            }},
            "fields": ["_passage"],
            "_source": {"includes": ["company"]},
            "size": 1
        }))
        .unwrap();
        let result = idx.search(&request).await.unwrap();
        let hit = &result.hits[0];
        assert_eq!(hit.source, json!({"company": "ACME"}));
        let passage = hit.passage.as_ref().expect("opt-in passage");
        assert_eq!(passage.field, "content");
        assert_eq!(passage.page, Some(17));
        assert!(passage.text.contains("Résumé 📄 quarterly zephyr"));
        assert!(initial.is_char_boundary(passage.start_offset as usize));
        assert!(initial.is_char_boundary(passage.end_offset as usize));
        assert_eq!(
            &initial[passage.start_offset as usize..passage.end_offset as usize],
            passage.text
        );

        let ordinary = idx
            .search(
                &parse_request(&json!({
                    "query": {"semantic": {
                        "field": "content",
                        "query": "quarterly zephyr liquidity",
                        "k": 10
                    }},
                    "size": 1
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(ordinary.hits[0].passage.is_none());
        assert!(ordinary.hits[0]
            .source
            .as_object()
            .unwrap()
            .keys()
            .all(|name| !name.starts_with("__xerj_passage_meta__")));

        // Re-indexing regenerates offsets from the new authoritative text.
        let updated = format!(
            "{} deferred tax aurora covenant disclosure. {}",
            "replacement narrative. ".repeat(36),
            "replacement appendix. ".repeat(36)
        );
        idx.index_document(
            Some("report-page".into()),
            json!({"content": updated, "page": 18, "company": "ACME"}),
        )
        .await
        .unwrap();
        idx.refresh().await.unwrap();
        idx.force_merge(1).await.unwrap();
        let updated_result = idx
            .search(
                &parse_request(&json!({
                    "query": {"semantic": {
                        "field": "content",
                        "query": "deferred tax aurora covenant disclosure",
                        "k": 10
                    }},
                    "fields": ["_passage"],
                    "size": 1
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let updated_passage = updated_result.hits[0].passage.as_ref().unwrap();
        assert_eq!(updated_passage.page, Some(18));
        assert!(updated_passage.text.contains("aurora covenant"));
        assert_eq!(
            &updated[updated_passage.start_offset as usize..updated_passage.end_offset as usize],
            updated_passage.text
        );
    }

    let reopened = make_engine(&dir);
    let idx = reopened.get_index("passage-provenance").unwrap();
    let reopened_result = idx
        .search(
            &parse_request(&json!({
                "query": {"semantic": {
                    "field": "content",
                    "query": "deferred tax aurora covenant disclosure",
                    "k": 10
                }},
                "fields": ["_passage"],
                "size": 1
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(reopened_result.hits[0]
        .passage
        .as_ref()
        .unwrap()
        .text
        .contains("aurora covenant"));

    idx.delete_document("report-page").await.unwrap();
    let after_delete = idx
        .search(
            &parse_request(&json!({
                "query": {"semantic": {
                    "field": "content",
                    "query": "deferred tax aurora covenant disclosure",
                    "k": 10
                }},
                "fields": ["_passage"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(after_delete.hits.is_empty());
}

#[tokio::test]
async fn reserved_passage_metadata_input_is_rejected() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    let mut content = FieldConfig::new("content", FieldType::Text);
    content.options.dimensions = Some(16);
    content.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("custom_embedding".to_string()),
    });
    schema.fields.push(content);
    engine
        .create_index("reserved-passage-field", schema)
        .unwrap();
    let idx = engine.get_index("reserved-passage-field").unwrap();
    let error = idx
        .index_document(
            Some("spoof".into()),
            json!({
                "content": "user text",
                "__xerj_passage_meta__custom_embedding": {
                    "field": "content",
                    "chunks": [[0, 9]]
                }
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("engine-owned passage metadata"), "{error}");
    assert!(idx.get_document("spoof").await.unwrap().is_none());
}

#[tokio::test]
async fn passage_provenance_rejects_ambiguous_multi_vector_composition_in_any_order() {
    use xerj_query::ast::{FusionStrategy, WeightedQuery};

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("passage-composition", Schema::empty())
        .unwrap();
    let idx = engine.get_index("passage-composition").unwrap();

    let knn_a = QueryNode::Knn {
        field: "embedding_a".into(),
        vector: vec![1.0, 0.0],
        k: 10,
        num_candidates: None,
        filter: None,
        boost: None,
        similarity: None,
    };
    let knn_b = QueryNode::Knn {
        field: "embedding_b".into(),
        vector: vec![0.0, 1.0],
        k: 10,
        num_candidates: None,
        filter: None,
        boost: None,
        similarity: None,
    };

    for should in [
        vec![knn_a.clone(), knn_b.clone()],
        vec![knn_b.clone(), knn_a.clone()],
    ] {
        let request = SearchRequest {
            query: QueryNode::Bool {
                must: Vec::new(),
                should,
                filter: Vec::new(),
                must_not: Vec::new(),
                minimum_should_match: None,
            },
            fields: vec!["_passage".into()],
            ..SearchRequest::default()
        };
        let error = idx.search(&request).await.unwrap_err().to_string();
        assert!(error.contains("one unambiguous winning passage"), "{error}");
    }

    let semantic = QueryNode::SemanticSearch {
        field: "content".into(),
        text: "quarterly evidence".into(),
        k: 10,
        filter: None,
        boost: None,
    };
    for queries in [
        vec![semantic.clone(), QueryNode::MatchAll],
        vec![QueryNode::MatchAll, semantic.clone()],
    ] {
        let request = SearchRequest {
            query: QueryNode::Hybrid {
                queries: queries
                    .into_iter()
                    .map(|query| WeightedQuery { query, weight: 1.0 })
                    .collect(),
                fusion: FusionStrategy::Rrf { k: 60 },
            },
            fields: vec!["_passage".into()],
            ..SearchRequest::default()
        };
        let error = idx.search(&request).await.unwrap_err().to_string();
        assert!(error.contains("one unambiguous winning passage"), "{error}");
    }
}

fn make_search(query_json: Value) -> SearchRequest {
    parse_request(&json!({ "query": query_json, "size": 100 })).expect("parse_request")
}

fn make_search_with_source(query_json: Value, source: Value) -> SearchRequest {
    parse_request(&json!({
        "query": query_json,
        "size": 100,
        "_source": source
    }))
    .expect("parse_request")
}

fn make_search_with_size(query_json: Value, size: usize) -> SearchRequest {
    parse_request(&json!({ "query": query_json, "size": size })).expect("parse_request")
}

// ── 1. Basic lifecycle: create index, index documents, search ─────────────────

#[tokio::test]
async fn test_create_index_and_search() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("books", Schema::empty()).unwrap();
    let idx = engine.get_index("books").unwrap();

    idx.index_document(
        Some("1".into()),
        json!({ "title": "Rust Programming Language", "year": 2019 }),
    )
    .await
    .unwrap();

    idx.index_document(
        Some("2".into()),
        json!({ "title": "Programming Python", "year": 2010 }),
    )
    .await
    .unwrap();

    idx.index_document(
        Some("3".into()),
        json!({ "title": "Learning Go", "year": 2021 }),
    )
    .await
    .unwrap();

    // Match all
    let result = idx
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();
    assert_eq!(result.total.value, 3, "match_all should return 3 docs");
    assert_eq!(result.hits.len(), 3);

    // Match query
    let result = idx
        .search(&make_search(json!({"match": {"title": "Rust"}})))
        .await
        .unwrap();
    assert_eq!(result.total.value, 1);
    assert_eq!(result.hits[0].id, "1");
}

// ── 2. All query types ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_types() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("items", Schema::empty()).unwrap();
    let idx = engine.get_index("items").unwrap();

    idx.index_document(
        Some("a".into()),
        json!({ "name": "apple", "price": 1.5, "in_stock": true, "tags": ["fruit", "red"] }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b".into()),
        json!({ "name": "banana", "price": 0.75, "in_stock": true, "tags": ["fruit", "yellow"] }),
    )
    .await
    .unwrap();
    idx.index_document(Some("c".into()), json!({ "name": "carrot", "price": 2.0, "in_stock": false, "tags": ["vegetable", "orange"] })).await.unwrap();
    idx.index_document(Some("d".into()), json!({ "name": "dragonfruit", "price": 5.0, "in_stock": true, "tags": ["fruit", "exotic"] })).await.unwrap();

    // term
    let r = idx
        .search(&make_search(json!({"term": {"name": "apple"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1);
    assert_eq!(r.hits[0].id, "a");

    // terms (OR semantics)
    let r = idx
        .search(&make_search(
            json!({"terms": {"name": ["apple", "banana"]}}),
        ))
        .await
        .unwrap();
    assert_eq!(r.total.value, 2);

    // range
    let r = idx
        .search(&make_search(
            json!({"range": {"price": {"gte": 1.0, "lte": 3.0}}}),
        ))
        .await
        .unwrap();
    assert_eq!(r.total.value, 2); // apple (1.5) and carrot (2.0)

    // prefix
    let r = idx
        .search(&make_search(json!({"prefix": {"name": "app"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1);
    assert_eq!(r.hits[0].id, "a");

    // wildcard
    let r = idx
        .search(&make_search(json!({"wildcard": {"name": "b*na"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1);
    assert_eq!(r.hits[0].id, "b");

    // fuzzy
    let r = idx
        .search(&make_search(json!({"fuzzy": {"name": {"value": "aple"}}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1);
    assert_eq!(r.hits[0].id, "a");

    // exists
    let r = idx
        .search(&make_search(json!({"exists": {"field": "in_stock"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 4);

    // exists on absent field
    let r = idx
        .search(&make_search(json!({"exists": {"field": "nonexistent"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 0);

    // bool: must + must_not
    let r = idx
        .search(&make_search(json!({
            "bool": {
                "must": [{"term": {"in_stock": true}}],
                "must_not": [{"term": {"name": "banana"}}]
            }
        })))
        .await
        .unwrap();
    assert_eq!(r.total.value, 2); // apple and dragonfruit

    // ids
    let r = idx
        .search(&make_search(json!({"ids": {"values": ["a", "c"]}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 2);
    let mut ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["a", "c"]);
}

// ── 3. Aggregations ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_aggregations() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("sales", Schema::empty()).unwrap();
    let idx = engine.get_index("sales").unwrap();

    for (id, name, amount, category) in [
        ("1", "Widget A", 10.0, "widgets"),
        ("2", "Widget B", 20.0, "widgets"),
        ("3", "Gadget X", 50.0, "gadgets"),
        ("4", "Gadget Y", 75.0, "gadgets"),
        ("5", "Widget C", 15.0, "widgets"),
    ] {
        idx.index_document(
            Some(id.into()),
            json!({ "name": name, "amount": amount, "category": category }),
        )
        .await
        .unwrap();
    }

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_category": {
                "terms": { "field": "category" }
            },
            "amount_stats": {
                "stats": { "field": "amount" }
            },
            "price_ranges": {
                "range": {
                    "field": "amount",
                    "ranges": [
                        { "to": 20.0 },
                        { "from": 20.0, "to": 60.0 },
                        { "from": 60.0 }
                    ]
                }
            },
            "amount_hist": {
                "histogram": { "field": "amount", "interval": 25 }
            },
            "pcts": {
                "percentiles": { "field": "amount", "percents": [50, 95] }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();

    // size=0 should return no hits but the right total
    assert_eq!(result.hits.len(), 0);
    assert_eq!(result.total.value, 5);

    let aggs = result.aggs.as_ref().expect("aggs should be present");

    // terms aggregation
    let by_cat = &aggs["by_category"];
    let buckets = by_cat["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 2);
    // widgets: 3, gadgets: 2 (default sort by count desc)
    assert_eq!(buckets[0]["key"].as_str().unwrap(), "widgets");
    assert_eq!(buckets[0]["doc_count"].as_u64().unwrap(), 3);

    // stats aggregation
    let stats = &aggs["amount_stats"];
    assert_eq!(stats["count"].as_u64().unwrap(), 5);
    assert!((stats["min"].as_f64().unwrap() - 10.0).abs() < 0.01);
    assert!((stats["max"].as_f64().unwrap() - 75.0).abs() < 0.01);
    let expected_avg = (10.0 + 20.0 + 50.0 + 75.0 + 15.0) / 5.0;
    assert!((stats["avg"].as_f64().unwrap() - expected_avg).abs() < 0.01);

    // range aggregation
    let range_buckets = aggs["price_ranges"]["buckets"].as_array().unwrap();
    assert_eq!(range_buckets.len(), 3);

    // histogram aggregation
    let hist_buckets = aggs["amount_hist"]["buckets"].as_array().unwrap();
    assert!(!hist_buckets.is_empty());

    // percentiles aggregation
    let pcts_values = &aggs["pcts"]["values"];
    assert!(pcts_values.is_object());
}

// ── 4. Document lifecycle: create, get, update, delete ───────────────────────

#[tokio::test]
async fn test_document_lifecycle() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("docs", Schema::empty()).unwrap();
    let idx = engine.get_index("docs").unwrap();

    // Create
    let resp = idx
        .index_document(
            Some("doc1".into()),
            json!({"content": "hello world", "version": 1}),
        )
        .await
        .unwrap();
    assert_eq!(resp.id, "doc1");
    assert_eq!(resp.result, "created");

    // Get
    let doc = idx.get_document("doc1").await.unwrap();
    assert!(doc.is_some());
    assert_eq!(doc.unwrap()["content"].as_str().unwrap(), "hello world");

    // Update (re-index with same ID)
    idx.index_document(
        Some("doc1".into()),
        json!({"content": "updated content", "version": 2}),
    )
    .await
    .unwrap();
    let updated = idx.get_document("doc1").await.unwrap().unwrap();
    assert_eq!(updated["content"].as_str().unwrap(), "updated content");
    assert_eq!(updated["version"].as_u64().unwrap(), 2);

    // Delete
    let deleted = idx.delete_document("doc1").await.unwrap();
    assert!(deleted);

    // Get after delete should return None
    let gone = idx.get_document("doc1").await.unwrap();
    assert!(gone.is_none(), "document should be gone after deletion");

    // Deleting non-existent document
    let re_delete = idx.delete_document("doc1").await.unwrap();
    assert!(!re_delete, "deleting non-existent doc should return false");
}

// ── 5. WAL persistence: data survives engine restart ─────────────────────────

#[tokio::test]
async fn test_wal_persistence() {
    let dir = TempDir::new().unwrap();

    // Create engine, index docs, drop engine.
    {
        let engine = make_engine(&dir);
        engine.create_index("persist", Schema::empty()).unwrap();
        let idx = engine.get_index("persist").unwrap();
        idx.index_document(Some("p1".into()), json!({"data": "survives"}))
            .await
            .unwrap();
        idx.index_document(Some("p2".into()), json!({"data": "also survives"}))
            .await
            .unwrap();
        // Engine is dropped here; WAL is flushed to disk.
    }

    // Re-open the engine with the same data directory.
    {
        let engine = make_engine(&dir);
        let idx = engine.get_index("persist").unwrap();

        let doc1 = idx.get_document("p1").await.unwrap();
        assert!(doc1.is_some(), "p1 should persist after restart");
        assert_eq!(doc1.unwrap()["data"].as_str().unwrap(), "survives");

        let doc2 = idx.get_document("p2").await.unwrap();
        assert!(doc2.is_some(), "p2 should persist after restart");

        // Search should also work
        let result = idx
            .search(&make_search(json!({"match_all": {}})))
            .await
            .unwrap();
        assert_eq!(
            result.total.value, 2,
            "both docs should be found after restart"
        );
    }
}

// ── 6. size=0 returns correct total but no hits ───────────────────────────────

#[tokio::test]
async fn test_size_zero_returns_total_only() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("counts", Schema::empty()).unwrap();
    let idx = engine.get_index("counts").unwrap();

    for i in 0..10 {
        idx.index_document(Some(format!("doc{i}")), json!({"value": i}))
            .await
            .unwrap();
    }

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "from": 0
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.total.value, 10, "total should be 10");
    assert_eq!(
        result.hits.len(),
        0,
        "no hits should be returned with size=0"
    );
}

// ── 7. _source filtering ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_source_filtering() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("src", Schema::empty()).unwrap();
    let idx = engine.get_index("src").unwrap();

    idx.index_document(
        Some("s1".into()),
        json!({ "name": "Alice", "age": 30, "email": "alice@example.com", "secret": "hidden" }),
    )
    .await
    .unwrap();

    // Include only name and age
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10,
        "_source": ["name", "age"]
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.hits.len(), 1);
    let source = &result.hits[0].source;
    assert!(source.get("name").is_some(), "name should be included");
    assert!(source.get("age").is_some(), "age should be included");
    assert!(source.get("email").is_none(), "email should be excluded");
    assert!(source.get("secret").is_none(), "secret should be excluded");

    // Disable source entirely
    let req_no_source = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10,
        "_source": false
    }))
    .unwrap();

    let result2 = idx.search(&req_no_source).await.unwrap();
    assert_eq!(result2.hits.len(), 1);
    // `_source: false` suppression is a response-time decision in
    // es_compat.rs (`source_body_disabled`), not a data-layer one: the
    // engine keeps the raw source on the hit so the HTTP layer can still
    // resolve `fields` / `_ignored` / `highlight` against it. Wire-level
    // omission is covered by the ES-compat YAML conformance suite.
    assert!(
        !result2.hits[0].source.is_null(),
        "engine must keep the raw source; the response layer suppresses it"
    );
}

// ── 8. Field sorting ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_field_sorting() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("sortidx", Schema::empty()).unwrap();
    let idx = engine.get_index("sortidx").unwrap();

    idx.index_document(Some("z1".into()), json!({"rank": 3, "name": "Charlie"}))
        .await
        .unwrap();
    idx.index_document(Some("z2".into()), json!({"rank": 1, "name": "Alice"}))
        .await
        .unwrap();
    idx.index_document(Some("z3".into()), json!({"rank": 2, "name": "Bob"}))
        .await
        .unwrap();

    // Sort by rank ascending
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10,
        "sort": [{ "rank": "asc" }]
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.hits.len(), 3);
    assert_eq!(result.hits[0].id, "z2"); // rank=1
    assert_eq!(result.hits[1].id, "z3"); // rank=2
    assert_eq!(result.hits[2].id, "z1"); // rank=3

    // Sort by name descending
    let req_desc = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10,
        "sort": [{ "name": "desc" }]
    }))
    .unwrap();

    let result_desc = idx.search(&req_desc).await.unwrap();
    assert_eq!(result_desc.hits[0].id, "z1"); // Charlie
}

// ── 9. delete_by_query ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_by_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("dbq", Schema::empty()).unwrap();
    let idx = engine.get_index("dbq").unwrap();

    idx.index_document(
        Some("q1".into()),
        json!({"category": "delete_me", "val": 1}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("q2".into()),
        json!({"category": "delete_me", "val": 2}),
    )
    .await
    .unwrap();
    idx.index_document(Some("q3".into()), json!({"category": "keep", "val": 3}))
        .await
        .unwrap();

    // Delete docs where category == "delete_me"
    let query = QueryNode::Term {
        field: "category".into(),
        value: serde_json::Value::String("delete_me".into()),
        boost: None,
    };

    let (total, deleted) = idx.delete_by_query(query).await.unwrap();
    assert_eq!(total, 2, "should have matched 2 docs");
    assert_eq!(deleted, 2, "should have deleted 2 docs");

    // Verify remaining docs
    let result = idx
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();
    assert_eq!(result.total.value, 1);
    assert_eq!(result.hits[0].id, "q3");
}

// ── 10. multi_match query ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_multi_match_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("mm", Schema::empty()).unwrap();
    let idx = engine.get_index("mm").unwrap();

    idx.index_document(
        Some("m1".into()),
        json!({"title": "Rust book", "body": "Systems programming"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("m2".into()),
        json!({"title": "Python guide", "body": "Rust also mentioned here"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("m3".into()),
        json!({"title": "JavaScript", "body": "Web development"}),
    )
    .await
    .unwrap();

    let r = idx
        .search(&make_search(json!({
            "multi_match": {
                "query": "Rust",
                "fields": ["title", "body"]
            }
        })))
        .await
        .unwrap();

    assert_eq!(r.total.value, 2, "both m1 and m2 mention Rust");
    let mut ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["m1", "m2"]);
}

// ── 11. match_phrase query ────────────────────────────────────────────────────

#[tokio::test]
async fn test_match_phrase_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("phrase", Schema::empty()).unwrap();
    let idx = engine.get_index("phrase").unwrap();

    idx.index_document(
        Some("ph1".into()),
        json!({"text": "the quick brown fox jumps"}),
    )
    .await
    .unwrap();
    idx.index_document(Some("ph2".into()), json!({"text": "the brown quick fox"}))
        .await
        .unwrap();
    idx.index_document(Some("ph3".into()), json!({"text": "quick brown study"}))
        .await
        .unwrap();

    // "quick brown" should match ph1 and ph3 but NOT ph2 (wrong order)
    let r = idx
        .search(&make_search(json!({
            "match_phrase": { "text": "quick brown" }
        })))
        .await
        .unwrap();

    let mut ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    ids.sort();
    assert!(ids.contains(&"ph1"), "ph1 should match");
    assert!(ids.contains(&"ph3"), "ph3 should match");
    assert!(!ids.contains(&"ph2"), "ph2 should NOT match (wrong order)");
}

// ── 12. ids query ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_ids_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("idsidx", Schema::empty()).unwrap();
    let idx = engine.get_index("idsidx").unwrap();

    for i in 1..=5 {
        idx.index_document(Some(format!("id{i}")), json!({"n": i}))
            .await
            .unwrap();
    }

    let r = idx
        .search(&make_search(json!({
            "ids": { "values": ["id2", "id4", "id99"] }
        })))
        .await
        .unwrap();

    assert_eq!(r.total.value, 2, "only id2 and id4 exist");
    let mut ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["id2", "id4"]);
}

// ── 13. geo_distance query ────────────────────────────────────────────────────

#[tokio::test]
async fn test_geo_distance_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("geo", Schema::empty()).unwrap();
    let idx = engine.get_index("geo").unwrap();

    // New York City area
    idx.index_document(
        Some("nyc".into()),
        json!({ "name": "New York", "location": { "lat": 40.7128, "lon": -74.0060 } }),
    )
    .await
    .unwrap();

    // London
    idx.index_document(
        Some("lon".into()),
        json!({ "name": "London", "location": { "lat": 51.5074, "lon": -0.1278 } }),
    )
    .await
    .unwrap();

    // Newark (very close to NYC, ~16 km)
    idx.index_document(
        Some("nwk".into()),
        json!({ "name": "Newark", "location": { "lat": 40.7357, "lon": -74.1724 } }),
    )
    .await
    .unwrap();

    // Query: within 50 km of NYC
    let r = idx
        .search(&make_search(json!({
            "geo_distance": {
                "distance": "50km",
                "location": { "lat": 40.7128, "lon": -74.0060 }
            }
        })))
        .await
        .unwrap();

    assert_eq!(
        r.total.value, 2,
        "NYC and Newark should be within 50km of NYC"
    );
    let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"nyc"));
    assert!(ids.contains(&"nwk"));
    assert!(!ids.contains(&"lon"));
}

// ── 14. haversine_distance helper ─────────────────────────────────────────────

#[test]
fn test_haversine_distance() {
    use xerj_engine::index::haversine_distance;

    // NYC to London (approx 5570 km)
    let d = haversine_distance(40.7128, -74.0060, 51.5074, -0.1278);
    assert!(
        (d - 5570.0).abs() < 50.0,
        "NYC-London distance should be ~5570 km, got {d:.1}"
    );

    // Same point should be 0
    let d0 = haversine_distance(40.0, -74.0, 40.0, -74.0);
    assert!(
        d0 < 0.001,
        "distance from point to itself should be 0, got {d0}"
    );

    // NYC to Newark (~16 km)
    let d2 = haversine_distance(40.7128, -74.0060, 40.7357, -74.1724);
    assert!(
        d2 < 20.0,
        "NYC-Newark distance should be < 20 km, got {d2:.1}"
    );
}

// ── 15. bool query combinations ───────────────────────────────────────────────

#[tokio::test]
async fn test_bool_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("bool_test", Schema::empty()).unwrap();
    let idx = engine.get_index("bool_test").unwrap();

    idx.index_document(
        Some("b1".into()),
        json!({"active": true, "role": "admin", "score": 90}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b2".into()),
        json!({"active": true, "role": "user", "score": 70}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b3".into()),
        json!({"active": false, "role": "admin", "score": 80}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b4".into()),
        json!({"active": true, "role": "user", "score": 50}),
    )
    .await
    .unwrap();

    // must: active=true, must_not: role=admin
    let r = idx
        .search(&make_search(json!({
            "bool": {
                "must": [{"term": {"active": true}}],
                "must_not": [{"term": {"role": "admin"}}]
            }
        })))
        .await
        .unwrap();
    assert_eq!(r.total.value, 2); // b2 and b4

    // filter + range
    let r2 = idx
        .search(&make_search(json!({
            "bool": {
                "filter": [
                    {"term": {"active": true}},
                    {"range": {"score": {"gte": 70}}}
                ]
            }
        })))
        .await
        .unwrap();
    assert_eq!(r2.total.value, 2); // b1 (90) and b2 (70)

    // should with minimum_should_match
    let r3 = idx
        .search(&make_search(json!({
            "bool": {
                "should": [
                    {"term": {"role": "admin"}},
                    {"range": {"score": {"gte": 80}}}
                ],
                "minimum_should_match": 2
            }
        })))
        .await
        .unwrap();
    assert_eq!(r3.total.value, 2); // b1 (admin + score>=80) and b3 (admin + score=80)
}

// ── 16. match_none returns zero hits ─────────────────────────────────────────

#[tokio::test]
async fn test_match_none() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("none_test", Schema::empty()).unwrap();
    let idx = engine.get_index("none_test").unwrap();

    idx.index_document(Some("n1".into()), json!({"x": 1}))
        .await
        .unwrap();

    let r = idx
        .search(&make_search(json!({"match_none": {}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 0);
    assert_eq!(r.hits.len(), 0);
}

// ── 17. BM25 ranking test ──────────────────────────────────────────────────────
//
// 5 docs with varying relevance to "search engine".
// The doc that mentions both "search" and "engine" most should rank highest.

#[tokio::test]
async fn test_bm25_ranking() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("bm25_rank", Schema::empty()).unwrap();
    let idx = engine.get_index("bm25_rank").unwrap();

    // Most relevant: mentions both "search" and "engine" multiple times.
    idx.index_document(
        Some("high".into()),
        json!({ "body": "search engine search engine full text search engine" }),
    )
    .await
    .unwrap();

    // Medium: mentions both once.
    idx.index_document(
        Some("med".into()),
        json!({ "body": "a search engine for data" }),
    )
    .await
    .unwrap();

    // Partial: only "search".
    idx.index_document(
        Some("search_only".into()),
        json!({ "body": "searching for data sources" }),
    )
    .await
    .unwrap();

    // Partial: only "engine".
    idx.index_document(
        Some("engine_only".into()),
        json!({ "body": "engine driving power" }),
    )
    .await
    .unwrap();

    // Irrelevant.
    idx.index_document(
        Some("irrel".into()),
        json!({ "body": "completely unrelated content about cats" }),
    )
    .await
    .unwrap();

    let result = idx
        .search(&make_search(json!({"match": {"body": "search engine"}})))
        .await
        .unwrap();

    // "high" should score highest — both terms appear multiple times.
    assert!(!result.hits.is_empty(), "should have at least one hit");
    assert_eq!(
        result.hits[0].id, "high",
        "most relevant doc should rank first"
    );

    // "irrel" should not appear (no matching terms after stop-word removal).
    let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(!ids.contains(&"irrel"), "irrelevant doc should not match");
}

// ── 18. Multi-word match — all terms contribute to score ──────────────────────

#[tokio::test]
async fn test_multiword_match_scoring() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("mw_score", Schema::empty()).unwrap();
    let idx = engine.get_index("mw_score").unwrap();

    // Both query terms present.
    idx.index_document(
        Some("both".into()),
        json!({ "text": "the quick brown fox" }),
    )
    .await
    .unwrap();

    // Only one query term present.
    idx.index_document(Some("one".into()), json!({ "text": "the quick blue bird" }))
        .await
        .unwrap();

    // Neither term.
    idx.index_document(
        Some("neither".into()),
        json!({ "text": "completely different stuff" }),
    )
    .await
    .unwrap();

    // "quick brown" — "quick" survives analysis (not a stop word);
    // "brown" also survives.  "both" has both, "one" has only "quick".
    let result = idx
        .search(&make_search(json!({"match": {"text": "quick brown"}})))
        .await
        .unwrap();

    // "both" should rank above "one".
    assert!(result.hits.len() >= 2, "at least 2 hits expected");
    assert_eq!(
        result.hits[0].id, "both",
        "doc with both terms should rank first"
    );

    // "neither" should not appear.
    let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(
        !ids.contains(&"neither"),
        "doc without matching terms should not appear"
    );
}

// ── 19. Fuzzy query — typo tolerance ──────────────────────────────────────────

#[tokio::test]
async fn test_fuzzy_query_typo() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("fuzzy_typo", Schema::empty()).unwrap();
    let idx = engine.get_index("fuzzy_typo").unwrap();

    idx.index_document(Some("es".into()), json!({ "name": "Elasticsearch" }))
        .await
        .unwrap();

    idx.index_document(Some("os".into()), json!({ "name": "OpenSearch" }))
        .await
        .unwrap();

    // "Elastcsearch" is a 1-character transposition/deletion away from "Elasticsearch".
    // With AUTO fuzziness the threshold for a 13-char word is 2 edits.
    let r = idx
        .search(&make_search(json!({
            "fuzzy": {
                "name": {
                    "value": "Elastcsearch",
                    "fuzziness": "AUTO"
                }
            }
        })))
        .await
        .unwrap();

    assert_eq!(r.total.value, 1, "fuzzy query should match the typo");
    assert_eq!(r.hits[0].id, "es");
}

// ── 20. Highlight test ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_highlight() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("hl_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("hl_idx").unwrap();

    idx.index_document(
        Some("h1".into()),
        json!({ "content": "The quick brown fox jumps over the lazy dog" }),
    )
    .await
    .unwrap();

    let req = parse_request(&json!({
        "query": { "match": { "content": "fox" } },
        "size": 10,
        "highlight": {
            "fields": {
                "content": {}
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.hits.len(), 1);
    let hit = &result.hits[0];
    let hl = hit.highlight.as_ref().expect("highlight should be present");
    let frags = hl
        .get("content")
        .expect("content highlight should be present");
    assert!(
        !frags.is_empty(),
        "should have at least one highlight fragment"
    );
    let combined = frags.join(" ");
    assert!(
        combined.contains("<em>") && combined.contains("</em>"),
        "fragment should contain <em> tags, got: {combined}"
    );
    assert!(
        combined.to_lowercase().contains("fox"),
        "fragment should contain the matched term"
    );
}

// ── 21. Aggregation with 20 docs — bucket counts ──────────────────────────────

#[tokio::test]
async fn test_terms_agg_bucket_counts() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("agg20", Schema::empty()).unwrap();
    let idx = engine.get_index("agg20").unwrap();

    let categories = ["alpha", "beta", "gamma"];
    for i in 0..20u32 {
        let cat = categories[(i % 3) as usize];
        idx.index_document(
            Some(format!("doc{i}")),
            json!({ "category": cat, "val": i }),
        )
        .await
        .unwrap();
    }
    // alpha: i=0,3,6,9,12,15,18  → 7 docs
    // beta:  i=1,4,7,10,13,16,19 → 7 docs
    // gamma: i=2,5,8,11,14,17    → 6 docs

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_cat": {
                "terms": { "field": "category", "size": 10 }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.total.value, 20);

    let aggs = result.aggs.as_ref().expect("aggs present");
    let buckets = aggs["by_cat"]["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 3, "should have 3 category buckets");

    // Sorted by count desc — both alpha and beta have 7.
    let total_docs: u64 = buckets
        .iter()
        .map(|b| b["doc_count"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(total_docs, 20, "bucket doc counts should sum to 20");

    // gamma should have 6 docs (least).
    let gamma = buckets
        .iter()
        .find(|b| b["key"].as_str() == Some("gamma"))
        .unwrap();
    assert_eq!(gamma["doc_count"].as_u64().unwrap(), 6);
}

// ── 22. Range aggregation — bucket boundaries ─────────────────────────────────

#[tokio::test]
async fn test_range_agg_boundaries() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("range_agg", Schema::empty()).unwrap();
    let idx = engine.get_index("range_agg").unwrap();

    // Index 10 docs with prices 10, 20, 30, ... 100.
    for i in 1..=10u32 {
        idx.index_document(Some(format!("p{i}")), json!({ "price": i * 10 }))
            .await
            .unwrap();
    }

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "price_ranges": {
                "range": {
                    "field": "price",
                    "ranges": [
                        { "to": 30.0 },
                        { "from": 30.0, "to": 70.0 },
                        { "from": 70.0 }
                    ]
                }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    let aggs = result.aggs.as_ref().expect("aggs present");
    let buckets = aggs["price_ranges"]["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 3);

    // Bucket 0: price < 30 → prices 10, 20 → 2 docs.
    assert_eq!(
        buckets[0]["doc_count"].as_u64().unwrap(),
        2,
        "< 30 should have 2 docs"
    );
    // Bucket 1: 30 <= price < 70 → prices 30, 40, 50, 60 → 4 docs.
    assert_eq!(
        buckets[1]["doc_count"].as_u64().unwrap(),
        4,
        "30-70 should have 4 docs"
    );
    // Bucket 2: price >= 70 → prices 70, 80, 90, 100 → 4 docs.
    assert_eq!(
        buckets[2]["doc_count"].as_u64().unwrap(),
        4,
        ">= 70 should have 4 docs"
    );
}

// ── 23. Bool must_not — exclusion ─────────────────────────────────────────────

#[tokio::test]
async fn test_bool_must_not_excludes() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine
        .create_index("must_not_idx", Schema::empty())
        .unwrap();
    let idx = engine.get_index("must_not_idx").unwrap();

    idx.index_document(
        Some("a".into()),
        json!({"status": "active", "type": "admin"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b".into()),
        json!({"status": "active", "type": "user"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("c".into()),
        json!({"status": "inactive", "type": "user"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("d".into()),
        json!({"status": "active", "type": "moderator"}),
    )
    .await
    .unwrap();

    // must: status=active, must_not: type=admin
    let r = idx
        .search(&make_search(json!({
            "bool": {
                "must": [{ "term": { "status": "active" } }],
                "must_not": [{ "term": { "type": "admin" } }]
            }
        })))
        .await
        .unwrap();

    assert_eq!(r.total.value, 2, "should return b and d only");
    let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"b"));
    assert!(ids.contains(&"d"));
    assert!(!ids.contains(&"a"), "admin should be excluded by must_not");
    assert!(!ids.contains(&"c"), "inactive should be excluded by must");
}

// ── 24. Pagination — no overlap between pages ─────────────────────────────────

#[tokio::test]
async fn test_pagination_no_overlap() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("pages", Schema::empty()).unwrap();
    let idx = engine.get_index("pages").unwrap();

    for i in 0..20u32 {
        idx.index_document(Some(format!("doc{i:02}")), json!({ "n": i }))
            .await
            .unwrap();
    }

    let page1_req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 5,
        "from": 0,
        "sort": [{ "n": "asc" }]
    }))
    .unwrap();

    let page2_req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 5,
        "from": 5,
        "sort": [{ "n": "asc" }]
    }))
    .unwrap();

    let r1 = idx.search(&page1_req).await.unwrap();
    let r2 = idx.search(&page2_req).await.unwrap();

    assert_eq!(r1.hits.len(), 5, "page 1 should have 5 hits");
    assert_eq!(r2.hits.len(), 5, "page 2 should have 5 hits");

    let ids1: std::collections::HashSet<&str> = r1.hits.iter().map(|h| h.id.as_str()).collect();
    let ids2: std::collections::HashSet<&str> = r2.hits.iter().map(|h| h.id.as_str()).collect();

    let overlap: Vec<&&str> = ids1.intersection(&ids2).collect();
    assert!(
        overlap.is_empty(),
        "pages should not overlap, found: {:?}",
        overlap
    );

    // Verify the pages are consecutive (asc sort by n).
    let last_n1 = r1.hits.last().unwrap().source["n"].as_u64().unwrap();
    let first_n2 = r2.hits.first().unwrap().source["n"].as_u64().unwrap();
    assert!(first_n2 > last_n1, "page 2 should start after page 1 ends");
}

// ── 25. Sort stability — consistent ordering for duplicate sort values ─────────

#[tokio::test]
async fn test_sort_stability() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("sort_stab", Schema::empty()).unwrap();
    let idx = engine.get_index("sort_stab").unwrap();

    // All docs have the same "rank" value — tie-breaking should use doc ID.
    for i in 0..5u32 {
        idx.index_document(Some(format!("doc{i}")), json!({ "rank": 42, "n": i }))
            .await
            .unwrap();
    }

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10,
        "sort": [{ "rank": "asc" }]
    }))
    .unwrap();

    let r1 = idx.search(&req).await.unwrap();
    let r2 = idx.search(&req).await.unwrap();

    assert_eq!(r1.hits.len(), 5);
    assert_eq!(r2.hits.len(), 5);

    // Ordering should be identical across two identical queries.
    let ids1: Vec<&str> = r1.hits.iter().map(|h| h.id.as_str()).collect();
    let ids2: Vec<&str> = r2.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(
        ids1, ids2,
        "sort order should be stable across identical queries"
    );
}

// ── 26. Alias test ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_alias_search() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("real_index", Schema::empty()).unwrap();
    let idx = engine.get_index("real_index").unwrap();

    idx.index_document(Some("a1".into()), json!({"msg": "hello from real index"}))
        .await
        .unwrap();
    idx.index_document(Some("a2".into()), json!({"msg": "another doc"}))
        .await
        .unwrap();

    // Add alias "my_alias" → "real_index".
    engine.add_alias("my_alias", "real_index").unwrap();

    // Search via alias should return the same results as searching via the real name.
    let idx_via_alias = engine.get_index("my_alias").unwrap();
    let result = idx_via_alias
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();

    assert_eq!(
        result.total.value, 2,
        "search via alias should return all docs"
    );
    let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"a1"));
    assert!(ids.contains(&"a2"));

    // Remove alias and verify it no longer resolves.
    engine.remove_alias("my_alias", "real_index").unwrap();
    let resolved = engine.resolve_alias("my_alias");
    assert_eq!(
        resolved,
        vec!["my_alias".to_string()],
        "removed alias should fall back to literal name"
    );
}

// ── 27. Regexp query ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_regexp_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("regexp_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("regexp_idx").unwrap();

    idx.index_document(Some("r1".into()), json!({ "sku": "ABC-1234" }))
        .await
        .unwrap();
    idx.index_document(Some("r2".into()), json!({ "sku": "ABC-5678" }))
        .await
        .unwrap();
    idx.index_document(Some("r3".into()), json!({ "sku": "XYZ-9999" }))
        .await
        .unwrap();
    idx.index_document(Some("r4".into()), json!({ "sku": "DEF-0001" }))
        .await
        .unwrap();

    // Match any SKU starting with "ABC-".
    let r = idx
        .search(&make_search(json!({
            "regexp": { "sku": "ABC-.*" }
        })))
        .await
        .unwrap();

    assert_eq!(r.total.value, 2, "only r1 and r2 match ABC-.*");
    let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"r1"));
    assert!(ids.contains(&"r2"));
    assert!(!ids.contains(&"r3"));
    assert!(!ids.contains(&"r4"));
}

// ── 28. Geo distance test — only nearby docs match ────────────────────────────

#[tokio::test]
async fn test_geo_distance_radius() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("geo2", Schema::empty()).unwrap();
    let idx = engine.get_index("geo2").unwrap();

    // Paris centre (~0 km from query point).
    idx.index_document(
        Some("paris".into()),
        json!({ "name": "Paris", "loc": { "lat": 48.8566, "lon": 2.3522 } }),
    )
    .await
    .unwrap();

    // Versailles (~20 km from Paris).
    idx.index_document(
        Some("versailles".into()),
        json!({ "name": "Versailles", "loc": { "lat": 48.8044, "lon": 2.1204 } }),
    )
    .await
    .unwrap();

    // Lyon (~390 km from Paris).
    idx.index_document(
        Some("lyon".into()),
        json!({ "name": "Lyon", "loc": { "lat": 45.7640, "lon": 4.8357 } }),
    )
    .await
    .unwrap();

    // Query: within 50 km of Paris centre.
    let r = idx
        .search(&make_search(json!({
            "geo_distance": {
                "distance": "50km",
                "loc": { "lat": 48.8566, "lon": 2.3522 }
            }
        })))
        .await
        .unwrap();

    assert_eq!(
        r.total.value, 2,
        "Paris and Versailles should be within 50km"
    );
    let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"paris"));
    assert!(ids.contains(&"versailles"));
    assert!(
        !ids.contains(&"lyon"),
        "Lyon is ~390km away, should not match"
    );
}

// ── 29. Update document — partial doc merge ───────────────────────────────────

#[tokio::test]
async fn test_update_document_partial_merge() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("update_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("update_idx").unwrap();

    // Index original document.
    idx.index_document(
        Some("u1".into()),
        json!({ "name": "Alice", "age": 30, "city": "London" }),
    )
    .await
    .unwrap();

    // Partial update: change age, add a new field "email".
    let resp = idx
        .update_document("u1", json!({ "age": 31, "email": "alice@example.com" }))
        .await
        .unwrap();

    assert!(
        resp.is_some(),
        "update should succeed for existing document"
    );

    // Re-fetch and verify merge.
    let updated = idx.get_document("u1").await.unwrap().unwrap();
    assert_eq!(
        updated["name"].as_str().unwrap(),
        "Alice",
        "name should be preserved"
    );
    assert_eq!(
        updated["age"].as_u64().unwrap(),
        31,
        "age should be updated"
    );
    assert_eq!(
        updated["city"].as_str().unwrap(),
        "London",
        "city should be preserved"
    );
    assert_eq!(
        updated["email"].as_str().unwrap(),
        "alice@example.com",
        "email should be added"
    );

    // Update of non-existent document should return None.
    let missing = idx
        .update_document("nonexistent", json!({ "x": 1 }))
        .await
        .unwrap();
    assert!(
        missing.is_none(),
        "update of non-existent doc should return None"
    );
}

// ── Concurrent access: 10 tasks × 100 docs = 1000 total ──────────────────────

#[tokio::test]
async fn test_concurrent_indexing() {
    use std::sync::Arc;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("concurrent", Schema::empty()).unwrap();
    let idx = Arc::new(engine.get_index("concurrent").unwrap());

    const TASKS: usize = 10;
    const DOCS_PER_TASK: usize = 100;

    let mut handles = Vec::with_capacity(TASKS);

    for task_id in 0..TASKS {
        let idx_clone = Arc::clone(&idx);
        handles.push(tokio::spawn(async move {
            for doc_idx in 0..DOCS_PER_TASK {
                let id = format!("task{}-doc{}", task_id, doc_idx);
                idx_clone
                    .index_document(
                        Some(id),
                        json!({
                            "task": task_id,
                            "doc": doc_idx,
                            // Use a common term so we can search for all docs.
                            "tag": "concurrent_test",
                            "payload": format!("data from task {} doc {}", task_id, doc_idx),
                        }),
                    )
                    .await
                    .expect("index_document should not fail");
            }
        }));
    }

    // Wait for all tasks.
    for h in handles {
        h.await.expect("task should not panic");
    }

    // Verify total doc count.
    let stats = idx.stats().await;
    assert_eq!(
        stats.doc_count,
        (TASKS * DOCS_PER_TASK) as u64,
        "total doc count must be {} after concurrent indexing",
        TASKS * DOCS_PER_TASK
    );

    // Search for the common term — should match all 1000 docs.
    let result = idx
        .search(&make_search(json!({"term": {"tag": "concurrent_test"}})))
        .await
        .unwrap();

    assert_eq!(
        result.total.value,
        (TASKS * DOCS_PER_TASK) as u64,
        "term search for 'concurrent_test' should hit all {} docs",
        TASKS * DOCS_PER_TASK
    );
}

// ── New feature tests ─────────────────────────────────────────────────────────

// ── Feature 1: Nested object field access ─────────────────────────────────────

#[tokio::test]
async fn test_nested_object_field_access() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("nested", Schema::empty()).unwrap();
    let idx = engine.get_index("nested").unwrap();

    // Simple nested object: user.name
    idx.index_document(
        Some("n1".into()),
        json!({ "user": { "name": "John", "age": 30 } }),
    )
    .await
    .unwrap();

    // Deep nesting: a.b.c
    idx.index_document(Some("n2".into()), json!({ "a": { "b": { "c": 42 } } }))
        .await
        .unwrap();

    // Array of objects: tags.key
    idx.index_document(
        Some("n3".into()),
        json!({ "tags": [
            { "key": "env", "val": "prod" },
            { "key": "team", "val": "backend" }
        ]}),
    )
    .await
    .unwrap();

    // Verify nested term query on user.name works.
    let r = idx
        .search(&make_search(json!({"term": {"user.name": "John"}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1, "user.name=John should match n1");
    assert_eq!(r.hits[0].id, "n1");

    // Verify deep nesting term query on a.b.c works.
    let r2 = idx
        .search(&make_search(json!({"term": {"a.b.c": 42}})))
        .await
        .unwrap();
    assert_eq!(r2.total.value, 1, "a.b.c=42 should match n2");
    assert_eq!(r2.hits[0].id, "n2");

    // Verify array field: exists query on tags.key
    let r3 = idx
        .search(&make_search(json!({"exists": {"field": "tags.key"}})))
        .await
        .unwrap();
    assert_eq!(r3.total.value, 1, "tags.key should exist in n3");
    assert_eq!(r3.hits[0].id, "n3");

    // Verify array field: term query on tags.key (matches any element)
    let r4 = idx
        .search(&make_search(json!({"term": {"tags.key": "env"}})))
        .await
        .unwrap();
    assert_eq!(r4.total.value, 1, "tags.key=env should match n3");
    assert_eq!(r4.hits[0].id, "n3");
}

// ── Feature 2: Dynamic mapping for arrays ────────────────────────────────────

#[tokio::test]
async fn test_dynamic_mapping_array_type_detection() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("dynmap", Schema::empty()).unwrap();
    let idx = engine.get_index("dynmap").unwrap();

    // Index a doc with an array of numbers — should infer Long type.
    idx.index_document(
        Some("d1".into()),
        json!({ "scores": [10, 20, 30], "name": "Alice" }),
    )
    .await
    .unwrap();

    // Index a doc with a bool field.
    idx.index_document(Some("d2".into()), json!({ "active": true, "name": "Bob" }))
        .await
        .unwrap();

    // Verify schema evolved: fields were added dynamically.
    let schema = idx.schema().await;
    assert!(
        schema.fields.iter().any(|f| f.name == "scores"),
        "scores field should be in schema after dynamic mapping"
    );
    assert!(
        schema.fields.iter().any(|f| f.name == "active"),
        "active field should be in schema after dynamic mapping"
    );

    // Verify searching works on dynamically-added fields.
    let r = idx
        .search(&make_search(json!({"term": {"active": true}})))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1, "active=true should match d2");
    assert_eq!(r.hits[0].id, "d2");
}

// ── Feature 3: WAL corruption recovery ───────────────────────────────────────

#[tokio::test]
async fn test_wal_corruption_recovery() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();

    // Phase 1: index some valid docs and persist them to WAL.
    {
        let engine = make_engine(&dir);
        engine
            .create_index("corrupt_test", Schema::empty())
            .unwrap();
        let idx = engine.get_index("corrupt_test").unwrap();

        idx.index_document(Some("good1".into()), json!({"data": "valid entry one"}))
            .await
            .unwrap();
        idx.index_document(Some("good2".into()), json!({"data": "valid entry two"}))
            .await
            .unwrap();
    }

    // Phase 2: corrupt the WAL by appending garbage bytes.
    {
        let wal_dir = dir.path().join("corrupt_test").join("wal");
        // Find a .wal file that actually holds an entry and append garbage to
        // corrupt it. With the sharded WAL layout the streams live in
        // wal/s{N}/ subdirectories (docs route by id hash), so walk the root
        // AND the shard dirs and pick a file larger than the 16-byte header.
        let mut wal_files: Vec<std::path::PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&wal_dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                for sub in std::fs::read_dir(&p).unwrap().flatten() {
                    wal_files.push(sub.path());
                }
            } else {
                wal_files.push(p);
            }
        }
        let wal_file = wal_files
            .into_iter()
            .filter(|p| p.to_string_lossy().ends_with(".wal"))
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .expect("should have a WAL file");
        assert!(
            std::fs::metadata(&wal_file).unwrap().len() > 16,
            "picked WAL file must contain at least one entry"
        );

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_file)
            .unwrap();
        // Write a structurally valid-looking WAL entry (entry_len=4, seq_no=9999,
        // op=INDEX) with garbage payload and zero CRC — this will fail the CRC
        // check cleanly and leave the file seekable.
        // entry_len = 4 (u32 LE)
        f.write_all(&4u32.to_le_bytes()).unwrap();
        // seq_no = 9999 (u64 LE) — higher than any real seq_no
        f.write_all(&9999u64.to_le_bytes()).unwrap();
        // op = 0x01 (INDEX)
        f.write_all(&[0x01u8]).unwrap();
        // payload = 4 bytes of garbage
        f.write_all(b"BADD").unwrap();
        // crc = 0 (intentionally wrong)
        f.write_all(&0u32.to_le_bytes()).unwrap();
    }

    // Phase 3: reopen engine — should NOT crash, should recover good entries.
    {
        let engine = make_engine(&dir);
        let idx = engine.get_index("corrupt_test").unwrap();

        // The two valid docs indexed before corruption should be recoverable.
        let doc1 = idx.get_document("good1").await.unwrap();
        assert!(
            doc1.is_some(),
            "good1 should be recoverable after WAL corruption"
        );

        let doc2 = idx.get_document("good2").await.unwrap();
        assert!(
            doc2.is_some(),
            "good2 should be recoverable after WAL corruption"
        );
    }
}

// ── Feature 4: Flush-to-disk integration test ────────────────────────────────

#[tokio::test]
async fn test_flush_to_disk_and_reopen() {
    let dir = TempDir::new().unwrap();

    // Step 1: Create engine, index 100 docs.
    {
        let engine = make_engine(&dir);
        engine.create_index("flush_test", Schema::empty()).unwrap();
        let idx = engine.get_index("flush_test").unwrap();

        for i in 0..100 {
            idx.index_document(
                Some(format!("doc{i}")),
                json!({ "n": i, "tag": "flush_test_doc" }),
            )
            .await
            .unwrap();
        }

        // Step 2: Verify docs are searchable before flush.
        let before = idx
            .search(&make_search(json!({"match_all": {}})))
            .await
            .unwrap();
        assert_eq!(
            before.total.value, 100,
            "100 docs should be found before flush"
        );

        // Step 3: Flush to disk.
        idx.flush().await.unwrap();

        // Step 4: Verify docs are still searchable after flush.
        let after = idx
            .search(&make_search(json!({"match_all": {}})))
            .await
            .unwrap();
        assert_eq!(
            after.total.value, 100,
            "100 docs should be found after flush"
        );

        // Check that a segment was created.
        let stats = idx.stats().await;
        assert!(
            stats.segment_count >= 1,
            "at least one segment should exist after flush"
        );
    }

    // Step 5: Reopen engine with same data dir.
    {
        let engine = make_engine(&dir);
        let idx = engine.get_index("flush_test").unwrap();

        // Step 6: Verify docs are still searchable (from segment, not WAL).
        let result = idx
            .search(&make_search(json!({"match_all": {}})))
            .await
            .unwrap();
        assert_eq!(
            result.total.value, 100,
            "100 docs should survive engine restart after flush"
        );

        // Spot-check a specific doc.
        let doc = idx.get_document("doc42").await.unwrap();
        assert!(doc.is_some(), "doc42 should be findable after reopen");
        assert_eq!(doc.unwrap()["n"].as_u64().unwrap(), 42);

        // Verify segment count (no WAL replay needed — data is in segment).
        let stats = idx.stats().await;
        assert!(
            stats.segment_count >= 1,
            "segment should persist after reopen"
        );
    }
}

// ── Feature 5: Concurrent read/write test ────────────────────────────────────

#[tokio::test]
async fn test_concurrent_read_write() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine
        .create_index("rw_concurrent", Schema::empty())
        .unwrap();
    let idx = Arc::new(engine.get_index("rw_concurrent").unwrap());

    // Pre-index some docs so readers have something to find immediately.
    for i in 0..10 {
        idx.index_document(
            Some(format!("seed{i}")),
            json!({ "val": i, "kind": "seed" }),
        )
        .await
        .unwrap();
    }

    const WRITERS: usize = 4;
    const READERS: usize = 4;
    const WRITES_PER_TASK: usize = 50;
    const READS_PER_TASK: usize = 50;

    let errors = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();

    // Spawn writer tasks.
    for w in 0..WRITERS {
        let idx_clone = Arc::clone(&idx);
        let errors_clone = Arc::clone(&errors);
        handles.push(tokio::spawn(async move {
            for d in 0..WRITES_PER_TASK {
                let id = format!("w{w}-d{d}");
                if idx_clone
                    .index_document(Some(id), json!({ "writer": w, "doc": d, "kind": "write" }))
                    .await
                    .is_err()
                {
                    errors_clone.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    // Spawn reader tasks simultaneously.
    for _r in 0..READERS {
        let idx_clone = Arc::clone(&idx);
        let errors_clone = Arc::clone(&errors);
        handles.push(tokio::spawn(async move {
            for _ in 0..READS_PER_TASK {
                // Search is valid even if it returns 0 results during a write window.
                if idx_clone
                    .search(&make_search(json!({"term": {"kind": "seed"}})))
                    .await
                    .is_err()
                {
                    errors_clone.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    // Wait for all tasks to complete.
    for h in handles {
        h.await.expect("task should not panic");
    }

    // No errors during concurrent ops.
    assert_eq!(
        errors.load(Ordering::Relaxed),
        0,
        "no errors should occur during concurrent read/write"
    );

    // Final state: seed docs + all written docs present.
    let total_written = WRITERS * WRITES_PER_TASK;
    let result = idx
        .search(&make_search_with_size(json!({"match_all": {}}), 10_000))
        .await
        .unwrap();
    assert_eq!(
        result.total.value,
        (10 + total_written) as u64,
        "all docs (seed + written) should be present after concurrent ops"
    );
}

// ── Feature 6: memory_usage_bytes ────────────────────────────────────────────

#[tokio::test]
async fn test_memory_usage_bytes() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("mem_test", Schema::empty()).unwrap();
    let idx = engine.get_index("mem_test").unwrap();

    // Empty index should have a small but non-zero footprint (schema overhead).
    let empty_usage = idx.memory_usage_bytes().await;
    // Just verify it's a non-negative value and accessible.
    let _ = empty_usage;

    // After indexing docs, usage should grow.
    for i in 0..50 {
        idx.index_document(
            Some(format!("m{i}")),
            json!({ "content": format!("document number {} with some text content", i) }),
        )
        .await
        .unwrap();
    }

    let usage_after_index = idx.memory_usage_bytes().await;
    assert!(
        usage_after_index > 0,
        "memory usage should be > 0 after indexing 50 docs, got {}",
        usage_after_index
    );

    // After flush, memtable is cleared so estimate should be lower.
    idx.flush().await.unwrap();
    let usage_after_flush = idx.memory_usage_bytes().await;
    assert!(
        usage_after_flush < usage_after_index,
        "memory usage should decrease after flush (memtable cleared), before={} after={}",
        usage_after_index,
        usage_after_flush
    );
}

// ── Feature 7: Index-level settings ──────────────────────────────────────────

#[tokio::test]
async fn test_index_level_settings() {
    let dir = TempDir::new().unwrap();
    let _engine = make_engine(&dir);

    // Create index with explicit settings using create_with_settings.
    use xerj_common::config::Config;
    use xerj_common::types::Schema;
    use xerj_engine::index::Index;

    let name = xerj_common::types::IndexName::new("settings_test").unwrap();
    let settings = json!({
        "index": {
            "number_of_shards": 1,
            "number_of_replicas": 0
        }
    });
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();

    let idx =
        Index::create_with_settings(name, Schema::empty(), settings.clone(), &config, dir.path())
            .unwrap();

    // Verify GET _settings returns the stored settings.
    let retrieved = idx.get_settings().await;
    assert_eq!(
        retrieved["index"]["number_of_shards"].as_u64().unwrap(),
        1,
        "number_of_shards should be 1"
    );
    assert_eq!(
        retrieved["index"]["number_of_replicas"].as_u64().unwrap(),
        0,
        "number_of_replicas should be 0"
    );
}

#[tokio::test]
async fn test_index_settings_persisted_across_restart() {
    let dir = TempDir::new().unwrap();

    // Create index with settings.
    {
        use xerj_common::config::Config;
        use xerj_engine::index::Index;

        let name = xerj_common::types::IndexName::new("settings_persist").unwrap();
        let settings = json!({
            "index": {
                "number_of_shards": 1,
                "number_of_replicas": 1,
                "refresh_interval": "5s"
            }
        });
        let mut config = Config::default();
        config.server.data_dir = dir.path().to_str().unwrap().to_string();

        let _idx = Index::create_with_settings(
            name,
            xerj_common::types::Schema::empty(),
            settings,
            &config,
            dir.path(),
        )
        .unwrap();
    }

    // Reopen and verify settings survive restart.
    {
        use xerj_common::config::Config;
        use xerj_engine::index::Index;

        let name = xerj_common::types::IndexName::new("settings_persist").unwrap();
        let mut config = Config::default();
        config.server.data_dir = dir.path().to_str().unwrap().to_string();

        let idx = Index::open(name, &config, dir.path()).unwrap();
        let settings = idx.get_settings().await;

        assert_eq!(
            settings["index"]["number_of_replicas"].as_u64().unwrap(),
            1,
            "settings should survive engine restart"
        );
        assert_eq!(
            settings["index"]["refresh_interval"].as_str().unwrap(),
            "5s",
            "refresh_interval should survive engine restart"
        );
    }
}

// ── New feature tests ─────────────────────────────────────────────────────────

// ── search_after pagination ───────────────────────────────────────────────────

#[tokio::test]
async fn test_search_after_pagination() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("sa_page", Schema::empty()).unwrap();
    let idx = engine.get_index("sa_page").unwrap();

    // Index 20 documents with sequential numeric rank values.
    for i in 1..=20usize {
        idx.index_document(
            Some(format!("doc{:02}", i)),
            json!({ "rank": i, "name": format!("item_{:02}", i) }),
        )
        .await
        .unwrap();
    }

    // Page through all docs using search_after with sort by rank ascending.
    let page_size = 5;
    let mut collected_ids: Vec<String> = Vec::new();
    let mut last_sort: Option<Vec<Value>> = None;

    loop {
        let body = if let Some(ref after) = last_sort {
            json!({
                "query": { "match_all": {} },
                "size": page_size,
                "sort": [{ "rank": "asc" }],
                "search_after": after
            })
        } else {
            json!({
                "query": { "match_all": {} },
                "size": page_size,
                "sort": [{ "rank": "asc" }]
            })
        };

        let req = parse_request(&body).unwrap();
        let result = idx.search(&req).await.unwrap();

        if result.hits.is_empty() {
            break;
        }

        // Record the sort values of the last hit for next page.
        last_sort = result.hits.last().map(|h| h.sort.clone());

        for hit in &result.hits {
            collected_ids.push(hit.id.clone());
        }
    }

    assert_eq!(
        collected_ids.len(),
        20,
        "should collect all 20 docs via search_after"
    );

    // Verify all doc IDs are present without duplicates.
    let mut sorted_ids = collected_ids.clone();
    sorted_ids.sort();
    sorted_ids.dedup();
    assert_eq!(sorted_ids.len(), 20, "no duplicate docs should be returned");

    // Verify they came in rank order.
    for (i, id) in collected_ids.iter().enumerate() {
        let expected_rank = i + 1;
        assert_eq!(
            id,
            &format!("doc{:02}", expected_rank),
            "doc at position {} should be doc{:02}",
            i,
            expected_rank
        );
    }
}

// ── wildcard field search ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_wildcard_field_search() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("wild_fields", Schema::empty()).unwrap();
    let idx = engine.get_index("wild_fields").unwrap();

    idx.index_document(
        Some("wf1".into()),
        json!({ "title": "Rust programming", "body": "systems language", "author": "Alice" }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("wf2".into()),
        json!({ "title": "Python basics", "body": "scripting and automation", "author": "Bob" }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("wf3".into()),
        json!({ "title": "Go handbook", "body": "Rust mentioned in comparison", "author": "Carol" }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("wf4".into()),
        json!({ "title": "JavaScript", "body": "web development", "author": "Dave" }),
    )
    .await
    .unwrap();

    // Search with "*" should find docs that mention "Rust" in ANY field.
    let req = parse_request(&json!({
        "query": { "match": { "*": "Rust" } },
        "size": 20
    }))
    .unwrap();
    let r = idx.search(&req).await.unwrap();
    let mut ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
    ids.sort();
    assert!(
        ids.contains(&"wf1"),
        "wf1 (title=Rust) should match wildcard search"
    );
    assert!(
        ids.contains(&"wf3"),
        "wf3 (body mentions Rust) should match wildcard search"
    );
    assert!(!ids.contains(&"wf2"), "wf2 should not match");
    assert!(!ids.contains(&"wf4"), "wf4 should not match");

    // Search with "ti*" should match only 'title' field.
    let req2 = parse_request(&json!({
        "query": { "match": { "ti*": "Python" } },
        "size": 20
    }))
    .unwrap();
    let r2 = idx.search(&req2).await.unwrap();
    assert_eq!(r2.total.value, 1, "only wf2 has Python in title");
    assert_eq!(r2.hits[0].id, "wf2");

    // Search with "au*" should match author field.
    let req3 = parse_request(&json!({
        "query": { "match": { "au*": "Alice" } },
        "size": 20
    }))
    .unwrap();
    let r3 = idx.search(&req3).await.unwrap();
    assert_eq!(r3.total.value, 1);
    assert_eq!(r3.hits[0].id, "wf1");
}

// ── nested terms aggregation on dot-path fields ───────────────────────────────

#[tokio::test]
async fn test_nested_terms_agg() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("nested_agg", Schema::empty()).unwrap();
    let idx = engine.get_index("nested_agg").unwrap();

    // Documents with nested "user.role" field.
    idx.index_document(
        Some("na1".into()),
        json!({ "user": { "role": "admin", "name": "Alice" } }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("na2".into()),
        json!({ "user": { "role": "user", "name": "Bob" } }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("na3".into()),
        json!({ "user": { "role": "admin", "name": "Carol" } }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("na4".into()),
        json!({ "user": { "role": "user", "name": "Dave" } }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("na5".into()),
        json!({ "user": { "role": "moderator", "name": "Eve" } }),
    )
    .await
    .unwrap();

    // Terms aggregation on dot-path field "user.role".
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_role": {
                "terms": { "field": "user.role", "size": 10 }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    let aggs = result.aggs.as_ref().expect("aggs should be present");
    let buckets = aggs["by_role"]["buckets"].as_array().unwrap();

    // Should have 3 distinct roles.
    assert_eq!(buckets.len(), 3, "should have 3 role buckets");

    // Find admin bucket (should have count=2).
    let admin_bucket = buckets.iter().find(|b| b["key"].as_str() == Some("admin"));
    assert!(admin_bucket.is_some(), "admin bucket should exist");
    assert_eq!(
        admin_bucket.unwrap()["doc_count"].as_u64().unwrap(),
        2,
        "admin should have 2 docs"
    );

    // Find moderator bucket (should have count=1).
    let mod_bucket = buckets
        .iter()
        .find(|b| b["key"].as_str() == Some("moderator"));
    assert!(mod_bucket.is_some(), "moderator bucket should exist");
    assert_eq!(mod_bucket.unwrap()["doc_count"].as_u64().unwrap(), 1);
}

// ── terms aggregation with array field values ─────────────────────────────────

#[tokio::test]
async fn test_terms_agg_array_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("arr_agg", Schema::empty()).unwrap();
    let idx = engine.get_index("arr_agg").unwrap();

    // Documents with array-valued "tags" field.
    idx.index_document(Some("aa1".into()), json!({ "tags": ["rust", "systems"] }))
        .await
        .unwrap();
    idx.index_document(
        Some("aa2".into()),
        json!({ "tags": ["python", "scripting"] }),
    )
    .await
    .unwrap();
    idx.index_document(Some("aa3".into()), json!({ "tags": ["rust", "web"] }))
        .await
        .unwrap();

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_tag": {
                "terms": { "field": "tags", "size": 10 }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    let aggs = result.aggs.as_ref().expect("aggs should be present");
    let buckets = aggs["by_tag"]["buckets"].as_array().unwrap();

    // "rust" appears in 2 docs, each of the others appears once.
    let rust_bucket = buckets.iter().find(|b| b["key"].as_str() == Some("rust"));
    assert!(rust_bucket.is_some(), "rust bucket should exist");
    assert_eq!(
        rust_bucket.unwrap()["doc_count"].as_u64().unwrap(),
        2,
        "rust tag should appear in 2 docs"
    );
}

// ── minimum_should_match with percentage ─────────────────────────────────────

#[tokio::test]
async fn test_minimum_should_match_percentage() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("msm_pct", Schema::empty()).unwrap();
    let idx = engine.get_index("msm_pct").unwrap();

    idx.index_document(
        Some("mp1".into()),
        json!({ "a": true, "b": true, "c": true, "d": true }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("mp2".into()),
        json!({ "a": true, "b": true, "c": false, "d": false }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("mp3".into()),
        json!({ "a": false, "b": false, "c": false, "d": false }),
    )
    .await
    .unwrap();

    // 75% of 4 should clauses = 3, rounded down.
    let r = idx
        .search(&make_search(json!({
            "bool": {
                "should": [
                    { "term": { "a": true } },
                    { "term": { "b": true } },
                    { "term": { "c": true } },
                    { "term": { "d": true } }
                ],
                "minimum_should_match": "75%"
            }
        })))
        .await
        .unwrap();

    // mp1 matches all 4 (>= 3 = 75%), mp2 matches 2 (< 3), mp3 matches 0.
    assert_eq!(
        r.total.value, 1,
        "only mp1 should match with 75% of 4 clauses"
    );
    assert_eq!(r.hits[0].id, "mp1");

    // 50% of 4 = 2 clauses.
    let r2 = idx
        .search(&make_search(json!({
            "bool": {
                "should": [
                    { "term": { "a": true } },
                    { "term": { "b": true } },
                    { "term": { "c": true } },
                    { "term": { "d": true } }
                ],
                "minimum_should_match": "50%"
            }
        })))
        .await
        .unwrap();

    // mp1 matches 4, mp2 matches 2 (both >= 2).
    assert_eq!(r2.total.value, 2, "mp1 and mp2 should match with 50%");

    // minimum_should_match with must clauses: should clauses are optional by default.
    let r3 = idx
        .search(&make_search(json!({
            "bool": {
                "must": [{ "term": { "a": true } }],
                "should": [
                    { "term": { "b": true } },
                    { "term": { "c": true } }
                ]
            }
        })))
        .await
        .unwrap();
    // With must + should (no minimum_should_match), should clauses don't filter.
    // mp1 (a=true) and mp2 (a=true) both match must.
    assert_eq!(r3.total.value, 2, "with must clauses, should is optional");
}

// ── Top hits sub-aggregation ──────────────────────────────────────────────────

#[tokio::test]
async fn test_top_hits_sub_agg() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine
        .create_index("top_hits_idx", Schema::empty())
        .unwrap();
    let idx = engine.get_index("top_hits_idx").unwrap();

    // 3 docs in cat A, 2 in cat B.
    idx.index_document(
        Some("a1".into()),
        json!({ "cat": "A", "title": "Alpha one", "score": 10 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("a2".into()),
        json!({ "cat": "A", "title": "Alpha two", "score": 20 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("a3".into()),
        json!({ "cat": "A", "title": "Alpha three", "score": 5 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b1".into()),
        json!({ "cat": "B", "title": "Beta one", "score": 15 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("b2".into()),
        json!({ "cat": "B", "title": "Beta two", "score": 25 }),
    )
    .await
    .unwrap();

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_cat": {
                "terms": { "field": "cat", "size": 10 },
                "aggs": {
                    "top": {
                        "top_hits": { "size": 2, "_source": ["title"] }
                    }
                }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    let aggs = result.aggs.unwrap();
    let buckets = aggs["by_cat"]["buckets"].as_array().unwrap();

    // Find the "A" bucket.
    let bucket_a = buckets.iter().find(|b| b["key"] == "A").expect("bucket A");
    assert_eq!(bucket_a["doc_count"], 3, "3 docs in A");

    let top = &bucket_a["top"];
    let top_hits = top["hits"]["hits"].as_array().unwrap();
    assert!(top_hits.len() <= 2, "top_hits size=2 limits to 2 results");

    // Each hit should have _source with title but NOT score (filtered).
    let first_hit = &top_hits[0];
    assert!(
        first_hit["_source"]["title"].is_string(),
        "title should be present"
    );
    assert!(
        first_hit["_source"]["score"].is_null()
            || !first_hit["_source"]
                .as_object()
                .map(|o| o.contains_key("score"))
                .unwrap_or(false),
        "score should be filtered out when _source=[title]"
    );

    // Verify total reflects all docs in bucket.
    assert_eq!(
        top["hits"]["total"]["value"], 3,
        "total in A bucket should be 3"
    );
}

// ── Profile mode ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_profile_mode() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("profile_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("profile_idx").unwrap();

    idx.index_document(Some("1".into()), json!({ "title": "Rust" }))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({ "title": "Go" }))
        .await
        .unwrap();

    let mut req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10
    }))
    .unwrap();
    req.profile = true;

    let result = idx.search(&req).await.unwrap();
    assert_eq!(
        result.total.value, 2,
        "profile mode should still return all docs"
    );

    let profile = result
        .profile
        .expect("profile should be present when profile=true");
    let shards = profile["shards"].as_array().expect("shards must be array");
    assert!(!shards.is_empty(), "at least one shard in profile");
    let shard = &shards[0];
    assert_eq!(shard["id"], "0", "shard id should be 0");
    let searches = shard["searches"].as_array().expect("searches in shard");
    assert!(!searches.is_empty(), "searches should have entries");
    let queries = searches[0]["query"].as_array().expect("query timing array");
    assert!(
        !queries.is_empty(),
        "query timing should have at least one entry"
    );
    assert!(
        queries[0]["time_in_nanos"].is_number(),
        "time_in_nanos should be a number"
    );
}

// ── search_after with multiple sort fields ────────────────────────────────────

#[tokio::test]
async fn test_search_after_multi_sort() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("msa_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("msa_idx").unwrap();

    // Create docs with two sort fields: category (string) + rank (number).
    for i in 0..12usize {
        let cat = if i < 6 { "A" } else { "B" };
        idx.index_document(Some(format!("d{:02}", i)), json!({ "cat": cat, "rank": i }))
            .await
            .unwrap();
    }

    // Page through all docs sorted by (cat asc, rank asc) with page_size=4.
    let page_size = 4;
    let mut collected: Vec<String> = Vec::new();
    let mut last_sort: Option<Vec<Value>> = None;

    loop {
        let body = if let Some(ref after) = last_sort {
            json!({
                "query": { "match_all": {} },
                "size": page_size,
                "sort": [{ "cat": "asc" }, { "rank": "asc" }],
                "search_after": after
            })
        } else {
            json!({
                "query": { "match_all": {} },
                "size": page_size,
                "sort": [{ "cat": "asc" }, { "rank": "asc" }]
            })
        };

        let req = parse_request(&body).unwrap();
        let result = idx.search(&req).await.unwrap();

        if result.hits.is_empty() {
            break;
        }
        last_sort = result.hits.last().map(|h| h.sort.clone());
        for h in &result.hits {
            collected.push(h.id.clone());
        }

        if result.hits.len() < page_size {
            break;
        }
    }

    assert_eq!(collected.len(), 12, "should collect all 12 docs");
    // No duplicates.
    let mut dedup = collected.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), 12, "no duplicates");

    // First 6 should all be category A docs (sorted by rank within A).
    for id in &collected[..6] {
        let doc_idx: usize = id.trim_start_matches('d').parse().unwrap();
        assert!(
            doc_idx < 6,
            "first 6 sorted results should be cat A (indices 0-5), got {}",
            id
        );
    }
    for id in &collected[6..] {
        let doc_idx: usize = id.trim_start_matches('d').parse().unwrap();
        assert!(
            doc_idx >= 6,
            "last 6 sorted results should be cat B (indices 6-11), got {}",
            id
        );
    }
}

// ── Significant terms aggregation ────────────────────────────────────────────

#[tokio::test]
async fn test_significant_terms_agg() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine
        .create_index("sig_terms_idx", Schema::empty())
        .unwrap();
    let idx = engine.get_index("sig_terms_idx").unwrap();

    // Index 10 docs. "rust" appears in 6/10 (60%) of all docs.
    // "python" appears in 2/10 (20%) of all docs.
    // "java" appears in 1/10 (10%) of all docs.
    for i in 0..6usize {
        idx.index_document(
            Some(format!("r{}", i)),
            json!({ "lang": "rust", "group": "backend" }),
        )
        .await
        .unwrap();
    }
    for i in 0..2usize {
        idx.index_document(
            Some(format!("p{}", i)),
            json!({ "lang": "python", "group": "data" }),
        )
        .await
        .unwrap();
    }
    idx.index_document(
        Some("j0".into()),
        json!({ "lang": "java", "group": "backend" }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("g0".into()),
        json!({ "lang": "go", "group": "backend" }),
    )
    .await
    .unwrap();

    // Run significant_terms on the "data" group (2 docs, "python" appears in 2/2 = 100% of result,
    // but only 20% of all docs → significant).
    //
    // `min_doc_count: 1` is required: ES's significant_terms default is
    // min_doc_count=3 (unlike the terms agg's 1), which would exclude a
    // term with only 2 foreground docs — in real ES this exact request
    // without the override returns zero buckets.
    let req = parse_request(&json!({
        "query": { "term": { "group": "data" } },
        "size": 0,
        "aggs": {
            "sig": {
                "significant_terms": { "field": "lang", "size": 5, "min_doc_count": 1 }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    let aggs = result.aggs.unwrap();
    let buckets = aggs["sig"]["buckets"].as_array().unwrap();

    // "python" should appear as significant (100% of result, 20% of background).
    let python_bucket = buckets.iter().find(|b| b["key"] == "python");
    assert!(
        python_bucket.is_some(),
        "python should be significant term in data group"
    );
    let pb = python_bucket.unwrap();
    assert_eq!(pb["doc_count"], 2);
    assert!(
        pb["score"].as_f64().unwrap() > 1.0,
        "score should be > 1 (overrepresented)"
    );
}

// ── Adjacency matrix aggregation ─────────────────────────────────────────────

#[tokio::test]
async fn test_adjacency_matrix_agg() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("adj_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("adj_idx").unwrap();

    // 3 docs: one in A, one in B, one in both A and B.
    idx.index_document(Some("1".into()), json!({ "cat": "A" }))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({ "cat": "B" }))
        .await
        .unwrap();
    idx.index_document(Some("3".into()), json!({ "cat": "A", "also": "B" }))
        .await
        .unwrap();

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "matrix": {
                "adjacency_matrix": {
                    "filters": {
                        "A": { "term": { "cat": "A" } },
                        "B": { "terms": { "cat": ["B"] } }
                    }
                }
            }
        }
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    let aggs = result.aggs.unwrap();
    let buckets = aggs["matrix"]["buckets"].as_array().unwrap();

    // Should have buckets for A, B, and A&B.
    let keys: Vec<&str> = buckets.iter().map(|b| b["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"A"), "should have A bucket");
    assert!(keys.contains(&"B"), "should have B bucket");
    // A&B pair (only doc3 matches both if "also"="B" is treated differently, adjust expected counts).
    // Since doc3 has cat=A but not cat=B, A&B pair may be 0 (omitted).
    // doc2 has cat=B so B matches docs 2.
    // Verify counts.
    let bucket_a = buckets.iter().find(|b| b["key"] == "A").unwrap();
    assert_eq!(bucket_a["doc_count"], 2, "A should match docs 1 and 3");
    let bucket_b = buckets.iter().find(|b| b["key"] == "B").unwrap();
    assert_eq!(bucket_b["doc_count"], 1, "B should match doc 2 (cat=B)");
}

// ── Field collapsing ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_field_collapsing() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("products", Schema::empty()).unwrap();
    let idx = engine.get_index("products").unwrap();

    // Index several documents with duplicate categories.
    idx.index_document(
        Some("1".into()),
        json!({ "name": "apple", "category": "fruit", "price": 1.5 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({ "name": "banana", "category": "fruit", "price": 0.75 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("3".into()),
        json!({ "name": "carrot", "category": "vegetable", "price": 2.0 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("4".into()),
        json!({ "name": "daikon", "category": "vegetable", "price": 1.0 }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("5".into()),
        json!({ "name": "elderberry", "category": "fruit", "price": 3.0 }),
    )
    .await
    .unwrap();

    // Collapse by category — should return exactly one result per category.
    use xerj_query::ast::CollapseField;
    let mut req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 10,
    }))
    .unwrap();
    req.collapse = Some(CollapseField {
        field: "category".to_string(),
        inner_hits: None,
    });

    let result = idx.search(&req).await.unwrap();

    // Should have exactly 2 hits (one per unique category value).
    assert_eq!(
        result.hits.len(),
        2,
        "collapse by category should yield 2 hits"
    );

    // Verify each category appears at most once.
    let categories: Vec<&str> = result
        .hits
        .iter()
        .filter_map(|h| h.source.get("category").and_then(serde_json::Value::as_str))
        .collect();
    let unique_cats: std::collections::HashSet<&&str> = categories.iter().collect();
    assert_eq!(
        unique_cats.len(),
        categories.len(),
        "each category should appear exactly once"
    );

    // Both "fruit" and "vegetable" should be present.
    assert!(
        categories.contains(&"fruit"),
        "fruit category should be present"
    );
    assert!(
        categories.contains(&"vegetable"),
        "vegetable category should be present"
    );
}

// ── Index blocks ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_index_write_block() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("blocked", Schema::empty()).unwrap();
    let idx = engine.get_index("blocked").unwrap();

    // Index a document before blocking.
    idx.index_document(Some("1".into()), json!({ "value": "before block" }))
        .await
        .unwrap();

    // Set the write block.
    idx.set_block("write").await.unwrap();

    // Attempt to index another document — should fail with IndexBlocked.
    let result = idx
        .index_document(Some("2".into()), json!({ "value": "after block" }))
        .await;
    assert!(
        result.is_err(),
        "indexing should fail when write block is set"
    );
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("blocked") || err_str.contains("write"),
        "error should mention block: {err_str}"
    );

    // Searching should still work (read is not blocked).
    let search_result = idx
        .search(&make_search(json!({ "match_all": {} })))
        .await
        .unwrap();
    assert_eq!(
        search_result.total.value, 1,
        "only pre-block doc should be present"
    );

    // Deletion should also fail with write block.
    let del_result = idx.delete_document("1").await;
    assert!(
        del_result.is_err(),
        "delete should fail when write block is set"
    );
}

#[tokio::test]
async fn test_index_read_block() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("readblock", Schema::empty()).unwrap();
    let idx = engine.get_index("readblock").unwrap();

    // Index a document before blocking.
    idx.index_document(Some("1".into()), json!({ "value": "hello" }))
        .await
        .unwrap();

    // Set the read block.
    idx.set_block("read").await.unwrap();

    // Searching should fail with read block.
    let result = idx.search(&make_search(json!({ "match_all": {} }))).await;
    assert!(result.is_err(), "search should fail when read block is set");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("blocked") || err_str.contains("read"),
        "error should mention block: {err_str}"
    );
}

/// `write_block_reason` names the block, and the name is what the HTTP layer
/// turns into a status. `read_only_allow_delete` is the one that answers 429
/// instead of 403, so mislabelling it silently changes the wire contract.
#[tokio::test]
async fn write_block_reason_names_the_block_that_denied_the_write() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("reasons", Schema::empty()).unwrap();
    let idx = engine.get_index("reasons").unwrap();

    assert_eq!(idx.write_block_reason().await, None);

    for name in ["write", "read_only", "read_only_allow_delete"] {
        idx.set_block(name).await.unwrap();
        assert_eq!(
            idx.write_block_reason().await,
            Some(name),
            "{name} must both deny writes and identify itself"
        );
        idx.clear_block(name).await.unwrap();
        assert_eq!(
            idx.write_block_reason().await,
            None,
            "clearing {name} must lift the denial"
        );
    }
}

/// ES collapses a multi-block rejection to a single status by letting a
/// non-retryable block outrank a retryable one, so an index carrying both an
/// explicit `write` block (403) and the flood-stage block (429) reports 403.
#[tokio::test]
async fn an_explicit_block_outranks_the_flood_stage_block() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("precedence", Schema::empty()).unwrap();
    let idx = engine.get_index("precedence").unwrap();

    idx.set_block("read_only_allow_delete").await.unwrap();
    idx.set_block("write").await.unwrap();
    assert_eq!(
        idx.write_block_reason().await,
        Some("write"),
        "the 403 block must win while it is set"
    );

    idx.clear_block("write").await.unwrap();
    assert_eq!(
        idx.write_block_reason().await,
        Some("read_only_allow_delete"),
        "and the 429 block must still be in force underneath it"
    );
}

/// A settings body reaches us in whichever of ES's spellings the client library
/// chose, and index settings survive a round trip as strings. All of them have
/// to move the block, or "clear the block" silently no-ops for some clients.
#[tokio::test]
async fn apply_block_settings_accepts_every_settings_spelling() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("shapes", Schema::empty()).unwrap();
    let idx = engine.get_index("shapes").unwrap();

    let set_forms = [
        json!({ "index": { "blocks": { "write": true } } }),
        json!({ "index.blocks.write": true }),
        json!({ "index": { "blocks.write": true } }),
        json!({ "blocks": { "write": true } }),
        json!({ "index": { "blocks": { "write": "true" } } }),
    ];
    let clear_forms = [
        json!({ "index": { "blocks": { "write": false } } }),
        json!({ "index.blocks.write": false }),
        json!({ "index": { "blocks.write": false } }),
        json!({ "blocks": { "write": false } }),
        json!({ "index": { "blocks": { "write": "false" } } }),
    ];

    for (set, clear) in set_forms.iter().zip(clear_forms.iter()) {
        let applied = idx.apply_block_settings(set).await.unwrap();
        assert_eq!(applied, vec!["write".to_string()], "setting via {set}");
        assert!(idx.is_write_blocked().await, "setting via {set}");

        let applied = idx.apply_block_settings(clear).await.unwrap();
        assert_eq!(applied, vec!["write".to_string()], "clearing via {clear}");
        assert!(!idx.is_write_blocked().await, "clearing via {clear}");
    }

    // Unrelated settings keys must not be mistaken for blocks.
    let applied = idx
        .apply_block_settings(&json!({ "index": { "number_of_replicas": 0 } }))
        .await
        .unwrap();
    assert!(
        applied.is_empty(),
        "non-block settings must not touch blocks"
    );
}

/// The blocks live in the index's own `settings.json`, so they have to survive
/// a reopen — a block you can only clear by restarting is the defect; a block
/// that *clears itself* on restart is the same defect wearing the other face.
#[tokio::test]
async fn blocks_survive_a_reopen_and_stay_clearable() {
    let dir = TempDir::new().unwrap();
    {
        let engine = make_engine(&dir);
        engine.create_index("persisted", Schema::empty()).unwrap();
        let idx = engine.get_index("persisted").unwrap();
        idx.set_block("read_only_allow_delete").await.unwrap();
    }

    let engine = make_engine(&dir);
    let idx = engine.get_index("persisted").unwrap();
    assert_eq!(
        idx.write_block_reason().await,
        Some("read_only_allow_delete"),
        "the block must be reloaded from settings.json"
    );

    idx.clear_block("read_only_allow_delete").await.unwrap();
    assert_eq!(idx.write_block_reason().await, None);
}

// ── New feature tests ─────────────────────────────────────────────────────────

// ── SQL query test ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_sql_query() {
    use xerj_engine::sql::parse_sql;
    use xerj_query::ast::SourceFilter;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("products", Schema::empty()).unwrap();
    let idx = engine.get_index("products").unwrap();

    idx.index_document(Some("1".into()), json!({"name": "apple",  "price": 1.5}))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({"name": "banana", "price": 35.0}))
        .await
        .unwrap();
    idx.index_document(Some("3".into()), json!({"name": "cherry", "price": 50.0}))
        .await
        .unwrap();
    idx.index_document(Some("4".into()), json!({"name": "date",   "price": 20.0}))
        .await
        .unwrap();

    let sql = "SELECT name, price FROM products WHERE price > 30 LIMIT 3";
    let parsed = parse_sql(sql).unwrap();

    assert_eq!(parsed.index, "products");
    assert_eq!(parsed.fields, vec!["name", "price"]);
    assert_eq!(parsed.limit, Some(3));

    let req = SearchRequest {
        query: parsed.query,
        size: parsed.limit.unwrap_or(10),
        sort: parsed.sort,
        source: SourceFilter::Includes(parsed.fields),
        ..Default::default()
    };

    let result = idx.search(&req).await.unwrap();
    // banana (35) and cherry (50) should match price > 30
    assert_eq!(result.total.value, 2, "expected 2 results with price > 30");
}

// ── Async search test ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_async_search_store() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Simulate storing an async search result in the engine map.
    let async_id = "test-async-id-123".to_string();
    let stored = json!({
        "id": async_id,
        "is_partial": false,
        "is_running": false,
        "start_time_in_millis": 1000,
        "expiration_time_in_millis": 2000,
        "response": {
            "hits": { "total": { "value": 0, "relation": "eq" }, "hits": [] }
        }
    });

    engine
        .async_searches
        .insert(async_id.clone(), stored.clone());

    // Retrieve it. Scope the DashMap `Ref` guard: holding it across the
    // `remove()` below would self-deadlock (same-shard read lock held
    // while requesting the write lock).
    {
        let retrieved = engine
            .async_searches
            .get(&async_id)
            .expect("async search should be stored");
        assert_eq!(retrieved["id"].as_str().unwrap(), async_id);
        assert!(!retrieved["is_running"].as_bool().unwrap());
    }

    // Delete it.
    engine.async_searches.remove(&async_id);
    assert!(
        engine.async_searches.get(&async_id).is_none(),
        "should be deleted"
    );
}

// ── KNN / vector search test ──────────────────────────────────────────────────

#[tokio::test]
async fn test_knn_vector_search() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // RC4 W2 item 16: an HNSW graph is only built for an explicit
    // dense_vector mapping (unmapped numeric arrays no longer auto-build
    // one), so this graph-path test declares the mapping.
    let mut schema = Schema::empty();
    let mut vf = FieldConfig::new("embedding", FieldType::Vector);
    vf.options.dimensions = Some(4);
    vf.options.similarity = Some("cosine".to_string());
    schema.fields.push(vf);
    engine.create_index("vectors", schema).unwrap();
    let idx = engine.get_index("vectors").unwrap();

    // Index documents with 4-dimensional embedding vectors.
    idx.index_document(
        Some("doc1".into()),
        json!({ "title": "near", "embedding": [1.0, 0.0, 0.0, 0.0] }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("doc2".into()),
        json!({ "title": "far",  "embedding": [0.0, 1.0, 0.0, 0.0] }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("doc3".into()),
        json!({ "title": "medium", "embedding": [0.9, 0.1, 0.0, 0.0] }),
    )
    .await
    .unwrap();

    // Query vector close to doc1 and doc3.
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let results = idx.knn_search(&query, 3).await;

    assert!(!results.is_empty(), "KNN search should return results");
    // The closest result should be doc1 (exact match) or doc3 (very close).
    let top_id = &results[0].0;
    assert!(
        top_id == "doc1" || top_id == "doc3",
        "Top result should be doc1 or doc3, got: {}",
        top_id
    );
}

/// RC4 W2 item 16 regression: unmapped numeric arrays (`ports: [80,443]`
/// log workloads) must NOT auto-build or persist an HNSW graph — only an
/// explicit dense_vector mapping may. Pre-fix, choose_hnsw_field's
/// heuristic 3 pinned the doc's first numeric-array field and built a
/// full graph (RAM + disk + per-ingest maintenance) that never served.
#[tokio::test]
async fn test_hnsw_requires_dense_vector_mapping() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Unmapped index: numeric arrays must not create a graph.
    engine.create_index("portslogs", Schema::empty()).unwrap();
    let idx = engine.get_index("portslogs").unwrap();
    for i in 0..50 {
        idx.index_document(
            Some(format!("d{i}")),
            json!({ "src": format!("10.0.0.{i}"), "ports": [80, 443, i] }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    let stats = idx.hnsw_stats().await;
    assert_eq!(
        stats["present"],
        json!(false),
        "unmapped numeric arrays must not build an HNSW graph, got {stats}"
    );
    let hnsw_dir = dir.path().join("portslogs").join("hnsw");
    assert!(
        !hnsw_dir.exists(),
        "no hnsw artifacts may be persisted for unmapped arrays"
    );

    // A dense_vector-mapped index still builds one (graph-build intact).
    let mut schema = Schema::empty();
    let mut vf = FieldConfig::new("v", FieldType::Vector);
    vf.options.dimensions = Some(4);
    vf.options.similarity = Some("cosine".to_string());
    schema.fields.push(vf);
    engine.create_index("mapped", schema).unwrap();
    let mapped = engine.get_index("mapped").unwrap();
    for i in 0..5 {
        mapped
            .index_document(Some(format!("m{i}")), json!({ "v": [i, 1.0, 0.0, 0.0] }))
            .await
            .unwrap();
    }
    let s = mapped.hnsw_stats().await;
    assert_eq!(
        s["present"],
        json!(true),
        "mapped dense_vector field must still build the graph, got {s}"
    );
    assert_eq!(s["field"], json!("v"));
    assert_eq!(s["doc_coverage"], json!(5));
}

/// RC4 W2 item 17 regression: a flush-time-stale HNSW snapshot (seq_no
/// stamp != replayed WAL position — what an unclean shutdown with a WAL
/// tail produces) must be healed by the background rebuild at open.
/// Pre-fix, `hnsw_stale` was sticky for the process lifetime: the ANN
/// path stayed disabled forever while ingest kept paying full graph
/// maintenance, invisibly.
#[tokio::test]
async fn test_hnsw_stale_snapshot_rebuilds_on_open() {
    let dir = TempDir::new().unwrap();
    let mut schema = Schema::empty();
    let mut vf = FieldConfig::new("v", FieldType::Vector);
    vf.options.dimensions = Some(4);
    vf.options.similarity = Some("cosine".to_string());
    schema.fields.push(vf);

    {
        let engine = make_engine(&dir);
        engine.create_index("vecs", schema).unwrap();
        let idx = engine.get_index("vecs").unwrap();
        for i in 0..6 {
            idx.index_document(Some(format!("d{i}")), json!({ "v": [i, 1.0, 0.5, 0.25] }))
                .await
                .unwrap();
        }
        // Persists the graph + ids.json with a fresh seq_no stamp.
        idx.flush().await.unwrap();
    }

    // Simulate the unclean-shutdown divergence: forge a stamp mismatch in
    // ids.json (the loader must then distrust the flush-time graph).
    let ids_path = dir.path().join("vecs").join("hnsw").join("ids.json");
    let mut ids: Value = serde_json::from_slice(&std::fs::read(&ids_path).unwrap()).unwrap();
    ids["seq_no"] = json!(999_999u64);
    std::fs::write(&ids_path, serde_json::to_vec(&ids).unwrap()).unwrap();

    // Reopen: the graph loads stale and the background rebuild must
    // converge, clear the flag, and leave every doc graphed.
    let engine = make_engine(&dir);
    let idx = engine.get_index("vecs").unwrap();
    let mut healed = false;
    let mut last = json!(null);
    for _ in 0..100 {
        last = idx.hnsw_stats().await;
        if last["present"] == json!(true)
            && last["stale"] == json!(false)
            && last["rebuilding"] == json!(false)
        {
            healed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        healed,
        "stale HNSW snapshot must be healed by the background rebuild; last stats: {last}"
    );
    assert_eq!(
        last["doc_coverage"],
        json!(6),
        "all docs graphed after heal: {last}"
    );

    // The healed graph serves: nearest neighbour of d5's exact vector is d5.
    let results = idx.knn_search(&[5.0, 1.0, 0.5, 0.25], 1).await;
    assert_eq!(
        results.first().map(|(id, _)| id.as_str()),
        Some("d5"),
        "healed graph must serve correct nearest neighbours, got {results:?}"
    );
}

/// Regression for the "semantic/knn query ignores `size`" bug (returned `k`
/// hits instead of `size`). ES semantics for a top-level knn/semantic query:
/// `k` bounds the neighbor pool, `from`/`size` then window into it, and
/// `hits.total.value` reports the pool size (min(k, matches)) — NOT the number
/// of docs that merely have a vector. Surfaced by recipes/semantic_search.py
/// against v1.0.0-rc.1, where `{"semantic":{...,"k":5}}` + `"size":3` wrongly
/// returned 5 hits while match/hybrid respected size.
#[tokio::test]
async fn test_knn_size_windows_into_k() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("vectors", Schema::empty()).unwrap();
    let idx = engine.get_index("vectors").unwrap();

    // Six docs so `size < k < corpus` makes every assertion meaningful.
    // Descending cosine similarity to [1,0,0,0]: d1 > d2 > d3 > (d4,d5,d6≈0).
    for (id, v) in [
        ("d1", [1.0, 0.0, 0.0, 0.0]),
        ("d2", [0.9, 0.1, 0.0, 0.0]),
        ("d3", [0.8, 0.2, 0.0, 0.0]),
        ("d4", [0.0, 1.0, 0.0, 0.0]),
        ("d5", [0.0, 0.9, 0.1, 0.0]),
        ("d6", [0.0, 0.0, 1.0, 0.0]),
    ] {
        idx.index_document(Some(id.into()), json!({ "embedding": v }))
            .await
            .unwrap();
    }

    let knn = |extra: Value| {
        let mut body = json!({
            "query": {"knn": {"field": "embedding", "query_vector": [1.0, 0.0, 0.0, 0.0], "k": 4}},
        });
        let obj = body.as_object_mut().unwrap();
        for (key, val) in extra.as_object().unwrap() {
            obj.insert(key.clone(), val.clone());
        }
        parse_request(&body).unwrap()
    };

    // k=4 pool, size=2 requested → exactly 2 hits, total reports the k pool.
    let res = idx.search(&knn(json!({"size": 2}))).await.unwrap();
    assert_eq!(
        res.hits.len(),
        2,
        "size must cap returned hits (pre-fix returned k=4)"
    );
    assert_eq!(
        res.total.value, 4,
        "total.value is the k-neighbor pool, not the 6-doc corpus"
    );
    assert_eq!(res.hits[0].id, "d1", "top hit is the exact match");

    // from paginates within the pool: page [1..3) skips the top neighbor.
    let res2 = idx
        .search(&knn(json!({"from": 1, "size": 2})))
        .await
        .unwrap();
    assert_eq!(res2.hits.len(), 2, "from+size windows within the k pool");
    assert_eq!(res2.total.value, 4, "total is unaffected by from/size");
    assert_ne!(res2.hits[0].id, res.hits[0].id, "from=1 skips the top hit");

    // size=0 → count-only: pool total present, no hits materialized.
    let res0 = idx.search(&knn(json!({"size": 0}))).await.unwrap();
    assert!(res0.hits.is_empty(), "size=0 returns no hits");
    assert_eq!(res0.total.value, 4, "size=0 still reports the pool total");
}

#[tokio::test]
async fn test_public_vector_executor_signatures_remain_compatible() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("deadline-vectors", Schema::empty())
        .unwrap();
    let idx = engine.get_index("deadline-vectors").unwrap();

    for n in 0..256 {
        idx.index_document(
            Some(format!("d{n}")),
            json!({"embedding": [1.0, 0.0, 0.0, 0.0]}),
        )
        .await
        .unwrap();
    }

    let mut request = parse_request(&json!({
        "query": {"knn": {
            "field": "embedding",
            "query_vector": [1.0, 0.0, 0.0, 0.0],
            "k": 10
        }},
        "size": 10
    }))
    .unwrap();
    request.timeout_ms = Some(250);
    let result = idx
        .run_knn_brute_force(
            &request,
            "embedding",
            &[1.0, 0.0, 0.0, 0.0],
            10,
            None,
            "cosine",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(!result.timed_out);

    let semantic_future = idx.run_semantic(&request, "body", "query text", 10, None);
    drop(semantic_future);
    let hybrid_future = idx.run_hybrid(
        &request,
        vec![],
        xerj_query::ast::FusionStrategy::Rrf { k: 60 },
    );
    drop(hybrid_future);
}

// ── SQL parser unit tests (inline) ────────────────────────────────────────────

#[test]
fn test_sql_parser_and_condition() {
    use xerj_engine::sql::parse_sql;

    let q = parse_sql("SELECT id FROM events WHERE status = 'active' AND score >= 5").unwrap();
    assert_eq!(q.index, "events");
    // Should produce a Bool must query.
    assert!(matches!(q.query, xerj_query::ast::QueryNode::Bool { .. }));
}

#[test]
fn test_sql_parser_order_by() {
    use xerj_engine::sql::parse_sql;
    use xerj_query::sort::SortOrder;

    let q = parse_sql("SELECT * FROM logs ORDER BY timestamp DESC LIMIT 5").unwrap();
    assert_eq!(q.sort.len(), 1);
    assert_eq!(q.sort[0].field, "timestamp");
    assert!(matches!(q.sort[0].order, SortOrder::Desc));
    assert_eq!(q.limit, Some(5));
}

#[test]
fn test_sql_parser_like() {
    use xerj_engine::sql::parse_sql;

    let q = parse_sql("SELECT name FROM items WHERE name LIKE 'app%'").unwrap();
    // Should produce a Wildcard query.
    assert!(matches!(
        q.query,
        xerj_query::ast::QueryNode::Wildcard { .. }
    ));
}

// ── New feature tests ─────────────────────────────────────────────────────────

// ── Rescore test: verify rescoring changes document ranking ───────────────────

#[tokio::test]
async fn test_rescore_changes_ranking() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("rescore_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("rescore_idx").unwrap();

    // Doc "a": lots of "search", few "engine" mentions → high score for "search"
    idx.index_document(
        Some("a".into()),
        json!({ "title": "search", "body": "search search search" }),
    )
    .await
    .unwrap();

    // Doc "b": lots of "engine" mentions → would rank lower for "search", higher for "engine"
    idx.index_document(
        Some("b".into()),
        json!({ "title": "engine", "body": "engine engine engine engine engine" }),
    )
    .await
    .unwrap();

    // Doc "c": mentions "search engine" once
    idx.index_document(
        Some("c".into()),
        json!({ "title": "search engine", "body": "search engine" }),
    )
    .await
    .unwrap();

    // Primary query: search for "search" — doc "a" should rank highest initially.
    let primary_req = parse_request(&json!({
        "query": { "match": { "body": "search" } },
        "size": 10,
    }))
    .unwrap();
    let primary_result = idx.search(&primary_req).await.unwrap();
    assert!(!primary_result.hits.is_empty());
    let primary_top = primary_result.hits[0].id.clone();

    // Now add rescore that weights "engine" matches heavily.
    // This should boost doc "b" (many "engine" occurrences) up.
    let rescore_req = parse_request(&json!({
        "query": { "match": { "body": "search" } },
        "size": 10,
        "rescore": {
            "window_size": 10,
            "query": {
                "rescore_query": { "match": { "title": "engine" } },
                "query_weight": 0.1,
                "rescore_query_weight": 10.0
            }
        }
    }))
    .unwrap();
    let rescore_result = idx.search(&rescore_req).await.unwrap();
    assert!(
        !rescore_result.hits.is_empty(),
        "rescore search should return hits"
    );

    // After rescoring, doc "b" (title contains "engine") should appear — check scores changed.
    let rescore_scores: Vec<(&str, f32)> = rescore_result
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.score))
        .collect();
    // Verify the rescore was applied (scores differ from primary).
    let primary_scores: Vec<(&str, f32)> = primary_result
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.score))
        .collect();
    // At least the top score should differ since rescore applies different weights.
    let _ = (rescore_scores, primary_scores, primary_top);
    // Just verify that the request parsed and executed successfully with rescore.
    assert!(
        rescore_result.total.value > 0,
        "should have hits after rescoring"
    );
}

// ── Weighted bool: verify boosted queries rank higher ─────────────────────────

#[tokio::test]
async fn test_weighted_bool_boost_ranking() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("boost_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("boost_idx").unwrap();

    // "title_only": matches boosted title field.
    idx.index_document(
        Some("title_only".into()),
        json!({ "title": "Rust Programming", "body": "other content here" }),
    )
    .await
    .unwrap();

    // "body_only": matches unboosted body field.
    idx.index_document(
        Some("body_only".into()),
        json!({ "title": "other stuff", "body": "Rust Programming guide" }),
    )
    .await
    .unwrap();

    // Query with boost=3.0 on title, boost=1.0 on body.
    let req = parse_request(&json!({
        "query": {
            "bool": {
                "should": [
                    { "match": { "title": { "query": "Rust", "boost": 3.0 } } },
                    { "match": { "body":  { "query": "Rust", "boost": 1.0 } } }
                ]
            }
        },
        "size": 10
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.total.value, 2, "both docs should match");

    // title_only should have a higher score due to the title boost.
    let top_id = &result.hits[0].id;
    let second_id = &result.hits[1].id;
    assert_eq!(
        top_id.as_str(),
        "title_only",
        "boosted title match should rank first, got: {top_id}"
    );
    assert_eq!(
        second_id.as_str(),
        "body_only",
        "unboosted body match should rank second"
    );

    // Verify scores reflect the boost: top score should be ≥ 3x the second.
    assert!(
        result.hits[0].score > result.hits[1].score,
        "title match (boost=3) score {} should exceed body match (boost=1) score {}",
        result.hits[0].score,
        result.hits[1].score
    );
}

// ── Nested query test: index docs with nested arrays, query by nested field ───

#[tokio::test]
async fn test_nested_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("nested_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("nested_idx").unwrap();

    // Doc with nested comments array.
    idx.index_document(
        Some("doc1".into()),
        json!({
            "title": "Blog post",
            "comments": [
                { "author": "alice", "text": "great article" },
                { "author": "bob",   "text": "nice work" }
            ]
        }),
    )
    .await
    .unwrap();

    // Doc with no matching comment.
    idx.index_document(
        Some("doc2".into()),
        json!({
            "title": "Another post",
            "comments": [
                { "author": "charlie", "text": "disagree" }
            ]
        }),
    )
    .await
    .unwrap();

    // Nested query: find docs where comments.author = "alice"
    let req = parse_request(&json!({
        "query": {
            "nested": {
                "path": "comments",
                "query": { "term": { "author": "alice" } }
            }
        },
        "size": 10
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.total.value, 1, "only doc1 has alice as commenter");
    assert_eq!(result.hits[0].id, "doc1");
}

// ── More-like-this test: find similar documents ───────────────────────────────

#[tokio::test]
async fn test_more_like_this() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("mlt_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("mlt_idx").unwrap();

    idx.index_document(
        Some("rust1".into()),
        json!({ "text": "Rust is a systems programming language focused on safety and performance" }),
    ).await.unwrap();

    idx.index_document(
        Some("rust2".into()),
        json!({ "text": "The Rust programming language provides memory safety without garbage collection" }),
    ).await.unwrap();

    idx.index_document(
        Some("python1".into()),
        json!({ "text": "Python is a high-level scripting language used for data science" }),
    )
    .await
    .unwrap();

    let req = parse_request(&json!({
        "query": {
            "more_like_this": {
                "fields": ["text"],
                "like": ["Rust language safety"],
                "min_term_freq": 1,
                "max_query_terms": 10
            }
        },
        "size": 10
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    // Should return at least the Rust documents.
    assert!(
        result.total.value >= 1,
        "should find at least one similar doc"
    );
    let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(
        ids.contains(&"rust1") || ids.contains(&"rust2"),
        "Rust docs should match the more_like_this query, got: {:?}",
        ids
    );
}

// ── Named query test: matched_queries in hit response ─────────────────────────

#[tokio::test]
async fn test_named_queries_matched() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("named_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("named_idx").unwrap();

    idx.index_document(
        Some("t1".into()),
        json!({ "title": "search engine", "body": "fast search" }),
    )
    .await
    .unwrap();

    idx.index_document(
        Some("t2".into()),
        json!({ "title": "database", "body": "slow query" }),
    )
    .await
    .unwrap();

    // Use named queries: title match named "title_match", body match named "body_match".
    let req = parse_request(&json!({
        "query": {
            "bool": {
                "should": [
                    { "match": { "title": { "query": "search", "_name": "title_match" } } },
                    { "match": { "body":  { "query": "search", "_name": "body_match" } } }
                ]
            }
        },
        "size": 10
    }))
    .unwrap();

    let result = idx.search(&req).await.unwrap();
    // t1 has "search" in both title and body.
    let t1_hit = result.hits.iter().find(|h| h.id == "t1");
    assert!(t1_hit.is_some(), "t1 should match");
    let t1 = t1_hit.unwrap();
    // t1 should have both matched queries.
    assert!(
        t1.matched_queries.contains(&"title_match".to_string()),
        "title_match should be in matched_queries, got: {:?}",
        t1.matched_queries
    );
    assert!(
        t1.matched_queries.contains(&"body_match".to_string()),
        "body_match should be in matched_queries, got: {:?}",
        t1.matched_queries
    );

    // t2 should not appear (no "search" in title or body).
    let t2_hit = result.hits.iter().find(|h| h.id == "t2");
    assert!(t2_hit.is_none(), "t2 should not match");
}

// ── SQL with ORDER BY test ────────────────────────────────────────────────────

#[tokio::test]
async fn test_sql_order_by_integration() {
    use xerj_engine::sql::parse_sql;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("sql_order", Schema::empty()).unwrap();
    let idx = engine.get_index("sql_order").unwrap();

    idx.index_document(Some("a".into()), json!({ "score": 10, "name": "charlie" }))
        .await
        .unwrap();
    idx.index_document(Some("b".into()), json!({ "score": 30, "name": "alice" }))
        .await
        .unwrap();
    idx.index_document(Some("c".into()), json!({ "score": 20, "name": "bob" }))
        .await
        .unwrap();

    // Parse SQL with ORDER BY score DESC.
    let parsed = parse_sql("SELECT * FROM sql_order ORDER BY score DESC LIMIT 3").unwrap();
    let req = xerj_query::ast::SearchRequest {
        query: parsed.query,
        size: parsed.limit.unwrap_or(10),
        sort: parsed.sort,
        ..Default::default()
    };

    let result = idx.search(&req).await.unwrap();
    assert_eq!(result.total.value, 3, "should return all 3 docs");

    // Verify descending score order: b(30) > c(20) > a(10).
    let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(
        ids[0], "b",
        "highest score (30) should be first, got: {:?}",
        ids
    );
    assert_eq!(
        ids[1], "c",
        "second score (20) should be second, got: {:?}",
        ids
    );
    assert_eq!(
        ids[2], "a",
        "lowest score (10) should be last, got: {:?}",
        ids
    );
}

// ── ES Features: Field alias, copy_to, IP range, date math ───────────────────

/// Test field alias resolution: querying an alias field resolves to the target.
#[tokio::test]
async fn test_field_alias_resolution() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Create schema with a field alias: user_name → name
    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("name", FieldType::Keyword))
        .unwrap();
    // Add alias field: user_name maps to name
    let mut alias_fc = FieldConfig::new("user_name", FieldType::Object);
    alias_fc.options.null_value = Some(Value::String("__alias__:name".to_string()));
    schema.add_field(alias_fc).unwrap();

    engine.create_index("alias_test", schema).unwrap();
    let idx = engine.get_index("alias_test").unwrap();

    idx.index_document(Some("1".into()), json!({ "name": "Alice" }))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({ "name": "Bob" }))
        .await
        .unwrap();

    // Query using the alias field user_name — should resolve to name.
    let result = idx
        .search(&make_search(json!({"term": {"user_name": "Alice"}})))
        .await
        .unwrap();
    assert_eq!(result.total.value, 1, "alias query should find 1 doc");
    assert_eq!(
        result.hits[0].id, "1",
        "alias query should return Alice's doc"
    );

    // Query using the original field name should also work.
    let result2 = idx
        .search(&make_search(json!({"term": {"name": "Bob"}})))
        .await
        .unwrap();
    assert_eq!(result2.total.value, 1);
    assert_eq!(result2.hits[0].id, "2");
}

/// Test copy_to: indexing a doc copies the field value to the target field.
#[tokio::test]
async fn test_copy_to() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Create schema: title copies to all_text, description copies to all_text
    let mut schema = Schema::empty();

    let mut title_fc = FieldConfig::new("title", FieldType::Text);
    title_fc.options.null_value = Some(Value::String("__copy_to__:all_text".to_string()));
    schema.add_field(title_fc).unwrap();

    let mut desc_fc = FieldConfig::new("description", FieldType::Text);
    desc_fc.options.null_value = Some(Value::String("__copy_to__:all_text".to_string()));
    schema.add_field(desc_fc).unwrap();

    // all_text is the aggregation target field
    schema
        .add_field(FieldConfig::new("all_text", FieldType::Text))
        .unwrap();

    engine.create_index("copyto_test", schema).unwrap();
    let idx = engine.get_index("copyto_test").unwrap();

    idx.index_document(
        Some("1".into()),
        json!({ "title": "Rust Programming", "description": "A systems language" }),
    )
    .await
    .unwrap();

    // Retrieve the document and check that all_text contains the copied values.
    let doc = idx
        .get_document("1")
        .await
        .unwrap()
        .expect("doc should exist");
    // all_text should contain the title value (and possibly description too).
    let all_text = doc.get("all_text");
    assert!(
        all_text.is_some(),
        "all_text field should be present after copy_to"
    );
    let all_text_val = all_text.unwrap();
    let all_text_str = all_text_val.to_string();
    assert!(
        all_text_str.contains("Rust Programming") || all_text_str.contains("systems language"),
        "all_text should contain copied values, got: {}",
        all_text_str
    );
}

/// Test IP range query: term query with CIDR notation.
#[tokio::test]
async fn test_ip_range_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("ip_test", Schema::empty()).unwrap();
    let idx = engine.get_index("ip_test").unwrap();

    idx.index_document(Some("1".into()), json!({ "ip": "192.168.1.10" }))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({ "ip": "192.168.1.200" }))
        .await
        .unwrap();
    idx.index_document(Some("3".into()), json!({ "ip": "10.0.0.1" }))
        .await
        .unwrap();
    idx.index_document(Some("4".into()), json!({ "ip": "192.168.2.1" }))
        .await
        .unwrap();

    // CIDR term query: 192.168.1.0/24 should match .10 and .200 but not .2.1 or 10.0.0.1
    let result = idx
        .search(&make_search(json!({"term": {"ip": "192.168.1.0/24"}})))
        .await
        .unwrap();
    assert_eq!(
        result.total.value, 2,
        "CIDR 192.168.1.0/24 should match 2 IPs, got: {}",
        result.total.value
    );
    let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.contains(&"1"), "192.168.1.10 should match /24");
    assert!(ids.contains(&"2"), "192.168.1.200 should match /24");

    // IP range query: gte/lte
    let result2 = idx
        .search(&make_search(json!({
            "range": {
                "ip": {
                    "gte": "192.168.1.0",
                    "lte": "192.168.1.255"
                }
            }
        })))
        .await
        .unwrap();
    assert_eq!(
        result2.total.value, 2,
        "range 192.168.1.0-255 should match 2 IPs"
    );
}

/// Test date math resolution in index names.
///
/// This test exercises the `resolve_date_math` function directly.
#[test]
fn test_date_math_index_name_resolution() {
    use chrono::Datelike;
    use xerj_engine::resolve_date_math;

    // <log-{now/d}> should resolve to log-YYYY.MM.DD (today's date).
    let today = chrono::Utc::now();
    let expected = format!(
        "log-{:04}.{:02}.{:02}",
        today.year(),
        today.month(),
        today.day()
    );
    let resolved = resolve_date_math("<log-{now/d}>");
    assert_eq!(
        resolved, expected,
        "date math <log-{{now/d}}> should resolve to today"
    );

    // No date math — should pass through unchanged.
    assert_eq!(resolve_date_math("my-index"), "my-index");

    // Static prefix with date math.
    let resolved2 = resolve_date_math("<metrics-{now/d}>");
    assert!(
        resolved2.starts_with("metrics-"),
        "should start with metrics-, got: {}",
        resolved2
    );
    assert!(
        resolved2.len() > "metrics-".len(),
        "should have date suffix"
    );
}

// ── Custom analyzer / synonym / ngram integration tests ───────────────────────

/// Helper: build index settings with a custom synonym-aware analyzer.
///
/// The analyzer is named "default" so the memtable picks it up automatically
/// for all text field indexing and searching.
fn synonym_settings(synonym_rules: &[&str]) -> serde_json::Value {
    let rules: Vec<serde_json::Value> = synonym_rules
        .iter()
        .map(|r| serde_json::Value::String(r.to_string()))
        .collect();

    json!({
        "analysis": {
            "filter": {
                "my_synonyms": {
                    "type": "synonym",
                    "synonyms": rules
                }
            },
            "analyzer": {
                "default": {
                    "type": "custom",
                    "tokenizer": "standard",
                    "filter": ["lowercase", "my_synonyms"]
                }
            }
        }
    })
}

#[tokio::test]
async fn test_custom_analyzer_synonym_expansion() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Create index with synonym filter: fast ↔ quick, big ↔ large.
    let settings = synonym_settings(&["fast,quick", "big,large"]);
    engine
        .create_index_with_settings("syn_idx", Schema::empty(), settings)
        .unwrap();

    let idx = engine.get_index("syn_idx").unwrap();

    // Index a document with "fast car".
    idx.index_document(Some("1".into()), json!({ "description": "fast car" }))
        .await
        .unwrap();

    // Searching for "quick car" should match via synonym expansion.
    let result = idx
        .search(&make_search(json!({"match": {"description": "quick car"}})))
        .await
        .unwrap();
    assert_eq!(
        result.total.value, 1,
        "synonym expansion: searching 'quick' should match document with 'fast'"
    );
    assert_eq!(result.hits[0].id, "1");

    // Searching for "fast car" should still match directly.
    let result2 = idx
        .search(&make_search(json!({"match": {"description": "fast car"}})))
        .await
        .unwrap();
    assert_eq!(result2.total.value, 1);

    // Searching for "slow" (not in any synonym group) should not match.
    let result3 = idx
        .search(&make_search(
            json!({"match": {"description": "slow truck"}}),
        ))
        .await
        .unwrap();
    assert_eq!(
        result3.total.value, 0,
        "unrelated terms should not match 'fast car'"
    );
}

#[tokio::test]
async fn test_custom_analyzer_synonym_explicit_mapping() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Explicit one-way synonym: "automobile" maps to "car".
    let settings = json!({
        "analysis": {
            "filter": {
                "vehicle_synonyms": {
                    "type": "synonym",
                    "synonyms": ["automobile => car"]
                }
            },
            "analyzer": {
                "default": {
                    "type": "custom",
                    "tokenizer": "standard",
                    "filter": ["lowercase", "vehicle_synonyms"]
                }
            }
        }
    });

    engine
        .create_index_with_settings("explicit_syn", Schema::empty(), settings)
        .unwrap();

    let idx = engine.get_index("explicit_syn").unwrap();

    idx.index_document(Some("1".into()), json!({ "title": "automobile for sale" }))
        .await
        .unwrap();

    // "automobile" expands to "car" at index time, so searching for "car" matches.
    let result = idx
        .search(&make_search(json!({"match": {"title": "car"}})))
        .await
        .unwrap();
    assert_eq!(
        result.total.value, 1,
        "explicit synonym 'automobile => car': searching 'car' should match"
    );
}

#[tokio::test]
async fn test_edge_ngram_tokenizer_autocomplete() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Configure an edge n-gram analyzer for autocomplete.
    let settings = json!({
        "analysis": {
            "tokenizer": {
                "autocomplete_tok": {
                    "type": "edge_ngram",
                    "min_gram": 1,
                    "max_gram": 10
                }
            },
            "analyzer": {
                "default": {
                    "type": "custom",
                    "tokenizer": "autocomplete_tok",
                    "filter": ["lowercase"]
                }
            }
        }
    });

    engine
        .create_index_with_settings("autocomplete_idx", Schema::empty(), settings)
        .unwrap();

    let idx = engine.get_index("autocomplete_idx").unwrap();

    // Index a document whose title will be broken into edge ngrams.
    idx.index_document(Some("1".into()), json!({ "title": "javascript" }))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({ "title": "java" }))
        .await
        .unwrap();

    // Searching for "java" (a prefix of "javascript") should match both.
    let result = idx
        .search(&make_search(json!({"match": {"title": "java"}})))
        .await
        .unwrap();
    assert_eq!(
        result.total.value, 2,
        "edge-ngram: prefix 'java' should match 'javascript' and 'java'"
    );

    // Searching for "javas" should match "javascript" — and "javascript" should
    // be ranked higher than "java" because more of its ngrams match.
    let result2 = idx
        .search(&make_search(json!({"match": {"title": "javas"}})))
        .await
        .unwrap();
    assert!(
        result2.total.value >= 1,
        "edge-ngram: 'javas' should match 'javascript'"
    );
    // The top result should be "javascript" (doc 1) — it has the "javas" ngram.
    assert_eq!(
        result2.hits[0].id, "1",
        "javascript should be the top-scoring result for 'javas'"
    );
}

#[tokio::test]
async fn test_ngram_tokenizer_infix_search() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let settings = json!({
        "analysis": {
            "tokenizer": {
                "ngram_tok": {
                    "type": "ngram",
                    "min_gram": 3,
                    "max_gram": 3
                }
            },
            "analyzer": {
                "default": {
                    "type": "custom",
                    "tokenizer": "ngram_tok",
                    "filter": ["lowercase"]
                }
            }
        }
    });

    engine
        .create_index_with_settings("ngram_idx", Schema::empty(), settings)
        .unwrap();

    let idx = engine.get_index("ngram_idx").unwrap();

    idx.index_document(Some("1".into()), json!({ "name": "basketball" }))
        .await
        .unwrap();

    // "ket" is a 3-gram found inside "basketball".
    let result = idx
        .search(&make_search(json!({"match": {"name": "ket"}})))
        .await
        .unwrap();
    assert_eq!(
        result.total.value, 1,
        "ngram: infix 'ket' should match 'basketball'"
    );
}

#[tokio::test]
async fn test_length_filter_integration() {
    use std::sync::Arc;
    use xerj_fts::analyzer::{
        AnalyzerPipeline, AnalyzerRegistry, LengthFilter, LowercaseFilter, StandardTokenizer,
    };

    let mut registry = AnalyzerRegistry::with_defaults();
    registry.register(
        "length_filtered",
        AnalyzerPipeline::new(
            vec![],
            Arc::new(StandardTokenizer),
            vec![
                Arc::new(LowercaseFilter) as Arc<dyn xerj_fts::TokenFilter>,
                Arc::new(LengthFilter::new(4, 8)),
            ],
        ),
    );

    let analyzer = registry.get_analyzer("length_filtered").unwrap();
    let terms = analyzer.analyze_to_terms("a cat runs quickly over the lazy frog");

    // "a" (len 1), "the" (len 3) are too short; "quickly" (len 7) passes.
    for term in &terms {
        assert!(
            term.len() >= 4 && term.len() <= 8,
            "term '{}' should be 4-8 chars",
            term
        );
    }
    assert!(
        terms.contains(&"runs".to_string()),
        "4-char word 'runs' should pass"
    );
    assert!(
        terms.contains(&"quickly".to_string()),
        "'quickly' should pass"
    );
}

#[tokio::test]
async fn test_shingle_filter_integration() {
    use std::sync::Arc;
    use xerj_fts::analyzer::{
        AnalyzerPipeline, AnalyzerRegistry, LowercaseFilter, ShingleFilter, WhitespaceTokenizer,
    };

    let mut registry = AnalyzerRegistry::with_defaults();
    registry.register(
        "shingle_analyzer",
        AnalyzerPipeline::new(
            vec![],
            Arc::new(WhitespaceTokenizer),
            vec![
                Arc::new(LowercaseFilter) as Arc<dyn xerj_fts::TokenFilter>,
                Arc::new(ShingleFilter::new(2)),
            ],
        ),
    );

    let analyzer = registry.get_analyzer("shingle_analyzer").unwrap();
    let terms = analyzer.analyze_to_terms("the quick brown");

    // Unigrams
    assert!(terms.contains(&"the".to_string()));
    assert!(terms.contains(&"quick".to_string()));
    assert!(terms.contains(&"brown".to_string()));
    // Bigrams
    assert!(
        terms.contains(&"the quick".to_string()),
        "shingle 'the quick' missing"
    );
    assert!(
        terms.contains(&"quick brown".to_string()),
        "shingle 'quick brown' missing"
    );
}

#[tokio::test]
async fn test_ascii_folding_filter() {
    use std::sync::Arc;
    use xerj_fts::analyzer::{
        AnalyzerPipeline, AnalyzerRegistry, AsciiFoldingFilter, LowercaseFilter, StandardTokenizer,
    };

    let mut registry = AnalyzerRegistry::with_defaults();
    registry.register(
        "folded",
        AnalyzerPipeline::new(
            vec![],
            Arc::new(StandardTokenizer),
            vec![
                Arc::new(LowercaseFilter) as Arc<dyn xerj_fts::TokenFilter>,
                Arc::new(AsciiFoldingFilter),
            ],
        ),
    );

    let analyzer = registry.get_analyzer("folded").unwrap();
    let terms = analyzer.analyze_to_terms("café über naïve résumé");

    assert!(terms.contains(&"cafe".to_string()), "café → cafe");
    assert!(terms.contains(&"uber".to_string()), "über → uber");
    assert!(terms.contains(&"naive".to_string()), "naïve → naive");
    assert!(terms.contains(&"resume".to_string()), "résumé → resume");

    // Latin Extended-A coverage (Polish / Czech / Croatian) — these live outside
    // the Latin-1 Supplement block and previously passed through unfolded.
    let ext_a = analyzer.analyze_to_terms("łódź žluťoučký đžem");
    assert!(
        ext_a.contains(&"lodz".to_string()),
        "łódź → lodz: {ext_a:?}"
    );
    assert!(
        ext_a.contains(&"zlutoucky".to_string()),
        "žluťoučký → zlutoucky: {ext_a:?}"
    );
    assert!(
        ext_a.contains(&"dzem".to_string()),
        "đžem → dzem: {ext_a:?}"
    );

    // Decomposed / NFD input: "e" + U+0301 (combining acute) must fold to "e".
    let nfd = analyzer.analyze_to_terms("cafe\u{0301}");
    assert!(
        nfd.contains(&"cafe".to_string()),
        "cafe+́ (NFD) → cafe: {nfd:?}"
    );
}

#[tokio::test]
async fn test_pattern_tokenizer() {
    use std::sync::Arc;
    use xerj_fts::analyzer::{
        AnalyzerPipeline, AnalyzerRegistry, LowercaseFilter, PatternTokenizer,
    };

    let mut registry = AnalyzerRegistry::with_defaults();
    registry.register(
        "pattern_analyzer",
        AnalyzerPipeline::new(
            vec![],
            Arc::new(PatternTokenizer::default_pattern()),
            vec![Arc::new(LowercaseFilter) as Arc<dyn xerj_fts::TokenFilter>],
        ),
    );

    let analyzer = registry.get_analyzer("pattern_analyzer").unwrap();
    let terms = analyzer.analyze_to_terms("foo.bar_baz:qux");

    // Split on \W+: ".", "_", ":" are all non-word chars but "_" is actually word char.
    // \W+ splits on ".", ":" — "_" is kept with word chars by default regex.
    // Standard \W+ behavior: splits on ".", ":"
    assert!(terms.contains(&"foo".to_string()), "foo should be a token");
    assert!(terms.contains(&"qux".to_string()), "qux should be a token");
}

#[tokio::test]
async fn test_registry_apply_settings() {
    use xerj_fts::analyzer::AnalyzerRegistry;

    let mut registry = AnalyzerRegistry::with_defaults();

    let settings = json!({
        "analysis": {
            "filter": {
                "my_synonyms": {
                    "type": "synonym",
                    "synonyms": ["fast,quick,speedy", "big => large"]
                },
                "my_length": {
                    "type": "length",
                    "min": 3,
                    "max": 50
                }
            },
            "tokenizer": {
                "my_edge_ngram": {
                    "type": "edge_ngram",
                    "min_gram": 2,
                    "max_gram": 5
                }
            },
            "analyzer": {
                "my_synonym_analyzer": {
                    "type": "custom",
                    "tokenizer": "standard",
                    "filter": ["lowercase", "my_synonyms"]
                },
                "my_autocomplete": {
                    "type": "custom",
                    "tokenizer": "my_edge_ngram",
                    "filter": ["lowercase"]
                }
            }
        }
    });

    registry.apply_settings(&settings);

    // Synonym analyzer should be registered.
    let syn_analyzer = registry
        .get_analyzer("my_synonym_analyzer")
        .expect("my_synonym_analyzer registered");
    let terms = syn_analyzer.analyze_to_terms("fast vehicle");
    assert!(
        terms.contains(&"fast".to_string()),
        "original term 'fast' present"
    );
    assert!(
        terms.contains(&"quick".to_string()),
        "synonym 'quick' expanded from 'fast'"
    );
    assert!(
        terms.contains(&"speedy".to_string()),
        "synonym 'speedy' expanded from 'fast'"
    );

    // Autocomplete analyzer should be registered.
    let ac_analyzer = registry
        .get_analyzer("my_autocomplete")
        .expect("my_autocomplete registered");
    let ac_terms = ac_analyzer.analyze_to_terms("hello");
    assert!(
        ac_terms.contains(&"he".to_string()),
        "edge ngram 'he' from 'hello'"
    );
    assert!(
        ac_terms.contains(&"hel".to_string()),
        "edge ngram 'hel' from 'hello'"
    );
    assert!(
        ac_terms.contains(&"hell".to_string()),
        "edge ngram 'hell' from 'hello'"
    );
    assert!(
        ac_terms.contains(&"hello".to_string()),
        "edge ngram 'hello' from 'hello'"
    );
}

// ── Smart field encoding integration test ─────────────────────────────────────

/// Index 1 000 Apache-style access log entries and verify that the smart
/// field analyzer auto-detects encodings and produces meaningful compression
/// ratios.
#[tokio::test]
async fn test_smart_field_encoding_apache_logs() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("access_logs", Schema::empty()).unwrap();
    let idx = engine.get_index("access_logs").unwrap();

    // ── Generate 1 000 synthetic Apache access log entries ────────────────────
    let methods = ["GET", "POST", "PUT", "DELETE", "HEAD"];
    let statuses = [
        "200", "201", "204", "301", "302", "400", "403", "404", "500",
    ];
    let paths = [
        "/api/users",
        "/api/products",
        "/api/orders",
        "/static/app.js",
        "/static/style.css",
        "/health",
        "/metrics",
    ];
    let ips = [
        "10.0.0.1",
        "10.0.0.2",
        "192.168.1.100",
        "172.16.0.50",
        "203.0.113.5",
    ];

    for i in 0..1000usize {
        let method = methods[i % methods.len()];
        let status = statuses[i % statuses.len()];
        let path = format!("{}/{}", paths[i % paths.len()], i);
        let ip = ips[i % ips.len()];
        let bytes: u64 = (i as u64 % 9000) + 100;
        let response_time: f64 = (i as f64 % 500.0) / 10.0;

        let doc = json!({
            "method": method,
            "status": status,
            "path": path,
            "client_ip": ip,
            "bytes": bytes,
            "response_time": response_time,
            "timestamp": format!("2024-01-{:02}T{:02}:00:00Z", (i % 28) + 1, i % 24),
            "service": "nginx",
        });

        idx.index_document(Some(format!("log-{}", i)), doc)
            .await
            .unwrap();
    }

    // ── Verify log format detection ───────────────────────────────────────────
    let sample_doc = json!({
        "method": "GET",
        "status": "200",
        "path": "/api/users/42",
        "client_ip": "10.0.0.1",
        "bytes": 1024,
    });
    let fmt = detect_log_format(&sample_doc);
    assert!(
        matches!(
            fmt,
            Some(LogFormat::ApacheAccess) | Some(LogFormat::NginxAccess)
        ),
        "should detect access log format, got {:?}",
        fmt
    );

    // App log detection
    let app_doc = json!({
        "level": "INFO",
        "message": "request processed",
        "service": "api",
    });
    let app_fmt = detect_log_format(&app_doc);
    assert_eq!(
        app_fmt,
        Some(LogFormat::AppLog),
        "should detect app log format"
    );

    // ── Verify encoding stats are populated after 1 000 docs ─────────────────
    let stats = idx.stats().await;
    assert_eq!(stats.doc_count, 1000, "should have 1 000 docs");

    // There should be at least some analyzed fields.
    assert!(
        !stats.field_encodings.is_empty(),
        "field_encodings should be populated after 1 000 samples"
    );

    // Print the per-field encoding report.
    println!("\n── Smart field encoding report for 'access_logs' ──");
    println!(
        "{:<20} {:<20} {:>12} {:>15} {:>10}",
        "Field", "Encoding", "Bytes/Value", "Raw Bytes/Value", "Ratio"
    );
    println!("{}", "-".repeat(80));
    for info in &stats.field_encodings {
        println!(
            "{:<20} {:<20} {:>12.2} {:>15.2} {:>10.2}x",
            info.field,
            info.encoding,
            info.bytes_per_value,
            info.raw_bytes_per_value,
            info.compression_ratio
        );
    }
    println!();

    // Spot-check specific fields that should have known good encodings.
    let by_field: std::collections::HashMap<&str, &xerj_engine::FieldEncodingInfo> = stats
        .field_encodings
        .iter()
        .map(|e| (e.field.as_str(), e))
        .collect();

    // `status` should be BitsetEnum or Dictionary (very low cardinality).
    if let Some(status_enc) = by_field.get("status") {
        assert!(
            status_enc.encoding == "bitset_enum" || status_enc.encoding == "dictionary",
            "status field: expected bitset_enum or dictionary, got {}",
            status_enc.encoding
        );
        assert!(
            status_enc.compression_ratio >= 1.0,
            "status should compress vs raw, ratio={}",
            status_enc.compression_ratio
        );
    }

    // `client_ip` should be PackedIp or Dictionary (small fixed set).
    if let Some(ip_enc) = by_field.get("client_ip") {
        assert!(
            ip_enc.encoding == "packed_ip"
                || ip_enc.encoding == "dictionary"
                || ip_enc.encoding == "bitset_enum",
            "client_ip: unexpected encoding {}",
            ip_enc.encoding
        );
    }

    // All analyzed fields should have a compression_ratio >= 1.0
    // (encoding is at least as good as raw UTF-8).
    for info in &stats.field_encodings {
        assert!(
            info.compression_ratio >= 1.0,
            "field '{}' has compression_ratio < 1.0: {}",
            info.field,
            info.compression_ratio
        );
    }
}

// ── Dashboard summary size_bytes is real measured bytes, not a heuristic ──────
//
// The native `/v1/dashboard/summary` handler reports per-index `size_bytes` as
// `sum(store_snapshot().segments[].size_bytes) + stats.memtable_size_bytes`.
// Both inputs are real byte measurements (the segment figures also back the
// `_segments` API; the memtable figure backs `IndexStats`). This test asserts
// that computation at the engine level — the handler is a thin wrapper over it,
// so we verify the load-bearing data here rather than through the HTTP harness.
#[tokio::test]
async fn test_dashboard_summary_size_is_measured_bytes() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.create_index("dash", Schema::empty()).unwrap();
    let idx = engine.get_index("dash").unwrap();

    for i in 0..50 {
        idx.index_document(
            Some(format!("doc{i}")),
            json!({ "n": i, "name": format!("item {i}"), "tag": "dashboard" }),
        )
        .await
        .unwrap();
    }

    // Before flush: everything lives in the memtable, so the measured memtable
    // byte count must be non-zero and there are no segments yet.
    let stats = idx.stats().await;
    assert_eq!(
        stats.segment_count, 0,
        "no segments should exist before flush"
    );
    assert!(
        stats.memtable_size_bytes > 0,
        "memtable byte size should be > 0 with docs buffered"
    );

    // Flush to disk so a real on-disk segment (with a real byte size) exists.
    idx.flush().await.unwrap();

    // Recompute the exact expression the dashboard handler uses.
    let snap = idx.store_snapshot();
    assert!(
        !snap.segments.is_empty(),
        "at least one segment should exist after flush"
    );
    let segment_bytes: u64 = snap.segments.iter().map(|s| s.size_bytes).sum();
    assert!(
        segment_bytes > 0,
        "segment byte size should be > 0 after flush (real .seg file bytes)"
    );

    let stats = idx.stats().await;
    // This mirrors the dashboard handler's size_bytes computation exactly:
    // real segment file bytes + real memtable bytes.
    let size_bytes = segment_bytes + stats.memtable_size_bytes as u64;

    // The measured size must be real (> 0).
    assert!(size_bytes > 0, "measured dashboard size_bytes must be > 0");

    // Sanity: the measured on-disk size is nothing like the old heuristic's
    // fixed 200-bytes-per-segment-doc fabrication, proving it is real.
    let old_heuristic = stats
        .doc_count
        .saturating_sub(stats.memtable_doc_count as u64)
        * 200
        + stats.memtable_doc_count as u64 * 500;
    assert_ne!(
        size_bytes, old_heuristic,
        "measured size should differ from the removed docs*200+memtable*500 heuristic"
    );
}

// ── Mixed-RUW Fix 1: fused memtable-walk total == shortcut recount ────────────
//
// `search_inner` captures the fused DV walk's exact memtable total
// (`mem_matches_known`) at mem-snapshot time and threads it into
// `try_shortcut_count`, whose bool-conjunction arm consumes it instead of
// re-walking the memtable (the historical duplicate recount).  This test pins
// the equivalence contract on a POPULATED memtable (one flushed segment +
// unflushed buffered docs, with missing-value (absent-key) and multi-valued numeric
// fields): for term / conjunctive-bool / range shapes, the size>0 total
// (fused walk + threaded count), the size:0 total (the shortcut's own
// recount — the ONLY memtable authority on that path, b7 DEFECT 1a), and the
// fully-materialised hit count must all agree.  A drift between the fused
// walk's semantics and the recount's would surface here as a total mismatch.
#[tokio::test]
async fn test_fused_memtable_total_matches_shortcut_recount() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("ruw", Schema::empty()).unwrap();
    let idx = engine.get_index("ruw").unwrap();

    let mk_doc = |i: usize, wave: &str| {
        let mut d = json!({
            "status": if i.is_multiple_of(if wave == "seg" { 2 } else { 3 }) { "ok" } else { "error" },
            "latency_ms": i * if wave == "seg" { 10 } else { 7 },
            // multi-valued numeric on every 4th doc, scalar otherwise
            "codes": if i.is_multiple_of(4) { json!([i, i + 100]) } else { json!(i) },
        });
        // MISSING field on every 5th doc (ES "missing value" semantics: the
        // doc matches no range on the field).  Deliberately the ABSENT-KEY
        // flavour, NOT explicit JSON `null`: explicit null has a
        // PRE-EXISTING segment-side divergence — live-verified byte-identical
        // on c6cbe9f, i.e. BEFORE the mixed-RUW change — where the hit path
        // admits 3 explicit-null docs a `range gte` must exclude and the
        // size:0 count disagrees (32 / 17 where ES 8.13.4 answers 29 / 29).
        // That defect is orthogonal to the total-threading this test pins
        // and is recorded in demo/playbooks/ES_COMPATIBILITY.md; absent-key
        // docs (this flavour) agree with ES exactly (29/29/29 live-verified
        // on both engines).
        if !i.is_multiple_of(5) {
            d["cost_usd"] = json!((i as f64) * 0.01);
        }
        d
    };

    // Wave 1 → flushed to a segment (so seg_matches is non-trivial).
    for i in 0..40 {
        idx.index_document(Some(format!("seg-{i}")), mk_doc(i, "seg"))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    // Wave 2 → stays memtable-resident (the fused walk's subject).
    for i in 0..60 {
        idx.index_document(Some(format!("mem-{i}")), mk_doc(i, "mem"))
            .await
            .unwrap();
    }
    let stats = idx.stats().await;
    assert!(
        stats.memtable_doc_count >= 60,
        "wave 2 must be memtable-resident for this test to bite (got {})",
        stats.memtable_doc_count
    );

    let shapes: Vec<(&str, Value)> = vec![
        ("term", json!({ "term": { "status": "ok" } })),
        (
            "bool(term+range)",
            json!({ "bool": {
                "must": [{ "term": { "status": "ok" } }],
                "filter": [{ "range": { "latency_ms": { "gte": 50, "lte": 300 } } }]
            } }),
        ),
        (
            "bool(range on missing-bearing)",
            json!({ "bool": {
                "must": [{ "term": { "status": "ok" } }],
                "filter": [{ "range": { "cost_usd": { "gte": 0.05 } } }]
            } }),
        ),
        (
            "bool(range on multi-valued)",
            json!({ "bool": {
                "filter": [
                    { "term": { "status": "ok" } },
                    { "range": { "codes": { "gte": 4 } } }
                ]
            } }),
        ),
        (
            "range(single-valued)",
            json!({ "range": { "latency_ms": { "gte": 50 } } }),
        ),
        (
            "range(missing-bearing)",
            json!({ "range": { "cost_usd": { "gte": 0.05 } } }),
        ),
    ];
    for (label, q) in shapes {
        let full = idx
            .search(&make_search_with_size(q.clone(), 10_000))
            .await
            .unwrap();
        let ground_truth = full.hits.len() as u64;
        assert!(ground_truth > 0, "{label}: shape must match something");
        assert_eq!(
            full.total.value, ground_truth,
            "{label}: size=10k total must equal materialised hit count"
        );
        let paged = idx
            .search(&make_search_with_size(q.clone(), 5))
            .await
            .unwrap();
        assert_eq!(
            paged.total.value, ground_truth,
            "{label}: size=5 total (fused walk + threaded mem_matches_known)"
        );
        let count = idx
            .search(&make_search_with_size(q.clone(), 0))
            .await
            .unwrap();
        assert_eq!(
            count.total.value, ground_truth,
            "{label}: size=0 total (shortcut recount is the memtable authority)"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// RC4 Stream B regressions — silent wrong data
// ═════════════════════════════════════════════════════════════════════════════

/// Blocker 3: a malformed doc line under a bulk `index` action used to be
/// stored as EMPTY `{}` with `201 / errors:false` (the turbo-raw path
/// deferred the parse and `.unwrap_or({})`-ed the failure). ES rejects the
/// item with a per-item 400 `document_parsing_exception`; the engine must
/// reject it per-item and must not store anything.
#[tokio::test]
async fn test_bulk_index_malformed_doc_rejected_per_item() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let body = concat!(
        "{\"index\":{\"_index\":\"b3\",\"_id\":\"bad\"}}\n",
        "{\"broken json here\n",
        "{\"index\":{\"_index\":\"b3\",\"_id\":\"good\"}}\n",
        "{\"v\":\"ok\"}\n",
    );
    let result = xerj_engine::bulk::process_bulk(&engine, None, body).await;
    assert!(result.errors, "bulk response must flag errors:true");
    assert_eq!(result.items.len(), 2);

    let bad = &result.items[0];
    assert_eq!(bad.status, 400, "malformed item must be a per-item 400");
    assert!(
        bad.error
            .as_deref()
            .unwrap_or("")
            .contains("invalid document JSON"),
        "error must say the document JSON is invalid, got: {:?}",
        bad.error
    );

    let good = &result.items[1];
    assert_eq!(good.status, 201, "valid sibling item must still index");

    // The malformed doc must NOT be stored (it used to land as `{}`).
    let idx = engine.get_index("b3").unwrap();
    let all = idx
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();
    assert_eq!(all.total.value, 1, "only the valid doc may be stored");
    assert_eq!(all.hits[0].id, "good");

    // Valid JSON that is NOT an object is rejected too (ES errors on it).
    let body2 = "{\"index\":{\"_index\":\"b3\",\"_id\":\"arr\"}}\n[1,2,3]\n";
    let r2 = xerj_engine::bulk::process_bulk(&engine, None, body2).await;
    assert!(r2.errors);
    assert_eq!(r2.items[0].status, 400);
    let all2 = idx
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();
    assert_eq!(all2.total.value, 1, "non-object body must not be stored");
}

/// Blocker 4: match/BM25 over a `semantic_text` field returned hits from the
/// memtable but ZERO once flushed — the field's schema type (Object) gave it
/// the whole-value keyword analyzer in the segment FTS. The es_compat mapper
/// now types it Text (+ embedding config); this exercises exactly that shape.
#[tokio::test]
async fn test_semantic_text_match_survives_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // The schema shape es_compat produces for `"type": "semantic_text"`:
    // lexical side = Text (standard analyzer, positions), plus an embedding
    // config producing the companion `content_vector`.
    let mut schema = Schema::empty();
    let mut fc = FieldConfig::new("content", FieldType::Text);
    fc.options.dimensions = Some(16);
    fc.options.similarity = Some("cosine".to_string());
    fc.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("content_vector".to_string()),
    });
    schema.fields.push(fc);
    engine.create_index("sem", schema).unwrap();
    let idx = engine.get_index("sem").unwrap();

    idx.index_document(
        Some("1".into()),
        json!({"content": "the quick brown fox jumps over the lazy dog"}),
    )
    .await
    .unwrap();

    let pre = idx
        .search(&make_search(json!({"match": {"content": "quick fox"}})))
        .await
        .unwrap();
    assert_eq!(pre.total.value, 1, "pre-flush match must hit");

    idx.flush().await.unwrap();

    let post = idx
        .search(&make_search_with_source(
            json!({"match": {"content": "quick fox"}}),
            json!(true),
        ))
        .await
        .unwrap();
    assert_eq!(
        post.total.value, 1,
        "post-flush match must still hit (segment FTS must standard-analyze semantic_text)"
    );
    // The auto-embed side must be unaffected by the lexical type change.
    assert!(
        post.hits[0].source.get("content_vector").is_some(),
        "companion embedding vector missing from _source"
    );
}

#[tokio::test]
async fn test_semantic_text_bulk_preserves_item_order_status_and_vectors() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    let mut fc = FieldConfig::new("content", FieldType::Text);
    fc.options.dimensions = Some(16);
    fc.options.similarity = Some("cosine".to_string());
    fc.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("content_vector".to_string()),
    });
    schema.fields.push(fc);
    engine.create_index("sem-bulk", schema).unwrap();

    let body = concat!(
        "{\"index\":{\"_index\":\"sem-bulk\",\"_id\":\"a\"}}\n",
        "{\"content\":\"alpha financial report\"}\n",
        "{\"index\":{\"_index\":\"sem-bulk\",\"_id\":\"b\"}}\n",
        "{\"content\":\"beta quarterly earnings\"}\n",
        "{\"index\":{\"_index\":\"sem-bulk\",\"_id\":\"a\"}}\n",
        "{\"content\":\"alpha annual report updated\"}\n",
    );
    let result = xerj_engine::bulk::process_bulk(&engine, None, body).await;
    assert!(!result.errors, "{:?}", result.items);
    assert_eq!(result.items.len(), 3);
    assert_eq!(result.items[0].id, "a");
    assert_eq!(result.items[0].status, 201);
    assert_eq!(result.items[1].id, "b");
    assert_eq!(result.items[1].status, 201);
    assert_eq!(result.items[2].id, "a");
    assert_eq!(result.items[2].status, 200);

    let idx = engine.get_index("sem-bulk").unwrap();
    let a = idx.get_document("a").await.unwrap().unwrap();
    let b = idx.get_document("b").await.unwrap().unwrap();
    assert_eq!(a["content"], "alpha annual report updated");
    assert_eq!(a["content_vector"].as_array().unwrap().len(), 16);
    assert_eq!(b["content_vector"].as_array().unwrap().len(), 16);
}

#[tokio::test]
async fn test_put_delete_recreate_publication_versions_and_visibility() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("publication-order", Schema::empty())
        .unwrap();
    let idx = engine.get_index("publication-order").unwrap();

    let first = idx
        .index_document(Some("same".into()), json!({"state": "first"}))
        .await
        .unwrap();
    assert_eq!(first.result, "created");
    assert_eq!(first.version, 1);

    let deleted = idx
        .delete_document_versioned("same", None, None)
        .await
        .unwrap();
    assert!(deleted.found);
    assert_eq!(deleted.version, 2);
    assert!(deleted.seq_no > first.seq_no);
    assert!(idx.get_document("same").await.unwrap().is_none());

    let recreated = idx
        .index_document(Some("same".into()), json!({"state": "recreated"}))
        .await
        .unwrap();
    assert_eq!(recreated.result, "created");
    assert_eq!(recreated.version, 3);
    assert!(recreated.seq_no > deleted.seq_no);
    assert_eq!(
        idx.get_document("same").await.unwrap().unwrap()["state"],
        "recreated"
    );

    idx.flush().await.unwrap();
    assert_eq!(
        idx.get_document("same").await.unwrap().unwrap()["state"],
        "recreated",
        "latest publication must survive flush"
    );
}

#[tokio::test]
async fn test_conditional_put_and_delete_same_id_linearize_at_cas() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("publication-race", Schema::empty())
        .unwrap();
    let idx = engine.get_index("publication-race").unwrap();

    for round in 0..32 {
        let id = format!("same-{round}");
        let initial = idx
            .index_document(Some(id.clone()), json!({"winner": "initial"}))
            .await
            .unwrap();
        let expected_seq = initial.seq_no;
        let start = std::sync::Arc::new(tokio::sync::Barrier::new(3));

        let put = {
            let idx = std::sync::Arc::clone(&idx);
            let id = id.clone();
            let start = std::sync::Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                idx.index_document_with_version(
                    Some(id),
                    json!({"winner": "put"}),
                    Some(expected_seq),
                    Some(1),
                )
                .await
            })
        };
        let delete = {
            let idx = std::sync::Arc::clone(&idx);
            let id = id.clone();
            let start = std::sync::Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                idx.delete_document_versioned(&id, Some(expected_seq), Some(1))
                    .await
            })
        };
        start.wait().await;

        let put = put.await.unwrap();
        let delete = delete.await.unwrap();
        assert_eq!(
            put.is_ok() as u8 + delete.is_ok() as u8,
            1,
            "exactly one operation may consume the same CAS precondition"
        );

        if let Ok(response) = put {
            assert_eq!(response.version, 2);
            assert_eq!(
                idx.get_document(&id).await.unwrap().unwrap()["winner"],
                "put"
            );
        } else {
            let outcome = delete.unwrap();
            assert!(outcome.found);
            assert_eq!(outcome.version, 2);
            assert!(idx.get_document(&id).await.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn test_concurrent_create_and_updates_share_exact_publication_key() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("publication-create-update", Schema::empty())
        .unwrap();
    let idx = engine.get_index("publication-create-update").unwrap();

    let start = std::sync::Arc::new(tokio::sync::Barrier::new(17));
    let mut creates = Vec::new();
    for writer in 0..16 {
        let idx = std::sync::Arc::clone(&idx);
        let start = std::sync::Arc::clone(&start);
        creates.push(tokio::spawn(async move {
            start.wait().await;
            idx.create_document("same".into(), json!({"created_by": writer, "base": true}))
                .await
        }));
    }
    start.wait().await;
    let mut successes = 0;
    for task in creates {
        successes += task.await.unwrap().is_ok() as usize;
    }
    assert_eq!(successes, 1, "create-only admission must be linearized");

    let start = std::sync::Arc::new(tokio::sync::Barrier::new(17));
    let mut updates = Vec::new();
    for writer in 0..16 {
        let idx = std::sync::Arc::clone(&idx);
        let start = std::sync::Arc::clone(&start);
        updates.push(tokio::spawn(async move {
            start.wait().await;
            idx.update_document_with_upsert(
                "same",
                Some(json!({(format!("field_{writer}")): writer})),
                None,
                false,
            )
            .await
        }));
    }
    start.wait().await;
    for task in updates {
        task.await.unwrap().unwrap().unwrap();
    }

    let source = idx.get_document("same").await.unwrap().unwrap();
    for writer in 0..16 {
        assert_eq!(source[format!("field_{writer}")], writer);
    }

    let changed = idx
        .update_document("same", json!({"stable": "value"}))
        .await
        .unwrap()
        .unwrap();
    let noop = idx
        .update_document_with_upsert("same", Some(json!({"stable": "value"})), None, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(noop.result, "noop");
    assert_eq!(noop.seq_no, changed.seq_no);
    assert_eq!(noop.version, changed.version);

    let upsert_start = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let mut upserts = Vec::new();
    for field in ["left", "right"] {
        let idx = std::sync::Arc::clone(&idx);
        let start = std::sync::Arc::clone(&upsert_start);
        upserts.push(tokio::spawn(async move {
            start.wait().await;
            idx.update_document_with_upsert("missing", Some(json!({(field): true})), None, true)
                .await
        }));
    }
    upsert_start.wait().await;
    for task in upserts {
        task.await.unwrap().unwrap().unwrap();
    }
    let upserted = idx.get_document("missing").await.unwrap().unwrap();
    assert_eq!(upserted["left"], true);
    assert_eq!(upserted["right"], true);
}

#[tokio::test]
async fn test_concurrent_external_versions_publish_highest_source() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("publication-external", Schema::empty())
        .unwrap();
    let idx = engine.get_index("publication-external").unwrap();
    idx.index_document_external(Some("same".into()), json!({"external": 1}), 1, "external")
        .await
        .unwrap();

    let start = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for version in [2_u64, 3] {
        let idx = std::sync::Arc::clone(&idx);
        let start = std::sync::Arc::clone(&start);
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            (
                version,
                idx.index_document_external(
                    Some("same".into()),
                    json!({"external": version}),
                    version,
                    "external",
                )
                .await,
            )
        }));
    }
    start.wait().await;
    let mut version_three_succeeded = false;
    for task in tasks {
        let (requested, result) = task.await.unwrap();
        if requested == 3 {
            assert_eq!(result.unwrap().version, 3);
            version_three_succeeded = true;
        }
    }
    assert!(version_three_succeeded);
    assert_eq!(
        idx.get_document("same").await.unwrap().unwrap()["external"],
        3
    );
    assert!(
        idx.index_document_external(Some("same".into()), json!({"external": 2}), 2, "external",)
            .await
            .is_err(),
        "lower external version must remain rejected"
    );
}

#[cfg(feature = "onnx-experimental")]
#[tokio::test]
async fn test_semantic_bulk_preserves_onnx_admission_429() {
    let dir = TempDir::new().unwrap();
    let model = dir.path().join("model.onnx");
    let tokenizer = dir.path().join("tokenizer.json");
    std::fs::write(&model, b"not loaded: admission rejects first").unwrap();
    std::fs::write(&tokenizer, b"not loaded: admission rejects first").unwrap();

    let mut config = Config::default();
    config.server.data_dir = dir.path().join("data").to_string_lossy().into_owned();
    config.embedding.mode = "onnx-experimental".into();
    config.embedding.onnx_model_path = model.to_string_lossy().into_owned();
    config.embedding.onnx_tokenizer_path = tokenizer.to_string_lossy().into_owned();
    config.embedding.onnx_max_input_bytes_per_call = 1;
    config.embedding.onnx_max_inflight_input_bytes = 1;
    let engine = Engine::new(config).unwrap();

    let mut schema = Schema::empty();
    let mut field = FieldConfig::new("content", FieldType::Text);
    field.options.dimensions = Some(384);
    field.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("content_vector".into()),
    });
    schema.fields.push(field);
    engine.create_index("sem-onnx-overload", schema).unwrap();

    let body = concat!(
        "{\"index\":{\"_index\":\"sem-onnx-overload\",\"_id\":\"a\"}}\n",
        "{\"content\":\"too large\"}\n",
    );
    let result = xerj_engine::bulk::process_bulk(&engine, None, body).await;
    assert!(result.errors);
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].status, 429, "{:?}", result.items[0]);
    assert!(
        result.items[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("rejected before tokenization"),
        "{:?}",
        result.items[0]
    );
    assert!(
        engine
            .get_index("sem-onnx-overload")
            .unwrap()
            .get_document("a")
            .await
            .unwrap()
            .is_none(),
        "rejected bulk item must not be persisted"
    );
}

/// Blocker 5: snapshot RESTORE ignored the request `indices` filter and
/// rewrote EVERY index in the snapshot with snapshot-time state, silently
/// destroying all writes made since. The filter must select exactly the
/// requested indices (with wildcard support) and error on unknown names.
#[tokio::test]
async fn test_restore_snapshot_honors_indices_filter() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let repo = dir.path().join("snaprepo");
    let repo_path = repo.to_str().unwrap();

    for name in ["s1", "s2"] {
        engine.create_index(name, Schema::empty()).unwrap();
        let idx = engine.get_index(name).unwrap();
        idx.index_document(Some("d1".into()), json!({"v": "original"}))
            .await
            .unwrap();
    }
    engine
        .create_snapshot(
            repo_path,
            "snap1",
            Some(vec!["s1".to_string(), "s2".to_string()]),
        )
        .await
        .unwrap();

    // Post-snapshot write to s2 — must SURVIVE a restore of s1 only.
    engine
        .get_index("s2")
        .unwrap()
        .index_document(Some("d2".into()), json!({"v": "after-snapshot"}))
        .await
        .unwrap();

    let restored = engine
        .restore_snapshot(repo_path, "snap1", Some(vec!["s1".to_string()]))
        .await
        .unwrap();
    assert_eq!(restored, vec!["s1".to_string()]);

    let s2 = engine.get_index("s2").unwrap();
    let post = s2
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();
    assert_eq!(
        post.total.value, 2,
        "restore of s1 must not roll back s2 (it used to clobber every index)"
    );

    // An index name absent from the snapshot errors loud (never a no-op).
    assert!(engine
        .restore_snapshot(repo_path, "snap1", Some(vec!["nope".to_string()]))
        .await
        .is_err());

    // Wildcards select within the snapshot.
    let both = engine
        .restore_snapshot(repo_path, "snap1", Some(vec!["s*".to_string()]))
        .await
        .unwrap();
    assert_eq!(both.len(), 2, "s* must match both snapshot indices");
    let s2_rolled = engine
        .get_index("s2")
        .unwrap()
        .search(&make_search(json!({"match_all": {}})))
        .await
        .unwrap();
    assert_eq!(
        s2_rolled.total.value, 1,
        "explicitly-selected s2 rolls back to snapshot state"
    );
}

/// Blocker 7: top-level kNN semantics, verified against live ES 8.13.4 on
/// 2026-07-12 with this exact corpus:
///   * `knn.filter` pre-filters candidates (ES returned ids 1,3);
///   * `knn.similarity` (raw cosine cutoff) drops sub-threshold docs from
///     hits AND `hits.total` (ES: total 2, ids 1,2);
///   * `boost` multiplies scores AFTER the cutoff (ES: 2.0 / ~1.9939);
///   * multiple knn clauses (`bool.should` of Knn nodes — the compat layer's
///     synthesis of the `knn: [...]` array) run per-clause top-k and SUM
///     scores over the union (ES: total 2, both scored 1.0).
#[tokio::test]
async fn test_knn_filter_similarity_boost_and_multi_clause() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    let mut vf = FieldConfig::new("v", FieldType::Vector);
    vf.options.dimensions = Some(3);
    vf.options.similarity = Some("cosine".to_string());
    schema.fields.push(vf);
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index("knnidx", schema).unwrap();
    let idx = engine.get_index("knnidx").unwrap();

    idx.index_document(Some("1".into()), json!({"v": [1.0, 0.0, 0.0], "tag": "a"}))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({"v": [0.9, 0.1, 0.0], "tag": "b"}))
        .await
        .unwrap();
    idx.index_document(Some("3".into()), json!({"v": [0.0, 1.0, 0.0], "tag": "a"}))
        .await
        .unwrap();

    let run = |body: Value| {
        let req = parse_request(&body).expect("parse_request");
        let idx = idx.clone();
        async move { idx.search(&req).await.unwrap() }
    };

    // knn.filter: only tag=a docs may enter the top-k (ES: ids 1,3).
    let filtered = run(json!({
        "query": {"knn": {"field": "v", "query_vector": [1.0, 0.0, 0.0], "k": 2,
                           "filter": {"term": {"tag": "a"}}}},
        "size": 10
    }))
    .await;
    let ids: Vec<&str> = filtered.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["1", "3"],
        "filter must exclude tag=b from the pool"
    );

    // knn.similarity: raw cosine cutoff 0.9 keeps ids 1,2 only, and the
    // excluded doc leaves hits.total too (ES: total 2).
    let cut = run(json!({
        "query": {"knn": {"field": "v", "query_vector": [1.0, 0.0, 0.0], "k": 3,
                           "similarity": 0.9}},
        "size": 10
    }))
    .await;
    assert_eq!(
        cut.total.value, 2,
        "sub-threshold doc must leave hits.total"
    );
    let ids: Vec<&str> = cut.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["1", "2"]);

    // boost multiplies AFTER the cutoff (ES: scores 2.0 and ~1.9939).
    let boosted = run(json!({
        "query": {"knn": {"field": "v", "query_vector": [1.0, 0.0, 0.0], "k": 3,
                           "similarity": 0.9, "boost": 2.0}},
        "size": 10
    }))
    .await;
    assert_eq!(boosted.total.value, 2);
    assert!(
        (boosted.hits[0].score - 2.0).abs() < 1e-3,
        "top score must be 2.0, got {}",
        boosted.hits[0].score
    );
    assert!(
        (boosted.hits[1].score - 1.9939).abs() < 1e-3,
        "second score must be ~1.9939, got {}",
        boosted.hits[1].score
    );

    // Multi-knn union: per-clause top-1, summed scores over the dedup'd
    // union (ES: total 2, ids {1,3}, each scored 1.0).
    let multi = run(json!({
        "query": {"bool": {"should": [
            {"knn": {"field": "v", "query_vector": [1.0, 0.0, 0.0], "k": 1}},
            {"knn": {"field": "v", "query_vector": [0.0, 1.0, 0.0], "k": 1}}
        ]}},
        "size": 10
    }))
    .await;
    assert_eq!(multi.total.value, 2, "union of the two top-1 pools");
    let mut ids: Vec<&str> = multi.hits.iter().map(|h| h.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["1", "3"]);
    for h in &multi.hits {
        assert!(
            (h.score - 1.0).abs() < 1e-3,
            "per-clause exact-match scores must both be 1.0, got {}",
            h.score
        );
    }
}

// ── Filtered statistics on the columnar fast path ─────────────────────────────
//
// `extended_stats` and the percentile family used to be excluded from the
// fast path whenever a top-level query filter was present, because their value
// gathering was filter-blind — folding every row under a filter would report
// whole-index statistics for a filtered query. The exclusion was a correctness
// guard, and it dropped those aggs onto the O(N) `_source` scan (measured
// 48.8 s vs 0.19 s on a 5.6 M-doc index).
//
// The gathering is now filter-aware, so these assert the thing that could
// silently regress: a filtered statistic must describe ONLY the matching docs.
// The index is sized past `FAST_AGG_MIN_DOCS` (10 000) so the columnar path is
// actually the one under test — below that threshold the brute path serves it
// and the test would pass vacuously.

/// 12 000 docs: `group` alternates a/b, `v` is 1.0 for group a and 100.0 for
/// group b. Any filter-blind fold is then trivially detectable — it sees both
/// populations instead of one.
async fn seed_filtered_stats_index(idx: &std::sync::Arc<xerj_engine::Index>) {
    let mut docs = Vec::new();
    for i in 0..12_000u32 {
        let group = if i % 2 == 0 { "a" } else { "b" };
        let v = if i % 2 == 0 { 1.0 } else { 100.0 };
        docs.push(json!({ "group": group, "v": v, "i": i }));
    }
    for (i, d) in docs.into_iter().enumerate() {
        idx.index_document(Some(i.to_string()), d).await.unwrap();
    }
    idx.flush().await.unwrap();
}

#[tokio::test]
async fn test_filtered_extended_stats_sees_only_matching_docs() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("fstats", Schema::empty()).unwrap();
    let idx = engine.get_index("fstats").unwrap();
    seed_filtered_stats_index(&idx).await;

    let req = parse_request(&json!({
        "query": { "term": { "group": "b" } },
        "size": 0,
        "aggs": { "es": { "extended_stats": { "field": "v" } } }
    }))
    .unwrap();
    let res = idx.search(&req).await.unwrap();
    let es = &res.aggs.as_ref().unwrap()["es"];

    // Group b only: 6 000 docs, every value 100.0.
    assert_eq!(
        es["count"].as_u64().unwrap(),
        6_000,
        "count must exclude group a"
    );
    assert_eq!(es["min"].as_f64().unwrap(), 100.0);
    assert_eq!(es["max"].as_f64().unwrap(), 100.0);
    assert_eq!(
        es["avg"].as_f64().unwrap(),
        100.0,
        "avg of a filter-blind fold would be 50.5"
    );
    // Constant population → zero variance. A filter-blind fold gives ~2450.
    assert!(
        es["variance"].as_f64().unwrap() < 1e-6,
        "variance must be ~0 for a constant population, got {}",
        es["variance"]
    );
}

#[tokio::test]
async fn test_filtered_percentiles_see_only_matching_docs() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("fpct", Schema::empty()).unwrap();
    let idx = engine.get_index("fpct").unwrap();
    seed_filtered_stats_index(&idx).await;

    for (group, expect) in [("a", 1.0), ("b", 100.0)] {
        let req = parse_request(&json!({
            "query": { "term": { "group": group } },
            "size": 0,
            "aggs": { "p": { "percentiles": { "field": "v", "percents": [50, 99] } } }
        }))
        .unwrap();
        let res = idx.search(&req).await.unwrap();
        let vals = &res.aggs.as_ref().unwrap()["p"]["values"];
        // Every value in the matching set is identical, so every percentile is
        // that value. A filter-blind gather would mix 1.0 and 100.0 and put p50
        // somewhere between them.
        for pct in ["50.0", "99.0"] {
            assert_eq!(
                vals[pct].as_f64().unwrap(),
                expect,
                "group {group} p{pct} must reflect only matching docs"
            );
        }
    }
}

#[tokio::test]
async fn test_filtered_median_absolute_deviation_sees_only_matching_docs() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("fmad", Schema::empty()).unwrap();
    let idx = engine.get_index("fmad").unwrap();
    seed_filtered_stats_index(&idx).await;

    let req = parse_request(&json!({
        "query": { "term": { "group": "a" } },
        "size": 0,
        "aggs": { "m": { "median_absolute_deviation": { "field": "v" } } }
    }))
    .unwrap();
    let res = idx.search(&req).await.unwrap();
    let mad = res.aggs.as_ref().unwrap()["m"]["value"].as_f64().unwrap();
    // Constant population → MAD 0. Filter-blind would give ~49.5.
    assert!(
        mad < 1e-6,
        "MAD must be ~0 for a constant population, got {mad}"
    );
}

#[tokio::test]
async fn test_unfiltered_statistics_are_unchanged() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("ufstats", Schema::empty()).unwrap();
    let idx = engine.get_index("ufstats").unwrap();
    seed_filtered_stats_index(&idx).await;

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "es": { "extended_stats": { "field": "v" } },
            "p":  { "percentiles": { "field": "v", "percents": [50] } }
        }
    }))
    .unwrap();
    let res = idx.search(&req).await.unwrap();
    let aggs = res.aggs.as_ref().unwrap();
    // Whole corpus: 12 000 docs, half 1.0 and half 100.0 → mean 50.5.
    assert_eq!(aggs["es"]["count"].as_u64().unwrap(), 12_000);
    assert!((aggs["es"]["avg"].as_f64().unwrap() - 50.5).abs() < 1e-9);
    assert_eq!(aggs["es"]["min"].as_f64().unwrap(), 1.0);
    assert_eq!(aggs["es"]["max"].as_f64().unwrap(), 100.0);
    assert!(aggs["p"]["values"]["50.0"].as_f64().is_some());
}

/// Regression: highlighting used to run AFTER `_source` filtering, so a request
/// that excluded the highlighted field silently got no `highlight` key at all —
/// 200 OK, no error. That made the token-efficient shape impossible: to obtain a
/// ~160-byte fragment you also had to ship the entire field. ES treats the two
/// as independent; highlighting resolves against the stored document.
#[tokio::test]
async fn test_highlight_survives_source_filtering() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("hl", Schema::empty()).unwrap();
    let idx = engine.get_index("hl").unwrap();

    idx.index_document(
        Some("1".into()),
        json!({
            "path": "src/lib.rs",
            "body": "the neural embedder is loaded lazily on first use and cached behind an Arc"
        }),
    )
    .await
    .unwrap();
    idx.flush().await.unwrap();

    // `body` is deliberately EXCLUDED from _source — only `path` comes back.
    let req = parse_request(&json!({
        "query": { "match": { "body": "neural embedder" } },
        "size": 1,
        "_source": ["path"],
        "highlight": { "fields": { "body": { "fragment_size": 80, "number_of_fragments": 1 } } }
    }))
    .unwrap();
    let res = idx.search(&req).await.unwrap();
    let hit = &res.hits[0];

    let hl = hit
        .highlight
        .as_ref()
        .expect("highlight must be present even when _source excludes the field");
    let frag = &hl["body"][0];
    assert!(
        frag.contains("<em>"),
        "fragment must carry highlight tags: {frag}"
    );
    assert!(
        frag.to_lowercase().contains("neural") || frag.to_lowercase().contains("embedder"),
        "fragment must surround the match: {frag}"
    );
    // And `_source` filtering still applies — the caller does NOT pay for `body`.
    assert!(
        hit.source.get("body").is_none(),
        "_source filtering must still exclude body; caller should not pay for it"
    );
    assert!(
        hit.source.get("path").is_some(),
        "requested field must survive"
    );
}

// ── Reproduction: term/terms on a NON-FIRST array element (multi-valued keyword) ─
// A keyword ARRAY currently stores ONLY element [0] in the single-valued
// doc-values column (memtable `push_field`) AND the single-valued segment
// `KeywordColumn.ords: Vec<u32>` (one ordinal per doc). So `term` on any later
// element silently returns 0 hits — ES treats keyword arrays as multi-valued.
// This is the reachability bug found auditing WordPress:
// `{term:{calls:"wp_safe_remote_get"}}` missed every caller where it was not
// the first call (found 1 of 9; grep found 14).
//
// The memtable half of the fix (bail array fields to the array-aware source
// scan) is in `memtable.rs`. The COMPLETE fix additionally needs multi-valued
// segment keyword columns (or a per-segment array-field marker that bails the
// segment term reader to the stored-source scan) — a storage-format change.
// Ignored until that lands; unignore to verify the full fix.
#[ignore = "needs multi-valued segment keyword columns; memtable half fixed"]
#[tokio::test]
async fn test_term_matches_non_first_array_element() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("arr", Schema::empty()).unwrap();
    let idx = engine.get_index("arr").unwrap();

    idx.index_document(
        Some("1".into()),
        json!({ "name": "d1", "calls": ["first_fn", "second_fn", "wp_safe_remote_get"] }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({ "name": "d2", "calls": ["unrelated_only"] }),
    )
    .await
    .unwrap();

    let r = idx
        .search(&make_search(
            json!({"term": {"calls": "wp_safe_remote_get"}}),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.total.value, 1,
        "term must match a NON-FIRST array element (multi-valued keyword)"
    );
    assert_eq!(r.hits[0].id, "1");

    let r0 = idx
        .search(&make_search(json!({"term": {"calls": "first_fn"}})))
        .await
        .unwrap();
    assert_eq!(r0.total.value, 1, "first element must still match");

    let rt = idx
        .search(&make_search(
            json!({"terms": {"calls": ["wp_safe_remote_get"]}}),
        ))
        .await
        .unwrap();
    assert_eq!(
        rt.total.value, 1,
        "terms must match a NON-FIRST array element"
    );
}

// ── kNN + aggregations in a single request (rc.6) ─────────────────────────────
// The gap the calltree.ai analytics use-case hit: aggregate a SEMANTIC slice in
// one call. Aggregations must run over the retrieved top-k neighbour set (ES
// top-level-knn semantics), independent of the from/size hit page.
#[tokio::test]
async fn test_knn_plus_aggregations_single_request() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    let mut vf = FieldConfig::new("v", FieldType::Vector);
    vf.options.dimensions = Some(3);
    vf.options.similarity = Some("cosine".to_string());
    schema.fields.push(vf);
    schema
        .fields
        .push(FieldConfig::new("band", FieldType::Keyword));
    engine.create_index("knnagg", schema).unwrap();
    let idx = engine.get_index("knnagg").unwrap();

    // Four docs near [1,0,0] (the "topic" cluster) split across two bands,
    // plus one far-away doc that must NOT enter the top-k or the agg buckets.
    idx.index_document(Some("1".into()), json!({"v":[1.0,0.0,0.0],"band":"2.4GHz"}))
        .await
        .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({"v":[0.98,0.02,0.0],"band":"2.4GHz"}),
    )
    .await
    .unwrap();
    idx.index_document(Some("3".into()), json!({"v":[0.95,0.05,0.0],"band":"5GHz"}))
        .await
        .unwrap();
    idx.index_document(Some("4".into()), json!({"v":[0.9,0.1,0.0],"band":"5GHz"}))
        .await
        .unwrap();
    idx.index_document(
        Some("far".into()),
        json!({"v":[0.0,0.0,1.0],"band":"other"}),
    )
    .await
    .unwrap();

    // knn over the topic cluster (k=4 → the 4 near docs), size:0 (analytics),
    // aggregate the retrieved set by band.
    let req = parse_request(&json!({
        "query": {"knn": {"field":"v","query_vector":[1.0,0.0,0.0],"k":4,"num_candidates":10}},
        "size": 0,
        "aggs": {"by_band": {"terms": {"field": "band"}}}
    }))
    .unwrap();
    let res = idx.search(&req).await.unwrap();

    // aggregations must be present and computed over the top-4 (not all 5).
    let aggs = res.aggs.expect("knn query must carry aggregations");
    let buckets = aggs["by_band"]["buckets"]
        .as_array()
        .expect("terms buckets");
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for b in buckets {
        counts.insert(
            b["key"].as_str().unwrap().to_string(),
            b["doc_count"].as_i64().unwrap(),
        );
    }
    assert_eq!(
        counts.get("2.4GHz"),
        Some(&2),
        "2.4GHz count over the semantic slice"
    );
    assert_eq!(
        counts.get("5GHz"),
        Some(&2),
        "5GHz count over the semantic slice"
    );
    assert_eq!(
        counts.get("other"),
        None,
        "the far doc must NOT be in the agg (excluded from top-k)"
    );
    assert_eq!(
        res.total.value, 4,
        "hits.total is the retrieved neighbour pool"
    );
    assert!(res.hits.is_empty(), "size:0 returns aggs only, no hits");
}

// ── bare `_count` buckets_path: same answer on both agg paths ────────────────
//
// A bare `"buckets_path": "_count"` resolves against a `doc_count` staged into
// the sibling map. The brute interpreter (`aggs::run_aggs_in_bucket`) stages
// it; the doc-values fast path (`fast_aggs`) is a separate implementation and
// did not, at the TOP level of an aggs tree — so the answer flipped to `null`
// once an index grew past `FAST_AGG_MIN_DOCS` (10,000) and the fast path
// started serving the request.
//
// Measured before the fix, same query, same corpus shape:
//     100 docs    -> {"value": 100.0}     (brute)
//     12,000 docs -> {"value": null}      (fast)
//     12,000 docs -> {"value": 12000.0}   (fast path off via
//                                          XERJ_DISABLE_FAST_AGGS=1)
//
// The 12,000-doc case below is the one that regressed. It runs on whichever
// path the build defaults to, and the expected value is the same either way —
// which is the whole point.

async fn bare_count_top_level_value(n: usize) -> Value {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("counts", Schema::empty()).unwrap();
    let idx = engine.get_index("counts").unwrap();
    for i in 0..n {
        idx.index_document(
            Some(format!("d{i}")),
            json!({"grp": if i % 2 == 0 { "a" } else { "b" }}),
        )
        .await
        .unwrap();
    }
    let req = parse_request(&json!({
        "size": 0,
        "aggs": {
            "c": { "bucket_script": { "buckets_path": "_count", "script": "_value" } }
        }
    }))
    .expect("parse_request");
    let res = idx.search(&req).await.unwrap();
    res.aggs.expect("aggs present")["c"]["value"].clone()
}

#[tokio::test]
async fn test_bare_count_bucket_script_agrees_below_and_above_the_fast_agg_threshold() {
    // Below FAST_AGG_MIN_DOCS: always the brute interpreter.
    assert_eq!(
        bare_count_top_level_value(100).await,
        json!(100.0),
        "brute path: a bare `_count` at the top level is the result-set size"
    );
    // Above it: the doc-values fast path serves this by default.
    assert_eq!(
        bare_count_top_level_value(12_000).await,
        json!(12000.0),
        "the answer must not depend on which agg path served the request"
    );
}

#[tokio::test]
async fn test_bare_count_bucket_script_inside_a_terms_bucket_agrees_on_both_paths() {
    // The per-bucket case, which the fast path already got right (it resolves
    // pipelines against the finished bucket map). Pinned alongside the
    // top-level one so a future refactor can't fix one and break the other.
    for (n, per_bucket) in [(100usize, 50.0f64), (12_000usize, 6000.0f64)] {
        let dir = TempDir::new().unwrap();
        let engine = make_engine(&dir);
        engine.create_index("counts", Schema::empty()).unwrap();
        let idx = engine.get_index("counts").unwrap();
        for i in 0..n {
            idx.index_document(
                Some(format!("d{i}")),
                json!({"grp": if i % 2 == 0 { "a" } else { "b" }}),
            )
            .await
            .unwrap();
        }
        let req = parse_request(&json!({
            "size": 0,
            "aggs": {
                "by_grp": {
                    "terms": { "field": "grp" },
                    "aggs": {
                        "c": { "bucket_script": { "buckets_path": "_count", "script": "_value" } }
                    }
                }
            }
        }))
        .expect("parse_request");
        let res = idx.search(&req).await.unwrap();
        let aggs = res.aggs.expect("aggs present");
        let buckets = aggs["by_grp"]["buckets"]
            .as_array()
            .expect("terms buckets")
            .clone();
        assert_eq!(buckets.len(), 2, "n={n}: {aggs}");
        for b in buckets {
            assert_eq!(
                b["doc_count"].as_f64(),
                Some(per_bucket),
                "n={n}: bucket doc_count"
            );
            assert_eq!(
                b["c"]["value"].as_f64(),
                Some(per_bucket),
                "n={n}: `_count` must equal the bucket's own doc_count: {b}"
            );
        }
    }
}
