//! The ONE output renderer. `xerj code` prints its `text`; the
//! `xerj_code_search` MCP tool returns the same bytes as its payload — parity
//! pinned by test, because two renderers always drift.
//!
//! A weak page is labelled [`WEAK_PAGE_PROSE`] and each passage is marked
//! `weak`. A page that clears the checks in [`page_is_weak_match`] is
//! byte-identical to the unlabelled rendering, including every strong passage
//! on a page whose tail alone is weak.

use std::collections::HashMap;

use serde_json::Value;

use super::licence;
use super::passage::{
    best_window, commaus, locator_repo, py_len, query_terms, symbol_passage, take_chars,
};

/// Printed once, above the hits, when [`page_is_weak_match`] is true.
/// Agents key off this exact line.
pub const WEAK_PAGE_PROSE: &str = "weak match: the corpus may not cover this";

/// MEATL form of [`WEAK_PAGE_PROSE`].
const WEAK_PAGE_MEATL: &str = "@weak why=corpus-may-not-cover";

/// A passage whose `_score` is strictly below this fraction of the page's
/// top score is marked `weak` on its own header. Half the best hit is "well
/// below": an exact tie with half, and every nearer score, stays unmarked.
const WEAK_SCORE_RATIO: f64 = 0.5;

/// Coverage is only a page-level signal once the query has this many content
/// terms. Two terms (`start node`) would flag a hit that shares one of them.
const MIN_TERMS_FOR_COVERAGE: usize = 3;

/// English function words of length >= 3. They are not coverage: a nearest
/// neighbour is still English, so `the` / `with` / `into` would otherwise
/// count as the query being present. Technical words (`list`, `type`,
/// `index`, `set`) stay content.
const STOPWORDS: &[&str] = &[
    "about", "across", "after", "all", "also", "and", "any", "are", "been", "before", "being",
    "both", "but", "can", "could", "did", "does", "doing", "each", "for", "from", "had", "has",
    "have", "how", "into", "its", "just", "more", "most", "not", "one", "only", "other", "our",
    "out", "per", "should", "some", "such", "than", "that", "the", "them", "then", "they", "this",
    "under", "use", "used", "using", "via", "was", "were", "what", "when", "where", "which", "who",
    "whose", "why", "will", "with", "within", "without", "would", "you", "your",
];

/// How to render the hit list.
pub struct RenderOpts<'a> {
    pub corpus: &'a str,
    pub query: &'a str,
    /// Max chars per passage; 0 = file-head mode (first 400 chars, labelled
    /// as the file head — never passed off as a matching passage).
    pub full: usize,
    pub no_symbol: bool,
    pub meatl: bool,
    /// Show `rrf {x:.4}` (native hybrid) instead of `score {x:.2}`. The
    /// per-leg `[bm25 #r, vec #r]` annotation is deliberately gone: the
    /// server-side fuser exposes no per-leg ranks, and the every-run arms
    /// note carries which arms ran.
    pub rrf_scores: bool,
    pub age_days: Option<i64>,
    /// MCP `licence_policy:"strict"`: restricted hits keep locator + licence
    /// + warning but lose the passage text. The CLI passes false (warn).
    pub strict_licence: bool,
    /// BM25 passages store a 0-based line index in `_passage.ordinal`.
    /// A semantic passage stores a chunk index in the same field. Only a
    /// caller that knows the hit came from a lexical query may turn the
    /// ordinal into a file line.
    pub lexical_lines: bool,
    /// The corpus checkout (`<root>/corpora/<corpus>`), when it is on this
    /// machine. Prose records (Markdown, text, HTML sections) store no file
    /// line, and their passage ordinal counts lines of the re-flowed SECTION,
    /// not of the file; with the checkout the line is found in the file itself.
    pub checkout: Option<&'a std::path::Path>,
    /// Corpus marked first-party (#2339). An unidentified licence stays in
    /// the header and loses the do-not-copy banner. A named restricted
    /// licence still warns and still honours `strict_licence`.
    pub own: bool,
}

/// The longest non-empty string field of a source object (excluding `ax_*`
/// provenance metadata), "" when there is none — the content-bearing field of
/// a raw-JSON record (#1158). Fields are visited in sorted key order and only
/// a STRICTLY longer field replaces the winner, so a tie goes to the
/// alphabetically first name and the passage never depends on the wire's key
/// order.
fn longest_string_field(src: &Value) -> String {
    let mut best: Option<(usize, &str)> = None;
    if let Some(obj) = src.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for key in keys {
            if key.starts_with("ax_") {
                continue;
            }
            if let Some(s) = obj[key].as_str() {
                if !s.trim().is_empty() && best.is_none_or(|(len, _)| s.chars().count() > len) {
                    best = Some((s.chars().count(), s));
                }
            }
        }
    }
    best.map(|(_, s)| s.to_string()).unwrap_or_default()
}

/// True for an autoindex `*-symbols` record: `name` and `kind` strings plus
/// a numeric `line`, alongside the declaration in `code`. A raw-JSON data
/// record that merely happens to have a `code` key does not match.
fn is_symbol_record(src: &Value) -> bool {
    src.get("name").is_some_and(Value::is_string)
        && src.get("kind").is_some_and(Value::is_string)
        && src.get("line").is_some_and(Value::is_u64)
        && src.get("code").is_some_and(Value::is_string)
}

/// Render the full result block: the optional arms-ran note line, the
/// weak-page line when [`page_is_weak_match`] says so, one block per hit,
/// the footer. `note` is printed EVERY time it exists — a silently
/// BM25-only "hybrid" result must never read as hybrid.
pub fn render(
    hits: &[Value],
    licences: &HashMap<String, String>,
    opts: &RenderOpts,
    note: Option<&str>,
) -> String {
    let top = top_score(hits);
    let page_weak = page_is_weak_match(hits, opts, top);
    let mut out = String::new();
    if let Some(n) = note {
        out.push_str(&note_line(n, opts.meatl));
        out.push('\n');
    }
    if page_weak {
        out.push_str(if opts.meatl {
            WEAK_PAGE_MEATL
        } else {
            WEAK_PAGE_PROSE
        });
        out.push('\n');
    }
    for h in hits {
        let score = hit_score(h);
        let weak = page_weak || passage_is_score_weak(score, top);
        out.push_str(&render_hit(h, licences, opts, weak));
    }
    if !opts.meatl {
        out.push('\n');
        out.push_str(&format!("{} passages from '{}'", hits.len(), opts.corpus));
        if opts.age_days.is_some_and(|d| d > 0) {
            out.push_str(&format!(" (index {}d old)", opts.age_days.unwrap_or(0)));
        }
        out.push_str("\nCite file:line for anything you rely on.\n");
    }
    out
}

/// The no-match text. Explicit, because a silent miss makes the next agent
/// re-run the same dead query — this is the line that stops the loop.
pub fn no_match_text(corpus: &str, query: &str, meatl: bool) -> String {
    if meatl {
        format!("@no q=\"{query}\" why=no-match-in-corpus\n")
    } else {
        format!(
            "No passage in '{corpus}' matches: {query}\n\
             The corpus is likely wrong for this task — fall back to normal work \
             rather than forcing a bad match.\n"
        )
    }
}

fn note_line(note: &str, meatl: bool) -> String {
    if meatl {
        format!("@mode {note}")
    } else {
        format!("[{note}]")
    }
}

fn hit_score(hit: &Value) -> f64 {
    hit.get("_score").and_then(Value::as_f64).unwrap_or(0.0)
}

fn top_score(hits: &[Value]) -> f64 {
    hits.iter().map(hit_score).fold(0.0, f64::max)
}

/// True when `score` sits strictly below [`WEAK_SCORE_RATIO`] of `top`.
fn passage_is_score_weak(score: f64, top: f64) -> bool {
    top > 0.0 && score < top * WEAK_SCORE_RATIO
}

/// Query content terms: [`query_terms`], stopwords removed, order kept,
/// duplicates dropped. Matching is a lowercase substring of the hit text,
/// the same comparison passage selection uses — literal, so a stem the
/// engine would equate (`channels` / `channel`) counts only when the
/// surface form occurs.
fn content_terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for term in query_terms(query) {
        if STOPWORDS.contains(&term.as_str()) || out.iter().any(|seen| seen == &term) {
            continue;
        }
        out.push(term);
    }
    out
}

