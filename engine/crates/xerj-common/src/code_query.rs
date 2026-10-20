//! Query shaping shared by the code-search surfaces: `xerj search`
//! (xerj-autoindex) and the `xerj_search` MCP tool (xerj-mcp), issue #1364.
//!
//! One implementation, so the CLI and the MCP tool cannot drift on which
//! natural-language glue words a multi-word question drops.

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
