//! End-to-end tests for `xerj brain` — the one-command second brain.
//!
//! The heavy test drives the REAL binary against the contract's five-note
//! fixture (SECOND_BRAIN_SPEC §8.1): run 1 must boot a detached server,
//! index the folder, relay the first-launch passkey-setup link, and — with
//! `--no-open` — never attempt to open a browser; run 2 must attach to the
//! running server and converge to the byte-identical edge set; run 3 (no
//! `--no-open`) must hand the console URL to the platform opener, observed
//! via the `XERJ_BROWSER` override.

#![cfg(unix)]

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use xerj_autoindex::detect;
use xerj_autoindex::esclient::Es;

/// §8.1 — bodies are exact; offsets in §8.2 are byte offsets into them.
fn write_fixture(dir: &Path) {
    fs::write(
        dir.join("alpha.md"),
        "Alpha is the hub note. It links to [[beta]] and [[gamma]].",
    )
    .unwrap();
    fs::write(
        dir.join("beta.md"),
        "Beta continues the thread and references [[gamma]].",
    )
    .unwrap();
    fs::write(
        dir.join("gamma.md"),
        "Gamma is the sink note with no outgoing links.",
    )
    .unwrap();
    fs::write(dir.join("delta.md"), "Delta cites [[alpha]] as its source.").unwrap();
    fs::write(dir.join("epsilon.md"), "Epsilon stands alone.").unwrap();
}

/// A free (P, P+1, P+2) triple — the booted server needs es/rest/grpc.
fn free_port_triple() -> u16 {
    for _ in 0..64 {
        let probe =
            xerj_common::loopback_test::expect_loopback(TcpListener::bind(("127.0.0.1", 0)));
        let p = probe.local_addr().unwrap().port();
        drop(probe);
        if !(1025..=u16::MAX - 2).contains(&p) {
            continue;
        }
        let all_free = (p..=p + 2).all(|q| TcpListener::bind(("127.0.0.1", q)).is_ok());
        if all_free {
            return p;
        }
    }
    panic!("no free port triple found");
}

