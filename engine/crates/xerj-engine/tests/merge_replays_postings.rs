//! #876 — a merge must not re-analyse the documents it merges, and the index
//! it produces must be indistinguishable from the one that did.
//!
//! The merge path used to rebuild every merged segment's FTS side-car by
//! walking back to each surviving document's stored `_source`, re-extracting
//! its field values, and re-running the analyzer chain — recomputing postings
//! that were already on disk and already correct. Under size-tiered levelling
//! that happens to every document once per level, which is why the post-index
//! merge tail rivalled the index itself.
//!
//! Two claims are tested here, and they only mean something together:
//!
//! 1. `Index::merge_reanalysed_document_count()` stays at zero across a
//!    forcemerge — the merge really did replay postings. Without this a
//!    silently-always-falling-back preflight would look perfect.
//! 2. Every hit and every `_score` is bit-for-bit what the re-analysing merge
//!    produces, which the `XERJ_MERGE_FTS_REANALYZE` arm computes in the same
//!    process from the same corpus.

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

/// `(id, score bits)` per hit: "the same score" has to mean identical, not
/// close, or a merge that quietly moved `avgdl` would slip through.
async fn hits(idx: &Index, query: Value) -> Vec<(String, u32)> {
    let request = parse_request(&json!({ "query": query, "size": 200 })).expect("parse_request");
    idx.search(&request)
        .await
        .unwrap()
        .hits
        .iter()
        .map(|hit| (hit.id.clone(), hit.score.to_bits()))
        .collect()
}

fn queries() -> Vec<Value> {
    vec![
        // Scored text: BM25 over doc_freq, term freq, field length and avgdl
        // — every statistic the merge has to carry.
        json!({"match": {"body": "quick"}}),
        json!({"match": {"body": "otter"}}),
        json!({"match": {"body": "quick otter settled"}}),
        // Positions.
        json!({"match_phrase": {"body": "quick brown"}}),
        // Must NOT match: the two values of a multi-valued field are held
        // apart by the position-increment gap.
        json!({"match_phrase": {"body": "cobalt second"}}),
        // Docs-only (keyword) posting lists.
        json!({"term": {"tags": "tag-3"}}),
        json!({"term": {"colour": "cobalt"}}),
        json!({"match_all": {}}),
    ]
}

const DOC_COUNT: usize = 400;

fn document(index: usize) -> Value {
    const NOUNS: [&str; 6] = ["fox", "hound", "otter", "falcon", "badger", "heron"];
    const COLOURS: [&str; 4] = ["amber", "cobalt", "russet", "verdant"];
    let noun = NOUNS[index % NOUNS.len()];
    json!({
        "body": if index.is_multiple_of(7) {
            json!([
                format!("quick brown {noun} leapt past a quick {noun} and settled at {index}"),
                format!("a second value about {index}"),
            ])
        } else {
            json!(format!(
                "quick brown {noun} leapt past a quick {noun} and settled at {index}"
            ))
        },
        "tags": [format!("tag-{}", index % 11), format!("group-{}", index % 3)],
        "colour": COLOURS[index % COLOURS.len()],
        "rank": index,
    })
}

fn schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("colour", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("rank", FieldType::Long));
    schema
}

/// Ingest the corpus in eight flushes (so the merge has eight inputs), delete
/// and overwrite a slice of it (so the merge has documents to drop and stale
/// copies to skip), then forcemerge to one segment.
async fn seed_and_merge(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    engine.create_index(name, schema()).unwrap();
    let idx = engine.get_index(name).unwrap();

    for index in 0..DOC_COUNT {
        idx.index_document(Some(index.to_string()), document(index))
            .await
            .unwrap();
        if (index + 1).is_multiple_of(50) {
            idx.flush().await.unwrap();
        }
    }
    // Deleted documents leave tombstones the merge has to honour.
    for index in (0..DOC_COUNT).step_by(11) {
        idx.delete_document(&index.to_string()).await.unwrap();
    }
    // Overwritten documents leave a stale copy in an older segment that the
    // merge has to skip while keeping the new one.
    for index in (3..DOC_COUNT).step_by(37) {
        let mut updated = document(index);
        updated["body"] = json!(format!("revised heron notes for {index}"));
        idx.index_document(Some(index.to_string()), updated)
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let merged = idx.force_merge(1).await.unwrap();
    assert!(merged >= 1, "forcemerge must actually merge something");
    idx
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forcemerge_replays_postings_and_changes_no_hit_or_score() {
    // Arm A: the postings merge (the default).
    let replay_dir = TempDir::new().unwrap();
    let replay_engine = make_engine(&replay_dir);
    let replayed = seed_and_merge(&replay_engine, "replayed").await;
    assert_eq!(
        replayed.merge_reanalysed_document_count(),
        0,
        "the forcemerge fell back to re-analysing documents, so this test would \
         be comparing the old path against itself"
    );
    let mut replay_hits = Vec::new();
    for query in queries() {
        replay_hits.push(hits(&replayed, query).await);
    }

    // Arm B: the same corpus, merged the old way. The escape hatch is a
    // process-wide env var, so this arm runs after arm A has finished
    // reading, and this test owns its binary.
    std::env::set_var("XERJ_MERGE_FTS_REANALYZE", "1");
    let rebuild_dir = TempDir::new().unwrap();
    let rebuild_engine = make_engine(&rebuild_dir);
    let rebuilt = seed_and_merge(&rebuild_engine, "rebuilt").await;
    std::env::remove_var("XERJ_MERGE_FTS_REANALYZE");
    assert!(
        rebuilt.merge_reanalysed_document_count() > 0,
        "the escape hatch did not put the merge back on the re-analysing path, \
         so the comparison arm is not the old behaviour"
    );

    for (query, expected) in queries().into_iter().zip(replay_hits) {
        let actual = hits(&rebuilt, query.clone()).await;
        assert_eq!(
            actual, expected,
            "a merge that replayed postings returned different hits or scores than \
             one that re-analysed the same documents, for {query}"
        );
    }
}

// ── Measurement ──────────────────────────────────────────────────────────────

/// Peak resident set of this process so far, in MiB (`VmHWM`).
#[cfg(target_os = "linux")]
fn peak_rss_mib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kib| kib / 1024)
        .unwrap_or(0)
}
#[cfg(not(target_os = "linux"))]
fn peak_rss_mib() -> u64 {
    0
}

