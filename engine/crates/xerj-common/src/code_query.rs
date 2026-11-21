//! Query shaping shared by the code-search surfaces: `xerj search`
//! (xerj-autoindex) and the `xerj_search` MCP tool (xerj-mcp), issue #1364.
//!
//! One implementation, so the CLI and the MCP tool cannot drift on which
//! natural-language glue words a multi-word question drops.

use std::collections::BTreeSet;

use serde_json::{json, Value};

fn is_two_letter_tech_term(w: &str) -> bool {
    matches!(
        w,
        "db" | "ui"
            | "ux"
            | "ip"
            | "gc"
            | "vm"
            | "id"
            | "kv"
            | "ir"
            | "ci"
            | "pr"
            | "ai"
            | "ml"
            | "tx"
            | "rx"
            | "fs"
            | "fd"
            | "pk"
            | "fk"
    )
}

fn is_three_letter_glue_word(w: &str) -> bool {
    matches!(
        w,
        "del"
            | "por"
            | "los"
            | "las"
            | "una"
            | "uno"
            | "con"
            | "sin"
            | "sus"
            | "mis"
            | "tus"
            | "nos"
            | "les"
            | "mas"
            | "muy"
            | "tan"
            | "que"
            | "son"
            | "fue"
            | "era"
            | "ser"
            | "hay"
            | "the"
            | "and"
            | "for"
            | "not"
            | "are"
            | "but"
            | "can"
            | "all"
            | "has"
            | "had"
            | "was"
            | "one"
            | "out"
            | "who"
            | "how"
            | "why"
            | "his"
            | "her"
            | "its"
            | "our"
            | "you"
            | "any"
            | "may"
            | "via"
            | "per"
            | "des"
            | "aux"
            | "par"
            | "sur"
            | "une"
            | "est"
            | "ses"
            | "vos"
            | "ils"
            | "pas"
            | "qui"
            | "der"
            | "die"
            | "das"
            | "den"
            | "dem"
            | "ein"
            | "und"
            | "mit"
            | "von"
            | "aus"
            | "bei"
            | "vor"
            | "zur"
            | "zum"
            | "wie"
            | "wir"
            | "sie"
            | "ich"
            | "uma"
            | "dos"
            | "nas"
            | "com"
            | "sem"
            | "seu"
            | "sua"
            | "nao"
            | "gli"
            | "nel"
            | "sul"
            | "col"
            | "tra"
            | "fra"
            | "che"
            | "non"
            | "piu"
    )
}

fn is_natural_language_glue_token(clean: &str) -> bool {
    if clean.is_empty() || clean.contains('_') {
        return false;
    }
    if !clean.chars().all(|c| c.is_alphabetic() && c.is_lowercase()) {
        return false;
    }
    let char_len = clean.chars().count();
    (char_len <= 2 && !is_two_letter_tech_term(clean)) || is_three_letter_glue_word(clean)
}

/// Normalize a plain-words `xerj search` / `xerj_search` query before expanding it into
/// `multi_match` / `match_phrase` / `term` clauses (#1364):
///
/// 1. Deduplicate tokens case-insensitively while preserving first-seen order
///    so a repeated word (`"de"` ×3) never gets 3× BM25 multiplication.
/// 2. When a query has 3+ distinct tokens, drop lowercase natural-language
///    glue tokens (`"de"`, `"el"`, `"la"`, `"en"`, `"the"`, `"of"`, `"in"`, …)
///    so short function/variable names like `de` or `in` on `name^4` / `title`
///    cannot outscore files matching the query's content words, and unrelated
///    questions that only share `"de"` do not collapse onto the same top hits.
///    Single-token queries (`xerj search de`) and 2-token queries keep all
///    tokens untouched, as do UPPERCASE/camelCase acronyms (`IO`, `DB`, `ID`)
///    and alphanumeric tokens (`v2`, `u8`, `o68`).
pub fn normalize_code_search_query(query: &str) -> String {
    let mut seen = Vec::<String>::new();
    let mut dedup = Vec::<&str>::new();
    for raw in query.split_whitespace() {
        let clean = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if clean.is_empty() {
            continue;
        }
        let lower = clean.to_lowercase();
        if !seen.contains(&lower) {
            seen.push(lower);
            dedup.push(clean);
        }
    }
    // Under three distinct tokens the query is a lookup, not a question:
    // return it verbatim. Re-joining the trimmed tokens here would strip
    // identifier punctuation — R's `.mensaje_motor` (the #1364 reporter's own
    // control query) would lose its leading dot and miss the exact-`name`
    // `term` clause.
    if dedup.len() < 3 {
        return query.trim().to_string();
    }
    let content: Vec<&str> = dedup
        .iter()
        .copied()
        .filter(|tok| !is_natural_language_glue_token(tok))
        .collect();
    if content.is_empty() {
        dedup.join(" ")
    } else {
        content.join(" ")
    }
}

/// True when `q` reads as a sentence or question rather than a lookup.
///
/// Three or more words, at least two of them plain words (letters only, no
/// `_`, `::`, `.` or inner capital, so `getUserInfo` and `parse_config` are
/// not), or any query ending in `?`. Scripts without case (Persian, CJK)
/// count as plain words. One or two words are a lookup.
pub fn looks_natural_language(q: &str) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return false;
    }
    if q.ends_with('?') && q.split_whitespace().count() >= 2 {
        return true;
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    let plain = words
        .iter()
        .filter(|w| {
            let w = w.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '"' | '\'' | '(' | ')'));
            let mut chars = w.chars();
            match chars.next() {
                Some(first) if first.is_alphabetic() => {
                    chars.all(|c| c.is_alphabetic() && !c.is_uppercase())
                }
                _ => false,
            }
        })
        .count();
    plain >= 2
}

/// Boost on the exact-phrase clause `xerj search` and the plain-string
/// `xerj_search` MCP tool put on content fields (`body`, and `text` /
/// `message` / `impl` where mapped).
///
/// Below the `defs` phrase boost (8) on purpose: when a query is both a
/// symbol's words and a phrase in some file, the definition still ranks
/// first. It only has to beat the `most_fields` sum a short `defs` list
/// collects from partial term overlap (`MemoryCandidate` matching
/// `memory candidate …`), which on a long file body is several times the
/// body's own BM25 score.
pub const EXACT_PHRASE_BOOST: f64 = 4.0;

