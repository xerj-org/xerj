//! Exact catalog projection for one committed corpus generation.
//!
//! Data-group operations are intentionally not catalog authority. A generation
//! which only adds junk, removes an alias, or changes one of many files still
//! has to publish a complete agent-facing data map. This module builds that
//! complete, retry-stable projection without performing HTTP I/O.

use crate::catalog;
use crate::sync::{CommittedManifest, GenerationManifest, SyncOperationKind};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub struct DatasetCatalogStats {
    pub record_count: u64,
    pub junk_records: u64,
    pub bytes: u64,
    pub formats: Vec<String>,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub sample_queries: Vec<Value>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationCatalogMetadata {
    /// Stable ID sealed into the pending generation. This is the `run_id`
    /// attached to every catalog document in this projection.
    pub generation_id: String,
    /// Stable timestamp sealed before publication. Retries must reuse it.
    pub started: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogProjection {
    pub generation_id: String,
    /// Complete desired managed document set, keyed by catalog `_id`.
    pub documents: BTreeMap<String, Value>,
    /// IDs managed by the previous generation which no longer exist.
    pub stale_ids: BTreeSet<String>,
}

impl CatalogProjection {
    /// Validate an exact read-back of all desired managed documents.
    ///
    /// The HTTP backend should fetch these IDs after refresh and pass the
    /// resulting `_source` values here. Extra unrelated catalog documents are
    /// outside this corpus projection and therefore are not accepted through
    /// this API.
    pub fn validate_observed(&self, observed: &BTreeMap<String, Value>) -> Result<()> {
        if observed.len() != self.documents.len() {
            let expected_ids: BTreeSet<&String> = self.documents.keys().collect();
            let observed_ids: BTreeSet<&String> = observed.keys().collect();
            let missing: Vec<&&String> = expected_ids.difference(&observed_ids).collect();
            let unexpected: Vec<&&String> = observed_ids.difference(&expected_ids).collect();
            anyhow::bail!(
                "catalog read-back contains {} documents; expected {} (missing: {:?}; \
                 unexpected: {:?})",
                observed.len(),
                self.documents.len(),
                missing,
                unexpected
            );
        }
        for (id, expected) in &self.documents {
            let actual = observed
                .get(id)
                .with_context(|| format!("catalog read-back is missing {id}"))?;
            anyhow::ensure!(
                actual == expected,
                "catalog read-back for {id} disagrees with the sealed generation \
                 projection: {}",
                describe_field_diff(expected, actual)
            );
        }
        Ok(())
    }
}

/// Name the field(s) by which a read-back catalog document differs from the
/// sealed projection, so a mismatch is diagnosable rather than an opaque
/// "disagrees". #367: a real-server run aborted here with no way to see which
/// field the server rewrote — the prime suspect is a dropped `null`-valued key
/// (`time_min`/`time_max`/`time_field`/`semantic_field` are `null` for a
/// timestamp-less, non-semantic dataset like `ds:docs`), which this surfaces as
/// "`time_min` dropped". Values are truncated so a large `fields_json` cannot
/// blow up the message.
fn describe_field_diff(expected: &Value, actual: &Value) -> String {
    fn short(v: &Value) -> String {
        let s = v.to_string();
        if s.len() > 120 {
            format!("{}…", &s[..120])
        } else {
            s
        }
    }
    match (expected.as_object(), actual.as_object()) {
        (Some(e), Some(a)) => {
            let mut diffs: Vec<String> = Vec::new();
            for (k, ev) in e {
                match a.get(k) {
                    None => diffs.push(format!("`{k}` dropped (was {})", short(ev))),
                    Some(av) if av != ev => {
                        diffs.push(format!("`{k}`: expected {}, got {}", short(ev), short(av)))
                    }
                    _ => {}
                }
            }
            for k in a.keys() {
                if !e.contains_key(k) {
                    diffs.push(format!("`{k}` unexpectedly added"));
                }
            }
            if diffs.is_empty() {
                "documents differ with no field-level difference (type or ordering?)".into()
            } else {
                diffs.join("; ")
            }
        }
        _ => format!("expected {}, got {}", short(expected), short(actual)),
    }
}

/// Build the complete catalog projection for `desired`.
///
/// `dataset_stats` must be exact, generation-bound values obtained from the
/// sealed prepared artifacts or from a refreshed exact read-back. Supplying
/// sampled estimates here would make the data map lie.
///
/// `prior_managed_ids` is the exact set observed through the authenticated
/// catalog endpoint for the prior committed catalog generation. The manifest
/// cannot derive correlation IDs, so callers must query or durably persist
/// this set.
pub fn project_generation(
    _base: &CommittedManifest,
    desired: &GenerationManifest,
    metadata: &GenerationCatalogMetadata,
    dataset_stats: &BTreeMap<String, DatasetCatalogStats>,
    correlations: &BTreeMap<String, Value>,
    prior_managed_ids: &BTreeSet<String>,
) -> Result<CatalogProjection> {
    anyhow::ensure!(
        !metadata.generation_id.is_empty() && !metadata.started.is_empty(),
        "catalog generation metadata is incomplete"
    );
    let execution = desired
        .execution
        .as_ref()
        .context("catalog generation has no execution identity")?;
    let mut documents = BTreeMap::new();
    // #294 tripwire: counted from the sealed manifest, so a resumed or no-op
    // generation republishes the same coverage instead of an empty one.
    let mut code = crate::CodeCoverage::default();

    for group in &desired.groups {
        let assignment = desired.plan.files.get(&group.content_id).with_context(|| {
            format!(
                "catalog group {} has no desired file assignment",
                group.group_id
            )
        })?;
        let format = assignment_format(assignment.family.as_str(), assignment.gzip);
        code.observe(&format, group.expected_records);
        let (id, doc) = catalog::file_doc(
            &execution.prefix,
            &group.content_id,
            &group.canonical.rel,
            &format,
            "indexed",
            None,
            group.expected_records,
            group.expected_junk_records,
            group.content_size,
            &metadata.generation_id,
        );
        insert_unique(&mut documents, id, doc)?;
        for alias in &group.aliases {
            let (id, doc) = catalog::duplicate_file_doc(
                &execution.prefix,
                &group.content_id,
                &alias.rel,
                &alias.path_id,
                &group.canonical.rel,
                group.content_size,
                &metadata.generation_id,
            );
            insert_unique(&mut documents, id, doc)?;
        }
    }

    for junk in &desired.plan.junk_files {
        // A junk/skipped file has no `plan.files` entry by construction, so
        // these cannot double-count the groups above.
        code.observe(&junk.format, 0);
        let (id, doc) = catalog::file_doc(
            &execution.prefix,
            &junk.file_key,
            &junk.rel,
            &junk.format,
            &junk.status,
            Some(&junk.reason),
            0,
            0,
            junk.bytes,
            &metadata.generation_id,
        );
        insert_unique(&mut documents, id, doc)?;
    }

    let mut total_records = 0u64;
    // The run total comes from the manifest, not from summing the per-dataset
    // statistics. A junk record belongs to no dataset — the dataset docs
    // attribute it to one of the group's slugs so a multi-dataset file is not
    // counted twice, and a group with no dataset slug at all would otherwise
    // drop out of the total entirely. Summing the sealed per-group counts is
    // the only arithmetic that is exact in both cases.
    let total_junk_records = desired.groups.iter().try_fold(0u64, |sum, group| {
        sum.checked_add(group.expected_junk_records)
            .context("catalog total junk-record count overflow")
    })?;
    for dataset in &desired.plan.datasets {
        let stats = dataset_stats.get(&dataset.slug).with_context(|| {
            format!(
                "catalog generation has no exact statistics for dataset {}",
                dataset.slug
            )
        })?;
        total_records = total_records
            .checked_add(stats.record_count)
            .context("catalog total record count overflow")?;
        let mut formats = stats.formats.clone();
        formats.sort();
        formats.dedup();
        let (id, doc) = catalog::dataset_doc(&catalog::DatasetDocInput {
            prefix: &execution.prefix,
            pd: dataset,
            record_count: stats.record_count,
            junk_records: stats.junk_records,
            bytes: stats.bytes,
            file_count: dataset.file_count,
            formats,
            time_min: stats.time_min.clone(),
            time_max: stats.time_max.clone(),
            sample_queries: stats.sample_queries.clone(),
            notes: stats.notes.clone(),
            run_id: &metadata.generation_id,
        });
        insert_unique(&mut documents, id, doc)?;
    }

    for (id, correlation) in correlations {
        let mut correlation = correlation.clone();
        anyhow::ensure!(
            correlation.get("doc_kind").and_then(Value::as_str) == Some("correlation"),
            "catalog correlation {id} has the wrong doc_kind"
        );
        correlation["run_id"] = Value::String(metadata.generation_id.clone());
        insert_unique(&mut documents, id.clone(), correlation)?;
    }

    let files_total = desired
        .groups
        .iter()
        .map(|group| 1usize + group.aliases.len())
        .sum::<usize>()
        .checked_add(desired.plan.junk_files.len())
        .context("catalog file count overflow")?;
    let run_doc = json!({
        "doc_kind": "run",
        "run_id": metadata.generation_id,
        "root": execution.root_identity,
        "url": execution.url,
        "prefix": execution.prefix,
        "started": metadata.started,
        "generation": desired.generation,
        "files_total": files_total,
        "unique_content_files": desired.groups.len(),
        "files_indexed": desired.groups.len(),
        "duplicate_files": desired.plan.duplicate_files.len(),
        "files_junk": desired.plan.junk_files.len(),
        "records_total": total_records,
        "junk_records_total": total_junk_records,
        // Code/AST coverage (`CodeCoverage`): `records_total` counts records,
        // not families, so it cannot say that every source file in the corpus
        // was junked. These three can, and they travel to the terminal line.
        "code_files": code.files,
        "code_files_indexed": code.indexed,
        "code_files_junked": code.junked,
        "semantic": desired
            .plan
            .datasets
            .iter()
            .any(|dataset| dataset.semantic_field.is_some()),
    });
    insert_unique(
        &mut documents,
        format!("run:{}", metadata.generation_id),
        run_doc,
    )?;

    anyhow::ensure!(
        documents.values().all(|doc| {
            doc.get("run_id").and_then(Value::as_str) == Some(metadata.generation_id.as_str())
        }),
        "every desired catalog document must carry the current generation run_id"
    );

    let desired_ids: BTreeSet<String> = documents.keys().cloned().collect();
    let stale_ids = prior_managed_ids
        .difference(&desired_ids)
        .filter(|id| !desired_ids.contains(*id))
        .cloned()
        .collect();

    Ok(CatalogProjection {
        generation_id: metadata.generation_id.clone(),
        documents,
        stale_ids,
    })
}

/// Provenance rule used by the publication backend.
///
/// Upserts publish newly prepared bytes and therefore must carry the desired
/// generation ID. Metadata-only operations must preserve the existing
/// document `ax_run`; deletes leave no live provenance value.
#[cfg_attr(not(test), allow(dead_code))]
pub fn expected_ax_run<'a>(
    kind: &SyncOperationKind,
    desired_generation_id: &'a str,
    existing_ax_run: Option<&'a str>,
) -> Result<Option<&'a str>> {
    match kind {
        SyncOperationKind::Upsert => Ok(Some(desired_generation_id)),
        SyncOperationKind::Metadata => existing_ax_run
            .map(Some)
            .context("metadata-only publication has no existing ax_run to preserve"),
        SyncOperationKind::Delete => Ok(None),
    }
}

