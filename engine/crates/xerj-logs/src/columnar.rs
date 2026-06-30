//! Column-oriented storage for log data.
//!
//! Each field of a log record is stored in its own typed column. This allows:
//! - Selective decompression (only load the columns you need)
//! - Type-specific encoding (delta-of-delta for timestamps, dictionary for strings)
//! - SIMD-friendly tight packing of numeric arrays

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use xerj_common::XerjError;
use xerj_compress::{
    codec::{get_codec, CompressionLevel},
    dictionary::{DictionaryDecoder, DictionaryEncoder},
};

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// ColumnType
// ─────────────────────────────────────────────────────────────────────────────

/// The data type of a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    /// Unix epoch in nanoseconds, stored with delta-of-delta encoding.
    Timestamp,
    /// 64-bit signed integer.
    I64,
    /// 64-bit float.
    F64,
    /// UTF-8 string — dictionary encoded if cardinality is low.
    String,
    /// Boolean.
    Bool,
    /// Raw binary.
    Binary,
}

// ─────────────────────────────────────────────────────────────────────────────
// Column value
// ─────────────────────────────────────────────────────────────────────────────

/// A typed value that can be stored in a column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColumnValue {
    Timestamp(i64),
    I64(i64),
    F64(f64),
    String(String),
    Bool(bool),
    Binary(Vec<u8>),
    Null,
}

// ─────────────────────────────────────────────────────────────────────────────
// Column metadata
// ─────────────────────────────────────────────────────────────────────────────

/// Metadata about a column's contents (used for block skipping).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnStats {
    pub row_count: u64,
    pub null_count: u64,
    pub min_i64: Option<i64>,
    pub max_i64: Option<i64>,
    pub min_f64: Option<f64>,
    pub max_f64: Option<f64>,
}

impl ColumnStats {
    pub fn new() -> Self {
        Self {
            row_count: 0,
            null_count: 0,
            min_i64: None,
            max_i64: None,
            min_f64: None,
            max_f64: None,
        }
    }

    pub fn update(&mut self, value: &ColumnValue) {
        self.row_count += 1;
        match value {
            ColumnValue::Null => self.null_count += 1,
            ColumnValue::Timestamp(v) | ColumnValue::I64(v) => {
                self.min_i64 = Some(self.min_i64.map_or(*v, |m| m.min(*v)));
                self.max_i64 = Some(self.max_i64.map_or(*v, |m| m.max(*v)));
            }
            ColumnValue::F64(v) => {
                self.min_f64 = Some(self.min_f64.map_or(*v, |m| m.min(*v)));
                self.max_f64 = Some(self.max_f64.map_or(*v, |m| m.max(*v)));
            }
            _ => {}
        }
    }
}

impl Default for ColumnStats {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Column
// ─────────────────────────────────────────────────────────────────────────────

/// An in-memory column holding typed values.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub column_type: ColumnType,
    pub values: Vec<ColumnValue>,
    pub stats: ColumnStats,
}

impl Column {
    pub fn new(name: impl Into<String>, column_type: ColumnType) -> Self {
        Self {
            name: name.into(),
            column_type,
            values: Vec::new(),
            stats: ColumnStats::new(),
        }
    }

    pub fn push(&mut self, value: ColumnValue) {
        self.stats.update(&value);
        self.values.push(value);
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ColumnWriter
// ─────────────────────────────────────────────────────────────────────────────

/// Encodes and compresses a column for on-disk storage.
///
/// Encoding strategies by type:
/// - Timestamp: delta-of-delta (exploits monotonic increase in log timestamps)
/// - I64: delta encoding
/// - F64: raw bytes (no general-purpose float compression applied here)
/// - String: dictionary encoding if ≤ 256 distinct values, else raw
/// - Bool: bit-packed
/// - Binary: raw, length-prefixed
pub struct ColumnWriter {
    column: Column,
    compression: CompressionLevel,
}

impl ColumnWriter {
    pub fn new(column: Column, compression: CompressionLevel) -> Self {
        Self { column, compression }
    }

    /// Serialize and compress the column, returning the encoded bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self.column.column_type {
            ColumnType::Timestamp => self.encode_timestamps(),
            ColumnType::I64 => self.encode_i64s(),
            ColumnType::F64 => self.encode_f64s(),
            ColumnType::String => self.encode_strings(),
            ColumnType::Bool => self.encode_bools(),
            ColumnType::Binary => self.encode_binary(),
        }
    }

