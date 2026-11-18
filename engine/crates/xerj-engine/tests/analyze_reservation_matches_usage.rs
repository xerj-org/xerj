//! The analyze reservation a raw `_bulk` batch takes before it is parsed
//! must cover what parsing and analyzing that batch allocates, without
//! charging several times that (#2427).
//!
//! A counting allocator records the peak of live heap bytes while a batch is
//! parsed into `serde_json::Value`s and analyzed with the standard analyzer,
//! the two steps the reservation is held across in the raw bulk path. The
//! reservation must be at least that peak for every corpus, including the
//! dense one-token-per-two-bytes input that set the old 28-bytes-per-byte
//! factor, and at most twice it for ordinary prose and code.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use serde_json::Value;
use xerj_common::types::Schema;
use xerj_engine::governor::analyze_reserve_bytes_for_doc;
use xerj_engine::memtable::analyze_doc;
use xerj_fts::analyzer::AnalyzerRegistry;

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
        // A growing realloc may hold both blocks at once; count the new one
        // before the old one is released.
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
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn word(rng: &mut Rng, min: u64, max: u64) -> String {
    let len = min + rng.below(max - min + 1);
    (0..len)
        .map(|_| (b'a' + rng.below(26) as u8) as char)
        .collect()
}

fn prose_doc(rng: &mut Rng) -> String {
    let words: Vec<String> = (0..120).map(|_| word(rng, 2, 10)).collect();
    serde_json::json!({
        "title": format!("{} {}", word(rng, 4, 9), word(rng, 4, 9)),
        "body": words.join(" "),
        "author": word(rng, 5, 8),
        "views": rng.below(100_000),
    })
    .to_string()
}

fn code_doc(rng: &mut Rng) -> String {
    let mut src = String::new();
    for _ in 0..40 {
        let a = word(rng, 3, 12);
        let b = word(rng, 3, 12);
        src.push_str(&format!(
            "    let {a} = self.{b}({}, &{a}_{b})?;\n",
            rng.below(1000)
        ));
    }
    serde_json::json!({
        "path": format!("src/{}/{}.rs", word(rng, 3, 8), word(rng, 3, 8)),
        "language": "rust",
        "content": src,
        "line": rng.below(5000),
    })
    .to_string()
}

fn dense_doc(_rng: &mut Rng) -> String {
    serde_json::json!({ "body": "a ".repeat(2048) }).to_string()
}

fn cjk_doc(rng: &mut Rng) -> String {
    let text: String = (0..800)
        .map(|_| char::from_u32(0x4E00 + rng.below(0x5000) as u32).unwrap())
        .collect();
    serde_json::json!({ "body": text }).to_string()
}

fn packed_vector_doc(rng: &mut Rng) -> String {
    let hex: String = (0..384)
        .map(|_| format!("{:08x}", rng.next() as u32))
        .collect();
    serde_json::json!({
        "body": format!("{} {}", word(rng, 4, 9), word(rng, 4, 9)),
        "body_vector": format!("xerj:f32:{hex}"),
    })
    .to_string()
}

/// Peak live heap bytes above the starting point while `docs` are parsed and
/// analyzed, with every parsed value and token list held until the end, the
/// way the raw bulk path holds them.
fn measured_peak(docs: &[String]) -> u64 {
    let registry = AnalyzerRegistry::default();
    let analyzer = registry.get_analyzer("standard").unwrap();
    let schema = Schema::default();
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let parsed: Vec<Value> = docs
        .iter()
        .map(|d| serde_json::from_slice(d.as_bytes()).unwrap())
        .collect();
    let analyzed: Vec<_> = parsed
        .iter()
        .map(|v| analyze_doc(v, &schema, &analyzer))
        .collect();
    let peak = (PEAK.load(Ordering::Relaxed) - base).max(0) as u64;
    drop(analyzed);
    drop(parsed);
    peak
}

fn check(name: &str, make: fn(&mut Rng) -> String, max_ratio: Option<f64>) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut rng = Rng(0x2427_2427);
    let docs: Vec<String> = (0..400).map(|_| make(&mut rng)).collect();
    let input: u64 = docs.iter().map(|d| d.len() as u64).sum();
    let reserve: u64 = docs
        .iter()
        .map(|d| analyze_reserve_bytes_for_doc(d.as_bytes()))
        .sum();
    let peak = measured_peak(&docs);
    let ratio = reserve as f64 / peak as f64;
    println!(
        "{name}: input {input} B, measured peak {peak} B ({:.1}/B), reserve {reserve} B ({:.1}/B), reserve/peak {ratio:.2}",
        peak as f64 / input as f64,
        reserve as f64 / input as f64,
    );
    assert!(
        reserve >= peak,
        "{name}: reservation {reserve} B is below the {peak} B the batch allocated"
    );
    if let Some(max_ratio) = max_ratio {
        assert!(
            ratio <= max_ratio,
            "{name}: reservation {reserve} B is {ratio:.2}x the {peak} B the batch allocated (max {max_ratio}x)"
        );
    }
}

#[test]
fn reservation_covers_but_does_not_overcharge_prose_2427() {
    check("prose", prose_doc, Some(2.0));
}

#[test]
fn reservation_covers_but_does_not_overcharge_code_2427() {
    check("code", code_doc, Some(2.0));
}

#[test]
fn reservation_covers_dense_single_letter_tokens_2427() {
    check("dense", dense_doc, None);
}

#[test]
fn reservation_covers_cjk_text_2427() {
    check("cjk", cjk_doc, None);
}

#[test]
fn reservation_covers_packed_vectors_2427() {
    check("packed_vector", packed_vector_doc, Some(2.0));
}
