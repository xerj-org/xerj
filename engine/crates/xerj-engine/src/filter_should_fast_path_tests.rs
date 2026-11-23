//! `bool{filter:[range], should:[..]}` with no `must` (#3518).
//!
//! A numeric `range` has no postings. #3507 made this shape decline the FTS
//! projection (a skipped filter left only `should` clauses, which then became
//! required), so every flushed segment answered it with the stored-doc scan:
//! correct, but it decodes every stored doc. The range filter now projects as
//! its exact doc-values match set, so the FTS bool narrows by it and scores
//! the `should` clauses with BM25.
//!
//! The randomised test checks, on a flushed index, a half-flushed one and a
//! memtable-only one, that every generated query returns the hit set and
//! total of the stored-doc scan and of a brute force over the generated
//! documents. The stored-scan reference is the same query with each `range`
//! filter wrapped in a filter-only `bool` (same meaning; it does not project,
//! so it keeps the #3507 decline). Scores on the flushed index are the BM25
//! of the matched `should` clauses (0.0 when none matched, as in ES).

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

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }
}

const WORDS: [&str; 7] = ["cat", "dog", "bird", "fish", "a", "red", "the"];
const COLORS: [&str; 3] = ["red", "blue", "green"];

struct Doc {
    id: String,
    t: Option<Vec<&'static str>>,
    n: Option<i64>,
    m: Option<f64>,
    k: Option<&'static str>,
}

impl Doc {
    fn source(&self) -> Value {
        let mut o = serde_json::Map::new();
        if let Some(t) = &self.t {
            o.insert("t".into(), json!(t.join(" ")));
        }
        if let Some(n) = self.n {
            o.insert("n".into(), json!(n));
        }
        if let Some(m) = self.m {
            o.insert("m".into(), json!(m));
        }
        if let Some(k) = self.k {
            o.insert("k".into(), json!(k));
        }
        Value::Object(o)
    }

    fn has_word(&self, w: &str) -> bool {
        self.t.as_ref().is_some_and(|t| t.contains(&w))
    }
}

fn gen_doc(rng: &mut Rng, i: usize) -> Doc {
    let t = (rng.below(10) != 0).then(|| {
        (0..1 + rng.below(5))
            .map(|_| rng.pick(&WORDS))
            .collect::<Vec<_>>()
    });
    let n = (rng.below(6) != 0).then(|| rng.below(20) as i64);
    let m = (rng.below(4) != 0).then(|| rng.below(100) as f64 / 4.0);
    let k = (rng.below(5) != 0).then(|| rng.pick(&COLORS));
    Doc {
        id: format!("d{i:04}"),
        t,
        n,
        m,
        k,
    }
}

/// A numeric bound: `(gte|gt, value)` / `(lte|lt, value)`.
#[derive(Clone, Copy)]
struct Bound {
    strict: bool,
    v: f64,
}