    fn encode_timestamps(&self) -> Result<Vec<u8>> {
        let vals: Vec<i64> = self.column.values.iter().map(|v| match v {
            ColumnValue::Timestamp(t) => *t,
            _ => 0,
        }).collect();

        let first = vals.first().copied().unwrap_or(0);

        // Delta-of-delta encoding — skip the first element (stored verbatim)
        let mut deltas = Vec::with_capacity(vals.len().saturating_sub(1));
        let mut prev_delta = 0i64;
        let mut prev = first;
        for &v in vals.iter().skip(1) {
            let delta = v - prev;
            deltas.push(delta - prev_delta);
            prev_delta = delta;
            prev = v;
        }

        let mut buf = Vec::with_capacity(vals.len() * 8 + 8);
        buf.write_u64::<LittleEndian>(first as u64)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        // total count (including first element)
        buf.write_u32::<LittleEndian>(vals.len() as u32)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        for d in &deltas {
            buf.write_i64::<LittleEndian>(*d)
                .map_err(|e| XerjError::internal(e.to_string()))?;
        }

        get_codec(self.compression).compress(&buf)
    }

    fn encode_i64s(&self) -> Result<Vec<u8>> {
        let vals: Vec<i64> = self.column.values.iter().map(|v| match v {
            ColumnValue::I64(n) => *n,
            _ => 0,
        }).collect();

        let mut buf = Vec::with_capacity(vals.len() * 8 + 4);
        buf.write_u32::<LittleEndian>(vals.len() as u32)
            .map_err(|e| XerjError::internal(e.to_string()))?;

        let mut prev = 0i64;
        for &v in &vals {
            let delta = v - prev;
            buf.write_i64::<LittleEndian>(delta)
                .map_err(|e| XerjError::internal(e.to_string()))?;
            prev = v;
        }

        get_codec(self.compression).compress(&buf)
    }

    fn encode_f64s(&self) -> Result<Vec<u8>> {
        let vals: Vec<f64> = self.column.values.iter().map(|v| match v {
            ColumnValue::F64(f) => *f,
            _ => 0.0,
        }).collect();

        let mut buf = Vec::with_capacity(vals.len() * 8 + 4);
        buf.write_u32::<LittleEndian>(vals.len() as u32)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        for &v in &vals {
            buf.write_f64::<LittleEndian>(v)
                .map_err(|e| XerjError::internal(e.to_string()))?;
        }

        get_codec(self.compression).compress(&buf)
    }

    fn encode_strings(&self) -> Result<Vec<u8>> {
        let strs: Vec<&str> = self.column.values.iter().map(|v| match v {
            ColumnValue::String(s) => s.as_str(),
            _ => "",
        }).collect();

        // Try dictionary encoding if cardinality is low
        let distinct: std::collections::HashSet<&str> = strs.iter().copied().collect();
        let use_dict = distinct.len() <= 256;

        let mut buf = Vec::new();
        buf.push(if use_dict { 1u8 } else { 0u8 });

        if use_dict {
            let mut encoder = DictionaryEncoder::new();
            encoder.build_from_sample(strs.iter().copied());
            let dict_bytes = encoder.serialize();

            buf.write_u32::<LittleEndian>(dict_bytes.len() as u32)
                .map_err(|e| XerjError::internal(e.to_string()))?;
            buf.extend_from_slice(&dict_bytes);
            buf.write_u32::<LittleEndian>(strs.len() as u32)
                .map_err(|e| XerjError::internal(e.to_string()))?;

            for s in &strs {
                let id = encoder.encode(s).unwrap_or(0);
                buf.write_u16::<LittleEndian>(id)
                    .map_err(|e| XerjError::internal(e.to_string()))?;
            }
        } else {
            buf.write_u32::<LittleEndian>(strs.len() as u32)
                .map_err(|e| XerjError::internal(e.to_string()))?;
            for s in &strs {
                let bytes = s.as_bytes();
                buf.write_u32::<LittleEndian>(bytes.len() as u32)
                    .map_err(|e| XerjError::internal(e.to_string()))?;
                buf.extend_from_slice(bytes);
            }
        }

        get_codec(self.compression).compress(&buf)
    }

