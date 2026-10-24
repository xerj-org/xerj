//! SQL source — schema, migrations, and routines — indexed as code.
//!
//! `.sql` is also the extension of a data dump, so it is not a tree-sitter
//! registry row: claiming the extension would run before the dump sniff and
//! steal `INSERT`/`COPY` files (#295, #2337). Symbols are the names on
//! `CREATE` statements. A file the row parser does not read (Postgres
//! `COPY … FROM stdin`, or an `INSERT` the dump lexer skips) falls through
//! here from [`super::sqldump`]; schema in that file is still indexed, and a
//! file with no schema is refused with [`DATA_ONLY_DUMP_REASON`].

use super::{ExtractStats, FieldOrigin, RawRecord, Sink};
use crate::sniff::{sql_dump_has_rows, SqlLex};
use anyhow::Result;
use serde_json::{Map, Value};
use std::io::Read;
use std::path::Path;

/// Stored file body. Past this, row data is not kept and the body is the
/// DDL text only — a multi-gigabyte `COPY` must not become one document.
const BODY_CAP: usize = 2 << 20;
/// One statement is buffered only up to here. A bulk `INSERT` is classified
/// from its head and the rest is discarded.
const STMT_CAP: usize = 64 << 10;
const CODE_CHARS: usize = 400;
const IMPL_CAP: usize = 16 << 10;

/// `scan_file` prints this instead of "no records extracted" when a `.sql`
/// file is row data and nothing in it is schema.
pub const DATA_ONLY_DUMP_REASON: &str = "SQL data-only dump: no schema statements to index; COPY ... FROM stdin row data is skipped, and INSERT rows are indexed only when the statement parses";

struct SqlSym {
    name: String,
    kind: &'static str,
    line: usize,
    end_line: usize,
    code: String,
    impl_text: String,
}

enum Phase {
    Sql,
    Copy { at_bol: bool, idx: usize },
}

struct ScanOut {
    symbols: Vec<SqlSym>,
    saw_row_data: bool,
    /// Full decoded text when the file fits in [`BODY_CAP`], otherwise the
    /// DDL statements only.
    body: String,
}

pub fn extract(path: &Path, logical: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let reader = super::open_reader(path, gzip, None)?;
    let scan = scan_sql(reader)?;
    if scan.symbols.is_empty() && scan.saw_row_data {
        return Ok(ExtractStats::default());
    }
    if scan.body.trim().is_empty() && scan.symbols.is_empty() {
        return Ok(ExtractStats::default());
    }
    Ok(emit(logical, &scan, sink))
}

fn scan_sql(mut reader: impl Read) -> Result<ScanOut> {
    let mut lex = SqlLex::default();
    let mut phase = Phase::Sql;
    let mut full: Vec<u8> = Vec::new();
    let mut keep_full = true;
    let mut stmt: Vec<u8> = Vec::new();
    let mut keep_stmt = true;
    let mut line: usize = 1;
    let mut stmt_line: usize = 1;
    let mut stmt_end_line: usize = 1;
    let mut symbols = Vec::new();
    let mut saw_row_data = false;
    let mut ddl = String::new();
    let mut buf = [0u8; 64 << 10];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if keep_full {
                if full.len() >= BODY_CAP {
                    keep_full = false;
                    full.clear();
                } else {
                    full.push(b);
                }
            }
            if let Phase::Copy { at_bol, idx } = &mut phase {
                if b == b'\n' {
                    line += 1;
                }
                if copy_terminator(b, at_bol, idx) {
                    phase = Phase::Sql;
                }
                continue;
            }

            if stmt.is_empty() && b.is_ascii_whitespace() {
                if b == b'\n' {
                    line += 1;
                }
                let mut ignored = [0u8; 2];
                let _ = lex.feed(b, &mut ignored);
                continue;
            }
            if stmt.is_empty() {
                stmt_line = line;
                stmt_end_line = line;
                keep_stmt = true;
            }
            if keep_stmt {
                if stmt.len() >= STMT_CAP {
                    keep_stmt = false;
                } else {
                    stmt.push(b);
                }
            }
            if b == b'\n' {
                line += 1;
            } else {
                stmt_end_line = line;
            }
            let mut visible = [0u8; 2];
            let step = lex.feed(b, &mut visible);
            if step.end_stmt {
                if take_statement(
                    &stmt,
                    stmt_line,
                    stmt_end_line,
                    &mut symbols,
                    &mut saw_row_data,
                    &mut ddl,
                ) {
                    phase = Phase::Copy {
                        at_bol: true,
                        idx: 0,
                    };
                }
                stmt.clear();
                keep_stmt = true;
            }
        }
    }
    if !stmt.is_empty() {
        let _ = take_statement(
            &stmt,
            stmt_line,
            stmt_end_line,
            &mut symbols,
            &mut saw_row_data,
            &mut ddl,
        );
    }
    // A `\.` with no trailing newline is still the end of COPY data.
    let body = if keep_full {
        String::from_utf8_lossy(&full).into_owned()
    } else {
        ddl
    };
    Ok(ScanOut {
        symbols,
        saw_row_data,
        body,
    })
}

