//! Plain text: prose files become one document record (title = first line, or
//! a frontmatter `title:`; body split into ~32KB sections); line-oriented
//! files become overlapping line-window chunks.
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

use super::{
    emit_document_extra, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_RECORDS_PER_FILE,
};
use anyhow::Result;
use serde_json::{Map, Value};
use std::io::Read;
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
/// Largest frontmatter block `split_frontmatter` will hand to the YAML parser.
/// The sniffer only routes frontmatter here when it closes inside its 8 KiB
/// prefix, so this bounds the parse locally rather than by that invariant.
const FRONTMATTER_MAX_BYTES: usize = 64 << 10;

/// `skill`: the logical file is an agent skill's `SKILL.md` ([`is_skill_file`]).
pub fn extract_prose(path: &Path, gzip: bool, skill: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, TXT_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    let card = if skill { skill_card(&text) } else { None };
    let title = card
        .as_ref()
        .and_then(|c| c.name.clone())
        .or_else(|| prose_title(&text).map(|t| cap_title(&t)))
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into())
        });
    // #1941: a prose section's record must carry the file line it starts on,
    // or a hit has no line to cite (`guide.md:1` for a passage at line 245).
    // `body` below is `text.trim()`, so shift by the newlines the trim removed.
    let leading = &text[..text.len() - text.trim_start().len()];
    let base_line = 1 + leading.bytes().filter(|b| *b == b'\n').count() as u64;
    emit_document_extra(
        &title,
        &[],
        &card.map(|c| c.fields()).unwrap_or_default(),
        text.trim(),
        Some(base_line),
        MAX_RECORDS_PER_FILE,
        sink,
        &mut stats,
    );
    Ok(stats)
}

/// The title of a prose document: the first non-blank line, unless the text
/// opens with a frontmatter block.
///
/// #2556: the sniffer routes frontmatter over a prose body here (#551), so for
/// every Jekyll/Hugo/Obsidian page the first line is the opening `---` and
/// every page was titled `---`. A frontmatter `title:` wins; without one, the
/// first line after the block. A bare `---` rule is never the title, even when
/// the leading block is not frontmatter we can parse. `None` when there is no
/// line to use.
fn prose_title(text: &str) -> Option<String> {
    let mut rest = text;
    if let Some((title, after)) = split_frontmatter(text) {
        if title.is_some() {
            return title;
        }
        rest = after;
    }
    rest.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != "---")
        .map(str::to_string)
}

/// A prose title, at most 200 characters, with `…` when it was cut.
fn cap_title(t: &str) -> String {
    cap_chars(t, 200)
}

/// A SKILL.md `description` is stamped on every record of its file, so it is
/// bounded too: 1024 characters, the Agent Skills format's own limit.
const DESCRIPTION_MAX_CHARS: usize = 1024;

/// `t`, at most `max` characters, with `…` when it was cut.
fn cap_chars(t: &str, max: usize) -> String {
    let mut s: String = t.chars().take(max).collect();
    if s.len() < t.len() {
        s.push('…');
    }
    s
}

/// Splits a leading frontmatter block off `text`: its `title:` (whitespace
/// collapsed, `None` when absent or blank) and the text after the closing
/// fence. See [`frontmatter`] for what counts as a block.
fn split_frontmatter(text: &str) -> Option<(Option<String>, &str)> {
    let (map, rest) = frontmatter(text)?;
    Some((frontmatter_line(&map, "title"), rest))
}

/// Splits a leading frontmatter block off `text`: its YAML mapping and the
/// text after the closing fence. The block must parse as a YAML mapping, so
/// prose between two `---` rules is not frontmatter.
fn frontmatter(text: &str) -> Option<(serde_yaml::Mapping, &str)> {
    let (block, rest) = frontmatter_block(text)?;
    let serde_yaml::Value::Mapping(map) = serde_yaml::from_str(block).ok()? else {
        return None;
    };
    Some((map, rest))
}

