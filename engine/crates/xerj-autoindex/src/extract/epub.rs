//! EPUB — zip container, one record per spine document in reading order.
//!
//! The package document is found through `META-INF/container.xml`; its
//! `<metadata>` gives the book's `title`, `author`, `language`, `publisher`
//! and `date`, its `<manifest>` maps ids to parts and its `<spine>` gives the
//! reading order. Each spine document is XHTML and is read by the HTML
//! extractor's tokenizer, so a chapter's text is what a `.html` page's would
//! be. `chapter_title` comes from the table of contents (the EPUB 3 `nav`
//! document, else the EPUB 2 NCX), else the document's first heading.
//!
//! Books often pack several chapters into one document (Project Gutenberg's
//! Moby-Dick: 135 chapters in 13 files), so a document is cut where each
//! table-of-contents entry's anchor (`file.xhtml#id`) begins, and each piece
//! is titled by its entry and numbered `part` (`k`). A long piece is sectioned
//! like a PDF page (`section`, `j`). Locators are `ch{n}-s{j}`, or
//! `ch{n}.{k}-s{j}` for a document cut into parts. `chapter` (`n`) is the
//! document's 1-based position among the spine's XHTML documents, so it stays
//! put when a neighbor is skipped or empty.
//!
//! Skipped: the navigation document when it is also in the spine (it repeats
//! the chapter titles), spine items that are not XHTML, and parts listed in
//! `META-INF/encryption.xml` under anything but font obfuscation — DRM, whose
//! ciphertext would index as noise. A book with no readable chapter is junk.

use super::html;
use super::opc::{attr, resolve_target, Budget};
use super::{split_sections, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_RECORDS_PER_FILE};
use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use std::path::Path;

// SECURITY: bound the DECOMPRESSED reads, as `pptx.rs` does: one cap per part
// (a chapter is kilobytes to a few megabytes) and one across the container (a
// thousand capped parts is still a zip bomb). Past either, the remaining
// chapters are dropped and the file is reported `truncated`.
const MAX_PART_BYTES: u64 = 32 << 20;
const MAX_TOTAL_DECOMPRESSED_BYTES: u64 = 256 << 20;

/// Extracted-text cap across the book, matching `docx.rs` and `pptx.rs`.
const MAX_BODY_BYTES: usize = 64 << 20;

/// `EncryptedData` algorithms that only obfuscate embedded fonts (IDPF and
/// Adobe); the text is still readable. Anything else is DRM.
const FONT_OBFUSCATION: &[&str] = &[
    "http://www.idpf.org/2008/embedding",
    "http://ns.adobe.com/pdf/enc#RC",
];

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
/// is not `path` (a sealed snapshot blob); a book with no `dc:title` is
/// titled from it.
pub fn extract(path: &Path, name: &Path, sink: Sink) -> Result<ExtractStats> {
    extract_bounded(path, name, sink, LIMITS)
}

/// What the package document says about the book.
#[derive(Debug, Default)]
struct Package {
    title: Option<String>,
    authors: Vec<String>,
    language: Option<String>,
    publisher: Option<String>,
    date: Option<String>,
    /// Spine documents in reading order, as package-root paths.
    spine: Vec<String>,
    /// The EPUB 3 navigation document, if the manifest names one.
    nav: Option<String>,
    /// The EPUB 2 NCX, from `spine@toc` or its media type.
    ncx: Option<String>,
}

/// One chapter ready to emit: a spine document, or one table-of-contents
/// piece of it (`part`, 1-based) when the document holds several entries.
struct Chapter {
    number: usize,
    part: Option<usize>,
    title: Option<String>,
    body: String,
}

