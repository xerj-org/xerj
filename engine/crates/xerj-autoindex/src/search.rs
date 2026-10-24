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
use crate::staleness;
use serde_json::{json, Value};
use xerj_common::xccode::demote_catalog_noise;

const USAGE: &str = "\
xerj search — retrieve code from a running XERJ node (reference coding, no scripts)
Default ranking is lexical: it matches the words you typed, and it is not a neural reading of the code.

USAGE:
    xerj search [OPTIONS] \"<what you need, in plain words>\"

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --prefix <P>    index prefix to search (default \"ax\" — what `xerj autoindex` writes)
    --api-key <K>   Authorization (or $XERJ_API_KEY; --insecure nodes need none)
    -k <N>          number of passages to return (default 5). A non-number is a
                    usage error, not a silent default
    --full <CHARS>  max characters of each passage to print (default 600)
    --json          print the raw response instead of formatted passages

EXAMPLE:
    xerj --insecure -d ./.xerj-data &                 # a node, running
    xerj autoindex ref/sled                           # index once
    xerj search \"fsync the WAL segment on rotation\"    # one line in, a passage out

SEE ALSO:
    xerj def <symbol>    exact go-to-definition when you already know the name

When the autoindex catalog records a folder on this machine for --prefix and a
file there is newer than the last index, or an indexed file has been removed,
the command prints the last index time and a reindex command, then prints the
passages. The check stats at most 4096 entries, stops after 100ms, and skips
hidden and ignored paths.
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

/// `xerj search` has no mode flag. `--mode semantic` and `--mode hybrid`
/// belong to `xerj code`. Saying "unexpected argument" and dumping USAGE
/// hides that, and it hides that those modes still use lexical feature
/// hashing unless the node was started with `--embed-mode neural`.
pub(crate) fn search_mode_refusal(asked: &str) -> String {
    let which = if asked.is_empty() {
        String::new()
    } else {
        format!(" ({asked})")
    };
    format!(
        "xerj search: no --mode{which}. This command always sends a lexical query. \
         xerj code --mode semantic and xerj code --mode hybrid are the commands that take a mode; \
         unless the node was started with --embed-mode neural they still embed with lexical feature hashing."
    )
}

/// One line for the human printer when the node says this lexical query
/// scored a `semantic_text` field and did not consult the embedding.
///
/// The response already carries `_xerj.hints[].reason`, which tells the
/// caller to POST a `semantic` query. That reason is the wrong sentence for
/// `xerj search`: this command has no semantic mode. `--json` returns the
/// hint unchanged.
pub(crate) fn lexical_semantic_note(resp: &Value) -> Option<&'static str> {
    let hints = resp.pointer("/_xerj/hints")?.as_array()?;
    let flagged = hints
        .iter()
        .any(|hint| hint.get("code").and_then(Value::as_str) == Some("lexical_on_semantic_text"));
    flagged.then_some(
        "xerj search: these hits are lexical; this command does not consult the embedding.",
    )
}

/// Text to print for one hit.
///
/// Passage first, then the symbol `code`. When the engine matched a document
/// but selected no passage and there is no `code`, a section `heading` or
/// document `title` is what the header would otherwise sit on top of — a
/// blank block.
pub(crate) fn hit_display_text(hit: &Value) -> Option<String> {
    if let Some(text) = passage_text(hit) {
        return Some(text);
    }
    let src = hit.get("_source")?;
    for key in ["code", "heading", "title"] {
        if let Some(text) = src
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(text.to_string());
        }
    }
    None
}

