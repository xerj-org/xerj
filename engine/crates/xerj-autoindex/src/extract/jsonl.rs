//! JSONL (newline-delimited JSON) — streaming, byte-offset locators.

use super::{flatten_object, ExtractStats, RawRecord, Sink, MAX_LINE};
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

pub fn extract(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut r = super::open_reader(path, gzip, limit_bytes)?;
    let mut stats = ExtractStats::default();
    let mut offset: u64 = 0;
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        let n = read_capped_line(&mut r, &mut line)?;
        if n == 0 {
            break;
        }
        let start = offset;
        offset += n as u64;
        let trimmed = trim_ws(&line);
        if trimmed.is_empty() {
            continue;
        }
        if line.len() >= MAX_LINE {
            stats.junk += 1;
            continue;
        }
        match serde_json::from_slice::<Value>(trimmed) {
            Ok(Value::Object(m)) => {
                stats.records += 1;
                if !sink(RawRecord {
                    fields: flatten_object(m),
                    locator: format!("b{start}"),
                    group: None,
                }) {
                    break;
                }
            }
            Ok(_) | Err(_) => {
                // Truncated tail line under a sampling limit isn't junk.
                if limit_bytes.is_some() && offset >= limit_bytes.unwrap_or(0) {
                    break;
                }
                stats.junk += 1;
            }
        }
    }
    Ok(stats)
}

/// read_until('\n') with a hard cap so a pathological line can't balloon RAM.
pub fn read_capped_line(r: &mut dyn std::io::BufRead, out: &mut Vec<u8>) -> Result<usize> {
    let mut total = 0usize;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(total);
        }
        match memchr::memchr(b'\n', buf) {
            Some(i) => {
                if out.len() < MAX_LINE {
                    out.extend_from_slice(&buf[..=i.min(MAX_LINE - 1)]);
                }
                r.consume(i + 1);
                return Ok(total + i + 1);
            }
            None => {
                let n = buf.len();
                if out.len() < MAX_LINE {
                    let take = n.min(MAX_LINE - out.len());
                    out.extend_from_slice(&buf[..take]);
                }
                r.consume(n);
                total += n;
            }
        }
    }
}

fn trim_ws(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(start);
    &b[start..end]
}