    fn encode_bools(&self) -> Result<Vec<u8>> {
        let vals: Vec<bool> = self.column.values.iter().map(|v| match v {
            ColumnValue::Bool(b) => *b,
            _ => false,
        }).collect();

        // Pack 8 bools per byte
        let byte_count = (vals.len() + 7) / 8;
        let mut packed = vec![0u8; byte_count];
        for (i, &b) in vals.iter().enumerate() {
            if b {
                packed[i / 8] |= 1 << (i % 8);
            }
        }

        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(vals.len() as u32)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        buf.extend_from_slice(&packed);
        get_codec(self.compression).compress(&buf)
    }

    fn encode_binary(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        let count = self.column.values.len();
        buf.write_u32::<LittleEndian>(count as u32)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        for v in &self.column.values {
            let bytes = match v {
                ColumnValue::Binary(b) => b.as_slice(),
                _ => &[],
            };
            buf.write_u32::<LittleEndian>(bytes.len() as u32)
                .map_err(|e| XerjError::internal(e.to_string()))?;
            buf.extend_from_slice(bytes);
        }
        get_codec(self.compression).compress(&buf)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ColumnReader
// ─────────────────────────────────────────────────────────────────────────────

/// Decompresses and decodes columns on demand.
pub struct ColumnReader {
    pub name: String,
    pub column_type: ColumnType,
    /// Compressed column bytes.
    encoded: Vec<u8>,
    /// Uncompressed length hint (for decompressors that need it).
    uncompressed_len: usize,
}

impl ColumnReader {
    pub fn new(
        name: impl Into<String>,
        column_type: ColumnType,
        encoded: Vec<u8>,
        uncompressed_len: usize,
    ) -> Self {
        Self {
            name: name.into(),
            column_type,
            encoded,
            uncompressed_len,
        }
    }

    /// Decompress and decode all values in the column.
    pub fn decode(&self, compression: CompressionLevel) -> Result<Vec<ColumnValue>> {
        let raw = get_codec(compression).decompress(&self.encoded, self.uncompressed_len)?;
        match self.column_type {
            ColumnType::Timestamp => self.decode_timestamps(&raw),
            ColumnType::I64 => self.decode_i64s(&raw),
            ColumnType::F64 => self.decode_f64s(&raw),
            ColumnType::String => self.decode_strings(&raw),
            ColumnType::Bool => self.decode_bools(&raw),
            ColumnType::Binary => self.decode_binary(&raw),
        }
    }

    fn decode_timestamps(&self, raw: &[u8]) -> Result<Vec<ColumnValue>> {
        let mut cur = Cursor::new(raw);
        let first = cur.read_u64::<LittleEndian>()
            .map_err(|e| XerjError::internal(e.to_string()))? as i64;
        let count = cur.read_u32::<LittleEndian>()
            .map_err(|e| XerjError::internal(e.to_string()))? as usize;

        let mut result = Vec::with_capacity(count);
        let mut prev = first;
        let mut prev_delta = 0i64;

        result.push(ColumnValue::Timestamp(first));
        for _ in 1..count {
            let dod = cur.read_i64::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))?;
            let delta = prev_delta + dod;
            let v = prev + delta;
            result.push(ColumnValue::Timestamp(v));
            prev_delta = delta;
            prev = v;
        }
        Ok(result)
    }

