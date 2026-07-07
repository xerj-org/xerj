//! Deterministic allocation-count harness for the memtable insert hot path.
//!
//! Wraps the global allocator with an atomic counter and measures the number
//! of heap allocations performed *inside the `insert_analyzed` loop only*
//! (analysis is done up front, outside the measured region).  This isolates
//! the exact under-shard-write-lock work the Arc<str> doc-id interning change
//! targets.  Fully deterministic and single-threaded — immune to the
//! concurrent-agent CPU contamination that skews wall-clock numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;
use xerj_common::types::Schema;
use xerj_engine::memtable::{analyze_doc, FtsMemtable};
use xerj_fts::analyzer::AnalyzerRegistry;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let corpus = "/home/claude/ai/xerj/demo/data/extras/chat-events.ndjson";
    let raw: Vec<String> = std::fs::read_to_string(corpus)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect();
    let n_docs: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(40_000);

    // Build inputs OUTSIDE the measured region: parse + analyze every doc,
    // and synthesize turbo-style auto ids (20-char, like the bulk path).
    let reg = AnalyzerRegistry::default();
    let analyzer = reg.get_analyzer("standard").unwrap();
    let schema = Schema::default();
    type DocInput = (
        String,
        Arc<Value>,
        Vec<(String, Vec<xerj_fts::analyzer::Token>)>,
    );
    let mut inputs: Vec<DocInput> = Vec::with_capacity(n_docs);
    for i in 0..n_docs {
        let line = &raw[i % raw.len()];
        let v: Value = serde_json::from_str(line).unwrap();
        let analyzed = analyze_doc(&v, &schema, &analyzer);
        // Auto-id shape: 20 base62-ish chars, unique per doc.
        let id = format!("{:020x}", 0xdead_0000_0000u64 + i as u64);
        inputs.push((id, Arc::new(v), analyzed));
    }

    let mut mem = FtsMemtable::new();

    // ── measured region: the insert_analyzed loop only ──
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let b0 = BYTES.load(Ordering::Relaxed);
    for (i, (id, src, analyzed)) in inputs.iter().enumerate() {
        mem.insert_analyzed(i as u64, id.clone(), Arc::clone(src), analyzed, 800);
    }
    let a1 = ALLOCS.load(Ordering::Relaxed);
    let b1 = BYTES.load(Ordering::Relaxed);

    let allocs = a1 - a0;
    let bytes = b1 - b0;
    // Subtract the unavoidable `id.clone()` (one String per doc, done by the
    // caller inside the loop) so the number reflects work INSIDE the memtable.
    println!("docs={}", n_docs);
    println!("total_allocs_in_insert_loop={}", allocs);
    println!("allocs_per_doc={:.3}", allocs as f64 / n_docs as f64);
    println!("bytes_per_doc={:.1}", bytes as f64 / n_docs as f64);
    // keep mem alive so drops don't happen inside the measured window
    std::hint::black_box(&mem);
}
