//! Drift guard: the published MCP tool schema vs the tools the server serves.
//!
//! `landing/docs/agents/schemas/mcp-tools.json` is what an agent reads to find
//! out what XERJ's MCP server can do. It was hand-written, and it drifted: it
//! advertised **six** tools while the binary served **ten**, missing every
//! `xerj_brain_*` tool. Nothing compared the two, so nothing failed — the file
//! simply told agents a smaller, wrong story for as long as it existed.
//!
//! The file is now generated from a live `tools/list` response
//! (`scripts/mcp-schema-check.sh --write`), and this test pins it to
//! [`xerj_mcp::tool_specs`], the function that response is built from. Adding,
//! renaming, or re-describing a tool fails here until the published schema is
//! regenerated. That is the point: the failure is cheap, and the silent
//! divergence it prevents is not.
//!
//! Scope note: this test covers the library that both binaries serve from. The
//! end-to-end path — build the real binary, speak JSON-RPC over stdio, diff the
//! wire response against the file — is `scripts/mcp-schema-check.sh`, wired
//! into CI next to the MCP smoke test.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Repository root, derived from this crate's manifest directory
/// (`<repo>/engine/crates/xerj-mcp`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate manifest dir must be <repo>/engine/crates/xerj-mcp")
        .to_path_buf()
}

fn published_path() -> PathBuf {
    repo_root().join("landing/docs/agents/schemas/mcp-tools.json")
}

/// Load the published schema. A missing or unparseable file is a hard failure,
/// never a skip: a guard that quietly checks nothing is the exact failure mode
/// this file exists to prevent.
fn published() -> Value {
    let path = published_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read the published MCP schema at {}: {e}\n\
             It is the agent-facing contract for this crate's tools. If it moved, \
             update this test and scripts/mcp-schema-check.sh together.",
            path.display()
        )
    });
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

fn names(tools: &Value) -> Vec<String> {
    tools
        .as_array()
        .expect("`tools` must be an array")
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .expect("every tool needs a string `name`")
                .to_string()
        })
        .collect()
}

/// The headline check: same tools, same order.
#[test]
fn published_tool_names_match_the_server() {
    // The published file is the with-key tool list. A process with
    // `XERJ_HUB_PUBKEY=none` omits the three hub tools from `tool_specs()`;
    // that omission is covered in the library tests, and must not look like
    // the committed schema drifted.
    let served = names(&xerj_mcp::tool_specs_with(true));
    let doc = names(&published()["tools"]);
    assert_eq!(
        doc, served,
        "\nlanding/docs/agents/schemas/mcp-tools.json advertises a different tool set \
         than the MCP server serves.\n  published: {doc:?}\n  served:    {served:?}\n\
         Regenerate it from the real binary:\n    \
         scripts/mcp-schema-check.sh --write\n"
    );
}

/// The full check: every field of every tool, byte-for-byte after JSON
/// normalisation. Descriptions are what an agent reads to decide whether to
/// call a tool at all, so a stale description is a real defect, not cosmetics —
/// the brain tools' honesty sentences live there.
#[test]
fn published_schemas_match_the_server_exactly() {
    let served = xerj_mcp::tool_specs_with(true);
    let doc = published()["tools"].clone();

    let served_by_name: std::collections::BTreeMap<String, &Value> = served
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap().to_string(), t))
        .collect();

    for tool in doc.as_array().expect("`tools` must be an array") {
        let name = tool["name"].as_str().expect("tool needs a name");
        let Some(want) = served_by_name.get(name) else {
            continue; // name-set mismatch is reported by the test above
        };
        assert_eq!(
            &tool, want,
            "\npublished schema for `{name}` differs from what the server serves.\n\
             Regenerate: scripts/mcp-schema-check.sh --write\n"
        );
    }
}

/// The published file must keep the envelope agents already fetch: a top-level
/// object with a `tools` array. Reshaping it silently breaks every consumer.
#[test]
fn published_envelope_is_preserved() {
    let doc = published();
    assert!(doc.is_object(), "top level must be a JSON object");
    assert!(
        doc.get("tools").map(Value::is_array).unwrap_or(false),
        "top level must carry a `tools` array"
    );
}

/// The `xerj mcp` subcommand is the only way most users can reach this server —
/// they installed one binary. If the dispatch arm is deleted, the crate still
/// builds and every other test still passes, so pin the wiring itself.
#[test]
fn main_binary_dispatches_the_mcp_subcommand() {
    let main_rs = repo_root().join("engine/crates/xerj-server/src/main.rs");
    let src = std::fs::read_to_string(&main_rs)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", main_rs.display()));
    assert!(
        src.contains("xerj_mcp::run"),
        "{} no longer calls xerj_mcp::run — `xerj mcp` would be unreachable, and \
         with it the only MCP server an installed user can start",
        main_rs.display()
    );
    assert!(
        src.contains("Some(\"mcp\")"),
        "{} no longer dispatches on argv[1] == \"mcp\"",
        main_rs.display()
    );
}
