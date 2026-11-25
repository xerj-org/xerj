//! `xerj mcp` with no node running answers a hub question end to end (#2760,
//! epic #2769), and its acceptance criteria one by one:
//!
//! 1. With no node running, the server lists the hub tools and answers a hub
//!    question: the signed catalogue and a real `.hubpack.tar.zst` (written by
//!    `xerj_hub::pack::write_pack`, signed with a throwaway hub key) come from a
//!    local HTTP server standing in for hub.xerj.org.
//! 2. The second call for the same corpus is served from the disk cache,
//!    measured two ways: the answer's `_meta` and the hub server's request
//!    counters. A third call is answered with the hub shut down.
//! 3. With a node answering, the call runs the `xerj ask` flow as before and the
//!    hub is never contacted.
//! 4. An unknown corpus, an unsupported pack format, an unsupported catalogue
//!    format and a tampered pack are errors, never an empty answer.
//!
//! One test, scenarios in order: they share process-global environment
//! (`XERJ_HUB_*`). It is its own test binary so that environment never races
//! another test's.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::Router;
use serde_json::{json, Value};
use xerj_autoindex::harvest::sign::{generate_keypair, sign_bytes};
use xerj_common::hub::{
    Catalogue, CatalogueEntry, DefRecord, LicenceClass, PackIndex, PackManifest, PackSource,
    CATALOGUE_FORMAT_VERSION, PACK_FORMAT_VERSION,
};
use xerj_hub::pack::{write_pack, PackFileEntry};

const META: &str = "xerj.org/hub_first";
const CATALOGUE_PATH: &str = "/api/v1/catalogue.json";
const LZ4_PIN: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";
const FUTURE_PIN: &str = "1111111111111111111111111111111111111111";
const TAMPERED_PIN: &str = "2222222222222222222222222222222222222222";

