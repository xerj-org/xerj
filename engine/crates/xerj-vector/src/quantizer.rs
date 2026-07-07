//! Vector quantization for memory-efficient storage.
//!
//! Quantization trades a small accuracy loss for 4–32x memory savings.
//! The HNSW index stores quantized vectors and uses approximate distances
//! during graph traversal, with optional re-ranking using full-precision
//! vectors for the final top-K.

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
}
