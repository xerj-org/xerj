//! Randomised cross-segment top-k checks (#3497).
//!
//! Every bounded page (`from`/`size`) must be the matching slice of the full
//! order, whatever segment a hit sits in: `(score DESC, seq_no ASC)` for
//! relevance and `(sort key, seq_no ASC)` for a field sort. Each case builds
//! many small segments (plus, in some cases, a memtable tail, deletes and
//! overwrites), so a better hit routinely sits in a segment walked after the
//! collector is already full. A multi-valued keyword field ships no doc-values
//! column, so `term` on it is answered by the stored scan, which scores a match
//! by how many query tokens appear across every array element
//! (`match_term_frequency`): scores differ between matches. For `term` and a
//! field-sorted `match_all` the expected order comes from an independent brute
//! force over the generated documents; for every other shape it is the
//! engine's own unbounded page (`size` larger than the match count, so nothing
//! is dropped before the final sort).

use super::*;
use serde_json::json;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const VALUES: [&str; 6] = [
    "alpha",
    "alpha beta",
    "beta",
    "gamma alpha alpha",
    "gamma",
    "delta",
];
const CATS: [&str; 4] = ["c0", "c1", "c2", "c3"];
const WORDS: [&str; 5] = ["red", "green", "blue", "red red", "amber"];

#[derive(Clone)]
struct Doc {
    id: String,
    seq: u64,
    tags: Vec<&'static str>,
    rank: i64,
}

struct Built {
    _dir: tempfile::TempDir,
    _engine: crate::Engine,
    idx: Arc<Index>,
    live: Vec<Doc>,
    segments: usize,
}

/// A fresh document. Later segments draw longer arrays, so their matches tend
/// to score higher than the earlier segments' that fill the page first.
fn fresh(rng: &mut Rng, id: String, seq: u64, seg: usize) -> (Doc, Value) {
    let n = 1 + rng.below(2 + seg as u64 % 4) as usize;
    let tags: Vec<&'static str> = (0..n)
        .map(|_| VALUES[rng.below(VALUES.len() as u64) as usize])
        .collect();
    let rank = 1 + rng.below(12) as i64;
    let words: Vec<&str> = (0..1 + rng.below(4))
        .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
        .collect();
    let body = json!({
        "tags": tags,
        "rank": rank,
        "cat": CATS[rng.below(CATS.len() as u64) as usize],
        "body": words.join(" "),
    });
    (
        Doc {
            id,
            seq,
            tags,
            rank,
        },
        body,
    )
}

async fn put(idx: &Arc<Index>, live: &mut Vec<Doc>, (doc, body): (Doc, Value)) {
    let id = doc.id.clone();
    live.retain(|d| d.id != id);
    live.push(doc);
    idx.index_document(Some(id), body).await.unwrap();
}

/// `segs` flushed segments of 1..=24 docs each, then (`tail`) unflushed docs.
/// With `churn`, some docs are deleted and some overwritten (new seq_no, new
/// content), which also turns off the count-authoritative early stop.
async fn build(name: &str, seed: u64, segs: usize, churn: bool, tail: bool) -> Built {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    // No background merge: BM25 statistics are per segment, so a merge that
    // lands between the unbounded and the bounded query would change scores
    // (seen at load 109: `match body` pages 0.6220 vs 0.6216 swapped). The
    // merged layout is covered by an explicit `force_merge` in each case.
    config.merge.min_segments = 1_000_000;
    config.merge.min_merge_count = 1_000_000;
    let engine = crate::Engine::new(config).unwrap();
    let mut schema = Schema::empty();
    for (field, ty) in [
        ("tags", FieldType::Keyword),
        ("cat", FieldType::Keyword),
        ("rank", FieldType::Long),
        ("body", FieldType::Text),
    ] {
        schema.add_field(FieldConfig::new(field, ty)).unwrap();
    }
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut live: Vec<Doc> = Vec::new();
    let mut seq = 0u64;
    let mut next_id = 0usize;
    for seg in 0..segs {
        for _ in 0..1 + rng.below(24) {
            let doc = fresh(&mut rng, format!("d{next_id:05}"), seq, seg);
            next_id += 1;
            seq += 1;
            put(&idx, &mut live, doc).await;
        }
        if churn && seg > 1 {
            for _ in 0..2 {
                let at = rng.below(live.len() as u64) as usize;
                let id = live[at].id.clone();
                if rng.below(2) == 0 {
                    idx.delete_document(&id).await.unwrap();
                    live.retain(|d| d.id != id);
                } else {
                    let doc = fresh(&mut rng, id, seq, seg + 3);
                    put(&idx, &mut live, doc).await;
                }
                seq += 1;
            }
        }
        idx.flush().await.unwrap();
    }
    if tail {
        for _ in 0..1 + rng.below(8) {
            let doc = fresh(&mut rng, format!("d{next_id:05}"), seq, segs + 2);
            next_id += 1;
            seq += 1;
            put(&idx, &mut live, doc).await;
        }
    }
    let segments = idx.store.snapshot().segments.len();
    Built {
        _dir: dir,
        _engine: engine,
        idx,
        live,
        segments,
    }
}

