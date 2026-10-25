//! `xerj def "<symbol>"` — go-to-definition over a XERJ index.
//!
//! `xerj search` answers "find me something relevant"; this answers the
//! sharper question an agent doing reference coding actually has most of the
//! time: "where is this symbol DEFINED, and what is its signature?". It leans
//! on the per-symbol documents a code autoindex writes (issue #500 —
//! `name`/`kind`/`line`/`code`, with `name` mapped as a keyword), so an exact
//! identifier is a `term` hit at the top, and falls back to the file-level
//! `defs` text for indexes built before symbol docs existed. Output is one
//! `path:line` plus the declaration per hit — the measured shape that lets an
//! agent act in one call instead of re-searching (~570 output tokens vs a
//! whole-file dump).

use std::collections::BTreeSet;

use crate::esclient::Es;
use crate::search::{
    corpus_snapshot, empty_lookup_report, field_lookup_ok, hit_location, is_identifier,
    mapped_property_names, numeric_property_names, parse_count_arg, passage_text,
    query_failure_message, weighted_query_fields,
};
use serde_json::{json, Value};

const USAGE: &str = "\
xerj def — find where a symbol is defined (go-to-definition, no scripts)
Default ranking is lexical: it matches the words you typed, and it is not a neural reading of the code.

USAGE:
    xerj def [OPTIONS] \"<symbol or capability>\"

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --prefix <P>    one index prefix to search (default \"ax\" — what `xerj autoindex` writes);
                    no comma lists
    --api-key <K>   Authorization (or $XERJ_API_KEY; --insecure nodes need none)
    -k <N>          number of definitions to return (default 5)
    --kind <KIND>   filter by symbol kind (function, class, method, ...)
    --lang <LANG>   filter by language (python, rust, typescript, ...)
    --json          print the raw response instead of formatted definitions

EXAMPLE:
    xerj def IOSIntfLine                       # exact identifier -> its declaration
    xerj def \"euler angles to rotation matrix\"  # plain words also resolve the definer

SEE ALSO:
    xerj search \"<plain words>\"    ranked passages when you don't know the name
";

/// How much of a declaration `xerj def` prints. `xerj search --full`
/// defaults to the same 600 characters. Two lines hid a constant that
/// sat further down a real signature.
const DECLARATION_PREVIEW_CHARS: usize = 600;

/// The declaration an agent can act on: later short lines stay, and a
/// passage longer than [`DECLARATION_PREVIEW_CHARS`] stops. The first
/// line is always kept, cut with an ellipsis when that line alone is
/// over the budget.
pub(crate) fn declaration_preview(text: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    for (i, line) in text.lines().enumerate() {
        let line_chars = line.chars().count();
        let extra = line_chars + usize::from(i > 0);
        if i > 0 && chars + extra > DECLARATION_PREVIEW_CHARS {
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        if i == 0 && line_chars > DECLARATION_PREVIEW_CHARS {
            out.extend(line.chars().take(DECLARATION_PREVIEW_CHARS));
            out.push('…');
            break;
        }
        out.push_str(line);
        chars += extra;
    }
    out
}

/// Historical `xerj def` multi_match weights. Order is part of the
/// code-corpus query.
const DEF_MM_FIELDS: &[(&str, &str)] = &[
    ("name", "name^4"),
    ("defs", "defs^3"),
    ("code", "code^2"),
    ("title", "title"),
];

/// Joined into the recall floor only when the target maps no `name` /
/// `code` / `defs` (#1601). `body` is not in the code-shaped list above.
const DEF_PROSE_EXTRA_FIELDS: &[&str] = &["body", "text", "impl", "message"];

/// Symbol kinds that are the definition a `xerj def` caller wants when
/// several hits share the name. A type alias and a `mod` line are not in
/// this list.
const PREFERRED_KINDS: &[&str] = &[
    "struct",
    "class",
    "function",
    "method",
    "enum",
    "trait",
    "interface",
    "impl",
    "protocol",
];

/// `ax_path` globs for tests, examples, fuzz targets, and benches. `*`
/// matches across `/` (the engine's wildcard is "zero or more chars").
const DEMOTED_PATH_GLOBS: &[&str] = &[
    "tests/*",
    "*/tests/*",
    "test/*",
    "*/test/*",
    "examples/*",
    "*/examples/*",
    "example/*",
    "*/example/*",
    "fuzz/*",
    "*/fuzz/*",
    "fuzz_targets/*",
    "*/fuzz_targets/*",
    "benches/*",
    "*/benches/*",
    "bench/*",
    "*/bench/*",
];

/// Fields `xerj def` may name. A numeric mapping of one of these on any
/// index drops it from the wildcard (#1284), including `term` / `match_phrase`
/// and the kind/`pub` ranking weights.
const DEF_LOOKUP_FIELDS: &[&str] = &[
    "name", "defs", "code", "title", "body", "text", "impl", "message", "kind", "language",
];

/// True only when a mapping was actually read and names `field`. `None` and
/// an empty set stay on the historical query (`field_mapped` treats both as
/// "assume the code fields"), so a new clause that is not in that historical
/// shape must use this and not [`field_lookup_ok`].
fn explicitly_mapped(mapped: Option<&BTreeSet<String>>, field: &str) -> bool {
    mapped.is_some_and(|set| set.contains(field))
}

/// The `xerj def` query body: definition-first ranking over the per-symbol
/// docs, narrowed by the optional `--kind` / `--lang` filters.
///
/// `mapped` is the union of property names under the prefix. `None` keeps
/// the historical code-shaped clauses. A read mapping drops every lookup
/// clause whose field no index maps, including `--kind` / `--lang` — those
/// `term` filters 400 a strict index the same way `name` does (#1601).
fn def_query(
    query: &str,
    kind: Option<&str>,
    lang: Option<&str>,
    k: usize,
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
) -> Value {
    let built = build_def_query(query, kind, lang, k, mapped, numeric);
    let empty = built
        .pointer("/query/bool/should")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    // A numeric field must stay dropped. Restoring the historical query
    // would name it again. `minimum_should_match: 1` with an empty `should`
    // matches nothing, which is the safe result.
    let numeric_lookup =
        numeric.is_some_and(|set| DEF_LOOKUP_FIELDS.iter().any(|name| set.contains(*name)));
    if empty && !numeric_lookup {
        return build_def_query(query, kind, lang, k, None, None);
    }
    built
}

/// Build the CLI/MCP definition query, including the wider local-rerank window.
/// An unreadable mapping keeps the historical code query; numeric fields on
/// any index in the scope are excluded from text lookups and filters.
pub fn definition_query(
    query: &str,
    kind: Option<&str>,
    lang: Option<&str>,
    k: usize,
    mapping: Option<&Value>,
) -> Value {
    let mapped = mapping.and_then(mapped_property_names);
    let numeric = mapped.as_ref().and(mapping.map(numeric_property_names));
    def_query(
        query,
        kind,
        lang,
        def_collect_size(k),
        mapped.as_ref(),
        numeric.as_ref(),
    )
}

/// Rank the fetched window, keep the best hit per location, and return `k`
/// definitions. Only `hits.hits` is replaced: totals still describe engine
/// matches before deduplication, just as in `xerj def --json`.
pub fn finalize_definition_response(response: &mut Value, query: &str, k: usize) -> Vec<Value> {
    let mut hits = response
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rerank_def_hits(&mut hits, query);
    let mut hits = unique_locations(hits);
    hits.truncate(k);
    if let Some(slot) = response.pointer_mut("/hits/hits") {
        *slot = Value::Array(hits.clone());
    }
    hits
}

fn build_def_query(
    query: &str,
    kind: Option<&str>,
    lang: Option<&str>,
    k: usize,
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
) -> Value {
    // Definition-first ranking, measured at 94% correct-file-at-rank-1 on an
    // 80-task cross-file benchmark (vs 38% for body-text search):
    //   - `term name`: the whole query IS the identifier — keyword-exact on
    //     the per-symbol docs, the strongest signal, boost 12.
    //   - `match_phrase defs`: the file-level definition list; also carries
    //     pre-#500 indexes that have no symbol docs, boost 8.
    //   - `multi_match`: recall floor so plain-words queries ("euler angles to
    //     rotation matrix") still surface the definer via `defs`/`code` text.
    let mut should = Vec::new();
    if field_lookup_ok(mapped, numeric, "name") {
        should.push(json!({ "term": { "name": { "value": query, "boost": 12 } } }));
    }
    if field_lookup_ok(mapped, numeric, "defs") {
        should.push(json!({ "match_phrase": { "defs": { "query": query, "boost": 8 } } }));
    }
    let mut fields = weighted_query_fields(
        DEF_MM_FIELDS,
        DEF_PROSE_EXTRA_FIELDS,
        mapped,
        numeric,
        false,
    );
    // A phrase ("flush the tree to disk") lives in the symbol body (`impl`),
    // not in the declaration. Only when the read mapping actually has `impl`
    // and no index maps it as numeric: the historical code query
    // (mapped = None) does not name that field, and a numeric `impl` would
    // fail the wildcard the same way a numeric `name` does.
    if !is_identifier(query)
        && explicitly_mapped(mapped, "impl")
        && !numeric.is_some_and(|set| set.contains("impl"))
        && !fields.iter().any(|f| f.split('^').next() == Some("impl"))
    {
        fields.push("impl^2".to_string());
    }
    if !fields.is_empty() {
        should.push(json!({ "multi_match": {
            "query": query,
            "fields": fields,
            "type": "most_fields"
        }}));
    }
    // Same coverage clauses as `xerj search` (#2327). An identifier does not
    // grow them: `coverage_match_clauses` is empty below three tokens, so
    // `xerj def Token` stays an exact `name` lookup.
    should.extend(xerj_common::code_query::coverage_match_clauses(
        query, &fields,
    ));
    if is_identifier(query) && field_lookup_ok(mapped, numeric, "code") {
        // `code` holds the declaration text; a phrase hit there ("def <name>(",
        // "class <name>:") is a definition even when `name` casing differs.
        should.push(json!({ "match_phrase": { "code": { "query": query, "boost": 4 } } }));
    }
    let mut filter = Vec::new();
    if let Some(kd) = kind {
        if field_lookup_ok(mapped, numeric, "kind") {
            filter.push(json!({ "term": { "kind": kd } }));
        }
    }
    if let Some(lg) = lang {
        if field_lookup_ok(mapped, numeric, "language") {
            filter.push(json!({ "term": { "language": lg } }));
        }
    }
    // A filter clause makes every `should` optional, so without this a
    // `--kind`/`--lang` filter alone matches: a name defined nowhere came
    // back as every definition of that kind instead of "no definition".
    // Kind and `pub` weights are applied in [`rerank_def_hits`] on the
    // returned page. They used to be a `function_score` filter. On the tokio
    // index (12,107 docs) that wrapper took 422ms of engine time against 6ms
    // for this bool; a `wildcard` on `ax_path` and a `match` on `code` inside
    // the function are what made the scan leave the bounded top-k path.
    json!({
        "size": k,
        "query": {
            "bool": {
                "should": should,
                "filter": filter,
                "minimum_should_match": 1
            }
        },
        "_source": ["ax_path", "path", "line", "start_line", "kind", "language", "name", "code", "page", "cell"],
        "fields": ["_passage"]
    })
}

/// How many hits to fetch before the local rerank. The printed page is `k`.
/// The extra window is what lets a `pub struct` just outside the top `k`
/// outrank a higher-BM25 test double. Capped so `-k 1000` does not pull
/// every match.
pub(crate) fn def_collect_size(k: usize) -> usize {
    const WINDOW: usize = 40;
    const CAP: usize = 200;
    k.saturating_add(WINDOW).min(k.max(CAP))
}

/// `*` is zero or more characters, including `/` — the same reading the
/// engine's `wildcard` query uses for [`DEMOTED_PATH_GLOBS`].
fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(pattern: &[u8], text: &[u8]) -> bool {
        let mut i = 0;
        let mut j = 0;
        while i < pattern.len() {
            if pattern[i] == b'*' {
                while i < pattern.len() && pattern[i] == b'*' {
                    i += 1;
                }
                if i == pattern.len() {
                    return true;
                }
                while j <= text.len() {
                    if rec(&pattern[i..], &text[j..]) {
                        return true;
                    }
                    if j == text.len() {
                        return false;
                    }
                    j += 1;
                }
                return false;
            }
            if j >= text.len() || pattern[i] != text[j] {
                return false;
            }
            i += 1;
            j += 1;
        }
        j == text.len()
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

/// True when `path` is a test, example, fuzz target, or bench
/// (`tests/…`, `examples/…`, `fuzz/…`, `benches/…`, at any directory depth).
///
/// `xerj def` withholds the preferred-kind bonus on these paths.
/// Plain-word `xerj search` and the plain-string `xerj_search` tool apply
/// [`penalize_demoted_paths`] with the same predicate (#2122) — one set of
/// path rules, not a second copy. `*` matches across `/`.
pub fn path_is_demoted(path: &str) -> bool {
    DEMOTED_PATH_GLOBS.iter().any(|glob| glob_match(glob, path))
}

/// Share of its BM25 `_score` a demoted path keeps.
///
/// Halving puts a test file that only barely outscored the implementation
/// behind it, and leaves a test file that scored more than twice as high —
/// the query is about that test — in front. The hit is never removed.
pub const DEMOTED_PATH_SCORE_FACTOR: f64 = 0.5;

fn source_path_for_demotion(src: &Value) -> &str {
    src.get("ax_path")
        .or_else(|| src.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Ranking penalty for plain-word search over a code index (#2122).
///
/// Demoted paths keep [`DEMOTED_PATH_SCORE_FACTOR`] of their `_score`, then
/// the slice is stable-sorted by that score. This is not a filter: a page
/// that is only tests is unchanged in order, and a test that still leads
/// after the penalty stays first. Paths the predicate does not match,
/// including `*.test.ts` and `fixtures/`, are left alone. Hits with no
/// numeric `_score` are not given one.
pub fn penalize_demoted_paths(hits: &mut [Value]) {
    let mut adjusted: Vec<(f64, Option<f64>)> = Vec::with_capacity(hits.len());
    let mut any = false;
    for hit in hits.iter() {
        let src = hit.get("_source").unwrap_or(&Value::Null);
        let demoted = path_is_demoted(source_path_for_demotion(src));
        let base = hit.get("_score").and_then(Value::as_f64);
        let (sort_score, rewrite) = match base {
            Some(base) if demoted => {
                let penalized = base * DEMOTED_PATH_SCORE_FACTOR;
                (penalized, Some(penalized))
            }
            Some(base) => (base, None),
            None => (0.0, None),
        };
        any |= demoted;
        adjusted.push((sort_score, rewrite));
    }
    if !any {
        return;
    }
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| {
        adjusted[b]
            .0
            .partial_cmp(&adjusted[a].0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let original = hits.to_vec();
    for (place, old) in order.into_iter().enumerate() {
        let mut hit = original[old].clone();
        if let Some(score) = adjusted[old].1 {
            if let Some(obj) = hit.as_object_mut() {
                obj.insert("_score".into(), json!(score));
            }
        }
        hits[place] = hit;
    }
}

/// The old `function_score` filter matched the tokens `pub`, `public`, and
/// `export` on `code`. Split on non-letters so `publication` does not count
/// and `pub(crate)` does.
fn declaration_is_public(code: &str) -> bool {
    code.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| matches!(word, "pub" | "public" | "export"))
}

/// Add the kind and visibility weights to hits the bool query already
/// returned, then sort. A preferred kind scores +20 unless the path is a
/// test, example, fuzz target, or bench. A public declaration scores +5.
/// Ties keep the engine order.
pub(crate) fn rerank_def_hits(hits: &mut [Value], query: &str) {
    const KIND_WEIGHT: f64 = 20.0;
    const PUB_WEIGHT: f64 = 5.0;
    let adjusted: Vec<f64> = hits
        .iter()
        .map(|hit| {
            let base = hit.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
            let src = hit.get("_source").unwrap_or(&Value::Null);
            let mut bonus = 0.0;
            let kind = src.get("kind").and_then(Value::as_str).unwrap_or("");
            if PREFERRED_KINDS.contains(&kind) && !path_is_demoted(source_path_for_demotion(src)) {
                bonus += KIND_WEIGHT;
            }
            if src
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(declaration_is_public)
            {
                bonus += PUB_WEIGHT;
            }
            base + bonus
        })
        .collect();
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| {
        adjusted[b]
            .partial_cmp(&adjusted[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let original = hits.to_vec();
    for (place, old) in order.into_iter().enumerate() {
        let mut hit = original[old].clone();
        if let Some(obj) = hit.as_object_mut() {
            obj.insert("_score".into(), json!(adjusted[old]));
        }
        hits[place] = hit;
    }
    // After the kind/visibility sort, so a +20 preferred kind cannot put a
    // one-word type back above a definition that covers the question (#2327).
    xerj_common::xccode::render::demote_sparse_definition_hits(hits, query);
}

/// Entry point for the `def` subcommand. Returns a process exit code.
pub fn run_def_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut prefix = crate::search::DEFAULT_SEARCH_PREFIX.to_string();
    let mut k: usize = 5;
    let mut kind: Option<String> = None;
    let mut lang: Option<String> = None;
    let mut as_json = false;
    let mut query: Option<String> = None;

    // Skip argv[0] (binary) + argv[1] ("def").
    let mut it = xerj_common::argv::args_after_subcommand().into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return 0;
            }
            "--url" => url = it.next().unwrap_or_default(),
            "--prefix" => prefix = it.next().unwrap_or(prefix),
            "--api-key" => api_key = it.next(),
            "-k" => {
                k = match parse_count_arg("-k", it.next()) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("xerj def: {e}\n\n{USAGE}");
                        return 2;
                    }
                };
            }
            "--kind" => kind = it.next(),
            "--lang" => {
                lang = it
                    .next()
                    .map(|l| crate::extract::code::lang_filter_value(&l))
            }
            "--json" => as_json = true,
            "--disable-feedback" => {}
            other if !other.starts_with('-') && query.is_none() => query = Some(other.to_string()),
            other => {
                eprintln!("xerj def: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        }
    }

    let query = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            eprintln!("xerj def: a symbol or phrase is required.\n\n{USAGE}");
            return 2;
        }
    };
    let resolved = match crate::search::resolve_prefix(&prefix) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("xerj def: {e}\n\n{USAGE}");
            return 2;
        }
    };
    if let Some(note) = crate::search::prefix_rewrite_note(&prefix, &resolved) {
        eprintln!("xerj def: {note}");
    }
    let prefix = resolved.prefix;
    let pattern = resolved.pattern;
    if url.is_empty() {
        url = "http://localhost:9200".to_string();
    }
    if api_key.is_none() && xerj_common::localauth::url_is_loopback(&url) {
        if let Some((key, path)) = xerj_common::localauth::discover_local_admin_key_for_url(&url) {
            eprintln!(
                "xerj def: no --api-key/XERJ_API_KEY given; using the admin key at {}",
                path.display()
            );
            api_key = Some(key);
        }
    }

    let es = match Es::new(&url, api_key) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj def: could not build client for {url}: {e}");
            return 2;
        }
    };
    if let Err(e) = es.ping() {
        if crate::esclient::Es::ping_failure_is_auth(&e) {
            eprintln!("xerj def: the node at {url} answered, but authentication failed.\n{e:#}");
            return 2;
        }
        eprintln!(
            "xerj def: no XERJ node reachable at {url} ({e}).\n\n\
             Start one, then index a repo:\n  \
             xerj --insecure -d ./.xerj-data &\n  \
             xerj autoindex <folder>\n\n\
             (or point at an existing node with --url / $XERJ_URL)"
        );
        return 2;
    }

    let raw_mapping = es.get_json(&format!("/{pattern}/_mapping")).ok();
    let mapped = raw_mapping.as_ref().and_then(mapped_property_names);
    if let Some(set) = mapped.as_ref().filter(|s| !s.is_empty()) {
        if kind.is_some() && !set.contains("kind") {
            eprintln!("xerj def: --kind ignored; no index under this prefix maps `kind`");
        }
        if lang.is_some() && !set.contains("language") {
            eprintln!("xerj def: --lang ignored; no index under this prefix maps `language`");
        }
    }
    let body = definition_query(
        &query,
        kind.as_deref(),
        lang.as_deref(),
        k,
        raw_mapping.as_ref(),
    );
    let mut resp = match es.search(&pattern, &body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", query_failure_message("def", &url, &pattern, e));
            return 1;
        }
    };

    let hits = finalize_definition_response(&mut resp, &query, k);

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );
        return 0;
    }

    if hits.is_empty() {
        let snap = corpus_snapshot(&es, &prefix);
        let (lines, exit) = empty_lookup_report("def", &prefix, &query, false, &snap);
        for line in lines {
            println!("{line}");
        }
        return exit;
    }

    for h in &hits {
        let src = h.get("_source").cloned().unwrap_or(Value::Null);
        let loc = hit_location(h);
        let kind = src.get("kind").and_then(Value::as_str).unwrap_or("");
        let tag = if kind.is_empty() {
            String::new()
        } else {
            format!("  [{kind}]")
        };
        println!("\n─── {loc}{tag}");
        // The declaration, not the file: `code` on symbol docs is the
        // signature block; `_passage` covers file-level hits. The preview
        // keeps later short lines up to 600 characters — `xerj search
        // --full` exists for reading the whole file.
        let passage_owned = passage_text(h);
        let decl = src
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(passage_owned);
        if let Some(text) = decl {
            for l in declaration_preview(&text).lines() {
                println!("    {l}");
            }
        }
    }
    println!(
        "\n{} definition(s) from '{pattern}'. Cite path:line for anything you rely on.",
        hits.len()
    );
    0
}

