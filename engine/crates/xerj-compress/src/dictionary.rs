//! Dictionary encoding for repeated string values.
//!
//! Ideal for low-cardinality fields like log levels, hostnames, and status
//! codes. Values are mapped to compact u16 IDs, reducing storage from ~20
//! bytes per value to 2 bytes after the dictionary is built.

use std::collections::HashMap;
use xerj_common::XerjError;

/// Result alias for dictionary operations.
pub type Result<T> = std::result::Result<T, XerjError>;

/// Maximum number of distinct values in a single dictionary.
///
/// u16::MAX entries — keeps IDs in 2 bytes.
pub const MAX_DICT_ENTRIES: usize = 65_535;

// ─────────────────────────────────────────────────────────────────────────────
// DictionaryEncoder
// ─────────────────────────────────────────────────────────────────────────────

/// Encodes string values as compact u16 dictionary IDs.
///
/// # Usage
///
/// 1. Call [`build_from_sample`] with a representative set of values.
/// 2. Call [`encode`] for each value to get its ID.
/// 3. Serialize the dictionary alongside the encoded data for decoding.
#[derive(Debug, Clone)]
pub struct DictionaryEncoder {
    /// value → ID
    map: HashMap<String, u16>,
    /// Ordered vocabulary — index is the ID.
    vocab: Vec<String>,
}

impl DictionaryEncoder {
    /// Create an empty encoder. Call [`build_from_sample`] before encoding.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            vocab: Vec::new(),
        }
    }

    /// Build the dictionary from a slice of sample values.
    ///
    /// Values are sorted by frequency (most common first) so that common
    /// values get the smallest IDs, which can compress better downstream.
    pub fn build_from_sample<'a, I>(&mut self, samples: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        // Count frequencies
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for s in samples {
            *freq.entry(s).or_insert(0) += 1;
        }

        // Sort by descending frequency, then alphabetically for stability
        let mut entries: Vec<(&str, usize)> = freq.into_iter().collect();
        entries.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

        self.map.clear();
        self.vocab.clear();

        for (value, _) in entries.into_iter().take(MAX_DICT_ENTRIES) {
            let id = self.vocab.len() as u16;
            self.vocab.push(value.to_owned());
            self.map.insert(value.to_owned(), id);
        }
    }

    /// Encode a value, returning its dictionary ID.
    ///
    /// Returns `None` if the value is not in the dictionary (out-of-vocabulary).
    /// Callers should handle OOV values by storing raw bytes or a fallback ID.
    pub fn encode(&self, value: &str) -> Option<u16> {
        self.map.get(value).copied()
    }

    /// Encode with insert — adds the value if not present, fails if dict is full.
    pub fn encode_or_insert(&mut self, value: &str) -> Result<u16> {
        if let Some(&id) = self.map.get(value) {
            return Ok(id);
        }
        if self.vocab.len() >= MAX_DICT_ENTRIES {
            return Err(XerjError::resource_exhausted(format!(
                "dictionary full ({MAX_DICT_ENTRIES} entries)"
            )));
        }
        let id = self.vocab.len() as u16;
        self.vocab.push(value.to_owned());
        self.map.insert(value.to_owned(), id);
        Ok(id)
    }

    /// Number of entries in the dictionary.
    pub fn len(&self) -> usize {
        self.vocab.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vocab.is_empty()
    }

    /// Return the ordered vocabulary for serialization.
    pub fn vocab(&self) -> &[String] {
        &self.vocab
    }

    /// Serialize the dictionary to bytes (length-prefixed UTF-8 strings).
    ///
    /// Format:
    /// ```text
    /// u32 LE  — entry count
    /// for each entry:
    ///   u16 LE — byte length of string
    ///   bytes  — UTF-8 string data
    /// ```
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let count = self.vocab.len() as u32;
        out.extend_from_slice(&count.to_le_bytes());
        for s in &self.vocab {
            let bytes = s.as_bytes();
            let len = bytes.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(bytes);
        }
        out
    }
}

