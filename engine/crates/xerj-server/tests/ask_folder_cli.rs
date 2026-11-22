//! End-to-end test for `xerj ask <folder> "<question>"` (#1800): the real
//! binary boots a detached node, indexes a three-note folder with autoindex
//! and answers from it; a second question attaches to that node and reuses
//! the index.

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write_fixture(dir: &Path) {
    fs::write(
        dir.join("release-checklist.md"),
        "# Release checklist\n\nBefore a release, run the conformance suite and tag the commit.\n",
    )
    .unwrap();
    fs::write(
        dir.join("garden.md"),
        "# Garden\n\nTomatoes need full sun and deep watering twice a week.\n",
    )
    .unwrap();
    fs::write(
        dir.join("travel.md"),
        "# Travel\n\nThe night train to Lisbon leaves at 07:40 from platform four.\n",
    )
    .unwrap();
}

/// A free (P, P+1, P+2) triple. `XERJ_TEST_PORT_BASE` confines the search to
/// `base..base+100` on a shared machine; otherwise the OS picks.
fn free_port_triple() -> u16 {
    let candidates: Vec<u16> = match std::env::var("XERJ_TEST_PORT_BASE")
        .ok()
        .and_then(|b| b.parse::<u16>().ok())
    {
        Some(base) => (base..base.saturating_add(98)).collect(),
        None => (0..64)
            .map(|_| {
                let probe = xerj_common::loopback_test::expect_loopback(TcpListener::bind((
                    "127.0.0.1",
                    0,
                )));
                probe.local_addr().unwrap().port()
            })
            .collect(),
    };
    for p in candidates {
        if !(1025..=u16::MAX - 2).contains(&p) {
            continue;
        }
        if (p..=p + 2).all(|q| TcpListener::bind(("127.0.0.1", q)).is_ok()) {
            return p;
        }
    }
    panic!("no free port triple found");
}

/// Kills the node `xerj ask` booted, via its pid file.
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

fn run_ask(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("ask")
        .args(args)
        .env("HOME", home)
        .env_remove("XERJ_URL")
        .env_remove("XERJ_API_KEY")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn ask_a_folder_boots_indexes_and_answers_then_reuses_the_index() {
    let root = tempfile::tempdir().unwrap();
    let notes = root.path().join("notes");
    let home = root.path().join("home");
    let data = root.path().join("node");
    fs::create_dir_all(&notes).unwrap();
    fs::create_dir_all(&home).unwrap();
    write_fixture(&notes);
    let port = free_port_triple();
    let url = format!("http://127.0.0.1:{port}");
    let _guard = ServerGuard {
        data_dir: data.clone(),
    };
    let folder = notes.to_string_lossy().to_string();
    let data_arg = data.to_string_lossy().to_string();

    let first = run_ask(
        &[
            &folder,
            "when does the night train to Lisbon leave",
            "--url",
            &url,
            "--data-dir",
            &data_arg,
        ],
        &home,
    );
    let (out, err) = (text(&first.stdout), text(&first.stderr));
    assert!(first.status.success(), "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("booted a xerj node"), "{err}");
    assert!(err.contains("time to first answer"), "{err}");
    let top = out.lines().find(|l| l.starts_with("───")).unwrap_or("");
    assert!(
        top.contains("travel.md"),
        "top hit is not travel.md:\n{out}"
    );
    // The autoindex summary goes to stderr; stdout is the answer only.
    assert!(!out.contains("xerj-done"), "{out}");

    let second = run_ask(
        &[
            &folder,
            "how often do tomatoes need watering",
            "--url",
            &url,
            "--data-dir",
            &data_arg,
        ],
        &home,
    );
    let (out, err) = (text(&second.stdout), text(&second.stderr));
    assert!(second.status.success(), "stdout:\n{out}\nstderr:\n{err}");
    assert!(!err.contains("booted a xerj node"), "{err}");
    let top = out.lines().find(|l| l.starts_with("───")).unwrap_or("");
    assert!(
        top.contains("garden.md"),
        "top hit is not garden.md:\n{out}"
    );
}

#[test]
fn ask_a_folder_help_and_missing_question() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let folder = root.path().to_string_lossy().to_string();

    let help = run_ask(&[&folder, "--help"], &home);
    assert!(help.status.success());
    assert!(text(&help.stdout).contains("usage: xerj ask <folder>"));

    let missing = run_ask(&[&folder], &home);
    assert_eq!(missing.status.code(), Some(2));
    assert!(text(&missing.stderr).contains("a question is required"));
}
