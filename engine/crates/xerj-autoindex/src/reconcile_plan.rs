//! Deterministic projection of a changed inventory onto a frozen typed plan.
//!
//! Fresh-run clustering is intentionally not reused here: cluster formation
//! and slug election depend on corpus order and membership. Incremental runs
//! keep the committed dataset identity. A new scalar field on a file that
//! already belongs to one of those datasets is added to that dataset's
//! mapping — the server accepts an additive `PUT _mapping` — so a code file
//! that grows a function (`code`, `defs`, `name`, …) does not wedge the
//! watcher (#2176). A change that cannot be applied in place (a type
//! conflict, a brand-new dataset, an ambiguous assignment) drops only that
//! file and names the rebuild command; the rest of the pass still commits.

use crate::content::Inventory;
use crate::infer::{FieldAcc, FieldSpec};
use crate::state::{FileAssignment, JunkFile, Plan, PlanDataset, RefusedDataset};
use crate::FileScan;
use anyhow::{Context, Result};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
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
/// identity, then deterministic schema matching. A dataset keeps its committed
/// specs — those are the generation identity — and `file_count` is recomputed.
/// A scalar field the specs do not name is recorded beside them and installed
/// by the mapping update (#2176).
pub(crate) fn reconcile_plan(
    inventory: &Inventory,
    previous: &Plan,
    scans: Vec<FileScan>,
    sample: usize,
    rebuild_command: &str,
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

    // #929: a dataset the server refused at genesis stays refused. Its files
    // are junk in the committed plan, so nothing above remembers them by path,
    // and left to the ordinary projection they would be re-classified as NEW
    // files needing a dataset that does not exist — which fails closed and
    // aborts the whole run ("use a new prefix"). That would turn "one dataset
    // was refused" into "this corpus can never be re-indexed", so refusals are
    // projected explicitly: by content identity when the file is unchanged, and
    // by schema (the refused definition competes as a classification candidate)
    // when a new or changed file has the refused shape.
    let refused_by_key: HashMap<&str, usize> = previous
        .refused_datasets
        .iter()
        .enumerate()
        .flat_map(|(owner, refusal)| {
            refusal
                .file_keys
                .iter()
                .map(move |key| (key.as_str(), owner))
        })
        .collect();
    let refused_by_slug: HashMap<&str, usize> = previous
        .refused_datasets
        .iter()
        .enumerate()
        .map(|(owner, refusal)| (refusal.dataset.slug.as_str(), owner))
        .collect();
    anyhow::ensure!(
        refused_by_slug.len() == previous.refused_datasets.len()
            && refused_by_slug
                .keys()
                .all(|slug| !datasets.contains_key(slug)),
        "frozen plan lists a refused dataset twice, or both refuses and publishes one"
    );
    let previous_junk: HashMap<&str, &JunkFile> = previous
        .junk_files
        .iter()
        .map(|junk| (junk.file_key.as_str(), junk))
        .collect();
    // Specs stay frozen. A new scalar is recorded in `incremental_fields` and
    // installed by the mapping update; writing it into `specs` would move the
    // generation identity and abort the pass. Refused definitions are not
    // widened — growing a mapping the server already rejected cannot help.
    let mut owned_datasets = previous.datasets.clone();
    let mut incremental_fields = previous.incremental_fields.clone();
    let refused_datasets_frozen: Vec<&PlanDataset> = previous
        .refused_datasets
        .iter()
        .map(|refusal| &refusal.dataset)
        .collect();
    let mut refused_keys: Vec<Vec<String>> = vec![Vec::new(); previous.refused_datasets.len()];

    let mut files = HashMap::new();
    let mut junk_files = Vec::new();
    for (((file, content_id), content_digest), scan) in inventory
        .files
        .iter()
        .zip(&inventory.keys)
        .zip(&inventory.digests)
        .zip(scans)
    {
        // Incremental fast path: content byte-identical to the committed plan
        // (content_id present AND its freshly-hashed digest matches the
        // committed one) carries its committed assignment forward verbatim — no
        // fresh scan required. This is the correctness authority for the
        // scan-skip in `project_reconcile_plan`: because the digest is the hash
        // of the CURRENT bytes, a match proves byte identity; a changed file
        // whose scan was (wrongly) skipped mismatches here, falls through to the
        // normal path, and fails closed on empty sketches rather than carrying
        // stale docs forward. A legacy plan without a committed digest
        // (`content_digest: None`) never matches, so it re-scans safely.
        if let Some(owner) = previous.files.get(content_id) {
            if owner.content_digest.as_deref() == Some(content_digest.as_str()) {
                let mut assignments = owner.assignments.clone();
                assignments.sort();
                assignments.dedup();
                let replaced = files.insert(
                    content_id.clone(),
                    FileAssignment {
                        rel: file.rel.clone(),
                        path_id: file.rel_id.clone(),
                        is_symlink: Some(file.is_symlink),
                        family: owner.family.clone(),
                        gzip: owner.gzip,
                        content_digest: Some(content_digest.clone()),
                        assignments,
                        as_document: owner.as_document,
                    },
                );
                anyhow::ensure!(
                    replaced.is_none(),
                    "byte-verified inventory contains duplicate content identity {content_id}"
                );
                continue;
            }
        }
        // Same content id as a file the refusal already cost: carry the junk
        // entry forward verbatim (only the path may have moved), so a no-op
        // re-run projects a byte-identical plan and commits nothing.
        if let Some(&owner) = refused_by_key.get(content_id.as_str()) {
            junk_files.push(JunkFile {
                file_key: content_id.clone(),
                rel: file.rel.clone(),
                format: previous_junk.get(content_id.as_str()).map_or_else(
                    || crate::format_str(scan.sniffed.as_ref()),
                    |junk| junk.format.clone(),
                ),
                status: "junk".into(),
                reason: previous.refused_datasets[owner].junk_reason(),
                bytes: file.size,
            });
            refused_keys[owner].push(content_id.clone());
            continue;
        }
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
        let mut file_reject: Option<String> = None;
        for sketch in &scan.sketches {
            let group = &sketch.group;
            let fields = document_fields.as_ref().unwrap_or(&sketch.fields);
            let assigned = if let Some(owner) = retained {
                retained_slug(
                    owner,
                    group,
                    sniffed.family.as_str(),
                    fields,
                    &owned_datasets,
                    &incremental_fields,
                )
            } else {
                // A routed document/config classifies as "docs" — its fresh-plan
                // fold target — instead of aborting on its raw family.
                let family = if route_to_docs {
                    "docs"
                } else {
                    sniffed.family.as_str()
                };
                classify_new(
                    group,
                    family,
                    fields,
                    &file.rel,
                    &owned_datasets,
                    &refused_datasets_frozen,
                    &incremental_fields,
                )
            };
            match assigned {
                Ok(slug) => assignments.push((group.clone(), slug, fields, sketch.records)),
                Err(AssignFailure::File(detail)) => {
                    file_reject = Some(detail);
                    break;
                }
                Err(AssignFailure::Fatal(error)) => {
                    return Err(error.context(format!("project file {}", file.rel)));
                }
            }
        }
        if let Some(detail) = file_reject {
            junk_files.push(JunkFile {
                file_key: content_id.clone(),
                rel: file.rel.clone(),
                format: crate::format_str(scan.sniffed.as_ref()),
                status: "junk".into(),
                reason: file_schema_reason(&detail, retained.is_some(), rebuild_command),
                bytes: file.size,
            });
            continue;
        }
        // A new or changed file whose shape belongs to a refused dataset is
        // refused with it — whole, for the reason `refuse_datasets` gives: a
        // partially assigned file cannot be prepared. Attributed to the first
        // refused slug so the choice does not depend on sketch order.
        if let Some(owner) = assignments
            .iter()
            .filter_map(|(_, slug, _, _)| refused_by_slug.get(slug.as_str()).copied())
            .min_by_key(|owner| previous.refused_datasets[*owner].dataset.slug.as_str())
        {
            junk_files.push(JunkFile {
                file_key: content_id.clone(),
                rel: file.rel.clone(),
                format: crate::format_str(scan.sniffed.as_ref()),
                status: "junk".into(),
                reason: previous.refused_datasets[owner].junk_reason(),
                bytes: file.size,
            });
            refused_keys[owner].push(content_id.clone());
            continue;
        }
        // Widen only after every sketch has a published dataset, and roll the
        // appends back if a later sketch cannot grow the mapping — a file is
        // assigned whole or not at all. The frozen specs are not touched.
        let mut widened_to: HashMap<String, usize> = HashMap::new();
        let mut widen_reject: Option<String> = None;
        for (_, slug, fields, records) in &assignments {
            let Some(index) = owned_datasets
                .iter()
                .position(|dataset| dataset.slug == *slug)
            else {
                continue;
            };
            let before = incremental_fields.get(slug.as_str()).map_or(0, Vec::len);
            match widen_dataset(
                &owned_datasets[index],
                incremental_fields.entry(slug.clone()).or_default(),
                fields,
                *records,
            ) {
                Ok(()) => {
                    if incremental_fields.get(slug.as_str()).map_or(0, Vec::len) > before {
                        widened_to.entry(slug.clone()).or_insert(before);
                    }
                }
                Err(AssignFailure::File(detail)) => {
                    rollback_widens(&mut incremental_fields, &widened_to);
                    widen_reject = Some(detail);
                    break;
                }
                Err(AssignFailure::Fatal(error)) => {
                    return Err(error.context(format!("project file {}", file.rel)));
                }
            }
        }
        if let Some(detail) = widen_reject {
            junk_files.push(JunkFile {
                file_key: content_id.clone(),
                rel: file.rel.clone(),
                format: crate::format_str(scan.sniffed.as_ref()),
                status: "junk".into(),
                reason: file_schema_reason(&detail, retained.is_some(), rebuild_command),
                bytes: file.size,
            });
            continue;
        }
        let mut assignments: Vec<(Option<String>, String)> = assignments
            .into_iter()
            .map(|(group, slug, _, _)| (group, slug))
            .collect();
        assignments.sort();
        assignments.dedup();
        if assignments.len() != scan.sketches.len() {
            junk_files.push(JunkFile {
                file_key: content_id.clone(),
                rel: file.rel.clone(),
                format: crate::format_str(scan.sniffed.as_ref()),
                status: "junk".into(),
                reason: file_schema_reason(
                    &format!("{} has duplicate dataset groups", file.rel),
                    retained.is_some(),
                    rebuild_command,
                ),
                bytes: file.size,
            });
            // The widens above belong to a file that is not indexed.
            rollback_widens(&mut incremental_fields, &widened_to);
            continue;
        }
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

    for dataset in &mut owned_datasets {
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

    // The refused DEFINITIONS are frozen exactly like the published ones; only
    // their membership is a projection of the current folder.
    let refused_datasets = previous
        .refused_datasets
        .iter()
        .zip(refused_keys)
        .map(|(refusal, mut file_keys)| {
            file_keys.sort();
            file_keys.dedup();
            RefusedDataset {
                dataset: refusal.dataset.clone(),
                reason: refusal.reason.clone(),
                file_keys,
            }
        })
        .collect();

    incremental_fields.retain(|_, specs| !specs.is_empty());

    Ok(Plan {
        datasets: owned_datasets,
        files,
        junk_files,
        duplicate_files,
        alias_paths_indexed: previous.alias_paths_indexed,
        refused_datasets,
        incremental_fields,
    })
}

/// A per-file projection failure, or a frozen plan that is internally inconsistent.
enum AssignFailure {
    /// This file cannot join the frozen mapping. The rest of the pass can.
    File(String),
    /// The committed plan itself is contradictory. Abort; do not publish a
    /// partial projection of a corrupt journal.
    Fatal(anyhow::Error),
}

impl From<anyhow::Error> for AssignFailure {
    fn from(error: anyhow::Error) -> Self {
        AssignFailure::Fatal(error)
    }
}

fn retained_slug(
    owner: &FileAssignment,
    group: &Option<String>,
    family: &str,
    fields: &HashMap<String, FieldAcc>,
    datasets: &[PlanDataset],
    incremental: &BTreeMap<String, Vec<FieldSpec>>,
) -> Result<String, AssignFailure> {
    if owner.family != family {
        return Err(AssignFailure::File(format!(
            "detected family changed from {} to {family}",
            owner.family
        )));
    }
    let matches = owner
        .assignments
        .iter()
        .filter(|(assigned_group, _)| assigned_group == group)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(AssignFailure::Fatal(anyhow::anyhow!(
            "frozen assignment has {} matches for group {:?}",
            matches.len(),
            group
        )));
    }
    let slug = &matches[0].1;
    let dataset = datasets
        .iter()
        .find(|dataset| dataset.slug == *slug)
        .with_context(|| format!("frozen assignment references absent dataset {slug}"))?;
    let extra = incremental
        .get(slug.as_str())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    ensure_compatible(fields, dataset, extra)?;
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
    rel: &str,
    datasets: &[PlanDataset],
    refused: &[&PlanDataset],
    incremental: &BTreeMap<String, Vec<FieldSpec>>,
) -> Result<String, AssignFailure> {
    let mut candidates = Vec::new();
    let mut rejected: Option<String> = None;
    for dataset in datasets
        .iter()
        .chain(refused.iter().copied())
        .filter(|dataset| dataset.family == family && &dataset.group == group)
    {
        let extra = incremental
            .get(dataset.slug.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        match ensure_compatible(fields, dataset, extra) {
            Ok(()) => {}
            Err(AssignFailure::File(detail)) => {
                rejected = Some(detail);
                continue;
            }
            Err(failure @ AssignFailure::Fatal(_)) => return Err(failure),
        }
        let affinity = scope_affinity(dataset.scope.as_deref(), rel);
        let (intersection, union) = field_overlap(fields, &dataset.specs);
        let threshold_met = if dataset.family == "docs" {
            // A `docs` dataset's specs are the UNION of every document/config
            // the fold produced. `ensure_compatible` (run just above) accepts a
            // scalar field the frozen specs do not name yet — `widen_dataset`
            // records it beside the specs, and the mapping update installs it —
            // and still tolerates `acc.n == 0` (pruned: e.g. a
            // code file's object-valued `symbols` sidecar, #580). So a
            // document/config that reached here belongs to the docs dataset
            // regardless of how many other fields the union carries; accept
            // it. (The 0.7 Jaccard would wrongly reject a document with fewer
            // fields than the union, and `== fields.len()` wrongly counts the
            // pruned `symbols` key and aborted a new code file — the #729
            // central case.) `let _` keeps the shared `field_overlap` result
            // in scope for the data-family branches.
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
            candidates.push((dataset, intersection, union, affinity));
        }
    }
    // #1230: field overlap is the primary key, then repository scope. Two
    // sibling forks produce byte-identical `docs` schemas, so a newly added
    // file overlaps both equally and the schema ratio alone is a genuine tie —
    // the refusal this used to raise. The file's own path resolves it: a
    // dataset whose frozen scope *contains* the file (nearest `.git` ancestor
    // wins, hence longest match) is the one a fresh plan would have folded it
    // into. Scope only ever breaks a ratio tie; it can never promote a
    // worse-overlapping dataset over a better one.
    candidates.sort_by(|(left, li, lu, la), (right, ri, ru, ra)| {
        ratio_cmp(*ri, *ru, *li, *lu)
            .then_with(|| ra.cmp(la))
            .then_with(|| left.slug.cmp(&right.slug))
    });
    let Some((winner, winner_intersection, winner_union, winner_affinity)) = candidates.first()
    else {
        let because = rejected
            .as_deref()
            .map(|detail| format!(" Last candidate refused: {detail}"))
            .unwrap_or_default();
        return Err(AssignFailure::File(format!(
            "no frozen dataset accepts family {family}, group {:?}, and fields {:?}; \
             this requires unsupported dataset/schema evolution (use a new prefix).{because}",
            group,
            sorted_field_names(fields)
        )));
    };
    if let Some((runner_up, runner_up_intersection, runner_up_union, runner_up_affinity)) =
        candidates.get(1)
    {
        // Ambiguous only when neither field overlap NOR repository scope can
        // separate the top two. A pre-#1230 plan carries no scope (`None` for
        // every dataset), so both affinities are `None`, they compare equal,
        // and this fails closed exactly as before.
        if ratio_cmp(
            *winner_intersection,
            *winner_union,
            *runner_up_intersection,
            *runner_up_union,
        ) == Ordering::Equal
            && winner_affinity == runner_up_affinity
        {
            return Err(AssignFailure::File(format!(
                "new file matches frozen datasets {} and {} equally for family {family}, group {:?}, \
                 and fields {:?}; refusing ambiguous assignment (use a new prefix)",
                winner.slug,
                runner_up.slug,
                group,
                sorted_field_names(fields)
            )));
        }
    }
    Ok(winner.slug.clone())
}

/// How strongly a frozen dataset's repository scope lays claim to a file at
/// `rel` (#1230). `None` means the scope does not contain the file (or the
/// plan predates scope recording and carries no scope at all); a `Some` value
/// is the matched scope's byte length, so the *nearest* enclosing repository —
/// the longest owning scope, which is what `compute_scopes` picks at genesis —
/// ranks highest. The autoindex root (`Some("")`) owns every file but at the
/// lowest precedence, below any nested repository. `Option<usize>` already
/// orders `None < Some(0) < Some(len)`, which is exactly that precedence.
fn scope_affinity(scope: Option<&str>, rel: &str) -> Option<usize> {
    let scope = scope?;
    if scope.is_empty() {
        Some(0)
    } else if rel == scope
        || rel
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
    {
        Some(scope.len())
    } else {
        None
    }
}

fn field_overlap(fields: &HashMap<String, FieldAcc>, specs: &[FieldSpec]) -> (usize, usize) {
    // Keep null-only and object-shaped fields in the schema fingerprint too.
    // FieldAcc deliberately does not count those as scalar observations, but
    // allowing an unknown one through would let the server create a dynamic
    // mapping outside the frozen plan. A scalar the specs do not name yet is
    // also a mismatch here: `widen_dataset` adds it only after a file has
    // already been assigned (a retained file, or a docs file the fold accepts).
    // Dropping it from this ratio would make a dataset that lacks the field
    // look identical to one that has it, and scope would then pick the worse
    // one.
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

fn ensure_compatible(
    fields: &HashMap<String, FieldAcc>,
    dataset: &PlanDataset,
    extra: &[FieldSpec],
) -> Result<(), AssignFailure> {
    let mut specs: HashMap<&str, &FieldSpec> = dataset
        .specs
        .iter()
        .map(|spec| (spec.name.as_str(), spec))
        .collect();
    for spec in extra {
        specs.entry(spec.name.as_str()).or_insert(spec);
    }
    // Sorted so the reported field does not depend on hash-map order.
    let mut names: Vec<&str> = fields.keys().map(String::as_str).collect();
    names.sort_unstable();
    for name in names {
        let acc = &fields[name];
        let Some(spec) = specs.get(name) else {
            // A field observed with NO scalar values (`acc.n == 0`) — e.g. the
            // code file's `symbols` sidecar (an array of objects, which
            // `FieldAcc` deliberately does not scalar-type) — legitimately never
            // received a frozen `FieldSpec` at genesis: it has nothing to type.
            // Its absence is therefore not drift (#580).
            //
            // #1237 / #2351: `impl`, `end_line`, and `start_line` are installed
            // on every docs/code mapping by `dataset_create_body`, outside
            // the frozen specs. Re-inferring them here can elect `keyword`
            // for a short `impl` and the server then refuses to change the
            // existing `text` field. `start_line` is the same line `line`
            // already stores, so it must not consume a field-cap slot.
            //
            // Any other absent scalar with a name the mapping can grow is
            // added by `widen_dataset` (#2176). A name that cannot be added
            // (empty, dotted, leading `_`) is a per-file refusal: letting it
            // through would dynamic-map it.
            if absent_field_is_tolerated(name, acc, dataset) {
                continue;
            }
            if widenable_field_name(name) {
                continue;
            }
            // #1815: genesis already published this name as an unmapped
            // overflow field of a dataset that hit the field cap.
            if cap_overflow_fields(dataset).is_some_and(|overflow| overflow.contains(name)) {
                continue;
            }
            return Err(AssignFailure::File(format!(
                "field {name} is absent from frozen dataset {} and cannot be added to its mapping",
                dataset.slug
            )));
        };
        if !compatible_with_spec(acc, spec) {
            return Err(AssignFailure::File(format!(
                "field {name} is incompatible with frozen {} mapping in dataset {}",
                spec.es_type, dataset.slug
            )));
        }
    }
    Ok(())
}

/// `impl` / `end_line` / `start_line` and object-valued sidecars are already
/// accounted for and must not be re-inferred onto the frozen specs.
fn absent_field_is_tolerated(name: &str, acc: &FieldAcc, dataset: &PlanDataset) -> bool {
    if acc.n == 0 {
        return true;
    }
    (name == "impl" || name == "end_line" || name == "start_line")
        && (dataset.family == "docs" || dataset.family == "code")
}

/// A top-level field name `PUT /_mapping` can add without nesting or colliding
/// with metadata. Dotted names become nested properties; a leading `_` is
/// reserved. Anything else is refused per file instead of dynamic-mapped.
fn widenable_field_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The overflow field names genesis recorded when `infer` truncated the
/// dataset at [`crate::infer::MAX_FIELDS_PER_DATASET`], or `None` when the
/// dataset never hit the cap.
fn cap_overflow_fields(dataset: &PlanDataset) -> Option<HashSet<&str>> {
    dataset.specs.iter().find_map(|spec| {
        spec.notes.iter().find_map(|note| {
            note.strip_prefix(CAP_OVERFLOW_NOTE)
                .map(|list| list.split(", ").filter(|name| !name.is_empty()).collect())
        })
    })
}

const CAP_OVERFLOW_NOTE: &str = "dataset field cap hit; unmapped overflow fields: ";

fn widen_dataset(
    dataset: &PlanDataset,
    added: &mut Vec<FieldSpec>,
    fields: &HashMap<String, FieldAcc>,
    records: u64,
) -> Result<(), AssignFailure> {
    let mut known: HashSet<&str> = dataset
        .specs
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    known.extend(added.iter().map(|spec| spec.name.as_str()));
    // #1815: a dataset that hit the field cap at genesis has an explicitly
    // partial schema. Its overflow fields were published unmapped and pass
    // through coercion (`coerce.rs`). A changed file that re-observes them, or
    // grows another field, is the same shape genesis accepted, not drift.
    let overflow = cap_overflow_fields(dataset);
    let mut pending: HashMap<String, FieldAcc> = HashMap::new();
    let mut names: Vec<&str> = fields.keys().map(String::as_str).collect();
    names.sort_unstable();
    for name in names {
        let acc = &fields[name];
        if known.contains(name) || absent_field_is_tolerated(name, acc, dataset) || acc.n == 0 {
            continue;
        }
        if let Some(overflow) = &overflow {
            if overflow.contains(name) || widenable_field_name(name) {
                // Unmapped, exactly like the genesis overflow: the cap leaves
                // no room to map it, and refusing it would drop the file.
                continue;
            }
        }
        if !widenable_field_name(name) {
            return Err(AssignFailure::File(format!(
                "field {name} is absent from frozen dataset {} and cannot be added to its mapping",
                dataset.slug
            )));
        }
        pending.insert(name.to_string(), acc.clone());
    }
    if pending.is_empty() {
        return Ok(());
    }
    if dataset.specs.len() + added.len() + pending.len() > crate::infer::MAX_FIELDS_PER_DATASET {
        let pending_len = pending.len();
        return Err(AssignFailure::File(format!(
            "dataset {} already maps {} fields; adding {pending_len} more would pass the {} field cap",
            dataset.slug,
            dataset.specs.len() + added.len(),
            crate::infer::MAX_FIELDS_PER_DATASET
        )));
    }
    // `no_semantic`: a new field must not elect `semantic_text`. That would
    // change the dataset's embedding contract without updating `semantic_field`,
    // and a fresh mapping install would then refuse or embed a field this
    // generation does not declare as the body. Lexical text/keyword/long still
    // search.
    let mut inferred = crate::infer::infer_fields(&pending, records.max(1), true);
    for spec in &mut inferred {
        if spec.es_type == "semantic_text" {
            spec.es_type = "text".into();
        }
        spec.notes
            .push("added on incremental reconcile when the field first appeared".into());
    }
    added.extend(inferred);
    Ok(())
}

fn rollback_widens(
    fields: &mut BTreeMap<String, Vec<FieldSpec>>,
    widened: &HashMap<String, usize>,
) {
    for (slug, len) in widened {
        if let Some(specs) = fields.get_mut(slug) {
            specs.truncate(*len);
        }
    }
    fields.retain(|_, specs| !specs.is_empty());
}

fn file_schema_reason(detail: &str, retained: bool, rebuild_command: &str) -> String {
    let fate = if retained {
        "Documents already indexed for this file are dropped with this generation."
    } else {
        "Nothing was indexed for this file."
    };
    format!(
        "{detail} This file was not indexed; every other file in this pass still is. {fate} \
         Stop the watcher and rebuild the folder so the schema is inferred again: {rebuild_command}"
    )
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

    /// Printed verbatim by a per-file schema refusal. A reworded error that
    /// drops this command is a failed recovery contract.
    const REBUILD: &str =
        "xerj autoindex /src --prefix p-reindex --state-dir /state-reindex --no-graph";

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
            num_min: None,
            num_max: None,
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
            text_analyzer: None,
            scope: None,
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
                population: 1,
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
            REBUILD,
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
            REBUILD,
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
        let plan = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(fields)],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(
            plan.files.is_empty(),
            "an equal tie must not assign a dataset"
        );
        assert_eq!(plan.junk_files.len(), 1);
        let message = &plan.junk_files[0].reason;
        assert!(
            message.contains("refusing ambiguous assignment"),
            "{message}"
        );
        assert!(message.contains("a-tie"), "{message}");
        assert!(message.contains("z-tie"), "{message}");
        assert!(message.contains(REBUILD), "{message}");
        assert!(
            message.contains("Nothing was indexed for this file."),
            "{message}"
        );
    }

    #[test]
    fn a_new_scalar_field_widens_and_an_unmappable_change_is_one_file() {
        let previous = Plan {
            datasets: vec![dataset("rows", vec![spec("id", "long")])],
            ..Plan::default()
        };
        // A file already assigned to this dataset grows a scalar the frozen
        // specs do not name. The mapping is extended. It used to abort the plan.
        let mut retained = previous.clone();
        retained
            .files
            .insert("old".into(), assignment("rows.jsonl", "unix:6e", "rows"));
        let unknown = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("surprise".into(), acc(&[json!("new mapping")])),
        ]);
        let plan = reconcile_plan(
            &inventory("rows.jsonl", "unix:6e", "new"),
            &retained,
            vec![scan(unknown)],
            50,
            REBUILD,
        )
        .unwrap();
        assert_eq!(plan.files["new"].assignments, vec![(None, "rows".into())]);
        assert!(plan.junk_files.is_empty());
        assert!(
            plan.datasets[0].specs.iter().all(|spec| spec.name == "id"),
            "frozen specs stay the generation identity"
        );
        assert!(
            plan.incremental_fields["rows"]
                .iter()
                .any(|spec| spec.name == "surprise"),
            "the new scalar is installed beside the frozen specs"
        );

        // The same extra field on a file with no committed assignment stays
        // below the group-less overlap threshold, so it does not join — and
        // it does not take the rest of the plan down with it.
        let plan = reconcile_plan(
            &inventory("new.jsonl", "unix:6f", "brand-new"),
            &previous,
            vec![scan(HashMap::from([
                ("id".into(), acc(&[json!(7)])),
                ("surprise".into(), acc(&[json!("new mapping")])),
            ]))],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(plan.files.is_empty());
        assert!(
            plan.junk_files[0]
                .reason
                .contains("unsupported dataset/schema evolution"),
            "{}",
            plan.junk_files[0].reason
        );
        assert!(plan.junk_files[0].reason.contains(REBUILD));
        assert!(plan.datasets[0].specs.iter().all(|spec| spec.name == "id"));
        assert!(plan.incremental_fields.is_empty());

        // Object-shaped values are not scalar FieldAcc evidence. Their name
        // stays in the overlap fingerprint, so a data file that introduces one
        // still fails to join — and only that file is junked.
        let object_field = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("object".into(), acc(&[json!({"nested": true})])),
        ]);
        let plan = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(object_field)],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(plan.files.is_empty());
        let message = &plan.junk_files[0].reason;
        assert!(
            message.contains("unsupported dataset/schema evolution"),
            "{message}"
        );
        assert!(message.contains(REBUILD), "{message}");
        assert!(
            plan.datasets[0].specs.iter().all(|spec| spec.name == "id"),
            "a refused file must not widen the frozen specs"
        );
        assert!(plan.incremental_fields.is_empty());

        let wrong_type = HashMap::from([("id".into(), acc(&[json!("not-a-number")]))]);
        let plan = reconcile_plan(
            &inventory("new.jsonl", "unix:6e", "new"),
            &previous,
            vec![scan(wrong_type)],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(plan.files.is_empty());
        let message = &plan.junk_files[0].reason;
        assert!(message.contains("incompatible"), "{message}");
        assert!(message.contains(REBUILD), "{message}");
        assert!(
            message.contains("Nothing was indexed for this file."),
            "{message}"
        );
    }

    /// #1815: a dataset genesis truncated at the field cap keeps its overflow
    /// fields unmapped. A retained file that changes a value and grows a new
    /// field must stay assigned, not be refused for "passing the field cap".
    #[test]
    fn capped_dataset_retains_a_changed_file_with_overflow_fields() {
        let mut genesis: HashMap<String, FieldAcc> = (0..crate::infer::MAX_FIELDS_PER_DATASET + 3)
            .map(|i| (format!("f_{i:04}"), acc(&[json!(i)])))
            .collect();
        genesis.insert("id".into(), acc(&[json!(1)]));
        let specs = crate::infer::infer_fields(&genesis, 1, true);
        assert_eq!(specs.len(), crate::infer::MAX_FIELDS_PER_DATASET);
        let mut ds = dataset("wide", specs);
        let overflow: Vec<String> = cap_overflow_fields(&ds)
            .expect("genesis records the cap overflow")
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(overflow.len(), 4);
        ds.file_count = 1;
        let mut previous = Plan {
            datasets: vec![ds],
            ..Plan::default()
        };
        previous
            .files
            .insert("old".into(), assignment("wide.json", "unix:77", "wide"));

        let mut changed = genesis.clone();
        changed.insert("id".into(), acc(&[json!(2)]));
        changed.insert(
            "paths_v1_new_route_get".into(),
            acc(&[json!("get /v1/new")]),
        );
        let plan = reconcile_plan(
            &inventory("wide.json", "unix:77", "new"),
            &previous,
            vec![scan(changed)],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(plan.junk_files.is_empty(), "{:?}", plan.junk_files);
        assert_eq!(plan.files["new"].assignments, vec![(None, "wide".into())]);
        assert!(
            plan.incremental_fields
                .get("wide")
                .is_none_or(|added| added.is_empty()),
            "a capped dataset cannot map more fields; overflow passes through"
        );
    }

    #[test]
    fn an_uncapped_dataset_has_no_overflow_fields() {
        assert!(cap_overflow_fields(&dataset("rows", vec![spec("id", "long")])).is_none());
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
            REBUILD,
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
        let plan = reconcile_plan(&inv, &previous, vec![file_scan], 50, REBUILD).unwrap();
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
            REBUILD,
        )
        .unwrap();
        assert_eq!(plan.files["code"].assignments, vec![(None, "docs".into())]);
        assert!(
            !plan.files["code"].as_document,
            "a code document indexes through its own extractor, not as_document"
        );
    }

    /// #2176: a code file indexed while it had no symbols freezes `docs` as
    /// title/language/body. Adding a function emits scalar fields (`code`,
    /// `name`, `defs`, …) the frozen specs do not name. The retained file must
    /// stay on `docs` and those fields must be recorded beside the specs —
    /// not written into them, which would move the generation identity.
    /// `impl`, `end_line`, and `start_line` stay out of that list too: the
    /// create body already maps them, and re-inferring `impl` can elect
    /// `keyword`.
    #[test]
    fn retained_code_file_grows_a_function_and_docs_gains_code() {
        let mut previous = Plan {
            datasets: vec![PlanDataset {
                family: "docs".into(),
                ..dataset(
                    "docs",
                    vec![
                        spec("title", "text"),
                        spec("language", "keyword"),
                        spec("body", "text"),
                    ],
                )
            }],
            ..Plan::default()
        };
        let mut prior = assignment("module.py", "unix:70", "docs");
        prior.family = "code".into();
        previous.files.insert("old-module".into(), prior);
        let fields = HashMap::from([
            ("title".to_string(), acc(&[json!("module.py")])),
            ("language".to_string(), acc(&[json!("python")])),
            (
                "body".to_string(),
                acc(&[json!("def cancel_request():\n    return \"birchfunction\"")]),
            ),
            ("defs".to_string(), acc(&[json!("function cancel_request")])),
            ("symbol_count".to_string(), acc(&[json!(1)])),
            ("code".to_string(), acc(&[json!("def cancel_request():")])),
            ("name".to_string(), acc(&[json!("cancel_request")])),
            ("kind".to_string(), acc(&[json!("function")])),
            ("line".to_string(), acc(&[json!(3)])),
            ("start_line".to_string(), acc(&[json!(3)])),
            (
                "symbols".to_string(),
                acc(&[json!({"name": "cancel_request"})]),
            ),
        ]);
        let mut file_scan = scan(fields);
        file_scan.sniffed.as_mut().unwrap().family = Family::Code;

        let plan = reconcile_plan(
            &inventory("module.py", "unix:70", "new-module"),
            &previous,
            vec![file_scan],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(
            plan.junk_files.is_empty(),
            "adding a function must not junk the file: {:?}",
            plan.junk_files
        );
        assert_eq!(
            plan.files["new-module"].assignments,
            vec![(None, "docs".into())]
        );
        let frozen: Vec<&str> = plan.datasets[0]
            .specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect();
        assert_eq!(frozen, ["title", "language", "body"]);
        let names: Vec<&str> = plan.incremental_fields["docs"]
            .iter()
            .map(|spec| spec.name.as_str())
            .collect();
        for field in ["code", "name", "kind", "line", "defs", "symbol_count"] {
            assert!(names.contains(&field), "missing {field} in {names:?}");
        }
        assert!(!names.contains(&"impl"));
        assert!(!names.contains(&"end_line"));
        assert!(!names.contains(&"start_line"));
        assert!(!names.contains(&"symbols"));
        let reason = plan.files["new-module"].rel.as_str();
        assert_eq!(reason, "module.py");
        let message = file_schema_reason("field id is incompatible", true, REBUILD);
        assert!(message.contains("Documents already indexed for this file are dropped"));
        assert!(message.contains(REBUILD));
    }

    /// Two frozen scope-keyed `docs` datasets (one per nested repository) whose
    /// genesis schemas are byte-identical. A code document is a group-less,
    /// empty-key-field shape, so it routes to the `docs` family (see
    /// `new_document_file_joins_the_docs_dataset_on_incremental`) and classifies
    /// as a document.
    fn two_fork_docs_scopes() -> Plan {
        let docs = |slug: &str, scope: &str| PlanDataset {
            family: "docs".into(),
            scope: Some(scope.into()),
            ..dataset(slug, vec![spec("title", "text"), spec("body", "text")])
        };
        Plan {
            datasets: vec![
                docs("bitcoin-core-docs", "bitcoin-core"),
                docs("bitcoin-knots-docs", "bitcoin-knots"),
            ],
            ..Plan::default()
        }
    }

    fn fork_doc_scan() -> FileScan {
        // `title`/`body`: the docs shape, present in both forks' identical
        // specs, so field overlap is a genuine tie. Empty key_fields + no group
        // + family code -> a NEW document that routes to the `docs` family.
        let fields = HashMap::from([
            ("title".into(), acc(&[json!("adjacent.ts")])),
            (
                "body".into(),
                acc(&[json!("export const adjacent = true;")]),
            ),
        ]);
        let mut scan = scan(fields);
        scan.sniffed.as_mut().unwrap().family = Family::Code;
        scan
    }

    /// #1230: adding a file inside one of several sibling repositories with
    /// matching schemas must route to THAT repository's frozen `docs` dataset,
    /// not abort the whole run as an ambiguous assignment. The file's path
    /// lands it under exactly one frozen scope, which breaks the field-overlap
    /// tie the identical fork schemas create.
    #[test]
    fn new_file_in_a_fork_joins_its_own_repository_scope_not_abort() {
        let previous = two_fork_docs_scopes();
        let plan = reconcile_plan(
            &inventory("bitcoin-core/adjacent.ts", "unix:a1", "core-add"),
            &previous,
            vec![fork_doc_scan()],
            50,
            REBUILD,
        )
        .expect("an adjacent file inside bitcoin-core must not abort the run");
        assert_eq!(
            plan.files["core-add"].assignments,
            vec![(None, "bitcoin-core-docs".into())],
            "a file under bitcoin-core/ must join bitcoin-core-docs, not its sibling fork"
        );

        // The sibling repository resolves symmetrically.
        let plan = reconcile_plan(
            &inventory("bitcoin-knots/adjacent.ts", "unix:a2", "knots-add"),
            &previous,
            vec![fork_doc_scan()],
            50,
            REBUILD,
        )
        .expect("an adjacent file inside bitcoin-knots must not abort the run");
        assert_eq!(
            plan.files["knots-add"].assignments,
            vec![(None, "bitcoin-knots-docs".into())],
        );
    }

    /// The scope tiebreak only ever breaks a field-overlap TIE; it never
    /// overrides a dataset that overlaps the file better. A file under
    /// bitcoin-knots whose fields overlap bitcoin-core-docs strictly more still
    /// joins bitcoin-core-docs.
    #[test]
    fn scope_never_overrides_a_strictly_better_field_overlap() {
        let core = PlanDataset {
            family: "jsonl".into(),
            scope: Some("bitcoin-core".into()),
            ..dataset(
                "bitcoin-core-rows",
                vec![spec("id", "long"), spec("body", "text")],
            )
        };
        let knots = PlanDataset {
            family: "jsonl".into(),
            scope: Some("bitcoin-knots".into()),
            ..dataset("bitcoin-knots-rows", vec![spec("id", "long")])
        };
        let previous = Plan {
            datasets: vec![core, knots],
            ..Plan::default()
        };
        // {id, body}: a perfect overlap with bitcoin-core-rows, a partial one
        // with bitcoin-knots-rows — even though the file sits under knots.
        let fields = HashMap::from([
            ("id".into(), acc(&[json!(7)])),
            ("body".into(), acc(&[json!("hello world")])),
        ]);
        let plan = reconcile_plan(
            &inventory("bitcoin-knots/rows.jsonl", "unix:a3", "row-add"),
            &previous,
            vec![scan(fields)],
            50,
            REBUILD,
        )
        .unwrap();
        assert_eq!(
            plan.files["row-add"].assignments,
            vec![(None, "bitcoin-core-rows".into())],
            "field overlap is primary; scope only breaks a genuine tie"
        );
    }

    /// A plan frozen before #1230 carries no scope (`None` for every dataset),
    /// so equal field overlap stays genuinely ambiguous and still fails closed —
    /// the fix is additive, never a silent change for legacy state dirs.
    #[test]
    fn legacy_plan_without_scope_still_fails_closed_on_an_equal_tie() {
        let previous = Plan {
            datasets: vec![
                PlanDataset {
                    family: "docs".into(),
                    ..dataset("z-docs", vec![spec("title", "text"), spec("body", "text")])
                },
                PlanDataset {
                    family: "docs".into(),
                    ..dataset("a-docs", vec![spec("title", "text"), spec("body", "text")])
                },
            ],
            ..Plan::default()
        };
        let plan = reconcile_plan(
            &inventory("bitcoin-core/adjacent.ts", "unix:a4", "legacy-add"),
            &previous,
            vec![fork_doc_scan()],
            50,
            REBUILD,
        )
        .unwrap();
        assert!(
            plan.files.is_empty(),
            "a legacy plan with no scope must not silently assign the tie"
        );
        let message = &plan.junk_files[0].reason;
        assert!(
            message.contains("refusing ambiguous assignment"),
            "{message}"
        );
        assert!(message.contains(REBUILD), "{message}");
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
        let plan = reconcile_plan(&current, &previous, vec![scan(fields)], 50, REBUILD).unwrap();
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
        let error =
            reconcile_plan(&conflict, &previous, vec![scan(fields())], 50, REBUILD).unwrap_err();
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
            REBUILD,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("duplicate content identity"));
    }
}