impl Default for DictionaryEncoder {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DictionaryDecoder
// ─────────────────────────────────────────────────────────────────────────────

/// Decodes u16 dictionary IDs back to their original string values.
#[derive(Debug, Clone)]
pub struct DictionaryDecoder {
    vocab: Vec<String>,
}

impl DictionaryDecoder {
    /// Build from the encoder's vocabulary.
    pub fn from_vocab(vocab: Vec<String>) -> Self {
        Self { vocab }
    }

    /// Build from an encoder directly.
    pub fn from_encoder(encoder: &DictionaryEncoder) -> Self {
        Self {
            vocab: encoder.vocab().to_vec(),
        }
    }

    /// Deserialize from bytes produced by [`DictionaryEncoder::serialize`].
    pub fn deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(XerjError::internal("dictionary data too short"));
        }
        let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let mut pos = 4usize;
        let mut vocab = Vec::with_capacity(count);

        for _ in 0..count {
            if pos + 2 > data.len() {
                return Err(XerjError::internal("dictionary truncated at string length"));
            }
            let slen = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
            pos += 2;
            if pos + slen > data.len() {
                return Err(XerjError::internal("dictionary truncated at string data"));
            }
            let s = std::str::from_utf8(&data[pos..pos + slen])
                .map_err(|e| XerjError::internal(format!("dictionary utf8: {e}")))?
                .to_owned();
            vocab.push(s);
            pos += slen;
        }

        Ok(Self { vocab })
    }

    /// Decode a u16 ID to the original string.
    ///
    /// Returns `None` if the ID is out of range.
    pub fn decode(&self, id: u16) -> Option<&str> {
        self.vocab.get(id as usize).map(String::as_str)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.vocab.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vocab.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let samples = vec!["INFO", "ERROR", "WARN", "INFO", "INFO", "DEBUG", "ERROR"];
        let mut encoder = DictionaryEncoder::new();
        encoder.build_from_sample(samples.iter().copied());

        // INFO should be ID 0 (most frequent)
        assert_eq!(encoder.encode("INFO"), Some(0));
        assert_eq!(encoder.encode("MISSING"), None);

        let decoder = DictionaryDecoder::from_encoder(&encoder);
        assert_eq!(decoder.decode(0), Some("INFO"));
    }

    #[test]
    fn serialize_deserialize() {
        let samples = vec!["alpha", "beta", "gamma", "alpha", "beta", "alpha"];
        let mut encoder = DictionaryEncoder::new();
        encoder.build_from_sample(samples.iter().copied());

        let bytes = encoder.serialize();
        let decoder = DictionaryDecoder::deserialize(&bytes).unwrap();

        assert_eq!(decoder.len(), encoder.len());
        // alpha is most frequent — should be ID 0
        let id = encoder.encode("alpha").unwrap();
        assert_eq!(decoder.decode(id), Some("alpha"));
    }

    #[test]
    fn encode_or_insert_grows_dict() {
        let mut encoder = DictionaryEncoder::new();
        let id0 = encoder.encode_or_insert("first").unwrap();
        let id1 = encoder.encode_or_insert("second").unwrap();
        let id0b = encoder.encode_or_insert("first").unwrap();

        assert_eq!(id0, id0b, "same value must produce same ID");
        assert_ne!(id0, id1);
        assert_eq!(encoder.len(), 2);
    }

    #[test]
    fn log_levels_realistic() {
        let levels = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"];
        let mut encoder = DictionaryEncoder::new();
        encoder.build_from_sample(levels.iter().copied());

        let bytes = encoder.serialize();
        let decoder = DictionaryDecoder::deserialize(&bytes).unwrap();

        for level in &levels {
            let id = encoder.encode(level).expect("level should be in dict");
            assert_eq!(decoder.decode(id), Some(*level));
        }
    }
}
