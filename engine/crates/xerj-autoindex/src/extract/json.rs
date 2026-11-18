//! Whole-value JSON files (object or array), capped at 64MB.
//! - array of objects  → one record per element
//! - object with one dominant top-level array of objects → one record per
//!   element, remaining top-level scalars merged in as shared fields
//! - object with two or more top-level arrays of objects, the largest longer
//!   than a config document → one dataset per array. The largest keeps the
//!   ungrouped locators (`{key}:e{i}`); each other array is its own group.
//!   Sibling arrays are not copied onto each other's rows (#1856, #1978)
//! - a shared value bigger than [`SHARED_REPLICATE_MAX`] is emitted once,
//!   not cloned onto every row
//! - a Google Keep note (Takeout `Keep/*.json`) → a DOCUMENT: `title`/`body`
//!   plus labels and dates — see [`keep_note`]
//! - anything else → a single record
//!
//! A file larger than [`xerj_common::document_limit::MAX_INDEXED_DOCUMENT_BYTES`]
//! (and still within [`MAX_WHOLE_FILE`]) is not parsed into one `Value`.
//! Record arrays are streamed one element at a time. A single document, or
//! one element, over that ceiling is skipped and named — it is not indexed
//! (#1122). Files at or under the ceiling keep the DOM path so existing
//! locators stay byte-identical.
//!
//! An uncompressed file larger than [`MAX_WHOLE_FILE`] is not loaded at all.
//! A cursor walks it from disk and keeps at most one document-sized value.
//! Gzip stays on the in-memory cap: the uncompressed length is not known
//! until the bytes are inflated.

use super::{
    emit_document_with_fields, ensure_text_passage, flatten_object, ExtractStats, FieldOrigin,
    RawRecord, Sink, MAX_LINE, MAX_WHOLE_FILE,
};
use anyhow::Result;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

/// Sentence appended to a skip that the 8 MiB document ceiling caused.
///
/// `--max-file-mb` is the scan cap (default 2 GiB). `limits.max_process_memory_mb`
/// is the process watermark. Neither raises this ceiling.
const FIXED_DOCUMENT_CEILING: &str =
    "This ceiling is fixed and is not raised by --max-file-mb or limits.max_process_memory_mb";

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    extract_limited(
        path,
        gzip,
        sink,
        xerj_common::document_limit::MAX_INDEXED_DOCUMENT_BYTES,
    )
}

fn extract_limited(path: &Path, gzip: bool, sink: Sink, limit: usize) -> Result<ExtractStats> {
    extract_with_whole_file_cap(path, gzip, sink, limit, MAX_WHOLE_FILE)
}

fn extract_with_whole_file_cap(
    path: &Path,
    gzip: bool,
    sink: Sink,
    limit: usize,
    whole_file_cap: u64,
) -> Result<ExtractStats> {
    // Uncompressed JSON past the in-memory cap is walked from disk. The
    // 8–64 MiB window stays on `stream_large` so its locators stay the ones
    // the DOM path produced.
    if !gzip {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.is_file() && meta.len() > whole_file_cap {
                return stream_uncompressed_over_cap(path, meta.len(), limit, sink);
            }
        }
    }
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, whole_file_cap)? else {
        stats.junk += 1;
        stats.refusal = Some(format!(
            "{} is larger than {whole_file_cap} bytes; a JSON file over that size is not read. \
             Split it into smaller files. {FIXED_DOCUMENT_CEILING}",
            file_label(path),
        ));
        return Ok(stats);
    };
    if bytes.len() > limit {
        return stream_large(&bytes, path, limit, sink);
    }
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            // Not one JSON value. Before junking, try the file as JSONL:
            // a JSONL export whose FIRST line is longer than the sniff
            // prefix (8 KB) sniffs as `Json` — the sample holds zero
            // COMPLETE lines, so `classify_structured`'s per-line test
            // never runs and `looks_like_json_start` routes the whole
            // file here. Measured on the published rust-vulns pack:
            // osv/records.jsonl (735 records, first line ≥ 8 KB) was
            // junked as "no records extracted (json candidate family)"
            // and `xerj corpus index` accepted 1,230 of 1,963 records as
            // a success. A file that is neither one JSON value nor a
            // sequence of JSON lines junks exactly as before — every
            // line fails the per-line parse too.
            return jsonl_fallback(&bytes, limit, path, sink);
        }
    };
    match v {
        Value::Array(arr) => {
            if !arr.is_empty() {
                stats.populations.push((None, arr.len() as u64));
            }
            for (i, el) in arr.into_iter().enumerate() {
                if !emit(el, &format!("e{i}"), sink, &mut stats) {
                    break;
                }
            }
        }
        Value::Object(obj) => {
            // A Google Keep note is a document, not a row. Tested before the
            // array rule below, which would otherwise explode a checklist
            // note's `listContent` into one record per bullet.
            if let Some(note) = keep_note(&obj) {
                emit_document_with_fields(
                    &note.fields,
                    &note.title,
                    note.body.trim(),
                    "note",
                    sink,
                    &mut stats,
                );
                return Ok(stats);
            }
            extract_object(obj, sink, &mut stats);
        }
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            emit(Value::Object(m), "doc", sink, &mut stats);
        }
    }
    Ok(stats)
}

/// A Google Keep note, as a document.
struct KeepNote {
    title: String,
    body: String,
    fields: Map<String, Value>,
}

/// Characters of a title-less note's first line used as its title.
const KEEP_TITLE_CHARS: usize = 80;

/// Recognise a Google Keep note exported by Takeout (`Takeout/Keep/*.json`).
///
/// By CONTENT, like every other format here: the four keys tested are the ones
/// every Keep export carries (`createdTimestampUsec`, `userEditedTimestampUsec`,
/// `isTrashed`, and one of `textContent`/`listContent`), and no general-purpose
/// JSON document has all four by accident.
///
/// Why it is worth a branch: left to the generic rules a note indexes as a DATA
/// record whose text lives in a field called `textContent` — a name
/// `xerj search` does not query — and a checklist note becomes one record per
/// bullet. As a document it lands in the same `title`/`body` corpus as the mail
/// and files beside it. A trashed or archived note is still indexed, flagged
/// (`keep_trashed`/`keep_archived`) rather than silently dropped: what to
/// exclude is the reader's decision.
///
/// The field names are the extractor's vocabulary (`FieldOrigin::Extractor`,
/// via `emit_document_with_fields`), so a folder of notes joins the scope's
/// document dataset instead of forming a schema cluster of its own.
fn keep_note(obj: &Map<String, Value>) -> Option<KeepNote> {
    let created = obj.get("createdTimestampUsec")?.as_i64()?;
    let edited = obj.get("userEditedTimestampUsec")?.as_i64()?;
    let trashed = obj.get("isTrashed")?.as_bool()?;
    let text = obj.get("textContent").and_then(Value::as_str);
    let list = obj.get("listContent").and_then(Value::as_array);
    if text.is_none() && list.is_none() {
        return None;
    }

    let mut body = text.unwrap_or("").to_string();
    for item in list.into_iter().flatten() {
        let Some(line) = item.get("text").and_then(Value::as_str) else {
            continue;
        };
        let checked = item.get("isChecked").and_then(Value::as_bool) == Some(true);
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(if checked { "- [x] " } else { "- [ ] " });
        body.push_str(line);
    }
    // Link annotations: the URL and page title Keep attached to the note.
    for ann in obj
        .get("annotations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let url = ann.get("url").and_then(Value::as_str).unwrap_or("");
        let title = ann.get("title").and_then(Value::as_str).unwrap_or("");
        if url.is_empty() && title.is_empty() {
            continue;
        }
        body.push_str("\n\n");
        body.push_str(title);
        if !title.is_empty() && !url.is_empty() {
            body.push_str(" — ");
        }
        body.push_str(url);
    }

    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            // Cut by CHARS, never by bytes: a note that opens in Arabic or
            // Chinese must not be sliced mid-character (panic = abort).
            let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let cut: String = first.trim().chars().take(KEEP_TITLE_CHARS).collect();
            if cut.is_empty() {
                "(untitled note)".to_string()
            } else {
                cut
            }
        });

    let mut fields = Map::new();
    let labels: Vec<Value> = obj
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| l.get("name").and_then(Value::as_str))
        .map(|n| Value::String(n.to_string()))
        .collect();
    if !labels.is_empty() {
        fields.insert("keep_labels".into(), Value::Array(labels));
    }
    for (name, usec) in [("keep_created", created), ("keep_edited", edited)] {
        if let Some(t) = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(usec) {
            fields.insert(
                name.into(),
                Value::String(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            );
        }
    }
    fields.insert("keep_trashed".into(), Value::Bool(trashed));
    for (name, key) in [("keep_archived", "isArchived"), ("keep_pinned", "isPinned")] {
        if let Some(b) = obj.get(key).and_then(Value::as_bool) {
            fields.insert(name.into(), Value::Bool(b));
        }
    }
    Some(KeepNote {
        title,
        body,
        fields,
    })
}

/// Largest file [`is_keep_note_file`] will open. Keep caps a note at roughly
/// 20k characters; a JSON file past this is not a note, and the walker must
/// not read megabytes per `.html` it meets just to find that out.
const KEEP_PROBE_MAX: u64 = 1 << 20;

/// Is the file at `path` a Google Keep note? The SAME predicate the extractor
/// applies ([`keep_note`]), exposed for the walker's Takeout rule so the two
/// can never disagree about which `.html` has a JSON twin that will be indexed
/// as the note. Any error — unreadable, oversized, not JSON — is `false`: the
/// caller is deciding whether to SKIP a file, and doubt means index it.
pub(crate) fn is_keep_note_file(path: &Path) -> bool {
    let small = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= KEEP_PROBE_MAX);
    if !small {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(obj)) => keep_note(&obj).is_some(),
        _ => false,
    }
}

/// A shared value longer than this is not cloned onto every row.
///
/// Cloning the compact JSON of one sibling array onto every element of
/// another is what wedged ingest: `subsystem_map.json` (#1856) copied a
/// 25 KB `runs` array onto each of 8,589 function rows and then elected
/// that string as `semantic_text`. A 42 MB file of the same shape (#1978)
/// multiplied several sibling arrays by ~123,000 rows until the process
/// ran out of memory. Past this size the value is emitted once, under its
/// own group.
const SHARED_REPLICATE_MAX: usize = 4096;

/// Shortest largest-array that is split into one dataset per array.
///
/// [`crate::dataset::DOC_DEMOTE_MAX_RECORDS_PER_FILE`] folds a JSON file of
/// at most 32 records into the document corpus. Splitting a file that
/// small would turn one config document into several indices. An array of
/// this length or more is a collection.
const MULTI_ARRAY_SPLIT_MIN: usize = 33;

