//! Streaming record extraction — one module per format family.
//! Every extractor is bounded-memory and never fatal: parse failures
//! downgrade (family → txt → junk-with-metadata) and are counted.

pub mod bvh;
pub mod code;
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
pub mod unity;
pub mod xml_x;
pub mod yaml_x;

use crate::sniff::{Family, Sniffed};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Where a record's FIELD NAMES came from — the input to dataset clustering.
///
/// Clustering groups files by their field names (`dataset::cluster`), and the
/// dataset slug is an ingredient of every `_id` (`ids::doc_id`). So a name that
/// can appear or disappear because the EXTRACTOR changed — not because the file
/// changed — silently moves that file's documents to a different index and
/// orphans the ones already written under the old slug (issue #178).
///
/// `Data` names are read out of the file (JSON keys, CSV header, SQL columns,
/// HTML table headers, parsed log keys): two files that share them really do
/// share a schema, and the names change only when the file changes.
/// `Extractor` names are the extractor's own vocabulary (`title`/`body`,
/// `defs`/`symbols`/`symbol_count`, `page`/`section`) — they describe the tool,
/// not the data, so they are excluded from the clustering key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldOrigin {
    /// Field names read out of the file itself.
    Data,
    /// Field names invented by the extractor.
    Extractor,
}

/// One extracted record before coercion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRecord {
    pub fields: Map<String, Value>,
    /// Canonical, content-positional locator (byte offset / ordinal /
    /// table+row) — the idempotent-_id ingredient.
    pub locator: String,
    /// Sub-dataset group within a file (table name for sql/sqlite).
    pub group: Option<String>,
    /// Whether `fields`' NAMES came from the file or from this extractor.
    /// Set it at every emit site: it decides whether those names are allowed
    /// to move the file between datasets.
    pub origin: FieldOrigin,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractStats {
    pub records: u64,
    pub junk: u64,
    /// Set when a per-file record cap (`MAX_RECORDS_PER_FILE`) stopped the
    /// split before the whole body was emitted (#381). Carried out so the
    /// truncation is reported, never silent.
    pub truncated: bool,
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

/// Hard cap on section records one file may emit (#381).
///
/// A magic-less printable byte payload (a raw texture, a packed `.bytes` asset)
/// decodes under the text fallback, classifies as prose and is sectioned in
/// full — one record per `SECTION_CHARS`, so the 16 MiB txt read cap becomes
/// ~8k noise records with no ceiling of its own. This bounds that; the drop is
/// surfaced via `ExtractStats.truncated`, so a genuinely large *legitimate*
/// document is reported and never silently cut. A resource guard keyed off
/// record count only — never off byte statistics, which is what deleted CJK
/// text worldwide and was removed in #371.
pub const MAX_RECORDS_PER_FILE: usize = 4 << 10; // 4096

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

/// Render a file as a DOCUMENT (title = file stem, body = decoded text,
/// section-split), regardless of its format family. This is how demoted
/// one-off config files (`dataset` module docs, #173) are indexed: their
/// key sets are configuration, not a schema, so their retrievable value is
/// the text itself. Emits the same `title`/`body`/`section` vocabulary as
/// every other document extractor (`FieldOrigin::Extractor`).
pub fn extract_as_document(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    extract_as_document_with_name(path, path, gzip, sink)
}

/// [`extract_as_document`], but the title comes from `logical_path` rather
/// than `content_path`. The `--no-graph` generated pipeline reads a
/// SEALED SNAPSHOT — a real file on disk, but under its own ordinal name
/// (`prepared/00000000`, …), not the source's — so deriving the title from
/// `content_path` there produced `"00000000"` instead of the file's own
/// name (#722). Mirrors `sniff::sniff_with_name`'s split for exactly the
/// same reason.
pub fn extract_as_document_with_name(
    content_path: &Path,
    logical_path: &Path,
    gzip: bool,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = read_whole(content_path, gzip, MAX_WHOLE_FILE)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    if text.trim().is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }
    let title = logical_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into());
    emit_document(
        &title,
        &[],
        text.trim(),
        MAX_RECORDS_PER_FILE,
        sink,
        &mut stats,
    );
    Ok(stats)
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
        Family::Code => code::extract(path, sn, sink),
        Family::UnityYaml => unity::extract_unity(path, sn.gzip, limit_bytes, sink),
        Family::UnityMeta => unity::extract_meta(path, sn.gzip, sink),
        Family::Bvh => bvh::extract(path, sn.gzip, sink),
        // `--stub`-designated file: one name card, the file is never opened.
        Family::Stub => {
            let mut stats = ExtractStats::default();
            let mut fields = Map::new();
            // The name card is this record's ENTIRE content, so it must come
            // from the logical name. Under durable preparation `path` is a
            // content-addressed blob and would title every stub `00000000`.
            let named = sn.logical_name.as_deref().unwrap_or(path);
            let title = named
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            fields.insert("title".into(), Value::String(title));
            stats.records += 1;
            sink(RawRecord {
                fields,
                locator: "stub".into(),
                group: None,
                origin: FieldOrigin::Extractor,
            });
            Ok(stats)
        }
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