/// A stand-in for hub.xerj.org: serves fixed bytes by path and counts requests.
struct TestHub {
    url: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    server: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct HubState {
    files: Arc<HashMap<String, Vec<u8>>>,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

async fn serve_file(State(state): State<HubState>, uri: Uri) -> impl IntoResponse {
    let path = uri.path().to_string();
    *state.hits.lock().unwrap().entry(path.clone()).or_default() += 1;
    match state.files.get(&path) {
        Some(bytes) => (StatusCode::OK, bytes.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "no such file").into_response(),
    }
}

impl TestHub {
    /// Bind first so the files (whose catalogue names pack URLs) can be built
    /// from this hub's own address.
    async fn start(files: impl FnOnce(&str) -> HashMap<String, Vec<u8>>) -> Self {
        let listener = xerj_common::loopback_test::expect_loopback(
            tokio::net::TcpListener::bind("127.0.0.1:0").await,
        );
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let app = Router::new().fallback(serve_file).with_state(HubState {
            files: Arc::new(files(&url)),
            hits: hits.clone(),
        });
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        Self { url, hits, server }
    }

    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    fn total_hits(&self) -> usize {
        self.hits.lock().unwrap().values().sum()
    }

    /// Stop answering: any later request is refused at the connection.
    async fn shut_down(self) {
        self.server.abort();
        let _ = self.server.await;
    }
}

/// One corpus the hub serves.
struct PackSpec {
    slug: &'static str,
    pin: &'static str,
    /// `PackManifest.format_version` inside the archive.
    format_version: u32,
}

struct Built {
    entry: CatalogueEntry,
    archive: Vec<u8>,
    sig: String,
}

fn defs() -> Vec<DefRecord> {
    let def = |symbol: &str, kind: &str, file: &str, line: u32, value: Option<&str>| DefRecord {
        symbol: symbol.into(),
        kind: kind.into(),
        lang: "c".into(),
        file: file.into(),
        line,
        text: match value {
            Some(v) => format!("#define {symbol} {v}"),
            None => format!("int {symbol}(const char* src, char* dst, int n, int cap)"),
        },
        value: value.map(Into::into),
    };
    vec![
        def("LZ4_compress_default", "function", "lib/lz4.c", 700, None),
        def("LZ4_VERSION_RELEASE", "define", "lib/lz4.h", 133, Some("0")),
        def("LZ4_VERSION_MAJOR", "define", "lib/lz4.h", 131, Some("1")),
    ]
}

fn files() -> Vec<PackFileEntry> {
    ["lib/lz4.c", "lib/lz4.h", "programs/lz4cli.c"]
        .into_iter()
        .map(|path| PackFileEntry {
            path: path.into(),
            lang: "c".into(),
            bytes: 4096,
            licence: "BSD-2-Clause".into(),
        })
        .collect()
}

/// A real pack: what the hub's builder writes, signed with the test hub key.
fn build_pack(work: &Path, hub_url: &str, spec: &PackSpec, seed: &str) -> Built {
    let dir = work.join(spec.slug);
    let snapshot = dir.join("snapshot");
    std::fs::create_dir_all(snapshot.join("idx")).unwrap();
    std::fs::write(snapshot.join("manifest.json"), br#"{"snapshot":"fixture"}"#).unwrap();
    std::fs::write(snapshot.join("idx/segment_0.bin"), b"\x00\x01binary\xff").unwrap();
    let manifest = PackManifest {
        format_version: spec.format_version,
        slug: spec.slug.into(),
        pin_sha: spec.pin.into(),
        pin_date: "2026-11-20".into(),
        built_at: "2026-11-24T00:00:00Z".into(),
        builder_version: "xerj 1.0.0".into(),
        index_prefix: format!("xc-{}-b1", spec.slug),
        indices: vec![PackIndex {
            name: format!("xc-{}-b1-defs", spec.slug),
            docs: 3,
            bytes: 2048,
        }],
        snapshot_name: format!("{}-{}", spec.slug, &spec.pin[..8]),
        licence_spdx: "BSD-2-Clause".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        sources: vec![PackSource {
            repo: spec.slug.into(),
            url: format!("https://github.com/example/{}", spec.slug),
            sha: spec.pin.into(),
            licence: "BSD-2-Clause".into(),
        }],
        defs_count: 3,
        files_count: 3,
    };
    let file_name = format!("{}-{}.hubpack.tar.zst", spec.slug, &spec.pin[..8]);
    let archive_path = dir.join(&file_name);
    let (sha256, bytes) =
        write_pack(&archive_path, &manifest, &defs(), &files(), &snapshot, seed).unwrap();
    let pack_url = format!("{hub_url}/packs/{file_name}");
    let entry = CatalogueEntry {
        slug: spec.slug.into(),
        domain: "NET".into(),
        topics: vec!["compression".into()],
        ask_for: format!("fixture corpus {}", spec.slug),
        upstream: format!("example/{}", spec.slug),
        pin_sha: spec.pin.into(),
        pin_date: "2026-11-20".into(),
        refresh_cadence_days: 7,
        licence_spdx: "BSD-2-Clause".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        pack_bytes: bytes,
        sig_url: format!("{pack_url}.sig"),
        pack_url,
        pack_sha256: sha256,
        dependency_names: vec![],
        quality_grade: None,
        term_sketch: xerj_hub::sketch::build(
            ["LZ4_VERSION_RELEASE", "LZ4_compress_default", spec.slug].map(String::from),
        ),
    };
    Built {
        entry,
        archive: std::fs::read(&archive_path).unwrap(),
        sig: std::fs::read_to_string(format!("{}.sig", archive_path.display())).unwrap(),
    }
}

fn pack_path(entry: &CatalogueEntry) -> String {
    format!(
        "/packs/{}-{}.hubpack.tar.zst",
        entry.slug,
        &entry.pin_sha[..8]
    )
}

/// The signed catalogue and each pack, as the hub lays them out.
fn hub_files(
    catalogue: &Catalogue,
    format_version: u32,
    seed: &str,
    packs: &[(&Built, Vec<u8>)],
) -> HashMap<String, Vec<u8>> {
    let mut value = serde_json::to_value(catalogue).unwrap();
    value["format_version"] = json!(format_version);
    let bytes = serde_json::to_vec(&value).unwrap();
    let sig = sign_bytes(&bytes, seed).unwrap();
    let mut files = HashMap::from([
        (CATALOGUE_PATH.to_string(), bytes),
        (format!("{CATALOGUE_PATH}.sig"), sig.into_bytes()),
    ]);
    for (built, served) in packs {
        let path = pack_path(&built.entry);
        files.insert(format!("{path}.sig"), built.sig.clone().into_bytes());
        files.insert(path, served.clone());
    }
    files
}

async fn call(node_url: &str, name: &str, arguments: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": {"name": name, "arguments": arguments}});
    let resp = xerj_mcp::handle_message_for_test(node_url, None, msg)
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

fn meta(result: &Value) -> &Value {
    &result["_meta"][META]
}

fn cached_pack_files(cache: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(cache)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("pack-"))
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_tools_answer_with_no_node_and_the_second_call_is_served_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("hub-cache");
    std::env::set_var("XERJ_HUB_HOME", tmp.path().join("hub-home"));
    std::env::set_var("XERJ_CODE_HOME", tmp.path().join("code-home"));
    std::env::set_var("XERJ_HUB_CACHE_DIR", &cache);
    // The automatic corpus review would POST to the real hub.
    std::env::set_var("XERJ_DISABLE_FEEDBACK", "true");
    let (seed, public) = generate_keypair().unwrap();
    std::env::set_var("XERJ_HUB_PUBKEY", &public);

    // ── no node: a port nothing listens on ──
    let node_url = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", probe.local_addr().unwrap())
    };

    // ── the hub: three corpora, one with a pack format this build cannot read,
    // and one whose served bytes are not the bytes the catalogue pins ──
    let work = tmp.path().join("packs");
    let hub = TestHub::start(|url| {
        let lz4 = build_pack(
            &work,
            url,
            &PackSpec {
                slug: "lz4",
                pin: LZ4_PIN,
                format_version: PACK_FORMAT_VERSION,
            },
            &seed,
        );
        let future = build_pack(
            &work,
            url,
            &PackSpec {
                slug: "future",
                pin: FUTURE_PIN,
                format_version: PACK_FORMAT_VERSION + 1,
            },
            &seed,
        );
        let tampered = build_pack(
            &work,
            url,
            &PackSpec {
                slug: "tampered",
                pin: TAMPERED_PIN,
                format_version: PACK_FORMAT_VERSION,
            },
            &seed,
        );
        let mut flipped = tampered.archive.clone();
        *flipped.last_mut().unwrap() ^= 0xff;
        let catalogue = Catalogue {
            format_version: CATALOGUE_FORMAT_VERSION,
            generated_at: "2026-11-24T00:00:00Z".into(),
            corpora: vec![
                lz4.entry.clone(),
                future.entry.clone(),
                tampered.entry.clone(),
            ],
        };
        hub_files(
            &catalogue,
            CATALOGUE_FORMAT_VERSION,
            &seed,
            &[
                (&lz4, lz4.archive.clone()),
                (&future, future.archive.clone()),
                (&tampered, flipped),
            ],
        )
    })
    .await;
    std::env::set_var("XERJ_HUB_URL", &hub.url);
    let lz4_pack = format!("/packs/lz4-{}.hubpack.tar.zst", &LZ4_PIN[..8]);

    // ── AC1: no node, the server lists the hub tools ──
    let list = xerj_mcp::handle_message_for_test(
        &node_url,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await
    .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for tool in ["xerj_hub_find", "xerj_hub_def", "xerj_hub_ask"] {
        assert!(names.contains(&tool), "tools/list lacks {tool}: {names:?}");
    }
    assert_eq!(hub.total_hits(), 0, "listing tools touches no network");

    // ── AC1: ... and answers a hub question end to end ──
    let first = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "lz4", "symbol": "LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(first["isError"], false, "{first}");
    let answer = &first["structuredContent"];
    assert_eq!(answer["corpus"], "lz4");
    assert_eq!(answer["mode"], "def");
    assert_eq!(answer["pin_sha"], LZ4_PIN);
    assert_eq!(answer["hits"].as_array().unwrap().len(), 1, "{first}");
    assert_eq!(answer["hits"][0]["value"], "0");
    assert_eq!(answer["hits"][0]["file"], "lib/lz4.h");
    assert_eq!(answer["hits"][0]["line"], 133);
    assert_eq!(answer["answered_from"], "hosted", "fetched on this call");
    assert!(answer["age_days"].is_u64());
    // Exactly an Answer (deny_unknown_fields): the shape every surface renders.
    let _: xerj_common::hub::Answer = serde_json::from_value(answer.clone()).unwrap();
    let said = text(&first);
    assert!(said.contains("value: 0"), "{said}");
    assert!(said.contains("answered from: hosted hub"), "{said}");
    assert!(said.contains("no local xerj node reachable"), "{said}");
    assert!(said.contains("fetched corpus 'lz4'"), "{said}");
    let m = meta(&first);
    assert_eq!(m["runtime_mode"]["mode"], "hub_only_fallback");
    assert_eq!(m["cache_outcome"], "fetched_and_cached");
    assert_eq!(m["network_fetches"], 2, "the catalogue and the pack");
    assert_eq!(m["disk_hits"], 0);
    assert_eq!(m["freshness"]["state"], "fresh");
    assert_eq!(hub.hits(CATALOGUE_PATH), 1);
    assert_eq!(hub.hits(&lz4_pack), 1);
    assert_eq!(hub.hits(&format!("{lz4_pack}.sig")), 1);
    assert_eq!(
        cached_pack_files(&cache),
        [
            format!("pack-lz4-{}.hubpack.tar.zst", &LZ4_PIN[..8]),
            format!("pack-lz4-{}.hubpack.tar.zst.sig", &LZ4_PIN[..8]),
        ],
        "the archive and its signature, as the hub serves them"
    );

    // ── AC2: the second call for the same corpus is served from disk ──
    let second = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "lz4", "symbol": "LZ4_compress_default"}),
    )
    .await;
    assert_eq!(second["isError"], false, "{second}");
    assert_eq!(second["structuredContent"]["hits"][0]["line"], 700);
    assert_eq!(second["structuredContent"]["answered_from"], "local_pack");
    assert!(
        text(&second).contains("answered from: local pack"),
        "{}",
        text(&second)
    );
    let m = meta(&second);
    assert_eq!(m["cache_outcome"], "hit_from_disk");
    assert_eq!(m["network_fetches"], 0);
    assert_eq!(m["disk_hits"], 2, "the catalogue and the pack");
    assert_eq!(
        hub.hits(CATALOGUE_PATH),
        1,
        "the catalogue was not re-fetched"
    );
    assert_eq!(hub.hits(&lz4_pack), 1, "the pack was not re-fetched");

