//! Thread-scaling probe for the Candle MiniLM encoder on real documents.
//!
//! Answers "are the cores used?" with CPU-seconds, not a guess: it embeds the
//! same passages with N OS threads calling `embed_blocking` on ONE shared model
//! and reports wall time, process CPU time and their ratio (average cores busy).
//!
//! It is not part of the xerj-ai crate. To run it, copy it next to the crate's
//! own harness and build it like that one (it downloads ~90 MB of MiniLM on
//! first use; `XERJ_NEURAL_LOCAL_DIR`/`HF_HUB_OFFLINE=1` work as usual):
//!
//! ```sh
//! cp benchmarks/beir-neural-baseline/encoder_scaling.rs \
//!    engine/crates/xerj-ai/examples/encoder_scaling.rs
//! cd engine && cargo build --release -p xerj-ai --features neural --example encoder_scaling
//! BIN=target/release/examples/encoder_scaling
//! # args: corpus.jsonl  docs  concurrent_callers  passages_per_call
//! $BIN /path/to/beir/scifact/corpus.jsonl 400 1 64                       # what one _bulk does today
//! RAYON_NUM_THREADS=1 $BIN /path/to/beir/scifact/corpus.jsonl 400 16 16  # data-parallel instead
//! ```
//!
//! `RAYON_NUM_THREADS` is the knob candle's CPU matmul reads
//! (`candle_core::utils::get_num_threads`); unset, it asks for every core.
//! Passages are produced exactly the way ingest produces them for a
//! `semantic_text` field: `TextChunker::new(512, 64)` over "<title>. <text>".
//! Linux only (reads /proc/self/stat).
use std::time::Instant;
use xerj_ai::neural::{NeuralConfig, NeuralEmbedder};
use xerj_ai::TextChunker;

fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = s.rsplit_once(')').unwrap().1.split_whitespace().collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("corpus.jsonl");
    let ndocs: usize = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(150);
    let workers: usize = std::env::args().nth(3).and_then(|v| v.parse().ok()).unwrap_or(1);
    let window: usize = std::env::args().nth(4).and_then(|v| v.parse().ok()).unwrap_or(64);
    let chunker = TextChunker::new(512, 64);
    let mut passages = Vec::new();
    for line in std::fs::read_to_string(&path)?.lines().take(ndocs) {
        let d: serde_json::Value = serde_json::from_str(line)?;
        let text = format!("{}. {}", d["title"].as_str().unwrap_or(""), d["text"].as_str().unwrap_or(""));
        let chunks = chunker.chunk(&text, None);
        if chunks.len() <= 1 { passages.push(text) } else { passages.extend(chunks.into_iter().map(|c| c.text)) }
    }
    let embedder = std::sync::Arc::new(NeuralEmbedder::load(&NeuralConfig::default())?);
    embedder.embed_blocking(&[passages[0].clone()])?;
    let lengths = embedder.token_lengths(&passages)?;
    let mean_tok = lengths.iter().sum::<usize>() as f64 / lengths.len() as f64;
    let windows: Vec<Vec<String>> = passages.chunks(window).map(|w| w.to_vec()).collect();
    let (c0, t0) = (cpu_secs(), Instant::now());
    // `workers` OS threads pull whole 64-passage windows off a shared queue and
    // run them through the SAME model concurrently (embed_blocking takes &self).
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= windows.len() { break; }
                embedder.embed_blocking(&windows[i]).unwrap();
            });
        }
    });
    let (wall, cpu) = (t0.elapsed().as_secs_f64(), cpu_secs() - c0);
    println!(
        "rayon_threads={:<3} workers={:<3} window={window:<3} passages={} mean_tokens={:.0} wall={:.2}s passages/s={:.1} cpu_s={:.1} avg_cores_busy={:.1}",
        std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "default".into()),
        workers, passages.len(), mean_tok, wall, passages.len() as f64 / wall, cpu, cpu / wall
    );
    Ok(())
}
