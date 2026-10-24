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
#[cfg(unix)]
pub(crate) mod rlimit;
pub mod sqldump;
pub mod sqlite_x;
pub mod sqlsrc;
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
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

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

#[derive(Debug, Default, Clone)]
pub struct ExtractStats {
    pub records: u64,
    pub junk: u64,
    /// Set when a per-file record cap (`MAX_RECORDS_PER_FILE`) stopped the
    /// split before the whole body was emitted (#381). Carried out so the
    /// truncation is reported, never silent.
    pub truncated: bool,
    /// Full length of each record array, keyed by the group the records were
    /// emitted under. `None` is the ungrouped array. Recorded before the
    /// sampler stops, so a 100_000-row array sampled at 500 still reports
    /// 100_000. Empty for families that do not know a length.
    pub populations: Vec<(Option<String>, u64)>,
    /// JSON values that were not indexed because one document would have
    /// exceeded [`xerj_common::document_limit::MAX_INDEXED_DOCUMENT_BYTES`]
    /// (#1122). Not a junk count: the rest of the file is still indexed.
    pub oversize_values: u64,
    /// Set when nothing from this file was indexed because of that ceiling,
    /// or because the file is larger than [`MAX_WHOLE_FILE`]. Phase A prints
    /// this instead of the generic "no records" line. `None` when the file
    /// produced records (an oversize value is then only counted above).
    pub refusal: Option<String>,
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

/// How far the paragraph-overlap tail may move back to land on a word (see
/// [`tail`]). A run with no whitespace inside the window snaps forward to the
/// next word instead, or keeps the char boundary when there is none.
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
    // A demoted catalog or bundle is re-sampled through this renderer. The
    // same skip as `extract` — otherwise the phrase text the code path
    // refused comes back in as `body`.
    if skip_phrase_index(logical_path, content_path, gzip) {
        return emit_phrase_skip(logical_path, sink);
    }
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
        None,
        MAX_RECORDS_PER_FILE,
        sink,
        &mut stats,
    );
    Ok(stats)
}

/// Bytes of a JS/CSS prefix read to decide "this is a bundle, not source".
const MINIFIED_PREFIX: u64 = 8192;
/// Below this, a one-line file is a small module, not the huge bundle the
/// #1504 report lost queries to (`quill.js`, `jssip.min.js`).
const MINIFIED_MIN_BYTES: usize = 1500;

/// Translation catalogs and minified bundles repeat every UI phrase and win
/// natural-language code queries (#1504). Named catalogs (`lang/*.php`,
/// `values*/strings.xml`, `*.lproj/*.strings`, `*.po`, `*.min.js`) are
/// decided from the logical path. A bundle that is not named `*.min.js`
/// (`quill.js`) is one enormous line; the prefix test catches that without
/// reading the rest of the file.
///
/// The phrase text is not indexed. The file still gets one title record, so
/// it stays in the corpus, and a query whose only hit would have been the
/// catalog no longer has the catalog outranking the implementing source —
/// the catalog is not a candidate. `.pyi` stubs are not skipped: `xerj def`
/// still has to resolve a name that exists only as a stub.
fn skip_phrase_index(logical: &Path, content: &Path, gzip: bool) -> bool {
    let rel = logical.to_string_lossy().replace('\\', "/");
    if xerj_common::xccode::is_translation_or_bundle(&rel) {
        return true;
    }
    js_or_css(logical) && prefix_is_minified_bundle(content, gzip)
}

fn js_or_css(logical: &Path) -> bool {
    let name = logical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = name.strip_suffix(".gz").unwrap_or(&name);
    matches!(
        name.rsplit('.').next().unwrap_or(""),
        "js" | "jsx" | "mjs" | "cjs" | "css"
    )
}

fn prefix_is_minified_bundle(path: &Path, gzip: bool) -> bool {
    let Ok(mut reader) = open_reader(path, gzip, Some(MINIFIED_PREFIX)) else {
        return false;
    };
    let mut buf = Vec::new();
    if reader.read_to_end(&mut buf).is_err() {
        return false;
    }
    if buf.len() < MINIFIED_MIN_BYTES {
        return false;
    }
    let newlines = buf.iter().filter(|b| **b == b'\n').count();
    if newlines <= 2 {
        return true;
    }
    // A short banner in front of one giant line: that line is most of the
    // prefix. A source file with one long string among real lines is not.
    let max_line = buf
        .split(|b| *b == b'\n')
        .map(|line| line.len())
        .max()
        .unwrap_or(0);
    max_line >= 4000 && max_line * 4 >= buf.len() * 3
}

