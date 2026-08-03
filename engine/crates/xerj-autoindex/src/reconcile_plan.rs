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

/// Project the current, byte-verified inventory onto committed dataset schemas.
///
/// Assignment precedence is stable content identity, then stable native path
/// identity, then deterministic schema matching. Dataset definitions are
/// copied byte-for-byte except for their derived `file_count`.
pub(crate) fn reconcile_plan(
    inventory: &Inventory,
    previous: &Plan,
    scans: Vec<FileScan>,
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

        let mut assignments = Vec::with_capacity(scan.sketches.len());
        for (group, fields, _records) in &scan.sketches {
            let slug = if let Some(owner) = retained {
                retained_slug(owner, group, sniffed.family.as_str(), fields, &datasets)
                    .with_context(|| format!("project retained file {}", file.rel))?
            } else {
                classify_new(group, sniffed.family.as_str(), fields, &previous.datasets)
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
        let threshold_met = if group.is_some() {
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
    candidates
        .first()
        .map(|(dataset, _, _)| dataset.slug.clone())
        .with_context(|| {
            format!(
                "no frozen dataset accepts family {family}, group {:?}, and fields {:?}; \
                 this requires unsupported dataset/schema evolution (use a new prefix)",
                group,
                sorted_field_names(fields)
            )
        })
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
        let spec = specs.get(name.as_str()).with_context(|| {
            format!(
                "field {name} is absent from frozen dataset {}",
                dataset.slug
            )
        })?;
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
            }),
            sketches: vec![(None, fields, 1)],
            junk: None,
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
        )
        .unwrap();
        assert_eq!(
            plan.files["new"].assignments,
            vec![(None, "z-owner".into())]
        );
    }

    #[test]
    fn new_file_uses_highest_jaccard_then_stable_slug_tie_break() {
        let previous = Plan {
            datasets: vec![
                dataset("z-tie", vec![spec("id", "long"), spec("body", "text")]),
                dataset("a-tie", vec![spec("id", "long"), spec("body", "text")]),
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
        )
        .unwrap();
        assert_eq!(plan.files["new"].assignments, vec![(None, "a-tie".into())]);
        assert_eq!(plan.datasets[0].file_count, 0);
        assert_eq!(plan.datasets[1].file_count, 1);
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
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported dataset/schema evolution"));

        let wrong_type = HashMap::from([("id".into(), acc(&[json!("not-a-number")]))]);
        let error = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(wrong_type)],
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("unsupported dataset/schema evolution"));
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
        let plan = reconcile_plan(&current, &previous, vec![scan(fields)]).unwrap();
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
        let error = reconcile_plan(&conflict, &previous, vec![scan(fields())]).unwrap_err();
        assert!(format!("{error:#}").contains("conflicting committed content and path owners"));

        let duplicate = Inventory {
            files: vec![file("c.jsonl", "unix:63"), file("d.jsonl", "unix:64")],
            keys: vec!["same-key".into(), "same-key".into()],
            digests: vec!["digest".into(), "digest".into()],
            duplicates: vec![],
        };
        let error = reconcile_plan(&duplicate, &previous, vec![scan(fields()), scan(fields())])
            .unwrap_err();
        assert!(format!("{error:#}").contains("duplicate content identity"));
    }
}
