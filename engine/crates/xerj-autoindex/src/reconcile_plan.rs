//! Deterministic projection of a changed inventory onto a frozen typed plan.
//!
//! Fresh-run clustering is intentionally not reused here: cluster formation
//! and slug election depend on corpus order and membership. Incremental runs
//! must preserve the committed dataset/schema identity and fail closed when a
//! file would require a new dataset or mapping.

use crate::content::Inventory;
use crate::infer::{FieldAcc, FieldSpec};
use crate::state::{FileAssignment, JunkFile, Plan, PlanDataset};
use crate::FileScan;
use anyhow::{Context, Result};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Field shape of a file as the *document* renderer sees it.
///
/// The mirror of the demoted-file re-sampling loop in `build_phase_a`: a
/// demoted config file's committed mapping was inferred from
/// `extract::extract_as_document`, so that is the only extractor whose output
/// may be compared against it. An unreadable file yields an empty shape, which
/// is trivially compatible — it junks at phase B like any other, and the
/// projection only needs the shape.
fn document_sample(path: &Path, gzip: bool, sample: usize) -> HashMap<String, FieldAcc> {
    let mut fields: HashMap<String, FieldAcc> = HashMap::new();
    let mut sampled = 0usize;
    let mut sink = |record: crate::extract::RawRecord| -> bool {
        if sampled < sample {
            for (name, value) in &record.fields {
                fields.entry(name.clone()).or_default().add(value);
            }
        }
        sampled += 1;
        sampled < sample
    };
    let _ = crate::extract::extract_as_document(path, gzip, &mut sink);
    fields
}

