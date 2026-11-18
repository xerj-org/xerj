//! Keep embedding vectors out of search tool responses (#3272).
//!
//! A `semantic_text` field writes its embedding into `_source` beside the
//! text (`body_vector`, and `body_vector_chunks[].vector`), and a
//! `dense_vector` field is a float array there too. Returned whole, a few
//! hits of 384 floats each used up an agent's context on its first call.
//! Every search tool now drops them from `_source` unless the caller passes
//! `include_vectors: true`, and says which fields it dropped.
//!
//! Detection is by shape, not by mapping, so it needs no extra request and
//! also covers a pattern whose indices map the same field differently: an
//! array of at least [`MIN_DIMS`] numbers with at least one fractional value,
//! or an array made only of such arrays. Integer lists (line numbers, ids)
//! are left alone.

use serde_json::Value;

/// Shortest numeric array treated as an embedding.
pub(crate) const MIN_DIMS: usize = 8;

/// `_xerj.hints` code for the note listing dropped fields.
pub(crate) const HINT_CODE: &str = "vectors_omitted";

/// The `include_vectors` argument: absent or null is `false`.
pub(crate) fn opt_include_vectors(args: &Value) -> Result<bool, String> {
    match args.get("include_vectors") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(
            "`include_vectors` must be a boolean, for example {\"include_vectors\": true}".into(),
        ),
    }
}

/// JSON Schema for `include_vectors`, shared by every search tool.
pub(crate) fn arg_schema() -> Value {
    serde_json::json!({
        "type": "boolean",
        "description": "Return embedding vectors (dense_vector and semantic_text embedding \
         arrays) in `_source`. Default false: they are dropped from every hit and \
         `_xerj.hints` code `vectors_omitted` names the fields."
    })
}

fn is_vector(v: &Value) -> bool {
    let Some(items) = v.as_array() else {
        return false;
    };
    if items.is_empty() {
        return false;
    }
    if items.iter().all(Value::is_number) {
        return items.len() >= MIN_DIMS
            && items
                .iter()
                .any(|n| n.as_f64().is_some_and(|f| f.fract() != 0.0));
    }
    items.iter().all(is_vector)
}

fn strip_object(obj: &mut serde_json::Map<String, Value>, path: &str, dropped: &mut Vec<String>) {
    let keys: Vec<String> = obj.keys().cloned().collect();
    for key in keys {
        let here = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if obj.get(&key).is_some_and(is_vector) {
            obj.remove(&key);
            if !dropped.contains(&here) {
                dropped.push(here);
            }
            continue;
        }
        match obj.get_mut(&key) {
            Some(Value::Object(inner)) => strip_object(inner, &here, dropped),
            Some(Value::Array(items)) => {
                let here = format!("{here}[]");
                for item in items {
                    if let Value::Object(inner) = item {
                        strip_object(inner, &here, dropped);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Drop vectors from every `_source` object anywhere in `resp` (hits,
/// `inner_hits`, batch items). Returns the dropped field paths, in order of
/// first appearance.
pub(crate) fn strip(resp: &mut Value) -> Vec<String> {
    let mut dropped = Vec::new();
    walk(resp, &mut dropped);
    dropped
}

fn walk(v: &mut Value, dropped: &mut Vec<String>) {
    match v {
        Value::Object(obj) => {
            for (key, child) in obj.iter_mut() {
                if key == "_source" {
                    if let Value::Object(source) = child {
                        strip_object(source, "", dropped);
                    }
                } else {
                    walk(child, dropped);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, dropped);
            }
        }
        _ => {}
    }
}

/// Record the dropped fields in `_xerj.hints`.
pub(crate) fn attach_hint(resp: &mut Value, dropped: &[String]) {
    if dropped.is_empty() {
        return;
    }
    crate::push_hint(
        resp,
        HINT_CODE,
        &format!(
            "embedding vectors were dropped from _source ({}); pass include_vectors: true to get them",
            dropped.join(", ")
        ),
    );
}

/// [`strip`] and [`attach_hint`] on a response body. Text that is not JSON,
/// or has nothing to drop, is returned unchanged.
pub(crate) fn strip_text(text: &str) -> String {
    let Ok(mut resp) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let dropped = strip(&mut resp);
    if dropped.is_empty() {
        return text.to_string();
    }
    attach_hint(&mut resp, &dropped);
    serde_json::to_string(&resp).unwrap_or_else(|_| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn floats(n: usize) -> Value {
        json!((0..n).map(|i| i as f64 * 0.01 + 0.005).collect::<Vec<_>>())
    }

    #[test]
    fn vectors_leave_every_source_and_text_stays() {
        let mut resp = json!({"hits":{"hits":[
            {"_id":"1","_source":{
                "body":"text","line":[1,2,3,4,5,6,7,8,9],
                "body_vector": floats(16),
                "body_vector_chunks":[{"text":"t","vector": floats(16)}],
                "multi": [floats(8), floats(8)],
                "short": [0.5, 0.25]
            }},
            {"_id":"2","_source":{"body":"other","body_vector": floats(16)}}
        ]}});
        let dropped = strip(&mut resp);
        assert_eq!(
            dropped,
            vec!["body_vector", "body_vector_chunks[].vector", "multi"]
        );
        let s = &resp["hits"]["hits"][0]["_source"];
        assert_eq!(s["body"], "text");
        assert_eq!(s["line"].as_array().unwrap().len(), 9, "integer lists stay");
        assert_eq!(s["short"].as_array().unwrap().len(), 2, "short arrays stay");
        assert_eq!(s["body_vector_chunks"][0]["text"], "t");
        assert!(resp["hits"]["hits"][1]["_source"]
            .get("body_vector")
            .is_none());
        attach_hint(&mut resp, &dropped);
        assert_eq!(resp["_xerj"]["hints"][0]["code"], HINT_CODE);
    }

    #[test]
    fn text_without_vectors_is_returned_byte_for_byte() {
        let text = r#"{"hits":{"hits":[{"_source":{"a":1}}]}}"#;
        assert_eq!(strip_text(text), text);
        assert_eq!(strip_text("not json"), "not json");
    }

    #[test]
    fn include_vectors_must_be_a_boolean() {
        assert_eq!(opt_include_vectors(&json!({})), Ok(false));
        assert_eq!(
            opt_include_vectors(&json!({"include_vectors": true})),
            Ok(true)
        );
        assert!(opt_include_vectors(&json!({"include_vectors": "yes"})).is_err());
    }
}
