//! `xerj ask` end to end (plan §5 "Tests").
//!
//! Real nodes, real packs, real pulls — the pieces `src/ask.rs`'s unit tests
//! fake. One `#[tokio::test]` walks the scenarios in order because they share
//! process-global environment (`XERJ_HUB_HOME`, `XERJ_CODE_HOME`,
//! `XERJ_HUB_PUBKEY`, `HOME`):
//!
//! - **A. first question, no hosted hub:** `ask` pulls the pack (sha256 and
//!   signature verified, snapshot restored into a node) and answers
//!   `--def MINI_LIMIT` from it — value `128`, "answered from: local pack". A
//!   second question is answered from the cache with the pack's source file
//!   deleted: it cannot have downloaded anything. How and Files answers come off
//!   the same pack.
//! - **B. hosted first, pull behind it:** a hosted stub answers at once, a
//!   detached `xerj hub pull` is started (PID file left under `pulls/`), it
//!   finishes on its own, and the next `ask` answers `LocalPack` with the hosted
//!   hub forbidden.
//! - **C. the automatic node:** with nothing on the requested loopback port,
//!   `ask` starts a node on port ≥ 9300 (flags, config and allowlist exactly as
//!   `xerj` would), pulls into it, answers, and `hub stop` stops exactly the
//!   recorded PID — a second process of the very same executable survives.
//! - **D. the disk budget:** a pull evicts a least-recently-used pack that is
//!   over the 4 GiB budget (sparse files, so nothing is written).
//! - **E. offline with no pack:** exit 1, and no node is started to say so.
//! - **G. an excluded corpus:** never pulled, and never hinted at — `--offline` and
//!   `--no-fetch` used to print `run: xerj hub pull <slug>` for it, and `hub pull`
//!   itself had no licence check.
//! - **F. a pull killed mid-run:** a second process running `xerj hub pull` is
//!   blocked mid-download holding the pull lock, killed with SIGKILL, and the retry
//!   is not blocked (the lock was a directory that only the pull's destructor
//!   removed, so a killed pull wedged its corpus for half an hour).
//!
//! The test binary doubles as the `xerj` executable the flow spawns: a
//! `NodeLauncher` points at it with `--ignored --exact child_main --nocapture --`
//! in front, and [`child_main`] turns what follows the `--` into either
//! `xerj hub …` (run in-process), a real engine node (`--insecure --port …`), or
//! a decoy that just sleeps. That keeps the spawned code paths identical to
//! production (`current_exe() <args>`) without needing the `xerj` binary.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};

use xerj_autoindex::harvest::sign::generate_keypair;
use xerj_hub::ask::{
    ask, AskArgs, AskDeps, AskOutcome, HostedHub, ModeRequest, RealBackend, Request,
    EXIT_NO_ANSWER, EXIT_OK,
};
use xerj_hub::catalogue::load_catalogue;
#[cfg(unix)]
use xerj_hub::node::StopOutcome;
use xerj_hub::node::{self, HubNode, NodeLauncher};
use xerj_hub::pack::{self, PackFileEntry};
use xerj_hub::router::HostedSearch;
use xerj_hub::{
    Answer, AnswerMode, AnsweredFrom, Catalogue, CatalogueEntry, DefRecord, Hit, LicenceClass,
    PackIndex, PackManifest, PackSource, Sketch, CATALOGUE_FORMAT_VERSION, PACK_FORMAT_VERSION,
};

const PIN_SHA: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";
const PIN8: &str = "0774d055";
const PIN_DATE: &str = "2026-10-02";
const PREFIX: &str = "xc-mini-c-b1";

// ── the child process: this binary standing in for `xerj` ────────────────────

/// Not a test. The scenarios start this binary with
/// `--ignored --exact child_main --nocapture -- <xerj arguments>`; libtest hands
/// everything after the `--` to us as plain arguments.
#[test]
#[ignore = "child-process entry point, started by the scenarios in this file"]
fn child_main() {
    let args: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    match args.first().map(String::as_str) {
        // `xerj hub pull …` — what the background pull runs.
        Some("hub") => std::process::exit(xerj_hub::cli::run_hub_cli(&args[1..])),
        // A second process of the same executable, to prove `hub stop` is exact.
        Some("decoy") => std::thread::sleep(Duration::from_secs(300)),
        // `--insecure --bind 127.0.0.1 --port P --data-dir D --config C` — a node.
        _ => node_main(&args),
    }
}