fn extract_bounded(path: &Path, name: &Path, sink: Sink, limits: Limits) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let f = std::fs::File::open(path)?;
    let mut z = zip::ZipArchive::new(f).context("open epub container")?;
    let mut budget = Budget::new(limits.part, limits.total);

    let Some(opf) = budget
        .read(&mut z, "META-INF/container.xml")
        .and_then(|x| rootfile(&x))
    else {
        stats.junk += 1;
        return Ok(stats);
    };
    let Some(pkg) = budget.read(&mut z, &opf).map(|x| parse_package(&x, &opf)) else {
        stats.junk += 1;
        return Ok(stats);
    };
    let locked = budget
        .read(&mut z, "META-INF/encryption.xml")
        .map(|x| encrypted_parts(&x))
        .unwrap_or_default();
    let toc = toc_entries(&mut z, &mut budget, &pkg);

    let mut chapters: Vec<Chapter> = Vec::new();
    let mut body_bytes = 0usize;
    for (i, part) in pkg.spine.iter().enumerate() {
        if body_bytes > limits.body {
            stats.truncated = true;
            break;
        }
        if pkg.nav.as_deref() == Some(part.as_str()) || locked.contains(part) {
            continue;
        }
        let Some(bytes) = budget.read(&mut z, part) else {
            continue;
        };
        let (text, _) = crate::sniff::decode_text(&bytes);
        let doc = html::parse(&text);
        let pieces = toc_pieces(&doc, toc.get(part).map(Vec::as_slice).unwrap_or(&[]));
        let split = pieces.len() > 1;
        for (k, (title, body)) in pieces.into_iter().enumerate() {
            body_bytes += body.len();
            chapters.push(Chapter {
                number: i + 1,
                part: split.then_some(k + 1),
                title,
                body,
            });
        }
    }
    if budget.exhausted {
        stats.truncated = true;
    }
    if chapters.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }

    let book_title = pkg.title.clone().unwrap_or_else(|| {
        name.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".into())
    });
    let author = (!pkg.authors.is_empty()).then(|| pkg.authors.join("; "));

    for ch in chapters {
        for (section, text) in split_sections(&ch.body).into_iter().enumerate() {
            if stats.records as usize >= MAX_RECORDS_PER_FILE {
                stats.truncated = true;
                return Ok(stats);
            }
            let mut fields = Map::new();
            fields.insert("title".into(), Value::String(book_title.clone()));
            for (k, v) in [
                ("author", &author),
                ("language", &pkg.language),
                ("publisher", &pkg.publisher),
                ("date", &pkg.date),
            ] {
                if let Some(v) = v {
                    fields.insert(k.into(), Value::String(v.clone()));
                }
            }
            fields.insert("chapter".into(), Value::Number((ch.number as u64).into()));
            if let Some(k) = ch.part {
                fields.insert("part".into(), Value::Number((k as u64).into()));
            }
            if let Some(t) = &ch.title {
                fields.insert("chapter_title".into(), Value::String(t.clone()));
            }
            if section > 0 {
                fields.insert("section".into(), Value::Number((section as u64).into()));
            }
            fields.insert("body".into(), Value::String(text));
            stats.records += 1;
            if !sink(RawRecord {
                fields,
                locator: match ch.part {
                    Some(k) => format!("ch{}.{k}-s{section}", ch.number),
                    None => format!("ch{}-s{section}", ch.number),
                },
                group: None,
                // This extractor's vocabulary; the metadata fields and
                // `part`/`chapter_title`/`section` appear only sometimes.
                origin: FieldOrigin::Extractor,
            }) {
                return Ok(stats);
            }
        }
    }
    Ok(stats)
}

