//! Row changes, their mapping to index operations, and the sink that applies
//! them. The connector knows Postgres; the sink knows the engine; this module
//! is the contract between the two.

use serde_json::{Map, Value};

/// One operation on the index, produced from one row change.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Full document: insert, or an update where every column was sent.
    Index {
        index: String,
        id: String,
        doc: Map<String, Value>,
    },
    /// Partial document: an update where some TOASTed columns were unchanged
    /// and therefore not sent by the server. Merged into the stored document.
    Update {
        index: String,
        id: String,
        doc: Map<String, Value>,
    },
    Delete {
        index: String,
        id: String,
    },
}

/// Where operations land. The node implements it in-process
/// (`xerj-api/src/sources_api.rs`), tests implement it in memory.
pub trait Sink: Send + Sync {
    /// Apply a batch. Must be idempotent: a batch may be re-applied after a
    /// crash, because the LSN is confirmed only after this returns `Ok`.
    fn apply(&self, ops: &[Op]) -> anyhow::Result<()>;
    /// Remove every document of an index (TRUNCATE, or a fresh snapshot).
    fn clear_index(&self, index: &str) -> anyhow::Result<()>;
}

/// One applied row change, offered to the facts hook (lane A: rows to facts).
/// `key` is the primary key as `column -> text` and is stable for the row's
/// lifetime, so it can be the fact's `subject_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct RowChange {
    pub source: String,
    pub schema: String,
    pub table: String,
    pub kind: RowChangeKind,
    pub key: Vec<(String, String)>,
    /// New column values (absent for a delete). Unchanged TOAST columns are
    /// absent too.
    pub row: Map<String, Value>,
    /// Commit LSN of the transaction, for idempotent replay on the facts side.
    pub lsn: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowChangeKind {
    Insert,
    Update,
    Delete,
}

/// Observer for row changes of sources that set `facts_namespace`.
pub trait FactsObserver: Send + Sync {
    fn on_changes(&self, namespace: &str, changes: &[RowChange]) -> anyhow::Result<()>;
}

/// Document id from primary-key values. Stable across snapshot and stream,
/// so re-applying any change is idempotent. Values are joined with `|`; a `|`
/// or `\` inside a value is escaped so two keys never collide.
pub fn doc_id(key: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (_, v)) in key.iter().enumerate() {
        if i > 0 {
            out.push('|');
        }
        for c in v.chars() {
            if c == '|' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out
}

/// Postgres type OIDs the mapper treats specially.
pub mod oid {
    pub const BOOL: u32 = 16;
    pub const BYTEA: u32 = 17;
    pub const INT8: u32 = 20;
    pub const INT2: u32 = 21;
    pub const INT4: u32 = 23;
    pub const OID: u32 = 26;
    pub const JSON: u32 = 114;
    pub const FLOAT4: u32 = 700;
    pub const FLOAT8: u32 = 701;
    pub const NUMERIC: u32 = 1700;
    pub const TIMESTAMP: u32 = 1114;
    pub const TIMESTAMPTZ: u32 = 1184;
    pub const JSONB: u32 = 3802;
}

/// A column value as Postgres sent it in text form, typed for the index.
/// `None` means the column is skipped (binary payloads have no text to index).
pub fn pg_value(type_oid: u32, text: &str) -> Option<Value> {
    use oid::*;
    match type_oid {
        BOOL => Some(Value::Bool(text == "t" || text == "true")),
        INT2 | INT4 | INT8 | OID => Some(
            text.parse::<i64>()
                .map(|n| Value::Number(n.into()))
                .unwrap_or_else(|_| Value::String(text.into())),
        ),
        FLOAT4 | FLOAT8 | NUMERIC => Some(
            text.parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or_else(|| Value::String(text.into())),
        ),
        JSON | JSONB => {
            Some(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into())))
        }
        // `2026-11-05 10:00:00+00` -> `2026-11-05T10:00:00+00:00`, the form
        // date detection recognises.
        TIMESTAMP | TIMESTAMPTZ => Some(Value::String(iso_timestamp(text))),
        BYTEA => None,
        _ => Some(Value::String(text.into())),
    }
}

fn iso_timestamp(t: &str) -> String {
    let mut s = t.replacen(' ', "T", 1);
    // A bare `+HH` offset is valid Postgres output but not RFC 3339.
    if let Some(pos) = s.rfind(['+', '-']) {
        if pos > 10 {
            let off = &s[pos + 1..];
            if off.len() == 2 && off.bytes().all(|b| b.is_ascii_digit()) {
                s.push_str(":00");
            }
        }
    }
    s
}

