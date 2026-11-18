//! Columnar stored encode without a parsed tree of the array and object
//! fields (#2917).
//!
//! [`super::encode_stored_v2_at_level`] used to parse the whole stored JSON
//! array into one `serde_json::Value` per document and hold that tree for
//! the entire encode. A 1024-d `dense_vector` in `_source` is 1024 `Value`
//! numbers per row, several times the row's JSON text, and the tree stayed
//! live through the long-effort zstd pass of an explicit `_forcemerge`. A
//! 730 MB vector index took a node from 3.0 to 10.4 GB (#2917).
//!
//! An array or object field never takes the dictionary, cross-dep or
//! typed-int codec: its column is CONSTANT, COPY_OF, or the JSON fallback,
//! and the fallback's input is the column serialised as `[row,row,…]`. So
//! this reader parses each row only down to its `_source` keys, keeps every
//! value as borrowed JSON text, and parses into `Value`s only the columns
//! that hold a scalar somewhere. A column whose rows are all arrays/objects
//! (or null/absent) stays text: each row is checked to be exactly what
//! `serde_json` writes for its parsed value, so the encoded bytes match the
//! tree build. A row that is not canonical uses the re-serialised form.
//!
//! Anything this reader does not model returns `None`, and the caller runs
//! the tree build unchanged: a top-level element that is not an object, a
//! duplicate top-level key, a duplicate `_source` key (the tree keeps the
//! first position and the last value), or fewer than `V2_MIN_DOCS` rows.
//! Rows that carry `semantic_text` passage metadata are parsed whole and go
//! through [`derived_vectors::strip_source`] exactly like the tree build.
//!
//! The approach follows tantivy's doc-store merge, which moves stored
//! documents as bytes and never rebuilds them as typed values
//! (`tantivy/src/store/writer.rs:126` `StoreWriter::stack`); XERJ's stored
//! section is one columnar blob per segment, so only the column values can
//! stay as text here, not whole compressed blocks.

use super::derived_vectors;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::Value;
use std::borrow::Cow;

/// One array/object column held as canonical JSON text per row (`None` =
/// null or absent).
#[derive(PartialEq)]
pub(super) struct JsonColumn<'a> {
    rows: Vec<Option<Cow<'a, [u8]>>>,
}

impl JsonColumn<'_> {
    /// Order-sensitive digest for COPY_OF candidates. Only ever compared
    /// with other JSON columns, and equality is verified before use.
    pub(super) fn digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.rows.len() as u64).hash(&mut h);
        for row in &self.rows {
            match row {
                None => 0u8.hash(&mut h),
                Some(bytes) => {
                    1u8.hash(&mut h);
                    bytes.hash(&mut h);
                }
            }
        }
        h.finish()
    }

    /// The CONSTANT payload: every row present and identical.
    pub(super) fn constant(&self) -> Option<Vec<u8>> {
        let first = self.rows.first()?.as_deref()?;
        self.rows
            .iter()
            .all(|row| row.as_deref() == Some(first))
            .then(|| first.to_vec())
    }

    /// `[row,row,…]` with `null` for a missing row: the bytes
    /// `serde_json::to_vec` writes for the parsed column.
    pub(super) fn to_json_array(&self) -> Vec<u8> {
        let len = 2
            + self.rows.len()
            + self
                .rows
                .iter()
                .map(|row| row.as_ref().map_or(4, |bytes| bytes.len()))
                .sum::<usize>();
        let mut out = Vec::with_capacity(len);
        out.push(b'[');
        for (i, row) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            out.extend_from_slice(row.as_deref().unwrap_or(b"null"));
        }
        out.push(b']');
        out
    }
}

/// A stored row read only down to its top-level keys.
#[derive(serde::Deserialize)]
struct LazyDoc<'a> {
    #[serde(rename = "_id", default)]
    id: Value,
    #[serde(rename = "_seq_no", default)]
    seq_no: Value,
    #[serde(rename = "_source", borrow, default)]
    source: Option<&'a RawValue>,
}

/// An object key, borrowed from the input when it has no escapes.
struct Key<'a>(Cow<'a, str>);

impl<'de> Deserialize<'de> for Key<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = Key<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object key")
            }
            fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Borrowed(v)))
            }
            fn visit_str<E>(self, v: &str) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Owned(v.to_owned())))
            }
            fn visit_string<E>(self, v: String) -> Result<Key<'de>, E> {
                Ok(Key(Cow::Owned(v)))
            }
        }
        deserializer.deserialize_str(KeyVisitor)
    }
}

