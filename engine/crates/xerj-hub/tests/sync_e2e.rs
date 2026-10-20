//! Corpus Hub v2 — `xerj hub sync` end to end (plan §8 "Tests").
//!
//! Real nodes (an ephemeral TCP port each, the way `pack_roundtrip.rs` does), the
//! real `xerj hub pull`, a real signed catalogue served over HTTP from a throwaway
//! local "hub", and the real `sync` command — everything except the process
//! boundary: sync normally runs `pull` as a child of the `xerj` binary, which a
//! test binary cannot be, so the pull is driven in-process through the same
//! `run_hub_cli(["pull", …])` entry point instead.
//!
//! The story, in one `#[tokio::test]` because it mutates process-global env
//! (`XERJ_CODE_HOME`, `XERJ_HUB_HOME`, `XERJ_HUB_PUBKEY`, `XERJ_HUB_URL`):
//!
//! 1. two packs of one corpus (`mini-c`) — an older pin and a newer one — are built
//!    and signed on a builder node;
//! 2. a client node pulls the older pin; `def` answers `MINI_LIMIT = 128`;
//! 3. the hub's catalogue moves to the newer pin and `sync` runs: the pack is
//!    replaced, the OLD directory is gone, `def` now answers `256` (the restore
//!    really happened), `last_sync` is written;
//! 4. a second sync pulls nothing;
//! 5. the catalogue moves to a pin whose archive is corrupted: sync exits 3, the
//!    pin that works stays in place and keeps answering, `last_sync` is untouched;
//! 6. a catalogue whose signature does not verify is refused (exit 1) and changes
//!    nothing.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::json;
use sha2::{Digest, Sha256};

use xerj_autoindex::harvest::sign::{generate_keypair, sign_bytes};
use xerj_hub::catalogue::canonical_json;
use xerj_hub::node::HubNode;
use xerj_hub::pack::{self, PackFileEntry};
use xerj_hub::sync::{run_sync_using, CliOps, Ledger, PullOutcome, SyncOps};
use xerj_hub::{
    Catalogue, CatalogueEntry, DefRecord, LicenceClass, PackIndex, PackManifest, PackSource,
    Sketch, CATALOGUE_FORMAT_VERSION, PACK_FORMAT_VERSION,
};

/// One pin of the `mini-c` corpus: the older and the newer differ in pin, date,
/// index prefix and in the value `MINI_LIMIT` carries, so a `def` lookup says
/// which pack is live.
#[derive(Clone, Copy)]
struct PackSpec {
    pin_sha: &'static str,
    pin_date: &'static str,
    prefix: &'static str,
    limit: &'static str,
}

const OLD: PackSpec = PackSpec {
    pin_sha: "0774d05537f9762f838f7ab541b7765f1a729cb5",
    pin_date: "2026-10-02",
    prefix: "xc-mini-c-b1",
    limit: "128",
};
const NEW: PackSpec = PackSpec {
    pin_sha: "9f3c1a2b4d5e60718293a4b5c6d7e8f901234567",
    pin_date: "2026-10-09",
    prefix: "xc-mini-c-b2",
    limit: "256",
};
/// A pin the catalogue will advertise with a corrupted archive behind it.
const BAD_SHA: &str = "c0ffee0011223344556677889900aabbccddeeff";

impl PackSpec {
    fn pin8(&self) -> &'static str {
        &self.pin_sha[..8]
    }
}

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
        HubNode::connect(&probe)
            .unwrap()
            .wait_until_reachable(Duration::from_secs(30))
            .unwrap();
    })
    .await
    .unwrap();
    url
}

fn sample_defs(spec: &PackSpec) -> Vec<DefRecord> {
    vec![
        DefRecord {
            symbol: "MINI_LIMIT".into(),
            kind: "define".into(),
            lang: "c".into(),
            file: "limits.h".into(),
            line: 5,
            text: format!("#define MINI_LIMIT {}", spec.limit),
            value: Some(spec.limit.into()),
        },
        DefRecord {
            symbol: "mini_clamp".into(),
            kind: "function".into(),
            lang: "c".into(),
            file: "clamp.c".into(),
            line: 4,
            text: "int mini_clamp(int value, int lo, int hi)".into(),
            value: None,
        },
    ]
}

