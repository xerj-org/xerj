//! End-to-end proof that the isolated `.xls` reader keeps the indexer alive on
//! hostile BIFF input. These tests drive the REAL parent path
//! (`xerj_autoindex::extract::xls::extract`), which spawns the real `xerj`
//! binary as the rlimit-capped worker — so they need that binary and are
//! `#[ignore]`d by default. Run them against a release build, under a tight
//! address-space ulimit, exactly as the live threat model would see it:
//!
//! ```sh
//! cargo build --release -p xerj-server
//! XERJ_XLS_WORKER_BIN=$PWD/target/release/xerj \
//!   bash -c 'ulimit -v 2000000; \
//!     cargo test --release -p xerj-autoindex --test xls_isolation -- --ignored --nocapture'
//! ```
//!
//! A cargo test's own `current_exe()` is the test binary, which carries no
//! `__extract-xls` dispatch, so `XERJ_XLS_WORKER_BIN` must point at the built
//! `xerj` executable. The crafted files are synthesised here with the `cfb`
//! writer and never committed; the optional benign fixture (a real multi-sheet
//! `.xls`, e.g. produced by `xlwt`) is passed via `XERJ_XLS_BENIGN_FIXTURE`.

use std::io::Write;
use std::path::{Path, PathBuf};

use xerj_autoindex::extract::xls;
use xerj_autoindex::extract::{ExtractStats, RawRecord};

/// Write a synthetic OLE2 workbook whose `Workbook` stream is exactly `biff`.
fn write_workbook(dir: &Path, file: &str, biff: &[u8]) -> PathBuf {
    let path = dir.join(file);
    {
        let mut comp = cfb::create(&path).unwrap();
        let mut s = comp.create_stream("Workbook").unwrap();
        s.write_all(biff).unwrap();
        s.flush().unwrap();
        comp.flush().unwrap();
    }
    path
}

/// One BIFF record: `[type u16][len u16][body…]`.
fn record(typ: u16, body: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(4 + body.len());
    r.extend_from_slice(&typ.to_le_bytes());
    r.extend_from_slice(&(body.len() as u16).to_le_bytes());
    r.extend_from_slice(body);
    r
}

/// A plausible BIFF8 workbook-globals BOF so calamine starts iterating records
/// normally and reaches the hostile one that follows.
fn bof() -> Vec<u8> {
    // vers=0x0600, dt=0x0005 (globals), rupBuild, rupYear, + reserved.
    record(
        0x0809,
        &[0x00, 0x06, 0x05, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
}

fn drain(path: &Path) -> ExtractStats {
    let mut recs: Vec<RawRecord> = Vec::new();
    let stats = xls::extract(path, path, None, &mut |r| {
        recs.push(r);
        true
    })
    .expect("parent extract must return Ok — never propagate a worker crash");
    // The record count the sink saw must agree with the reported stats.
    assert_eq!(recs.len() as u64, stats.records);
    stats
}

/// The six crafted vectors from the adversarial review. Four are caught by the
/// allocation pre-filter (no worker spawned); two (`Lbl`/`ExternSheet`) slip the
/// pre-filter and crash calamine inside the worker. Every one must come back as
/// junk, with the parent returning `Ok` and the process still standing.
#[test]
#[ignore = "spawns the real xerj worker binary; run under ulimit with XERJ_XLS_WORKER_BIN"]
fn hostile_xls_yields_junk_and_the_parent_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // 1) 14-byte DIMENSIONS, huge rwMac — pre-filter DoS vector.
    let mut dim14 = vec![0u8; 14];
    dim14[4..8].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes()); // rwMac
    dim14[10..12].copy_from_slice(&0x0100u16.to_le_bytes()); // colMac = 256
    let f1 = write_workbook(dir, "dim14.xls", &[bof(), record(0x0200, &dim14)].concat());

    // 2) 10-byte DIMENSIONS, rl = cl = 0xFFFF — pre-filter DoS vector.
    let mut dim10 = vec![0u8; 10];
    dim10[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes()); // rl
    dim10[6..8].copy_from_slice(&0xFFFFu16.to_le_bytes()); // cl
    let f2 = write_workbook(dir, "dim10.xls", &[bof(), record(0x0200, &dim10)].concat());

    // 3) SST, huge cstUnique — pre-filter DoS vector.
    let mut sst_huge = vec![0u8; 8];
    sst_huge[4..8].copy_from_slice(&0x7FFF_FFFFi32.to_le_bytes());
    let f3 = write_workbook(
        dir,
        "sst_huge.xls",
        &[bof(), record(0x00FC, &sst_huge)].concat(),
    );

    // 4) SST, negative cstUnique — pre-filter DoS vector (calamine's unwrap).
    let mut sst_neg = vec![0u8; 8];
    sst_neg[4..8].copy_from_slice(&(-1i32).to_le_bytes());
    let f4 = write_workbook(
        dir,
        "sst_neg.xls",
        &[bof(), record(0x00FC, &sst_neg)].concat(),
    );

    // 5) Lbl (0x0018) with an 8-byte body — calamine does `&r.data[14..]`.
    let f5 = write_workbook(dir, "lbl.xls", &[bof(), record(0x0018, &[0u8; 8])].concat());

    // 6) ExternSheet (0x0017) with a 0-byte body — calamine read_u16 on < 2 bytes.
    let f6 = write_workbook(
        dir,
        "externsheet.xls",
        &[bof(), record(0x0017, &[])].concat(),
    );

    for path in [&f1, &f2, &f3, &f4, &f5, &f6] {
        let stats = drain(path);
        assert_eq!(
            (stats.records, stats.junk),
            (0, 1),
            "hostile file {} must be skipped as junk, got {stats:?}",
            path.display()
        );
    }
}

/// A real, benign multi-sheet workbook still indexes through the isolated path.
/// Skipped unless a fixture is supplied (calamine has no writer, so a genuine
/// BIFF file cannot be synthesised in-process).
#[test]
#[ignore = "needs a real .xls at XERJ_XLS_BENIGN_FIXTURE; run under ulimit with XERJ_XLS_WORKER_BIN"]
fn benign_xls_still_yields_rows_through_the_worker() {
    let Some(fixture) = std::env::var_os("XERJ_XLS_BENIGN_FIXTURE") else {
        eprintln!("skipping: set XERJ_XLS_BENIGN_FIXTURE to a real .xls to run this");
        return;
    };
    let path = PathBuf::from(fixture);
    let stats = drain(&path);
    assert!(
        stats.records > 0 && stats.junk == 0,
        "a benign workbook must yield real rows, got {stats:?}"
    );
}