/// Every non-`ax_*` string on the hit, plus highlight passages. Coverage
/// reads the record, so a term outside the rendered window still counts.
fn hit_haystack(hit: &Value) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(src) = hit.get("_source").and_then(Value::as_object) {
        for (key, value) in src {
            if !key.starts_with("ax_") {
                collect_strings(value, &mut parts);
            }
        }
    }
    if let Some(passages) = hit.pointer("/fields/_passage").and_then(Value::as_array) {
        for passage in passages {
            if let Some(text) = passage.get("text").and_then(Value::as_str) {
                parts.push(text);
            }
        }
    }
    parts.join("\n").to_lowercase()
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

fn terms_covered(haystack: &str, terms: &[String]) -> usize {
    terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count()
}

/// A directory or file name that marks archived documentation.
fn path_is_archived(path: &str) -> bool {
    path.split(['/', '\\']).any(|seg| {
        let s = seg.to_ascii_lowercase();
        matches!(
            s.as_str(),
            "archive" | "archived" | "archives" | "_archive" | "_archived"
        ) || s.starts_with("archived.")
            || s.ends_with(".archived")
            || s.contains(".archived.")
    })
}

/// True when the hit is an archived note or lives under an archive path.
///
/// Semantic pages mix current and archived docs with no other signal
/// (#2327). The path is already on the hit; this is the explicit mark the
/// header prints. A source file named `archive.ts` is not archived.
pub fn source_looks_archived(src: &Value) -> bool {
    if src.get("keep_archived").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let path = ["ax_path", "ax_file", "path", "file"]
        .iter()
        .find_map(|k| src.get(*k).and_then(Value::as_str))
        .unwrap_or("");
    path_is_archived(path)
}

/// True when `path` names a backup or archived copy rather than the live file:
/// an archive directory ([`path_is_archived`]), a backup directory
/// (`backup/`, `backups/`, `bak/`, `old/`, `备份/`, `root-bak-20261001/`), or a
/// backup file name (`notes.md.bak`, `notes.md.orig`, `notes.md~`,
/// `notes.md.old`).
///
/// Only used to choose WHICH name of a byte-identical group is cited, so a
/// false positive costs nothing but the name shown (#2402).
pub fn path_looks_like_copy(path: &str) -> bool {
    if path_is_archived(path) {
        return true;
    }
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let Some((file, dirs)) = segments.split_last() else {
        return false;
    };
    let dir_is_copy = dirs.iter().any(|seg| {
        let s = seg.to_lowercase();
        matches!(
            s.as_str(),
            "backup" | "backups" | "bak" | "_backup" | "_backups" | "old" | "_old" | "备份"
        ) || s.starts_with("bak-")
            || s.starts_with("bak_")
            || s.starts_with("backup-")
            || s.starts_with("backup_")
            || s.ends_with("-bak")
            || s.ends_with("_bak")
            || s.ends_with(".bak")
            || s.contains("-bak-")
            || s.contains("_bak_")
    });
    let f = file.to_lowercase();
    dir_is_copy
        || f.ends_with('~')
        || [".bak", ".orig", ".old", ".backup"]
            .iter()
            .any(|ext| f.ends_with(ext))
}

/// Cite the live name of a deduplicated file, not its backup copy (#2402).
///
/// Byte-identical files are indexed once; the record keeps every name in
/// `ax_paths` and the canonical `ax_path` is the lowest native path, which is
/// stable for the sync manifest but is often `archive/...` or
/// `backups/...` — so a search answered with the backup's path even though
/// `docs/...` holds the same bytes. When the cited path looks like a copy
/// ([`path_looks_like_copy`]) and another name in `ax_paths` does not, the hit
/// cites that name instead (`ax_path`, and `path` when it mirrored it). The
/// bytes are identical, so lines and passages are unchanged; `ax_paths` and
/// `ax_file` are untouched. Returns whether any hit changed.
pub fn prefer_live_alias_paths(resp: &mut Value) -> bool {
    let Some(hits) = resp.pointer_mut("/hits/hits").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for hit in hits {
        let Some(src) = hit.get_mut("_source").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(cited) = src
            .get("ax_path")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if !path_looks_like_copy(&cited) {
            continue;
        }
        let live = src
            .get("ax_paths")
            .and_then(Value::as_array)
            .and_then(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|p| *p != cited && !path_looks_like_copy(p))
                    .map(str::to_owned)
            });
        let Some(live) = live else {
            continue;
        };
        if src.get("path").and_then(Value::as_str) == Some(cited.as_str()) {
            src.insert("path".into(), Value::String(live.clone()));
        }
        src.insert("ax_path".into(), Value::String(live));
        changed = true;
    }
    changed
}

/// Move a one-term symbol definition behind hits that cover more of a
/// multi-term question (#2327).
///
/// The definition is the per-symbol document (`name` + `kind`) whose name
/// matches exactly one content term and whose text matches no other. Hits
/// that cover more terms keep their relative order, then the sparse
/// definitions follow in theirs. A one- or two-term query is a lookup and
/// is left alone, including two definitions that share one name. When no
/// hit covers a second term, the page is unchanged.
pub fn demote_sparse_definition_hits(hits: &mut [Value], query: &str) {
    let terms = content_terms(query);
    if terms.len() < MIN_TERMS_FOR_COVERAGE || hits.len() < 2 {
        return;
    }
    let covered: Vec<usize> = hits
        .iter()
        .map(|hit| terms_covered(&hit_haystack(hit), &terms))
        .collect();
    if covered.iter().copied().max().unwrap_or(0) < 2 {
        return;
    }
    let sparse: Vec<bool> = hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let src = hit.get("_source").unwrap_or(&Value::Null);
            let Some(name) = src.get("name").and_then(Value::as_str) else {
                return false;
            };
            if name.is_empty() || src.get("kind").and_then(Value::as_str).is_none() {
                return false;
            }
            let name_l = name.to_lowercase();
            let name_hits = terms.iter().filter(|t| name_l.contains(t.as_str())).count();
            name_hits == 1 && covered[i] == 1
        })
        .collect();
    if sparse.iter().all(|flag| !flag) {
        return;
    }
    let mut order = Vec::with_capacity(hits.len());
    for (i, flag) in sparse.iter().enumerate() {
        if !flag {
            order.push(i);
        }
    }
    if order.is_empty() {
        return;
    }
    for (i, flag) in sparse.iter().enumerate() {
        if *flag {
            order.push(i);
        }
    }
    let original = hits.to_vec();
    for (dest, src) in order.into_iter().enumerate() {
        hits[dest] = original[src].clone();
    }
}

/// One RRF leg at rank 1: `1/(k+1)`. Two legs at rank 1 — a typical strong
/// hybrid hit — score twice that. k is [`super::RRF_K`] (60), the value the
/// request sends.
fn rrf_one_leg_rank1() -> f64 {
    1.0 / (super::RRF_K as f64 + 1.0)
}

/// The page is a weak match when the best passage does not cover the query,
/// or when a hybrid/RRF top score is below one leg at rank 1.
///
/// Coverage: the query has at least [`MIN_TERMS_FOR_COVERAGE`] content terms
/// and the best hit's text contains strictly fewer than half of them
/// (`matched * 2 < terms`). That is the nearest-neighbour page, whose RRF
/// scores sit next to a real hit's (rank 1 on both legs is `2/(k+1)` either
/// way), and the one-term collision (`recall` inside `recall@k`).
///
/// Score: with `rrf_scores`, a top score strictly below [`rrf_one_leg_rank1`]
/// is under half of the two-leg rank-1 score. BM25 has no such bar — its
/// magnitude is the corpus's IDF — so a BM25 page is weak only on coverage,
/// and a passage is marked on its own only by [`passage_is_score_weak`].
fn page_is_weak_match(hits: &[Value], opts: &RenderOpts, top: f64) -> bool {
    if opts.rrf_scores && top > 0.0 && top < rrf_one_leg_rank1() {
        return true;
    }
    let terms = content_terms(opts.query);
    if terms.len() < MIN_TERMS_FOR_COVERAGE {
        return false;
    }
    let best = hits
        .iter()
        .map(|hit| terms_covered(&hit_haystack(hit), &terms))
        .max()
        .unwrap_or(0);
    best * 2 < terms.len()
}

