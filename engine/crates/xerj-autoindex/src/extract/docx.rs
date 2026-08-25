//! DOCX — zip container, `word/document.xml` streamed through quick-xml.
//! Collects w:t runs, breaks paragraphs on w:p, records Heading-styled
//! paragraphs as headings. One document record (sectioned at 32KB).

use super::{emit_document, ExtractStats, Sink, MAX_RECORDS_PER_FILE};
use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufReader;
use std::path::Path;

// SECURITY: bound the DECOMPRESSED read. `word/document.xml` inflates from
// the zip at whatever ratio the author chose — a ~400 KB docx can expand to
// hundreds of MB, and a single 400 MB `<w:t>` text run makes quick-xml
// allocate that whole run for one event (measured 1.68 GB RSS from an 815 KB
// file). Reading through a `Take` caps peak memory no matter the ratio; a
// real document's document.xml is far below this. Past the cap the reader
// hits EOF and the loop ends; quick-xml reports that as `Eof`, not an error,
// so the paragraphs read so far are emitted and the remainder is dropped
// silently — truncation is not counted as junk.
const MAX_DECOMPRESSED_BYTES: u64 = 72 << 20;

/// Extraction body cap (also bounds any single paragraph — see the Text arm).
const MAX_BODY_BYTES: usize = 64 << 20;

pub fn extract(path: &Path, sink: Sink) -> Result<ExtractStats> {
    extract_bounded(path, sink, MAX_DECOMPRESSED_BYTES, MAX_BODY_BYTES)
}

