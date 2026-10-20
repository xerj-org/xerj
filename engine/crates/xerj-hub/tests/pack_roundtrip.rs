//! Corpus Hub v2 — the pack build → pull → `def` roundtrip (plan §2/§3 "Tests").
//!
//! This boots REAL nodes in-process (an ephemeral TCP port each, the way
//! `xerj-api`'s endpoint tests build a router) so the snapshot/restore HTTP
//! calls run end to end:
//!
//! - build a mini-c pack on node A (create a `-defs` index, bulk the defs,
//!   snapshot it, [`pack::write_pack`] + a test keypair);
//! - pull it into node B's empty data dir through the real `xerj hub pull` CLI,
//!   verifying sha256 THEN signature before extracting;
//! - assert `def::lookup` answers `MINI_LIMIT = 128` and the state ledger says
//!   `source: hubpack`;
//! - a flipped archive byte makes `pull` exit 3 and extract nothing;
//! - a second pull of the same pin says "already present";
//! - a `format_version: 2` pack is refused (exit 3).
//!
//! The archive-level guarantees (verify-before-extract, SHA256SUMS coverage,
//! format-version refusal) are also covered by `pack.rs`'s unit tests; this file
//! adds the parts that need a live node: restore and the `def` query over it.
//!
//! One `#[tokio::test]` holds the whole flow because it mutates process-global
//! env (`XERJ_CODE_HOME`, `XERJ_HUB_HOME`, `XERJ_HUB_PUBKEY`) between scenarios.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};

use xerj_autoindex::harvest::sign::generate_keypair;
use xerj_hub::node::HubNode;
use xerj_hub::pack::{self, PackFileEntry};
use xerj_hub::{
    Catalogue, CatalogueEntry, DefRecord, LicenceClass, PackIndex, PackManifest, PackSource,
    Sketch, AUTO_PULL_MAX_BYTES, CATALOGUE_FORMAT_VERSION, DISK_BUDGET_BYTES, PACK_FORMAT_VERSION,
};

const PIN_SHA: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";
const PIN8: &str = "0774d055";
const PIN_DATE: &str = "2026-10-02";
const PREFIX: &str = "xc-mini-c-b1";

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

