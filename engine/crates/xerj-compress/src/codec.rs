//! Compression codecs: LZ4, Zstd, and passthrough.

use xerj_common::XerjError;

/// Result alias for codec operations.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// Trait
// ─────────────────────────────────────────────────────────────────────────────

/// A stateless, thread-safe compression/decompression codec.
pub trait Codec: Send + Sync {
    /// Compress `data` and return the compressed bytes.
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>>;

    /// Decompress `data` into a buffer of exactly `output_len` bytes.
    ///
    /// `output_len` must match the original uncompressed length.
    fn decompress(&self, data: &[u8], output_len: usize) -> Result<Vec<u8>>;

    /// Human-readable name of this codec.
    fn name(&self) -> &'static str;
}

// ─────────────────────────────────────────────────────────────────────────────
// Compression level
// ─────────────────────────────────────────────────────────────────────────────

/// Selects the compression algorithm and effort level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionLevel {
    /// No compression — raw bytes pass through unchanged.
    None,
    /// LZ4: very fast compression with moderate ratio.
    #[default]
    Fast,
    /// Zstd level 3: balanced speed vs. ratio.
    Balanced,
    /// Zstd level 19: maximum compression ratio.
    Best,
}

impl CompressionLevel {
    /// Parse from a string (case-insensitive).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "fast" => Some(Self::Fast),
            "balanced" => Some(Self::Balanced),
            "best" => Some(Self::Best),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// NoneCodec — passthrough
// ─────────────────────────────────────────────────────────────────────────────

/// Passthrough codec — no compression applied.
#[derive(Debug, Default, Clone)]
pub struct NoneCodec;

impl Codec for NoneCodec {
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn decompress(&self, data: &[u8], output_len: usize) -> Result<Vec<u8>> {
        if data.len() != output_len {
            return Err(XerjError::internal(format!(
                "NoneCodec: expected {output_len} bytes, got {}",
                data.len()
            )));
        }
        Ok(data.to_vec())
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lz4Codec
// ─────────────────────────────────────────────────────────────────────────────

/// LZ4-based codec wrapping [`lz4_flex`].
#[derive(Debug, Default, Clone)]
pub struct Lz4Codec;

impl Codec for Lz4Codec {
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        // lz4_flex::compress_prepend_size returns Vec<u8> (not a Result)
        Ok(lz4_flex::compress_prepend_size(data))
    }

    fn decompress(&self, data: &[u8], _output_len: usize) -> Result<Vec<u8>> {
        lz4_flex::decompress_size_prepended(data)
            .map_err(|e| XerjError::internal(format!("LZ4 decompress: {e}")))
    }

    fn name(&self) -> &'static str {
        "lz4"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ZstdCodec
// ─────────────────────────────────────────────────────────────────────────────

/// Zstd-based codec wrapping the [`zstd`] crate.
#[derive(Debug, Clone)]
pub struct ZstdCodec {
    /// Zstd compression level (1–22; defaults to 3 for balanced, 19 for best).
    level: i32,
}

impl ZstdCodec {
    pub fn new(level: i32) -> Self {
        Self { level }
    }

    /// Balanced Zstd (level 3).
    pub fn balanced() -> Self {
        Self::new(3)
    }

    /// Maximum Zstd compression (level 19).
    pub fn best() -> Self {
        Self::new(19)
    }
}

impl Default for ZstdCodec {
    fn default() -> Self {
        Self::balanced()
    }
}

impl Codec for ZstdCodec {
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        zstd::bulk::compress(data, self.level)
            .map_err(|e| XerjError::internal(format!("Zstd compress: {e}")))
    }

    fn decompress(&self, data: &[u8], output_len: usize) -> Result<Vec<u8>> {
        zstd::bulk::decompress(data, output_len)
            .map_err(|e| XerjError::internal(format!("Zstd decompress: {e}")))
    }

    fn name(&self) -> &'static str {
        "zstd"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Factory
// ─────────────────────────────────────────────────────────────────────────────

/// Return a boxed codec appropriate for the given compression level.
pub fn get_codec(level: CompressionLevel) -> Box<dyn Codec> {
    match level {
        CompressionLevel::None => Box::new(NoneCodec),
        CompressionLevel::Fast => Box::new(Lz4Codec),
        CompressionLevel::Balanced => Box::new(ZstdCodec::balanced()),
        CompressionLevel::Best => Box::new(ZstdCodec::best()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &[u8] = b"hello world this is a test payload that should compress well \
        because it has repeated patterns repeated patterns repeated patterns";

    #[test]
    fn none_roundtrip() {
        let codec = NoneCodec;
        let compressed = codec.compress(PAYLOAD).unwrap();
        let decompressed = codec.decompress(&compressed, PAYLOAD.len()).unwrap();
        assert_eq!(decompressed, PAYLOAD);
    }

    #[test]
    fn lz4_roundtrip() {
        let codec = Lz4Codec;
        let compressed = codec.compress(PAYLOAD).unwrap();
        let decompressed = codec.decompress(&compressed, PAYLOAD.len()).unwrap();
        assert_eq!(decompressed, PAYLOAD);
    }

    #[test]
    fn lz4_reduces_size() {
        let codec = Lz4Codec;
        let compressed = codec.compress(PAYLOAD).unwrap();
        assert!(compressed.len() < PAYLOAD.len(), "LZ4 should compress repetitive data");
    }

    #[test]
    fn zstd_balanced_roundtrip() {
        let codec = ZstdCodec::balanced();
        let compressed = codec.compress(PAYLOAD).unwrap();
        let decompressed = codec.decompress(&compressed, PAYLOAD.len()).unwrap();
        assert_eq!(decompressed, PAYLOAD);
    }

    #[test]
    fn zstd_best_roundtrip() {
        let codec = ZstdCodec::best();
        let compressed = codec.compress(PAYLOAD).unwrap();
        let decompressed = codec.decompress(&compressed, PAYLOAD.len()).unwrap();
        assert_eq!(decompressed, PAYLOAD);
    }

    #[test]
    fn get_codec_factory() {
        for level in [
            CompressionLevel::None,
            CompressionLevel::Fast,
            CompressionLevel::Balanced,
            CompressionLevel::Best,
        ] {
            let codec = get_codec(level);
            let compressed = codec.compress(PAYLOAD).unwrap();
            let decompressed = codec.decompress(&compressed, PAYLOAD.len()).unwrap();
            assert_eq!(decompressed, PAYLOAD, "roundtrip failed for {level:?}");
        }
    }
}
