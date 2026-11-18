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