/// The #876 A/B, as a rerunnable measurement rather than an assertion.
///
/// Run each arm in its OWN process — the peak-RSS figure is a process-wide
/// high-water mark, so two arms in one process would report the larger of
/// them twice:
///
/// ```sh
/// cargo test --release -p xerj-engine --test merge_replays_postings -- \
///     --ignored --nocapture measure_forcemerge_cost
/// XERJ_MERGE_FTS_REANALYZE=1 cargo test --release -p xerj-engine \
///     --test merge_replays_postings -- --ignored --nocapture measure_forcemerge_cost
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement, not an assertion — see the doc comment for the two commands"]
async fn measure_forcemerge_cost() {
    // Shaped after the corpus in the issue: a few thousand large text
    // documents (~150 MB of source and prose across ~4 700 files), not many
    // small ones. The merge's old cost was proportional to the TEXT, so the
    // document size is the part that has to be realistic.
    let docs: usize = std::env::var("XERJ_876_DOCS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_000);
    let words_per_doc: usize = std::env::var("XERJ_876_WORDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4_000);
    const WORDS: [&str; 16] = [
        "retrieval",
        "segment",
        "posting",
        "analyzer",
        "tokenizer",
        "merge",
        "compaction",
        "throughput",
        "latency",
        "vector",
        "dictionary",
        "quantised",
        "checkpoint",
        "tombstone",
        "ordinal",
        "normalisation",
    ];

    let arm = if std::env::var("XERJ_MERGE_FTS_REANALYZE").is_ok() {
        "re-analysing merge (old path)"
    } else {
        "postings merge (#876)"
    };

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("measure", schema()).unwrap();
    let idx = engine.get_index("measure").unwrap();

    let ingest = std::time::Instant::now();
    let mut bytes: u64 = 0;
    for index in 0..docs {
        // A wide vocabulary with a skewed reuse pattern, so the term
        // dictionary is the shape real text produces rather than sixteen
        // words repeated — the postings replay pays for distinct terms and
        // the analyzer pays for tokens, and both have to be represented.
        let body: String = (0..words_per_doc)
            .map(|word| {
                let seed = index * 7919 + word * 104_729;
                format!("{}{}", WORDS[seed % WORDS.len()], seed % 4_096)
            })
            .collect::<Vec<_>>()
            .join(" ");
        bytes += body.len() as u64;
        idx.index_document(
            Some(index.to_string()),
            json!({
                "body": body,
                "tags": [format!("tag-{}", index % 97), format!("group-{}", index % 7)],
                "colour": WORDS[index % WORDS.len()],
                "rank": index,
            }),
        )
        .await
        .unwrap();
        if (index + 1).is_multiple_of(250) {
            idx.flush().await.unwrap();
        }
    }
    idx.flush().await.unwrap();
    let ingest = ingest.elapsed();
    let rss_after_ingest = peak_rss_mib();

    let merge = std::time::Instant::now();
    let merged = idx.force_merge(1).await.unwrap();
    let merge = merge.elapsed();

    eprintln!(
        "#876 {arm}\n  docs={docs} text={}MiB ingest={:.2}s batches_merged={merged}\n  \
         FORCEMERGE={:.2}s  re-analysed_docs={}  peak_rss={}MiB (was {}MiB after ingest)",
        bytes / (1024 * 1024),
        ingest.as_secs_f64(),
        merge.as_secs_f64(),
        idx.merge_reanalysed_document_count(),
        peak_rss_mib(),
        rss_after_ingest,
    );
}