fn manifest(spec: &PackSpec) -> PackManifest {
    PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: "mini-c".into(),
        pin_sha: spec.pin_sha.into(),
        pin_date: spec.pin_date.into(),
        built_at: "2026-10-14T00:00:00Z".into(),
        builder_version: "xerj test".into(),
        index_prefix: spec.prefix.into(),
        indices: vec![
            PackIndex {
                name: format!("{}-defs", spec.prefix),
                docs: 2,
                bytes: 0,
            },
            PackIndex {
                name: format!("{}-docs", spec.prefix),
                docs: 1,
                bytes: 0,
            },
        ],
        snapshot_name: format!("mini-c-{}", spec.pin8()),
        licence_spdx: "MIT".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        sources: vec![PackSource {
            repo: "mini-c".into(),
            url: "file:///fixtures/mini-c".into(),
            sha: spec.pin_sha.into(),
            licence: "MIT".into(),
        }],
        defs_count: 2,
        files_count: 2,
    }
}

/// Create the docs + defs indices for `spec` on the builder node and snapshot
/// `<prefix>-*` into `location`.
fn seed_and_snapshot(url: &str, spec: &PackSpec, location: &Path) {
    let node = HubNode::connect(url).unwrap();
    let prefix = spec.prefix;
    node.request_ok(
        "PUT",
        &format!("/{prefix}-docs"),
        Some(&json!({"mappings":{"properties":{"body":{"type":"text"}}}})),
    )
    .unwrap();
    node.bulk_ndjson(
        &format!("/{prefix}-docs/_bulk"),
        "{\"index\":{}}\n{\"body\":\"mini c reference\"}\n".to_string(),
    )
    .unwrap();
    node.request_ok(
        "PUT",
        &format!("/{prefix}-defs"),
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
    for d in sample_defs(spec) {
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
    node.bulk_ndjson(&format!("/{prefix}-defs/_bulk"), ndjson)
        .unwrap();
    node.request_ok("POST", &format!("/{prefix}-defs/_refresh"), None)
        .ok();

    let repo = format!("hub-build-{}", spec.pin8());
    node.request_ok(
        "PUT",
        &format!("/_snapshot/{repo}"),
        Some(&json!({"type":"fs","settings":{"location": location.to_string_lossy()}})),
    )
    .unwrap();
    node.request_ok(
        "PUT",
        &format!(
            "/_snapshot/{repo}/mini-c-{}?wait_for_completion=true",
            spec.pin8()
        ),
        Some(&json!({"indices": format!("{prefix}-*")})),
    )
    .unwrap();
}

/// Build and sign one pack on the builder node; returns the archive path and its
/// sha256.
fn build_pack(builder_url: &str, spec: &PackSpec, out: &Path, seed: &str) -> (PathBuf, String) {
    let location = out.join(format!("snapshot-{}", spec.pin8()));
    seed_and_snapshot(builder_url, spec, &location);
    let files = vec![PackFileEntry {
        path: "limits.h".into(),
        lang: "c".into(),
        bytes: 300,
        licence: "MIT".into(),
    }];
    let archive = out.join(format!("mini-c-{}.hubpack.tar.zst", spec.pin8()));
    pack::write_pack(
        &archive,
        &manifest(spec),
        &sample_defs(spec),
        &files,
        &location,
        seed,
    )
    .unwrap();
    let sha = sha256_hex(&archive);
    (archive, sha)
}

fn sha256_hex(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn sig_of(archive: &Path) -> PathBuf {
    let mut s = archive.as_os_str().to_os_string();
    s.push(".sig");
    PathBuf::from(s)
}

/// A catalogue entry whose pack is a `file://` archive.
fn catalogue_entry(
    pin_sha: &str,
    pin_date: &str,
    archive: &Path,
    pack_sha256: &str,
) -> CatalogueEntry {
    CatalogueEntry {
        slug: "mini-c".into(),
        domain: "DEV".into(),
        topics: vec!["c".into()],
        ask_for: "a tiny C fixture corpus".into(),
        pin_sha: pin_sha.into(),
        pin_date: pin_date.into(),
        refresh_cadence_days: 7,
        licence_spdx: "MIT".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        pack_bytes: std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0),
        pack_url: format!("file://{}", archive.display()),
        pack_sha256: pack_sha256.into(),
        sig_url: format!("file://{}", sig_of(archive).display()),
        dependency_names: vec![],
        quality_grade: None,
        term_sketch: Sketch {
            m_bits: 64,
            k_hashes: 7,
            bits_base64: "AAAAAAAAAAA=".into(),
        },
    }
}

/// The signed bytes of a one-corpus catalogue: `(catalogue.json, signature)`.
fn signed_catalogue(entry: CatalogueEntry, seed: &str) -> (Vec<u8>, String) {
    let cat = Catalogue {
        format_version: CATALOGUE_FORMAT_VERSION,
        generated_at: "2026-10-14T00:00:00Z".into(),
        corpora: vec![entry],
    };
    let bytes = canonical_json(&cat).unwrap();
    let sig = sign_bytes(&bytes, seed).unwrap();
    (bytes, sig)
}

/// A throwaway local "hub": serves whatever catalogue it currently holds at
/// `/api/v1/catalogue.json` and its signature at `….sig`, for as many requests as
/// come.
struct TinyHub {
    url: String,
    addr: std::net::SocketAddr,
    body: Arc<Mutex<(Vec<u8>, String)>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TinyHub {
    fn start(initial: (Vec<u8>, String)) -> TinyHub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = Arc::new(Mutex::new(initial));
        let stop = Arc::new(AtomicBool::new(false));
        let (b, s) = (body.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let first = req.lines().next().unwrap_or("");
                let (cat, sig) = b.lock().unwrap().clone();
                let payload: Vec<u8> = if first.contains(".sig") {
                    sig.into_bytes()
                } else {
                    cat
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&payload);
                let _ = stream.flush();
            }
        });
        TinyHub {
            url: format!("http://{addr}"),
            addr,
            body,
            stop,
            handle: Some(handle),
        }
    }

    fn serve(&self, bytes: Vec<u8>, sig: String) {
        *self.body.lock().unwrap() = (bytes, sig);
    }

    fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr); // wake the accept loop
        if let Some(h) = self.handle.take() {
            h.join().unwrap();
        }
    }
}