/// `true` when the statement is `COPY … FROM stdin` and the following bytes
/// are row data, not SQL.
fn take_statement(
    stmt: &[u8],
    line: usize,
    end_line: usize,
    symbols: &mut Vec<SqlSym>,
    saw_row_data: &mut bool,
    ddl: &mut String,
) -> bool {
    let text = String::from_utf8_lossy(stmt);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let head = trim_sql_noise(trimmed);
    if head.is_empty() {
        return false;
    }
    let copy = is_copy_stdin(head);
    let insert = is_insert_values(head);
    if copy || insert {
        *saw_row_data = true;
        return copy;
    }
    if let Some((kind, name)) = match_create(head) {
        let code = cap_chars(trimmed, CODE_CHARS);
        let impl_text = cap_bytes(trimmed, IMPL_CAP);
        symbols.push(SqlSym {
            name,
            kind,
            line,
            end_line: end_line.max(line),
            code,
            impl_text,
        });
    }
    if is_source_stmt(head) {
        append_ddl(ddl, trimmed);
    }
    false
}

fn is_copy_stdin(head: &str) -> bool {
    starts_with_kw(head, "copy") && sql_dump_has_rows(head.as_bytes())
}

fn is_insert_values(head: &str) -> bool {
    starts_with_kw(head, "insert") && sql_dump_has_rows(head.as_bytes())
}

fn is_source_stmt(head: &str) -> bool {
    let Some(word) = first_word(head) else {
        return false;
    };
    matches!(
        word.to_ascii_lowercase().as_str(),
        "create" | "alter" | "drop" | "grant" | "revoke" | "comment" | "do"
    )
}

fn first_word(s: &str) -> Option<&str> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(&s[..end])
    }
}

/// `true` when `b` finishes a `\.` terminator line.
fn copy_terminator(b: u8, at_bol: &mut bool, idx: &mut usize) -> bool {
    const END: &[u8] = b"\\.";
    if *at_bol {
        if *idx < END.len() && b == END[*idx] {
            *idx += 1;
            return false;
        }
        if *idx == END.len() && (b == b'\n' || b == b'\r') {
            *idx = 0;
            *at_bol = true;
            return true;
        }
        *idx = 0;
        if b == b'\n' {
            return false;
        }
        *at_bol = false;
        return false;
    }
    if b == b'\n' {
        *at_bol = true;
        *idx = 0;
    }
    false
}

fn append_ddl(ddl: &mut String, stmt: &str) {
    if ddl.len() >= BODY_CAP {
        return;
    }
    if !ddl.is_empty() {
        ddl.push('\n');
    }
    let room = BODY_CAP.saturating_sub(ddl.len());
    if stmt.len() <= room {
        ddl.push_str(stmt);
    } else {
        let mut cut = room;
        while cut > 0 && !stmt.is_char_boundary(cut) {
            cut -= 1;
        }
        ddl.push_str(&stmt[..cut]);
    }
}

fn trim_sql_noise(s: &str) -> &str {
    let mut s = s.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            if let Some(i) = rest.find('\n') {
                s = rest[i + 1..].trim_start();
                continue;
            }
            return "";
        }
        if let Some(rest) = s.strip_prefix("/*") {
            if let Some(i) = rest.find("*/") {
                s = rest[i + 2..].trim_start();
                continue;
            }
            return "";
        }
        return s;
    }
}

