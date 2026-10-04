//! XLSX — Excel workbooks read as tables, on the CSV/SQLite record model.
//!
//! Each worksheet is its own dataset (`RawRecord.group` = sheet name, as a
//! SQLite table is) and each row below the sheet's header row is one record,
//! keyed by the header cells. Values keep the type Excel stored: numbers stay
//! numbers, booleans stay booleans, and a number whose cell format is a date
//! format becomes an ISO-8601 date string, so type inference elects `date`
//! (with real min/max ranges for `/_ask`) instead of seeing serial numbers
//! like `45839`. A formula contributes its cached value.
//!
//! The header is the first row, among the first `HEADER_SCAN_ROWS` non-empty
//! rows, that has at least two cells, holds text and no numbers or booleans,
//! and is not followed by a wider row of the same kind. Rows above it (a
//! report title, a "prepared by" line) are not part of the table and are not
//! indexed. A sheet with no such row — a form, a key/value assumptions tab,
//! free-form notes — is indexed as a document instead: one line per row with
//! cells joined by ` | `, so its text stays searchable.
//!
//! A cell merged DOWN over several rows (a pandas MultiIndex export, a
//! category label beside its group of line items) stores its value only in
//! the top row; every row it covers gets that value, so `region: East`
//! matches all of East's rows and a `terms` aggregation counts them. Merges
//! ACROSS columns are not expanded: a title merged over a table's width stays
//! one cell rather than turning into a header-shaped row.
//!
//! Known limits: a sheet is one table (a second table lower on the same sheet
//! becomes rows under the first header); a header that spans two rows keeps
//! only one of them; values are the stored ones, not the displayed ones (a
//! cell showing `21%` is `0.21`); chart sheets are skipped. Hidden sheets are
//! indexed.

use super::opc::{attr, parse_rels, resolve_target, Budget};
use super::{
    emit_document_with_fields, sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink,
    MAX_FIELDS_PER_RECORD,
};
use anyhow::{Context, Result};
use chrono::{NaiveDate, Timelike};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Seek};
use std::path::Path;

// SECURITY: bound the DECOMPRESSED reads (see `opc::Budget`). The workbook,
// relationship, style and shared-string parts are held whole, so they get the
// small cap. Worksheets are streamed row by row and never held whole, so their
// cap only bounds the time a crafted part can cost.
const MAX_PART_BYTES: u64 = 64 << 20;
const MAX_SHEET_BYTES: u64 = 1 << 30;
const MAX_TOTAL_DECOMPRESSED_BYTES: u64 = 2 << 30;

/// Text kept from one sheet indexed as a document.
const MAX_DOC_SHEET_BYTES: usize = 16 << 20;

/// Worksheet XML kept from the `<mergeCells>` tag on, for reading the merge
/// list. One `<mergeCell ref="A1:B2"/>` is about 25 bytes, so this holds
/// several hundred thousand merges; the rest of a longer list is not read.
const MAX_MERGE_TAIL_BYTES: usize = 16 << 20;

/// Largest sheet (decompressed) whose merges a SAMPLING run reads; above it
/// the sample is taken without fill-down. 64 MB is about 0.35 s of scanning
/// at the measured rate.
const MAX_SAMPLE_MERGE_SCAN_BYTES: u64 = 64 << 20;

/// Non-empty rows searched for a header before the sheet is treated as a
/// document.
const HEADER_SCAN_ROWS: usize = 20;

const REL_WORKSHEET: &str = "/relationships/worksheet";
const REL_SHARED_STRINGS: &str = "/relationships/sharedStrings";
const REL_STYLES: &str = "/relationships/styles";

#[derive(Clone, Copy)]
struct Limits {
    part: u64,
    sheet: u64,
    total: u64,
    doc_sheet: usize,
    merge_tail: usize,
    sample_merge_scan: u64,
}

const LIMITS: Limits = Limits {
    part: MAX_PART_BYTES,
    sheet: MAX_SHEET_BYTES,
    total: MAX_TOTAL_DECOMPRESSED_BYTES,
    doc_sheet: MAX_DOC_SHEET_BYTES,
    merge_tail: MAX_MERGE_TAIL_BYTES,
    sample_merge_scan: MAX_SAMPLE_MERGE_SCAN_BYTES,
};

/// `name` is the file as the corpus names it, which under durable preparation
/// is not `path` (a sealed snapshot blob); document sheets are titled from it.
/// `per_sheet_limit` caps the rows taken from each sheet (phase-A sampling,
/// like SQLite's per-table cap); `None` reads every row.
pub fn extract(
    path: &Path,
    name: &Path,
    per_sheet_limit: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    extract_bounded(path, name, per_sheet_limit, sink, LIMITS)
}

fn extract_bounded(
    path: &Path,
    name: &Path,
    per_sheet_limit: Option<u64>,
    sink: Sink,
    limits: Limits,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let f = std::fs::File::open(path)?;
    let mut z = zip::ZipArchive::new(f).context("open xlsx container")?;
    let mut budget = Budget::new(limits.part, limits.total);

    let wb = budget
        .read(&mut z, "xl/workbook.xml")
        .map(|x| parse_workbook(&x))
        .unwrap_or_default();
    let rels = budget
        .read(&mut z, "xl/_rels/workbook.xml.rels")
        .map(|x| parse_rels(&x))
        .unwrap_or_default();
    let part_of = |kind: &str, fallback: &str| {
        rels.iter()
            .find(|r| r.kind.ends_with(kind))
            .map(|r| resolve_target("xl", &r.target))
            .unwrap_or_else(|| fallback.to_string())
    };
    let ctx = Ctx {
        shared: budget
            .read(&mut z, &part_of(REL_SHARED_STRINGS, "xl/sharedStrings.xml"))
            .map(|x| parse_shared_strings(&x))
            .unwrap_or_default(),
        formats: budget
            .read(&mut z, &part_of(REL_STYLES, "xl/styles.xml"))
            .map(|x| parse_styles(&x))
            .unwrap_or_default(),
        date1904: wb.date1904,
    };
    let sheets = sheet_parts(&z, &wb, &rels);

    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into());
    budget.part = limits.sheet;
    for (name, part) in sheets {
        let mut sheet = SheetState::new(name, per_sheet_limit, limits.doc_sheet);
        // The merge list follows the cell data, so it is read in a pass of
        // its own before the rows are streamed. That pass reads the whole
        // sheet (measured: ~10% of a full extraction), which a sampling run
        // reading a few hundred rows must not pay on a large sheet; there the
        // sample goes without fill-down.
        let scan = per_sheet_limit.is_none()
            || z.by_name(&part)
                .is_ok_and(|e| e.size() <= limits.sample_merge_scan);
        let mut fill = FillDown::new(if scan {
            budget
                .stream(&mut z, &part, |r| scan_merges(r, limits.merge_tail))
                .unwrap_or_default()
        } else {
            Vec::new()
        });
        let flow = budget
            .stream(&mut z, &part, |r| {
                read_rows(r, &ctx, &mut |mut row| {
                    fill.apply(&mut row);
                    sheet.push(row, sink, &mut stats)
                })
            })
            .unwrap_or(Flow::Continue);
        if flow == Flow::Stop || !sheet.finish(&stem, sink, &mut stats) {
            break;
        }
    }
    if budget.exhausted {
        stats.truncated = true;
    }
    if stats.records == 0 {
        stats.junk += 1;
    }
    Ok(stats)
}

/// Worksheet `(name, part)` pairs in workbook (tab) order. Chart sheets and
/// dialog sheets have other relationship types and are skipped. Falls back to
/// `xl/worksheets/sheetN.xml` in numeric order when the workbook part or its
/// relationships are unusable.
fn sheet_parts<R: Read + Seek>(
    z: &zip::ZipArchive<R>,
    wb: &Workbook,
    rels: &[super::opc::Rel],
) -> Vec<(String, String)> {
    let listed: Vec<(String, String)> = wb
        .sheets
        .iter()
        .filter_map(|(name, rid)| {
            rels.iter()
                .find(|r| &r.id == rid && r.kind.ends_with(REL_WORKSHEET))
                .map(|r| (name.clone(), resolve_target("xl", &r.target)))
        })
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    let mut numbered: Vec<(u32, String)> = z
        .file_names()
        .filter_map(|n| {
            let num = n
                .strip_prefix("xl/worksheets/sheet")?
                .strip_suffix(".xml")?;
            Some((num.parse().ok()?, n.to_string()))
        })
        .collect();
    numbered.sort();
    numbered
        .into_iter()
        .map(|(i, n)| (format!("Sheet{i}"), n))
        .collect()
}

