//! Passage selection — the difference between "search found the file" and
//! "the agent got the code".
//!
//! Measured on a real valkey corpus: a query about null replies ranks
//! networking.c correctly, but `addReplyNull` is at line 1460 — roughly 40 KB
//! in — so any head- or window-based slice of a 200 KB file is a coin flip
//! on whether the answer is inside it. The record already carries
//! `{name, kind, line}` for every definition; using them turns a guess into
//! a lookup.

use serde_json::{json, Value};

/// Query terms: identifiers of >= 3 chars (`[A-Za-z_][A-Za-z0-9_]{2,}`).
/// Hand-rolled scanner — the ASCII class is the contract, and it must stop
/// at non-ASCII characters exactly like the original regex did.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in query.chars() {
        let ascii_word = c.is_ascii_alphanumeric() || c == '_';
        if ascii_word {
            if cur.is_empty() && !(c.is_ascii_alphabetic() || c == '_') {
                // A token never starts with a digit.
                continue;
            }
            cur.push(c);
        } else {
            if cur.len() >= 3 {
                out.push(cur.to_lowercase());
            }
            cur.clear();
        }
    }
    if cur.len() >= 3 {
        out.push(cur.to_lowercase());
    }
    out
}

/// `[(name, kind, start_line, end_line)]` from the record's own `symbols`.
/// A definition runs until the next definition starts — a coarse but honest
/// bound: it never claims a range the index did not report. When the record
/// carries the symbol's own `end_line` (#1237), the span also stops there, so
/// trailing non-definition lines (comments, top-level statements) after a
/// function are not scored as part of it.
///
/// The recorded `end_line` only ever TIGHTENS the next-definition bound; it
/// never extends a span past a nested definition. A container (`class`,
/// `impl`, `module`, ...) records an `end_line` covering every method inside
/// it, so its full extent is a superset of each method's text: it would
/// out-score (or tie and, being earlier, beat) the method that actually
/// matches, and the 800-char truncation would then show the container's head
/// instead of the matching method.
fn symbol_spans(src: &Value) -> Vec<(String, String, i64, Option<i64>)> {
    let Some(syms) = src.get("symbols").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut got: Vec<(String, String, i64, Option<i64>)> = syms
        .iter()
        .filter(|s| s.is_object())
        .filter_map(|s| {
            let name = s.get("name").and_then(Value::as_str)?;
            let line = s.get("line").and_then(Value::as_i64)?;
            if name.is_empty() {
                return None;
            }
            let end_line = s
                .get("end_line")
                .and_then(Value::as_i64)
                .filter(|&e| e >= line);
            Some((
                name.to_string(),
                s.get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                line,
                end_line,
            ))
        })
        .collect();
    got.sort_by_key(|(_, _, line, _)| *line);
    got.windows(2)
        .map(|w| {
            let (name, kind, line, end_line) = w[0].clone();
            let next_def = w.get(1).map(|(_, _, l, _)| *l - 1);
            let end = match (end_line, next_def) {
                (Some(e), Some(n)) => Some(e.min(n)),
                (e, n) => e.or(n),
            };
            (name, kind, line, end)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .chain(
            got.last()
                .map(|(n, k, l, end_line)| (n.clone(), k.clone(), *l, *end_line)),
        )
        .collect()
}

/// The best matching DEFINITION, as `(text, label)`.
///
/// Distinct query terms in the span rank first. A term appearing in the
/// symbol's NAME adds 25 only when the query is a single identifier, or the
/// name contains every query term — `addReplyNull` still returns
/// addReplyNull. A multi-term question must not let a generic one-word name
/// (`type Token`) outrank the span that covers the rest of the question
/// (#2327). Truncation keeps the label so the truncation is visible. A
/// definition longer than `width` shows its densest [`best_window`], not its
/// head: the match can sit far below the signature (#2464). When that window
/// is the head, the output is the plain truncation it always was.
pub fn symbol_passage(
    body: &str,
    src: &Value,
    query: &str,
    width: usize,
) -> Option<(String, String)> {
    let spans = symbol_spans(src);
    if spans.is_empty() {
        return None;
    }
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.is_empty() {
        return None;
    }
    let terms = query_terms(query);
    if terms.is_empty() {
        return None;
    }
    // (distinct terms covered, score, name, kind, line, text)
    let mut best: Option<(usize, i64, String, String, i64, String)> = None;
    for (name, kind, start, end) in spans {
        let lo = (start - 1).max(0) as usize;
        let hi = end
            .map(|e| e as usize)
            .unwrap_or(lines.len())
            .min(lines.len());
        if hi <= lo {
            continue;
        }
        let text = lines[lo..hi].join("\n");
        let low = text.to_lowercase();
        let nm = name.to_lowercase();
        let covered = terms.iter().filter(|t| low.contains(t.as_str())).count();
        let name_hits = terms.iter().filter(|t| nm.contains(t.as_str())).count();
        let mut score: i64 = terms.iter().map(|t| count_occurrences(&low, t)).sum();
        if terms.len() <= 1 || name_hits == terms.len() {
            score += 25 * name_hits as i64;
        }
        if score <= 0 {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|b| covered > b.0 || (covered == b.0 && score > b.1))
        {
            best = Some((covered, score, name, kind, start, text));
        }
    }
    let (_, _, name, kind, start, mut text) = best?;
    if py_len(&text) > width {
        let (window, offset, total) = best_window(&text, query, width);
        text = if offset == 0 {
            format!(
                "{}\n    ... [{kind} {name} truncated at {width} chars]",
                take_chars(&text, width)
            )
        } else {
            // The offset is into the definition, so the label's start line
            // (and every citation built from it) is unchanged. The marker
            // trails the window, as the truncation marker does, so a budget
            // that cuts from the end drops the marker before any code.
            format!(
                "{window}\n    ... [{kind} {name}: showing {} of {} chars from offset {}, \
                 window centred on the match]",
                commaus(py_len(&window)),
                commaus(total),
                commaus(offset)
            )
        };
    }
    Some((text, format!("{kind} {name} @ line {start}")))
}

/// The densest sampled `width`-char window, snapped to lines when that does
/// not lose query evidence. Returns `(window, start_offset, total_len)`.
///
/// Taking the HEAD instead is the single worst bug this tool had: retrieval
/// ranked the correct files, then handed back 21,461 chars containing ZERO
/// occurrences of the terms the query was about — licence banners and
/// #include lines and nothing else.
pub fn best_window(body: &str, query: &str, width: usize) -> (String, usize, usize) {
    let cs: Vec<char> = body.chars().collect();
    let total = cs.len();
    if total <= width {
        return (body.to_string(), 0, total);
    }
    let terms = query_terms(query);
    if terms.is_empty() {
        return (take_chars(body, width), 0, total);
    }
    // Score every candidate start on a coarse stride: dense term hits win.
    // The stride keeps this linear-ish on multi-hundred-KB sources. The
    // range matches python's `range(0, total - width + stride, stride)`.
    let stride = (width / 8).max(1);
    let last_start_exclusive = total - width + stride;
    let mut best_start = 0usize;
    let mut best_score: i64 = -1;
    let mut start = 0usize;
    while start < last_start_exclusive {
        // Slice BEFORE lowercasing: Unicode lowercase can expand a character,
        // so offsets in a lowercased copy need not be offsets in the source.
        let end = (start + width).min(total);
        let chunk: String = cs[start..end].iter().collect::<String>().to_lowercase();
        let score: i64 = terms.iter().map(|t| count_occurrences(&chunk, t)).sum();
        if score > best_score {
            best_start = start;
            best_score = score;
        }
        start += stride;
    }
    if best_score <= 0 {
        return (take_chars(body, width), 0, total);
    }
    // Prefer complete lines, but never discard query evidence just to align
    // them: the final partial line may hold the match. The end snaps to the
    // LAST newline inside the window (xc.py's `rfind`), not the first —
    // cutting at the first would throw away the tail that earned the score.
    let nl = cs[..best_start].iter().rposition(|&c| c == '\n');
    let start2 = nl.map(|i| i + 1).unwrap_or(best_start);
    let end2 = cs[start2..(start2 + width).min(total)]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|i| start2 + i);
    let end2 = match end2 {
        Some(e) if e > start2 => e,
        _ => (start2 + width).min(total),
    };
    let snapped: String = cs[start2..end2].iter().collect::<String>().to_lowercase();
    let snapped_score: i64 = terms.iter().map(|t| count_occurrences(&snapped, t)).sum();
    if snapped_score < best_score {
        let win: String = cs[best_start..(best_start + width).min(total)]
            .iter()
            .collect();
        return (win, best_start, total);
    }
    let win: String = cs[start2..end2].iter().collect();
    (win, start2, total)
}

/// `path:line` when `path` has no colon. A Windows drive (`C:/src/main.rs`)
/// or any other colon in the path makes the first colon the drive, not the
/// line, so the line is written beside the path instead.
pub fn cite_path_line(path: &str, line: u64) -> String {
    if path.contains(':') {
        format!("{path} (line {line})")
    } else {
        format!("{path}:{line}")
    }
}

/// `file:line`, or the best locator the record actually carries. NEVER an
/// invented line number — a fabricated citation is worse than none, because
/// it survives review by looking checkable.
pub fn provenance(src: &Value) -> String {
    let path = ["ax_path", "ax_file", "path", "file"]
        .iter()
        .find_map(|k| src.get(*k).and_then(Value::as_str))
        .unwrap_or("?");
    let line = ["line", "start_line", "lineno"]
        .iter()
        .find_map(|k| src.get(*k).and_then(Value::as_u64))
        .filter(|n| *n != 0);
    match line {
        Some(n) => cite_path_line(path, n),
        None => path.to_string(),
    }
}

/// The repo a hit's locator belongs to: the first path segment (`repo/file.rs`
/// -> `repo`), which keys the licence map. A locator with no `/` carries no
/// repo — `""`, which simply misses in the map (no licence line).
pub fn locator_repo(loc: &str) -> &str {
    match loc.split_once('/') {
        Some((repo, _)) => repo,
        None => "",
    }
}

/// Non-overlapping occurrences, python `str.count` semantics.
pub(crate) fn count_occurrences(haystack: &str, needle: &str) -> i64 {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count() as i64
}

/// Python `len(str)` — code points, not bytes. Every char count in the
/// contract (label sizes, window offsets, truncation) is in code points.
pub(crate) fn py_len(s: &str) -> usize {
    s.chars().count()
}

/// Python `s[:n]` — code-point slice, never panics on a boundary.
pub(crate) fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `1234567` -> `"1,234,567"` (python `{:,}`).
pub(crate) fn commaus(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ── match line (#2266) ──────────────────────────────────────────────────────
//
// `start_line` is the first line of the passage (or of the section the
// passage was cut from). Agents asked to cite `file:line` were counting
// lines inside the snippet by hand. `match_line` is the absolute 1-based
// source line of the best-scoring query term in that snippet.

/// How many matched lines `match_lines` keeps. The best line is always one
/// of them: when more lines match, the highest scores stay (ties keep the
/// earlier line) and the survivors are written back in source order.
pub const MATCH_LINES_CAP: usize = 8;

/// Absolute source lines of the query terms inside one passage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedLines {
    /// Best-scoring line. Equal scores take the earliest line.
    pub match_line: u64,
    /// Every matched line, source order, at most [`MATCH_LINES_CAP`].
    pub match_lines: Vec<u64>,
}

/// Locate query terms in `passage` and report absolute source lines.
///
/// `origin_line` is the 1-based file line of `passage`'s first line.
/// `terms` are already lowercased (as [`query_terms`] emits them). Matching
/// is case-insensitive and whole-word: a character continues a word when it
/// is alphanumeric or `_`, the same boundary the highlighter uses
/// (`is_word_char` / `next_term_match` in
/// `engine/crates/xerj-engine/src/index.rs`). A line's score sums, per term,
/// `occurrences * 1/(1+n)²` where `n` is how often that term occurs in the
/// passage — the rarity weight `select_lexical_passage` uses so a repeated
/// function word cannot outrank one distinctive hit
/// (`engine/crates/xerj-engine/src/index.rs`, the weighting note above
/// `apply_lexical_passages`).
///
/// Returns `None` when `origin_line` is 0, `terms` is empty, or no term
/// occurs as a whole word. Callers omit the field rather than emit a line
/// they could not find.
pub fn matched_source_lines(
    passage: &str,
    origin_line: u64,
    terms: &[String],
) -> Option<MatchedLines> {
    if origin_line == 0 || terms.is_empty() || passage.is_empty() {
        return None;
    }
    let lines: Vec<String> = passage.lines().map(str::to_lowercase).collect();
    if lines.is_empty() {
        return None;
    }
    let weights: Vec<i64> = terms
        .iter()
        .map(|term| {
            let total: i64 = lines.iter().map(|line| whole_word_count(line, term)).sum();
            rarity_weight(total)
        })
        .collect();
    let mut scored: Vec<(u64, i64)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(line_no) = origin_line.checked_add(i as u64) else {
            break;
        };
        let score: i64 = terms
            .iter()
            .zip(&weights)
            .map(|(term, weight)| whole_word_count(line, term).saturating_mul(*weight))
            .sum();
        if score > 0 {
            scored.push((line_no, score));
        }
    }
    if scored.is_empty() {
        return None;
    }
    // Higher score wins; equal scores take the earlier line (`max_by` keeps
    // the last maximum, so the line number is ordered descending).
    let (match_line, _) = scored
        .iter()
        .copied()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;
    if scored.len() > MATCH_LINES_CAP {
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(MATCH_LINES_CAP);
        scored.sort_by_key(|row| row.0);
    }
    Some(MatchedLines {
        match_line,
        match_lines: scored.into_iter().map(|row| row.0).collect(),
    })
}

/// `n/(1+n)²` scaled into integers. A term that never occurs scores 0 and
/// cannot mark a line. The peak is one occurrence (`SCALE/4`); more copies
/// of the same word are worth strictly less in total, which is the bound
/// the lexical passage picker relies on.
fn rarity_weight(occurrences: i64) -> i64 {
    if occurrences <= 0 {
        return 0;
    }
    const SCALE: i64 = 1_000_000;
    let n = occurrences.min(10_000);
    let denom = (1 + n).saturating_mul(1 + n);
    SCALE / denom
}

/// Whole-word hits of `needle` in an already-lowercased `hay`.
///
/// The boundary is the highlighter's: the character on either side must not
/// be alphanumeric or `_`, so `form` does not match inside `format` and
/// `or` does not match inside `for`.
fn whole_word_count(hay: &str, needle: &str) -> i64 {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0i64;
    let mut from = 0usize;
    while from < hay.len() {
        let Some(pos) = hay[from..].find(needle) else {
            break;
        };
        let abs = from + pos;
        let end = abs + needle.len();
        let before_ok = hay[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !is_match_word_char(c));
        let after_ok = hay[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_match_word_char(c));
        if before_ok && after_ok {
            count += 1;
        }
        let step = hay[abs..].chars().next().map_or(1, char::len_utf8);
        from = abs + step;
    }
    count
}

fn is_match_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Terms to hunt for in a passage. A plain string uses [`query_terms`]. A
/// JSON query object (the DSL `xerj_search` forwards) contributes the text
/// of its `match` / `multi_match` / `query_string` / `term` clauses. A
/// `semantic` or `knn` clause has no lexical line — its passage `ordinal`
/// is a chunk index — so the term list is empty and the caller omits
/// `match_line`.
pub fn match_query_terms(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if json_is_nonlexical(&value) {
                return Vec::new();
            }
            let mut text = String::new();
            collect_dsl_text(&value, &mut text);
            return dedup_terms(&text);
        }
    }
    dedup_terms(trimmed)
}

