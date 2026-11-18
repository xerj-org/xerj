//! `xerj memory store` — one agent memory, on a node that is already running.
//!
//! A flood-stage write block used to come back as a nested
//! `cluster_block_exception` (#2284). This command prints the plain sentence
//! from [`crate::flood::explain_memory_flood`]: the disk holding the data dir
//! is nearly full, the free space, the path, and what to do.

use serde_json::{json, Value};

use crate::esclient::Es;
use crate::flood::{disk_facts_from_allocation, explain_memory_flood, DiskFacts};

const USAGE: &str = "\
xerj memory — store one agent memory on a running XERJ node

USAGE:
    xerj memory store <namespace> --text <text>

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --api-key <K>   API key (or $XERJ_API_KEY; a loopback node falls back to <data-dir>/admin.key)
    --text <text>   the memory to store
    --id <id>       optional explicit id
    --json          print the raw response body on success

A disk flood-stage write block is explained in plain text: free space, the
data-dir path, and whether to free space or restart with --data-dir on
another drive. Other errors are the server's own reason.

EXAMPLE:
    xerj --insecure --data-dir ./xerj-data &
    xerj memory store agent-ada --text \"the user prefers dark mode\"
";

/// Entry point for the `memory` subcommand. Returns a process exit code.
pub fn run_memory_cli() -> i32 {
    run_memory_from(&xerj_common::argv::args_after_subcommand())
}

pub fn run_memory_from(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return 0;
    }
    let Some(verb) = args.first() else {
        print!("{USAGE}");
        return 0;
    };
    if verb != "store" {
        eprintln!("xerj memory: unknown command '{verb}'\n\n{USAGE}");
        return 2;
    }

    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut namespace: Option<String> = None;
    let mut text: Option<String> = None;
    let mut id: Option<String> = None;
    let mut as_json = false;

    let mut it = args.iter().skip(1).cloned();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--url" => url = it.next().unwrap_or_default(),
            "--api-key" => api_key = it.next(),
            "--text" => text = it.next(),
            "--id" => id = it.next(),
            "--json" => as_json = true,
            "--disable-feedback" => {}
            other if other.starts_with('-') => {
                eprintln!("xerj memory: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
            other if namespace.is_none() => namespace = Some(other.to_string()),
            other => {
                eprintln!("xerj memory: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        }
    }

    let namespace = match namespace {
        Some(ns) if !ns.trim().is_empty() => ns,
        _ => {
            eprintln!("xerj memory: a namespace is required.\n\n{USAGE}");
            return 2;
        }
    };
    let text = match text {
        Some(text) if !text.is_empty() => text,
        _ => {
            eprintln!("xerj memory: --text is required.\n\n{USAGE}");
            return 2;
        }
    };
    if url.is_empty() {
        url = "http://localhost:9200".to_string();
    }
    if api_key.is_none() && xerj_common::localauth::url_is_loopback(&url) {
        if let Some(found) =
            xerj_common::localauth::discover_local_admin_key_for_url_with_origin(&url)
        {
            eprintln!(
                "xerj memory: no --api-key/XERJ_API_KEY given; using the admin key at {} ({})",
                found.path.display(),
                found.provenance()
            );
            if let Some(warning) = found.foreign_warning("--api-key or XERJ_API_KEY") {
                eprintln!("xerj memory: warning: {warning}");
            }
            api_key = Some(found.key);
        }
    }

    let es = match Es::new(&url, api_key) {
        Ok(es) => es,
        Err(error) => {
            eprintln!("xerj memory: {error}");
            return 1;
        }
    };
    let mut payload = json!({ "text": text });
    if let Some(id) = id {
        payload["id"] = json!(id);
    }
    let path = format!("/_memory/{namespace}");
    let (status, body) = match es.request_json("POST", &path, Some(&payload)) {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("xerj memory: {error}");
            return 1;
        }
    };
    if (200..300).contains(&status) {
        if as_json {
            println!(
                "{}",
                serde_json::to_string(&body).unwrap_or_else(|_| body.to_string())
            );
        } else if let Some(id) = body.get("id").and_then(Value::as_str) {
            let created = body
                .get("created")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            println!("stored {id} created={created}");
        } else {
            println!(
                "{}",
                serde_json::to_string(&body).unwrap_or_else(|_| body.to_string())
            );
        }
        return 0;
    }
    report_write_failure(&es, status, &body)
}

fn report_write_failure(es: &Es, status: u16, body: &Value) -> i32 {
    let facts = allocation_facts(es);
    match explain_memory_flood(body, facts.as_ref()) {
        Some(text) => eprintln!("xerj memory: {text}"),
        None => {
            let reason = body.pointer("/error/reason").and_then(Value::as_str);
            match reason {
                Some(reason) if !reason.is_empty() => eprintln!("xerj memory: {reason}"),
                _ => eprintln!("xerj memory: HTTP {status}: {body}"),
            }
        }
    }
    1
}

/// Allocation is how a client learns free space and `path.data` when the
/// error body is still the raw block (#2077's probe). A stats blip leaves
/// the facts empty; the sentence then says unknown.
fn allocation_facts(es: &Es) -> Option<DiskFacts> {
    let (status, body) = es
        .request_json("GET", "/_cat/allocation?format=json", None)
        .ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    Some(disk_facts_from_allocation(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn help_and_usage_do_not_talk_to_a_node() {
        assert_eq!(run_memory_from(&v(&[])), 0);
        assert_eq!(run_memory_from(&v(&["--help"])), 0);
        assert_eq!(run_memory_from(&v(&["store", "--help"])), 0);
        assert_eq!(run_memory_from(&v(&["recall", "ns"])), 2);
        assert_eq!(run_memory_from(&v(&["store"])), 2);
        assert_eq!(run_memory_from(&v(&["store", "ns"])), 2);
        assert_eq!(run_memory_from(&v(&["store", "ns", "--text"])), 2);
        assert_eq!(run_memory_from(&v(&["store", "--bogus", "x"])), 2);
    }
}