#[derive(Clone, Copy)]
enum Filter {
    Range {
        field: &'static str,
        lo: Option<Bound>,
        hi: Option<Bound>,
    },
    TermK(&'static str),
}

#[derive(Clone, Copy)]
enum Leaf {
    Match(&'static str),
    TermK(&'static str),
    TermN(i64),
}

struct Q {
    filter: Vec<Filter>,
    should: Vec<Leaf>,
    must_not: Vec<Leaf>,
    msm: Option<u32>,
}

fn gen_bound(rng: &mut Rng, field: &str) -> Bound {
    let v = if field == "n" {
        rng.below(20) as f64
    } else {
        rng.below(100) as f64 / 4.0
    };
    Bound {
        strict: rng.below(2) == 0,
        v,
    }
}

fn gen_filter(rng: &mut Rng) -> Filter {
    if rng.below(5) == 0 {
        return Filter::TermK(rng.pick(&COLORS));
    }
    let field = if rng.below(3) == 0 { "m" } else { "n" };
    let lo = (rng.below(3) != 0).then(|| gen_bound(rng, field));
    let hi = (lo.is_none() || rng.below(2) == 0).then(|| gen_bound(rng, field));
    Filter::Range { field, lo, hi }
}

fn gen_leaf(rng: &mut Rng) -> Leaf {
    match rng.below(4) {
        0 | 1 => Leaf::Match(rng.pick(&WORDS)),
        2 => Leaf::TermK(rng.pick(&COLORS)),
        _ => Leaf::TermN(rng.below(20) as i64),
    }
}

fn gen_q(rng: &mut Rng) -> Q {
    let mut filter: Vec<Filter> = (0..1 + rng.below(2)).map(|_| gen_filter(rng)).collect();
    if !filter.iter().any(|f| matches!(f, Filter::Range { .. })) {
        filter.push(Filter::Range {
            field: "n",
            lo: Some(gen_bound(rng, "n")),
            hi: None,
        });
    }
    let text_only = rng.below(2) == 0;
    let should: Vec<Leaf> = (0..1 + rng.below(3))
        .map(|_| {
            if text_only {
                Leaf::Match(rng.pick(&WORDS))
            } else {
                gen_leaf(rng)
            }
        })
        .collect();
    let must_not: Vec<Leaf> = (0..rng.below(3).saturating_sub(1))
        .map(|_| gen_leaf(rng))
        .collect();
    let msm = match rng.below(4) {
        0 | 1 => None,
        2 => Some(0),
        _ => Some(1 + rng.below(should.len() as u64) as u32),
    };
    Q {
        filter,
        should,
        must_not,
        msm,
    }
}

fn range_json(field: &str, lo: Option<Bound>, hi: Option<Bound>) -> Value {
    let mut r = serde_json::Map::new();
    if let Some(b) = lo {
        r.insert(if b.strict { "gt" } else { "gte" }.into(), json!(b.v));
    }
    if let Some(b) = hi {
        r.insert(if b.strict { "lt" } else { "lte" }.into(), json!(b.v));
    }
    json!({"range": {field: Value::Object(r)}})
}

fn filter_json(f: &Filter, wrap_ranges: bool) -> Value {
    match f {
        Filter::Range { field, lo, hi } => {
            let r = range_json(field, *lo, *hi);
            if wrap_ranges {
                json!({"bool": {"filter": [r]}})
            } else {
                r
            }
        }
        Filter::TermK(v) => json!({"term": {"k": v}}),
    }
}

fn leaf_json(l: &Leaf) -> Value {
    match l {
        Leaf::Match(w) => json!({"match": {"t": w}}),
        Leaf::TermK(v) => json!({"term": {"k": v}}),
        Leaf::TermN(v) => json!({"term": {"n": v}}),
    }
}

/// The query. `wrap_ranges` gives the stored-scan reference.
fn q_json(q: &Q, wrap_ranges: bool) -> Value {
    let mut b = serde_json::Map::new();
    b.insert(
        "filter".into(),
        q.filter
            .iter()
            .map(|f| filter_json(f, wrap_ranges))
            .collect(),
    );
    b.insert("should".into(), q.should.iter().map(leaf_json).collect());
    if !q.must_not.is_empty() {
        b.insert(
            "must_not".into(),
            q.must_not.iter().map(leaf_json).collect(),
        );
    }
    if let Some(n) = q.msm {
        b.insert("minimum_should_match".into(), json!(n));
    }
    json!({ "bool": Value::Object(b) })
}

/// The `should` clauses alone (BM25 reference for the scores).
fn should_json(q: &Q) -> Value {
    let mut b = serde_json::Map::new();
    b.insert("should".into(), q.should.iter().map(leaf_json).collect());
    if let Some(n) = q.msm.filter(|n| *n >= 1) {
        b.insert("minimum_should_match".into(), json!(n));
    }
    json!({ "bool": Value::Object(b) })
}

fn leaf_eval(l: &Leaf, d: &Doc) -> bool {
    match l {
        Leaf::Match(w) => d.has_word(w),
        Leaf::TermK(v) => d.k == Some(*v),
        Leaf::TermN(v) => d.n == Some(*v),
    }
}

fn filter_eval(f: &Filter, d: &Doc) -> bool {
    match f {
        Filter::Range { field, lo, hi } => {
            let x = match *field {
                "n" => d.n.map(|n| n as f64),
                _ => d.m,
            };
            x.is_some_and(|x| {
                lo.is_none_or(|b| if b.strict { x > b.v } else { x >= b.v })
                    && hi.is_none_or(|b| if b.strict { x < b.v } else { x <= b.v })
            })
        }
        Filter::TermK(v) => d.k == Some(*v),
    }
}

/// ES semantics: the `filter` makes `should` optional unless
/// `minimum_should_match` says otherwise.
fn q_eval(q: &Q, d: &Doc) -> bool {
    let matched = q.should.iter().filter(|l| leaf_eval(l, d)).count() as u32;
    q.filter.iter().all(|f| filter_eval(f, d))
        && !q.must_not.iter().any(|l| leaf_eval(l, d))
        && matched >= q.msm.unwrap_or(0)
}

async fn build(docs: &[Doc]) -> (tempfile::TempDir, crate::Engine, [Arc<Index>; 3]) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let engine = crate::Engine::new(config).unwrap();
    let mut out = Vec::new();
    for name in ["seg", "mixed", "mem"] {
        let mut schema = Schema::empty();
        for (field, ty) in [
            ("t", FieldType::Text),
            ("n", FieldType::Long),
            ("m", FieldType::Double),
            ("k", FieldType::Keyword),
        ] {
            schema.add_field(FieldConfig::new(field, ty)).unwrap();
        }
        engine.create_index(name, schema).unwrap();
        let idx = engine.get_index(name).unwrap();
        let half = docs.len() / 2;
        for (i, d) in docs.iter().enumerate() {
            if name == "mixed" && i == half {
                idx.flush().await.unwrap();
            }
            idx.index_document(Some(d.id.clone()), d.source())
                .await
                .unwrap();
        }
        if name == "seg" {
            idx.flush().await.unwrap();
        }
        out.push(idx);
    }
    let [seg, mixed, mem]: [Arc<Index>; 3] = out.try_into().ok().unwrap();
    assert!(!seg.store.snapshot().segments.is_empty(), "seg is flushed");
    assert!(
        mem.store.snapshot().segments.is_empty(),
        "mem stays unflushed"
    );
    (dir, engine, [seg, mixed, mem])
}

async fn run(idx: &Index, query: &Value) -> (Vec<(String, f32)>, u64) {
    let body = json!({"size": 1000, "track_total_hits": true, "query": query});
    let req = xerj_query::parse_request(&body).unwrap();
    let res = idx.search(&req).await.unwrap();
    let mut hits: Vec<(String, f32)> = res.hits.iter().map(|h| (h.id.clone(), h.score)).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    (hits, res.total.value)
}

fn ids(hits: &[(String, f32)]) -> Vec<&str> {
    hits.iter().map(|(id, _)| id.as_str()).collect()
}

#[tokio::test]
async fn filter_range_should_matches_the_stored_scan_3518() {
    // `XERJ_FILTER_SHOULD_SEEDS=N` widens the sweep for a local soak.
    let seeds: u64 = std::env::var("XERJ_FILTER_SHOULD_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let mut checked = 0u64;
    let mut scored = 0u64;
    for seed in 1..=seeds {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        let docs: Vec<Doc> = (0..300).map(|i| gen_doc(&mut rng, i)).collect();
        let (_dir, _engine, [seg, mixed, mem]) = build(&docs).await;
        for _ in 0..120 {
            let q = gen_q(&mut rng);
            let fast = q_json(&q, false);
            let reference = q_json(&q, true);
            let expected: Vec<&str> = docs
                .iter()
                .filter(|d| q_eval(&q, d))
                .map(|d| d.id.as_str())
                .collect();
            let (seg_hits, seg_total) = run(&seg, &fast).await;
            let (scan_hits, scan_total) = run(&seg, &reference).await;
            assert_eq!(
                (ids(&seg_hits), seg_total),
                (expected.clone(), expected.len() as u64),
                "seed {seed}, segment: {fast}"
            );
            assert_eq!(
                (ids(&scan_hits), scan_total),
                (ids(&seg_hits), seg_total),
                "seed {seed}, stored scan vs fast path: {reference}"
            );
            for (label, idx) in [("mixed", &mixed), ("memtable", &mem)] {
                let (hits, total) = run(idx, &fast).await;
                assert_eq!(
                    (ids(&hits), total),
                    (expected.clone(), expected.len() as u64),
                    "seed {seed}, {label}: {fast}"
                );
            }
            // Scores, when every `should` is a text `match` (other leaves do
            // not all project, and then the stored scan answers both
            // queries with its own scorer): the BM25 of the matched `should`
            // clauses, as the same segment scores the `should` clauses alone.
            if !q.should.iter().all(|l| matches!(l, Leaf::Match(_))) {
                checked += 1;
                continue;
            }
            let (should_hits, _) = run(&seg, &should_json(&q)).await;
            let should_score: HashMap<&str, f32> = should_hits
                .iter()
                .map(|(id, s)| (id.as_str(), *s))
                .collect();
            for (id, score) in &seg_hits {
                let want = should_score.get(id.as_str()).copied().unwrap_or(0.0);
                assert!(
                    (score - want).abs() <= 1e-5 * want.abs().max(1.0),
                    "seed {seed}, {id}: score {score}, should-only {want}: {fast}"
                );
                scored += 1;
            }
            checked += 1;
        }
    }
    assert_eq!(checked, seeds * 120);
    assert!(scored > 0, "the sweep scored some hits");
}

/// The fast path is taken: the probe projects the shape (so segments take
/// the FTS bool), and a range wrapped in a filter-only `bool` still declines.
#[test]
fn filter_range_beside_should_projects_3518() {
    let mut schema = Schema::empty();
    for (field, ty) in [("t", FieldType::Text), ("n", FieldType::Long)] {
        schema.add_field(FieldConfig::new(field, ty)).unwrap();
    }
    let fields = date_doc_field_names(&schema);
    let text = vec!["t".to_string()];
    let exact = HashSet::new();
    let project = |body: Value| {
        let q = xerj_query::parse_query(&body).unwrap();
        query_node_to_fts_projected(
            &q,
            &text,
            &exact,
            &exact,
            SegmentAnalyzerBinding::Standard,
            None,
            DateDocs::Probe(&fields),
        )
    };
    let range = json!({"range": {"n": {"gte": 3}}});
    for msm in [None, Some(0), Some(1)] {
        let mut b = json!({"filter": [range.clone()], "should": [{"match": {"t": "cat"}}]});
        if let Some(n) = msm {
            b["minimum_should_match"] = json!(n);
        }
        let Some(FtsQuery::Bool(bq)) = project(json!({"bool": b})) else {
            panic!("msm {msm:?}: the shape projects");
        };
        assert_eq!(
            bq.filter.len(),
            1,
            "msm {msm:?}: the range is the FTS filter"
        );
        assert!(matches!(bq.filter[0], FtsQuery::DocScores(_)));
    }
    // A `must` keeps the residual gate (the range is not a doc set there).
    let Some(FtsQuery::Bool(bq)) = project(json!({"bool": {
        "must": [{"match": {"t": "cat"}}], "filter": [range.clone()]
    }})) else {
        panic!("must + filter projects");
    };
    assert!(bq.filter.is_empty());
    // The stored-scan reference shape still declines.
    assert!(project(json!({"bool": {
        "filter": [{"bool": {"filter": [range]}}],
        "should": [{"match": {"t": "cat"}}]
    }}))
    .is_none());
}

/// #3524: the ES score of three filter-context shapes, in the memtable, on a
/// segment and half flushed (both arms answer).
#[tokio::test]
async fn filter_context_shapes_score_the_es_value_3524() {
    let docs: Vec<Doc> = (0..6)
        .map(|i| Doc {
            id: format!("d{i:04}"),
            t: Some(vec!["cat"]),
            n: Some(i as i64),
            m: None,
            k: Some(if i % 2 == 0 { "red" } else { "blue" }),
        })
        .collect();
    let (_dir, _engine, indices) = build(&docs).await;
    let cases = [
        (
            json!({"bool": {"filter": [{"range": {"n": {"gte": 1}}}], "boost": 2}}),
            5,
            0.0,
        ),
        (
            json!({"bool": {"filter": [{"term": {"k": "red"}}], "boost": 2}}),
            3,
            0.0,
        ),
        (
            json!({"bool": {"filter": [{"range": {"n": {"gte": 1}}}],
                "should": [{"term": {"k": "nope"}}]}}),
            5,
            0.0,
        ),
        (
            json!({"bool": {"filter": [{"term": {"k": "red"}}],
                "should": [{"match": {"t": "nope"}}]}}),
            3,
            0.0,
        ),
        (
            json!({"bool": {"must": [{"constant_score": {
                "filter": {"range": {"n": {"gte": 1}}}, "boost": 3.5}}]}}),
            5,
            3.5,
        ),
        (
            json!({"bool": {"must": [{"constant_score": {
                "filter": {"term": {"k": "red"}}, "boost": 3.5}}]}}),
            3,
            3.5,
        ),
    ];
    for (query, total, score) in cases {
        for (label, idx) in ["segment", "mixed", "memtable"].iter().zip(&indices) {
            let (hits, got) = run(idx, &query).await;
            assert_eq!(got, total, "{label}: {query}");
            for (id, s) in &hits {
                assert_eq!(*s, score, "{label}, {id}: {query}");
            }
        }
    }
}