/// A `_source` object as `(key, raw value)` pairs in document order.
struct Fields<'a>(Vec<(Cow<'a, str>, &'a RawValue)>);

impl<'de> Deserialize<'de> for Fields<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;
        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = Fields<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a _source object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields<'de>, A::Error> {
                let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(Key(key)) = map.next_key::<Key<'de>>()? {
                    let value: &'de RawValue = map.next_value()?;
                    pairs.push((key, value));
                }
                Ok(Fields(pairs))
            }
        }
        deserializer.deserialize_map(FieldsVisitor)
    }
}

enum DocFields<'a> {
    /// No `_source`, or one that is not an object.
    Empty,
    Lazy(Vec<(Cow<'a, str>, &'a RawValue)>),
    /// Parsed whole: carries `semantic_text` metadata, already stripped.
    Parsed(Value),
}

#[derive(Clone, Copy)]
enum Cell<'f> {
    Absent,
    Raw(&'f RawValue),
    Parsed(&'f Value),
}

fn first_byte(text: &str) -> Option<u8> {
    text.trim_start().as_bytes().first().copied()
}

fn doc_fields<'a>(doc: &LazyDoc<'a>) -> Option<DocFields<'a>> {
    let Some(raw) = doc.source else {
        return Some(DocFields::Empty);
    };
    let text: &'a str = raw.get();
    if first_byte(text) != Some(b'{') {
        return Some(DocFields::Empty);
    }
    let Fields(pairs) = serde_json::from_str(text).ok()?;
    if pairs.len() > 1 {
        let mut seen: rustc_hash::FxHashSet<&str> = rustc_hash::FxHashSet::default();
        if !pairs.iter().all(|(key, _)| seen.insert(key)) {
            return None;
        }
    }
    if pairs
        .iter()
        .any(|(key, _)| key.starts_with(derived_vectors::PASSAGE_METADATA_PREFIX))
    {
        let value: Value = serde_json::from_str(text).ok()?;
        let value = derived_vectors::strip_source(&value).unwrap_or(value);
        return Some(DocFields::Parsed(value));
    }
    Some(DocFields::Lazy(pairs))
}

impl Cell<'_> {
    fn is_null(self) -> bool {
        match self {
            Cell::Absent => false,
            Cell::Raw(raw) => raw.get().trim() == "null",
            Cell::Parsed(value) => value.is_null(),
        }
    }

    /// An array or object.
    fn is_container(self) -> bool {
        match self {
            Cell::Absent => false,
            Cell::Raw(raw) => matches!(first_byte(raw.get()), Some(b'[' | b'{')),
            Cell::Parsed(value) => value.is_array() || value.is_object(),
        }
    }
}

/// The canonical JSON text of one container row: the input bytes when they
/// are already what `serde_json` writes for the parsed value.
fn canonical_row<'a>(cell: Cell<'a>) -> Option<Option<Cow<'a, [u8]>>> {
    match cell {
        Cell::Absent => Some(None),
        _ if cell.is_null() => Some(None),
        Cell::Raw(raw) => {
            let text = raw.get();
            let value: Value = serde_json::from_str(text).ok()?;
            let canonical = serde_json::to_vec(&value).ok()?;
            if canonical == text.as_bytes() {
                Some(Some(Cow::Borrowed(text.as_bytes())))
            } else {
                Some(Some(Cow::Owned(canonical)))
            }
        }
        Cell::Parsed(value) => Some(Some(Cow::Owned(serde_json::to_vec(value).ok()?))),
    }
}

fn parsed_row(cell: Cell<'_>) -> Option<Value> {
    match cell {
        Cell::Absent => Some(Value::Null),
        Cell::Raw(raw) => serde_json::from_str(raw.get()).ok(),
        Cell::Parsed(value) => Some(value.clone()),
    }
}

