//! Plain text: prose files become one document record (title = first line,
//! body split into ~32KB sections); line-record files become one record per
//! line.

use super::{emit_document, ExtractStats, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

const TXT_CAP: u64 = 16 << 20;

pub fn extract_prose(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, TXT_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    let title = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            let t = l.trim();
            let mut s: String = t.chars().take(200).collect();
            if s.len() < t.len() {
                s.push('…');
            }
            s
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into())
        });
    emit_document(&title, &[], text.trim(), sink, &mut stats);
    Ok(stats)
}

pub fn extract_lines(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut r = super::open_reader(path, gzip, limit_bytes)?;
    let mut stats = ExtractStats::default();
    let mut offset = 0u64;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = super::jsonl::read_capped_line(&mut r, &mut buf)?;
        if n == 0 {
            break;
        }
        let start = offset;
        offset += n as u64;
        let (line, _) = crate::sniff::decode_text(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = Map::new();
        fields.insert("line".into(), Value::String(line.to_string()));
        stats.records += 1;
        if !sink(RawRecord {
            fields,
            locator: format!("b{start}"),
            group: None,
        }) {
            break;
        }
    }
    Ok(stats)
}
