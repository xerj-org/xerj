//! Whole-value JSON files (object or array), capped at 64MB.
//! - array of objects  → one record per element
//! - object with a dominant top-level array of objects → one record per
//!   element, remaining top-level scalars merged in as shared fields
//! - anything else → a single record

use super::{flatten_object, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_WHOLE_FILE};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, MAX_WHOLE_FILE)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            stats.junk += 1;
            return Ok(stats);
        }
    };
    match v {
        Value::Array(arr) => {
            for (i, el) in arr.into_iter().enumerate() {
                if !emit(el, &format!("e{i}"), sink, &mut stats) {
                    break;
                }
            }
        }
        Value::Object(mut obj) => {
            // find the largest top-level array-of-objects
            let mut best: Option<(String, usize)> = None;
            for (k, vv) in obj.iter() {
                if let Value::Array(a) = vv {
                    if a.len() >= 2
                        && a.iter().all(|e| e.is_object())
                        && best.as_ref().map(|(_, n)| a.len() > *n).unwrap_or(true)
                    {
                        best = Some((k.clone(), a.len()));
                    }
                }
            }
            match best {
                Some((key, _)) => {
                    let Value::Array(arr) = obj.remove(&key).unwrap() else {
                        unreachable!()
                    };
                    let shared = flatten_object(obj);
                    for (i, el) in arr.into_iter().enumerate() {
                        if let Value::Object(m) = el {
                            let mut fields = shared.clone();
                            for (k, v) in flatten_object(m) {
                                fields.insert(k, v); // element wins collisions
                            }
                            stats.records += 1;
                            if !sink(RawRecord {
                                fields,
                                locator: format!("{key}:e{i}"),
                                group: None,
                                origin: FieldOrigin::Data,
                            }) {
                                break;
                            }
                        }
                    }
                }
                None => {
                    emit(Value::Object(obj), "doc", sink, &mut stats);
                }
            }
        }
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            emit(Value::Object(m), "doc", sink, &mut stats);
        }
    }
    Ok(stats)
}

fn emit(v: Value, locator: &str, sink: Sink, stats: &mut ExtractStats) -> bool {
    let fields = match v {
        Value::Object(m) => flatten_object(m),
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: locator.to_string(),
        group: None,
        origin: FieldOrigin::Data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    #[test]
    fn an_array_of_objects_becomes_one_record_per_element() {
        let (stats, recs) = run(
            r#"[{"id":1,"user":{"name":"ann","addr":{"city":"NY"}},"tags":["a","b"]},{"id":2}]"#,
        );
        assert_eq!(stats.records, 2);
        assert_eq!(stats.junk, 0);
        assert_eq!(recs[0].locator, "e0");
        assert_eq!(recs[1].locator, "e1");
        assert_eq!(recs[0].fields["id"], serde_json::json!(1));
        assert_eq!(
            recs[0].fields["user_name"],
            serde_json::json!("ann"),
            "two levels of nesting flatten into a_b keys"
        );
        assert_eq!(recs[0].fields["user_addr_city"], serde_json::json!("NY"));
        assert_eq!(
            recs[0].fields["tags"],
            serde_json::json!(["a", "b"]),
            "an array of scalars stays an array"
        );
        assert_eq!(recs[1].fields.len(), 1, "absent keys are not filled in");
    }

    #[test]
    fn a_wrapper_object_lifts_its_largest_array_and_shares_the_outer_scalars() {
        let (stats, recs) = run(r#"{"generated":"2026-01-01","count":2,
                "items":[{"id":1},{"id":2,"generated":"per-item"}],
                "errors":[{"e":1}]}"#);
        assert_eq!(stats.records, 2, "records come from the biggest array");
        assert_eq!(recs[0].locator, "items:e0");
        assert_eq!(recs[0].fields["generated"], serde_json::json!("2026-01-01"));
        assert_eq!(recs[0].fields["count"], serde_json::json!(2));
        assert_eq!(
            recs[1].fields["generated"],
            serde_json::json!("per-item"),
            "the element wins a collision with a shared field"
        );
        assert_eq!(
            recs[0].fields["errors"],
            serde_json::json!(r#"[{"e":1}]"#),
            "the other arrays of objects travel as JSON strings"
        );
    }

    #[test]
    fn an_object_with_no_repeating_array_is_a_single_document_record() {
        let (stats, recs) =
            run(r#"{"meta":"m","nested":{"deep":{"deeper":{"x":1}}},"items":[{"id":1}]}"#);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].locator, "doc");
        assert_eq!(recs[0].fields["meta"], serde_json::json!("m"));
        assert_eq!(
            recs[0].fields["items"],
            serde_json::json!(r#"[{"id":1}]"#),
            "a single-element array is below the record-lifting threshold"
        );
        assert_eq!(
            recs[0].fields["nested_deep_deeper"],
            serde_json::json!(r#"{"x":1}"#),
            "structure past two levels is stored as a JSON string"
        );
    }

    #[test]
    fn a_bare_scalar_document_is_wrapped_in_a_value_field() {
        let (stats, recs) = run("42");
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].locator, "doc");
        assert_eq!(recs[0].fields["value"], serde_json::json!(42));

        let (stats, recs) = run("[1,2,3]");
        assert_eq!(stats.records, 3, "array elements each become a record");
        assert_eq!(recs[2].fields["value"], serde_json::json!(3));
    }

    #[test]
    fn a_data_field_in_the_provenance_namespace_is_renamed_not_dropped() {
        let (_, recs) = run(r#"{"ax_kind":"invoice","ok":1}"#);
        assert_eq!(recs[0].fields["data_ax_kind"], serde_json::json!("invoice"));
        assert!(recs[0].fields.get("ax_kind").is_none());
    }

    #[test]
    fn truncated_json_is_junk_filed_and_emits_nothing() {
        let (stats, recs) = run(r#"{"a":1,"#);
        assert_eq!((stats.records, stats.junk), (0, 1));
        assert!(recs.is_empty());

        let (stats, recs) = run("not json at all");
        assert_eq!((stats.records, stats.junk), (0, 1));
        assert!(recs.is_empty());
    }

    #[test]
    fn an_empty_array_yields_nothing_at_all_without_being_junk() {
        let (stats, recs) = run("[]");
        assert_eq!((stats.records, stats.junk), (0, 0));
        assert!(recs.is_empty());
    }
}
