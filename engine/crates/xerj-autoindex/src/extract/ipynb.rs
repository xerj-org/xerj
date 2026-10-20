//! Jupyter notebooks (`.ipynb`, any kernel) — one record per Markdown-heading
//! section, in cell order.
//!
//! A notebook is JSON, and the generic JSON extractor made one record per
//! cell, each repeating a dozen notebook-metadata fields, with code kept as an
//! array of lines and every notebook clustered into a dataset of its own. Here
//! cells are grouped the way a reader sees them: a section starts at each
//! Markdown heading (`#` … `######`, also mid-cell, never inside a code
//! fence) and holds every cell up to the next one. Fields:
//!
//! - `title`: notebook `metadata.title`, else its first heading, else the file
//!   stem; `author` from `metadata.authors`; `language`: the kernel's
//!   (`kernelspec.language`, else `language_info.name`), so R, Julia and
//!   JavaScript notebooks say so.
//! - `heading` (the section's own) and `heading_path` (` > `-joined ancestors
//!   and itself, when it is nested); both absent before the first heading.
//! - `cell`: 1-based index of the section's first cell (what an agent needs
//!   to find it in Jupyter); `section` when a long section is split.
//! - `body`: Markdown as written; code as a fenced block in the kernel's
//!   language; text outputs after their cell as an unlabelled fenced block —
//!   `stream` text (ANSI colors stripped, `\r` progress redraws collapsed),
//!   `text/plain` results, `error` as `ename: evalue`. Outputs are capped per
//!   output. Images (and the `text/plain` stand-in shown beside one, e.g.
//!   `<Figure size …>`) and HTML-only outputs are not indexed. Nothing is
//!   ever executed.
//!
//! nbformat 3 (`worksheets`, `input`, `heading` cells, `pyout`/`pyerr`) is read
//! too. A notebook that does not parse, or has no text, is junk.

use super::{split_sections, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_RECORDS_PER_FILE};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

/// Per-output caps: a cell printing a 100k-row frame or a training log must
/// not become the notebook. Lines first, then bytes.
const MAX_OUTPUT_LINES: usize = 40;
const MAX_OUTPUT_BYTES: usize = 4 << 10;

/// Extracted-text cap across the notebook, as for DOCX/PPTX/EPUB.
const MAX_BODY_BYTES: usize = 64 << 20;

/// Whether `body` (the sniff prefix, BOM removed) is a Jupyter notebook.
///
/// Jupyter and VS Code write keys sorted, so `"cells"` is the first key and a
/// `"cell_type"` follows within the first cell. Colab writes `"nbformat"`
/// first, and nbformat 3 starts with `"metadata"`; for those an `nbformat`
/// version and a cell list must both be in the prefix. Plain JSON that merely
/// has a `cells` key (a spreadsheet export, a game level) has no `cell_type`.
pub fn looks_like_notebook(body: &str) -> bool {
    let t = body.trim_start();
    let Some(rest) = t.strip_prefix('{') else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('"') else {
        return false;
    };
    let key = rest.split('"').next().unwrap_or("");
    let has = |needle: &str| t.contains(needle);
    let nbformat_version = || {
        t.match_indices("\"nbformat\"").any(|(i, m)| {
            t[i + m.len()..]
                .trim_start()
                .strip_prefix(':')
                .is_some_and(|v| v.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        })
    };
    match key {
        "cells" => has("\"cell_type\""),
        "nbformat" | "nbformat_minor" | "metadata" => {
            nbformat_version() && (has("\"cells\"") || has("\"worksheets\""))
        }
        _ => false,
    }
}

/// `name` is the file as the corpus names it (not a snapshot blob); an
/// untitled notebook is titled from it.
pub fn extract(path: &Path, name: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, super::MAX_WHOLE_FILE)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let Ok(nb) = serde_json::from_slice::<Value>(&bytes) else {
        stats.junk += 1;
        return Ok(stats);
    };
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into());
    emit(&nb, &stem, sink, &mut stats);
    Ok(stats)
}

/// One heading-delimited run of cells.
#[derive(Debug, Default)]
struct Section {
    heading: Option<String>,
    path: Vec<String>,
    /// 1-based index of the first cell contributing to this section.
    cell: usize,
    body: String,
}