/// Drives the real `hub pull` in-process and reads the real ledger, counting the
/// pulls and noting what was on disk around each one.
struct InProcessOps {
    node_url: String,
    hub_home: PathBuf,
    code_home: PathBuf,
    pulls: Arc<AtomicUsize>,
    /// For each pull: was the OLD pin's directory present before it, and were
    /// BOTH directories present when it returned (i.e. sync had not yet removed
    /// the old one).
    around_pull: Arc<Mutex<Vec<(bool, bool)>>>,
}

impl SyncOps for InProcessOps {
    fn pull(&self, slug: &str) -> PullOutcome {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        let old = self.hub_home.join("packs").join(slug).join(OLD.pin8());
        let new = self.hub_home.join("packs").join(slug).join(NEW.pin8());
        let old_before = old.is_dir();
        let rc = xerj_hub::cli::run_hub_cli(&[
            "pull".into(),
            slug.into(),
            "--url".into(),
            self.node_url.clone(),
            "--quiet".into(),
        ]);
        self.around_pull
            .lock()
            .unwrap()
            .push((old_before, old.is_dir() && new.is_dir()));
        if rc == 0 {
            PullOutcome::Done
        } else {
            PullOutcome::Failed {
                code: rc,
                message: format!("hub pull exited {rc}"),
            }
        }
    }

    fn ledger(&self, slug: &str) -> Ledger {
        // The real ledger reader, pointed at this test's code root.
        CliOps::with_program("unused", vec![], None, self.code_home.clone()).ledger(slug)
    }
}

