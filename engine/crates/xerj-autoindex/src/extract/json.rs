//! Whole-value JSON files (object or array), capped at 64MB.
//! - array of objects  → one record per element
//! - object with a dominant top-level array of objects → one record per
//!   element, remaining top-level scalars merged in as shared fields
//! - anything else → a single record

use super::{flatten_object, ExtractStats, RawRecord, Sink, MAX_WHOLE_FILE};
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
                    if a.len() >= 2 && a.iter().all(|e| e.is_object()) {
                        if best.as_ref().map(|(_, n)| a.len() > *n).unwrap_or(true) {
                            best = Some((k.clone(), a.len()));
                        }
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
    })
}
