//! Streaming record extraction — one module per format family.
//! Every extractor is bounded-memory and never fatal: parse failures
//! downgrade (family → txt → junk-with-metadata) and are counted.

pub mod bvh;
pub mod code;
pub mod csv_x;
pub mod doc;
pub mod docx;
pub mod eml;
pub mod epub;
pub mod html;
pub mod ipynb;
pub mod json;
pub mod jsonl;
pub mod logs;
pub mod man;
pub mod mbox;
pub(crate) mod opc;
pub mod pdf;
pub mod pptx;
pub mod sqldump;
pub mod sqlite_x;
pub mod txt;
pub mod unity;
pub mod xls;
pub mod xlsx;
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

/// How far a hard split may move back from `SECTION_CHARS` to land on
/// whitespace. A run with no whitespace inside the window keeps the char
/// boundary, so multi-byte scalars and unbroken tokens are never severed
/// just to chase a word that is not there.
const WORD_SNAP_BYTES: usize = 64;

/// Open a (possibly gzipped) file as a buffered reader of DECODED-transparent
/// bytes, optionally capped at `limit` decoded bytes (sampling).
pub fn open_reader(path: &Path, gzip: bool, limit: Option<u64>) -> Result<Box<dyn BufRead + Send>> {
    let f = std::fs::File::open(path)?;
    let inner: Box<dyn Read + Send> = if gzip {
        Box::new(flate2::read::MultiGzDecoder::new(f))
    } else {
        Box::new(f)
    };
    // `Send` so a container extractor (mbox) can move the reader to its
    // splitter thread; every concrete reader here is.
    let inner: Box<dyn Read + Send> = match limit {
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
    let skipped = text.len() - text.trim_start().len();
    let skipped_lines = text[..skipped].bytes().filter(|b| *b == b'\n').count() as u64;
    emit_document_from(
        skipped_lines,
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
        Family::Eml => eml::extract(path, sn.gzip, sink),
        Family::Mbox => mbox::extract(path, sn.gzip, limit_bytes, sink),
        Family::Docx => docx::extract(path, sink),
        Family::Doc => doc::extract(path, sn.logical_name.as_deref().unwrap_or(path), sink),
        Family::Pptx => pptx::extract(path, sn.logical_name.as_deref().unwrap_or(path), sink),
        Family::Epub => epub::extract(path, sn.logical_name.as_deref().unwrap_or(path), sink),
        Family::Ipynb => ipynb::extract(
            path,
            sn.logical_name.as_deref().unwrap_or(path),
            sn.gzip,
            sink,
        ),
        Family::Man => man::extract(
            path,
            sn.logical_name.as_deref().unwrap_or(path),
            sn.gzip,
            sink,
        ),
        Family::Xlsx => xlsx::extract(
            path,
            sn.logical_name.as_deref().unwrap_or(path),
            limit_bytes.map(|_| 500),
            sink,
        ),
        Family::Xls => xls::extract(
            path,
            sn.logical_name.as_deref().unwrap_or(path),
            limit_bytes.map(|_| 500),
            sink,
        ),
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

/// A nested object or array stored as one JSON string. Past this, the
/// string is cut. A 12 MB nested value used to become one text field, and
/// analysis of that field wedged ingest. The cap is the bytes kept before
/// the truncation marker; the cut is on a char boundary.
pub const NESTED_JSON_FIELD_MAX: usize = 64 * 1024;

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
                out.insert(key.to_string(), Value::String(bounded_json(&m)));
            }
        }
        Value::Array(a) => {
            if a.iter().all(|e| !e.is_object() && !e.is_array()) {
                out.insert(key.to_string(), Value::Array(a));
            } else {
                out.insert(key.to_string(), Value::String(bounded_json(&a)));
            }
        }
        other => {
            out.insert(key.to_string(), other);
        }
    }
}