/// Top-level arrays of objects (len ≥ 2, every element an object), longest
/// first. Equal lengths break toward the lexicographically smaller key so
/// the primary array does not depend on map iteration order.
fn record_arrays(obj: &Map<String, Value>) -> Vec<(String, usize)> {
    let mut arrays = Vec::new();
    for (k, vv) in obj {
        if let Value::Array(a) = vv {
            if a.len() >= 2 && a.iter().all(|e| e.is_object()) {
                arrays.push((k.clone(), a.len()));
            }
        }
    }
    arrays.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    arrays
}

fn extract_object(obj: Map<String, Value>, sink: Sink, stats: &mut ExtractStats) {
    let arrays = record_arrays(&obj);
    if arrays.len() >= 2 && arrays[0].1 >= MULTI_ARRAY_SPLIT_MIN {
        extract_split(obj, arrays, sink, stats);
    } else if let Some((key, _)) = arrays.into_iter().next() {
        extract_one_array(obj, key, sink, stats);
    } else {
        emit(Value::Object(obj), "doc", sink, stats);
    }
}

/// One record array. Other arrays stay shared fields, unless one of them
/// is too big to copy onto every row.
fn extract_one_array(
    mut obj: Map<String, Value>,
    key: String,
    sink: Sink,
    stats: &mut ExtractStats,
) {
    let Value::Array(arr) = obj.remove(&key).unwrap() else {
        unreachable!("record_arrays only yields array keys")
    };
    stats.populations.push((None, arr.len() as u64));
    let (shared, sidecars) = partition_shared(obj);
    for (i, el) in arr.into_iter().enumerate() {
        let Value::Object(m) = el else { continue };
        let fields = merge_shared(&shared, m);
        if !emit_fields(fields, format!("{key}:e{i}"), None, sink, stats) {
            break;
        }
    }
    emit_sidecars(sidecars, sink, stats);
}

/// Two or more record arrays. The longest keeps group `None` and the
/// `{key}:e{i}` locator a single-array file already used, so an existing
/// document id for that array does not move. Every other array is a group
/// of its own and is not serialized onto the primary rows.
fn extract_split(
    mut obj: Map<String, Value>,
    arrays: Vec<(String, usize)>,
    sink: Sink,
    stats: &mut ExtractStats,
) {
    let mut pulled = Vec::with_capacity(arrays.len());
    for (key, _) in &arrays {
        let Value::Array(arr) = obj.remove(key).unwrap() else {
            unreachable!("record_arrays only yields array keys")
        };
        pulled.push((key.clone(), arr));
    }
    let (shared, sidecars) = partition_shared(obj);
    for (idx, (key, arr)) in pulled.into_iter().enumerate() {
        let group = if idx == 0 { None } else { Some(key.clone()) };
        stats.populations.push((group.clone(), arr.len() as u64));
        for (i, el) in arr.into_iter().enumerate() {
            let Value::Object(m) = el else { continue };
            let fields = merge_shared(&shared, m);
            // A `false` sink means this group's sample is full. The next
            // array is a different group and still needs its own sample.
            if !emit_fields(fields, format!("{key}:e{i}"), group.clone(), sink, stats) {
                break;
            }
        }
    }
    emit_sidecars(sidecars, sink, stats);
}

fn merge_shared(shared: &Map<String, Value>, element: Map<String, Value>) -> Map<String, Value> {
    let mut fields = shared.clone();
    for (k, v) in flatten_object(element) {
        fields.insert(k, v); // element wins collisions
    }
    fields
}

/// Flatten the wrapper left after record arrays are removed, then hold back
/// any value too large to clone onto every row.
fn partition_shared(obj: Map<String, Value>) -> (Map<String, Value>, Vec<(String, Value)>) {
    let flat = flatten_object(obj);
    let mut small = Map::new();
    let mut sidecars = Vec::new();
    for (k, v) in flat {
        if shared_value_too_big(&v) {
            sidecars.push((k, v));
        } else {
            small.insert(k, v);
        }
    }
    (small, sidecars)
}

fn shared_value_too_big(v: &Value) -> bool {
    match v {
        Value::String(s) => s.len() > SHARED_REPLICATE_MAX,
        Value::Array(a) => serde_json::to_string(a)
            .map(|s| s.len() > SHARED_REPLICATE_MAX)
            .unwrap_or(true),
        _ => false,
    }
}

fn emit_sidecars(sidecars: Vec<(String, Value)>, sink: Sink, stats: &mut ExtractStats) {
    for (key, value) in sidecars {
        stats.populations.push((Some(key.clone()), 1));
        let mut fields = Map::new();
        fields.insert(key.clone(), value);
        // One record. A full sample on an earlier group must not drop it:
        // the sink's `false` is per group, and this group has not been seen.
        let _ = emit_fields(fields, format!("{key}:shared"), Some(key), sink, stats);
    }
}

fn emit_fields(
    mut fields: Map<String, Value>,
    locator: String,
    group: Option<String>,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    ensure_text_passage(&mut fields);
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator,
        group,
        origin: FieldOrigin::Data,
    })
}

/// Whole-file parse failed — re-parse the same in-memory bytes line by
/// line under the JSONL contract (see the fallback note in [`extract`]).
/// Mirrors `jsonl::extract`'s per-line semantics: blank lines skip, a line
/// at the 16 MB cap junks, an object becomes one flattened record with a
/// byte-offset locator, anything else junks. No `limit_bytes` exists here
/// (the family reads the whole file), so there is no truncated-tail
/// exemption to carry over.
fn jsonl_fallback(bytes: &[u8], limit: usize, path: &Path, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let mut offset: u64 = 0;
    for line in bytes.split(|&b| b == b'\n') {
        let start = offset;
        offset += line.len() as u64 + 1;
        let trimmed = super::jsonl::trim_ws(line);
        if trimmed.is_empty() {
            continue;
        }
        if line.len() >= MAX_LINE {
            stats.junk += 1;
            continue;
        }
        // The 8–16 MiB gap under MAX_LINE used to be parsed and tokenized
        // as one document (#1122).
        if trimmed.len() > limit {
            stats.oversize_values += 1;
            continue;
        }
        match serde_json::from_slice::<Value>(trimmed) {
            Ok(Value::Object(m)) => {
                let mut fields = flatten_object(m);
                ensure_text_passage(&mut fields);
                stats.records += 1;
                if !sink(RawRecord {
                    fields,
                    locator: format!("b{start}"),
                    group: None,
                    origin: FieldOrigin::Data,
                }) {
                    return Ok(stats);
                }
            }
            Ok(_) | Err(_) => {
                stats.junk += 1;
            }
        }
    }
    finish_empty_refusal(path, bytes.len(), limit, &mut stats);
    Ok(stats)
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn finish_empty_refusal(path: &Path, nbytes: usize, limit: usize, stats: &mut ExtractStats) {
    finish_empty_refusal_len(path, nbytes as u64, limit, stats);
}

fn finish_empty_refusal_len(path: &Path, nbytes: u64, limit: usize, stats: &mut ExtractStats) {
    if stats.records == 0 && stats.oversize_values > 0 && stats.refusal.is_none() {
        stats.refusal = Some(format!(
            "{} is {nbytes} bytes; {} JSON value(s) were not indexed (over the {limit}-byte limit for one document). Split the file into smaller documents. {FIXED_DOCUMENT_CEILING}",
            file_label(path),
            stats.oversize_values,
        ));
    }
}

fn stream_large(bytes: &[u8], path: &Path, limit: usize, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    match first_significant(bytes) {
        Some(b'[') => {
            if let Err(error) = stream_top_array(bytes, limit, sink, &mut stats) {
                if stats.records == 0 && stats.oversize_values == 0 && stats.junk == 0 {
                    return jsonl_fallback(bytes, limit, path, sink);
                }
                stats.junk += 1;
                let _ = error;
            }
        }
        Some(b'{') => {
            if let Err(error) = stream_top_object(bytes, path, limit, sink, &mut stats) {
                if stats.records == 0 && stats.oversize_values == 0 && stats.junk == 0 {
                    return jsonl_fallback(bytes, limit, path, sink);
                }
                stats.junk += 1;
                let _ = error;
            }
        }
        _ => return jsonl_fallback(bytes, limit, path, sink),
    }
    finish_empty_refusal(path, bytes.len(), limit, &mut stats);
    Ok(stats)
}

fn first_significant(bytes: &[u8]) -> Option<u8> {
    bytes
        .iter()
        .copied()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
}

/// Byte range of `raw` inside `input`.
///
/// `serde_json`'s `&RawValue`, deserialized from a slice, borrows that slice.
/// The pointer difference is only used after a byte-for-byte check that the
/// range really is `raw`. A mismatch copies nothing and fails the value, so a
/// future serde that copies `RawValue` cannot silently read the wrong bytes.
fn raw_span(input: &[u8], raw: &str) -> Option<Range<usize>> {
    let raw_bytes = raw.as_bytes();
    let input_addr = input.as_ptr() as usize;
    let raw_addr = raw_bytes.as_ptr() as usize;
    if raw_addr < input_addr {
        return None;
    }
    let start = raw_addr - input_addr;
    let end = start.checked_add(raw_bytes.len())?;
    if end > input.len() || input.get(start..end) != Some(raw_bytes) {
        return None;
    }
    Some(start..end)
}

fn object_spans(input: &[u8]) -> Result<Vec<(String, Range<usize>)>, serde_json::Error> {
    struct SpanVisitor<'a> {
        input: &'a [u8],
    }
    impl<'de> Visitor<'de> for SpanVisitor<'de> {
        type Value = Vec<(String, Range<usize>)>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a JSON object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut spans: Vec<(String, Range<usize>)> = Vec::new();
            let mut at: HashMap<String, usize> = HashMap::new();
            while let Some(key) = map.next_key::<String>()? {
                let raw = map.next_value::<&RawValue>()?;
                let range = raw_span(self.input, raw.get()).ok_or_else(|| {
                    serde::de::Error::custom("JSON value is not a slice of the input")
                })?;
                if let Some(slot) = at.get(&key).copied() {
                    spans[slot].1 = range;
                } else {
                    at.insert(key.clone(), spans.len());
                    spans.push((key, range));
                }
            }
            Ok(spans)
        }
    }
    let mut de = serde_json::Deserializer::from_slice(input);
    let spans = de.deserialize_any(SpanVisitor { input })?;
    de.end()?;
    Ok(spans)
}

fn looks_like_array(bytes: &[u8]) -> bool {
    first_significant(bytes) == Some(b'[')
}

