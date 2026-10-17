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
    weighted_query_fields,
};
use serde_json::{json, Value};

const USAGE: &str = "\
xerj def — find where a symbol is defined (go-to-definition, no scripts)

USAGE:
    xerj def [OPTIONS] \"<symbol or capability>\"

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --prefix <P>    index prefix to search (default \"ax\" — what `xerj autoindex` writes)
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
    let mut fields = weighted_query_fields(DEF_MM_FIELDS, DEF_PROSE_EXTRA_FIELDS, mapped, numeric);
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
    let bool_query = json!({
        "bool": {
            "should": should,
            "filter": filter,
            "minimum_should_match": 1
        }
    });
    // Kind and `pub` weights sit on function_score, not on a bare `should`:
    // a should clause would match every struct in the index. The negative
    // boosting clause only multiplies hits that already matched.
    let query = rank_adjusted_query(bool_query, mapped, numeric);
    json!({
        "size": k,
        "query": query,
        "_source": ["ax_path", "path", "line", "start_line", "kind", "language", "name", "code"],
        "fields": ["_passage"]
    })
}

fn rank_adjusted_query(
    bool_query: Value,
    mapped: Option<&BTreeSet<String>>,
    numeric: Option<&BTreeSet<String>>,
) -> Value {
    // `function_score` (boost_mode sum) adds weight only to hits the bool
    // already matched. A `boosting` wrapper was tried first; this engine's
    // planner rewrites it to a bool and every hit came back with `_score`
    // 1.0, so the weights never moved the order.
    let mut functions = Vec::new();
    // A numeric `kind` or `code` cannot carry the ranking filter: the
    // wildcard would fail on that index the same way a `term` on a numeric
    // `name` does. Unreadable mappings (numeric = None) keep both weights.
    if field_lookup_ok(mapped, numeric, "kind") {
        let mut kind_filter = json!({
            "bool": { "must": [ { "terms": { "kind": PREFERRED_KINDS } } ] }
        });
        // A preferred kind under tests/examples/fuzz/benches does not get
        // the weight. `ax_path` is only named when the read mapping has it,
        // so the historical query (mapped = None) stays free of that field.
        if explicitly_mapped(mapped, "ax_path") {
            let globs: Vec<Value> = DEMOTED_PATH_GLOBS
                .iter()
                .map(|glob| json!({ "wildcard": { "ax_path": glob } }))
                .collect();
            kind_filter["bool"]["must_not"] = json!([{
                "bool": { "should": globs, "minimum_should_match": 1 }
            }]);
        }
        functions.push(json!({ "filter": kind_filter, "weight": 20.0 }));
    }
    if field_lookup_ok(mapped, numeric, "code") {
        functions.push(json!({
            "filter": { "bool": { "should": [
                { "match": { "code": "pub" } },
                { "match": { "code": "public" } },
                { "match": { "code": "export" } }
            ], "minimum_should_match": 1 } },
            "weight": 5.0
        }));
    }
    if functions.is_empty() {
        return bool_query;
    }
    json!({
        "function_score": {
            "query": bool_query,
            "functions": functions,
            "score_mode": "sum",
            "boost_mode": "sum"
        }
    })
}

