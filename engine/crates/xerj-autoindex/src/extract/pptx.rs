//! PPTX — zip container, one record per slide in presentation order.
//!
//! Slide order comes from `ppt/presentation.xml` (`p:sldIdLst`) resolved
//! through `ppt/_rels/presentation.xml.rels`, so `slide` is the number
//! PowerPoint shows, not the part's file name (parts keep their names when
//! slides are reordered). Each slide's DrawingML text is read shape by shape:
//! `a:p` paragraphs, table rows as ` | `-joined cells so a cell keeps its row,
//! and the title placeholder as `slide_title`. Speaker notes, reached through
//! the slide's `notesSlide` relationship, are appended to the slide's body —
//! they are often where a deck's actual sentences are. Date, footer, header
//! and slide-number placeholders and `a:fld` field text are skipped: they
//! repeat on every slide and carry nothing to retrieve.

use super::opc::{attr, parse_rels, resolve_target, Budget};
use super::{split_sections, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_RECORDS_PER_FILE};
use anyhow::{Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::{Map, Value};
use std::io::{Read, Seek};
use std::path::Path;

// SECURITY: bound the DECOMPRESSED reads, as `docx.rs` does for its one part.
// A deck is many parts, so there are two caps: one per part (a single slide's
// XML is kilobytes; this only stops a crafted part inflating without limit)
// and one across the whole container (a thousand capped parts is still a zip
// bomb). Past either, the remaining slides are dropped and the file is
// reported `truncated` — never silently cut.
const MAX_PART_BYTES: u64 = 32 << 20;
const MAX_TOTAL_DECOMPRESSED_BYTES: u64 = 256 << 20;

/// Extracted-text cap across the deck, matching `docx.rs`.
const MAX_BODY_BYTES: usize = 64 << 20;

/// Placeholder types whose text is boilerplate repeated on every slide (or, on
/// a notes page, the slide thumbnail).
const SKIPPED_PLACEHOLDERS: &[&[u8]] = &[b"dt", b"ftr", b"hdr", b"sldNum", b"sldImg"];

const REL_SLIDE: &str = "/relationships/slide";
const REL_NOTES: &str = "/relationships/notesSlide";

#[derive(Clone, Copy)]
struct Limits {
    part: u64,
    total: u64,
    body: usize,
}

const LIMITS: Limits = Limits {
    part: MAX_PART_BYTES,
    total: MAX_TOTAL_DECOMPRESSED_BYTES,
    body: MAX_BODY_BYTES,
};

/// `name` is the file as the corpus names it, which under durable preparation
/// is not `path` (a sealed snapshot blob); a deck with no slide title is
/// titled from it.
pub fn extract(path: &Path, name: &Path, sink: Sink) -> Result<ExtractStats> {
    extract_bounded(path, name, sink, LIMITS)
}

/// Text of one slide (or notes page).
#[derive(Debug, Default)]
struct SlideText {
    title: Option<String>,
    body: String,
}

/// A slide ready to emit: its 1-based position and its text.
struct Slide {
    number: usize,
    title: Option<String>,
    body: String,
}

fn extract_bounded(path: &Path, name: &Path, sink: Sink, limits: Limits) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let f = std::fs::File::open(path)?;
    let mut z = zip::ZipArchive::new(f).context("open pptx container")?;
    let mut budget = Budget::new(limits.part, limits.total);

    let order = slide_order(&mut z, &mut budget);
    let mut slides: Vec<Slide> = Vec::new();
    let mut body_bytes = 0usize;
    for (i, part) in order.iter().enumerate() {
        if body_bytes > limits.body {
            stats.truncated = true;
            break;
        }
        let Some(xml) = budget.read(&mut z, part) else {
            continue;
        };
        let slide = parse_shapes(&xml);
        let notes = notes_part(&mut z, &mut budget, part)
            .and_then(|n| budget.read(&mut z, &n))
            .map(|x| parse_shapes(&x).body)
            .unwrap_or_default();
        let body = match (slide.body.is_empty(), notes.is_empty()) {
            (_, true) => slide.body,
            (true, false) => format!("Speaker notes:\n{notes}"),
            (false, false) => format!("{}\n\nSpeaker notes:\n{notes}", slide.body),
        };
        if body.is_empty() {
            continue;
        }
        body_bytes += body.len();
        slides.push(Slide {
            number: i + 1,
            title: slide.title,
            body,
        });
    }
    if budget.exhausted {
        stats.truncated = true;
    }
    if slides.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }

    // The deck's title is its first slide title — normally the title slide.
    // `docProps/core.xml` is not used: PowerPoint fills it with its own
    // defaults far more often than authors set it.
    let deck_title = slides
        .iter()
        .find_map(|s| s.title.clone())
        .unwrap_or_else(|| {
            name.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into())
        });

    for slide in slides {
        for (section, text) in split_sections(&slide.body).into_iter().enumerate() {
            if stats.records as usize >= MAX_RECORDS_PER_FILE {
                stats.truncated = true;
                return Ok(stats);
            }
            let mut fields = Map::new();
            fields.insert("title".into(), Value::String(deck_title.clone()));
            fields.insert("slide".into(), Value::Number((slide.number as u64).into()));
            if let Some(t) = &slide.title {
                fields.insert("slide_title".into(), Value::String(t.clone()));
            }
            if section > 0 {
                fields.insert("section".into(), Value::Number((section as u64).into()));
            }
            fields.insert("body".into(), Value::String(text));
            stats.records += 1;
            if !sink(RawRecord {
                fields,
                locator: format!("slide{}-s{}", slide.number, section),
                group: None,
                // title/slide/slide_title/section/body are this extractor's
                // vocabulary; `slide_title` and `section` appear only sometimes.
                origin: FieldOrigin::Extractor,
            }) {
                return Ok(stats);
            }
        }
    }
    Ok(stats)
}