/// Cut one spine document into `(chapter_title, text)` pieces at the
/// table-of-contents entries that point into it.
///
/// An entry with a fragment starts a piece where the element with that id
/// begins; one without starts at the top. Entries whose anchor is missing are
/// ignored (if none resolves, the first entry titles the whole document), and
/// where two land on the same spot the first label wins. Text
/// before the first entry is a piece of its own, untitled (front matter, or a
/// chapter's opening when the toc points at a heading inside it). A document
/// no entry points into is one piece titled by its first heading. Empty
/// pieces are dropped, and a piece holding only its title joins the next.
fn toc_pieces(doc: &html::Doc, entries: &[Entry]) -> Vec<(Option<String>, String)> {
    let body = doc.body.as_str();
    let mut anchors: HashMap<&str, usize> = HashMap::new();
    for (id, at) in &doc.anchors {
        anchors.entry(id.as_str()).or_insert(*at);
    }
    // An anchor recorded just before `line_break` trimmed a trailing space can
    // sit one byte past the end; offsets are otherwise on char boundaries.
    let clamp = |mut at: usize| {
        at = at.min(body.len());
        while !body.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let mut cuts: Vec<(usize, Option<String>)> = Vec::new();
    for e in entries {
        let at = match &e.fragment {
            None => 0,
            Some(f) => match anchors.get(f.as_str()) {
                Some(&at) => clamp(at),
                None => continue,
            },
        };
        if !cuts.iter().any(|(c, _)| *c == at) {
            cuts.push((at, Some(e.label.clone())));
        }
    }
    cuts.sort_by_key(|(at, _)| *at);
    if cuts.is_empty() {
        // No entry resolved to an anchor here: the whole document, titled by
        // its first entry as when it is the entry's own file.
        let title = entries.first().map(|e| e.label.clone());
        cuts.push((0, title.or_else(|| doc.headings.first().cloned())));
    } else if cuts[0].0 > 0 {
        cuts.insert(0, (0, None));
    }
    // A piece that is nothing but its own title (a chapter heading whose
    // subtitle is a toc entry of its own, a part heading before its first
    // chapter) is folded into the next piece, and the titles are joined.
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: Vec<(Option<String>, String)> = Vec::with_capacity(cuts.len());
    let mut carry: Option<(Option<String>, String)> = None;
    for (k, (at, title)) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).map_or(body.len(), |(next, _)| *next);
        let text = body[*at..end].trim();
        if text.is_empty() {
            continue;
        }
        let heading_only = title.as_deref().is_some_and(|t| words(t) == words(text));
        let (title, text) = match carry.take() {
            Some((t0, x0)) => (
                match (t0, title.clone()) {
                    (Some(a), Some(b)) => Some(format!("{a} — {b}")),
                    (a, b) => a.or(b),
                },
                format!("{x0}\n\n{text}"),
            ),
            None => (title.clone(), text.to_string()),
        };
        if heading_only {
            carry = Some((title, text));
        } else {
            out.push((title, text));
        }
    }
    out.extend(carry);
    out
}