/// Entry point for the `def` subcommand. Returns a process exit code.
pub fn run_def_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut prefix = "ax".to_string();
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
            "--lang" => lang = it.next(),
            "--json" => as_json = true,
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

    let pattern = format!("{prefix}-*");
    let raw_mapping = es.get_json(&format!("/{pattern}/_mapping")).ok();
    let mapped = raw_mapping.as_ref().and_then(mapped_property_names);
    let numeric = mapped
        .as_ref()
        .and(raw_mapping.as_ref().map(numeric_property_names));
    if let Some(set) = mapped.as_ref().filter(|s| !s.is_empty()) {
        if kind.is_some() && !set.contains("kind") {
            eprintln!("xerj def: --kind ignored; no index under this prefix maps `kind`");
        }
        if lang.is_some() && !set.contains("language") {
            eprintln!("xerj def: --lang ignored; no index under this prefix maps `language`");
        }
    }
    let body = def_query(
        &query,
        kind.as_deref(),
        lang.as_deref(),
        k,
        mapped.as_ref(),
        numeric.as_ref(),
    );
    let resp = match es.search(&pattern, &body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "xerj def: query failed against {url}/{pattern}: {e}\n\
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
        // signature block; `_passage` covers file-level hits. Two lines is
        // enough to act on — `xerj search --full` exists for reading more.
        let passage_owned = passage_text(h);
        let decl = src
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(passage_owned);
        if let Some(text) = decl {
            for l in text.lines().take(2) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ES `bool` semantics: once a `filter` clause is present, `should`
    /// clauses are optional unless `minimum_should_match` says otherwise. A
    /// `--kind`/`--lang` filter must narrow the definitions of the name, not
    /// replace the name with "anything of this kind".
    #[test]
    fn kind_and_lang_filters_still_require_the_name_to_match() {
        for (kind, lang) in [
            (Some("function"), None),
            (None, Some("python")),
            (Some("class"), Some("rust")),
            (None, None),
        ] {
            let body = def_query("zzqq_nonexistent", kind, lang, 5, None, None);
            // Kind/pub weights wrap the bool; the filter still has to sit on
            // that bool, not replace the name clauses.
            let b = body
                .pointer("/query/function_score/query/bool")
                .or_else(|| body.pointer("/query/bool"))
                .expect("bool query");
            assert_eq!(b["minimum_should_match"], 1, "{body}");
            assert!(!b["should"].as_array().unwrap().is_empty());
            let filters = b["filter"].as_array().unwrap().len();
            assert_eq!(filters, kind.is_some() as usize + lang.is_some() as usize);
        }
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

    /// A struct/function outranks a type alias and a `mod` line of the same
    /// name, and a `pub` declaration outranks a private one. The weight is a
    /// function_score filter, not a `should`, so it cannot match every struct.
    #[test]
    fn preferred_kinds_and_pub_are_weighted_and_aliases_are_demoted() {
        let body = def_query("Db", None, None, 5, None, None);
        let rendered = body["query"].to_string();
        assert!(rendered.contains("function_score"), "{body}");
        assert!(
            rendered.contains("\"weight\":20.0") || rendered.contains("\"weight\":20"),
            "{body}"
        );
        for kind in ["struct", "function", "enum", "trait"] {
            assert!(
                rendered.contains(&format!("\"{kind}\"")),
                "{kind} missing: {body}"
            );
        }
        // A type alias is not a preferred kind, so it does not receive the
        // weight. Path globs are not part of the historical query.
        let kinds = body.pointer("/query/function_score/functions/0/filter/bool/must/0/terms/kind");
        assert!(
            kinds
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .all(|k| k.as_str() != Some("type") && k.as_str() != Some("module")),
            "{kinds:?}"
        );
        assert!(!rendered.contains("ax_path"), "{body}");
        let with_path = names_of(&["name", "defs", "code", "title", "kind", "ax_path"]);
        let body = def_query("Db", None, None, 5, Some(&with_path), None);
        let rendered = body["query"].to_string();
        assert!(rendered.contains("*/tests/*"), "{body}");
        assert!(rendered.contains("*/examples/*"), "{body}");
        assert!(rendered.contains("*/fuzz/*"), "{body}");
        assert!(rendered.contains("*/benches/*"), "{body}");
        assert!(rendered.contains("must_not"), "{body}");
    }

    /// "flush the tree to disk" has to search the symbol body. An identifier
    /// does not grow that leg, and a mapping without `impl` does not either.
    #[test]
    fn a_phrase_searches_impl_only_when_the_mapping_has_it() {
        let code = names_of(&["name", "defs", "code", "title", "impl", "kind"]);
        let phrase = def_query("flush the tree to disk", None, None, 5, Some(&code), None);
        let fields = phrase["query"]["function_score"]["query"]["bool"]["should"]
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
        // `code` is still text, so the pub ranking weight wraps function_score.
        // The numeric `kind` must not: that filter would name `kind`.
        let should = body
            .pointer("/query/function_score/query/bool/should")
            .or_else(|| body.pointer("/query/bool/should"))
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
