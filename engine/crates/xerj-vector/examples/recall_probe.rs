//! Isolated HNSW recall + graph-structure diagnostic (dev tool, not shipped).
//!
//! Builds the same corpus shape as the official kNN bench cell (50k x 128-d
//! uniform random, cosine), then reports recall@10 at several ef values and
//! layer-0 reachability from the entry point. Run with:
//!   cargo run --release -p xerj-vector --example recall_probe [N] [DIM]

use xerj_vector::distance::DistanceMetric;
use xerj_vector::hnsw::{HnswIndex, HnswParams};

// Deterministic RNG (mulberry32, same family as the JS bench).
struct Rng(u32);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B79F5);
        let mut t = self.0 as u64;
        t = (t ^ (t >> 15)).wrapping_mul(1 | t);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t));
        ((t ^ (t >> 14)) & 0xFFFF_FFFF) as f64 / 4294967296.0
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut d, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..a.len() {
        d += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    d / (na.sqrt() * nb.sqrt()).max(1e-12)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(50_000);
    let dim: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(128);
    let k = 10;

    let mut rng = Rng(0xC0FFEE);
    let vecs: Vec<Vec<f32>> = (0..n)
        .map(|_| {
            (0..dim)
                .map(|_| (rng.next_f64() * 2.0 - 1.0) as f32)
                .collect()
        })
        .collect();

    let idx = HnswIndex::new(HnswParams::new(dim, DistanceMetric::Cosine));
    let t0 = std::time::Instant::now();
    for (i, v) in vecs.iter().enumerate() {
        idx.insert(i as u64 + 1, v.clone()).unwrap();
        if (i + 1) % 10_000 == 0 {
            eprintln!("inserted {} in {:?}", i + 1, t0.elapsed());
        }
    }
    eprintln!("build done in {:?}", t0.elapsed());

    // 20 probe queries from a different seed.
    let mut qrng = Rng(0xBEEF);
    let queries: Vec<Vec<f32>> = (0..20)
        .map(|_| {
            (0..dim)
                .map(|_| (qrng.next_f64() * 2.0 - 1.0) as f32)
                .collect()
        })
        .collect();

    // ── Graph-structure diagnostics via the debug stats hook ──
    let (ep, ep_layer, layer_hist, deg0_hist, reach0) = idx.debug_structure();
    eprintln!("entry_point={ep:?} entry_layer={ep_layer}");
    eprintln!("layer histogram (level → count): {layer_hist:?}");
    eprintln!("layer-0 out-degree histogram (bucket → count): {deg0_hist:?}");
    eprintln!("layer-0 BFS reachable from entry: {reach0} / {n}");

    // Pre-compute exact ground truth per query.
    let exact_sets: Vec<(std::collections::HashSet<u64>, u64, f32)> = queries
        .iter()
        .map(|q| {
            let mut scored: Vec<(usize, f32)> = vecs
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(q, v)))
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let set: std::collections::HashSet<u64> =
                scored[..k].iter().map(|&(i, _)| i as u64 + 1).collect();
            (set, scored[0].0 as u64 + 1, 1.0 - scored[0].1) // (top-k set, true-NN id, true-NN distance)
        })
        .collect();

    // Descent diagnostics: where does the hierarchy drop us at layer 0?
    let mut dstats = Vec::new();
    for (q, (_, _nn, nnd)) in queries.iter().zip(&exact_sets) {
        if let Some((_start, d)) = idx.debug_descent(q) {
            dstats.push((d, *nnd));
        }
    }
    let avg_start: f32 = dstats.iter().map(|x| x.0).sum::<f32>() / dstats.len() as f32;
    let avg_nn: f32 = dstats.iter().map(|x| x.1).sum::<f32>() / dstats.len() as f32;
    eprintln!("descent: avg layer-0 start dist={avg_start:.4}  avg true-NN dist={avg_nn:.4}  (random pair ~= 1.0)");

    for ef in [100usize, 200, 400, 800] {
        let (mut recalls, mut oracle_recalls) = (Vec::new(), Vec::new());
        let mut total_ms = 0.0;
        for (q, (exact, nn_id, _)) in queries.iter().zip(&exact_sets) {
            let t = std::time::Instant::now();
            let got = idx.search(q, k, ef).unwrap();
            total_ms += t.elapsed().as_secs_f64() * 1e3;
            let hit = got.iter().filter(|(id, _)| exact.contains(id)).count();
            recalls.push(hit as f32 / k as f32);
            // Oracle start: beam from the TRUE nearest neighbor.
            let got_o = idx.debug_search_from(*nn_id, q, k, ef);
            let hit_o = got_o.iter().filter(|(id, _)| exact.contains(id)).count();
            oracle_recalls.push(hit_o as f32 / k as f32);
        }
        let mean: f32 = recalls.iter().sum::<f32>() / recalls.len() as f32;
        let min = recalls.iter().cloned().fold(f32::MAX, f32::min);
        let omean: f32 = oracle_recalls.iter().sum::<f32>() / oracle_recalls.len() as f32;
        println!(
            "ef={ef:4}  mean_recall={mean:.4}  min_recall={min:.3}  oracle_start_recall={omean:.4}  avg_search_ms={:.3}",
            total_ms / queries.len() as f64
        );
    }
}
