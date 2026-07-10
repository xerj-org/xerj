//! SQL dump — targeted streaming parser, O(1) memory per tuple.
//! No sqlparser: multi-row `INSERT … VALUES (…),(…)…` statements in dumps
//! can reach hundreds of MB; this lexer never materializes a statement.
//! Handles MySQL + Postgres dump styles: CREATE TABLE column capture, tuple
//! records with `''`/backslash escapes, NULL, numbers, hex literals.
//! One dataset (group) per table.

use super::{sanitize_field_name, ExtractStats, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

const HEAD_CAP: usize = 1 << 20;

#[derive(PartialEq, Debug, Clone, Copy)]
enum St {
    Head,      // accumulating statement text (CREATE/SET/…, or INSERT up to VALUES)
    SkipStmt,  // oversized/unparseable statement — discard to ';'
    TupleScan, // between tuples of an INSERT
    InTuple,   // inside (…)
    InString,  // inside '…'
    Bare,      // bare token value (number / NULL / hex)
}

pub fn extract(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut r = super::open_reader(path, gzip, limit_bytes)?;
    let mut stats = ExtractStats::default();

    let mut tables: HashMap<String, Vec<String>> = HashMap::new();
    let mut stmt_ord: HashMap<String, u64> = HashMap::new();

    let mut state = St::Head;
    let mut head = String::new();
    let mut head_quote: Option<u8> = None;

    let mut cur_table = String::new();
    let mut cur_cols: Vec<String> = Vec::new();
    let mut cur_stmt = 0u64;
    let mut tuple_ord = 0u64;
    let mut values: Vec<Value> = Vec::new();
    let mut sval = String::new(); // current string value
    let mut maybe_end = false; // saw closing ' — is it '' escape?
    let mut esc = false;

    let mut chunk = vec![0u8; 256 << 10];
    let mut stop = false;
    'outer: loop {
        let n = r.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let mut i = 0usize;
        while i < n {
            let b = chunk[i];
            match state {
                St::Head => {
                    match head_quote {
                        Some(q) => {
                            if b == q {
                                head_quote = None;
                            }
                            if head.len() < HEAD_CAP {
                                head.push(b as char);
                            }
                        }
                        None => {
                            if b == b'\'' || b == b'"' {
                                head_quote = Some(b);
                                head.push(b as char);
                            } else if b == b';' {
                                process_statement_head(&head, &mut tables);
                                head.clear();
                            } else {
                                if head.len() >= HEAD_CAP {
                                    head.clear();
                                    state = St::SkipStmt;
                                    i += 1;
                                    continue;
                                }
                                head.push(b as char);
                                // did we just complete the VALUES keyword of an INSERT?
                                if (b == b'S' || b == b's') && head.len() >= 6 {
                                    let tail = &head[head.len() - 6..];
                                    if tail.eq_ignore_ascii_case("values")
                                        && head.trim_start().len() >= 6
                                        && head.trim_start()[..6].eq_ignore_ascii_case("insert")
                                        && !head[..head.len() - 6]
                                            .ends_with(|c: char| c.is_ascii_alphanumeric())
                                    {
                                        if let Some((t, cols)) = parse_insert_head(&head) {
                                            cur_table = t;
                                            cur_cols = if cols.is_empty() {
                                                tables
                                                    .get(&cur_table)
                                                    .cloned()
                                                    .unwrap_or_default()
                                            } else {
                                                cols
                                            };
                                            let e =
                                                stmt_ord.entry(cur_table.clone()).or_insert(0);
                                            cur_stmt = *e;
                                            *e += 1;
                                            tuple_ord = 0;
                                            state = St::TupleScan;
                                        } else {
                                            state = St::SkipStmt;
                                        }
                                        head.clear();
                                    }
                                }
                            }
                        }
                    }
                    i += 1;
                }
                St::SkipStmt => {
                    // discard to ';' (quote-aware)
                    match head_quote {
                        Some(q) => {
                            if b == q {
                                head_quote = None;
                            }
                        }
                        None => {
                            if b == b'\'' || b == b'"' {
                                head_quote = Some(b);
                            } else if b == b';' {
                                state = St::Head;
                            }
                        }
                    }
                    i += 1;
                }
                St::TupleScan => {
                    match b {
                        b'(' => {
                            values.clear();
                            state = St::InTuple;
                        }
                        b';' => {
                            state = St::Head;
                        }
                        _ => {} // commas, whitespace
                    }
                    i += 1;
                }
                St::InTuple => {
                    match b {
                        b'\'' => {
                            sval.clear();
                            esc = false;
                            maybe_end = false;
                            state = St::InString;
                            i += 1;
                        }
                        b')' => {
                            if !emit_tuple(
                                &cur_table,
                                &cur_cols,
                                &mut values,
                                cur_stmt,
                                &mut tuple_ord,
                                sink,
                                &mut stats,
                            ) {
                                stop = true;
                                break 'outer;
                            }
                            state = St::TupleScan;
                            i += 1;
                        }
                        b',' => {
                            i += 1; // empty slot separators handled by Bare/Str paths
                        }
                        c if c.is_ascii_whitespace() => {
                            i += 1;
                        }
                        _ => {
                            sval.clear();
                            state = St::Bare;
                            // do not advance — Bare consumes this byte
                        }
                    }
                }
                St::InString => {
                    if maybe_end {
                        maybe_end = false;
                        if b == b'\'' {
                            sval.push('\'');
                            i += 1;
                        } else {
                            // string finished; value complete. Reprocess b in InTuple.
                            values.push(Value::String(std::mem::take(&mut sval)));
                            state = St::InTuple;
                        }
                    } else if esc {
                        esc = false;
                        let c = match b {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            b'0' => '\0',
                            other => other as char,
                        };
                        sval.push(c);
                        i += 1;
                    } else {
                        match b {
                            b'\\' => {
                                esc = true;
                                i += 1;
                            }
                            b'\'' => {
                                maybe_end = true;
                                i += 1;
                            }
                            _ => {
                                // bytes are pushed raw; multibyte UTF-8 comes
                                // through byte-by-byte — collect as latin-1 then
                                // fix below? No: push into a byte buffer instead.
                                sval.push(b as char);
                                i += 1;
                            }
                        }
                    }
                }
                St::Bare => {
                    if matches!(b, b',' | b')') || b.is_ascii_whitespace() {
                        values.push(bare_value(&std::mem::take(&mut sval)));
                        state = St::InTuple;
                        // reprocess delimiter in InTuple
                    } else {
                        if sval.len() < HEAD_CAP {
                            sval.push(b as char);
                        }
                        i += 1;
                    }
                }
            }
        }
    }
    // EOF with a dangling string value under sampling limits is expected —
    // discard silently when sampling, count as junk otherwise.
    if !stop && limit_bytes.is_none() && (state == St::InString || state == St::Bare) {
        stats.junk += 1;
    }
    Ok(stats)
}

