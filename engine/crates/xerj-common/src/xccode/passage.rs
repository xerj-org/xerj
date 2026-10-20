//! Passage selection — the difference between "search found the file" and
//! "the agent got the code".
//!
//! Measured on a real valkey corpus: a query about null replies ranks
//! networking.c correctly, but `addReplyNull` is at line 1460 — roughly 40 KB
//! in — so any head- or window-based slice of a 200 KB file is a coin flip
//! on whether the answer is inside it. The record already carries
//! `{name, kind, line}` for every definition; using them turns a guess into
//! a lookup.

use serde_json::Value;

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
/// Query terms score by count-in-span; a term appearing in the symbol's NAME
/// adds 25 — the strongest signal an agent can act on: a query mentioning
/// `addReplyNull` should return addReplyNull, not whichever unrelated span
/// repeats a common word most often. Truncation keeps the label so the
/// truncation is visible.
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
    let mut best: Option<(i64, String, String, i64, String)> = None;
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
        let mut score: i64 = terms.iter().map(|t| count_occurrences(&low, t)).sum();
        score += 25 * terms.iter().filter(|t| nm.contains(t.as_str())).count() as i64;
        if score <= 0 {
            continue;
        }
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, name, kind, start, text));
        }
    }
    let (_, name, kind, start, mut text) = best?;
    if py_len(&text) > width {
        text = format!(
            "{}\n    ... [{kind} {name} truncated at {width} chars]",
            take_chars(&text, width)
        );
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
        Some(n) => format!("{path}:{n}"),
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
}