/// JSON for a nested value that will be stored as one string field.
///
/// `serde_json::to_string` of a value already in memory is cheap. What is
/// not cheap is handing the whole string to analysis: a 12 MB nested
/// document becomes one text field and ingest stops making progress.
/// Values under the cap are unchanged, so a small nested object still
/// round-trips.
fn bounded_json(value: &impl serde::Serialize) -> String {
    let full = serde_json::to_string(value).unwrap_or_default();
    if full.len() <= NESTED_JSON_FIELD_MAX {
        return full;
    }
    let mut end = NESTED_JSON_FIELD_MAX.min(full.len());
    while end > 0 && !full.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = String::with_capacity(end + 16);
    cut.push_str(&full[..end]);
    cut.push_str("…[truncated]");
    cut
}

/// A string field counts as prose at this many characters …
const PASSAGE_PROSE_MIN_CHARS: usize = 24;
/// … and this many whitespace-separated tokens. Below both, a string is an
/// id, an enum or a date — the values that map `keyword` and leave a record
/// invisible to passage search.
const PASSAGE_PROSE_MIN_TOKENS: usize = 4;
/// Cap on a synthesized passage, in characters. The passage exists so a
/// record's own field names and values are matchable; past this size the
/// record is prose-like and synthesis never ran anyway.
pub const SYNTH_PASSAGE_MAX_CHARS: usize = 4 << 10;
/// Cap per rendered value inside a synthesized passage, in characters.
const SYNTH_PASSAGE_VALUE_CHARS: usize = 256;

/// Does this record already carry a string field that reads as prose? Such a
/// field maps `text` and the query side already searches it (`resolve_fields`
/// includes `text` and, since rc.86, falls back to the index's own text-typed
/// fields — `xerj-common/src/xccode/fields.rs`), so synthesizing anything
/// would only shadow it.
fn carries_prose(fields: &Map<String, Value>) -> bool {
    fields.values().any(|v| {
        v.as_str().is_some_and(|s| {
            let t = s.trim();
            t.chars().count() >= PASSAGE_PROSE_MIN_CHARS
                && t.split_whitespace().count() >= PASSAGE_PROSE_MIN_TOKENS
        })
    })
}

