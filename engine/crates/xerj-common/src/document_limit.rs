//! Per-document byte ceiling for indexed JSON (#1122).
//!
//! One bulk item and one `index_document` call tokenize the whole source
//! before the process memory breaker can sample. The FTS `Token` owns a
//! `String` per word. A standalone reproduction of that ownership (not a live
//! `analyze_doc` run), measured 2026-10-23: 8,388,605 bytes of the word
//! `word ` (1,677,721 tokens) grew a `Vec<String>` from 10,160 KB RSS to
//! 101,912 KB RSS — 91,752 KB, about 56 bytes per token. It was not repeated
//! at 50 MB.
//!
//! The ceiling is a fixed constant. `limits` is already 15 settings and the
//! user-facing total is 137; a new knob would have to touch every settings
//! census for a bound that must not be raised by a config edit.

use serde_json::Value;

/// Largest JSON document source that may be parsed and analyzed.
///
/// 8 MiB is under the 50 MB files that killed a node during autoindex, and
/// above a normal record. A larger file is streamed into smaller records or
/// skipped. A single value over this limit is not indexed.
pub const MAX_INDEXED_DOCUMENT_BYTES: usize = 8 << 20;

/// Extra bytes an `_update` envelope may use beyond two documents at the ceiling.
///
/// The body can carry a partial `doc` and an `upsert`, each a full indexed
/// document, plus a script. The engine caps a script source at 64 KiB;
/// JSON escaping can double that, and the object keys sit beside the two
/// bodies. 256 KiB covers that wrapping. A larger raw body is refused
/// before `serde_json` builds a DOM (#1122).
pub const UPDATE_ENVELOPE_SLACK_BYTES: usize = 256 * 1024;

/// Largest raw `POST /{index}/_update/{id}` body that is parsed.
///
/// Bytes past this limit are refused before `serde_json`. A body at or
/// under the limit is still subject to [`MAX_INDEXED_DOCUMENT_BYTES`] on
/// the document the update would write.
pub const MAX_UPDATE_BODY_BYTES: usize =
    2 * MAX_INDEXED_DOCUMENT_BYTES + UPDATE_ENVELOPE_SLACK_BYTES;

/// `true` when an `_update` body is past [`MAX_UPDATE_BODY_BYTES`].
pub fn update_body_too_large(len: usize) -> bool {
    len > MAX_UPDATE_BODY_BYTES
}

/// Refusal text for an `_update` body of `len` bytes.
///
/// The text does not say the body was invalid JSON: the size check runs
/// before parse, including when the bytes are not JSON.
pub fn update_body_refusal(len: usize) -> String {
    format!(
        "update body is {len} bytes, over the {MAX_UPDATE_BODY_BYTES} byte limit; \
         it was not parsed and the existing document was not changed"
    )
}

/// `true` when `len` is past [`MAX_INDEXED_DOCUMENT_BYTES`].
pub fn indexed_document_too_large(len: usize) -> bool {
    len > MAX_INDEXED_DOCUMENT_BYTES
}

/// Refusal text for a document whose raw JSON is `len` bytes.
pub fn indexed_document_refusal(len: usize) -> String {
    format!(
        "document is {len} bytes, over the {MAX_INDEXED_DOCUMENT_BYTES} byte limit for one indexed JSON document; it was not indexed. Split it into smaller documents"
    )
}

/// Refusal text when a walk of an already-parsed value stopped at `seen`.
pub fn indexed_document_refusal_at_least(seen: usize) -> String {
    format!(
        "document is at least {seen} bytes, over the {MAX_INDEXED_DOCUMENT_BYTES} byte limit for one indexed JSON document; it was not indexed. Split it into smaller documents"
    )
}

/// `Some(accounted)` when a walk of `value` passes the limit.
///
/// The walk stops at the first byte past the limit, so `accounted` can be
/// smaller than the full value. Strings contribute their decoded length.
/// Numbers contribute a fixed 8 bytes (serde_json stores them as `i64`,
/// `u64`, or `f64` without `arbitrary_precision`). One byte is charged per
/// array element so a giant array of nulls still trips.
pub fn json_value_exceeds_indexed_document_limit(value: &Value) -> Option<usize> {
    let mut accounted = 0usize;
    if exceeds(value, &mut accounted) {
        Some(accounted)
    } else {
        None
    }
}