/// True when the query looks like a single code identifier (`IOSIntfLine`,
/// `euler_to_rotationmatrix`) rather than a plain-words phrase — the case
/// where `xerj def` is the better tool.
pub(crate) fn is_identifier(q: &str) -> bool {
    !q.is_empty()
        && !q.contains(char::is_whitespace)
        && q.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Whether a zero-hit `xerj search` should point at `xerj def`.
///
/// [`is_identifier`] stays the narrower predicate `xerj def` uses to shape
/// its query. This one also accepts a qualified name (`Tree::flush`): a
/// search that misses still reads as "try def", without changing how def
/// ranks that name. A phrase and a `path/file.rs` do not.
pub(crate) fn suggests_def(q: &str) -> bool {
    if is_identifier(q) {
        return true;
    }
    !q.is_empty()
        && !q.contains(char::is_whitespace)
        && !q.contains('/')
        && q.chars().any(|c| c.is_alphabetic())
        && q.contains(':')
        && q.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
}

pub(crate) use xerj_common::code_query::{
    field_lookup_ok, mapped_property_names, numeric_property_names, weighted_query_fields,
};
/// The mapping-aware query body is shared with the `xerj_search` MCP tool
/// (#1961); the implementation lives in `xerj_common::code_query` so the two
/// surfaces cannot drift again (#1601/#1602).
pub use xerj_common::code_query::{search_query_body, search_query_body_for_mapping};

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

/// Format the header token for a search or definition hit.
///
/// A source line wins (`path:line`). A PDF page or a notebook cell is cited
/// as `path page N` / `path cell N` — never as `:1` from a passage ordinal of
/// 0, which is a chunk or a section start, not a line. A body passage with no
/// page and no cell still uses the 1-based lexical ordinal.
pub(crate) fn hit_location(hit: &Value) -> String {
    let src = hit.get("_source").unwrap_or(&Value::Null);
    let path = src
        .get("ax_path")
        .or_else(|| src.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    // #2266: the line of the matched term, when the response was annotated.
    // `start_line` / `line` stay the passage or declaration start.
    if let Some(n) = hit
        .get("match_line")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
    {
        return xerj_common::xccode::passage::cite_path_line(path, n);
    }
    if let Some(n) = src
        .get("line")
        .or_else(|| src.get("start_line"))
        .and_then(Value::as_u64)
    {
        return xerj_common::xccode::passage::cite_path_line(path, n);
    }
    let page = hit
        .pointer("/fields/_passage/0/page")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .or_else(|| src.get("page").and_then(Value::as_u64).filter(|n| *n > 0));
    if let Some(n) = page {
        return format!("{path} page {n}");
    }
    if let Some(n) = src.get("cell").and_then(Value::as_u64).filter(|n| *n > 0) {
        return format!("{path} cell {n}");
    }
    // A row of a JSONL/CSV/JSON/YAML/spreadsheet file is one record: the
    // passage ordinal counts lines of that record's own text, not of the
    // file, so `rows.jsonl:1` named a different row for every hit (#1934).
    // Cite the record by its locator instead (a byte offset or a row number).
    if let Some(loc) = record_locator(src) {
        return format!("{path}#{loc}");
    }
    match passage_line(hit) {
        Some(n) => xerj_common::xccode::passage::cite_path_line(path, n),
        None => path.to_string(),
    }
}

/// The `ax_locator` of a record that is one row or element of a data file,
/// where a passage ordinal is not a file line: `b<byte offset>` (JSONL rows,
/// log records, byte-addressed chunks), `r<N>` (CSV rows), `row<N>` (HTML
/// table rows), `<sheet>!r<N>` (spreadsheet rows), `<key>:e<N>` (JSON array
/// elements) and `d<N>` / `d<N>e<N>` (YAML documents). Whole files (`file`),
/// code records and prose sections are left to the line logic.
fn record_locator(src: &Value) -> Option<&str> {
    let loc = src.get("ax_locator").and_then(Value::as_str)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let yaml = loc
        .strip_prefix('d')
        .is_some_and(|rest| match rest.split_once('e') {
            Some((doc, elem)) => digits(doc) && digits(elem),
            None => digits(rest),
        });
    let row = ["b", "r", "row"]
        .iter()
        .any(|p| loc.strip_prefix(p).is_some_and(digits));
    let sheet_row = loc.rsplit_once("!r").is_some_and(|(_, n)| digits(n));
    let element = loc.rsplit_once(":e").is_some_and(|(_, n)| digits(n));
    (row || yaml || sheet_row || element).then_some(loc)
}

/// A line an agent can cite: `_source.line` / `start_line`, else a lexical
/// passage ordinal. A `defs` / `title` / `name` passage has none.
fn citable_line(hit: &Value) -> Option<u64> {
    let src = hit.get("_source").unwrap_or(&Value::Null);
    src.get("line")
        .or_else(|| src.get("start_line"))
        .and_then(Value::as_u64)
        .or_else(|| passage_line(hit))
}

fn hit_path(hit: &Value) -> String {
    hit.get("_source")
        .and_then(|src| src.get("ax_path").or_else(|| src.get("path")))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Move a no-line hit to just after the last lined hit on the same path.
///
/// A `defs` catalog for `src/lib.rs` can outscore `src/lib.rs:255` because the
/// phrase boost is on the symbol list, not the declaration. An agent that
/// cites the first header then cites a file with no line. Other paths keep
/// their relative order, and a bare hit whose path has no lined sibling stays
/// where the score put it. This does not demote `defs` hits as a class: that
/// phrase clause is what ranks the defining file first.
pub(crate) fn prefer_citable_line_on_the_same_path(hits: &mut [Value]) {
    let mut last_lined: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, hit) in hits.iter().enumerate() {
        let path = hit_path(hit);
        if path.is_empty() {
            continue;
        }
        if citable_line(hit).is_some() {
            last_lined.insert(path, i);
        }
    }
    if last_lined.is_empty() {
        return;
    }
    let mut bare_of_mixed: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, hit) in hits.iter().enumerate() {
        let path = hit_path(hit);
        if last_lined.contains_key(&path) && citable_line(hit).is_none() {
            bare_of_mixed.entry(path).or_default().push(i);
        }
    }
    if bare_of_mixed.is_empty() {
        return;
    }
    let mut order = Vec::with_capacity(hits.len());
    for (i, hit) in hits.iter().enumerate() {
        let path = hit_path(hit);
        if bare_of_mixed
            .get(&path)
            .is_some_and(|idxs| idxs.contains(&i))
        {
            continue;
        }
        order.push(i);
        if last_lined.get(&path) == Some(&i) {
            if let Some(bares) = bare_of_mixed.get(&path) {
                order.extend(bares.iter().copied());
            }
        }
    }
    debug_assert_eq!(order.len(), hits.len());
    if order.iter().enumerate().all(|(n, i)| n == *i) {
        return;
    }
    let original = hits.to_vec();
    for (dest, src) in order.into_iter().enumerate() {
        hits[dest] = original[src].clone();
    }
}

/// How many hits to fetch before the local test/example penalty trims to `k`.
///
/// The extra window is the one `xerj def` uses, so an implementation just
/// outside the engine's top `k` can outrank a higher-BM25 test file. The
/// query stays a plain bool: a `function_score` wildcard on `ax_path` is the
/// path `xerj def` already measured at 422ms against 6ms.
pub fn search_fetch_size(k: usize) -> usize {
    crate::def::def_collect_size(k)
}

/// Order a plain-word search page.
///
/// On a code corpus, [`crate::def::penalize_demoted_paths`] halves the score
/// of test/example/fuzz/bench paths and the page is trimmed back to `page`
/// after the existing same-path and catalog passes. A prose corpus skips
/// the penalty and the trim — the node was asked for `page` hits already.
pub(crate) fn rank_plain_search_hits(
    hits: &mut Vec<Value>,
    code_corpus: bool,
    page: usize,
    query: &str,
) {
    if code_corpus {
        crate::def::penalize_demoted_paths(hits);
    }
    // A one-word symbol (`type Token`) must not stay ahead of a hit that
    // covers more terms of a multi-term question (#2327). One-token lookups
    // are unchanged inside that pass.
    xerj_common::xccode::render::demote_sparse_definition_hits(hits, query);
    prefer_citable_line_on_the_same_path(hits);
    let mut ordered = demote_catalog_noise(std::mem::take(hits));
    if code_corpus {
        ordered.truncate(page);
    }
    *hits = ordered;
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
/// on the node, in that same line. The line says it is not a search result:
/// it returns before any query runs, and a one-line reading of "no indices
/// named …" was taken for an instant empty search (#2321). The
/// `xerj autoindex <folder>` hint is only for a node whose catalog and index
/// list are both empty — a miss against a real corpus is not a reason to
/// tell the operator to index again.
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
        lines.push(missing_prefix_line(&pattern, snap));
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
        // The missing-prefix headline already lists these. A real miss (the
        // prefix exists, the query did not match) still needs the line.
        if !prefix_absent {
            lines.push(format!("prefixes on this node: {}", prefixes.join(", ")));
        }
        if let Some(other) = prefixes.iter().find(|p| p.as_str() != prefix) {
            lines.push(format!("try: xerj {command} --prefix {other} \"{query}\""));
        }
    } else if let Some(indices) = snap.all_indices.as_ref().filter(|n| !n.is_empty()) {
        if !prefix_absent {
            lines.push(format!(
                "indices on this node: {}",
                join_capped(indices, 20)
            ));
        }
        lines.push(
            "the catalog lists no prefixes; --prefix is the index name before the dataset slug"
                .to_string(),
        );
    }
    let exit = if prefix_absent { 1 } else { 0 };
    (lines, exit)
}

