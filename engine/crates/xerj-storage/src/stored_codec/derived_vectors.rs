//! Derived `semantic_text` vectors are not persisted in stored `_source`
//! (#1122, #2433).
//!
//! With the built-in lexical embedder, ingest writes three companions next to
//! each `semantic_text` field: the pooled vector (`<target>`), the passage
//! metadata (`__xerj_passage_meta__<target>`) and, for multi-passage text,
//! the per-passage vectors (`<target>_chunks`). The vectors are a pure
//! function of the text ([`xerj_ai::semantic_lexical`]), and the HNSW index
//! holds the pooled vector again, so persisting them in every stored row made
//! a semantic index ~2.2x its source.
//!
//! Like Elasticsearch, which keeps inference results out of the stored
//! `_source` by default, the columnar encoder drops them and the decoders
//! rebuild them:
//!
//! * **Encode** ([`strip_source`]): a target is dropped only after the vectors
//!   re-derived from the text compare bit-equal to the stored ones. Vectors
//!   from an external/neural/ONNX backend, or supplied by the caller, never
//!   match and are kept verbatim. The passage metadata gains a marker
//!   (`"derived":"lexical-v1"`, `"dims":N`) so readers know to rebuild them.
//! * **Decode** ([`rehydrate_source_map`]): every reader that can return the
//!   vectors puts them back from the text, so `_source`, kNN, reindex and
//!   snapshot readers see exactly the documents that were ingested.
//! * Merges read with the marker preserved and stay stripped; old segments
//!   (no marker) still carry their vectors and decode unchanged, and are
//!   stripped when a merge rewrites them.
//!
//! `XERJ_STORE_SEMANTIC_VECTORS=true` keeps the vectors in newly written
//! segments, and merges then rebuild the vectors of already-stripped rows
//! ([`super::decode_stored_for_rewrite`]), so a `force_merge` under the
//! opt-out leaves every segment with full vectors before a downgrade to a
//! binary without this code.

use serde_json::{Map, Value};
pub use xerj_ai::semantic_lexical::LexicalSemanticVectors;

/// Must equal `xerj_query::executor::PASSAGE_METADATA_PREFIX` (asserted by an
/// engine test; storage cannot depend on the query crate).
pub const PASSAGE_METADATA_PREFIX: &str = "__xerj_passage_meta__";
/// Marker value: vectors rebuilt by [`xerj_ai::semantic_lexical`] v1.
pub const MARKER_LEXICAL_V1: &str = "lexical-v1";
const MARKER_KEY: &str = "derived";
const DIMS_KEY: &str = "dims";
const CHUNKS_SUFFIX: &str = "_chunks";

/// Byte pattern a canonical serialisation contains iff some row is stripped.
pub(super) const MARKER_BYTES: &[u8] = br#""derived":"lexical-v1""#;

/// 0 = read `XERJ_STORE_SEMANTIC_VECTORS` on first use, 1 = strip, 2 = store.
static STORE_VECTORS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `XERJ_STORE_SEMANTIC_VECTORS` is set: write full vectors, and have merges
/// put back the vectors of rows an earlier write stripped.
pub fn store_vectors_opt_out() -> bool {
    use std::sync::atomic::Ordering;
    match STORE_VECTORS.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var("XERJ_STORE_SEMANTIC_VECTORS")
                .map(|v| {
                    matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                })
                .unwrap_or(false);
            let _ = STORE_VECTORS.compare_exchange(
                0,
                if on { 2 } else { 1 },
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            STORE_VECTORS.load(Ordering::Relaxed) == 2
        }
    }
}