fn charge(accounted: &mut usize, n: usize) -> bool {
    *accounted = accounted.saturating_add(n);
    *accounted > MAX_INDEXED_DOCUMENT_BYTES
}

fn exceeds(value: &Value, accounted: &mut usize) -> bool {
    match value {
        Value::Null | Value::Bool(_) => charge(accounted, 4),
        Value::Number(_) => charge(accounted, 8),
        Value::String(text) => charge(accounted, text.len().saturating_add(2)),
        Value::Array(items) => {
            if charge(accounted, 2) {
                return true;
            }
            for item in items {
                if charge(accounted, 1) || exceeds(item, accounted) {
                    return true;
                }
            }
            false
        }
        Value::Object(map) => {
            if charge(accounted, 2) {
                return true;
            }
            for (key, child) in map {
                if charge(accounted, key.len().saturating_add(3)) || exceeds(child, accounted) {
                    return true;
                }
            }
            false
        }
    }
}

/// Name prefix of the passage metadata member the server writes beside each
/// semantic vector. Equal to `xerj_query::executor::PASSAGE_METADATA_PREFIX`.
const PASSAGE_METADATA_PREFIX: &str = "__xerj_passage_meta__";

/// Bytes the server adds to a document's compact JSON when it embeds one
/// `semantic_text` field (#2807).
///
/// The server checks [`MAX_INDEXED_DOCUMENT_BYTES`] after it has added the
/// vectors, so a client that needs to know whether a record fits adds this to
/// the record's own length. `passages` are the UTF-8 byte offsets ingest
/// embeds (`xerj_ai::semantic_lexical::semantic_passage_offsets`), and `dims`
/// is the vector width the server reports. The count follows what
/// `xerj-engine` `apply_semantic_embeddings_batch` inserts: the pooled
/// vector, the passage metadata, and one vector per passage when there is
/// more than one. Every member is charged a separating comma, so the result is
/// one byte high for a document that had no other member. It assumes every
/// vector packs (`packed_f32`); the server writes a vector with a non-finite
/// value as a longer number array, which this does not count.
pub fn semantic_vector_bytes(
    field: &str,
    target: &str,
    dims: usize,
    passages: &[(usize, usize)],
) -> usize {
    if passages.is_empty() {
        return 0;
    }
    // `,"key":value`
    let member = |key: &str, value: usize| 2 + json_string_len(key) + value;
    let packed = crate::packed_f32::PACKED_F32_PREFIX.len() + 8 * dims + 2;
    // `[[start,end],...]`
    let offsets = 1 + passages
        .iter()
        .map(|&(start, end)| 4 + decimal_len(start) + decimal_len(end))
        .sum::<usize>();
    // `{"field":"…","chunks":[…]}`: two braces, and no comma before the first member.
    let metadata = 2 + member("field", json_string_len(field)) + member("chunks", offsets) - 1;
    let mut bytes =
        member(target, packed) + member(&format!("{PASSAGE_METADATA_PREFIX}{target}"), metadata);
    if passages.len() > 1 {
        bytes += member(
            &format!("{target}_chunks"),
            1 + passages.len() * (packed + 1),
        );
    }
    bytes
}

fn json_string_len(text: &str) -> usize {
    serde_json::to_string(text).map_or(text.len() * 6 + 2, |json| json.len())
}