/// Project the current, byte-verified inventory onto committed dataset schemas.
///
/// Assignment precedence is stable content identity, then stable native path
/// identity, then deterministic schema matching. Dataset definitions are
/// copied byte-for-byte except for their derived `file_count`.
pub(crate) fn reconcile_plan(
    inventory: &Inventory,
    previous: &Plan,
    scans: Vec<FileScan>,
    sample: usize,
) -> Result<Plan> {
    anyhow::ensure!(
        inventory.files.len() == inventory.keys.len()
            && inventory.files.len() == inventory.digests.len()
            && inventory.files.len() == scans.len(),
        "inventory and scan cardinalities disagree"
    );

    let datasets: HashMap<&str, &PlanDataset> = previous
        .datasets
        .iter()
        .map(|dataset| (dataset.slug.as_str(), dataset))
        .collect();
    anyhow::ensure!(
        datasets.len() == previous.datasets.len(),
        "frozen plan has duplicate dataset slugs"
    );

    let mut prior_path_owner: HashMap<&str, (&str, &FileAssignment)> = HashMap::new();
    for (content_id, assignment) in &previous.files {
        anyhow::ensure!(
            !assignment.path_id.is_empty(),
            "frozen assignment {} has no native path identity",
            assignment.rel
        );
        anyhow::ensure!(
            prior_path_owner
                .insert(&assignment.path_id, (content_id, assignment))
                .is_none(),
            "frozen plan assigns one native path identity more than once"
        );
    }
    for alias in &previous.duplicate_files {
        let assignment = previous
            .files
            .get(&alias.file_key)
            .with_context(|| format!("frozen alias {} has no canonical assignment", alias.rel))?;
        anyhow::ensure!(
            !alias.path_id.is_empty(),
            "frozen alias {} has no native path identity",
            alias.rel
        );
        anyhow::ensure!(
            prior_path_owner
                .insert(&alias.path_id, (&alias.file_key, assignment))
                .is_none(),
            "frozen plan assigns one native path identity more than once"
        );
    }

    let mut files = HashMap::new();
    let mut junk_files = Vec::new();
    for (((file, content_id), content_digest), scan) in inventory
        .files
        .iter()
        .zip(&inventory.keys)
        .zip(&inventory.digests)
        .zip(scans)
    {
        if let Some((status, reason)) = scan.junk {
            junk_files.push(JunkFile {
                file_key: content_id.clone(),
                rel: file.rel.clone(),
                format: crate::format_str(scan.sniffed.as_ref()),
                status,
                reason,
                bytes: file.size,
            });
            continue;
        }
        let sniffed = scan
            .sniffed
            .as_ref()
            .with_context(|| format!("{} has sketches but no detected family", file.rel))?;
        anyhow::ensure!(
            !scan.sketches.is_empty(),
            "{} has no dataset sketches and was not classified as junk",
            file.rel
        );

        let content_owner = previous.files.get(content_id);
        let path_owner = prior_path_owner.get(file.rel_id.as_str()).copied();
        if let (Some(content), Some((path_content_id, path))) = (content_owner, path_owner) {
            anyhow::ensure!(
                std::ptr::eq(content, path)
                    || previous
                        .files
                        .get(path_content_id)
                        .is_some_and(|candidate| std::ptr::eq(candidate, content)),
                "{} has conflicting committed content and path owners",
                file.rel
            );
        }
        let retained = content_owner.or_else(|| path_owner.map(|(_, assignment)| assignment));

        // A demoted one-off config file (#173) is committed to a *docs*
        // dataset and indexed through `extract::extract_as_document`, not its
        // family extractor. Its frozen mapping therefore describes the fixed
        // document shape, never the file's own flattened fields — so the scan
        // sketches (which come from the family extractor) are the wrong thing
        // to compare against it. Re-sample the file through the same document
        // renderer `build_phase_a` uses, exactly as the fresh path does, and
        // carry the committed `as_document` decision forward. Losing it would
        // index flattened records into a document mapping.
        // A fresh plan folds a group-less DOCUMENT (empty `key_fields`:
        // prose/code/PDF, dataset.rs:119) AND a demoted one-off CONFIG (a
        // single-record `demotable_family` JSON/YAML/XML, the demotion loop)
        // into the scope's `docs` dataset. On the `--no-graph` incremental path
        // a NEW file of either shape must reconcile the same way — else it is
        // compared by its raw family here, matches no frozen dataset, and
        // aborts the whole run ("use a new prefix"). Gate on the frozen plan
        // having actually done the fold: a group-less `docs` dataset exists to
        // join, and NO group-less dataset of the file's own family exists (a
        // >8-config fleet forms its own data dataset instead, and such a file
        // must join THAT, not be forced into docs).
        // #731 (1): unlike `dataset::cluster`, this fold does not bound the
        // demoted fleet at `DOC_DEMOTE_MAX_FILES`. A 9th single-record config
        // landing in a scope whose frozen plan already demoted 8 into `docs`
        // (so no group-less dataset of its own family exists) joins docs too,
        // where a *fresh* re-cluster would instead form a new data dataset for
        // that family. This is not drift from the committed plan — it is
        // incremental's committed-identity-preservation contract doing its
        // job (it must not re-cluster) — but it does mean a long-lived
        // incremental run and a from-scratch genesis can diverge on where a
        // >8-file fleet ends up.
        let all_group_less = scan.sketches.iter().all(|s| s.group.is_none());
        let all_empty_key_fields = scan.sketches.iter().all(|s| s.key_fields.is_empty());
        let docs_target = retained.is_none()
            && all_group_less
            && previous
                .datasets
                .iter()
                .any(|d| d.family == "docs" && d.group.is_none())
            && !previous
                .datasets
                .iter()
                .any(|d| d.family == sniffed.family.as_str() && d.group.is_none());
        let new_document = docs_target && all_empty_key_fields;
        let new_demoted_config = docs_target
            && !all_empty_key_fields
            && crate::dataset::demotable_family(sniffed.family)
            && scan.sketches.iter().map(|s| s.records).sum::<u64>() == 1;
        let route_to_docs = new_document || new_demoted_config;

        // Only a demoted CONFIG re-samples through the document renderer (its
        // family extractor's flattened fields are the wrong mapping). A
        // prose/code document indexes through its OWN extractor, so its scan
        // fields already describe the document shape and `as_document` stays
        // false — exactly as `build_phase_a` leaves non-demoted docs members
        // (1946-1962).
        let as_document = retained.is_some_and(|owner| owner.as_document) || new_demoted_config;
        let document_fields = if as_document {
            Some(document_sample(&file.path, sniffed.gzip, sample))
        } else {
            None
        };

        let mut assignments = Vec::with_capacity(scan.sketches.len());
        for sketch in &scan.sketches {
            let group = &sketch.group;
            let fields = document_fields.as_ref().unwrap_or(&sketch.fields);
            let slug = if let Some(owner) = retained {
                retained_slug(owner, group, sniffed.family.as_str(), fields, &datasets)
                    .with_context(|| format!("project retained file {}", file.rel))?
            } else {
                // A routed document/config classifies as "docs" — its fresh-plan
                // fold target — instead of aborting on its raw family.
                let family = if route_to_docs {
                    "docs"
                } else {
                    sniffed.family.as_str()
                };
                classify_new(group, family, fields, &previous.datasets)
                    .with_context(|| format!("project new file {}", file.rel))?
            };
            assignments.push((group.clone(), slug));
        }
        assignments.sort();
        assignments.dedup();
        anyhow::ensure!(
            assignments.len() == scan.sketches.len(),
            "{} has duplicate dataset groups",
            file.rel
        );
        let replaced = files.insert(
            content_id.clone(),
            FileAssignment {
                rel: file.rel.clone(),
                path_id: file.rel_id.clone(),
                is_symlink: Some(file.is_symlink),
                family: sniffed.family.as_str().to_owned(),
                gzip: sniffed.gzip,
                content_digest: Some(content_digest.clone()),
                assignments,
                as_document,
            },
        );
        anyhow::ensure!(
            replaced.is_none(),
            "byte-verified inventory contains duplicate content identity {content_id}"
        );
    }

    let live_content: HashSet<&str> = files.keys().map(String::as_str).collect();
    let mut duplicate_files = inventory
        .duplicates
        .iter()
        .filter(|alias| live_content.contains(alias.file_key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    duplicate_files.sort_by(|left, right| {
        left.file_key
            .cmp(&right.file_key)
            .then_with(|| left.path_id.cmp(&right.path_id))
            .then_with(|| left.rel.cmp(&right.rel))
    });
    junk_files.sort_by(|left, right| {
        left.file_key
            .cmp(&right.file_key)
            .then_with(|| left.rel.cmp(&right.rel))
    });

    let mut frozen_datasets = previous.datasets.clone();
    for dataset in &mut frozen_datasets {
        dataset.file_count = files
            .values()
            .filter(|assignment| {
                assignment
                    .assignments
                    .iter()
                    .any(|(_, slug)| slug == &dataset.slug)
            })
            .count();
    }

    Ok(Plan {
        datasets: frozen_datasets,
        files,
        junk_files,
        duplicate_files,
        alias_paths_indexed: previous.alias_paths_indexed,
    })
}

fn retained_slug(
    owner: &FileAssignment,
    group: &Option<String>,
    family: &str,
    fields: &HashMap<String, FieldAcc>,
    datasets: &HashMap<&str, &PlanDataset>,
) -> Result<String> {
    anyhow::ensure!(
        owner.family == family,
        "detected family changed from {} to {family}",
        owner.family
    );
    let matches = owner
        .assignments
        .iter()
        .filter(|(assigned_group, _)| assigned_group == group)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matches.len() == 1,
        "frozen assignment has {} matches for group {:?}",
        matches.len(),
        group
    );
    let slug = &matches[0].1;
    let dataset = datasets
        .get(slug.as_str())
        .with_context(|| format!("frozen assignment references absent dataset {slug}"))?;
    ensure_compatible(fields, dataset)?;
    Ok(slug.clone())
}

// #731 (2): scope-agnostic by construction — candidates are filtered by
// `family`/`group` only, never by which scope (e.g. which nested `.git`
// repo) the file lives under. With multiple group-less `docs` datasets
// already frozen (one per scope), a routed document is free to join
// whichever one wins on field-overlap ratio + slug, not necessarily the one
// a fresh, scope-keyed plan would have folded it into. `ensure_compatible`
// still gates by type, so this can only pick a *compatible* docs dataset,
// never a mismatched one — but it is pre-existing family-based classify
// behavior, not something #729/#730 introduced, and is not demonstrable in
// a single-scope tree.
fn classify_new(
    group: &Option<String>,
    family: &str,
    fields: &HashMap<String, FieldAcc>,
    datasets: &[PlanDataset],
) -> Result<String> {
    let mut candidates = Vec::new();
    for dataset in datasets
        .iter()
        .filter(|dataset| dataset.family == family && &dataset.group == group)
    {
        if ensure_compatible(fields, dataset).is_err() {
            continue;
        }
        let (intersection, union) = field_overlap(fields, &dataset.specs);
        let threshold_met = if dataset.family == "docs" {
            // A `docs` dataset's specs are the UNION of every document/config
            // the fold produced. `ensure_compatible` (run just above) already
            // requires every observed SCALAR field to be present in the frozen
            // specs — a field absent from the specs is tolerated ONLY when
            // `acc.n == 0` (pruned: e.g. a code file's object-valued `symbols`
            // sidecar, #580). So a document/config that reached here belongs to
            // the docs dataset regardless of how many other fields the union
            // carries; accept it. (The 0.7 Jaccard would wrongly reject a
            // document with fewer fields than the union, and `== fields.len()`
            // wrongly counts the pruned `symbols` key and aborted a new code
            // file — the #729 central case.) `let _` keeps the shared
            // `field_overlap` result in scope for the data-family branches.
            //
            // #731 (3): this also silently admits a routed file with ZERO
            // rendered fields (e.g. an unreadable/empty document) — harmless,
            // since it junks at phase B (see `document_sample`'s doc
            // comment), but unscreened here rather than flagged.
            //
            // #731 (4): and it tolerates a brand-new object-valued
            // (`acc.n == 0`) field beyond the known `symbols` sidecar — e.g. a
            // document introducing a never-before-seen `metadata: {...}`
            // field absent from the frozen specs. By design, not a
            // mapping-widening regression: `FieldAcc` never scalar-types
            // objects (see `ensure_compatible` above), and genesis folds
            // object-field documents without a `FieldSpec` too, so this is
            // consistent with genesis admission.
            let _ = (intersection, union);
            true
        } else if group.is_some() {
            intersection * 2 >= union // 0.5
        } else {
            intersection * 10 >= union * 7 // 0.7
        };
        if threshold_met {
            candidates.push((dataset, intersection, union));
        }
    }
    candidates.sort_by(|(left, li, lu), (right, ri, ru)| {
        ratio_cmp(*ri, *ru, *li, *lu).then_with(|| left.slug.cmp(&right.slug))
    });
    let Some((winner, winner_intersection, winner_union)) = candidates.first() else {
        anyhow::bail!(
            "no frozen dataset accepts family {family}, group {:?}, and fields {:?}; \
             this requires unsupported dataset/schema evolution (use a new prefix)",
            group,
            sorted_field_names(fields)
        );
    };
    if let Some((runner_up, runner_up_intersection, runner_up_union)) = candidates.get(1) {
        anyhow::ensure!(
            ratio_cmp(
                *winner_intersection,
                *winner_union,
                *runner_up_intersection,
                *runner_up_union
            ) != Ordering::Equal,
            "new file matches frozen datasets {} and {} equally for family {family}, group {:?}, \
             and fields {:?}; refusing ambiguous assignment (use a new prefix)",
            winner.slug,
            runner_up.slug,
            group,
            sorted_field_names(fields)
        );
    }
    Ok(winner.slug.clone())
}

fn field_overlap(fields: &HashMap<String, FieldAcc>, specs: &[FieldSpec]) -> (usize, usize) {
    // Keep null-only and object-shaped fields in the schema fingerprint too.
    // FieldAcc deliberately does not count those as scalar observations, but
    // allowing an unknown one through would let the server create a dynamic
    // mapping outside the frozen plan.
    let observed: HashSet<&str> = fields.keys().map(String::as_str).collect();
    let frozen: HashSet<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    (
        observed.intersection(&frozen).count(),
        observed.union(&frozen).count().max(1),
    )
}

fn ratio_cmp(left_n: usize, left_d: usize, right_n: usize, right_d: usize) -> Ordering {
    (left_n * right_d).cmp(&(right_n * left_d))
}

fn ensure_compatible(fields: &HashMap<String, FieldAcc>, dataset: &PlanDataset) -> Result<()> {
    let specs: HashMap<&str, &FieldSpec> = dataset
        .specs
        .iter()
        .map(|spec| (spec.name.as_str(), spec))
        .collect();
    for (name, acc) in fields {
        let Some(spec) = specs.get(name.as_str()) else {
            // A field observed with NO scalar values (`acc.n == 0`) — e.g. the
            // code file's `symbols` sidecar (an array of objects, which
            // `FieldAcc` deliberately does not scalar-type) — legitimately never
            // received a frozen `FieldSpec` at genesis: it has nothing to type.
            // Its absence is therefore not drift; there is nothing to validate,
            // so mirror `compatible_with_spec`'s `acc.n == 0 => true` and skip
            // it. Without this, re-indexing ANY code file aborted the whole run
            // (#580), because `symbols` is observed on every generation but was
            // pruned from the frozen schema. A field that DOES carry scalar
            // values yet is absent is still a real incompatibility.
            anyhow::ensure!(
                acc.n == 0,
                "field {name} is absent from frozen dataset {}",
                dataset.slug
            );
            continue;
        };
        anyhow::ensure!(
            compatible_with_spec(acc, spec),
            "field {name} is incompatible with frozen {} mapping in dataset {}",
            spec.es_type,
            dataset.slug
        );
    }
    Ok(())
}

fn compatible_with_spec(acc: &FieldAcc, spec: &FieldSpec) -> bool {
    if acc.n == 0 {
        return true;
    }
    let at_least_95 = |accepted: u64| accepted.saturating_mul(100) >= acc.n.saturating_mul(95);
    match spec.es_type.as_str() {
        "boolean" => at_least_95(acc.bool_ok),
        "long" => at_least_95(acc.long_ok),
        "double" => at_least_95(acc.double_ok),
        "date" => {
            let string_dates = acc.date_hits.values().copied().sum::<u64>();
            let numeric_dates = match spec.date_enc.as_deref() {
                Some("epoch_millis" | "epoch_seconds") => acc.long_ok,
                _ => 0,
            };
            at_least_95(string_dates.saturating_add(numeric_dates).min(acc.n))
        }
        "keyword" | "text" | "semantic_text" => true,
        _ => false,
    }
}

fn sorted_field_names(fields: &HashMap<String, FieldAcc>) -> Vec<&str> {
    let mut names = fields.keys().map(String::as_str).collect::<Vec<_>>();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff::{Family, Sniffed};
    use crate::state::{DuplicateFile, PlanDataset};
    use crate::walk::FileEntry;
    use serde_json::json;
    use std::path::PathBuf;

    fn acc(values: &[serde_json::Value]) -> FieldAcc {
        let mut acc = FieldAcc::default();
        for value in values {
            acc.add(value);
        }
        acc
    }

    fn spec(name: &str, es_type: &str) -> FieldSpec {
        FieldSpec {
            name: name.into(),
            es_type: es_type.into(),
            date_enc: None,
            semantic: None,
            cardinality_est: 0,
            cardinality_overflow: false,
            null_ratio: 0.0,
            avg_len: 0.0,
            coverage: 1.0,
            examples: vec![],
            notes: vec![],
            date_min: None,
            date_max: None,
            date_evidence: vec![],
        }
    }

    fn dataset(slug: &str, specs: Vec<FieldSpec>) -> PlanDataset {
        PlanDataset {
            slug: slug.into(),
            index: format!("ax-{slug}"),
            family: "jsonl".into(),
            group: None,
            specs,
            time_field: None,
            semantic_field: None,
            sampled_records: 1,
            file_count: 1,
        }
    }

    fn file(rel: &str, path_id: &str) -> FileEntry {
        FileEntry {
            path: PathBuf::from(rel),
            rel: rel.into(),
            rel_id: path_id.into(),
            is_symlink: false,
            size: 10,
        }
    }

    fn scan(fields: HashMap<String, FieldAcc>) -> FileScan {
        FileScan {
            sniffed: Some(Sniffed {
                family: Family::Jsonl,
                gzip: false,
                binary_kind: None,
                csv: None,
                encoding: "utf-8",
                logical_name: None,
            }),
            sketches: vec![crate::GroupSketch {
                group: None,
                fields,
                key_fields: std::collections::HashSet::new(),
                records: 1,
            }],
            junk: None,
            // Reconciliation re-samples from the source tree and publishes
            // through the sealed snapshot, so it never carries a run-local
            // PDF artifact (#248 is a phase A→B accelerator).
            pdf_spool: None,
            pdf_spool_fallbacks: Vec::new(),
        }
    }

    fn inventory(rel: &str, path_id: &str, key: &str) -> Inventory {
        Inventory {
            files: vec![file(rel, path_id)],
            keys: vec![key.into()],
            digests: vec![format!("digest-{key}")],
            duplicates: vec![],
        }
    }

    fn assignment(rel: &str, path_id: &str, slug: &str) -> FileAssignment {
        FileAssignment {
            rel: rel.into(),
            path_id: path_id.into(),
            is_symlink: Some(false),
            family: "jsonl".into(),
            gzip: false,
            content_digest: Some("old-digest".into()),
            assignments: vec![(None, slug.into())],
            as_document: false,
        }
    }

    #[test]
    fn same_path_replacement_retains_dataset_and_never_re_elects_slug() {
        let mut previous = Plan {
            datasets: vec![
                dataset("z-owner", vec![spec("id", "long"), spec("body", "text")]),
                dataset("a-other", vec![spec("id", "long"), spec("body", "text")]),
            ],
            ..Plan::default()
        };
        previous
            .files
            .insert("old".into(), assignment("a.jsonl", "unix:61", "z-owner"));
        let fields = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("body".into(), acc(&[json!("hello world")])),
        ]);
        let plan = reconcile_plan(
            &inventory("a.jsonl", "unix:61", "new"),
            &previous,
            vec![scan(fields)],
            50,
        )
        .unwrap();
        assert_eq!(
            plan.files["new"].assignments,
            vec![(None, "z-owner".into())]
        );
    }

    #[test]
    fn new_file_uses_unique_highest_jaccard_match() {
        let previous = Plan {
            datasets: vec![
                dataset("winner", vec![spec("id", "long"), spec("body", "text")]),
                dataset(
                    "lower",
                    vec![
                        spec("id", "long"),
                        spec("body", "text"),
                        spec("extra", "keyword"),
                    ],
                ),
            ],
            ..Plan::default()
        };
        let fields = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("body".into(), acc(&[json!("hello")])),
        ]);
        let plan = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(fields)],
            50,
        )
        .unwrap();
        assert_eq!(plan.files["new"].assignments, vec![(None, "winner".into())]);
        assert_eq!(plan.datasets[0].file_count, 1);
        assert_eq!(plan.datasets[1].file_count, 0);
    }

    #[test]
    fn new_file_with_equal_best_schema_matches_fails_closed() {
        let previous = Plan {
            datasets: vec![
                dataset("z-tie", vec![spec("id", "long"), spec("body", "text")]),
                dataset("a-tie", vec![spec("id", "long"), spec("body", "text")]),
            ],
            ..Plan::default()
        };
        let fields = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("body".into(), acc(&[json!("hello")])),
        ]);
        let error = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(fields)],
            50,
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("refusing ambiguous assignment"),
            "{message}"
        );
        assert!(message.contains("a-tie"), "{message}");
        assert!(message.contains("z-tie"), "{message}");
    }

    #[test]
    fn new_field_and_incompatible_type_fail_before_plan_changes() {
        let previous = Plan {
            datasets: vec![dataset("rows", vec![spec("id", "long")])],
            ..Plan::default()
        };
        let unknown = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("surprise".into(), acc(&[json!("new mapping")])),
        ]);
        let error = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(unknown)],
            50,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported dataset/schema evolution"));

        // Object-shaped values are not counted as scalar FieldAcc evidence,
        // but their field name must still be rejected rather than dynamically
        // mapped by the server.
        let object_field = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("object".into(), acc(&[json!({"nested": true})])),
        ]);
        let error = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(object_field)],
            50,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported dataset/schema evolution"));

        let wrong_type = HashMap::from([("id".into(), acc(&[json!("not-a-number")]))]);
        let error = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(wrong_type)],
            50,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported dataset/schema evolution"));
    }

    /// A NEW document file (empty `key_fields`, no group — the shape a fresh
    /// plan folds into the scope's docs dataset, dataset.rs:119) added to an
    /// already-indexed folder must join the frozen `docs` dataset on the
    /// `--no-graph` incremental path, not be compared by its raw family
    /// (`txt-prose`, `code`, …) — which matches no frozen dataset and aborts
    /// the whole run ("use a new prefix"). It is NOT `as_document` (a prose
    /// document indexes through its own extractor; only demoted configs
    /// re-sample). Its rendered fields are a SUBSET of the docs dataset's
    /// union, so `classify_new` must accept a docs-family subset.
    #[test]
    fn new_document_file_joins_the_docs_dataset_on_incremental() {
        let previous = Plan {
            datasets: vec![PlanDataset {
                family: "docs".into(),
                ..dataset("docs", vec![spec("title", "text"), spec("body", "text")])
            }],
            ..Plan::default()
        };
        // A prose document: `title`/`body` (a subset of the frozen docs union),
        // empty key_fields (the `scan` helper default), family txt-prose.
        let fields = HashMap::from([
            ("title".to_string(), acc(&[json!("Runbook")])),
            (
                "body".to_string(),
                acc(&[json!("promote the standby with pg_ctl")]),
            ),
        ]);
        let mut file_scan = scan(fields);
        file_scan.sniffed.as_mut().unwrap().family = Family::TxtProse;

        let plan = reconcile_plan(
            &inventory("notes.txt", "unix:7b", "doc"),
            &previous,
            vec![file_scan],
            50,
        )
        .unwrap();
        assert_eq!(
            plan.files["doc"].assignments,
            vec![(None, "docs".into())],
            "a new prose document must join the frozen docs dataset, not abort on family txt-prose"
        );
        assert!(
            !plan.files["doc"].as_document,
            "a prose document indexes through its own extractor, not as_document"
        );
    }

    /// A NEW demotable one-off config (single-record JSON/YAML/XML, non-empty
    /// key_fields) joins the frozen docs dataset AND re-samples `as_document` —
    /// even when the docs dataset carries MORE fields than the config renders
    /// (a realistic docs folder of multi-section markdown adds `section`). The
    /// config re-samples to `{title, body}`, a SUBSET of the docs union, which
    /// the old 0.7-Jaccard threshold rejected (aborting the run) — the
    /// docs-subset relaxation in `classify_new` fixes it.
    #[test]
    fn new_demotable_config_joins_a_richer_docs_dataset_as_a_document() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, "{\"host\":\"localhost\",\"port\":8080}").unwrap();
        let previous = Plan {
            datasets: vec![PlanDataset {
                family: "docs".into(),
                ..dataset(
                    "docs",
                    vec![
                        spec("title", "text"),
                        spec("body", "text"),
                        spec("section", "text"),
                    ],
                )
            }],
            ..Plan::default()
        };
        let fields = HashMap::from([
            ("host".to_string(), acc(&[json!("localhost")])),
            ("port".to_string(), acc(&[json!(8080)])),
        ]);
        let mut file_scan = scan(fields);
        file_scan.sniffed.as_mut().unwrap().family = Family::Json;
        file_scan.sketches[0].key_fields = ["host".to_string(), "port".to_string()]
            .into_iter()
            .collect();
        let inv = Inventory {
            files: vec![FileEntry {
                path: cfg_path.clone(),
                rel: "config.json".into(),
                rel_id: "unix:7c".into(),
                is_symlink: false,
                size: 32,
            }],
            keys: vec!["cfg".into()],
            digests: vec!["digest-cfg".into()],
            duplicates: vec![],
        };
        let plan = reconcile_plan(&inv, &previous, vec![file_scan], 50).unwrap();
        assert_eq!(plan.files["cfg"].assignments, vec![(None, "docs".into())]);
        assert!(
            plan.files["cfg"].as_document,
            "a demoted config re-samples as a document"
        );
    }

    /// #729: a code file emits a `symbols` sidecar (array-of-objects, #580)
    /// that `FieldAcc` records with `acc.n == 0`, so it is pruned from every
    /// frozen docs `FieldSpec`. A NEW code file carrying it must still join the
    /// docs dataset on the incremental path — `ensure_compatible` tolerates the
    /// pruned key, so the docs-family acceptance must not re-count it (the
    /// earlier `intersection == fields.len()` did, and aborted every new
    /// `.rs`/`.py` with functions — the central #729 case for code repos).
    #[test]
    fn new_code_file_with_a_pruned_symbols_sidecar_joins_docs_not_aborts() {
        let previous = Plan {
            datasets: vec![PlanDataset {
                family: "docs".into(),
                ..dataset(
                    "docs",
                    vec![
                        spec("title", "text"),
                        spec("language", "keyword"),
                        spec("body", "text"),
                        spec("defs", "text"),
                        spec("symbol_count", "long"),
                    ],
                )
            }],
            ..Plan::default()
        };
        let fields = HashMap::from([
            ("title".to_string(), acc(&[json!("alpha.rs")])),
            ("language".to_string(), acc(&[json!("rust")])),
            ("body".to_string(), acc(&[json!("fn main() {}")])),
            ("defs".to_string(), acc(&[json!("struct Alpha")])),
            ("symbol_count".to_string(), acc(&[json!(3)])),
            // Observed key, object-valued -> acc.n == 0, pruned from the frozen
            // specs — exactly the shape that aborted before the fix.
            ("symbols".to_string(), acc(&[json!({"name": "main"})])),
        ]);
        let mut file_scan = scan(fields);
        file_scan.sniffed.as_mut().unwrap().family = Family::Code;

        let plan = reconcile_plan(
            &inventory("alpha.rs", "unix:7d", "code"),
            &previous,
            vec![file_scan],
            50,
        )
        .unwrap();
        assert_eq!(plan.files["code"].assignments, vec![(None, "docs".into())]);
        assert!(
            !plan.files["code"].as_document,
            "a code document indexes through its own extractor, not as_document"
        );
    }

    #[test]
    fn rename_by_content_retains_assignment_and_rebuilds_aliases_and_counts() {
        let mut previous = Plan {
            datasets: vec![dataset("rows", vec![spec("id", "long")])],
            ..Plan::default()
        };
        previous
            .files
            .insert("same".into(), assignment("old.jsonl", "unix:6f", "rows"));
        let mut current = inventory("renamed.jsonl", "unix:72", "same");
        current.duplicates.push(DuplicateFile {
            file_key: "same".into(),
            rel: "alias.jsonl".into(),
            path_id: "unix:61".into(),
            is_symlink: Some(false),
            duplicate_of: "renamed.jsonl".into(),
            bytes: 10,
        });
        let fields = HashMap::from([("id".into(), acc(&[json!(7)]))]);
        let plan = reconcile_plan(&current, &previous, vec![scan(fields)], 50).unwrap();
        assert_eq!(plan.files["same"].rel, "renamed.jsonl");
        assert_eq!(plan.datasets[0].file_count, 1);
        assert_eq!(plan.duplicate_files.len(), 1);
    }

    #[test]
    fn duplicate_content_identity_and_conflicting_owners_fail_closed() {
        let mut previous = Plan {
            datasets: vec![dataset("rows", vec![spec("id", "long")])],
            ..Plan::default()
        };
        previous
            .files
            .insert("content-a".into(), assignment("a.jsonl", "unix:61", "rows"));
        previous
            .files
            .insert("content-b".into(), assignment("b.jsonl", "unix:62", "rows"));
        let fields = || HashMap::from([("id".into(), acc(&[json!(7)]))]);

        let conflict = inventory("a.jsonl", "unix:62", "content-a");
        let error = reconcile_plan(&conflict, &previous, vec![scan(fields())], 50).unwrap_err();
        assert!(format!("{error:#}").contains("conflicting committed content and path owners"));

        let duplicate = Inventory {
            files: vec![file("c.jsonl", "unix:63"), file("d.jsonl", "unix:64")],
            keys: vec!["same-key".into(), "same-key".into()],
            digests: vec!["digest".into(), "digest".into()],
            duplicates: vec![],
        };
        let error = reconcile_plan(
            &duplicate,
            &previous,
            vec![scan(fields()), scan(fields())],
            50,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("duplicate content identity"));
    }
}