/// Undo COPY text-format escaping. `\N` (the whole field) is NULL and is
/// handled by the caller.
pub fn copy_unescape(field: &[u8]) -> String {
    if !field.contains(&b'\\') {
        return String::from_utf8_lossy(field).into_owned();
    }
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let b = field[i];
        if b != b'\\' || i + 1 >= field.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let n = field[i + 1];
        i += 2;
        match n {
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = (n - b'0') as u32;
                let mut k = 0;
                while k < 2 && i < field.len() && (b'0'..=b'7').contains(&field[i]) {
                    v = v * 8 + (field[i] - b'0') as u32;
                    i += 1;
                    k += 1;
                }
                out.push(v as u8);
            }
            b'x' => {
                let mut v = 0u32;
                let mut k = 0;
                while k < 2 && i < field.len() && field[i].is_ascii_hexdigit() {
                    v = v * 16 + (field[i] as char).to_digit(16).unwrap();
                    i += 1;
                    k += 1;
                }
                out.push(v as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Render ops as an `_bulk` NDJSON body.
pub fn bulk_body(ops: &[Op]) -> String {
    let mut s = String::new();
    for op in ops {
        match op {
            Op::Index { index, id, doc } => {
                s.push_str(&serde_json::json!({"index": {"_index": index, "_id": id}}).to_string());
                s.push('\n');
                s.push_str(&Value::Object(doc.clone()).to_string());
            }
            Op::Update { index, id, doc } => {
                s.push_str(
                    &serde_json::json!({"update": {"_index": index, "_id": id}}).to_string(),
                );
                s.push('\n');
                s.push_str(&serde_json::json!({"doc": doc, "doc_as_upsert": true}).to_string());
            }
            Op::Delete { index, id } => {
                s.push_str(
                    &serde_json::json!({"delete": {"_index": index, "_id": id}}).to_string(),
                );
            }
        }
        s.push('\n');
    }
    s
}

/// The first real failure in an `_bulk` response. A delete of a document that
/// is not there is not a failure: re-applying a delete after a crash is the
/// idempotent replay this design relies on.
pub fn bulk_failure(resp: &Value) -> Option<String> {
    if resp.get("errors").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    for item in resp.get("items")?.as_array()? {
        let Some((action, r)) = item.as_object().and_then(|o| o.iter().next()) else {
            continue;
        };
        let status = r.get("status").and_then(Value::as_u64).unwrap_or(0);
        if action == "delete" && status == 404 {
            continue;
        }
        if status >= 300 {
            return Some(format!(
                "{action} {}/{}: {}",
                r.get("_index").and_then(Value::as_str).unwrap_or("?"),
                r.get("_id").and_then(Value::as_str).unwrap_or("?"),
                r.get("error").map(|e| e.to_string()).unwrap_or_default()
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn doc_ids_are_stable_and_composite_keys_never_collide() {
        let k = |v: &[&str]| -> Vec<(String, String)> {
            v.iter().map(|s| ("c".to_string(), s.to_string())).collect()
        };
        assert_eq!(doc_id(&k(&["42"])), "42");
        assert_eq!(doc_id(&k(&["a", "b"])), "a|b");
        assert_ne!(doc_id(&k(&["a|b"])), doc_id(&k(&["a", "b"])));
        assert_ne!(doc_id(&k(&["a\\", "b"])), doc_id(&k(&["a", "\\b"])));
    }

    #[test]
    fn values_are_typed_by_column_type() {
        assert_eq!(pg_value(oid::INT4, "7"), Some(json!(7)));
        assert_eq!(pg_value(oid::NUMERIC, "12.50"), Some(json!(12.5)));
        assert_eq!(pg_value(oid::NUMERIC, "NaN"), Some(json!("NaN")));
        assert_eq!(pg_value(oid::BOOL, "t"), Some(json!(true)));
        assert_eq!(pg_value(oid::JSONB, r#"{"a":1}"#), Some(json!({"a":1})));
        assert_eq!(
            pg_value(oid::TIMESTAMPTZ, "2026-11-05 10:00:00.5+00"),
            Some(json!("2026-11-05T10:00:00.5+00:00"))
        );
        assert_eq!(pg_value(oid::BYTEA, "\\x00"), None);
        assert_eq!(pg_value(25, "hello"), Some(json!("hello")));
    }

    #[test]
    fn copy_text_escapes_are_undone() {
        assert_eq!(copy_unescape(b"a\\tb\\nc\\\\d"), "a\tb\nc\\d");
        assert_eq!(copy_unescape(b"\\101\\x42"), "AB");
        assert_eq!(copy_unescape(b"plain"), "plain");
    }

    #[test]
    fn bulk_failures_ignore_deletes_of_missing_documents() {
        let ok = json!({"errors": true, "items": [{"delete": {"_index": "i", "_id": "1", "status": 404}}]});
        assert_eq!(bulk_failure(&ok), None);
        let bad = json!({"errors": true, "items": [{"index": {"_index": "i", "_id": "1", "status": 400, "error": {"type": "x"}}}]});
        assert!(bulk_failure(&bad).unwrap().contains("index i/1"));
    }

    #[test]
    fn the_bulk_body_has_one_action_line_per_op() {
        let mut d = Map::new();
        d.insert("a".into(), json!(1));
        let body = bulk_body(&[
            Op::Index {
                index: "i".into(),
                id: "1".into(),
                doc: d.clone(),
            },
            Op::Update {
                index: "i".into(),
                id: "1".into(),
                doc: d,
            },
            Op::Delete {
                index: "i".into(),
                id: "2".into(),
            },
        ]);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[3].contains("doc_as_upsert"));
        assert!(lines[4].starts_with(r#"{"delete""#));
    }
}