#[cfg(test)]
fn managed_non_run_ids(prefix: &str, plan: &crate::state::Plan) -> BTreeSet<String> {
    // #416: these prior-generation ids MUST match the write path's (prefixed)
    // ids exactly, or the stale-sweep never removes them → orphan rows.
    let mut ids = BTreeSet::new();
    ids.extend(plan.files.keys().map(|key| catalog::file_id(prefix, key)));
    ids.extend(
        plan.junk_files
            .iter()
            .map(|junk| catalog::file_id(prefix, &junk.file_key)),
    );
    ids.extend(plan.duplicate_files.iter().map(|alias| {
        catalog::duplicate_file_id(prefix, &alias.file_key, &alias.rel, &alias.path_id)
    }));
    ids.extend(
        plan.datasets
            .iter()
            .map(|dataset| format!("ds:{}:{}", prefix, dataset.slug)),
    );
    ids
}

fn assignment_format(family: &str, gzip: bool) -> String {
    if gzip {
        format!("{family}(gzip)")
    } else {
        family.to_owned()
    }
}

fn insert_unique(documents: &mut BTreeMap<String, Value>, id: String, doc: Value) -> Result<()> {
    anyhow::ensure!(
        documents.insert(id.clone(), doc).is_none(),
        "catalog projection contains duplicate document ID {id}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DuplicateFile, FileAssignment, JunkFile, Plan, PlanDataset};
    use crate::sync::{
        ExecutionIdentity, ManifestGroup, ManifestPath, SourceExecutionPolicy,
        EXECUTION_IDENTITY_VERSION,
    };
    fn unix_path_id(path: &str) -> String {
        let encoded = path
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("unix:{encoded}")
    }

    fn dataset() -> PlanDataset {
        PlanDataset {
            slug: "reports".into(),
            index: "ax-reports".into(),
            family: "csv".into(),
            group: None,
            specs: Vec::new(),
            time_field: None,
            semantic_field: None,
            sampled_records: 2,
            file_count: 1,
        }
    }

    fn assignment(path: &str) -> FileAssignment {
        FileAssignment {
            rel: path.into(),
            path_id: unix_path_id(path),
            is_symlink: Some(false),
            family: "csv".into(),
            gzip: false,
            content_digest: Some("digest".into()),
            assignments: vec![(None, "reports".into())],
            as_document: false,
        }
    }

    fn group(key: &str, path: &str, aliases: Vec<ManifestPath>) -> ManifestGroup {
        ManifestGroup {
            group_id: format!("group-{key}"),
            content_id: key.into(),
            content_digest: "digest".into(),
            content_size: 10,
            canonical: ManifestPath {
                path_id: unix_path_id(path),
                rel: path.into(),
                is_symlink: false,
            },
            aliases,
            dataset_slugs: vec!["reports".into()],
            expected_records: 2,
            expected_passages: 0,
            expected_vectors: 0,
            expected_junk_records: 0,
            expected_records_by_dataset: BTreeMap::from([("reports".to_string(), 2)]),
        }
    }

    fn execution(generation_id: &str) -> ExecutionIdentity {
        ExecutionIdentity {
            version: EXECUTION_IDENTITY_VERSION,
            root_identity: "/corpus".into(),
            url: "http://engine".into(),
            prefix: "ax".into(),
            follow_symlinks: false,
            chunker_identity: "prepared-records-v1".into(),
            embedding_identity_sha256: "a".repeat(64),
            embedding_backend: "lexical".into(),
            embedding_dimension: Some(384),
            embedding_semantic_contract: "semantic_text-derived-vector.v1".into(),
            embedding_resumable: true,
            graph_enabled: false,
            brain: "none".into(),
            detector_identity: "disabled".into(),
            schema_identity: "schema".into(),
            index_identity: "index".into(),
            source_policy: SourceExecutionPolicy::DurableSnapshot {
                reference: format!("sync-snapshots/{generation_id}"),
                snapshot_digest: "snapshot".into(),
            },
        }
    }

    fn manifest(
        generation: u64,
        id: &str,
        plan: Plan,
        groups: Vec<ManifestGroup>,
    ) -> GenerationManifest {
        GenerationManifest {
            generation,
            execution: Some(execution(id)),
            plan,
            groups,
        }
    }

    fn committed(generation: GenerationManifest) -> CommittedManifest {
        CommittedManifest {
            generation: generation.generation,
            manifest_digest: "base".into(),
            plan: generation.plan,
            groups: generation.groups,
            execution: generation.execution,
        }
    }

    fn stats() -> BTreeMap<String, DatasetCatalogStats> {
        BTreeMap::from([(
            "reports".into(),
            DatasetCatalogStats {
                record_count: 2,
                junk_records: 0,
                bytes: 10,
                formats: vec!["csv".into()],
                time_min: None,
                time_max: None,
                sample_queries: Vec::new(),
                notes: Vec::new(),
            },
        )])
    }

    fn metadata(id: &str) -> GenerationCatalogMetadata {
        GenerationCatalogMetadata {
            generation_id: id.into(),
            started: "2026-08-03T00:00:00Z".into(),
        }
    }

    /// #294 dropped every source file on this path — prepared as zero
    /// documents, then committed and reported as a success. The run document
    /// could not say so: `records_total` counts records, not families, so a
    /// corpus whose whole code half was junked projected the same shape as a
    /// healthy one-prose-file corpus. Coverage is what distinguishes them, and
    /// it is what the terminal line reads.
    #[test]
    fn the_run_document_reports_code_coverage_for_junked_and_indexed_source_files() {
        let mut code_assignment = assignment("app.py");
        code_assignment.family = "code".into();
        let mut plan = Plan {
            datasets: vec![dataset()],
            ..Plan::default()
        };
        plan.files.insert("prose".into(), assignment("notes.md"));
        plan.files.insert("code".into(), code_assignment);
        let base = committed(manifest(0, "generation-0", Plan::default(), vec![]));

        // The defect: the code file prepared nothing at all.
        let mut junked = group("code", "app.py", vec![]);
        junked.expected_records = 0;
        junked.expected_junk_records = 1;
        let desired = manifest(
            1,
            "generation-1",
            plan.clone(),
            vec![group("prose", "notes.md", vec![]), junked],
        );
        let projection = project_generation(
            &base,
            &desired,
            &metadata("generation-1"),
            &stats(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        let run = &projection.documents["run:generation-1"];
        assert_eq!(run["code_files"], 1);
        assert_eq!(
            run["code_files_indexed"], 0,
            "a corpus that indexed no source code must say so: {run}"
        );
        assert_eq!(run["code_files_junked"], 1);

        // The healthy shape of the same corpus.
        let desired = manifest(
            1,
            "generation-1",
            plan,
            vec![
                group("prose", "notes.md", vec![]),
                group("code", "app.py", vec![]),
            ],
        );
        let projection = project_generation(
            &base,
            &desired,
            &metadata("generation-1"),
            &stats(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        )
        .unwrap();
        let run = &projection.documents["run:generation-1"];
        assert_eq!(
            (
                run["code_files"].clone(),
                run["code_files_indexed"].clone(),
                run["code_files_junked"].clone()
            ),
            (json!(1), json!(1), json!(0)),
            "{run}"
        );
    }

    #[test]
    fn every_current_document_moves_to_latest_catalog_generation() {
        let mut base_plan = Plan {
            datasets: vec![dataset()],
            ..Plan::default()
        };
        base_plan
            .files
            .insert("keep".into(), assignment("keep.csv"));
        base_plan
            .files
            .insert("change".into(), assignment("change.csv"));
        let base_generation = manifest(
            1,
            "generation-1",
            base_plan.clone(),
            vec![
                group("keep", "keep.csv", vec![]),
                group("change", "change.csv", vec![]),
            ],
        );
        let base = committed(base_generation);

        let desired = manifest(
            2,
            "generation-2",
            base_plan,
            vec![
                group("keep", "keep.csv", vec![]),
                group("change", "change.csv", vec![]),
            ],
        );
        let projection = project_generation(
            &base,
            &desired,
            &metadata("generation-2"),
            &stats(),
            &BTreeMap::new(),
            &managed_non_run_ids("ax", &base.plan),
        )
        .unwrap();

        assert!(projection.documents.len() >= 4);
        assert!(projection
            .documents
            .values()
            .all(|doc| { doc.get("run_id").and_then(Value::as_str) == Some("generation-2") }));
        assert_eq!(
            projection.documents["file:ax:keep"]["run_id"], "generation-2",
            "an unchanged peer remains visible in the latest data map"
        );
    }

    #[test]
    fn junk_transitions_and_removed_aliases_have_exact_stale_ids() {
        let alias = ManifestPath {
            path_id: unix_path_id("alias.csv"),
            rel: "alias.csv".into(),
            is_symlink: false,
        };
        let duplicate = DuplicateFile {
            file_key: "indexed".into(),
            rel: alias.rel.clone(),
            path_id: alias.path_id.clone(),
            is_symlink: Some(false),
            duplicate_of: "indexed.csv".into(),
            bytes: 10,
        };
        let mut base_plan = Plan {
            datasets: vec![dataset()],
            duplicate_files: vec![duplicate.clone()],
            ..Plan::default()
        };
        base_plan
            .files
            .insert("indexed".into(), assignment("indexed.csv"));
        base_plan.junk_files.push(JunkFile {
            file_key: "old-junk".into(),
            rel: "old.bin".into(),
            format: "binary".into(),
            status: "junk".into(),
            reason: "binary".into(),
            bytes: 10,
        });
        let base = committed(manifest(
            1,
            "generation-1",
            base_plan,
            vec![group("indexed", "indexed.csv", vec![alias])],
        ));

        let mut desired_plan = Plan {
            datasets: vec![dataset()],
            ..Plan::default()
        };
        desired_plan.junk_files.push(JunkFile {
            file_key: "indexed".into(),
            rel: "indexed.csv".into(),
            format: "binary".into(),
            status: "junk".into(),
            reason: "became binary".into(),
            bytes: 10,
        });
        let desired = manifest(2, "generation-2", desired_plan, Vec::new());
        let mut no_dataset_stats = stats();
        no_dataset_stats.get_mut("reports").unwrap().record_count = 0;
        let projection = project_generation(
            &base,
            &desired,
            &metadata("generation-2"),
            &no_dataset_stats,
            &BTreeMap::new(),
            &managed_non_run_ids("ax", &base.plan),
        )
        .unwrap();

        assert_eq!(projection.documents["file:ax:indexed"]["status"], "junk");
        assert!(projection.stale_ids.contains("file:ax:old-junk"));
        assert!(projection.stale_ids.contains(&catalog::duplicate_file_id(
            "ax",
            &duplicate.file_key,
            &duplicate.rel,
            &duplicate.path_id
        )));
        assert!(!projection.stale_ids.contains("file:ax:indexed"));
    }

    #[test]
    fn provenance_contract_changes_only_upsert_ax_run() {
        assert_eq!(
            expected_ax_run(&SyncOperationKind::Upsert, "g2", Some("g1")).unwrap(),
            Some("g2")
        );
        assert_eq!(
            expected_ax_run(&SyncOperationKind::Metadata, "g2", Some("g1")).unwrap(),
            Some("g1")
        );
        assert_eq!(
            expected_ax_run(&SyncOperationKind::Delete, "g2", Some("g1")).unwrap(),
            None
        );
        assert!(expected_ax_run(&SyncOperationKind::Metadata, "g2", None).is_err());
    }

    #[test]
    fn exact_readback_rejects_missing_or_different_documents() {
        let plan = Plan {
            datasets: vec![dataset()],
            ..Plan::default()
        };
        let base = committed(manifest(1, "g1", plan.clone(), Vec::new()));
        let desired = manifest(2, "g2", plan, Vec::new());
        let projection = project_generation(
            &base,
            &desired,
            &metadata("g2"),
            &stats(),
            &BTreeMap::new(),
            &managed_non_run_ids("ax", &base.plan),
        )
        .unwrap();
        projection.validate_observed(&projection.documents).unwrap();

        let mut missing = projection.documents.clone();
        missing.pop_first();
        assert!(projection.validate_observed(&missing).is_err());

        let mut changed = projection.documents.clone();
        changed.get_mut("ds:ax:reports").unwrap()["record_count"] = json!(999);
        // #367: the message must NAME the offending field, not just "disagrees".
        let err = projection
            .validate_observed(&changed)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("record_count"),
            "the diff must name the changed field: {err}"
        );

        // A dropped key (the #367 null-key-drop shape) is surfaced as "dropped".
        let mut dropped = projection.documents.clone();
        dropped
            .get_mut("ds:ax:reports")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("time_min");
        let err = projection
            .validate_observed(&dropped)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("time_min") && err.contains("dropped"),
            "a dropped key must be named: {err}"
        );
    }
}