/// Score added when a multi-term question matches on coverage, not on one
/// short symbol field (#2327).
///
/// A `name^4` / `code^2` hit whose text is `type Token = string` collects a
/// large BM25 score from field length. Definition pages in this crate's
/// tests sit near 172. One thousand sits above that, so a hit that contains
/// at least half the content terms sorts ahead of a one-term name. A hit
/// that contains every term gets this boost a second time.
pub const COVERAGE_MATCH_BOOST: f64 = 1000.0;

/// Extra `constant_score` clauses for a question with at least three content
/// tokens. Empty for a one- or two-token lookup — that is an identifier
/// search, and the exact `name` term stays the strongest signal.
///
/// The first clause matches at least half the tokens (rounded up). The
/// second matches every token. Both are filters: they do not multiply BM25,
/// and a document that fails them still matches the recall clauses.
pub fn coverage_match_clauses(query: &str, fields: &[String]) -> Vec<Value> {
    let shaped = normalize_code_search_query(query);
    let tokens: Vec<&str> = shaped.split_whitespace().collect();
    if tokens.len() < 3 || fields.is_empty() {
        return Vec::new();
    }
    let need = tokens.len().saturating_add(1) / 2;
    let per_term: Vec<Value> = tokens
        .iter()
        .map(|token| {
            json!({
                "multi_match": {
                    "query": token,
                    "fields": fields,
                    "type": "most_fields"
                }
            })
        })
        .collect();
    vec![
        json!({
            "constant_score": {
                "filter": {
                    "bool": {
                        "should": per_term,
                        "minimum_should_match": need
                    }
                },
                "boost": COVERAGE_MATCH_BOOST
            }
        }),
        json!({
            "constant_score": {
                "filter": {
                    "multi_match": {
                        "query": shaped,
                        "fields": fields,
                        "type": "most_fields",
                        "operator": "and"
                    }
                },
                "boost": COVERAGE_MATCH_BOOST
            }
        }),
    ]
}

/// The text an exact-phrase content clause matches, or `None` when the query
/// is a single token.
///
/// This is the trimmed query as typed, not [`normalize_code_search_query`]'s
/// output: dropping glue words or duplicate tokens from a pasted error
/// message would turn it into a phrase the file does not contain. A single
/// token is left to the existing clauses — as a phrase it scores the same
/// documents as the `multi_match` and would only inflate them.
pub fn exact_phrase_query(query: &str) -> Option<&str> {
    let trimmed = query.trim();
    trimmed.split_whitespace().nth(1).map(|_| trimmed)
}

// ─────────────────────────────────────────────────────────────────────────────
// CJK runs (#2580)
// ─────────────────────────────────────────────────────────────────────────────

/// Boost on the `match_phrase` clause each CJK run of the query gets.
///
/// The default `standard` analyzer emits one token per Han character, so
/// `雷同` is the bag `{雷, 同}`: a long note full of `雷` and `同` outscores
/// a file that contains `雷同` itself (#2580 ranked it 4th). The phrase
/// clause only matches the characters in order, which is what the agent
/// typed. Same weight as [`EXACT_PHRASE_BOOST`] — the run is an exact
/// phrase that happens to have no spaces.
pub const CJK_RUN_PHRASE_BOOST: f64 = 4.0;

/// At most this many runs get a phrase clause, so a pasted paragraph does
/// not grow the body by one clause per phrase.
pub const MAX_CJK_RUNS: usize = 8;

/// Han (with extension A and compatibility ideographs), Hiragana, Katakana
/// and Hangul syllables: the scripts written without spaces between words.
pub fn is_cjk_char(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x309F     // Hiragana
        | 0x30A0..=0x30FF   // Katakana
        | 0x3400..=0x4DBF   // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0xAC00..=0xD7AF   // Hangul syllables
        | 0xF900..=0xFAFF   // CJK Compatibility Ideographs
    )
}

/// Contiguous runs of two or more CJK characters in `query`, first
/// occurrence order, without duplicates, capped at [`MAX_CJK_RUNS`].
///
/// A single character is left to the recall clauses: as a phrase it would
/// score the same documents as the `multi_match`.
pub fn cjk_runs(query: &str) -> Vec<String> {
    fn flush(current: &mut String, runs: &mut Vec<String>) {
        if current.chars().count() >= 2 && !runs.contains(current) {
            runs.push(std::mem::take(current));
        } else {
            current.clear();
        }
    }
    let mut runs: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in query.chars() {
        if is_cjk_char(c) {
            current.push(c);
        } else {
            flush(&mut current, &mut runs);
        }
    }
    flush(&mut current, &mut runs);
    runs.truncate(MAX_CJK_RUNS);
    runs
}

// ─────────────────────────────────────────────────────────────────────────────
// Dotted member lookups (#2426)
// ─────────────────────────────────────────────────────────────────────────────

/// File extensions that make `name.ext` a file name, not `Type.member`.
const FILE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cfg", "conf", "cpp", "cs", "css", "csv", "go", "h", "hpp", "html", "ini", "java",
    "js", "json", "jsx", "kt", "lock", "log", "md", "mjs", "pdf", "php", "png", "py", "rb", "rs",
    "sh", "sql", "svg", "swift", "toml", "ts", "tsx", "txt", "xml", "yaml", "yml", "zip",
];

