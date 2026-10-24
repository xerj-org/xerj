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
    /// Managed documents this generation must WRITE, keyed by catalog `_id`.
    ///
    /// #971: a document whose desired content is identical to the prior
    /// generation's publication except for `run_id` is deliberately absent —
    /// it stays live under the run_id of the generation that last touched it,
    /// so a one-file change rewrites O(changed) catalog documents instead of
    /// the whole corpus. Publication is convergent either way: ids are
    /// content-derived and upserted in place, so re-sending a skipped document
    /// (a retry that cannot prove it unchanged) reproduces the same live set.
    pub documents: BTreeMap<String, Value>,
    /// Complete desired managed id set — written plus intentionally kept.
    /// The publication sweep's authority for "which prior-generation documents
    /// are allowed to remain".
    pub managed_ids: BTreeSet<String>,
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
        // Truncate on a CHAR boundary (values can hold non-ASCII, e.g. a CJK
        // `fields_json`); byte slicing `&s[..120]` would panic mid-codepoint.
        if let Some((idx, _)) = s.char_indices().nth(120) {
            format!("{}…", &s[..idx])
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

/// Build the catalog projection for `desired`.
///
/// `dataset_stats` must be exact, generation-bound values obtained from the
/// sealed prepared artifacts or from a refreshed exact read-back. Supplying
/// sampled estimates here would make the data map lie.
///
/// `prior_managed_ids` is the set of ids observed for the prior committed
/// catalog generation that the manifest cannot derive — correlation IDs are
/// the case in point, which is why callers must query or durably persist
/// them. The prior generation's own file/junk/duplicate/dataset ids and run
/// document are derived from `base` here.
///
/// #971: the projection is incremental. A managed document whose desired
/// content equals the prior generation's publication except for `run_id` is
/// NOT returned in `documents` — it stays live untouched, keeping the run_id
/// of the generation that last wrote it. Dataset and run documents are always
/// written (few per corpus, and their content is live-derived); everything
/// else is compared against `base`.
pub fn project_generation(
    base: &CommittedManifest,
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
    let mut managed_ids = BTreeSet::new();
    // #294 tripwire: counted from the sealed manifest, so a resumed or no-op
    // generation republishes the same coverage instead of an empty one.
    let mut code = crate::CodeCoverage::default();

    // #971 change-detection indexes over the prior generation. A group that is
    // wholly equal to its committed self produced a byte-identical file
    // document except for `run_id` (same content id, digest, size, records,
    // aliases — `ManifestGroup` carries every field `file_doc` embeds), so it
    // is kept, not rewritten. Equality is also exactly the condition under
    // which `sync::plan_operations` plans no operation for the group: nothing
    // this generation does touches its live records or its document.
    let base_group_by_id: BTreeMap<&str, &crate::sync::ManifestGroup> = base
        .groups
        .iter()
        .map(|group| (group.group_id.as_str(), group))
        .collect();
    let base_prefix = base
        .execution
        .as_ref()
        .map_or(execution.prefix.as_str(), |prior| prior.prefix.as_str());
    let mut base_junk_by_key: BTreeMap<&str, Value> = BTreeMap::new();
    for junk in &base.plan.junk_files {
        base_junk_by_key.insert(junk.file_key.as_str(), serde_json::to_value(junk)?);
    }
    let base_duplicate_by_id: BTreeMap<String, &crate::state::DuplicateFile> = base
        .plan
        .duplicate_files
        .iter()
        .map(|alias| {
            (
                catalog::duplicate_file_id(
                    base_prefix,
                    &alias.file_key,
                    &alias.rel,
                    &alias.path_id,
                ),
                alias,
            )
        })
        .collect();

    for group in &desired.groups {
        let assignment = desired.plan.files.get(&group.content_id).with_context(|| {
            format!(
                "catalog group {} has no desired file assignment",
                group.group_id
            )
        })?;
        let format = assignment_format(assignment.family.as_str(), assignment.gzip);
        code.observe(&format, group.expected_records);
        let group_unchanged = base_group_by_id.get(group.group_id.as_str()).copied() == Some(group);
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
        managed_ids.insert(id.clone());
        if !group_unchanged {
            insert_unique(&mut documents, id, doc)?;
        }
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
            managed_ids.insert(id.clone());
            // The alias document embeds the group's canonical rel and size, so
            // it is only unchanged when the group is AND the prior generation
            // published this very alias id.
            if !(group_unchanged && base_duplicate_by_id.contains_key(&id)) {
                insert_unique(&mut documents, id, doc)?;
            }
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
        managed_ids.insert(id.clone());
        if base_junk_by_key.get(junk.file_key.as_str()) != Some(&serde_json::to_value(junk)?) {
            insert_unique(&mut documents, id, doc)?;
        }
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
        // Dataset documents are always written: their numbers are read back
        // live from the index, not derivable from the plan comparison, and
        // there is one per dataset, not one per file.
        managed_ids.insert(id.clone());
        insert_unique(&mut documents, id, doc)?;
    }

    for (id, correlation) in correlations {
        let mut correlation = correlation.clone();
        anyhow::ensure!(
            correlation.get("doc_kind").and_then(Value::as_str) == Some("correlation"),
            "catalog correlation {id} has the wrong doc_kind"
        );
        correlation["run_id"] = Value::String(metadata.generation_id.clone());
        managed_ids.insert(id.clone());
        insert_unique(&mut documents, id.clone(), correlation)?;
    }

    let files_total = desired
        .groups
        .iter()
        .map(|group| 1usize + group.aliases.len())
        .sum::<usize>()
        .checked_add(desired.plan.junk_files.len())
        .context("catalog file count overflow")?;
    let mut run_doc = json!({
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
    // #2336: absent unless every PDF failed to start the isolated parser, so
    // a generation with no such failure serializes the run document it always
    // did. The terminal line reads this field back and exits 1.
    if let Some(n) = desired.plan.pdf_worker_unavailable_count() {
        run_doc["pdf_worker_unavailable"] = json!(n);
    }
    // #2370: omitted when no family is entirely or mostly junked, so a clean
    // generation serializes the run document it always did.
    let family_warnings = crate::family_junk::from_groups(&desired.plan, &desired.groups);
    if !family_warnings.is_empty() {
        run_doc["family_junk_warnings"] = serde_json::to_value(&family_warnings)
            .expect("family junk warnings are strings and counts");
    }
    // #929: a generation that lacks a dataset says so in the one document every
    // reader of the corpus starts from (`Plan::refused_run_fields`).
    for (key, value) in desired.plan.refused_run_fields() {
        run_doc[key] = value;
    }
    managed_ids.insert(format!("run:{}", metadata.generation_id));
    insert_unique(
        &mut documents,
        format!("run:{}", metadata.generation_id),
        run_doc,
    )?;

    anyhow::ensure!(
        documents.values().all(|doc| {
            doc.get("run_id").and_then(Value::as_str) == Some(metadata.generation_id.as_str())
        }),
        "every catalog document this generation writes must carry the current generation run_id \
         (kept-unchanged documents keep the prior generation's, #971)"
    );

    // The prior generation's own managed ids, derived from its committed plan
    // and its run document, plus whatever the caller observed beyond the
    // manifest's knowledge (correlations). Everything in that universe that
    // the desired generation no longer manages is stale and must be deleted.
    let mut prior_ids = prior_managed_ids.clone();
    prior_ids.extend(
        base.plan
            .files
            .keys()
            .map(|key| catalog::file_id(base_prefix, key)),
    );
    prior_ids.extend(
        base.plan
            .junk_files
            .iter()
            .map(|junk| catalog::file_id(base_prefix, &junk.file_key)),
    );
    prior_ids.extend(base_duplicate_by_id.keys().cloned());
    prior_ids.extend(
        base.plan
            .datasets
            .iter()
            .map(|dataset| format!("ds:{}:{}", base_prefix, dataset.slug)),
    );
    if let Some(prior_tx) =
        base.execution
            .as_ref()
            .and_then(|execution| match &execution.source_policy {
                crate::sync::SourceExecutionPolicy::DurableSnapshot { reference, .. } => {
                    reference.strip_prefix("sync-snapshots/").map(str::to_owned)
                }
                crate::sync::SourceExecutionPolicy::AbortOnSourceChange { .. } => None,
            })
    {
        prior_ids.insert(format!("run:{}", prior_tx));
    }
    let stale_ids = prior_ids.difference(&managed_ids).cloned().collect();

    Ok(CatalogProjection {
        generation_id: metadata.generation_id.clone(),
        documents,
        managed_ids,
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
            text_analyzer: None,
            scope: None,
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
        // Indexed with zero records is not a junked family (#2370).
        assert!(run.get("family_junk_warnings").is_none(), "{run}");

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
        // Zero-record indexed code is the #294 case, not a junked family.
        // A clean corpus must not grow a family-junk field (#2370).
        assert!(run.get("family_junk_warnings").is_none(), "{run}");
    }

    /// #2370: the run document names a family whose files were all junked, and
    /// one that was only mostly junked, and stays silent for a clean family.
    #[test]
    fn the_run_document_warns_for_all_junked_and_mostly_junked_families_only() {
        let mut plan = Plan {
            datasets: vec![dataset()],
            ..Plan::default()
        };
        let mut code = assignment("app.py");
        code.family = "code".into();
        let mut html = assignment("index.html");
        html.family = "html".into();
        plan.files.insert("code".into(), code);
        plan.files.insert("html".into(), html);
        plan.junk_files.push(JunkFile {
            file_key: "pdf-1".into(),
            rel: "a.pdf".into(),
            format: "pdf".into(),
            status: "junk".into(),
            reason: "could not start the isolated PDF parser".into(),
            bytes: 4,
        });
        plan.junk_files.push(JunkFile {
            file_key: "pdf-2".into(),
            rel: "b.pdf".into(),
            format: "pdf(gzip)".into(),
            status: "junk".into(),
            reason: "could not start the isolated PDF parser".into(),
            bytes: 4,
        });
        plan.junk_files.push(JunkFile {
            file_key: "html-junk".into(),
            rel: "broken.html".into(),
            format: "html".into(),
            status: "junk".into(),
            reason: "no records extracted".into(),
            bytes: 2,
        });
        plan.junk_files.push(JunkFile {
            file_key: "html-junk-2".into(),
            rel: "broken-2.html".into(),
            format: "html".into(),
            status: "junk".into(),
            reason: "no records extracted".into(),
            bytes: 2,
        });
        let desired = manifest(
            1,
            "generation-1",
            plan,
            vec![
                group("code", "app.py", vec![]),
                group("html", "index.html", vec![]),
            ],
        );
        let base = committed(manifest(0, "generation-0", Plan::default(), vec![]));
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
        // Prior fields are still there.
        assert_eq!(run["files_junk"], 4);
        assert_eq!(run["code_files_indexed"], 1);
        let warnings = run["family_junk_warnings"]
            .as_array()
            .expect("all-junked and mostly-junked families are named");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(warnings[0]["family"], "html");
        assert_eq!(warnings[0]["extent"], "most");
        assert_eq!(warnings[0]["files"], 3);
        assert_eq!(warnings[0]["junk_files"], 2);
        assert_eq!(warnings[0]["indexed_files"], 1);
        assert_eq!(warnings[0]["top_reason"], "no records extracted");
        assert_eq!(warnings[0]["top_reason_files"], 2);
        assert_eq!(warnings[1]["family"], "pdf");
        assert_eq!(warnings[1]["extent"], "all");
        assert_eq!(warnings[1]["files"], 2);
        assert_eq!(warnings[1]["junk_files"], 2);
        assert_eq!(warnings[1]["indexed_files"], 0);
        assert_eq!(
            warnings[1]["top_reason"],
            "could not start the isolated PDF parser"
        );
        // code was indexed and not junked.
        assert!(warnings.iter().all(|w| w["family"] != "code"));
    }

    /// #971: this was `every_current_document_moves_to_latest_catalog_generation`
    /// — the invariant that made a one-file change rewrite the whole corpus's
    /// catalog documents. The contract inverted: an unchanged file's document
    /// is KEPT under the prior generation's run_id (still visible in the data
    /// map — ids are content-derived and never deleted, the map is
    /// prefix-scoped now), and only the documents that actually moved carry
    /// the new generation's run_id.
    #[test]
    fn a_one_file_change_writes_only_that_file_and_the_small_shared_documents() {
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

        // One file changed: its group moved (new digest, new record count).
        let mut changed_group = group("change", "change.csv", vec![]);
        changed_group.content_digest = "digest-2".into();
        changed_group.expected_records = 7;
        let desired = manifest(
            2,
            "generation-2",
            base_plan,
            vec![group("keep", "keep.csv", vec![]), changed_group],
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

        // Exactly the changed file plus the two per-corpus documents that are
        // always written. The unchanged peer is NOT re-sent.
        assert_eq!(
            projection.documents.keys().cloned().collect::<Vec<_>>(),
            vec![
                "ds:ax:reports".to_string(),
                "file:ax:change".to_string(),
                "run:generation-2".to_string(),
            ]
        );
        assert!(projection
            .documents
            .values()
            .all(|doc| { doc.get("run_id").and_then(Value::as_str) == Some("generation-2") }));
        // The unchanged peer is still a managed id — the sweep's authority
        // allows it to remain under the prior generation's run_id — and it is
        // not stale, so nothing deletes it.
        assert!(projection.managed_ids.contains("file:ax:keep"));
        assert!(!projection.stale_ids.contains("file:ax:keep"));
        // The prior generation's run document IS stale: superseded by
        // run:generation-2.
        assert!(projection.stale_ids.contains("run:generation-1"));

        // The same projection with nothing changed at all writes only the two
        // per-corpus documents — an incremental generation never touches the
        // corpus's file documents wholesale.
        let mut plan = Plan {
            datasets: vec![dataset()],
            ..Plan::default()
        };
        plan.files.insert("keep".into(), assignment("keep.csv"));
        plan.files.insert("change".into(), assignment("change.csv"));
        let desired = manifest(
            2,
            "generation-2",
            plan,
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
        assert_eq!(
            projection.documents.keys().cloned().collect::<Vec<_>>(),
            vec!["ds:ax:reports".to_string(), "run:generation-2".to_string(),]
        );
        for id in [
            "file:ax:keep",
            "file:ax:change",
            "ds:ax:reports",
            "run:generation-2",
        ] {
            assert!(projection.managed_ids.contains(id), "missing {id}");
        }
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

        // A long non-ASCII differing value must be reported, not panic the
        // truncation (byte-slicing would split a multibyte codepoint).
        let mut wide = projection.documents.clone();
        wide.get_mut("ds:ax:reports").unwrap()["notes"] = json!(["名前".repeat(60)]);
        let err = projection.validate_observed(&wide).unwrap_err().to_string();
        assert!(
            err.contains("notes"),
            "a wide non-ASCII diff must be reported: {err}"
        );
    }
}
