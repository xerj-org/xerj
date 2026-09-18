//! #929 — demoting a dataset the server refused, as a pure plan transformation.
//!
//! The end-to-end behaviour (exit code, catalog, incremental carry-forward) is
//! covered over real HTTP in `incremental_reconcile_http_tests`. These pin the
//! invariants the rest of the generation machinery relies on, which an
//! end-to-end run only exercises by accident: canonical ordering, whole-file
//! demotion of a multi-dataset file, alias cleanup, and the serialized plan of
//! a run that refused nothing staying byte-identical to earlier releases.

use super::*;
use crate::state::{DuplicateFile, FileAssignment, JunkFile, Plan, PlanDataset};

fn dataset(slug: &str) -> PlanDataset {
    PlanDataset {
        slug: slug.into(),
        index: format!("ax-{slug}"),
        family: "jsonl".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 0,
    }
}

fn assignment(rel: &str, slugs: &[(Option<&str>, &str)]) -> FileAssignment {
    FileAssignment {
        rel: rel.into(),
        path_id: format!("id:{rel}"),
        is_symlink: Some(false),
        family: "jsonl".into(),
        gzip: false,
        content_digest: Some(format!("digest:{rel}")),
        assignments: slugs
            .iter()
            .map(|(group, slug)| (group.map(str::to_owned), (*slug).to_owned()))
            .collect(),
        as_document: false,
    }
}

fn alias(file_key: &str, rel: &str) -> DuplicateFile {
    DuplicateFile {
        file_key: file_key.into(),
        rel: rel.into(),
        path_id: format!("id:{rel}"),
        is_symlink: Some(false),
        duplicate_of: "canonical".into(),
        bytes: 1,
    }
}

/// logs: k-log-1, k-log-2 · users: k-users · a dump feeding BOTH: k-dump.
fn plan() -> Plan {
    let mut plan = Plan {
        datasets: vec![dataset("logs"), dataset("users"), dataset("orders")],
        ..Plan::default()
    };
    for (key, rel, slugs) in [
        ("k-log-2", "b/log2.jsonl", vec![(None, "logs")]),
        ("k-log-1", "a/log1.jsonl", vec![(None, "logs")]),
        ("k-users", "users.jsonl", vec![(None, "users")]),
        ("k-orders", "orders.jsonl", vec![(None, "orders")]),
        (
            "k-dump",
            "dump.sql",
            vec![(Some("users"), "users"), (Some("logs"), "logs")],
        ),
    ] {
        plan.files.insert(key.into(), assignment(rel, &slugs));
    }
    plan.duplicate_files = vec![
        alias("k-log-1", "copy/log1.jsonl"),
        alias("k-users", "u2.jsonl"),
    ];
    plan.junk_files.push(JunkFile {
        file_key: "k-binary".into(),
        rel: "blob.bin".into(),
        format: "binary".into(),
        status: "skipped".into(),
        reason: "binary".into(),
        bytes: 9,
    });
    plan
}

fn sizes() -> HashMap<&'static str, u64> {
    HashMap::from([("k-log-1", 100), ("k-log-2", 200), ("k-dump", 4000)])
}

#[test]
fn nothing_refused_leaves_the_plan_and_its_serialization_untouched() {
    let mut demoted = plan();
    refuse_datasets(&mut demoted, &[], &sizes());
    assert!(demoted.refused_datasets.is_empty());
    assert_eq!(demoted.files.len(), 5);
    // The serialized plan is hashed into the preparation contract and compared
    // for equality on every no-op re-run; a key that appears for a plan with
    // nothing refused would turn every committed corpus into a spurious
    // "plan changed" generation on upgrade.
    let serialized = serde_json::to_value(&demoted).unwrap();
    assert!(
        serialized.get("refused_datasets").is_none(),
        "an empty refusal list must not serialize: {serialized}"
    );
    assert!(demoted.refused_run_fields().is_empty());
    // …and a plan written before the field existed still loads.
    let legacy: Plan = serde_json::from_value(serialized).unwrap();
    assert!(legacy.refused_datasets.is_empty());
}