/// Boot an ES-compatible engine node on the port/data dir/config named by the
/// flags `xerj hub` passes, and serve until killed. The config comes from the
/// real `Config::load`, so a malformed `node.toml` or a missing snapshot
/// allowlist fails here exactly as it would in the real server.
fn node_main(args: &[String]) {
    let (mut port, mut data_dir, mut config) = (0u16, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => port = it.next().and_then(|p| p.parse().ok()).expect("--port N"),
            "--data-dir" => data_dir = it.next().cloned(),
            "--config" => config = it.next().cloned(),
            _ => {}
        }
    }
    let mut cfg = xerj_common::config::Config::load(config.expect("--config <file>"))
        .expect("node config loads");
    cfg.server.data_dir = data_dir.expect("--data-dir <dir>");
    cfg.auth.enabled = false;
    cfg.tls.enabled = false;
    cfg.storage.wal_sync = xerj_common::config::WalSync::Async;
    std::fs::create_dir_all(&cfg.server.data_dir).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let metrics = xerj_common::metrics::Metrics::new().unwrap();
        let engine = xerj_engine::Engine::new(cfg.clone()).unwrap();
        let state = xerj_api::state::AppState::new(cfg, engine, metrics);
        let app = xerj_api::router::build_es_compat_router(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind the node port");
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
}

/// The test binary as a `xerj` the flow can spawn.
fn test_exe_launcher() -> NodeLauncher {
    NodeLauncher {
        exe: std::env::current_exe().unwrap(),
        pre_args: ["--ignored", "--exact", "child_main", "--nocapture", "--"]
            .map(String::from)
            .to_vec(),
    }
}

/// A launcher that fails loudly if the flow ever tries to start anything.
fn forbidden_launcher() -> NodeLauncher {
    NodeLauncher {
        exe: PathBuf::from("/nonexistent/xerj-must-not-be-spawned"),
        pre_args: vec![],
    }
}

// ── hosted stand-ins ─────────────────────────────────────────────────────────

/// Answers a `MINI_LIMIT` definition lookup at once.
struct CannedHosted;

impl HostedSearch for CannedHosted {
    fn search(&self, _q: &str) -> anyhow::Result<Vec<(String, f32)>> {
        Ok(vec![])
    }
}

impl HostedHub for CannedHosted {
    fn answer(&self, _req: &Request<'_>) -> anyhow::Result<Answer> {
        Ok(Answer {
            corpus: "mini-c".to_string(),
            pin_sha: PIN_SHA.to_string(),
            pin_date: PIN_DATE.to_string(),
            age_days: 12,
            licence_spdx: "MIT".to_string(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: AnsweredFrom::Hosted,
            mode: AnswerMode::Def,
            hits: vec![Hit {
                file: "limits.h".to_string(),
                line: 5,
                kind: "define".to_string(),
                text: "#define MINI_LIMIT 128".to_string(),
                value: Some("128".to_string()),
                score: 1.0,
                licence_warning: None,
            }],
            truncated: false,
            warnings: vec![],
        })
    }
}

/// A hosted hub that panics if it is contacted.
struct ForbiddenHosted;

impl HostedSearch for ForbiddenHosted {
    fn search(&self, _q: &str) -> anyhow::Result<Vec<(String, f32)>> {
        panic!("the hosted hub must not be contacted");
    }
}

impl HostedHub for ForbiddenHosted {
    fn answer(&self, _req: &Request<'_>) -> anyhow::Result<Answer> {
        panic!("the hosted hub must not be contacted");
    }
}

/// Stops what scenario C starts even when an assertion fails halfway, so a red
/// run cannot leave a node or a decoy behind. It signals only the PID recorded
/// in the hub's `node.pid` (the same rule as `hub stop`) and the decoy it holds.
#[cfg(unix)]
struct Reaper {
    hub: PathBuf,
    decoy: Option<std::process::Child>,
}

#[cfg(unix)]
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = node::stop_node(&self.hub);
        if let Some(mut d) = self.decoy.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
    }
}

#[derive(Clone, Copy)]
enum HostedKind {
    None,
    Canned,
    Forbidden,
}

// ── fixtures (the same mini-c pack `pack_roundtrip.rs` builds) ───────────────

/// Boot an es-compat node on an ephemeral port with `data_dir` and the given
/// snapshot-repo allowlist, and wait until it answers.
async fn spawn_node(data_dir: &Path, allowlist: Vec<String>) -> String {
    std::fs::create_dir_all(data_dir).unwrap();
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.limits.snapshot_repo_allowlist = allowlist;
    let metrics = xerj_common::metrics::Metrics::new().unwrap();
    let engine = xerj_engine::Engine::new(config.clone()).unwrap();
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    let url = format!("http://127.0.0.1:{port}");
    let probe = url.clone();
    tokio::task::spawn_blocking(move || {
        HubNode::connect_insecure(&probe)
            .unwrap()
            .wait_until_reachable(Duration::from_secs(30))
            .unwrap();
    })
    .await
    .unwrap();
    url
}