/// Slide part names in presentation order. Falls back to `ppt/slides/slideN.xml`
/// sorted by N when the presentation part or its relationships are unusable.
fn slide_order<R: Read + Seek>(z: &mut zip::ZipArchive<R>, budget: &mut Budget) -> Vec<String> {
    let ids = budget
        .read(z, "ppt/presentation.xml")
        .map(|x| slide_rel_ids(&x))
        .unwrap_or_default();
    let rels = budget
        .read(z, "ppt/_rels/presentation.xml.rels")
        .map(|x| parse_rels(&x))
        .unwrap_or_default();
    let ordered: Vec<String> = ids
        .iter()
        .filter_map(|id| {
            rels.iter()
                .find(|r| &r.id == id && r.kind.ends_with(REL_SLIDE))
                .map(|r| resolve_target("ppt", &r.target))
        })
        .collect();
    if !ordered.is_empty() {
        return ordered;
    }
    let mut numbered: Vec<(u32, String)> = z
        .file_names()
        .filter_map(|n| {
            // zip 9: entry names are fallible (non-UTF-8 names); skip those.
            let n = n.ok()?;
            let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
            Some((num.parse().ok()?, n.to_string()))
        })
        .collect();
    numbered.sort();
    numbered.into_iter().map(|(_, n)| n).collect()
}

/// The notes part for a slide part, through the slide's relationships.
fn notes_part<R: Read + Seek>(
    z: &mut zip::ZipArchive<R>,
    budget: &mut Budget,
    slide: &str,
) -> Option<String> {
    let (dir, file) = slide.rsplit_once('/')?;
    let rels = parse_rels(&budget.read(z, &format!("{dir}/_rels/{file}.rels"))?);
    rels.iter()
        .find(|r| r.kind.ends_with(REL_NOTES))
        .map(|r| resolve_target(dir, &r.target))
}

