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

use crate::esclient::Es;
use crate::search::{is_identifier, passage_text};
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
    let mut it = std::env::args().skip(2);
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

    let es = match Es::new(&url, api_key) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj def: could not build client for {url}: {e}");
            return 2;
        }
    };
    if let Err(e) = es.ping() {
        eprintln!(
            "xerj def: no XERJ node reachable at {url} ({e}).\n\n\
             Start one, then index a repo:\n  \
             xerj --insecure -d ./.xerj-data &\n  \
             xerj autoindex <folder>\n\n\
             (or point at an existing node with --url / $XERJ_URL)"
        );
        return 2;
    }

    // Definition-first ranking, measured at 94% correct-file-at-rank-1 on an
    // 80-task cross-file benchmark (vs 38% for body-text search):
    //   - `term name`: the whole query IS the identifier — keyword-exact on
    //     the per-symbol docs, the strongest signal, boost 12.
    //   - `match_phrase defs`: the file-level definition list; also carries
    //     pre-#500 indexes that have no symbol docs, boost 8.
    //   - `multi_match`: recall floor so plain-words queries ("euler angles to
    //     rotation matrix") still surface the definer via `defs`/`code` text.
    let mut should = vec![
        json!({ "term": { "name": { "value": query, "boost": 12 } } }),
        json!({ "match_phrase": { "defs": { "query": query, "boost": 8 } } }),
        json!({ "multi_match": {
            "query": query,
            "fields": ["name^4", "defs^3", "code^2", "title"],
            "type": "most_fields"
        }}),
    ];
    if is_identifier(&query) {
        // `code` holds the declaration text; a phrase hit there ("def <name>(",
        // "class <name>:") is a definition even when `name` casing differs.
        should.push(json!({ "match_phrase": { "code": { "query": query, "boost": 4 } } }));
    }
    let mut filter = Vec::new();
    if let Some(kd) = &kind {
        filter.push(json!({ "term": { "kind": kd } }));
    }
    if let Some(lg) = &lang {
        filter.push(json!({ "term": { "language": lg } }));
    }
    let body = json!({
        "size": k,
        "query": { "bool": { "should": should, "filter": filter } },
        "_source": ["ax_path", "path", "line", "kind", "language", "name", "code"],
        "fields": ["_passage"]
    });

    let pattern = format!("{prefix}-*");
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
        println!("no definition in {pattern} for: {query}");
        println!("(broader ranked retrieval: xerj search \"{query}\")");
        return 0;
    }

    for h in &hits {
        let src = h.get("_source").cloned().unwrap_or(Value::Null);
        let path = src
            .get("ax_path")
            .or_else(|| src.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        let line = src.get("line").and_then(Value::as_u64);
        let loc = match line {
            Some(n) => format!("{path}:{n}"),
            None => path.to_string(),
        };
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
