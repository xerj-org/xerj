//! Content-based format detection. NEVER trusts file extensions.
//!
//! Order: magic bytes → binary check → text heuristics
//! (json/jsonl → html/xml → logs → sql dump → csv → yaml → txt).

use anyhow::Result;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Jsonl,
    Json,
    Csv,
    Logs,
    Xml,
    Html,
    Yaml,
    TxtProse,
    TxtLines,
    Pdf,
    Docx,
    Sqlite,
    SqlDump,
    Binary,
}

impl Family {
    pub fn as_str(&self) -> &'static str {
        match self {
            Family::Jsonl => "jsonl",
            Family::Json => "json",
            Family::Csv => "csv",
            Family::Logs => "logs",
            Family::Xml => "xml",
            Family::Html => "html",
            Family::Yaml => "yaml",
            Family::TxtProse => "txt-prose",
            Family::TxtLines => "txt-lines",
            Family::Pdf => "pdf",
            Family::Docx => "docx",
            Family::Sqlite => "sqlite",
            Family::SqlDump => "sqldump",
            Family::Binary => "binary",
        }
    }
    /// Document-family formats produce one record per document/section.
    pub fn is_document(&self) -> bool {
        matches!(self, Family::Pdf | Family::Docx | Family::TxtProse)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CsvDialect {
    pub delim: u8,
    pub has_header: bool,
    pub decimal_comma: bool,
}

#[derive(Debug, Clone)]
pub struct Sniffed {
    pub family: Family,
    pub gzip: bool,
    /// e.g. "png", "zip", "elf", "unknown" — set when family == Binary.
    pub binary_kind: Option<String>,
    pub csv: Option<CsvDialect>,
    /// "utf-8" or "windows-1252 (lossy)"
    pub encoding: &'static str,
}

fn read_prefix(path: &Path, gzip: bool, n: usize) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(n.min(1 << 20));
    if gzip {
        let mut r = flate2::read::MultiGzDecoder::new(f).take(n as u64);
        r.read_to_end(&mut buf).ok(); // truncated gz prefix is fine for sniffing
    } else {
        let mut r = f.take(n as u64);
        r.read_to_end(&mut buf)?;
    }
    Ok(buf)
}

pub fn sniff(path: &Path) -> Result<Sniffed> {
    let head = read_prefix(path, false, 8)?;
    let gzip = head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b;
    let prefix = read_prefix(path, gzip, 8192)?;
    let mut s = sniff_bytes(&prefix, path, gzip)?;
    s.gzip = gzip;
    Ok(s)
}