fn tf(tags: &[&str], term: &str) -> usize {
    let q: Vec<String> = term.split_whitespace().map(str::to_lowercase).collect();
    tags.iter()
        .map(|e| {
            let lc = e.to_lowercase();
            let toks: Vec<&str> = lc.split(|c: char| !c.is_alphanumeric()).collect();
            q.iter()
                .map(|t| toks.iter().filter(|x| **x == t.as_str()).count())
                .sum::<usize>()
        })
        .sum()
}

/// Brute-force order for `term` on `tags` (`None`: every doc, score 1).
fn oracle(live: &[Doc], term: Option<&str>, sort: Option<bool>) -> Vec<String> {
    let mut hits: Vec<(usize, &Doc)> = live
        .iter()
        .filter_map(|d| match term {
            None => Some((1, d)),
            Some(t) => d.tags.contains(&t).then(|| (tf(&d.tags, t), d)),
        })
        .collect();
    hits.sort_by(|a, b| {
        let primary = match sort {
            None => b.0.cmp(&a.0),
            Some(true) => a.1.rank.cmp(&b.1.rank),
            Some(false) => b.1.rank.cmp(&a.1.rank),
        };
        primary.then(a.1.seq.cmp(&b.1.seq))
    });
    hits.into_iter().map(|(_, d)| d.id.clone()).collect()
}

async fn page(
    idx: &Index,
    query: &Value,
    sort: Option<bool>,
    from: usize,
    size: usize,
) -> (Vec<String>, u64) {
    let mut body = json!({
        "from": from,
        "size": size,
        "track_total_hits": true,
        "query": query,
    });
    if let Some(asc) = sort {
        body["sort"] = json!([{"rank": if asc { "asc" } else { "desc" }}]);
    }
    let req = xerj_query::parse_request(&body).unwrap();
    let res = idx.search(&req).await.unwrap();
    (
        res.hits.iter().map(|h| h.id.clone()).collect(),
        res.total.value,
    )
}

const SEEDS_A: std::ops::RangeInclusive<u64> = 1..=8;
const SEEDS_B: std::ops::RangeInclusive<u64> = 100..=104;
const SEEDS_C: std::ops::RangeInclusive<u64> = 200..=204;

const PAGES: [(usize, usize); 8] = [
    (0, 1),
    (0, 2),
    (0, 3),
    (0, 5),
    (0, 8),
    (0, 13),
    (2, 3),
    (5, 5),
];

