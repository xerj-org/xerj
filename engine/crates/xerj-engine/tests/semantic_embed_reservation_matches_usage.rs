//! The embedding reservation a semantic `_bulk` group takes before it is
//! parsed and embedded must cover what that step allocates (#2427).
//!
//! On the 1 GB release gate the ingest node's heap profile held 436-536 MB
//! of packed `xerj:f32:` vectors in in-flight `_bulk` requests
//! (`pack_f32_vector` under `apply_semantic_embeddings_batch`), and nothing
//! had reserved them: the bulk path drops its parse reservation before the
//! embedding step. `bulk.rs` now reserves
//! `governor::semantic_embed_reserve_bytes` on each document's embedding
//! text first.
//!
//! A counting allocator records the peak of live heap bytes while a batch is
//! parsed and expanded the way `apply_semantic_embeddings_batch` expands it
//! with the built-in lexical embedder: text copy, chunk copies, windows of at
//! most 64 passages, per-window vectors, per-document vector copies, and the
//! packed pooled and passage vectors plus the passage metadata written into
//! the source. The reservation must be at least that peak for every corpus
//! (code, prose, CJK, tiny documents) and no more than twice it (four times
//! for CJK, whose passages the byte-length bound over-counts).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use xerj_ai::local::local_embed;
use xerj_ai::semantic_lexical::{mean_pool_normalize, semantic_chunker};
use xerj_common::packed_f32::pack_f32_vector;
use xerj_engine::governor::semantic_embed_reserve_bytes;