#[derive(Default)]
struct Workbook {
    /// `(name, relationship id)` in tab order.
    sheets: Vec<(String, String)>,
    /// Serial dates count from 1904-01-01 (old Mac Excel) instead of 1900.
    date1904: bool,
}

fn parse_workbook(xml: &[u8]) -> Workbook {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut wb = Workbook::default();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"workbookPr" => {
                    wb.date1904 = attr(&e, |k| k == b"date1904")
                        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
                }
                b"sheet" => {
                    // `sheetId` is a number; the relationship id is the
                    // namespaced `r:id`.
                    if let (Some(name), Some(rid)) = (
                        attr(&e, |k| k == b"name"),
                        attr(&e, |k| k.ends_with(b":id")),
                    ) {
                        wb.sheets.push((name, rid));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    wb
}

/// The shared string table: `<si>` items in order. Rich-text runs are
/// concatenated; phonetic guides (`rPh`, furigana) are not part of the value.
fn parse_shared_strings(xml: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_t = false;
    let mut in_rph = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"si" => cur.clear(),
                b"t" => in_t = true,
                b"rPh" => in_rph = true,
                _ => {}
            },
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"si" => out.push(String::new()),
            Ok(Event::Text(t)) if in_t && !in_rph => {
                cur.push_str(&t.xml10_content().unwrap_or_default());
            }
            Ok(Event::GeneralRef(r)) if in_t && !in_rph => {
                if let Some(resolved) = super::xml_x::resolve_general_ref(&r) {
                    cur.push_str(&resolved);
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"si" => out.push(std::mem::take(&mut cur)),
                b"t" => in_t = false,
                b"rPh" => in_rph = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    out
}

/// How a cell's number format says to read its stored number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fmt {
    General,
    /// A date, or a date and time: the number is a serial date.
    Date,
    /// A time of day with no date part.
    Time,
}

/// Number format of every cell style (`cellXfs`), by style index — the `s`
/// attribute of a cell.
fn parse_styles(xml: &[u8]) -> Vec<Fmt> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut custom: HashMap<u32, String> = HashMap::new();
    let mut xf_ids: Vec<u32> = Vec::new();
    let mut in_cell_xfs = false;
    let num_fmt_id = |e: &BytesStart| {
        attr(e, |k| k == b"numFmtId")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"numFmt" => {
                    if let Some(code) = attr(&e, |k| k == b"formatCode") {
                        custom.insert(num_fmt_id(&e), code);
                    }
                }
                b"cellXfs" => in_cell_xfs = true,
                // `cellStyleXfs` holds xf elements too; only `cellXfs` is
                // indexed by a cell's `s`.
                b"xf" if in_cell_xfs => xf_ids.push(num_fmt_id(&e)),
                _ => {}
            },
            Ok(Event::End(e)) if e.local_name().as_ref() == b"cellXfs" => in_cell_xfs = false,
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    xf_ids
        .into_iter()
        .map(|id| match custom.get(&id) {
            Some(code) => classify_format_code(code),
            None => builtin_format(id),
        })
        .collect()
}

/// The built-in number formats (ECMA-376 §18.8.30) that are dates or times.
/// 46 (`[h]:mm:ss`) is an elapsed duration, so it stays a number.
fn builtin_format(id: u32) -> Fmt {
    match id {
        14..=17 | 22 | 27..=36 | 50..=58 => Fmt::Date,
        18..=21 | 45 | 47 => Fmt::Time,
        _ => Fmt::General,
    }
}

/// Classify a custom format code by its date and time tokens. Quoted literals,
/// escaped characters, padding/fill directives and `[...]` blocks (colors,
/// locales, conditions) are ignored; an elapsed-time block such as `[h]` makes
/// the format a duration, which stays a number. Only the first (positive)
/// section is read.
fn classify_format_code(code: &str) -> Fmt {
    let mut tokens = String::new();
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            ';' => break,
            '"' => {
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let inner: String = chars.by_ref().take_while(|&d| d != ']').collect();
                if !inner.is_empty()
                    && inner
                        .chars()
                        .all(|d| matches!(d, 'h' | 'H' | 'm' | 'M' | 's' | 'S'))
                {
                    return Fmt::General;
                }
            }
            c => tokens.push(c.to_ascii_lowercase()),
        }
    }
    let tokens = tokens
        .replace("general", "")
        .replace("am/pm", "")
        .replace("a/p", "");
    if tokens.contains(['d', 'y']) {
        Fmt::Date
    } else if tokens.contains(['h', 's']) {
        Fmt::Time
    } else if tokens.contains('m') {
        // `m` with no hour or second next to it is a month (`mmm`, `mmmm`).
        Fmt::Date
    } else {
        Fmt::General
    }
}

/// An Excel serial date as ISO-8601: `2025-07-01` at midnight, otherwise
/// `2025-07-01T12:30:00` (with `.mmm` when there are milliseconds). `None`
/// for values that are not dates — negative, past 9999-12-31, or serial 60 in
/// the 1900 system, which is 1900-02-29, a day that never existed (Excel keeps
/// it for Lotus 1-2-3 compatibility).
fn serial_to_iso(v: f64, date1904: bool) -> Option<String> {
    if !v.is_finite() || !(0.0..2_958_466.0).contains(&v) {
        return None;
    }
    let base = if date1904 {
        NaiveDate::from_ymd_opt(1904, 1, 1)?
    } else if v < 60.0 {
        NaiveDate::from_ymd_opt(1899, 12, 31)?
    } else if v < 61.0 {
        return None;
    } else {
        NaiveDate::from_ymd_opt(1899, 12, 30)?
    };
    let ms = (v * 86_400_000.0).round() as i64;
    let dt = base.and_hms_opt(0, 0, 0)? + chrono::TimeDelta::milliseconds(ms);
    let fmt = if dt.num_seconds_from_midnight() == 0 && dt.nanosecond() == 0 {
        "%Y-%m-%d"
    } else if dt.nanosecond() == 0 {
        "%Y-%m-%dT%H:%M:%S"
    } else {
        "%Y-%m-%dT%H:%M:%S%.3f"
    };
    Some(dt.format(fmt).to_string())
}