fn starts_with_kw(s: &str, kw: &str) -> bool {
    let s = trim_sql_noise(s);
    if s.len() < kw.len() || !s[..kw.len()].eq_ignore_ascii_case(kw) {
        return false;
    }
    s.as_bytes()
        .get(kw.len())
        .is_none_or(|b| !is_ident_byte(*b))
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

struct Words<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Words<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s: trim_sql_noise(s),
            i: 0,
        }
    }

    fn skip_ws(&mut self) {
        let rest = &self.s[self.i..];
        self.i += rest.len() - rest.trim_start().len();
    }

    fn eat(&mut self, kw: &str) -> bool {
        self.skip_ws();
        let rest = &self.s[self.i..];
        if rest.len() >= kw.len()
            && rest[..kw.len()].eq_ignore_ascii_case(kw)
            && rest
                .as_bytes()
                .get(kw.len())
                .is_none_or(|b| !is_ident_byte(*b))
        {
            self.i += kw.len();
            true
        } else {
            false
        }
    }

    fn rest(&mut self) -> &'a str {
        self.skip_ws();
        &self.s[self.i..]
    }
}

fn match_create(stmt: &str) -> Option<(&'static str, String)> {
    let mut w = Words::new(stmt);
    if !w.eat("create") {
        return None;
    }
    loop {
        if w.eat("or") {
            if !w.eat("replace") {
                return None;
            }
            continue;
        }
        if w.eat("temp")
            || w.eat("temporary")
            || w.eat("unlogged")
            || w.eat("unique")
            || w.eat("materialized")
            || w.eat("constraint")
            || w.eat("global")
            || w.eat("local")
        {
            continue;
        }
        break;
    }
    let kind = if w.eat("table") {
        "table"
    } else if w.eat("view") {
        "view"
    } else if w.eat("function") {
        "function"
    } else if w.eat("procedure") {
        "procedure"
    } else if w.eat("index") {
        "index"
    } else if w.eat("trigger") {
        "trigger"
    } else if w.eat("type") {
        "type"
    } else if w.eat("schema") {
        "schema"
    } else if w.eat("sequence") {
        "sequence"
    } else {
        return None;
    };
    loop {
        if w.eat("concurrently") {
            continue;
        }
        if w.eat("if") {
            let _ = w.eat("not");
            let _ = w.eat("exists");
            continue;
        }
        break;
    }
    let name = parse_qualified_name(w.rest())?;
    if name.is_empty() || name.chars().count() > 200 {
        return None;
    }
    Some((kind, name))
}

fn parse_qualified_name(s: &str) -> Option<String> {
    let mut rest = s.trim_start();
    let (name, next) = parse_ident(rest)?;
    if name.is_empty() {
        return None;
    }
    let mut last = name;
    rest = next.trim_start();
    while let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start();
        let (name, next) = parse_ident(rest)?;
        if name.is_empty() {
            return None;
        }
        last = name;
        rest = next.trim_start();
    }
    Some(last)
}

fn parse_ident(s: &str) -> Option<(String, &str)> {
    let mut chars = s.chars();
    let c = chars.next()?;
    match c {
        '"' | '`' => {
            let mut name = String::new();
            let mut i = c.len_utf8();
            while i < s.len() {
                let ch = s[i..].chars().next()?;
                let n = i + ch.len_utf8();
                if ch == c {
                    if s[n..].starts_with(c) {
                        name.push(c);
                        i = n + c.len_utf8();
                        continue;
                    }
                    return Some((name, &s[n..]));
                }
                name.push(ch);
                i = n;
            }
            None
        }
        '[' => {
            let end = s.find(']')?;
            Some((s[1..end].to_string(), &s[end + 1..]))
        }
        ch if ch.is_ascii_alphabetic() || ch == '_' => {
            let end = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(s.len());
            Some((s[..end].to_string(), &s[end..]))
        }
        _ => None,
    }
}

fn cap_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