    // The same cache answers the other two tools.
    let files = call(
        &node_url,
        "xerj_hub_ask",
        json!({"question": "lib/", "corpus": "lz4", "mode": "files"}),
    )
    .await;
    assert_eq!(files["isError"], false, "{files}");
    assert_eq!(files["structuredContent"]["mode"], "files");
    let listed: Vec<&str> = files["structuredContent"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["file"].as_str())
        .collect();
    assert_eq!(
        listed,
        ["lib/lz4.c", "lib/lz4.h"],
        "filtered by path fragment"
    );
    assert!(text(&files).contains("path fragment"), "{}", text(&files));
    assert_eq!(meta(&files)["cache_outcome"], "hit_from_disk");
    let asked = call(
        &node_url,
        "xerj_hub_ask",
        json!({"question": "what is the value of LZ4_VERSION_MAJOR", "corpus": "lz4"}),
    )
    .await;
    assert_eq!(asked["isError"], false, "{asked}");
    assert_eq!(asked["structuredContent"]["hits"][0]["value"], "1");
    let found = call(
        &node_url,
        "xerj_hub_find",
        json!({"question": "what is the value of LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(found["isError"], false, "{found}");
    let corpora = found["structuredContent"]["corpora"].as_array().unwrap();
    assert!(
        corpora
            .iter()
            .any(|c| c["slug"] == "lz4" && c["cached"] == true),
        "{found}"
    );
    assert_eq!(meta(&found)["cache_outcome"], "hit_from_disk");
    assert_eq!(hub.hits(CATALOGUE_PATH), 1);
    assert_eq!(hub.hits(&lz4_pack), 1);

    // ── AC4: loud errors, never an empty result ──
    let unknown = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "no-such-corpus", "symbol": "X"}),
    )
    .await;
    assert_eq!(unknown["isError"], true, "{unknown}");
    assert!(unknown.get("structuredContent").is_none(), "{unknown}");
    assert!(
        text(&unknown).contains("unknown hub corpus 'no-such-corpus'"),
        "{}",
        text(&unknown)
    );
    assert!(
        text(&unknown).contains("xerj_hub_find"),
        "{}",
        text(&unknown)
    );
    let future = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "future", "symbol": "LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(future["isError"], true, "{future}");
    assert!(
        text(&future).contains("unsupported pack_manifest format_version 2"),
        "{}",
        text(&future)
    );
    let tampered = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "tampered", "symbol": "LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(tampered["isError"], true, "{tampered}");
    assert!(
        text(&tampered).contains("sha256 mismatch"),
        "{}",
        text(&tampered)
    );
    assert_eq!(
        cached_pack_files(&cache).len(),
        2,
        "a refused pack leaves nothing in the cache"
    );
    // A passage question needs a node; with no corpus synced the flow says so.
    let how = call(
        &node_url,
        "xerj_hub_ask",
        json!({"question": "how does block compression work", "corpus": "lz4", "mode": "how"}),
    )
    .await;
    assert_eq!(how["isError"], true, "{how}");
    assert!(how.get("_meta").is_none(), "{how}");
    // No corpus named and no catalogue synced: say what to do, not "run xerj hub sync".
    let unrouted = call(
        &node_url,
        "xerj_hub_ask",
        json!({"question": "what is LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(unrouted["isError"], true, "{unrouted}");
    assert!(
        text(&unrouted).contains("xerj_hub_find"),
        "{}",
        text(&unrouted)
    );

    // ── AC2, hardest form: the hub is gone, the cache still answers ──
    let before = (hub.hits(CATALOGUE_PATH), hub.hits(&lz4_pack));
    let hub_url = hub.url.clone();
    hub.shut_down().await;
    let offline = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "lz4", "symbol": "LZ4_VERSION"}),
    )
    .await;
    assert_eq!(offline["isError"], false, "{offline}");
    let symbols: Vec<&str> = offline["structuredContent"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["text"].as_str())
        .collect();
    assert_eq!(symbols.len(), 2, "{offline}");
    assert_eq!(meta(&offline)["cache_outcome"], "hit_from_disk");
    assert_eq!(meta(&offline)["network_fetches"], 0);
    assert_eq!(before, (1, 1));
    // A corpus the cache does not hold cannot be fetched from a dead hub: an
    // error that names the hub, not an empty answer.
    let fresh_cache = tmp.path().join("fresh-cache");
    std::env::set_var("XERJ_HUB_CACHE_DIR", &fresh_cache);
    let dead = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "lz4", "symbol": "LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(dead["isError"], true, "{dead}");
    assert!(text(&dead).contains("hub fetch failed"), "{}", text(&dead));
    assert!(text(&dead).contains(&hub_url), "{}", text(&dead));