struct Counting;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
/// The counters are process-wide, so the corpora are measured one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(l.size() as isize, Ordering::Relaxed) + l.size() as isize;
        PEAK.fetch_max(live, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let delta = new as isize - l.size() as isize;
        let live = LIVE.fetch_add(new as isize, Ordering::Relaxed) + new as isize;
        PEAK.fetch_max(live, Ordering::Relaxed);
        let out = unsafe { System.realloc(p, l, new) };
        LIVE.fetch_sub(new as isize - delta, Ordering::Relaxed);
        out
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Deterministic xorshift so the corpora are identical on every run.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, words: &[&'a str]) -> &'a str {
        words[(self.next() % words.len() as u64) as usize]
    }
}

const WINDOW_PASSAGES: usize = 64;

fn code_text(rng: &mut Rng, bytes: usize) -> String {
    const WORDS: &[&str] = &[
        "fn",
        "let",
        "mut",
        "self",
        "impl",
        "return",
        "match",
        "Some(x)",
        "None",
        "=>",
        "{",
        "}",
        "vec.push(item);",
        "if",
        "else",
        "for",
        "in",
        "0..n",
        "Result<()>",
        "map_err(|e|",
        "segment_id",
        "doc_count",
        "u64",
        "String::new()",
        "//",
        "parse",
        "config.limits",
    ];
    let mut s = String::with_capacity(bytes + 32);
    while s.len() < bytes {
        s.push_str(rng.pick(WORDS));
        s.push(if rng.next().is_multiple_of(9) {
            '\n'
        } else {
            ' '
        });
    }
    s
}

fn prose_text(rng: &mut Rng, bytes: usize) -> String {
    const WORDS: &[&str] = &[
        "the",
        "memory",
        "budget",
        "of",
        "a",
        "search",
        "node",
        "is",
        "shared",
        "between",
        "ingest",
        "and",
        "merges",
        "while",
        "queries",
        "wait",
        "for",
        "headroom",
        "under",
        "cap",
        "release",
        "gate",
        "corpus",
        "documents",
        "passages",
        "vectors",
    ];
    let mut s = String::with_capacity(bytes + 32);
    while s.len() < bytes {
        s.push_str(rng.pick(WORDS));
        s.push(if rng.next().is_multiple_of(14) {
            '.'
        } else {
            ' '
        });
    }
    s
}

fn cjk_text(rng: &mut Rng, bytes: usize) -> String {
    const CHARS: &[&str] = &["検", "索", "記", "憶", "語", "文", "書", "索", "引", "量"];
    let mut s = String::with_capacity(bytes + 8);
    while s.len() < bytes {
        s.push_str(rng.pick(CHARS));
    }
    s
}

/// Raw `_bulk` source lines: `{"path": …, "body": <text>}`.
fn corpus(kind: &str, docs: usize, text_bytes: usize) -> Vec<Vec<u8>> {
    let mut rng = Rng(0x2427_u64 ^ text_bytes as u64);
    (0..docs)
        .map(|i| {
            let body = match kind {
                "code" => code_text(&mut rng, text_bytes),
                "prose" => prose_text(&mut rng, text_bytes),
                _ => cjk_text(&mut rng, text_bytes),
            };
            serde_json::to_vec(&json!({"path": format!("src/f{i}.rs"), "body": body})).unwrap()
        })
        .collect()
}

struct Job {
    source_idx: usize,
    _original_text: String,
    texts: Vec<String>,
    offsets: Vec<(usize, usize)>,
}

/// Parse `raw` and expand every `body` the way the ingest embedding step does
/// with the lexical embedder (see the module docs).
fn parse_and_expand(raw: &[Vec<u8>], dims: usize) -> Vec<Value> {
    let mut sources: Vec<Value> = raw
        .iter()
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    let mut jobs = Vec::new();
    for (source_idx, source) in sources.iter().enumerate() {
        let text = source["body"].as_str().unwrap();
        let chunks = semantic_chunker().chunk(text, None);
        let (texts, offsets) = if chunks.len() <= 1 {
            (vec![text.to_string()], vec![(0, text.len())])
        } else {
            (
                chunks.iter().map(|c| c.text.clone()).collect(),
                chunks
                    .iter()
                    .map(|c| (c.start_offset, c.end_offset))
                    .collect(),
            )
        };
        jobs.push(Job {
            source_idx,
            _original_text: text.to_string(),
            texts,
            offsets,
        });
    }
    // Windows of at most WINDOW_PASSAGES passages (at least one job each).
    let mut windows = Vec::new();
    let mut start = 0;
    while start < jobs.len() {
        let mut end = start;
        let mut passages = 0;
        while end < jobs.len()
            && (end == start || passages + jobs[end].texts.len() <= WINDOW_PASSAGES)
        {
            passages += jobs[end].texts.len();
            end += 1;
        }
        let texts: Vec<String> = jobs[start..end]
            .iter()
            .flat_map(|j| j.texts.iter().cloned())
            .collect();
        let vectors: Vec<Vec<f32>> = texts.into_iter().map(|t| local_embed(&t, dims)).collect();
        windows.push((start, end, vectors));
        start = end;
    }
    let mut outputs: Vec<Option<Vec<Vec<f32>>>> = vec![None; jobs.len()];
    for (start, end, vectors) in windows {
        let mut offset = 0;
        for (job_idx, job) in jobs.iter().enumerate().take(end).skip(start) {
            let count = job.texts.len();
            outputs[job_idx] = Some(vectors[offset..offset + count].to_vec());
            offset += count;
        }
    }
    for (job_idx, job) in jobs.iter().enumerate() {
        let chunk_vecs = outputs[job_idx].take().unwrap();
        let pooled = if chunk_vecs.len() == 1 {
            chunk_vecs[0].clone()
        } else {
            mean_pool_normalize(&chunk_vecs)
        };
        let obj = sources[job.source_idx].as_object_mut().unwrap();
        obj.insert(
            "body_vector".into(),
            Value::String(pack_f32_vector(&pooled).unwrap()),
        );
        let offsets: Vec<Value> = job
            .offsets
            .iter()
            .map(|(s, e)| json!([*s as u64, *e as u64]))
            .collect();
        obj.insert(
            "_xerj_passages_body_vector".into(),
            json!({"field": "body", "chunks": offsets}),
        );
        if chunk_vecs.len() > 1 {
            let packed: Vec<Value> = chunk_vecs
                .iter()
                .map(|v| Value::String(pack_f32_vector(v).unwrap()))
                .collect();
            obj.insert("body_vector_chunks".into(), Value::Array(packed));
        }
    }
    sources
}

fn measure(raw: &[Vec<u8>], dims: usize) -> (u64, u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // What `bulk.rs` charges: the reservation on each embedding field's text.
    let reserve: u64 = raw
        .iter()
        .map(|b| {
            let doc: Value = serde_json::from_slice(b).unwrap();
            let text_len = doc["body"].as_str().unwrap().len() as u64;
            semantic_embed_reserve_bytes(text_len, dims as u64)
        })
        .sum();
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let out = parse_and_expand(raw, dims);
    let peak = (PEAK.load(Ordering::Relaxed) - base).max(0) as u64;
    drop(out);
    (reserve, peak)
}

#[test]
fn semantic_embed_reservation_covers_the_expansion_peak() {
    // (corpus, documents, text bytes per document, most reservation/peak
    // allowed). The reservation counts passages from the byte length while
    // the chunker counts characters, so three-byte CJK text cuts about a
    // third of the passages the bound assumes: up to 4x there, 2x elsewhere.
    let cases = [
        ("code", 120, 16 * 1024, 2),
        ("prose", 100, 20 * 1024, 2),
        ("cjk", 60, 12 * 1024, 4),
        ("prose", 4000, 300, 2),
        ("code", 4, 512 * 1024, 2),
    ];
    for dims in [384usize, 1024] {
        for (kind, docs, text_bytes, max_ratio) in cases {
            let raw = corpus(kind, docs, text_bytes);
            let (reserve, peak) = measure(&raw, dims);
            let raw_bytes: usize = raw.iter().map(Vec::len).sum();
            eprintln!(
                "{kind:5} dims={dims:4} docs={docs:5} raw={:7.1} KB peak={:8.1} KB reserve={:8.1} KB ratio={:.2}",
                raw_bytes as f64 / 1024.0,
                peak as f64 / 1024.0,
                reserve as f64 / 1024.0,
                reserve as f64 / peak.max(1) as f64
            );
            assert!(
                reserve >= peak,
                "{kind} dims={dims}: reservation {reserve} below the measured peak {peak}"
            );
            assert!(
                reserve <= peak.saturating_mul(max_ratio),
                "{kind} dims={dims}: reservation {reserve} is more than {max_ratio}x the peak {peak}"
            );
        }
    }
}