/// One title record and no phrase text. `title` is the file name, which a
/// natural-language query does not contain, so the record cannot outrank
/// the implementing source. No `body`: the semantic election embeds `body`,
/// and a stub sentence here would put the catalog back into the vector leg.
fn emit_phrase_skip(logical: &Path, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let title = logical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let mut fields = Map::new();
    fields.insert("title".into(), Value::String(title));
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: "code".into(),
        group: None,
        origin: FieldOrigin::Extractor,
    });
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
    // Logical name, never the content path: durable preparation reads a
    // content-addressed blob (`blobs/00000000`) whose name is not the source
    // (#294). The bytes still come from `path`.
    let logical = sn.logical_name.as_deref().unwrap_or(path);
    if skip_phrase_index(logical, path, sn.gzip) {
        return emit_phrase_skip(logical, sink);
    }
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
        Family::SqlDump => {
            let stats = sqldump::extract(path, sn.gzip, limit_bytes, sink)?;
            // #2337: COPY dumps and other row formats this parser does not
            // read yield zero records, and `scan_file` then junks the whole
            // file — schema included. A `.sql` file in that state is source
            // when it has DDL, and an explicit skip when it is only row data.
            if stats.records == 0
                && stats.refusal.is_none()
                && crate::sniff::is_sql_filename(sn.logical_name.as_deref().unwrap_or(path))
            {
                let alt = sqlsrc::extract(
                    path,
                    sn.logical_name.as_deref().unwrap_or(path),
                    sn.gzip,
                    sink,
                )?;
                if alt.records > 0 {
                    return Ok(alt);
                }
                let mut stats = stats;
                stats.refusal = Some(sqlsrc::DATA_ONLY_DUMP_REASON.to_string());
                return Ok(stats);
            }
            Ok(stats)
        }
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

/// Last `n` bytes of `s`, snapped back to a char boundary, then onto a word
/// so the next section does not open in the middle of a token.
///
/// A raw 200-byte tail of the previous paragraph is what made a markdown
/// section of `docs/contributing/pull-requests.md` begin `le to use in tests`
/// (the tail landed inside `available`). This is the paragraph-overlap
/// path only; the hard split still cuts on a char boundary.
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
    let start = snap_overlap_start(s, start);
    s[start..].to_string()
}

