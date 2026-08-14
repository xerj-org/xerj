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
        anyhow::ensure!(
            observed.len() == self.documents.len(),
            "catalog read-back contains {} documents; expected {} ({})",
            observed.len(),
            self.documents.len(),
            id_set_diff(&self.documents, observed)
        );
        for (id, expected) in &self.documents {
            let actual = observed
                .get(id)
                .with_context(|| format!("catalog read-back is missing {id}"))?;
            anyhow::ensure!(
                actual == expected,
                "catalog read-back for {id} disagrees with the sealed generation projection: {}",
                document_diff(expected, actual)
            );
        }
        Ok(())
    }
}

/// How many differing keys or IDs a diff names before it truncates. A 7.5 k
/// document projection whose read-back lost one field per document has to stay
/// readable on a terminal.
const DIFF_SAMPLE: usize = 5;

/// Name the IDs that explain a read-back cardinality mismatch.
fn id_set_diff(expected: &BTreeMap<String, Value>, actual: &BTreeMap<String, Value>) -> String {
    let missing: Vec<&str> = expected
        .keys()
        .filter(|id| !actual.contains_key(*id))
        .map(String::as_str)
        .collect();
    let extra: Vec<&str> = actual
        .keys()
        .filter(|id| !expected.contains_key(*id))
        .map(String::as_str)
        .collect();
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing {}", sample(&missing)));
    }
    if !extra.is_empty() {
        parts.push(format!("unexpected {}", sample(&extra)));
    }
    if parts.is_empty() {
        // Both sets hold the same IDs, so the counts can only disagree if one
        // side carried a duplicate — which the callers' `BTreeMap` cannot.
        "no ID differs".into()
    } else {
        parts.join("; ")
    }
}

/// Per-key diff of one catalog document.
///
/// The bare "disagrees with the sealed generation projection" this replaces
/// named neither the key nor the values, so a read-back that silently dropped
/// every `null`-valued field (`time_field`, `time_min`, `time_max`,
/// `semantic_field` on `ds:*`; `reason` on an indexed `file:*`) was
/// indistinguishable from a miscounted `record_count` — and the first key of
/// the projection's `BTreeMap` was the only one the operator ever saw.
fn document_diff(expected: &Value, actual: &Value) -> String {
    let (Some(expected), Some(actual)) = (expected.as_object(), actual.as_object()) else {
        return format!(
            "expected {}, got {}",
            truncate(&expected.to_string()),
            truncate(&actual.to_string())
        );
    };
    let mut parts = Vec::new();
    let missing: Vec<String> = expected
        .iter()
        .filter(|(key, _)| !actual.contains_key(*key))
        .map(|(key, value)| format!("{key} (expected {})", truncate(&value.to_string())))
        .collect();
    if !missing.is_empty() {
        parts.push(format!("missing key {}", sample_owned(&missing)));
    }
    let extra: Vec<&str> = actual
        .keys()
        .filter(|key| !expected.contains_key(*key))
        .map(String::as_str)
        .collect();
    if !extra.is_empty() {
        parts.push(format!("unexpected key {}", sample(&extra)));
    }
    let changed: Vec<String> = expected
        .iter()
        .filter_map(|(key, value)| {
            let observed = actual.get(key)?;
            (observed != value).then(|| {
                format!(
                    "{key} (expected {}, got {})",
                    truncate(&value.to_string()),
                    truncate(&observed.to_string())
                )
            })
        })
        .collect();
    if !changed.is_empty() {
        parts.push(format!("changed value {}", sample_owned(&changed)));
    }
    if parts.is_empty() {
        "no key differs".into()
    } else {
        parts.join("; ")
    }
}

fn sample(items: &[&str]) -> String {
    sample_owned(
        &items
            .iter()
            .map(|item| (*item).to_owned())
            .collect::<Vec<_>>(),
    )
}