/// `file:line` when the record or a lexical `_passage` carries one.
/// Otherwise a line-unknown header that says why and how to get a line —
/// a bare path reads as a finished citation, and a semantic chunk ordinal
/// is not a line.
#[cfg(test)]
fn citation(hit: &Value, lexical_lines: bool) -> String {
    citation_in(hit, lexical_lines, None, "")
}

/// Why a header has no line, and the command that puts one there.
///
/// A pack built before file lines were stored (no `line` / `start_line`, no
/// `symbols[].line`, no `code:<line>:<name>` locator) and with no checkout
/// to search used to print a bare `(line unknown)`. The footer still asks
/// the agent to cite file:line, so the header names the gap and the fix.
const LINE_UNKNOWN_HINT: &str = "no file line on this record or in the corpus checkout; re-pull the pack or rebuild with xerj corpus index <name> --fresh";

fn line_unknown(path: &str, explain: bool) -> String {
    if explain {
        format!("{path} (line unknown — {LINE_UNKNOWN_HINT})")
    } else {
        format!("{path} (line unknown)")
    }
}

/// [`citation`] with the corpus checkout and the query that selected the hit.
///
/// A prose section record (it has a `section` field, or an `s<N>` locator)
/// never turns its passage ordinal into a file line: that ordinal counts
/// lines of the re-flowed section, so every Markdown hit used to read
/// `file:1` or `file (line unknown)` (issue #1892). With the checkout, the
/// passage's first distinctive line is looked up in the file.
///
/// A whole-file code record (Python `.py`, locator `code`) has no top-level
/// line. Its matching line is the lexical `_passage` ordinal when that
/// passage is the source text. When the winning passage is the synthetic
/// `defs` / `title` / `name` list — the usual case for a long module whose
/// symbol names pack the query into one short field — that ordinal is not a
/// file line (issue #2195). The same record's `symbols[].line`, or the
/// passage text located in the checkout, is.
fn citation_in(
    hit: &Value,
    lexical_lines: bool,
    checkout: Option<&std::path::Path>,
    query: &str,
) -> String {
    let src = hit.get("_source").unwrap_or(&Value::Null);
    let path = ["ax_path", "ax_file", "path", "file"]
        .iter()
        .find_map(|k| src.get(*k).and_then(Value::as_str))
        .unwrap_or("?");
    let stored = ["line", "start_line", "lineno"]
        .iter()
        .find_map(|k| src.get(*k).and_then(Value::as_u64))
        .filter(|n| *n != 0);
    let line = stored.or_else(|| line_from_code_locator(src)).or_else(|| {
        if is_prose_section(src) {
            checkout.and_then(|dir| line_in_checkout(hit, src, dir, path))
        } else {
            lexical_passage_line(hit, lexical_lines)
                .or_else(|| symbol_file_line(src, query))
                .or_else(|| {
                    checkout.and_then(|dir| source_passage_line_in_checkout(hit, dir, path))
                })
                .or_else(|| symbol_named_in_passage(src, hit))
        }
    });
    match line {
        Some(n) => format!("{path}:{n}"),
        // A path that would leave the checkout is never opened, and it is
        // not a pack that a rebuild would fix.
        None => line_unknown(path, rel_stays_in_checkout(path)),
    }
}

/// `code:<line>:<name>` — the per-declaration locator. The line is the
/// file line even when the `line` field itself was not stored.
fn line_from_code_locator(src: &Value) -> Option<u64> {
    let loc = src.get("ax_locator").and_then(Value::as_str)?;
    let rest = loc.strip_prefix("code:")?;
    let (num, _) = rest.split_once(':')?;
    let n: u64 = num.parse().ok()?;
    (n != 0).then_some(n)
}

/// File line of the definition [`super::passage::symbol_passage`] would
/// show for `query`. That is the line on `symbols[].line`, which a
/// whole-file code record stores even when the header's passage came from
/// the `defs` list. The label format is `{kind} {name} @ line {start}`.
fn symbol_file_line(src: &Value, query: &str) -> Option<u64> {
    if query.is_empty() {
        return None;
    }
    let body = src
        .get("body")
        .and_then(Value::as_str)
        .or_else(|| src.get("text").and_then(Value::as_str))
        .unwrap_or("");
    if body.is_empty() {
        return None;
    }
    let (_, label) = symbol_passage(body, src, query, usize::MAX)?;
    let n = label.rsplit_once("@ line ")?.1.parse::<u64>().ok()?;
    (n != 0).then_some(n)
}

/// A `symbols[].line` whose name occurs in the passage text. Used when the
/// body is not on the hit (so [`symbol_file_line`] cannot score a span) and
/// the checkout does not hold the file. The earliest name in the passage
/// wins; equal positions keep the earlier symbol, which the extractor emits
/// in source order.
fn symbol_named_in_passage(src: &Value, hit: &Value) -> Option<u64> {
    let text = hit
        .pointer("/fields/_passage/0/text")
        .and_then(Value::as_str)?;
    let syms = src.get("symbols").and_then(Value::as_array)?;
    let mut found: Option<(usize, u64)> = None;
    for s in syms {
        let Some(name) = s.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(line) = s.get("line").and_then(Value::as_u64).filter(|n| *n != 0) else {
            continue;
        };
        if name.chars().count() < 3 {
            continue;
        }
        // Whole identifier only: `per` must not match inside `get_permissions`.
        if let Some(pos) = identifier_at(text, name) {
            if found.is_none_or(|(at, _)| pos < at) {
                found = Some((pos, line));
            }
        }
    }
    found.map(|(_, line)| line)
}