fn is_identifier_part(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// `(Type, member)` when the query is one dotted or `::` member path:
/// `Position.open_price`, `ActiveUtils.activeDialog`, `Tree::flush`.
///
/// The `standard` tokenizer keeps `position.open_price` as one token
/// (UAX #29 does not break a word at `.` between letters), so the plain
/// `multi_match` only finds documents that spell the dotted path out —
/// never the `struct Position` that declares `open_price` (#2426). A longer
/// path (`a.b.c`) uses its last two parts. File names (`search.rs`) and
/// version strings are not member paths.
pub fn dotted_member(query: &str) -> Option<(&str, &str)> {
    let q = query.trim();
    if q.is_empty() || q.contains(char::is_whitespace) {
        return None;
    }
    let sep = if q.contains("::") { "::" } else { "." };
    let parts: Vec<&str> = q.split(sep).collect();
    if parts.len() < 2 || !parts.iter().all(|p| is_identifier_part(p)) {
        return None;
    }
    let owner = parts[parts.len() - 2];
    let member = parts[parts.len() - 1];
    if sep == "." && FILE_EXTENSIONS.contains(&member.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some((owner, member))
}

/// Boost on `term name = <Type>` for a dotted member query: the owning
/// type's own definition. Same weight as the whole-query `name` term.
pub const MEMBER_OWNER_NAME_BOOST: f64 = 10.0;

/// Boost on the file-level clause "defines `<Type>` and mentions
/// `<member>`" — the file that holds the declaration.
pub const MEMBER_OWNER_FILE_BOOST: f64 = 8.0;

/// Boost on `term name = <member>`: a method or function of that name.
/// Below the owner so `open` (a common method name) does not outrank the
/// type the agent qualified it with.
pub const MEMBER_NAME_BOOST: f64 = 4.0;

// ─────────────────────────────────────────────────────────────────────────────
// Exact advisory and package identifiers (#1788)
// ─────────────────────────────────────────────────────────────────────────────

/// Constant score an exact advisory or package identifier match adds.
///
/// An ID names one record, so it must beat every other clause, including a
/// question's two [`COVERAGE_MATCH_BOOST`]s: `what does advisory
/// RUSTSEC-2021-0003 affect` ranked the advisory itself below records that
/// share `advisory` and `affect` when this was a plain boost-20 `term`
/// (measured on the rust-vulns pack, 2/30 at rank 1). A `constant_score`, so
/// the keyword's IDF does not scale it.
pub const EXACT_ID_TERM_BOOST: f64 = 3.0 * COVERAGE_MATCH_BOOST;
const _: () = assert!(EXACT_ID_TERM_BOOST > 2.0 * COVERAGE_MATCH_BOOST);

fn exact_id_clause(field: &str, id: &str) -> Value {
    json!({ "constant_score": {
        "filter": { "term": { field: id } },
        "boost": EXACT_ID_TERM_BOOST
    }})
}

/// Boost on the `match_phrase` of an identifier in a content field (the
/// advisory body that cites `CVE-2024-1234`).
pub const EXACT_ID_PHRASE_BOOST: f64 = 6.0;

/// Keyword fields an exact identifier is looked up in, when a read mapping
/// names them. `name` is handled with the code fields.
pub const EXACT_ID_KEYWORD_FIELDS: &[&str] = &["id", "aliases"];

fn all_digits(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// Canonical form of one advisory ID token, or `None`.
///
/// `CVE-YYYY-NNNN+`, `RUSTSEC-YYYY-NNNN`, `PYSEC-YYYY-N+`, `GO-YYYY-NNNN`
/// upper-cased; `GHSA-xxxx-xxxx-xxxx` keeps the prefix upper and the body
/// lower, as GitHub prints it.
fn advisory_id(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('-').collect();
    let prefix = parts.first()?.to_ascii_uppercase();
    match (prefix.as_str(), parts.len()) {
        ("CVE", 3) if all_digits(parts[1], 4, 4) && all_digits(parts[2], 4, 9) => {
            Some(format!("CVE-{}-{}", parts[1], parts[2]))
        }
        ("RUSTSEC" | "GO", 3) if all_digits(parts[1], 4, 4) && all_digits(parts[2], 4, 6) => {
            Some(format!("{prefix}-{}-{}", parts[1], parts[2]))
        }
        ("PYSEC", 3) if all_digits(parts[1], 4, 4) && all_digits(parts[2], 1, 6) => {
            Some(format!("PYSEC-{}-{}", parts[1], parts[2]))
        }
        ("GHSA", 4)
            if parts[1..]
                .iter()
                .all(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_alphanumeric())) =>
        {
            Some(format!(
                "GHSA-{}-{}-{}",
                parts[1].to_ascii_lowercase(),
                parts[2].to_ascii_lowercase(),
                parts[3].to_ascii_lowercase()
            ))
        }
        _ => None,
    }
}

/// `pkg@version` (`serde@1.0.130`, `@scope/pkg@2.1.0`) kept as typed.
fn package_at_version(token: &str) -> Option<String> {
    let at = token.rfind('@').filter(|&i| i > 0)?;
    let (pkg, version) = (&token[..at], &token[at + 1..]);
    let pkg_ok = pkg.chars().any(|c| c.is_ascii_alphabetic())
        && pkg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@'));
    let version_ok = version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    (pkg_ok && version_ok).then(|| token.to_string())
}

/// Advisory IDs and `pkg@version` identifiers in `query`, in order, without
/// duplicates. Surrounding punctuation (`(CVE-2024-1234),`) is trimmed.
pub fn exact_ids(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '@' | '.' | '/' | '+'))
        });
        let token = token.trim_end_matches(['.', '/']);
        if let Some(id) = advisory_id(token).or_else(|| package_at_version(token)) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The exact-identifier clauses for `query` (#1788): an advisory ID or
/// `pkg@version` names one record. Exact `term` on the keyword ID fields a
/// read mapping names (never on an unread mapping: autoindex indices are
/// strict and do not map `id`), on `name` when the ID is not the whole
/// query, and a phrase on each of `content_fields` that cite it. Empty when
/// the query holds no such ID.
///
/// Shared by `xerj search` / `xerj_search` (`cited` = [`IdCitation::Phrase`])
/// and the `xerj code` BM25 body ([`IdCitation::Constant`]).
pub fn exact_id_clauses(
    query: &str,
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
    content_fields: &[&str],
    cited: IdCitation,
) -> Vec<Value> {
    let mut out = Vec::new();
    for id in exact_ids(query) {
        for field in EXACT_ID_KEYWORD_FIELDS {
            if mapped.is_some_and(|set| set.contains(*field))
                && !numeric.is_some_and(|set| set.contains(*field))
            {
                out.push(exact_id_clause(field, &id));
            }
        }
        if id != query.trim() && field_lookup_ok(mapped, numeric, "name") {
            out.push(exact_id_clause("name", &id));
        }
        for base in content_fields {
            out.push(match cited {
                IdCitation::Phrase => json!({ "match_phrase": { *base: {
                    "query": id,
                    "boost": EXACT_ID_PHRASE_BOOST
                } } }),
                IdCitation::Constant => json!({ "constant_score": {
                    "filter": { "match_phrase": { *base: id.as_str() } },
                    "boost": EXACT_ID_CITED_BOOST
                }}),
            });
        }
    }
    out
}

/// How a content field that cites an exact ID scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdCitation {
    /// `match_phrase` at [`EXACT_ID_PHRASE_BOOST`] (BM25-scaled), the
    /// `xerj search` body #3336 measured on the rust-vulns pack, where the
    /// record's own keyword `id` carries the constant.
    Phrase,
    /// `constant_score` at [`EXACT_ID_CITED_BOOST`]. A code corpus has no
    /// `id` keyword: the ID sits in a comment or a string (`RUSTSEC-2021-0003`
    /// in a lockfile gate), and a BM25-scaled phrase scored under 3 there,
    /// behind a file that holds `rustsec`, `2021`, `0003` and `affect` apart
    /// and so collects both coverage boosts (fixture
    /// benchmarks/code-search-field-bm25-2026-11/fixtures/ids: rank 3).
    Constant,
}

