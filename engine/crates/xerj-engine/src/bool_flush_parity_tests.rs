//! Randomised memtable-vs-segment parity for `bool` (#3507).
//!
//! The same documents are written to three indices: one left in the
//! memtable, one flushed to a segment, and one half flushed and half in the
//! memtable. Every generated `bool` (`must`/`filter`/`should`/`must_not` over
//! `match`/`term`/`range` leaves, sometimes one nested `bool`) must return the
//! same hit set on all three, and that set must equal an independent brute
//! force over the generated documents.
//!
//! The text vocabulary holds one-letter words on purpose. #3507: the stored
//! source matcher dropped one-letter alphabetic query tokens, so a flushed
//! `bool{must:[match t:"a"], filter:[range]}` returned 0 of 3 hits while the
//! memtable returned 3, and `must_not: match "a"` excluded nothing on either.

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

const WORDS: [&str; 9] = ["a", "i", "b", "cat", "dog", "bird", "the", "x", "7"];
const COLORS: [&str; 3] = ["red", "blue", "green"];

struct Doc {
    id: String,
    t: Option<Vec<&'static str>>,
    n: Option<i64>,
    k: Option<&'static str>,
}

impl Doc {
    fn source(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(t) = &self.t {
            m.insert("t".into(), json!(t.join(" ")));
        }
        if let Some(n) = self.n {
            m.insert("n".into(), json!(n));
        }
        if let Some(k) = self.k {
            m.insert("k".into(), json!(k));
        }
        Value::Object(m)
    }

    fn has_word(&self, w: &str) -> bool {
        self.t.as_ref().is_some_and(|t| t.contains(&w))
    }
}

fn gen_doc(rng: &mut Rng, i: usize) -> Doc {
    let t = (rng.below(10) != 0).then(|| {
        (0..1 + rng.below(4))
            .map(|_| rng.pick(&WORDS))
            .collect::<Vec<_>>()
    });
    let n = (rng.below(8) != 0).then(|| rng.below(10) as i64);
    let k = (rng.below(8) != 0).then(|| rng.pick(&COLORS));
    Doc {
        id: format!("d{i:04}"),
        t,
        n,
        k,
    }
}