/// `Some(len)` when `input` is an array of at least two objects.
fn record_array_len(input: &[u8]) -> Result<Option<u64>, serde_json::Error> {
    struct ShapeVisitor;
    impl<'de> Visitor<'de> for ShapeVisitor {
        type Value = Option<u64>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a JSON array")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut count = 0u64;
            let mut objects = true;
            while let Some(raw) = seq.next_element::<&RawValue>()? {
                count += 1;
                if objects && !raw.get().trim_start().starts_with('{') {
                    objects = false;
                }
            }
            Ok(if objects && count >= 2 {
                Some(count)
            } else {
                None
            })
        }
    }
    let mut de = serde_json::Deserializer::from_slice(input);
    let len = de.deserialize_any(ShapeVisitor)?;
    de.end()?;
    Ok(len)
}

struct Field {
    key: String,
    range: Range<usize>,
    record_len: Option<u64>,
}

fn stream_top_object(
    bytes: &[u8],
    path: &Path,
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), serde_json::Error> {
    let spans = object_spans(bytes)?;
    let mut fields = Vec::with_capacity(spans.len());
    for (key, range) in spans {
        let record_len = if looks_like_array(&bytes[range.clone()]) {
            record_array_len(&bytes[range.clone()])?
        } else {
            None
        };
        fields.push(Field {
            key,
            range,
            record_len,
        });
    }
    let mut arrays: Vec<usize> = (0..fields.len())
        .filter(|&i| fields[i].record_len.is_some())
        .collect();
    arrays.sort_by(|&a, &b| {
        let la = fields[a].record_len.unwrap_or(0);
        let lb = fields[b].record_len.unwrap_or(0);
        lb.cmp(&la).then_with(|| fields[a].key.cmp(&fields[b].key))
    });
    if arrays.len() >= 2
        && fields[arrays[0]].record_len.unwrap_or(0) >= MULTI_ARRAY_SPLIT_MIN as u64
    {
        let skip: HashSet<&str> = arrays.iter().map(|&i| fields[i].key.as_str()).collect();
        let shared = parse_shared_fields(bytes, &fields, &skip, limit, stats)?;
        let (small, sidecars) = partition_shared(shared);
        for (idx, field_i) in arrays.iter().copied().enumerate() {
            let group = if idx == 0 {
                None
            } else {
                Some(fields[field_i].key.clone())
            };
            let len = fields[field_i].record_len.unwrap_or(0);
            stats.populations.push((group.clone(), len));
            emit_record_array(
                &bytes[fields[field_i].range.clone()],
                &fields[field_i].key,
                group,
                &small,
                limit,
                sink,
                stats,
            )?;
        }
        emit_sidecars_bounded(sidecars, limit, sink, stats);
    } else if let Some(field_i) = arrays.into_iter().next() {
        let key = fields[field_i].key.clone();
        let range = fields[field_i].range.clone();
        let len = fields[field_i].record_len.unwrap_or(0);
        let mut skip = HashSet::new();
        skip.insert(key.as_str());
        let shared = parse_shared_fields(bytes, &fields, &skip, limit, stats)?;
        let (small, sidecars) = partition_shared(shared);
        stats.populations.push((None, len));
        emit_record_array(&bytes[range], &key, None, &small, limit, sink, stats)?;
        emit_sidecars_bounded(sidecars, limit, sink, stats);
    } else {
        emit_single_object(bytes, path, &fields, limit, sink, stats)?;
    }
    Ok(())
}

fn parse_shared_fields(
    bytes: &[u8],
    fields: &[Field],
    skip: &HashSet<&str>,
    limit: usize,
    stats: &mut ExtractStats,
) -> Result<Map<String, Value>, serde_json::Error> {
    let mut obj = Map::new();
    for field in fields {
        if skip.contains(field.key.as_str()) {
            continue;
        }
        if field.range.len() > limit {
            stats.oversize_values += 1;
            continue;
        }
        let value: Value = serde_json::from_slice(&bytes[field.range.clone()])?;
        obj.insert(field.key.clone(), value);
    }
    Ok(obj)
}

fn emit_sidecars_bounded(
    sidecars: Vec<(String, Value)>,
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) {
    let mut kept = Vec::new();
    for (key, value) in sidecars {
        let raw_len = match &value {
            Value::String(text) => text.len(),
            other => serde_json::to_string(other)
                .map(|text| text.len())
                .unwrap_or(usize::MAX),
        };
        if raw_len.saturating_add(key.len()).saturating_add(8) > limit {
            stats.oversize_values += 1;
            continue;
        }
        kept.push((key, value));
    }
    emit_sidecars(kept, sink, stats);
}

fn emit_single_object(
    bytes: &[u8],
    path: &Path,
    fields: &[Field],
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), serde_json::Error> {
    let mut skipped = 0u64;
    let mut kept = Vec::new();
    let mut sum = 2usize;
    for field in fields {
        if field.range.len() > limit {
            skipped += 1;
            continue;
        }
        sum = sum.saturating_add(field.key.len() + field.range.len() + 4);
        kept.push(field);
    }
    if (skipped > 0 && kept.is_empty()) || sum > limit {
        stats.oversize_values += 1 + skipped;
        stats.refusal = Some(format!(
            "{} is {} bytes; a JSON document over {limit} bytes is not indexed. Split the file into smaller documents. {FIXED_DOCUMENT_CEILING}",
            file_label(path),
            bytes.len(),
        ));
        return Ok(());
    }
    stats.oversize_values += skipped;
    let mut obj = Map::new();
    for field in kept {
        let value: Value = serde_json::from_slice(&bytes[field.range.clone()])?;
        obj.insert(field.key.clone(), value);
    }
    emit(Value::Object(obj), "doc", sink, stats);
    Ok(())
}

fn stream_top_array(
    bytes: &[u8],
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), serde_json::Error> {
    struct TopVisitor<'a> {
        limit: usize,
        sink: Sink<'a>,
        stats: &'a mut ExtractStats,
    }
    impl<'de> Visitor<'de> for TopVisitor<'_> {
        type Value = ();
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a JSON array")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let TopVisitor { limit, sink, stats } = self;
            let mut count = 0u64;
            let mut live = true;
            loop {
                let raw = match seq.next_element::<&RawValue>() {
                    Ok(Some(raw)) => raw,
                    Ok(None) => break,
                    Err(error) => {
                        if count > 0 {
                            stats.populations.push((None, count));
                        }
                        return Err(error);
                    }
                };
                let i = count;
                count += 1;
                if raw.get().len() > limit {
                    stats.oversize_values += 1;
                    continue;
                }
                if !live {
                    continue;
                }
                let value: Value = match serde_json::from_str(raw.get()) {
                    Ok(value) => value,
                    Err(_) => {
                        stats.junk += 1;
                        continue;
                    }
                };
                if !emit(value, &format!("e{i}"), sink, stats) {
                    live = false;
                }
            }
            if count > 0 {
                stats.populations.push((None, count));
            }
            Ok(())
        }
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.deserialize_any(TopVisitor { limit, sink, stats })?;
    de.end()?;
    Ok(())
}

fn emit_record_array<'a>(
    input: &'a [u8],
    key: &'a str,
    group: Option<String>,
    shared: &'a Map<String, Value>,
    limit: usize,
    sink: Sink<'a>,
    stats: &'a mut ExtractStats,
) -> Result<(), serde_json::Error> {
    struct RecVisitor<'a> {
        key: &'a str,
        group: Option<String>,
        shared: &'a Map<String, Value>,
        limit: usize,
        sink: Sink<'a>,
        stats: &'a mut ExtractStats,
    }
    impl<'de> Visitor<'de> for RecVisitor<'_> {
        type Value = ();
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a JSON array of objects")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let RecVisitor {
                key,
                group,
                shared,
                limit,
                sink,
                stats,
            } = self;
            let mut live = true;
            let mut index = 0u64;
            while let Some(raw) = seq.next_element::<&RawValue>()? {
                let i = index;
                index += 1;
                if raw.get().len() > limit {
                    stats.oversize_values += 1;
                    continue;
                }
                if !live {
                    continue;
                }
                let value: Value = match serde_json::from_str(raw.get()) {
                    Ok(value) => value,
                    Err(_) => {
                        stats.junk += 1;
                        continue;
                    }
                };
                let Value::Object(element) = value else {
                    continue;
                };
                let fields = merge_shared(shared, element);
                if !emit_fields(fields, format!("{key}:e{i}"), group.clone(), sink, stats) {
                    live = false;
                }
            }
            Ok(())
        }
    }
    let mut de = serde_json::Deserializer::from_slice(input);
    de.deserialize_any(RecVisitor {
        key,
        group,
        shared,
        limit,
        sink,
        stats,
    })?;
    de.end()?;
    Ok(())
}

fn emit(v: Value, locator: &str, sink: Sink, stats: &mut ExtractStats) -> bool {
    let mut fields = match v {
        Value::Object(m) => flatten_object(m),
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    ensure_text_passage(&mut fields);
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: locator.to_string(),
        group: None,
        origin: FieldOrigin::Data,
    })
}

// Peak bytes retained from one JSON value. Tests assert this stays within the
// per-document cap while a larger sibling value is skipped.
#[cfg(test)]
thread_local! {
    static JSON_SCAN_STORED_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn note_stored(capacity: usize) {
    #[cfg(test)]
    {
        JSON_SCAN_STORED_PEAK.with(|peak| {
            if capacity > peak.get() {
                peak.set(capacity);
            }
        });
    }
    #[cfg(not(test))]
    {
        let _ = capacity;
    }
}

#[derive(Debug)]
struct ScanError(String);

impl ScanError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ScanError {}

impl From<std::io::Error> for ScanError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

enum ScanOutcome {
    Fits(Vec<u8>),
    Oversize,
}

struct Recorder {
    cap: usize,
    stored: Vec<u8>,
    over: bool,
}

impl Recorder {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            stored: Vec::new(),
            over: false,
        }
    }

    fn push(&mut self, byte: u8) {
        if self.over {
            return;
        }
        if self.cap == 0 || self.stored.len() >= self.cap {
            self.over = true;
            self.stored = Vec::new();
            return;
        }
        if self.stored.len() == self.stored.capacity() {
            let room = self.cap - self.stored.len();
            let grow = room.min(self.stored.capacity().max(64).saturating_mul(2));
            if grow == 0 {
                self.over = true;
                self.stored = Vec::new();
                return;
            }
            self.stored.reserve_exact(grow);
        }
        self.stored.push(byte);
        note_stored(self.stored.len());
    }

    fn finish(self) -> ScanOutcome {
        if self.over {
            ScanOutcome::Oversize
        } else {
            ScanOutcome::Fits(self.stored)
        }
    }
}