/// The refusal line for a prefix that matches nothing.
///
/// Prefixes from the catalog win. Index names are the fallback when the
/// catalog has none (an autoindex that aborted before writing a run or
/// dataset document still left `{prefix}-{slug}` indices).
fn missing_prefix_line(pattern: &str, snap: &CorpusSnapshot) -> String {
    let mut line = format!("no indices named {pattern} — this is not a search result");
    if let Some(prefixes) = snap.catalog_prefixes.as_ref().filter(|p| !p.is_empty()) {
        line.push_str(&format!("; prefixes on this node: {}", prefixes.join(", ")));
    } else if let Some(indices) = snap.all_indices.as_ref().filter(|n| !n.is_empty()) {
        line.push_str(&format!(
            "; indices on this node: {}",
            join_capped(indices, 20)
        ));
    }
    line
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

/// Distinct `prefix` values on catalog run and dataset documents, sorted.
///
/// Run documents are the historical source. Dataset documents carry the same
/// field and are written as indexing proceeds, so a run that aborted before
/// its run document (#2321) still names the prefix. `None` when the catalog
/// cannot be read — that is not the same as an empty catalog. A dataset
/// query that fails leaves the run prefixes in place.
fn catalog_run_prefixes(es: &Es) -> Option<Vec<String>> {
    let runs = catalog_kind_body(es, "run")?;
    let mut prefixes = prefixes_from_catalog_body(&runs);
    if let Some(datasets) = catalog_kind_body(es, "dataset") {
        for prefix in prefixes_from_catalog_body(&datasets) {
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
        prefixes.sort();
    }
    Some(prefixes)
}

fn catalog_kind_body(es: &Es, doc_kind: &str) -> Option<Value> {
    let body = json!({
        "size": 200,
        "query": { "term": { "doc_kind": doc_kind } },
        "_source": ["prefix"]
    });
    es.search(xerj_common::AUTOINDEX_CATALOG_INDEX, &body).ok()
}

fn prefixes_from_catalog_body(v: &Value) -> Vec<String> {
    let mut prefixes = BTreeSet::new();
    let Some(hits) = v.pointer("/hits/hits").and_then(Value::as_array) else {
        return Vec::new();
    };
    for hit in hits {
        if let Some(prefix) = hit.pointer("/_source/prefix").and_then(Value::as_str) {
            if !prefix.is_empty() {
                prefixes.insert(prefix.to_string());
            }
        }
    }
    prefixes.into_iter().collect()
}

/// What `xerj search` / `xerj def` print when the query request itself fails.
///
/// A missing index (`index_not_found_exception`, or a body that says
/// `no such index`) is the case the prefix sentence was written for. Every
/// other failure — including HTTP 400 `For input string`, which is a numeric
/// field being queried as text — must not tell the reader to change `--prefix`.
pub(crate) fn query_failure_message(
    command: &str,
    url: &str,
    pattern: &str,
    err: impl std::fmt::Display,
) -> String {
    let err = err.to_string();
    let mut msg = format!("xerj {command}: query failed against {url}/{pattern}: {err}");
    let lower = err.to_ascii_lowercase();
    if lower.contains("index_not_found") || lower.contains("no such index") {
        msg.push_str(
            "\n(is the prefix right? `xerj autoindex` writes 'ax' by default — \
             pass --prefix if you indexed under another.)",
        );
    }
    msg
}

/// Default `--full` width, and the width suggested when the user asked for 0.
const DEFAULT_FULL_CHARS: usize = 600;

/// Default `--prefix` for `xerj search`, `xerj def`, and MCP `xerj_search`
/// when the caller names neither an index nor a prefix. This is the prefix
/// `xerj autoindex` writes when `--prefix` is omitted.
pub const DEFAULT_SEARCH_PREFIX: &str = "ax";

/// A `--prefix` the way `xerj autoindex` stored it, plus the index pattern
/// `xerj search` and `xerj def` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPrefix {
    /// Prefix autoindex stored (`my-proj`, not `My_Proj`).
    pub prefix: String,
    /// `{prefix}-*` index pattern.
    pub pattern: String,
}

/// Resolve a prefix the way `xerj autoindex --prefix` stores it.
///
/// Autoindex keeps lower-case `[a-z0-9]` runs joined by `-`
/// ([`crate::dataset::sanitize_prefix`]) and refuses a result that cannot
/// name an index ([`crate::dataset::index_prefix_error`]). Searching the
/// characters the caller typed (`My_Proj-*`) misses the corpus those
/// commands wrote (`my-proj-*`). `xerj search`, `xerj def`, and the
/// `xerj_search` MCP tool all call this, so the three cannot drift.
pub fn resolve_prefix(prefix: &str) -> Result<ResolvedPrefix, String> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        return Err(
            "must contain at least one [a-z0-9] character; for example \"project\" searches project-*"
                .into(),
        );
    }
    let sanitized = crate::dataset::sanitize_prefix(trimmed);
    if sanitized.is_empty() {
        return Err(format!(
            "{trimmed:?} keeps no [a-z0-9] character. xerj autoindex --prefix stores \
             lower-case [a-z0-9] runs joined by '-'; for example \"project\" searches project-*"
        ));
    }
    if let Some(err) = crate::dataset::index_prefix_error(&sanitized) {
        // That sentence is about the walk autoindex has not started. Search
        // and def have nothing to walk.
        return Err(err.trim_end_matches(" Nothing was walked.").to_string());
    }
    Ok(ResolvedPrefix {
        pattern: format!("{sanitized}-*"),
        prefix: sanitized,
    })
}

/// The autoindex prefix a `xerj_search` call can be checked for staleness.
///
/// An explicit `prefix` wins, resolved the same way as `--prefix`. Neither
/// argument is the CLI default `ax`. An `index` of `{prefix}-*` is that
/// prefix. A concrete index name is not: `proj-docs` is a dataset, and
/// guessing the prefix would check the wrong folder. Both arguments is
/// `None` — the tool already rejects that pair.
pub fn prefix_from_scope(prefix: Option<&str>, index: Option<&str>) -> Option<String> {
    let prefix = prefix.map(str::trim).filter(|s| !s.is_empty());
    let index = index.map(str::trim).filter(|s| !s.is_empty());
    match (prefix, index) {
        (Some(_), Some(_)) => None,
        (Some(prefix), None) => resolve_prefix(prefix).ok().map(|r| r.prefix),
        (None, None) => Some(DEFAULT_SEARCH_PREFIX.to_string()),
        (None, Some(index)) => wildcard_prefix(index),
    }
}

/// `{prefix}-*` → `prefix`. Any other pattern is not one autoindex folder.
fn wildcard_prefix(index: &str) -> Option<String> {
    if index.contains([',', ' ', '\t']) {
        return None;
    }
    let stem = index.strip_suffix("-*")?;
    if stem.is_empty() || stem.contains(['*', '?']) {
        return None;
    }
    resolve_prefix(stem).ok().map(|r| r.prefix)
}

/// Stderr note when the stored prefix is not the characters the caller typed.
/// `None` when they already match, including surrounding whitespace.
pub fn prefix_rewrite_note(given: &str, resolved: &ResolvedPrefix) -> Option<String> {
    let given = given.trim();
    if given == resolved.prefix {
        None
    } else {
        Some(format!(
            "--prefix {given:?} is stored as {:?}; searching {}",
            resolved.prefix, resolved.pattern
        ))
    }
}

/// `-k` / `--full`. A value that does not parse used to be consumed and
/// dropped, so `xerj search -k abc` kept the default of 5 and never said so.
pub(crate) fn parse_count_arg(flag: &str, next: Option<String>) -> Result<usize, String> {
    match next {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| format!("{flag} must be a non-negative whole number, got '{v}'")),
        None => Err(format!("{flag} needs a number")),
    }
}

/// What `(--full N to see more)` should offer. `full * 2` when `full` is 0
/// is 0, so the hint told the reader to pass the value that hid the passage.
pub(crate) fn fuller_than(full: usize) -> usize {
    if full == 0 {
        DEFAULT_FULL_CHARS
    } else {
        full.saturating_mul(2)
    }
}

#[derive(Debug)]
struct ParsedSearch {
    url: String,
    api_key: Option<String>,
    prefix: String,
    k: usize,
    full: usize,
    as_json: bool,
    query: Option<String>,
}

#[derive(Debug)]
enum SearchCli {
    Help,
    /// A flag the command refuses (e.g. `--mode`): the one-line reason is
    /// already printed to stderr, and the caller exits 2 without the USAGE
    /// dump the generic `Err` path adds.
    Refuse,
    Query(ParsedSearch),
}

