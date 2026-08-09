//! YAML — multi-document streams; each doc becomes a record (a top-level
//! sequence of mappings becomes per-element records). 16MB file cap.

use super::{flatten_object, yaml_to_json, ExtractStats, FieldOrigin, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

const YAML_CAP: u64 = 16 << 20;

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, YAML_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    let mut any = false;
    for (d, de) in serde_yaml::Deserializer::from_str(&text).enumerate() {
        let yv: serde_yaml::Value = match serde_yaml::Value::deserialize(de) {
            Ok(v) => v,
            Err(_) => {
                // MUST break, never continue: serde_yaml's multi-document
                // iterator does not advance past a document that fails to
                // parse — `continue` re-yields the same broken document
                // forever (live hang: a markdown task list `- [ ] …` sniffed
                // as YAML spun autoindex phase A indefinitely). Junk-file the
                // rest of the stream instead; the loss is recorded, not
                // hidden.
                stats.junk += 1;
                break;
            }
        };
        any = true;
        let jv = yaml_to_json(yv);
        match jv {
            Value::Array(arr) if arr.iter().all(|e| e.is_object()) && !arr.is_empty() => {
                for (i, el) in arr.into_iter().enumerate() {
                    if let Value::Object(m) = el {
                        stats.records += 1;
                        if !sink(RawRecord {
                            fields: flatten_object(m),
                            locator: format!("d{d}e{i}"),
                            group: None,
                            origin: FieldOrigin::Data,
                        }) {
                            return Ok(stats);
                        }
                    }
                }
            }
            Value::Object(m) => {
                stats.records += 1;
                if !sink(RawRecord {
                    fields: flatten_object(m),
                    locator: format!("d{d}"),
                    group: None,
                    origin: FieldOrigin::Data,
                }) {
                    return Ok(stats);
                }
            }
            other => {
                let mut m = Map::new();
                m.insert("value".into(), other);
                stats.records += 1;
                if !sink(RawRecord {
                    fields: m,
                    locator: format!("d{d}"),
                    group: None,
                    origin: FieldOrigin::Data,
                }) {
                    return Ok(stats);
                }
            }
        }
    }
    if !any && stats.junk == 0 {
        stats.junk += 1;
    }
    Ok(stats)
}

use serde::Deserialize;

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_str(text: &str) -> (ExtractStats, usize) {
        let dir = std::env::temp_dir().join(format!(
            "xerj-yamlx-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.yaml");
        std::fs::write(&path, text).unwrap();
        let mut n = 0usize;
        let stats = extract(&path, false, &mut |_r| {
            n += 1;
            true
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        (stats, n)
    }

    /// Regression for a live hang: serde_yaml's multi-document iterator does
    /// not advance past a document that fails to parse, so `continue` on the
    /// error re-yielded the same broken document forever. A markdown task
    /// list sniffed as YAML (`- [ ] …` is invalid YAML) spun autoindex phase
    /// A indefinitely. The extractor must terminate and junk-file instead.
    #[test]
    fn broken_document_terminates_as_junk_not_forever() {
        let (stats, n) = extract_str("# Launch checklist\n- [ ] alpha\n- [ ] beta\n");
        assert_eq!(n, 0, "invalid YAML must not fabricate records");
        assert!(stats.junk >= 1, "the unparseable stream is junk-filed");
    }

    #[test]
    fn valid_multi_document_stream_still_yields_records() {
        let (stats, n) = extract_str("a: 1\nb: two\n---\nc: 3\n");
        assert_eq!(n, 2);
        assert_eq!(stats.records, 2);
        assert_eq!(stats.junk, 0);
    }
}