/// `r:id` of every `p:sldId` in `ppt/presentation.xml`, in document order.
fn slide_rel_ids(xml: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut ids = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"sldId" => {
                // `sldId` carries a numeric `id` too; the relationship id is
                // the namespaced one (`r:id`).
                if let Some(v) = attr(&e, |k| k.ends_with(b":id")) {
                    ids.push(v);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    ids
}

#[derive(PartialEq)]
enum Kind {
    Normal,
    Title,
    Skip,
}

/// What a `p:ph` placeholder makes of its shape.
fn placeholder_kind(ph: &BytesStart) -> Kind {
    match attr(ph, |k| k == b"type").as_deref() {
        Some("title" | "ctrTitle") => Kind::Title,
        Some(t) if SKIPPED_PLACEHOLDERS.contains(&t.as_bytes()) => Kind::Skip,
        _ => Kind::Normal,
    }
}

/// Shape-by-shape text of a slide or notes part.
///
/// Paragraphs within a shape are joined by `\n`, shapes by a blank line (the
/// boundary `split_sections` cuts on). A table becomes one block with a line
/// per row. Matching is on local names, so the strict-OOXML namespaces read
/// the same as the transitional ones.
fn parse_shapes(xml: &[u8]) -> SlideText {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut out = SlideText::default();
    let mut blocks: Vec<String> = Vec::new();
    let mut kind = Kind::Normal;
    let mut shape: Vec<String> = Vec::new();
    let mut para = String::new();
    let mut in_t = false;
    let mut in_fld = false;
    let mut in_tbl = false;
    let mut cell = String::new();
    let mut row: Vec<String> = Vec::new();
    let mut rows: Vec<String> = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"sp" | b"graphicFrame" | b"cxnSp" => {
                    kind = Kind::Normal;
                    shape.clear();
                }
                b"tbl" => {
                    in_tbl = true;
                    rows.clear();
                }
                b"tr" => row.clear(),
                b"tc" => cell.clear(),
                b"t" => in_t = true,
                b"fld" => in_fld = true,
                // Both can carry children (`a:br` usually has an `a:rPr`), so
                // they arrive as Start as often as Empty.
                b"ph" => kind = placeholder_kind(&e),
                b"br" => para.push(if in_tbl { ' ' } else { '\n' }),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"ph" => kind = placeholder_kind(&e),
                b"br" => para.push(if in_tbl { ' ' } else { '\n' }),
                _ => {}
            },
            Ok(Event::Text(t)) if in_t && !in_fld => {
                para.push_str(&t.xml10_content().unwrap_or_default());
            }
            Ok(Event::GeneralRef(r)) if in_t && !in_fld => {
                if let Some(resolved) = super::xml_x::resolve_general_ref(&r) {
                    para.push_str(&resolved);
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_t = false,
                b"fld" => in_fld = false,
                b"p" => {
                    let text = para.trim();
                    if !text.is_empty() {
                        if in_tbl {
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(text);
                        } else {
                            shape.push(text.to_string());
                        }
                    }
                    para.clear();
                }
                b"tc" => row.push(std::mem::take(&mut cell)),
                b"tr" => {
                    if row.iter().any(|c| !c.is_empty()) {
                        rows.push(row.join(" | "));
                    }
                    row.clear();
                }
                b"tbl" => {
                    in_tbl = false;
                    if !rows.is_empty() {
                        shape.push(rows.join("\n"));
                    }
                }
                b"sp" | b"graphicFrame" | b"cxnSp" => {
                    if kind != Kind::Skip && !shape.is_empty() {
                        let text = shape.join("\n");
                        if kind == Kind::Title && out.title.is_none() {
                            out.title = Some(text.split_whitespace().collect::<Vec<_>>().join(" "));
                        }
                        blocks.push(text);
                    }
                    shape.clear();
                    kind = Kind::Normal;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    // Text outside any shape element (not produced by PowerPoint, but valid).
    if !shape.is_empty() {
        blocks.push(shape.join("\n"));
    }
    out.body = blocks.join("\n\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = concat!(
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#
    );
    const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const REL_T: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

    /// A text shape; `ph` is the placeholder type (`""` = body placeholder,
    /// `None` = plain text box). Each string is one `a:p`.
    fn sp(ph: Option<&str>, paras: &[&str]) -> String {
        let ph = match ph {
            None => String::new(),
            Some("") => "<p:ph idx=\"1\"/>".into(),
            Some(t) => format!("<p:ph type=\"{t}\"/>"),
        };
        let paras: String = paras
            .iter()
            .map(|p| format!("<a:p><a:r><a:rPr lang=\"en-US\"/><a:t>{p}</a:t></a:r></a:p>"))
            .collect();
        format!(
            "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"s\"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr>\
             <p:spPr/><p:txBody><a:bodyPr/>{paras}</p:txBody></p:sp>"
        )
    }

    fn table(rows: &[&[&str]]) -> String {
        let rows: String = rows
            .iter()
            .map(|r| {
                let cells: String = r
                    .iter()
                    .map(|c| format!("<a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>{c}</a:t></a:r></a:p></a:txBody></a:tc>"))
                    .collect();
                format!("<a:tr h=\"370840\">{cells}</a:tr>")
            })
            .collect();
        format!(
            "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"4\" name=\"t\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr>\
             <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/table\">\
             <a:tbl><a:tblGrid/>{rows}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"
        )
    }

    fn slide_xml(shapes: &[String]) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <p:sld {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:sld>",
            shapes.concat()
        )
    }

    fn notes_xml(text: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <p:notes {NS}><p:cSld><p:spTree>{}{}{}</p:spTree></p:cSld></p:notes>",
            sp(Some("sldImg"), &[]),
            sp(Some(""), &[text]),
            sp(Some("sldNum"), &["7"]),
        )
    }

    fn rels(entries: &[(&str, &str, &str)]) -> String {
        let body: String = entries
            .iter()
            .map(|(id, ty, target)| {
                format!("<Relationship Id=\"{id}\" Type=\"{REL_T}/{ty}\" Target=\"{target}\"/>")
            })
            .collect();
        format!("<?xml version=\"1.0\"?><Relationships xmlns=\"{REL_NS}\">{body}</Relationships>")
    }

    /// A deck whose slides are listed in `order` (part numbers) — so a deck
    /// reordered in PowerPoint, where `slide2.xml` is shown first, is
    /// expressible. `notes[i]` belongs to part `i + 1`.
    fn write_deck(path: &Path, slides: &[String], order: &[usize], notes: &[Option<&str>]) {
        let mut members: Vec<(String, String)> = Vec::new();
        let ids: String = order
            .iter()
            .enumerate()
            .map(|(i, n)| format!("<p:sldId id=\"{}\" r:id=\"rId{n}\"/>", 256 + i))
            .collect();
        members.push((
            "ppt/presentation.xml".into(),
            format!("<?xml version=\"1.0\"?><p:presentation {NS}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"),
        ));
        let pres_rels: Vec<(String, &str, String)> = (1..=slides.len())
            .map(|n| (format!("rId{n}"), "slide", format!("slides/slide{n}.xml")))
            .collect();
        let pres_rels: Vec<(&str, &str, &str)> = pres_rels
            .iter()
            .map(|(a, b, c)| (a.as_str(), *b, c.as_str()))
            .collect();
        members.push(("ppt/_rels/presentation.xml.rels".into(), rels(&pres_rels)));
        for (i, s) in slides.iter().enumerate() {
            let n = i + 1;
            members.push((format!("ppt/slides/slide{n}.xml"), s.clone()));
            if let Some(Some(text)) = notes.get(i) {
                members.push((
                    format!("ppt/notesSlides/notesSlide{n}.xml"),
                    notes_xml(text),
                ));
                let target = format!("../notesSlides/notesSlide{n}.xml");
                members.push((
                    format!("ppt/slides/_rels/slide{n}.xml.rels"),
                    rels(&[
                        ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                        ("rId2", "notesSlide", target.as_str()),
                    ]),
                ));
            }
        }
        write_zip(path, &members);
    }

    fn write_zip(path: &Path, members: &[(String, String)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in members {
            z.start_file(name.as_str(), opts).unwrap();
            z.write_all(content.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    fn run(path: &Path, limits: Limits) -> (Vec<RawRecord>, ExtractStats) {
        let mut out = Vec::new();
        let stats = extract_bounded(
            path,
            path,
            &mut |r| {
                out.push(r);
                true
            },
            limits,
        )
        .unwrap();
        (out, stats)
    }

    fn field<'a>(r: &'a RawRecord, k: &str) -> &'a Value {
        static NULL: Value = Value::Null;
        r.fields.get(k).unwrap_or(&NULL)
    }

    #[test]
    fn one_record_per_slide_with_titles_tables_and_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pptx");
        let s1 = slide_xml(&[
            sp(Some("ctrTitle"), &["Q3 Review"]),
            sp(Some("subTitle"), &["Finance &amp; Ops"]),
        ]);
        let s2 = slide_xml(&[
            sp(Some("title"), &["Revenue by region"]),
            table(&[
                &["Region", "Q2", "Q3"],
                &["EMEA", "1.2", "1.5"],
                &["APAC", "0.8", ""],
            ]),
            sp(Some("dt"), &["10/1/2026"]),
            sp(Some("ftr"), &["Confidential"]),
        ]);
        write_deck(
            &path,
            &[s1, s2],
            &[1, 2],
            &[None, Some("EMEA grew on the Lisbon contract.")],
        );
        let (recs, stats) = run(&path, LIMITS);
        assert_eq!((stats.records, stats.junk, stats.truncated), (2, 0, false));

        let r1 = &recs[0];
        assert_eq!(*field(r1, "title"), "Q3 Review");
        assert_eq!(*field(r1, "slide"), 1);
        assert_eq!(*field(r1, "slide_title"), "Q3 Review");
        assert_eq!(*field(r1, "body"), "Q3 Review\n\nFinance & Ops");
        assert_eq!(r1.locator, "slide1-s0");
        assert_eq!(r1.origin, FieldOrigin::Extractor);

        let r2 = &recs[1];
        // Deck title on every slide; the slide's own title beside it.
        assert_eq!(*field(r2, "title"), "Q3 Review");
        assert_eq!(*field(r2, "slide_title"), "Revenue by region");
        assert_eq!(
            *field(r2, "body"),
            "Revenue by region\n\nRegion | Q2 | Q3\nEMEA | 1.2 | 1.5\nAPAC | 0.8 | \n\n\
             Speaker notes:\nEMEA grew on the Lisbon contract."
        );
        // Boilerplate placeholders and the notes page's slide number are gone.
        let body = field(r2, "body").as_str().unwrap();
        assert!(
            !body.contains("Confidential") && !body.contains("10/1/2026"),
            "{body}"
        );
        assert!(!body.contains("\n7"), "{body}");
    }

    /// Slide parts keep their names when slides are reordered; the number a
    /// reader sees is the position in `p:sldIdLst`.
    #[test]
    fn slides_follow_presentation_order_not_part_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reordered.pptx");
        let a = slide_xml(&[sp(Some("title"), &["Appendix"])]);
        let b = slide_xml(&[sp(Some("title"), &["Agenda"])]);
        write_deck(&path, &[a, b], &[2, 1], &[None, None]);
        let (recs, _) = run(&path, LIMITS);
        let got: Vec<(u64, &str)> = recs
            .iter()
            .map(|r| {
                (
                    field(r, "slide").as_u64().unwrap(),
                    field(r, "slide_title").as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(got, vec![(1, "Agenda"), (2, "Appendix")]);
        assert_eq!(*field(&recs[0], "title"), "Agenda");
    }

    /// Without a usable `presentation.xml`, slides still come out, in part
    /// number order (numerically: slide10 after slide9).
    #[test]
    fn falls_back_to_part_numbers_without_presentation_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.pptx");
        let members: Vec<(String, String)> = [10, 9, 1]
            .iter()
            .map(|n| {
                (
                    format!("ppt/slides/slide{n}.xml"),
                    slide_xml(&[sp(None, &[format!("part {n}").as_str()])]),
                )
            })
            .collect();
        write_zip(&path, &members);
        let (recs, _) = run(&path, LIMITS);
        let bodies: Vec<&str> = recs
            .iter()
            .map(|r| field(r, "body").as_str().unwrap())
            .collect();
        assert_eq!(bodies, ["part 1", "part 9", "part 10"]);
        // No title placeholder anywhere: the file stem titles the deck.
        assert_eq!(*field(&recs[0], "title"), "bare");
        assert!(field(&recs[0], "slide_title").is_null());
    }

    /// A slide with nothing but notes is kept; a slide with no text at all is
    /// skipped without renumbering the rest; a deck with no text is junk.
    #[test]
    fn empty_slides_and_empty_decks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.pptx");
        let blank = slide_xml(&[]);
        let last = slide_xml(&[sp(None, &["closing"])]);
        write_deck(
            &path,
            &[blank.clone(), blank.clone(), last],
            &[1, 2, 3],
            &[None, Some("only notes"), None],
        );
        let (recs, _) = run(&path, LIMITS);
        let got: Vec<(u64, &str)> = recs
            .iter()
            .map(|r| {
                (
                    field(r, "slide").as_u64().unwrap(),
                    field(r, "body").as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(got, vec![(2, "Speaker notes:\nonly notes"), (3, "closing")]);

        let empty = dir.path().join("images-only.pptx");
        write_deck(&empty, &[blank], &[1], &[None]);
        let (recs, stats) = run(&empty, LIMITS);
        assert!(recs.is_empty());
        assert_eq!((stats.records, stats.junk), (0, 1));
    }

    /// Durable preparation extracts a sealed snapshot blob (`00000000`); a deck
    /// with no slide title must still be titled from its own name (#722's
    /// class).
    #[test]
    fn an_untitled_deck_is_titled_from_the_logical_name_not_the_blob() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("00000000");
        write_deck(
            &blob,
            &[slide_xml(&[sp(None, &["body text"])])],
            &[1],
            &[None],
        );
        let sn = crate::sniff::sniff_with_name(&blob, Path::new("decks/kickoff.pptx")).unwrap();
        assert_eq!(sn.family, crate::sniff::Family::Pptx);
        let mut recs = Vec::new();
        super::super::extract(&blob, &sn, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(*field(&recs[0], "title"), "kickoff");
    }

    /// A slide longer than one section splits like a PDF page does.
    #[test]
    fn a_long_slide_is_sectioned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dense.pptx");
        let paras: Vec<String> = (0..40)
            .map(|i| format!("{i:02} {}", "word ".repeat(30)))
            .collect();
        let shapes: Vec<String> = paras.iter().map(|p| sp(None, &[p.as_str()])).collect();
        write_deck(&path, &[slide_xml(&shapes)], &[1], &[None]);
        let (recs, _) = run(&path, LIMITS);
        assert!(recs.len() > 1, "{}", recs.len());
        assert!(field(&recs[0], "section").is_null());
        assert_eq!(*field(&recs[1], "section"), 1);
        assert_eq!(recs[1].locator, "slide1-s1");
        assert!(recs.iter().all(|r| *field(r, "slide") == 1));
    }

    /// The container-wide decompression budget stops reading slides and says
    /// so; what was read before the cap is still indexed.
    #[test]
    fn decompression_budget_truncates_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.pptx");
        let slides: Vec<String> = (0..20)
            .map(|i| {
                slide_xml(&[sp(
                    None,
                    &[format!("slide {i} {}", "x".repeat(2000)).as_str()],
                )])
            })
            .collect();
        let order: Vec<usize> = (1..=20).collect();
        write_deck(&path, &slides, &order, &[]);
        let (all, stats) = run(&path, LIMITS);
        assert_eq!((all.len(), stats.truncated), (20, false));

        let tight = Limits {
            total: 16 << 10,
            ..LIMITS
        };
        let (some, stats) = run(&path, tight);
        assert!(stats.truncated);
        assert!(!some.is_empty() && some.len() < 20, "{}", some.len());
    }
}