/// The defs the test indexes and packs. `MINI_LIMIT` is the §3 lookup target.
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

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A `PackManifest` for the mini-c pack at a given `format_version`.
fn manifest(format_version: u32) -> PackManifest {
    PackManifest {
        format_version,
        slug: "mini-c".into(),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        built_at: "2026-10-14T00:00:00Z".into(),
        builder_version: "xerj test".into(),
        index_prefix: PREFIX.into(),
        indices: vec![
            PackIndex {
                name: format!("{PREFIX}-defs"),
                docs: 3,
                bytes: 0,
            },
            PackIndex {
                name: format!("{PREFIX}-docs"),
                docs: 1,
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
        defs_count: 3,
        files_count: 2,
    }
}

/// Create the docs + defs indices on `url`, bulk the defs, and snapshot
/// `<prefix>-*` into `location`. Leaves the snapshot files on disk for packing.
fn seed_and_snapshot(url: &str, location: &Path) {
    let node = HubNode::connect(url).unwrap();
    // A docs index so the snapshot carries more than one index (realistic).
    node.request_ok(
        "PUT",
        &format!("/{PREFIX}-docs"),
        Some(&json!({"mappings":{"properties":{"body":{"type":"text"}}}})),
    )
    .unwrap();
    node.bulk_ndjson(
        &format!("/{PREFIX}-docs/_bulk"),
        "{\"index\":{}}\n{\"body\":\"mini c reference\"}\n".to_string(),
    )
    .unwrap();

    // The defs index with the §3 mapping.
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

/// Write a catalogue with one mini-c entry pointing at a `file://` pack, taking
/// the pack's real on-disk size as the advertised `pack_bytes`.
fn write_catalogue(hub_home: &Path, archive: &Path, pack_sha256: &str) {
    let real = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    write_catalogue_bytes(hub_home, archive, pack_sha256, real);
}

/// `write_catalogue` with an explicit advertised `pack_bytes` — lets a test lie
/// about the size to exercise the size cap.
fn write_catalogue_bytes(hub_home: &Path, archive: &Path, pack_sha256: &str, pack_bytes: u64) {
    write_catalogue_full(
        hub_home,
        archive,
        pack_sha256,
        pack_bytes,
        LicenceClass::AdaptWithAttribution,
    );
}

/// `write_catalogue` with an explicit licence class — lets a test publish an
/// `excluded` (or `approach-only`) corpus.
fn write_catalogue_class(
    hub_home: &Path,
    archive: &Path,
    pack_sha256: &str,
    licence_class: LicenceClass,
) {
    let real = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
    write_catalogue_full(hub_home, archive, pack_sha256, real, licence_class);
}

fn write_catalogue_full(
    hub_home: &Path,
    archive: &Path,
    pack_sha256: &str,
    pack_bytes: u64,
    licence_class: LicenceClass,
) {
    let sig = {
        let mut s = archive.as_os_str().to_os_string();
        s.push(".sig");
        PathBuf::from(s)
    };
    let entry = CatalogueEntry {
        slug: "mini-c".into(),
        domain: "DEV".into(),
        topics: vec!["c".into()],
        ask_for: "a tiny C fixture corpus".into(),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        refresh_cadence_days: 7,
        licence_spdx: "MIT".into(),
        licence_class,
        pack_bytes,
        pack_url: format!("file://{}", archive.display()),
        pack_sha256: pack_sha256.to_string(),
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pack_build_pull_def_roundtrip() {
    // One canonical work root so every snapshot location is inside the nodes'
    // allowlist (the engine refuses a repo location outside data_dir or the
    // allowlist — plan §2; we configure the root, never weaken the check).
    let work_tmp = tempfile::tempdir().unwrap();
    let work = std::fs::canonicalize(work_tmp.path()).unwrap();
    let out = work.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let allowlist = vec![work.to_string_lossy().into_owned()];

    let (seed, public) = generate_keypair().unwrap();

    // ── Build the pack on node A ──────────────────────────────────────────
    let url_a = spawn_node(&work.join("data-a"), allowlist.clone()).await;
    let location = out.join("mini-c").join("snapshot");
    {
        let url_a = url_a.clone();
        let location = location.clone();
        tokio::task::spawn_blocking(move || seed_and_snapshot(&url_a, &location))
            .await
            .unwrap();
    }

    // write_pack from the on-disk snapshot + the matching defs/files.
    let defs = sample_defs();
    let files = vec![
        PackFileEntry {
            path: "LICENSE".into(),
            lang: "".into(),
            bytes: 1069,
            licence: "MIT".into(),
        },
        PackFileEntry {
            path: "limits.h".into(),
            lang: "c".into(),
            bytes: 300,
            licence: "MIT".into(),
        },
    ];
    let archive = out.join(format!("mini-c-{PIN8}.hubpack.tar.zst"));
    {
        let (m, d, f, loc, seed) = (
            manifest(PACK_FORMAT_VERSION),
            defs.clone(),
            files.clone(),
            location.clone(),
            seed.clone(),
        );
        let archive = archive.clone();
        tokio::task::spawn_blocking(move || {
            pack::write_pack(&archive, &m, &d, &f, &loc, &seed).unwrap();
        })
        .await
        .unwrap();
    }
    assert!(archive.exists(), "archive written");
    let good_sha = sha256_hex(&archive);

    // ── Scenario 1: a flipped byte is refused (exit 3), nothing extracted ──
    {
        let code_home = work.join("code-flip");
        let hub_home = work.join("hub-flip");
        let flipped = out.join("flipped.hubpack.tar.zst");
        std::fs::copy(&archive, &flipped).unwrap();
        // keep the sibling .sig next to the flipped copy
        std::fs::copy(
            archive.with_file_name(format!("mini-c-{PIN8}.hubpack.tar.zst.sig")),
            flipped.with_file_name("flipped.hubpack.tar.zst.sig"),
        )
        .unwrap();
        let mut bytes = std::fs::read(&flipped).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&flipped, &bytes).unwrap();
        // catalogue records the ORIGINAL sha, so the flip is caught.
        write_catalogue(&hub_home, &flipped, &good_sha);

        let url_a2 = url_a.clone();
        let (ch, hh, pk) = (code_home.clone(), hub_home.clone(), public.clone());
        let rc = tokio::task::spawn_blocking(move || {
            std::env::set_var("XERJ_CODE_HOME", &ch);
            std::env::set_var("XERJ_HUB_HOME", &hh);
            std::env::set_var("XERJ_HUB_PUBKEY", &pk);
            xerj_hub::cli::run_hub_cli(&[
                "pull".into(),
                "mini-c".into(),
                "--url".into(),
                url_a2,
                "--quiet".into(),
            ])
        })
        .await
        .unwrap();
        assert_eq!(rc, 3, "a flipped archive fails verification with exit 3");
        let extract = hub_home.join("packs").join("mini-c").join(PIN8);
        assert!(
            !extract.exists(),
            "nothing is extracted when verification fails"
        );
    }

    // ── Scenario 2: a format_version: 2 pack is refused (exit 3) ───────────
    {
        let code_home = work.join("code-v2");
        let hub_home = work.join("hub-v2");
        let v2 = out.join("v2.hubpack.tar.zst");
        {
            let (m, d, f, loc, seed) = (
                manifest(2),
                defs.clone(),
                files.clone(),
                location.clone(),
                seed.clone(),
            );
            let v2 = v2.clone();
            tokio::task::spawn_blocking(move || {
                pack::write_pack(&v2, &m, &d, &f, &loc, &seed).unwrap();
            })
            .await
            .unwrap();
        }
        let v2_sha = sha256_hex(&v2);
        write_catalogue(&hub_home, &v2, &v2_sha);

        let url_a2 = url_a.clone();
        let (ch, hh, pk) = (code_home.clone(), hub_home.clone(), public.clone());
        let rc = tokio::task::spawn_blocking(move || {
            std::env::set_var("XERJ_CODE_HOME", &ch);
            std::env::set_var("XERJ_HUB_HOME", &hh);
            std::env::set_var("XERJ_HUB_PUBKEY", &pk);
            xerj_hub::cli::run_hub_cli(&[
                "pull".into(),
                "mini-c".into(),
                "--url".into(),
                url_a2,
                "--quiet".into(),
            ])
        })
        .await
        .unwrap();
        assert_eq!(rc, 3, "a future pack format_version is refused");
    }

    // ── Scenario 3: a valid pull into node B, then def, then already-present ─
    let url_b = spawn_node(&work.join("data-b"), allowlist.clone()).await;
    let code_home = work.join("code-pull");
    let hub_home = work.join("hub-pull");
    write_catalogue(&hub_home, &archive, &good_sha);

    // First pull — downloads, verifies, restores, writes the ledger.
    let rc = {
        let (url_b, ch, hh, pk) = (
            url_b.clone(),
            code_home.clone(),
            hub_home.clone(),
            public.clone(),
        );
        tokio::task::spawn_blocking(move || {
            std::env::set_var("XERJ_CODE_HOME", &ch);
            std::env::set_var("XERJ_HUB_HOME", &hh);
            std::env::set_var("XERJ_HUB_PUBKEY", &pk);
            xerj_hub::cli::run_hub_cli(&[
                "pull".into(),
                "mini-c".into(),
                "--url".into(),
                url_b,
                "--quiet".into(),
            ])
        })
        .await
        .unwrap()
    };
    assert_eq!(rc, 0, "a valid pack pulls cleanly");

    // The ledger records source: hubpack.
    let state_json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(code_home.join("state").join("mini-c.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state_json["source"], json!("hubpack"));
    assert_eq!(state_json["index_prefix"], json!(PREFIX));
    assert_eq!(state_json["pin_sha"], json!(PIN_SHA));

    // def answers MINI_LIMIT = 128 from the restored defs index.
    let answer = {
        let (url_b, ch) = (url_b.clone(), code_home.clone());
        tokio::task::spawn_blocking(move || {
            std::env::set_var("XERJ_CODE_HOME", &ch);
            let node = HubNode::connect(&url_b).unwrap();
            xerj_hub::def::lookup(&node, "mini-c", "MINI_LIMIT", 5)
        })
        .await
        .unwrap()
    };
    assert_eq!(answer.corpus, "mini-c");
    assert_eq!(answer.pin_date, PIN_DATE);
    assert_eq!(answer.hits.len(), 1, "one exact MINI_LIMIT hit: {answer:?}");
    assert_eq!(answer.hits[0].value.as_deref(), Some("128"));
    assert_eq!(answer.hits[0].file, "limits.h");
    assert_eq!(answer.hits[0].line, 5);

    // Second pull of the same pin → already present, restores nothing.
    let rc2 = {
        let (url_b, ch, hh, pk) = (
            url_b.clone(),
            code_home.clone(),
            hub_home.clone(),
            public.clone(),
        );
        tokio::task::spawn_blocking(move || {
            std::env::set_var("XERJ_CODE_HOME", &ch);
            std::env::set_var("XERJ_HUB_HOME", &hh);
            std::env::set_var("XERJ_HUB_PUBKEY", &pk);
            xerj_hub::cli::run_hub_cli(&[
                "pull".into(),
                "mini-c".into(),
                "--url".into(),
                url_b,
                "--quiet".into(),
            ])
        })
        .await
        .unwrap()
    };
    assert_eq!(rc2, 0, "a second pull of the same pin is a no-op success");

    // ── A PARTIAL index set must NOT be mistaken for a complete pack ───────
    // Drop the -defs index on node B: def now misses. A re-pull must detect the
    // partial set, RESTORE (not skip), and only then is the ledger honest.
    {
        let before = {
            let (u, ch) = (url_b.clone(), code_home.clone());
            tokio::task::spawn_blocking(move || {
                std::env::set_var("XERJ_CODE_HOME", &ch);
                let node = HubNode::connect(&u).unwrap();
                node.request_ok("DELETE", &format!("/{PREFIX}-defs"), None)
                    .unwrap();
                xerj_hub::def::lookup(&node, "mini-c", "MINI_LIMIT", 5)
            })
            .await
            .unwrap()
        };
        assert!(
            before.hits.is_empty(),
            "with -defs deleted, def must miss (partial set)"
        );

        let rc = run_pull(&code_home, &hub_home, Some(&public), &url_b).await;
        assert_eq!(rc, 0, "a partial set re-pulls cleanly (restore, not skip)");

        let after = {
            let (u, ch) = (url_b.clone(), code_home.clone());
            tokio::task::spawn_blocking(move || {
                std::env::set_var("XERJ_CODE_HOME", &ch);
                let node = HubNode::connect(&u).unwrap();
                xerj_hub::def::lookup(&node, "mini-c", "MINI_LIMIT", 5)
            })
            .await
            .unwrap()
        };
        assert_eq!(
            after.hits.len(),
            1,
            "the partial set was restored, not skipped: {after:?}"
        );
        assert_eq!(after.hits[0].value.as_deref(), Some("128"));
    }

    // ── Advertised over the AUTO-pull cap but under the disk budget: the EXPLICIT
    //    `xerj hub pull` fetches it. The 50 MiB cap is on the implicit auto-pull a
    //    question would trigger, not on the command a person (or an agent that was
    //    handed the hint) types — otherwise a corpus whose pack is over the cap
    //    would be unreachable by any route (hosted search is plan step 6/7). ──
    {
        let hh = work.join("hub-overcap");
        write_catalogue_bytes(&hh, &archive, &good_sha, AUTO_PULL_MAX_BYTES + 1);
        let rc = run_pull(&work.join("code-overcap"), &hh, Some(&public), &url_b).await;
        assert_eq!(
            rc, 0,
            "an explicit pull fetches a pack over the auto-pull cap"
        );
        assert!(
            hh.join("packs/mini-c").join(PIN8).exists(),
            "the over-cap pack was extracted by the explicit pull"
        );
    }

    // ── Advertised larger than the whole disk budget: refused BEFORE any download,
    //    even on an explicit pull (the DoS cap — a malicious hub must not OOM the
    //    client; the transfer is bounded again by `fetch_to` if the field lies). ──
    {
        let hh = work.join("hub-cap");
        write_catalogue_bytes(&hh, &archive, &good_sha, DISK_BUDGET_BYTES + 1);
        let rc = run_pull(&work.join("code-cap"), &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 1, "over-budget pack → policy/infra exit 1");
        assert!(
            !hh.join("packs/mini-c").join(format!("{PIN8}.tmp")).exists(),
            "nothing downloaded over the budget"
        );
        assert!(
            !hh.join("packs/mini-c").join(PIN8).exists(),
            "nothing extracted over the budget"
        );
    }

    // ── No verifying key → its OWN exit code (5), NOT "failed verification" (3) ──
    {
        let hh = work.join("hub-nokey");
        write_catalogue(&hh, &archive, &good_sha);
        // A real key is compiled in, so "no key" is the explicit test override.
        let rc = run_pull(
            &work.join("code-nokey"),
            &hh,
            Some(xerj_common::hub::HUB_PUBKEY_NONE),
            &url_b,
        )
        .await;
        assert_eq!(
            rc, 5,
            "no key → \"this build cannot verify anything\" (5), which is not exit 3: the \
             bytes were never judged, let alone found wanting"
        );
        assert!(
            !hh.join("packs/mini-c").join(format!("{PIN8}.tmp")).exists(),
            "the empty-key path must not leak a .tmp to re-download"
        );
        assert!(
            !hh.join("packs/mini-c").join(PIN8).exists(),
            "nothing extracted"
        );
    }

    // ── An EXCLUDED corpus is refused by `hub pull` itself (plan §11) ──
    {
        let hh = work.join("hub-excluded");
        write_catalogue_class(&hh, &archive, &good_sha, LicenceClass::Excluded);
        let rc = run_pull(&work.join("code-excluded"), &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 1, "an excluded corpus is a policy refusal (exit 1)");
        assert!(
            !hh.join("packs").exists(),
            "nothing was downloaded, extracted or even given a directory"
        );
        assert!(
            !work.join("code-excluded").join("state").exists(),
            "and no ledger was written"
        );
        // An approach-only corpus is a different matter: it is pulled (with a warning
        // wherever it is answered).
        let hh = work.join("hub-approach");
        write_catalogue_class(&hh, &archive, &good_sha, LicenceClass::ApproachOnly);
        let rc = run_pull(&work.join("code-approach"), &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 0, "approach-only is allowed through");
    }

    // ── Everything that can refuse a pull is decided BEFORE a node is started ──
    {
        // No --url, XERJ_URL on a silent port: the old order started (and waited for)
        // a whole node first and only then looked at the slug, the licence, the size
        // and the key.
        let hh = work.join("hub-preflight");
        write_catalogue_class(&hh, &archive, &good_sha, LicenceClass::Excluded);
        let silent = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let silent_url = format!("http://127.0.0.1:{silent}");
        for (slug, want_rc, why) in [
            ("mini-c", 1, "excluded"),
            ("no-such-corpus", 2, "unknown slug"),
            ("../../etc", 2, "malformed slug"),
        ] {
            let rc = run_pull_without_url(
                &work.join("code-preflight"),
                &hh,
                Some(&public),
                &silent_url,
                slug,
            )
            .await;
            assert_eq!(rc, want_rc, "{why}");
            for started in [
                "node",
                "node.pid",
                "node.port",
                "node.log",
                "node.toml",
                "node.lock",
            ] {
                assert!(
                    !hh.join(started).exists(),
                    "{why}: `{started}` exists, so a node was prepared or started before the \
                     pull was refused"
                );
            }
        }
        std::env::remove_var("XERJ_URL");
    }

    // ── A pull that fails verification must not delete the pack an earlier pull
    //    verified (it used to `remove_dir_all` the extract on ANY failure) ──
    {
        let code_home = work.join("code-keep");
        let hh = work.join("hub-keep");
        write_catalogue(&hh, &archive, &good_sha);
        let rc = run_pull(&code_home, &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 0, "the first pull succeeds");
        let extract = hh.join("packs").join("mini-c").join(PIN8);
        assert!(extract.join("pack.json").is_file());
        assert!(extract.join("snapshot").is_dir());

        // A re-pull whose archive no longer matches the catalogue's hash.
        let flipped = out.join("flipped.hubpack.tar.zst");
        write_catalogue(&hh, &flipped, &good_sha);
        let rc = run_pull(&code_home, &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 3, "the bad archive is refused");
        assert!(
            extract.join("pack.json").is_file() && extract.join("snapshot").is_dir(),
            "the pack verified earlier is still there: a failed re-pull deletes only what \
             that run created"
        );
        assert!(
            !hh.join("packs/mini-c").join(format!("{PIN8}.tmp")).exists(),
            "and the failed run's own temp is gone"
        );

        // A good re-pull replaces it whole and leaves no staging behind.
        write_catalogue(&hh, &archive, &good_sha);
        let rc = run_pull(&code_home, &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 0);
        let leftovers: Vec<String> = std::fs::read_dir(hh.join("packs").join("mini-c"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != PIN8 && n != &format!("{PIN8}.lock"))
            .collect();
        assert!(leftovers.is_empty(), "no temp/staging left: {leftovers:?}");
    }

    // ── An unreachable node is an INFRA failure (exit 1), not 2 or 3 ──
    {
        let hh = work.join("hub-infra");
        write_catalogue(&hh, &archive, &good_sha);
        let rc = run_pull(
            &work.join("code-infra"),
            &hh,
            Some(&public),
            "http://127.0.0.1:1",
        )
        .await;
        assert_eq!(rc, 1, "unreachable node → infra exit 1 (distinct from 2/3)");
    }

    // ── A concurrent pull of the same pin: the second WAITS for the first ──
    // (it used to be refused outright, and a lock left by a killed pull blocked the
    //  corpus for half an hour; the lock is now the OS's, held by a live process.)
    {
        let hh = work.join("hub-lock");
        write_catalogue(&hh, &archive, &good_sha);
        let lockdir = hh.join("packs").join("mini-c");
        std::fs::create_dir_all(&lockdir).unwrap();
        // Another process (here: another open file description) holds the pin's lock.
        let holder = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lockdir.join(format!("{PIN8}.lock")))
            .unwrap();
        holder.try_lock().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            drop(holder); // the first pull finishes
        });
        let t0 = std::time::Instant::now();
        let rc = run_pull(&work.join("code-lock"), &hh, Some(&public), &url_b).await;
        release.join().unwrap();
        assert_eq!(
            rc, 0,
            "the second pull waited for the first, then went ahead"
        );
        assert!(
            t0.elapsed() >= Duration::from_millis(1200),
            "and it really did wait: {:?}",
            t0.elapsed()
        );
    }

    // ── A lock DIRECTORY left by an older build blocks nothing ──
    {
        let hh = work.join("hub-oldlock");
        write_catalogue(&hh, &archive, &good_sha);
        let lockdir = hh.join("packs").join("mini-c");
        std::fs::create_dir_all(lockdir.join(format!("{PIN8}.lock"))).unwrap();
        let rc = run_pull(&work.join("code-oldlock"), &hh, Some(&public), &url_b).await;
        assert_eq!(rc, 0, "a stale lock directory is replaced, not obeyed");
    }

    // ── A traversal slug is rejected before ANY filesystem write (exit 2) ──
    {
        let hh = work.join("hub-traversal");
        std::fs::create_dir_all(&hh).unwrap();
        let rc = run_pull_slug(
            &work.join("code-traversal"),
            &hh,
            Some(&public),
            &url_b,
            "../../../tmp/evil",
        )
        .await;
        assert_eq!(rc, 2, "a traversal slug is an invalid-slug error (exit 2)");
        assert!(
            !hh.join("packs").exists(),
            "no packs dir created for a rejected slug"
        );
    }

    // Clean up the env so it does not leak to other tests in the binary.
    std::env::remove_var("XERJ_CODE_HOME");
    std::env::remove_var("XERJ_HUB_HOME");
    std::env::remove_var("XERJ_HUB_PUBKEY");
}

/// Run `xerj hub pull mini-c --url <url> --quiet` with the given homes and key
/// (None unsets the pubkey), returning the exit code. Env is process-global;
/// callers `.await` these sequentially so they do not race.
async fn run_pull(code: &Path, hub: &Path, pubkey: Option<&str>, url: &str) -> i32 {
    run_pull_slug(code, hub, pubkey, url, "mini-c").await
}

/// `xerj hub pull <slug> --quiet` with NO `--url`: the node comes from `XERJ_URL`
/// (here a silent loopback port), so the automatic-node path is what would run if
/// the pull got that far.
async fn run_pull_without_url(
    code: &Path,
    hub: &Path,
    pubkey: Option<&str>,
    xerj_url: &str,
    slug: &str,
) -> i32 {
    let (code, hub, pubkey, xerj_url, slug) = (
        code.to_path_buf(),
        hub.to_path_buf(),
        pubkey.map(str::to_string),
        xerj_url.to_string(),
        slug.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        std::env::set_var("XERJ_CODE_HOME", &code);
        std::env::set_var("XERJ_HUB_HOME", &hub);
        std::env::set_var("XERJ_URL", &xerj_url);
        match pubkey {
            Some(pk) => std::env::set_var("XERJ_HUB_PUBKEY", &pk),
            None => std::env::remove_var("XERJ_HUB_PUBKEY"),
        }
        xerj_hub::cli::run_hub_cli(&["pull".into(), slug, "--quiet".into()])
    })
    .await
    .unwrap()
}

async fn run_pull_slug(
    code: &Path,
    hub: &Path,
    pubkey: Option<&str>,
    url: &str,
    slug: &str,
) -> i32 {
    let (code, hub, pubkey, url, slug) = (
        code.to_path_buf(),
        hub.to_path_buf(),
        pubkey.map(str::to_string),
        url.to_string(),
        slug.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        std::env::set_var("XERJ_CODE_HOME", &code);
        std::env::set_var("XERJ_HUB_HOME", &hub);
        match pubkey {
            Some(pk) => std::env::set_var("XERJ_HUB_PUBKEY", &pk),
            None => std::env::remove_var("XERJ_HUB_PUBKEY"),
        }
        xerj_hub::cli::run_hub_cli(&["pull".into(), slug, "--url".into(), url, "--quiet".into()])
    })
    .await
    .unwrap()
}