fn sample_owned(items: &[String]) -> String {
    let shown = items.len().min(DIFF_SAMPLE);
    let mut out = items[..shown].join(", ");
    if items.len() > shown {
        out.push_str(&format!(" (+{} more)", items.len() - shown));
    }
    out
}

fn truncate(rendered: &str) -> String {
    const LIMIT: usize = 80;
    if rendered.chars().count() <= LIMIT {
        return rendered.to_owned();
    }
    let head: String = rendered.chars().take(LIMIT).collect();
    format!("{head}…")
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
fn managed_non_run_ids(plan: &crate::state::Plan) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    ids.extend(plan.files.keys().map(|key| format!("file:{key}")));
    ids.extend(
        plan.junk_files
            .iter()
            .map(|junk| format!("file:{}", junk.file_key)),
    );
    ids.extend(
        plan.duplicate_files
            .iter()
            .map(|alias| catalog::duplicate_file_id(&alias.file_key, &alias.rel, &alias.path_id)),
    );
    ids.extend(
        plan.datasets
            .iter()
            .map(|dataset| format!("ds:{}", dataset.slug)),
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
            &managed_non_run_ids(&base.plan),
        )
        .unwrap();

        assert!(projection.documents.len() >= 4);
        assert!(projection
            .documents
            .values()
            .all(|doc| { doc.get("run_id").and_then(Value::as_str) == Some("generation-2") }));
        assert_eq!(
            projection.documents["file:keep"]["run_id"], "generation-2",
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
            &managed_non_run_ids(&base.plan),
        )
        .unwrap();

        assert_eq!(projection.documents["file:indexed"]["status"], "junk");
        assert!(projection.stale_ids.contains("file:old-junk"));
        assert!(projection.stale_ids.contains(&catalog::duplicate_file_id(
            &duplicate.file_key,
            &duplicate.rel,
            &duplicate.path_id
        )));
        assert!(!projection.stale_ids.contains("file:indexed"));
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
            &managed_non_run_ids(&base.plan),
        )
        .unwrap();
        projection.validate_observed(&projection.documents).unwrap();

        let mut missing = projection.documents.clone();
        missing.pop_first();
        assert!(projection.validate_observed(&missing).is_err());

        let mut changed = projection.documents.clone();
        changed.get_mut("ds:reports").unwrap()["record_count"] = json!(999);
        assert!(projection.validate_observed(&changed).is_err());
    }

    /// #367: the read-back that aborted `lucene-meta` differed from the sealed
    /// projection by exactly the four `null`-valued keys the stored-section
    /// codec dropped, and the message named none of them — it named `ds:docs`,
    /// which is just the first key of the `BTreeMap`. The failure has to say
    /// which key and which value, or the next reporter spends a day on it too.
    #[test]
    fn readback_disagreement_names_the_keys_and_values_that_differ() {
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
            &managed_non_run_ids(&base.plan),
        )
        .unwrap();

        // Exactly what a v2 stored section hands back: every null-valued key
        // is simply absent.
        let mut dropped = projection.documents.clone();
        let dataset_doc = dropped.get_mut("ds:reports").unwrap();
        for key in ["time_field", "time_min", "time_max", "semantic_field"] {
            assert!(dataset_doc[key].is_null(), "{key} must be null to drop");
            dataset_doc.as_object_mut().unwrap().remove(key);
        }
        let message = projection
            .validate_observed(&dropped)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("time_field (expected null)"),
            "the dropped null key must be named: {message}"
        );
        assert!(
            message.contains("semantic_field"),
            "every dropped key must be named: {message}"
        );

        let mut changed = projection.documents.clone();
        changed.get_mut("ds:reports").unwrap()["record_count"] = json!(999);
        let message = projection
            .validate_observed(&changed)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("record_count (expected 2, got 999)"),
            "a changed value must show both sides: {message}"
        );

        let mut missing = projection.documents.clone();
        let (removed, _) = missing.pop_first().unwrap();
        let message = projection
            .validate_observed(&missing)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains(&removed),
            "a cardinality mismatch must name the absent ID: {message}"
        );
    }
}