/// Byte index of `name` in `text` where both sides are identifier boundaries
/// (start/end, or a character that is not ASCII alphanumeric or `_`).
fn identifier_at(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(pos) = text[from..].find(name) {
        let abs = from + pos;
        let end = abs + name.len();
        // `find` returns a char boundary and the match is exactly `name`, so
        // `end` is a char boundary too.
        let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        if before_ok && after_ok {
            return Some(abs);
        }
        from = end;
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A record cut from prose by the document extractor: `section` is stamped on
/// multi-section documents, and the locator is `s<N>` (or `<prefix>-s<N>`).
fn is_prose_section(src: &Value) -> bool {
    if src.get("section").is_some() {
        return true;
    }
    let loc = src.get("ax_locator").and_then(Value::as_str).unwrap_or("");
    let tail = loc.rsplit('-').next().unwrap_or(loc);
    tail.len() > 1 && tail.starts_with('s') && tail[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A line distinctive enough to look up: at least 12 non-space characters.
fn distinctive_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|l| l.chars().filter(|c| !c.is_whitespace()).count() >= 12)
}

/// True when `rel` is a relative path with only normal components, so joining
/// it onto the checkout cannot escape that directory.
fn rel_stays_in_checkout(rel: &str) -> bool {
    let rel_path = std::path::Path::new(rel);
    !rel_path.is_absolute()
        && rel_path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// 1-based line of `needle` in the checked-out file. `None` when the path
/// would leave the checkout, the file is not there, or the text is absent.
fn line_of_needle(dir: &std::path::Path, rel: &str, needle: &str) -> Option<u64> {
    if !rel_stays_in_checkout(rel) {
        return None;
    }
    let file = std::fs::read_to_string(dir.join(rel)).ok()?;
    file.lines()
        .position(|l| l.contains(needle))
        .map(|i| i as u64 + 1)
}

/// 1-based line of the passage (or, without one, of the section start) in the
/// checked-out file. `None` when the file is not there, the path would leave
/// the checkout, or the text is not found verbatim.
fn line_in_checkout(hit: &Value, src: &Value, dir: &std::path::Path, rel: &str) -> Option<u64> {
    let passage = hit
        .pointer("/fields/_passage/0/text")
        .and_then(Value::as_str)
        .and_then(distinctive_line);
    let section = src
        .get("body")
        .and_then(Value::as_str)
        .and_then(distinctive_line);
    let needle = passage.or(section)?;
    line_of_needle(dir, rel, needle)
}

/// Locate a code passage in the checkout. Only a passage taken from source
/// text (`body`, `text`, `code`, `impl`, or an unnamed field) is searched:
/// a `defs` / `title` / `name` passage is a synthetic list and is not a line
/// of the file. The whole `body` is not searched either — its first line is
/// the file head, not the match.
fn source_passage_line_in_checkout(hit: &Value, dir: &std::path::Path, rel: &str) -> Option<u64> {
    let field = hit
        .pointer("/fields/_passage/0/field")
        .and_then(Value::as_str);
    if matches!(field, Some("defs" | "title" | "name" | "defs_expanded")) {
        return None;
    }
    let needle = hit
        .pointer("/fields/_passage/0/text")
        .and_then(Value::as_str)
        .and_then(distinctive_line)?;
    line_of_needle(dir, rel, needle)
}

/// 1-based file line from a lexical passage. `ordinal` is a chunk index
/// on a semantic hit, so this stays `None` unless the caller set
/// `lexical_lines`. `defs` / `defs_expanded` / `title` / `name` ordinals are
/// lines of a synthetic list, not of the file, and a passage without
/// `start_offset` has no window to cite.
fn lexical_passage_line(hit: &Value, lexical_lines: bool) -> Option<u64> {
    if !lexical_lines {
        return None;
    }
    let first = hit.pointer("/fields/_passage/0")?.as_object()?;
    if matches!(
        first.get("field").and_then(Value::as_str),
        Some("defs" | "title" | "name" | "defs_expanded")
    ) {
        return None;
    }
    first.get("start_offset")?;
    first.get("ordinal").and_then(Value::as_u64).map(|o| o + 1)
}

fn render_hit(
    h: &Value,
    licences: &HashMap<String, String>,
    opts: &RenderOpts,
    weak: bool,
) -> String {
    let src = h.get("_source").cloned().unwrap_or(Value::Null);
    let loc = citation_in(h, opts.lexical_lines, opts.checkout, opts.query);
    let score = hit_score(h);
    let lic = licences
        .get(locator_repo(&loc))
        .cloned()
        .unwrap_or_default();
    // The header still names `lic` (including UNKNOWN / NONE-FOUND). The
    // banner and the strict withhold follow the copy-warning rule, which
    // drops only an unidentified licence on an own corpus.
    let restricted = licence::copy_warning_applies(&lic, opts.own);

    if opts.meatl {
        let shown = if opts.rrf_scores {
            format!("rrf={score:.4}")
        } else {
            format!("score={score:.2}")
        };
        let why = if lic.is_empty() {
            String::new()
        } else {
            format!(" why={lic}")
        };
        let weak_tok = if weak { " weak" } else { "" };
        let archived_tok = if source_looks_archived(&src) {
            " archived"
        } else {
            ""
        };
        return format!("@ok f={loc} {shown}{why}{weak_tok}{archived_tok}\n");
    }

    let shown = if opts.rrf_scores {
        format!("rrf {score:.4}")
    } else {
        format!("score {score:.2}")
    };
    let lic_part = if lic.is_empty() {
        String::new()
    } else {
        format!(", {lic}")
    };
    let weak_part = if weak { ", weak" } else { "" };
    let archived_part = if source_looks_archived(&src) {
        ", archived"
    } else {
        ""
    };
    let mut out = format!("\n─── {loc}  ({shown}{lic_part}{weak_part}{archived_part})\n");

    // `body` is the content field of every family EXCEPT plain text (`.txt`
    // mirrors put their passage in `text`, #1139) and `*-symbols` records
    // (which store the declaration in `code`). `code` is trusted ONLY on a
    // record with the symbol shape (`name` + `kind` + numeric `line`): a
    // raw-JSON record (#1158) may carry its own short `code` field (an error
    // code, a CWE id) that must not displace its real content. A raw-JSON
    // record's text lives in schema-named fields (`summary`, `details`, …),
    // so the last resort is the longest non-`ax_*` string field.
    let body = src
        .get("body")
        .and_then(Value::as_str)
        .or_else(|| src.get("text").and_then(Value::as_str))
        .or_else(|| {
            is_symbol_record(&src)
                .then(|| src.get("code").and_then(Value::as_str))
                .flatten()
        })
        .map(str::to_string)
        .unwrap_or_else(|| longest_string_field(&src));

    if opts.full > 0 && !(restricted && opts.strict_licence) {
        // Prefer the matching DEFINITION over a byte window; fall back to the
        // window only when the record carries no usable symbols (a data
        // file, or a language whose extractor found nothing — issue #170).
        let sp = if opts.no_symbol {
            None
        } else {
            symbol_passage(&body, &src, opts.query, opts.full)
        };
        match sp {
            Some((text, label)) => {
                out.push_str(&format!(
                    "    [{label} — {} of {} chars]\n",
                    commaus(py_len(&text)),
                    commaus(py_len(&body))
                ));
                out.push_str(&text);
                out.push('\n');
            }
            None => {
                let (window, start, total) = best_window(&body, opts.query, opts.full);
                if total > py_len(&window) {
                    out.push_str(&format!(
                        "    [no symbol match; showing {} of {} chars from offset {}, \
                         window centred on the match]\n",
                        commaus(py_len(&window)),
                        commaus(total),
                        commaus(start)
                    ));
                }
                out.push_str(&window);
                out.push('\n');
            }
        }
    } else if opts.full > 0 {
        // strict licence policy: the code itself is the thing not to hand
        // over; provenance and the warning stay.
        out.push_str("    [passage withheld: restricted licence (licence_policy strict)]\n");
    } else {
        // Only reachable via `--full 0`. No highlight is ever requested
        // (issue #177 — it reorders hits), so this is the file head and is
        // labelled as such rather than passed off as a matching passage.
        let head = take_chars(&body, 400);
        if py_len(&body) > py_len(&head) {
            out.push_str(&format!(
                "    [file head; {} of {} chars — pass --full N for the matching definition]\n",
                commaus(py_len(&head)),
                commaus(py_len(&body))
            ));
        }
        out.push_str(&format!("    {}\n", head.replace('\n', "\n    ")));
    }

    if restricted {
        out.push_str(&licence::hit_warning_line(&lic));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_copies_cite_the_live_name_not_the_backup() {
        // #2402: the four names of one byte-identical file. The canonical
        // (lowest path) is the archived backup; the hit must cite docs/.
        let mut resp = json!({"hits":{"hits":[{"_source":{
            "ax_path":"archive/backup-cn/root-bak-20261001/models.md",
            "path":"archive/backup-cn/root-bak-20261001/models.md",
            "ax_paths":[
                "archive/backup-cn/root-bak-20261001/models.md",
                "docs/models.md",
                "docs/models.md.orig",
                "evidence/backups/models.md.bak"
            ],
            "ax_file":"axf2-x"
        }}]}});
        assert!(prefer_live_alias_paths(&mut resp));
        let src = &resp["hits"]["hits"][0]["_source"];
        assert_eq!(src["ax_path"], "docs/models.md");
        assert_eq!(src["path"], "docs/models.md");
        assert_eq!(src["ax_paths"].as_array().unwrap().len(), 4);
        assert_eq!(src["ax_file"], "axf2-x");
        assert!(!source_looks_archived(src));
    }

    #[test]
    fn live_canonical_or_all_copies_are_left_alone() {
        let mut live = json!({"hits":{"hits":[{"_source":{
            "ax_path":"docs/a.md","ax_paths":["docs/a.md","backups/a.md"]}}]}});
        assert!(!prefer_live_alias_paths(&mut live));
        assert_eq!(live["hits"]["hits"][0]["_source"]["ax_path"], "docs/a.md");
        let mut copies = json!({"hits":{"hits":[{"_source":{
            "ax_path":"backups/a.md","ax_paths":["backups/a.md","old/a.md.bak"]}}]}});
        assert!(!prefer_live_alias_paths(&mut copies));
        let mut single = json!({"hits":{"hits":[{"_source":{"ax_path":"backups/a.md"}}]}});
        assert!(!prefer_live_alias_paths(&mut single));
    }

    #[test]
    fn search_bodies_fetch_every_alias_name() {
        // Without `ax_paths` in `_source` the hit never sees the live name
        // and keeps citing the backup copy (#2402).
        for body in [
            crate::code_query::search_query_body("GGUF quantized weights", 5),
            crate::code_query::search_query_body_for_mapping("GGUF", 5, None, None),
        ] {
            let source = body["_source"].as_array().expect("_source list");
            assert!(source.iter().any(|f| f == "ax_paths"), "{body}");
            assert!(source.iter().any(|f| f == "ax_path"), "{body}");
        }
    }

    #[test]
    fn copy_names_are_recognised_without_catching_ordinary_code() {
        for p in [
            "evidence/backups/x.md.bak",
            "a/备份/root-bak-20261001/x.md",
            "notes.md~",
            "src/lib.rs.orig",
            "archive/x.md",
        ] {
            assert!(path_looks_like_copy(p), "{p}");
        }
        for p in [
            "src/backup.rs",
            "docs/models.md",
            "crates/bakery/src/lib.rs",
            "src/old_api.rs",
        ] {
            assert!(!path_looks_like_copy(p), "{p}");
        }
    }

    fn hit(loc_fields: Value, body: &str, symbols: Value, score: f64) -> Value {
        let mut src = loc_fields;
        src["body"] = json!(body);
        if !symbols.is_null() {
            src["symbols"] = symbols;
        }
        json!({ "_source": src, "_score": score })
    }

    fn apache_hit() -> Value {
        hit(
            json!({ "ax_path": "tantivy/src/reader.rs", "line": 30 }),
            "impl SegmentReader {\n    pub fn rebuild(&self) {}\n}\n",
            json!([{ "name": "rebuild", "kind": "method", "line": 2 }]),
            12.345,
        )
    }

    fn agpl_hit() -> Value {
        hit(
            json!({ "ax_path": "elasticsearch/server/src/main/Node.java", "line": 80 }),
            "class Node {\n    void start() {}\n}\n",
            json!([{ "name": "start", "kind": "method", "line": 2 }]),
            9.01,
        )
    }

    fn licences() -> HashMap<String, String> {
        HashMap::from([
            ("tantivy".to_string(), "Apache-2.0/MIT".to_string()),
            ("elasticsearch".to_string(), "AGPL".to_string()),
        ])
    }

    #[test]
    fn prose_hits_carry_loc_score_licence_passage_and_footer() {
        let hits = vec![apache_hit()];
        let text = render(
            &hits,
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "rebuild segment reader",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: Some(3),
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            text.starts_with("\n─── tantivy/src/reader.rs:30  (score 12.35, Apache-2.0/MIT)\n"),
            "{text}"
        );
        assert!(text.contains("    [method rebuild @ line 2 — "), "{text}");
        assert!(
            text.contains("1 passages from 'xerj-search' (index 3d old)"),
            "{text}"
        );
        assert!(text.ends_with("Cite file:line for anything you rely on.\n"));
        // Compatible licence: no warning line.
        assert!(!text.contains("!!"), "{text}");
    }

    #[test]
    fn restricted_hits_carry_the_warning_under_the_passage() {
        let text = render(
            &[agpl_hit()],
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "start node",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        let passage_at = text.find("void start()").expect("passage present");
        let warn_at = text
            .find("    !! AGPL: adapt the APPROACH, do not copy the code")
            .expect("warning");
        assert!(warn_at > passage_at, "the warning sits UNDER the passage");
        assert!(text.contains("(score 9.01, AGPL)"), "{text}");
    }

    /// #2339: an own corpus keeps UNKNOWN / NONE-FOUND in the header and drops
    /// the do-not-copy banner. A named restricted licence still warns, and
    /// strict mode still withholds that named licence's passage. Strict mode
    /// does not withhold the owner's unidentified passage.
    #[test]
    fn own_corpus_omits_the_banner_for_an_unidentified_licence_only() {
        let unknown = hit(
            json!({ "ax_path": "mine/src/lib.rs", "line": 4 }),
            "fn mine() {}\n",
            json!(null),
            3.5,
        );
        let map = HashMap::from([("mine".to_string(), "NONE-FOUND".to_string())]);
        let mut opts = RenderOpts {
            corpus: "mine",
            query: "fn mine",
            full: 800,
            no_symbol: true,
            meatl: false,
            rrf_scores: false,
            age_days: None,
            strict_licence: false,
            own: true,
            lexical_lines: false,
            checkout: None,
        };
        let text = render(std::slice::from_ref(&unknown), &map, &opts, None);
        assert!(text.contains("(score 3.50, NONE-FOUND)"), "{text}");
        assert!(text.contains("fn mine()"), "{text}");
        assert!(!text.contains("!!"), "{text}");
        assert!(!text.contains("do not copy"), "{text}");

        opts.own = false;
        let warned = render(std::slice::from_ref(&unknown), &map, &opts, None);
        assert!(warned.contains("(score 3.50, NONE-FOUND)"), "{warned}");
        assert!(
            warned.contains("!! NONE-FOUND: adapt the APPROACH, do not copy the code"),
            "{warned}"
        );

        opts.own = true;
        opts.strict_licence = true;
        let strict_own = render(std::slice::from_ref(&unknown), &map, &opts, None);
        assert!(strict_own.contains("fn mine()"), "{strict_own}");
        assert!(!strict_own.contains("!!"), "{strict_own}");

        opts.strict_licence = true;
        let strict_agpl = render(&[agpl_hit()], &licences(), &opts, None);
        assert!(!strict_agpl.contains("void start()"), "{strict_agpl}");
        assert!(
            strict_agpl.contains("!! AGPL: adapt the APPROACH, do not copy the code"),
            "{strict_agpl}"
        );
        assert!(strict_agpl.contains("(score 9.01, AGPL)"), "{strict_agpl}");
    }

    /// #1158: a raw-JSON record has neither `body` nor `text` — its content
    /// lives in schema-named fields. The passage must come from the longest
    /// string field (the advisory's `details`), and a tie on length goes to
    /// the alphabetically first name so the passage is stable regardless of
    /// key order on the wire.
    #[test]
    fn a_raw_json_hit_renders_its_longest_string_field_as_the_passage() {
        let raw_hit = json!({
            "_score": 7.5,
            "_source": {
                "ax_path": "ghsa-db/advisories/GHSA-9j.json",
                "id": "GHSA-9j",
                "summary": "lodash command injection",
                "details": "Applications using lodash are vulnerable to command injection via the template function."
            }
        });
        let text = render(
            &[raw_hit],
            &HashMap::new(),
            &RenderOpts {
                corpus: "ghsa-db",
                query: "command injection",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            text.contains("vulnerable to command injection"),
            "the passage is `details`, the longest field: {text}"
        );
        assert!(
            !text.contains("[0 of 0 chars"),
            "no empty-passage shape: {text}"
        );

        // Tie: two fields of equal length — the alphabetically first wins.
        let tie = json!({
            "_score": 1.0,
            "_source": {
                "ax_path": "x/y.json",
                "zzz_field": "same length exactly",
                "aaa_field": "same length exactly"
            }
        });
        let text = render(
            &[tie],
            &HashMap::new(),
            &RenderOpts {
                corpus: "kv",
                query: "length",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(text.contains("aaa_field") || text.contains("same length exactly"));
        // Both same content here — the deterministic pick is still ONE field.
        assert!(text.contains("same length exactly"), "{text}");
    }

    #[test]
    fn symbol_index_hit_and_short_raw_json_hit_never_render_ax_provenance_as_passage() {
        // Autoindex `*-symbols` records store the declaration in `code` (with
        // no `body`, `text`, or `symbols` array) alongside a 42-char `ax_file`
        // provenance digest (`axf2-<32-hex>-<size-hex>`).
        let sym_hit = json!({
            "_score": 11.2,
            "_source": {
                "ax_file": "axf2-1fbdaa51d7231323867e3200dc599463-1075",
                "ax_path": "tantivy/src/error.rs",
                "title": "enum TantivyError — error.rs",
                "path": "tantivy/src/error.rs",
                "language": "rust",
                "name": "TantivyError",
                "kind": "enum",
                "line": 15,
                "code": "pub enum TantivyError {"
            }
        });
        let text = render(
            &[sym_hit],
            &licences(),
            &RenderOpts {
                corpus: "tantivy",
                query: "TantivyError",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            text.contains("─── tantivy/src/error.rs:15  (score 11.20, Apache-2.0/MIT)\n"),
            "{text}"
        );
        assert!(
            text.contains("pub enum TantivyError {"),
            "symbol hit must render `code`, not `ax_file`: {text}"
        );
        assert!(
            !text.contains("axf2-"),
            "internal `ax_file` digest must never be rendered as passage: {text}"
        );

        // A raw-JSON record whose content field is shorter than the 42-char
        // `ax_file` digest must still render the content field, never `ax_*`.
        let short_raw = json!({
            "_score": 4.0,
            "_source": {
                "ax_file": "axf2-1fbdaa51d7231323867e3200dc599463-1075",
                "ax_path": "notes/short.json",
                "summary": "short advisory note"
            }
        });
        let raw_text = render(
            &[short_raw],
            &HashMap::new(),
            &RenderOpts {
                corpus: "notes",
                query: "advisory",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            raw_text.contains("short advisory note"),
            "short raw-JSON record must render `summary`, not `ax_file`: {raw_text}"
        );
        assert!(
            !raw_text.contains("axf2-"),
            "internal `ax_file` digest must never win longest_string_field: {raw_text}"
        );
    }

    /// #1158 must survive the symbol-`code` passage source: a raw-JSON
    /// record with its OWN short `code` field (here a CWE id) still renders
    /// its longest content field, not the code.
    #[test]
    fn a_raw_json_hit_with_a_short_code_field_still_renders_its_content() {
        let raw_hit = json!({
            "_score": 3.0,
            "_source": {
                "ax_file": "axf2-1fbdaa51d7231323867e3200dc599463-1075",
                "ax_path": "cwe/records/79.json",
                "code": "CWE-79",
                "name": "XSS",
                "details": "Improper neutralization of input during web page generation allows cross-site scripting."
            }
        });
        let text = render(
            &[raw_hit],
            &HashMap::new(),
            &RenderOpts {
                corpus: "cwe",
                query: "cross-site scripting",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            text.contains("allows cross-site scripting"),
            "the passage is `details`, not the short `code`: {text}"
        );
        assert!(!text.contains("axf2-"), "{text}");
    }

    #[test]
    fn strict_licence_strips_the_passage_but_keeps_provenance_and_warning() {
        let text = render(
            &[agpl_hit()],
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "start node",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: true,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            !text.contains("void start()"),
            "no code handed over: {text}"
        );
        assert!(
            text.contains("─── elasticsearch/server/src/main/Node.java:80"),
            "{text}"
        );
        assert!(text.contains("!! AGPL"), "{text}");
        // Compatible corpora are untouched by strict mode.
        let ok = render(
            &[apache_hit()],
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "rebuild",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: true,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(ok.contains("pub fn rebuild"), "{ok}");
    }

    #[test]
    fn full_zero_is_the_labelled_file_head() {
        let text = render(
            &[apache_hit()],
            &licences(),
            &RenderOpts {
                corpus: "c",
                query: "rebuild",
                full: 0,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            text.contains(
                "[file head; 58 of 58 chars — pass --full N for the matching definition]"
            ) || text.contains("impl SegmentReader {"),
            "{text}"
        );
        // head only, no definition chase
        assert!(!text.contains("@ line"), "{text}");
    }

    #[test]
    fn meatl_is_one_line_per_hit_with_licence_and_mode_notes() {
        let hits = vec![agpl_hit(), apache_hit()];
        let text = render(
            &hits,
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "start",
                full: 800,
                no_symbol: false,
                meatl: true,
                rrf_scores: false,
                age_days: Some(2),
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            Some("hybrid RRF(k=60) — BM25 over 9 index(es), vector over 3 of 9"),
        );
        assert!(
            text.starts_with(
                "@mode hybrid RRF(k=60) — BM25 over 9 index(es), vector over 3 of 9\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("@ok f=elasticsearch/server/src/main/Node.java:80 score=9.01 why=AGPL\n"),
            "{text}"
        );
        assert!(
            text.contains("@ok f=tantivy/src/reader.rs:30 score=12.35 why=Apache-2.0/MIT\n"),
            "{text}"
        );
        assert!(
            !text.contains("passages from"),
            "no footer in meatl: {text}"
        );
    }

    #[test]
    fn rrf_scores_render_four_decimals_without_per_leg_ranks() {
        let text = render(
            &[agpl_hit()],
            &licences(),
            &RenderOpts {
                corpus: "c",
                query: "q",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: true,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(text.contains("(rrf 9.0100, AGPL)"), "{text}");
        assert!(!text.contains("[bm25 #"), "per-leg ranks are gone: {text}");
    }

    #[test]
    fn no_match_prose_and_meatl_shapes_are_verbatim() {
        assert_eq!(
            no_match_text("kv", "two-way substring", false),
            "No passage in 'kv' matches: two-way substring\n\
             The corpus is likely wrong for this task — fall back to normal work rather \
             than forcing a bad match.\n"
        );
        assert_eq!(
            no_match_text("kv", "xyzzy", true),
            "@no q=\"xyzzy\" why=no-match-in-corpus\n"
        );
    }

    #[test]
    fn a_bm25_body_hit_cites_the_passage_line_and_a_bare_path_says_unknown() {
        let whole_file = json!({
            "_score": 4.2,
            "_source": { "ax_path": "pr-agent/pr_agent/algo/inline_comment_dedup.py", "body": "def dedupe():\n    pass\n" },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 41,
                "start_offset": 1200,
                "end_offset": 1300,
                "text": "def dedupe():\n    pass"
            }]}
        });
        assert_eq!(
            citation(&whole_file, true),
            "pr-agent/pr_agent/algo/inline_comment_dedup.py:42"
        );
        let text = render(
            &[whole_file],
            &HashMap::new(),
            &RenderOpts {
                corpus: "qabot-review",
                query: "dedupe",
                full: 0,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: true,
                checkout: None,
            },
            None,
        );
        assert!(
            text.contains("─── pr-agent/pr_agent/algo/inline_comment_dedup.py:42"),
            "{text}"
        );

        let bare = json!({ "_source": { "ax_path": "readme.md", "body": "hello" } });
        assert_eq!(citation(&bare, true), line_unknown("readme.md", true));

        // A semantic chunk ordinal is not a file line, even with start_offset.
        let semantic = json!({
            "_source": { "ax_path": "readme.md" },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 3,
                "start_offset": 10,
                "text": "chunk"
            }]}
        });
        assert_eq!(citation(&semantic, false), line_unknown("readme.md", true));

        let defs = json!({
            "_source": { "ax_path": "a.rs" },
            "fields": { "_passage": [{
                "field": "defs",
                "ordinal": 0,
                "start_offset": 0,
                "text": "fn commit"
            }]}
        });
        assert_eq!(citation(&defs, true), line_unknown("a.rs", true));
    }

    #[test]
    fn prose_section_cites_the_file_line_from_the_checkout() {
        // issue #1892: Markdown sections printed `(line unknown)`, or `:1` from a
        // section-relative passage ordinal, while the footer asked for file:line.
        let dir = std::env::temp_dir().join(format!("xc-prose-lines-{}", std::process::id()));
        let doc = dir.join("book/Concurrency.md");
        std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
        std::fs::write(
            &doc,
            "# Concurrency\n\nIntro paragraph.\n\n```swift\nasync let firstPhoto = downloadPhoto(named: photoNames[0])\n```\n",
        )
        .unwrap();
        let hit = json!({
            "_source": { "ax_path": "book/Concurrency.md", "ax_locator": "s3", "section": 3,
                         "body": "Intro paragraph.\n\nasync let firstPhoto = downloadPhoto(named: photoNames[0])" },
            "fields": { "_passage": [{ "field": "body", "ordinal": 0, "start_offset": 18,
                         "text": "async let firstPhoto = downloadPhoto(named: photoNames[0])" }]}
        });
        assert_eq!(
            citation_in(&hit, true, Some(&dir), ""),
            "book/Concurrency.md:6"
        );
        // without the checkout a section-relative ordinal is never passed off as a line
        assert_eq!(
            citation_in(&hit, true, None, ""),
            line_unknown("book/Concurrency.md", true)
        );
        // a single-section file (locator s0, no `section` field) is prose too
        let single = json!({
            "_source": { "ax_path": "book/Concurrency.md", "ax_locator": "s0", "body": "Intro paragraph that is long." },
            "fields": { "_passage": [{ "field": "body", "ordinal": 0, "start_offset": 0, "text": "Intro paragraph." }]}
        });
        assert_eq!(
            citation_in(&single, true, Some(&dir), ""),
            "book/Concurrency.md:3"
        );
        // a path that would leave the checkout is not opened, and the header
        // does not suggest rebuilding to cite it
        let escape = json!({ "_source": { "ax_path": "../etc/passwd", "ax_locator": "s0", "body": "root:x:0:0:root:/root" } });
        assert_eq!(
            citation_in(&escape, true, Some(&dir), ""),
            "../etc/passwd (line unknown)"
        );
        // code records keep the lexical ordinal line
        let code = json!({
            "_source": { "ax_path": "a.rs", "ax_locator": "code" },
            "fields": { "_passage": [{ "field": "body", "ordinal": 41, "start_offset": 9, "text": "fn x()" }]}
        });
        assert_eq!(citation_in(&code, true, Some(&dir), ""), "a.rs:42");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// issue #2195: a hub-corpus Python hit whose winning passage is the
    /// synthetic `defs` list printed `(line unknown)` even though the same
    /// record stores `symbols[].line` and the checkout holds the source.
    #[test]
    fn python_passage_cites_a_real_start_line() {
        let body =
            "class EC2:\n    pass\n\ndef get_permissions(self):\n    return ['ec2:Describe']\n";
        let defs_hit = json!({
            "_source": {
                "ax_path": "c7n/resources/ec2.py",
                "ax_locator": "code",
                "language": "python",
                "defs": "class EC2\nfunction get_permissions",
                "body": body,
                "symbols": [
                    {"name": "EC2", "kind": "class", "line": 1, "end_line": 2},
                    {"name": "get_permissions", "kind": "function", "line": 4, "end_line": 5}
                ]
            },
            "fields": { "_passage": [{
                "field": "defs",
                "ordinal": 1,
                "start_offset": 10,
                "text": "function get_permissions"
            }]}
        });
        assert_eq!(
            citation_in(&defs_hit, true, None, "get_permissions webhook"),
            "c7n/resources/ec2.py:4"
        );
        // A body passage line stays the passage start, not the symbol the
        // query names. ordinal 3 is the `def` line.
        let precise = json!({
            "_source": {
                "ax_path": "c7n/resources/ec2.py",
                "ax_locator": "code",
                "language": "python",
                "body": body,
                "symbols": [{"name": "EC2", "kind": "class", "line": 1, "end_line": 2}]
            },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 3,
                "start_offset": 18,
                "text": "def get_permissions(self):"
            }]}
        });
        assert_eq!(
            citation_in(&precise, true, None, "EC2"),
            "c7n/resources/ec2.py:4"
        );
        // No symbol table: a semantic ordinal is not a line, but the passage
        // text is in the checkout.
        let dir = std::env::temp_dir().join(format!("xc-py-lines-{}", std::process::id()));
        let file = dir.join("c7n/resources/ec2.py");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, body).unwrap();
        let semantic = json!({
            "_source": {
                "ax_path": "c7n/resources/ec2.py",
                "ax_locator": "code",
                "language": "python",
                "body": body
            },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 0,
                "start_offset": 20,
                "text": "def get_permissions(self):"
            }]}
        });
        assert_eq!(
            citation_in(&semantic, false, Some(&dir), "get_permissions"),
            "c7n/resources/ec2.py:4"
        );
        // A declaration record whose `line` field was dropped still cites the
        // line baked into `code:<line>:<name>`.
        let located = json!({
            "_source": {
                "ax_path": "c7n/resources/ec2.py",
                "ax_locator": "code:4:get_permissions",
                "language": "python",
                "name": "get_permissions",
                "kind": "function"
            }
        });
        assert_eq!(
            citation_in(&located, true, None, ""),
            "c7n/resources/ec2.py:4"
        );
        // Body omitted from the hit: the defs passage still names the symbol,
        // and that symbol's stored line is the file line.
        let no_body = json!({
            "_source": {
                "ax_path": "c7n/resources/ec2.py",
                "ax_locator": "code",
                "symbols": [
                    {"name": "per", "kind": "function", "line": 9},
                    {"name": "get_permissions", "kind": "function", "line": 4}
                ]
            },
            "fields": { "_passage": [{
                "field": "defs",
                "ordinal": 1,
                "start_offset": 10,
                "text": "helper get_permissions"
            }]}
        });
        assert_eq!(
            citation_in(&no_body, true, None, ""),
            "c7n/resources/ec2.py:4"
        );
        // Genuinely no line anywhere: say so, and how to get one.
        let old = json!({
            "_source": {
                "ax_path": "c7n/resources/ec2.py",
                "ax_locator": "code",
                "body": "x = 1\n"
            },
            "fields": { "_passage": [{
                "field": "defs",
                "ordinal": 0,
                "start_offset": 0,
                "text": "function missing"
            }]}
        });
        let msg = citation_in(&old, true, None, "missing");
        assert_eq!(msg, line_unknown("c7n/resources/ec2.py", true), "{msg}");
        assert!(
            msg.contains("re-pull") && msg.contains("xerj corpus index"),
            "{msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn prose_opts<'a>(query: &'a str, rrf: bool) -> RenderOpts<'a> {
        RenderOpts {
            corpus: "recommenders",
            query,
            full: 800,
            no_symbol: true,
            meatl: false,
            rrf_scores: rrf,
            age_days: None,
            strict_licence: false,
            own: false,
            lexical_lines: false,
            checkout: None,
        }
    }

    fn doc(path: &str, score: f64, body: &str) -> Value {
        json!({
            "_score": score,
            "_source": { "ax_path": path, "line": 10, "body": body }
        })
    }

    /// Issue #1986: a hybrid page of nearest neighbours carries the same RRF
    /// magnitude as a real hit (rank 1 on both legs is `2/(k+1)` either way)
    /// and used to render as five confident passages.
    #[test]
    fn nearest_neighbour_page_is_labelled_weak_and_a_covering_page_is_not() {
        let query = "combine multiple recall channel results into one final ranked list";
        let misses = vec![
            doc(
                "scann/tune.md",
                2.0 / 61.0,
                "ScaNN recall@k improves when the number of leaves is about 200.",
            ),
            doc(
                "scann/other.md",
                2.0 / 65.0,
                "More notes on ANN leaf size and how recall moves with it.",
            ),
        ];
        let opts = prose_opts(query, true);
        let top = top_score(&misses);
        assert!(
            page_is_weak_match(&misses, &opts, top),
            "one shared stem (recall) is under half the content terms"
        );
        let text = render(
            &misses,
            &HashMap::new(),
            &opts,
            Some("hybrid RRF(k=60) — BM25 over 1 index(es), vector over 1 of 1"),
        );
        assert!(
            text.starts_with(
                "[hybrid RRF(k=60) — BM25 over 1 index(es), vector over 1 of 1]\n\
                 weak match: the corpus may not cover this\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("─── scann/tune.md:10  (rrf 0.0328, weak)\n"),
            "{text}"
        );
        assert!(text.contains(", weak)"), "{text}");
        assert!(text.contains("2 passages from 'recommenders'\n"), "{text}");

        // The same scores, with the passage actually covering the need, stay
        // a strong page: no banner, no per-hit marker.
        let covered = vec![doc(
            "docs/ranking.md",
            2.0 / 61.0,
            "Combine multiple recall channel results into one final ranked list \
             by a second-stage scorer.",
        )];
        let covered_opts = prose_opts(query, true);
        assert!(
            !page_is_weak_match(&covered, &covered_opts, top_score(&covered)),
            "a passage that contains the query terms is coverage"
        );
        let strong = render(&covered, &HashMap::new(), &covered_opts, None);
        assert!(!strong.contains("weak"), "{strong}");
        assert!(strong.contains("(rrf 0.0328)\n"), "{strong}");
    }

    #[test]
    fn coverage_uses_the_best_hit_and_half_the_terms_is_enough() {
        // 4 content terms. 2/4 is half (`matched * 2 < 4` is false). The
        // first hit covers one term; the second covers two. The page follows
        // the best hit.
        let query = "lazy dfa cache eviction";
        let hits = vec![
            doc("a.rs", 10.0, "unrelated buffer pooling notes"),
            doc("b.rs", 9.0, "the cache eviction policy lives here"),
        ];
        let opts = prose_opts(query, false);
        assert_eq!(content_terms(query), ["lazy", "dfa", "cache", "eviction"]);
        assert!(
            !page_is_weak_match(&hits, &opts, top_score(&hits)),
            "two of four terms on the best hit is half, which still stands"
        );
        let only_one = vec![doc("a.rs", 10.0, "a cache of buffers")];
        assert!(
            page_is_weak_match(&only_one, &opts, 10.0),
            "one of four terms is under half"
        );
        // A two-term query never takes the coverage path.
        let short = prose_opts("start node", false);
        let miss = vec![doc("a.rs", 9.0, "nothing relevant here at all")];
        assert!(!page_is_weak_match(&miss, &short, 9.0));
    }

    #[test]
    fn stopwords_do_not_count_as_coverage() {
        let query = "the and for with cold start users";
        assert_eq!(content_terms(query), ["cold", "start", "users"]);
        let prose = vec![doc("a.md", 5.0, "the and for with something else")];
        let opts = prose_opts(query, false);
        assert!(page_is_weak_match(&prose, &opts, 5.0));
        let hit = vec![doc("a.md", 5.0, "Cold-start users receive a prior.")];
        assert!(!page_is_weak_match(&hit, &opts, 5.0));
    }

    /// #2327: synthetic TypeScript symbol + Markdown. The one-word `Token`
    /// type outscores the note that answers the question; after the pass it
    /// does not. `xerj def Token` (one term) keeps the type first.
    #[test]
    fn one_term_definition_does_not_outrank_a_multi_term_hit() {
        let query = "session token refresh handler";
        let token = json!({
            "_score": 180.0,
            "_source": {
                "ax_path": "src/types.ts",
                "name": "Token",
                "kind": "type",
                "line": 4,
                "code": "export type Token = string;"
            }
        });
        let refresh = json!({
            "_score": 12.0,
            "_source": {
                "ax_path": "src/session.ts",
                "name": "refreshSession",
                "kind": "function",
                "line": 20,
                "code": "export function refreshSession(token: Token, handler: Handler): Session"
            }
        });
        let note = json!({
            "_score": 6.0,
            "_source": {
                "ax_path": "docs/session-token.md",
                "title": "Refreshing a session token",
                "body": "The refresh handler exchanges a session token for a new session."
            }
        });
        let mut hits = vec![token, refresh, note];
        demote_sparse_definition_hits(&mut hits, query);
        let names: Vec<&str> = hits
            .iter()
            .map(|h| {
                h["_source"]
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(h["_source"]["ax_path"].as_str().unwrap())
            })
            .collect();
        assert_eq!(
            names,
            vec!["refreshSession", "docs/session-token.md", "Token"],
            "{names:?}"
        );

        let mut exact = vec![
            json!({
                "_score": 40.0,
                "_source": { "name": "Token", "kind": "type", "line": 4, "code": "export type Token = string;", "ax_path": "src/types.ts" }
            }),
            json!({
                "_score": 11.0,
                "_source": { "name": "TokenStore", "kind": "class", "line": 8, "code": "export class TokenStore { token: Token }", "ax_path": "src/store.ts" }
            }),
        ];
        let before = exact.clone();
        demote_sparse_definition_hits(&mut exact, "Token");
        assert_eq!(exact, before, "a one-term lookup keeps engine order");

        // Nothing on the page covers a second term: leave the order alone.
        let mut only = vec![
            json!({
                "_score": 90.0,
                "_source": { "name": "Token", "kind": "type", "code": "export type Token = string" }
            }),
            json!({
                "_score": 40.0,
                "_source": { "name": "Handler", "kind": "type", "code": "export type Handler = object" }
            }),
        ];
        let only_before = only.clone();
        demote_sparse_definition_hits(&mut only, query);
        assert_eq!(only, only_before);
    }

    #[test]
    fn archived_path_is_marked_on_the_header() {
        let current = doc(
            "docs/session-token.md",
            8.0,
            "The refresh handler exchanges a session token.",
        );
        let archived = doc(
            "docs/archive/session-token.md",
            9.0,
            "The refresh handler exchanges a session token.",
        );
        let text = render(
            &[archived, current],
            &HashMap::new(),
            &prose_opts("session token refresh", false),
            None,
        );
        assert!(
            text.contains("docs/archive/session-token.md:10  (score 9.00, archived)"),
            "{text}"
        );
        assert!(
            text.contains("docs/session-token.md:10  (score 8.00)\n"),
            "{text}"
        );
        assert!(!text.contains("session-token.md:10  (score 8.00, archived)"));
        assert!(!source_looks_archived(
            &json!({ "ax_path": "src/archive.ts" })
        ));
        assert!(source_looks_archived(
            &json!({ "path": "notes/today.md", "keep_archived": true })
        ));
    }

    #[test]
    fn a_passage_below_half_the_top_score_is_marked_and_the_top_is_not() {
        assert!(
            !passage_is_score_weak(6.0, 12.0),
            "exactly half stays a hit"
        );
        assert!(passage_is_score_weak(5.99, 12.0));
        assert!(!passage_is_score_weak(0.0, 0.0));

        let mut tail = apache_hit();
        tail["_score"] = json!(4.0);
        tail["_source"]["ax_path"] = json!("tantivy/src/other.rs");
        tail["_source"]["line"] = json!(3);
        let hits = vec![apache_hit(), tail];
        let text = render(
            &hits,
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "rebuild segment reader",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            None,
        );
        assert!(
            text.contains("─── tantivy/src/reader.rs:30  (score 12.35, Apache-2.0/MIT)\n"),
            "the strong hit is unchanged: {text}"
        );
        assert!(
            text.contains("─── tantivy/src/other.rs:3  (score 4.00, Apache-2.0/MIT, weak)\n"),
            "{text}"
        );
        assert!(
            !text.contains("weak match"),
            "a covered top hit means the corpus has the need: {text}"
        );
    }

    #[test]
    fn rrf_below_one_leg_at_rank_one_is_weak_even_when_the_terms_match() {
        // k = 60. Rank 1 on one leg is 1/61. Rank 2 on one leg is 1/62,
        // which is under half of the two-leg rank-1 score 2/61.
        let rank1 = 1.0 / 61.0;
        let rank2 = 1.0 / 62.0;
        let body = "pub fn rebuild_segment_reader() {}";
        let strong = vec![doc("a.rs", rank1, body)];
        let weak = vec![doc("a.rs", rank2, body)];
        let opts = prose_opts("rebuild segment reader", true);
        assert!(!page_is_weak_match(&strong, &opts, rank1));
        assert!(page_is_weak_match(&weak, &opts, rank2));
        let text = render(&weak, &HashMap::new(), &opts, None);
        assert!(
            text.contains("weak match: the corpus may not cover this\n"),
            "{text}"
        );
        assert!(text.contains(", weak)\n"), "{text}");
        let ok = render(&strong, &HashMap::new(), &opts, None);
        assert!(!ok.contains("weak"), "{ok}");
    }

    #[test]
    fn meatl_marks_a_weak_page_on_its_own_record() {
        let hits = vec![doc(
            "scann/tune.md",
            2.0 / 61.0,
            "ScaNN recall@k improves when leaves grow.",
        )];
        let text = render(
            &hits,
            &HashMap::new(),
            &RenderOpts {
                corpus: "tfrs",
                query: "combine multiple recall channel results into one final ranked list",
                full: 800,
                no_symbol: true,
                meatl: true,
                rrf_scores: true,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: false,
                checkout: None,
            },
            Some("hybrid RRF(k=60)"),
        );
        assert_eq!(
            text,
            "@mode hybrid RRF(k=60)\n\
             @weak why=corpus-may-not-cover\n\
             @ok f=scann/tune.md:10 rrf=0.0328 weak\n"
        );
    }
}
