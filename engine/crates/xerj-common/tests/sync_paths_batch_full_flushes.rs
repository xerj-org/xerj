//! `fsio::sync_paths_batch` must sync every file in a wide batch while keeping
//! full device flushes bounded on Apple targets and failing cleanly before the
//! trailing flush if any path fails (#3449).
//!
//! This file holds one test on purpose: `fsio::full_flush_count()` is
//! process-wide, so a dedicated test binary ensures no parallel test writes
//! alongside it (same pattern as `xerj-fts/tests/finish_full_flushes.rs`).

use std::path::Path;

#[test]
fn sync_paths_batch_bounds_full_flushes_by_platform() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Empty batch: zero flushes.
    let before = xerj_common::fsio::full_flush_count();
    let empty: [&Path; 0] = [];
    xerj_common::fsio::sync_paths_batch(&empty).expect("empty batch");
    assert_eq!(xerj_common::fsio::full_flush_count() - before, 0);

    // Single file: exactly one full flush on every platform.
    let one = dir.path().join("one.ndjson");
    std::fs::write(&one, b"one\n").expect("write one");
    let before = xerj_common::fsio::full_flush_count();
    xerj_common::fsio::sync_paths_batch(&[one.as_path()]).expect("single file");
    assert_eq!(xerj_common::fsio::full_flush_count() - before, 1);

    // Wide batch (wider than the 16-worker pool) on one filesystem.
    const COUNT: usize = 49;
    let paths: Vec<_> = (0..COUNT)
        .map(|i| {
            let path = dir.path().join(format!("item-{i:04}.ndjson"));
            std::fs::write(&path, format!("record-{i}\n")).expect("write batch item");
            path
        })
        .collect();
    let before = xerj_common::fsio::full_flush_count();
    xerj_common::fsio::sync_paths_batch(&paths).expect("wide batch");
    let flushes = xerj_common::fsio::full_flush_count() - before;
    let expected = if cfg!(target_vendor = "apple") {
        // Apple workers issue `fsync(2)` concurrently and drain the drive
        // write cache once with `F_FULLFSYNC` per filesystem.
        1
    } else {
        COUNT as u64
    };
    assert_eq!(
        flushes, expected,
        "wide batch of {COUNT} files issued {flushes} full flushes; expected {expected}"
    );

    // A missing file fails the batch and, on Apple, does not issue the
    // trailing full flush.
    std::fs::remove_file(&paths[20]).expect("remove one item");
    let before = xerj_common::fsio::full_flush_count();
    let err = xerj_common::fsio::sync_paths_batch(&paths).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    if cfg!(target_vendor = "apple") {
        assert_eq!(xerj_common::fsio::full_flush_count() - before, 0);
    }
}
