//! HTML — a tolerant hand-rolled tokenizer (no DOM dependency; bounded by a
//! 64MB whole-file cap). Generic extraction rule, no hardcoded names:
//! a dominant `<table>` (≥5 rows, consistent column count, header row) →
//! one record per row with header-derived field names; otherwise one
//! document record {title, headings, body}.

use super::{
    emit_document, sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink,
    MAX_RECORDS_PER_FILE, MAX_WHOLE_FILE,
};
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
                origin: FieldOrigin::Data,
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
    emit_document(
        &title,
        &doc.headings,
        doc.body.trim(),
        MAX_RECORDS_PER_FILE,
        sink,
        &mut stats,
    );
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
            let (tag_end, self_closing) = scan_tag(bytes, j);

            flush_text(&mut cur_text, &text_sink, &mut doc, &mut cur_cell);

            // Raw-text elements. Their contents are code, so jump the cursor
            // straight to the literal close tag rather than tokenizing them:
            // never buffered, never flushed, and a `<` in JS cannot be mistaken
            // for a tag and eat the rest of the page. XHTML-style `<script/>`
            // has no contents to skip.
            // The name scan above stops at any byte outside [A-Za-z0-9!], so
            // `<script-loader>` yields the name `script`. Entering raw-text
            // mode on that would skip to a `</script>` the document never
            // contains — `raw_text_end` rightly refuses `</script-loader>` —
            // and swallow everything after it. Custom element names are
            // REQUIRED to contain a hyphen, so this is a live shape. Only
            // treat the name as the raw-text element when it actually ended
            // there: whitespace, `/`, or `>`.
            let name_ended = bytes
                .get(j)
                .is_some_and(|&c| c.is_ascii_whitespace() || c == b'/' || c == b'>');
            if !close && name_ended && matches!(name.as_str(), "script" | "style") && !self_closing
            {
                i = raw_text_end(bytes, tag_end + 1, name.as_bytes());
                continue;
            }

            match (close, name.as_str()) {
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

/// Where the attribute scanner stands, which is what decides whether a `/`
/// closes the tag or is just a byte of an attribute value.
#[derive(PartialEq)]
enum Attr {
    /// Between attributes, or inside an attribute name.
    Bare,
    /// Just past `=`, with the value not yet started.
    BeforeValue,
    /// Inside an unquoted value, where every byte up to the next whitespace
    /// or `>` belongs to the value.
    Unquoted,
}

/// Offset of the tag's `>` (or EOF) given `from` at the end of its name, plus
/// whether the tag self-closes.
///
/// The byte before `>` cannot answer the second question on its own: a slash
/// is also legal inside an unquoted attribute value, as in
/// `<script src=/cdn/lib/>`. Reading that as `/>` would skip raw-text mode for
/// an element that is never void and does have contents to skip. Only a slash
/// outside quotes and outside an unquoted value closes the tag.
fn scan_tag(bytes: &[u8], from: usize) -> (usize, bool) {
    let mut k = from;
    let mut quote: Option<u8> = None;
    let mut attr = Attr::Bare;
    let mut self_closing = false;
    while k < bytes.len() {
        let c = bytes[k];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                    attr = Attr::Bare;
                }
            }
            None => {
                if c == b'>' {
                    break;
                }
                // Only a slash immediately before `>` closes the tag; anything
                // following one reopens attribute scanning.
                self_closing = false;
                match c {
                    b'"' | b'\'' => quote = Some(c),
                    b'=' if attr != Attr::Unquoted => attr = Attr::BeforeValue,
                    // `<p a= />` makes the slash itself the value.
                    b'/' if attr == Attr::BeforeValue => attr = Attr::Unquoted,
                    b'/' if attr == Attr::Bare => self_closing = true,
                    _ if c.is_ascii_whitespace() => {
                        if attr == Attr::Unquoted {
                            attr = Attr::Bare;
                        }
                    }
                    _ => {
                        if attr == Attr::BeforeValue {
                            attr = Attr::Unquoted;
                        }
                    }
                }
            }
        }
        k += 1;
    }
    (k, self_closing)
}