/// Encode `stored_docs_json` like the tree build in
/// [`super::encode_stored_v2_at_level`], or `None` when this reader does not
/// handle its shape (see the module docs).
pub(super) fn encode(
    stored_docs_json: &[u8],
    level: i32,
    long_effort_level: Option<i32>,
) -> Option<Vec<u8>> {
    use rayon::prelude::*;

    let docs: Vec<LazyDoc<'_>> = serde_json::from_slice(stored_docs_json).ok()?;
    let num_docs = docs.len();
    if num_docs < super::V2_MIN_DOCS {
        return None;
    }
    let fields: Vec<DocFields<'_>> = docs
        .par_iter()
        .map(doc_fields)
        .collect::<Option<Vec<_>>>()?;

    // Column discovery and routing in document order, as the tree build
    // does it: `__id` / `__seq_no` are the synthetic first two columns, and
    // a `_source` key with either name is skipped.
    let mut col_order: Vec<String> = vec!["__id".into(), "__seq_no".into()];
    let mut col_seen: rustc_hash::FxHashMap<&str, usize> = rustc_hash::FxHashMap::default();
    col_seen.insert("__id", 0);
    col_seen.insert("__seq_no", 1);
    let mut cells: Vec<Vec<Cell<'_>>> = vec![Vec::new(), Vec::new()];
    let mut present_nulls: Vec<(u32, u32)> = Vec::new();
    for (doc_idx, doc) in fields.iter().enumerate() {
        let pairs: Box<dyn Iterator<Item = (&str, Cell<'_>)>> = match doc {
            DocFields::Empty => Box::new(std::iter::empty()),
            DocFields::Lazy(pairs) => Box::new(
                pairs
                    .iter()
                    .map(|(key, raw)| (key.as_ref(), Cell::Raw(raw))),
            ),
            DocFields::Parsed(value) => match value.as_object() {
                Some(obj) => Box::new(obj.iter().map(|(key, v)| (key.as_str(), Cell::Parsed(v)))),
                None => Box::new(std::iter::empty()),
            },
        };
        for (key, cell) in pairs {
            let cix = match col_seen.get(key) {
                Some(&cix) if cix < 2 => continue,
                Some(&cix) => cix,
                None => {
                    let cix = col_order.len();
                    col_seen.insert(key, cix);
                    col_order.push(key.to_owned());
                    cells.push(Vec::new());
                    cix
                }
            };
            if cell.is_null() {
                present_nulls.push((doc_idx as u32, cix as u32));
            }
            let column = &mut cells[cix];
            column.resize(doc_idx, Cell::Absent);
            column.push(cell);
        }
    }
    for column in cells.iter_mut().skip(2) {
        column.resize(num_docs, Cell::Absent);
    }

    // Containers-only columns stay text; the rest are parsed.
    let mut json_columns: Vec<Option<JsonColumn<'_>>> = Vec::with_capacity(cells.len());
    let mut parsed_columns: Vec<Vec<Value>> = Vec::with_capacity(cells.len());
    for (cix, column) in cells.iter().enumerate() {
        let text_only = cix >= 2
            && column.iter().any(|cell| cell.is_container())
            && column
                .iter()
                .all(|cell| matches!(cell, Cell::Absent) || cell.is_null() || cell.is_container());
        if text_only {
            let rows = column
                .par_iter()
                .map(|cell| canonical_row(*cell))
                .collect::<Option<Vec<_>>>()?;
            json_columns.push(Some(JsonColumn { rows }));
            parsed_columns.push(Vec::new());
        } else {
            json_columns.push(None);
            parsed_columns.push(
                column
                    .iter()
                    .map(|cell| parsed_row(*cell))
                    .collect::<Option<Vec<_>>>()?,
            );
        }
    }
    drop(cells);

    let mut columns: Vec<Vec<&Value>> = Vec::with_capacity(parsed_columns.len());
    columns.push(docs.iter().map(|doc| &doc.id).collect());
    columns.push(docs.iter().map(|doc| &doc.seq_no).collect());
    for parsed in parsed_columns.iter().skip(2) {
        columns.push(parsed.iter().collect());
    }

    Some(super::wrap_stored_v3(
        super::encode_v2_columns_with_json(
            num_docs,
            &col_order,
            &columns,
            &json_columns,
            Some(stored_docs_json),
            level,
            long_effort_level,
        ),
        present_nulls,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::{
        decode_stored, encode_stored_v2_at_level, encode_stored_v2_forcemerge_at_level,
        encode_stored_v2_tree, long_effort_level_for_len, RAW_LONG_LEVEL, STORED_ZSTD_LEVEL,
    };
    use serde_json::{json, Value};

    fn stored_array(docs: &[Value]) -> Vec<u8> {
        serde_json::to_vec(&Value::Array(docs.to_vec())).unwrap()
    }

    fn vector(i: usize, dims: usize) -> Value {
        Value::Array(
            (0..dims)
                .map(|d| json!(((i * 31 + d * 7) % 1000) as f64 / 997.0 - 0.5))
                .collect(),
        )
    }

    /// Every shape the lazy reader keeps as text or parses, in one batch.
    fn mixed_docs(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| {
                let tag = ["a", "b", "c"][i % 3];
                let mut source = json!({
                    "title": format!("doc {i}"),
                    "n": i,
                    "tag": tag,
                    "emb": vector(i, 16),
                    "same": [1, 2, 3],
                    "twin_a": {"k": i % 5, "v": [i, i + 1]},
                    "twin_b": {"k": i % 5, "v": [i, i + 1]},
                    "mixed": if i % 4 == 0 { json!("one") } else { json!(["x", i]) },
                    "f": i as f64 / 3.0,
                });
                if i % 5 == 0 {
                    source["maybe"] = Value::Null;
                } else if i % 5 == 1 {
                    source["maybe"] = json!([i]);
                }
                if i % 7 == 0 {
                    source["__id"] = json!("shadow");
                }
                json!({"_id": format!("id-{i}"), "_seq_no": i, "_source": source})
            })
            .collect()
    }

    #[test]
    fn lazy_encode_is_byte_identical_to_the_tree_build_2917() {
        let json = stored_array(&mixed_docs(600));
        for long in [None, Some(RAW_LONG_LEVEL)] {
            let lazy = super::encode(&json, STORED_ZSTD_LEVEL, long).expect("lazy path handles it");
            let tree = encode_stored_v2_tree(&json, STORED_ZSTD_LEVEL, long);
            assert_eq!(lazy, tree, "long effort {long:?}");
        }
        // The public entry point takes the lazy path and decodes back to the
        // same documents.
        let encoded = encode_stored_v2_at_level(&json, STORED_ZSTD_LEVEL);
        let decoded: Value = serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        let mut original: Value = serde_json::from_slice(&json).unwrap();
        // A `_source` key named `__id` is not stored by either build (the
        // synthetic column owns that name) — pre-existing, not #2917.
        for doc in original.as_array_mut().unwrap() {
            doc["_source"].as_object_mut().unwrap().shift_remove("__id");
        }
        assert_eq!(decoded, original);
    }

    #[test]
    fn non_canonical_row_text_still_matches_the_tree_build_2917() {
        // Whitespace and exponent forms the tree build normalises.
        let mut text = String::from("[");
        for i in 0..200 {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!(
                r#"{{"_id":"d{i}","_seq_no":{i},"_source":{{"v":[ 1.0e2 , {i} ,-0.0],"o":{{ "a" : [ ] }},"s":"x"}}}}"#
            ));
        }
        text.push(']');
        let lazy = super::encode(text.as_bytes(), STORED_ZSTD_LEVEL, None).unwrap();
        let tree = encode_stored_v2_tree(text.as_bytes(), STORED_ZSTD_LEVEL, None);
        assert_eq!(lazy, tree);
    }

    #[test]
    fn source_shapes_without_fields_keep_their_rows_2917() {
        let docs: Vec<Value> = (0..300)
            .map(|i| match i % 4 {
                0 => json!({"_id": format!("{i}"), "_seq_no": i}),
                1 => json!({"_id": format!("{i}"), "_seq_no": i, "_source": "not-an-object"}),
                2 => json!({"_id": format!("{i}"), "_seq_no": i, "_source": null}),
                _ => json!({"_id": format!("{i}"), "_seq_no": i, "_source": {"v": [i, 1]}}),
            })
            .collect();
        let json = stored_array(&docs);
        let lazy = super::encode(&json, STORED_ZSTD_LEVEL, None).unwrap();
        assert_eq!(lazy, encode_stored_v2_tree(&json, STORED_ZSTD_LEVEL, None));
    }

    #[test]
    fn passage_metadata_rows_go_through_the_tree_strip_2917() {
        let docs: Vec<Value> = (0..200)
            .map(|i| {
                json!({"_id": format!("{i}"), "_seq_no": i, "_source": {
                    "body": format!("text {i}"),
                    "__xerj_passage_meta__body_vec": {"field": "body"},
                    "body_vec": vector(i, 8),
                }})
            })
            .collect();
        let json = stored_array(&docs);
        let lazy = super::encode(&json, STORED_ZSTD_LEVEL, None).unwrap();
        assert_eq!(lazy, encode_stored_v2_tree(&json, STORED_ZSTD_LEVEL, None));
    }

    #[test]
    fn shapes_the_lazy_reader_does_not_model_fall_back_2917() {
        // Too few rows for the columnar form.
        assert!(super::encode(&stored_array(&mixed_docs(10)), STORED_ZSTD_LEVEL, None).is_none());
        // A duplicate `_source` key.
        let mut text = String::from("[");
        for i in 0..200 {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!(
                r#"{{"_id":"d{i}","_seq_no":{i},"_source":{{"a":[1],"a":[2]}}}}"#
            ));
        }
        text.push(']');
        assert!(super::encode(text.as_bytes(), STORED_ZSTD_LEVEL, None).is_none());
        // ...and the public entry point still encodes it (tree build).
        let encoded = encode_stored_v2_at_level(text.as_bytes(), STORED_ZSTD_LEVEL);
        let decoded: Value = serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        assert_eq!(decoded[0]["_source"]["a"], json!([2]));
        // A top-level element that is not an object.
        let mut docs = mixed_docs(200);
        docs.push(json!(42));
        assert!(super::encode(&stored_array(&docs), STORED_ZSTD_LEVEL, None).is_none());
    }

    #[test]
    fn forcemerge_encode_of_a_vector_column_round_trips_2917() {
        let docs: Vec<Value> = (0..400)
            .map(|i| {
                json!({"_id": format!("{i}"), "_seq_no": i, "_source": {"emb": vector(i, 64), "t": "x"}})
            })
            .collect();
        let json = stored_array(&docs);
        let encoded = encode_stored_v2_forcemerge_at_level(&json, STORED_ZSTD_LEVEL);
        assert_eq!(
            encoded,
            encode_stored_v2_tree(&json, STORED_ZSTD_LEVEL, Some(RAW_LONG_LEVEL))
        );
        let decoded: Value = serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        assert_eq!(decoded, Value::Array(docs));
    }

    /// The row-text decode must give the same bytes as the `Value` decode,
    /// for every column codec a segment can carry: present nulls (ZBS3),
    /// COPY_OF of a JSON column, LZ4/zstd JSON, dictionaries, typed ints,
    /// constants and cross-dep columns.
    #[test]
    fn row_text_decode_is_byte_identical_to_the_value_decode_2917() {
        use super::super::{decode_stored_via_values, encode_stored_v2_from_values};
        let mut docs = mixed_docs(700);
        for (i, doc) in docs.iter_mut().enumerate() {
            let source = doc["_source"].as_object_mut().unwrap();
            source.insert("grp".into(), json!(i % 5));
            source.insert("grp_x7".into(), json!((i % 5) * 7));
            source.insert("const_arr".into(), json!([{"a": 1}]));
            source.insert("esc".into(), json!(format!("line\n\"q\" é {i}")));
        }
        let json = stored_array(&docs);
        let parts: Vec<(String, u64, Value)> = docs
            .iter()
            .map(|d| {
                (
                    d["_id"].as_str().unwrap().to_string(),
                    d["_seq_no"].as_u64().unwrap(),
                    d["_source"].clone(),
                )
            })
            .collect();
        let refs: Vec<(&str, u64, &Value)> = parts
            .iter()
            .map(|(id, seq, src)| (id.as_str(), *seq, src))
            .collect();
        for encoded in [
            encode_stored_v2_at_level(&json, STORED_ZSTD_LEVEL),
            encode_stored_v2_forcemerge_at_level(&json, STORED_ZSTD_LEVEL),
            encode_stored_v2_tree(&json, STORED_ZSTD_LEVEL, None),
            encode_stored_v2_from_values(&json, &refs),
        ] {
            let text = decode_stored(&encoded).unwrap();
            let values = decode_stored_via_values(&encoded).unwrap();
            assert_eq!(
                String::from_utf8(text).unwrap(),
                String::from_utf8(values).unwrap()
            );
        }
    }

    #[test]
    fn long_effort_level_steps_down_on_large_columns_2917() {
        const MIB: usize = 1024 * 1024;
        assert_eq!(long_effort_level_for_len(MIB, 19), 19);
        assert_eq!(long_effort_level_for_len(16 * MIB, 19), 19);
        assert_eq!(long_effort_level_for_len(16 * MIB + 1, 19), 12);
        assert_eq!(long_effort_level_for_len(128 * MIB, 19), 12);
        assert_eq!(long_effort_level_for_len(128 * MIB + 1, 19), 9);
        // Never raises a lower request.
        assert_eq!(long_effort_level_for_len(512 * MIB, 5), 5);
    }
}