    fn decode_i64s(&self, raw: &[u8]) -> Result<Vec<ColumnValue>> {
        let mut cur = Cursor::new(raw);
        let count = cur.read_u32::<LittleEndian>()
            .map_err(|e| XerjError::internal(e.to_string()))? as usize;
        let mut result = Vec::with_capacity(count);
        let mut prev = 0i64;
        for _ in 0..count {
            let delta = cur.read_i64::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))?;
            let v = prev + delta;
            result.push(ColumnValue::I64(v));
            prev = v;
        }
        Ok(result)
    }

    fn decode_f64s(&self, raw: &[u8]) -> Result<Vec<ColumnValue>> {
        let mut cur = Cursor::new(raw);
        let count = cur.read_u32::<LittleEndian>()
            .map_err(|e| XerjError::internal(e.to_string()))? as usize;
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            let v = cur.read_f64::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))?;
            result.push(ColumnValue::F64(v));
        }
        Ok(result)
    }

    fn decode_strings(&self, raw: &[u8]) -> Result<Vec<ColumnValue>> {
        let use_dict = raw[0] == 1;
        let mut cur = Cursor::new(&raw[1..]);

        if use_dict {
            let dict_len = cur.read_u32::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))? as usize;
            let mut dict_bytes = vec![0u8; dict_len];
            cur.read_exact(&mut dict_bytes)
                .map_err(|e| XerjError::internal(e.to_string()))?;
            let decoder = DictionaryDecoder::deserialize(&dict_bytes)?;

            let count = cur.read_u32::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))? as usize;
            let mut result = Vec::with_capacity(count);
            for _ in 0..count {
                let id = cur.read_u16::<LittleEndian>()
                    .map_err(|e| XerjError::internal(e.to_string()))?;
                let s = decoder.decode(id).unwrap_or("").to_owned();
                result.push(ColumnValue::String(s));
            }
            Ok(result)
        } else {
            let count = cur.read_u32::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))? as usize;
            let mut result = Vec::with_capacity(count);
            for _ in 0..count {
                let slen = cur.read_u32::<LittleEndian>()
                    .map_err(|e| XerjError::internal(e.to_string()))? as usize;
                let mut sbytes = vec![0u8; slen];
                cur.read_exact(&mut sbytes)
                    .map_err(|e| XerjError::internal(e.to_string()))?;
                let s = String::from_utf8(sbytes)
                    .map_err(|e| XerjError::internal(format!("utf8: {e}")))?;
                result.push(ColumnValue::String(s));
            }
            Ok(result)
        }
    }

    fn decode_bools(&self, raw: &[u8]) -> Result<Vec<ColumnValue>> {
        let mut cur = Cursor::new(raw);
        let count = cur.read_u32::<LittleEndian>()
            .map_err(|e| XerjError::internal(e.to_string()))? as usize;
        let byte_count = (count + 7) / 8;
        let mut packed = vec![0u8; byte_count];
        cur.read_exact(&mut packed)
            .map_err(|e| XerjError::internal(e.to_string()))?;

        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let b = (packed[i / 8] >> (i % 8)) & 1 == 1;
            result.push(ColumnValue::Bool(b));
        }
        Ok(result)
    }

    fn decode_binary(&self, raw: &[u8]) -> Result<Vec<ColumnValue>> {
        let mut cur = Cursor::new(raw);
        let count = cur.read_u32::<LittleEndian>()
            .map_err(|e| XerjError::internal(e.to_string()))? as usize;
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            let blen = cur.read_u32::<LittleEndian>()
                .map_err(|e| XerjError::internal(e.to_string()))? as usize;
            let mut bytes = vec![0u8; blen];
            cur.read_exact(&mut bytes)
                .map_err(|e| XerjError::internal(e.to_string()))?;
            result.push(ColumnValue::Binary(bytes));
        }
        Ok(result)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(column: Column) -> Vec<ColumnValue> {
        let compression = CompressionLevel::Fast;
        let encoded = ColumnWriter::new(column.clone(), compression).encode().unwrap();
        let reader = ColumnReader::new(&column.name, column.column_type, encoded, 65536);
        reader.decode(compression).unwrap()
    }

    #[test]
    fn timestamp_roundtrip() {
        let mut col = Column::new("ts", ColumnType::Timestamp);
        let now = 1_700_000_000_000_000_000i64;
        for i in 0..10 {
            col.push(ColumnValue::Timestamp(now + i * 1_000_000)); // 1ms apart
        }
        let decoded = roundtrip(col.clone());
        assert_eq!(decoded.len(), col.values.len());
        for (a, b) in col.values.iter().zip(decoded.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn string_dict_roundtrip() {
        let mut col = Column::new("level", ColumnType::String);
        let levels = ["INFO", "ERROR", "WARN", "INFO", "INFO", "DEBUG"];
        for l in &levels {
            col.push(ColumnValue::String(l.to_string()));
        }
        let decoded = roundtrip(col);
        assert_eq!(decoded.len(), levels.len());
        for (i, l) in levels.iter().enumerate() {
            assert_eq!(decoded[i], ColumnValue::String(l.to_string()));
        }
    }

    #[test]
    fn bool_roundtrip() {
        let mut col = Column::new("active", ColumnType::Bool);
        let vals = [true, false, true, true, false];
        for v in &vals {
            col.push(ColumnValue::Bool(*v));
        }
        let decoded = roundtrip(col);
        for (i, v) in vals.iter().enumerate() {
            assert_eq!(decoded[i], ColumnValue::Bool(*v));
        }
    }

    #[test]
    fn i64_roundtrip() {
        let mut col = Column::new("count", ColumnType::I64);
        for i in [0i64, 100, -50, 1_000_000, -1] {
            col.push(ColumnValue::I64(i));
        }
        let decoded = roundtrip(col.clone());
        assert_eq!(decoded, col.values);
    }
}
