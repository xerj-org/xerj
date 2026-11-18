//! JSON-in/JSON-out CLI for the same batch adapter MCP calls.
use std::io::Read;
use std::time::Duration;

use crate::{batch, node_http_builder, resolve_auth, Ctx, DEFAULT_XERJ_URL};

const HELP: &str = "xerj batch-search — ordered batch retrieval (#1801)

USAGE: xerj batch-search --input <file|-> [--url URL] [--auth VALUE]

Read one JSON object (at most 1 MiB) using the xerj_batch_search schema:
  {\"prefix\":\"project\",\"size\":3,\"queries\":[
    {\"key\":\"question\",\"query\":\"rotate the WAL\"},
    {\"key\":\"advisory\",\"identifier\":\"CVE-2026-12345\",\"field\":\"id\"}
  ]}

queries: 1..50 objects, each query (text or ES query clause) OR identifier+field.
A plain string item is shorthand for {\"query\": <string>}.
Exact identifiers require a flat indexed keyword field. index/prefix, size
(1..50, default 3) and optional max_tokens are shared by all items.
Output is one JSON object with ordered results; position is zero-based and
authoritative even for duplicate keys. status is hits, abstain (complete empty
search only), or error. Hits contain citations and raw evidence, not generated
answers or calibrated confidence. A miss does not discard its siblings.
max_tokens defaults to 8000 when omitted or null; pass a larger integer for more.
It counts the entire JSON as UAX #29 words, not bytes or model/BPE tokens.
It removes raw bodies and then extra hits, retaining every item outcome and
one citation per matched item; an insufficient budget is an error, not truncation.

--url / --auth override XERJ_URL / XERJ_AUTH. Default URL: http://localhost:9200.
Local loopback admin-key discovery is the same as xerj mcp. A node must already
be running. This command writes no data and does not download any corpus.
Exit: 0 all items completed (including abstentions), 3 per-item errors,
      2 invalid CLI/input, 1 a batch-wide execution/budget error.
";

struct Options {
    input: String,
    url: Option<String>,
    auth: Option<String>,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut out = Options {
        input: String::new(),
        url: None,
        auth: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--input" | "--url" | "--auth" => {
                let value = it.next().ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--input" => out.input = value.clone(),
                    "--url" => out.url = Some(value.clone()),
                    _ => out.auth = Some(value.clone()),
                }
            }
            xerj_common::feedback::DISABLE_FLAG => {}
            _ => {
                return Err(format!(
                    "unknown argument `{arg}`; see xerj batch-search --help"
                ))
            }
        }
    }
    if out.input.is_empty() {
        return Err("--input <file|-> is required; see xerj batch-search --help".into());
    }
    Ok(out)
}

fn read_input(path: &str) -> Result<serde_json::Value, String> {
    let reader: Box<dyn Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path).map_err(|e| format!("cannot open input: {e}"))?)
    };
    let mut bytes = Vec::new();
    reader
        .take(batch::MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read input: {e}"))?;
    if bytes.len() > batch::MAX_INPUT_BYTES {
        return Err("batch input exceeds 1 MiB; split it".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid batch JSON: {e}"))
}

/// Called before node startup by the main binary; never starts a node.
pub async fn run_batch_cli(args: &[String]) -> i32 {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return 0;
    }
    let opts = match options(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let input = match read_input(&opts.input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(e) = batch::envelope(&input) {
        eprintln!("error: {e}");
        return 2;
    }
    let base_url = opts
        .url
        .or_else(|| std::env::var("XERJ_URL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_XERJ_URL.into())
        .trim_end_matches('/')
        .to_string();
    let (auth, found) = resolve_auth(opts.auth, std::env::var("XERJ_AUTH").ok(), &base_url);
    if let Some(found) = found {
        eprintln!(
            "xerj batch-search: using local admin key at {} ({})",
            found.path.display(),
            found.provenance()
        );
        if let Some(warning) = found.foreign_warning("--auth or XERJ_AUTH") {
            eprintln!("xerj batch-search: warning: {warning}");
        }
    }
    let client = match node_http_builder(&base_url)
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    match batch::execute(
        &Ctx {
            client,
            base_url,
            auth,
        },
        &input,
    )
    .await
    {
        Ok(text) => {
            let result: serde_json::Value =
                serde_json::from_str(&text).expect("batch returns JSON");
            let errors = result["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["status"] == "error");
            println!("{text}");
            if errors {
                3
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_requires_explicit_input_and_rejects_typos() {
        assert!(options(&[]).is_err());
        assert!(options(&["--input".into()]).is_err());
        assert!(options(&["--input".into(), "-".into(), "--urll".into()]).is_err());
        let parsed = options(&[
            "--input".into(),
            "-".into(),
            "--url".into(),
            "http://localhost:9201".into(),
        ])
        .unwrap();
        assert_eq!(parsed.input, "-");
        assert_eq!(parsed.url.as_deref(), Some("http://localhost:9201"));
    }
}