enum Q {
    Match(&'static str),
    Match2(&'static str, &'static str, bool),
    TermK(&'static str),
    TermN(i64),
    Range(Option<i64>, Option<i64>),
    Bool(Box<B>),
}

#[derive(Default)]
struct B {
    must: Vec<Q>,
    filter: Vec<Q>,
    should: Vec<Q>,
    must_not: Vec<Q>,
}

fn gen_leaf(rng: &mut Rng) -> Q {
    match rng.below(5) {
        0 | 1 => Q::Match(rng.pick(&WORDS)),
        2 => Q::Match2(rng.pick(&WORDS), rng.pick(&WORDS), rng.below(2) == 0),
        3 => {
            if rng.below(2) == 0 {
                Q::TermK(rng.pick(&COLORS))
            } else {
                Q::TermN(rng.below(10) as i64)
            }
        }
        _ => {
            let lo = (rng.below(3) != 0).then(|| rng.below(10) as i64);
            let hi = (lo.is_none() || rng.below(2) == 0).then(|| rng.below(10) as i64);
            Q::Range(lo, hi)
        }
    }
}

fn gen_bool(rng: &mut Rng, depth: u32) -> B {
    loop {
        let mut b = B::default();
        for slot in 0..4 {
            for _ in 0..rng.below(3) {
                let q = if depth == 0 && rng.below(6) == 0 {
                    Q::Bool(Box::new(gen_bool(rng, depth + 1)))
                } else {
                    gen_leaf(rng)
                };
                match slot {
                    0 => b.must.push(q),
                    1 => b.filter.push(q),
                    2 => b.should.push(q),
                    _ => b.must_not.push(q),
                }
            }
        }
        if !(b.must.is_empty() && b.filter.is_empty() && b.should.is_empty()) {
            return b;
        }
        if !b.must_not.is_empty() {
            return b;
        }
    }
}

fn q_json(q: &Q) -> Value {
    match q {
        Q::Match(w) => json!({"match": {"t": w}}),
        Q::Match2(a, b, and) => json!({"match": {"t": {
            "query": format!("{a} {b}"),
            "operator": if *and { "and" } else { "or" },
        }}}),
        Q::TermK(v) => json!({"term": {"k": v}}),
        Q::TermN(v) => json!({"term": {"n": v}}),
        Q::Range(lo, hi) => {
            let mut r = serde_json::Map::new();
            if let Some(lo) = lo {
                r.insert("gte".into(), json!(lo));
            }
            if let Some(hi) = hi {
                r.insert("lte".into(), json!(hi));
            }
            json!({"range": {"n": Value::Object(r)}})
        }
        Q::Bool(b) => b_json(b),
    }
}

fn b_json(b: &B) -> Value {
    let list = |qs: &[Q]| Value::Array(qs.iter().map(q_json).collect());
    let mut m = serde_json::Map::new();
    for (name, qs) in [
        ("must", &b.must),
        ("filter", &b.filter),
        ("should", &b.should),
        ("must_not", &b.must_not),
    ] {
        if !qs.is_empty() {
            m.insert(name.into(), list(qs));
        }
    }
    json!({ "bool": Value::Object(m) })
}

/// The brute force: ES semantics over the generated fields. `standard`
/// analysis of these lowercase ASCII words is the word itself.
fn q_eval(q: &Q, d: &Doc) -> bool {
    match q {
        Q::Match(w) => d.has_word(w),
        Q::Match2(a, b, true) => d.has_word(a) && d.has_word(b),
        Q::Match2(a, b, false) => d.has_word(a) || d.has_word(b),
        Q::TermK(v) => d.k == Some(*v),
        Q::TermN(v) => d.n == Some(*v),
        Q::Range(lo, hi) => {
            d.n.is_some_and(|n| lo.is_none_or(|lo| n >= lo) && hi.is_none_or(|hi| n <= hi))
        }
        Q::Bool(b) => b_eval(b, d),
    }
}

fn b_eval(b: &B, d: &Doc) -> bool {
    let required = !b.must.is_empty() || !b.filter.is_empty();
    b.must.iter().all(|q| q_eval(q, d))
        && b.filter.iter().all(|q| q_eval(q, d))
        && !b.must_not.iter().any(|q| q_eval(q, d))
        && (required || b.should.is_empty() || b.should.iter().any(|q| q_eval(q, d)))
}

struct Built {
    _dir: tempfile::TempDir,
    _engine: crate::Engine,
    mem: Arc<Index>,
    seg: Arc<Index>,
    mixed: Arc<Index>,
}

async fn build(docs: &[Doc]) -> Built {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let engine = crate::Engine::new(config).unwrap();
    let mut out = Vec::new();
    for name in ["mem", "seg", "mixed"] {
        let mut schema = Schema::empty();
        for (field, ty) in [
            ("t", FieldType::Text),
            ("n", FieldType::Long),
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
    let mixed = out.pop().unwrap();
    let seg = out.pop().unwrap();
    let mem = out.pop().unwrap();
    assert!(
        mem.store.snapshot().segments.is_empty(),
        "mem stays unflushed"
    );
    assert!(!seg.store.snapshot().segments.is_empty(), "seg is flushed");
    Built {
        _dir: dir,
        _engine: engine,
        mem,
        seg,
        mixed,
    }
}

async fn hit_set(idx: &Index, query: &Value) -> (Vec<String>, u64) {
    let body = json!({"size": 1000, "track_total_hits": true, "query": query});
    let req = xerj_query::parse_request(&body).unwrap();
    let res = idx.search(&req).await.unwrap();
    let mut ids: Vec<String> = res.hits.iter().map(|h| h.id.clone()).collect();
    ids.sort();
    (ids, res.total.value)
}

#[tokio::test]
async fn bool_hit_sets_match_across_memtable_and_segment_3507() {
    // `XERJ_BOOL_PARITY_SEEDS=N` widens the sweep for a local soak.
    let seeds: u64 = std::env::var("XERJ_BOOL_PARITY_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let mut checked = 0usize;
    for seed in 1..=seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let docs: Vec<Doc> = (0..120).map(|i| gen_doc(&mut rng, i)).collect();
        let built = build(&docs).await;
        for _ in 0..150 {
            let b = gen_bool(&mut rng, 0);
            let query = b_json(&b);
            let mut expected: Vec<String> = docs
                .iter()
                .filter(|d| b_eval(&b, d))
                .map(|d| d.id.clone())
                .collect();
            expected.sort();
            for (label, idx) in [
                ("memtable", &built.mem),
                ("segment", &built.seg),
                ("mixed", &built.mixed),
            ] {
                let (ids, total) = hit_set(idx, &query).await;
                assert_eq!(
                    (ids.as_slice(), total),
                    (expected.as_slice(), expected.len() as u64),
                    "seed {seed}, {label}: {query}"
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked as u64, seeds * 150);
}

/// The #3507 report, exactly: one-letter `match` inside a `bool` with a
/// `range` filter, before and after `_flush`.
#[tokio::test]
async fn one_letter_match_with_range_filter_survives_flush_3507() {
    let docs: Vec<Doc> = [("a cat", 1), ("a dog", 2), ("a bird", 3), ("the cat", 4)]
        .iter()
        .enumerate()
        .map(|(i, (t, n))| Doc {
            id: format!("d{i}"),
            t: Some(
                t.split(' ')
                    .map(|w| *WORDS.iter().find(|x| **x == w).unwrap())
                    .collect(),
            ),
            n: Some(*n),
            k: None,
        })
        .collect();
    let built = build(&docs).await;
    let must_a = json!({"bool": {
        "must": [{"match": {"t": "a"}}],
        "filter": [{"range": {"n": {"gte": 1}}}],
    }});
    let not_a = json!({"bool": {
        "must_not": [{"match": {"t": "a"}}],
        "filter": [{"range": {"n": {"gte": 1}}}],
    }});
    for idx in [&built.mem, &built.seg, &built.mixed] {
        assert_eq!(hit_set(idx, &must_a).await.1, 3);
        assert_eq!(hit_set(idx, &not_a).await.0, vec!["d3".to_string()]);
    }
}