struct JsonSrc {
    inner: std::fs::File,
    buf: Vec<u8>,
    buf_start: u64,
    pos: usize,
    filled: usize,
}

impl JsonSrc {
    fn new(inner: std::fs::File) -> Self {
        Self {
            inner,
            buf: vec![0; 64 * 1024],
            buf_start: 0,
            pos: 0,
            filled: 0,
        }
    }

    fn position(&self) -> u64 {
        self.buf_start + self.pos as u64
    }

    fn fill(&mut self) -> Result<(), ScanError> {
        if self.pos < self.filled {
            return Ok(());
        }
        let at = self.position();
        self.inner.seek(SeekFrom::Start(at))?;
        let n = self.inner.read(&mut self.buf)?;
        self.buf_start = at;
        self.pos = 0;
        self.filled = n;
        Ok(())
    }

    fn peek(&mut self) -> Result<Option<u8>, ScanError> {
        self.fill()?;
        if self.pos >= self.filled {
            return Ok(None);
        }
        Ok(Some(self.buf[self.pos]))
    }

    fn bump(&mut self) -> Result<Option<u8>, ScanError> {
        let byte = self.peek()?;
        if byte.is_some() {
            self.pos += 1;
        }
        Ok(byte)
    }

    fn skip_ws(&mut self) -> Result<(), ScanError> {
        while let Some(byte) = self.peek()? {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.bump()?;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn seek_to(&mut self, at: u64) -> Result<(), ScanError> {
        if at >= self.buf_start && (at - self.buf_start) < self.filled as u64 {
            self.pos = (at - self.buf_start) as usize;
            return Ok(());
        }
        self.inner.seek(SeekFrom::Start(at))?;
        self.buf_start = at;
        self.pos = 0;
        self.filled = 0;
        Ok(())
    }

    fn read_range(&mut self, start: u64, end: u64) -> Result<Vec<u8>, ScanError> {
        if end < start {
            return Err(ScanError::new("invalid JSON span"));
        }
        let len = usize::try_from(end - start)
            .map_err(|_| ScanError::new("JSON value does not fit in memory"))?;
        let mut out = vec![0; len];
        self.inner.seek(SeekFrom::Start(start))?;
        self.inner.read_exact(&mut out)?;
        self.buf_start = end;
        self.pos = 0;
        self.filled = 0;
        Ok(out)
    }
}

fn expect_byte(src: &mut JsonSrc, want: u8) -> Result<(), ScanError> {
    match src.bump()? {
        Some(byte) if byte == want => Ok(()),
        Some(byte) => Err(ScanError::new(format!(
            "expected '{}', found byte {byte}",
            want as char
        ))),
        None => Err(ScanError::new("truncated JSON")),
    }
}

const JSON_NEST_MAX: usize = 128;

fn scan_value(src: &mut JsonSrc, cap: usize) -> Result<ScanOutcome, ScanError> {
    let mut rec = Recorder::new(cap);
    let byte = src
        .bump()?
        .ok_or_else(|| ScanError::new("truncated JSON value"))?;
    rec.push(byte);
    match byte {
        b'"' => scan_string_body(src, &mut rec)?,
        b'{' => scan_nest(src, &mut rec, b'}')?,
        b'[' => scan_nest(src, &mut rec, b']')?,
        b't' => scan_exact(src, &mut rec, b"rue")?,
        b'f' => scan_exact(src, &mut rec, b"alse")?,
        b'n' => scan_exact(src, &mut rec, b"ull")?,
        b'-' | b'0'..=b'9' => scan_number_rest(src, &mut rec)?,
        _ => return Err(ScanError::new("invalid JSON value")),
    }
    Ok(rec.finish())
}

fn scan_exact(src: &mut JsonSrc, rec: &mut Recorder, rest: &[u8]) -> Result<(), ScanError> {
    for &expected in rest {
        match src.bump()? {
            Some(byte) if byte == expected => rec.push(byte),
            _ => return Err(ScanError::new("invalid JSON literal")),
        }
    }
    Ok(())
}

fn scan_number_rest(src: &mut JsonSrc, rec: &mut Recorder) -> Result<(), ScanError> {
    loop {
        match src.peek()? {
            Some(byte)
                if byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E') =>
            {
                let Some(byte) = src.bump()? else {
                    return Ok(());
                };
                rec.push(byte);
            }
            _ => return Ok(()),
        }
    }
}

fn scan_string_body(src: &mut JsonSrc, rec: &mut Recorder) -> Result<(), ScanError> {
    loop {
        let byte = src
            .bump()?
            .ok_or_else(|| ScanError::new("truncated JSON string"))?;
        rec.push(byte);
        if byte == b'\\' {
            let escaped = src
                .bump()?
                .ok_or_else(|| ScanError::new("truncated JSON string"))?;
            rec.push(escaped);
            if escaped == b'u' {
                for _ in 0..4 {
                    let hex = src
                        .bump()?
                        .ok_or_else(|| ScanError::new("truncated JSON string"))?;
                    if !hex.is_ascii_hexdigit() {
                        return Err(ScanError::new("invalid JSON unicode escape"));
                    }
                    rec.push(hex);
                }
            }
            continue;
        }
        if byte == b'"' {
            return Ok(());
        }
    }
}

fn scan_nest(src: &mut JsonSrc, rec: &mut Recorder, closer: u8) -> Result<(), ScanError> {
    let mut stack = Vec::with_capacity(8);
    stack.push(closer);
    let mut in_string = false;
    let mut escape = false;
    while !stack.is_empty() {
        let byte = src
            .bump()?
            .ok_or_else(|| ScanError::new("truncated JSON value"))?;
        rec.push(byte);
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if byte == b'\\' {
                escape = true;
                continue;
            }
            if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                if stack.len() >= JSON_NEST_MAX {
                    return Err(ScanError::new("JSON nesting exceeds 128"));
                }
                stack.push(if byte == b'{' { b'}' } else { b']' });
            }
            b'}' | b']' if stack.pop() != Some(byte) => {
                return Err(ScanError::new("mismatched JSON bracket"));
            }
            b'}' | b']' => {}
            _ => {}
        }
    }
    Ok(())
}

fn read_object_key(src: &mut JsonSrc) -> Result<String, ScanError> {
    let ScanOutcome::Fits(bytes) = scan_value(src, 1 << 20)? else {
        return Err(ScanError::new("JSON object key is too long"));
    };
    serde_json::from_slice(&bytes).map_err(|error| ScanError::new(error.to_string()))
}

struct DiskField {
    key: String,
    start: u64,
    end: u64,
    record_len: Option<u64>,
}

struct ArrayMeasure {
    end: u64,
    record_len: Option<u64>,
}

fn after_value(src: &mut JsonSrc, end: u8) -> Result<(), ScanError> {
    src.skip_ws()?;
    match src.peek()? {
        Some(b',') => {
            src.bump()?;
            src.skip_ws()?;
            if matches!(src.peek()?, Some(byte) if byte == end) || src.peek()?.is_none() {
                return Err(ScanError::new("trailing comma in JSON"));
            }
            Ok(())
        }
        Some(byte) if byte == end => Ok(()),
        _ => Err(ScanError::new("expected comma or end of JSON value")),
    }
}

fn measure_array(src: &mut JsonSrc) -> Result<ArrayMeasure, ScanError> {
    expect_byte(src, b'[')?;
    let mut objects = 0u64;
    let mut others = 0u64;
    loop {
        src.skip_ws()?;
        match src.peek()? {
            None => return Err(ScanError::new("truncated JSON array")),
            Some(b']') => {
                src.bump()?;
                break;
            }
            Some(byte) => {
                if byte == b'{' {
                    objects += 1;
                } else {
                    others += 1;
                }
                let _ = scan_value(src, 0)?;
                after_value(src, b']')?;
            }
        }
    }
    let record_len = if others == 0 && objects >= 2 {
        Some(objects)
    } else {
        None
    };
    Ok(ArrayMeasure {
        end: src.position(),
        record_len,
    })
}

fn scan_object_fields(src: &mut JsonSrc) -> Result<Vec<DiskField>, ScanError> {
    expect_byte(src, b'{')?;
    let mut fields = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    loop {
        src.skip_ws()?;
        match src.peek()? {
            None => return Err(ScanError::new("truncated JSON object")),
            Some(b'}') => {
                src.bump()?;
                break;
            }
            Some(b'"') => {
                let key = read_object_key(src)?;
                src.skip_ws()?;
                expect_byte(src, b':')?;
                src.skip_ws()?;
                let start = src.position();
                let (end, record_len) = if src.peek()? == Some(b'[') {
                    let measured = measure_array(src)?;
                    (measured.end, measured.record_len)
                } else {
                    let _ = scan_value(src, 0)?;
                    (src.position(), None)
                };
                let field = DiskField {
                    key: key.clone(),
                    start,
                    end,
                    record_len,
                };
                if let Some(slot) = at.get(&key).copied() {
                    fields[slot] = field;
                } else {
                    at.insert(key, fields.len());
                    fields.push(field);
                }
                after_value(src, b'}')?;
            }
            _ => return Err(ScanError::new("expected object key")),
        }
    }
    Ok(fields)
}

fn field_raw_len(field: &DiskField) -> Option<usize> {
    usize::try_from(field.end.saturating_sub(field.start)).ok()
}

fn read_field_value(
    src: &mut JsonSrc,
    field: &DiskField,
    limit: usize,
) -> Result<Option<Value>, ScanError> {
    let Some(len) = field_raw_len(field) else {
        return Ok(None);
    };
    if len > limit {
        return Ok(None);
    }
    let bytes = src.read_range(field.start, field.end)?;
    let value =
        serde_json::from_slice(&bytes).map_err(|error| ScanError::new(error.to_string()))?;
    Ok(Some(value))
}

type SharedSplit = (Map<String, Value>, Vec<(String, Value)>);

fn shared_from_disk(
    src: &mut JsonSrc,
    fields: &[DiskField],
    skip: &HashSet<&str>,
    limit: usize,
    stats: &mut ExtractStats,
) -> Result<SharedSplit, ScanError> {
    let mut obj = Map::new();
    for field in fields {
        if skip.contains(field.key.as_str()) {
            continue;
        }
        match read_field_value(src, field, limit)? {
            None => stats.oversize_values += 1,
            Some(value) => {
                obj.insert(field.key.clone(), value);
            }
        }
    }
    Ok(partition_shared(obj))
}