fn dedup_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for term in query_terms(text) {
        if !out.iter().any(|seen| seen == &term) {
            out.push(term);
        }
    }
    out
}

fn json_is_nonlexical(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|k| k == "semantic" || k == "knn")
                || map.values().any(json_is_nonlexical)
        }
        Value::Array(items) => items.iter().any(json_is_nonlexical),
        _ => false,
    }
}

fn collect_dsl_text(value: &Value, out: &mut String) {
    let Value::Object(map) = value else {
        if let Value::Array(items) = value {
            for item in items {
                collect_dsl_text(item, out);
            }
        }
        return;
    };
    for (key, val) in map {
        match key.as_str() {
            "match"
            | "match_phrase"
            | "match_phrase_prefix"
            | "multi_match"
            | "simple_query_string"
            | "query_string" => absorb_query_clause(val, out),
            "term" => absorb_term_clause(val, out),
            _ => collect_dsl_text(val, out),
        }
    }
}

fn absorb_query_clause(val: &Value, out: &mut String) {
    match val {
        Value::String(s) => push_term_text(out, s),
        Value::Object(map) => {
            if let Some(q) = map.get("query").and_then(Value::as_str) {
                push_term_text(out, q);
            }
            for (key, inner) in map {
                if matches!(
                    key.as_str(),
                    "query" | "fields" | "type" | "operator" | "analyzer" | "boost"
                ) {
                    continue;
                }
                match inner {
                    Value::String(s) => push_term_text(out, s),
                    Value::Object(obj) => {
                        if let Some(q) = obj.get("query").and_then(Value::as_str) {
                            push_term_text(out, q);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn absorb_term_clause(val: &Value, out: &mut String) {
    let Value::Object(map) = val else {
        return;
    };
    for (key, inner) in map {
        if key == "boost" {
            continue;
        }
        match inner {
            Value::String(s) => push_term_text(out, s),
            Value::Object(obj) => {
                if let Some(s) = obj.get("value").and_then(Value::as_str) {
                    push_term_text(out, s);
                }
            }
            _ => {}
        }
    }
}

fn push_term_text(out: &mut String, text: &str) {
    out.push(' ');
    out.push_str(text);
}

/// Stamp `match_line` / `match_lines` onto each hit of an ES search
/// response. Returns whether any hit gained the fields. A hit whose passage
/// does not contain a term, or whose passage start line is not known, is
/// left untouched — a missing field, not a guessed line.
pub fn annotate_search_match_lines(resp: &mut Value, query: &str) -> bool {
    // #2402: every search surface (CLI, `xerj_search`, batch) runs this pass,
    // so it is also where a deduplicated hit stops citing its backup copy.
    let renamed = super::render::prefer_live_alias_paths(resp);
    let terms = match_query_terms(query);
    if terms.is_empty() {
        return renamed;
    }
    let Some(hits) = resp.pointer_mut("/hits/hits").and_then(Value::as_array_mut) else {
        return renamed;
    };
    let mut changed = renamed;
    for hit in hits {
        if stamp_match_line(hit, &terms) {
            changed = true;
        }
    }
    changed
}

/// [`annotate_search_match_lines`] on a response body. Unchanged responses
/// (non-JSON, no hits, nothing located) come back byte-identical.
pub fn annotate_search_response(text: &str, query: &str) -> String {
    let Ok(mut resp) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    if !annotate_search_match_lines(&mut resp, query) {
        return text.to_string();
    }
    serde_json::to_string(&resp).unwrap_or_else(|_| text.to_string())
}

fn stamp_match_line(hit: &mut Value, terms: &[String]) -> bool {
    let found = passage_text_and_origin(hit)
        .and_then(|(text, origin)| matched_source_lines(&text, origin, terms));
    let Some(found) = found else {
        return false;
    };
    let Some(obj) = hit.as_object_mut() else {
        return false;
    };
    let lines = Value::Array(
        found
            .match_lines
            .iter()
            .copied()
            .map(|n| json!(n))
            .collect(),
    );
    let same = obj.get("match_line").and_then(Value::as_u64) == Some(found.match_line)
        && obj.get("match_lines") == Some(&lines);
    if same {
        return false;
    }
    obj.insert("match_line".to_string(), json!(found.match_line));
    obj.insert("match_lines".to_string(), lines);
    true
}

/// Passage text plus the file line of its first line.
///
/// `start_line` is the file line of line 0 of the stored field, and a
/// lexical window's `ordinal` counts lines into that field, so the snippet
/// starts at `start_line + ordinal`. `line` on a `code` passage is the same
/// kind of origin. A whole-file `body` window has no `start_line`; its
/// `ordinal` is already the file line (`start_offset` marks it as lexical,
/// the same guard `lexical_passage_line` uses). A `defs` / `title` / `name`
/// list is not source text. A data-row locator (`b<offset>`, `r<N>`, … —
/// `record_locator` in `engine/crates/xerj-autoindex/src/search.rs`) makes
/// `ordinal` a line of the row, not of the file, so it is only used as an
/// offset from a stored `start_line` or `line`. A PDF `page` or notebook
/// `cell` with no stored line is not a line.
fn passage_text_and_origin(hit: &Value) -> Option<(String, u64)> {
    let src = hit.get("_source");
    let start_line = stored_positive(src, "start_line");
    let line = stored_positive(src, "line");
    let data_row = src
        .and_then(|s| s.get("ax_locator"))
        .and_then(Value::as_str)
        .is_some_and(locator_is_data_row);
    let page_or_cell = src.is_some_and(value_has_page_or_cell);

    if let Some(first) = hit.pointer("/fields/_passage/0") {
        let field = first.get("field").and_then(Value::as_str);
        if is_synthetic_passage_field(field) {
            return code_anchored_at_line(src, line);
        }
        if let Some(text) = slice_text(first) {
            let page_or_cell = page_or_cell || value_has_page_or_cell(first);
            return origin_of_slice(first, start_line, line, data_row, page_or_cell)
                .map(|origin| (text, origin));
        }
    }
    if let Some(start) = start_line {
        if let Some(text) = source_text(src, &["body", "text"]) {
            return Some((text, start));
        }
    }
    code_anchored_at_line(src, line)
}

fn code_anchored_at_line(src: Option<&Value>, line: Option<u64>) -> Option<(String, u64)> {
    let line = line?;
    let text = source_text(src, &["code"])?;
    Some((text, line))
}

fn source_text(src: Option<&Value>, keys: &[&str]) -> Option<String> {
    let src = src?;
    for key in keys {
        if let Some(text) = src.get(*key).and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn stored_positive(src: Option<&Value>, key: &str) -> Option<u64> {
    src?.get(key).and_then(Value::as_u64).filter(|n| *n > 0)
}

fn slice_text(passage: &Value) -> Option<String> {
    match passage {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(obj) => obj
            .get("text")
            .or_else(|| obj.get("passage"))
            .or_else(|| obj.get("snippet"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn is_synthetic_passage_field(field: Option<&str>) -> bool {
    matches!(field, Some("defs" | "defs_expanded" | "title" | "name"))
}

fn value_has_page_or_cell(value: &Value) -> bool {
    ["page", "cell"].iter().any(|key| {
        value
            .get(*key)
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 0)
    })
}

/// A locator whose passage ordinal counts lines of the record, not of the
/// file. Same shapes as `record_locator` in xerj-autoindex's search command.
fn locator_is_data_row(loc: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let yaml = loc
        .strip_prefix('d')
        .is_some_and(|rest| match rest.split_once('e') {
            Some((doc, elem)) => digits(doc) && digits(elem),
            None => digits(rest),
        });
    let row = ["b", "r", "row"]
        .iter()
        .any(|prefix| loc.strip_prefix(prefix).is_some_and(digits));
    let sheet_row = loc.rsplit_once("!r").is_some_and(|(_, n)| digits(n));
    let element = loc.rsplit_once(":e").is_some_and(|(_, n)| digits(n));
    row || yaml || sheet_row || element
}

fn origin_of_slice(
    passage: &Value,
    start_line: Option<u64>,
    line: Option<u64>,
    data_row: bool,
    page_or_cell: bool,
) -> Option<u64> {
    let ordinal = passage.get("ordinal").and_then(Value::as_u64);
    let field = passage.get("field").and_then(Value::as_str);
    let lexical = passage.get("start_offset").is_some();
    if data_row {
        let base = start_line.or(line)?;
        return base.checked_add(ordinal.unwrap_or(0));
    }
    if let Some(start) = start_line {
        return start.checked_add(ordinal.unwrap_or(0));
    }
    if let Some(line) = line {
        return match field {
            Some("code") => line.checked_add(ordinal.unwrap_or(0)),
            Some("body" | "text" | "content") if lexical => ordinal.and_then(|o| o.checked_add(1)),
            _ if ordinal.unwrap_or(0) == 0 => Some(line),
            _ => None,
        };
    }
    if page_or_cell {
        return None;
    }
    if lexical {
        return ordinal.and_then(|o| o.checked_add(1));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terms_are_ascii_identifiers_of_three_or_more_chars() {
        assert_eq!(
            query_terms("addReplyNull handles $-1 and CRLF"),
            vec!["addreplynull", "handles", "and", "crlf"]
        );
        assert!(query_terms("a b xy").is_empty());
        // Non-ASCII ends a token, exactly like the original regex class:
        // "xerjö" still yields the ASCII prefix "xerj" (4 chars, kept).
        assert_eq!(query_terms("xerjö code"), vec!["xerj", "code"]);
    }

    #[test]
    fn symbol_passage_picks_the_named_definition_and_truncates_visibly() {
        let body = "int filler(void) { return 0; }\n\
                    void addReplyNull(client *c) {\n\
                        addReplyProto(c, \"$-1\\r\\n\", 5);\n\
                    }\n\
                    void other(void) { addReplyNull(0); }\n";
        let src = json!({
            "symbols": [
                { "name": "filler", "kind": "function", "line": 1 },
                { "name": "addReplyNull", "kind": "function", "line": 2 },
                { "name": "other", "kind": "function", "line": 5 }
            ]
        });
        let (text, label) = symbol_passage(body, &src, "addReplyNull null reply", 800).unwrap();
        assert_eq!(label, "function addReplyNull @ line 2");
        assert!(
            text.contains("addReplyProto"),
            "the definition body: {text}"
        );
        assert!(!text.contains("filler"));

        // Truncation keeps the label so it is visible.
        let (short, _) = symbol_passage(body, &src, "addReplyNull", 20).unwrap();
        assert!(
            short.contains("[function addReplyNull truncated at 20 chars]"),
            "{short}"
        );

        // No symbols / no matching term -> None (caller falls back).
        assert!(symbol_passage(body, &json!({}), "addReplyNull", 800).is_none());
        assert!(symbol_passage(body, &src, "zzzzz", 800).is_none());
    }

    /// #2464: a definition longer than `width` whose match sits below its
    /// signature shows the matching window of THAT definition, with the
    /// offset into the definition; the label (and so the citation) keeps the
    /// definition's start line.
    #[test]
    fn long_definition_shows_the_matching_window_not_its_head() {
        let filler: Vec<String> = (0..30)
            .map(|i| format!("    Filler line {i}: unrelated parameters and options."))
            .collect();
        let def = format!(
            "def long_function(arg):\n    \"\"\"\n    Do many things.\n\n{}\n\n    \
             Localize a naive timestamp to a timezone first, then convert it.\n    \
             \"\"\"\n    return arg",
            filler.join("\n")
        );
        let body = format!("def short_helper(x):\n    return x\n{def}");
        let src = json!({
            "symbols": [
                { "name": "short_helper", "kind": "function", "line": 1, "end_line": 2 },
                { "name": "long_function", "kind": "function", "line": 3 }
            ]
        });
        let (text, label) =
            symbol_passage(&body, &src, "localize naive timestamp timezone", 300).unwrap();
        assert_eq!(label, "function long_function @ line 3");
        assert!(text.contains("Localize a naive timestamp"), "{text}");
        assert!(!text.contains("def long_function"), "not the head: {text}");
        let (window, marker) = text.rsplit_once('\n').unwrap();
        // The window comes first, so a cut from the end loses the marker
        // before the match.
        assert!(
            take_chars(&text, 300).contains("Localize a naive timestamp"),
            "{text}"
        );
        let prefix = "    ... [function long_function: showing ";
        assert!(marker.starts_with(prefix), "{marker}");
        assert!(
            marker.ends_with(", window centred on the match]"),
            "{marker}"
        );
        // The offset is into the definition, not into the file body.
        let offset: usize = marker
            .split_once(" from offset ")
            .and_then(|(_, r)| r.split_once(", window"))
            .map(|(n, _)| n.replace(',', ""))
            .and_then(|n| n.parse().ok())
            .unwrap();
        assert!(offset > 0);
        let from_offset: String = def.chars().skip(offset).collect();
        assert!(from_offset.starts_with(window), "offset {offset}: {window}");
        assert!(window.chars().count() <= 300);
        assert!(marker.contains(&format!(" of {} chars", commaus(def.chars().count()))));
    }

    /// When the densest window is the head, the output is byte-for-byte the
    /// plain truncation it was before #2464; a short definition is untouched.
    #[test]
    fn head_window_and_short_definition_are_unchanged() {
        let def = format!(
            "fn needle_here() {{\n    // needle needle\n{}}}",
            "    let unrelated = 0;\n".repeat(40)
        );
        let src = json!({
            "symbols": [{ "name": "needle_here", "kind": "function", "line": 1 }]
        });
        let (text, label) = symbol_passage(&def, &src, "needle", 120).unwrap();
        assert_eq!(label, "function needle_here @ line 1");
        assert_eq!(
            text,
            format!(
                "{}\n    ... [function needle_here truncated at 120 chars]",
                take_chars(&def, 120)
            )
        );

        let short = "fn needle() {\n    needle_call();\n}";
        let src = json!({
            "symbols": [{ "name": "needle", "kind": "function", "line": 1 }]
        });
        let (text, label) = symbol_passage(short, &src, "needle", 800).unwrap();
        assert_eq!(label, "function needle @ line 1");
        assert_eq!(text, short);
    }

    /// #2327: a multi-term question must not prefer a one-word type whose
    /// name matches a single term. An exact one-word lookup still prefers
    /// the definition that carries that name.
    #[test]
    fn multi_term_query_prefers_the_span_covering_more_terms() {
        let body = "\
export type Token = string;\n\
\n\
export function apply(token: Token, handler: Handler): Session {\n\
  return handler.refresh(token);\n\
}\n\
\n\
export type Handler = { refresh(token: Token): Session };\n\
";
        let src = json!({
            "symbols": [
                { "name": "Token", "kind": "type", "line": 1, "end_line": 1 },
                { "name": "apply", "kind": "function", "line": 3, "end_line": 5 },
                { "name": "Handler", "kind": "type", "line": 7, "end_line": 7 }
            ]
        });
        let (text, label) =
            symbol_passage(body, &src, "session token refresh handler", 800).unwrap();
        assert_eq!(label, "function apply @ line 3");
        assert!(text.contains("handler.refresh"), "{text}");
        assert!(!text.contains("export type Token"), "{text}");

        let (exact, exact_label) = symbol_passage(body, &src, "Token", 800).unwrap();
        assert_eq!(exact_label, "type Token @ line 1");
        assert!(exact.contains("type Token"), "{exact}");
    }

    #[test]
    fn best_window_is_densest_and_snaps_only_without_evidence_loss() {
        let body = format!(
            "{}{}",
            "A".repeat(200) + "\nmatch term here\n",
            "B".repeat(200)
        );
        let (win, _start, total) = best_window(&body, "match term here", 24);
        assert!(win.contains("match term here"), "the densest window: {win}");
        assert_eq!(total, body.chars().count());

        // A query with no terms gets the head, labelled as such by the caller.
        let (win, start, _) = best_window(&body, "$ $$", 16);
        assert_eq!(start, 0);
        assert_eq!(win.chars().count(), 16);
    }

    #[test]
    fn window_offsets_survive_unicode_expansion_in_lowercase() {
        // Lowercasing can EXPAND characters; offsets must stay source-true.
        // 'İ' (U+0130) lowercases to two chars in some paths — use a multi-byte
        // mix around the match so a byte/char/lowercase confusion shows.
        let body = format!("{}matchpoint{}", "ÄÖÜ".repeat(30), "äöü".repeat(30));
        let (win, _s, _t) = best_window(&body, "matchpoint", 12);
        assert!(
            win.contains("matchpoint"),
            "window must hold the match: {win:?}"
        );
    }

    #[test]
    fn provenance_prefers_ax_fields_and_never_invents_a_line() {
        assert_eq!(
            provenance(&json!({ "ax_path": "repo/a.c", "path": "x", "line": 12 })),
            "repo/a.c:12"
        );
        assert_eq!(
            provenance(&json!({ "ax_file": "repo/b.c", "start_line": 3 })),
            "repo/b.c:3"
        );
        assert_eq!(provenance(&json!({ "path": "repo/c.c" })), "repo/c.c");
        assert_eq!(
            provenance(&json!({ "file": "d.c", "lineno": 0 })),
            "d.c",
            "line 0 is absent, not a line"
        );
        assert_eq!(provenance(&json!({})), "?");
        assert_eq!(
            provenance(&json!({ "ax_path": "C:/src/main.rs", "line": 12 })),
            "C:/src/main.rs (line 12)",
            "the drive colon is not the line number"
        );
        assert_eq!(cite_path_line("src/lib.rs", 4), "src/lib.rs:4");
    }

    #[test]
    fn locator_repo_is_the_first_path_segment() {
        assert_eq!(locator_repo("valkey/src/networking.c"), "valkey");
        assert_eq!(locator_repo("plain.rs"), "");
    }

    #[test]
    fn commaus_matches_python_formatting() {
        assert_eq!(commaus(0), "0");
        assert_eq!(commaus(999), "999");
        assert_eq!(commaus(1_000), "1,000");
        assert_eq!(commaus(21_461), "21,461");
        assert_eq!(commaus(1_234_567), "1,234,567");
    }

    /// A container's recorded `end_line` covers its methods; it must not let
    /// the container swallow (and out-rank) the method that actually matches.
    #[test]
    fn recorded_end_line_does_not_let_a_container_swallow_its_methods() {
        let mut body = String::from("class Widget:\n");
        for i in 0..60 {
            body.push_str(&format!("    pad_{i} = {i}\n"));
        }
        body.push_str("    def render(self):\n        return needle\n");
        let render_line = 62;
        let src = json!({
            "symbols": [
                { "name": "Widget", "kind": "class", "line": 1, "end_line": 63 },
                { "name": "render", "kind": "method", "line": render_line, "end_line": 63 }
            ]
        });
        let (text, label) = symbol_passage(&body, &src, "needle", 800).unwrap();
        assert_eq!(label, format!("method render @ line {render_line}"));
        assert!(text.contains("return needle"), "{text}");
    }

    #[test]
    fn symbol_passage_bounds_span_by_recorded_end_line() {
        let body = "fn alpha() {\n    let needle = 1;\n}\n// trailing comment mentioning needle needle needle\nfn beta() {}\n";
        let src = json!({
            "symbols": [
                { "name": "alpha", "kind": "function", "line": 1, "end_line": 3 },
                { "name": "beta", "kind": "function", "line": 5, "end_line": 5 }
            ]
        });
        let (text, label) = symbol_passage(body, &src, "needle", 800).unwrap();
        assert_eq!(label, "function alpha @ line 1");
        assert_eq!(text, "fn alpha() {\n    let needle = 1;\n}");
    }

    fn prose_hit(start: u64, ordinal: u64, text: &str) -> Value {
        json!({
            "_source": { "ax_path": "guide.md", "start_line": start, "ax_locator": "s2" },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": ordinal,
                "start_offset": 0,
                "text": text
            }] }
        })
    }

    fn stamped(hit: Value, query: &str) -> Value {
        let mut resp = json!({ "hits": { "hits": [hit] } });
        annotate_search_match_lines(&mut resp, query);
        resp["hits"]["hits"][0].clone()
    }

    #[test]
    fn match_on_the_first_line_is_the_passage_start() {
        let hit = stamped(
            prose_hit(169, 0, "Slot filling binds the token.\nLater prose."),
            "slot filling",
        );
        assert_eq!(hit["match_line"], 169);
        assert_eq!(hit["match_lines"], json!([169]));
    }

    #[test]
    fn match_on_a_later_line_is_start_line_plus_the_offset() {
        let hit = stamped(
            prose_hit(
                169,
                0,
                "Intro paragraph about the form.\n\nThe token is required when unbound.",
            ),
            "unbound token",
        );
        assert_eq!(hit["match_line"], 171, "{hit}");
        assert_eq!(hit["match_lines"], json!([171]));
    }

    /// A window that does not start at the section's first line: `ordinal`
    /// is the line offset inside the field `start_line` names.
    #[test]
    fn passage_ordinal_shifts_the_window_off_start_line() {
        let hit = stamped(
            prose_hit(10, 5, "preamble\nSlot filling lives here."),
            "slot",
        );
        assert_eq!(hit["match_line"], 16, "{hit}");
    }

    #[test]
    fn multi_term_picks_the_best_score_then_the_earliest_line() {
        let passage = "\
alpha only\n\
alpha beta\n\
alpha beta\n\
the the the the";
        let terms = match_query_terms("alpha beta the");
        let found = matched_source_lines(passage, 1, &terms).unwrap();
        // Both content terms beat the line that only has one, and beat a
        // line that repeats a common word. The two equal lines take the first.
        assert_eq!(found.match_line, 2, "{found:?} terms={terms:?}");
        // The repeated "the" is a real hit, and it does not win.
        assert_eq!(found.match_lines, vec![1, 2, 3, 4]);

        // Case fold, and `form` is not a hit inside `format`.
        let folded =
            matched_source_lines("format\nThe Form field", 40, &match_query_terms("form")).unwrap();
        assert_eq!(folded.match_line, 41);

        let inside = matched_source_lines("format only", 1, &match_query_terms("form"));
        assert!(inside.is_none(), "{inside:?}");
    }

    #[test]
    fn no_match_or_unknown_start_line_omits_the_field() {
        let missing = stamped(
            prose_hit(169, 0, "nothing about this topic"),
            "slot filling",
        );
        assert!(missing.get("match_line").is_none(), "{missing}");
        assert!(missing.get("match_lines").is_none(), "{missing}");

        let no_line = json!({
            "_source": { "ax_path": "guide.md" },
            "fields": { "_passage": [{ "field": "body", "ordinal": 4, "text": "slot filling" }] }
        });
        let unknown = stamped(no_line, "slot");
        assert!(
            unknown.get("match_line").is_none(),
            "a section-relative ordinal is not a file line: {unknown}"
        );

        let row = json!({
            "_source": { "ax_path": "rows.jsonl", "ax_locator": "b68801" },
            "fields": { "_passage": [{
                "field": "body", "ordinal": 0, "start_offset": 0, "text": "slot filling"
            }] }
        });
        let data_row = stamped(row, "slot");
        assert!(
            data_row.get("match_line").is_none(),
            "a row ordinal is not a file line: {data_row}"
        );

        let pdf = json!({
            "_source": { "ax_path": "paper.pdf", "page": 4 },
            "fields": { "_passage": [{
                "field": "body", "ordinal": 0, "start_offset": 0, "page": 4, "text": "slot filling"
            }] }
        });
        assert!(stamped(pdf, "slot").get("match_line").is_none());

        // Semantic ordinals are chunk indexes. No line is better than a wrong one.
        let mut semantic = json!({ "hits": { "hits": [prose_hit(8, 0, "slot filling")] } });
        let query = r#"{"query":{"semantic":{"field":"body","query":"slot filling"}}}"#;
        assert!(!annotate_search_match_lines(&mut semantic, query));
        assert!(semantic["hits"]["hits"][0].get("match_line").is_none());
    }

    #[test]
    fn a_whole_file_lexical_ordinal_is_the_file_line() {
        let hit = json!({
            "_source": { "ax_path": "src/lib.rs", "ax_locator": "code" },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 41,
                "start_offset": 900,
                "text": "fn helper() {\n    slot()\n}"
            }] }
        });
        let stamped = stamped(hit, "slot");
        assert_eq!(stamped["match_line"], 43, "{stamped}");
    }

    /// Text chunks (`extract/txt.rs`) store `start_line` on a `b<byte>` locator.
    /// The locator marks a byte offset; the line is still a file line, and the
    /// passage ordinal is an offset into that chunk.
    #[test]
    fn a_text_chunk_uses_its_stored_start_line() {
        let hit = json!({
            "_source": {
                "ax_path": "notes.txt",
                "ax_locator": "b120",
                "start_line": 40
            },
            "fields": { "_passage": [{
                "field": "text",
                "ordinal": 2,
                "text": "preface\nmore\nslot filling"
            }] }
        });
        assert_eq!(stamped(hit, "slot")["match_line"], 44);
    }

    #[test]
    fn a_declaration_without_a_passage_is_anchored_at_line() {
        let hit = json!({
            "_source": {
                "ax_path": "src/lib.rs",
                "line": 8,
                "code": "fn alpha() {\n    slot()\n}"
            }
        });
        assert_eq!(stamped(hit, "slot")["match_line"], 9);
    }

    #[test]
    fn match_lines_are_capped_and_keep_the_best_line() {
        let passage = std::iter::repeat_n("slot here", 20)
            .collect::<Vec<_>>()
            .join("\n");
        let found = matched_source_lines(&passage, 1, &match_query_terms("slot")).unwrap();
        assert_eq!(found.match_line, 1);
        assert_eq!(found.match_lines.len(), MATCH_LINES_CAP);
        assert_eq!(found.match_lines[0], 1);
        assert_eq!(*found.match_lines.last().unwrap(), MATCH_LINES_CAP as u64);
    }

    #[test]
    fn a_dsl_match_clause_supplies_the_terms() {
        let hit = prose_hit(3, 0, "preface\nslot filling example");
        let stamped = stamped(hit, r#"{"match":{"body":"slot filling"}}"#);
        assert_eq!(stamped["match_line"], 4, "{stamped}");
    }

    #[test]
    fn unchanged_responses_stay_byte_identical() {
        let raw = r#"{"hits":{"hits":[]}}"#;
        assert_eq!(annotate_search_response(raw, "slot"), raw);
        assert_eq!(annotate_search_response("not json", "slot"), "not json");
    }
}
