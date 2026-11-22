//! Drift guard: the published size of the MCP tool catalog vs what the server
//! serves.
//!
//! Every MCP client loads the whole `tools/list` answer into the model's
//! context before the first call, so its size is a cost an agent pays every
//! session. #1785 asks for it to be measured in tokens and published.
//! `scripts/mcp/catalog_tokens.py --write` measures it (bytes exact, tokens
//! with two public BPE tokenizers) into
//! `benchmarks/mcp-catalog-size/results/catalog.json`.
//!
//! Tokens cannot be recounted here without a tokenizer, but bytes can, and the
//! two move together. This test serialises [`xerj_mcp::tool_specs_with`] the
//! way the script does (compact JSON) and fails when the recorded tool count
//! differs, or the recorded bytes are more than 10% off, so the published
//! numbers cannot quietly describe a catalog the server no longer serves.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Allowed relative drift between recorded and served bytes before the
/// published numbers must be regenerated.
const MAX_DRIFT: f64 = 0.10;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate manifest dir must be <repo>/engine/crates/xerj-mcp")
        .to_path_buf()
}

/// A missing or unparseable results file is a hard failure, never a skip.
fn recorded() -> Value {
    let path = repo_root().join("benchmarks/mcp-catalog-size/results/catalog.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\nRegenerate it with `python3 scripts/mcp/catalog_tokens.py --write`.",
            path.display()
        )
    });
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

fn check(label: &str, include_hub: bool) {
    let specs = xerj_mcp::tool_specs_with(include_hub);
    let served_tools = specs.as_array().expect("tool_specs is an array").len();
    let served_bytes = serde_json::to_string(&specs)
        .expect("tool_specs serialises")
        .len();

    let rec = recorded();
    let total = &rec["catalogs"][label]["total"];
    let rec_tools = total["tools"]
        .as_u64()
        .unwrap_or_else(|| panic!("catalogs.{label}.total.tools missing"));
    let rec_bytes = total["bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("catalogs.{label}.total.bytes missing"));
    for enc in ["o200k_base", "cl100k_base"] {
        assert!(
            total[enc].as_u64().is_some_and(|n| n > 0),
            "catalogs.{label}.total.{enc} missing: the results must carry token counts"
        );
    }

    let regen = "regenerate with `scripts/mcp-schema-check.sh --write` then \
                 `python3 scripts/mcp/catalog_tokens.py --write`";
    assert_eq!(
        rec_tools as usize, served_tools,
        "{label}: catalog.json records {rec_tools} tools, the server serves {served_tools}; {regen}"
    );
    let drift = (served_bytes as f64 - rec_bytes as f64).abs() / rec_bytes as f64;
    assert!(
        drift <= MAX_DRIFT,
        "{label}: catalog.json records {rec_bytes} bytes, the server serves {served_bytes} \
         ({:.1}% off, limit {:.0}%); {regen}",
        drift * 100.0,
        MAX_DRIFT * 100.0
    );
}

#[test]
fn recorded_catalog_size_with_hub_key_matches_the_server() {
    check("with_hub_key", true);
}

#[test]
fn recorded_catalog_size_without_hub_key_matches_the_server() {
    check("no_hub_key", false);
}