#[test]
fn a_refused_dataset_leaves_with_every_file_that_feeds_it() {
    let mut demoted = plan();
    let reason = "PUT /ax-logs/_mapping failed: 400 Bad Request {…}";
    refuse_datasets(&mut demoted, &[("logs".into(), reason.into())], &sizes());

    let slugs: Vec<&str> = demoted.datasets.iter().map(|d| d.slug.as_str()).collect();
    assert_eq!(
        slugs,
        ["users", "orders"],
        "the accepted datasets keep their order"
    );

    // The dump feeds `users` too, and still leaves WHOLE: a partially assigned
    // file cannot be prepared (its `logs` records would have nowhere to go).
    let mut kept: Vec<&str> = demoted.files.keys().map(String::as_str).collect();
    kept.sort_unstable();
    assert_eq!(kept, ["k-orders", "k-users"]);

    assert_eq!(demoted.refused_datasets.len(), 1);
    let refusal = &demoted.refused_datasets[0];
    assert_eq!(refusal.dataset.slug, "logs");
    assert_eq!(
        refusal.dataset.index, "ax-logs",
        "the frozen definition rides along"
    );
    assert_eq!(refusal.reason, reason);
    assert_eq!(
        refusal.file_keys,
        ["k-dump", "k-log-1", "k-log-2"],
        "file keys are sorted — `validate_plan_projection` requires canonical order"
    );

    // Each lost file is a catalogued junk file carrying the server's words and
    // its real size; the junk that was already there is untouched.
    let lost: Vec<&JunkFile> = demoted
        .junk_files
        .iter()
        .filter(|junk| junk.file_key != "k-binary")
        .collect();
    assert_eq!(lost.len(), 3);
    for junk in &lost {
        assert_eq!(junk.status, "junk");
        assert_eq!(junk.reason, refusal.junk_reason());
        assert!(junk.reason.contains("dataset logs") && junk.reason.contains(reason));
    }
    let bytes: HashMap<&str, u64> = lost
        .iter()
        .map(|j| (j.file_key.as_str(), j.bytes))
        .collect();
    assert_eq!(bytes, sizes());
    assert!(demoted
        .junk_files
        .iter()
        .any(|junk| junk.file_key == "k-binary"));

    // Aliases of a dropped file go with it (#283: an alias must belong to a
    // live group); the alias of a surviving file stays.
    let aliases: Vec<&str> = demoted
        .duplicate_files
        .iter()
        .map(|a| a.rel.as_str())
        .collect();
    assert_eq!(aliases, ["u2.jsonl"]);

    // Surviving datasets are recounted against the files that are left.
    let counts: HashMap<&str, usize> = demoted
        .datasets
        .iter()
        .map(|d| (d.slug.as_str(), d.file_count))
        .collect();
    assert_eq!(counts, HashMap::from([("users", 1), ("orders", 1)]));
}

#[test]
fn a_file_feeding_two_refused_datasets_is_attributed_once_deterministically() {
    // Refusals arrive in plan order; attribution must not depend on it, nor on
    // `HashMap` iteration order, because the incremental projection has to
    // reproduce this plan byte for byte.
    for refusals in [
        vec![
            ("users".to_owned(), "r-users".to_owned()),
            ("logs".to_owned(), "r-logs".to_owned()),
        ],
        vec![
            ("logs".to_owned(), "r-logs".to_owned()),
            ("users".to_owned(), "r-users".to_owned()),
        ],
    ] {
        let mut demoted = plan();
        refuse_datasets(&mut demoted, &refusals, &sizes());
        let by_slug: HashMap<&str, &[String]> = demoted
            .refused_datasets
            .iter()
            .map(|r| (r.dataset.slug.as_str(), r.file_keys.as_slice()))
            .collect();
        assert_eq!(
            demoted
                .refused_datasets
                .iter()
                .map(|r| r.dataset.slug.as_str())
                .collect::<Vec<_>>(),
            ["logs", "users"],
            "refusals are stored sorted by slug"
        );
        // `dump.sql` feeds both; it is counted under `logs` (first by slug) only.
        assert_eq!(by_slug["logs"], ["k-dump", "k-log-1", "k-log-2"]);
        assert_eq!(by_slug["users"], ["k-users"]);
        let total: usize = demoted
            .refused_datasets
            .iter()
            .map(|r| r.file_keys.len())
            .sum();
        assert_eq!(total, 4, "no file is counted twice");
        assert_eq!(demoted.files.keys().collect::<Vec<_>>(), ["k-orders"]);
    }
}