/// Convert a parsed YAML value into JSON (shared by the YAML and Unity
/// extractors). Tagged values unwrap to their inner value; non-string mapping
/// keys are stringified.
pub(crate) fn yaml_to_json(v: serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                Value::Number(u.into())
            } else {
                serde_json::Number::from_f64(n.as_f64().unwrap_or(0.0))
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
        }
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(seq) => {
            Value::Array(seq.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(m) => {
            let mut out = Map::new();
            for (k, vv) in m {
                let key = match k {
                    serde_yaml::Value::String(s) => s,
                    other => serde_yaml::to_string(&other)
                        .unwrap_or_else(|_| "key".into())
                        .trim()
                        .to_string(),
                };
                out.insert(key, yaml_to_json(vv));
            }
            Value::Object(out)
        }
        serde_yaml::Value::Tagged(t) => yaml_to_json(t.value),
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

/// Last `n` bytes of `s`, snapped back to a char boundary.
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

/// Smallest char boundary of `s` that is `>= i` (`s.len()` if `i` is past the
/// end). Used for the hard-split cut so a multi-byte char is never severed.
fn ceil_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Largest char boundary of `s` that is `<= i` (`s.len()` if `i` is past the
/// end).
fn floor_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Top up `cur` from the front of `rest` until it holds `want` bytes, moving
/// whole chars only. Guarantees progress: if `want - cur.len()` lands inside
/// the next char, that whole char moves anyway.
fn fill_window(cur: &mut String, rest: &mut &str, want: usize) {
    if rest.is_empty() || cur.len() >= want {
        return;
    }
    let need = want - cur.len();
    let mut take = floor_boundary(rest, need.min(rest.len()));
    if take == 0 {
        take = rest
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(rest.len());
    }
    cur.push_str(&rest[..take]);
    *rest = &rest[take..];
}

/// Stream `text` as retrieval-sized sections, split at paragraph boundaries and
/// repeating `SECTION_OVERLAP` characters across each boundary. `emit` returns
/// false to stop; `for_each_section` then returns false without doing the rest
/// of the work.
///
/// Linear in `text.len()` in both time and allocation. The predecessor built a
/// `Vec` with `String::split_off` per section, which re-copied the whole
/// remaining paragraph each time *and* left every emitted section carrying the
/// capacity of that paragraph: a 16 MiB paragraph with no blank line cost
/// 16.3 s and 65.6 GB peak RSS (issue #239). Sections are cut out of a window
/// that never exceeds `2 * SECTION_CHARS + 4` bytes, and the paragraph itself
/// stays borrowed from `text`.
pub fn for_each_section(text: &str, emit: &mut dyn FnMut(String) -> bool) -> bool {
    if text.len() <= SECTION_CHARS {
        return emit(text.to_string());
    }
    // One byte past the largest section the loop below will ever hold, so a
    // full window always trips the hard-split condition.
    let window = 2 * SECTION_CHARS + 1;
    let mut cur = String::new();

    for para in text.split("\n\n") {
        if !cur.is_empty() && cur.len() + para.len() > SECTION_CHARS {
            let done = std::mem::take(&mut cur);
            let carry = tail(&done, SECTION_OVERLAP);
            if !emit(done) {
                return false;
            }
            if !carry.is_empty() {
                cur.push_str(&carry);
            }
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        // `rest` is the not-yet-buffered remainder of this paragraph, borrowed
        // from `text` — it is never copied wholesale into `cur`.
        let mut rest = para;
        loop {
            fill_window(&mut cur, &mut rest, window);
            // Identical test to the old `while cur.len() > 2 * SECTION_CHARS`,
            // where `cur` was the whole `cur ++ rest` concatenation.
            if cur.len() + rest.len() <= 2 * SECTION_CHARS {
                break;
            }
            let cut = ceil_boundary(&cur, SECTION_CHARS);
            let section = cur[..cut].to_string();
            cur.drain(..cut);
            if !emit(section) {
                return false;
            }
        }
    }
    if !cur.trim().is_empty() {
        return emit(cur);
    }
    true
}

/// Collecting wrapper over [`for_each_section`], for callers that need every
/// section at once. Prefer the streaming form for whole-file bodies.
pub fn split_sections(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for_each_section(text, &mut |s| {
        out.push(s);
        true
    });
    out
}

fn section_record(
    title: &str,
    headings: &[String],
    i: usize,
    multi: bool,
    section: String,
) -> RawRecord {
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
    fields.insert("body".into(), Value::String(section));
    RawRecord {
        fields,
        locator: format!("s{i}"),
        group: None,
        // title/headings/section/body are this function's vocabulary, and
        // `headings`/`section` come and go — never a clustering key.
        origin: FieldOrigin::Extractor,
    }
}

/// Emit a document (title/body/headings) as one or more section records.
///
/// Streams: sections are cut one at a time, so a sink that stops early (phase-A
/// `--sample`) stops the split too instead of paying for the whole body first.
pub fn emit_document(
    title: &str,
    headings: &[String],
    body: &str,
    max_records: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    // One section of lookahead: the `section` field is only present when the
    // document actually splits, which is not known until a second one arrives.
    let mut pending: Option<String> = None;
    let mut next: usize = 0;
    let mut alive = true;
    for_each_section(body, &mut |sec| {
        let Some(prev) = pending.replace(sec) else {
            return true;
        };
        // #381: bound noise from a misclassified payload. Stopping the split
        // (not just the sink) also stops paying to section the rest of the body.
        if next >= max_records {
            stats.truncated = true;
            return false;
        }
        let i = next;
        next += 1;
        stats.records += 1;
        alive = sink(section_record(title, headings, i, true, prev));
        alive
    });
    // A sink that stopped early (sampling) sets `alive = false`; the cap leaves
    // it true and sets `truncated` instead. Distinguish so a capped run still
    // reports success and does NOT emit the over-cap `pending` tail section.
    if !alive {
        return false;
    }
    if stats.truncated {
        return true;
    }
    match pending {
        Some(last) => {
            if next >= max_records {
                stats.truncated = true;
                return true;
            }
            stats.records += 1;
            sink(section_record(title, headings, next, next > 0, last))
        }
        None => true,
    }
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

    /// The pre-#239 implementation, kept verbatim as an oracle. It is quadratic
    /// — only ever call it on inputs of a few hundred KB.
    fn legacy_split_sections(text: &str) -> Vec<String> {
        if text.len() <= SECTION_CHARS {
            return vec![text.to_string()];
        }
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
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

    /// #239 rewrote the splitter for cost, not for behaviour: every input must
    /// still produce byte-identical sections. Multi-byte cases are the ones a
    /// hand-rolled boundary walk gets wrong, so they are all in here.
    #[test]
    fn matches_the_pre_fix_implementation_byte_for_byte() {
        let mut cases: Vec<String> = vec![
            String::new(),
            "one paragraph only".into(),
            "\n\n\n\n".into(),
            "   \n\n   ".into(),
            "z".repeat(SECTION_CHARS),
            "z".repeat(SECTION_CHARS + 1),
            "z".repeat(2 * SECTION_CHARS),
            "z".repeat(2 * SECTION_CHARS + 1),
            "z".repeat(10 * SECTION_CHARS),
            doc(200, 400),
            doc(60, 300),
            doc(3, 9000),
            // Multi-byte, with the cut landing inside a char: 3-byte and 4-byte
            // sequences do not divide SECTION_CHARS evenly.
            "é".repeat(4 * SECTION_CHARS),
            "设".repeat(4 * SECTION_CHARS),
            "🦀".repeat(4 * SECTION_CHARS),
            format!("{}{}", "a", "😀".repeat(3 * SECTION_CHARS)),
            format!("{}{}", "ab", "ا".repeat(3 * SECTION_CHARS)),
            // Mixed: paragraph path and hard-split path interleaved.
            format!(
                "{}\n\n{}\n\n{}",
                "x".repeat(300),
                "設定".repeat(2 * SECTION_CHARS),
                doc(5, 500)
            ),
        ];
        // Every offset around a section boundary, so an off-by-one in the cut
        // shows up.
        for d in 0..8usize {
            cases.push("q".repeat(2 * SECTION_CHARS + d));
            cases.push(format!(
                "{}{}",
                "w".repeat(d),
                "é".repeat(2 * SECTION_CHARS)
            ));
        }
        for t in &cases {
            assert_eq!(
                split_sections(t),
                legacy_split_sections(t),
                "section output diverged for a {}-byte input",
                t.len()
            );
        }
    }

    /// The #239 memory bug in one assertion. `String::split_off` left every
    /// emitted section holding the capacity of the whole remaining paragraph:
    /// this input measured 269 MB of capacity for 1 MB of content before the
    /// fix (and 65.6 GB peak RSS at 16 MB).
    #[test]
    fn section_capacity_tracks_content_not_input_size() {
        // ~1 MB as a single paragraph — no blank line anywhere.
        let text = "lorem ipsum dolor sit amet ".repeat((1 << 20) / 27 + 1);
        assert!(text.len() >= 1 << 20, "test input too small");
        assert!(!text.contains("\n\n"));
        let secs = split_sections(&text);
        let len: usize = secs.iter().map(String::len).sum();
        let cap: usize = secs.iter().map(String::capacity).sum();
        assert_eq!(len, text.len(), "content lost or duplicated");
        assert!(
            cap <= 2 * text.len(),
            "sections hold {cap} bytes of capacity for {len} bytes of content"
        );

        // …and the same input run through the pre-fix implementation blows the
        // bound, so the guard above genuinely catches a revert rather than
        // passing for both. (A quarter GB, once, on 1 MB of input.)
        let legacy: usize = legacy_split_sections(&text)
            .iter()
            .map(String::capacity)
            .sum();
        assert!(
            legacy > 8 * text.len(),
            "guard cannot discriminate: pre-fix capacity was only {legacy}"
        );
    }

    /// Streaming contract: a sink that stops must stop the split too. Before
    /// #239 the whole `Vec` was built first, so phase-A `--sample` paid for
    /// every section of every file it only wanted three records from.
    #[test]
    fn emit_stops_the_split_early() {
        let text = "z".repeat(400 * SECTION_CHARS);
        let mut seen = 0usize;
        let done = for_each_section(&text, &mut |_| {
            seen += 1;
            seen < 3
        });
        assert!(!done, "for_each_section reported completion after a stop");
        assert_eq!(seen, 3, "split kept going after the sink said stop");
    }

    /// `emit_document` needs one section of lookahead to know whether to stamp
    /// a `section` field; both arms must stay right.
    #[test]
    fn emit_document_labels_sections() {
        fn run(body: &str) -> Vec<(String, bool)> {
            let mut got = Vec::new();
            let mut stats = ExtractStats::default();
            let mut sink = |r: RawRecord| {
                got.push((r.locator.clone(), r.fields.contains_key("section")));
                true
            };
            assert!(emit_document(
                "t",
                &[],
                body,
                MAX_RECORDS_PER_FILE,
                &mut sink,
                &mut stats
            ));
            assert_eq!(stats.records as usize, got.len());
            got
        }
        assert_eq!(run("short body"), vec![("s0".to_string(), false)]);
        let many = run(&doc(60, 300));
        assert!(many.len() > 1);
        for (i, (loc, has_section)) in many.iter().enumerate() {
            assert_eq!(loc, &format!("s{i}"));
            assert!(
                has_section,
                "multi-section record {i} lost its section field"
            );
        }
    }

    /// Sampling sinks must see records in order and stop the run, with `stats`
    /// counting exactly what was handed over.
    #[test]
    fn emit_document_honours_an_early_stop() {
        let mut got = Vec::new();
        let mut stats = ExtractStats::default();
        let mut sink = |r: RawRecord| {
            got.push(r.locator.clone());
            got.len() < 2
        };
        assert!(!emit_document(
            "t",
            &[],
            &doc(60, 300),
            MAX_RECORDS_PER_FILE,
            &mut sink,
            &mut stats
        ));
        assert_eq!(got, vec!["s0".to_string(), "s1".to_string()]);
        assert_eq!(stats.records, 2);
    }

    /// #381: a body that sections into far more chunks than the cap yields
    /// exactly the cap and REPORTS the truncation — it is never silent.
    #[test]
    fn emit_document_caps_records_and_reports_truncation() {
        let mut got = Vec::new();
        let mut stats = ExtractStats::default();
        let mut sink = |r: RawRecord| {
            got.push(r.locator.clone());
            true
        };
        // doc(60, 300) sections into well more than 3 chunks.
        assert!(emit_document(
            "t",
            &[],
            &doc(60, 300),
            3,
            &mut sink,
            &mut stats
        ));
        assert_eq!(got.len(), 3, "record count must be bounded by the cap");
        assert_eq!(stats.records, 3);
        assert!(stats.truncated, "truncation must be reported, not silent");
    }

    /// A document within the cap indexes in full and is not flagged truncated.
    #[test]
    fn emit_document_under_the_cap_is_not_truncated() {
        let mut n = 0usize;
        let mut stats = ExtractStats::default();
        let mut sink = |_r: RawRecord| {
            n += 1;
            true
        };
        let body = doc(60, 300); // ~9 sections, well under the cap
        assert!(emit_document(
            "t",
            &[],
            &body,
            MAX_RECORDS_PER_FILE,
            &mut sink,
            &mut stats
        ));
        assert!(!stats.truncated);
        assert_eq!(stats.records as usize, n);
        assert!(
            n > 1 && n < MAX_RECORDS_PER_FILE,
            "sanity: multi-section but under the cap, got {n}"
        );
    }
}
