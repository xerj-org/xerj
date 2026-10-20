//! The `xerj_hub_*` MCP tools end to end (plan §7 "Tests"): JSON-RPC
//! `tools/list` and `tools/call`, the way an MCP host sends them, against a real
//! in-process node holding the keystone's `mini-c` fixture corpus as a restored
//! pack (ledger, pack manifest, cached catalogue — the layout `xerj hub pull`
//! leaves), so the tools run the real `xerj ask` flow and `RealBackend`.
//!
//! One test, scenarios in order: they share process-global environment
//! (`XERJ_HUB_HOME`, `XERJ_CODE_HOME`, `XERJ_HUB_URL`). It is its own test
//! binary so that environment never races the crate's unit tests.

use std::path::Path;

use serde_json::{json, Value};
use xerj_common::hub::{
    Catalogue, CatalogueEntry, LicenceClass, PackIndex, PackManifest, CATALOGUE_FORMAT_VERSION,
    PACK_FORMAT_VERSION,
};

const PIN_SHA: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";
const PIN8: &str = "0774d055";
const PIN_DATE: &str = "2026-10-02";

fn entry(slug: &str, spdx: &str, class: LicenceClass, terms: &[&str]) -> CatalogueEntry {
    CatalogueEntry {
        slug: slug.into(),
        domain: "DEV".into(),
        topics: vec![],
        ask_for: format!("fixture corpus {slug}"),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        refresh_cadence_days: 7,
        licence_spdx: spdx.into(),
        licence_class: class,
        pack_bytes: 4096,
        pack_url: "https://example.invalid/never-fetched.tar.zst".into(),
        pack_sha256: "0".repeat(64),
        sig_url: "https://example.invalid/never-fetched.sig".into(),
        dependency_names: vec![],
        quality_grade: None,
        term_sketch: xerj_hub::sketch::build(terms.iter().map(|t| t.to_string())),
    }
}

