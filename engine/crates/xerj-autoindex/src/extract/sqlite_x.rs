//! SQLite databases — read-only immutable open (WAL/journal never touched);
//! one dataset (group) per table; locator = rowid where available.

use super::{sanitize_field_name, ExtractStats, RawRecord, Sink};
use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use std::path::Path;

pub fn extract(path: &Path, per_table_limit: Option<u64>, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let uri = format!(
        "file:{}?immutable=1&mode=ro",
        path.to_string_lossy().replace('?', "%3f")
    );
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .context("open sqlite (read-only immutable)")?;

    let tables: Vec<String> = {
        let mut st = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.flatten().collect()
    };

    'tables: for table in tables {
        let quoted = format!("\"{}\"", table.replace('"', "\"\""));
        // rowid may not exist (WITHOUT ROWID) — fall back to ordinal.
        let (sql, has_rowid) = (format!("SELECT rowid, * FROM {quoted}"), true);
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => (s, has_rowid),
            Err(_) => match conn.prepare(&format!("SELECT * FROM {quoted}")) {
                Ok(s) => (s, false),
                Err(_) => {
                    stats.junk += 1;
                    continue;
                }
            },
        };
        let has_rowid = stmt.1;
        let stmt = &mut stmt.0;
        let col_names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|c| sanitize_field_name(c))
            .collect();
        let ncols = col_names.len();
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(_) => {
                stats.junk += 1;
                continue;
            }
        };
        let mut ordinal: u64 = 0;
        let mut emitted: u64 = 0;
        while let Ok(Some(row)) = rows.next() {
            let start_col = if has_rowid { 1 } else { 0 };
            let rowid: Option<i64> = if has_rowid { row.get(0).ok() } else { None };
            let mut fields = Map::new();
            for i in start_col..ncols {
                let name = &col_names[i];
                let v = match row.get_ref(i) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v {
                    ValueRef::Null => {}
                    ValueRef::Integer(n) => {
                        fields.insert(name.clone(), Value::Number(n.into()));
                    }
                    ValueRef::Real(f) => {
                        if let Some(n) = serde_json::Number::from_f64(f) {
                            fields.insert(name.clone(), Value::Number(n));
                        }
                    }
                    ValueRef::Text(t) => {
                        fields.insert(
                            name.clone(),
                            Value::String(String::from_utf8_lossy(t).to_string()),
                        );
                    }
                    ValueRef::Blob(_) => {} // skipped, non-text payload
                }
            }
            let loc = match rowid {
                Some(r) => format!("t{table}:r{r}"),
                None => format!("t{table}:o{ordinal}"),
            };
            ordinal += 1;
            if fields.is_empty() {
                continue;
            }
            stats.records += 1;
            emitted += 1;
            if !sink(RawRecord {
                fields,
                locator: loc,
                group: Some(table.clone()),
            }) {
                break 'tables;
            }
            if let Some(lim) = per_table_limit {
                if emitted >= lim {
                    break;
                }
            }
        }
    }
    Ok(stats)
}
