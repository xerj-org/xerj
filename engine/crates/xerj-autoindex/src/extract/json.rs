//! Whole-value JSON files (object or array), capped at 64MB.
//! - array of objects  → one record per element
//! - object with a dominant top-level array of objects → one record per
//!   element, remaining top-level scalars merged in as shared fields
//! - a Google Keep note (Takeout `Keep/*.json`) → a DOCUMENT: `title`/`body`
//!   plus labels and dates — see [`keep_note`]
//! - anything else → a single record

use super::{
    emit_document_with_fields, ensure_text_passage, flatten_object, ExtractStats, FieldOrigin,
    RawRecord, Sink, MAX_LINE, MAX_WHOLE_FILE,
};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, MAX_WHOLE_FILE)? else {
        stats.junk += 1;
        return Ok(stats);
    };
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
            return jsonl_fallback(&bytes, sink);
        }
    };
    match v {
        Value::Array(arr) => {
            for (i, el) in arr.into_iter().enumerate() {
                if !emit(el, &format!("e{i}"), sink, &mut stats) {
                    break;
                }
            }
        }
        Value::Object(mut obj) => {
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
            // find the largest top-level array-of-objects
            let mut best: Option<(String, usize)> = None;
            for (k, vv) in obj.iter() {
                if let Value::Array(a) = vv {
                    if a.len() >= 2
                        && a.iter().all(|e| e.is_object())
                        && best.as_ref().map(|(_, n)| a.len() > *n).unwrap_or(true)
                    {
                        best = Some((k.clone(), a.len()));
                    }
                }
            }
            match best {
                Some((key, _)) => {
                    let Value::Array(arr) = obj.remove(&key).unwrap() else {
                        unreachable!()
                    };
                    let shared = flatten_object(obj);
                    for (i, el) in arr.into_iter().enumerate() {
                        if let Value::Object(m) = el {
                            let mut fields = shared.clone();
                            for (k, v) in flatten_object(m) {
                                fields.insert(k, v); // element wins collisions
                            }
                            ensure_text_passage(&mut fields);
                            stats.records += 1;
                            if !sink(RawRecord {
                                fields,
                                locator: format!("{key}:e{i}"),
                                group: None,
                                origin: FieldOrigin::Data,
                            }) {
                                break;
                            }
                        }
                    }
                }
                None => {
                    emit(Value::Object(obj), "doc", sink, &mut stats);
                }
            }
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

/// Whole-file parse failed — re-parse the same in-memory bytes line by
/// line under the JSONL contract (see the fallback note in [`extract`]).
/// Mirrors `jsonl::extract`'s per-line semantics: blank lines skip, a line
/// at the 16 MB cap junks, an object becomes one flattened record with a
/// byte-offset locator, anything else junks. No `limit_bytes` exists here
/// (the family reads the whole file), so there is no truncated-tail
/// exemption to carry over.
fn jsonl_fallback(bytes: &[u8], sink: Sink) -> Result<ExtractStats> {
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
    Ok(stats)
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
}