    // ── AC4: a catalogue format this build cannot read ──
    let newer = TestHub::start(|url| {
        let lz4 = build_pack(
            &work,
            url,
            &PackSpec {
                slug: "lz4",
                pin: LZ4_PIN,
                format_version: PACK_FORMAT_VERSION,
            },
            &seed,
        );
        let catalogue = Catalogue {
            format_version: CATALOGUE_FORMAT_VERSION,
            generated_at: "2026-11-24T00:00:00Z".into(),
            corpora: vec![lz4.entry.clone()],
        };
        hub_files(
            &catalogue,
            CATALOGUE_FORMAT_VERSION + 1,
            &seed,
            &[(&lz4, lz4.archive.clone())],
        )
    })
    .await;
    std::env::set_var("XERJ_HUB_URL", &newer.url);
    std::env::set_var("XERJ_HUB_CACHE_DIR", tmp.path().join("newer-cache"));
    let r = call(
        &node_url,
        "xerj_hub_def",
        json!({"slug": "lz4", "symbol": "LZ4_VERSION_RELEASE"}),
    )
    .await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        text(&r).contains("unsupported catalogue format_version 3"),
        "{}",
        text(&r)
    );
    assert_eq!(newer.hits(&lz4_pack), 0, "no pack is fetched from it");

    // ── AC3: a node that answers is used as before, and the hub is not asked ──
    let node = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let with_node = format!("http://{}", node.local_addr().unwrap());
    std::env::set_var("XERJ_HUB_CACHE_DIR", tmp.path().join("third-cache"));
    let requests_before = newer.total_hits();
    let r = call(
        &with_node,
        "xerj_hub_def",
        json!({"slug": "lz4", "symbol": "LZ4_VERSION_RELEASE"}),
    )
    .await;
    // The `xerj ask` flow, which reads only a synced catalogue: none here.
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("xerj hub sync"), "{}", text(&r));
    assert!(r.get("_meta").is_none(), "hub-first did not run: {r}");
    assert_eq!(
        newer.total_hits(),
        requests_before,
        "with a node answering, the hub is not contacted"
    );
    drop(node);
    newer.shut_down().await;
}
