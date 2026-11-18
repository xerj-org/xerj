//! Distance metrics for vector similarity search.
//!
//! These are single-accumulator loops. LLVM does not reassociate f32
//! addition, so `x86-64-v3` does not vectorize them (measured: the
//! assembly stays scalar `mulss`/`addss`, and a v3 build is not faster
//! than v2). The beam-search hot path is the eight-accumulator kernel in
//! `hnsw`, which dispatches to AVX2 at runtime. Widening the reductions
//! here would change bits that callers treat as exact.

use serde::{Deserialize, Serialize};

/// The distance metric to use during index construction and search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    /// Cosine similarity (higher = more similar).
    #[default]
    Cosine,
    /// Squared Euclidean (L2) distance (lower = more similar).
    L2,
    /// Dot product (higher = more similar; assumes normalized vectors).
    DotProduct,
}

// ─────────────────────────────────────────────────────────────────────────────
// Core functions
// ─────────────────────────────────────────────────────────────────────────────

/// Cosine similarity between two vectors.
///
/// Returns a value in `[-1, 1]` where 1 means identical direction.
/// Vectors need not be pre-normalized.
#[inline]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must have the same dimension");

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (&ai, &bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Squared Euclidean (L2) distance between two vectors.
///
/// Avoids the sqrt for efficiency; use the square root if the raw L2
/// distance is required.
#[inline]
pub fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must have the same dimension");

    let mut sum = 0.0f32;
    for (&ai, &bi) in a.iter().zip(b.iter()) {
        let diff = ai - bi;
        sum += diff * diff;
    }
    sum
}

/// Dot product (inner product) of two vectors.
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must have the same dimension");

    let mut sum = 0.0f32;
    for (&ai, &bi) in a.iter().zip(b.iter()) {
        sum += ai * bi;
    }
    sum
}

/// Compute a distance score for two vectors using the specified metric.
///
/// The returned value is always oriented as **lower = more similar** to keep
/// priority queues consistent regardless of metric:
/// - Cosine: returns `1.0 - cosine_similarity`
/// - L2: returns `l2_squared`
/// - DotProduct: returns `-dot_product`
#[inline]
pub fn compute_distance(metric: DistanceMetric, a: &[f32], b: &[f32]) -> f32 {
    match metric {
        DistanceMetric::Cosine => 1.0 - cosine(a, b),
        DistanceMetric::L2 => l2_squared(a, b),
        DistanceMetric::DotProduct => -dot_product(a, b),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn cosine_identical() {
        let v = vec![1.0, 2.0, 3.0];
        assert!(approx_eq(cosine(&v, &v), 1.0));
    }

    #[test]
    fn cosine_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(approx_eq(cosine(&a, &b), 0.0));
    }

    #[test]
    fn cosine_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!(approx_eq(cosine(&a, &b), -1.0));
    }

    #[test]
    fn l2_zero_distance() {
        let v = vec![3.0, 4.0];
        assert!(approx_eq(l2_squared(&v, &v), 0.0));
    }

    #[test]
    fn l2_known_distance() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        // l2_squared = 9 + 16 = 25
        assert!(approx_eq(l2_squared(&a, &b), 25.0));
    }

    #[test]
    fn dot_product_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(approx_eq(dot_product(&a, &b), 0.0));
    }

    #[test]
    fn compute_distance_orientation() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        // All metrics should return >= 0 for orthogonal vectors
        assert!(compute_distance(DistanceMetric::Cosine, &a, &b) >= 0.0);
        assert!(compute_distance(DistanceMetric::L2, &a, &b) >= 0.0);
        // DotProduct of orthogonal unit vectors is 0, so -0.0 >= 0
        assert!(compute_distance(DistanceMetric::DotProduct, &a, &b) <= 0.01);
    }

    #[test]
    fn cosine_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        // Should not panic, returns 0
        assert_eq!(cosine(&a, &b), 0.0);
    }

    /// The committed x86_64 rustflags must not force AVX2. Release builds
    /// pin `x86-64-v2` and pick AVX2 up at runtime in the HNSW kernel.
    /// `RUSTFLAGS` can still override this file for a local native build;
    /// this test locks the file, not the ambient flags.
    #[test]
    fn x86_64_release_baseline_is_v2_not_v3() {
        let config = include_str!("../../../.cargo/config.toml");
        let code = config
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("x86-64-v3"),
            "non-comment rustflags must not force x86-64-v3:\n{code}"
        );
        for target in [
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc",
        ] {
            let section = code
                .split("[target.")
                .find(|part| part.starts_with(target))
                .unwrap_or_else(|| panic!("missing [target.{target}]"));
            assert!(
                section.contains("target-cpu=x86-64-v2"),
                "{target} must pin x86-64-v2, section was:\n{section}"
            );
        }
    }
}