/// Query shapes with an independent oracle (`Some(term)` on `tags`, `None`
/// for match_all) and without one (compared with the unbounded page).
fn shapes() -> Vec<(Value, Option<Option<&'static str>>)> {
    let fvf = |q: Value, mode: &str| {
        json!({"function_score": {
            "query": q,
            "field_value_factor": {"field": "rank", "missing": 1},
            "boost_mode": mode,
        }})
    };
    vec![
        (json!({"term": {"tags": "alpha"}}), Some(Some("alpha"))),
        (json!({"term": {"tags": "beta"}}), Some(Some("beta"))),
        (
            json!({"term": {"tags": "gamma alpha alpha"}}),
            Some(Some("gamma alpha alpha")),
        ),
        (json!({"match_all": {}}), Some(None)),
        (json!({"match": {"tags": "alpha"}}), None),
        (
            json!({"bool": {"should": [
                {"term": {"tags": "alpha"}},
                {"term": {"tags": "beta"}}
            ]}}),
            None,
        ),
        (
            json!({"bool": {
                "must": [{"term": {"tags": "alpha"}}],
                "filter": [{"range": {"rank": {"gte": 4}}}]
            }}),
            None,
        ),
        (fvf(json!({"term": {"cat": "c1"}}), "replace"), None),
        (fvf(json!({"term": {"tags": "alpha"}}), "multiply"), None),
        (fvf(json!({"match_all": {}}), "replace"), None),
        (json!({"match": {"body": "red"}}), None),
        (json!({"term": {"cat": "c2"}}), None),
        (
            json!({"bool": {"should": [
                {"term": {"cat": "c1"}},
                {"range": {"rank": {"gte": 8}}}
            ]}}),
            None,
        ),
        (
            json!({"bool": {
                "must": [{"term": {"cat": "c1"}}],
                "should": [{"range": {"rank": {"gte": 8}}}]
            }}),
            None,
        ),
        (
            json!({"bool": {"should": [
                {"term": {"cat": "c1"}},
                {"term": {"cat": "c2"}},
                {"range": {"rank": {"gte": 9}}},
                {"term": {"tags": "alpha"}}
            ]}}),
            None,
        ),
        (
            json!({"bool": {"must": [
                {"term": {"cat": "c3"}},
                {"range": {"rank": {"gte": 3}}}
            ]}}),
            None,
        ),
        (
            json!({"bool": {"must": [
                {"range": {"rank": {"gte": 2}}}
            ], "should": [
                {"range": {"rank": {"gte": 10, "boost": 3.0}}}
            ]}}),
            None,
        ),
    ]
}

async fn check(b: &Built, label: &str) {
    for (query, independent) in shapes() {
        for sort in [None, Some(true), Some(false)] {
            let full_label = format!("{label} segs={} q={query} sort={sort:?}", b.segments);
            let (unbounded, unbounded_total) = page(&b.idx, &query, sort, 0, 10_000).await;
            let want = match independent {
                // match_all by relevance is arrival order everywhere; skip.
                Some(None) if sort.is_none() => continue,
                Some(term) => {
                    let want = oracle(&b.live, term, sort);
                    assert_eq!(unbounded, want, "{full_label} unbounded vs brute force");
                    want
                }
                None => unbounded,
            };
            assert_eq!(unbounded_total as usize, want.len(), "{full_label} total");
            for (from, size) in PAGES {
                let expect: Vec<String> = want.iter().skip(from).take(size).cloned().collect();
                for pass in ["cold", "warm"] {
                    let (got, total) = page(&b.idx, &query, sort, from, size).await;
                    assert_eq!(got, expect, "{full_label} from={from} size={size} {pass}");
                    assert_eq!(
                        total as usize,
                        want.len(),
                        "{full_label} total from={from} size={size} {pass}"
                    );
                }
            }
        }
    }
}

/// Checks the layout as flushed, then again after a force-merge to a few
/// segments (one large segment pages its best matches, not its first).
async fn check_flushed_then_merged(b: &Built, label: &str, merged_to: usize) {
    check(b, label).await;
    b.idx.force_merge(merged_to).await.unwrap();
    check(b, &format!("{label} merged")).await;
}

#[tokio::test]
async fn bounded_pages_match_brute_force_across_many_segments() {
    for seed in SEEDS_A {
        let segs = 6 + (seed as usize % 3) * 4;
        let b = build(&format!("sstk{seed}"), seed, segs, false, false).await;
        assert!(b.segments > 1, "seed {seed}: {} segments", b.segments);
        check_flushed_then_merged(&b, &format!("seed={seed}"), 1 + seed as usize % 3).await;
    }
}

#[tokio::test]
async fn bounded_pages_match_brute_force_with_a_memtable_tail() {
    for seed in SEEDS_B {
        let b = build(&format!("sstkt{seed}"), seed, 8, false, true).await;
        check(&b, &format!("tail seed={seed}")).await;
    }
}

#[tokio::test]
async fn bounded_pages_match_brute_force_with_deletes_and_overwrites() {
    for seed in SEEDS_C {
        let b = build(&format!("sstkc{seed}"), seed, 10, true, false).await;
        check_flushed_then_merged(&b, &format!("churn seed={seed}"), 2).await;
    }
}
