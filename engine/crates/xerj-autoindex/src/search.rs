//! `xerj search "<query>"` — the binary as a query client.
//!
//! Reference coding is "index a repo, then retrieve the definition before you
//! write". Indexing is already one command (`xerj autoindex`); this makes the
//! retrieval side one command too, so the whole loop is the binary plus a
//! running node — no wrapper scripts. It detects a node the same way the
//! flagship path does (health-ping the ES-compat endpoint) and runs a
//! `multi_match` biased toward symbol definitions, printing a passage to read
//! rather than a JSON blob to parse.

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
";

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
        eprintln!(
            "xerj search: no XERJ node reachable at {url} ({e}).\n\n\
             Start one, then index a repo:\n  \
             xerj --insecure -d ./.xerj-data &\n  \
             xerj autoindex <folder>\n\n\
             (or point at an existing node with --url / $XERJ_URL)"
        );
        return 2;
    }

    // Bias the match toward symbol definitions (defs), then body, then title —
    // the same ranking the reference-coding wrapper used, now built in.
    let body = json!({
        "size": k,
        "query": { "multi_match": {
            "query": query,
            "fields": ["defs^3", "body", "title"],
            "type": "most_fields"
        }},
        "_source": ["ax_path", "start_line", "language", "body", "title"]
    });

    let pattern = format!("{prefix}-*");
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
        println!("no match in {pattern} for: {query}");
        println!("(index a relevant repo first: xerj autoindex <folder>)");
        return 0;
    }

    for h in &hits {
        let src = h.get("_source").cloned().unwrap_or(Value::Null);
        let path = src.get("ax_path").and_then(Value::as_str).unwrap_or("?");
        let score = h.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        let line = src.get("start_line").and_then(Value::as_u64);
        let loc = match line {
            Some(n) => format!("{path}:{n}"),
            None => path.to_string(),
        };
        println!("\n─── {loc}  (score {score:.2})");
        if let Some(text) = src.get("body").and_then(Value::as_str) {
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