fn sample_defs() -> Vec<DefRecord> {
    vec![
        DefRecord {
            symbol: "MINI_LIMIT".into(),
            kind: "define".into(),
            lang: "c".into(),
            file: "limits.h".into(),
            line: 5,
            text: "#define MINI_LIMIT 128".into(),
            value: Some("128".into()),
        },
        DefRecord {
            symbol: "MINI_HASH_SEED".into(),
            kind: "define".into(),
            lang: "c".into(),
            file: "hash.c".into(),
            line: 3,
            text: "#define MINI_HASH_SEED 5381".into(),
            value: Some("5381".into()),
        },
    ]
}

fn manifest() -> PackManifest {
    PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: "mini-c".into(),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        built_at: "2026-10-14T00:00:00Z".into(),
        builder_version: "xerj test".into(),
        index_prefix: PREFIX.into(),
        indices: vec![
            PackIndex {
                name: format!("{PREFIX}-defs"),
                docs: 2,
                bytes: 0,
            },
            PackIndex {
                name: format!("{PREFIX}-docs"),
                docs: 2,
                bytes: 0,
            },
        ],
        snapshot_name: format!("mini-c-{PIN8}"),
        licence_spdx: "MIT".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        sources: vec![PackSource {
            repo: "mini-c".into(),
            url: "file:///fixtures/mini-c".into(),
            sha: PIN_SHA.into(),
            licence: "MIT".into(),
        }],
        defs_count: 2,
        files_count: 2,
    }
}

/// Create the docs + defs indices on `url`, bulk them, and snapshot
/// `<prefix>-*` into `location`. The docs records carry `ax_path` / `line` /
/// `symbols` the way the autoindex code family does, so How and Files answers
/// have something real to rank and cite.
fn seed_and_snapshot(url: &str, location: &Path) {
    let node = HubNode::connect_insecure(url).unwrap();
    node.request_ok(
        "PUT",
        &format!("/{PREFIX}-docs"),
        Some(&json!({"mappings":{"properties":{
            "body":{"type":"text"},
            "ax_path":{"type":"keyword"},
            "line":{"type":"integer"}
        }}})),
    )
    .unwrap();
    let docs = [
        json!({
            "ax_path": "mini-c/limits.h",
            "body": "/* the clamp ceiling */\n#define MINI_LIMIT 128\n",
            "symbols": [{"name": "MINI_LIMIT", "kind": "define", "line": 2}]
        }),
        json!({
            "ax_path": "mini-c/hash.c", "line": 1,
            "body": "unsigned mini_hash(const char *s) { return MINI_HASH_SEED; }\n"
        }),
    ];
    let mut ndjson = String::new();
    for d in &docs {
        ndjson.push_str("{\"index\":{}}\n");
        ndjson.push_str(&d.to_string());
        ndjson.push('\n');
    }
    node.bulk_ndjson(&format!("/{PREFIX}-docs/_bulk"), ndjson)
        .unwrap();

    node.request_ok(
        "PUT",
        &format!("/{PREFIX}-defs"),
        Some(&json!({"mappings":{"properties":{
            "symbol":{"type":"keyword"},
            "symbol_text":{"type":"text"},
            "kind":{"type":"keyword"},
            "lang":{"type":"keyword"},
            "file":{"type":"keyword"},
            "line":{"type":"integer"},
            "text":{"type":"text"},
            "value":{"type":"keyword"}
        }}})),
    )
    .unwrap();
    let mut ndjson = String::new();
    for d in sample_defs() {
        ndjson.push_str("{\"index\":{}}\n");
        ndjson.push_str(
            &json!({
                "symbol": d.symbol, "symbol_text": d.symbol, "kind": d.kind,
                "lang": d.lang, "file": d.file, "line": d.line, "text": d.text, "value": d.value,
            })
            .to_string(),
        );
        ndjson.push('\n');
    }
    node.bulk_ndjson(&format!("/{PREFIX}-defs/_bulk"), ndjson)
        .unwrap();
    node.request_ok("POST", &format!("/{PREFIX}-defs/_refresh"), None)
        .ok();
    node.request_ok("POST", &format!("/{PREFIX}-docs/_refresh"), None)
        .ok();

    node.request_ok(
        "PUT",
        "/_snapshot/hub-build",
        Some(&json!({"type":"fs","settings":{"location": location.to_string_lossy()}})),
    )
    .unwrap();
    node.request_ok(
        "PUT",
        &format!("/_snapshot/hub-build/mini-c-{PIN8}?wait_for_completion=true"),
        Some(&json!({"indices": format!("{PREFIX}-*")})),
    )
    .unwrap();
}