/// Override `XERJ_STORE_SEMANTIC_VECTORS` for the rest of the process.
/// For tests that model "restart with the opt-out set" in one process.
#[doc(hidden)]
pub fn set_store_vectors_opt_out(on: bool) {
    STORE_VECTORS.store(if on { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn chunks_name(target: &str) -> String {
    format!("{target}{CHUNKS_SUFFIX}")
}

/// The target a passage-metadata column/key describes.
pub(super) fn metadata_target(name: &str) -> Option<&str> {
    name.strip_prefix(PASSAGE_METADATA_PREFIX)
        .filter(|target| !target.is_empty())
}

pub(super) fn vector_f32(value: &Value) -> Option<Vec<f32>> {
    match value {
        Value::String(s) => xerj_common::packed_f32::unpack_f32_vector(s),
        Value::Array(items) if !items.is_empty() => {
            items.iter().map(|v| v.as_f64().map(|f| f as f32)).collect()
        }
        _ => None,
    }
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// The JSON form ingest writes for one vector (packed `xerj:f32:` string,
/// or a number array when the vector cannot be packed).
fn vector_value(vector: &[f32]) -> Value {
    match xerj_common::packed_f32::pack_f32_vector(vector) {
        Some(packed) => Value::String(packed),
        None => Value::Array(vector.iter().map(|f| Value::from(*f as f64)).collect()),
    }
}

/// Typed vectors for a stripped row: `(pooled, chunks)`. `None` when the
/// metadata carries no marker or the text is missing.
pub(super) fn regenerate(meta: &Value, text: Option<&str>) -> Option<LexicalSemanticVectors> {
    let meta = meta.as_object()?;
    if meta.get(MARKER_KEY).and_then(Value::as_str) != Some(MARKER_LEXICAL_V1) {
        return None;
    }
    let dims = meta.get(DIMS_KEY).and_then(Value::as_u64)? as usize;
    xerj_ai::semantic_lexical::lexical_semantic_vectors(text?, dims)
}

/// The text field a passage metadata value names.
pub(super) fn metadata_field(meta: &Value) -> Option<&str> {
    meta.get("field").and_then(Value::as_str)
}

pub(super) fn is_stripped(meta: &Value) -> bool {
    meta.get(MARKER_KEY).and_then(Value::as_str) == Some(MARKER_LEXICAL_V1)
}

/// Encode side: a copy of `source` without the derived vectors it can prove
/// are regenerable, or `None` when nothing is dropped (no semantic field,
/// non-lexical or caller-supplied vectors, already stripped, or opt-out).
pub fn strip_source(source: &Value) -> Option<Value> {
    if store_vectors_opt_out() {
        return None;
    }
    let obj = source.as_object()?;
    let mut stripped: Vec<(&str, String)> = Vec::new();
    for (key, meta) in obj {
        let Some(target) = metadata_target(key) else {
            continue;
        };
        if is_stripped(meta) {
            continue;
        }
        let Some(text) = metadata_field(meta)
            .and_then(|f| obj.get(f))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(stored) = obj.get(target).and_then(vector_f32) else {
            continue;
        };
        let Some((pooled, chunks)) =
            xerj_ai::semantic_lexical::lexical_semantic_vectors(text, stored.len())
        else {
            continue;
        };
        if !same_bits(&pooled, &stored) {
            continue;
        }
        let chunks_key = chunks_name(target);
        let stored_chunks = obj.get(&chunks_key).filter(|v| !v.is_null());
        let chunks_match = match (&chunks, stored_chunks) {
            (None, None) => true,
            (Some(expected), Some(Value::Array(items))) => {
                items.len() == expected.len()
                    && items.iter().zip(expected).all(|(item, want)| {
                        vector_f32(item).is_some_and(|got| same_bits(&got, want))
                    })
            }
            _ => false,
        };
        if chunks_match {
            stripped.push((target, chunks_key));
        }
    }
    if stripped.is_empty() {
        return None;
    }
    // Rebuild in order (no `swap_remove` reordering of the visible fields).
    let mut out = Map::new();
    for (key, value) in obj {
        if stripped
            .iter()
            .any(|(target, chunks)| key == target || key == chunks)
        {
            continue;
        }
        let mut value = value.clone();
        if let Some(target) = metadata_target(key) {
            if stripped.iter().any(|(t, _)| *t == target) {
                let dims = obj.get(target).and_then(vector_f32).map_or(0, |v| v.len());
                if let Some(meta) = value.as_object_mut() {
                    meta.insert(MARKER_KEY.into(), Value::from(MARKER_LEXICAL_V1));
                    meta.insert(DIMS_KEY.into(), Value::from(dims as u64));
                }
            }
        }
        out.insert(key.clone(), value);
    }
    Some(Value::Object(out))
}

/// Decode side: put the vectors back for every stripped target in `map`, and
/// remove the marker, so the document is the one ingest produced. Returns
/// whether anything changed. Order matches ingest: `<target>`, metadata,
/// `<target>_chunks`.
pub fn rehydrate_source_map(map: &mut Map<String, Value>) -> bool {
    let mut regenerated: Vec<(String, LexicalSemanticVectors)> = Vec::new();
    for (key, meta) in map.iter() {
        let Some(target) = metadata_target(key) else {
            continue;
        };
        if !is_stripped(meta) {
            continue;
        }
        let text = metadata_field(meta)
            .and_then(|f| map.get(f))
            .and_then(Value::as_str);
        if let Some((pooled, chunks)) = regenerate(meta, text) {
            regenerated.push((target.to_string(), (pooled, chunks)));
        }
    }
    // A marker whose text is gone (cannot happen for an ingested doc) is
    // still removed so the metadata reads as written.
    let any_marker = map
        .iter()
        .any(|(key, meta)| metadata_target(key).is_some() && is_stripped(meta));
    if !any_marker {
        return false;
    }
    let old = std::mem::take(map);
    for (key, mut value) in old {
        if let Some(target) = metadata_target(&key) {
            if is_stripped(&value) {
                if let Some(meta) = value.as_object_mut() {
                    meta.remove(MARKER_KEY);
                    meta.remove(DIMS_KEY);
                }
                if let Some(ix) = regenerated.iter().position(|(t, _)| t == target) {
                    let (target, (pooled, chunks)) = regenerated.swap_remove(ix);
                    map.insert(target.clone(), vector_value(&pooled));
                    map.insert(key, value);
                    if let Some(chunks) = chunks {
                        map.insert(
                            chunks_name(&target),
                            Value::Array(chunks.iter().map(|c| vector_value(c)).collect()),
                        );
                    }
                    continue;
                }
            }
        }
        map.insert(key, value);
    }
    true
}

/// [`rehydrate_source_map`] over a canonical `[{"_id","_seq_no","_source"}]`
/// byte array (the legacy LZ4/raw decode). Returns the input unchanged when
/// no row is stripped.
pub(super) fn rehydrate_canonical(bytes: Vec<u8>) -> crate::Result<Vec<u8>> {
    if !contains_marker(&bytes) {
        return Ok(bytes);
    }
    let mut docs: Vec<Value> = serde_json::from_slice(&bytes)
        .map_err(|e| crate::StorageError::Other(anyhow::anyhow!("rehydrate parse: {e}")))?;
    for doc in &mut docs {
        if let Some(source) = doc.get_mut("_source").and_then(Value::as_object_mut) {
            rehydrate_source_map(source);
        }
    }
    serde_json::to_vec(&docs)
        .map_err(|e| crate::StorageError::Other(anyhow::anyhow!("rehydrate serialize: {e}")))
}

pub(super) fn contains_marker(bytes: &[u8]) -> bool {
    memchr::memmem::find(bytes, MARKER_BYTES).is_some()
}

/// Whether a reader that keeps the columns `keep` accepts must rebuild
/// vectors: some passage-metadata column exists and `keep` asks for its
/// target or the target's chunks.
pub(super) fn keep_wants_derived<'a>(
    names: impl Iterator<Item = &'a str>,
    keep: &dyn Fn(&str) -> bool,
) -> bool {
    names
        .filter_map(metadata_target)
        .any(|target| keep(target) || keep(&chunks_name(target)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn ingested(text: &str, dims: usize) -> Value {
        let (pooled, chunks) =
            xerj_ai::semantic_lexical::lexical_semantic_vectors(text, dims).unwrap();
        let mut src = Map::new();
        src.insert("title".into(), json!("t"));
        src.insert("body".into(), json!(text));
        src.insert("body_vector".into(), vector_value(&pooled));
        src.insert(
            format!("{PASSAGE_METADATA_PREFIX}body_vector"),
            json!({"field": "body", "chunks": [[0, text.len()]]}),
        );
        if let Some(chunks) = chunks {
            src.insert(
                "body_vector_chunks".into(),
                Value::Array(chunks.iter().map(|c| vector_value(c)).collect()),
            );
        }
        Value::Object(src)
    }

    #[test]
    fn strip_then_rehydrate_round_trips_short_and_long_text() {
        for text in [
            "a short note about rust",
            &"lorem ipsum dolor sit amet ".repeat(60),
        ] {
            let original = ingested(text, 24);
            let stripped = strip_source(&original).expect("lexical vectors are stripped");
            let obj = stripped.as_object().unwrap();
            assert!(!obj.contains_key("body_vector"));
            assert!(!obj.contains_key("body_vector_chunks"));
            assert!(is_stripped(
                &obj[&format!("{PASSAGE_METADATA_PREFIX}body_vector")]
            ));
            let mut back = obj.clone();
            assert!(rehydrate_source_map(&mut back));
            assert_eq!(Value::Object(back), original);
        }
    }

    #[test]
    fn foreign_vectors_are_kept() {
        let mut original = ingested("a short note about rust", 8);
        original["body_vector"] = vector_value(&[0.5; 8]);
        assert!(strip_source(&original).is_none());
        let mut long = ingested(&"lorem ipsum dolor sit amet ".repeat(60), 8);
        long["body_vector_chunks"][0] = vector_value(&[0.5; 8]);
        assert!(strip_source(&long).is_none());
        // Edited text no longer derives the stored vector.
        let mut edited = ingested("a short note about rust", 8);
        edited["body"] = json!("something else");
        assert!(strip_source(&edited).is_none());
    }

    #[test]
    fn number_array_vectors_are_stripped_and_rebuilt_packed() {
        let mut original = ingested("a short note about rust", 8);
        let vector = vector_f32(&original["body_vector"]).unwrap();
        original["body_vector"] = json!(vector.iter().map(|f| *f as f64).collect::<Vec<_>>());
        let stripped = strip_source(&original).unwrap();
        let mut back = stripped.as_object().unwrap().clone();
        rehydrate_source_map(&mut back);
        assert_eq!(vector_f32(&back["body_vector"]).unwrap(), vector);
    }

    #[test]
    fn documents_without_semantic_fields_are_untouched() {
        assert!(strip_source(&json!({"a": 1, "b": "x"})).is_none());
        let mut map = json!({"a": 1}).as_object().unwrap().clone();
        assert!(!rehydrate_source_map(&mut map));
        assert!(contains_marker(br#"{"derived":"lexical-v1","dims":8}"#));
    }
}