#[test]
fn the_run_document_fields_name_each_refused_dataset() {
    let mut demoted = plan();
    refuse_datasets(&mut demoted, &[("logs".into(), "because".into())], &sizes());
    let fields: HashMap<&str, Value> = demoted.refused_run_fields().into_iter().collect();
    assert_eq!(fields["datasets_refused"], 1);
    assert_eq!(fields["files_refused"], 3);
    // A JSON *string*, like `fields_json`: the catalog mapping is frozen, and an
    // array of objects would have the engine infer a nested mapping for a field
    // no release declared.
    let detail: Vec<Value> =
        serde_json::from_str(fields["refused_datasets_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        detail,
        [json!({"slug": "logs", "index": "ax-logs", "files": 3, "reason": "because"})]
    );
}

#[test]
fn the_catalog_map_says_a_corpus_lacks_a_dataset_before_it_lists_the_rest() {
    let mut demoted = plan();
    refuse_datasets(&mut demoted, &[("logs".into(), "because".into())], &sizes());
    let mut run = json!({"doc_kind": "run", "run_id": "r", "root": "/corpus"});
    for (key, value) in demoted.refused_run_fields() {
        run[key] = value;
    }
    let rendered = catalog::render_map(Some(&run), &[], &[], &[], &[], 0);
    let refused = rendered
        .find("## Refused datasets — NOT indexed")
        .expect("the map names refused datasets");
    let datasets = rendered.find("## Datasets").unwrap();
    assert!(
        refused < datasets,
        "the warning comes before the table it qualifies"
    );
    assert!(
        rendered.contains("`ax-logs` — 3 file(s): because"),
        "{rendered}"
    );

    // A whole corpus renders no such section.
    let whole = catalog::render_map(Some(&json!({"doc_kind": "run"})), &[], &[], &[], &[], 0);
    assert!(!whole.contains("Refused datasets"));
}

/// The refusal reason is the server's response body, and it is copied into the
/// plan, the manifest, the run document and every refused file's catalog entry,
/// then printed on stdout and rendered by `map`. It is therefore bounded, and a
/// body cannot use a newline or an escape sequence to start a line of its own.
#[test]
fn the_recorded_reason_is_bounded_and_cannot_forge_a_line() {
    let hostile = format!(
        "{{\"error\":\"no\"}}\nREFUSED dataset everything (0 file(s)): forged\r\n\u{1b}[2K{}",
        "é".repeat(REFUSAL_REASON_MAX * 3)
    );
    let error = anyhow::Error::new(esclient::MappingRefused {
        path: "/ax-logs/_mapping".into(),
        status: 400,
        detail: hostile,
    })
    .context("install generation mapping for ax-logs");
    let reason = refusal_reason(&error);

    assert!(
        reason.starts_with(
            "install generation mapping for ax-logs: PUT /ax-logs/_mapping failed: 400 Bad Request"
        ),
        "the useful head of the refusal survives: {reason}"
    );
    assert!(
        !reason.contains('\n') && !reason.contains('\r') && !reason.contains('\u{1b}'),
        "no control character reaches a stored or printed reason"
    );
    // Counted in characters, so a multi-byte body cannot be split mid code point
    // (a byte slice here is the class of bug that once aborted a whole run).
    assert_eq!(reason.chars().count(), REFUSAL_REASON_MAX + 1);
    assert!(reason.ends_with('…'));

    // An ordinary refusal is stored exactly as the server sent it.
    let ordinary = anyhow::Error::new(esclient::MappingRefused {
        path: "/ax-logs/_mapping".into(),
        status: 400,
        detail: "{\"error\":{\"reason\":\"mapper [value] cannot be changed\"}}".into(),
    });
    assert_eq!(
        refusal_reason(&ordinary),
        "PUT /ax-logs/_mapping failed: 400 Bad Request \
         {\"error\":{\"reason\":\"mapper [value] cannot be changed\"}}"
    );
}