fn decimal_len(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |digits| digits as usize + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_ceiling_is_eight_mebibytes() {
        assert_eq!(MAX_INDEXED_DOCUMENT_BYTES, 8 * 1024 * 1024);
        assert!(!indexed_document_too_large(MAX_INDEXED_DOCUMENT_BYTES));
        assert!(indexed_document_too_large(MAX_INDEXED_DOCUMENT_BYTES + 1));
    }

    #[test]
    fn an_update_envelope_fits_two_documents_and_refuses_one_byte_over() {
        assert_eq!(
            MAX_UPDATE_BODY_BYTES,
            2 * MAX_INDEXED_DOCUMENT_BYTES + 256 * 1024
        );
        assert!(!update_body_too_large(MAX_UPDATE_BODY_BYTES));
        assert!(update_body_too_large(MAX_UPDATE_BODY_BYTES + 1));
        let text = update_body_refusal(MAX_UPDATE_BODY_BYTES + 1);
        assert!(
            text.contains(&(MAX_UPDATE_BODY_BYTES + 1).to_string()),
            "{text}"
        );
        assert!(text.contains(&MAX_UPDATE_BODY_BYTES.to_string()), "{text}");
        assert!(text.contains("not parsed"), "{text}");
        assert!(text.contains("not changed"), "{text}");
        assert!(
            !text.contains("invalid JSON"),
            "the size refusal is not a parse error: {text}"
        );
    }

    #[test]
    fn a_fifty_mebibyte_document_is_refused_before_it_is_indexed() {
        // Field report on #1122: a single JSON file over 50 MB killed the
        // node. The ceiling is 8 MiB, so that file is the same refusal as
        // one byte over the ceiling — checked on the length, before parse.
        let len = 50 * 1024 * 1024;
        assert!(indexed_document_too_large(len));
        let text = indexed_document_refusal(len);
        assert!(text.contains("52428800"), "{text}");
        assert!(
            text.contains(&MAX_INDEXED_DOCUMENT_BYTES.to_string()),
            "{text}"
        );
        assert!(text.contains("not indexed"), "{text}");
    }

    #[test]
    fn refusal_names_the_size_and_the_limit() {
        let text = indexed_document_refusal(50 * 1024 * 1024);
        assert!(text.contains("52428800"));
        assert!(text.contains(&MAX_INDEXED_DOCUMENT_BYTES.to_string()));
        assert!(text.contains("not indexed"));
        let at_least = indexed_document_refusal_at_least(MAX_INDEXED_DOCUMENT_BYTES + 3);
        assert!(at_least.contains("at least"));
        assert!(at_least.contains("not indexed"));
    }

    #[test]
    fn semantic_vector_bytes_matches_what_the_server_inserts() {
        // The members `apply_semantic_embeddings_batch` writes, built the
        // same way. A change to that shape has to change this function too.
        let vector = |seed: f32| vec![seed; 384];
        for passages in [
            vec![(0usize, 7usize)],
            vec![(0, 512), (448, 960), (896, 10_042)],
        ] {
            let mut doc = json!({"title": "t", "body": "text"});
            let object = doc.as_object_mut().unwrap();
            let before = Value::Object(object.clone()).to_string().len();
            let packed =
                |seed| Value::String(crate::packed_f32::pack_f32_vector(&vector(seed)).unwrap());
            object.insert("body_vector".into(), packed(0.5));
            let offsets: Vec<Value> = passages
                .iter()
                .map(|&(start, end)| json!([start, end]))
                .collect();
            object.insert(
                "__xerj_passage_meta__body_vector".into(),
                json!({"field": "body", "chunks": offsets}),
            );
            if passages.len() > 1 {
                let chunks: Vec<Value> = (0..passages.len()).map(|i| packed(i as f32)).collect();
                object.insert("body_vector_chunks".into(), Value::Array(chunks));
            }
            let after = doc.to_string().len();
            assert_eq!(
                semantic_vector_bytes("body", "body_vector", 384, &passages),
                after - before,
                "{passages:?}"
            );
        }
        assert_eq!(semantic_vector_bytes("body", "body_vector", 384, &[]), 0);
    }

    #[test]
    fn a_small_value_is_under_the_limit_and_a_long_string_is_not() {
        assert!(json_value_exceeds_indexed_document_limit(&json!({"id": 1})).is_none());
        let huge = "x".repeat(MAX_INDEXED_DOCUMENT_BYTES + 1);
        let seen = json_value_exceeds_indexed_document_limit(&json!({"blob": huge}))
            .expect("a string past the ceiling must trip");
        assert!(seen > MAX_INDEXED_DOCUMENT_BYTES);
        // The walk stops on that string. It must not report a second full copy.
        assert!(seen < MAX_INDEXED_DOCUMENT_BYTES + 64);
    }
}
