//! PDF text extraction — a self-contained content-stream scanner (no
//! external PDF crate: the workspace release profile is panic=abort, so a
//! panicky dependency would take the whole process down; this scanner is
//! Result-only by construction).
//!
//! Strategy: locate `stream…endstream` ranges, inflate FlateDecode bodies
//! (or use raw bodies that already look like content streams), then scan for
//! text-showing operators: `(…) Tj`, `(…) '`, `[…] TJ`, with PDF string
//! escapes and hex strings. Newlines derive from Td/TD/T*/ET ops. `/Title`
//! is pulled from the document info dictionary when present.

use super::{emit_document, ExtractStats, Sink};
use anyhow::Result;
use std::io::Read;
use std::path::Path;

const PDF_CAP: u64 = 512 << 20;
const STREAM_INFLATE_CAP: u64 = 64 << 20;

pub fn extract(path: &Path, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let size = std::fs::metadata(path)?.len();
    if size > PDF_CAP {
        stats.junk += 1;
        return Ok(stats);
    }
    let bytes = std::fs::read(path)?;
    let mut text = String::new();
    for body in stream_bodies(&bytes) {
        let content: Vec<u8> = match inflate(body) {
            Some(d) => d,
            None => {
                if looks_like_content(body) {
                    body.to_vec()
                } else {
                    continue;
                }
            }
        };
        if !looks_like_content(&content) {
            continue;
        }
        scan_text_ops(&content, &mut text);
    }
    let title = doc_title(&bytes).unwrap_or_else(|| {
        text.lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().chars().take(200).collect())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled".into())
            })
    });
    let body = text.trim();
    if body.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }
    emit_document(&title, &[], body, sink, &mut stats);
    Ok(stats)
}

fn stream_bodies(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(p) = find(bytes, i, b"stream") {
        // must be the keyword, not part of "endstream"
        let word_ok = p == 0 || !bytes[p - 1].is_ascii_alphanumeric();
        let mut start = p + b"stream".len();
        if start < bytes.len() && bytes[start] == b'\r' {
            start += 1;
        }
        if start < bytes.len() && bytes[start] == b'\n' {
            start += 1;
        }
        match find(bytes, start, b"endstream") {
            Some(e) if word_ok => {
                out.push(&bytes[start..e]);
                i = e + b"endstream".len();
            }
            Some(e) => {
                i = e + b"endstream".len();
            }
            None => break,
        }
    }
    out
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    memchr::memmem::find(&hay[from..], needle).map(|p| p + from)
}