/// Offset of the `<` opening `</name...>` at or after `from`, or EOF if there
/// is none — an unterminated raw-text element runs to the end of the file, as
/// it does in a browser.
///
/// Only an *appropriate* end tag terminates raw text: HTML5 lets the name be
/// followed by whitespace, `/` or `>` and nothing else, so `</script-foo>` is
/// script text that the surrounding code goes on hiding. Accepting any
/// non-alphanumeric follower would end the skip at `-`, `_` or `.` and index
/// the code after it.
fn raw_text_end(bytes: &[u8], from: usize, name: &[u8]) -> usize {
    let mut p = from.min(bytes.len());
    while let Some(off) = memchr::memchr(b'<', &bytes[p..]) {
        let at = p + off;
        let after = at + 2 + name.len();
        if bytes.get(at + 1) == Some(&b'/')
            && after <= bytes.len()
            && bytes[at + 2..after].eq_ignore_ascii_case(name)
            && bytes
                .get(after)
                .is_some_and(|&c| c.is_ascii_whitespace() || c == b'/' || c == b'>')
        {
            return at;
        }
        p = at + 1;
    }
    bytes.len()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, html: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, html).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    fn body_of(rec: &RawRecord) -> &str {
        rec.fields["body"].as_str().unwrap()
    }

    fn table_html(header: Option<[&str; 2]>, data_rows: usize) -> String {
        let mut h = String::from("<html><body><table>");
        if let Some([a, b]) = header {
            h.push_str(&format!("<tr><th>{a}</th><th>{b}</th></tr>"));
        }
        for i in 1..=data_rows {
            h.push_str(&format!("<tr><td>item{i}</td><td>{i}</td></tr>"));
        }
        h.push_str("</table></body></html>");
        h
    }

    #[test]
    fn a_prose_page_becomes_one_document_of_title_headings_and_tag_free_body() {
        let (stats, recs) = run(
            "page.html",
            "<html><head><title>Quarterly Report</title></head><body>\
             <h1>Revenue</h1><p>Growth was steady.</p>\
             <h2>Costs</h2><div>Cloud costs declined.</div></body></html>",
        );
        assert_eq!(stats.records, 1);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].locator, "s0");
        assert_eq!(
            recs[0].fields["title"],
            serde_json::json!("Quarterly Report")
        );
        assert_eq!(
            recs[0].fields["headings"],
            serde_json::json!(["Revenue", "Costs"])
        );
        let body = body_of(&recs[0]);
        assert!(body.contains("Growth was steady."));
        assert!(body.contains("Cloud costs declined."));
        assert!(
            !body.contains('<') && !body.contains("div"),
            "markup leaked into the body: {body:?}"
        );
    }

    #[test]
    fn attributes_and_comments_never_reach_the_body() {
        let (_, recs) = run(
            "attrs.html",
            "<html><body><p class=\"lede\">visible</p><!-- hidden note -->\
             <a href=\"https://example.com/hidden-path\" title=\"hidden title\">link text</a>\
             </body></html>",
        );
        let body = body_of(&recs[0]);
        assert!(body.contains("visible") && body.contains("link text"));
        assert!(
            !body.contains("hidden") && !body.contains("class") && !body.contains("href"),
            "attribute or comment text was indexed: {body:?}"
        );
    }

    #[test]
    fn character_entities_are_unescaped_in_the_title_and_the_body() {
        let (_, recs) = run(
            "ents.html",
            "<html><head><title>Q4 &amp; FY</title></head><body>\
             <p>5 &lt; 10 &gt; 2, &quot;quoted&quot; &amp; &#39;single&#39;&nbsp;spaced</p>\
             </body></html>",
        );
        assert_eq!(recs[0].fields["title"], serde_json::json!("Q4 & FY"));
        let body = body_of(&recs[0]);
        assert!(body.contains("5 < 10 > 2"), "{body:?}");
        assert!(body.contains("\"quoted\" & 'single' spaced"), "{body:?}");
        assert!(!body.contains("&amp;") && !body.contains("&lt;"));
    }

    /// Inline scripts routinely carry API keys and endpoints; indexing them
    /// would put those in `_source`. Prose on both sides of a skipped element
    /// still has to survive.
    #[test]
    fn script_and_style_text_never_reaches_the_body() {
        let (_, recs) = run(
            "skip.html",
            "<html><head><title>Doc</title><style>.lede{color:#fff}</style>\
             <script>var token=1;</script></head><body><p>before</p>\
             <script>cfg={api:'https://secret.example/v1',key:'abc123'}</script>\
             <p>after</p></body></html>",
        );
        assert_eq!(recs[0].fields["title"], serde_json::json!("Doc"));
        let body = body_of(&recs[0]);
        assert!(
            body.contains("before") && body.contains("after"),
            "{body:?}"
        );
        for leak in ["lede", "color", "token", "secret.example", "abc123", "cfg"] {
            assert!(
                !body.contains(leak),
                "{leak:?} was indexed as prose: {body:?}"
            );
        }
    }

    /// `<` inside JS/CSS is an operator, not a tag. Tokenizing it would run the
    /// tag scan past `</script>` and drop the remainder of the page.
    #[test]
    fn a_less_than_inside_a_script_does_not_swallow_the_rest_of_the_page() {
        let (_, recs) = run(
            "lt.html",
            "<html><body><p>alpha</p><script>if(a<b){s=\"x>y\";}</script>\
             <style>@media (max-width:40em){.a{top:0}}</style>\
             <p>omega</p></body></html>",
        );
        let body = body_of(&recs[0]);
        assert!(body.contains("alpha") && body.contains("omega"), "{body:?}");
        assert!(
            !body.contains("max-width") && !body.contains('{'),
            "{body:?}"
        );
    }

    /// `</SCRIPT >` is the same terminator; an XHTML-style `<script src=… />`
    /// has no contents, so it must not put the tokenizer into raw-text mode.
    #[test]
    fn raw_text_ends_at_a_case_insensitive_or_spaced_close_and_self_closing_skips_nothing() {
        let (_, recs) = run(
            "case.html",
            "<html><body><SCRIPT>hidden=1;</SCRIPT >kept one\
             <script src=\"app.js\"/><p>kept two</p></body></html>",
        );
        let body = body_of(&recs[0]);
        assert!(
            body.contains("kept one") && body.contains("kept two"),
            "{body:?}"
        );
        assert!(!body.contains("hidden"), "{body:?}");
    }

    /// A slash also ends an unquoted attribute value, and `script` is never a
    /// void element — so the byte before `>` cannot decide self-closing on its
    /// own, and `<script src=/cdn/lib/>` still has contents to skip.
    #[test]
    fn a_slash_ending_an_unquoted_attribute_value_does_not_read_as_self_closing() {
        let (_, recs) = run(
            "unquoted-slash.html",
            "<html><body><p>alpha</p><script src=/cdn/lib/>var leak=\"SEKRIT\";</script>\
             <p>omega</p></body></html>",
        );
        let body = body_of(&recs[0]);
        assert!(body.contains("alpha") && body.contains("omega"), "{body:?}");
        for leak in ["SEKRIT", "leak", "cdn"] {
            assert!(
                !body.contains(leak),
                "{leak:?} was indexed as prose: {body:?}"
            );
        }
    }

    /// Raw text ends only at an *appropriate* end tag: HTML5 requires
    /// whitespace, `/` or `>` after the name, so `</script-foo>` is script
    /// text. Any other follower — `_` and `.` alike — is the same case.
    #[test]
    fn a_run_on_end_tag_name_does_not_end_raw_text_early() {
        let (_, recs) = run(
            "runon.html",
            "<p>alpha</p><script>a=1;</script-foo>b=\"LEAK2\";</script><p>omega</p>",
        );
        let body = body_of(&recs[0]);
        assert!(body.contains("alpha") && body.contains("omega"), "{body:?}");
        for leak in ["LEAK2", "a=1", "script"] {
            assert!(
                !body.contains(leak),
                "{leak:?} was indexed as prose: {body:?}"
            );
        }
    }

    /// A custom element whose name merely STARTS with a raw-text element's
    /// name is an ordinary element. Custom element names are required to
    /// contain a hyphen, so `<script-loader>` is a real shape — and reading
    /// it as `script` would enter raw-text mode looking for a `</script>`
    /// the document does not contain, discarding everything after it.
    #[test]
    fn a_custom_element_named_after_a_raw_text_element_does_not_swallow_the_page() {
        for (tag, marker) in [("script-loader", "SLOAD"), ("style-sheet", "SSHEET")] {
            let (_, recs) = run(
                "custom.html",
                &format!("<p>alpha</p><{tag} data-k=\"{marker}\">middle</{tag}><p>omega</p>"),
            );
            let body = body_of(&recs[0]);
            assert!(
                body.contains("alpha") && body.contains("omega"),
                "<{tag}> swallowed the page: {body:?}"
            );
            assert!(
                body.contains("middle"),
                "<{tag}> content was dropped: {body:?}"
            );
            assert!(
                !body.contains(marker),
                "attribute leaked into body: {body:?}"
            );
        }
    }

    /// An unclosed `<script>` swallows the rest of the file, as a real HTML
    /// parser does — there is no terminator, so the tail is code. It is
    /// bounded to the tail: text before the tag was already flushed.
    #[test]
    fn an_unclosed_script_drops_the_tail_but_keeps_the_text_before_it() {
        let (stats, recs) = run(
            "trunc.html",
            "<html><body><h1>Head</h1><p>kept</p><script>var token=1;<p>lost</p>",
        );
        assert_eq!(stats.records, 1);
        let body = body_of(&recs[0]);
        assert!(body.contains("kept"), "{body:?}");
        assert!(
            !body.contains("token") && !body.contains("lost"),
            "{body:?}"
        );
    }

    #[test]
    fn a_dominant_table_becomes_one_record_per_row_named_from_its_header_cells() {
        let (stats, recs) = run("t.html", &table_html(Some(["Product Name", "Qty"]), 5));
        assert_eq!(stats.records, 5);
        assert_eq!(recs.len(), 5);
        assert_eq!(recs[0].fields["Product_Name"], serde_json::json!("item1"));
        assert_eq!(recs[0].fields["Qty"], serde_json::json!("1"));
        assert_eq!(recs[4].fields["Product_Name"], serde_json::json!("item5"));
        assert!(
            recs.iter().all(|r| r.fields.len() == 2),
            "the header row must not become a record"
        );
        let locs: Vec<&str> = recs.iter().map(|r| r.locator.as_str()).collect();
        assert_eq!(locs, ["row1", "row2", "row3", "row4", "row5"]);
        assert!(recs.iter().all(|r| r.group.is_none()));
    }

    #[test]
    fn a_headerless_table_gets_positional_column_names_and_keeps_its_first_row() {
        let (stats, recs) = run("t.html", &table_html(None, 6));
        assert_eq!(stats.records, 6, "no header row means no row is consumed");
        assert_eq!(recs[0].locator, "row0");
        assert_eq!(recs[0].fields["col_1"], serde_json::json!("item1"));
        assert_eq!(recs[0].fields["col_2"], serde_json::json!("1"));
    }

    #[test]
    fn empty_cells_are_omitted_rather_than_indexed_as_blank_values() {
        let mut h = String::from("<html><body><table><tr><th>A &amp; B</th><th>Note</th></tr>");
        for i in 1..=5 {
            h.push_str(&format!("<tr><td>x&lt;{i}</td><td>  </td></tr>"));
        }
        h.push_str("</table></body></html>");
        let (stats, recs) = run("cells.html", &h);
        assert_eq!(stats.records, 5);
        assert_eq!(recs[0].fields["A_B"], serde_json::json!("x<1"));
        assert_eq!(
            recs[0].fields.len(),
            1,
            "the blank Note cell must not be stored"
        );
    }

    /// DEFECT, pinned as CURRENT behaviour.
    ///
    /// A table under the 5-row dominance threshold loses its content twice
    /// over: it is not emitted as rows, and `flush_text` has already diverted
    /// every cell into `cur_cell` instead of `doc.body`, so the cells are not
    /// in the document record either. Small tables — the common case on a
    /// documentation page — are silently unindexed.
    #[test]
    fn a_table_below_the_dominance_threshold_leaves_its_cells_unindexed() {
        let (stats, recs) = run(
            "small.html",
            "<html><body><h1>Head</h1><p>prose</p>\
             <table><tr><td>cellA</td><td>cellB</td></tr></table>\
             <p>after</p></body></html>",
        );
        assert_eq!(stats.records, 1, "falls back to the document record");
        let body = body_of(&recs[0]);
        assert!(body.contains("prose") && body.contains("after"));
        assert!(
            !body.contains("cellA") && !body.contains("cellB"),
            "small-table cells are now indexed — flip this assertion: {body:?}"
        );
    }

    #[test]
    fn a_page_without_a_title_falls_back_to_a_heading_then_to_the_file_stem() {
        let (_, recs) = run(
            "fallback.html",
            "<html><body><h2>Only Heading</h2><p>x</p></body></html>",
        );
        assert_eq!(recs[0].fields["title"], serde_json::json!("Only Heading"));

        let (_, recs) = run("stem-name.html", "<html><body><p>x</p></body></html>");
        assert_eq!(recs[0].fields["title"], serde_json::json!("stem-name"));
        assert!(recs[0].fields.get("headings").is_none());
    }

    #[test]
    fn unclosed_and_nonsense_markup_is_never_fatal() {
        let (stats, recs) = run(
            "junk.html",
            "<<<>>> <div class=\"unclosed <p>dangling &amp; text <script>x=1;",
        );
        assert_eq!(stats.junk, 0);
        assert_eq!(stats.records, 1, "a broken page still yields a document");
        assert_eq!(recs[0].fields["title"], serde_json::json!("junk"));

        let (stats, recs) = run("empty.html", "");
        assert_eq!(stats.records, 1);
        assert_eq!(body_of(&recs[0]), "");
    }
}