/// #1158's invisible-record class, fixed at the source: a JSON record whose
/// flattened fields carry no prose-like string is a row of ids, enums and
/// numbers — nothing `xerj code` passage search can match, because it reads
/// `body`/`defs`/`title`/`text` and none of them exist. Measured on the live
/// case: `ghsa-db`, 36,263 raw advisory JSONs indexed to 193k typed records
/// that answered `No passage matches` while the same index returned 10,000+
/// hits over its typed fields. This synthesizes a `text` passage from the
/// record's own fields (`key: value` lines, bounded) so the corpus is
/// passage-searchable by default instead of only after an undocumented
/// pre-flatten workaround.
///
/// Bounds and rules:
/// - Never clobbers an existing `text` field (even a short one — the record
///   owns that name, and its dataset already maps it).
/// - Only runs when the record carries NO prose field: a record with real
///   prose is already reachable through the query-side field resolution, and
///   a rendered key/value soup would outrank that prose in the field order.
/// - Values are capped ([`SYNTH_PASSAGE_VALUE_CHARS`]) and the passage with
///   them ([`SYNTH_PASSAGE_MAX_CHARS`]); scalar arrays (ids, aliases) render
///   joined so an identifier query can reach the record.
/// - Scoped to the JSON family (whole-file and JSONL) — the measured class is
///   raw-JSON mirrors. The same helper can ride the csv/yaml/xml Data sites
///   if a real corpus ever needs it; until then those families are covered
///   by the dataset-level zero-text warning instead.
///
/// Returns whether a passage was inserted, so the call sites' tests can pin
/// the predicate without reading the record back out.
pub(crate) fn ensure_text_passage(fields: &mut Map<String, Value>) -> bool {
    if carries_prose(fields) || fields.contains_key("text") {
        return false;
    }
    let mut passage = String::new();
    for (k, v) in fields.iter() {
        let render_value = |txt: &mut String, s: &str| {
            // Char cap, checked in chars: a byte check would stop a multibyte
            // passage at a third of its budget (the values that trip it are
            // exactly the ones already capped to 256 chars, so this recount
            // is bounded and cheap).
            if txt.chars().count() >= SYNTH_PASSAGE_MAX_CHARS {
                return;
            }
            if !txt.is_empty() {
                txt.push('\n');
            }
            txt.push_str(k);
            txt.push_str(": ");
            let cut = s
                .trim()
                .chars()
                .take(SYNTH_PASSAGE_VALUE_CHARS)
                .collect::<String>();
            txt.push_str(&cut);
        };
        match v {
            Value::String(s) => render_value(&mut passage, s),
            Value::Number(n) => render_value(&mut passage, &n.to_string()),
            Value::Bool(b) => render_value(&mut passage, &b.to_string()),
            // Scalar arrays (aliases, tags, versions) join with ", " — their
            // identifiers are exactly what a passage exists to make matchable.
            Value::Array(a) if a.iter().all(|e| !e.is_object() && !e.is_array()) => {
                let joined = a
                    .iter()
                    .map(|e| match e {
                        Value::String(s) => s.trim().to_string(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                render_value(&mut passage, &joined);
            }
            _ => {}
        }
        if passage.chars().count() >= SYNTH_PASSAGE_MAX_CHARS {
            break;
        }
    }
    if passage.is_empty() {
        return false;
    }
    // The per-line cap check runs before each append, so a final line can
    // push past the bound by one line's worth. Enforce it exactly, on a
    // char boundary (a byte slice could land mid-character: panic = abort).
    if passage.chars().count() > SYNTH_PASSAGE_MAX_CHARS {
        passage = passage
            .chars()
            .take(SYNTH_PASSAGE_MAX_CHARS)
            .collect::<String>();
    }
    fields.insert("text".to_string(), Value::String(passage));
    true
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

/// Cut `cur` near `target` bytes. Prefer the nearest whitespace at or before
/// `target` within [`WORD_SNAP_BYTES`], and cut *after* that whitespace so
/// the next section starts on a word. No whitespace in the window → the char
/// boundary at `target`, which never splits a scalar.
fn cut_on_word_or_char(cur: &str, target: usize) -> usize {
    let boundary = ceil_boundary(cur, target.min(cur.len()));
    if boundary == 0 || boundary >= cur.len() {
        return boundary;
    }
    let look = floor_boundary(cur, boundary.saturating_sub(WORD_SNAP_BYTES));
    let window = &cur[look..boundary];
    if let Some(rel) = window.rfind(char::is_whitespace) {
        let at = look + rel;
        let ws_len = cur[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        let after = at + ws_len;
        if after > 0 && after < cur.len() {
            return after;
        }
    }
    boundary
}

/// 1-based line number of `text` at byte offset `byte`.
fn line_number_at(text: &str, byte: usize) -> u64 {
    let byte = byte.min(text.len());
    1 + text[..byte].bytes().filter(|b| *b == b'\n').count() as u64
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
    for_each_section_spans(text, &mut |_at, section| emit(section))
}

/// [`for_each_section`] that also reports the byte offset in `text` where each
/// section begins. Paragraph overlap is included: a section that repeats the
/// previous tail starts at that tail.
fn for_each_section_spans(text: &str, emit: &mut dyn FnMut(usize, String) -> bool) -> bool {
    if text.len() <= SECTION_CHARS {
        return emit(0, text.to_string());
    }
    // One byte past the largest section the loop below will ever hold, so a
    // full window always trips the hard-split condition.
    let window = 2 * SECTION_CHARS + 1;
    let mut cur = String::new();
    // Byte offset in `text` of `cur`'s first byte once `cur` is non-empty.
    let mut origin = 0usize;
    let mut pos = 0usize;

    for para in text.split("\n\n") {
        // `cur.len() > SECTION_OVERLAP`: a shorter section would be carried
        // WHOLE into the next as its overlap, so emitting it adds a record
        // with no text of its own (a heading before a long first paragraph).
        // It stays in `cur` and leads the next section instead.
        if !cur.is_empty() && cur.len() + para.len() > SECTION_CHARS && cur.len() > SECTION_OVERLAP
        {
            let done = std::mem::take(&mut cur);
            let start = origin;
            let carry = tail(&done, SECTION_OVERLAP);
            origin = start + done.len() - carry.len();
            if !carry.is_empty() {
                cur.push_str(&carry);
            }
            if !emit(start, done) {
                return false;
            }
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        } else {
            origin = pos;
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
            let cut = cut_on_word_or_char(&cur, SECTION_CHARS);
            let section = cur[..cut].to_string();
            let start = origin;
            cur.drain(..cut);
            origin += cut;
            if !emit(start, section) {
                return false;
            }
        }
        pos += para.len();
        if pos < text.len() {
            // The `\n\n` separator `split` consumed.
            pos += 2;
        }
    }
    if !cur.trim().is_empty() {
        return emit(origin, cur);
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
    start_line: u64,
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
    fields.insert("start_line".into(), Value::Number(start_line.into()));
    RawRecord {
        fields,
        locator: format!("s{i}"),
        group: None,
        // title/headings/section/body/start_line are this function's vocabulary,
        // and `headings`/`section` come and go — never a clustering key.
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
    emit_document_from(0, title, headings, body, max_records, sink, stats)
}

/// [`emit_document`] whose `start_line` values are shifted by `skipped_lines`.
///
/// Callers that strip leading whitespace before sectioning (prose) pass the
/// number of newlines they removed, so `start_line` stays a file line.
pub(crate) fn emit_document_from(
    skipped_lines: u64,
    title: &str,
    headings: &[String],
    body: &str,
    max_records: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    // One section of lookahead: the `section` field is only present when the
    // document actually splits, which is not known until a second one arrives.
    let mut pending: Option<(usize, String)> = None;
    let mut next: usize = 0;
    let mut alive = true;
    for_each_section_spans(body, &mut |at, sec| {
        let Some((prev_at, prev)) = pending.replace((at, sec)) else {
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
        let line = skipped_lines + line_number_at(body, prev_at);
        alive = sink(section_record(title, headings, i, true, prev, line));
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
        Some((at, last)) => {
            if next >= max_records {
                stats.truncated = true;
                return true;
            }
            stats.records += 1;
            let line = skipped_lines + line_number_at(body, at);
            sink(section_record(title, headings, next, next > 0, last, line))
        }
        None => true,
    }
}

/// [`emit_document`] for a document that carries FIELDS of its own: emit `body`
/// as one or more section records, each stamped with `base_fields` (an email's
/// headers, an attachment's link-back fields, a note's labels) plus
/// `title`/`body`/`section`. Locators are `{loc_prefix}-s{i}`.
///
/// Returns the sink's last answer: `false` = stop extracting.
pub(crate) fn emit_document_with_fields(
    base_fields: &Map<String, Value>,
    title: &str,
    body: &str,
    loc_prefix: &str,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    // Collect sections first so we know whether to stamp a `section` field.
    // One past the cap is collected so that "exactly at the cap" and "over the
    // cap" are distinguishable: only the latter dropped anything (#381).
    let mut secs: Vec<(usize, String)> = Vec::new();
    for_each_section_spans(body, &mut |at, s| {
        secs.push((at, s));
        secs.len() <= MAX_RECORDS_PER_FILE
    });
    if secs.len() > MAX_RECORDS_PER_FILE {
        secs.truncate(MAX_RECORDS_PER_FILE);
        stats.truncated = true;
    }
    if secs.is_empty() {
        secs.push((0, String::new()));
    }
    let multi = secs.len() > 1;
    for (i, (at, sec)) in secs.into_iter().enumerate() {
        let mut fields = base_fields.clone();
        fields.insert("title".into(), Value::String(title.to_string()));
        fields.insert("body".into(), Value::String(sec));
        fields.insert(
            "start_line".into(),
            Value::Number(line_number_at(body, at).into()),
        );
        if multi {
            fields.insert("section".into(), Value::Number((i as u64).into()));
        }
        stats.records += 1;
        if !sink(RawRecord {
            fields,
            locator: format!("{loc_prefix}-s{i}"),
            group: None,
            origin: FieldOrigin::Extractor,
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

    /// A short paragraph (a heading) followed by one too big to join it was
    /// emitted as a section of its own, and the next section then repeated it
    /// whole as its overlap: a record with no text of its own. Seen as
    /// "CHAPTER 26. Knights and Squires." alone on a Gutenberg EPUB.
    #[test]
    fn a_section_the_next_would_repeat_whole_is_not_emitted() {
        // Each ~2100 bytes: too big to join the heading in one section.
        let long = |i: usize| format!("w{i} ").repeat(700);
        let t = format!(
            "CHAPTER 26. Knights and Squires.\n\n{}\n\n{}\n\n{}",
            long(1),
            long(2),
            long(3)
        );
        let secs = split_sections(&t);
        assert!(
            secs[0].starts_with("CHAPTER 26.") && secs[0].contains("w1 w1"),
            "the heading leads a real section: {:?}",
            &secs[0][..secs[0].len().min(60)]
        );
        for w in secs.windows(2) {
            assert!(
                !w[1].starts_with(w[0].as_str()),
                "a section repeated whole by its successor: {:?}",
                w[0]
            );
        }
        let short = format!("{}\n\nCHAPTER 26.", "y".repeat(SECTION_CHARS));
        assert!(
            split_sections(&short)
                .last()
                .unwrap()
                .contains("CHAPTER 26."),
            "a short LAST paragraph is still emitted"
        );
    }

    /// A single spaced paragraph is cut after a word, and the pieces still
    /// partition the input: nothing dropped, nothing repeated. A hard split
    /// does not overlap (overlap is the paragraph-boundary path).
    #[test]
    fn hard_split_of_spaced_prose_cuts_on_a_word_boundary() {
        let text = "alpha ".repeat(SECTION_CHARS);
        assert!(!text.contains('\n'));
        let secs = split_sections(&text);
        assert!(secs.len() > 2, "expected a hard split, got {}", secs.len());
        let mut pos = 0usize;
        for (i, section) in secs.iter().enumerate() {
            assert_eq!(
                &text[pos..pos + section.len()],
                section.as_str(),
                "section {i} dropped or duplicated content"
            );
            if i + 1 < secs.len() {
                assert!(
                    section.ends_with(' '),
                    "section {i} ends mid-word: {:?}",
                    &section[section.len().saturating_sub(24)..]
                );
                assert!(
                    secs[i + 1].starts_with("alpha"),
                    "section {} starts mid-word: {:?}",
                    i + 1,
                    &secs[i + 1][..secs[i + 1].len().min(24)]
                );
            }
            pos += section.len();
        }
        assert_eq!(pos, text.len(), "sections did not cover the input");
    }

    /// `start_line` is the file line of the section's first byte, not the
    /// passage ordinal and not always 1.
    #[test]
    fn section_records_carry_the_source_line() {
        let body = "word\n".repeat(SECTION_CHARS);
        assert!(!body.contains("\n\n"));
        let mut lines = Vec::new();
        let mut stats = ExtractStats::default();
        let mut sink = |r: RawRecord| {
            lines.push(r.fields["start_line"].as_u64().unwrap());
            true
        };
        assert!(emit_document(
            "t",
            &[],
            &body,
            MAX_RECORDS_PER_FILE,
            &mut sink,
            &mut stats
        ));
        let secs = split_sections(&body);
        assert_eq!(lines.len(), secs.len());
        assert!(secs.len() > 1);
        let mut at = 0usize;
        for (i, section) in secs.iter().enumerate() {
            let expect = 1 + body[..at].bytes().filter(|b| *b == b'\n').count() as u64;
            assert_eq!(lines[i], expect, "section {i}");
            if i > 0 {
                assert!(lines[i] > lines[i - 1]);
            }
            at += section.len();
        }
        assert_eq!(lines[0], 1);
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

    /// The pre-#239 implementation, kept as an oracle (verbatim but for the
    /// redundant-section rule, changed in both in step). It is quadratic
    /// — only ever call it on inputs of a few hundred KB.
    fn legacy_split_sections(text: &str) -> Vec<String> {
        if text.len() <= SECTION_CHARS {
            return vec![text.to_string()];
        }
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
        for para in text.split("\n\n") {
            // The one deliberate change since #239, mirrored from
            // `for_each_section`: a section its successor would repeat whole
            // is not emitted.
            if !cur.is_empty()
                && cur.len() + para.len() > SECTION_CHARS
                && cur.len() > SECTION_OVERLAP
            {
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

    /// #239 rewrote the splitter for cost, not for behaviour: every input that
    /// does not hard-split on whitespace must still produce byte-identical
    /// sections. Multi-byte cases are the ones a hand-rolled boundary walk
    /// gets wrong, so they are all in here. Spaced hard splits snap to a word
    /// and are asserted separately.
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
            // doc(3, 9000) hard-splits inside a spaced paragraph. Word snapping
            // intentionally moves that cut; it is covered by
            // `hard_split_of_spaced_prose_cuts_on_a_word_boundary` instead.
            // A short paragraph before ones too big to join it, at the
            // overlap boundary and one byte either side.
            format!("{}\n\n{}", "h".repeat(32), doc(4, 2100)),
            format!("{}\n\n{}", "h".repeat(SECTION_OVERLAP - 1), doc(4, 2100)),
            format!("{}\n\n{}", "h".repeat(SECTION_OVERLAP), doc(4, 2100)),
            format!("{}\n\n{}", "h".repeat(SECTION_OVERLAP + 1), doc(4, 2100)),
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

    /// #381 boundary (from the #758 correctness review): a body of EXACTLY the
    /// cap emits every section and is NOT flagged truncated; one section over
    /// emits exactly the cap and IS truncated, dropping the tail. Driven by the
    /// body's real section count so it pins the off-by-one directly.
    #[test]
    fn emit_document_boundary_at_and_one_over_the_cap() {
        let body = doc(60, 300);
        let total = split_sections(&body).len();
        assert!(total >= 3, "need a multi-section body, got {total}");

        // N == cap: all emitted, not truncated.
        let mut at = 0usize;
        let mut stats = ExtractStats::default();
        let mut sink = |_r: RawRecord| {
            at += 1;
            true
        };
        assert!(emit_document("t", &[], &body, total, &mut sink, &mut stats));
        assert_eq!(at, total, "N==cap must emit every section");
        assert!(!stats.truncated, "N==cap must NOT set truncated");

        // N == cap + 1 (cap = total - 1): exactly cap emitted, tail dropped.
        let mut over = 0usize;
        let mut stats2 = ExtractStats::default();
        let mut sink2 = |_r: RawRecord| {
            over += 1;
            true
        };
        assert!(emit_document(
            "t",
            &[],
            &body,
            total - 1,
            &mut sink2,
            &mut stats2
        ));
        assert_eq!(over, total - 1, "N>cap must emit exactly the cap");
        assert!(stats2.truncated, "N>cap must set truncated");
    }
}