fn inflate(body: &[u8]) -> Option<Vec<u8>> {
    let mut d = flate2::read::ZlibDecoder::new(body).take(STREAM_INFLATE_CAP);
    let mut out = Vec::new();
    d.read_to_end(&mut out).ok()?;
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn looks_like_content(b: &[u8]) -> bool {
    let head = &b[..b.len().min(4096)];
    memchr::memmem::find(head, b"BT").is_some()
        || memchr::memmem::find(head, b"Tj").is_some()
        || memchr::memmem::find(head, b"TJ").is_some()
}

/// PDFDocEncoding ≈ latin-1 for the printable range — good enough for
/// generated PDFs; unknown bytes are passed through as latin-1.
fn scan_text_ops(content: &[u8], out: &mut String) {
    let mut i = 0usize;
    let mut pending: Vec<String> = Vec::new(); // strings awaiting an operator
    let mut line = String::new();
    let flush_line = |line: &mut String, out: &mut String| {
        let t = line.trim();
        if !t.is_empty() {
            out.push_str(t);
            out.push('\n');
        }
        line.clear();
    };
    while i < content.len() {
        match content[i] {
            b'(' => {
                let (s, ni) = parse_literal_string(content, i);
                pending.push(s);
                i = ni;
            }
            b'<' if i + 1 < content.len() && content[i + 1] != b'<' => {
                let (s, ni) = parse_hex_string(content, i);
                pending.push(s);
                i = ni;
            }
            b'[' => {
                // TJ array: collect strings until ]
                let mut j = i + 1;
                let mut parts: Vec<String> = Vec::new();
                while j < content.len() && content[j] != b']' {
                    match content[j] {
                        b'(' => {
                            let (s, nj) = parse_literal_string(content, j);
                            parts.push(s);
                            j = nj;
                        }
                        b'<' => {
                            let (s, nj) = parse_hex_string(content, j);
                            parts.push(s);
                            j = nj;
                        }
                        _ => j += 1,
                    }
                }
                pending.push(parts.join(""));
                i = j + 1;
            }
            b'%' => {
                // comment to EOL
                i = memchr::memchr(b'\n', &content[i..])
                    .map(|p| i + p + 1)
                    .unwrap_or(content.len());
            }
            c if c.is_ascii_alphabetic() || c == b'\'' || c == b'"' || c == b'*' => {
                let start = i;
                while i < content.len()
                    && (content[i].is_ascii_alphanumeric()
                        || matches!(content[i], b'\'' | b'"' | b'*'))
                {
                    i += 1;
                }
                let op = &content[start..i];
                match op {
                    b"Tj" | b"TJ" | b"'" | b"\"" => {
                        for s in pending.drain(..) {
                            if !line.is_empty() && !line.ends_with(' ') {
                                line.push(' ');
                            }
                            line.push_str(&s);
                        }
                        if op == b"'" || op == b"\"" {
                            flush_line(&mut line, out);
                        }
                    }
                    b"Td" | b"TD" | b"T*" | b"ET" => {
                        pending.clear();
                        flush_line(&mut line, out);
                    }
                    b"BT" => {
                        pending.clear();
                    }
                    _ => {
                        pending.clear();
                    }
                }
            }
            _ => i += 1,
        }
        if out.len() > 32 << 20 {
            break; // hard safety cap
        }
    }
    flush_line(&mut line, out);
}

fn parse_literal_string(b: &[u8], open: usize) -> (String, usize) {
    let mut s = String::new();
    let mut i = open + 1;
    let mut depth = 1usize;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                let c = b[i + 1];
                match c {
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    b'(' => s.push('('),
                    b')' => s.push(')'),
                    b'\\' => s.push('\\'),
                    b'0'..=b'7' => {
                        // up to 3 octal digits
                        let mut v = 0u32;
                        let mut n = 0;
                        while n < 3 && i + 1 + n < b.len() && (b'0'..=b'7').contains(&b[i + 1 + n])
                        {
                            v = v * 8 + (b[i + 1 + n] - b'0') as u32;
                            n += 1;
                        }
                        if let Some(ch) = char::from_u32(v) {
                            s.push(ch);
                        }
                        i += n + 1;
                        continue;
                    }
                    b'\n' => {} // line continuation
                    other => s.push(other as char),
                }
                i += 2;
            }
            b'(' => {
                depth += 1;
                s.push('(');
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (s, i + 1);
                }
                s.push(')');
                i += 1;
            }
            c => {
                s.push(c as char); // latin-1 passthrough
                i += 1;
            }
        }
        if s.len() > 1 << 20 {
            break;
        }
    }
    (s, i)
}

fn parse_hex_string(b: &[u8], open: usize) -> (String, usize) {
    let mut i = open + 1;
    let mut nibbles: Vec<u8> = Vec::new();
    while i < b.len() && b[i] != b'>' {
        let c = b[i];
        if c.is_ascii_hexdigit() {
            nibbles.push(c);
        }
        i += 1;
        if nibbles.len() > 1 << 20 {
            break;
        }
    }
    if nibbles.len() % 2 == 1 {
        nibbles.push(b'0');
    }
    let mut s = String::new();
    for pair in nibbles.chunks(2) {
        let hi = (pair[0] as char).to_digit(16).unwrap_or(0);
        let lo = (pair[1] as char).to_digit(16).unwrap_or(0);
        if let Some(ch) = char::from_u32(hi * 16 + lo) {
            s.push(ch);
        }
    }
    (s, i + 1)
}

fn doc_title(bytes: &[u8]) -> Option<String> {
    let p = memchr::memmem::find(bytes, b"/Title")?;
    let after = &bytes[p + 6..(p + 4096).min(bytes.len())];
    let start = after.iter().position(|&c| !c.is_ascii_whitespace())?;
    match after[start] {
        b'(' => {
            let (s, _) = parse_literal_string(after, start);
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        b'<' => {
            let (s, _) = parse_hex_string(after, start);
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        _ => None,
    }
}
