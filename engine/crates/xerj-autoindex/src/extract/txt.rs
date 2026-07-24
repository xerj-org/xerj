//! Plain text: prose files become one document record (title = first line,
//! body split into ~32KB sections); line-oriented files become overlapping
//! line-window chunks.
//!
//! ## Why line-oriented text is chunked, not emitted per line
//!
//! `extract_lines` used to emit ONE record per line. That makes BM25 nearly
//! useless for anything but exact-substring lookup: relevance is scored per
//! document, so a single line can only match when it literally contains the
//! query terms. A question phrased in its own words — the normal case for an
//! agent — matches nothing, because no single line carries all of it.
//!
//! Measured on this repository (234 files, 170k LOC + docs + 460 commit
//! messages), answering 8 "where/why is X" questions:
//!
//!   one record per line ....... 3/8 answers found
//!   40-line windows, 10 overlap 8/8 answers found
//!
//! The window carries enough surrounding context that a natural-language
//! query matches the region, and the overlap stops an answer from being split
//! across a boundary. `start_line` / `end_line` travel with each chunk so a
//! caller can jump straight to the source — the old per-line record only
//! carried an opaque byte locator.

use super::{emit_document, ExtractStats, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

const TXT_CAP: u64 = 16 << 20;

/// Lines per chunk, and how many of them the next chunk repeats.  40/10 is the
/// configuration measured at 8/8 answer recall (see module docs); the overlap
/// is what keeps an answer that straddles a boundary retrievable from both
/// sides.
const CHUNK_LINES: usize = 40;
const CHUNK_OVERLAP: usize = 10;
/// Byte ceiling per chunk, so a file of very long lines cannot produce a
/// multi-megabyte record that would blow up `_source` on every hit.
const CHUNK_MAX_BYTES: usize = 8 << 10;

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

    // Sliding window over non-blank lines.  Each entry carries the line's text,
    // its 1-based line number, and the byte offset it started at (the offset of
    // the window's first line becomes the chunk locator, so ids stay stable
    // across re-runs exactly like the old per-line records did).
    let mut window: Vec<(String, usize, u64)> = Vec::with_capacity(CHUNK_LINES);
    let mut line_no = 0usize;
    let mut stopped = false;

    // Emit `window[..CHUNK_LINES]` as one record, then drop the non-overlapping
    // prefix.  Returns false when the sink asks us to stop.
    fn flush(
        window: &mut Vec<(String, usize, u64)>,
        drain: bool,
        stats: &mut ExtractStats,
        sink: Sink,
    ) -> bool {
        if window.is_empty() {
            return true;
        }
        let mut text = String::new();
        let mut used = 0usize;
        for (l, _, _) in window.iter() {
            if used > 0 && used + l.len() + 1 > CHUNK_MAX_BYTES {
                break;
            }
            if used > 0 {
                text.push('\n');
            }
            text.push_str(l);
            used += l.len() + 1;
        }
        if text.trim().is_empty() {
            return true;
        }
        let start_line = window[0].1;
        let start_byte = window[0].2;
        let end_line = window[window.len() - 1].1;

        let mut fields = Map::new();
        fields.insert("text".into(), Value::String(text));
        fields.insert(
            "start_line".into(),
            Value::Number((start_line as u64).into()),
        );
        fields.insert("end_line".into(), Value::Number((end_line as u64).into()));
        stats.records += 1;
        let ok = sink(RawRecord {
            fields,
            locator: format!("b{start_byte}"),
            group: None,
        });
        if drain {
            window.clear();
        } else {
            let keep = CHUNK_OVERLAP.min(window.len());
            window.drain(..window.len() - keep);
        }
        ok
    }

    loop {
        buf.clear();
        let n = super::jsonl::read_capped_line(&mut r, &mut buf)?;
        if n == 0 {
            break;
        }
        let start = offset;
        offset += n as u64;
        line_no += 1;
        let (line, _) = crate::sniff::decode_text(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        window.push((line.to_string(), line_no, start));
        if window.len() >= CHUNK_LINES && !flush(&mut window, false, &mut stats, sink) {
            stopped = true;
            break;
        }
    }
    // Trailing partial window (also the whole-file case for short files).
    if !stopped {
        flush(&mut window, true, &mut stats, sink);
    }
    Ok(stats)
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use std::io::Write;

    fn run(lines: &[&str]) -> Vec<(String, u64, u64, String)> {
        let dir = std::env::temp_dir().join(format!("xerj-chunk-{}", lines.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.txt");
        let mut f = std::fs::File::create(&p).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        drop(f);
        let mut out = Vec::new();
        {
            let mut sink = |r: RawRecord| {
                out.push((
                    r.fields["text"].as_str().unwrap().to_string(),
                    r.fields["start_line"].as_u64().unwrap(),
                    r.fields["end_line"].as_u64().unwrap(),
                    r.locator.clone(),
                ));
                true
            };
            extract_lines(&p, false, None, &mut sink).unwrap();
        }
        let _ = std::fs::remove_file(&p);
        out
    }

    #[test]
    fn short_file_is_one_chunk_covering_every_line() {
        let lines: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let out = run(&refs);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].1, out[0].2), (1, 10));
        assert!(out[0].0.contains("line 1") && out[0].0.contains("line 10"));
    }

    /// The property that fixed retrieval: consecutive chunks OVERLAP, so an
    /// answer straddling a boundary is findable from either side.
    #[test]
    fn consecutive_chunks_overlap_by_the_configured_amount() {
        let lines: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let out = run(&refs);
        assert!(out.len() >= 2, "100 lines must produce several chunks");
        for w in out.windows(2) {
            let (_, _s0, e0, _) = &w[0];
            let (_, s1, _, _) = &w[1];
            assert!(
                s1 <= e0,
                "chunk {s1} must start at/before previous end {e0}"
            );
            let repeated = e0 - s1 + 1;
            assert_eq!(
                repeated as usize, CHUNK_OVERLAP,
                "expected {CHUNK_OVERLAP} repeated lines across the boundary"
            );
        }
    }

    #[test]
    fn every_line_appears_in_some_chunk() {
        let lines: Vec<String> = (1..=95).map(|i| format!("unique{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let out = run(&refs);
        for i in 1..=95 {
            let tok = format!("unique{i}");
            assert!(
                out.iter()
                    .any(|(t, _, _, _)| t.split('\n').any(|l| l == tok)),
                "line {i} lost"
            );
        }
    }

    #[test]
    fn line_numbers_are_one_based_and_ordered() {
        let lines: Vec<String> = (1..=60).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let out = run(&refs);
        assert_eq!(out[0].1, 1, "first chunk starts at line 1, not 0");
        for (_, s, e, _) in &out {
            assert!(s <= e);
        }
    }

    /// A file of very long lines must not produce an unbounded record.
    #[test]
    fn chunk_is_byte_capped() {
        let long = "x".repeat(4000);
        let lines: Vec<&str> = (0..50).map(|_| long.as_str()).collect();
        let out = run(&lines);
        for (t, _, _, _) in &out {
            assert!(
                t.len() <= CHUNK_MAX_BYTES + 4001,
                "chunk of {} bytes exceeds the cap",
                t.len()
            );
        }
    }

    /// Blank lines are skipped but must not desynchronise the line numbering.
    #[test]
    fn blank_lines_do_not_break_numbering() {
        let out = run(&["alpha", "", "beta", "", "gamma"]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, 1);
        assert_eq!(
            out[0].2, 5,
            "end_line must be the real file line of `gamma`"
        );
    }
}