/// UTF-8 repair: strings were accumulated byte-as-char (latin-1); re-encode.
fn fix_utf8(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let bytes: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
    match String::from_utf8(bytes) {
        Ok(fixed) => fixed,
        Err(_) => s.to_string(),
    }
}

fn bare_value(tok: &str) -> Value {
    let t = tok.trim();
    if t.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if t.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if let Ok(n) = t.parse::<i64>() {
        return Value::Number(n.into());
    }
    if let Ok(f) = t.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(t.to_string())
}

fn emit_tuple(
    table: &str,
    cols: &[String],
    values: &mut Vec<Value>,
    stmt: u64,
    tuple_ord: &mut u64,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    let vals = std::mem::take(values);
    if table.is_empty() {
        stats.junk += 1;
        return true;
    }
    let mut fields = Map::new();
    for (j, v) in vals.into_iter().enumerate() {
        if v.is_null() {
            continue;
        }
        let name = cols
            .get(j)
            .cloned()
            .unwrap_or_else(|| format!("col_{}", j + 1));
        let v = match v {
            Value::String(s) => Value::String(fix_utf8(&s)),
            other => other,
        };
        fields.insert(name, v);
    }
    let loc = format!("t{table}:s{stmt}:t{tuple_ord}");
    *tuple_ord += 1;
    if fields.is_empty() {
        return true;
    }
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: loc,
        group: Some(table.to_string()),
    })
}

