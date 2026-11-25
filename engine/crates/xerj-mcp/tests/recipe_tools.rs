//! #2762: a TOML recipe in a recipe directory is a working MCP tool, with no
//! Rust change. The recipe below is the only definition of `xerj_advisories_by_cwe`:
//! these tests load it, find it in `tools/list`, call it through the server's
//! `tools/call` dispatch (over the JSON-RPC handler and over `--http`), and check
//! that what comes back is the sealed, RFC 8785 (JCS) report the recipe runner
//! produced. The node URL points at a closed port throughout: a recipe tool
//! never calls the node.

use std::path::Path;

use serde_json::{json, Value};
use xerj_common::pack_recipe::{PackRecipe, RecipeExecutionReport};
use xerj_mcp::{
    handle_message_with_recipes_for_test, tool_specs_with, tool_specs_with_recipes, HttpOptions,
    RecipeRegistry, CORPORA_SUBDIR,
};

/// Never contacted; a recipe tool needs no node.
const NO_NODE: &str = "http://127.0.0.1:9";

const TOOL: &str = "xerj_advisories_by_cwe";

const RECIPE: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "advisories"
name = "advisories.by_cwe"
description = "Cited advisories for one CWE from the demo-advisories corpus, best score first."
static_notice = "Advisories are evidence, not a verdict on any system."

[[inputs]]
name = "cwe"
kind = "string"
required = true
description = "CWE identifier to filter on (for example CWE-918)."
min_len = 5
max_len = 16

[corpora]
required = ["demo-advisories"]
require_redistribution_licence = true
min_hits = 1
min_evidence_hits = 2

[[filters]]
field = "cwe"
op = "equals"
from_input = "cwe"

[ranking]
strategy = "score_desc"
top_k = 3

[output_schema]
required_fields = ["recipe", "pack", "notice", "cwe", "items"]

[output_schema.properties.recipe]
kind = "string"
non_empty = true

[output_schema.properties.pack]
kind = "string"
non_empty = true

[output_schema.properties.notice]
kind = "string"
non_empty = true

[output_schema.properties.cwe]
kind = "string"
non_empty = true
pattern_prefix = "CWE-"

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at", "cwe"]

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"

[[checks]]
id = "notice-present"
kind = "require_notice"
target_field = "notice"
expected_substring = "not a verdict"
"#;

fn hit(record_id: &str, cwe: &str, score: f64, licence: &str) -> String {
    json!({
        "corpus": "demo-advisories",
        "licence_class": licence,
        "record_id": record_id,
        "title": format!("Advisory {record_id}"),
        "excerpt": format!("Excerpt of {record_id}."),
        "score": score,
        "published_date": "2026-11-01",
        "pin_sha": "b".repeat(40),
        "fetched_at": "2026-11-05",
        "fields": { "cwe": cwe },
    })
    .to_string()
}

/// A recipe directory with the recipe and the hits its corpus holds: three for
/// CWE-918 (best score is `adv-2`), one for CWE-79.
fn recipe_dir(licence: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("advisories_by_cwe.toml"), RECIPE).unwrap();
    write_hits(
        dir.path(),
        &[
            hit("adv-1", "CWE-918", 0.70, licence),
            hit("adv-2", "CWE-918", 0.90, licence),
            hit("adv-3", "CWE-918", 0.80, licence),
            hit("adv-4", "CWE-79", 0.99, licence),
        ],
    );
    dir
}

fn write_hits(dir: &Path, lines: &[String]) {
    let corpora = dir.join(CORPORA_SUBDIR);
    std::fs::create_dir_all(&corpora).unwrap();
    std::fs::write(corpora.join("demo-advisories.jsonl"), lines.join("\n")).unwrap();
}

fn list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

fn call(args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
           "params": {"name": TOOL, "arguments": args}})
}

async fn rpc(registry: &RecipeRegistry, msg: Value) -> Value {
    handle_message_with_recipes_for_test(NO_NODE, None, registry, msg)
        .await
        .expect("a request is answered")
}

fn tool_names(reply: &Value) -> Vec<String> {
    reply["result"]["tools"]
        .as_array()
        .expect("tools/list result has `tools`")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// The text of the one content block of a `tools/call` result.
fn text(reply: &Value) -> &str {
    let content = reply["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{reply}");
    assert_eq!(content[0]["type"], "text", "{reply}");
    content[0]["text"].as_str().unwrap()
}

#[tokio::test]
async fn a_recipe_file_is_listed_and_called_as_an_mcp_tool_with_no_rust() {
    let dir = recipe_dir("adapt-with-attribution");
    let registry = RecipeRegistry::load_dir(dir.path()).unwrap();
    let recipe = PackRecipe::from_toml_str(RECIPE).unwrap();

    // tools/list: the built-in tools, unchanged and in order, then the recipe's
    // tool exactly as the recipe projects it.
    let listed = rpc(&registry, list()).await;
    let names = tool_names(&listed);
    let built_in = xerj_mcp::tool_specs();
    let built_in_names: Vec<&str> = built_in
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), built_in_names.len() + 1, "{names:?}");
    assert_eq!(names[..built_in_names.len()], built_in_names);
    assert_eq!(names.last().unwrap(), TOOL);
    let spec = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(spec, &recipe.to_mcp_tool_spec());
    assert_eq!(spec["inputSchema"]["required"], json!(["cwe"]));
    assert_eq!(spec["inputSchema"]["additionalProperties"], false);
    assert_eq!(spec["inputSchema"]["properties"]["cwe"]["type"], "string");

    // tools/call: the runner's sealed report, as RFC 8785 bytes.
    let reply = rpc(&registry, call(json!({"cwe": "CWE-918"}))).await;
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let body = text(&reply);
    let report: RecipeExecutionReport = serde_json::from_str(body).unwrap();
    report
        .verify(&recipe)
        .expect("the digest and the output hold");
    assert_eq!(report.to_jcs(&recipe).unwrap(), body.as_bytes(), "not JCS");
    assert_eq!(report.mcp_tool_name, TOOL);
    assert_eq!(report.recipe_name, "advisories.by_cwe");
    assert_eq!(report.report_sha256.len(), 64);
    assert_eq!(report.checks.len(), 2);
    assert!(report.checks.iter().all(|c| c.passed));

    // Filtered to CWE-918, best score first, top_k applied; the CWE-79 hit
    // (higher score) is not there.
    let ids: Vec<&str> = report.output["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["record_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["adv-2", "adv-3", "adv-1"]);
    assert_eq!(report.output["cwe"], "CWE-918");
    assert_eq!(
        report.output["notice"],
        "Advisories are evidence, not a verdict on any system."
    );

    // Deterministic: the same call seals the same bytes.
    let again = rpc(&registry, call(json!({"cwe": "CWE-918"}))).await;
    assert_eq!(text(&again), body);
}