/// Constant score of a content field that cites an exact ID in `xerj code`
/// ([`IdCitation::Constant`]): above a question's two coverage boosts, below
/// the keyword `id` term, so the record that IS the advisory still outranks
/// a file that only mentions it.
pub const EXACT_ID_CITED_BOOST: f64 = 2.5 * COVERAGE_MATCH_BOOST;
const _: () = assert!(EXACT_ID_CITED_BOOST > 2.0 * COVERAGE_MATCH_BOOST);
const _: () = assert!(EXACT_ID_CITED_BOOST < EXACT_ID_TERM_BOOST);

// ─────────────────────────────────────────────────────────────────────────────
// File-name lane for `xerj code` (#1788)
// ─────────────────────────────────────────────────────────────────────────────

/// Score of a symbol whose `name` is the query's words joined as one
/// identifier (#1788): `cat pending tasks` → `cat_pending_tasks`.
///
/// A constant, and below one [`COVERAGE_MATCH_BOOST`]: a file that holds
/// every word of a three-word question still outranks the bare symbol
/// record, while the symbol beats files that only share some of the words.
/// The clause may match alone — a symbol record has no `body`, so the
/// `multi_match` never reached it for a spaced-out name. Swept 50 and 500
/// in `xerj search`-style bodies first (no change while it could not match
/// alone), then 500 as an alternative recall clause: +5 rank-1 on the 300
/// search-exact-phrase questions, no change on 208 Lucene questions.
pub const IDENT_SPELLING_BOOST: f64 = 500.0;
const _: () = assert!(IDENT_SPELLING_BOOST < COVERAGE_MATCH_BOOST);

/// The identifier spellings of a two- to six-word query of plain ASCII
/// words: `snake_case`, `camelCase`, `PascalCase`, `SCREAMING_SNAKE`,
/// sorted and deduplicated. Empty for one word (that is already the name),
/// more than six, or any word with punctuation (`Tree::flush`, a question
/// mark): those are not a name read aloud.
pub fn identifier_spellings(query: &str) -> Vec<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    if !(2..=6).contains(&words.len())
        || !words
            .iter()
            .all(|w| w.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Vec::new();
    }
    let capital = |w: &str| {
        let mut c = w.chars();
        c.next()
            .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
            .unwrap_or_default()
    };
    let snake = words.join("_");
    let camel = words[0].clone() + &words[1..].iter().map(|w| capital(w)).collect::<String>();
    let pascal: String = words.iter().map(|w| capital(w)).collect();
    let mut out = vec![snake.to_ascii_uppercase(), snake, camel, pascal];
    out.sort();
    out.dedup();
    out
}

/// Score a document gets per query word its `title` contains (#1788).
///
/// On a code corpus `title` is the file name of a file record and the
/// symbol name of a symbol record, mapped `keyword`, so the BM25
/// `multi_match` only ever matched it when the whole query was the name.
/// `HNSW graph` never reached `HnswGraphBuilder.java` through it. Swept 2,
/// 3, 5, 8 and 15 on the 300 search-exact-phrase questions and 200 Lucene
/// Javadoc questions (benchmarks/code-search-field-bm25-2026-11): 5 had the
/// best sum (3 was 0.001 nDCG@10 higher on the first set and 0.008 lower on
/// the second); 15 lost on the first set.
pub const NAME_LANE_BOOST: f64 = 5.0;

/// At most this many words get a name clause, so a pasted paragraph does
/// not grow the body by one wildcard per word.
pub const MAX_NAME_LANE_TOKENS: usize = 8;

/// English words that are in a question but say nothing about which file
/// is meant. Three-letter words shorter than this list are already cut by
/// the length floor in [`name_lane_tokens`].
const NAME_LANE_STOP_WORDS: &[&str] = &[
    "the", "and", "for", "are", "with", "from", "its", "this", "that", "how", "what", "when",
    "where", "which", "who", "does",
];

/// Lower-cased query words of three or more ASCII letters, digits, or `_`,
/// minus [`NAME_LANE_STOP_WORDS`], in order, deduplicated, at most
/// [`MAX_NAME_LANE_TOKENS`].
pub fn name_lane_tokens(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in query.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        let lower = word.to_ascii_lowercase();
        if lower.len() < 3 || NAME_LANE_STOP_WORDS.contains(&lower.as_str()) || out.contains(&lower)
        {
            continue;
        }
        out.push(lower);
        if out.len() == MAX_NAME_LANE_TOKENS {
            break;
        }
    }
    out
}