/// The package document's path from `META-INF/container.xml`: the first
/// `rootfile` of the OPF media type, else the first `rootfile`.
fn rootfile(xml: &[u8]) -> Option<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut first: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"rootfile" => {
                if let Some(p) = attr(&e, |k| k == b"full-path") {
                    let p = resolve_target("", &percent_decode(&p));
                    if attr(&e, |k| k == b"media-type").as_deref()
                        == Some("application/oebps-package+xml")
                    {
                        return Some(p);
                    }
                    first.get_or_insert(p);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    first
}

/// Metadata, spine order and navigation parts from the package document at
/// `opf` (manifest hrefs are relative to its directory).
fn parse_package(xml: &[u8], opf: &str) -> Package {
    let dir = opf.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut pkg = Package::default();
    // id -> (path, media type, properties)
    let mut manifest: HashMap<String, (String, String, String)> = HashMap::new();
    let mut spine_ids: Vec<String> = Vec::new();
    let mut toc_id: Option<String> = None;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut in_metadata = false;
    // The Dublin Core element whose text is being read, and that text.
    let mut field: Option<&'static str> = None;
    let mut text = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"metadata" => in_metadata = true,
                b"title" if in_metadata => field = Some("title"),
                b"creator" if in_metadata => field = Some("creator"),
                b"language" if in_metadata => field = Some("language"),
                b"publisher" if in_metadata => field = Some("publisher"),
                b"date" if in_metadata => field = Some("date"),
                b"item" => manifest_item(&e, dir, &mut manifest),
                b"itemref" => spine_ids.extend(attr(&e, |k| k == b"idref")),
                b"spine" => toc_id = attr(&e, |k| k == b"toc"),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"item" => manifest_item(&e, dir, &mut manifest),
                b"itemref" => spine_ids.extend(attr(&e, |k| k == b"idref")),
                _ => {}
            },
            Ok(Event::Text(t)) if field.is_some() => {
                text.push_str(&t.xml10_content().unwrap_or_default());
            }
            Ok(Event::GeneralRef(r)) if field.is_some() => {
                if let Some(resolved) = super::xml_x::resolve_general_ref(&r) {
                    text.push_str(&resolved);
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == b"metadata" {
                    in_metadata = false;
                }
                if let Some(f) = field.take() {
                    let v = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    text.clear();
                    if !v.is_empty() {
                        // The first of each wins (EPUB 3 refines a main title
                        // with `meta` elements; the first `dc:title` is the
                        // one readers show without them). Every creator is
                        // kept.
                        match f {
                            "title" => drop(pkg.title.get_or_insert(v)),
                            "creator" => pkg.authors.push(v),
                            "language" => drop(pkg.language.get_or_insert(v)),
                            "publisher" => drop(pkg.publisher.get_or_insert(v)),
                            _ => drop(pkg.date.get_or_insert(v)),
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }

    let mut seen = HashSet::new();
    for id in spine_ids {
        if let Some((path, media, _)) = manifest.get(&id) {
            if is_xhtml(media) && seen.insert(path.clone()) {
                pkg.spine.push(path.clone());
            }
        }
    }
    pkg.nav = manifest
        .values()
        .find(|(_, _, props)| props.split_whitespace().any(|p| p == "nav"))
        .map(|(p, _, _)| p.clone());
    pkg.ncx = toc_id
        .and_then(|id| manifest.get(&id))
        .or_else(|| {
            manifest
                .values()
                .find(|(_, m, _)| m == "application/x-dtbncx+xml")
        })
        .map(|(p, _, _)| p.clone());
    pkg
}

fn manifest_item(
    e: &quick_xml::events::BytesStart,
    dir: &str,
    manifest: &mut HashMap<String, (String, String, String)>,
) {
    if let (Some(id), Some(href)) = (attr(e, |k| k == b"id"), attr(e, |k| k == b"href")) {
        let path = resolve_target(dir, &percent_decode(strip_fragment(&href)));
        let media = attr(e, |k| k == b"media-type").unwrap_or_default();
        let props = attr(e, |k| k == b"properties").unwrap_or_default();
        manifest.insert(id, (path, media, props));
    }
}

fn is_xhtml(media: &str) -> bool {
    matches!(media, "application/xhtml+xml" | "text/html")
}

/// Package-root paths of parts encrypted with anything but font obfuscation.
fn encrypted_parts(xml: &[u8]) -> HashSet<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = HashSet::new();
    let mut algorithm: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"EncryptedData" => algorithm = None,
                b"EncryptionMethod" => algorithm = attr(&e, |k| k == b"Algorithm"),
                b"CipherReference" => {
                    let font = algorithm
                        .as_deref()
                        .is_some_and(|a| FONT_OBFUSCATION.contains(&a));
                    if let (false, Some(uri)) = (font, attr(&e, |k| k == b"URI")) {
                        out.insert(resolve_target("", &percent_decode(&uri)));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    out
}

/// One table-of-contents entry: the spine document it points into, the
/// fragment (`#id`) within it, if any, and its label.
struct Entry {
    path: String,
    fragment: Option<String>,
    label: String,
}

/// Table-of-contents entries by spine path, in table-of-contents order. The
/// EPUB 3 `nav` document is preferred; the NCX is read when there is no `nav`
/// or it yields nothing.
fn toc_entries<R: Read + Seek>(
    z: &mut zip::ZipArchive<R>,
    budget: &mut Budget,
    pkg: &Package,
) -> HashMap<String, Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    if let Some(nav) = &pkg.nav {
        if let Some(x) = budget.read(z, nav) {
            entries = nav_entries(&x, parent(nav));
        }
    }
    if entries.is_empty() {
        if let Some(ncx) = &pkg.ncx {
            if let Some(x) = budget.read(z, ncx) {
                entries = ncx_entries(&x, parent(ncx));
            }
        }
    }
    let mut out: HashMap<String, Vec<Entry>> = HashMap::new();
    for e in entries {
        out.entry(e.path.clone()).or_default().push(e);
    }
    out
}

/// Each link in the `toc` nav (else the first nav) of an EPUB 3 navigation
/// document, in document order.
fn nav_entries(xml: &[u8], dir: &str) -> Vec<Entry> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    // Per nav element: whether it is the toc, and its entries.
    let mut navs: Vec<(bool, Vec<Entry>)> = Vec::new();
    let mut nav_depth = 0usize;
    let mut link: Option<(String, String)> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"nav" => {
                    if nav_depth == 0 {
                        let toc = attr(&e, |k| k.ends_with(b"type"))
                            .is_some_and(|t| t.split_whitespace().any(|w| w == "toc"));
                        navs.push((toc, Vec::new()));
                    }
                    nav_depth += 1;
                }
                b"a" if nav_depth > 0 => {
                    link = attr(&e, |k| k == b"href").map(|h| (h, String::new()));
                }
                // A line break inside a label separates words
                // ("Around the<br/>World").
                b"br" => {
                    if let Some((_, label)) = link.as_mut() {
                        label.push(' ');
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"br" => {
                if let Some((_, label)) = link.as_mut() {
                    label.push(' ');
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"nav" => nav_depth = nav_depth.saturating_sub(1),
                b"a" => {
                    if let (Some((href, label)), Some((_, list))) = (link.take(), navs.last_mut()) {
                        push_entry(list, dir, &href, &label);
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Some((_, label)) = link.as_mut() {
                    label.push_str(&t.xml10_content().unwrap_or_default());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let (Some((_, label)), Some(s)) =
                    (link.as_mut(), super::xml_x::resolve_general_ref(&r))
                {
                    label.push_str(&s);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    let pick = navs.iter().position(|(toc, _)| *toc).unwrap_or(0);
    navs.into_iter()
        .nth(pick)
        .map(|(_, l)| l)
        .unwrap_or_default()
}

/// Each `navPoint` of an EPUB 2 NCX, in document order (a `navPoint`'s label
/// precedes its `content`).
fn ncx_entries(xml: &[u8], dir: &str) -> Vec<Entry> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut in_label = false;
    let mut in_text = false;
    let mut label = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"navLabel" => {
                    in_label = true;
                    label.clear();
                }
                b"text" if in_label => in_text = true,
                b"content" => {
                    if let Some(src) = attr(&e, |k| k == b"src") {
                        push_entry(&mut out, dir, &src, &label);
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"content" => {
                if let Some(src) = attr(&e, |k| k == b"src") {
                    push_entry(&mut out, dir, &src, &label);
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"navLabel" => in_label = false,
                b"text" => in_text = false,
                _ => {}
            },
            Ok(Event::Text(t)) if in_text => {
                label.push_str(&t.xml10_content().unwrap_or_default());
            }
            Ok(Event::GeneralRef(r)) if in_text => {
                if let Some(s) = super::xml_x::resolve_general_ref(&r) {
                    label.push_str(&s);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    out
}

fn push_entry(out: &mut Vec<Entry>, dir: &str, href: &str, label: &str) {
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let (file, fragment) = match href.split_once('#') {
        Some((f, frag)) => (f, Some(percent_decode(frag)).filter(|f| !f.is_empty())),
        None => (href, None),
    };
    // External links (and empty labels) are not chapter titles.
    if label.is_empty() || file.is_empty() || file.contains("://") {
        return;
    }
    out.push(Entry {
        path: resolve_target(dir, &percent_decode(file)),
        fragment,
        label,
    });
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn strip_fragment(href: &str) -> &str {
    href.split('#').next().unwrap_or("")
}

/// Decode `%XX` escapes in an href (`Chapter%201.xhtml` names the zip member
/// `Chapter 1.xhtml`). A malformed escape is kept as written.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    fn xhtml(title: &str, body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n\
             <html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{title}</title></head>\
             <body>{body}</body></html>"
        )
    }

    /// An EPUB 3 book: spine order differs from file names and manifest
    /// order, the nav document is itself in the spine, and one chapter holds
    /// a table.
    fn epub3() -> Vec<(&'static str, String)> {
        vec![
            ("mimetype", "application/epub+zip".into()),
            ("META-INF/container.xml", CONTAINER.into()),
            (
                "OEBPS/content.opf",
                r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>The   Harbor &amp; the Sea</dc:title>
    <dc:creator>Ann Author</dc:creator><dc:creator>Bo Second</dc:creator>
    <dc:language>en</dc:language><dc:publisher>Small Press</dc:publisher>
    <dc:date>2021-05-04</dc:date>
  </metadata>
  <manifest>
    <item id="c1" href="text/a.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="text/b.xhtml" media-type="application/xhtml+xml"/>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="cover" href="img/cover.jpg" media-type="image/jpeg"/>
  </manifest>
  <spine><itemref idref="nav"/><itemref idref="c2"/><itemref idref="cover"/><itemref idref="c1" linear="no"/></spine>
</package>"#
                    .into(),
            ),
            (
                "OEBPS/nav.xhtml",
                r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="landmarks"><ol><li><a href="text/a.xhtml">Landmark A</a></li></ol></nav>
<nav epub:type="toc"><ol>
  <li><a href="text/b.xhtml#start">Chapter <em>One</em></a></li>
  <li><a href="text/b.xhtml#later">Not the first entry</a></li>
  <li><a href="https://example.com/x.xhtml">External</a></li>
</ol></nav></body></html>"#
                    .into(),
            ),
            (
                "OEBPS/text/b.xhtml",
                xhtml(
                    "The Harbor",
                    "<h1>One</h1><p>The tide came in.</p>\
                     <table><tr><th>Port</th><th>Depth</th></tr><tr><td>North</td><td>12</td></tr></table>",
                ),
            ),
            (
                "OEBPS/text/a.xhtml",
                xhtml("The Harbor", "<h2>Endnotes</h2><p>A note at the back.</p>"),
            ),
            ("OEBPS/img/cover.jpg", "\u{1}binary".into()),
        ]
    }

    fn zip_of(parts: &[(&str, String)]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        {
            let mut w = zip::ZipWriter::new(f.as_file_mut());
            let opts = zip::write::SimpleFileOptions::default();
            for (name, body) in parts {
                w.start_file(*name, opts).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        f
    }

    fn run_with(parts: &[(&str, String)], limits: Limits) -> (ExtractStats, Vec<RawRecord>) {
        let f = zip_of(parts);
        let mut recs = Vec::new();
        let stats = extract_bounded(
            f.path(),
            Path::new("books/harbor.epub"),
            &mut |r| {
                recs.push(r);
                true
            },
            limits,
        )
        .unwrap();
        (stats, recs)
    }

    fn run(parts: &[(&str, String)]) -> (ExtractStats, Vec<RawRecord>) {
        run_with(parts, LIMITS)
    }

    fn s<'a>(r: &'a RawRecord, k: &str) -> Option<&'a str> {
        r.fields.get(k).and_then(Value::as_str)
    }

    #[test]
    fn chapters_follow_the_spine_with_toc_titles_and_book_metadata() {
        let (stats, recs) = run(&epub3());
        assert_eq!(stats.records, 2, "nav and cover are not chapters");
        assert!(!stats.truncated);

        // Spine order: b before a, numbered by position among the spine's
        // XHTML documents. The cover image is not one; the nav is, and keeps
        // its number (1) though it is not emitted.
        let locs: Vec<&str> = recs.iter().map(|r| r.locator.as_str()).collect();
        assert_eq!(locs, ["ch2-s0", "ch3-s0"]);
        assert_eq!(recs[0].fields["chapter"], serde_json::json!(2));

        // The toc nav, not landmarks; its FIRST entry into the file; inline
        // markup in the label flattened.
        assert_eq!(s(&recs[0], "chapter_title"), Some("Chapter One"));
        // Not in the toc: the document's first heading.
        assert_eq!(s(&recs[1], "chapter_title"), Some("Endnotes"));

        for r in &recs {
            assert_eq!(s(r, "title"), Some("The Harbor & the Sea"));
            assert_eq!(s(r, "author"), Some("Ann Author; Bo Second"));
            assert_eq!(s(r, "language"), Some("en"));
            assert_eq!(s(r, "publisher"), Some("Small Press"));
            assert_eq!(s(r, "date"), Some("2021-05-04"));
            assert!(matches!(r.origin, FieldOrigin::Extractor));
        }
        let body = s(&recs[0], "body").unwrap();
        assert!(body.contains("The tide came in."), "{body:?}");
        assert!(
            body.contains("North | 12"),
            "chapter tables keep their rows: {body:?}"
        );
        assert!(
            !body.contains("Landmark") && !body.contains('<'),
            "{body:?}"
        );
    }

    /// EPUB 2: no nav, titles from the NCX named by `spine@toc`; the package
    /// sits in a subdirectory and hrefs are percent-encoded and use `..`.
    #[test]
    fn an_epub2_book_takes_titles_from_the_ncx_and_decodes_hrefs() {
        let container = CONTAINER.replace("OEBPS/content.opf", "pkg/book.opf");
        let parts = vec![
            ("META-INF/container.xml", container),
            (
                "pkg/book.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Old Book</dc:title></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="c1" href="../text/Chapter%201.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="c1"/></spine>
</package>"#
                    .into(),
            ),
            (
                "pkg/toc.ncx",
                r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
  <navPoint id="p1"><navLabel><text>I. Arrival</text></navLabel><content src="../text/Chapter%201.xhtml"/></navPoint>
</navMap></ncx>"#
                    .into(),
            ),
            ("text/Chapter 1.xhtml", xhtml("x", "<p>We arrived.</p>")),
        ];
        let (stats, recs) = run(&parts);
        assert_eq!(stats.records, 1);
        assert_eq!(s(&recs[0], "title"), Some("Old Book"));
        assert_eq!(s(&recs[0], "chapter_title"), Some("I. Arrival"));
        assert!(recs[0].fields.get("author").is_none(), "absent, not empty");
        assert!(s(&recs[0], "body").unwrap().contains("We arrived."));
    }

    /// DRM-encrypted chapters are ciphertext: skipped, and a book that is
    /// nothing else is junk. Font obfuscation leaves the text readable.
    #[test]
    fn drm_encrypted_chapters_are_skipped_but_font_obfuscation_is_not_drm() {
        let enc = |alg: &str, uri: &str| {
            format!(
                r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:EncryptionMethod Algorithm="{alg}"/>
    <enc:CipherData><enc:CipherReference URI="{uri}"/></enc:CipherData></enc:EncryptedData>
</encryption>"#
            )
        };
        let aes = "http://www.w3.org/2001/04/xmlenc#aes128-cbc";
        let mut parts = epub3();
        parts.push(("META-INF/encryption.xml", enc(aes, "OEBPS/text/b.xhtml")));
        let (stats, recs) = run(&parts);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].locator, "ch3-s0", "b.xhtml is locked");

        let mut parts = epub3();
        parts.push((
            "META-INF/encryption.xml",
            enc("http://www.idpf.org/2008/embedding", "OEBPS/text/b.xhtml"),
        ));
        assert_eq!(run(&parts).0.records, 2);

        let mut parts = epub3();
        parts.retain(|(n, _)| *n != "OEBPS/text/a.xhtml");
        parts.push(("META-INF/encryption.xml", enc(aes, "OEBPS/text/b.xhtml")));
        let (stats, recs) = run(&parts);
        assert_eq!((stats.records, stats.junk, recs.len()), (0, 1, 0));
    }

    #[test]
    fn a_book_without_a_package_or_without_text_is_junk() {
        let mut parts = epub3();
        parts.retain(|(n, _)| *n != "META-INF/container.xml");
        assert_eq!(run(&parts).0.junk, 1);

        let mut parts = epub3();
        for (n, body) in parts.iter_mut() {
            if n.ends_with("a.xhtml") || n.ends_with("b.xhtml") {
                *body = xhtml("t", "<img src=\"p.jpg\"/>");
            }
        }
        let (stats, recs) = run(&parts);
        assert_eq!((stats.records, stats.junk, recs.len()), (0, 1, 0));
    }

    #[test]
    fn a_book_without_a_title_is_titled_from_its_logical_name() {
        let mut parts = epub3();
        for (n, body) in parts.iter_mut() {
            if *n == "OEBPS/content.opf" {
                *body = body.replace("<dc:title>The   Harbor &amp; the Sea</dc:title>", "");
            }
        }
        let (_, recs) = run(&parts);
        assert_eq!(s(&recs[0], "title"), Some("harbor"));
    }

    #[test]
    fn a_long_chapter_is_sectioned() {
        let para = "<p>".to_string() + &"word ".repeat(2000) + "</p>";
        let mut parts = epub3();
        for (n, body) in parts.iter_mut() {
            if n.ends_with("b.xhtml") {
                *body = xhtml("t", &para.repeat(10));
            }
        }
        let (_, recs) = run(&parts);
        let ch2: Vec<&RawRecord> = recs
            .iter()
            .filter(|r| r.locator.starts_with("ch2-"))
            .collect();
        assert!(ch2.len() > 1, "a 100 KB chapter splits");
        assert_eq!(ch2[1].locator, "ch2-s1");
        assert_eq!(ch2[1].fields["section"], serde_json::json!(1));
        assert!(ch2[0].fields.get("section").is_none());
    }

    #[test]
    fn the_decompression_budget_truncates_rather_than_reading_on() {
        let limits = Limits {
            part: 1 << 20,
            total: 1_500,
            body: MAX_BODY_BYTES,
        };
        let (stats, _) = run_with(&epub3(), limits);
        assert!(stats.truncated, "a spent budget is reported");
    }

    /// One document holding several chapters (Gutenberg packs 7-15 per file)
    /// is cut at each table-of-contents anchor, so a chapter's text carries
    /// its own title. Text before the first anchor is an untitled part; an
    /// entry whose anchor is missing is ignored; two entries on one anchor
    /// keep the first label; a `<br/>` in a label separates words.
    #[test]
    fn a_document_holding_several_chapters_is_cut_at_its_toc_anchors() {
        let mut parts = epub3();
        for (n, body) in parts.iter_mut() {
            if n.ends_with("nav.xhtml") {
                *body = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
  <li><a href="text/b.xhtml#c1">Chapter<br/>1. Loomings</a></li>
  <li><a href="text/b.xhtml#gone">Broken link</a></li>
  <li><a href="text/b.xhtml#c2">Chapter 2. The Carpet-Bag</a></li>
  <li><a href="text/b.xhtml#c2">Duplicate of chapter 2</a></li>
  <li><a href="text/b.xhtml#c3">Chapter 3. The Spouter-Inn</a></li>
  <li><a href="text/b.xhtml#c4">CHAPTER 4</a></li>
  <li><a href="text/b.xhtml#c4b">The Counterpane.</a></li>
</ol></nav></body></html>"#
                    .into();
            }
            if n.ends_with("b.xhtml") {
                *body = xhtml(
                    "Moby",
                    "<p>Front matter before any chapter.</p>\
                     <h2 id=\"c1\">CHAPTER 1</h2><p>Call me Ishmael.</p>\
                     <h2><a name=\"c2\"></a>CHAPTER 2</h2><p>I stuffed a shirt or two.</p>\
                     <div class='chapter' id='c3'><h2>CHAPTER 3</h2><p>Entering that gable-ended inn.</p></div>\
                     <h2 id=\"c4\">CHAPTER 4</h2><h3 id=\"c4b\">The Counterpane.</h3>\
                     <p>Upon waking next morning.</p>",
                );
            }
        }
        let (_, recs) = run(&parts);
        let ch2: Vec<&RawRecord> = recs
            .iter()
            .filter(|r| r.locator.starts_with("ch2"))
            .collect();
        let got: Vec<(&str, Option<&str>)> = ch2
            .iter()
            .map(|r| (r.locator.as_str(), s(r, "chapter_title")))
            .collect();
        assert_eq!(
            got,
            [
                ("ch2.1-s0", None),
                ("ch2.2-s0", Some("Chapter 1. Loomings")),
                ("ch2.3-s0", Some("Chapter 2. The Carpet-Bag")),
                ("ch2.4-s0", Some("Chapter 3. The Spouter-Inn")),
                // "CHAPTER 4" alone is only its heading: folded forward.
                ("ch2.5-s0", Some("CHAPTER 4 — The Counterpane.")),
            ]
        );
        assert_eq!(ch2[2].fields["part"], serde_json::json!(3));
        assert_eq!(ch2[2].fields["chapter"], serde_json::json!(2));
        let body = |i: usize| s(ch2[i], "body").unwrap();
        assert_eq!(body(0), "Front matter before any chapter.");
        assert!(body(1).starts_with("CHAPTER 1") && body(1).contains("Ishmael"));
        assert!(
            !body(1).contains("shirt"),
            "a part stops at the next anchor"
        );
        assert!(body(2).starts_with("CHAPTER 2") && body(2).contains("shirt"));
        assert!(body(3).contains("gable-ended") && !body(3).contains("shirt"));
        assert!(body(4).starts_with("CHAPTER 4") && body(4).contains("waking"));

        // A document cut into no parts keeps the plain locator and no `part`.
        let a = recs.iter().find(|r| r.locator.starts_with("ch3")).unwrap();
        assert_eq!(a.locator, "ch3-s0");
        assert!(a.fields.get("part").is_none());
    }

    #[test]
    fn percent_escapes_decode_and_malformed_ones_survive() {
        assert_eq!(percent_decode("Chapter%201.xhtml"), "Chapter 1.xhtml");
        assert_eq!(percent_decode("caf%C3%A9.xhtml"), "café.xhtml");
        assert_eq!(percent_decode("100%.xhtml"), "100%.xhtml");
        assert_eq!(percent_decode("a%zzb%4"), "a%zzb%4");
    }
}
