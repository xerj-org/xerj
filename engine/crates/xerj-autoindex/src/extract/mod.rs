//! Streaming record extraction — one module per format family.
//! Every extractor is bounded-memory and never fatal: parse failures
//! downgrade (family → txt → junk-with-metadata) and are counted.

pub mod csv_x;
pub mod docx;
pub mod html;
pub mod json;
pub mod jsonl;
pub mod logs;
pub mod pdf;
pub mod sqldump;
pub mod sqlite_x;
pub mod txt;
pub mod xml_x;
pub mod yaml_x;

use crate::sniff::{Family, Sniffed};
use anyhow::Result;
use serde_json::{Map, Value};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// One extracted record before coercion.
#[derive(Debug, Clone)]
pub struct RawRecord {
    pub fields: Map<String, Value>,
    /// Canonical, content-positional locator (byte offset / ordinal /
    /// table+row) — the idempotent-_id ingredient.
    pub locator: String,
    /// Sub-dataset group within a file (table name for sql/sqlite).
    pub group: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractStats {
    pub records: u64,
    pub junk: u64,
}

/// Sink returns false to stop extraction early (sampling).
pub type Sink<'a> = &'a mut dyn FnMut(RawRecord) -> bool;

pub const MAX_LINE: usize = 16 << 20; // 16MB line cap
pub const MAX_WHOLE_FILE: u64 = 64 << 20; // whole-file parse cap (json/html/yaml/txt)
/// Target characters per document section.
///
/// Was 32 KB, which is a *storage* granularity, not a *retrieval* one: BM25
/// scores per document, so a 32 KB section dilutes any match into noise and
/// every hit drags 32 KB through `_source`.  Measured on a 460-commit history
/// file, 32 KB sections produced 25 documents for 15,407 lines and the
/// relevant commit was not retrievable; at 2 KB with paragraph overlap it is.
///
/// 2 KB is roughly the 40-line window validated for line-oriented text, and
/// comfortably inside the 512-token limit of the built-in neural embedder, so
/// a section maps to one vector without truncation.
pub const SECTION_CHARS: usize = 2 << 10;

/// Characters of the previous section repeated at the start of the next, so an
/// answer spanning a boundary stays retrievable from both sides.
pub const SECTION_OVERLAP: usize = 200;

/// Open a (possibly gzipped) file as a buffered reader of DECODED-transparent
/// bytes, optionally capped at `limit` decoded bytes (sampling).
pub fn open_reader(path: &Path, gzip: bool, limit: Option<u64>) -> Result<Box<dyn BufRead>> {
    let f = std::fs::File::open(path)?;
    let inner: Box<dyn Read> = if gzip {
        Box::new(flate2::read::MultiGzDecoder::new(f))
    } else {
        Box::new(f)
    };
    let inner: Box<dyn Read> = match limit {
        Some(n) => Box::new(inner.take(n)),
        None => Box::new(inner),
    };
    Ok(Box::new(BufReader::with_capacity(256 << 10, inner)))
}

/// Read a whole (possibly gzipped) file, capped; None if over cap.
pub fn read_whole(path: &Path, gzip: bool, cap: u64) -> Result<Option<Vec<u8>>> {
    let mut r = open_reader(path, gzip, Some(cap + 1))?;
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// Dispatch to the family extractor. `limit_bytes` bounds SAMPLING reads;
/// `None` = full stream.
pub fn extract(
    path: &Path,
    sn: &Sniffed,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    match sn.family {
        Family::Jsonl => jsonl::extract(path, sn.gzip, limit_bytes, sink),
        Family::Json => json::extract(path, sn.gzip, sink),
        Family::Csv => csv_x::extract(path, sn, limit_bytes, sink),
        Family::Logs => logs::extract(path, sn.gzip, limit_bytes, sink),
        Family::Xml => xml_x::extract(path, sn.gzip, sink),
        Family::Html => html::extract(path, sn.gzip, sink),
        Family::Yaml => yaml_x::extract(path, sn.gzip, sink),
        Family::TxtProse => txt::extract_prose(path, sn.gzip, sink),
        Family::TxtLines => txt::extract_lines(path, sn.gzip, limit_bytes, sink),
        Family::Pdf => pdf::extract(path, sink),
        Family::Docx => docx::extract(path, sink),
        Family::Sqlite => sqlite_x::extract(path, limit_bytes.map(|_| 500), sink),
        Family::SqlDump => sqldump::extract(path, sn.gzip, limit_bytes, sink),
        Family::Binary => Ok(ExtractStats::default()),
    }
}

// ─── shared helpers ──────────────────────────────────────────────────────

pub const MAX_FIELDS_PER_RECORD: usize = 512;

/// Flatten a JSON object into a flat field map: up to TWO levels of nesting
/// become `a_b_c` keys; deeper structure and arrays-of-objects are stored as
/// JSON strings; arrays of scalars stay as arrays. Fields named `ax_*` are
/// renamed `ax__*` (provenance namespace collision).
pub fn flatten_object(obj: Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in obj {
        flatten_into(&sanitize_collision(&k), v, 0, &mut out);
        if out.len() >= MAX_FIELDS_PER_RECORD {
            break;
        }
    }
    out
}

fn sanitize_collision(k: &str) -> String {
    if k.starts_with("ax_") {
        // Data field collides with the ax_* provenance namespace — move it
        // out (recorded in the catalog as a rename note).
        format!("data_{k}")
    } else {
        k.to_string()
    }
}

fn flatten_into(key: &str, v: Value, depth: usize, out: &mut Map<String, Value>) {
    if out.len() >= MAX_FIELDS_PER_RECORD {
        return;
    }
    match v {
        Value::Object(m) => {
            if depth < 2 {
                for (k, vv) in m {
                    flatten_into(&format!("{key}_{k}"), vv, depth + 1, out);
                }
            } else {
                out.insert(
                    key.to_string(),
                    Value::String(serde_json::to_string(&m).unwrap_or_default()),
                );
            }
        }
        Value::Array(a) => {
            if a.iter().all(|e| !e.is_object() && !e.is_array()) {
                out.insert(key.to_string(), Value::Array(a));
            } else {
                out.insert(
                    key.to_string(),
                    Value::String(serde_json::to_string(&a).unwrap_or_default()),
                );
            }
        }
        other => {
            out.insert(key.to_string(), other);
        }
    }
}

/// Sanitize a discovered field/column name to a safe ES field name.
pub fn sanitize_field_name(name: &str) -> String {
    let mut s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '@' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "field".to_string()
    } else {
        sanitize_collision(&s)
    }
}

/// Split long document text into retrieval-sized sections at paragraph
/// boundaries, repeating `SECTION_OVERLAP` characters across each boundary.
pub fn split_sections(text: &str) -> Vec<String> {
    if text.len() <= SECTION_CHARS {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();

    fn tail(s: &str, n: usize) -> String {
        if s.len() <= n {
            return s.to_string();
        }
        let start = s
            .char_indices()
            .rev()
            .take_while(|(i, _)| s.len() - *i <= n)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        s[start..].to_string()
    }

    for para in text.split("\n\n") {
        if !cur.is_empty() && cur.len() + para.len() > SECTION_CHARS {
            let done = std::mem::take(&mut cur);
            let carry = tail(&done, SECTION_OVERLAP);
            out.push(done);
            if !carry.is_empty() {
                cur.push_str(&carry);
            }
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
        while cur.len() > 2 * SECTION_CHARS {
            let cut = cur
                .char_indices()
                .take_while(|(i, _)| *i < SECTION_CHARS)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(cur.len());
            let rest = cur.split_off(cut);
            out.push(std::mem::replace(&mut cur, rest));
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Emit a document (title/body/headings) as one or more section records.
pub fn emit_document(
    title: &str,
    headings: &[String],
    body: &str,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    let sections = split_sections(body);
    let multi = sections.len() > 1;
    for (i, sec) in sections.into_iter().enumerate() {
        let mut fields = Map::new();
        fields.insert("title".into(), Value::String(title.to_string()));
        if !headings.is_empty() {
            fields.insert(
                "headings".into(),
                Value::Array(headings.iter().map(|h| Value::String(h.clone())).collect()),
            );
        }
        if multi {
            fields.insert("section".into(), Value::Number((i as u64).into()));
        }
        fields.insert("body".into(), Value::String(sec));
        stats.records += 1;
        if !sink(RawRecord {
            fields,
            locator: format!("s{i}"),
            group: None,
        }) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod section_tests {
    use super::*;

    fn doc(paras: usize, para_chars: usize) -> String {
        (0..paras)
            .map(|i| format!("p{i} ").repeat(para_chars / 4))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn short_text_stays_one_section() {
        let t = "one paragraph only";
        assert_eq!(split_sections(t), vec![t.to_string()]);
    }

    /// Sections must be retrieval-sized. At the old 32 KB a whole commit
    /// history collapsed into 25 documents and BM25 could not discriminate.
    #[test]
    fn sections_are_retrieval_sized() {
        let t = doc(200, 400);
        let secs = split_sections(&t);
        assert!(
            secs.len() > 10,
            "expected many sections, got {}",
            secs.len()
        );
        for s in &secs {
            assert!(
                s.len() <= 2 * SECTION_CHARS,
                "section of {} bytes exceeds 2x target",
                s.len()
            );
        }
    }

    #[test]
    fn consecutive_sections_share_an_overlap() {
        let t = doc(120, 300);
        let secs = split_sections(&t);
        assert!(secs.len() >= 2);
        let mut overlaps = 0;
        for w in secs.windows(2) {
            let prev_tail: String = w[0]
                .chars()
                .rev()
                .take(60)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if w[1].starts_with(&prev_tail[..prev_tail.len().min(30)]) {
                overlaps += 1;
            }
        }
        assert!(
            overlaps > 0,
            "no section carried an overlap from its predecessor"
        );
    }

    #[test]
    fn no_content_is_dropped() {
        let t = doc(60, 300);
        let secs = split_sections(&t);
        for i in 0..60 {
            let marker = format!("p{i} ");
            assert!(
                secs.iter().any(|s| s.contains(&marker)),
                "paragraph {i} lost"
            );
        }
    }

    /// A single paragraph larger than two sections must still be bounded.
    #[test]
    fn pathological_single_paragraph_is_hard_split() {
        let t = "z".repeat(10 * SECTION_CHARS);
        let secs = split_sections(&t);
        assert!(secs.len() > 1);
        for s in &secs {
            assert!(s.len() <= 2 * SECTION_CHARS);
        }
    }
}