/// `INSERT INTO `t` (a,b,c) VALUES` → (table, [cols])
fn parse_insert_head(head: &str) -> Option<(String, Vec<String>)> {
    let re = regex::Regex::new(
        r#"(?is)^\s*insert\s+(?:ignore\s+)?into\s+[`"]?([A-Za-z0-9_.$-]+)[`"]?\s*(\(([^)]*)\))?\s*values\s*$"#,
    )
    .ok()?;
    let c = re.captures(head)?;
    let table = c.get(1)?.as_str().trim_matches('`').to_string();
    let cols = c
        .get(3)
        .map(|m| {
            m.as_str()
                .split(',')
                .map(|s| sanitize_field_name(s.trim().trim_matches(['`', '"', ' '])))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some((table, cols))
}

/// Capture column names from a complete CREATE TABLE statement head.
fn process_statement_head(head: &str, tables: &mut HashMap<String, Vec<String>>) {
    let trimmed = head.trim_start();
    if trimmed.len() < 12 || !trimmed[..12].eq_ignore_ascii_case("create table") {
        return;
    }
    let name_re = regex::Regex::new(
        r#"(?is)^\s*create\s+table\s+(?:if\s+not\s+exists\s+)?[`"]?([A-Za-z0-9_.$-]+)[`"]?\s*\("#,
    )
    .unwrap();
    let Some(c) = name_re.captures(trimmed) else {
        return;
    };
    let table = c.get(1).unwrap().as_str().to_string();
    let body_start = c.get(0).unwrap().end();
    // matching close paren at depth 0
    let body = &trimmed[body_start..];
    let mut depth = 1i32;
    let mut end = body.len();
    let mut quote: Option<char> = None;
    for (i, ch) in body.char_indices() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' | '`' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            },
        }
    }
    let body = &body[..end];
    // split at top-level commas
    let mut cols = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut part = String::new();
    let mut parts: Vec<String> = Vec::new();
    for ch in body.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
                part.push(ch);
            }
            None => match ch {
                '\'' | '"' | '`' => {
                    quote = Some(ch);
                    part.push(ch);
                }
                '(' => {
                    depth += 1;
                    part.push(ch);
                }
                ')' => {
                    depth -= 1;
                    part.push(ch);
                }
                ',' if depth == 0 => parts.push(std::mem::take(&mut part)),
                _ => part.push(ch),
            },
        }
    }
    parts.push(part);
    const NON_COLS: [&str; 8] = [
        "primary", "key", "unique", "constraint", "foreign", "index", "check", "fulltext",
    ];
    for p in parts {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let first_word: String = p
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_lowercase();
        if p.starts_with('`') || p.starts_with('"') {
            let q = p.chars().next().unwrap();
            if let Some(e) = p[1..].find(q) {
                cols.push(sanitize_field_name(&p[1..1 + e]));
                continue;
            }
        }
        if !first_word.is_empty() && !NON_COLS.contains(&first_word.as_str()) {
            cols.push(sanitize_field_name(&first_word));
        }
    }
    if !cols.is_empty() {
        tables.insert(table, cols);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dump() {
        let sql = br#"-- dump
SET NAMES utf8mb4;
DROP TABLE IF EXISTS `users`;
CREATE TABLE `users` (
  `id` int NOT NULL,
  `name` varchar(64),
  `note` text,
  PRIMARY KEY (`id`)
);
INSERT INTO `users` VALUES (1,'Ann''s','a\'b'),(2,'Bob',NULL);
INSERT INTO `users` (`id`,`name`) VALUES (3,'Cle, (weird)');
"#;
        let dir = std::env::temp_dir().join("xerj-autoindex-test-sqldump");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.sql");
        std::fs::write(&p, &sql[..]).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&p, false, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(stats.records, 3, "{recs:?}");
        assert_eq!(recs[0].fields["name"], serde_json::json!("Ann's"));
        assert_eq!(recs[0].fields["note"], serde_json::json!("a'b"));
        assert_eq!(recs[1].fields["id"], serde_json::json!(2));
        assert!(recs[1].fields.get("note").is_none());
        assert_eq!(recs[2].fields["name"], serde_json::json!("Cle, (weird)"));
        assert_eq!(recs[0].group.as_deref(), Some("users"));
    }
}
