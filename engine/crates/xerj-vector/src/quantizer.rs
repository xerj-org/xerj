//! Vector quantization for memory-efficient storage.
//!
//! Quantization trades a small accuracy loss for 4–32x memory savings.
//! The HNSW index stores quantized vectors and uses approximate distances
//! during graph traversal, with optional re-ranking using full-precision
//! vectors for the final top-K.

use serde::{Deserialize, Serialize};
use xerj_common::XerjError;

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// Quantized data containers
// ─────────────────────────────────────────────────────────────────────────────

/// The result of quantizing a batch of vectors.
#[derive(Debug, Clone)]
pub struct QuantizedVectors {
    /// Number of vectors.
    pub count: usize,
    /// Vector dimensionality.
    pub dim: usize,
    /// Quantized byte data for all vectors, packed contiguously.
    pub data: QuantizedData,
}

/// The actual quantized data, parameterized by the quantization scheme.
#[derive(Debug, Clone)]
pub enum QuantizedData {
    /// Raw f32 (no quantization).
    F32(Vec<f32>),
    /// Scalar-quantized u8 values (8-bit, 4× compression).
    U8 {
        bytes: Vec<u8>,
        /// Per-dimension min values for dequantization.
        mins: Vec<f32>,
        /// Per-dimension scales for dequantization.
        scales: Vec<f32>,
    },
    /// Scalar-quantized 4-bit nibble values packed two-per-byte (8× compression).
    ///
    /// Two consecutive dimensions are packed into a single byte:
    /// - high nibble (bits 7–4) → even dimension `d`
    /// - low  nibble (bits 3–0) → odd  dimension `d+1`
    ///
    /// For odd-dimensional vectors the last dimension is stored in the high
    /// nibble of the final byte with the low nibble set to 0.
    Nibble {
        /// Packed nibble bytes; `ceil(dim/2)` bytes per vector.
        bytes: Vec<u8>,
        /// Per-dimension min values for dequantization.
        mins: Vec<f32>,
        /// Per-dimension scales for dequantization.
        scales: Vec<f32>,
    },
}