fn parse_search_cli(
    args: impl Iterator<Item = String>,
    mut url: String,
    mut api_key: Option<String>,
) -> Result<SearchCli, String> {
    let mut prefix = DEFAULT_SEARCH_PREFIX.to_string();
    let mut k: usize = 5;
    let mut full: usize = DEFAULT_FULL_CHARS;
    let mut as_json = false;
    let mut query: Option<String> = None;

    let mut it = args;
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(SearchCli::Help),
            "--url" => url = it.next().unwrap_or_default(),
            "--prefix" => prefix = it.next().unwrap_or(prefix),
            "--api-key" => api_key = it.next(),
            "-k" => k = parse_count_arg("-k", it.next())?,
            "--full" => full = parse_count_arg("--full", it.next())?,
            "--json" => as_json = true,
            // Read by feedback::enabled from the raw argv. Accepted so
            // `xerj search --disable-feedback` is not an unexpected argument.
            "--disable-feedback" => {}
            "--mode" => {
                let asked = it.next().unwrap_or_default();
                eprintln!("{}", search_mode_refusal(&asked));
                return Ok(SearchCli::Refuse);
            }
            other if !other.starts_with('-') && query.is_none() => query = Some(other.to_string()),
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    Ok(SearchCli::Query(ParsedSearch {
        url,
        api_key,
        prefix,
        k,
        full,
        as_json,
        query,
    }))
}

