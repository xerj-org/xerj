//! SQLite databases — read-only immutable open (WAL/journal never touched);
//! one dataset (group) per table; locator = rowid where available.

use super::{sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink};
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
            for (i, name) in col_names.iter().enumerate().take(ncols).skip(start_col) {
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
                origin: FieldOrigin::Data,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real database with the bundled driver the extractor itself uses.
    fn db(dir: &tempfile::TempDir, name: &str, sql: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(sql).unwrap();
        path
    }

    fn run(path: &Path, per_table_limit: Option<u64>) -> (ExtractStats, Vec<RawRecord>) {
        let mut recs = Vec::new();
        let stats = extract(path, per_table_limit, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    #[test]
    fn every_row_of_every_table_is_a_record_grouped_and_located_by_table_and_rowid() {
        let dir = tempfile::tempdir().unwrap();
        let path = db(
            &dir,
            "shop.db",
            r#"CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT);
               INSERT INTO authors VALUES (1,'Ann'),(2,'Bob');
               CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT);
               INSERT INTO books VALUES (1,'First');"#,
        );
        let (stats, recs) = run(&path, None);
        assert_eq!(stats.records, 3);
        assert_eq!(stats.junk, 0);
        assert_eq!(
            recs.iter()
                .map(|r| (r.group.clone().unwrap(), r.locator.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("authors".into(), "tauthors:r1".to_string()),
                ("authors".into(), "tauthors:r2".to_string()),
                ("books".into(), "tbooks:r1".to_string()),
            ],
            "tables are walked in name order and each is its own dataset"
        );
        assert_eq!(recs[0].fields["name"], serde_json::json!("Ann"));
        assert!(
            recs[0].fields.get("rowid").is_none(),
            "the rowid drives the locator and is never a field"
        );
    }

    #[test]
    fn column_names_are_sanitized_and_null_and_blob_cells_are_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = db(
            &dir,
            "types.db",
            r#"CREATE TABLE t (id INTEGER PRIMARY KEY, "full name" TEXT, score REAL,
                               note TEXT, pic BLOB);
               INSERT INTO t VALUES (1,'Ann Lee',9.5,NULL,x'0102');
               INSERT INTO t VALUES (2,'Bob Ray',7.25,'hi',NULL);"#,
        );
        let (_, recs) = run(&path, None);
        assert_eq!(recs[0].fields["full_name"], serde_json::json!("Ann Lee"));
        assert_eq!(recs[0].fields["id"], serde_json::json!(1));
        assert_eq!(recs[0].fields["score"], serde_json::json!(9.5));
        assert!(
            recs[0].fields.get("note").is_none(),
            "a NULL cell is absent, not empty"
        );
        assert!(
            recs[0].fields.get("pic").is_none(),
            "a BLOB carries no text to index"
        );
        assert_eq!(recs[1].fields["note"], serde_json::json!("hi"));
    }

    #[test]
    fn a_table_without_a_rowid_falls_back_to_ordinal_locators() {
        let dir = tempfile::tempdir().unwrap();
        let path = db(
            &dir,
            "norowid.db",
            r#"CREATE TABLE books (isbn TEXT PRIMARY KEY, title TEXT) WITHOUT ROWID;
               INSERT INTO books VALUES ('111','First'),('222','Second');"#,
        );
        let (stats, recs) = run(&path, None);
        assert_eq!(stats.records, 2);
        assert_eq!(
            recs.iter().map(|r| r.locator.as_str()).collect::<Vec<_>>(),
            ["tbooks:o0", "tbooks:o1"]
        );
        assert_eq!(recs[0].fields["isbn"], serde_json::json!("111"));
    }

    #[test]
    fn views_and_sqlite_internal_tables_are_never_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let path = db(
            &dir,
            "views.db",
            r#"CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT);
               INSERT INTO t (v) VALUES ('a'),('b');
               CREATE VIEW v_t AS SELECT * FROM t;
               CREATE TABLE blank (x INTEGER);"#,
        );
        let (stats, recs) = run(&path, None);
        assert_eq!(stats.records, 2, "an empty table contributes no records");
        assert!(
            recs.iter().all(|r| r.group.as_deref() == Some("t")),
            "{recs:?}"
        );
    }

    #[test]
    fn the_row_limit_applies_to_each_table_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = db(
            &dir,
            "sample.db",
            r#"CREATE TABLE a (id INTEGER PRIMARY KEY);
               INSERT INTO a VALUES (1),(2),(3);
               CREATE TABLE b (id INTEGER PRIMARY KEY);
               INSERT INTO b VALUES (1),(2),(3);"#,
        );
        let (stats, recs) = run(&path, Some(1));
        assert_eq!(stats.records, 2, "one row sampled from each of two tables");
        assert_eq!(
            recs.iter()
                .map(|r| r.group.clone().unwrap())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn a_zero_byte_database_yields_no_records_and_no_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.db");
        std::fs::write(&path, b"").unwrap();
        let (stats, recs) = run(&path, None);
        assert_eq!((stats.records, stats.junk), (0, 0));
        assert!(recs.is_empty());
    }

    /// Unlike every text extractor, a corrupt database is a hard error rather
    /// than a junk count — there is no partial parse to salvage. The caller
    /// junk-files the file with this message, so the failure is recorded and
    /// never fatal, but nothing is extracted from the readable pages.
    #[test]
    fn a_corrupt_database_fails_the_file_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let good = db(
            &dir,
            "good.db",
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t VALUES (1,'a');",
        );
        let bytes = std::fs::read(&good).unwrap();
        let path = dir.path().join("corrupt.db");
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

        let mut seen = 0usize;
        let err = extract(&path, None, &mut |_r| {
            seen += 1;
            true
        })
        .unwrap_err();
        assert_eq!(seen, 0);
        assert!(
            err.to_string().contains("malformed") || err.to_string().contains("not a database"),
            "unexpected error: {err}"
        );
    }
}