/// The same `path:line` can come back twice: a symbol doc and a file-level
/// hit, or two passages of one declaration. `xerj def` is a location list,
/// so the first (highest-ranked) hit for a location is the one that is shown.
fn unique_locations(hits: Vec<Value>) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        if seen.insert(hit_location(&hit)) {
            out.push(hit);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_says_the_default_ranking_is_lexical() {
        assert!(
            USAGE.contains("Default ranking is lexical") && USAGE.contains("not a neural reading"),
            "{USAGE}"
        );
    }

    #[test]
    fn declaration_preview_keeps_a_later_constant_and_caps_a_huge_line() {
        let sig = "pub fn open(\n    path: &Path,\n    mode: u32,\n) -> Result<File> {\n    const MODE_MASK: u32 = 0o777;\n    let _ = path;\n";
        let preview = declaration_preview(sig);
        assert!(
            preview.contains("const MODE_MASK: u32 = 0o777;"),
            "a constant past line 2 is part of the signature block: {preview}"
        );
        assert!(preview.contains("pub fn open("), "{preview}");

        let huge = format!("pub fn wide({})", "a: i32, ".repeat(200));
        let capped = declaration_preview(&huge);
        assert!(
            capped.chars().count() <= DECLARATION_PREVIEW_CHARS + 1,
            "{capped}"
        );
        assert!(capped.ends_with('…'), "{capped}");
        assert!(capped.starts_with("pub fn wide("), "{capped}");
    }

    /// ES `bool` semantics: once a `filter` clause is present, `should`
    /// clauses are optional unless `minimum_should_match` says otherwise. A
    /// `--kind`/`--lang` filter must narrow the definitions of the name, not
    /// replace the name with "anything of this kind".
    #[test]
    fn the_same_path_and_line_is_listed_once() {
        let hit = |id: &str, line: u64| {
            json!({
                "_id": id,
                "_source": { "ax_path": "qabot/agents.py", "line": line, "code": "def _run():" }
            })
        };
        let kept = unique_locations(vec![hit("a", 266), hit("b", 266), hit("c", 10)]);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(kept[0]["_id"], "a");
        assert_eq!(kept[1]["_id"], "c");
    }

    #[test]
    fn kind_and_lang_filters_still_require_the_name_to_match() {
        for (kind, lang) in [
            (Some("function"), None),
            (None, Some("python")),
            (Some("class"), Some("rust")),
            (None, None),
        ] {
            let body = def_query("zzqq_nonexistent", kind, lang, 5, None, None);
            let b = body.pointer("/query/bool").expect("bool query");
            assert_eq!(b["minimum_should_match"], 1, "{body}");
            assert!(!b["should"].as_array().unwrap().is_empty());
            let filters = b["filter"].as_array().unwrap().len();
            assert_eq!(filters, kind.is_some() as usize + lang.is_some() as usize);
        }
    }

    /// `xerj def` prints with the same `hit_location` as search, so a PDF or
    /// notebook definition can cite `page` / `cell` only when the projection
    /// asks for them.
    #[test]
    fn def_source_projection_includes_page_and_cell() {
        let body = def_query("open_path", None, None, 5, None, None);
        let fields = body["_source"].as_array().unwrap();
        assert!(fields.iter().any(|f| f == "page"), "{body}");
        assert!(fields.iter().any(|f| f == "cell"), "{body}");
    }

    fn names_of(fields: &[&str]) -> BTreeSet<String> {
        let mut props = serde_json::Map::new();
        for f in fields {
            props.insert((*f).to_string(), json!({ "type": "text" }));
        }
        mapped_property_names(&json!({
            "c-docs": { "mappings": { "properties": props } }
        }))
        .expect("mapping shape")
    }

    /// #1601: `xerj def` on a corpus with no symbol fields must not send
    /// `term name`, `match_phrase defs`, or `match_phrase code`. An identifier
    /// query is the case that used to add the `code` leg.
    #[test]
    fn prose_mapping_drops_unmapped_definition_legs() {
        let prose = names_of(&["body", "title", "text", "impl"]);
        let body = def_query(
            "compressPayload",
            Some("function"),
            Some("markdown"),
            5,
            Some(&prose),
            None,
        );
        let should = body["query"]["bool"]["should"].as_array().unwrap();
        assert_eq!(should.len(), 1, "{body}");
        assert_eq!(
            should[0]["multi_match"]["fields"],
            json!(["title", "body", "text", "impl"])
        );
        assert!(
            body["query"]["bool"]["filter"]
                .as_array()
                .unwrap()
                .is_empty(),
            "kind and language are unmapped, so their term filters must not 400 the query: {body}"
        );
        let rendered = body["query"].to_string();
        assert!(!rendered.contains("\"defs\""), "{rendered}");
        assert!(!rendered.contains("\"name\""), "{rendered}");
        assert!(!rendered.contains("\"code\""), "{rendered}");
        assert!(!rendered.contains("\"kind\""), "{rendered}");
    }

    /// The measured code-corpus query is unchanged when those fields are mapped.
    #[test]
    fn code_mapping_keeps_the_historical_def_query() {
        let code = names_of(&[
            "name", "defs", "code", "title", "body", "kind", "language", "text",
        ]);
        let query = "compressPayload";
        assert_eq!(
            def_query(query, Some("function"), Some("rust"), 5, Some(&code), None),
            def_query(query, Some("function"), Some("rust"), 5, None, None)
        );
        assert_eq!(
            def_query(query, None, None, 5, Some(&BTreeSet::new()), None),
            def_query(query, None, None, 5, None, None)
        );
    }

    /// A struct/function outranks a type alias and a test double of the same
    /// name, and a `pub` declaration outranks a private one. The weights are
    /// applied to the hits already returned — not as a `function_score`,
    /// which scored every match (422ms vs 6ms on the tokio index).
    #[test]
    fn preferred_kinds_and_pub_are_weighted_and_aliases_are_demoted() {
        let body = def_query("Db", None, None, 5, None, None);
        let rendered = body["query"].to_string();
        assert!(
            !rendered.contains("function_score"),
            "the slow wrapper must stay out of the request: {body}"
        );
        assert!(
            !rendered.contains("ax_path"),
            "path demotion is local, not a wildcard filter: {rendered}"
        );
        assert_eq!(def_collect_size(5), 45);
        assert_eq!(def_collect_size(180), 200);
        assert_eq!(def_collect_size(500), 500);

        let test_struct = json!({
            "_score": 172.15,
            "_source": {
                "ax_path": "tokio/src/runtime/tests/task.rs",
                "name": "Runtime",
                "kind": "struct",
                "line": 384,
                "code": "#[derive(Clone)]\nstruct Runtime"
            }
        });
        let src_struct = json!({
            "_score": 170.17,
            "_source": {
                "ax_path": "tokio/src/runtime/runtime.rs",
                "name": "Runtime",
                "kind": "struct",
                "line": 97,
                "code": "#[derive(Debug)]\npub struct Runtime"
            }
        });
        let alias = json!({
            "_score": 170.17,
            "_source": {
                "ax_path": "tokio/src/runtime/runtime.rs",
                "name": "Runtime",
                "kind": "type",
                "line": 12,
                "code": "pub type Runtime = Handle"
            }
        });
        let mut hits = vec![test_struct, alias.clone(), src_struct];
        rerank_def_hits(&mut hits, "");
        assert_eq!(
            hits[0]["_source"]["ax_path"],
            "tokio/src/runtime/runtime.rs"
        );
        assert_eq!(hits[0]["_source"]["kind"], "struct");
        assert_eq!(hits[1]["_source"]["kind"], "type");
        assert!(
            hits[0]["_score"].as_f64().unwrap() > hits[2]["_score"].as_f64().unwrap(),
            "src struct must outrank the higher-BM25 test double: {hits:?}"
        );
        // The type alias shares the src path but is not a preferred kind.
        // It still gets the pub weight, so it sorts above the private test
        // struct and below the pub struct.
        assert!(hits[1]["_score"].as_f64().unwrap() > hits[2]["_score"].as_f64().unwrap());

        assert!(path_is_demoted("tokio/src/runtime/tests/task.rs"));
        assert!(path_is_demoted("examples/dump.rs"));
        assert!(path_is_demoted("fuzz/fuzz_targets/x.rs") || path_is_demoted("tokio/fuzz/a.rs"));
        assert!(!path_is_demoted("tokio/src/runtime/runtime.rs"));
        assert!(
            !path_is_demoted("src/Foo.test.tsx"),
            "a test suffix is not one of the path rules"
        );
        assert!(!path_is_demoted("src/fixtures/transcript.json"));
        assert!(!declaration_is_public("struct Runtime"));
        assert!(declaration_is_public("pub(crate) struct Runtime"));
        assert!(!declaration_is_public("publication"));
    }

    /// #2327: kind and `pub` weights run first. A one-word type with a higher
    /// score still finishes behind the definition that covers the question.
    /// An exact `Token` lookup does not move.
    #[test]
    fn a_one_word_type_does_not_outrank_a_multi_term_definition() {
        let token = json!({
            "_score": 400.0,
            "_source": {
                "ax_path": "src/types.ts",
                "name": "Token",
                "kind": "type",
                "line": 1,
                "code": "export type Token = string"
            }
        });
        let refresh = json!({
            "_score": 3.0,
            "_source": {
                "ax_path": "src/session.ts",
                "name": "refreshSession",
                "kind": "function",
                "line": 12,
                "code": "export function refreshSession(token: Token, handler: Handler): Session"
            }
        });
        let mut hits = vec![token, refresh];
        rerank_def_hits(&mut hits, "session token refresh handler");
        assert_eq!(hits[0]["_source"]["name"], "refreshSession");
        assert_eq!(hits[1]["_source"]["name"], "Token");

        let mut exact = vec![
            json!({
                "_score": 50.0,
                "_source": { "name": "Token", "kind": "type", "code": "export type Token = string", "ax_path": "a.ts" }
            }),
            json!({
                "_score": 10.0,
                "_source": { "name": "Alias", "kind": "type", "code": "type Alias = number", "ax_path": "b.ts" }
            }),
        ];
        rerank_def_hits(&mut exact, "Token");
        assert_eq!(exact[0]["_source"]["name"], "Token");
        assert_eq!(exact[1]["_source"]["name"], "Alias");
    }

    /// "flush the tree to disk" has to search the symbol body. An identifier
    /// does not grow that leg, and a mapping without `impl` does not either.
    #[test]
    fn a_phrase_searches_impl_only_when_the_mapping_has_it() {
        let code = names_of(&["name", "defs", "code", "title", "impl", "kind"]);
        let phrase = def_query("flush the tree to disk", None, None, 5, Some(&code), None);
        let fields = phrase["query"]["bool"]["should"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|c| c.get("multi_match"))
            .expect("multi_match");
        assert!(
            fields["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f.as_str() == Some("impl^2")),
            "{phrase}"
        );
        let ident = def_query("flush", None, None, 5, Some(&code), None);
        let rendered = ident.to_string();
        assert!(!rendered.contains("impl^2"), "{ident}");
        let no_impl = names_of(&["name", "defs", "code", "title", "kind"]);
        let phrase = def_query(
            "flush the tree to disk",
            None,
            None,
            5,
            Some(&no_impl),
            None,
        );
        let fields = phrase.to_string();
        assert!(
            !fields.contains("impl^2"),
            "no impl field, so the body leg must stay out: {phrase}"
        );
    }

    /// #1284: `name` mapped `long` on any index must not be a `term` or a
    /// `multi_match` leg, and `defs` mapped `double` must not be a phrase.
    /// `keyword` is not numeric, so a keyword-only `name` stays.
    #[test]
    fn a_numeric_name_or_defs_field_is_not_looked_up() {
        assert!(!numeric_property_names(&json!({
            "only": { "mappings": { "properties": { "name": { "type": "keyword" } } } }
        }))
        .contains("name"));
        let mapping = json!({
            "symbols": { "mappings": { "properties": {
                "name": { "type": "keyword" },
                "defs": { "type": "text" },
                "code": { "type": "text" },
                "title": { "type": "text" },
                "kind": { "type": "keyword" }
            }}},
            "metrics": { "mappings": { "properties": {
                "name": { "type": "long" },
                "defs": { "type": "double" },
                "kind": { "type": "integer" }
            }}}
        });
        let names = mapped_property_names(&mapping).unwrap();
        let numeric = numeric_property_names(&mapping);
        let body = def_query(
            "compressPayload",
            Some("function"),
            None,
            5,
            Some(&names),
            Some(&numeric),
        );
        let query = body["query"].to_string();
        assert!(!query.contains("\"name\""), "{query}");
        assert!(!query.contains("defs"), "{query}");
        assert!(!query.contains("kind"), "{query}");
        assert!(query.contains("code"), "{query}");
        assert!(query.contains("title"), "{query}");
        // `code` is still text. The numeric `kind` must not be named: that
        // filter would 400 the wildcard. The pub weight is applied locally.
        let should = body
            .pointer("/query/bool/should")
            .and_then(Value::as_array)
            .expect("should clauses");
        let fields = should
            .iter()
            .find_map(|clause| clause.get("multi_match"))
            .expect("recall floor")["fields"]
            .as_array()
            .unwrap();
        let bare: Vec<&str> = fields
            .iter()
            .map(|f| f.as_str().unwrap().split('^').next().unwrap())
            .collect();
        assert_eq!(bare, vec!["code", "title"]);
    }
}