/// Splits a leading fenced block off `text`, unparsed: the text between the
/// fences and the text after the closing one. The fences follow the sniffer's
/// #551 lookahead (first non-blank line `---`, next `---` line closes it); the
/// block must be at most `FRONTMATTER_MAX_BYTES`.
fn frontmatter_block(text: &str) -> Option<(&str, &str)> {
    let mut lines = text.split_inclusive('\n');
    let mut offset = 0;
    loop {
        let line = lines.next()?;
        offset += line.len();
        if !line.trim().is_empty() {
            if line.trim() != "---" {
                return None;
            }
            break;
        }
    }
    let block_start = offset;
    for line in lines {
        let line_start = offset;
        if line_start - block_start > FRONTMATTER_MAX_BYTES {
            return None;
        }
        offset += line.len();
        if line.trim() == "---" {
            return Some((&text[block_start..line_start], &text[offset..]));
        }
    }
    None
}

/// A frontmatter key as one line of text: a string, number or bool (`title:
/// 1984` is a YAML number, still a title), whitespace collapsed. `None` when
/// the key is absent, not a scalar, or blank.
fn frontmatter_line(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|v| match v {
            serde_yaml::Value::String(s) => Some(s.clone()),
            serde_yaml::Value::Number(n) => Some(n.to_string()),
            serde_yaml::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        })
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
}

/// Whether `logical` is an agent skill's definition file. Skill loaders read a
/// file named exactly `SKILL.md`; the name is the logical one, never the
/// content path, which can be a snapshot blob (#294).
pub(crate) fn is_skill_file(logical: &Path) -> bool {
    logical.file_name().is_some_and(|n| n == "SKILL.md")
}

/// #2644: what a SKILL.md's frontmatter says the skill is. An agent with
/// hundreds of skills finds one by what it is for, and two keys already say
/// that: `name` and `description`. Without them as fields, a prose SKILL.md was
/// titled by the first line after its frontmatter and a line-window one had no
/// title at all.
struct SkillCard {
    name: Option<String>,
    description: Option<String>,
}

impl SkillCard {
    /// The fields every record of the file carries: `title` from `name`, and
    /// `description`. A key the frontmatter does not have adds nothing.
    fn fields(self) -> Map<String, Value> {
        let mut fields = Map::new();
        if let Some(name) = self.name {
            fields.insert("title".into(), Value::String(name));
        }
        if let Some(description) = self.description {
            fields.insert("description".into(), Value::String(description));
        }
        fields
    }
}

/// The skill card in `text`'s frontmatter. `None` when there is no
/// frontmatter, or it has neither key as a non-blank scalar.
///
/// A block strict YAML refuses is still a skill's frontmatter to the loaders
/// that read it: an `argument-hint: [path] [--dry-run]` line, which 3 of 78
/// skills in one real library carry, is two flow sequences on one line and a
/// YAML syntax error. For such a block the two keys are read from their own
/// lines ([`loose_line`]). A leading byte-order mark, which a Windows editor
/// may write, is skipped here only, so no other file's title changes.
fn skill_card(text: &str) -> Option<SkillCard> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (block, _) = frontmatter_block(text)?;
    let card = match serde_yaml::from_str(block) {
        Ok(serde_yaml::Value::Mapping(map)) => SkillCard {
            name: frontmatter_line(&map, "name"),
            description: frontmatter_line(&map, "description"),
        },
        Ok(_) => return None,
        Err(_) => SkillCard {
            name: loose_line(block, "name"),
            description: loose_line(block, "description"),
        },
    };
    let card = SkillCard {
        name: card.name.map(|n| cap_title(&n)),
        description: card
            .description
            .map(|d| cap_chars(&d, DESCRIPTION_MAX_CHARS)),
    };
    (card.name.is_some() || card.description.is_some()).then_some(card)
}

/// `key`'s value in a frontmatter block YAML refused, when it is written on
/// one top-level line: plain, or wrapped in one pair of quotes. A block
/// scalar (`>`, `|`), a flow collection, an anchor or a tag is not guessed at,
/// and neither is a quoted value with an escape in it, nor one continued on
/// an indented line. Whitespace collapsed; `None` when blank.
fn loose_line(block: &str, key: &str) -> Option<String> {
    let mut lines = block.lines();
    let value = loop {
        let line = lines.next()?;
        if let Some(v) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
            break v.trim();
        }
    };
    if lines
        .next()
        .is_some_and(|n| n.starts_with([' ', '\t']) && !n.trim().is_empty())
    {
        return None;
    }
    let value = if let Some(q) = value.strip_prefix('\'') {
        q.strip_suffix('\'')?.replace("''", "'")
    } else if let Some(q) = value.strip_prefix('"') {
        let q = q.strip_suffix('"')?;
        if q.contains('\\') {
            return None;
        }
        q.to_string()
    } else {
        if value.starts_with(['>', '|', '[', '{', '&', '*', '!', '#']) {
            return None;
        }
        value.split(" #").next().unwrap_or_default().to_string()
    };
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

