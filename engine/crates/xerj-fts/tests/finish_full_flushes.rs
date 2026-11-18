//! A segment's FTS side-cars must not cost a full device flush per file.
//!
//! `File::sync_all` is `fcntl(F_FULLFSYNC)` on Apple targets, which drains the
//! whole device write cache and costs milliseconds per call. Writing each of a
//! segment's side-cars as its own durable write (sync the temp file, rename,
//! sync the directory) paid two of those per file: about 250 for a 31-field
//! segment, and on a Mac those flushes were most of the indexing time.
//!
//! One device flush after the temp files are written, and one after they are
//! renamed into place (Apple targets; elsewhere `fsync` stays per file, and
//! only the per-file directory sync goes), give the segment the same guarantee at the moment
//! `finish` returns: every side-car's bytes are on stable storage before its
//! name points at them, and every name is on stable storage before the caller
//! publishes the segment.
//!
//! This file holds one test on purpose: the flush counter is process-wide, and
//! a test binary of its own is the only way no other test writes alongside it.

use std::collections::HashMap;
use std::sync::Arc;

use xerj_fts::analyzer::AnalyzerRegistry;
use xerj_fts::index::{FieldValues, FtsIndexReader, FtsIndexWriter};

const FIELDS: usize = 31;
const DOCS: u32 = 50;

fn field(i: usize) -> String {
    format!("field_{i:02}")
}

#[test]
fn finishing_a_wide_segment_costs_a_bounded_number_of_full_flushes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut writer =
        FtsIndexWriter::new(dir.path(), "seg_1", Arc::new(AnalyzerRegistry::default()));
    for doc in 0..DOCS {
        let fields: HashMap<String, FieldValues> = (0..FIELDS)
            .map(|i| {
                (
                    field(i),
                    FieldValues::One(format!("alpha beta value{} doc{doc}", i % 5)),
                )
            })
            .collect();
        writer.add_document(doc, &fields);
    }
    writer
        .publish_encoded_filename_layout()
        .expect("layout marker");

    let before = xerj_common::fsio::full_flush_count();
    let stats = writer.finish().expect("finish");
    let flushes = xerj_common::fsio::full_flush_count() - before;
    assert_eq!(stats.len(), FIELDS);

    // The old per-file shape issued 2 per side-car (temp file, then the
    // directory after its rename): 8 per field, 248 here.
    //
    // Apple targets: one device flush before the renames and one after.
    // Elsewhere `fsync` is the per-file barrier and stays per file, so the
    // saving is the per-file directory sync: one per side-car plus one.
    let side_cars = (FIELDS * 4) as u64;
    let bound = if cfg!(target_vendor = "apple") {
        2
    } else {
        side_cars + 1
    };
    assert!(
        flushes <= bound,
        "finish issued {flushes} full flushes for {FIELDS} fields; expected at most {bound}"
    );

    // Same segment, still readable: every field opens and answers.
    let names: Vec<String> = (0..FIELDS).map(field).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let reader = FtsIndexReader::open(dir.path(), "seg_1", &refs).expect("open");
    for name in &names {
        assert!(reader.field_stats(name).is_some(), "{name} has no stats");
        assert_eq!(reader.term_doc_freq(name, "alpha"), Some(DOCS), "{name}");
    }

    // No temp file survives a successful finish.
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}