fn cap_bytes(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut cut = n;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

/// Same record shape as `code::emit_code_doc`: one file document plus one
/// document per declaration. `impl` is the statement itself, so a large file
/// whose stored body dropped the `COPY` payload still cites the source line.
fn emit(logical: &Path, scan: &ScanOut, sink: Sink) -> ExtractStats {
    let mut stats = ExtractStats::default();
    let title = logical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("source")
        .to_string();
    let mut fields = Map::new();
    fields.insert("title".into(), Value::String(title.clone()));
    fields.insert("language".into(), Value::String("sql".into()));
    if !scan.symbols.is_empty() {
        let mut seen = std::collections::HashSet::new();
        let defs: Vec<String> = scan
            .symbols
            .iter()
            .filter(|s| seen.insert((s.kind, s.name.as_str())))
            .map(|s| format!("{} {}", s.kind, s.name))
            .collect();
        fields.insert("defs".into(), Value::String(defs.join("\n")));
        let arr: Vec<Value> = scan
            .symbols
            .iter()
            .map(|s| {
                let mut m = Map::new();
                m.insert("name".into(), Value::String(s.name.clone()));
                m.insert("kind".into(), Value::String(s.kind.to_string()));
                m.insert("line".into(), Value::Number((s.line as u64).into()));
                m.insert("start_line".into(), Value::Number((s.line as u64).into()));
                m.insert("end_line".into(), Value::Number((s.end_line as u64).into()));
                Value::Object(m)
            })
            .collect();
        fields.insert("symbols".into(), Value::Array(arr));
        fields.insert(
            "symbol_count".into(),
            Value::Number((scan.symbols.len() as u64).into()),
        );
    }
    fields.insert("body".into(), Value::String(scan.body.clone()));
    let line_count = scan.body.lines().count() as u64;
    if line_count > 0 {
        fields.insert("start_line".into(), Value::Number(1.into()));
        fields.insert("end_line".into(), Value::Number(line_count.into()));
    }
    stats.records += 1;
    if !sink(RawRecord {
        fields,
        locator: "code".into(),
        group: None,
        origin: FieldOrigin::Extractor,
    }) {
        return stats;
    }

    let mut emitted = std::collections::HashSet::new();
    for s in &scan.symbols {
        if s.code.is_empty() {
            continue;
        }
        let locator = format!("code:{}:{}", s.line, s.name);
        if !emitted.insert(locator.clone()) {
            continue;
        }
        let mut sf = Map::new();
        sf.insert("title".into(), Value::String(s.name.clone()));
        // Basename here; stamp time rewrites it to the repo-relative path
        // when it still matches that basename (#2351).
        sf.insert("path".into(), Value::String(title.clone()));
        sf.insert("language".into(), Value::String("sql".into()));
        sf.insert("name".into(), Value::String(s.name.clone()));
        sf.insert("kind".into(), Value::String(s.kind.to_string()));
        sf.insert("line".into(), Value::Number((s.line as u64).into()));
        sf.insert("start_line".into(), Value::Number((s.line as u64).into()));
        sf.insert("end_line".into(), Value::Number((s.end_line as u64).into()));
        sf.insert("code".into(), Value::String(s.code.clone()));
        if !s.impl_text.is_empty() {
            sf.insert("impl".into(), Value::String(s.impl_text.clone()));
        }
        stats.records += 1;
        if !sink(RawRecord {
            fields: sf,
            locator,
            group: None,
            origin: FieldOrigin::Extractor,
        }) {
            return stats;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff::{self, Family};

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn indexed(path: &Path) -> (crate::extract::ExtractStats, Vec<crate::extract::RawRecord>) {
        let sn = sniff::sniff(path).unwrap();
        let mut recs = Vec::new();
        let stats = crate::extract::extract(path, &sn, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    #[test]
    fn ddl_symbols_include_qualified_and_quoted_names() {
        let sql = "CREATE TABLE public.rules (id int);\n\
                   CREATE OR REPLACE VIEW \"Odd Name\" AS SELECT 1;\n\
                   CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS idx_rules ON rules (id);\n\
                   CREATE SEQUENCE IF NOT EXISTS rules_id_seq;\n\
                   CREATE SCHEMA app;\n\
                   CREATE TYPE rule_status AS ENUM ('on');\n\
                   CREATE TRIGGER rules_touch AFTER INSERT ON rules FOR EACH ROW EXECUTE FUNCTION touch();\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "schema.sql", sql);
        let (stats, recs) = indexed(&path);
        assert!(stats.refusal.is_none());
        assert!(stats.records >= 7, "{stats:?}");
        let defs = recs
            .iter()
            .find_map(|r| r.fields.get("defs").and_then(|v| v.as_str()))
            .unwrap_or("");
        for want in [
            "table rules",
            "view Odd Name",
            "index idx_rules",
            "sequence rules_id_seq",
            "schema app",
            "type rule_status",
            "trigger rules_touch",
        ] {
            assert!(defs.contains(want), "missing {want} in {defs}");
        }
        let table = recs
            .iter()
            .find(|r| r.fields.get("name").and_then(|v| v.as_str()) == Some("rules"))
            .expect("table symbol");
        assert_eq!(table.fields["start_line"], table.fields["line"]);
        assert!(table.fields.get("end_line").is_some());
        let file = recs.iter().find(|r| r.locator == "code").expect("file doc");
        assert_eq!(file.fields["start_line"], serde_json::json!(1));
    }

    #[test]
    fn a_copy_dump_with_schema_indexes_the_table_and_skips_the_rows() {
        let sql = "CREATE TABLE users (id int, email text);\n\
                   COPY users (id, email) FROM stdin;\n\
                   1\tann@example.com\n\
                   \\.\n\
                   CREATE INDEX idx_users_email ON users (email);\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "dump.sql", sql);
        let sn = sniff::sniff(&path).unwrap();
        assert_eq!(sn.family, Family::SqlDump);
        let (stats, recs) = indexed(&path);
        assert!(stats.records > 0, "schema in a COPY dump must not be junk");
        assert!(stats.refusal.is_none(), "{stats:?}");
        let defs = recs
            .iter()
            .find_map(|r| r.fields.get("defs").and_then(|v| v.as_str()))
            .unwrap_or("");
        assert!(
            defs.contains("table users") && defs.contains("index idx_users_email"),
            "{defs}"
        );
        // The indexed body is the source we kept. Row text may remain when
        // the file is small; the structured row fields must not.
        assert!(
            recs.iter().all(|r| r.fields.get("email").is_none()),
            "COPY rows are not dump fields: {recs:?}"
        );
    }

    #[test]
    fn a_data_only_copy_dump_is_skipped_with_a_reason() {
        let sql = "COPY users (id, email) FROM stdin;\n1\tann@example.com\n\\.\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "rows.sql", sql);
        let sn = sniff::sniff(&path).unwrap();
        assert_eq!(sn.family, Family::SqlDump);
        let (stats, recs) = indexed(&path);
        assert_eq!(stats.records, 0);
        assert!(recs.is_empty());
        assert_eq!(
            stats.refusal.as_deref(),
            Some(DATA_ONLY_DUMP_REASON),
            "{stats:?}"
        );
    }

    #[test]
    fn an_insert_dump_still_yields_rows() {
        let sql = "CREATE TABLE users (id int, name text);\n\
                   INSERT INTO users VALUES (1,'ann');\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "dump.sql", sql);
        let (stats, recs) = indexed(&path);
        assert_eq!(stats.records, 1, "{recs:?}");
        assert_eq!(recs[0].fields["name"], serde_json::json!("ann"));
    }

    #[test]
    fn a_procedure_whose_body_inserts_is_a_function_symbol() {
        let sql = "CREATE PROCEDURE seed_rules()\n\
                   BEGIN\n\
                     INSERT INTO rules VALUES (1);\n\
                   END;\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "seed.sql", sql);
        let sn = sniff::sniff(&path).unwrap();
        // The INSERT is not dollar-quoted, so the probe may call this a dump.
        // Either way the procedure has to be indexed, not junked.
        let (stats, recs) = indexed(&path);
        assert!(
            stats.records > 0,
            "family {:?} junked the procedure",
            sn.family
        );
        let defs = recs
            .iter()
            .find_map(|r| r.fields.get("defs").and_then(|v| v.as_str()))
            .unwrap_or("");
        assert!(
            defs.contains("procedure seed_rules"),
            "{defs} family {:?}",
            sn.family
        );
    }
}