fn sniff_bytes(prefix: &[u8], path: &Path, gzip: bool) -> Result<Sniffed> {
    let mk = |family: Family| Sniffed {
        family,
        gzip: false,
        binary_kind: None,
        csv: None,
        encoding: "utf-8",
    };
    if prefix.is_empty() {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("empty".into());
        return Ok(s);
    }

    // 1. Magic bytes.
    if prefix.starts_with(b"%PDF-") {
        return Ok(mk(Family::Pdf));
    }
    if prefix.starts_with(b"SQLite format 3\0") {
        return Ok(mk(Family::Sqlite));
    }
    if prefix.starts_with(b"PK\x03\x04") {
        // zip container: DOCX iff it holds word/document.xml
        if !gzip {
            if let Ok(f) = std::fs::File::open(path) {
                if let Ok(mut z) = zip::ZipArchive::new(f) {
                    let is_docx = (0..z.len()).any(|i| {
                        z.by_index_raw(i)
                            .map(|e| e.name() == "word/document.xml")
                            .unwrap_or(false)
                    });
                    if is_docx {
                        return Ok(mk(Family::Docx));
                    }
                }
            }
        }
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("zip".into());
        return Ok(s);
    }
    for (magic, kind) in [
        (&b"\x89PNG"[..], "png"),
        (&b"GIF8"[..], "gif"),
        (&b"\xff\xd8\xff"[..], "jpeg"),
        (&b"\x7fELF"[..], "elf"),
        (&b"BM"[..], "bmp"),
        (&b"\x00\x00\x01\x00"[..], "ico"),
    ] {
        if prefix.starts_with(magic) {
            let mut s = mk(Family::Binary);
            s.binary_kind = Some(kind.into());
            return Ok(s);
        }
    }

    // 2. Binary vs text: decode UTF-8, fall back windows-1252.
    let (text, encoding) = decode(prefix);
    let nul = prefix.iter().filter(|&&b| b == 0).count();
    if nul * 10 > prefix.len() {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("unknown".into());
        return Ok(s);
    }
    // High ratio of control chars (excluding \t \n \r) → binary.
    let ctrl = text
        .chars()
        .filter(|c| (*c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r'))
        .count();
    if ctrl * 10 > text.chars().count().max(1) {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("unknown".into());
        return Ok(s);
    }

    // 3. Text heuristics — complete lines only (last line may be truncated).
    let mut lines: Vec<&str> = text.lines().collect();
    if !text.ends_with('\n') && lines.len() > 1 {
        lines.pop();
    }
    let nonblank: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();

    let mut out = mk(classify_text(&text, &nonblank));
    out.encoding = encoding;
    if out.family == Family::Csv {
        out.csv = sniff_csv_dialect(&nonblank);
        if out.csv.is_none() {
            out.family = txt_kind(&nonblank);
        }
    }
    Ok(out)
}

fn decode(bytes: &[u8]) -> (String, &'static str) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "utf-8"),
        Err(e) => {
            // Tolerate a multi-byte char cut at the prefix boundary.
            if e.valid_up_to() + 4 >= bytes.len() {
                (
                    String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
                    "utf-8",
                )
            } else {
                let (s, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
                (s.into_owned(), "windows-1252 (lossy)")
            }
        }
    }
}

/// Decode a whole byte buffer for extraction (same policy as sniffing).
pub fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    decode(bytes)
}

fn classify_text(text: &str, nonblank: &[&str]) -> Family {
    let trimmed = text.trim_start();
    // JSON / JSONL
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if nonblank.len() >= 2 {
            let parse_ok = nonblank
                .iter()
                .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
                .count();
            if parse_ok * 10 >= nonblank.len() * 9 {
                return Family::Jsonl;
            }
        } else if nonblank.len() == 1
            && serde_json::from_str::<serde_json::Value>(nonblank[0]).is_ok()
        {
            // single complete JSON line — treat as JSON value file
            return Family::Json;
        }
        // Pretty-printed or multi-line JSON value.
        if looks_like_json_start(trimmed) {
            return Family::Json;
        }
    }

    // HTML / XML — declaration within the first 256 bytes.
    let head_lc: String = text.chars().take(256).collect::<String>().to_lowercase();
    if head_lc.contains("<!doctype html") || head_lc.contains("<html") {
        return Family::Html;
    }
    if head_lc.contains("<?xml") || (trimmed.starts_with('<') && text.contains("</")) {
        // xhtml disguised as xml?
        let lc: String = text.to_lowercase();
        if lc.contains("<html") || lc.contains("<body") {
            return Family::Html;
        }
        return Family::Xml;
    }

    // Log lines
    if nonblank.len() >= 3 {
        let hits = nonblank
            .iter()
            .filter(|l| crate::extract::logs::probe_line(l))
            .count();
        if hits * 10 >= nonblank.len() * 6 {
            return Family::Logs;
        }
    }

    // SQL dump
    let upper: String = text.chars().take(4096).collect::<String>().to_uppercase();
    if (upper.contains("CREATE TABLE") || upper.contains("INSERT INTO")) && text.contains(';') {
        return Family::SqlDump;
    }

    // CSV — dialect probe happens in caller; here just a cheap plausibility test.
    if nonblank.len() >= 2 && sniff_csv_dialect(nonblank).is_some() {
        return Family::Csv;
    }

    // YAML
    if nonblank.first().map(|l| l.trim() == "---").unwrap_or(false)
        || yaml_line_ratio(nonblank) >= 0.6
    {
        return Family::Yaml;
    }

    txt_kind(nonblank)
}

fn looks_like_json_start(t: &str) -> bool {
    // starts with { or [ and the first ~200 chars look like JSON tokens
    let head: String = t.chars().take(200).collect();
    head.contains(':') || head.contains('[') || head.contains('{')
}

fn yaml_line_ratio(nonblank: &[&str]) -> f64 {
    if nonblank.len() < 3 {
        return 0.0;
    }
    let re = regex::Regex::new(r"^\s*(- )?[\w.@/-]+:(\s|$)").unwrap();
    let hits = nonblank
        .iter()
        .filter(|l| re.is_match(l) || l.trim_start().starts_with("- "))
        .count();
    hits as f64 / nonblank.len() as f64
}