/// One `constant_score` case-insensitive `*word*` wildcard on `field` per
/// [`name_lane_tokens`] word, each worth [`NAME_LANE_BOOST`]. A constant,
/// not BM25: the field is a keyword, and the lane is a tie-breaker inside
/// the coverage tiers ([`COVERAGE_MATCH_BOOST`] is 200 times larger).
pub fn name_lane_clauses(query: &str, field: &str) -> Vec<Value> {
    name_lane_tokens(query)
        .into_iter()
        .map(|word| {
            json!({ "constant_score": {
                "filter": { "wildcard": { field: {
                    "value": format!("*{word}*"),
                    "case_insensitive": true
                } } },
                "boost": NAME_LANE_BOOST
            }})
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// All-terms-first tier (#3007, opt-in)
// ─────────────────────────────────────────────────────────────────────────────

/// The first-pass body for `xerj search --all-terms-first`: `body` with a
/// filter that every content token of `query` matches at least one of
/// `body`'s `multi_match` fields. Scoring is unchanged; the filter only
/// narrows the candidates.
///
/// `None` for a query with fewer than two content tokens (a lookup has
/// nothing to tier) and for a body with no `multi_match` to take fields
/// from. The caller runs the plain body for the rest of the page — see
/// [`merge_tiered_hits`].
pub fn all_terms_first_body(body: &Value, query: &str) -> Option<Value> {
    let shaped = normalize_code_search_query(query);
    let tokens: Vec<&str> = shaped.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let fields = body
        .pointer("/query/bool/should")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|c| c.pointer("/multi_match/fields").cloned())?;
    let must: Vec<Value> = tokens
        .iter()
        .map(|t| json!({ "multi_match": { "query": t, "fields": fields, "type": "most_fields" } }))
        .collect();
    let mut tiered = body.clone();
    let bool_q = tiered.pointer_mut("/query/bool")?.as_object_mut()?;
    bool_q.insert("filter".into(), json!([{ "bool": { "must": must } }]));
    Some(tiered)
}

/// `first` (all terms) followed by the `fallback` hits it does not already
/// hold, by `_index` + `_id`, cut to `k`. The fallback's own order is kept.
pub fn merge_tiered_hits(first: &[Value], fallback: &[Value], k: usize) -> Vec<Value> {
    let key = |h: &Value| {
        (
            h.get("_index")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            h.get("_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    };
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out = Vec::with_capacity(k);
    for hit in first.iter().chain(fallback.iter()) {
        if out.len() >= k {
            break;
        }
        if seen.insert(key(hit)) {
            out.push(hit.clone());
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Mapping-aware query bodies (#1601/#1284)
//
// Shared by `xerj search` and the `xerj_search` MCP tool (#1961): the MCP path
// used to build this body itself, so #1602's mapping-aware fix never reached it
// and a prose corpus answered HTTP 400 `strict_unknown_fields`.
// ─────────────────────────────────────────────────────────────────────────────

/// `name` / `code` / `defs` are the code-shaped legs (#916). A corpus that
/// maps none of them — markdown, logs, anything autoindex accepted that is
/// not source — must not be queried with those legs: autoindex indices are
/// created `strict_unknown_fields`, and a `term` or `match_phrase` on a
/// field no resolved index maps is HTTP 400 (#1601).
pub const CODE_SYMBOL_FIELDS: &[&str] = &["name", "code", "defs"];

/// Historical `xerj search` multi_match weights. Order is part of the
/// code-corpus query: do not reorder.
pub const SEARCH_MM_FIELDS: &[(&str, &str)] = &[
    ("name", "name^4"),
    ("code", "code^2"),
    ("body", "body"),
    ("defs", "defs^3"),
    ("title", "title"),
];

/// Content fields a non-code autoindex maps (`text` on prose, `impl` on
/// markdown sections, `message` on the log family). Joined only when the
/// target maps none of [`CODE_SYMBOL_FIELDS`], so a code corpus keeps the
/// #916 shape exactly.
pub const PROSE_EXTRA_FIELDS: &[&str] = &["text", "impl", "message"];

/// Analyzed content fields that get the exact-phrase clause when the
/// `multi_match` names them. `name`, `code` and `title` are keywords and
/// `defs` has its own phrase clause.
pub const PHRASE_CONTENT_FIELDS: &[&str] = &["body", "text", "impl", "message"];

/// Every field `xerj search` might name in `multi_match`, `term`, or
/// `match_phrase`. A numeric mapping of one of these on any index in the
/// wildcard fails the whole request (`For input string`, #1284).
pub const SEARCH_LOOKUP_FIELDS: &[&str] = &[
    "name", "code", "body", "defs", "title", "text", "impl", "message",
];

/// Union of property names in a `GET /{pattern}/_mapping` body.
///
/// `None` when `mapping` is not that shape, so a transport failure keeps the
/// historical code-shaped query. An empty object is a real answer (no
/// indices) and comes back empty — callers treat an empty set like an
/// unreadable mapping and do not drop clauses they cannot prove unmapped.
pub fn mapped_property_names(mapping: &Value) -> Option<BTreeSet<String>> {
    let obj = mapping.as_object()?;
    if obj.is_empty() {
        return Some(BTreeSet::new());
    }
    let mut names = BTreeSet::new();
    let mut saw_mapping = false;
    for body in obj.values() {
        let Some(props) = body
            .pointer("/mappings/properties")
            .and_then(Value::as_object)
        else {
            continue;
        };
        saw_mapping = true;
        names.extend(props.keys().cloned());
    }
    if !saw_mapping {
        return None;
    }
    Some(names)
}

/// Types stored as `long` or `double`. A `multi_match`, `term`, or
/// `match_phrase` that names one fails with `For input string: "<query>"`
/// (#1284). `keyword` is not in this list: `name` stays an exact term.
fn is_numeric_mapping_type(ty: &str) -> bool {
    matches!(
        ty,
        "long"
            | "integer"
            | "short"
            | "byte"
            | "unsigned_long"
            | "double"
            | "float"
            | "half_float"
            | "scaled_float"
    )
}

/// Field names any index in a `GET /{pattern}/_mapping` body maps as numeric.
///
/// One numeric mapping is enough to drop the field from a wildcard query:
/// the request is refused for every index, the way `xerj code` drops a
/// numeric field from its union (#1605). An unreadable body yields an empty
/// set; callers ignore that unless [`mapped_property_names`] also succeeded.
pub fn numeric_property_names(mapping: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(obj) = mapping.as_object() else {
        return out;
    };
    for body in obj.values() {
        let Some(props) = body
            .pointer("/mappings/properties")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (key, spec) in props {
            if spec
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(is_numeric_mapping_type)
            {
                out.insert(key.clone());
            }
        }
    }
    out
}

/// A lookup clause may name `field` when it is mapped (or the mapping was
/// unreadable) and no index maps it as numeric.
pub fn field_lookup_ok(
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
    field: &str,
) -> bool {
    field_mapped(mapped, field) && !numeric.is_some_and(|set| set.contains(field))
}

/// True when a numeric mapping removed a field this command would look up,
/// so the historical query must not be restored — it would name that field
/// again and the wildcard would fail.
fn numeric_drops_a_lookup(numeric: Option<&BTreeSet<String>>, candidates: &[&str]) -> bool {
    numeric.is_some_and(|set| candidates.iter().any(|name| set.contains(*name)))
}

/// Whether `field` may be named in a lookup clause.
///
/// An unreadable mapping (`None`) or an empty union keeps every historical
/// clause: dropping them with no evidence would change the #916 ranking on
/// a code corpus whose mapping we failed to read.
pub fn field_mapped(mapped: Option<&BTreeSet<String>>, field: &str) -> bool {
    match mapped {
        None => true,
        Some(set) if set.is_empty() => true,
        Some(set) => set.contains(field),
    }
}

/// True when the mapping was read and no index maps `name`, `code`, or `defs`.
pub fn code_symbol_fields_absent(mapped: Option<&BTreeSet<String>>) -> bool {
    match mapped {
        Some(set) if !set.is_empty() => CODE_SYMBOL_FIELDS.iter().all(|f| !set.contains(*f)),
        _ => false,
    }
}

/// `preferred` entries whose base field is mapped, then `extras` (bare names).
///
/// `prose_beside_code`: also name `text` and `message` when a code field is
/// mapped. A folded `docs` index stores changelog and config lines in `text`
/// next to `name`/`code`/`defs`. Leaving `text` off made `xerj search` miss
/// `tokio/CHANGELOG.md` (122 sections) while a direct `match_phrase` on
/// `text` hit them. `impl` stays off in that case so a code corpus does not
/// grow a second body leg (#916). A corpus with no code field still gets
/// every extra it maps, including `impl`.
pub fn weighted_query_fields(
    preferred: &[(&str, &str)],
    extras: &[&str],
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
    prose_beside_code: bool,
) -> Vec<String> {
    let mut out: Vec<String> = preferred
        .iter()
        .filter(|(name, _)| field_lookup_ok(mapped, numeric, name))
        .map(|(_, weighted)| (*weighted).to_string())
        .collect();
    let code_present = !code_symbol_fields_absent(mapped);
    if let Some(set) = mapped.filter(|set| !set.is_empty()) {
        for name in extras {
            if code_present && (*name == "impl" || !prose_beside_code) {
                continue;
            }
            if set.contains(*name)
                && !numeric.is_some_and(|nums| nums.contains(*name))
                && !out.iter().any(|f| f.split('^').next() == Some(*name))
            {
                out.push((*name).to_string());
            }
        }
    }
    out
}

/// Build the ES search request body used by `xerj search`.
///
/// `mapped` is the union of property names under the prefix. `None` (and an
/// empty union) is the historical code-shaped query.
pub fn search_query_body(query: &str, k: usize) -> Value {
    search_query_body_for_mapping(query, k, None, None)
}

/// [`search_query_body`] with the target's mapped field names (#1601) and
/// the names any index maps as numeric (#1284).
pub fn search_query_body_for_mapping(
    query: &str,
    k: usize,
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
) -> Value {
    let built = build_search_query(query, k, mapped, numeric);
    if should_clauses(&built).is_empty() {
        if numeric_drops_a_lookup(numeric, SEARCH_LOOKUP_FIELDS) {
            // Restoring the historical query would name the numeric field
            // again. An empty `should` matches every document; match nothing.
            let mut built = built;
            built["query"] = json!({ "match_none": {} });
            return built;
        }
        // Nothing we can name is mapped. The historical shape is what the
        // server's 400 then explains, listing the searchable fields.
        return build_search_query(query, k, None, None);
    }
    built
}

fn should_clauses(body: &Value) -> &[Value] {
    body.pointer("/query/bool/should")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn build_search_query(
    query: &str,
    k: usize,
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
) -> Value {
    let shaped = normalize_code_search_query(query);
    let mut should = Vec::new();
    let fields = weighted_query_fields(SEARCH_MM_FIELDS, PROSE_EXTRA_FIELDS, mapped, numeric, true);
    if !fields.is_empty() {
        should.push(json!({ "multi_match": {
            "query": shaped,
            "fields": fields,
            "type": "most_fields"
        }}));
    }
    if field_lookup_ok(mapped, numeric, "defs") {
        should.push(json!({ "match_phrase": { "defs": { "query": shaped, "boost": 8 } } }));
    }
    if field_lookup_ok(mapped, numeric, "name") {
        should.push(json!({ "term": { "name": { "value": query.trim(), "boost": 10 } } }));
    }
    // An exact phrase in a content field. A pasted error message or log
    // line is the strongest signal of which file is meant, and the
    // bag-of-words `multi_match` alone ranks a short `defs` list sharing two
    // of its words above the long file holding all of them in order. Only
    // the content fields the `multi_match` already names, so the mapping and
    // numeric drops (#1601, #1284) apply unchanged. Appended last so the
    // historical clause positions do not move.
    if let Some(phrase) = exact_phrase_query(query) {
        for field in &fields {
            let base = field.split('^').next().unwrap_or(field);
            if PHRASE_CONTENT_FIELDS.contains(&base) {
                should.push(json!({ "match_phrase": { base: {
                    "query": phrase,
                    "boost": EXACT_PHRASE_BOOST
                } } }));
            }
        }
    }
    // Appended last so the #916 clause positions do not move. A one-word
    // symbol still matches the recall `multi_match`; these clauses only
    // add score when the document covers the question (#2327).
    should.extend(coverage_match_clauses(&shaped, &fields));
    // Everything below is appended after the #916/#2327 clauses so their
    // positions do not move, and each block is empty for a query it does
    // not apply to.
    let content_fields: Vec<&str> = fields
        .iter()
        .map(|f| f.split('^').next().unwrap_or(f))
        .filter(|base| PHRASE_CONTENT_FIELDS.contains(base))
        .collect();
    // #2580: each CJK run as a phrase, so the document holding `雷同`
    // outranks one that only shares the characters `雷` and `同`.
    for run in cjk_runs(query) {
        for base in &content_fields {
            should.push(json!({ "match_phrase": { *base: {
                "query": run,
                "boost": CJK_RUN_PHRASE_BOOST
            } } }));
        }
    }
    // #2426: `Position.open_price` is one token to the analyzer. Search the
    // two parts as words, and rank the owner's definition first.
    if let Some((owner, member)) = dotted_member(query) {
        if !fields.is_empty() {
            should.push(json!({ "multi_match": {
                "query": format!("{owner} {member}"),
                "fields": fields,
                "type": "most_fields"
            }}));
        }
        if field_lookup_ok(mapped, numeric, "name") {
            should.push(
                json!({ "term": { "name": { "value": owner, "boost": MEMBER_OWNER_NAME_BOOST } } }),
            );
            should.push(
                json!({ "term": { "name": { "value": member, "boost": MEMBER_NAME_BOOST } } }),
            );
        }
        if field_lookup_ok(mapped, numeric, "defs") && !fields.is_empty() {
            should.push(json!({ "bool": {
                "must": [
                    { "match_phrase": { "defs": owner } },
                    { "multi_match": { "query": member, "fields": fields, "type": "most_fields" } }
                ],
                "boost": MEMBER_OWNER_FILE_BOOST
            }}));
        }
    }
    should.extend(exact_id_clauses(
        query,
        mapped,
        numeric,
        &content_fields,
        IdCitation::Phrase,
    ));
    json!({
        "size": k,
        "query": { "bool": { "should": should }},
        // No `body` here: whole-file bodies are the token flood (measured ~9x
        // the size of the matching passage). `_passage` carries the snippet.
        // `ax_paths` lists every name a deduplicated file has, so the hit can
        // cite the live name instead of a backup copy (#2402).
        "_source": ["ax_path", "ax_paths", "ax_locator", "line", "start_line", "language", "kind", "code", "title", "heading", "page", "cell"],
        "fields": ["_passage"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clauses(body: &Value) -> Vec<Value> {
        should_clauses(body).to_vec()
    }

    fn phrases_on(body: &Value, field: &str) -> Vec<(String, f64)> {
        clauses(body)
            .iter()
            .filter_map(|c| c.pointer(&format!("/match_phrase/{field}")))
            .filter_map(|p| {
                Some((
                    p.get("query")?.as_str()?.to_string(),
                    p.get("boost")?.as_f64()?,
                ))
            })
            .collect()
    }

    fn names(fields: &[&str]) -> BTreeSet<String> {
        fields.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn spaced_words_spell_the_identifier_four_ways() {
        assert_eq!(
            identifier_spellings("cat pending tasks"),
            vec![
                "CAT_PENDING_TASKS",
                "CatPendingTasks",
                "catPendingTasks",
                "cat_pending_tasks"
            ]
        );
        assert_eq!(
            identifier_spellings("max term length"),
            vec![
                "MAX_TERM_LENGTH",
                "MaxTermLength",
                "maxTermLength",
                "max_term_length"
            ]
        );
        assert_eq!(identifier_spellings("encode v2 Block").len(), 4);
        for not_a_name in [
            "cat_pending_tasks",
            "Tree::flush now",
            "how are segments merged?",
            "a b c d e f g",
            "",
        ] {
            assert!(identifier_spellings(not_a_name).is_empty(), "{not_a_name}");
        }
    }

    #[test]
    fn name_lane_words_skip_glue_short_words_and_repeats_and_are_capped() {
        assert_eq!(
            name_lane_tokens("When does the HNSW graph builder link nodes?"),
            vec!["hnsw", "graph", "builder", "link", "nodes"]
        );
        assert_eq!(
            name_lane_tokens("cat_pending_tasks"),
            vec!["cat_pending_tasks"]
        );
        assert_eq!(name_lane_tokens("Tree::flush"), vec!["tree", "flush"]);
        assert_eq!(name_lane_tokens("io db x"), Vec::<String>::new());
        assert_eq!(name_lane_tokens("graph Graph GRAPH"), vec!["graph"]);
        assert!(name_lane_tokens("WAL 雷同 rotation").contains(&"rotation".to_string()));
        let long = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        assert_eq!(name_lane_tokens(long).len(), MAX_NAME_LANE_TOKENS);
        let lane = name_lane_clauses("graph builder", "title");
        assert_eq!(lane.len(), 2);
        assert_eq!(
            lane[0],
            json!({ "constant_score": {
                "filter": { "wildcard": { "title": { "value": "*graph*", "case_insensitive": true } } },
                "boost": NAME_LANE_BOOST
            }})
        );
    }

    #[test]
    fn cjk_runs_are_contiguous_deduplicated_and_skip_single_characters() {
        assert_eq!(cjk_runs("雷同"), vec!["雷同"]);
        assert_eq!(cjk_runs("PEG 估值 可投资性"), vec!["估值", "可投资性"]);
        assert_eq!(cjk_runs("无名小炒 and 无名小炒"), vec!["无名小炒"]);
        assert_eq!(cjk_runs("東京タワー"), vec!["東京タワー"]);
        assert_eq!(cjk_runs("한국어 검색"), vec!["한국어", "검색"]);
        assert!(
            cjk_runs("雷 同").is_empty(),
            "single characters stay recall-only"
        );
        assert!(cjk_runs("WAL rotation").is_empty());
        let repeated: String = (0..20).map(|i| format!("雷同{i}")).collect();
        assert_eq!(cjk_runs(&repeated).len(), 1, "a repeated run is one clause");
        let distinct = "一二 三四 五六 七八 九十 甲乙 丙丁 戊己 庚辛 壬癸";
        assert_eq!(cjk_runs(distinct).len(), MAX_CJK_RUNS);
    }

    /// #2580: `雷同` is one whitespace token, so the exact-phrase leg never
    /// fired and the body ORed `雷` / `同`. Every content field the
    /// `multi_match` names now gets the run as a phrase, code and prose alike.
    #[test]
    fn a_cjk_query_gets_a_phrase_clause_per_run_on_content_fields() {
        let code = search_query_body("雷同", 5);
        assert_eq!(
            phrases_on(&code, "body"),
            vec![("雷同".to_string(), CJK_RUN_PHRASE_BOOST)]
        );
        let prose_names = names(&["title", "text", "body"]);
        let prose = search_query_body_for_mapping("青椒猪杂", 5, Some(&prose_names), None);
        for field in ["text", "body"] {
            assert_eq!(
                phrases_on(&prose, field),
                vec![("青椒猪杂".to_string(), CJK_RUN_PHRASE_BOOST)],
                "{field}"
            );
        }
        let mixed = search_query_body("PEG 估值 可投资性", 5);
        let body_phrases: Vec<String> = phrases_on(&mixed, "body")
            .into_iter()
            .map(|(q, _)| q)
            .collect();
        assert_eq!(body_phrases, vec!["PEG 估值 可投资性", "估值", "可投资性"]);
        let latin = search_query_body("rotate the WAL segment", 5);
        assert_eq!(
            phrases_on(&latin, "body").len(),
            1,
            "no CJK clause for Latin text"
        );
        let keyword = names(&["name", "code"]);
        let only_keywords = search_query_body_for_mapping("雷同", 5, Some(&keyword), None);
        assert!(phrases_on(&only_keywords, "body").is_empty());
    }

    #[test]
    fn dotted_member_splits_type_and_member_but_not_file_names() {
        assert_eq!(
            dotted_member("Position.open_price"),
            Some(("Position", "open_price"))
        );
        assert_eq!(
            dotted_member(" ActiveUtils.activeDialog "),
            Some(("ActiveUtils", "activeDialog"))
        );
        assert_eq!(dotted_member("Tree::flush"), Some(("Tree", "flush")));
        assert_eq!(
            dotted_member("a.b.Position.open"),
            Some(("Position", "open"))
        );
        for not_member in [
            "search.rs",
            "README.md",
            "config.TOML",
            "1.2.3",
            "Position",
            "Position.",
            ".mensaje_motor",
            "Position.open price",
            "serde@1.0",
        ] {
            assert_eq!(dotted_member(not_member), None, "{not_member}");
        }
    }

    /// #2426: the body searches the split words and puts the owner's
    /// definition (`term name = Position`) above a method named `open_price`.
    #[test]
    fn a_dotted_member_query_ranks_the_owner_definition() {
        let body = search_query_body("Position.open_price", 5);
        let all = clauses(&body);
        assert!(
            all.iter()
                .any(|c| c.pointer("/multi_match/query") == Some(&json!("Position open_price"))),
            "{body}"
        );
        let term_boost = |value: &str| {
            all.iter()
                .find(|c| c.pointer("/term/name/value") == Some(&json!(value)))
                .and_then(|c| c.pointer("/term/name/boost"))
                .and_then(Value::as_f64)
        };
        assert_eq!(term_boost("Position"), Some(MEMBER_OWNER_NAME_BOOST));
        assert_eq!(term_boost("open_price"), Some(MEMBER_NAME_BOOST));
        assert!(all
            .iter()
            .any(|c| c.pointer("/bool/must/0/match_phrase/defs") == Some(&json!("Position"))));
        let prose_names = names(&["title", "text"]);
        let prose =
            search_query_body_for_mapping("Position.open_price", 5, Some(&prose_names), None);
        let rendered = prose.to_string();
        assert!(!rendered.contains("\"name\""), "{rendered}");
        assert!(!rendered.contains("\"defs\""), "{rendered}");
        assert!(rendered.contains("Position open_price"), "{rendered}");
        // An identifier without a dot keeps the historical three clauses.
        let plain = search_query_body("open_price", 5);
        assert_eq!(clauses(&plain).len(), 3, "{plain}");
    }

    #[test]
    fn exact_ids_are_found_canonicalised_and_deduplicated() {
        assert_eq!(
            exact_ids("is cve-2021-44228 (CVE-2021-44228), fixed?"),
            vec!["CVE-2021-44228"]
        );
        assert_eq!(
            exact_ids("GHSA-JFH8-C2JP-5V3Q and RUSTSEC-2021-0124"),
            vec!["GHSA-jfh8-c2jp-5v3q", "RUSTSEC-2021-0124"]
        );
        assert_eq!(
            exact_ids("tokio@1.18.4 @scope/pkg@2.0.0-rc.1"),
            vec!["tokio@1.18.4", "@scope/pkg@2.0.0-rc.1"]
        );
        for none in [
            "CVE-21-1",
            "CVE-2021-12",
            "GHSA-abc-defg-hijk",
            "user@example.com",
            "ci@host.example:org/repo",
            "WAL rotation",
            "@1.2.3",
        ] {
            assert!(exact_ids(none).is_empty(), "{none}");
        }
    }

    /// #1788: `term` on the keyword ID fields only when a read mapping names
    /// them — an autoindex index is strict and maps no `id`.
    #[test]
    fn an_exact_id_gets_term_clauses_only_on_mapped_id_fields() {
        let pack = names(&["id", "aliases", "title", "body"]);
        let none = BTreeSet::new();
        let body =
            search_query_body_for_mapping("CVE-2021-44228 log4j", 3, Some(&pack), Some(&none));
        let all = clauses(&body);
        for field in ["id", "aliases"] {
            let clause = all
                .iter()
                .find(|c| {
                    c.pointer(&format!("/constant_score/filter/term/{field}"))
                        == Some(&json!("CVE-2021-44228"))
                })
                .unwrap_or_else(|| panic!("no {field} term in {body}"));
            assert_eq!(
                clause.pointer("/constant_score/boost"),
                Some(&json!(EXACT_ID_TERM_BOOST))
            );
        }
        assert!(phrases_on(&body, "body")
            .contains(&("CVE-2021-44228".to_string(), EXACT_ID_PHRASE_BOOST)));
        let historical = search_query_body("CVE-2021-44228", 3).to_string();
        assert!(!historical.contains("\"id\""), "{historical}");
        assert!(!historical.contains("\"aliases\""), "{historical}");
        let numeric = names(&["id"]);
        let num_body =
            search_query_body_for_mapping("CVE-2021-44228", 3, Some(&pack), Some(&numeric));
        assert!(!num_body.to_string().contains("\"id\""));
        let words = search_query_body_for_mapping("log4j remote code", 3, Some(&pack), None);
        assert!(!words.to_string().contains("\"aliases\""));
    }

    #[test]
    fn all_terms_first_filters_every_token_and_leaves_scoring_alone() {
        let body = search_query_body("heap overflow parser", 10);
        let tiered = all_terms_first_body(&body, "heap overflow parser").expect("tiered");
        assert_eq!(
            tiered["query"]["bool"]["should"],
            body["query"]["bool"]["should"]
        );
        let must = tiered
            .pointer("/query/bool/filter/0/bool/must")
            .and_then(Value::as_array)
            .unwrap();
        let tokens: Vec<&str> = must
            .iter()
            .filter_map(|m| m.pointer("/multi_match/query").and_then(Value::as_str))
            .collect();
        assert_eq!(tokens, vec!["heap", "overflow", "parser"]);
        assert!(all_terms_first_body(&search_query_body("Token", 10), "Token").is_none());
    }

    #[test]
    fn tiered_merge_keeps_first_tier_then_fills_without_duplicates() {
        let h = |id: &str| json!({"_index": "i", "_id": id});
        let first = vec![h("a"), h("b")];
        let fallback = vec![h("c"), h("a"), h("d"), h("e")];
        let merged = merge_tiered_hits(&first, &fallback, 4);
        let ids: Vec<&str> = merged.iter().map(|m| m["_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
        assert_eq!(merge_tiered_hits(&first, &fallback, 1).len(), 1);
    }
}