fn sha256_hex(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A scenario's own copy of the pack, "published" at a `file://` URL, and the
/// catalogue that advertises it, written into `hub_home`.
struct Published {
    archive: PathBuf,
    sig: PathBuf,
}

fn publish(src_archive: &Path, src_sig: &Path, dir: &Path, hub_home: &Path) -> Published {
    std::fs::create_dir_all(dir).unwrap();
    let archive = dir.join(format!("mini-c-{PIN8}.hubpack.tar.zst"));
    let sig = dir.join(format!("mini-c-{PIN8}.hubpack.tar.zst.sig"));
    std::fs::copy(src_archive, &archive).unwrap();
    std::fs::copy(src_sig, &sig).unwrap();
    let entry = CatalogueEntry {
        slug: "mini-c".into(),
        domain: "DEV".into(),
        topics: vec!["c".into()],
        ask_for: "a tiny C fixture corpus".into(),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        refresh_cadence_days: 7,
        licence_spdx: "MIT".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        pack_bytes: std::fs::metadata(&archive).unwrap().len(),
        pack_url: format!("file://{}", archive.display()),
        pack_sha256: sha256_hex(&archive),
        sig_url: format!("file://{}", sig.display()),
        dependency_names: vec![],
        quality_grade: None,
        term_sketch: Sketch {
            m_bits: 64,
            k_hashes: 7,
            bits_base64: "AAAAAAAAAAA=".into(),
        },
    };
    let cat = Catalogue {
        format_version: CATALOGUE_FORMAT_VERSION,
        generated_at: "2026-10-14T00:00:00Z".into(),
        corpora: vec![entry],
    };
    std::fs::create_dir_all(hub_home).unwrap();
    std::fs::write(
        hub_home.join("catalogue.json"),
        serde_json::to_vec(&cat).unwrap(),
    )
    .unwrap();
    Published { archive, sig }
}

/// Point the process-global environment at one scenario's directories. `HOME` is
/// a scratch directory so no developer-local admin key is ever discovered.
fn set_env(hub: &Path, code: &Path, home: &Path, public: &str) {
    std::env::set_var("XERJ_HUB_HOME", hub);
    std::env::set_var("XERJ_CODE_HOME", code);
    std::env::set_var("XERJ_HUB_PUBKEY", public);
    std::env::set_var("HOME", home);
    std::env::remove_var("XERJ_URL");
    std::env::remove_var("XERJ_API_KEY");
}

struct Case {
    hub: PathBuf,
    cwd: PathBuf,
    url: Option<String>,
    launcher: NodeLauncher,
    hosted: HostedKind,
    args: AskArgs,
}

/// Run one `ask` on a blocking thread (the in-process nodes need the runtime's
/// workers free to answer the blocking HTTP calls).
async fn run(case: Case) -> AskOutcome {
    tokio::task::spawn_blocking(move || {
        let catalogue = load_catalogue("https://hub.invalid", &case.hub, true).expect("catalogue");
        let backend = RealBackend::new(case.hub.clone(), case.url.clone(), case.launcher.clone());
        let hosted: Option<Box<dyn HostedHub>> = match case.hosted {
            HostedKind::None => None,
            HostedKind::Canned => Some(Box::new(CannedHosted)),
            HostedKind::Forbidden => Some(Box::new(ForbiddenHosted)),
        };
        ask(
            &case.args,
            &AskDeps {
                hub_home: &case.hub,
                cwd: &case.cwd,
                catalogue: &catalogue,
                hosted: hosted.as_deref(),
                backend: &backend,
            },
        )
    })
    .await
    .unwrap()
}

fn def_args() -> AskArgs {
    let mut a = AskArgs::new("MINI_LIMIT");
    a.corpus = Some("mini-c".to_string());
    a.mode = ModeRequest::Def("MINI_LIMIT".to_string());
    a
}

/// `node::probe` builds a blocking HTTP client, which must not live on an async
/// worker thread.
async fn probe(url: String, timeout: Duration) -> Option<node::Health> {
    tokio::task::spawn_blocking(move || node::probe(&url, timeout))
        .await
        .unwrap()
}

async fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// ── the scenarios ────────────────────────────────────────────────────────────

// The process environment is global: this must stay the only test here that sets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_end_to_end() {
    let work_tmp = tempfile::tempdir().unwrap();
    let work = std::fs::canonicalize(work_tmp.path()).unwrap();
    let allowlist = vec![work.to_string_lossy().into_owned()];
    let scratch_home = work.join("home");
    std::fs::create_dir_all(&scratch_home).unwrap();
    let cwd = work.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let (seed, public) = generate_keypair().unwrap();

    // Build the pack once, on a builder node.
    let out = work.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let url_a = spawn_node(&work.join("data-a"), allowlist.clone()).await;
    let location = out.join("mini-c").join("snapshot");
    {
        let (url_a, location) = (url_a.clone(), location.clone());
        tokio::task::spawn_blocking(move || seed_and_snapshot(&url_a, &location))
            .await
            .unwrap();
    }
    let archive = out.join(format!("mini-c-{PIN8}.hubpack.tar.zst"));
    {
        let files = vec![
            PackFileEntry {
                path: "limits.h".into(),
                lang: "c".into(),
                bytes: 300,
                licence: "MIT".into(),
            },
            PackFileEntry {
                path: "hash.c".into(),
                lang: "c".into(),
                bytes: 200,
                licence: "MIT".into(),
            },
        ];
        let (archive, location, seed) = (archive.clone(), location.clone(), seed.clone());
        tokio::task::spawn_blocking(move || {
            pack::write_pack(
                &archive,
                &manifest(),
                &sample_defs(),
                &files,
                &location,
                &seed,
            )
            .unwrap();
        })
        .await
        .unwrap();
    }
    let sig = out.join(format!("mini-c-{PIN8}.hubpack.tar.zst.sig"));
    assert!(
        archive.exists() && sig.exists(),
        "pack and signature written"
    );

    // ── A. no hosted hub: pull on first use, then answer from the cache ──────
    {
        let hub = work.join("hub-a");
        let code = work.join("code-a");
        let published = publish(&archive, &sig, &work.join("net-a"), &hub);
        set_env(&hub, &code, &scratch_home, &public);
        let url_b = spawn_node(&work.join("data-b"), allowlist.clone()).await;
        let case = |hosted, args| Case {
            hub: hub.clone(),
            cwd: cwd.clone(),
            url: Some(url_b.clone()),
            launcher: forbidden_launcher(),
            hosted,
            args,
        };

        let first = run(case(HostedKind::None, def_args())).await;
        assert_eq!(first.exit, EXIT_OK, "stderr: {:?}", first.stderr);
        assert!(first.stdout.contains("value: 128"), "{}", first.stdout);
        assert!(
            first.stdout.contains("answered from: local pack"),
            "{}",
            first.stdout
        );
        assert!(
            first.stdout.starts_with("mini-c  pin 2026-10-02 ("),
            "{}",
            first.stdout
        );
        let pack_dir = hub.join("packs").join("mini-c").join(PIN8);
        assert!(pack_dir.join("pack.json").is_file(), "the pack is cached");
        assert!(
            pack_dir.join("last_used").is_file(),
            "every answer touches last_used"
        );
        assert!(
            code.join("state").join("mini-c.json").is_file(),
            "the ledger is written"
        );

        // The "network" goes away: a second question cannot be pulling anything,
        // and the hosted hub must not be asked either.
        std::fs::remove_file(&published.archive).unwrap();
        std::fs::remove_file(&published.sig).unwrap();
        let again = run(case(HostedKind::Forbidden, def_args())).await;
        assert_eq!(again.exit, EXIT_OK, "stderr: {:?}", again.stderr);
        assert_eq!(
            again.answer.as_ref().unwrap().answered_from,
            AnsweredFrom::LocalPack
        );
        assert!(again.stdout.contains("value: 128"));

        // A how-does-it-work question and a file listing come off the same pack.
        let mut how = AskArgs::new("MINI_LIMIT clamp ceiling");
        how.corpus = Some("mini-c".to_string());
        how.mode = ModeRequest::How;
        let how = run(case(HostedKind::Forbidden, how)).await;
        assert_eq!(
            how.exit, EXIT_OK,
            "stderr: {:?}\nstdout: {}",
            how.stderr, how.stdout
        );
        let ans = how.answer.unwrap();
        assert_eq!(ans.mode, AnswerMode::How);
        assert_eq!(ans.hits[0].file, "mini-c/limits.h");
        assert_eq!(ans.hits[0].kind, "define", "the matching definition");
        assert_eq!(ans.hits[0].line, 2, "the definition's line in the record");
        assert!(ans.hits[0].text.contains("MINI_LIMIT"));

        let mut files = AskArgs::new("unsigned return");
        files.corpus = Some("mini-c".to_string());
        files.mode = ModeRequest::Files;
        let files = run(case(HostedKind::Forbidden, files)).await;
        assert_eq!(files.exit, EXIT_OK, "stderr: {:?}", files.stderr);
        let ans = files.answer.unwrap();
        assert_eq!(ans.mode, AnswerMode::Files);
        assert!(ans
            .hits
            .iter()
            .all(|h| h.kind == "file" && h.text == "licence: MIT"));
        assert!(
            ans.hits.iter().any(|h| h.file == "mini-c/hash.c"),
            "{:?}",
            ans.hits
        );

        // `--max-tokens` is honoured on a real answer: the first hit is kept,
        // its text shortened, and the answer says it was cut.
        let mut capped = def_args();
        capped.max_tokens = Some(10);
        let capped = run(case(HostedKind::None, capped)).await;
        assert_eq!(capped.exit, EXIT_OK, "stderr: {:?}", capped.stderr);
        let ans = capped.answer.unwrap();
        assert!(ans.truncated, "{ans:?}");
        assert!(ans.hits[0].text.ends_with('…'), "{:?}", ans.hits[0].text);
    }

    // ── B. hosted answers now, a detached pull completes behind it ───────────
    {
        let hub = work.join("hub-b");
        let code = work.join("code-b");
        publish(&archive, &sig, &work.join("net-b"), &hub);
        set_env(&hub, &code, &scratch_home, &public);
        let url = spawn_node(&work.join("data-b2"), allowlist.clone()).await;
        let case = |hosted, launcher| Case {
            hub: hub.clone(),
            cwd: cwd.clone(),
            url: Some(url.clone()),
            launcher,
            hosted,
            args: def_args(),
        };

        let first = run(case(HostedKind::Canned, test_exe_launcher())).await;
        assert_eq!(first.exit, EXIT_OK, "stderr: {:?}", first.stderr);
        assert_eq!(
            first.answer.as_ref().unwrap().answered_from,
            AnsweredFrom::Hosted
        );
        assert!(
            first.stdout.contains("answered from: hosted hub"),
            "{}",
            first.stdout
        );
        assert!(
            first.stderr.iter().any(|l| l.contains("background")),
            "{:?}",
            first.stderr
        );
        assert!(
            hub.join("pulls").join("mini-c.pid").is_file(),
            "the pull's PID is recorded"
        );
        // The pull finishes on its own: the ledger is written last.
        let ledger = code.join("state").join("mini-c.json");
        wait_for("the background pull", Duration::from_secs(90), || {
            ledger.is_file()
        })
        .await;
        assert!(
            hub.join("packs")
                .join("mini-c")
                .join(PIN8)
                .join("pack.json")
                .is_file(),
            "log: {}",
            std::fs::read_to_string(hub.join("pulls").join("mini-c.log")).unwrap_or_default()
        );

        let second = run(case(HostedKind::Forbidden, forbidden_launcher())).await;
        assert_eq!(second.exit, EXIT_OK, "stderr: {:?}", second.stderr);
        assert_eq!(
            second.answer.unwrap().answered_from,
            AnsweredFrom::LocalPack
        );
    }

    // ── C. the automatic node: start, pull into it, answer, stop exactly it ──
    #[cfg(unix)]
    {
        let hub = work.join("hub-c");
        let code = work.join("code-c");
        publish(&archive, &sig, &work.join("net-c"), &hub);
        set_env(&hub, &code, &scratch_home, &public);
        // A loopback port with nothing on it: the requested node is "down".
        let silent = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let silent_url = format!("http://127.0.0.1:{silent}");
        // A second process of the same executable that is NOT the node.
        let decoy = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "child_main",
                "--nocapture",
                "--",
                "decoy",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let decoy_pid = decoy.id();
        let mut reaper = Reaper {
            hub: hub.clone(),
            decoy: Some(decoy),
        };

        let out = run(Case {
            hub: hub.clone(),
            cwd: cwd.clone(),
            url: Some(silent_url.clone()),
            launcher: test_exe_launcher(),
            hosted: HostedKind::None,
            args: def_args(),
        })
        .await;
        assert_eq!(out.exit, EXIT_OK, "stderr: {:?}", out.stderr);
        assert!(out.stdout.contains("value: 128"), "{}", out.stdout);

        let pid: u32 = std::fs::read_to_string(hub.join("node.pid"))
            .expect("node.pid is recorded")
            .trim()
            .parse()
            .unwrap();
        let port: u16 = std::fs::read_to_string(hub.join("node.port"))
            .expect("node.port is recorded")
            .trim()
            .parse()
            .unwrap();
        assert!(port >= 9300, "auto node port {port} must be >= 9300");
        assert_ne!(pid, decoy_pid);
        assert!(
            probe(format!("http://127.0.0.1:{port}"), Duration::from_secs(2))
                .await
                .is_some(),
            "the recorded node answers"
        );
        // Its flags and config are what `ask` promised.
        let cfg = std::fs::read_to_string(hub.join("node.toml")).unwrap();
        assert!(cfg.contains("snapshot_repo_allowlist"), "{cfg}");
        assert!(
            cfg.contains(&hub.join("packs").to_string_lossy().into_owned()),
            "{cfg}"
        );

        // A second question finds the recorded node and starts nothing.
        let again = run(Case {
            hub: hub.clone(),
            cwd: cwd.clone(),
            url: Some(silent_url),
            launcher: forbidden_launcher(),
            hosted: HostedKind::Forbidden,
            args: def_args(),
        })
        .await;
        assert_eq!(again.exit, EXIT_OK, "stderr: {:?}", again.stderr);
        assert_eq!(
            std::fs::read_to_string(hub.join("node.pid"))
                .unwrap()
                .trim(),
            pid.to_string(),
            "the same node, not a new one"
        );

        // `hub stop` stops exactly the recorded PID.
        let hub_stop = hub.clone();
        let outcome = tokio::task::spawn_blocking(move || node::stop_node(&hub_stop))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome, StopOutcome::Stopped { pid });
        assert!(
            probe(format!("http://127.0.0.1:{port}"), Duration::from_secs(1))
                .await
                .is_none(),
            "the node is gone"
        );
        let decoy = reaper.decoy.as_mut().unwrap();
        assert!(
            decoy.try_wait().unwrap().is_none(),
            "the look-alike process survives"
        );
        assert!(!hub.join("node.pid").exists() && !hub.join("node.port").exists());
    }

    // ── D. a pull evicts the least recently used pack that busts the budget ──
    #[cfg(unix)]
    {
        let hub = work.join("hub-d");
        let code = work.join("code-d");
        publish(&archive, &sig, &work.join("net-d"), &hub);
        set_env(&hub, &code, &scratch_home, &public);
        // Two other packs, each a 3 GiB sparse file (no disk is written): over the
        // 4 GiB budget together. "stale" was used longest ago.
        for (slug, used_ago) in [("stale", 90_000u64), ("recent", 60)] {
            let dir = hub.join("packs").join(slug).join("eeee0001");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("pack.json"), "{}").unwrap();
            let big = std::fs::File::create(dir.join("big")).unwrap();
            big.set_len(3 * 1024 * 1024 * 1024).unwrap();
            let used = dir.join("last_used");
            std::fs::write(&used, "x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&used)
                .unwrap()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(used_ago))
                .unwrap();
        }
        let url = spawn_node(&work.join("data-d"), allowlist.clone()).await;
        let out = run(Case {
            hub: hub.clone(),
            cwd: cwd.clone(),
            url: Some(url),
            launcher: forbidden_launcher(),
            hosted: HostedKind::None,
            args: def_args(),
        })
        .await;
        assert_eq!(out.exit, EXIT_OK, "stderr: {:?}", out.stderr);
        assert!(
            !hub.join("packs").join("stale").exists(),
            "the least recently used pack is evicted"
        );
        assert!(
            hub.join("packs").join("recent").join("eeee0001").exists(),
            "the more recently used pack stays: evicting one was enough"
        );
        assert!(hub
            .join("packs")
            .join("mini-c")
            .join(PIN8)
            .join("pack.json")
            .is_file());
    }

    // ── E. offline, nothing cached: exit 1, and no node is started to say so ─
    {
        let hub = work.join("hub-e");
        let code = work.join("code-e");
        publish(&archive, &sig, &work.join("net-e"), &hub);
        set_env(&hub, &code, &scratch_home, &public);
        let mut args = def_args();
        args.offline = true;
        let out = run(Case {
            hub: hub.clone(),
            cwd: cwd.clone(),
            url: Some("http://127.0.0.1:1".to_string()),
            launcher: forbidden_launcher(),
            hosted: HostedKind::Forbidden,
            args,
        })
        .await;
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        let msg = out.stderr.join("\n");
        assert!(msg.contains("--offline"), "{msg}");
        assert!(msg.contains("run: xerj hub pull mini-c"), "{msg}");
        assert!(!hub.join("node.pid").exists(), "no node was started");
        assert!(!hub.join("packs").exists(), "nothing was downloaded");
    }

    // ── G. an EXCLUDED corpus: no pull, and no hint to pull it, offline or not ─
    {
        let hub = work.join("hub-g");
        let code = work.join("code-g");
        publish(&archive, &sig, &work.join("net-g"), &hub);
        set_catalogue_field(&hub, "licence_class", json!("excluded"));
        set_env(&hub, &code, &scratch_home, &public);
        for flag in ["--offline", "--no-fetch", "(neither)"] {
            let mut args = def_args();
            match flag {
                "--offline" => args.offline = true,
                "--no-fetch" => args.no_fetch = true,
                _ => {}
            }
            let out = run(Case {
                hub: hub.clone(),
                cwd: cwd.clone(),
                url: Some("http://127.0.0.1:1".to_string()),
                launcher: forbidden_launcher(),
                hosted: HostedKind::None,
                args,
            })
            .await;
            assert_eq!(out.exit, EXIT_NO_ANSWER, "{flag}");
            let msg = out.stderr.join("\n");
            assert!(msg.contains("excluded licence class"), "{flag}: {msg}");
            assert!(
                !msg.contains("xerj hub pull") && !msg.contains("run:"),
                "{flag}: a hint to pull an excluded corpus is a way round the policy: {msg}"
            );
            assert!(!hub.join("packs").exists(), "{flag}: nothing downloaded");
            assert!(!hub.join("node.pid").exists(), "{flag}: no node started");
        }
        // The command the old hint named refuses it too.
        let rc = tokio::task::spawn_blocking({
            let url = "http://127.0.0.1:1".to_string();
            move || {
                xerj_hub::cli::run_hub_cli(&[
                    "pull".into(),
                    "mini-c".into(),
                    "--url".into(),
                    url,
                    "--quiet".into(),
                ])
            }
        })
        .await
        .unwrap();
        assert_eq!(rc, 1, "`xerj hub pull` itself refuses an excluded corpus");
        assert!(!hub.join("packs").exists());
    }

    // ── F. a pull killed mid-run does not wedge its corpus ───────────────────
    // The old pull lock was a directory removed only by the pull's own destructor:
    // SIGKILL, an OOM kill or an agent's tool timeout left the corpus un-pullable
    // for half an hour, with no owner recorded. It is an OS lock now.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let hub = work.join("hub-f");
        let code = work.join("code-f");
        publish(&archive, &sig, &work.join("net-f"), &hub);
        let real_catalogue = std::fs::read(hub.join("catalogue.json")).unwrap();
        set_env(&hub, &code, &scratch_home, &public);
        let url = spawn_node(&work.join("data-f"), allowlist.clone()).await;

        // A pack URL that never delivers a byte: a FIFO with no writer. The pull takes
        // its lock, starts "downloading" and blocks in open(2).
        let fifo = work.join("net-f").join("never.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0, "mkfifo");
        set_catalogue_field(
            &hub,
            "pack_url",
            json!(format!("file://{}", fifo.display())),
        );

        // The victim is a separate process running `xerj hub pull` (this binary).
        let mut victim = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "child_main",
                "--nocapture",
                "--",
                "hub",
                "pull",
                "mini-c",
                "--url",
                &url,
                "--quiet",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let lock_path = hub
            .join("packs")
            .join("mini-c")
            .join(format!("{PIN8}.lock"));
        wait_for(
            "the victim to take the pull lock",
            Duration::from_secs(90),
            || lock_is_held(&lock_path),
        )
        .await;
        assert!(
            hub.join("packs")
                .join("mini-c")
                .join(format!("{PIN8}.tmp"))
                .is_dir(),
            "it is mid-pull: past the lock, into its temp dir"
        );
        assert!(
            victim.try_wait().unwrap().is_none(),
            "still blocked in the download"
        );

        // SIGKILL: no destructor runs. (The kernel closes its descriptors.)
        victim.kill().unwrap();
        victim.wait().unwrap();
        assert!(!lock_is_held(&lock_path), "the dead process holds nothing");

        // The retry is NOT blocked — and, pointed at the real pack, it completes.
        std::fs::write(hub.join("catalogue.json"), &real_catalogue).unwrap();
        let t0 = Instant::now();
        let rc = tokio::task::spawn_blocking({
            let url = url.clone();
            move || {
                xerj_hub::cli::run_hub_cli(&[
                    "pull".into(),
                    "mini-c".into(),
                    "--url".into(),
                    url,
                    "--quiet".into(),
                ])
            }
        })
        .await
        .unwrap();
        assert_eq!(rc, 0, "the retry after a killed pull succeeds");
        assert!(
            t0.elapsed() < Duration::from_secs(45),
            "and does not sit out a 30-minute lock: {:?}",
            t0.elapsed()
        );
        assert!(hub
            .join("packs")
            .join("mini-c")
            .join(PIN8)
            .join("pack.json")
            .is_file());
        let _ = std::fs::remove_file(&fifo);
    }
}

/// Set one field of the first catalogue entry in `<hub>/catalogue.json`.
fn set_catalogue_field(hub: &Path, field: &str, value: serde_json::Value) {
    let path = hub.join("catalogue.json");
    let mut cat: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cat["corpora"][0][field] = value;
    std::fs::write(&path, serde_json::to_vec(&cat).unwrap()).unwrap();
}

/// Is somebody holding the lock file `path`? (Tries it, and lets go again.)
#[cfg(unix)]
fn lock_is_held(path: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(path)
    else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(e)) => panic!("cannot probe {}: {e}", path.display()),
    }
}