impl QuantizedData {
    /// Get the raw f32 vector for a specific index.
    pub fn get_f32(&self, idx: usize, dim: usize) -> Option<Vec<f32>> {
        match self {
            QuantizedData::F32(data) => {
                let start = idx * dim;
                let end = start + dim;
                if end <= data.len() {
                    Some(data[start..end].to_vec())
                } else {
                    None
                }
            }
            QuantizedData::U8 {
                bytes,
                mins,
                scales,
            } => {
                let start = idx * dim;
                let end = start + dim;
                if end <= bytes.len() {
                    let vec: Vec<f32> = bytes[start..end]
                        .iter()
                        .enumerate()
                        .map(|(d, &b)| mins[d] + (b as f32 / 255.0) * scales[d])
                        .collect();
                    Some(vec)
                } else {
                    None
                }
            }
            QuantizedData::Nibble {
                bytes,
                mins,
                scales,
            } => {
                // Each vector uses `ceil(dim/2)` bytes.
                let bytes_per_vec = dim.div_ceil(2);
                let byte_start = idx * bytes_per_vec;
                let byte_end = byte_start + bytes_per_vec;
                if byte_end > bytes.len() {
                    return None;
                }
                let mut vec = Vec::with_capacity(dim);
                for d in 0..dim {
                    let byte_idx = byte_start + d / 2;
                    let nibble = if d % 2 == 0 {
                        (bytes[byte_idx] >> 4) & 0x0F
                    } else {
                        bytes[byte_idx] & 0x0F
                    };
                    vec.push(mins[d] + (nibble as f32 / 15.0) * scales[d]);
                }
                Some(vec)
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Trait
// ─────────────────────────────────────────────────────────────────────────────

/// A vector quantizer that compresses f32 vectors for space-efficient storage.
pub trait Quantizer: Send + Sync {
    /// Quantize a batch of vectors.
    fn quantize(&self, vectors: &[Vec<f32>]) -> Result<QuantizedVectors>;

    /// Compute an approximate distance from a full-precision query to a
    /// quantized stored vector by index.
    fn distance(&self, query: &[f32], quantized: &QuantizedData, id: usize) -> Result<f32>;

    /// Human-readable name.
    fn name(&self) -> &'static str;
}

// ─────────────────────────────────────────────────────────────────────────────
// NoneQuantizer
// ─────────────────────────────────────────────────────────────────────────────

/// Passthrough — stores raw f32 vectors without compression.
#[derive(Debug, Default, Clone)]
pub struct NoneQuantizer;

impl Quantizer for NoneQuantizer {
    fn quantize(&self, vectors: &[Vec<f32>]) -> Result<QuantizedVectors> {
        let dim = vectors.first().map(|v| v.len()).unwrap_or(0);
        let flat: Vec<f32> = vectors.iter().flat_map(|v| v.iter().copied()).collect();
        Ok(QuantizedVectors {
            count: vectors.len(),
            dim,
            data: QuantizedData::F32(flat),
        })
    }

    fn distance(&self, query: &[f32], quantized: &QuantizedData, id: usize) -> Result<f32> {
        let dim = query.len();
        let stored = quantized
            .get_f32(id, dim)
            .ok_or_else(|| XerjError::internal(format!("NoneQuantizer: id {id} out of range")))?;

        let dist: f32 = query
            .iter()
            .zip(stored.iter())
            .map(|(&a, &b)| {
                let d = a - b;
                d * d
            })
            .sum();
        Ok(dist)
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Scalar8Quantizer
// ─────────────────────────────────────────────────────────────────────────────

/// Scalar quantization: maps each f32 dimension linearly to a u8.
///
/// Each dimension gets its own `min` and `scale` computed from the training
/// set, so the quantization is adaptive to the actual data distribution.
///
/// Memory usage: 1 byte per dimension vs. 4 bytes for f32 — 4x compression.
#[derive(Debug, Default, Clone)]
pub struct Scalar8Quantizer;

impl Scalar8Quantizer {
    /// Encode a single f32 dimension value to u8.
    #[inline]
    fn encode_dim(value: f32, min: f32, scale: f32) -> u8 {
        if scale == 0.0 {
            return 0;
        }
        let normalized = (value - min) / scale;
        (normalized * 255.0).round().clamp(0.0, 255.0) as u8
    }

    /// Decode a u8 back to an approximate f32.
    #[inline]
    fn decode_dim(byte: u8, min: f32, scale: f32) -> f32 {
        min + (byte as f32 / 255.0) * scale
    }
}

impl Quantizer for Scalar8Quantizer {
    fn quantize(&self, vectors: &[Vec<f32>]) -> Result<QuantizedVectors> {
        if vectors.is_empty() {
            return Ok(QuantizedVectors {
                count: 0,
                dim: 0,
                data: QuantizedData::U8 {
                    bytes: vec![],
                    mins: vec![],
                    scales: vec![],
                },
            });
        }

        let dim = vectors[0].len();

        // Compute per-dimension min and max
        let mut mins = vec![f32::MAX; dim];
        let mut maxs = vec![f32::MIN; dim];

        for vec in vectors {
            if vec.len() != dim {
                return Err(XerjError::invalid_mapping(
                    "Scalar8Quantizer: inconsistent vector dimensions",
                ));
            }
            for (d, &v) in vec.iter().enumerate() {
                if v < mins[d] {
                    mins[d] = v;
                }
                if v > maxs[d] {
                    maxs[d] = v;
                }
            }
        }

        let scales: Vec<f32> = mins
            .iter()
            .zip(maxs.iter())
            .map(|(&mn, &mx)| mx - mn)
            .collect();

        // Encode all vectors
        let mut bytes = Vec::with_capacity(vectors.len() * dim);
        for vec in vectors {
            for (d, &v) in vec.iter().enumerate() {
                bytes.push(Self::encode_dim(v, mins[d], scales[d]));
            }
        }

        Ok(QuantizedVectors {
            count: vectors.len(),
            dim,
            data: QuantizedData::U8 {
                bytes,
                mins,
                scales,
            },
        })
    }

    fn distance(&self, query: &[f32], quantized: &QuantizedData, id: usize) -> Result<f32> {
        let (bytes, mins, scales) = match quantized {
            QuantizedData::U8 {
                bytes,
                mins,
                scales,
            } => (bytes, mins, scales),
            _ => {
                return Err(XerjError::internal(
                    "Scalar8Quantizer: wrong QuantizedData variant",
                ))
            }
        };

        let dim = query.len();
        let start = id * dim;
        let end = start + dim;

        if end > bytes.len() {
            return Err(XerjError::internal(format!(
                "Scalar8Quantizer: id {id} out of range"
            )));
        }

        let dist: f32 = query
            .iter()
            .enumerate()
            .map(|(d, &q)| {
                let stored = Self::decode_dim(bytes[start + d], mins[d], scales[d]);
                let diff = q - stored;
                diff * diff
            })
            .sum();

        Ok(dist)
    }

    fn name(&self) -> &'static str {
        "scalar8"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sq8Params — serializable serving-path scalar8 codec
// ─────────────────────────────────────────────────────────────────────────────

/// Serializable per-dimension scalar-quantization (SQ8) parameters.
///
/// This is the serving-path counterpart to [`Scalar8Quantizer`]: instead of
/// packing a whole batch into one blob, it exposes a fitted codec that the
/// engine keeps *per dense_vector field*. The engine stores one shared
/// `Sq8Params` (fitted from the first ~1000 ingested vectors for the field)
/// plus a `doc_id -> Vec<u8>` code map, so the brute-force kNN scan reads
/// **1 byte/dim** instead of the 4 bytes/dim an f32 vector costs — a ~4×
/// reduction on the quantized field's vector working set.
///
/// `mins[d]`/`scales[d]` are the per-dimension minimum and range (`max-min`)
/// observed in the fitting sample. `encode` maps `v[d]` linearly to a u8 in
/// `[0,255]`; `decode` reverses it. A zero scale (constant dimension) encodes
/// to 0 and decodes back to `min`. The encode/decode math is bit-for-bit
/// identical to [`Scalar8Quantizer`]'s private `encode_dim`/`decode_dim`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sq8Params {
    /// Per-dimension minimum value.
    pub mins: Vec<f32>,
    /// Per-dimension scale (`max - min`).
    pub scales: Vec<f32>,
}

impl Sq8Params {
    /// Fit per-dimension min/max over `sample`.
    ///
    /// `dim` is the required vector dimensionality; sample vectors of a
    /// different length are skipped. If no usable vector is present the
    /// params degenerate to zero scale (every code becomes 0, decoding to 0).
    pub fn fit(sample: &[Vec<f32>], dim: usize) -> Sq8Params {
        let mut mins = vec![f32::MAX; dim];
        let mut maxs = vec![f32::MIN; dim];
        let mut seen = false;
        for v in sample {
            if v.len() != dim {
                continue;
            }
            seen = true;
            for (d, &x) in v.iter().enumerate() {
                if x < mins[d] {
                    mins[d] = x;
                }
                if x > maxs[d] {
                    maxs[d] = x;
                }
            }
        }
        if !seen {
            mins = vec![0.0; dim];
            maxs = vec![0.0; dim];
        }
        let scales = mins
            .iter()
            .zip(maxs.iter())
            .map(|(&mn, &mx)| mx - mn)
            .collect();
        Sq8Params { mins, scales }
    }

    /// Vector dimensionality this codec was fitted for.
    #[inline]
    pub fn dim(&self) -> usize {
        self.mins.len()
    }

    /// Encode a full-precision vector to one u8 per dimension.
    #[inline]
    pub fn encode(&self, v: &[f32]) -> Vec<u8> {
        v.iter()
            .enumerate()
            .map(|(d, &x)| {
                let min = self.mins.get(d).copied().unwrap_or(0.0);
                let scale = self.scales.get(d).copied().unwrap_or(0.0);
                if scale == 0.0 {
                    return 0u8;
                }
                let normalized = (x - min) / scale;
                (normalized * 255.0).round().clamp(0.0, 255.0) as u8
            })
            .collect()
    }

    /// Decode a code slice back to approximate f32 values (allocating).
    #[inline]
    pub fn decode(&self, codes: &[u8]) -> Vec<f32> {
        let mut out = vec![0.0f32; codes.len()];
        self.decode_into(codes, &mut out);
        out
    }

    /// Decode into a caller-provided buffer, avoiding per-score allocation on
    /// the hot kNN scan. `out` must be at least `codes.len()` long.
    #[inline]
    pub fn decode_into(&self, codes: &[u8], out: &mut [f32]) {
        for (d, &b) in codes.iter().enumerate() {
            let min = self.mins.get(d).copied().unwrap_or(0.0);
            let scale = self.scales.get(d).copied().unwrap_or(0.0);
            out[d] = min + (b as f32 / 255.0) * scale;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Scalar4Quantizer
// ─────────────────────────────────────────────────────────────────────────────

/// 4-bit scalar quantization: packs two dimensions into each byte (nibble encoding).
///
/// Each dimension is mapped to a value in `[0, 15]` using per-dimension min/max
/// statistics.  Two adjacent dimensions are stored in the high and low nibbles
/// of a single byte, giving **8× memory compression** vs. full-precision f32.
///
/// This is the recommended quantization scheme when vector count exceeds
/// `hnsw_offload_threshold` and memory must be conserved (see config).
/// Accuracy loss is typically 2–4% vs. 1–2% for [`Scalar8Quantizer`].
///
/// # Memory usage
/// - f32:     4 bytes/dim
/// - Scalar8: 1 byte/dim  (4× compression)
/// - Scalar4: 0.5 byte/dim (8× compression)
#[derive(Debug, Default, Clone)]
pub struct Scalar4Quantizer;

impl Scalar4Quantizer {
    /// Encode a single f32 dimension value to a 4-bit nibble `[0, 15]`.
    #[inline]
    fn encode_dim(value: f32, min: f32, scale: f32) -> u8 {
        if scale == 0.0 {
            return 0;
        }
        let normalized = (value - min) / scale;
        (normalized * 15.0).round().clamp(0.0, 15.0) as u8
    }

    /// Decode a nibble `[0, 15]` back to an approximate f32.
    #[inline]
    fn decode_dim(nibble: u8, min: f32, scale: f32) -> f32 {
        min + (nibble as f32 / 15.0) * scale
    }
}

impl Quantizer for Scalar4Quantizer {
    fn quantize(&self, vectors: &[Vec<f32>]) -> Result<QuantizedVectors> {
        if vectors.is_empty() {
            return Ok(QuantizedVectors {
                count: 0,
                dim: 0,
                data: QuantizedData::Nibble {
                    bytes: vec![],
                    mins: vec![],
                    scales: vec![],
                },
            });
        }

        let dim = vectors[0].len();

        // Compute per-dimension min and max.
        let mut mins = vec![f32::MAX; dim];
        let mut maxs = vec![f32::MIN; dim];

        for vec in vectors {
            if vec.len() != dim {
                return Err(XerjError::invalid_mapping(
                    "Scalar4Quantizer: inconsistent vector dimensions",
                ));
            }
            for (d, &v) in vec.iter().enumerate() {
                if v < mins[d] {
                    mins[d] = v;
                }
                if v > maxs[d] {
                    maxs[d] = v;
                }
            }
        }

        let scales: Vec<f32> = mins
            .iter()
            .zip(maxs.iter())
            .map(|(&mn, &mx)| mx - mn)
            .collect();

        // Each vector takes `ceil(dim/2)` bytes.
        let bytes_per_vec = dim.div_ceil(2);
        let mut bytes = Vec::with_capacity(vectors.len() * bytes_per_vec);

        for vec in vectors {
            let mut d = 0;
            while d < dim {
                let hi = Self::encode_dim(vec[d], mins[d], scales[d]);
                let lo = if d + 1 < dim {
                    Self::encode_dim(vec[d + 1], mins[d + 1], scales[d + 1])
                } else {
                    0u8
                };
                bytes.push((hi << 4) | (lo & 0x0F));
                d += 2;
            }
        }

        Ok(QuantizedVectors {
            count: vectors.len(),
            dim,
            data: QuantizedData::Nibble {
                bytes,
                mins,
                scales,
            },
        })
    }

    fn distance(&self, query: &[f32], quantized: &QuantizedData, id: usize) -> Result<f32> {
        let (bytes, mins, scales) = match quantized {
            QuantizedData::Nibble {
                bytes,
                mins,
                scales,
            } => (bytes, mins, scales),
            _ => {
                return Err(XerjError::internal(
                    "Scalar4Quantizer: wrong QuantizedData variant",
                ))
            }
        };

        let dim = query.len();
        let bytes_per_vec = dim.div_ceil(2);
        let byte_start = id * bytes_per_vec;
        let byte_end = byte_start + bytes_per_vec;

        if byte_end > bytes.len() {
            return Err(XerjError::internal(format!(
                "Scalar4Quantizer: id {id} out of range"
            )));
        }

        let dist: f32 = (0..dim)
            .map(|d| {
                let byte_idx = byte_start + d / 2;
                let nibble = if d % 2 == 0 {
                    (bytes[byte_idx] >> 4) & 0x0F
                } else {
                    bytes[byte_idx] & 0x0F
                };
                let stored = Self::decode_dim(nibble, mins[d], scales[d]);
                let diff = query[d] - stored;
                diff * diff
            })
            .sum();

        Ok(dist)
    }

    fn name(&self) -> &'static str {
        "scalar4"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn none_quantizer_roundtrip() {
        let q = NoneQuantizer;
        let vecs = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let qv = q.quantize(&vecs).unwrap();

        let v0 = qv.data.get_f32(0, 3).unwrap();
        assert_eq!(v0, vec![1.0, 2.0, 3.0]);

        let v1 = qv.data.get_f32(1, 3).unwrap();
        assert_eq!(v1, vec![4.0, 5.0, 6.0]);
    }

    #[test]
    fn scalar8_roundtrip_approx() {
        let q = Scalar8Quantizer;
        let vecs = vec![vec![0.0, 0.5, 1.0], vec![0.2, 0.7, 0.9]];
        let qv = q.quantize(&vecs).unwrap();

        // Dequantized values should be close to originals (within ~1% error)
        let recovered = qv.data.get_f32(0, 3).unwrap();
        for (orig, rec) in vecs[0].iter().zip(recovered.iter()) {
            assert!(approx_eq(*orig, *rec, 0.02), "expected ~{orig}, got {rec}");
        }
    }

    #[test]
    fn scalar8_empty_input() {
        let q = Scalar8Quantizer;
        let qv = q.quantize(&[]).unwrap();
        assert_eq!(qv.count, 0);
    }

    #[test]
    fn scalar8_distance_ordering() {
        let q = Scalar8Quantizer;
        let vecs = vec![
            vec![1.0, 0.0, 0.0], // close to query
            vec![0.0, 0.0, 1.0], // far from query
        ];
        let qv = q.quantize(&vecs).unwrap();

        let query = vec![1.0, 0.0, 0.0];
        let d0 = q.distance(&query, &qv.data, 0).unwrap();
        let d1 = q.distance(&query, &qv.data, 1).unwrap();
        assert!(d0 < d1, "closer vector should have smaller distance");
    }

    #[test]
    fn scalar4_roundtrip_approx() {
        let q = Scalar4Quantizer;
        let vecs = vec![vec![0.0, 0.5, 1.0], vec![0.2, 0.7, 0.9]];
        let qv = q.quantize(&vecs).unwrap();
        // 4-bit gives 16 levels over the range — tolerance of ~7%
        let recovered = qv.data.get_f32(0, 3).unwrap();
        for (orig, rec) in vecs[0].iter().zip(recovered.iter()) {
            assert!((orig - rec).abs() < 0.08, "expected ~{orig}, got {rec}");
        }
    }

    #[test]
    fn scalar4_empty_input() {
        let q = Scalar4Quantizer;
        let qv = q.quantize(&[]).unwrap();
        assert_eq!(qv.count, 0);
    }

    #[test]
    fn scalar4_distance_ordering() {
        let q = Scalar4Quantizer;
        let vecs = vec![
            vec![1.0, 0.0, 0.0], // close to query
            vec![0.0, 0.0, 1.0], // far from query
        ];
        let qv = q.quantize(&vecs).unwrap();

        let query = vec![1.0, 0.0, 0.0];
        let d0 = q.distance(&query, &qv.data, 0).unwrap();
        let d1 = q.distance(&query, &qv.data, 1).unwrap();
        assert!(
            d0 < d1,
            "closer vector should have smaller distance (4-bit)"
        );
    }

    #[test]
    fn scalar4_memory_is_half_scalar8() {
        // For N vectors of dim D:
        //   scalar8 → N * D bytes
        //   scalar4 → N * ceil(D/2) bytes  ≈ N * D/2 bytes
        let q4 = Scalar4Quantizer;
        let q8 = Scalar8Quantizer;
        let vecs: Vec<Vec<f32>> = (0..10).map(|i| vec![i as f32; 64]).collect();

        let qv4 = q4.quantize(&vecs).unwrap();
        let qv8 = q8.quantize(&vecs).unwrap();

        let bytes4 = match &qv4.data {
            QuantizedData::Nibble { bytes, .. } => bytes.len(),
            _ => panic!(),
        };
        let bytes8 = match &qv8.data {
            QuantizedData::U8 { bytes, .. } => bytes.len(),
            _ => panic!(),
        };

        // scalar4 should use ≤ 55% of scalar8 byte count (target is 50%)
        assert!(
            bytes4 <= bytes8 * 55 / 100,
            "scalar4 ({bytes4} B) should be ~50% of scalar8 ({bytes8} B)"
        );
    }

    // ── Sq8Params (serving-path codec) ────────────────────────────────────────

    /// Tiny deterministic xorshift RNG so the recall/memory tests are
    /// reproducible without pulling in the `rand` crate.
    struct XorShift(u64);
    impl XorShift {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        /// Standard-normal-ish sample via sum of 4 uniforms (CLT), in ~[-2,2].
        fn next_gauss(&mut self) -> f32 {
            let mut s = 0.0f32;
            for _ in 0..4 {
                s += (self.next_u64() as f64 / u64::MAX as f64) as f32;
            }
            s - 2.0 // mean 0
        }
    }

    fn l2_normalize(v: &mut [f32]) {
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na > 0.0 && nb > 0.0 {
            dot / (na * nb)
        } else {
            0.0
        }
    }

    /// (b) MEMORY: the SQ8 code store is ~4× smaller than the f32 equivalent.
    #[test]
    fn sq8_store_is_quarter_of_f32() {
        let n = 2000usize;
        let dim = 128usize;
        let mut rng = XorShift(0x1234_5678_9abc_def0);
        let vecs: Vec<Vec<f32>> = (0..n)
            .map(|_| {
                let mut v: Vec<f32> = (0..dim).map(|_| rng.next_gauss()).collect();
                l2_normalize(&mut v);
                v
            })
            .collect();
        let params = Sq8Params::fit(&vecs, dim);
        let code_bytes: usize = vecs.iter().map(|v| params.encode(v).len()).sum();
        // params overhead is 2 * dim * 4 bytes, shared across all N vectors.
        let params_bytes = (params.mins.len() + params.scales.len()) * std::mem::size_of::<f32>();
        let f32_bytes = n * dim * std::mem::size_of::<f32>();
        let ratio = f32_bytes as f64 / (code_bytes + params_bytes) as f64;
        println!(
            "SQ8 memory: f32={f32_bytes}B codes={code_bytes}B params={params_bytes}B ratio={ratio:.3}x"
        );
        assert!(
            ratio >= 3.5,
            "SQ8 store should be >=3.5x smaller than f32, got {ratio:.3}x"
        );
    }

    /// (c) RECALL: scalar8 serving-path scoring keeps recall@10 >= 0.90 vs the
    /// exact (none) path over >=2000 vectors / >=100 queries at dim>=64.
    #[test]
    fn sq8_recall_at_10_above_090() {
        let n = 2000usize;
        let dim = 128usize;
        let k = 10usize;
        let queries = 100usize;
        let mut rng = XorShift(0xdead_beef_cafe_0001);

        // Corpus, L2-normalized (cosine convention, as the serving path does).
        let corpus: Vec<Vec<f32>> = (0..n)
            .map(|_| {
                let mut v: Vec<f32> = (0..dim).map(|_| rng.next_gauss()).collect();
                l2_normalize(&mut v);
                v
            })
            .collect();

        // Fit params from the first 1000 vectors (mirrors the engine's sample).
        let sample: Vec<Vec<f32>> = corpus.iter().take(1000).cloned().collect();
        let params = Sq8Params::fit(&sample, dim);
        let codes: Vec<Vec<u8>> = corpus.iter().map(|v| params.encode(v)).collect();

        let mut recall_sum = 0.0f64;
        let mut decoded = vec![0.0f32; dim];
        for _ in 0..queries {
            let q: Vec<f32> = (0..dim).map(|_| rng.next_gauss()).collect();

            // Exact (ground truth) top-k by cosine on the raw f32 corpus.
            let mut exact: Vec<(usize, f32)> = corpus
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine_sim(&q, v)))
                .collect();
            exact.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let truth: std::collections::HashSet<usize> =
                exact.iter().take(k).map(|(i, _)| *i).collect();

            // Approximate top-k by cosine on the DECODED SQ8 codes.
            let mut approx: Vec<(usize, f32)> = codes
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    params.decode_into(c, &mut decoded);
                    (i, cosine_sim(&q, &decoded))
                })
                .collect();
            approx.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            let hit = approx
                .iter()
                .take(k)
                .filter(|(i, _)| truth.contains(i))
                .count();
            recall_sum += hit as f64 / k as f64;
        }
        let mean_recall = recall_sum / queries as f64;
        println!("SQ8 recall@{k}: {mean_recall:.4} over {queries} queries / {n} vectors dim={dim}");
        assert!(
            mean_recall >= 0.90,
            "SQ8 recall@{k} should be >=0.90, got {mean_recall:.4}"
        );
    }
}
