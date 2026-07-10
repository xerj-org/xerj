//! DOCX — zip container, `word/document.xml` streamed through quick-xml.
//! Collects w:t runs, breaks paragraphs on w:p, records Heading-styled
//! paragraphs as headings. One document record (sectioned at 32KB).

use super::{emit_document, ExtractStats, Sink};
use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::BufReader;
use std::path::Path;

pub fn extract(path: &Path, sink: Sink) -> Result<ExtractStats> {
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
    let mut reader = Reader::from_reader(BufReader::new(entry));
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
                para.push_str(&t.unescape().unwrap_or_default());
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
        if body.len() > 64 << 20 {
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
    emit_document(&title, &headings, body, sink, &mut stats);
    Ok(stats)
}
