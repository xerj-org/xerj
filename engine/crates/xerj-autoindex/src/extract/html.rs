//! HTML — a tolerant hand-rolled tokenizer (no DOM dependency; bounded by a
//! 64MB whole-file cap). Generic extraction rule, no hardcoded names:
//! a dominant `<table>` (≥5 rows, consistent column count, header row) →
//! one record per row with header-derived field names; otherwise one
//! document record {title, headings, body}.

use super::{emit_document, sanitize_field_name, ExtractStats, RawRecord, Sink, MAX_WHOLE_FILE};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

#[derive(Default)]
struct Doc {
    title: String,
    headings: Vec<String>,
    body: String,
    tables: Vec<Vec<Vec<String>>>, // tables → rows → cells
    header_cells: Vec<Vec<bool>>,  // per table: was first row <th>?
}

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, MAX_WHOLE_FILE)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    let doc = parse(&text);

    // Dominant-table rule.
    if let Some((rows, first_row_th)) = dominant_table(&doc) {
        let header: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            rows[0]
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let mut name = if first_row_th || looks_like_header(&rows[0]) {
                        sanitize_field_name(h)
                    } else {
                        format!("col_{}", i + 1)
                    };
                    while !seen.insert(name.clone()) {
                        name.push('2');
                    }
                    name
                })
                .collect()
        };
        let data_rows: Box<dyn Iterator<Item = (usize, &Vec<String>)>> =
            if first_row_th || looks_like_header(&rows[0]) {
                Box::new(rows.iter().enumerate().skip(1))
            } else {
                Box::new(rows.iter().enumerate())
            };
        for (i, row) in data_rows {
            let mut fields = Map::new();
            for (j, cell) in row.iter().enumerate() {
                if cell.trim().is_empty() {
                    continue;
                }
                let name = header
                    .get(j)
                    .cloned()
                    .unwrap_or_else(|| format!("col_{}", j + 1));
                fields.insert(name, Value::String(cell.trim().to_string()));
            }
            if fields.is_empty() {
                continue;
            }
            stats.records += 1;
            if !sink(RawRecord {
                fields,
                locator: format!("row{i}"),
                group: None,
            }) {
                return Ok(stats);
            }
        }
        return Ok(stats);
    }

    // Document record.
    let title = if doc.title.trim().is_empty() {
        doc.headings
            .first()
            .cloned()
            .unwrap_or_else(|| stem_of(path))
    } else {
        doc.title.trim().to_string()
    };
    emit_document(&title, &doc.headings, doc.body.trim(), sink, &mut stats);
    Ok(stats)
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into())
}

fn looks_like_header(row: &[String]) -> bool {
    let numericish = |s: &str| {
        let t = s.trim();
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-'))
    };
    !row.is_empty() && row.iter().all(|c| !numericish(c) && !c.trim().is_empty())
}

fn dominant_table(doc: &Doc) -> Option<(&Vec<Vec<String>>, bool)> {
    let mut best: Option<usize> = None;
    for (i, rows) in doc.tables.iter().enumerate() {
        if rows.len() < 5 {
            continue;
        }
        let w = rows[0].len();
        if w < 2 {
            continue;
        }
        let consistent = rows.iter().filter(|r| r.len() == w).count();
        if consistent * 10 < rows.len() * 9 {
            continue;
        }
        if best
            .map(|b| rows.len() > doc.tables[b].len())
            .unwrap_or(true)
        {
            best = Some(i);
        }
    }
    best.map(|i| {
        (
            &doc.tables[i],
            doc.header_cells
                .get(i)
                .and_then(|h| h.first())
                .copied()
                .unwrap_or(false),
        )
    })
}

