//! Compact stand-in for a JSON array of finite `f32`s.
//!
//! A 384-d embedding stored as `serde_json::Value::Number`s is one `Value`
//! per component. On the lexical embedder that is what `semantic_text`
//! writes into every autoindexed record, and both memtables retain that
//! tree until flush. Measured on the #1940 corpus (5,000 records, ~195
//! bytes each) the parsed tree was ~43 KB/doc in the stored-values cache
//! and ~78 KB/doc of RSS growth during `index`, against ~1.5 KB of actual
//! `f32` payload.
//!
//! The packed form is a single string, `xerj:f32:` plus lowercase hex of
//! the little-endian `f32` bits. It round-trips bit-exactly, survives the
//! stored-section JSON codec, and expands back to a number array only when
//! a response actually returns the field.

use serde_json::Value;

pub const PACKED_F32_PREFIX: &str = "xerj:f32:";

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Pack `values` when every element is finite and the slice is non-empty.
pub fn pack_f32_vector(values: &[f32]) -> Option<String> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let mut out = String::with_capacity(PACKED_F32_PREFIX.len() + values.len() * 8);
    out.push_str(PACKED_F32_PREFIX);
    for value in values {
        for byte in value.to_le_bytes() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    Some(out)
}

pub fn is_packed_f32(value: &str) -> bool {
    value.starts_with(PACKED_F32_PREFIX)
        && value.len() > PACKED_F32_PREFIX.len()
        && (value.len() - PACKED_F32_PREFIX.len()).is_multiple_of(8)
}

/// Decode a packed vector. `None` for any other string, a bad nibble, a
/// non-multiple-of-8 payload, or a non-finite element.
pub fn unpack_f32_vector(value: &str) -> Option<Vec<f32>> {
    let rest = value.strip_prefix(PACKED_F32_PREFIX)?;
    if rest.is_empty() || !rest.len().is_multiple_of(8) {
        return None;
    }
    let mut out = Vec::with_capacity(rest.len() / 8);
    let bytes = rest.as_bytes();
    for chunk in bytes.chunks_exact(8) {
        let mut le = [0u8; 4];
        for (i, pair) in chunk.chunks_exact(2).enumerate() {
            le[i] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
        }
        let decoded = f32::from_le_bytes(le);
        if !decoded.is_finite() {
            return None;
        }
        out.push(decoded);
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// A JSON number array, or a packed string, as finite `f32`s.
///
/// Number arrays use the same `as_f64() as f32` conversion the exact kNN
/// scan has always used, which is bit-exact for every finite `f32`.
pub fn json_to_f32_vector(value: &Value) -> Option<Vec<f32>> {
    match value {
        Value::String(text) => unpack_f32_vector(text),
        // An empty array is a present-but-empty vector. The kNN projection
        // treats that as supported (`Some(vec![])`), distinct from null and
        // from a shape it cannot score. Packing still refuses it.
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let decoded = item.as_f64()? as f32;
                if !decoded.is_finite() {
                    return None;
                }
                out.push(decoded);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Replace a number array (or an array of number arrays) with the packed
/// string form, in place. Leaves every other shape untouched, including a
/// value that is already packed and a vector containing a non-finite
/// element.
pub fn compact_vector_value(value: &mut Value) {
    let Some(packed) = try_compact(value) else {
        return;
    };
    *value = packed;
}

fn try_compact(value: &Value) -> Option<Value> {
    let Value::Array(items) = value else {
        return None;
    };
    if items.is_empty() {
        return None;
    }
    if items.iter().all(Value::is_number) {
        return pack_f32_vector(&json_to_f32_vector(value)?).map(Value::String);
    }
    if items.iter().all(|item| {
        item.as_array()
            .is_some_and(|inner| !inner.is_empty() && inner.iter().all(Value::is_number))
    }) {
        let mut chunks = Vec::with_capacity(items.len());
        for item in items {
            chunks.push(Value::String(pack_f32_vector(&json_to_f32_vector(item)?)?));
        }
        return Some(Value::Array(chunks));
    }
    None
}

/// Expand packed strings back into JSON number arrays, recursively.
///
/// Response paths call this on the page (or the one document) they are
/// about to return. The memtable and the stored-values cache keep the
/// packed form.
pub fn expand_packed_f32_values(value: &mut Value) {
    if let Value::String(text) = value {
        if let Some(floats) = unpack_f32_vector(text) {
            *value = Value::Array(floats.into_iter().map(|f| Value::from(f as f64)).collect());
        }
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                expand_packed_f32_values(item);
            }
        }
        Value::Object(map) => {
            for child in map.values_mut() {
                expand_packed_f32_values(child);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_preserves_f32_bits() {
        let values = [0.0_f32, -0.0, 1.0, -1.5, 1.0e-20, f32::MIN, f32::MAX];
        let packed = pack_f32_vector(&values).unwrap();
        assert!(packed.starts_with(PACKED_F32_PREFIX));
        assert_eq!(unpack_f32_vector(&packed).unwrap(), values);
    }

    #[test]
    fn rejects_non_finite_and_foreign_strings() {
        assert!(pack_f32_vector(&[]).is_none());
        assert!(pack_f32_vector(&[f32::NAN]).is_none());
        assert!(pack_f32_vector(&[f32::INFINITY]).is_none());
        assert!(unpack_f32_vector("xerj:f32:zz").is_none());
        assert!(unpack_f32_vector("hello").is_none());
        assert!(unpack_f32_vector("xerj:f32:0000000").is_none());
    }

    #[test]
    fn compact_then_expand_matches_the_number_array() {
        let mut pooled = json!([0.25, -2.0, 4.0]);
        compact_vector_value(&mut pooled);
        assert!(pooled.as_str().unwrap().starts_with(PACKED_F32_PREFIX));
        expand_packed_f32_values(&mut pooled);
        let back = json_to_f32_vector(&pooled).unwrap();
        assert_eq!(back, vec![0.25, -2.0, 4.0]);

        let mut chunks = json!([[1.0, 0.0], [0.0, 1.0]]);
        compact_vector_value(&mut chunks);
        assert!(chunks.as_array().unwrap()[0].is_string());
        expand_packed_f32_values(&mut chunks);
        assert_eq!(chunks[0].as_array().unwrap().len(), 2);
        assert_eq!(chunks[1].as_array().unwrap().len(), 2);
    }
}