/// Both caps are parameters so the truncation boundaries can be exercised with
/// kilobyte fixtures; production always passes the two constants above.
fn extract_bounded(
    path: &Path,
    sink: Sink,
    max_decompressed_bytes: u64,
    max_body_bytes: usize,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let f = std::fs::File::open(path)?;
    let mut z = zip::ZipArchive::new(f).context("open docx container")?;
    let entry = match z.by_name("word/document.xml") {
        Ok(e) => e,
        Err(_) => {
            stats.junk += 1;
            return Ok(stats);
        }
    };
    let capped = std::io::Read::take(entry, max_decompressed_bytes);
    let mut reader = Reader::from_reader(BufReader::new(capped));
    reader.config_mut().trim_text(false);

    let mut body = String::new();
    let mut headings: Vec<String> = Vec::new();
    let mut para = String::new();
    let mut para_is_heading = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let local = name.as_ref();
                if local == b"w:pStyle" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == b"w:val" {
                            let v = String::from_utf8_lossy(&a.value).to_string();
                            if v.to_lowercase().contains("heading")
                                || v.to_lowercase().contains("title")
                            {
                                para_is_heading = true;
                            }
                        }
                    }
                } else if local == b"w:br" || local == b"w:tab" {
                    para.push(' ');
                }
            }
            Ok(Event::Text(t)) => {
                // SECURITY: `para` only flushes into `body` (and gets length-
                // checked) at `</w:p>`. A crafted docx with a single never-closed
                // `<w:p>` — an 815 KB file whose document.xml inflates to
                // hundreds of MB inside one paragraph — otherwise grows `para`
                // without bound: measured 1.68 GB RSS from that 815 KB input.
                // Stop appending once one paragraph reaches the body cap; the
                // per-paragraph text a real document holds is far below it.
                if para.len() < max_body_bytes {
                    para.push_str(&t.xml10_content().unwrap_or_default());
                }
            }
            // quick-xml >= 0.38 emits `&amp;` and friends as their own event
            // instead of folding them into the neighbouring text, so a
            // paragraph containing an entity loses its `&`/`<`/`>` unless this
            // arm puts it back.
            Ok(Event::GeneralRef(r)) => {
                if para.len() < max_body_bytes {
                    if let Some(resolved) = super::xml_x::resolve_general_ref(&r) {
                        para.push_str(&resolved);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"w:p" {
                    let text = para.trim().to_string();
                    if !text.is_empty() {
                        if para_is_heading {
                            headings.push(text.clone());
                        }
                        body.push_str(&text);
                        body.push_str("\n\n");
                    }
                    para.clear();
                    para_is_heading = false;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => {
                stats.junk += 1;
                break;
            }
        }
        buf.clear();
        if body.len() > max_body_bytes {
            break;
        }
    }
    let body = body.trim();
    if body.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }
    let title = headings.first().cloned().unwrap_or_else(|| {
        body.lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().chars().take(200).collect())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled".into())
            })
    });
    emit_document(
        &title,
        &headings,
        body,
        MAX_RECORDS_PER_FILE,
        sink,
        &mut stats,
    );
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::{extract_bounded, ExtractStats};
    use std::io::Write;
    use std::path::Path;

    const XML_HEAD: &str = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#
    );
    const XML_TAIL: &str = "</w:body></w:document>";

    /// A docx is a zip; the extractor only ever looks at `word/document.xml`,
    /// so fixtures name their members explicitly.
    fn write_container(path: &Path, members: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in members {
            z.start_file(*name, opts).unwrap();
            z.write_all(content.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    fn write_docx(path: &Path, document_xml: &str) {
        write_container(path, &[("word/document.xml", document_xml)]);
    }

    /// Section bodies in emission order, plus the stats the run reported.
    fn extract_sections(
        path: &Path,
        max_decompressed_bytes: u64,
        max_body_bytes: usize,
    ) -> (Vec<String>, ExtractStats) {
        let mut sections = Vec::new();
        let stats = extract_bounded(
            path,
            &mut |r| {
                sections.push(r.fields["body"].as_str().unwrap().to_string());
                true
            },
            max_decompressed_bytes,
            max_body_bytes,
        )
        .unwrap();
        (sections, stats)
    }

    /// Every paragraph carries a distinct fixed-width marker, so "was this byte
    /// range read?" is answerable exactly rather than by size heuristics.
    fn marker(i: usize) -> String {
        format!("m{i:06}")
    }

    /// quick-xml >= 0.38 emits `&amp;` as its own `Event::GeneralRef` instead
    /// of folding it into the neighbouring `Event::Text`, so a reader that only
    /// handles `Event::Text` drops every ampersand, angle bracket and numeric
    /// character reference in the document — silently, since the surrounding
    /// words still arrive. This pins that they come back.
    #[test]
    fn entity_and_character_references_survive_extraction() {
        let xml = format!(
            "{XML_HEAD}{}{XML_TAIL}",
            r#"<w:p><w:r><w:t>Ben &amp; Jerry &lt;tag&gt; caf&#233;</w:t></w:r></w:p>"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entities.docx");
        write_docx(&path, &xml);

        let (sections, stats) = extract_sections(&path, 1 << 20, 1 << 20);
        assert_eq!(stats.junk, 0);
        assert_eq!(
            sections.concat().trim(),
            "Ben & Jerry <tag> café",
            "entity and numeric character references must resolve, and the text \
             around them must not be re-split or lose its spaces"
        );
    }

    #[test]
    fn a_document_inflating_past_the_decompression_cap_is_truncated_at_the_byte_boundary() {
        const PARAS: usize = 20_000;
        // Repetitive filler is what gives a docx its inflation ratio; the
        // per-paragraph marker keeps every paragraph the same width.
        let para = |i: usize| {
            format!(
                "<w:p><w:r><w:t>{} {}</w:t></w:r></w:p>",
                marker(i),
                "filler ".repeat(24)
            )
        };
        let xml = format!(
            "{XML_HEAD}{}{XML_TAIL}",
            (0..PARAS).map(para).collect::<String>()
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.docx");
        write_docx(&path, &xml);

        // The bomb shape: a small file on disk that inflates by orders of
        // magnitude. Without the cap the whole inflated stream is resident.
        let on_disk = std::fs::metadata(&path).unwrap().len();
        assert!(
            xml.len() as u64 > on_disk * 20,
            "fixture is not a compression bomb: {} inflated from {on_disk} on disk",
            xml.len()
        );

        const CAP: u64 = 16 << 10;
        let (sections, stats) = extract_sections(&path, CAP, 64 << 20);

        // Paragraphs are fixed width, so the last one wholly inside the cap is
        // arithmetic: the next one is at best half-read, never closes, and so
        // can never reach `body`.
        let last_whole = (CAP as usize - XML_HEAD.len()) / para(0).len();
        assert!(last_whole > 0 && last_whole < PARAS);
        assert!(stats.records > 0, "nothing was extracted before the cap");
        // Cutting the stream mid-tag reads as EOF to quick-xml, so truncation
        // is silent: the caller sees a clean document, just a shorter one.
        assert_eq!(stats.junk, 0);
        for present in [marker(0), marker(last_whole - 1)] {
            assert!(
                sections.iter().any(|s| s.contains(&present)),
                "{present} sits inside the cap but was not extracted"
            );
        }
        for absent in [marker(last_whole), marker(PARAS - 1)] {
            assert!(
                sections.iter().all(|s| !s.contains(&absent)),
                "{absent} sits past the {CAP}-byte cap but was still read"
            );
        }
        let extracted: usize = sections.iter().map(|s| s.len()).sum();
        assert!(
            extracted < xml.len() / 100,
            "extracted {extracted} bytes of a {}-byte stream: it was read whole",
            xml.len()
        );
    }

    #[test]
    fn a_single_paragraph_stops_absorbing_text_at_the_body_cap() {
        const CAP: usize = 8 << 10;
        const RUNS: usize = 4_000;
        // Fixed-width runs with no whitespace between them: the paragraph
        // buffer holds exactly `i * run_bytes` before run `i` is considered.
        let run = |i: usize| format!("<w:r><w:t>{}{}</w:t></w:r>", marker(i), "x".repeat(57));
        let run_bytes = marker(0).len() + 57;
        let xml = format!(
            "{XML_HEAD}<w:p>{}</w:p>{XML_TAIL}",
            (0..RUNS).map(run).collect::<String>()
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one-huge-paragraph.docx");
        write_docx(&path, &xml);

        let (sections, stats) = extract_sections(&path, 64 << 20, CAP);
        assert!(stats.records > 0);

        // A lone paragraph is only ever cut mid-paragraph, never overlapped, so
        // the sections concatenate back into exactly what the buffer held.
        let buffered = sections.concat();
        assert!(
            buffered.len() <= CAP + run_bytes,
            "paragraph buffer reached {} bytes against a {CAP}-byte cap",
            buffered.len()
        );
        let last_accepted = CAP / run_bytes - 1;
        assert!(buffered.contains(&marker(last_accepted)));
        assert!(!buffered.contains(&marker(last_accepted + 1)));
        assert!(!buffered.contains(&marker(RUNS - 1)));
    }

    #[test]
    fn a_never_closed_paragraph_ends_extraction_instead_of_growing_without_bound() {
        // `para` only drains at `</w:p>`, so a paragraph that never closes is
        // the shape that drove RSS to 1.68 GB. Its buffer is not observable
        // from outside — what is observable is that the run ends, reports the
        // unterminated document as junk, and emits nothing.
        let run = |i: usize| format!("<w:r><w:t>{}{}</w:t></w:r>", marker(i), "x".repeat(57));
        let xml = format!("{XML_HEAD}<w:p>{}", (0..4_000).map(run).collect::<String>());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unterminated.docx");
        write_docx(&path, &xml);

        let (sections, stats) = extract_sections(&path, 64 << 20, 8 << 10);
        assert!(sections.is_empty());
        assert_eq!(stats.records, 0);
        assert_eq!(stats.junk, 1);
    }

    #[test]
    fn a_well_formed_document_keeps_its_paragraphs_and_pstyle_headings() {
        let xml = format!(
            "{XML_HEAD}\
             <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Quarterly Report</w:t></w:r></w:p>\
             <w:p><w:r><w:t>Subscription revenue</w:t></w:r><w:r><w:t> grew again.</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"Heading2\"/></w:pPr><w:r><w:t>Outlook</w:t></w:r></w:p>\
             <w:p><w:r><w:t>Cash flow supports investment.</w:t></w:r></w:p>\
             {XML_TAIL}"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.docx");
        write_docx(&path, &xml);

        let mut records = Vec::new();
        let stats = extract_bounded(
            &path,
            &mut |r| {
                records.push(r);
                true
            },
            super::MAX_DECOMPRESSED_BYTES,
            super::MAX_BODY_BYTES,
        )
        .unwrap();

        assert_eq!(stats.junk, 0);
        assert_eq!(records.len(), 1);
        let fields = &records[0].fields;
        assert_eq!(fields["title"], "Quarterly Report");
        assert_eq!(
            fields["headings"],
            serde_json::json!(["Quarterly Report", "Outlook"])
        );
        let body = fields["body"].as_str().unwrap();
        assert_eq!(
            body,
            "Quarterly Report\n\nSubscription revenue grew again.\n\nOutlook\n\nCash flow supports investment."
        );
    }

    #[test]
    fn a_container_without_word_document_xml_is_counted_as_junk_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-docx.docx");
        write_container(
            &path,
            &[("[Content_Types].xml", "<Types/>"), ("xl/workbook.xml", "")],
        );

        let (sections, stats) = extract_sections(&path, super::MAX_DECOMPRESSED_BYTES, 64 << 20);
        assert!(sections.is_empty());
        assert_eq!(stats.records, 0);
        assert_eq!(stats.junk, 1);
    }
}
