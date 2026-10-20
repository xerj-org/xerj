//! `xerj search "<query>"` — the binary as a query client.
//!
//! Reference coding is "index a repo, then retrieve the definition before you
//! write". Indexing is already one command (`xerj autoindex`); this makes the
//! retrieval side one command too, so the whole loop is the binary plus a
//! running node — no wrapper scripts. It detects a node the same way the
//! flagship path does (health-ping the ES-compat endpoint) and runs a
//! `multi_match` biased toward symbol definitions, printing a passage to read
//! rather than a JSON blob to parse.

use std::collections::BTreeSet;

use crate::esclient::Es;
use serde_json::{json, Value};

const USAGE: &str = "\
xerj search — retrieve code from a running XERJ node (reference coding, no scripts)

USAGE:
    xerj search [OPTIONS] \"<what you need, in plain words>\"

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --prefix <P>    index prefix to search (default \"ax\" — what `xerj autoindex` writes)
    --api-key <K>   Authorization (or $XERJ_API_KEY; --insecure nodes need none)
    -k <N>          number of passages to return (default 5)
    --full <CHARS>  max characters of each passage to print (default 600)
    --json          print the raw response instead of formatted passages

EXAMPLE:
    xerj --insecure -d ./.xerj-data &                 # a node, running
    xerj autoindex ref/sled                           # index once
    xerj search \"fsync the WAL segment on rotation\"    # one line in, a passage out

SEE ALSO:
    xerj def <symbol>    exact go-to-definition when you already know the name
";

/// Extract the `_passage` snippet from a hit. The engine returns it under
/// `fields._passage` as an array whose first element is either a plain string
/// or an object carrying the text — tolerate both shapes rather than betting
/// on one (the shape has already bitten one external client).
pub(crate) fn passage_text(hit: &Value) -> Option<String> {
    let first = hit.pointer("/fields/_passage/0")?;
    match first {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => {
            if matches!(
                o.get("field").and_then(Value::as_str),
                Some("name" | "title" | "defs")
            ) && hit
                .pointer("/_source/code")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
            {
                return None;
            }
            o.get("text")
                .or_else(|| o.get("passage"))
                .or_else(|| o.get("snippet"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        _ => None,
    }
}

/// True when the query looks like a single code identifier (`IOSIntfLine`,
/// `euler_to_rotationmatrix`) rather than a plain-words phrase — the case
/// where `xerj def` is the better tool.
pub(crate) fn is_identifier(q: &str) -> bool {
    !q.is_empty()
        && !q.contains(char::is_whitespace)
        && q.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `name` / `code` / `defs` are the code-shaped legs (#916). A corpus that
/// maps none of them — markdown, logs, anything autoindex accepted that is
/// not source — must not be queried with those legs: autoindex indices are
/// created `strict_unknown_fields`, and a `term` or `match_phrase` on a
/// field no resolved index maps is HTTP 400 (#1601).
const CODE_SYMBOL_FIELDS: &[&str] = &["name", "code", "defs"];

/// Historical `xerj search` multi_match weights. Order is part of the
/// code-corpus query: do not reorder.
const SEARCH_MM_FIELDS: &[(&str, &str)] = &[
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
const PROSE_EXTRA_FIELDS: &[&str] = &["text", "impl", "message"];

/// Every field `xerj search` might name in `multi_match`, `term`, or
/// `match_phrase`. A numeric mapping of one of these on any index in the
/// wildcard fails the whole request (`For input string`, #1284).
const SEARCH_LOOKUP_FIELDS: &[&str] = &[
    "name", "code", "body", "defs", "title", "text", "impl", "message",
];

/// Union of property names in a `GET /{pattern}/_mapping` body.
///
/// `None` when `mapping` is not that shape, so a transport failure keeps the
/// historical code-shaped query. An empty object is a real answer (no
/// indices) and comes back empty — callers treat an empty set like an
/// unreadable mapping and do not drop clauses they cannot prove unmapped.
pub(crate) fn mapped_property_names(mapping: &Value) -> Option<BTreeSet<String>> {
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
pub(crate) fn numeric_property_names(mapping: &Value) -> BTreeSet<String> {
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
pub(crate) fn field_lookup_ok(
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
pub(crate) fn field_mapped(mapped: Option<&BTreeSet<String>>, field: &str) -> bool {
    match mapped {
        None => true,
        Some(set) if set.is_empty() => true,
        Some(set) => set.contains(field),
    }
}

/// True when the mapping was read and no index maps `name`, `code`, or `defs`.
pub(crate) fn code_symbol_fields_absent(mapped: Option<&BTreeSet<String>>) -> bool {
    match mapped {
        Some(set) if !set.is_empty() => CODE_SYMBOL_FIELDS.iter().all(|f| !set.contains(*f)),
        _ => false,
    }
}

/// `preferred` entries whose base field is mapped, then `extras` (bare names)
/// when the corpus maps no code-symbol field.
pub(crate) fn weighted_query_fields(
    preferred: &[(&str, &str)],
    extras: &[&str],
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
) -> Vec<String> {
    let mut out: Vec<String> = preferred
        .iter()
        .filter(|(name, _)| field_lookup_ok(mapped, numeric, name))
        .map(|(_, weighted)| (*weighted).to_string())
        .collect();
    if code_symbol_fields_absent(mapped) {
        if let Some(set) = mapped {
            for name in extras {
                if set.contains(*name)
                    && !numeric.is_some_and(|nums| nums.contains(*name))
                    && !out.iter().any(|f| f.split('^').next() == Some(*name))
                {
                    out.push((*name).to_string());
                }
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
    let shaped = xerj_common::code_query::normalize_code_search_query(query);
    let mut should = Vec::new();
    let fields = weighted_query_fields(SEARCH_MM_FIELDS, PROSE_EXTRA_FIELDS, mapped, numeric);
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
    json!({
        "size": k,
        "query": { "bool": { "should": should }},
        // No `body` here: whole-file bodies are the token flood (measured ~9x
        // the size of the matching passage). `_passage` carries the snippet.
        "_source": ["ax_path", "line", "start_line", "language", "kind", "code", "title"],
        "fields": ["_passage"]
    })
}

/// Extract the 1-based starting line number from `fields._passage[0].ordinal`
/// when `_passage` comes from a document body field (ignoring synthetic
/// single-line or symbol-list metadata fields such as `defs` or `title`).
///
/// Only valid for LEXICAL passages, where the engine defines `ordinal` as
/// the zero-based line index (`PassageMatch` in xerj-query). On a semantic
/// hit it is a chunk ordinal instead; `xerj search` and `xerj def` send
/// lexical queries only, so every passage they render is lexical.
pub(crate) fn passage_line(hit: &Value) -> Option<u64> {
    let first = hit.pointer("/fields/_passage/0")?.as_object()?;
    if matches!(
        first.get("field").and_then(Value::as_str),
        Some("defs" | "title" | "name")
    ) {
        return None;
    }
    first.get("ordinal").and_then(Value::as_u64).map(|o| o + 1)
}

/// Format the `path[:line]` header token for a search or definition hit.
pub(crate) fn hit_location(hit: &Value) -> String {
    let src = hit.get("_source").unwrap_or(&Value::Null);
    let path = src
        .get("ax_path")
        .or_else(|| src.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let line = src
        .get("line")
        .or_else(|| src.get("start_line"))
        .and_then(Value::as_u64)
        .or_else(|| passage_line(hit));
    match line {
        Some(n) => format!("{path}:{n}"),
        None => path.to_string(),
    }
}

/// What the node held when a search or def returned no hits.
///
/// `None` means that read failed. An empty `Some` is a real answer.
pub(crate) struct CorpusSnapshot {
    /// Indices matching `{prefix}-*`.
    pub under_prefix: Option<Vec<String>>,
    /// Non-system indices other than the autoindex catalog.
    pub all_indices: Option<Vec<String>>,
    /// `prefix` values on catalog run documents.
    pub catalog_prefixes: Option<Vec<String>>,
}

/// Lines to print, and the process exit code, when `xerj search` or
/// `xerj def` has no hits.
///
/// A prefix that matches no index exits `1` and names the prefixes already
/// on the node. The `xerj autoindex <folder>` hint is only for a node whose
/// catalog and index list are both empty — a miss against a real corpus is
/// not a reason to tell the operator to index again.
pub(crate) fn empty_lookup_report(
    command: &str,
    prefix: &str,
    query: &str,
    identifier: bool,
    snap: &CorpusSnapshot,
) -> (Vec<String>, i32) {
    let pattern = format!("{prefix}-*");
    let prefix_absent = matches!(snap.under_prefix, Some(ref names) if names.is_empty());
    let mut lines = Vec::new();
    if prefix_absent {
        lines.push(format!("no indices named {pattern}"));
    } else if command == "def" {
        lines.push(format!("no definition in {pattern} for: {query}"));
        lines.push(format!(
            "(broader ranked retrieval: xerj search \"{query}\")"
        ));
    } else {
        lines.push(format!("no match in {pattern} for: {query}"));
    }
    if command == "search" && identifier {
        lines.push(format!(
            "(looks like a symbol name — try: xerj def {query})"
        ));
    }

    // A missing catalog index (the search failed) on a node with no user
    // indices is an empty catalog. A failed read while indices exist is not:
    // those indices are listed below instead of the autoindex hint.
    let no_indices = matches!(snap.all_indices, Some(ref names) if names.is_empty());
    let no_prefixes = match &snap.catalog_prefixes {
        Some(prefixes) => prefixes.is_empty(),
        None => no_indices,
    };
    let catalog_empty = no_indices && no_prefixes;
    if catalog_empty {
        lines.push("(index a relevant repo first: xerj autoindex <folder>)".to_string());
    } else if let Some(prefixes) = snap.catalog_prefixes.as_ref().filter(|p| !p.is_empty()) {
        lines.push(format!("prefixes on this node: {}", prefixes.join(", ")));
        if let Some(other) = prefixes.iter().find(|p| p.as_str() != prefix) {
            lines.push(format!("try: xerj {command} --prefix {other} \"{query}\""));
        }
    } else if let Some(indices) = snap.all_indices.as_ref().filter(|n| !n.is_empty()) {
        lines.push(format!(
            "indices on this node: {}",
            join_capped(indices, 20)
        ));
        lines.push(
            "the catalog lists no prefixes; --prefix is the index name before the dataset slug"
                .to_string(),
        );
    }
    let exit = if prefix_absent { 1 } else { 0 };
    (lines, exit)
}

fn join_capped(items: &[String], cap: usize) -> String {
    if items.len() <= cap {
        items.join(", ")
    } else {
        format!(
            "{}, … ({} more)",
            items[..cap].join(", "),
            items.len() - cap
        )
    }
}

pub(crate) fn corpus_snapshot(es: &Es, prefix: &str) -> CorpusSnapshot {
    let under_prefix = es.cat_indices_json(&format!("{prefix}-*")).ok();
    let all_indices = es.cat_indices().ok().map(|rows| {
        let catalog = xerj_common::AUTOINDEX_CATALOG_INDEX;
        let mut names: Vec<String> = rows
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name != catalog)
            .collect();
        names.sort();
        names
    });
    CorpusSnapshot {
        under_prefix,
        all_indices,
        catalog_prefixes: catalog_run_prefixes(es),
    }
}

/// Distinct `prefix` values on catalog run documents, sorted. `None` when
/// the catalog cannot be read — that is not the same as an empty catalog.
fn catalog_run_prefixes(es: &Es) -> Option<Vec<String>> {
    let body = json!({
        "size": 200,
        "query": { "term": { "doc_kind": "run" } },
        "_source": ["prefix"]
    });
    let v = es
        .search(xerj_common::AUTOINDEX_CATALOG_INDEX, &body)
        .ok()?;
    let mut prefixes = BTreeSet::new();
    let hits = v.pointer("/hits/hits").and_then(Value::as_array)?;
    for hit in hits {
        if let Some(prefix) = hit.pointer("/_source/prefix").and_then(Value::as_str) {
            if !prefix.is_empty() {
                prefixes.insert(prefix.to_string());
            }
        }
    }
    Some(prefixes.into_iter().collect())
}

/// Entry point for the `search` subcommand. Returns a process exit code.
pub fn run_search_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut prefix = "ax".to_string();
    let mut k: usize = 5;
    let mut full: usize = 600;
    let mut as_json = false;
    let mut query: Option<String> = None;

    // Skip argv[0] (binary) + argv[1] ("search").
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
            "-k" => k = it.next().and_then(|s| s.parse().ok()).unwrap_or(k),
            "--full" => full = it.next().and_then(|s| s.parse().ok()).unwrap_or(full),
            "--json" => as_json = true,
            other if !other.starts_with('-') && query.is_none() => query = Some(other.to_string()),
            other => {
                eprintln!("xerj search: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        }
    }

    let query = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            eprintln!("xerj search: a query is required.\n\n{USAGE}");
            return 2;
        }
    };
    if url.is_empty() {
        url = "http://localhost:9200".to_string();
    }
    if api_key.is_none() && xerj_common::localauth::url_is_loopback(&url) {
        if let Some((key, path)) = xerj_common::localauth::discover_local_admin_key_for_url(&url) {
            eprintln!(
                "xerj search: no --api-key/XERJ_API_KEY given; using the admin key at {}",
                path.display()
            );
            api_key = Some(key);
        }
    }

    let es = match Es::new(&url, api_key) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj search: could not build client for {url}: {e}");
            return 2;
        }
    };

    // Detect the node. If nothing answers, say exactly how to start one rather
    // than dumping a transport error — this is the most common first-run miss.
    if let Err(e) = es.ping() {
        if Es::ping_failure_is_auth(&e) {
            eprintln!("xerj search: the node at {url} answered, but authentication failed.\n{e:#}");
            return 2;
        }
        eprintln!(
            "xerj search: no XERJ node reachable at {url} ({e}).\n\n\
             Start one, then index a repo:\n  \
             xerj --insecure -d ./.xerj-data &\n  \
             xerj autoindex <folder>\n\n\
             (or point at an existing node with --url / $XERJ_URL)"
        );
        return 2;
    }

    // Defs-first shape (same rationale as the catalog's suggested query, and
    // measured on an 80-task cross-file benchmark: the phrase clause on `defs`
    // is what ranks the file that DEFINES a symbol above files that merely
    // mention it — 38%→94% correct-file-at-rank-1 vs the plain multi_match).
    // The multi_match stays as the recall floor; the phrase clauses are
    // self-gating (a multi-word conceptual query that matches no `defs` line
    // simply contributes no score). `name` is a keyword field on symbol docs
    // (issue #500), so the `term` clause fires only when the whole query IS
    // the identifier — the strongest possible signal.
    //
    // Those legs name fields a non-code autoindex does not map. The indices
    // are strict, so the clause is HTTP 400 rather than a silent miss
    // (#1601). Read the mapping and drop a leg no resolved index maps; an
    // unreadable mapping keeps the code-shaped query.
    let pattern = format!("{prefix}-*");
    let raw_mapping = es.get_json(&format!("/{pattern}/_mapping")).ok();
    let mapped = raw_mapping.as_ref().and_then(mapped_property_names);
    // Types are only trusted when the name union was readable. An error body
    // has no properties, and treating that as "nothing is numeric" while the
    // names fall back to the historical query is the #1601 contract.
    let numeric = mapped
        .as_ref()
        .and(raw_mapping.as_ref().map(numeric_property_names));
    let body = search_query_body_for_mapping(&query, k, mapped.as_ref(), numeric.as_ref());
    let resp = match es.search(&pattern, &body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "xerj search: query failed against {url}/{pattern}: {e}\n\
                 (is the prefix right? `xerj autoindex` writes 'ax' by default — \
                 pass --prefix if you indexed under another.)"
            );
            return 1;
        }
    };

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );
        return 0;
    }

    let hits = resp
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if hits.is_empty() {
        let snap = corpus_snapshot(&es, &prefix);
        let (lines, exit) =
            empty_lookup_report("search", &prefix, &query, is_identifier(&query), &snap);
        for line in lines {
            println!("{line}");
        }
        return exit;
    }

    for h in &hits {
        let src = h.get("_source").cloned().unwrap_or(Value::Null);
        let score = h.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        let loc = hit_location(h);
        let kind = src.get("kind").and_then(Value::as_str).unwrap_or("");
        let tag = if kind.is_empty() {
            String::new()
        } else {
            format!("  [{kind}]")
        };
        println!("\n─── {loc}{tag}  (score {score:.2})");
        // Passage-first: `_passage` is the engine's matching snippet (an
        // array-wrapped object with a text payload); `code` is the symbol
        // declaration. Never the whole file body — that is the 9x token flood
        // this command exists to avoid.
        let passage_owned = passage_text(h);
        let passage = passage_owned
            .as_deref()
            .or_else(|| src.get("code").and_then(Value::as_str));
        if let Some(text) = passage {
            let snippet: String = text.chars().take(full).collect();
            for l in snippet.lines() {
                println!("    {l}");
            }
            if text.chars().count() > full {
                println!("    … (--full {} to see more)", full * 2);
            }
        }
    }
    println!(
        "\n{} passage(s) from '{pattern}'. Cite ax_path:line for anything you rely on.",
        hits.len()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_common::code_query::normalize_code_search_query;

    /// #1364: multi-word natural-language queries strip repeated/glue particles
    /// (`de`, `la`, `por`, `en`, `the`, `of`) so a 2-letter function named `de`
    /// cannot hijack `name^4`/`title` and unrelated questions do not return
    /// identical top hits, while single-identifier lookups (`de`), acronyms
    /// (`IO`, `WAL`), and 2-letter tech terms (`db`, `gc`, `tx`) are preserved.
    #[test]
    fn normalize_code_search_query_strips_multiword_glue_and_deduplicates() {
        assert_eq!(
            normalize_code_search_query(
                "factor de calibracion de la encuesta multiplicador de ponderacion"
            ),
            "factor calibracion encuesta multiplicador ponderacion"
        );
        assert_eq!(
            normalize_code_search_query("ponderacion de hogares por departamento en la encuesta"),
            "ponderacion hogares departamento encuesta"
        );
        assert_eq!(
            normalize_code_search_query("ingreso total del hogar correccion por inflacion"),
            "ingreso total hogar correccion inflacion"
        );
        // Single-token lookup for a function literally named `de` is untouched.
        assert_eq!(normalize_code_search_query("de"), "de");
        // Identifier punctuation survives a short lookup verbatim: the
        // reporter's control query `.mensaje_motor` (an R name with a leading
        // dot) must still reach the exact-`name` term clause unchanged.
        assert_eq!(
            normalize_code_search_query(".mensaje_motor"),
            ".mensaje_motor"
        );
        let lookup = search_query_body(".mensaje_motor", 5);
        assert_eq!(
            lookup["query"]["bool"]["should"][2]["term"]["name"]["value"],
            ".mensaje_motor"
        );
        assert_eq!(
            lookup["query"]["bool"]["should"][0]["multi_match"]["query"],
            ".mensaje_motor"
        );
        // Two-letter technical terms, uppercase acronyms, and alphanumeric tokens stay.
        assert_eq!(
            normalize_code_search_query("fsync the WAL segment on db tx rotation v2"),
            "fsync WAL segment db tx rotation v2"
        );
        assert_eq!(
            normalize_code_search_query("indicador de agua potable o68"),
            "indicador agua potable o68"
        );

        let body = search_query_body(
            "factor de calibracion de la encuesta multiplicador de ponderacion",
            5,
        );
        assert_eq!(
            body["query"]["bool"]["should"][0]["multi_match"]["query"],
            "factor calibracion encuesta multiplicador ponderacion"
        );
    }

    /// #1364: if `_passage` came back from a bare identifier metadata field
    /// (`name` or `title`) on a symbol doc that carries `_source.code`,
    /// `passage_text` yields `None` so the caller falls back to `_source.code`
    /// (`de <- function(col)`) instead of printing bare `de`.
    #[test]
    fn passage_text_skips_bare_name_field_when_source_code_is_present() {
        let hit = json!({
            "_source": {
                "ax_path": "hogares_ech.R",
                "line": 1221,
                "kind": "function",
                "name": "de",
                "title": "de",
                "code": "de <- function(col)"
            },
            "fields": {
                "_passage": [{
                    "field": "name",
                    "ordinal": 0,
                    "text": "de"
                }]
            }
        });
        assert_eq!(passage_text(&hit), None);
    }

    /// #1365: whole-file hits (`ax-code-*`, `ax-docs-md-*`, `ax-text-*`) carry
    /// no `_source.line` or `_source.start_line`, so `hit_location` must
    /// convert `fields._passage[0].ordinal` (0-based line index of the snippet)
    /// to a 1-based `:line` suffix when `_passage` comes from the file body
    /// (and ignore synthetic metadata fields like `defs` or `title`).
    #[test]
    fn whole_file_hit_uses_one_based_passage_ordinal_when_source_has_no_line() {
        let whole_file_hit = json!({
            "_source": {
                "ax_path": "crates/fjall/src/batch.rs",
                "language": "rust"
            },
            "fields": {
                "_passage": [{
                    "field": "body",
                    "ordinal": 41,
                    "start_offset": 1200,
                    "end_offset": 1450,
                    "text": "pub fn commit(&mut self) -> crate::Result<()> {\n    Ok(())\n}"
                }]
            }
        });
        assert_eq!(
            hit_location(&whole_file_hit),
            "crates/fjall/src/batch.rs:42"
        );

        let symbol_hit = json!({
            "_source": {
                "ax_path": "crates/fjall/src/batch.rs",
                "line": 108,
                "kind": "function"
            },
            "fields": {
                "_passage": [{
                    "field": "code",
                    "ordinal": 0,
                    "text": "pub fn commit(&mut self) -> crate::Result<()>"
                }]
            }
        });
        assert_eq!(hit_location(&symbol_hit), "crates/fjall/src/batch.rs:108");

        let synthetic_defs_hit = json!({
            "_source": {
                "ax_path": "crates/fjall/src/batch.rs"
            },
            "fields": {
                "_passage": [{
                    "field": "defs",
                    "ordinal": 0,
                    "text": "fn commit"
                }]
            }
        });
        assert_eq!(
            hit_location(&synthetic_defs_hit),
            "crates/fjall/src/batch.rs"
        );
    }

    fn index_mapping(fields: &[&str]) -> Value {
        let mut props = serde_json::Map::new();
        for f in fields {
            props.insert((*f).to_string(), json!({ "type": "text" }));
        }
        json!({ "c-docs": { "mappings": { "properties": props } } })
    }

    fn names_of(fields: &[&str]) -> BTreeSet<String> {
        mapped_property_names(&index_mapping(fields)).expect("mapping shape")
    }

    /// #1601: a markdown/prose corpus maps `body`/`title`/`text`/`impl` and
    /// none of `defs`/`name`/`code`. The code-shaped `term` and `match_phrase`
    /// legs are what strict autoindex indices refuse with HTTP 400.
    #[test]
    fn prose_mapping_drops_unmapped_code_legs() {
        let mapped = names_of(&["body", "title", "text", "impl"]);
        let body = search_query_body_for_mapping("retention ceiling", 5, Some(&mapped), None);
        let should = body["query"]["bool"]["should"].as_array().unwrap();
        assert_eq!(should.len(), 1, "{body}");
        assert!(should[0].get("match_phrase").is_none(), "{body}");
        assert!(should[0].get("term").is_none(), "{body}");
        assert_eq!(
            should[0]["multi_match"]["fields"],
            json!(["body", "title", "text", "impl"])
        );
        let rendered = body["query"].to_string();
        assert!(!rendered.contains("\"defs\""), "{rendered}");
        assert!(!rendered.contains("\"name\""), "{rendered}");
        assert!(!rendered.contains("\"code\""), "{rendered}");
    }

    /// A log index whose text lives in `message` still gets a query the
    /// strict mapping will accept.
    #[test]
    fn log_mapping_searches_message_and_names_no_code_field() {
        let mapped = names_of(&["message", "ts", "host"]);
        let body = search_query_body_for_mapping("SIP 403 Forbidden", 3, Some(&mapped), None);
        let should = body["query"]["bool"]["should"].as_array().unwrap();
        assert_eq!(should.len(), 1, "{body}");
        assert_eq!(should[0]["multi_match"]["fields"], json!(["message"]));
    }

    /// A code corpus — and an unreadable mapping — keep the #916 shape,
    /// including when the union also carries prose fields.
    #[test]
    fn code_mapping_and_unreadable_mapping_keep_the_historical_query() {
        let query = "fsync the WAL segment";
        let historical = search_query_body(query, 5);
        assert_eq!(
            search_query_body_for_mapping(query, 5, None, None),
            historical
        );
        let empty = BTreeSet::new();
        assert_eq!(
            search_query_body_for_mapping(query, 5, Some(&empty), None),
            historical
        );
        assert!(mapped_property_names(&json!({"error": "no such index"})).is_none());
        let code = names_of(&["name", "code", "body", "defs", "title", "text", "impl"]);
        assert_eq!(
            search_query_body_for_mapping(query, 5, Some(&code), Some(&BTreeSet::new())),
            historical
        );
        // One index in the union mapping `defs` is enough to keep the leg:
        // strict mode refuses a field no resolved index maps, not a field
        // some index does not map.
        let partial = names_of(&["body", "defs", "title"]);
        let body = search_query_body_for_mapping(query, 5, Some(&partial), None);
        let should = body["query"]["bool"]["should"].as_array().unwrap();
        assert!(
            should.iter().any(|c| c.get("match_phrase").is_some()),
            "{body}"
        );
        assert!(should.iter().all(|c| c.get("term").is_none()), "{body}");
        assert_eq!(
            should[0]["multi_match"]["fields"],
            json!(["body", "defs^3", "title"])
        );
    }

    fn snap(
        under: Option<Vec<&str>>,
        all: Option<Vec<&str>>,
        prefixes: Option<Vec<&str>>,
    ) -> CorpusSnapshot {
        let owned =
            |v: Option<Vec<&str>>| v.map(|names| names.into_iter().map(str::to_string).collect());
        CorpusSnapshot {
            under_prefix: owned(under),
            all_indices: owned(all),
            catalog_prefixes: owned(prefixes),
        }
    }

    #[test]
    fn a_missing_prefix_exits_nonzero_and_names_prefixes_on_the_node() {
        let (lines, exit) = empty_lookup_report(
            "search",
            "typo",
            "flush the tree",
            false,
            &snap(
                Some(vec![]),
                Some(vec!["sled-docs"]),
                Some(vec!["eng", "sled"]),
            ),
        );
        assert_eq!(exit, 1);
        let text = lines.join("\n");
        assert!(text.contains("no indices named typo-*"), "{text}");
        assert!(text.contains("prefixes on this node: eng, sled"), "{text}");
        assert!(
            text.contains("xerj search --prefix eng \"flush the tree\""),
            "{text}"
        );
        assert!(
            !text.contains("xerj autoindex"),
            "a node that already has a catalog must not be told to index: {text}"
        );
    }

    #[test]
    fn an_empty_catalog_keeps_the_autoindex_hint() {
        let (lines, exit) = empty_lookup_report(
            "search",
            "ax",
            "Db",
            true,
            &snap(Some(vec![]), Some(vec![]), Some(vec![])),
        );
        assert_eq!(exit, 1);
        let text = lines.join("\n");
        assert!(text.contains("xerj autoindex <folder>"), "{text}");
        assert!(text.contains("xerj def Db"), "{text}");
        assert!(!text.contains("prefixes on this node"), "{text}");

        let (missing_catalog, exit) = empty_lookup_report(
            "search",
            "ax",
            "Db",
            false,
            &snap(Some(vec![]), Some(vec![]), None),
        );
        assert_eq!(exit, 1);
        let text = missing_catalog.join("\n");
        assert!(
            text.contains("xerj autoindex <folder>"),
            "a node with no catalog index and no user indices is empty: {text}"
        );
    }

    #[test]
    fn a_miss_on_a_real_prefix_stays_exit_zero_and_lists_the_others() {
        let (lines, exit) = empty_lookup_report(
            "search",
            "sled",
            "not a thing",
            false,
            &snap(
                Some(vec!["sled-docs"]),
                Some(vec!["sled-docs", "eng-docs"]),
                Some(vec!["sled"]),
            ),
        );
        assert_eq!(exit, 0, "{lines:?}");
        let text = lines.join("\n");
        assert!(text.contains("no match in sled-*"), "{text}");
        assert!(text.contains("prefixes on this node: sled"), "{text}");
        assert!(!text.contains("try: xerj search --prefix"), "{text}");
        assert!(!text.contains("xerj autoindex"), "{text}");

        let (def_lines, def_exit) = empty_lookup_report(
            "def",
            "nope",
            "Db",
            false,
            &snap(Some(vec![]), None, Some(vec!["sled"])),
        );
        assert_eq!(def_exit, 1);
        let def_text = def_lines.join("\n");
        assert!(def_text.contains("no indices named nope-*"), "{def_text}");
        assert!(
            def_text.contains("xerj def --prefix sled \"Db\""),
            "{def_text}"
        );
    }

    #[test]
    fn indices_are_listed_when_the_catalog_has_no_prefix() {
        let (lines, exit) = empty_lookup_report(
            "search",
            "typo",
            "q",
            false,
            &snap(
                Some(vec![]),
                Some(vec!["sled-docs", "eng-logs"]),
                Some(vec![]),
            ),
        );
        assert_eq!(exit, 1);
        let text = lines.join("\n");
        assert!(
            text.contains("indices on this node: sled-docs, eng-logs"),
            "{text}"
        );
        assert!(text.contains("catalog lists no prefixes"), "{text}");
        assert!(!text.contains("xerj autoindex"), "{text}");
    }

    /// #1284: one index mapping `code` as `long` fails a wildcard `multi_match`
    /// or `match_phrase` on `code` for every index. Drop it. `keyword` `name`
    /// stays. A prose field that is numeric on any index is dropped too, and
    /// when that was the only lookup the query matches nothing rather than
    /// restoring the historical legs.
    #[test]
    fn a_numeric_field_is_dropped_from_multi_match_term_and_phrase() {
        let mapping = json!({
            "p-code": { "mappings": { "properties": {
                "name": { "type": "keyword" },
                "code": { "type": "text" },
                "defs": { "type": "text" },
                "title": { "type": "text" },
                "body": { "type": "text" }
            }}},
            "p-metrics": { "mappings": { "properties": {
                "code": { "type": "long" },
                "title": { "type": "integer" },
                "line": { "type": "long" }
            }}}
        });
        let names = mapped_property_names(&mapping).expect("mapping");
        let numeric = numeric_property_names(&mapping);
        assert!(numeric.contains("code") && numeric.contains("title") && numeric.contains("line"));
        assert!(!numeric.contains("name"), "keyword is not numeric");
        assert!(
            numeric_property_names(&json!({
                "only": { "mappings": { "properties": {
                    "half": { "type": "half_float" },
                    "scaled": { "type": "scaled_float" },
                    "unsigned": { "type": "unsigned_long" }
                }}}
            }))
            .len()
                == 3
        );

        let body = search_query_body_for_mapping("timeout", 5, Some(&names), Some(&numeric));
        let query = body["query"].to_string();
        assert!(!query.contains("code"), "{query}");
        assert!(!query.contains("title"), "{query}");
        assert!(query.contains("name"), "{query}");
        assert!(query.contains("defs"), "{query}");
        assert!(query.contains("body"), "{query}");
        let fields = body["query"]["bool"]["should"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|clause| clause.get("multi_match"))
            .unwrap()["fields"]
            .as_array()
            .unwrap();
        assert!(fields
            .iter()
            .all(|f| f.as_str().unwrap().split('^').next() != Some("code")));
        assert!(fields
            .iter()
            .all(|f| f.as_str().unwrap().split('^').next() != Some("title")));

        let prose = json!({
            "logs": { "mappings": { "properties": {
                "message": { "type": "double" }
            }}}
        });
        let prose_names = mapped_property_names(&prose).unwrap();
        let prose_numeric = numeric_property_names(&prose);
        let none =
            search_query_body_for_mapping("timeout", 5, Some(&prose_names), Some(&prose_numeric));
        assert!(none["query"].get("match_none").is_some(), "{none}");
    }
}