/// If `start` falls inside a word, move to the nearest whitespace within
/// [`WORD_SNAP_BYTES`] behind it (the whole word stays in the overlap), or
/// forward to the next word when that lookbehind has no break.
fn snap_overlap_start(s: &str, start: usize) -> usize {
    if start == 0 || start >= s.len() {
        return start.min(s.len());
    }
    let prev_is_word = s[..start]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace());
    let next_is_word = s[start..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace());
    if !(prev_is_word && next_is_word) {
        return start;
    }
    let look = floor_boundary(s, start.saturating_sub(WORD_SNAP_BYTES));
    if let Some(rel) = s[look..start].rfind(char::is_whitespace) {
        let at = look + rel;
        let ws_len = s[at..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        let after = at + ws_len;
        if after > 0 && after < s.len() {
            return after;
        }
    }
    let rest = &s[start..];
    if let Some(rel) = rest.find(char::is_whitespace) {
        let ws_len = rest[rel..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        let after = start + rel + ws_len;
        if after < s.len() {
            return after;
        }
    }
    start
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
    for_each_section_lined(text, &mut |section, _| emit(section))
}

/// [`for_each_section`] with the 1-based line of each section's first byte
/// (#1941).
///
/// The paragraphs are the source's own `"\n\n"`-separated slices, and the
/// builder re-joins them with the same two newlines, so counting the newlines
/// in the emitted prefix gives the section's line exactly. A section that
/// starts with the overlap carry starts inside the previous section; its line
/// is the carry's own line, which is the first line a reader sees printed.
pub fn for_each_section_lined(text: &str, emit: &mut dyn FnMut(String, u64) -> bool) -> bool {
    if text.len() <= SECTION_CHARS {
        return emit(text.to_string(), 1);
    }
    // One byte past the largest section the loop below will ever hold, so a
    // full window always trips the hard-split condition.
    let window = 2 * SECTION_CHARS + 1;
    let mut cur = String::new();
    // The line `cur`'s first byte sits on; re-derived from the paragraph
    // whenever `cur` is empty at a paragraph boundary (a section start).
    let mut cur_line: u64 = 1;
    let mut pos = 0usize;
    let mut line_at_pos: u64 = 1;

    // A manual split: `split("\n\n")` yields no offsets, and rescanning for
    // each paragraph would make the walk quadratic on a large body.
    while pos <= text.len() {
        let (para, next) = match text[pos..].find("\n\n") {
            Some(rel) => (&text[pos..pos + rel], Some(pos + rel + 2)),
            None => (&text[pos..], None),
        };
        if cur.is_empty() {
            cur_line = line_at_pos;
        }
        // `cur.len() > SECTION_OVERLAP`: a shorter section would be carried
        // WHOLE into the next as its overlap, so emitting it adds a record
        // with no text of its own (a heading before a long first paragraph).
        // It stays in `cur` and leads the next section instead.
        if !cur.is_empty() && cur.len() + para.len() > SECTION_CHARS && cur.len() > SECTION_OVERLAP
        {
            let done = std::mem::take(&mut cur);
            let carry_at = done.len() - tail(&done, SECTION_OVERLAP).len();
            let carry_line =
                cur_line + done[..carry_at].bytes().filter(|b| *b == b'\n').count() as u64;
            let carried = done[carry_at..].to_string();
            let done_line = cur_line;
            if !emit(done, done_line) {
                return false;
            }
            if !carried.is_empty() {
                cur.push_str(&carried);
                cur_line = carry_line;
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
            let section_line = cur_line;
            cur.drain(..cut);
            cur_line += section.bytes().filter(|b| *b == b'\n').count() as u64;
            if !emit(section, section_line) {
                return false;
            }
        }
        let Some(next) = next else { break };
        // The two newlines the join adds are the two of the delimiter.
        line_at_pos += para.bytes().filter(|b| *b == b'\n').count() as u64 + 2;
        pos = next;
    }
    if !cur.trim().is_empty() {
        return emit(cur, cur_line);
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
    start_line: Option<u64>,
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
    // #1941: the line this section starts on in the source file. Without it a
    // prose hit has no line to cite and the CLI fell back to `ordinal + 1`
    // (`guide.md:1` for a passage at line 245). Only formats where a line
    // means something pass a base; DOCX/HTML sections carry none.
    if let Some(line) = start_line {
        fields.insert("start_line".into(), Value::Number(line.into()));
        // #2351: a `_source` include of start_line/end_line needs both.
        // The span is the lines this section actually stored.
        let span = section.lines().count().max(1) as u64;
        fields.insert("end_line".into(), Value::Number((line + span - 1).into()));
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
///
/// `base_line` is the 1-based source line of `body`'s first byte when the
/// format has real file lines (plain text/prose, #1941); the records then carry
/// `start_line`. `None` for formats where a line is meaningless (DOCX, HTML).
pub fn emit_document(
    title: &str,
    headings: &[String],
    body: &str,
    base_line: Option<u64>,
    max_records: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    // One section of lookahead: the `section` field is only present when the
    // document actually splits, which is not known until a second one arrives.
    let mut pending: Option<String> = None;
    // The lookahead's line travels with it: `for_each_section_lined` reports
    // the section it hands over, and we emit the PREVIOUS one.
    let mut pending_line: u64 = 1;
    let mut next: usize = 0;
    let mut alive = true;
    let line_of = |section_line: u64| base_line.map(|base| base + section_line - 1);
    for_each_section_lined(body, &mut |sec, line| {
        let Some(prev) = pending.replace(sec) else {
            pending_line = line;
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
        alive = sink(section_record(
            title,
            headings,
            i,
            true,
            prev,
            line_of(pending_line),
        ));
        pending_line = line;
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
            sink(section_record(
                title,
                headings,
                next,
                next > 0,
                last,
                line_of(pending_line),
            ))
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
    let mut secs: Vec<String> = Vec::new();
    for_each_section(body, &mut |s| {
        secs.push(s);
        secs.len() <= MAX_RECORDS_PER_FILE
    });
    if secs.len() > MAX_RECORDS_PER_FILE {
        secs.truncate(MAX_RECORDS_PER_FILE);
        stats.truncated = true;
    }
    if secs.is_empty() {
        secs.push(String::new());
    }
    let multi = secs.len() > 1;
    for (i, sec) in secs.into_iter().enumerate() {
        let mut fields = base_fields.clone();
        fields.insert("title".into(), Value::String(title.to_string()));
        fields.insert("body".into(), Value::String(sec));
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

/// How often a full extract slot says it is still waiting. The wait is not
/// failed: a legal PDF or mailbox parse can hold its slot for a long time.
/// The line exists so a leaked permit is not a silent futex.
pub(crate) const SLOT_HEARTBEAT: Duration = Duration::from_secs(15);

pub(crate) fn slot_wait_line(kind: &str, in_use: usize, limit: usize, waited: Duration) -> String {
    format!(
        "autoindex: still waiting for a {kind} slot (in_use={in_use} limit={limit}) after {}s",
        waited.as_secs()
    )
}

/// Block until `full` is false. Each wait is sliced so a notify is rechecked
/// and a stuck slot prints [`slot_wait_line`] on `heartbeat` cadence. The
/// caller adds the permit after this returns; this function does not.
pub(crate) fn wait_until_slot<'a, T>(
    kind: &str,
    lock: &'a Mutex<T>,
    ready: &Condvar,
    heartbeat: Duration,
    full: impl Fn(&T) -> bool,
    counts: impl Fn(&T) -> (usize, usize),
    mut note: impl FnMut(&str),
) -> std::sync::MutexGuard<'a, T> {
    let heartbeat = heartbeat.max(Duration::from_millis(1));
    let mut guard = lock.lock().expect("extract slot lock poisoned");
    let started = Instant::now();
    let mut next_note = started + heartbeat;
    while full(&guard) {
        let now = Instant::now();
        if now >= next_note {
            let (in_use, limit) = counts(&guard);
            note(&slot_wait_line(
                kind,
                in_use,
                limit,
                now.duration_since(started),
            ));
            next_note = now + heartbeat;
        }
        let slice = next_note
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5));
        let (next, _) = ready
            .wait_timeout(guard, slice)
            .expect("extract slot lock poisoned");
        guard = next;
    }
    guard
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

    /// Paragraph overlap used to keep a raw 200-byte tail, so the next
    /// section opened inside a word (`le to use` from `available`). The
    /// overlap still repeats the boundary, and it starts on a word.
    #[test]
    fn paragraph_overlap_opens_on_a_word() {
        let s = "utilities available to use in tests, no matter the crate";
        // "utilities " is 10 bytes; index 13 is inside "available".
        let opened = tail(s, s.len() - 13);
        assert!(
            opened.starts_with("available"),
            "overlap opened mid-word: {opened}"
        );

        // A token longer than the lookbehind has no whitespace behind the
        // cut, so the overlap skips forward to the next word instead.
        let long = "A".repeat(WORD_SNAP_BYTES + 8);
        let big = format!("xx{long} tail of the paragraph");
        let mid = 2 + 4; // inside the run of A's
        let opened = tail(&big, big.len() - mid);
        assert!(
            opened.starts_with("tail"),
            "long token should snap forward: {opened:?}"
        );

        let para = format!(
            "{}utilities available to use in tests",
            "pad ".repeat(SECTION_CHARS)
        );
        let text = format!(
            "{para}\n\n{}",
            "The next paragraph is long enough to force a new section. ".repeat(40)
        );
        let secs = split_sections(&text);
        assert!(secs.len() >= 2, "{}", secs.len());
        let next = &secs[1];
        let at = text.find(next.as_str()).expect("overlap is a substring");
        if at > 0 {
            let prev = text[..at].chars().next_back().unwrap();
            assert!(
                prev.is_whitespace(),
                "section opened mid-word ({prev:?}): {:?}",
                &next[..next.len().min(48)]
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
                None,
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
            None,
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
            None,
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
            None,
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
        assert!(emit_document(
            "t",
            &[],
            &body,
            None,
            total,
            &mut sink,
            &mut stats
        ));
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
            None,
            total - 1,
            &mut sink2,
            &mut stats2
        ));
        assert_eq!(over, total - 1, "N>cap must emit exactly the cap");
        assert!(stats2.truncated, "N>cap must set truncated");
    }

    /// #1941: `base_line` shifts every section's `start_line`, and each record
    /// must point at the line its first printed text starts on. The source
    /// itself is the oracle: a section that starts mid-line (an overlap carry
    /// or a hard cut) starts with a SUFFIX of that source line.
    #[test]
    fn emit_document_stamps_section_start_lines() {
        let body = doc(60, 300);
        let mut got: Vec<(u64, String)> = Vec::new();
        let mut stats = ExtractStats::default();
        let mut sink = |r: RawRecord| {
            got.push((
                r.fields["start_line"]
                    .as_u64()
                    .expect("a line-bearing document stamps start_line"),
                r.fields["body"].as_str().unwrap().to_string(),
            ));
            true
        };
        assert!(emit_document(
            "t",
            &[],
            &body,
            Some(10),
            MAX_RECORDS_PER_FILE,
            &mut sink,
            &mut stats
        ));
        assert!(got.len() > 1, "the fixture must split");
        assert_eq!(got[0].0, 10, "the first section starts at base_line");
        // The sink above kept only start_line. Re-read end_line from a
        // one-section document: it is the last stored line, not a copy of
        // the start when the body spans more than one line.
        let mut one = Vec::new();
        let mut one_stats = ExtractStats::default();
        assert!(emit_document(
            "t",
            &[],
            "alpha\nbeta\n",
            Some(4),
            MAX_RECORDS_PER_FILE,
            &mut |r| {
                one.push(r);
                true
            },
            &mut one_stats
        ));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].fields["start_line"], Value::Number(4.into()));
        assert_eq!(one[0].fields["end_line"], Value::Number(5.into()));
        let lines: Vec<&str> = body.lines().collect();
        for (line, section) in &got {
            let first = section.lines().next().unwrap_or("");
            let source = lines
                .get(*line as usize - 10)
                .unwrap_or_else(|| panic!("start_line {line} is past the body"));
            assert!(
                source.ends_with(first),
                "start_line {line} does not hold the section start: {first:?} vs {source:?}"
            );
        }

        // `None` (DOCX/HTML) stamps no line at all.
        let mut none_stats = ExtractStats::default();
        let mut none_sink = |r: RawRecord| {
            assert!(
                r.fields.get("start_line").is_none(),
                "a line-less format must not invent one"
            );
            true
        };
        assert!(emit_document(
            "t",
            &[],
            "short body",
            None,
            MAX_RECORDS_PER_FILE,
            &mut none_sink,
            &mut none_stats
        ));
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn slot_wait_line_names_the_slot_and_how_long() {
        let line = slot_wait_line("pdf worker", 4, 4, Duration::from_secs(45));
        assert_eq!(
            line,
            "autoindex: still waiting for a pdf worker slot (in_use=4 limit=4) after 45s"
        );
    }

    /// A full slot used to park on `Condvar::wait` with no timeout and no
    /// line. The waiter must say so, then proceed when the holder releases.
    #[test]
    fn a_full_extract_slot_heartbeats_and_then_proceeds() {
        let lock = Arc::new(Mutex::new((1usize, 1usize)));
        let ready = Arc::new(Condvar::new());
        let notes = Arc::new(Mutex::new(Vec::new()));
        let lock2 = Arc::clone(&lock);
        let ready2 = Arc::clone(&ready);
        let notes2 = Arc::clone(&notes);
        let waiter = thread::spawn(move || {
            let guard = wait_until_slot(
                "pdf worker",
                &lock2,
                &ready2,
                Duration::from_millis(20),
                |state| state.0 >= state.1,
                |state| (state.0, state.1),
                |line| notes2.lock().expect("notes").push(line.to_string()),
            );
            guard.0
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while notes.lock().expect("notes").is_empty() {
            assert!(
                Instant::now() < deadline,
                "a full slot must heartbeat before the holder releases it"
            );
            thread::sleep(Duration::from_millis(10));
        }
        {
            let mut guard = lock.lock().expect("lock");
            guard.0 = 0;
            ready.notify_all();
        }
        assert_eq!(waiter.join().expect("waiter"), 0);
        let lines = notes.lock().expect("notes");
        assert!(
            lines[0].contains("pdf worker")
                && lines[0].contains("in_use=1")
                && lines[0].contains("limit=1"),
            "{lines:?}"
        );
    }
}

#[cfg(test)]
mod phrase_skip_tests {
    use super::*;
    use crate::sniff::{Family, Sniffed};

    const PHRASE: &str = "push notification wait for mobile device to register before dial";

    fn sniffed(family: Family, logical: &str) -> Sniffed {
        Sniffed {
            family,
            gzip: false,
            binary_kind: None,
            csv: None,
            encoding: "utf-8",
            logical_name: Some(std::path::PathBuf::from(logical)),
        }
    }

    fn indexed_text(logical: &str, family: Family, bytes: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        std::fs::write(&path, bytes).unwrap();
        let sn = sniffed(family, logical);
        let mut recs = Vec::new();
        extract(&path, &sn, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        recs.iter()
            .flat_map(|r| r.fields.values())
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #1504: a multilingual repo's catalogs contain every UI phrase once per
    /// language and fill the fetch window, so reordering the hits that came
    /// back cannot surface the implementing file. The phrase must not be
    /// indexed from the catalog. The service file that implements it keeps it.
    #[test]
    fn translation_catalogs_do_not_index_the_phrase_the_source_keeps() {
        let catalog = format!("<?php\nreturn [\n  'dial.wait' => '{PHRASE}',\n];\n");
        let from_catalog = indexed_text("web/lang/en.php", Family::Code, &catalog);
        assert!(
            !from_catalog.contains(PHRASE),
            "lang/*.php indexed the UI phrase: {from_catalog}"
        );
        assert!(
            from_catalog.contains("en.php"),
            "the catalog file stays in the corpus by name"
        );

        let strings = format!(
            "<?xml version=\"1.0\"?>\n<resources>\n  <string name=\"dial_wait\">{PHRASE}</string>\n</resources>\n"
        );
        let from_xml = indexed_text("res/values-da/strings.xml", Family::Xml, &strings);
        assert!(
            !from_xml.contains(PHRASE),
            "strings.xml indexed the phrase: {from_xml}"
        );

        let po = format!("msgid \"{PHRASE}\"\nmsgstr \"{PHRASE}\"\n");
        let from_po = indexed_text("locale/da.po", Family::TxtProse, &po);
        assert!(
            !from_po.contains(PHRASE),
            ".po indexed the phrase: {from_po}"
        );

        let apple = format!("\"dial.wait\" = \"{PHRASE}\";\n");
        let from_strings =
            indexed_text("ios/da.lproj/Localizable.strings", Family::TxtLines, &apple);
        assert!(
            !from_strings.contains(PHRASE),
            ".strings indexed the phrase: {from_strings}"
        );

        let source = format!(
            "<?php\nfunction waitForMobile() {{\n  // {PHRASE}\n  return $this->dial();\n}}\n"
        );
        let from_source = indexed_text(
            "web/src/services/phones/NecTemplate.php",
            Family::Code,
            &source,
        );
        assert!(
            from_source.contains(PHRASE),
            "the implementing file must stay searchable: {from_source}"
        );
    }

    /// #1504: `quill.js` is one line and is not named `*.min.js`, so the path
    /// rule does not see it. A normal source file with the same phrase, wrapped
    /// like source, stays indexed. A named `*.min.js` is skipped even when short.
    #[test]
    fn one_line_js_bundles_are_skipped_and_source_js_is_not() {
        let pad = "x".repeat(1800);
        let bundle = format!("!function(){{var s=\"{PHRASE}\";{pad}}}();");
        assert!(
            bundle.len() > MINIFIED_MIN_BYTES,
            "fixture must clear the size floor, got {}",
            bundle.len()
        );
        assert!(!bundle.contains('\n'));
        let from_bundle = indexed_text("assets/quill.js", Family::Code, &bundle);
        assert!(
            !from_bundle.contains(PHRASE),
            "one-line quill.js indexed the phrase: {from_bundle}"
        );

        let named = format!("/* {PHRASE} */\n");
        let from_min = indexed_text("assets/jssip.min.js", Family::Code, &named);
        assert!(
            !from_min.contains(PHRASE),
            "named min.js indexed the phrase: {from_min}"
        );

        let mut source = format!("function waitForMobile() {{\n  // {PHRASE}\n");
        for i in 0..40 {
            source.push_str(&format!("  var line{i} = {i};\n"));
        }
        source.push_str("}\n");
        let from_source = indexed_text("web/src/dial.js", Family::Code, &source);
        assert!(
            from_source.contains(PHRASE),
            "ordinary javascript must stay searchable: {from_source}"
        );
    }

    /// A type stub stays indexed. Query-time demotion ranks it behind the
    /// implementation; stripping it would hide a name that exists only here.
    #[test]
    fn pyi_stubs_keep_their_text() {
        let stub = "def wait_for_register() -> None:\n    \"\"\"push notification wait\"\"\"\n";
        let text = indexed_text("candle/quantized.pyi", Family::Code, stub);
        assert!(
            text.contains("wait_for_register"),
            "a stub must stay indexed for xerj def: {text}"
        );
    }
}