fn txt_kind(nonblank: &[&str]) -> Family {
    if nonblank.is_empty() {
        return Family::TxtLines;
    }
    let avg_len = nonblank.iter().map(|l| l.len()).sum::<usize>() as f64 / nonblank.len() as f64;
    if avg_len > 60.0 {
        Family::TxtProse
    } else {
        // Short lines with no structure → line records; but a handful of
        // short lines in a note-like file is still prose.
        if nonblank.len() <= 5 {
            Family::TxtProse
        } else {
            Family::TxtLines
        }
    }
}

/// Quote-aware field split (supports " and ' quoting).
fn split_quoted(line: &str, delim: u8) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c as u32 == delim as u32 {
                    fields.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
        }
    }
    fields.push(cur);
    fields
}

fn sniff_csv_dialect(nonblank: &[&str]) -> Option<CsvDialect> {
    if nonblank.len() < 2 {
        return None;
    }
    let sample: Vec<&str> = nonblank.iter().take(64).copied().collect();
    let mut best: Option<(u8, usize)> = None; // (delim, field count)
    for delim in *b",;\t|" {
        let counts: Vec<usize> = sample
            .iter()
            .map(|l| split_quoted(l, delim).len())
            .collect();
        let first = counts[0];
        if first < 2 {
            continue;
        }
        let consistent = counts.iter().filter(|&&c| c == first).count();
        // ≥90% of lines share the same field count
        if consistent * 10 >= counts.len() * 9 {
            match best {
                Some((_, bc)) if bc >= first => {}
                _ => best = Some((delim, first)),
            }
        }
    }
    let (delim, _) = best?;
    let head_fields = split_quoted(sample[0], delim);
    let numericish = |s: &str| {
        let t = s.trim();
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
            && t.chars().any(|c| c.is_ascii_digit())
    };
    let has_header = {
        let mut distinct = std::collections::HashSet::new();
        let all_nonnum = head_fields.iter().all(|f| !numericish(f));
        let all_distinct = head_fields
            .iter()
            .all(|f| distinct.insert(f.trim().to_string()));
        let body_has_num = sample
            .iter()
            .skip(1)
            .any(|l| split_quoted(l, delim).iter().any(|f| numericish(f)));
        all_nonnum && all_distinct && body_has_num
    };
    // decimal comma: with ';' delimiter, a meaningful share of fields look like 12,3
    let decimal_comma = if delim == b';' {
        let re = regex::Regex::new(r"^-?\d{1,9},\d+$").unwrap();
        let (mut num, mut hits) = (0usize, 0usize);
        for l in sample.iter().skip(if has_header { 1 } else { 0 }) {
            for f in split_quoted(l, delim) {
                let t = f.trim().to_string();
                if numericish(&t) {
                    num += 1;
                    if re.is_match(&t) {
                        hits += 1;
                    }
                }
            }
        }
        num > 0 && hits * 10 >= num * 3
    } else {
        false
    };
    Some(CsvDialect {
        delim,
        has_header,
        decimal_comma,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(s: &str) -> Family {
        let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        classify_text(s, &lines)
    }

    #[test]
    fn sniff_families() {
        assert_eq!(classify("{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n"), Family::Jsonl);
        assert_eq!(
            classify("{\n  \"a\": 1,\n  \"b\": [1,2]\n}\n"),
            Family::Json
        );
        assert_eq!(
            classify("<!DOCTYPE html>\n<html><head></head></html>"),
            Family::Html
        );
        assert_eq!(
            classify("<?xml version='1.0'?>\n<r><a>1</a></r>"),
            Family::Xml
        );
        assert_eq!(classify("a,b,c\n1,2,3\n4,5,6\n"), Family::Csv);
        assert_eq!(
            classify("CREATE TABLE `t` (\n `a` int\n);\nINSERT INTO `t` VALUES (1);\n"),
            Family::SqlDump
        );
        assert_eq!(
            classify("key: value\nother: 1\nnested:\n  a: 2\n"),
            Family::Yaml
        );
    }

    #[test]
    fn csv_dialect_semicolon_decimal_comma() {
        let lines = vec![
            "geraet;zeitpunkt;temperatur_c",
            "dev-1;2026-03-09T02:09:26Z;50,6",
            "dev-2;2026-03-10T19:10:36Z;57,0",
        ];
        let d = sniff_csv_dialect(&lines).unwrap();
        assert_eq!(d.delim, b';');
        assert!(d.has_header);
        assert!(d.decimal_comma);
    }
}