/// Entry point for the `search` subcommand. Returns a process exit code.
pub fn run_search_cli() -> i32 {
    let url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());

    // Arguments after the subcommand, including a flag that was written before it.
    let parsed = match parse_search_cli(
        xerj_common::argv::args_after_subcommand().into_iter(),
        url,
        api_key,
    ) {
        Ok(SearchCli::Help) => {
            print!("{USAGE}");
            return 0;
        }
        // The refusal line is already on stderr; exit 2 without a USAGE dump.
        Ok(SearchCli::Refuse) => return 2,
        Ok(SearchCli::Query(parsed)) => parsed,
        Err(e) => {
            eprintln!("xerj search: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let ParsedSearch {
        mut url,
        mut api_key,
        prefix,
        k,
        full,
        as_json,
        query,
    } = parsed;

    let query = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            eprintln!("xerj search: a query is required.\n\n{USAGE}");
            return 2;
        }
    };
    let resolved = match resolve_prefix(&prefix) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("xerj search: {e}\n\n{USAGE}");
            return 2;
        }
    };
    if let Some(note) = prefix_rewrite_note(&prefix, &resolved) {
        eprintln!("xerj search: {note}");
    }
    let prefix = resolved.prefix;
    let pattern = resolved.pattern;
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

    let es = match Es::new(&url, api_key.clone()) {
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
    let raw_mapping = es.get_json(&format!("/{pattern}/_mapping")).ok();
    let mapped = raw_mapping.as_ref().and_then(mapped_property_names);
    // Types are only trusted when the name union was readable. An error body
    // has no properties, and treating that as "nothing is numeric" while the
    // names fall back to the historical query is the #1601 contract.
    let numeric = mapped
        .as_ref()
        .and(raw_mapping.as_ref().map(numeric_property_names));
    // A readable mapping with none of `name` / `code` / `defs` is prose.
    // An unreadable mapping keeps the code-shaped query, and the same
    // test/example penalty (#2122).
    let code_corpus = !xerj_common::code_query::code_symbol_fields_absent(mapped.as_ref());
    let mut body = search_query_body_for_mapping(&query, k, mapped.as_ref(), numeric.as_ref());
    if code_corpus && k > 0 {
        body["size"] = json!(search_fetch_size(k));
    }
    let mut resp = match es.search(&pattern, &body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", query_failure_message("search", &url, &pattern, e));
            return 1;
        }
    };
    // Test/example penalty, then the same-path line preference and the
    // catalog partition (#1504). `--json` sees the same order as the printer.
    // The penalty trims the wider fetch back to `-k`.
    if let Some(hits) = resp.pointer_mut("/hits/hits").and_then(Value::as_array_mut) {
        rank_plain_search_hits(hits, code_corpus, k, &query);
    }
    // Same annotation `xerj_search` adds (#2266), so `--json` and the printed
    // header cite the matched line. `xerj code --json` is the raw server
    // body and does not share this hit post-pass.
    xerj_common::xccode::passage::annotate_search_match_lines(&mut resp, &query);
    // After the query, so a failed search does not pay for the catalog read.
    // A missing catalog, a remote folder, or a check that hits its cap leaves
    // the response unchanged (#2345).
    let hint = staleness::check(&url, api_key.as_deref(), &prefix);
    if as_json {
        if let Some(hint) = &hint {
            staleness::attach(&mut resp, hint);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );
        return 0;
    }
    if let Some(note) = lexical_semantic_note(&resp) {
        eprintln!("{note}");
    }
    if let Some(hint) = &hint {
        println!("{}", hint.cli_line());
    }

    let hits = resp
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if hits.is_empty() {
        let snap = corpus_snapshot(&es, &prefix);
        let (lines, exit) =
            empty_lookup_report("search", &prefix, &query, suggests_def(&query), &snap);
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
        let archived = if xerj_common::xccode::render::source_looks_archived(&src) {
            ", archived"
        } else {
            ""
        };
        println!("\n─── {loc}{tag}  (score {score:.2}{archived})");
        // Passage-first: `_passage` is the engine's matching snippet (an
        // array-wrapped object with a text payload); `code` is the symbol
        // declaration. Never the whole file body — that is the 9x token flood
        // this command exists to avoid. A hit with neither prints its heading
        // or title rather than a blank block.
        let passage_owned = hit_display_text(h);
        if let Some(text) = passage_owned.as_deref() {
            let snippet: String = text.chars().take(full).collect();
            for l in snippet.lines() {
                println!("    {l}");
            }
            if text.chars().count() > full {
                println!("    … (--full {} to see more)", fuller_than(full));
            }
        }
    }
    println!(
        "\n{} passage(s) from '{pattern}'. Cite ax_path:line (ax_path#locator for a data row) for anything you rely on.",
        hits.len()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_common::code_query::normalize_code_search_query;

    /// `My_Proj` is stored as `my-proj-*`. Searching the typed characters
    /// misses the corpus. A trailing `-*` is the pattern, not part of the
    /// prefix. A prefix that cannot name an index is refused before HTTP.
    #[test]
    fn resolve_prefix_matches_what_autoindex_stores() {
        let project = resolve_prefix("project").unwrap();
        assert_eq!(project.prefix, "project");
        assert_eq!(project.pattern, "project-*");
        assert!(prefix_rewrite_note("project", &project).is_none());
        assert!(prefix_rewrite_note("  project  ", &project).is_none());

        let rewritten = resolve_prefix("  My_Proj  ").unwrap();
        assert_eq!(rewritten.prefix, "my-proj");
        assert_eq!(rewritten.pattern, "my-proj-*");
        assert_eq!(
            crate::dataset::sanitize_prefix("My_Proj"),
            rewritten.prefix,
            "resolution must be sanitize_prefix, not a second copy of the rules"
        );
        let note = prefix_rewrite_note("My_Proj", &rewritten).unwrap();
        assert!(note.contains("my-proj-*"), "{note}");

        assert_eq!(
            resolve_prefix("owner--repo").unwrap().pattern,
            "owner--repo-*"
        );
        assert_eq!(resolve_prefix("project-*").unwrap().pattern, "project-*");
        assert_eq!(
            resolve_prefix(DEFAULT_SEARCH_PREFIX).unwrap().pattern,
            "ax-*"
        );

        assert_eq!(prefix_from_scope(None, None).as_deref(), Some("ax"));
        assert_eq!(
            prefix_from_scope(Some("My_Proj"), None).as_deref(),
            Some("my-proj")
        );
        assert_eq!(
            prefix_from_scope(None, Some("ok-knowledge-*")).as_deref(),
            Some("ok-knowledge")
        );
        assert_eq!(prefix_from_scope(Some("proj"), Some("proj-*")), None);
        assert_eq!(prefix_from_scope(None, Some("proj-docs")), None);
        assert_eq!(prefix_from_scope(None, Some("a,b-*")), None);
        assert_eq!(prefix_from_scope(Some("___"), None), None);

        let empty = resolve_prefix("___").unwrap_err();
        assert!(empty.contains("[a-z0-9]"), "{empty}");
        assert!(resolve_prefix("   ").is_err());
        let digit = resolve_prefix("2042").unwrap_err();
        assert!(digit.contains("2042"), "{digit}");
        assert!(
            !digit.contains("walked"),
            "search must not talk about a walk it does not do: {digit}"
        );
        assert!(digit.contains("lowercase letter"), "{digit}");
    }

    #[test]
    fn help_says_the_default_ranking_is_lexical() {
        assert!(
            USAGE.contains("Default ranking is lexical") && USAGE.contains("not a neural reading"),
            "{USAGE}"
        );
    }

    /// HTTP 400 `For input string` is a numeric field, not a wrong prefix.
    /// A missing index still gets the prefix sentence.
    #[test]
    fn query_failure_blames_the_prefix_only_when_the_index_is_missing() {
        let numeric = query_failure_message(
            "search",
            "http://127.0.0.1:9200",
            "ax-*",
            "search /ax-* HTTP 400: {\"error\":{\"reason\":\"For input string: \\\"fingerprint dedupe comment\\\"\"}}",
        );
        assert!(numeric.contains("For input string"), "{numeric}");
        assert!(
            !numeric.contains("is the prefix right"),
            "a parse failure must not blame --prefix, got: {numeric}"
        );
        assert!(
            numeric.starts_with("xerj search: query failed against http://127.0.0.1:9200/ax-*: ")
        );

        let missing = query_failure_message(
            "def",
            "http://127.0.0.1:9200",
            "nope-*",
            "search /nope-* HTTP 404: {\"error\":{\"type\":\"index_not_found_exception\"}}",
        );
        assert!(missing.contains("index_not_found_exception"), "{missing}");
        assert!(missing.contains("is the prefix right"), "{missing}");
        assert!(missing.contains("xerj def:"), "{missing}");

        let phrase = query_failure_message(
            "search",
            "http://localhost:9200",
            "ax-*",
            "no such index: ax-missing",
        );
        assert!(phrase.contains("is the prefix right"), "{phrase}");

        let other = query_failure_message(
            "search",
            "http://localhost:9200",
            "ax-*",
            "search /ax-* HTTP 403: {\"error\":{\"type\":\"security_exception\"}}",
        );
        assert!(!other.contains("is the prefix right"), "{other}");
    }

    #[test]
    fn search_refuses_a_mode_flag_and_names_the_embedder() {
        for asked in ["semantic", "hybrid", "bm25"] {
            let line = search_mode_refusal(asked);
            assert!(line.contains(&format!("({asked})")), "{line}");
            assert!(line.contains("always sends a lexical query"), "{line}");
            assert!(line.contains("xerj code --mode semantic"), "{line}");
            assert!(line.contains("--embed-mode neural"), "{line}");
            assert!(line.contains("lexical feature hashing"), "{line}");
            assert!(!line.contains("POST"), "{line}");
        }
        let bare = search_mode_refusal("");
        assert!(bare.contains("no --mode."));
        assert!(!bare.contains("()"), "{bare}");
    }

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn disable_feedback_is_not_an_unexpected_argument() {
        for args in [
            vec!["--disable-feedback", "needle"],
            vec!["needle", "--disable-feedback"],
            vec!["--disable-feedback", "--json", "needle"],
        ] {
            let parsed = parse_search_cli(words(&args).into_iter(), String::new(), None)
                .unwrap_or_else(|e| panic!("{args:?}: {e}"));
            match parsed {
                SearchCli::Query(q) => assert_eq!(q.query.as_deref(), Some("needle")),
                SearchCli::Help => panic!("help from {args:?}"),
                SearchCli::Refuse => panic!("refusal from {args:?}"),
            }
        }
    }

    #[test]
    fn a_non_numeric_k_is_a_usage_error() {
        let err = parse_search_cli(
            words(&["-k", "abc", "flush"]).into_iter(),
            String::new(),
            None,
        )
        .expect_err("-k abc must not be ignored");
        assert!(err.contains("-k"), "{err}");
        assert!(err.contains("abc"), "{err}");
        assert!(parse_count_arg("--full", Some("nope".into()))
            .unwrap_err()
            .contains("nope"));
        assert!(parse_count_arg("-k", None)
            .unwrap_err()
            .contains("needs a number"));
    }

    #[test]
    fn a_zero_full_hint_offers_the_default_width() {
        assert_eq!(fuller_than(0), DEFAULT_FULL_CHARS);
        assert_ne!(fuller_than(0), 0);
        assert_eq!(fuller_than(600), 1200);
        let parsed = parse_search_cli(
            words(&["--full", "0", "rotate keys"]).into_iter(),
            String::new(),
            None,
        )
        .unwrap();
        let SearchCli::Query(parsed) = parsed else {
            panic!("expected a query");
        };
        assert_eq!(parsed.full, 0);
        assert_eq!(parsed.query.as_deref(), Some("rotate keys"));
    }

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
        assert_eq!(
            hit_display_text(&hit).as_deref(),
            Some("de <- function(col)")
        );
    }

    /// A notebook hit can match on BM25 and still come back with no `_passage`.
    /// The human line then has a path and a score and nothing under them.
    /// Heading wins over title; a real passage still wins over both.
    #[test]
    fn a_hit_with_no_passage_prints_its_heading_or_title() {
        let headed = json!({
            "_source": {
                "ax_path": "10-Iterators.ipynb",
                "title": "Iterators",
                "heading": "map and filter"
            }
        });
        assert_eq!(hit_display_text(&headed).as_deref(), Some("map and filter"));

        let titled = json!({
            "_source": { "ax_path": "Index.ipynb", "title": "Index" }
        });
        assert_eq!(hit_display_text(&titled).as_deref(), Some("Index"));

        let with_passage = json!({
            "_source": {
                "ax_path": "a.ipynb",
                "title": "should not win",
                "heading": "nope"
            },
            "fields": { "_passage": [{ "field": "body", "text": "the matching cell" }] }
        });
        assert_eq!(
            hit_display_text(&with_passage).as_deref(),
            Some("the matching cell")
        );

        let blank = json!({ "_source": { "ax_path": "empty.ipynb" } });
        assert_eq!(hit_display_text(&blank), None);

        let source = search_query_body("map and filter", 5);
        assert!(
            source["_source"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "heading"),
            "{source}"
        );
    }

    #[test]
    fn a_lexical_on_semantic_text_hint_is_one_line_and_not_the_post_recipe() {
        let flagged = json!({
            "_xerj": { "hints": [{
                "code": "lexical_on_semantic_text",
                "reason": "body is semantic_text, but this query scored it with BM25. POST a semantic query.",
                "try": { "request": "POST /smoke-nb-docs/_search" }
            }] }
        });
        let note = lexical_semantic_note(&flagged).unwrap();
        assert!(note.contains("lexical"));
        assert!(note.contains("does not consult the embedding"));
        assert!(!note.contains("POST"), "{note}");
        assert!(!note.contains('\n'), "{note}");

        let other = json!({
            "_xerj": { "hints": [{ "code": "unknown_field", "reason": "no such field" }] }
        });
        assert!(lexical_semantic_note(&other).is_none());
        assert!(lexical_semantic_note(&json!({"hits": []})).is_none());
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

        let windows_hit = json!({
            "_source": { "ax_path": "C:/src/main.rs", "line": 12 }
        });
        assert_eq!(
            hit_location(&windows_hit),
            "C:/src/main.rs (line 12)",
            "a drive letter must not be parsed as the line"
        );

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

    /// #1941: a prose section record carries `start_line` (the file line the
    /// section starts on), so the header cites that line instead of the
    /// record-relative `ordinal + 1` — the `guide.md:1` this issue reported
    /// for a passage at line 245.
    #[test]
    fn a_prose_section_is_cited_at_its_file_start_line() {
        let hit = json!({
            "_source": {
                "ax_path": "guide.md",
                "ax_locator": "s2",
                "ax_format": "txt-prose",
                "section": 2,
                "start_line": 169
            },
            "fields": { "_passage": [{ "field": "body", "ordinal": 0 }] }
        });
        assert_eq!(hit_location(&hit), "guide.md:169");
    }

    /// #2266: once the response carries `match_line`, the header cites that
    /// line. `start_line` stays the passage start for hits that have no match.
    #[test]
    fn hit_location_cites_match_line_ahead_of_the_passage_start() {
        let mut hit = json!({
            "_source": {
                "ax_path": "guide.md",
                "ax_locator": "s2",
                "start_line": 169
            },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 0,
                "text": "Intro.\nThe token is required when unbound."
            }] }
        });
        assert_eq!(hit_location(&hit), "guide.md:169");
        let mut resp = json!({ "hits": { "hits": [hit] } });
        xerj_common::xccode::passage::annotate_search_match_lines(&mut resp, "unbound");
        hit = resp["hits"]["hits"][0].clone();
        assert_eq!(hit["match_line"], 170);
        assert_eq!(hit_location(&hit), "guide.md:170");
    }

    /// #1934: two JSONL rows both printed as `xerj-prs.jsonl:1` because the
    /// passage ordinal counts lines inside the row's own `body`. A data row
    /// is cited by its locator; whole files, code and sections keep lines.
    #[test]
    fn a_data_row_is_cited_by_its_locator_not_a_passage_ordinal() {
        let row = |loc: &str, ordinal: u64| {
            json!({
                "_source": { "ax_path": "xerj-prs.jsonl", "ax_locator": loc },
                "fields": { "_passage": [{ "field": "body", "ordinal": ordinal }] }
            })
        };
        assert_eq!(hit_location(&row("b0", 0)), "xerj-prs.jsonl#b0");
        assert_eq!(hit_location(&row("b68801", 0)), "xerj-prs.jsonl#b68801");
        assert_ne!(
            hit_location(&row("b9229", 0)),
            hit_location(&row("b68801", 0)),
            "two rows never share a citation"
        );
        for (loc, want) in [
            ("r17", "xerj-prs.jsonl#r17"),
            ("row3", "xerj-prs.jsonl#row3"),
            ("Sheet1!r5", "xerj-prs.jsonl#Sheet1!r5"),
            ("items:e12", "xerj-prs.jsonl#items:e12"),
            ("d0e4", "xerj-prs.jsonl#d0e4"),
            ("d2", "xerj-prs.jsonl#d2"),
        ] {
            assert_eq!(hit_location(&row(loc, 3)), want);
        }
        // Lines still win where they are lines.
        assert_eq!(hit_location(&row("file", 4)), "xerj-prs.jsonl:5");
        assert_eq!(hit_location(&row("code", 4)), "xerj-prs.jsonl:5");
        assert_eq!(hit_location(&row("s2", 4)), "xerj-prs.jsonl:5");
        assert_eq!(hit_location(&row("bogus", 4)), "xerj-prs.jsonl:5");
        let lined = json!({
            "_source": { "ax_path": "a.jsonl", "ax_locator": "b40", "line": 3 }
        });
        assert_eq!(hit_location(&lined), "a.jsonl:3");
        // No locator (an older index, or `xerj def`): unchanged.
        let bare = json!({
            "_source": { "ax_path": "a.rs" },
            "fields": { "_passage": [{ "field": "body", "ordinal": 9 }] }
        });
        assert_eq!(hit_location(&bare), "a.rs:10");
    }

    /// A PDF passage with ordinal 0 is page 4, not line 1. A notebook section
    /// heading is cell 19, not line 1. A symbol line still wins over a page.
    #[test]
    fn pdf_page_and_notebook_cell_are_not_cited_as_line_one() {
        let pdf = json!({
            "_source": { "ax_path": "nonfree/naacl06-shinyama.pdf" },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 0,
                "page": 4,
                "text": "extractable text"
            }] }
        });
        let loc = hit_location(&pdf);
        assert_eq!(loc, "nonfree/naacl06-shinyama.pdf page 4");
        assert!(!loc.ends_with(":1"), "{loc}");

        let from_source = json!({
            "_source": { "ax_path": "samples/rc4-40.pdf", "page": 2 }
        });
        assert_eq!(hit_location(&from_source), "samples/rc4-40.pdf page 2");

        let notebook = json!({
            "_source": {
                "ax_path": "11-List-Comprehensions.ipynb",
                "cell": 19,
                "title": "Conditionals on the Value"
            },
            "fields": { "_passage": [{
                "field": "body",
                "ordinal": 0,
                "text": "a list comprehension filters"
            }] }
        });
        assert_eq!(
            hit_location(&notebook),
            "11-List-Comprehensions.ipynb cell 19"
        );

        let symbol_on_a_page = json!({
            "_source": { "ax_path": "notes.pdf", "line": 12, "page": 3 },
            "fields": { "_passage": [{ "field": "code", "ordinal": 0, "page": 3 }] }
        });
        assert_eq!(hit_location(&symbol_on_a_page), "notes.pdf:12");

        let source = search_query_body("list comprehension", 5);
        let fields = source["_source"].as_array().unwrap();
        assert!(fields.iter().any(|f| f == "page"), "{source}");
        assert!(fields.iter().any(|f| f == "cell"), "{source}");
    }

    /// A defs catalog of `src/lib.rs` outscored `mod ensure;` at `src/lib.rs:255`
    /// on an anyhow index. The line has to lead that path. A later file stays
    /// after both, and a bare hit with no lined sibling is left alone.
    #[test]
    fn a_bare_defs_hit_does_not_outrank_a_lined_hit_on_the_same_path() {
        let mut hits = vec![
            json!({
                "_score": 23.42,
                "_source": { "ax_path": "src/lib.rs" },
                "fields": { "_passage": [{
                    "field": "defs",
                    "ordinal": 0,
                    "text": "module ensure"
                }] }
            }),
            json!({
                "_score": 15.53,
                "_source": { "ax_path": "src/lib.rs", "line": 255, "kind": "module" },
                "fields": { "_passage": [{
                    "field": "code",
                    "ordinal": 0,
                    "text": "mod ensure;"
                }] }
            }),
            json!({
                "_score": 12.09,
                "_source": { "ax_path": "tests/test_chain.rs" },
                "fields": { "_passage": [{
                    "field": "body",
                    "ordinal": 0,
                    "text": "ensure the chain"
                }] }
            }),
        ];
        prefer_citable_line_on_the_same_path(&mut hits);
        assert_eq!(hit_location(&hits[0]), "src/lib.rs:255");
        assert_eq!(hit_location(&hits[1]), "src/lib.rs");
        assert_eq!(hit_location(&hits[2]), "tests/test_chain.rs:1");

        let mut only_bare = vec![
            json!({
                "_source": { "ax_path": "only.rs" },
                "fields": { "_passage": [{ "field": "defs", "ordinal": 0 }] }
            }),
            json!({ "_source": { "ax_path": "other.rs", "line": 4 } }),
        ];
        prefer_citable_line_on_the_same_path(&mut only_bare);
        assert_eq!(hit_location(&only_bare[0]), "only.rs");
        assert_eq!(hit_location(&only_bare[1]), "other.rs:4");

        let mut sandwiched = vec![
            json!({ "_source": { "ax_path": "src/lib.rs", "line": 10 } }),
            json!({
                "_source": { "ax_path": "src/lib.rs" },
                "fields": { "_passage": [{ "field": "defs", "ordinal": 0 }] }
            }),
            json!({ "_source": { "ax_path": "src/lib.rs", "line": 20 } }),
            json!({ "_source": { "ax_path": "other.rs", "line": 1 } }),
        ];
        prefer_citable_line_on_the_same_path(&mut sandwiched);
        assert_eq!(hit_location(&sandwiched[0]), "src/lib.rs:10");
        assert_eq!(hit_location(&sandwiched[1]), "src/lib.rs:20");
        assert_eq!(hit_location(&sandwiched[2]), "src/lib.rs");
        assert_eq!(hit_location(&sandwiched[3]), "other.rs:1");
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
        assert!(should[0].get("match_phrase").is_none(), "{body}");
        assert!(should.iter().all(|c| c.get("term").is_none()), "{body}");
        assert_eq!(
            should[0]["multi_match"]["fields"],
            json!(["body", "title", "text", "impl"])
        );
        // The exact-phrase legs name only mapped content fields; `title` is
        // a keyword and gets none.
        assert_eq!(phrase_fields(&body), vec!["body", "text", "impl"], "{body}");
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
        // Recall multi_match, the exact-phrase leg, then the two coverage
        // clauses a three-token question adds (#2327).
        assert_eq!(should.len(), 4, "{body}");
        assert_eq!(should[0]["multi_match"]["fields"], json!(["message"]));
        assert!(should[2].get("constant_score").is_some(), "{body}");
        assert!(should[3].get("constant_score").is_some(), "{body}");
        assert_eq!(phrase_fields(&body), vec!["message"], "{body}");
        let rendered = body["query"].to_string();
        assert!(!rendered.contains("\"code\""), "{rendered}");
        assert!(!rendered.contains("\"name\""), "{rendered}");
        assert!(!rendered.contains("\"defs\""), "{rendered}");
    }

    /// Fields named by the exact-phrase legs, in order. The `defs` phrase
    /// leg is the symbol clause, not one of these.
    fn phrase_fields(body: &Value) -> Vec<String> {
        body["query"]["bool"]["should"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.get("match_phrase").and_then(Value::as_object))
            .flat_map(|m| m.keys().cloned())
            .filter(|f| f != "defs")
            .collect()
    }

    /// A pasted error message is matched as typed, glue words included:
    /// the normalized `multi_match` text drops `the`/`must`, and a phrase
    /// built from that would not occur in the file.
    #[test]
    fn exact_phrase_leg_matches_the_query_as_typed() {
        let query = "  the boosted whole-value doc must rank first ";
        let body = search_query_body(query, 5);
        let should = body["query"]["bool"]["should"].as_array().unwrap();
        let leg = should
            .iter()
            .find_map(|c| c.pointer("/match_phrase/body"))
            .unwrap_or_else(|| panic!("no exact-phrase leg on body: {body}"));
        assert_eq!(leg["query"], "the boosted whole-value doc must rank first");
        assert_eq!(
            leg["boost"],
            json!(xerj_common::code_query::EXACT_PHRASE_BOOST)
        );
        assert_ne!(
            should[0]["multi_match"]["query"], leg["query"],
            "the multi_match keeps the normalized text"
        );
    }

    /// One token is a lookup: no phrase leg, and the #916 clauses stay at
    /// their historical positions for every query.
    #[test]
    fn single_token_query_gets_no_exact_phrase_leg() {
        for query in ["normalize_code_search_query", "IOSIntfLine", "  flush  "] {
            let body = search_query_body(query, 5);
            assert!(phrase_fields(&body).is_empty(), "{body}");
        }
        let body = search_query_body("rotate the WAL segment", 5);
        let should = body["query"]["bool"]["should"].as_array().unwrap();
        assert!(should[0].get("multi_match").is_some(), "{body}");
        assert!(should[1].pointer("/match_phrase/defs").is_some(), "{body}");
        assert!(should[2].pointer("/term/name").is_some(), "{body}");
        assert_eq!(phrase_fields(&body), vec!["body"], "{body}");
        // Three content tokens (glue word "the" is dropped) grow the coverage
        // clauses. A one-token lookup above does not.
        assert!(
            should.iter().any(|c| c.get("constant_score").is_some()),
            "{body}"
        );
        assert!(
            !search_query_body("Token", 5)
                .to_string()
                .contains("constant_score"),
            "an exact symbol lookup must not grow a coverage clause"
        );
    }

    /// A content field some index maps as numeric is dropped from the
    /// phrase legs too (#1284): naming it fails the whole wildcard request.
    #[test]
    fn exact_phrase_leg_skips_a_numeric_content_field() {
        let mapped = names_of(&["name", "code", "body", "defs", "title", "message"]);
        let numeric: BTreeSet<String> = ["message".to_string()].into_iter().collect();
        let body =
            search_query_body_for_mapping("SIP 403 Forbidden", 3, Some(&mapped), Some(&numeric));
        assert_eq!(phrase_fields(&body), vec!["body"], "{body}");
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
        let code_only = names_of(&["name", "code", "body", "defs", "title"]);
        assert_eq!(
            search_query_body_for_mapping(query, 5, Some(&code_only), Some(&BTreeSet::new())),
            historical,
            "a code corpus with no prose field keeps the #916 field list"
        );
        // A folded docs index maps `text` (changelogs, line-oriented files)
        // beside the code fields. Search it. `impl` stays off so the code
        // legs do not grow a second body.
        let mixed = names_of(&[
            "name", "code", "body", "defs", "title", "text", "impl", "message",
        ]);
        let body = search_query_body_for_mapping(query, 5, Some(&mixed), Some(&BTreeSet::new()));
        let fields = body["query"]["bool"]["should"][0]["multi_match"]["fields"]
            .as_array()
            .unwrap();
        let bare: Vec<&str> = fields
            .iter()
            .map(|f| f.as_str().unwrap().split('^').next().unwrap())
            .collect();
        assert!(bare.contains(&"text"), "{bare:?}");
        assert!(bare.contains(&"message"), "{bare:?}");
        assert!(!bare.contains(&"impl"), "{bare:?}");
        assert!(bare.contains(&"body") && bare.contains(&"defs"), "{bare:?}");
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
        let headline = lines.first().expect("headline");
        assert!(
            headline.starts_with("no indices named typo-* — this is not a search result"),
            "{headline}"
        );
        assert!(
            headline.contains("prefixes on this node: eng, sled"),
            "the refusal line itself must name the prefixes: {headline}"
        );
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
        assert!(suggests_def("Tree::flush"));
        assert!(suggests_def("Db"));
        assert!(!suggests_def("flush the tree"));
        assert!(!suggests_def("src/lib.rs"));
        assert!(
            !is_identifier("Tree::flush"),
            "def's query shape stays the narrower predicate"
        );
        let (qualified, _) = empty_lookup_report(
            "search",
            "eng",
            "Tree::flush",
            suggests_def("Tree::flush"),
            &snap(
                Some(vec!["eng-docs"]),
                Some(vec!["eng-docs"]),
                Some(vec!["eng"]),
            ),
        );
        let qualified = qualified.join("\n");
        assert!(
            qualified.contains("no match in eng-* for: Tree::flush"),
            "{qualified}"
        );
        assert!(
            qualified.contains("xerj def Tree::flush"),
            "a qualified name with zero hits must name def: {qualified}"
        );
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
        let headline = lines.first().expect("headline");
        assert!(
            headline.contains("indices on this node: sled-docs, eng-logs"),
            "index names stand in for prefixes on the refusal line: {headline}"
        );
        assert!(
            text.contains("indices on this node: sled-docs, eng-logs"),
            "{text}"
        );
        assert!(text.contains("catalog lists no prefixes"), "{text}");
        assert!(!text.contains("xerj autoindex"), "{text}");
    }

    #[test]
    fn catalog_prefix_values_keep_dataset_documents_and_drop_an_empty_prefix() {
        let body = json!({
            "hits": {"hits": [
                {"_source": {"doc_kind": "dataset", "prefix": "repo"}},
                {"_source": {"doc_kind": "run", "prefix": "repo"}},
                {"_source": {"doc_kind": "dataset", "prefix": "other"}},
                {"_source": {"prefix": ""}}
            ]}
        });
        assert_eq!(
            prefixes_from_catalog_body(&body),
            vec!["other".to_string(), "repo".to_string()]
        );
        assert!(prefixes_from_catalog_body(&json!({"hits": {}})).is_empty());
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

    fn hit(path_key: &str, path: &str, score: f64) -> Value {
        json!({
            "_score": score,
            "_source": { path_key: path }
        })
    }

    /// #2122: a test/example path that only barely leads is ranked behind the
    /// implementation, and it is still in the page. A test that scores more
    /// than twice as high stays ahead of the implementation. A `*.test.ts`
    /// file is not a demoted path — the predicate is `path_is_demoted`, not
    /// a second rule list.
    #[test]
    fn code_search_penalizes_test_paths_and_keeps_them() {
        let mut hits = vec![
            hit("ax_path", "src/tests/render.test.tsx", 12.0),
            hit("ax_path", "examples/App.tsx", 11.0),
            hit("ax_path", "src/render.tsx", 9.0),
            hit("path", "tests/render_contract.rs", 30.0),
            hit("ax_path", "src/Foo.test.tsx", 14.0),
            hit("ax_path", "src/fixtures/transcript.json", 8.0),
        ];
        rank_plain_search_hits(&mut hits, true, 5, "");
        let paths: Vec<&str> = hits
            .iter()
            .map(|h| {
                h["_source"]
                    .get("ax_path")
                    .or_else(|| h["_source"].get("path"))
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect();
        // 30→15 and 14 stay ahead of the implementation at 9. 12→6 drops behind
        // it but stays on the page. 11→5.5 falls off the page of 5; it is
        // not deleted when the page has room (asserted below).
        assert_eq!(
            paths,
            vec![
                "tests/render_contract.rs",
                "src/Foo.test.tsx",
                "src/render.tsx",
                "src/fixtures/transcript.json",
                "src/tests/render.test.tsx",
            ],
            "{paths:?}"
        );
        let mut wide = vec![
            hit("ax_path", "examples/App.tsx", 11.0),
            hit("ax_path", "src/render.tsx", 9.0),
        ];
        rank_plain_search_hits(&mut wide, true, 5, "");
        assert_eq!(wide.len(), 2, "a demoted hit stays when it still fits");
        assert_eq!(wide[0]["_source"]["ax_path"], "src/render.tsx");
        assert_eq!(wide[1]["_source"]["ax_path"], "examples/App.tsx");
        assert_eq!(wide[1]["_score"], json!(5.5));
        assert_eq!(wide[0]["_score"], json!(9.0));

        let mut only_tests = vec![
            hit("ax_path", "tests/a.rs", 10.0),
            hit("ax_path", "fuzz/targets/b.rs", 8.0),
        ];
        rank_plain_search_hits(&mut only_tests, true, 5, "");
        assert_eq!(only_tests[0]["_source"]["ax_path"], "tests/a.rs");
        assert_eq!(only_tests[1]["_source"]["ax_path"], "fuzz/targets/b.rs");
        assert_eq!(only_tests[0]["_score"], json!(5.0));

        let mut prose = vec![
            hit("ax_path", "tests/a.rs", 12.0),
            hit("ax_path", "src/render.tsx", 9.0),
        ];
        rank_plain_search_hits(&mut prose, false, 1, "");
        assert_eq!(prose.len(), 2, "a prose page is not trimmed");
        assert_eq!(prose[0]["_source"]["ax_path"], "tests/a.rs");
        assert_eq!(prose[0]["_score"], json!(12.0));
    }

    /// The fetch is wider than `-k` on purpose, and the query body does not
    /// grow a `function_score` / `ax_path` wildcard to do the demotion.
    #[test]
    fn code_search_window_is_local_and_wider_than_the_page() {
        assert_eq!(search_fetch_size(5), 45);
        assert!(search_fetch_size(5) > 5);
        let body = search_query_body("render the transcript", 5);
        let rendered = body["query"].to_string();
        assert!(!rendered.contains("function_score"), "{rendered}");
        assert!(!rendered.contains("ax_path"), "{rendered}");
        assert_eq!(body["size"], 5, "the builder stays at k; the CLI widens");
    }

    /// Catalog stubs still sink, on a code page and a prose page.
    #[test]
    fn path_penalty_does_not_undo_catalog_demotion() {
        let mut hits = vec![
            json!({"_score": 30.0, "_source": {"ax_path": "src/model.pyi"}}),
            hit("ax_path", "tests/test_model.py", 12.0),
            hit("ax_path", "src/model.py", 9.0),
        ];
        rank_plain_search_hits(&mut hits, true, 5, "");
        let paths: Vec<&str> = hits
            .iter()
            .map(|h| h["_source"]["ax_path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            vec!["src/model.py", "tests/test_model.py", "src/model.pyi"]
        );
    }
}