fn parse(html: &str) -> Doc {
    let mut doc = Doc::default();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut text_sink: Vec<&'static str> = Vec::new(); // element context stack (interned kinds)
    let mut cur_text = String::new();
    let mut skip_until: Option<&'static str> = None; // script/style

    // table state
    let mut in_table = false;
    let mut cur_table: Vec<Vec<String>> = Vec::new();
    let mut cur_row: Vec<String> = Vec::new();
    let mut cur_cell: Option<String> = None;
    let mut cur_row_th: Vec<bool> = Vec::new();
    let mut table_header_flags: Vec<bool> = Vec::new();

    let flush_text = |cur_text: &mut String,
                      ctx: &Vec<&'static str>,
                      doc: &mut Doc,
                      cur_cell: &mut Option<String>| {
        let t = normalize_ws(cur_text);
        cur_text.clear();
        if t.is_empty() {
            return;
        }
        if let Some(cell) = cur_cell.as_mut() {
            if !cell.is_empty() {
                cell.push(' ');
            }
            cell.push_str(&t);
            return;
        }
        match ctx.last().copied() {
            Some("title") => {
                if !doc.title.is_empty() {
                    doc.title.push(' ');
                }
                doc.title.push_str(&t);
            }
            Some("h") => {
                doc.headings.push(t.clone());
                doc.body.push_str(&t);
                doc.body.push_str("\n\n");
            }
            _ => {
                doc.body.push_str(&t);
                doc.body.push(' ');
            }
        }
    };

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // comment?
            if html[i..].starts_with("<!--") {
                i = html[i..]
                    .find("-->")
                    .map(|p| i + p + 3)
                    .unwrap_or(bytes.len());
                continue;
            }
            // parse tag
            let close = i + 1 < bytes.len() && bytes[i + 1] == b'/';
            let name_start = if close { i + 2 } else { i + 1 };
            let mut j = name_start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'!') {
                j += 1;
            }
            let name = html[name_start..j].to_lowercase();
            // find tag end, respecting quoted attrs
            let mut k = j;
            let mut quote: Option<u8> = None;
            while k < bytes.len() {
                let c = bytes[k];
                match quote {
                    Some(q) => {
                        if c == q {
                            quote = None;
                        }
                    }
                    None => {
                        if c == b'"' || c == b'\'' {
                            quote = Some(c);
                        } else if c == b'>' {
                            break;
                        }
                    }
                }
                k += 1;
            }
            let tag_end = k.min(bytes.len());

            if let Some(until) = skip_until {
                if close && name == until {
                    skip_until = None;
                }
                i = tag_end + 1;
                continue;
            }

            flush_text(&mut cur_text, &text_sink, &mut doc, &mut cur_cell);

            match (close, name.as_str()) {
                (false, "script") | (false, "style") => {
                    skip_until = Some(if name == "script" { "script" } else { "style" });
                }
                (false, "title") => text_sink.push("title"),
                (true, "title") => {
                    text_sink.pop();
                }
                (false, "h1") | (false, "h2") | (false, "h3") => text_sink.push("h"),
                (true, "h1") | (true, "h2") | (true, "h3") => {
                    text_sink.pop();
                }
                (false, "table") => {
                    in_table = true;
                    cur_table.clear();
                    cur_row.clear();
                    cur_row_th.clear();
                    cur_cell = None;
                    table_header_flags.clear();
                }
                (true, "table") => {
                    if let Some(c) = cur_cell.take() {
                        cur_row.push(c);
                    }
                    if !cur_row.is_empty() {
                        table_header_flags
                            .push(cur_row_th.iter().all(|&b| b) && !cur_row_th.is_empty());
                        cur_table.push(std::mem::take(&mut cur_row));
                    }
                    if !cur_table.is_empty() {
                        doc.header_cells
                            .push(vec![table_header_flags.first().copied().unwrap_or(false)]);
                        doc.tables.push(std::mem::take(&mut cur_table));
                    }
                    in_table = false;
                }
                (false, "tr") if in_table => {
                    if let Some(c) = cur_cell.take() {
                        cur_row.push(c);
                    }
                    if !cur_row.is_empty() {
                        table_header_flags
                            .push(cur_row_th.iter().all(|&b| b) && !cur_row_th.is_empty());
                        cur_table.push(std::mem::take(&mut cur_row));
                    }
                    cur_row_th.clear();
                }
                (false, "td") | (false, "th") if in_table => {
                    if let Some(c) = cur_cell.take() {
                        cur_row.push(c);
                    }
                    cur_row_th.push(name == "th");
                    cur_cell = Some(String::new());
                }
                (false, "br")
                | (false, "p")
                | (true, "p")
                | (false, "div")
                | (true, "div")
                | (false, "li")
                | (true, "tr")
                    if !doc.body.ends_with('\n') && !doc.body.is_empty() =>
                {
                    doc.body.push('\n');
                }
                _ => {}
            }
            i = tag_end + 1;
        } else {
            let next = memchr::memchr(b'<', &bytes[i..])
                .map(|p| i + p)
                .unwrap_or(bytes.len());
            cur_text.push_str(&decode_entities(&html[i..next]));
            i = next;
        }
    }
    flush_text(&mut cur_text, &text_sink, &mut doc, &mut cur_cell);
    doc
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}