fn age_file(path: &Path, secs: u64) {
    let when = SystemTime::now() - Duration::from_secs(secs);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Every DIRECTORY under the corpus's pack dir: installed pins, plus any
/// half-extracted `<pin8>.tmp` scratch dir a broken pull or sync would leave
/// (which must show up and fail the assertion). The `<pin8>.lock` files are
/// left out on purpose: they are flock files that `crate::flock` never deletes
/// by design (unlinking one lets a waiter lock a dead inode), so they are not
/// state a pull or a sync is responsible for removing.
fn pin_dirs(hub_home: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(hub_home.join("packs/mini-c"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_replaces_the_pack_with_a_newer_pin_and_the_old_dir_is_gone() {
    // One canonical work root so every snapshot location is inside the nodes'
    // allowlist (the engine refuses a repo location outside data_dir or the
    // allowlist; we configure the root, never weaken the check).
    let work_tmp = tempfile::tempdir().unwrap();
    let work = std::fs::canonicalize(work_tmp.path()).unwrap();
    let out = work.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let allowlist = vec![work.to_string_lossy().into_owned()];
    let (seed, public) = generate_keypair().unwrap();

    // ── 1. Build + sign both pins on the builder node ─────────────────────
    let builder = spawn_node(&work.join("data-builder"), allowlist.clone()).await;
    let (old_archive, old_sha, new_archive, new_sha) = {
        let (builder, out, seed) = (builder.clone(), out.clone(), seed.clone());
        tokio::task::spawn_blocking(move || {
            let (oa, os) = build_pack(&builder, &OLD, &out, &seed);
            let (na, ns) = build_pack(&builder, &NEW, &out, &seed);
            (oa, os, na, ns)
        })
        .await
        .unwrap()
    };

    // A corrupted copy of the NEW archive, advertised under a third pin with the
    // original (now mismatching) sha256 — the way a tampered download looks.
    let bad_archive = out.join("mini-c-c0ffee00.hubpack.tar.zst");
    std::fs::copy(&new_archive, &bad_archive).unwrap();
    std::fs::copy(sig_of(&new_archive), sig_of(&bad_archive)).unwrap();
    let mut bytes = std::fs::read(&bad_archive).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&bad_archive, &bytes).unwrap();

    // ── 2. The local hub starts out advertising the OLD pin ───────────────
    let hub = TinyHub::start(signed_catalogue(
        catalogue_entry(OLD.pin_sha, OLD.pin_date, &old_archive, &old_sha),
        &seed,
    ));
    let client = spawn_node(&work.join("data-client"), allowlist).await;
    let hub_home = work.join("hub-home");
    let code_home = work.join("code-home");
    std::fs::create_dir_all(&hub_home).unwrap();
    std::env::set_var("XERJ_CODE_HOME", &code_home);
    std::env::set_var("XERJ_HUB_HOME", &hub_home);
    std::env::set_var("XERJ_HUB_PUBKEY", &public);
    std::env::set_var("XERJ_HUB_URL", &hub.url);

    // The first pull is the ordinary one: nothing cached yet, so sync has no
    // part in it. Seed the catalogue cache the way a first `ask` would.
    {
        let (bytes, _) = signed_catalogue(
            catalogue_entry(OLD.pin_sha, OLD.pin_date, &old_archive, &old_sha),
            &seed,
        );
        std::fs::write(hub_home.join("catalogue.json"), bytes).unwrap();
    }
    let rc = {
        let client = client.clone();
        tokio::task::spawn_blocking(move || {
            xerj_hub::cli::run_hub_cli(&[
                "pull".into(),
                "mini-c".into(),
                "--url".into(),
                client,
                "--quiet".into(),
            ])
        })
        .await
        .unwrap()
    };
    assert_eq!(rc, 0, "the older pin pulls cleanly");
    assert_eq!(pin_dirs(&hub_home), vec![OLD.pin8()]);
    let limit = |client: String| {
        tokio::task::spawn_blocking(move || {
            let node = HubNode::connect(&client).unwrap();
            let a = xerj_hub::def::lookup(&node, "mini-c", "MINI_LIMIT", 5);
            (
                a.hits.first().and_then(|h| h.value.clone()),
                a.pin_date.clone(),
            )
        })
    };
    assert_eq!(
        limit(client.clone()).await.unwrap(),
        (Some("128".to_string()), OLD.pin_date.to_string()),
        "the old pin answers 128"
    );

    // The shared ops factory: every sync below goes through the real command.
    let pulls = Arc::new(AtomicUsize::new(0));
    let around_pull = Arc::new(Mutex::new(Vec::new()));
    let run = {
        let (client, hub_home, code_home) = (client.clone(), hub_home.clone(), code_home.clone());
        let (pulls, around_pull) = (pulls.clone(), around_pull.clone());
        move || {
            let (client, hub_home, code_home) =
                (client.clone(), hub_home.clone(), code_home.clone());
            let (pulls, around_pull) = (pulls.clone(), around_pull.clone());
            tokio::task::spawn_blocking(move || {
                run_sync_using(&[], &move |_url| {
                    Ok(Box::new(InProcessOps {
                        node_url: client.clone(),
                        hub_home: hub_home.clone(),
                        code_home: code_home.clone(),
                        pulls: pulls.clone(),
                        around_pull: around_pull.clone(),
                    }) as Box<dyn SyncOps>)
                })
            })
        }
    };

    // ── 3. The hub moves to the NEW pin; sync replaces the pack ───────────
    let (bytes, sig) = signed_catalogue(
        catalogue_entry(NEW.pin_sha, NEW.pin_date, &new_archive, &new_sha),
        &seed,
    );
    hub.serve(bytes, sig);
    assert!(
        !hub_home.join("last_sync").exists(),
        "precondition: sync has never run"
    );

    assert_eq!(run().await.unwrap(), 0, "a sync with a newer pin succeeds");

    assert_eq!(
        pin_dirs(&hub_home),
        vec![NEW.pin8()],
        "the new pin replaced the old one and the old directory is gone"
    );
    assert!(hub_home
        .join("packs/mini-c")
        .join(NEW.pin8())
        .join("pack.json")
        .is_file());
    assert_eq!(pulls.load(Ordering::SeqCst), 1, "exactly one pull");
    assert_eq!(
        *around_pull.lock().unwrap(),
        vec![(true, true)],
        "the old pin was on disk before the pull and STILL on disk when it returned: \
         it is removed only after the new pin has restored"
    );
    let ledger: serde_json::Value = serde_json::from_slice(
        &std::fs::read(code_home.join("state").join("mini-c.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ledger["source"], json!("hubpack"));
    assert_eq!(ledger["pin_sha"], json!(NEW.pin_sha));
    assert_eq!(ledger["index_prefix"], json!(NEW.prefix));
    assert_eq!(
        limit(client.clone()).await.unwrap(),
        (Some("256".to_string()), NEW.pin_date.to_string()),
        "the new pin is the one answering: the restore really ran"
    );
    let stamp = std::fs::read_to_string(hub_home.join("last_sync")).unwrap();
    chrono::DateTime::parse_from_rfc3339(stamp.trim()).expect("last_sync is RFC 3339");
    assert!(
        hub_home.join("catalogue.json").is_file(),
        "the refreshed catalogue was cached"
    );

    // ── 4. Nothing moved: a second sync pulls nothing ─────────────────────
    assert_eq!(run().await.unwrap(), 0);
    assert_eq!(
        pulls.load(Ordering::SeqCst),
        1,
        "a pack already at the catalogue's pin costs no download"
    );
    assert_eq!(pin_dirs(&hub_home), vec![NEW.pin8()]);

    // ── 5. The hub advertises a pin whose archive is corrupt ──────────────
    age_file(&hub_home.join("last_sync"), 3 * 86_400);
    let before = std::fs::metadata(hub_home.join("last_sync"))
        .unwrap()
        .modified()
        .unwrap();
    let (bytes, sig) = signed_catalogue(
        catalogue_entry(BAD_SHA, "2026-10-16", &bad_archive, &new_sha),
        &seed,
    );
    hub.serve(bytes, sig);

    assert_eq!(
        run().await.unwrap(),
        3,
        "a pack that fails verification is exit 3"
    );
    assert_eq!(
        pin_dirs(&hub_home),
        vec![NEW.pin8()],
        "nothing was extracted for the bad pin and the working pin was not touched"
    );
    assert!(!hub_home.join("packs/mini-c/c0ffee00.tmp").exists());
    assert_eq!(
        limit(client.clone()).await.unwrap().0,
        Some("256".to_string()),
        "the pin that works keeps answering"
    );
    assert_eq!(
        std::fs::metadata(hub_home.join("last_sync"))
            .unwrap()
            .modified()
            .unwrap(),
        before,
        "a failed sync does not claim to have synced"
    );

    // ── 6. A catalogue that does not verify is refused, and changes nothing ─
    let (good, good_sig) = signed_catalogue(
        catalogue_entry(OLD.pin_sha, OLD.pin_date, &old_archive, &old_sha),
        &seed,
    );
    let mut tampered = good.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0x01;
    hub.serve(tampered, good_sig);
    let pulls_before = pulls.load(Ordering::SeqCst);

    assert_eq!(
        run().await.unwrap(),
        1,
        "a catalogue whose signature fails is a refresh failure, exit 1"
    );
    assert_eq!(
        pulls.load(Ordering::SeqCst),
        pulls_before,
        "nothing was pulled"
    );
    assert_eq!(pin_dirs(&hub_home), vec![NEW.pin8()]);

    // Clean up so the env does not leak and the local hub thread ends.
    hub.shutdown();
    for k in [
        "XERJ_CODE_HOME",
        "XERJ_HUB_HOME",
        "XERJ_HUB_PUBKEY",
        "XERJ_HUB_URL",
    ] {
        std::env::remove_var(k);
    }
}