/// The skill card of a SKILL.md read as line windows. The windows stream, so
/// the frontmatter is read first from the head of the file: as much as the
/// longest block [`frontmatter`] accepts, plus room for the fences. Each line
/// is decoded on its own, as the windows decode theirs, so one Latin-1 byte
/// further down cannot re-decode the frontmatter as Windows-1252. The head
/// is read whole even when `limit_bytes` bounds a sampling pass.
fn skill_card_from_head(path: &Path, gzip: bool) -> Result<Option<SkillCard>> {
    let mut head = Vec::new();
    super::open_reader(path, gzip, Some(FRONTMATTER_MAX_BYTES as u64 + 8192))?
        .read_to_end(&mut head)?;
    let text: String = head
        .split_inclusive(|b| *b == b'\n')
        .map(|line| crate::sniff::decode_text(line).0)
        .collect();
    Ok(skill_card(&text))
}

/// `skill`: the logical file is an agent skill's `SKILL.md` ([`is_skill_file`]);
/// every window then carries its [`SkillCard`] fields.
pub fn extract_lines(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    skill: bool,
    sink: Sink,
) -> Result<ExtractStats> {
    let extra = if skill {
        skill_card_from_head(path, gzip)?
            .map(SkillCard::fields)
            .unwrap_or_default()
    } else {
        Map::new()
    };
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

    // Emit the window's leading lines as one record, up to `CHUNK_MAX_BYTES`,
    // then drop the non-overlapping prefix of what was emitted.  `drain`
    // (end of file) keeps emitting until every buffered line is in a record.
    // Returns false when the sink asks us to stop.
    //
    // Only lines that made it into the record may be dropped: when the byte
    // cap cuts the window short, the lines past the cut stay buffered and lead
    // the next record.  Dropping `window.len() - CHUNK_OVERLAP` lines
    // regardless lost every line between the cut and the overlap tail — one
    // long line among short ones made up to 29 neighbours unsearchable — and
    // reported an `end_line` the record's text did not reach.
    fn flush(
        window: &mut Vec<(String, usize, u64)>,
        extra: &Map<String, Value>,
        drain: bool,
        stats: &mut ExtractStats,
        sink: Sink,
    ) -> bool {
        loop {
            if window.is_empty() {
                return true;
            }
            let mut text = String::new();
            let mut used = 0usize;
            let mut taken = 0usize;
            for (l, _, _) in window.iter() {
                if used > 0 && used + l.len() + 1 > CHUNK_MAX_BYTES {
                    break;
                }
                if used > 0 {
                    text.push('\n');
                }
                text.push_str(l);
                used += l.len() + 1;
                taken += 1;
            }
            let start_line = window[0].1;
            let start_byte = window[0].2;
            let end_line = window[taken - 1].1;

            let mut ok = true;
            if !text.trim().is_empty() {
                let mut fields = extra.clone();
                fields.insert("text".into(), Value::String(text));
                fields.insert(
                    "start_line".into(),
                    Value::Number((start_line as u64).into()),
                );
                fields.insert("end_line".into(), Value::Number((end_line as u64).into()));
                stats.records += 1;
                ok = sink(RawRecord {
                    fields,
                    locator: format!("b{start_byte}"),
                    group: None,
                    // text/start_line/end_line describe this chunker, not the file.
                    origin: FieldOrigin::Extractor,
                });
            }
            if taken == window.len() {
                if drain {
                    window.clear();
                } else {
                    let keep = CHUNK_OVERLAP.min(window.len());
                    window.drain(..window.len() - keep);
                }
                return ok;
            }
            // Cut short by the byte cap: keep the overlap tail of what was
            // emitted plus everything after it.  At least one line is always
            // dropped, so a run of oversized lines still advances.
            let keep = CHUNK_OVERLAP.min(taken - 1);
            window.drain(..taken - keep);
            if !drain || !ok {
                return ok;
            }
        }
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
        if window.len() >= CHUNK_LINES && !flush(&mut window, &extra, false, &mut stats, sink) {
            stopped = true;
            break;
        }
    }
    // Trailing partial window (also the whole-file case for short files).
    if !stopped {
        flush(&mut window, &extra, true, &mut stats, sink);
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
            extract_lines(&p, false, None, false, &mut sink).unwrap();
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

    /// FAIL-BEFORE: the first record held lines 1-2 (the byte cap stopped it
    /// at the 4000-byte line 3), then 30 lines were dropped from the window, so
    /// lines 3-30 appeared in no record, and that record claimed `end_line` 40.
    #[test]
    fn byte_cap_cut_loses_no_line_and_reports_its_real_end() {
        let long = format!("long {}", "y".repeat(4000));
        let mut lines: Vec<String> = (1..=77).map(|i| format!("line{i}")).collect();
        for i in [2usize, 3, 4, 41, 42] {
            lines[i] = format!("{long} {i}");
        }
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let out = run(&refs);
        for (i, line) in lines.iter().enumerate() {
            assert!(
                out.iter()
                    .any(|(t, _, _, _)| t.split('\n').any(|l| l == line)),
                "line {} lost",
                i + 1
            );
        }
        for (t, s, e, _) in &out {
            let got = t.split('\n').count() as u64;
            assert_eq!(e - s + 1, got, "record {s}..{e} holds {got} lines");
            assert!(
                t.split('\n').next_back() == Some(lines[*e as usize - 1].as_str()),
                "end_line {e} must be the record's last line"
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

    /// #1941: a prose section carries the FILE line it starts on, so a hit can
    /// cite `guide.md:4`, not the record-relative `:1`. The source is the
    /// oracle — a section can start mid-line after a hard cut.
    #[test]
    fn prose_sections_carry_their_file_start_line() {
        let paragraph = "word ".repeat(700); // 3500 chars: forces section cuts
        let text = format!("\n\n\n{paragraph}\n\n{paragraph}\n\n{paragraph}\n");
        let dir = std::env::temp_dir().join(format!("xerj-prose-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("guide.md");
        std::fs::write(&p, &text).unwrap();
        let mut got: Vec<(u64, String)> = Vec::new();
        {
            let mut sink = |r: RawRecord| {
                got.push((
                    r.fields["start_line"]
                        .as_u64()
                        .expect("every prose section carries start_line"),
                    r.fields["body"].as_str().unwrap().to_string(),
                ));
                true
            };
            extract_prose(&p, false, false, &mut sink).unwrap();
        }
        let _ = std::fs::remove_file(&p);
        let lines: Vec<&str> = text.lines().collect();
        assert!(got.len() > 1, "3500-char paragraphs must split");
        assert_eq!(
            got[0].0, 4,
            "three leading blank lines put the body on line 4"
        );
        for (line, body) in &got {
            let first = body.lines().next().unwrap_or("");
            let source = lines
                .get(*line as usize - 1)
                .unwrap_or_else(|| panic!("start_line {line} is past the file"));
            assert!(
                source.ends_with(first),
                "start_line {line} does not hold the section start: {first:?} vs {source:?}"
            );
        }
    }

    /// Runs `extract_prose` over `text` and returns each record's
    /// (title, start_line, body).
    fn prose(name: &str, text: &str) -> Vec<(String, u64, String)> {
        let dir = std::env::temp_dir().join(format!("xerj-prose-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        let mut got = Vec::new();
        {
            let mut sink = |r: RawRecord| {
                got.push((
                    r.fields["title"].as_str().unwrap().to_string(),
                    r.fields["start_line"].as_u64().unwrap(),
                    r.fields["body"].as_str().unwrap().to_string(),
                ));
                true
            };
            extract_prose(&p, false, false, &mut sink).unwrap();
        }
        let _ = std::fs::remove_file(&p);
        got
    }

    /// #2644: [`loose_line`] reads only what a one-line YAML scalar would
    /// say. A nested key, a block scalar, a flow value and an escaped quote
    /// are refused rather than read wrong.
    #[test]
    fn loose_line_reads_only_one_line_scalars() {
        let block = "name: pdf # the short name\nlicense: x\n  description: nested\ndescription: \"Fill \\\"forms\\\".\"\nargument-hint: [a] [b]\n";
        assert_eq!(loose_line(block, "name").as_deref(), Some("pdf"));
        assert_eq!(loose_line(block, "description"), None);
        assert_eq!(
            loose_line("description: >-\n  Folded.\n", "description"),
            None
        );
        assert_eq!(loose_line("name: [a]\n", "name"), None);
        assert_eq!(
            loose_line("name: 'it''s   here'\n", "name").as_deref(),
            Some("it's here")
        );
        assert_eq!(loose_line("names: x\n", "name"), None);
        assert_eq!(loose_line("name:\n", "name"), None);
        assert_eq!(
            loose_line(
                "description: Fill PDF forms\n  and merge them.\n",
                "description"
            ),
            None,
            "a value continued on an indented line is not cut short"
        );
    }

    /// Review of #2644: a byte-order mark does not hide a skill's frontmatter,
    /// and a description is bounded like a title.
    #[test]
    fn skill_card_skips_a_bom_and_caps_the_description() {
        let long = "d".repeat(DESCRIPTION_MAX_CHARS + 50);
        let card = skill_card(&format!(
            "\u{feff}---\nname: pdf\ndescription: {long}\n---\nbody\n"
        ))
        .expect("a card");
        assert_eq!(card.name.as_deref(), Some("pdf"));
        let d = card.description.unwrap();
        assert_eq!(d.chars().count(), DESCRIPTION_MAX_CHARS + 1);
        assert!(d.ends_with('…'));
    }

    /// Review of #2644: one Latin-1 byte deep in the body does not re-decode a
    /// UTF-8 frontmatter as Windows-1252 (`café` read back as `cafÃ©`).
    #[test]
    fn a_latin1_byte_in_the_body_leaves_the_head_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("SKILL.md");
        let mut bytes = "---\nname: café\ndescription: Ünïcode\n---\n"
            .as_bytes()
            .to_vec();
        for _ in 0..500 {
            bytes.extend_from_slice(b"some body line of text here\n");
        }
        bytes.extend_from_slice(b"caf\xe9 latin-1\n");
        std::fs::write(&p, &bytes).unwrap();
        let card = skill_card_from_head(&p, false).unwrap().expect("a card");
        assert_eq!(card.name.as_deref(), Some("café"));
        assert_eq!(card.description.as_deref(), Some("Ünïcode"));
    }

    /// #2556: the sniffer routes frontmatter over a prose body here (#551), so
    /// the title must come from the frontmatter, not its opening `---` fence.
    /// The body and its start line are unchanged: the frontmatter stays
    /// searchable.
    #[test]
    fn frontmatter_title_key_titles_the_document() {
        let text = "---\ntitle: Deploy runbook\ndate: 2026-10-01\n---\n\n# Deploy runbook\n\nRoll the canary first.\n";
        let got = prose("deploy.md", text);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "Deploy runbook");
        assert_eq!(got[0].1, 1, "the body still starts at the fence");
        assert_eq!(got[0].2, text.trim(), "the frontmatter stays in the body");
    }

    #[test]
    fn quoted_frontmatter_title_is_unquoted() {
        let got = prose(
            "q.md",
            "---\ntitle: \"Deploy: the runbook\"\n---\nRoll the canary first.\n",
        );
        assert_eq!(got[0].0, "Deploy: the runbook");
        let got = prose(
            "s.md",
            "---\ntitle: 'It''s live'\n---\nRoll the canary first.\n",
        );
        assert_eq!(got[0].0, "It's live");
        let got = prose(
            "f.md",
            "---\ntitle: >\n  Deploy\n  runbook\n---\nRoll the canary first.\n",
        );
        assert_eq!(got[0].0, "Deploy runbook", "a folded title is one line");
    }

    /// No `title:` key: the first line after the closing fence, as a file
    /// without frontmatter would get.
    #[test]
    fn frontmatter_without_a_title_uses_the_first_body_line() {
        let got = prose(
            "n.md",
            "\n---\ndate: 2026-10-01\ntags: [ops]\n---\n\nRoll the canary first.\nThen promote.\n",
        );
        assert_eq!(got[0].0, "Roll the canary first.");
        let got = prose("e.md", "---\ntitle: \"\"\n---\nRoll the canary first.\n");
        assert_eq!(
            got[0].0, "Roll the canary first.",
            "an empty title is no title"
        );
    }

    #[test]
    fn crlf_frontmatter_and_a_number_title() {
        let got = prose(
            "c.md",
            "---\r\ntitle: Deploy runbook\r\n---\r\nRoll the canary first.\r\n",
        );
        assert_eq!(got[0].0, "Deploy runbook");
        let got = prose("y.md", "---\ntitle: 1984\n---\nRoll the canary first.\n");
        assert_eq!(got[0].0, "1984", "a YAML number is still a title");
    }

    /// Only a closed, bounded block that parses as a YAML mapping is
    /// frontmatter. Anything else (an unclosed fence, prose between two `---`
    /// rules, YAML the parser refuses, an oversized block) takes the first
    /// line that is not a bare `---` rule.
    #[test]
    fn a_leading_rule_that_is_not_frontmatter_never_titles_the_page() {
        let got = prose("u.md", "---\ntitle: never closed\nRoll the canary first.\n");
        assert_eq!(got[0].0, "title: never closed");
        let got = prose(
            "r.md",
            "---\nRoll the canary first, then promote.\n---\nCheck the dashboards.\n",
        );
        assert_eq!(got[0].0, "Roll the canary first, then promote.");
        let got = prose(
            "d.md",
            "---\ntitle: First\ntitle: Second\n---\nRoll the canary first.\n",
        );
        assert_eq!(got[0].0, "title: First", "duplicate keys do not parse");
        let pad = "x".repeat(FRONTMATTER_MAX_BYTES);
        let got = prose(
            "big.md",
            &format!("---\ntitle: Big\npad: {pad}\n---\nRoll the canary first.\n"),
        );
        assert_eq!(got[0].0, "title: Big", "an oversized block is not parsed");
    }

    /// The route the fix depends on: a Jekyll-shaped page sniffs as prose
    /// (#551) and then takes its frontmatter title.
    #[test]
    fn a_sniffed_frontmatter_page_takes_its_frontmatter_title() {
        let dir = std::env::temp_dir().join(format!("xerj-prose-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("deploy.md");
        std::fs::write(
            &p,
            "---\ntitle: Deploy runbook\ndate: 2026-10-01\n---\n\n# Deploy runbook\n\nRoll the canary first, then promote the release.\n",
        )
        .unwrap();
        let family = crate::sniff::sniff(&p).unwrap().family;
        assert_eq!(family, crate::sniff::Family::TxtProse);
        let mut titles = Vec::new();
        {
            let mut sink = |r: RawRecord| {
                titles.push(r.fields["title"].as_str().unwrap().to_string());
                true
            };
            extract_prose(&p, false, false, &mut sink).unwrap();
        }
        let _ = std::fs::remove_file(&p);
        assert_eq!(titles, ["Deploy runbook"]);
    }

    #[test]
    fn short_markdown_frontmatter_keeps_its_title_and_complete_body() {
        let dir = tempfile::tempdir().unwrap();
        for extension in [
            "md", "MD", "markdown", "mdx", "mdown", "mkd", "mkdn", "mdwn",
        ] {
            let path = dir.path().join(format!("install.{extension}"));
            let text = "---\ntitle: Install guide\n---\n\n- npm\n- pnpm\n- yarn\n";
            std::fs::write(&path, text).unwrap();
            assert_eq!(
                crate::sniff::sniff(&path).unwrap().family,
                crate::sniff::Family::TxtProse,
                "short body must survive the YAML-looking bullets: {extension}"
            );
            let mut records = Vec::new();
            extract_prose(&path, false, false, &mut |record| {
                records.push(record);
                true
            })
            .unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].fields["title"], "Install guide");
            assert_eq!(records[0].fields["body"], text.trim());
            assert_eq!(records[0].fields["start_line"], 1);
        }
    }
}