/// A fraction of a day as `HH:MM:SS`.
fn time_of_day(v: f64) -> String {
    let secs = ((v * 86_400.0).round() as u64) % 86_400;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

struct Ctx {
    shared: Vec<String>,
    formats: Vec<Fmt>,
    date1904: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Number,
    Bool,
    /// A date or time, as an ISO-8601 string.
    Date,
}

#[derive(Debug, Clone)]
struct Cell {
    /// 1-based column (A = 1).
    col: u32,
    value: Value,
    kind: Kind,
}

impl Cell {
    fn display(&self) -> String {
        match &self.value {
            Value::String(s) => s.clone(),
            Value::Bool(true) => "TRUE".into(),
            Value::Bool(false) => "FALSE".into(),
            v => v.to_string(),
        }
    }
}

/// A row with at least one non-empty cell, cells in column order.
#[derive(Debug)]
struct Row {
    /// 1-based row number, as Excel shows it.
    num: u32,
    cells: Vec<Cell>,
}

/// A cell being read: its position, type and style, and its raw text.
struct RawCell {
    col: u32,
    t: String,
    style: usize,
    v: String,
    inline: String,
}

fn start_cell(e: &BytesStart, next_col: &mut u32) -> RawCell {
    let col = attr(e, |k| k == b"r")
        .and_then(|r| column_of(&r))
        .unwrap_or(*next_col);
    *next_col = col.saturating_add(1);
    RawCell {
        col,
        t: attr(e, |k| k == b"t").unwrap_or_default(),
        style: attr(e, |k| k == b"s")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        v: String::new(),
        inline: String::new(),
    }
}

/// Column number of a cell reference (`AB12` → 28).
fn column_of(r: &str) -> Option<u32> {
    let letters: &str = &r[..r
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(r.len())];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    Some(letters.bytes().fold(0u32, |n, b| {
        n * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32
    }))
}

/// Column letters of a column number (28 → `AB`).
fn column_letters(mut col: u32) -> String {
    let mut out = Vec::new();
    while col > 0 {
        let rem = (col - 1) % 26;
        out.push(b'A' + rem as u8);
        col = (col - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn text_cell(col: u32, s: &str) -> Option<Cell> {
    let s = s.trim();
    (!s.is_empty()).then(|| Cell {
        col,
        value: Value::String(s.to_string()),
        kind: Kind::Text,
    })
}

fn number_value(f: f64) -> Option<Value> {
    if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
        Some(Value::Number((f as i64).into()))
    } else {
        serde_json::Number::from_f64(f).map(Value::Number)
    }
}

/// The typed value of a finished cell; `None` for an empty cell, an error
/// (`#N/A`, `#DIV/0!`) or a formula with no cached value.
fn finish_cell(c: RawCell, ctx: &Ctx) -> Option<Cell> {
    let col = c.col;
    match c.t.as_str() {
        "s" => text_cell(col, ctx.shared.get(c.v.trim().parse::<usize>().ok()?)?),
        "inlineStr" => text_cell(col, &c.inline),
        // A formula's string result.
        "str" => text_cell(col, &c.v),
        "b" => match c.v.trim() {
            "" => None,
            v => Some(Cell {
                col,
                value: Value::Bool(v == "1" || v.eq_ignore_ascii_case("true")),
                kind: Kind::Bool,
            }),
        },
        "e" => None,
        // An ISO-8601 date stored as such (strict OOXML).
        "d" => text_cell(col, &c.v).map(|cell| Cell {
            kind: Kind::Date,
            ..cell
        }),
        _ => {
            let f: f64 = c.v.trim().parse().ok()?;
            let fmt = ctx.formats.get(c.style).copied().unwrap_or(Fmt::General);
            let date = match fmt {
                Fmt::General => None,
                Fmt::Time if (0.0..1.0).contains(&f) => Some(time_of_day(f)),
                Fmt::Date | Fmt::Time => serial_to_iso(f, ctx.date1904),
            };
            match date {
                Some(d) => Some(Cell {
                    col,
                    value: Value::String(d),
                    kind: Kind::Date,
                }),
                None => Some(Cell {
                    col,
                    value: number_value(f)?,
                    kind: Kind::Number,
                }),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    /// Stop reading this sheet (its row limit is reached); go on to the next.
    NextSheet,
    /// The sink stopped: stop the whole file.
    Stop,
}

/// Stream a worksheet's `sheetData`, handing each non-empty row to `on_row`.
fn read_rows(r: &mut dyn BufRead, ctx: &Ctx, on_row: &mut dyn FnMut(Row) -> Flow) -> Flow {
    let mut reader = Reader::from_reader(r);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut row: Option<Row> = None;
    let mut row_num = 0u32;
    let mut next_col = 1u32;
    let mut cell: Option<RawCell> = None;
    let (mut in_v, mut in_is, mut in_t, mut in_rph) = (false, false, false, false);
    fn start_row(e: &BytesStart, row_num: &mut u32, next_col: &mut u32) {
        *row_num = attr(e, |k| k == b"r")
            .and_then(|r| r.parse().ok())
            .unwrap_or(row_num.saturating_add(1));
        *next_col = 1;
    }
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"row" => {
                    start_row(&e, &mut row_num, &mut next_col);
                    row = Some(Row {
                        num: row_num,
                        cells: Vec::new(),
                    });
                }
                b"c" => cell = Some(start_cell(&e, &mut next_col)),
                b"v" => in_v = true,
                b"is" => in_is = true,
                b"t" => in_t = true,
                b"rPh" => in_rph = true,
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"row" => start_row(&e, &mut row_num, &mut next_col),
                // A styled cell with no value still takes up its column.
                b"c" => {
                    start_cell(&e, &mut next_col);
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Some(c) = cell.as_mut() {
                    if in_v {
                        c.v.push_str(&t.xml10_content().unwrap_or_default());
                    } else if in_is && in_t && !in_rph {
                        c.inline.push_str(&t.xml10_content().unwrap_or_default());
                    }
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let (Some(c), Some(resolved)) =
                    (cell.as_mut(), super::xml_x::resolve_general_ref(&r))
                {
                    if in_v {
                        c.v.push_str(&resolved);
                    } else if in_is && in_t && !in_rph {
                        c.inline.push_str(&resolved);
                    }
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"v" => in_v = false,
                b"is" => in_is = false,
                b"t" => in_t = false,
                b"rPh" => in_rph = false,
                b"c" => {
                    if let (Some(c), Some(r)) = (cell.take(), row.as_mut()) {
                        if let Some(done) = finish_cell(c, ctx) {
                            r.cells.push(done);
                        }
                    }
                }
                b"row" => {
                    if let Some(r) = row.take().filter(|r| !r.cells.is_empty()) {
                        let flow = on_row(r);
                        if flow != Flow::Continue {
                            return flow;
                        }
                    }
                }
                // Merged ranges, filters and print settings follow; nothing
                // after the cell data is read.
                b"sheetData" => break,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    Flow::Continue
}

/// `(column, row)` of a cell reference (`AB12` → `(28, 12)`).
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let digits = r.find(|c: char| c.is_ascii_digit())?;
    Some((column_of(r)?, r[digits..].parse().ok()?))
}

/// A merged range that spans more than one row: `(first row, last row,
/// column of its top-left cell)`. Ranges one row tall are dropped — they are
/// the across-columns merges that are deliberately not expanded.
type DownMerge = (u32, u32, u32);

/// Read a worksheet's vertical merges, sorted by first row.
///
/// `<mergeCells>` follows `<sheetData>`, so this reads the whole part, but it
/// does not parse the cells: it searches the bytes for the `<mergeCells` tag
/// (unambiguous, since `<` cannot appear unescaped in XML text) and XML-parses
/// only what follows it, up to `tail_cap` bytes. Parsing the tail as XML,
/// rather than grepping it for `ref="`, keeps the `ref` of a `<hyperlink>`
/// that follows the list from being taken for a merge.
fn scan_merges(r: &mut dyn BufRead, tail_cap: usize) -> Vec<DownMerge> {
    const TAG: &[u8] = b"mergeCells";
    // Enough of the previous chunk to see `<` + a namespace prefix + TAG
    // across a chunk boundary.
    const KEEP: usize = 64;
    let finder = memchr::memmem::Finder::new(TAG);
    let mut window: Vec<u8> = Vec::new();
    let mut tail: Option<Vec<u8>> = None;
    loop {
        let chunk = match r.fill_buf() {
            Ok([]) | Err(_) => break,
            Ok(c) => c,
        };
        let n = chunk.len();
        if let Some(t) = tail.as_mut() {
            t.extend_from_slice(&chunk[..n.min(tail_cap.saturating_sub(t.len()))]);
            r.consume(n);
            if t.len() >= tail_cap {
                break;
            }
            continue;
        }
        window.extend_from_slice(chunk);
        r.consume(n);
        let start = finder
            .find_iter(&window)
            .find_map(|i| tag_start(&window, i));
        match start {
            Some(s) => {
                let mut t = window.split_off(s);
                t.truncate(tail_cap);
                tail = Some(t);
            }
            None => {
                let cut = window.len().saturating_sub(KEEP);
                window.drain(..cut);
            }
        }
    }
    let Some(tail) = tail else {
        return Vec::new();
    };
    let mut reader = Reader::from_reader(tail.as_slice());
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.local_name().as_ref() == b"mergeCell" =>
            {
                let range = attr(&e, |k| k == b"ref").and_then(|r| {
                    let (a, b) = r.split_once(':')?;
                    Some((cell_ref(a)?, cell_ref(b)?))
                });
                if let Some(((col, first), (_, last))) = range {
                    if last > first {
                        out.push((first, last, col));
                    }
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"mergeCells" => break,
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    out.sort_unstable();
    out
}

/// Where the tag whose name starts at `i` opens: `i - 1` for `<mergeCells`,
/// or the `<` before a namespace prefix for `<x:mergeCells`. `None` when the
/// bytes at `i` are not a tag name.
fn tag_start(buf: &[u8], i: usize) -> Option<usize> {
    let before = &buf[..i];
    match before.last()? {
        b'<' => Some(i - 1),
        b':' => {
            let lt = before.iter().rposition(|&b| b == b'<')?;
            let prefix = &before[lt + 1..before.len() - 1];
            (!prefix.is_empty()
                && prefix
                    .iter()
                    .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'))
            .then_some(lt)
        }
        _ => None,
    }
}

/// Gives each row the value of any vertical merge that covers it, as rows
/// stream past in order.
struct FillDown {
    merges: Vec<DownMerge>,
    next: usize,
    /// column → (last row covered, the merge's value). Merges do not
    /// overlap, so a column has at most one live merge.
    live: HashMap<u32, (u32, Cell)>,
}

impl FillDown {
    fn new(merges: Vec<DownMerge>) -> Self {
        FillDown {
            merges,
            next: 0,
            live: HashMap::new(),
        }
    }

    /// Fill `row`'s covered cells. Only rows that exist reach this: a row
    /// whose only content would be the merged value is not invented.
    fn apply(&mut self, row: &mut Row) {
        if self.merges.is_empty() {
            return;
        }
        self.live.retain(|_, (last, _)| *last >= row.num);
        for (col, (_, cell)) in &self.live {
            if !row.cells.iter().any(|c| c.col == *col) {
                let at = row.cells.partition_point(|c| c.col < *col);
                row.cells.insert(at, cell.clone());
            }
        }
        while let Some(&(first, last, col)) = self.merges.get(self.next) {
            if first > row.num {
                break;
            }
            self.next += 1;
            // A merge whose top row was empty (and so never arrived) has no
            // value to give.
            if first == row.num {
                if let Some(c) = row.cells.iter().find(|c| c.col == col) {
                    self.live.insert(col, (last, c.clone()));
                }
            }
        }
    }
}

/// Whether a row can be a header: two or more cells, some text, and no
/// numbers or booleans. Date cells are allowed, for month-column headers.
fn is_header_candidate(row: &Row) -> bool {
    row.cells.len() >= 2
        && row
            .cells
            .iter()
            .all(|c| matches!(c.kind, Kind::Text | Kind::Date))
        && row.cells.iter().any(|c| c.kind == Kind::Text)
}

/// Index of the header row among the rows read so far, once it can be told
/// (a header needs a row after it). A candidate followed by a WIDER candidate
/// is a title line (`Prepared by | Finance`) above the real header and is
/// passed over. A wider DATA row does not disqualify a header: data often has
/// a column the header left blank.
fn find_header(rows: &[Row]) -> Option<usize> {
    rows.windows(2).position(|w| {
        is_header_candidate(&w[0])
            && !(is_header_candidate(&w[1]) && w[1].cells.len() > w[0].cells.len())
    })
}

struct Table {
    names: HashMap<u32, String>,
    /// The header row's `(column, text)`, to recognize a repeated header.
    header: Vec<(u32, String)>,
    emitted: u64,
}

impl Table {
    fn new(header: &Row) -> Self {
        let mut seen = HashSet::new();
        let mut names = HashMap::new();
        for c in &header.cells {
            let mut name = sanitize_field_name(&c.display());
            while !seen.insert(name.clone()) {
                name.push_str("_2");
            }
            names.insert(c.col, name);
        }
        Table {
            names,
            header: header.cells.iter().map(|c| (c.col, c.display())).collect(),
            emitted: 0,
        }
    }
}

enum Mode {
    /// Rows read before the header is found.
    Probe(Vec<Row>),
    Table(Table),
    /// No header: the sheet's text, one line per row.
    Doc {
        body: String,
        last_row: u32,
    },
}

struct SheetState {
    name: String,
    mode: Mode,
    limit: Option<u64>,
    doc_cap: usize,
    truncated: bool,
}

impl SheetState {
    fn new(name: String, limit: Option<u64>, doc_cap: usize) -> Self {
        SheetState {
            name,
            mode: Mode::Probe(Vec::new()),
            limit,
            doc_cap,
            truncated: false,
        }
    }

    fn push(&mut self, row: Row, sink: Sink, stats: &mut ExtractStats) -> Flow {
        match &mut self.mode {
            Mode::Probe(rows) => {
                rows.push(row);
                if let Some(i) = find_header(rows) {
                    let mut rows = std::mem::take(rows);
                    let data = rows.split_off(i + 1);
                    self.mode = Mode::Table(Table::new(&rows[i]));
                    for r in data {
                        let flow = self.push(r, sink, stats);
                        if flow != Flow::Continue {
                            return flow;
                        }
                    }
                } else if rows.len() >= HEADER_SCAN_ROWS {
                    let rows = std::mem::take(rows);
                    self.mode = Mode::Doc {
                        body: String::new(),
                        last_row: 0,
                    };
                    for r in rows {
                        self.push(r, sink, stats);
                    }
                }
                Flow::Continue
            }
            Mode::Table(t) => {
                let row_text: Vec<(u32, String)> =
                    row.cells.iter().map(|c| (c.col, c.display())).collect();
                if row_text == t.header {
                    return Flow::Continue;
                }
                let mut fields = Map::new();
                for c in row.cells {
                    if fields.len() >= MAX_FIELDS_PER_RECORD {
                        break;
                    }
                    let name = t
                        .names
                        .get(&c.col)
                        .cloned()
                        .unwrap_or_else(|| format!("col_{}", column_letters(c.col)));
                    fields.insert(name, c.value);
                }
                stats.records += 1;
                if !sink(RawRecord {
                    fields,
                    locator: format!("{}!r{}", self.name, row.num),
                    group: Some(self.name.clone()),
                    origin: FieldOrigin::Data,
                }) {
                    return Flow::Stop;
                }
                t.emitted += 1;
                match self.limit {
                    Some(lim) if t.emitted >= lim => Flow::NextSheet,
                    _ => Flow::Continue,
                }
            }
            Mode::Doc { body, last_row } => {
                if self.truncated {
                    return Flow::Continue;
                }
                let line = row
                    .cells
                    .iter()
                    .map(Cell::display)
                    .collect::<Vec<_>>()
                    .join(" | ");
                if body.len() + line.len() + 2 > self.doc_cap {
                    self.truncated = true;
                    return Flow::Continue;
                }
                if !body.is_empty() {
                    // A blank row on the sheet is a paragraph break, which is
                    // where `split_sections` prefers to cut.
                    body.push_str(if row.num > *last_row + 1 {
                        "\n\n"
                    } else {
                        "\n"
                    });
                }
                body.push_str(&line);
                *last_row = row.num;
                Flow::Continue
            }
        }
    }

    /// Emit a sheet that turned out to be a document. Returns the sink's last
    /// answer: `false` = stop extracting.
    fn finish(mut self, stem: &str, sink: Sink, stats: &mut ExtractStats) -> bool {
        if let Mode::Probe(rows) = &mut self.mode {
            let rows = std::mem::take(rows);
            self.mode = Mode::Doc {
                body: String::new(),
                last_row: 0,
            };
            for r in rows {
                self.push(r, sink, stats);
            }
        }
        if self.truncated {
            stats.truncated = true;
        }
        let Mode::Doc { body, .. } = &self.mode else {
            return true;
        };
        if body.is_empty() {
            return true;
        }
        let mut base = Map::new();
        base.insert("sheet".into(), Value::String(self.name.clone()));
        emit_document_with_fields(
            &base,
            &format!("{stem} — {}", self.name),
            body,
            &self.name,
            sink,
            stats,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::path::PathBuf;

    const NS: &str = concat!(
        r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#
    );
    const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const REL_T: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

    /// Cell styles by index: 0 General, 1 built-in date (14), 2 custom
    /// date-time, 3 custom time of day, 4 built-in percent (10), 5 custom
    /// elapsed duration. The `cellStyleXfs` date entry must not shift them.
    const STYLES: &str = concat!(
        r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
        r#"<numFmts count="3"><numFmt numFmtId="164" formatCode="yyyy\-mm\-dd\ hh:mm"/>"#,
        r#"<numFmt numFmtId="165" formatCode="h:mm"/><numFmt numFmtId="166" formatCode="[h]:mm:ss"/></numFmts>"#,
        r#"<cellStyleXfs count="1"><xf numFmtId="14"/></cellStyleXfs>"#,
        r#"<cellXfs count="6"><xf numFmtId="0" xfId="0"/><xf numFmtId="14" xfId="0" applyNumberFormat="1"/>"#,
        r#"<xf numFmtId="164" xfId="0"><alignment horizontal="left"/></xf><xf numFmtId="165" xfId="0"/>"#,
        r#"<xf numFmtId="10" xfId="0"/><xf numFmtId="166" xfId="0"/></cellXfs></styleSheet>"#
    );

    fn s(r: &str, i: usize) -> String {
        format!(r#"<c r="{r}" t="s"><v>{i}</v></c>"#)
    }
    fn n(r: &str, v: &str) -> String {
        format!(r#"<c r="{r}"><v>{v}</v></c>"#)
    }
    fn styled(r: &str, style: u32, v: &str) -> String {
        format!(r#"<c r="{r}" s="{style}"><v>{v}</v></c>"#)
    }
    fn inl(r: &str, text: &str) -> String {
        format!(r#"<c r="{r}" t="inlineStr"><is><t>{text}</t></is></c>"#)
    }
    fn b(r: &str, v: bool) -> String {
        format!(r#"<c r="{r}" t="b"><v>{}</v></c>"#, v as u8)
    }
    fn row(num: u32, cells: &[String]) -> String {
        format!(r#"<row r="{num}">{}</row>"#, cells.concat())
    }
    /// A plain shared-string item.
    fn t(text: &str) -> String {
        format!(r#"<t xml:space="preserve">{text}</t>"#)
    }

    fn sheet_xml(rows: &[String]) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet {NS}><dimension ref="A1"/><sheetData>{}</sheetData><mergeCells count="1"><mergeCell ref="A1:D1"/></mergeCells></worksheet>"#,
            rows.concat()
        )
    }

    fn rels_xml(entries: &[(&str, &str, &str)]) -> String {
        let body: String = entries
            .iter()
            .map(|(id, ty, target)| {
                format!(r#"<Relationship Id="{id}" Type="{REL_T}/{ty}" Target="{target}"/>"#)
            })
            .collect();
        format!(r#"<Relationships xmlns="{REL_NS}">{body}</Relationships>"#)
    }

    /// `sheets` are `(escaped name, rid)` in tab order.
    fn workbook_xml(sheets: &[(&str, &str)], date1904: bool) -> String {
        let pr = if date1904 {
            r#"<workbookPr date1904="1"/>"#
        } else {
            r#"<workbookPr defaultThemeVersion="164011"/>"#
        };
        let list: String = sheets
            .iter()
            .enumerate()
            .map(|(i, (name, rid))| {
                format!(r#"<sheet name="{name}" sheetId="{}" r:id="{rid}"/>"#, i + 1)
            })
            .collect();
        format!(r#"<workbook {NS}>{pr}<sheets>{list}</sheets></workbook>"#)
    }

    fn write_zip(path: &Path, parts: &[(&str, String)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            z.start_file(*name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    /// A workbook laid out the way Excel writes one. Sheet `i` is
    /// `xl/worksheets/sheet{i+1}.xml` behind `rId{i+1}`; `shared` items are raw
    /// `<si>` content.
    fn book(
        dir: &tempfile::TempDir,
        sheets: &[(&str, Vec<String>)],
        shared: &[String],
        date1904: bool,
    ) -> PathBuf {
        let xml: Vec<(&str, String)> = sheets.iter().map(|(n, r)| (*n, sheet_xml(r))).collect();
        book_xml(dir, &xml, shared, date1904)
    }

    /// A worksheet with the given rows and, after them, `tail` (the merge
    /// list and whatever else follows `sheetData`).
    fn sheet_with_tail(rows: &[String], tail: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet {NS}><sheetData>{}</sheetData>{tail}</worksheet>"#,
            rows.concat()
        )
    }

    fn book_xml(
        dir: &tempfile::TempDir,
        sheets: &[(&str, String)],
        shared: &[String],
        date1904: bool,
    ) -> PathBuf {
        let path = dir.path().join("book.xlsx");
        let n = sheets.len();
        let ids: Vec<String> = (1..=n).map(|i| format!("rId{i}")).collect();
        let targets: Vec<String> = (1..=n)
            .map(|i| format!("worksheets/sheet{i}.xml"))
            .collect();
        let part_names: Vec<String> = (1..=n)
            .map(|i| format!("xl/worksheets/sheet{i}.xml"))
            .collect();
        let listed: Vec<(&str, &str)> = sheets
            .iter()
            .zip(&ids)
            .map(|((name, _), id)| (*name, id.as_str()))
            .collect();
        let mut rels: Vec<(&str, &str, &str)> = ids
            .iter()
            .zip(&targets)
            .map(|(id, target)| (id.as_str(), "worksheet", target.as_str()))
            .collect();
        rels.push(("rIdS", "sharedStrings", "sharedStrings.xml"));
        rels.push(("rIdT", "styles", "styles.xml"));
        let sst: String = shared.iter().map(|x| format!("<si>{x}</si>")).collect();
        let mut parts: Vec<(&str, String)> = vec![
            ("[Content_Types].xml", "<Types/>".into()),
            ("xl/workbook.xml", workbook_xml(&listed, date1904)),
            ("xl/_rels/workbook.xml.rels", rels_xml(&rels)),
            (
                "xl/sharedStrings.xml",
                format!(
                    r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{sst}</sst>"#
                ),
            ),
            ("xl/styles.xml", STYLES.into()),
        ];
        for (name, (_, xml)) in part_names.iter().zip(sheets) {
            parts.push((name.as_str(), xml.clone()));
        }
        write_zip(&path, &parts);
        path
    }

    fn run_limited(
        path: &Path,
        limit: Option<u64>,
        limits: Limits,
    ) -> (ExtractStats, Vec<RawRecord>) {
        let mut recs = Vec::new();
        let stats = extract_bounded(
            path,
            path,
            limit,
            &mut |r| {
                recs.push(r);
                true
            },
            limits,
        )
        .unwrap();
        (stats, recs)
    }

    fn run(path: &Path) -> (ExtractStats, Vec<RawRecord>) {
        run_limited(path, None, LIMITS)
    }

    fn fields(r: &RawRecord) -> Value {
        Value::Object(r.fields.clone())
    }

    #[test]
    fn a_title_row_is_skipped_and_each_row_under_the_header_is_a_typed_record() {
        let dir = tempfile::tempdir().unwrap();
        let shared: Vec<String> = [
            "Q3 Sales Report",
            "Region",
            "Month",
            "Revenue",
            "Closed",
            "EMEA",
            "APAC",
        ]
        .iter()
        .map(|x| t(x))
        .collect();
        let rows = vec![
            row(1, &[s("A1", 0)]),
            row(3, &[s("A3", 1), s("B3", 2), s("C3", 3), s("D3", 4)]),
            row(
                4,
                &[
                    s("A4", 5),
                    styled("B4", 1, "45839"),
                    n("C4", "1250"),
                    b("D4", true),
                ],
            ),
            row(
                5,
                &[
                    s("A5", 6),
                    styled("B5", 1, "45870"),
                    n("C5", "980.5"),
                    b("D5", false),
                ],
            ),
        ];
        let path = book(&dir, &[("Sales", rows)], &shared, false);
        let (stats, recs) = run(&path);
        assert_eq!((stats.records, stats.junk, stats.truncated), (2, 0, false));
        let r = &recs[0];
        assert_eq!(r.group.as_deref(), Some("Sales"));
        assert_eq!(
            r.locator, "Sales!r4",
            "locators carry Excel's own row number"
        );
        assert_eq!(r.origin, FieldOrigin::Data);
        assert_eq!(
            fields(r),
            json!({"Region": "EMEA", "Month": "2025-07-01", "Revenue": 1250, "Closed": true})
        );
        assert_eq!(
            fields(&recs[1]),
            json!({"Region": "APAC", "Month": "2025-08-01", "Revenue": 980.5, "Closed": false})
        );
    }

    #[test]
    fn the_cell_format_decides_between_a_date_a_time_and_a_number() {
        let rows = vec![
            row(
                1,
                &[
                    inl("A1", "When"),
                    inl("B1", "At"),
                    inl("C1", "Share"),
                    inl("D1", "Took"),
                    inl("E1", "Day"),
                ],
            ),
            row(
                2,
                &[
                    styled("A2", 2, "45839.5"),
                    styled("B2", 3, "0.75"),
                    styled("C2", 4, "0.21"),
                    styled("D2", 5, "1.5"),
                    styled("E2", 1, "44377"),
                ],
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = book(&dir, &[("T", rows.clone())], &[], false);
        let (_, recs) = run(&path);
        assert_eq!(
            fields(&recs[0]),
            json!({
                "When": "2025-07-01T12:00:00",
                "At": "18:00:00",
                "Share": 0.21,
                "Took": 1.5,
                "Day": "2021-06-30",
            }),
            "percent and elapsed-duration formats keep the stored number"
        );

        // The same serials in a 1904-system workbook are 1462 days later.
        let dir1904 = tempfile::tempdir().unwrap();
        let path = book(&dir1904, &[("T", rows)], &[], true);
        let (_, recs) = run(&path);
        assert_eq!(recs[0].fields["Day"], json!("2025-07-01"));
        assert_eq!(recs[0].fields["When"], json!("2029-07-02T12:00:00"));
    }

    #[test]
    fn serial_dates_follow_excel_including_the_1900_leap_year_bug() {
        assert_eq!(serial_to_iso(1.0, false).as_deref(), Some("1900-01-01"));
        assert_eq!(serial_to_iso(59.0, false).as_deref(), Some("1900-02-28"));
        assert_eq!(serial_to_iso(60.0, false), None, "1900-02-29 never existed");
        assert_eq!(serial_to_iso(61.0, false).as_deref(), Some("1900-03-01"));
        assert_eq!(serial_to_iso(45839.0, false).as_deref(), Some("2025-07-01"));
        assert_eq!(
            serial_to_iso(46204.75, false).as_deref(),
            Some("2026-07-01T18:00:00")
        );
        assert_eq!(
            serial_to_iso(45839.0 + 0.123 / 86_400.0, false).as_deref(),
            Some("2025-07-01T00:00:00.123")
        );
        assert_eq!(serial_to_iso(0.0, true).as_deref(), Some("1904-01-01"));
        assert_eq!(serial_to_iso(44377.0, true).as_deref(), Some("2025-07-01"));
        assert_eq!(serial_to_iso(-1.0, false), None);
        assert_eq!(serial_to_iso(3e6, false), None);
        assert_eq!(serial_to_iso(f64::NAN, false), None);
        assert_eq!(time_of_day(0.5), "12:00:00");
        assert_eq!(
            time_of_day(0.999_999_99),
            "00:00:00",
            "rounds into the next day"
        );
    }

    #[test]
    fn format_codes_are_classified_by_their_date_and_time_tokens() {
        for (code, want) in [
            ("yyyy-mm-dd", Fmt::Date),
            ("m/d/yy h:mm", Fmt::Date),
            (r"dd\.mm\.yyyy", Fmt::Date),
            ("[$-409]mmmm d, yyyy;@", Fmt::Date),
            ("mmm", Fmt::Date),
            ("h:mm AM/PM", Fmt::Time),
            ("mm:ss", Fmt::Time),
            ("[h]:mm:ss", Fmt::General),
            ("0.00%", Fmt::General),
            (r#"#,##0.00 "days""#, Fmt::General),
            ("[Red]#,##0;[Blue]-#,##0", Fmt::General),
            (r#"_(* #,##0_);_(* (#,##0);_(* "-"_)"#, Fmt::General),
            ("[$€-2] #,##0.00", Fmt::General),
            ("0.00E+00", Fmt::General),
            ("General", Fmt::General),
            ("@", Fmt::General),
        ] {
            assert_eq!(classify_format_code(code), want, "{code}");
        }
        assert_eq!(builtin_format(14), Fmt::Date);
        assert_eq!(builtin_format(22), Fmt::Date);
        assert_eq!(builtin_format(20), Fmt::Time);
        assert_eq!(builtin_format(46), Fmt::General);
        assert_eq!(builtin_format(0), Fmt::General);
    }

    fn kv_sheet(v: &str) -> String {
        sheet_xml(&[
            row(1, &[inl("A1", "k"), inl("B1", "v")]),
            row(2, &[inl("A2", "x"), inl("B2", v)]),
        ])
    }

    /// Tab order comes from the workbook, not the part names, sheet names are
    /// unescaped, and a chart sheet (no cells) is skipped.
    #[test]
    fn each_worksheet_is_its_own_dataset_in_tab_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tabs.xlsx");
        write_zip(
            &path,
            &[
                (
                    "xl/workbook.xml",
                    workbook_xml(
                        &[("P&amp;L", "rId2"), ("First", "rId1"), ("Chart", "rId3")],
                        false,
                    ),
                ),
                (
                    "xl/_rels/workbook.xml.rels",
                    rels_xml(&[
                        ("rId1", "worksheet", "worksheets/sheet1.xml"),
                        ("rId2", "worksheet", "/xl/worksheets/sheet2.xml"),
                        ("rId3", "chartsheet", "chartsheets/sheet1.xml"),
                    ]),
                ),
                ("xl/worksheets/sheet1.xml", kv_sheet("one")),
                ("xl/worksheets/sheet2.xml", kv_sheet("two")),
                ("xl/chartsheets/sheet1.xml", kv_sheet("chart")),
            ],
        );
        let (stats, recs) = run(&path);
        assert_eq!(stats.records, 2);
        let got: Vec<(Option<&str>, &str, &Value)> = recs
            .iter()
            .map(|r| (r.group.as_deref(), r.locator.as_str(), &r.fields["v"]))
            .collect();
        assert_eq!(
            got,
            [
                (Some("P&L"), "P&L!r2", &json!("two")),
                (Some("First"), "First!r2", &json!("one")),
            ]
        );
    }

    /// No workbook part: worksheets are found by name, in numeric order.
    #[test]
    fn without_a_workbook_part_worksheets_are_read_in_numeric_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.xlsx");
        write_zip(
            &path,
            &[
                ("xl/worksheets/sheet10.xml", kv_sheet("ten")),
                ("xl/worksheets/sheet9.xml", kv_sheet("nine")),
            ],
        );
        let (_, recs) = run(&path);
        assert_eq!(
            recs.iter().map(|r| r.group.as_deref()).collect::<Vec<_>>(),
            [Some("Sheet9"), Some("Sheet10")]
        );
    }

    #[test]
    fn a_sheet_without_a_header_is_indexed_as_a_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = book(
            &dir,
            &[
                (
                    "Assumptions",
                    vec![
                        row(1, &[inl("A1", "Discount rate"), styled("B1", 4, "0.08")]),
                        row(2, &[inl("A2", "Tax rate"), styled("B2", 4, "0.21")]),
                        row(4, &[inl("A4", "Owner"), inl("B4", "Finance team")]),
                    ],
                ),
                (
                    "Data",
                    vec![
                        row(1, &[inl("A1", "a"), inl("B1", "b")]),
                        row(2, &[n("A2", "1"), n("B2", "2")]),
                    ],
                ),
            ],
            &[],
            false,
        );
        let (stats, recs) = run(&path);
        assert_eq!(stats.records, 2);
        let doc = &recs[0];
        assert_eq!(
            doc.group, None,
            "a document sheet joins the document dataset"
        );
        assert_eq!(doc.origin, FieldOrigin::Extractor);
        assert_eq!(doc.locator, "Assumptions-s0");
        assert_eq!(
            fields(doc),
            json!({
                "sheet": "Assumptions",
                "title": "book — Assumptions",
                "body": "Discount rate | 0.08\nTax rate | 0.21\n\nOwner | Finance team",
            }),
            "a blank row on the sheet is a paragraph break in the body"
        );
        assert_eq!(recs[1].group.as_deref(), Some("Data"));
        assert_eq!(recs[1].origin, FieldOrigin::Data);
    }

    /// Durable preparation extracts a sealed snapshot blob (`00000000`); the
    /// document title must still come from the file's own name (#722's class).
    #[test]
    fn a_document_sheet_is_titled_from_the_logical_name_not_the_blob() {
        let dir = tempfile::tempdir().unwrap();
        let built = book(
            &dir,
            &[("Notes", vec![row(1, &[inl("A1", "free-form notes")])])],
            &[],
            false,
        );
        let blob = dir.path().join("00000000");
        std::fs::rename(&built, &blob).unwrap();
        let sn = crate::sniff::sniff_with_name(&blob, Path::new("plans/q1.xlsx")).unwrap();
        assert_eq!(sn.family, crate::sniff::Family::Xlsx);
        let mut recs = Vec::new();
        super::super::extract(&blob, &sn, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(recs[0].fields["title"], json!("q1 — Notes"));
    }

    /// A number-only grid has no header row anywhere in the scan window, and a
    /// header-like row past the window does not count.
    #[test]
    fn a_header_is_only_looked_for_in_the_first_rows() {
        let dir = tempfile::tempdir().unwrap();
        let cell = |col: &str, i: u32, v: &str| n(&format!("{col}{i}"), v);
        let mut rows: Vec<String> = (1..=HEADER_SCAN_ROWS as u32)
            .map(|i| row(i, &[cell("A", i, "1"), cell("B", i, "2")]))
            .collect();
        let late = HEADER_SCAN_ROWS as u32 + 1;
        rows.push(row(
            late,
            &[inl(&format!("A{late}"), "x"), inl(&format!("B{late}"), "y")],
        ));
        rows.push(row(
            late + 1,
            &[cell("A", late + 1, "3"), cell("B", late + 1, "4")],
        ));
        let path = book(&dir, &[("Grid", rows)], &[], false);
        let (stats, recs) = run(&path);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].group, None);
        let body = recs[0].fields["body"].as_str().unwrap();
        assert!(
            body.starts_with("1 | 2\n1 | 2") && body.ends_with("x | y\n3 | 4"),
            "{body}"
        );
    }

    #[test]
    fn rich_text_entities_formulas_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let shared = vec![
            t("Item"),
            t("Total"),
            concat!(
                "<r><t>Net </t></r><r><rPr><b/></rPr><t>income</t></r>",
                "<rPh sb=\"0\" eb=\"1\"><t>ネット</t></rPh>"
            )
            .to_string(),
            t("P&amp;L"),
            t("Label"),
            t("Check"),
        ];
        let rows = vec![
            row(1, &[s("A1", 0), s("B1", 1), s("C1", 4), s("D1", 5)]),
            row(
                2,
                &[
                    s("A2", 2),
                    r#"<c r="B2"><f>SUM(B3:B4)</f><v>3</v></c>"#.into(),
                    r#"<c r="C2" t="str"><f>A2&amp;"!"</f><v>Net income!</v></c>"#.into(),
                    r#"<c r="D2" t="e"><f>1/0</f><v>#DIV/0!</v></c>"#.into(),
                ],
            ),
            row(
                3,
                &[
                    s("A3", 3),
                    r#"<c r="B3"><f>NOW()</f></c>"#.into(),
                    r#"<c r="C3" t="inlineStr"><is><r><t>a &lt; </t></r><r><t>b</t></r></is></c>"#
                        .into(),
                    r#"<c r="D3" t="b"><v>0</v></c>"#.into(),
                ],
            ),
        ];
        let path = book(&dir, &[("S", rows)], &shared, false);
        let (_, recs) = run(&path);
        assert_eq!(
            fields(&recs[0]),
            json!({"Item": "Net income", "Total": 3, "Label": "Net income!"}),
            "rich-text runs join; phonetic guides and error values are dropped"
        );
        assert_eq!(
            fields(&recs[1]),
            json!({"Item": "P&L", "Label": "a < b", "Check": false}),
            "a formula with no cached value is absent"
        );
    }

    #[test]
    fn header_names_are_sanitized_deduplicated_and_unheaded_columns_named_by_letter() {
        let dir = tempfile::tempdir().unwrap();
        let header = |r: u32| {
            row(
                r,
                &[
                    inl(&format!("A{r}"), "Unit price ($)"),
                    inl(&format!("B{r}"), "id"),
                    inl(&format!("C{r}"), "id"),
                    inl(&format!("E{r}"), "note"),
                ],
            )
        };
        let rows = vec![
            header(1),
            row(
                2,
                &[
                    n("A2", "9.99"),
                    n("B2", "1"),
                    n("C2", "2"),
                    inl("D2", "x"),
                    inl("E2", "y"),
                    inl("G2", "z"),
                ],
            ),
            // Some exports repeat the header at every page break.
            header(3),
            // Cells without an `r` follow the previous cell; a blank string
            // is no value.
            concat!(
                r#"<row r="4"><c r="B4"><v>7</v></c><c><v>8</v></c>"#,
                r#"<c r="E4" t="inlineStr"><is><t> </t></is></c></row>"#
            )
            .into(),
            r#"<row r="5"><c r="A5" s="1"/></row>"#.into(),
        ];
        let path = book(&dir, &[("S", rows)], &[], false);
        let (stats, recs) = run(&path);
        assert_eq!(
            stats.records, 2,
            "a repeated header and an empty row are not records"
        );
        assert_eq!(
            fields(&recs[0]),
            json!({"Unit_price": 9.99, "id": 1, "id_2": 2, "col_D": "x", "note": "y", "col_G": "z"})
        );
        assert_eq!(recs[1].locator, "S!r4");
        assert_eq!(fields(&recs[1]), json!({"id": 7, "id_2": 8}));
    }

    #[test]
    fn a_title_line_above_a_wider_header_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let rows = vec![
            row(1, &[inl("A1", "Prepared by"), inl("B1", "Finance")]),
            row(2, &[inl("A2", "Account"), inl("B2", "Q1"), inl("C2", "Q2")]),
            row(3, &[inl("A3", "Revenue"), n("B3", "100"), n("C3", "120")]),
        ];
        let path = book(&dir, &[("PL", rows)], &[], false);
        let (_, recs) = run(&path);
        assert_eq!(recs.len(), 1);
        assert_eq!(
            fields(&recs[0]),
            json!({"Account": "Revenue", "Q1": 100, "Q2": 120})
        );
    }

    #[test]
    fn the_row_limit_applies_to_each_sheet_independently() {
        let dir = tempfile::tempdir().unwrap();
        let rows = || {
            let mut rows = vec![row(1, &[inl("A1", "k"), inl("B1", "v")])];
            rows.extend(
                (2..=4).map(|i| row(i, &[n(&format!("A{i}"), "1"), n(&format!("B{i}"), "2")])),
            );
            rows
        };
        let path = book(&dir, &[("a", rows()), ("b", rows())], &[], false);
        let (stats, recs) = run_limited(&path, Some(1), LIMITS);
        assert_eq!(stats.records, 2);
        assert_eq!(
            recs.iter().map(|r| r.locator.as_str()).collect::<Vec<_>>(),
            ["a!r2", "b!r2"]
        );
        assert_eq!(run(&path).1.len(), 6);
    }

    #[test]
    fn a_workbook_with_no_cells_is_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = book(
            &dir,
            &[(
                "Empty",
                vec![r#"<row r="1"><c r="A1" s="1"/></row>"#.into()],
            )],
            &[],
            false,
        );
        let (stats, recs) = run(&path);
        assert!(recs.is_empty());
        assert_eq!((stats.records, stats.junk), (0, 1));
    }

    #[test]
    fn a_worksheet_over_its_decompression_cap_is_cut_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = vec![row(1, &[inl("A1", "k"), inl("B1", "v")])];
        rows.extend(
            (2..2000).map(|i| row(i, &[n(&format!("A{i}"), "1"), n(&format!("B{i}"), "2")])),
        );
        let path = book(&dir, &[("Big", rows)], &[], false);
        let (stats, all) = run(&path);
        assert_eq!(all.len(), 1998);
        assert!(!stats.truncated);

        let tight = Limits {
            sheet: 8 << 10,
            ..LIMITS
        };
        let (stats, some) = run_limited(&path, None, tight);
        assert!(stats.truncated);
        assert!(!some.is_empty() && some.len() < 1998, "{}", some.len());
    }

    #[test]
    fn a_document_sheet_over_its_text_cap_is_cut_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<String> = (1..=200)
            .map(|i| {
                row(
                    i,
                    &[inl(&format!("A{i}"), "a long line of free-form notes")],
                )
            })
            .collect();
        let path = book(&dir, &[("Notes", rows)], &[], false);
        let tight = Limits {
            doc_sheet: 1000,
            ..LIMITS
        };
        let (stats, recs) = run_limited(&path, None, tight);
        assert!(stats.truncated);
        let body: usize = recs
            .iter()
            .map(|r| r.fields["body"].as_str().unwrap().len())
            .sum();
        assert!(body > 0 && body <= 1000, "{body}");
    }

    #[test]
    fn column_references_round_trip() {
        for (letters, n) in [("A", 1), ("Z", 26), ("AA", 27), ("AB", 28), ("XFD", 16384)] {
            assert_eq!(column_of(&format!("{letters}12")), Some(n));
            assert_eq!(column_letters(n), letters);
        }
        assert_eq!(column_of("12"), None);
        assert_eq!(column_of("ABCD1"), None);
    }

    fn merges(tail: &str) -> String {
        format!(r#"<mergeCells count="9">{tail}</mergeCells>"#)
    }

    fn mc(r: &str) -> String {
        format!(r#"<mergeCell ref="{r}"/>"#)
    }

    /// The shape pandas writes for a MultiIndex: the outer level is stored
    /// once, in the top cell of a vertical merge.
    #[test]
    fn a_vertical_merge_gives_its_value_to_every_row_it_covers() {
        let dir = tempfile::tempdir().unwrap();
        let shared: Vec<String> = ["region", "product", "sales", "East", "West", "A", "B", "C"]
            .iter()
            .map(|x| t(x))
            .collect();
        let rows = vec![
            row(1, &[s("A1", 0), s("B1", 1), s("C1", 2)]),
            row(2, &[s("A2", 3), s("B2", 5), n("C2", "10")]),
            row(3, &[s("B3", 6), n("C3", "20")]),
            row(4, &[s("B4", 7), n("C4", "30")]),
            row(5, &[s("A5", 4), s("B5", 5), n("C5", "40")]),
            row(6, &[s("B6", 6), n("C6", "50")]),
        ];
        // Out of order, as pandas writes them, and followed by a hyperlink
        // whose `ref` must not be read as a merge.
        let tail = format!(
            r#"{}<hyperlinks><hyperlink ref="B2:B3" r:id="rId9"/></hyperlinks>"#,
            merges(&format!("{}{}", mc("A5:A6"), mc("A2:A4")))
        );
        let path = book_xml(
            &dir,
            &[("Sales", sheet_with_tail(&rows, &tail))],
            &shared,
            false,
        );
        let (_, recs) = run(&path);
        let got: Vec<Value> = recs.iter().map(fields).collect();
        assert_eq!(
            got,
            vec![
                json!({"region": "East", "product": "A", "sales": 10}),
                json!({"region": "East", "product": "B", "sales": 20}),
                json!({"region": "East", "product": "C", "sales": 30}),
                json!({"region": "West", "product": "A", "sales": 40}),
                json!({"region": "West", "product": "B", "sales": 50}),
            ]
        );
    }

    #[test]
    fn merges_across_columns_are_not_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let shared: Vec<String> = [
            "Annual Report",
            "Category",
            "Item",
            "Amount",
            "Travel",
            "Flights",
            "Hotels",
            "Note",
        ]
        .iter()
        .map(|x| t(x))
        .collect();
        let rows = vec![
            // A title merged over the table's width stays one cell, so it is
            // still not mistaken for a header.
            row(1, &[s("A1", 0)]),
            row(2, &[s("A2", 1), s("B2", 2), s("C2", 3)]),
            // A block merge (two columns, two rows) fills down its own
            // column only.
            row(3, &[s("A3", 4), s("C3", 7)]),
            row(4, &[s("B4", 6), n("C4", "800")]),
        ];
        let tail = merges(&format!("{}{}", mc("A1:C1"), mc("A3:B4")));
        let path = book_xml(
            &dir,
            &[("R", sheet_with_tail(&rows, &tail))],
            &shared,
            false,
        );
        let (_, recs) = run(&path);
        let got: Vec<Value> = recs.iter().map(fields).collect();
        assert_eq!(
            got,
            vec![
                json!({"Category": "Travel", "Amount": "Note"}),
                json!({"Category": "Travel", "Item": "Hotels", "Amount": 800}),
            ]
        );
    }

    #[test]
    fn fill_down_invents_no_rows_and_overwrites_no_values() {
        let dir = tempfile::tempdir().unwrap();
        let shared: Vec<String> = ["k", "v", "x", "own", "y"].iter().map(|x| t(x)).collect();
        let rows = vec![
            row(1, &[s("A1", 0), s("B1", 1)]),
            row(2, &[s("A2", 2), n("B2", "1")]),
            // Row 3 is absent: nothing is on it but the merged value.
            // Row 4 holds a value inside the merge (a malformed file); it is
            // kept.
            row(4, &[s("A4", 3), n("B4", "4")]),
            row(5, &[n("B5", "5")]),
            // B6:B7's top cell is empty, so it has nothing to give.
            row(7, &[s("A7", 4)]),
        ];
        let tail = merges(&format!("{}{}", mc("A2:A5"), mc("B6:B7")));
        let path = book_xml(
            &dir,
            &[("S", sheet_with_tail(&rows, &tail))],
            &shared,
            false,
        );
        let (_, recs) = run(&path);
        let got: Vec<(String, Value)> = recs
            .iter()
            .map(|r| (r.locator.clone(), fields(r)))
            .collect();
        assert_eq!(
            got,
            vec![
                ("S!r2".into(), json!({"k": "x", "v": 1})),
                ("S!r4".into(), json!({"k": "own", "v": 4})),
                ("S!r5".into(), json!({"k": "x", "v": 5})),
                ("S!r7".into(), json!({"k": "y"})),
            ]
        );
    }

    /// `scan_merges` searches raw bytes, so the cases that could fool a byte
    /// search are pinned here: a prefixed tag, a tag split across reads, the
    /// tag's name in cell text, and the tail cap.
    #[test]
    fn the_merge_scan_finds_the_tag_and_only_the_tag() {
        let scan = |xml: &str, cap: usize| {
            let mut r = std::io::BufReader::with_capacity(7, xml.as_bytes());
            scan_merges(&mut r, cap)
        };
        let body = r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>mergeCells x:mergeCells</t></is></c></row></sheetData>"#;
        assert_eq!(
            scan(
                &format!(
                    r#"<worksheet>{body}<mergeCells><mergeCell ref="B2:B9"/><mergeCell ref="A1:C1"/></mergeCells></worksheet>"#
                ),
                1 << 20
            ),
            vec![(2, 9, 2)]
        );
        assert_eq!(
            scan(
                &format!(
                    r#"<x:worksheet>{body}<x:mergeCells count="1"><x:mergeCell ref="AA10:AB12"/></x:mergeCells></x:worksheet>"#
                ),
                1 << 20
            ),
            vec![(10, 12, 27)]
        );
        assert_eq!(
            scan(&format!("<worksheet>{body}</worksheet>"), 1 << 20),
            vec![]
        );
        // A list longer than the cap keeps the merges read before it.
        let many: String = (1..=100)
            .map(|i| mc(&format!("A{}:A{}", i * 2, i * 2 + 1)))
            .collect();
        let got = scan(
            &format!("<worksheet>{body}<mergeCells>{many}</mergeCells></worksheet>"),
            200,
        );
        assert!(!got.is_empty() && got.len() < 100, "{}", got.len());
    }

    #[test]
    fn cell_references_parse_to_column_and_row() {
        assert_eq!(cell_ref("A1"), Some((1, 1)));
        assert_eq!(cell_ref("AB12"), Some((28, 12)));
        assert_eq!(cell_ref("A"), None);
        assert_eq!(cell_ref("12"), None);
    }

    #[test]
    fn a_sampling_run_skips_the_merge_scan_only_on_a_large_sheet() {
        let dir = tempfile::tempdir().unwrap();
        let shared: Vec<String> = ["k", "v", "x"].iter().map(|x| t(x)).collect();
        let rows = vec![
            row(1, &[s("A1", 0), s("B1", 1)]),
            row(2, &[s("A2", 2), n("B2", "1")]),
            row(3, &[n("B3", "2")]),
        ];
        let tail = merges(&mc("A2:A3"));
        let path = book_xml(
            &dir,
            &[("S", sheet_with_tail(&rows, &tail))],
            &shared,
            false,
        );
        let k = |recs: &[RawRecord]| {
            recs.iter()
                .map(|r| r.fields.get("k").cloned())
                .collect::<Vec<_>>()
        };
        let filled = vec![Some(json!("x")), Some(json!("x"))];
        let (_, recs) = run_limited(&path, Some(10), LIMITS);
        assert_eq!(k(&recs), filled);
        let small = Limits {
            sample_merge_scan: 10,
            ..LIMITS
        };
        let (_, recs) = run_limited(&path, Some(10), small);
        assert_eq!(k(&recs), vec![Some(json!("x")), None]);
        // A full run always reads the merges.
        let (_, recs) = run_limited(&path, None, small);
        assert_eq!(k(&recs), filled);
    }
}
