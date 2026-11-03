//! A plain-string `xerj_search` for an exact error message ranks the file
//! that contains it, end to end against a real in-process node.
//!
//! The corpus is the autoindex shape in miniature: per-file docs with a
//! `body` and a `defs` symbol list, plus a per-symbol doc. One short file's
//! `defs` (`Memory`, `MemoryCandidate`) shares two words of the query; one
//! long file holds the whole message in order. Without an exact-phrase leg
//! the `most_fields` sum puts the short `defs` list first, because a 400-line
//! body scores low on BM25 length normalisation however many query terms it
//! holds.

use serde_json::{json, Value};

const PHRASE: &str = "Memory candidate capacity exceeded";

fn long_module() -> String {
    let mut out = String::from("class BackendError(Exception):\n    pass\n\n");
    for i in 0..40 {
        if i == 20 {
            out.push_str(&format!(
                "def load_candidates(rows):\n    picked = []\n    for row in rows:\n        \
                 picked.append(row)\n        if len(picked) > 1000:\n            \
                 raise BackendError(\"{PHRASE}\")\n    return picked\n\n"
            ));
        }
        out.push_str(&format!(
            "def helper_{i}(rows, limit):\n    \"\"\"Normalise batch {i} of loaded rows.\"\"\"\n    \
             out = []\n    for row in rows:\n        if not isinstance(row, dict):\n            \
             raise ValueError(\"row {i} is not an object\")\n        out.append(row)\n    \
             return out[:limit]\n\n"
        ));
    }
    out
}

async fn seed(client: &reqwest::Client, url: &str) {
    let index = format!("{url}/ax-docs");
    let r = client
        .put(&index)
        .json(&json!({ "mappings": { "properties": {
            "ax_path": {"type": "keyword"}, "title": {"type": "keyword"},
            "name": {"type": "keyword"}, "code": {"type": "keyword"},
            "kind": {"type": "keyword"}, "line": {"type": "long"},
            "defs": {"type": "text"}, "body": {"type": "text"}
        }}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "create {index}");
    let mut defs = vec!["class BackendError".to_string()];
    defs.extend((0..40).map(|i| format!("function helper_{i}")));
    defs.insert(21, "function load_candidates".into());
    let docs = [
        json!({"ax_path": "memory_view.tsx", "title": "memory_view.tsx",
               "defs": "function Memory\nconst MemoryCandidate\nfunction MemoryCandidate",
               "body": "export function Memory({ items }) {\n  const memory = items.filter(Boolean);\n  \
                        // Memory panel: each memory candidate the user saved.\n}\n\
                        export const MemoryCandidate = (props) => props.text;\n"}),
        json!({"ax_path": "memory_view.tsx", "title": "Memory", "name": "Memory",
               "kind": "function", "line": 1, "code": "export function Memory({ items })"}),
        json!({"ax_path": "bridge.py", "title": "bridge.py", "defs": defs.join("\n"),
               "body": long_module()}),
        json!({"ax_path": "bridge.py", "title": "load_candidates", "name": "load_candidates",
               "kind": "function", "line": 23, "code": "def load_candidates(rows):"}),
        json!({"ax_path": "notes.md", "title": "notes.md",
               "body": "# Memory\n\nMemory records are saved per project. A memory candidate \
                        is reviewed before it is shared.\n"}),
    ];
    let mut bulk = String::new();
    for d in docs {
        bulk.push_str(&format!("{{\"index\":{{}}}}\n{d}\n"));
    }
    let r = client
        .post(format!("{index}/_bulk?refresh=true"))
        .header("content-type", "application/x-ndjson")
        .body(bulk)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "bulk {index}");
    // Autoindex leaves its corpus flushed into segments; rank against that,
    // not the memtable.
    let r = client.post(format!("{index}/_flush")).send().await.unwrap();
    assert!(r.status().is_success(), "flush {index}");
}

async fn ranked_paths(url: &str, query: &str) -> Vec<String> {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": {"name": "xerj_search", "arguments": {"query": query}}});
    let resp = xerj_mcp::handle_message_for_test(url, None, msg)
        .await
        .expect("a request gets a response");
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {resp}"));
    let body: Value =
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: not JSON: {text}"));
    body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits array: {body}"))
        .iter()
        .filter_map(|h| h.pointer("/_source/ax_path").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exact_error_message_ranks_the_file_that_contains_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = tmp.path().join("data").to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.auth.enabled = false;
    let metrics = xerj_common::metrics::Metrics::new().unwrap();
    let engine = xerj_engine::Engine::new(config.clone()).unwrap();
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state);
    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service()).await });
    seed(&reqwest::Client::new(), &url).await;

    let paths = ranked_paths(&url, PHRASE).await;
    assert_eq!(
        paths.first().map(String::as_str),
        Some("bridge.py"),
        "the file holding \"{PHRASE}\" must rank first: {paths:?}"
    );

    // The definition-first shape is untouched: an identifier still lands on
    // its symbol doc through the `name` term leg.
    let paths = ranked_paths(&url, "load_candidates").await;
    assert_eq!(
        paths.first().map(String::as_str),
        Some("bridge.py"),
        "{paths:?}"
    );
    let paths = ranked_paths(&url, "Memory").await;
    assert_eq!(
        paths.first().map(String::as_str),
        Some("memory_view.tsx"),
        "{paths:?}"
    );
}