#[tokio::test]
async fn a_server_with_no_recipe_serves_exactly_the_default_tools() {
    let empty = RecipeRegistry::new();
    for include_hub in [true, false] {
        assert_eq!(
            tool_specs_with_recipes(include_hub, &empty),
            tool_specs_with(include_hub)
        );
    }
    let names = tool_names(&rpc(&empty, list()).await);
    assert!(!names.iter().any(|n| n == TOOL), "{names:?}");

    // Without the recipe the name is no tool at all.
    let reply = rpc(&empty, call(json!({"cwe": "CWE-918"}))).await;
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(text(&reply).starts_with("unknown tool"), "{reply}");
}

#[tokio::test]
async fn a_refusal_is_an_error_result_that_names_the_reason() {
    let dir = recipe_dir("adapt-with-attribution");
    let registry = RecipeRegistry::load_dir(dir.path()).unwrap();

    for (args, expect) in [
        (json!({}), "missing required argument"),
        (json!({"cwe": "CWE-918", "extra": 1}), "unknown argument"),
        (json!({"cwe": "918"}), "shorter than min_len"),
        // One CWE-79 hit passes the filter; the recipe needs two.
        (
            json!({"cwe": "CWE-79"}),
            "abstained: matched 1 hits, need at least 2",
        ),
        (json!("CWE-918"), "arguments must be a JSON object"),
    ] {
        let reply = rpc(&registry, call(args.clone())).await;
        assert_eq!(reply["result"]["isError"], true, "{args}: {reply}");
        assert!(text(&reply).contains(expect), "{args}: {reply}");
    }

    // A corpus the operator marked non-redistributable is refused outright.
    write_hits(
        dir.path(),
        &[
            hit("adv-1", "CWE-918", 0.7, "excluded"),
            hit("adv-2", "CWE-918", 0.9, "excluded"),
        ],
    );
    let reply = rpc(&registry, call(json!({"cwe": "CWE-918"}))).await;
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(
        text(&reply).contains("licence class is Excluded"),
        "{reply}"
    );

    // No snapshot for the required corpus: the runner's own refusal.
    std::fs::remove_dir_all(dir.path().join(CORPORA_SUBDIR)).unwrap();
    let reply = rpc(&registry, call(json!({"cwe": "CWE-918"}))).await;
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(
        text(&reply).contains("missing required corpus `demo-advisories`"),
        "{reply}"
    );
}

#[tokio::test]
async fn a_recipe_tool_is_served_over_http_read_only_scope() {
    let dir = recipe_dir("adapt-with-attribution");
    let registry = RecipeRegistry::load_dir(dir.path()).unwrap();

    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        xerj_mcp::serve_http_with_recipes_for_test(
            listener,
            NO_NODE,
            None,
            registry,
            HttpOptions::default(),
        )
        .await
        .unwrap()
    });

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let post = |body: Value| {
        let client = client.clone();
        let url = url.clone();
        async move {
            let resp = client
                .post(url)
                .header("Content-Type", "application/json")
                .body(body.to_string())
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            resp.json::<Value>().await.unwrap()
        }
    };

    // Read-only scope: the recipe tool is listed beside the read-only tools,
    // and a writer stays out.
    let names = tool_names(&post(list()).await);
    assert!(names.iter().any(|n| n == TOOL), "{names:?}");
    assert!(names.iter().any(|n| n == "xerj_search"), "{names:?}");
    assert!(!names.iter().any(|n| n == "xerj_memory_store"), "{names:?}");

    let reply = post(call(json!({"cwe": "CWE-918"}))).await;
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let report: RecipeExecutionReport = serde_json::from_str(text(&reply)).unwrap();
    report
        .verify(&PackRecipe::from_toml_str(RECIPE).unwrap())
        .unwrap();
}

#[tokio::test]
async fn an_invalid_recipe_directory_stops_the_server_before_it_starts() {
    let missing = std::env::temp_dir().join("xerj-mcp-no-such-recipe-dir");
    let flag = missing.to_str().unwrap().to_string();
    let err = xerj_mcp::run(&["--recipe-dir".to_string(), flag])
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("cannot read the recipe directory"),
        "{err:#}"
    );

    let dir = recipe_dir("adapt-with-attribution");
    std::fs::write(dir.path().join("broken.toml"), "[recipe]\nname = ").unwrap();
    let flag = dir.path().to_str().unwrap().to_string();
    let err = xerj_mcp::run(&["--recipe-dir".to_string(), flag])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("broken.toml"), "{err:#}");

    let err = xerj_mcp::run(&["--recipe-dir".to_string()])
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("--recipe-dir requires a directory"),
        "{err}"
    );
}