fn emit_disk_array(
    src: &mut JsonSrc,
    field: &DiskField,
    group: Option<String>,
    shared: &Map<String, Value>,
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), ScanError> {
    let key = field.key.as_str();
    src.seek_to(field.start)?;
    expect_byte(src, b'[')?;
    let mut live = true;
    let mut index = 0u64;
    loop {
        src.skip_ws()?;
        match src.peek()? {
            None => return Err(ScanError::new("truncated JSON array")),
            Some(b']') => {
                src.bump()?;
                break;
            }
            Some(_) => {
                let cap = if live { limit } else { 0 };
                let outcome = scan_value(src, cap)?;
                let i = index;
                index += 1;
                if live {
                    match outcome {
                        ScanOutcome::Oversize => stats.oversize_values += 1,
                        ScanOutcome::Fits(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                            Ok(Value::Object(element)) => {
                                let fields = merge_shared(shared, element);
                                if !emit_fields(
                                    fields,
                                    format!("{key}:e{i}"),
                                    group.clone(),
                                    sink,
                                    stats,
                                ) {
                                    live = false;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => stats.junk += 1,
                        },
                    }
                }
                after_value(src, b']')?;
            }
        }
    }
    Ok(())
}

fn emit_disk_single_object(
    src: &mut JsonSrc,
    path: &Path,
    file_len: u64,
    fields: &[DiskField],
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), ScanError> {
    let mut skipped = 0u64;
    let mut kept = Vec::new();
    let mut sum = 2usize;
    for field in fields {
        let Some(raw_len) = field_raw_len(field) else {
            skipped += 1;
            continue;
        };
        if raw_len > limit {
            skipped += 1;
            continue;
        }
        sum = sum.saturating_add(field.key.len() + raw_len + 4);
        kept.push(field);
    }
    if (skipped > 0 && kept.is_empty()) || sum > limit {
        stats.oversize_values += 1 + skipped;
        stats.refusal = Some(format!(
            "{} is {file_len} bytes; a JSON document over {limit} bytes is not indexed. \
             Split the file into smaller documents. {FIXED_DOCUMENT_CEILING}",
            file_label(path),
        ));
        return Ok(());
    }
    stats.oversize_values += skipped;
    let mut obj = Map::new();
    for field in kept {
        let Some(value) = read_field_value(src, field, limit)? else {
            return Err(ScanError::new("JSON field exceeded the document limit"));
        };
        obj.insert(field.key.clone(), value);
    }
    emit(Value::Object(obj), "doc", sink, stats);
    Ok(())
}

fn emit_scanned_object(
    src: &mut JsonSrc,
    path: &Path,
    file_len: u64,
    fields: &[DiskField],
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), ScanError> {
    let mut arrays: Vec<usize> = (0..fields.len())
        .filter(|&index| fields[index].record_len.is_some())
        .collect();
    arrays.sort_by(|&left, &right| {
        let left_len = fields[left].record_len.unwrap_or(0);
        let right_len = fields[right].record_len.unwrap_or(0);
        right_len
            .cmp(&left_len)
            .then_with(|| fields[left].key.cmp(&fields[right].key))
    });
    if arrays.len() >= 2
        && fields[arrays[0]].record_len.unwrap_or(0) >= MULTI_ARRAY_SPLIT_MIN as u64
    {
        let skip: HashSet<&str> = arrays
            .iter()
            .map(|&index| fields[index].key.as_str())
            .collect();
        let (small, sidecars) = shared_from_disk(src, fields, &skip, limit, stats)?;
        for (idx, field_i) in arrays.iter().copied().enumerate() {
            let group = if idx == 0 {
                None
            } else {
                Some(fields[field_i].key.clone())
            };
            let len = fields[field_i].record_len.unwrap_or(0);
            stats.populations.push((group.clone(), len));
            emit_disk_array(src, &fields[field_i], group, &small, limit, sink, stats)?;
        }
        emit_sidecars_bounded(sidecars, limit, sink, stats);
    } else if let Some(field_i) = arrays.into_iter().next() {
        let key = fields[field_i].key.clone();
        let len = fields[field_i].record_len.unwrap_or(0);
        let mut skip = HashSet::new();
        skip.insert(key.as_str());
        let (small, sidecars) = shared_from_disk(src, fields, &skip, limit, stats)?;
        stats.populations.push((None, len));
        emit_disk_array(src, &fields[field_i], None, &small, limit, sink, stats)?;
        emit_sidecars_bounded(sidecars, limit, sink, stats);
    } else {
        emit_disk_single_object(src, path, file_len, fields, limit, sink, stats)?;
    }
    Ok(())
}

fn stream_disk_top_array(
    src: &mut JsonSrc,
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), ScanError> {
    expect_byte(src, b'[')?;
    let mut count = 0u64;
    let mut live = true;
    let result = (|| -> Result<(), ScanError> {
        loop {
            src.skip_ws()?;
            match src.peek()? {
                None => return Err(ScanError::new("truncated JSON array")),
                Some(b']') => {
                    src.bump()?;
                    break;
                }
                Some(_) => {
                    let cap = if live { limit } else { 0 };
                    let outcome = scan_value(src, cap)?;
                    let index = count;
                    count += 1;
                    if live {
                        match outcome {
                            ScanOutcome::Oversize => stats.oversize_values += 1,
                            ScanOutcome::Fits(bytes) => {
                                match serde_json::from_slice::<Value>(&bytes) {
                                    Ok(value) => {
                                        if !emit(value, &format!("e{index}"), sink, stats) {
                                            live = false;
                                        }
                                    }
                                    Err(_) => stats.junk += 1,
                                }
                            }
                        }
                    }
                    after_value(src, b']')?;
                }
            }
        }
        src.skip_ws()?;
        if src.peek()?.is_some() {
            return Err(ScanError::new("trailing data after JSON array"));
        }
        Ok(())
    })();
    if count > 0 {
        stats.populations.push((None, count));
    }
    result
}

fn stream_disk_top_object(
    src: &mut JsonSrc,
    path: &Path,
    file_len: u64,
    limit: usize,
    sink: Sink,
    stats: &mut ExtractStats,
) -> Result<(), ScanError> {
    let fields = scan_object_fields(src)?;
    src.skip_ws()?;
    if src.peek()?.is_some() {
        return Err(ScanError::new("trailing data after JSON object"));
    }
    emit_scanned_object(src, path, file_len, &fields, limit, sink, stats)
}

fn nothing_emitted(stats: &ExtractStats) -> bool {
    stats.records == 0 && stats.oversize_values == 0 && stats.junk == 0
}

fn stream_uncompressed_over_cap(
    path: &Path,
    file_len: u64,
    limit: usize,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let file = std::fs::File::open(path)?;
    let mut src = JsonSrc::new(file);
    let kind = src.skip_ws().and_then(|_| src.peek());
    match kind {
        Ok(Some(b'[')) => {
            if let Err(error) = stream_disk_top_array(&mut src, limit, sink, &mut stats) {
                if nothing_emitted(&stats) {
                    return jsonl_from_disk(path, file_len, limit, sink);
                }
                stats.junk += 1;
                let _ = error;
            }
        }
        Ok(Some(b'{')) => {
            if let Err(error) =
                stream_disk_top_object(&mut src, path, file_len, limit, sink, &mut stats)
            {
                if nothing_emitted(&stats) {
                    return jsonl_from_disk(path, file_len, limit, sink);
                }
                stats.junk += 1;
                let _ = error;
            }
        }
        _ => return jsonl_from_disk(path, file_len, limit, sink),
    }
    finish_empty_refusal_len(path, file_len, limit, &mut stats);
    Ok(stats)
}

fn jsonl_from_disk(path: &Path, file_len: u64, limit: usize, sink: Sink) -> Result<ExtractStats> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut stats = ExtractStats::default();
    let mut offset = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = super::jsonl::read_capped_line(&mut reader, &mut line)?;
        if n == 0 {
            break;
        }
        let start = offset;
        offset += n as u64;
        let trimmed = super::jsonl::trim_ws(&line);
        if trimmed.is_empty() {
            continue;
        }
        if line.len() >= MAX_LINE {
            stats.junk += 1;
            continue;
        }
        if trimmed.len() > limit {
            stats.oversize_values += 1;
            continue;
        }
        match serde_json::from_slice::<Value>(trimmed) {
            Ok(Value::Object(map)) => {
                let mut fields = flatten_object(map);
                ensure_text_passage(&mut fields);
                stats.records += 1;
                if !sink(RawRecord {
                    fields,
                    locator: format!("b{start}"),
                    group: None,
                    origin: FieldOrigin::Data,
                }) {
                    break;
                }
            }
            Ok(_) | Err(_) => stats.junk += 1,
        }
    }
    finish_empty_refusal_len(path, file_len, limit, &mut stats);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    /// A Google Keep export is a document: title/body, labels and dates as
    /// fields, extractor-origin so it joins the document dataset.
    #[test]
    fn a_google_keep_note_becomes_a_document() {
        let (stats, recs) = run(
            r#"{"color":"DEFAULT","isTrashed":false,"isPinned":true,"isArchived":false,
                "textContent":"Call Dana about the term sheet.\nBring the 設計書.",
                "title":"Acquisition follow-ups",
                "userEditedTimestampUsec":1700000000000000,
                "createdTimestampUsec":1690000000000000,
                "labels":[{"name":"work"},{"name":"deal"}],
                "annotations":[{"url":"https://example.org/ts","title":"Term sheet"}]}"#,
        );
        assert_eq!((stats.records, stats.junk), (1, 0));
        let r = &recs[0];
        assert_eq!(r.locator, "note-s0");
        assert_eq!(r.origin, FieldOrigin::Extractor);
        assert_eq!(
            r.fields["title"],
            serde_json::json!("Acquisition follow-ups")
        );
        let body = r.fields["body"].as_str().unwrap();
        assert!(body.contains("Bring the 設計書."), "{body}");
        assert!(
            body.contains("Term sheet — https://example.org/ts"),
            "{body}"
        );
        assert_eq!(r.fields["keep_labels"], serde_json::json!(["work", "deal"]));
        assert_eq!(
            r.fields["keep_created"],
            serde_json::json!("2023-07-22T04:26:40Z")
        );
        assert_eq!(
            r.fields["keep_edited"],
            serde_json::json!("2023-11-14T22:13:20Z")
        );
        assert_eq!(r.fields["keep_trashed"], serde_json::json!(false));
        assert_eq!(r.fields["keep_pinned"], serde_json::json!(true));
        assert!(r.fields.get("textContent").is_none(), "not a raw data row");
    }

    /// A checklist note must stay ONE document — the generic array rule would
    /// make every bullet its own record.
    #[test]
    fn a_keep_checklist_is_one_document_not_one_record_per_bullet() {
        let (stats, recs) = run(
            r#"{"isTrashed":true,"isArchived":false,"isPinned":false,"title":"",
                "listContent":[{"text":"مرحبا milk","isChecked":true},
                               {"text":"eggs","isChecked":false},{"text":"flour","isChecked":false}],
                "userEditedTimestampUsec":1700000000000000,
                "createdTimestampUsec":1700000000000000}"#,
        );
        assert_eq!(stats.records, 1);
        assert_eq!(
            recs[0].fields["body"],
            serde_json::json!("- [x] مرحبا milk\n- [ ] eggs\n- [ ] flour")
        );
        assert_eq!(
            recs[0].fields["title"],
            serde_json::json!("- [x] مرحبا milk"),
            "a title-less note is titled by its first line"
        );
        assert_eq!(
            recs[0].fields["keep_trashed"],
            serde_json::json!(true),
            "trashed notes are indexed and flagged, not silently dropped"
        );
    }

    /// The title fallback cuts by CHARS. Every length around the cut, in a
    /// 3-byte and a 2-byte script, so a byte-offset slice would land inside a
    /// character on most of them (panic = abort).
    #[test]
    fn a_keep_title_is_cut_on_a_char_boundary() {
        for unit in ["設", "م", "é", "a"] {
            for n in (KEEP_TITLE_CHARS - 2)..(KEEP_TITLE_CHARS + 3) {
                let text = unit.repeat(n);
                let note = format!(
                    r#"{{"isTrashed":false,"textContent":"{text}","title":"  ",
                        "userEditedTimestampUsec":1,"createdTimestampUsec":1}}"#
                );
                let (_, recs) = run(&note);
                let title = recs[0].fields["title"].as_str().unwrap();
                assert_eq!(
                    title.chars().count(),
                    n.min(KEEP_TITLE_CHARS),
                    "{unit} x{n}"
                );
            }
        }
    }

    /// Only the full Keep key set is a note. Ordinary JSON that shares a key or
    /// two keeps the generic data-row treatment.
    #[test]
    fn json_that_merely_resembles_a_keep_note_is_left_alone() {
        for text in [
            r#"{"textContent":"hello","title":"t"}"#,
            r#"{"isTrashed":false,"createdTimestampUsec":1,"userEditedTimestampUsec":2}"#,
            r#"{"isTrashed":"no","textContent":"x","createdTimestampUsec":1,"userEditedTimestampUsec":2}"#,
            r#"{"isTrashed":false,"textContent":"x","createdTimestampUsec":"1","userEditedTimestampUsec":2}"#,
        ] {
            let (_, recs) = run(text);
            assert_eq!(recs[0].locator, "doc", "{text}");
            assert_eq!(recs[0].origin, FieldOrigin::Data, "{text}");
        }
    }

    #[test]
    fn an_array_of_objects_becomes_one_record_per_element() {
        let (stats, recs) = run(
            r#"[{"id":1,"user":{"name":"ann","addr":{"city":"NY"}},"tags":["a","b"]},{"id":2}]"#,
        );
        assert_eq!(stats.records, 2);
        assert_eq!(stats.junk, 0);
        assert_eq!(recs[0].locator, "e0");
        assert_eq!(recs[1].locator, "e1");
        assert_eq!(recs[0].fields["id"], serde_json::json!(1));
        assert_eq!(
            recs[0].fields["user_name"],
            serde_json::json!("ann"),
            "two levels of nesting flatten into a_b keys"
        );
        assert_eq!(recs[0].fields["user_addr_city"], serde_json::json!("NY"));
        assert_eq!(
            recs[0].fields["tags"],
            serde_json::json!(["a", "b"]),
            "an array of scalars stays an array"
        );
        assert_eq!(
            recs[1].fields.len(),
            2,
            "absent keys are not filled in — the record's own fields plus its \
             synthesized passage (`id: 2`), nothing else"
        );
        assert_eq!(recs[1].fields["text"], serde_json::json!("id: 2"));
    }

    #[test]
    fn a_wrapper_object_lifts_its_largest_array_and_shares_the_outer_scalars() {
        let (stats, recs) = run(r#"{"generated":"2026-01-01","count":2,
                "items":[{"id":1},{"id":2,"generated":"per-item"}],
                "errors":[{"e":1}]}"#);
        assert_eq!(stats.records, 2, "records come from the biggest array");
        assert_eq!(recs[0].locator, "items:e0");
        assert_eq!(recs[0].fields["generated"], serde_json::json!("2026-01-01"));
        assert_eq!(recs[0].fields["count"], serde_json::json!(2));
        assert_eq!(
            recs[1].fields["generated"],
            serde_json::json!("per-item"),
            "the element wins a collision with a shared field"
        );
        assert_eq!(
            recs[0].fields["errors"],
            serde_json::json!(r#"[{"e":1}]"#),
            "the other arrays of objects travel as JSON strings"
        );
    }

    #[test]
    fn an_object_with_no_repeating_array_is_a_single_document_record() {
        let (stats, recs) =
            run(r#"{"meta":"m","nested":{"deep":{"deeper":{"x":1}}},"items":[{"id":1}]}"#);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].locator, "doc");
        assert_eq!(recs[0].fields["meta"], serde_json::json!("m"));
        assert_eq!(
            recs[0].fields["items"],
            serde_json::json!(r#"[{"id":1}]"#),
            "a single-element array is below the record-lifting threshold"
        );
        assert_eq!(
            recs[0].fields["nested_deep_deeper"],
            serde_json::json!(r#"{"x":1}"#),
            "structure past two levels is stored as a JSON string"
        );
    }

    #[test]
    fn a_bare_scalar_document_is_wrapped_in_a_value_field() {
        let (stats, recs) = run("42");
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].locator, "doc");
        assert_eq!(recs[0].fields["value"], serde_json::json!(42));

        let (stats, recs) = run("[1,2,3]");
        assert_eq!(stats.records, 3, "array elements each become a record");
        assert_eq!(recs[2].fields["value"], serde_json::json!(3));
    }

    #[test]
    fn a_data_field_in_the_provenance_namespace_is_renamed_not_dropped() {
        let (_, recs) = run(r#"{"ax_kind":"invoice","ok":1}"#);
        assert_eq!(recs[0].fields["data_ax_kind"], serde_json::json!("invoice"));
        assert!(recs[0].fields.get("ax_kind").is_none());
    }

    #[test]
    fn truncated_json_is_junk_filed_and_emits_nothing() {
        let (stats, recs) = run(r#"{"a":1,"#);
        assert_eq!((stats.records, stats.junk), (0, 1));
        assert!(recs.is_empty());

        let (stats, recs) = run("not json at all");
        assert_eq!((stats.records, stats.junk), (0, 1));
        assert!(recs.is_empty());
    }

    /// The published rust-vulns pack's osv half is a JSONL file whose first
    /// line (an OSV record with a large `affected` array) exceeds the sniff
    /// prefix — zero complete lines in the sample, so the file sniffs as
    /// `Json` and arrives HERE, not at the JSONL extractor. It must index
    /// line by line, not junk: `corpus index` accepted 1,230 of 1,963
    /// records as a success before this fallback existed.
    #[test]
    fn a_jsonl_file_whose_first_line_beats_the_sniff_prefix_still_indexes_per_line() {
        let fat = format!(
            r#"{{"id":"OSV-1","affected":{:?}}}"#,
            vec!["x".repeat(9_000); 2usize]
        );
        assert!(
            fat.len() > 8_192,
            "the first line must beat the 8 KB sample"
        );
        let text = format!("{fat}\n{{\"id\":\"OSV-2\"}}\n");
        let (stats, recs) = run(&text);
        assert_eq!((stats.records, stats.junk), (2, 0));
        assert_eq!(recs[0].locator, "b0", "byte-offset locator, JSONL contract");
        assert_eq!(recs[1].fields["id"], serde_json::json!("OSV-2"));

        // A mixed file — some parseable lines, one corrupt — keeps the
        // JSONL semantics: good lines index, the corrupt one junks.
        let (stats, _) = run("{\"id\":\"a\"}\n{broken\n{\"id\":\"c\"}\n");
        assert_eq!((stats.records, stats.junk), (2, 1));
    }

    /// A nested array of objects is stored as one JSON string. Past
    /// [`NESTED_JSON_FIELD_MAX`] that string is cut, so a 12 MB nested
    /// file cannot become one analyzed text field. A value under the cap
    /// is stored whole.
    #[test]
    fn a_large_nested_value_is_cut_before_it_becomes_a_field() {
        let cap = super::super::NESTED_JSON_FIELD_MAX;
        let big = "x".repeat(cap + 1024);
        let text = format!(r#"{{"id":"row","payload":[{{"blob":"{big}"}}]}}"#);
        let (stats, recs) = run(&text);
        assert_eq!(stats.records, 1);
        let payload = recs[0].fields["payload"].as_str().unwrap();
        assert!(
            payload.ends_with("…[truncated]"),
            "over-cap nested JSON must say it was cut: {} bytes",
            payload.len()
        );
        assert!(
            payload.len() <= cap + "…[truncated]".len(),
            "field is {} bytes, cap is {cap}",
            payload.len()
        );
        assert_eq!(recs[0].fields["id"], serde_json::json!("row"));

        let (stats, recs) = run(r#"{"id":"row","payload":[{"n":1}]}"#);
        assert_eq!(stats.records, 1);
        let payload = recs[0].fields["payload"].as_str().unwrap();
        assert!(
            !payload.contains("truncated"),
            "a small nested value is stored whole: {payload}"
        );
        assert!(payload.contains("\"n\":1"), "{payload}");
    }

    #[test]
    fn an_empty_array_yields_nothing_at_all_without_being_junk() {
        let (stats, recs) = run("[]");
        assert_eq!((stats.records, stats.junk), (0, 0));
        assert!(recs.is_empty());
    }

    // ─── #1158: synthesized text passages ────────────────────────────────

    /// The ghsa-db class, in miniature: a record of ids, enums, numbers and
    /// dates carries nothing passage search can match. Its own fields become
    /// the passage, so a query naming any of them reaches the record.
    #[test]
    fn a_record_with_no_prose_gains_a_synthesized_text_passage() {
        let (_, recs) = run(r#"[{"sha256":"a3f19c","size":123456,"severity":"HIGH",
                 "modified":"2026-01-01","aliases":["CVE-2026-0001","GHSA-xx"]}]"#);
        let text = recs[0].fields["text"].as_str().unwrap();
        for expect in [
            "sha256: a3f19c",
            "size: 123456",
            "severity: HIGH",
            "modified: 2026-01-01",
            "aliases: CVE-2026-0001, GHSA-xx",
        ] {
            assert!(text.contains(expect), "passage missing {expect:?}:\n{text}");
        }
    }

    /// A record with a prose field is already reachable through it (the
    /// query side searches text-typed fields), and a rendered key/value soup
    /// would only outrank the real prose. No synthesis.
    #[test]
    fn a_record_with_prose_gains_no_synthesized_passage() {
        let (_, recs) = run(
            r#"[{"id":"GHSA-1","summary":"Use after free in the connection pool cleanup path"}]"#,
        );
        assert!(
            recs[0].fields.get("text").is_none(),
            "a prose record must not gain a synthesized passage: {:?}",
            recs[0].fields
        );
    }

    /// A record that owns a field named `text` keeps it, whatever its length:
    /// the record named that field, and its dataset already maps it.
    #[test]
    fn an_existing_text_field_is_never_clobbered() {
        let (_, recs) = run(r#"[{"text":"tiny","n":1}]"#);
        assert_eq!(recs[0].fields["text"], serde_json::json!("tiny"));
    }

    /// The passage is bounded even for a record with hundreds of fields. The
    /// final cut goes through `chars()`, so it lands on a character boundary
    /// and never slices a multi-byte character in half.
    #[test]
    fn the_synthesized_passage_is_hard_capped() {
        let fields: Vec<String> = (0..900)
            .map(|i| format!(r#""f{i}":"{}""#, "v".repeat(40)))
            .collect();
        let (_, recs) = run(&format!("[{{{}}}]", fields.join(",")));
        let text = recs[0].fields["text"].as_str().unwrap();
        assert!(
            text.chars().count() <= super::super::SYNTH_PASSAGE_MAX_CHARS,
            "passage is {} chars, cap is {}",
            text.chars().count(),
            super::super::SYNTH_PASSAGE_MAX_CHARS
        );

        // A record whose values are multi-byte: 20 fields x the 256-char
        // per-value cap pushes the pastiche past the total cap, so the FINAL
        // truncation runs — on 設, a 3-byte character. `chars().take()` cannot
        // produce an invalid string, which is exactly what is being pinned.
        let mut m = serde_json::Map::new();
        for i in 0..20 {
            m.insert(
                format!("k{i}"),
                serde_json::Value::String("設".repeat(9_000)),
            );
        }
        assert!(super::ensure_text_passage(&mut m));
        let passage = m["text"].as_str().unwrap();
        assert_eq!(
            passage.chars().count(),
            super::super::SYNTH_PASSAGE_MAX_CHARS,
            "a passage over the cap is cut to exactly the cap"
        );
    }

    /// A mixed dataset is handled per record: prose rows stay as they were,
    /// prose-less rows gain the passage. The prose field still maps text for
    /// the whole dataset either way.
    #[test]
    fn synthesis_is_per_record_inside_one_dataset() {
        let (_, recs) = run(
            r#"[{"id":1,"note":"a genuinely long prose sentence about pooling"},
                {"id":2,"count":7}]"#,
        );
        assert!(recs[0].fields.get("text").is_none());
        assert!(recs[1].fields["text"]
            .as_str()
            .unwrap()
            .contains("count: 7"));
    }

    /// #1856 / #1978: two top-level arrays of objects are two datasets.
    /// The smaller array used to be serialized and cloned onto every row
    /// of the larger one, and that JSON string then won `semantic_text`.
    #[test]
    fn two_top_level_arrays_stay_separate_datasets() {
        let mut functions = Vec::new();
        for i in 0..MULTI_ARRAY_SPLIT_MIN {
            functions.push(format!(r#"{{"name":"f{i}","addr":{i}}}"#));
        }
        let text = format!(
            r#"{{"generated":"2026-01-01","functions":[{}],"runs":[{{"label":"boot","start":1}},{{"label":"loop","start":9}}]}}"#,
            functions.join(",")
        );
        let (stats, recs) = run(&text);
        let primary: Vec<_> = recs.iter().filter(|r| r.group.is_none()).collect();
        let runs: Vec<_> = recs
            .iter()
            .filter(|r| r.group.as_deref() == Some("runs"))
            .collect();
        assert_eq!(primary.len(), MULTI_ARRAY_SPLIT_MIN);
        assert_eq!(primary[0].locator, "functions:e0");
        assert!(
            primary[0].fields.get("runs").is_none(),
            "the sibling array must not be copied onto the primary row: {:?}",
            primary[0].fields
        );
        assert_eq!(
            primary[0].fields["generated"],
            serde_json::json!("2026-01-01"),
            "a small shared scalar is still merged"
        );
        assert_eq!(primary[0].fields["name"], serde_json::json!("f0"));
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].locator, "runs:e0");
        assert_eq!(runs[0].fields["label"], serde_json::json!("boot"));
        assert!(runs[0].fields.get("name").is_none());
        assert_eq!(runs[0].fields["generated"], serde_json::json!("2026-01-01"));
        assert!(stats
            .populations
            .contains(&(None, MULTI_ARRAY_SPLIT_MIN as u64)));
        assert!(stats.populations.contains(&(Some("runs".into()), 2)));
    }

    /// Equal lengths must not depend on object-key order.
    #[test]
    fn equal_length_arrays_pick_the_lexicographically_smaller_key_as_primary() {
        let row = r#"{"n":1},{"n":2},{"n":3},{"n":4},{"n":5},{"n":6},{"n":7},{"n":8},{"n":9},{"n":10},{"n":11},{"n":12},{"n":13},{"n":14},{"n":15},{"n":16},{"n":17},{"n":18},{"n":19},{"n":20},{"n":21},{"n":22},{"n":23},{"n":24},{"n":25},{"n":26},{"n":27},{"n":28},{"n":29},{"n":30},{"n":31},{"n":32},{"n":33}"#;
        let text = format!(r#"{{"zeta":[{row}],"alpha":[{row}]}}"#);
        let (_, recs) = run(&text);
        let primary: Vec<_> = recs.iter().filter(|r| r.group.is_none()).collect();
        assert_eq!(primary.len(), MULTI_ARRAY_SPLIT_MIN);
        assert_eq!(primary[0].locator, "alpha:e0");
        assert!(recs.iter().any(|r| r.group.as_deref() == Some("zeta")));
        assert!(recs.iter().all(|r| r.group.as_deref() != Some("alpha")));
    }

    /// A wrapper value too big to clone is one sidecar record, not a field
    /// on every row. A value under the cap stays shared, which is what the
    /// single-element `errors` array in the largest-array test pins.
    #[test]
    fn an_oversized_shared_value_is_emitted_once() {
        let blob = "x".repeat(SHARED_REPLICATE_MAX + 32);
        let text = format!(r#"{{"blob":"{blob}","items":[{{"id":1}},{{"id":2}},{{"id":3}}]}}"#);
        let (stats, recs) = run(&text);
        let rows: Vec<_> = recs.iter().filter(|r| r.group.is_none()).collect();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter().all(|r| r.fields.get("blob").is_none()),
            "the blob must not be cloned onto every row"
        );
        let side: Vec<_> = recs.iter().filter(|r| r.group.is_some()).collect();
        assert_eq!(side.len(), 1);
        assert_eq!(side[0].group.as_deref(), Some("blob"));
        assert_eq!(side[0].locator, "blob:shared");
        assert!(side[0].fields["blob"].as_str().unwrap().len() > SHARED_REPLICATE_MAX);
        assert_eq!(
            stats
                .populations
                .iter()
                .find(|(g, _)| g.is_none())
                .map(|(_, n)| *n),
            Some(3)
        );
        assert!(stats.populations.contains(&(Some("blob".into()), 1)));
    }

    /// The sink stopping one group (the phase-A sample) must not drop the
    /// declared length or the next array.
    #[test]
    fn stopping_one_array_keeps_its_length_and_the_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        let mut functions = Vec::new();
        for i in 0..MULTI_ARRAY_SPLIT_MIN {
            functions.push(format!(r#"{{"name":"f{i}"}}"#));
        }
        let text = format!(
            r#"{{"functions":[{}],"runs":[{{"label":"a"}},{{"label":"b"}},{{"label":"c"}}]}}"#,
            functions.join(",")
        );
        std::fs::write(&path, text).unwrap();
        let mut seen = std::collections::HashMap::<Option<String>, usize>::new();
        let stats = extract(&path, false, &mut |r| {
            let n = seen.entry(r.group.clone()).or_insert(0);
            *n += 1;
            *n < 2
        })
        .unwrap();
        assert_eq!(seen.get(&None).copied(), Some(2));
        assert_eq!(seen.get(&Some("runs".into())).copied(), Some(2));
        assert!(stats
            .populations
            .contains(&(None, MULTI_ARRAY_SPLIT_MIN as u64)));
        assert!(stats.populations.contains(&(Some("runs".into()), 3)));
    }

    fn run_limited(text: &str, limit: usize) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract_limited(
            &path,
            false,
            &mut |r| {
                recs.push(r);
                true
            },
            limit,
        )
        .unwrap();
        (stats, recs)
    }

    fn assert_same_records(dom: &[RawRecord], streamed: &[RawRecord]) {
        assert_eq!(dom.len(), streamed.len());
        for (left, right) in dom.iter().zip(streamed) {
            assert_eq!(left.locator, right.locator);
            assert_eq!(left.group, right.group);
            assert_eq!(left.origin, right.origin);
            assert_eq!(left.fields, right.fields);
        }
    }

    /// A file past the per-document ceiling must emit the same locators as the
    /// DOM path when every element itself fits. This is the 50 MB array of
    /// small objects: the DOM path is what shipped, the stream is what runs
    /// once the file is larger than the ceiling.
    #[test]
    fn a_file_over_the_document_limit_streams_the_same_records() {
        let mut functions = Vec::new();
        for i in 0..MULTI_ARRAY_SPLIT_MIN {
            functions.push(format!(r#"{{"name":"f{i}","addr":{i}}}"#));
        }
        let split = format!(
            r#"{{"generated":"2026-01-01","functions":[{}],"runs":[{{"label":"boot","start":1}},{{"label":"loop","start":9}}]}}"#,
            functions.join(",")
        );
        let array = r#"[{"id":1,"user":{"name":"ann"}},{"id":2}]"#;
        let wrapper = r#"{"generated":"2026-01-01","count":2,"items":[{"id":1},{"id":2,"generated":"per-item"}],"errors":[{"e":1}]}"#;
        // Each limit is under the file and over every element, so the stream
        // emits the same records the DOM path would.
        for (text, limit) in [
            (split.as_str(), 256usize),
            (array, 36),
            (wrapper, 80),
            ("[1,2,3]", 4),
        ] {
            assert!(
                text.len() > limit,
                "the fixture must take the streaming path ({})",
                text.len()
            );
            let (dom_stats, dom) = run_limited(text, usize::MAX);
            let (stream_stats, streamed) = run_limited(text, limit);
            assert_eq!(dom_stats.records, stream_stats.records, "{text}");
            assert_eq!(dom_stats.populations, stream_stats.populations, "{text}");
            assert_same_records(&dom, &streamed);
            assert_eq!(stream_stats.oversize_values, 0, "{text}");
        }
    }

    #[test]
    fn a_single_object_over_the_document_limit_names_the_file_and_is_not_indexed() {
        let blob = "x".repeat(80);
        let text = format!(r#"{{"blob":"{blob}"}}"#);
        let (stats, recs) = run_limited(&text, 32);
        assert!(recs.is_empty());
        assert_eq!(stats.records, 0);
        assert!(stats.oversize_values >= 1);
        let reason = stats.refusal.expect("a refusal naming the file");
        assert!(reason.contains("t.json"), "{reason}");
        assert!(reason.contains("32"), "{reason}");
        assert!(reason.contains("not indexed"), "{reason}");
        assert!(!stats.truncated, "this is not the section cap");
    }

    #[test]
    fn a_huge_shared_string_is_skipped_and_the_record_array_is_indexed() {
        let blob = "x".repeat(80);
        let text = format!(r#"{{"name":"n","items":[{{"id":1}},{{"id":2}}],"blob":"{blob}"}}"#);
        let (stats, recs) = run_limited(&text, 40);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].locator, "items:e0");
        assert_eq!(recs[1].locator, "items:e1");
        assert!(recs.iter().all(|r| r.fields.get("blob").is_none()));
        assert_eq!(recs[0].fields["name"], serde_json::json!("n"));
        assert!(stats.oversize_values >= 1);
        assert!(stats.refusal.is_none(), "the records were indexed");
    }

    #[test]
    fn an_oversized_array_element_keeps_its_siblings_and_its_index() {
        let blob = "y".repeat(80);
        let text = format!(r#"[{{"id":1}},{{"blob":"{blob}"}},{{"id":2}}]"#);
        let (stats, recs) = run_limited(&text, 40);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].locator, "e0");
        assert_eq!(recs[1].locator, "e2", "the skipped element keeps its index");
        assert_eq!(recs[1].fields["id"], serde_json::json!(2));
        assert_eq!(stats.oversize_values, 1);
        assert_eq!(
            stats.populations,
            vec![(None, 3)],
            "the population is the whole array, including the skipped element"
        );
    }

    #[test]
    fn a_jsonl_line_over_the_document_limit_is_not_indexed() {
        let blob = "z".repeat(80);
        let text = format!("{{\"id\":\"small\"}}\n{{\"blob\":\"{blob}\"}}\n");
        let (stats, recs) = run_limited(&text, 40);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].fields["id"], serde_json::json!("small"));
        assert_eq!(stats.oversize_values, 1);
        assert_eq!(stats.junk, 0);
        assert!(stats.refusal.is_none());
    }

    #[test]
    fn stopping_a_streamed_array_keeps_its_population_and_the_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        let mut functions = Vec::new();
        for i in 0..MULTI_ARRAY_SPLIT_MIN {
            functions.push(format!(r#"{{"name":"f{i}"}}"#));
        }
        let text = format!(
            r#"{{"functions":[{}],"runs":[{{"label":"a"}},{{"label":"b"}},{{"label":"c"}}]}}"#,
            functions.join(",")
        );
        assert!(text.len() > 64);
        std::fs::write(&path, &text).unwrap();
        let mut seen = std::collections::HashMap::<Option<String>, usize>::new();
        let stats = extract_limited(
            &path,
            false,
            &mut |r| {
                let n = seen.entry(r.group.clone()).or_insert(0);
                *n += 1;
                *n < 2
            },
            64,
        )
        .unwrap();
        assert_eq!(seen.get(&None).copied(), Some(2));
        assert_eq!(seen.get(&Some("runs".into())).copied(), Some(2));
        assert!(stats
            .populations
            .contains(&(None, MULTI_ARRAY_SPLIT_MIN as u64)));
        assert!(stats.populations.contains(&(Some("runs".into()), 3)));
    }

    fn run_with_caps(
        text: &str,
        limit: usize,
        whole_file_cap: u64,
    ) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract_with_whole_file_cap(
            &path,
            false,
            &mut |record| {
                recs.push(record);
                true
            },
            limit,
            whole_file_cap,
        )
        .unwrap();
        (stats, recs)
    }

    fn run_disk(text: &str, limit: usize) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = stream_uncompressed_over_cap(&path, text.len() as u64, limit, &mut |record| {
            recs.push(record);
            true
        })
        .unwrap();
        (stats, recs)
    }

    /// The disk walker is what runs once a file is past 64 MiB. It must emit
    /// the same records as the DOM path, including a multi-array split and a
    /// shared value that is too big to copy onto every row.
    #[test]
    fn a_file_read_from_disk_matches_the_dom_path() {
        let mut functions = Vec::new();
        for i in 0..MULTI_ARRAY_SPLIT_MIN {
            functions.push(format!(r#"{{"name":"f{i}","addr":{i}}}"#));
        }
        let split = format!(
            r#"{{"generated":"2026-01-01","functions":[{}],"runs":[{{"label":"boot","start":1}},{{"label":"loop","start":9}}]}}"#,
            functions.join(",")
        );
        let blob = "x".repeat(SHARED_REPLICATE_MAX + 8);
        let sidecar = format!(
            r#"{{"name":"n","items":[{{"id":1}},{{"id":2,"name":"row"}}],"blob":"{blob}"}}"#
        );
        let cases = [
            split.as_str(),
            sidecar.as_str(),
            r#"[{"id":1,"user":{"name":"ann"}},{"id":2}]"#,
            r#"{"generated":"2026-01-01","count":2,"items":[{"id":1},{"id":2,"generated":"per-item"}],"errors":[{"e":1}]}"#,
            "[1,2,3]",
            "[\n  {\"id\": 1}, {\"id\": 2}\n]",
            r#"{"a":1,"note":"ok"}"#,
        ];
        for text in cases {
            let (dom_stats, dom) = run_limited(text, usize::MAX);
            let (disk_stats, disk) = run_disk(text, usize::MAX);
            assert_eq!(dom_stats.records, disk_stats.records, "{text}");
            assert_eq!(dom_stats.populations, disk_stats.populations, "{text}");
            assert_eq!(
                dom_stats.oversize_values, disk_stats.oversize_values,
                "{text}"
            );
            assert_same_records(&dom, &disk);
        }
    }

    /// Routing: a file past the whole-file cap is streamed, not refused.
    #[test]
    fn a_file_over_the_whole_file_cap_is_streamed_not_refused() {
        let text = r#"[{"id":1},{"id":2}]"#;
        let (stats, recs) = run_with_caps(text, usize::MAX, 4);
        assert!(stats.refusal.is_none(), "{:?}", stats.refusal);
        assert_eq!(stats.records, 2);
        assert_eq!(recs[0].locator, "e0");
        assert_eq!(recs[1].locator, "e1");
        assert_eq!(recs[1].fields["id"], serde_json::json!(2));
    }

    /// One element past the document ceiling is skipped. The bytes retained
    /// for that value stay within the ceiling; its siblings keep their indexes.
    #[test]
    fn a_disk_stream_skips_one_oversized_element_within_the_cap() {
        let blob = "y".repeat(200);
        let text = format!(r#"[{{"id":1}},{{"blob":"{blob}"}},{{"id":2}}]"#);
        super::JSON_SCAN_STORED_PEAK.with(|peak| peak.set(0));
        let (stats, recs) = run_with_caps(&text, 40, 8);
        let peak = super::JSON_SCAN_STORED_PEAK.with(|peak| peak.get());
        assert!(
            peak <= 40,
            "retained {peak} bytes of a value past the 40-byte cap"
        );
        assert_eq!(stats.oversize_values, 1);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].locator, "e0");
        assert_eq!(recs[1].locator, "e2");
        assert_eq!(recs[1].fields["id"], serde_json::json!(2));
        assert!(stats.refusal.is_none(), "the siblings were indexed");
    }

    #[test]
    fn a_single_oversized_document_on_disk_names_the_file_and_the_fixed_ceiling() {
        let blob = "x".repeat(80);
        let text = format!(r#"{{"blob":"{blob}"}}"#);
        let (stats, recs) = run_with_caps(&text, 32, 8);
        assert!(recs.is_empty());
        let reason = stats.refusal.expect("a refusal");
        assert!(reason.contains("t.json"), "{reason}");
        assert!(reason.contains(&text.len().to_string()), "{reason}");
        assert!(reason.contains("32"), "{reason}");
        assert!(reason.contains("not indexed"), "{reason}");
        assert!(reason.contains("--max-file-mb"), "{reason}");
        assert!(reason.contains("limits.max_process_memory_mb"), "{reason}");
    }

    /// A `.json` file whose first line is one object and whose next line is
    /// another is JSONL that sniffed as JSON once the first line outgrew the
    /// sniff prefix. Past the whole-file cap it must still index every line.
    #[test]
    fn a_jsonl_file_over_the_whole_file_cap_streams_every_line() {
        let text = "{\"id\":1}\n{\"id\":2}\n";
        let (stats, recs) = run_with_caps(text, usize::MAX, 4);
        assert_eq!(stats.records, 2, "{:?}", stats.refusal);
        assert_eq!(recs[0].locator, "b0");
        assert_eq!(recs[0].fields["id"], serde_json::json!(1));
        let second = text.find('\n').unwrap() + 1;
        assert_eq!(recs[1].locator, format!("b{second}"));
        assert_eq!(recs[1].fields["id"], serde_json::json!(2));
    }

    #[test]
    fn gzip_over_the_whole_file_cap_names_the_fixed_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json.gz");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, br#"{"id":1,"name":"ann"}"#).unwrap();
        std::fs::write(&path, encoder.finish().unwrap()).unwrap();
        let stats = extract_with_whole_file_cap(&path, true, &mut |_| true, usize::MAX, 4).unwrap();
        let reason = stats.refusal.expect("gzip is not streamed past the cap");
        assert!(reason.contains("t.json.gz"), "{reason}");
        assert!(reason.contains("--max-file-mb"), "{reason}");
        assert!(reason.contains("limits.max_process_memory_mb"), "{reason}");
        assert_eq!(stats.records, 0);
    }
}