/// Fake platform opener: records the URL it was asked to open.
fn write_fake_browser(script: &Path, marker: &Path) {
    fs::write(
        script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(script).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(script, perm).unwrap();
}

/// Kills the detached server `xerj brain` booted, via its pid file.
struct ServerGuard {
    data_dir: PathBuf,
}
impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(self.data_dir.join("server.pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

fn run_brain(args: &[&str], home: &Path, browser: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("brain")
        .args(args)
        // Isolate ~/.xerj (autoindex resume journal) into the test root.
        .env("HOME", home)
        .env("XERJ_BROWSER", browser)
        .env_remove("BROWSER")
        .env_remove("XERJ_API_KEY")
        .output()
        .unwrap()
}

const EDGES_INDEX: &str = ".xerj-memory-notes-edges";

fn live_edges(es: &Es) -> Vec<Value> {
    let resp = es
        .search(
            EDGES_INDEX,
            &json!({
                "size": 100,
                "query": {"bool": {
                    "filter": [{"exists": {"field": "src"}}],
                    "must_not": [{"exists": {"field": "invalid_at"}}]
                }}
            }),
        )
        .expect("search edges index");
    resp.pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn brain_end_to_end_five_note_fixture() {
    let root = tempfile::tempdir().unwrap();
    let notes = root.path().join("notes");
    fs::create_dir(&notes).unwrap();
    write_fixture(&notes);
    let data_dir = root.path().join("data");
    let marker = root.path().join("browser-marker");
    let fake_browser = root.path().join("fake-browser.sh");
    write_fake_browser(&fake_browser, &marker);
    let port = free_port_triple();
    let url = format!("http://localhost:{port}");
    let _guard = ServerGuard {
        data_dir: data_dir.clone(),
    };

    let notes_arg = notes.to_str().unwrap();
    let data_arg = data_dir.to_str().unwrap();
    let base_args = ["--url", &url, "--data-dir", data_arg];

    // ── run 1: nothing listening → boot, index, relay setup link ─────────
    let mut args1: Vec<&str> = vec![notes_arg, "--no-open"];
    args1.extend_from_slice(&base_args);
    let out1 = run_brain(&args1, root.path(), &fake_browser);
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    assert!(
        out1.status.success(),
        "run 1 failed (status {:?})\nstdout:\n{stdout1}\nstderr:\n{stderr1}",
        out1.status
    );
    assert!(
        stderr1.contains("booted xerj server"),
        "run 1 must say it booted a server\nstderr:\n{stderr1}"
    );
    assert!(
        stdout1.contains("your second brain is ready"),
        "stdout:\n{stdout1}"
    );
    assert!(
        stdout1.contains(&format!("{url}/_xerj-console/#/second-brain?brain=notes")),
        "the console deep link must be printed\nstdout:\n{stdout1}"
    );
    assert!(
        stdout1.contains("/_xerj-console/setup#token="),
        "a fresh boot must relay the one-time passkey-setup link\nstdout:\n{stdout1}"
    );
    assert!(
        stdout1.contains("browser not opened (--no-open)"),
        "stdout:\n{stdout1}"
    );
    assert!(
        !marker.exists(),
        "--no-open must not attempt to open a browser"
    );

    // ── the brain index exists with the §8.3 edge set ────────────────────
    let api_key = fs::read_to_string(data_dir.join("admin.key"))
        .expect("booted server persists admin.key")
        .trim()
        .to_string();
    let es = Es::new(&url, Some(api_key)).unwrap();

    let meta = es
        .get_doc(EDGES_INDEX, detect::BRAIN_META_ID)
        .expect("read brain meta")
        .expect("brain meta doc must exist");
    assert_eq!(
        meta["brain"],
        json!("notes"),
        "brain defaults to the folder name"
    );

    let edges1 = live_edges(&es);
    assert_eq!(
        edges1.len(),
        13,
        "fixture yields 4 wikilink + 4 same_dir + 5 sequence openers (SPEC \u{00a7}8.3), got: {edges1:#?}"
    );
    let mut by_detector: BTreeMap<String, u64> = BTreeMap::new();
    for e in &edges1 {
        *by_detector
            .entry(e["_source"]["detector"].as_str().unwrap().to_string())
            .or_default() += 1;
    }
    assert_eq!(
        by_detector,
        BTreeMap::from([
            ("wikilink@2".to_string(), 4),
            ("samedir@2".to_string(), 4),
            ("sequence@2".to_string(), 5),
        ])
    );
    // §8.2 — wikilink evidence (source file, byte offset), exact.
    let wikilinks: BTreeSet<(String, u64)> = edges1
        .iter()
        .filter(|e| e["_source"]["detector"] == json!("wikilink@2"))
        .map(|e| {
            (
                e["_source"]["evidence"]["source"].as_str().unwrap().into(),
                e["_source"]["evidence"]["offset"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        wikilinks,
        BTreeSet::from([
            ("alpha.md".to_string(), 35),
            ("alpha.md".to_string(), 48),
            ("beta.md".to_string(), 41),
            ("delta.md".to_string(), 12),
        ])
    );
    let ids1: BTreeSet<String> = edges1
        .iter()
        .map(|e| e["_id"].as_str().unwrap().to_string())
        .collect();

    // ── run 2: server already listening → attach; edge set converges ─────
    let out2 = run_brain(&args1, root.path(), &fake_browser);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        out2.status.success(),
        "run 2 failed\nstdout:\n{stdout2}\nstderr:\n{stderr2}"
    );
    assert!(
        stderr2.contains("attached to the running xerj server"),
        "run 2 must say it attached\nstderr:\n{stderr2}"
    );
    let edges2 = live_edges(&es);
    let ids2: BTreeSet<String> = edges2
        .iter()
        .map(|e| e["_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids1, ids2,
        "re-running on an unchanged folder must converge to identical edge_ids"
    );
    assert!(!marker.exists(), "--no-open again: still no browser");

    // ── run 3: without --no-open, the opener gets the console URL ────────
    let mut args3: Vec<&str> = vec![notes_arg];
    args3.extend_from_slice(&base_args);
    let out3 = run_brain(&args3, root.path(), &fake_browser);
    let stdout3 = String::from_utf8_lossy(&out3.stdout);
    assert!(out3.status.success(), "run 3 failed\nstdout:\n{stdout3}");
    // The opener is spawned fire-and-forget; give it a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let opened = fs::read_to_string(&marker).expect("opener must have been invoked");
    assert!(
        opened.contains("/_xerj-console/") && opened.contains("second-brain?brain=notes"),
        "opened URL must land in the second-brain view, got: {opened}"
    );
}

#[test]
fn brain_refuses_an_empty_folder_before_touching_a_server() {
    let root = tempfile::tempdir().unwrap();
    let empty = root.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let data_dir = root.path().join("data-empty");
    let marker = root.path().join("browser-marker");
    let fake_browser = root.path().join("fake-browser.sh");
    write_fake_browser(&fake_browser, &marker);
    let url = format!("http://localhost:{}", free_port_triple());

    let out = run_brain(
        &[
            empty.to_str().unwrap(),
            "--url",
            &url,
            "--data-dir",
            data_dir.to_str().unwrap(),
        ],
        root.path(),
        &fake_browser,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("nothing to build a brain from"),
        "must say honestly why, and what would work\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("xerj brain ~/notes"),
        "must show a working example\nstderr:\n{stderr}"
    );
    assert!(
        !data_dir.exists(),
        "an empty ask must not boot a server or create a data dir"
    );
    assert!(!marker.exists(), "and must certainly not open a browser");
}

#[test]
fn brain_help_is_stranger_readable() {
    let out = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["brain", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "turn a folder into a running, browsable second brain",
        "indexes every readable file",
        "opens your knowledge in",
        "USAGE",
        "--no-open",
        "--brain",
    ] {
        assert!(help.contains(needle), "help missing {needle:?}:\n{help}");
    }
}