impl Section {
    fn push_block(&mut self, text: &str) {
        let text = text.trim_end();
        if text.trim().is_empty() {
            return;
        }
        if !self.body.is_empty() {
            self.body.push_str("\n\n");
        }
        self.body.push_str(text);
    }
}

fn emit(nb: &Value, stem: &str, sink: Sink, stats: &mut ExtractStats) {
    let meta = nb.get("metadata");
    let language = meta
        .and_then(|m| m.pointer("/kernelspec/language"))
        .or_else(|| meta.and_then(|m| m.pointer("/language_info/name")))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            meta.and_then(|m| m.pointer("/kernelspec/name"))
                .and_then(Value::as_str)
                .and_then(language_of_kernel)
        });
    let fence_lang = language.clone().unwrap_or_default();

    // nbformat 4 keeps `cells` at the top; nbformat 3 inside `worksheets`.
    let cells: Vec<&Value> = match nb.get("cells").and_then(Value::as_array) {
        Some(c) => c.iter().collect(),
        None => nb
            .get("worksheets")
            .and_then(Value::as_array)
            .map(|ws| {
                ws.iter()
                    .filter_map(|w| w.get("cells").and_then(Value::as_array))
                    .flatten()
                    .collect()
            })
            .unwrap_or_default(),
    };

    let mut sections: Vec<Section> = Vec::new();
    let mut cur = Section {
        cell: 1,
        ..Section::default()
    };
    // (level, text) of the open headings, outermost first.
    let mut stack: Vec<(usize, String)> = Vec::new();
    // Text in finished sections; `counted` of them are already summed.
    let (mut done_bytes, mut counted) = (0usize, 0usize);
    // The title is the first level-1 heading; a notebook without one takes
    // its first heading of any level (Colab notebooks often open with a
    // `#####` licence banner above their `#` title).
    let mut first_heading: Option<String> = None;
    let mut first_h1: Option<String> = None;

    for (i, cell) in cells.iter().enumerate() {
        for s in &sections[counted..] {
            done_bytes += s.body.len();
        }
        counted = sections.len();
        if done_bytes + cur.body.len() > MAX_BODY_BYTES {
            stats.truncated = true;
            break;
        }
        let n = i + 1;
        if cur.body.is_empty() && cur.heading.is_none() {
            cur.cell = n;
        }
        let kind = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "markdown" => {
                let src = source(cell, "source");
                let mut buf = String::new();
                let mut fence: Option<&str> = None;
                for line in src.lines() {
                    let t = line.trim_start();
                    match fence {
                        Some(f) if t.starts_with(f) => fence = None,
                        Some(_) => {}
                        None if t.starts_with("```") => fence = Some("```"),
                        None if t.starts_with("~~~") => fence = Some("~~~"),
                        None => {
                            if let Some((level, text)) = atx_heading(line) {
                                cur.push_block(&buf);
                                buf.clear();
                                first_heading.get_or_insert_with(|| text.clone());
                                if level == 1 {
                                    first_h1.get_or_insert_with(|| text.clone());
                                }
                                open_heading(&mut sections, &mut cur, &mut stack, level, text, n);
                            }
                        }
                    }
                    buf.push_str(line);
                    buf.push('\n');
                }
                cur.push_block(&buf);
            }
            // nbformat 3 kept headings in cells of their own.
            "heading" => {
                let text = clean_heading(&source(cell, "source"));
                if !text.is_empty() {
                    let level = cell
                        .get("level")
                        .and_then(Value::as_u64)
                        .map_or(1, |l| l.clamp(1, 6) as usize);
                    first_heading.get_or_insert_with(|| text.clone());
                    if level == 1 {
                        first_h1.get_or_insert_with(|| text.clone());
                    }
                    open_heading(&mut sections, &mut cur, &mut stack, level, text.clone(), n);
                    cur.push_block(&format!("{} {text}", "#".repeat(level)));
                }
            }
            "code" => {
                let mut code = source(cell, "source");
                if code.trim().is_empty() {
                    code = source(cell, "input");
                }
                if !code.trim().is_empty() {
                    cur.push_block(&format!("```{fence_lang}\n{}\n```", code.trim_end()));
                }
                for out in cell
                    .get("outputs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = output_text(out) {
                        cur.push_block(&format!("```\n{text}\n```"));
                    }
                }
            }
            // `raw` cells (LaTeX, reST for nbconvert) and anything unknown:
            // their text, as written.
            _ => cur.push_block(&source(cell, "source")),
        }
    }
    if !cur.body.trim().is_empty() {
        sections.push(cur);
    }
    let sections = fold_heading_only(sections);
    if sections.is_empty() {
        stats.junk += 1;
        return;
    }

    let title = meta
        .and_then(|m| m.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or(first_h1)
        .or(first_heading)
        .unwrap_or_else(|| stem.to_string());
    let author = meta
        .and_then(|m| m.get("authors"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.get("name")
                        .and_then(Value::as_str)
                        .or_else(|| x.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|s| !s.is_empty());

    // Locators must be unique within the file (they key the record id): the
    // second and later sections starting in one cell are `c{cell}.{k}`.
    let mut prev_cell = 0usize;
    let mut k = 0usize;
    for sec in sections {
        k = if sec.cell == prev_cell { k + 1 } else { 1 };
        prev_cell = sec.cell;
        let at = match k {
            1 => format!("c{}", sec.cell),
            _ => format!("c{}.{k}", sec.cell),
        };
        for (part, text) in split_sections(&sec.body).into_iter().enumerate() {
            if stats.records as usize >= MAX_RECORDS_PER_FILE {
                stats.truncated = true;
                return;
            }
            let mut fields = Map::new();
            fields.insert("title".into(), Value::String(title.clone()));
            if let Some(a) = &author {
                fields.insert("author".into(), Value::String(a.clone()));
            }
            if let Some(l) = &language {
                fields.insert("language".into(), Value::String(l.clone()));
            }
            if let Some(h) = &sec.heading {
                fields.insert("heading".into(), Value::String(h.clone()));
            }
            if sec.path.len() > 1 {
                fields.insert("heading_path".into(), Value::String(sec.path.join(" > ")));
            }
            fields.insert("cell".into(), Value::Number((sec.cell as u64).into()));
            if part > 0 {
                fields.insert("section".into(), Value::Number((part as u64).into()));
            }
            fields.insert("body".into(), Value::String(text));
            stats.records += 1;
            if !sink(RawRecord {
                fields,
                locator: format!("{at}-s{part}"),
                group: None,
                // This extractor's vocabulary, so every notebook in a folder
                // lands in one dataset; author/language/heading/heading_path/
                // section appear only sometimes.
                origin: FieldOrigin::Extractor,
            }) {
                return;
            }
        }
    }
}

/// Close the current section and open one at `text` (level `level`), which
/// starts in cell `n`.
fn open_heading(
    sections: &mut Vec<Section>,
    cur: &mut Section,
    stack: &mut Vec<(usize, String)>,
    level: usize,
    text: String,
    n: usize,
) {
    let done = std::mem::take(cur);
    if !done.body.trim().is_empty() {
        sections.push(done);
    }
    while stack.last().is_some_and(|(l, _)| *l >= level) {
        stack.pop();
    }
    stack.push((level, text.clone()));
    *cur = Section {
        heading: Some(text),
        path: stack.iter().map(|(_, t)| t.clone()).collect(),
        cell: n,
        body: String::new(),
    };
}

/// A section whose body is nothing but its heading line (`# Title` directly
/// followed by `## Intro`) has no text of its own: its line is carried into
/// the next section, which keeps its own heading and first cell. A trailing
/// one is kept.
fn fold_heading_only(sections: Vec<Section>) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::with_capacity(sections.len());
    let mut carry = String::new();
    let n = sections.len();
    for (i, mut sec) in sections.into_iter().enumerate() {
        if !carry.is_empty() {
            sec.body = format!("{carry}\n\n{}", sec.body);
            carry.clear();
        }
        let only_heading =
            !sec.body.trim().contains('\n') && atx_heading(sec.body.trim()).is_some();
        if only_heading && i + 1 < n {
            carry = sec.body;
        } else {
            out.push(sec);
        }
    }
    out
}

/// The language of a well-known kernel, for notebooks whose metadata names
/// only the kernel (Colab writes `{"name": "python3"}` and nothing else).
fn language_of_kernel(name: &str) -> Option<String> {
    let n = name.to_ascii_lowercase();
    let lang = if n.starts_with("python") {
        "python"
    } else if n == "ir" {
        "R"
    } else if n.starts_with("julia") {
        "julia"
    } else if n == "deno" {
        "typescript"
    } else {
        return None;
    };
    Some(lang.to_string())
}

/// A cell field that is a string or (the usual form) a list of line strings.
fn source(cell: &Value, key: &str) -> String {
    match cell.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// `(level, text)` of an ATX heading line (`## Title ##`), CommonMark-style:
/// at most three spaces of indent, 1-6 `#`, then a space or the line's end.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let t = &line[indent..];
    let level = t.len() - t.trim_start_matches('#').len();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let text = clean_heading(rest.trim().trim_end_matches('#'));
    (!text.is_empty()).then_some((level, text))
}

/// Heading text as a title: `*` emphasis and `` ` `` code markers and simple
/// HTML tags (`<a id=…></a>` anchors are common in notebooks) removed,
/// whitespace collapsed. `_` is kept: in a notebook it is far more often part
/// of an identifier (`create_app`) than emphasis.
fn clean_heading(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '*' | '`' => {}
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The indexable text of one output, capped, or `None`.
fn output_text(out: &Value) -> Option<String> {
    let kind = out.get("output_type").and_then(Value::as_str).unwrap_or("");
    let text = match kind {
        "error" | "pyerr" => {
            let name = out.get("ename").and_then(Value::as_str).unwrap_or("");
            let value = out.get("evalue").and_then(Value::as_str).unwrap_or("");
            let t = format!("{name}: {value}");
            (t.len() > 2).then_some(t)?
        }
        // v4 `stream`; v3 `stream`/`pyout`/`display_data` keep `text` here.
        _ if out.get("text").is_some() && out.get("data").is_none() => {
            collapse_redraws(&strip_ansi(&source(out, "text")))
        }
        _ => {
            let data = out.get("data")?;
            let has_image = data
                .as_object()
                .is_some_and(|d| d.keys().any(|k| k.starts_with("image/")));
            if has_image {
                return None;
            }
            source(data, "text/plain")
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(cap_output(text))
}

fn cap_output(text: &str) -> String {
    let total = text.lines().count();
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i == MAX_OUTPUT_LINES || out.len() + line.len() > MAX_OUTPUT_BYTES {
            out.push_str(&format!("… ({} more lines)", total - i));
            return out;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Remove ANSI escape sequences (CSI `ESC [ … final`, and lone `ESC x`).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if it.peek() == Some(&'[') {
            it.next();
            // Parameters and intermediates, then one final byte in @..~.
            for d in it.by_ref() {
                if ('@'..='~').contains(&d) {
                    break;
                }
            }
        } else {
            it.next();
        }
    }
    out
}

/// A progress bar redraws its line with `\r`: keep what the terminal ended up
/// showing, the text after the last `\r` of each line.
fn collapse_redraws(s: &str) -> String {
    s.split('\n')
        .map(|l| l.trim_end_matches('\r').rsplit('\r').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_json(nb: &Value, file: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file);
        std::fs::write(&path, serde_json::to_vec_pretty(nb).unwrap()).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, &path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    fn s<'a>(r: &'a RawRecord, k: &str) -> Option<&'a str> {
        r.fields.get(k).and_then(Value::as_str)
    }

    fn md(src: &str) -> Value {
        json!({"cell_type": "markdown", "metadata": {}, "source": src.split_inclusive('\n').collect::<Vec<_>>()})
    }

    fn code(src: &str, outputs: Value) -> Value {
        json!({"cell_type": "code", "execution_count": 1, "metadata": {}, "outputs": outputs,
               "source": src.split_inclusive('\n').collect::<Vec<_>>()})
    }

    fn notebook(cells: Vec<Value>, metadata: Value) -> Value {
        json!({"cells": cells, "metadata": metadata, "nbformat": 4, "nbformat_minor": 5})
    }

    fn py() -> Value {
        json!({"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
               "language_info": {"name": "python", "version": "3.11.4"}})
    }

    #[test]
    fn cells_group_into_sections_under_their_markdown_headings() {
        let nb = notebook(
            vec![
                md("# Aggregation and Grouping\nIntro text."),
                code("import pandas as pd", json!([])),
                md("## GroupBy\nSplit, apply, combine.\n### Aggregate\nMid-cell sub-heading."),
                code(
                    "df.groupby('key').sum()",
                    json!([{"output_type": "execute_result", "execution_count": 1, "metadata": {},
                            "data": {"text/plain": ["     data\n", "key      \n", "A       3"],
                                     "text/html": ["<table>…</table>"]}}]),
                ),
                md("## Plots"),
                code(
                    "df.plot(); print('done')",
                    json!([
                        {"output_type": "display_data", "metadata": {},
                         "data": {"image/png": "iVBORw0KGgo=", "text/plain": ["<Figure size 432x288 with 1 Axes>"]}},
                        {"output_type": "stream", "name": "stdout", "text": ["done\n"]},
                        {"output_type": "error", "ename": "KeyError", "evalue": "'missing'",
                         "traceback": ["\u{1b}[0;31mKeyError\u{1b}[0m Traceback ..."]}
                    ]),
                ),
            ],
            py(),
        );
        let (stats, recs) = run_json(&nb, "groupby.ipynb");
        assert_eq!(stats.records, 4);
        let got: Vec<(&str, Option<&str>, Option<&str>)> = recs
            .iter()
            .map(|r| (r.locator.as_str(), s(r, "heading"), s(r, "heading_path")))
            .collect();
        assert_eq!(
            got,
            [
                ("c1-s0", Some("Aggregation and Grouping"), None),
                (
                    "c3-s0",
                    Some("GroupBy"),
                    Some("Aggregation and Grouping > GroupBy")
                ),
                // Second section starting in cell 3: its own locator, or the
                // two would share a record id and one would overwrite the other.
                (
                    "c3.2-s0",
                    Some("Aggregate"),
                    Some("Aggregation and Grouping > GroupBy > Aggregate")
                ),
                (
                    "c5-s0",
                    Some("Plots"),
                    Some("Aggregation and Grouping > Plots")
                ),
            ]
        );
        for r in &recs {
            assert_eq!(s(r, "title"), Some("Aggregation and Grouping"));
            assert_eq!(s(r, "language"), Some("python"));
            assert!(matches!(r.origin, FieldOrigin::Extractor));
        }
        assert_eq!(recs[1].fields["cell"], json!(3));
        assert_eq!(recs[2].fields["cell"], json!(3));
        let mut locs: Vec<&str> = recs.iter().map(|r| r.locator.as_str()).collect();
        locs.sort_unstable();
        locs.dedup();
        assert_eq!(locs.len(), recs.len(), "locators key record ids: unique");

        let b0 = s(&recs[0], "body").unwrap();
        assert!(
            b0.contains("Intro text.") && b0.contains("```python\nimport pandas as pd\n```"),
            "{b0}"
        );
        let b1 = s(&recs[1], "body").unwrap();
        assert!(
            b1.contains("Split, apply, combine.") && !b1.contains("Mid-cell"),
            "{b1}"
        );
        let b2 = s(&recs[2], "body").unwrap();
        assert!(
            b2.starts_with("### Aggregate\nMid-cell sub-heading."),
            "{b2}"
        );
        assert!(
            b2.contains("df.groupby('key').sum()") && b2.contains("key      \nA       3"),
            "{b2}"
        );
        assert!(
            !b2.contains("<table>"),
            "html outputs are not indexed: {b2}"
        );
        let b3 = s(&recs[3], "body").unwrap();
        assert!(
            b3.contains("```\ndone\n```") && b3.contains("KeyError: 'missing'"),
            "{b3}"
        );
        assert!(
            !b3.contains("Figure size") && !b3.contains("iVBOR"),
            "image outputs skipped: {b3}"
        );
        assert!(!b3.contains("Traceback") && !b3.contains('\u{1b}'), "{b3}");
    }

    #[test]
    fn a_hash_in_code_fences_or_without_a_space_is_not_a_heading() {
        let nb = notebook(
            vec![
                md("Some text\n```bash\n# not a heading\n```\n#hashtag\n    # indented code"),
                code("# a python comment\nx = 1", json!([])),
            ],
            py(),
        );
        let (stats, recs) = run_json(&nb, "plain.ipynb");
        assert_eq!(stats.records, 1);
        assert!(recs[0].fields.get("heading").is_none());
        assert_eq!(
            s(&recs[0], "title"),
            Some("plain"),
            "no heading: the file stem"
        );
        assert_eq!(recs[0].locator, "c1-s0");
    }

    #[test]
    fn notebook_metadata_titles_and_kernel_language_names_the_language() {
        let nb = notebook(
            vec![
                md("## Fit\nA linear model."),
                code("lm(y ~ x)", json!([])),
                md("## `fit_model` *notes* <a id=\"n\"></a>\nMore."),
            ],
            json!({"title": "Regression notes", "authors": [{"name": "Ada"}, {"name": "Bo"}],
                   "kernelspec": {"display_name": "R", "language": "R", "name": "ir"}}),
        );
        let (_, recs) = run_json(&nb, "r.ipynb");
        assert_eq!(s(&recs[0], "title"), Some("Regression notes"));
        assert_eq!(s(&recs[0], "author"), Some("Ada; Bo"));
        assert_eq!(s(&recs[0], "language"), Some("R"));
        assert!(s(&recs[0], "body")
            .unwrap()
            .contains("```R\nlm(y ~ x)\n```"));
        assert_eq!(
            s(&recs[1], "heading"),
            Some("fit_model notes"),
            "`_` kept, emphasis/code markers and tags dropped"
        );
    }

    #[test]
    fn outputs_lose_ansi_and_progress_redraws_and_are_capped() {
        let log: String = (0..200)
            .map(|i| format!("epoch {i} loss 0.{i}\n"))
            .collect();
        let nb = notebook(
            vec![code(
                "train()",
                json!([
                    {"output_type": "stream", "name": "stderr",
                     "text": ["\u{1b}[32mok\u{1b}[0m\n", " 10%|#   |\r 50%|#####|\r100%|##########|\n"]},
                    {"output_type": "stream", "name": "stdout", "text": log}
                ]),
            )],
            py(),
        );
        let (_, recs) = run_json(&nb, "train.ipynb");
        let body = s(&recs[0], "body").unwrap();
        assert!(body.contains("```\nok\n100%|##########|\n```"), "{body}");
        assert!(!body.contains("10%") && !body.contains('\u{1b}'), "{body}");
        assert!(
            body.contains("epoch 39 loss") && !body.contains("epoch 40 loss"),
            "40 lines kept"
        );
        assert!(body.contains("… (160 more lines)"), "{body}");
    }

    #[test]
    fn nbformat_3_notebooks_are_read_too() {
        let nb = json!({
            "metadata": {"name": "old"},
            "nbformat": 3, "nbformat_minor": 0,
            "worksheets": [{"cells": [
                {"cell_type": "heading", "level": 1, "metadata": {}, "source": ["Old Analysis"]},
                {"cell_type": "markdown", "metadata": {}, "source": ["Legacy notebook."]},
                {"cell_type": "code", "language": "python", "input": ["print(1)"], "metadata": {},
                 "outputs": [{"output_type": "stream", "stream": "stdout", "text": ["1\n"]},
                             {"output_type": "pyout", "prompt_number": 1, "metadata": {}, "text": ["2"]},
                             {"output_type": "pyerr", "ename": "ValueError", "evalue": "bad", "traceback": []}]}
            ]}]
        });
        let (stats, recs) = run_json(&nb, "old.ipynb");
        assert_eq!(stats.records, 1);
        assert_eq!(s(&recs[0], "title"), Some("Old Analysis"));
        assert_eq!(s(&recs[0], "heading"), Some("Old Analysis"));
        let body = s(&recs[0], "body").unwrap();
        for want in [
            "# Old Analysis",
            "Legacy notebook.",
            "print(1)",
            "```\n1\n```",
            "```\n2\n```",
            "ValueError: bad",
        ] {
            assert!(body.contains(want), "{want:?} in {body}");
        }
    }

    /// Shapes from real notebooks: Colab's TensorFlow tutorials open with a
    /// `#####` licence banner above the `#` title and name only the kernel;
    /// a `# Title` cell directly followed by `## Intro` has no text of its
    /// own and folds into the next section.
    #[test]
    fn colab_shapes_title_language_and_heading_only_sections() {
        let nb = notebook(
            vec![
                md("##### Copyright 2018 The TensorFlow Authors."),
                code("#@title Licensed under the Apache License", json!([])),
                md("# Basic classification"),
                md("## Import the dataset\nFashion MNIST."),
                code("import tensorflow as tf", json!([])),
            ],
            json!({"colab": {"name": "classification.ipynb"},
                   "kernelspec": {"display_name": "Python 3", "name": "python3"}}),
        );
        let (_, recs) = run_json(&nb, "classification.ipynb");
        assert_eq!(s(&recs[0], "title"), Some("Basic classification"));
        assert_eq!(s(&recs[0], "language"), Some("python"));
        let got: Vec<(&str, Option<&str>)> = recs
            .iter()
            .map(|r| (r.locator.as_str(), s(r, "heading")))
            .collect();
        assert_eq!(
            got,
            [
                ("c1-s0", Some("Copyright 2018 The TensorFlow Authors.")),
                ("c4-s0", Some("Import the dataset")),
            ],
            "the heading-only `# Basic classification` folded forward"
        );
        let b = s(&recs[1], "body").unwrap();
        assert!(
            b.starts_with("# Basic classification\n\n## Import the dataset"),
            "{b}"
        );
        assert!(b.contains("```python\nimport tensorflow as tf\n```"), "{b}");
    }

    #[test]
    fn broken_or_empty_notebooks_are_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ipynb");
        std::fs::write(&path, b"{\"cells\": [{\"cell_type\": ").unwrap();
        let stats = extract(&path, &path, false, &mut |_| true).unwrap();
        assert_eq!((stats.records, stats.junk), (0, 1));

        let nb = notebook(vec![code("", json!([])), md("   ")], py());
        let (stats, recs) = run_json(&nb, "empty.ipynb");
        assert_eq!((stats.records, stats.junk, recs.len()), (0, 1, 0));
    }

    #[test]
    fn notebooks_are_recognized_by_content_and_plain_json_is_not() {
        let jupyter = serde_json::to_string_pretty(&notebook(vec![md("# T")], py())).unwrap();
        assert!(looks_like_notebook(&jupyter));
        let colab = r#"{"nbformat":4,"nbformat_minor":0,"metadata":{"colab":{"provenance":[]}},"cells":[{"cell_type":"code"}]}"#;
        assert!(looks_like_notebook(colab));
        let v3 =
            r#"{"metadata": {"name": ""}, "nbformat": 3, "nbformat_minor": 0, "worksheets": []}"#;
        assert!(looks_like_notebook(v3));
        for not in [
            r#"{"cells": [[1, 2], [3, 4]], "rows": 2}"#,
            r#"{"metadata": {"nbformat": "x"}, "cells": []}"#,
            r#"[{"cell_type": "code"}]"#,
            "cells: [1, 2]\nnbformat: 4\n",
        ] {
            assert!(!looks_like_notebook(not), "{not}");
        }

        // End to end through sniff: a notebook is Ipynb whatever its name;
        // a JSON file with a `cells` key stays Json.
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("analysis.json");
        std::fs::write(&nb_path, &jupyter).unwrap();
        assert_eq!(
            crate::sniff::sniff(&nb_path).unwrap().family,
            crate::sniff::Family::Ipynb
        );
        let plain = dir.path().join("grid.json");
        std::fs::write(
            &plain,
            "{\n  \"cells\": [[1, 2], [3, 4]],\n  \"rows\": 2\n}\n",
        )
        .unwrap();
        assert_eq!(
            crate::sniff::sniff(&plain).unwrap().family,
            crate::sniff::Family::Json
        );
    }
}