/// What `xerj hub pull` leaves for a restored pack: the ledger, and
/// `<hub>/packs/<slug>/<pin8>/pack.json`.
fn restored(code: &Path, hub: &Path, url: &str, slug: &str, spdx: &str, class: &str) {
    let prefix = format!("xc-{slug}-b1");
    xerj_common::xccode::state::write_hubpack_state(
        code, slug, url, &prefix, PIN_SHA, PIN_DATE, spdx, class,
    )
    .unwrap();
    let dir = hub.join("packs").join(slug).join(PIN8);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: slug.into(),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        built_at: "2026-10-02T00:00:00Z".into(),
        builder_version: "test".into(),
        index_prefix: prefix.clone(),
        indices: ["defs", "docs"]
            .iter()
            .map(|s| PackIndex {
                name: format!("{prefix}-{s}"),
                docs: 1,
                bytes: 1,
            })
            .collect(),
        snapshot_name: "snap".into(),
        licence_spdx: spdx.into(),
        licence_class: serde_json::from_value(json!(class)).unwrap(),
        sources: vec![],
        defs_count: 1,
        files_count: 1,
    };
    std::fs::write(
        dir.join("pack.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

async fn seed(client: &reqwest::Client, url: &str, slug: &str, symbol: &str, value: &str) {
    let defs = format!("{url}/xc-{slug}-b1-defs");
    let r = client
        .put(&defs)
        .json(&json!({ "mappings": { "properties": {
            "symbol": {"type": "keyword"}, "symbol_text": {"type": "text"},
            "kind": {"type": "keyword"}, "lang": {"type": "keyword"},
            "file": {"type": "keyword"}, "line": {"type": "integer"},
            "text": {"type": "text"}, "value": {"type": "keyword"}
        }}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "create {defs}");
    let doc = json!({"symbol": symbol, "symbol_text": symbol, "kind": "define", "lang": "c",
        "file": "limits.h", "line": 5, "text": format!("#define {symbol} {value}"), "value": value});
    let r = client
        .post(format!("{defs}/_bulk?refresh=true"))
        .header("content-type", "application/x-ndjson")
        .body(format!("{{\"index\":{{}}}}\n{doc}\n"))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "bulk {defs}");
    let docs = json!({"ax_path": format!("{slug}/hash.c"), "title": "hash.c",
        "body": "unsigned mini_hash(const char *s) { frobnicate the buffer; }"});
    let r = client
        .post(format!("{url}/xc-{slug}-b1-docs/_bulk?refresh=true"))
        .header("content-type", "application/x-ndjson")
        .body(format!("{{\"index\":{{}}}}\n{docs}\n"))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "bulk docs");
}

async fn call(url: &str, name: &str, arguments: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": {"name": name, "arguments": arguments}});
    let resp = xerj_mcp::handle_message_for_test(url, None, msg)
        .await
        .expect("a request gets a response");
    resp["result"].clone()
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_tools_answer_from_a_local_pack_and_never_download() {
    let tmp = tempfile::tempdir().unwrap();
    let (hub, code) = (tmp.path().join("hub"), tmp.path().join("code"));
    std::fs::create_dir_all(&hub).unwrap();
    std::env::set_var("XERJ_HUB_HOME", &hub);
    std::env::set_var("XERJ_CODE_HOME", &code);
    // Nothing listens there: a catalogue fetch would fail loudly. The cached
    // catalogue below is fresh, so none is attempted.
    std::env::set_var("XERJ_HUB_URL", "http://127.0.0.1:9");

    // ── a real node, auth off, on an ephemeral loopback port ──
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = tmp.path().join("data").to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.auth.enabled = false;
    let metrics = xerj_common::metrics::Metrics::new().unwrap();
    let engine = xerj_engine::Engine::new(config.clone()).unwrap();
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service()).await });
    let client = reqwest::Client::new();
    seed(&client, &url, "mini-c", "MINI_LIMIT", "128").await;
    seed(&client, &url, "mini-gpl", "GPL_LIMIT", "7").await;

    restored(&code, &hub, &url, "mini-c", "MIT", "adapt-with-attribution");
    restored(
        &code,
        &hub,
        &url,
        "mini-gpl",
        "GPL-3.0-only",
        "approach-only",
    );
    let cat = Catalogue {
        format_version: CATALOGUE_FORMAT_VERSION,
        generated_at: "2026-10-14T00:00:00Z".into(),
        corpora: vec![
            entry(
                "mini-c",
                "MIT",
                LicenceClass::AdaptWithAttribution,
                &["MINI_LIMIT", "mini_hash"],
            ),
            entry(
                "mini-gpl",
                "GPL-3.0-only",
                LicenceClass::ApproachOnly,
                &["GPL_LIMIT"],
            ),
            entry(
                "mini-closed",
                "LicenseRef-Closed",
                LicenceClass::Excluded,
                &["CLOSED_LIMIT"],
            ),
            // In the catalogue, pullable, not on this machine.
            entry(
                "mini-far",
                "MIT",
                LicenceClass::AdaptWithAttribution,
                &["FAR_LIMIT"],
            ),
            entry(
                "filler-a",
                "MIT",
                LicenceClass::AdaptWithAttribution,
                &["alpha"],
            ),
            entry(
                "filler-b",
                "MIT",
                LicenceClass::AdaptWithAttribution,
                &["bravo"],
            ),
        ],
    };
    std::fs::write(
        hub.join("catalogue.json"),
        serde_json::to_vec(&cat).unwrap(),
    )
    .unwrap();

    // ── tools/list carries the three, with schemas an MCP host accepts ──
    let list = xerj_mcp::handle_message_for_test(
        &url,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await
    .unwrap();
    let tools = list["result"]["tools"].as_array().unwrap();
    for name in ["xerj_hub_find", "xerj_hub_def", "xerj_hub_ask"] {
        let t = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tools/list lacks {name}"));
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        let props = schema["properties"].as_object().unwrap();
        for req in schema["required"].as_array().unwrap() {
            assert!(
                props.contains_key(req.as_str().unwrap()),
                "{name}: {req} is required but undeclared"
            );
        }
        for (prop, spec) in props {
            let ty = spec["type"].as_str().unwrap_or_default();
            assert!(
                ["string", "integer"].contains(&ty),
                "{name}.{prop}: type {ty}"
            );
            assert!(
                spec["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{name}.{prop}"
            );
        }
        assert!(
            t["description"].as_str().is_some_and(|d| d.len() > 40),
            "{name}"
        );
    }

    // ── xerj_hub_def against the fixture pack returns the fixture value ──
    let r = call(
        &url,
        "xerj_hub_def",
        json!({"slug": "mini-c", "symbol": "MINI_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let a = &r["structuredContent"];
    assert_eq!(a["corpus"], "mini-c");
    assert_eq!(a["mode"], "def");
    assert_eq!(a["answered_from"], "local_pack");
    assert_eq!(a["pin_date"], PIN_DATE);
    assert_eq!(a["hits"][0]["value"], "128");
    assert_eq!(a["hits"][0]["file"], "limits.h");
    assert!(text(&r).contains("value: 128"), "{}", text(&r));
    assert!(
        text(&r).contains("answered from: local pack"),
        "{}",
        text(&r)
    );
    // The structured content is exactly an Answer (deny_unknown_fields).
    let _: xerj_common::hub::Answer = serde_json::from_value(a.clone()).unwrap();

    // k is carried through.
    let r = call(
        &url,
        "xerj_hub_def",
        json!({"slug": "mini-c", "symbol": "MINI_LIMIT", "k": 1}),
    )
    .await;
    assert_eq!(r["structuredContent"]["hits"].as_array().unwrap().len(), 1);

    // ── xerj_hub_ask with no corpus: routed by the sketch, named in the answer ──
    let r = call(
        &url,
        "xerj_hub_ask",
        json!({"question": "what is the value of MINI_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["corpus"], "mini-c");
    assert_eq!(r["structuredContent"]["hits"][0]["value"], "128");

    // ── approach-only: every hit carries the licence warning ──
    let r = call(
        &url,
        "xerj_hub_ask",
        json!({"question": "GPL_LIMIT", "corpus": "mini-gpl", "mode": "def"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    for h in r["structuredContent"]["hits"].as_array().unwrap() {
        assert_eq!(
            h["licence_warning"],
            "approach-only licence (GPL-3.0-only): read, do not copy"
        );
    }
    assert!(text(&r).contains("approach-only licence"), "{}", text(&r));

    // ── excluded: never served, never pulled, never hinted at ──
    let r = call(
        &url,
        "xerj_hub_def",
        json!({"slug": "mini-closed", "symbol": "CLOSED_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("excluded"), "{}", text(&r));
    assert!(!text(&r).contains("xerj hub pull"), "{}", text(&r));

    // ── not on this machine: the pull line, and nothing downloaded or written ──
    let r = call(
        &url,
        "xerj_hub_def",
        json!({"slug": "mini-far", "symbol": "FAR_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("xerj hub pull mini-far"), "{}", text(&r));
    assert!(
        !hub.join("packs").join("mini-far").exists(),
        "a tool call never pulls"
    );
    assert!(!hub.join("pulls").exists(), "nor starts a background pull");

    // ── xerj_hub_find: candidates, best first, excluded never listed ──
    let r = call(
        &url,
        "xerj_hub_find",
        json!({"question": "what is the value of MINI_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let found = r["structuredContent"]["corpora"].as_array().unwrap();
    assert_eq!(found[0]["slug"], "mini-c");
    assert_eq!(found[0]["cached"], true);
    let r = call(&url, "xerj_hub_find", json!({"question": "CLOSED_LIMIT"})).await;
    assert!(
        r["structuredContent"]["corpora"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["slug"] != "mini-closed"),
        "{r}"
    );

    // ── refused before anything runs ──
    for (name, args) in [
        ("xerj_hub_def", json!({"slug": "../etc", "symbol": "X"})),
        (
            "xerj_hub_def",
            json!({"slug": "mini-c", "symbol": "X", "k": 0}),
        ),
        ("xerj_hub_ask", json!({"question": "x".repeat(600)})),
        ("xerj_hub_ask", json!({"question": "q", "mode": "semantic"})),
        ("xerj_hub_find", json!({})),
    ] {
        let r = call(&url, name, args.clone()).await;
        assert_eq!(r["isError"], true, "{name} {args}: {r}");
    }

    // Nothing above wrote to the node: the four fixture indices, no more.
    let cat: Value = client
        .get(format!("{url}/_cat/indices/xc-*?format=json&h=index"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cat.as_array().unwrap().len(), 4, "{cat}");

    // ── a tool call never fetches the catalogue (#B, review of 9dffe9fcc) ──
    // A hub that accepts and counts connections. Before the fix, a stale or
    // missing cache sent `xerj_hub_find` and `xerj_hub_ask` to the network
    // (up to the 10 s/30 s fetch timeouts) from inside an agent's tool call.
    let hub_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hub_url = format!("http://{}", hub_listener.local_addr().unwrap());
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let hits = hits.clone();
        std::thread::spawn(move || {
            for conn in hub_listener.incoming() {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(conn);
            }
        });
    }
    std::env::set_var("XERJ_HUB_URL", &hub_url);
    let requests = || hits.load(std::sync::atomic::Ordering::SeqCst);

    // A stale cache (older than the 24 h freshness window) is still served as is.
    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    std::fs::File::options()
        .write(true)
        .open(hub.join("catalogue.json"))
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();
    let r = call(
        &url,
        "xerj_hub_find",
        json!({"question": "what is the value of MINI_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["corpora"][0]["slug"], "mini-c");
    let r = call(
        &url,
        "xerj_hub_def",
        json!({"slug": "mini-c", "symbol": "MINI_LIMIT"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["hits"][0]["value"], "128");
    assert_eq!(
        requests(),
        0,
        "a stale cache made a tool call fetch the catalogue"
    );

    // No cache at all: every tool says what would help, and still fetches
    // nothing. With a hub key (the compiled-in one, here) that is `xerj hub
    // sync`; with no key (`XERJ_HUB_PUBKEY=none`, see xerj-hub's no_hub_key.rs)
    // sync cannot help, so the tools say the hub is not available in this build
    // instead of sending the agent round a loop.
    std::fs::remove_file(hub.join("catalogue.json")).unwrap();
    let wanted = if xerj_common::hub::hub_key_configured() {
        "xerj hub sync"
    } else {
        "not available in this build"
    };
    for (name, args) in [
        ("xerj_hub_find", json!({"question": "MINI_LIMIT"})),
        (
            "xerj_hub_ask",
            json!({"question": "what is the value of MINI_LIMIT"}),
        ),
        (
            "xerj_hub_def",
            json!({"slug": "mini-c", "symbol": "MINI_LIMIT"}),
        ),
    ] {
        let r = call(&url, name, args).await;
        assert_eq!(r["isError"], true, "{name}: {r}");
        assert!(text(&r).contains(wanted), "{name}: {}", text(&r));
    }
    assert_eq!(
        requests(),
        0,
        "a missing cache made a tool call fetch the catalogue"
    );
}
