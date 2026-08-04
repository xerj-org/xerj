//! JSONL (newline-delimited JSON) — streaming, byte-offset locators.

use super::{flatten_object, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_LINE};
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
                    origin: FieldOrigin::Data,
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
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(start);
    &b[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(bytes: &[u8], limit: Option<u64>) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, bytes).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, false, limit, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    fn locators(recs: &[RawRecord]) -> Vec<&str> {
        recs.iter().map(|r| r.locator.as_str()).collect()
    }

    #[test]
    fn every_line_is_one_record_located_at_the_byte_offset_it_starts_at() {
        let (stats, recs) = run(b"{\"id\":1,\"m\":{\"k\":\"v\"}}\n{\"id\":2}\n", None);
        assert_eq!(stats.records, 2);
        assert_eq!(stats.junk, 0);
        assert_eq!(
            locators(&recs),
            ["b0", "b23"],
            "the second line starts after the 23 bytes of the first"
        );
        assert_eq!(recs[0].fields["id"], serde_json::json!(1));
        assert_eq!(
            recs[0].fields["m_k"],
            serde_json::json!("v"),
            "nested objects flatten exactly as in whole-file JSON"
        );
    }

    #[test]
    fn blank_lines_are_skipped_without_being_counted_as_junk() {
        let (stats, recs) = run(b"{\"id\":1}\n\n   \n{\"id\":2}\n", None);
        assert_eq!((stats.records, stats.junk), (2, 0));
        assert_eq!(locators(&recs), ["b0", "b14"]);
    }

    #[test]
    fn a_bad_line_is_junk_and_never_stops_the_lines_after_it() {
        let (stats, recs) = run(b"{\"id\":1}\n[1,2]\n{oops\n{\"id\":3}\n", None);
        assert_eq!(stats.records, 2);
        assert_eq!(
            stats.junk, 2,
            "a valid non-object line is junk just like a parse failure"
        );
        assert_eq!(recs[1].fields["id"], serde_json::json!(3));
    }

    /// The distinction from `json::extract`: JSONL is line-framed, so a
    /// pretty-printed document is not one record — it is one junk line per
    /// physical line.
    #[test]
    fn a_pretty_printed_object_is_not_a_jsonl_stream() {
        let (stats, recs) = run(b"{\n  \"id\": 1\n}\n", None);
        assert_eq!(stats.records, 0);
        assert_eq!(stats.junk, 3);
        assert!(recs.is_empty());
    }

    /// Sampling stops mid-file: the last line arrives cut in half. That is a
    /// truncation, not corrupt data, so it must not inflate the junk count the
    /// catalog reports for the file.
    #[test]
    fn a_line_cut_short_by_a_sampling_limit_is_not_counted_as_junk() {
        let data = b"{\"id\":1}\n{\"id\":2}\n{\"id\":3}\n";
        let (stats, recs) = run(data, Some(14));
        assert_eq!(stats.records, 1);
        assert_eq!(stats.junk, 0);
        assert_eq!(locators(&recs), ["b0"]);

        let (stats, _) = run(data, Some(27));
        assert_eq!((stats.records, stats.junk), (3, 0), "the full stream fits");
    }

    #[test]
    fn carriage_returns_are_trimmed_but_still_counted_in_the_offset() {
        let (stats, recs) = run(b"{\"id\":1}\r\n{\"id\":2}\r\n", None);
        assert_eq!((stats.records, stats.junk), (2, 0));
        assert_eq!(locators(&recs), ["b0", "b10"]);
    }

    #[test]
    fn a_line_over_the_cap_is_junk_rather_than_a_giant_record() {
        let mut data = vec![b'{'];
        data.resize(MAX_LINE + 1, b' ');
        data.extend_from_slice(b"}\n{\"id\":2}\n");
        let (stats, recs) = run(&data, None);
        assert_eq!(stats.junk, 1);
        assert_eq!(stats.records, 1, "the stream continues after the cap");
        assert_eq!(recs[0].fields["id"], serde_json::json!(2));
    }
}
