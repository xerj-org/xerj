//! Durable source preparation for incremental corpus reconciliation.
//!
//! This slice deliberately performs no remote mutation. It projects a
//! byte-verified inventory into generation groups and makes desired bytes
//! immutable before a later executor writes `sync_begin`.

use crate::content::Inventory;
use crate::state::{Journal, Plan};
use crate::sync::{
    self, CommittedManifest, DesiredContentGroup, GenerationManifest, ManifestGroup, ManifestPath,
    PendingSync, SourceExecutionPolicy, SyncOperation, SyncOperationState,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const SNAPSHOT_VERSION: u32 = 2;

#[cfg(test)]
thread_local! {
    // Test failpoints belong to the thread that arms them: an unrelated test
    // running in parallel must not consume another test's one-shot failure.
    static SNAPSHOT_FAILPOINT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static REPLAY_FAIL_AFTER_APPLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
static SNAPSHOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
static POST_SEAL_SOURCE_REPLACEMENT: std::sync::Mutex<Option<(std::path::PathBuf, Vec<u8>)>> =
    std::sync::Mutex::new(None);
#[cfg(test)]
pub(crate) static REPLAY_FAILPOINT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
static GC_FAIL_AFTER_RENAME: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotFile {
    pub content_id: String,
    pub content_digest: String,
    pub content_size: u64,
    pub relative_blob: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<PreparedArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedArtifact {
    pub relative_ndjson: String,
    pub records: u64,
    pub passages: u64,
    pub vectors: u64,
    /// Records the extractor produced that no dataset assignment could accept.
    /// Sealed here because nothing downstream can recover the number: junk is
    /// never published, so no read-back count sees it.
    #[serde(default)]
    pub junk: u64,
    /// Per-dataset breakdown of `records`, keyed by the dataset each record was
    /// written under. One file can feed several datasets — a SQL dump is one
    /// file and N tables — so the flat total is not comparable to any single
    /// dataset's read-back. Omitted when empty: `snapshot_digest` covers the
    /// serialized artifact, so an in-flight snapshot sealed before this field
    /// existed must keep hashing to the same value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub records_by_dataset: BTreeMap<String, u64>,
    pub bytes: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub version: u32,
    pub tx_id: String,
    /// Publication timestamp sealed once and reused by every replay.
    pub started: String,
    pub preparation_contract_digest: String,
    pub footprint: SnapshotFootprint,
    pub files: Vec<SnapshotFile>,
    pub snapshot_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotFootprint {
    pub source_bytes: u64,
    pub prepared_bytes: u64,
    pub total_bytes: u64,
    pub hard_budget_bytes: u64,
}

/// Logical payload accounting for source and prepared-record bytes.
/// This is deliberately not a filesystem-allocation limit: directory,
/// manifest, and block-allocation overhead are excluded.
struct PayloadBudget {
    used: u64,
    limit: u64,
}

impl PayloadBudget {
    fn charge(&mut self, bytes: usize) -> std::io::Result<()> {
        let bytes = u64::try_from(bytes)
            .map_err(|_| std::io::Error::other("snapshot payload write length overflow"))?;
        let next = self
            .used
            .checked_add(bytes)
            .ok_or_else(|| std::io::Error::other("snapshot payload byte count overflow"))?;
        if next > self.limit {
            return Err(std::io::Error::other(format!(
                "snapshot logical payload write of {bytes} bytes would raise staged payload from \
                 {} to {next} bytes, exceeding configured limit of {} bytes; no generation was \
                 committed and the partial snapshot is removed automatically",
                self.used, self.limit
            )));
        }
        self.used = next;
        Ok(())
    }
}

struct BudgetWriter<'a, W> {
    inner: W,
    budget: &'a mut PayloadBudget,
}

impl<W: Write> Write for BudgetWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.budget.charge(buf.len())?;
        match self.inner.write(buf) {
            Ok(written) if written == buf.len() => Ok(written),
            Ok(written) => {
                self.budget.used -= (buf.len() - written) as u64;
                Ok(written)
            }
            Err(error) => {
                self.budget.used -= buf.len() as u64;
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct StagingCleanup {
    path: std::path::PathBuf,
    armed: bool,
}

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Remote mutations must be convergent: calling `apply` twice for one
/// operation after an accepted-but-unrecorded response must produce exactly
/// the same live state. `validate` is the final generation-wide barrier.
#[cfg_attr(not(test), allow(dead_code))]
pub trait SyncOperationBackend {
    fn provision_generation(&mut self, _desired: &GenerationManifest) -> Result<()> {
        Ok(())
    }

    fn apply(
        &mut self,
        operation: &SyncOperation,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()>;

    fn validate(
        &mut self,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()>;

    /// Publish and exactly read back the complete agent-facing catalog for
    /// this generation. This runs before the validation/authority barrier.
    fn publish_generation_catalog(
        &mut self,
        _base: &CommittedManifest,
        _desired: &GenerationManifest,
        _snapshot: &SourceSnapshot,
    ) -> Result<()> {
        Ok(())
    }
}

/// Production ES-compatible operation backend for graph-disabled generations.
///
/// Upserts stream sealed index actions after removing both replaced and
/// retry-partial content. Metadata operations read deterministic IDs from the
/// committed snapshot and issue partial updates containing provenance only,
/// so unchanged semantic fields are never re-embedded.
#[cfg_attr(not(test), allow(dead_code))]
pub struct EsSyncBackend<'a> {
    es: &'a crate::esclient::Es,
    state_dir: &'a Path,
    bulk_bytes: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<'a> EsSyncBackend<'a> {
    pub fn new(es: &'a crate::esclient::Es, state_dir: &'a Path, bulk_bytes: usize) -> Self {
        Self {
            es,
            state_dir,
            bulk_bytes: bulk_bytes.max(64 * 1024),
        }
    }

    fn delete_group(&self, group: &ManifestGroup, plan: &Plan) -> Result<()> {
        for index in group_indices(group, plan)? {
            self.es.delete_by_query(
                &index,
                &serde_json::json!({"term": {
                    "ax_file": &group.content_id
                }}),
            )?;
        }
        Ok(())
    }

    fn replay_prepared(&self, snapshot: &SourceSnapshot, content_id: &str) -> Result<()> {
        let prepared = prepared_for(snapshot, content_id)?;
        let snapshot_dir = self.state_dir.join("sync-snapshots").join(&snapshot.tx_id);
        let file = File::open(snapshot_dir.join(&prepared.relative_ndjson))?;
        stream_ndjson_pairs(BufReader::new(file), self.bulk_bytes, |body| {
            checked_bulk(self.es, body)
        })
    }

    fn replay_metadata(
        &self,
        base: &CommittedManifest,
        desired_group: &ManifestGroup,
    ) -> Result<()> {
        let base_snapshot = open_committed_snapshot(self.state_dir, base)?;
        let old_group = base
            .groups
            .iter()
            .find(|group| group.group_id == desired_group.group_id)
            .context("metadata operation has no committed group")?;
        let prepared = prepared_for(&base_snapshot, &old_group.content_id)?;
        let snapshot_dir = self
            .state_dir
            .join("sync-snapshots")
            .join(&base_snapshot.tx_id);
        let reader = BufReader::new(File::open(snapshot_dir.join(&prepared.relative_ndjson))?);
        let paths: Vec<Value> = std::iter::once(&desired_group.canonical)
            .chain(desired_group.aliases.iter())
            .map(|path| Value::String(path.rel.clone()))
            .collect();
        stream_metadata_updates(
            reader,
            self.bulk_bytes,
            &desired_group.canonical.rel,
            &paths,
            |body| checked_bulk(self.es, body),
        )
    }

    fn exact_group_count(&self, group: &ManifestGroup, plan: &Plan) -> Result<u64> {
        let mut total = 0u64;
        for index in group_indices(group, plan)? {
            let response = self.es.search(
                &index,
                &serde_json::json!({
                    "size": 0,
                    "track_total_hits": true,
                    "query": {"term": {"ax_file": &group.content_id}}
                }),
            )?;
            total = total
                .checked_add(
                    response
                        .pointer("/hits/total/value")
                        .and_then(Value::as_u64)
                        .context("exact validation response has no total hit count")?,
                )
                .context("exact validation count overflow")?;
        }
        Ok(total)
    }

    fn exact_semantic_count(&self, group: &ManifestGroup, plan: &Plan) -> Result<u64> {
        let by_slug: HashMap<&str, &crate::state::PlanDataset> = plan
            .datasets
            .iter()
            .map(|dataset| (dataset.slug.as_str(), dataset))
            .collect();
        let mut total = 0u64;
        for slug in &group.dataset_slugs {
            let dataset = by_slug
                .get(slug.as_str())
                .copied()
                .with_context(|| format!("group references absent dataset {slug}"))?;
            let Some(field) = &dataset.semantic_field else {
                continue;
            };
            let response = self.es.search(
                &dataset.index,
                &serde_json::json!({
                    "size": 0,
                    "track_total_hits": true,
                    "query": {"bool": {"must": [
                        {"term": {"ax_file": &group.content_id}},
                        {"exists": {"field": field}}
                    ]}}
                }),
            )?;
            total = total
                .checked_add(
                    response
                        .pointer("/hits/total/value")
                        .and_then(Value::as_u64)
                        .context("semantic validation response has no total hit count")?,
                )
                .context("semantic validation count overflow")?;
        }
        Ok(total)
    }

    fn catalog_generation(&self, run_id: &str) -> Result<BTreeMap<String, Value>> {
        let mut documents = BTreeMap::new();
        let mut search_after: Option<Value> = None;
        loop {
            let mut body = serde_json::json!({
                "size": 1000,
                "sort": [{"_id": "asc"}],
                "query": {"term": {"run_id": run_id}}
            });
            if let Some(after) = &search_after {
                body["search_after"] = after.clone();
            }
            let response = self.es.search(crate::catalog::CATALOG_INDEX, &body)?;
            let hits = response
                .pointer("/hits/hits")
                .and_then(Value::as_array)
                .context("catalog generation query has no hits")?;
            for hit in hits {
                let id = hit
                    .get("_id")
                    .and_then(Value::as_str)
                    .context("catalog hit has no _id")?
                    .to_owned();
                let source = hit
                    .get("_source")
                    .cloned()
                    .context("catalog hit has no _source")?;
                anyhow::ensure!(
                    documents.insert(id.clone(), source).is_none(),
                    "catalog generation query returned duplicate ID {id}"
                );
            }
            if hits.len() < 1000 {
                break;
            }
            search_after = hits.last().and_then(|hit| hit.get("sort")).cloned();
            anyhow::ensure!(
                search_after.is_some(),
                "full catalog generation page has no continuation sort key"
            );
        }
        Ok(documents)
    }

    fn exact_dataset_catalog_stats(
        &self,
        desired: &GenerationManifest,
    ) -> Result<BTreeMap<String, crate::generation_catalog::DatasetCatalogStats>> {
        let mut out = BTreeMap::new();
        for dataset in &desired.plan.datasets {
            let groups: Vec<&ManifestGroup> = desired
                .groups
                .iter()
                .filter(|group| group.dataset_slugs.contains(&dataset.slug))
                .collect();
            let content_ids: Vec<&str> = groups
                .iter()
                .map(|group| group.content_id.as_str())
                .collect();
            // Filter on `ax_dataset` as well as `ax_file`: the read-back is then
            // per-dataset by construction, matching the identity the records
            // were sealed under, and stays exact even if two datasets ever share
            // one index. `ax_dataset` is written at every sink site and is a
            // mapped `PROVENANCE_FIELDS` keyword.
            let mut body = serde_json::json!({
                "size": 0,
                "track_total_hits": true,
                "query": {"bool": {"filter": [
                    {"terms": {"ax_file": content_ids}},
                    {"term": {"ax_dataset": dataset.slug}}
                ]}}
            });
            if let Some(field) = &dataset.time_field {
                body["aggs"] = serde_json::json!({
                    "time_min": {"min": {"field": field}},
                    "time_max": {"max": {"field": field}}
                });
            }
            let response = self.es.search(&dataset.index, &body)?;
            let record_count = response
                .pointer("/hits/total/value")
                .and_then(Value::as_u64)
                .context("dataset exact read-back has no total")?;
            // A group's sealed record count is a property of the *content*, not
            // of a dataset: one file fans out over N datasets (a SQL dump is N
            // tables), so its flat total is comparable to no single dataset's
            // read-back. Fold the per-dataset ledger instead — the counts keyed
            // by the identity they were written under. `None` is a group sealed
            // before that ledger existed which fanned out anyway: genuinely
            // unattributable, so the equality is skipped for this dataset rather
            // than guessed at, and the exact read-back is still published as the
            // statistic. The check itself stays fatal — a read-back that
            // disagrees with a seal it *can* be compared against is a corruption
            // signal, not junk.
            let expected = groups
                .iter()
                .map(|group| group.expected_records_for(&dataset.slug))
                .try_fold(Some(0u64), |sum, count| match (sum, count) {
                    (Some(sum), Some(count)) => sum
                        .checked_add(count)
                        .context("dataset expected record count overflow")
                        .map(Some),
                    _ => Ok(None),
                })?;
            if let Some(expected) = expected {
                anyhow::ensure!(
                    record_count == expected,
                    "dataset {} exact read-back count {record_count} disagrees with sealed count \
                     {expected} across groups {}",
                    dataset.slug,
                    groups
                        .iter()
                        .map(|group| group.group_id.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
            let mut formats: Vec<String> = desired
                .plan
                .files
                .values()
                .filter(|assignment| {
                    assignment
                        .assignments
                        .iter()
                        .any(|(_, slug)| slug == &dataset.slug)
                })
                .map(|assignment| {
                    if assignment.gzip {
                        format!("{}(gzip)", assignment.family)
                    } else {
                        assignment.family.clone()
                    }
                })
                .collect();
            formats.sort();
            formats.dedup();
            let bytes = groups.iter().try_fold(0u64, |sum, group| {
                sum.checked_add(group.content_size)
                    .context("dataset source byte count overflow")
            })?;
            // Junk is a property of the *file*, not of a dataset: a record that
            // no assignment accepted belongs to no dataset by definition. A
            // group that feeds several datasets therefore charges its junk to
            // exactly one of them — the first of its sorted slugs — so the
            // run-level `junk_records_total` (a sum over datasets) stays exact
            // instead of multiplying by fan-out. The legacy path attributes it
            // the same way, to `fa.assignments.first()` (lib.rs).
            let junk_records = groups
                .iter()
                .filter(|group| group.dataset_slugs.first() == Some(&dataset.slug))
                .try_fold(0u64, |sum, group| {
                    sum.checked_add(group.expected_junk_records)
                        .context("dataset junk-record count overflow")
                })?;
            let time = |name: &str| {
                response
                    .pointer(&format!("/aggregations/{name}/value_as_string"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            };
            out.insert(
                dataset.slug.clone(),
                crate::generation_catalog::DatasetCatalogStats {
                    record_count,
                    junk_records,
                    bytes,
                    formats,
                    time_min: time("time_min"),
                    time_max: time("time_max"),
                    sample_queries: crate::catalog::build_sample_queries(dataset, &[]),
                    notes: dataset
                        .group
                        .iter()
                        .map(|group| format!("source table: {group}"))
                        .chain(dataset.specs.iter().flat_map(|spec| {
                            spec.notes
                                .iter()
                                .map(move |note| format!("{}: {note}", spec.name))
                        }))
                        .collect(),
                },
            );
        }
        Ok(out)
    }
}

impl SyncOperationBackend for EsSyncBackend<'_> {
    fn provision_generation(&mut self, desired: &GenerationManifest) -> Result<()> {
        let execution = desired
            .execution
            .as_ref()
            .context("desired generation has no execution identity")?;
        let (_, index_identity) = crate::generation_contract_identities(&desired.plan)?;
        anyhow::ensure!(
            execution.index_identity == index_identity,
            "desired generation index identity disagrees with its frozen mappings"
        );
        crate::ensure_generation_mappings(self.es, &desired.plan)
    }

    fn apply(
        &mut self,
        operation: &SyncOperation,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()> {
        anyhow::ensure!(
            desired
                .execution
                .as_ref()
                .is_some_and(|execution| !execution.graph_enabled),
            "production incremental graph reconciliation is not enabled yet"
        );
        let old = base
            .groups
            .iter()
            .find(|group| group.group_id == operation.group_id);
        let new = desired
            .groups
            .iter()
            .find(|group| group.group_id == operation.group_id);
        match operation.kind {
            crate::sync::SyncOperationKind::Delete => self.delete_group(
                old.context("delete operation has no committed group")?,
                &base.plan,
            )?,
            crate::sync::SyncOperationKind::Upsert => {
                if let Some(old) = old {
                    self.delete_group(old, &base.plan)?;
                }
                let new = new.context("upsert operation has no desired group")?;
                // Remove a partial prior retry of the desired identity too.
                self.delete_group(new, &desired.plan)?;
                self.replay_prepared(snapshot, &new.content_id)?;
            }
            crate::sync::SyncOperationKind::Metadata => {
                self.replay_metadata(
                    base,
                    new.context("metadata operation has no desired group")?,
                )?;
            }
        }
        Ok(())
    }

    fn publish_generation_catalog(
        &mut self,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()> {
        for index in desired
            .plan
            .datasets
            .iter()
            .map(|dataset| dataset.index.as_str())
            .collect::<std::collections::BTreeSet<_>>()
        {
            self.es.refresh(index)?;
        }
        self.es.refresh(crate::catalog::CATALOG_INDEX)?;
        let prior_run_id =
            base.execution
                .as_ref()
                .and_then(|execution| match &execution.source_policy {
                    SourceExecutionPolicy::DurableSnapshot { reference, .. } => {
                        reference.strip_prefix("sync-snapshots/")
                    }
                    SourceExecutionPolicy::AbortOnSourceChange { .. } => None,
                });
        let prior_documents = prior_run_id
            .map(|run_id| self.catalog_generation(run_id))
            .transpose()?
            .unwrap_or_default();
        let prior_ids = prior_documents.keys().cloned().collect();
        let stats = self.exact_dataset_catalog_stats(desired)?;
        let projection = crate::generation_catalog::project_generation(
            base,
            desired,
            &crate::generation_catalog::GenerationCatalogMetadata {
                generation_id: snapshot.tx_id.clone(),
                started: snapshot.started.clone(),
            },
            &stats,
            &BTreeMap::new(),
            &prior_ids,
        )?;
        let mut body = Vec::new();
        for id in &projection.stale_ids {
            let action =
                serde_json::json!({"delete": {"_index": crate::catalog::CATALOG_INDEX, "_id": id}});
            body.extend_from_slice(serde_json::to_string(&action)?.as_bytes());
            body.push(b'\n');
        }
        for (id, document) in &projection.documents {
            append_index_action(crate::catalog::CATALOG_INDEX, id, document, &mut body)?;
        }
        checked_bulk(self.es, body)?;
        self.es.refresh(crate::catalog::CATALOG_INDEX)?;
        projection.validate_observed(&self.catalog_generation(&snapshot.tx_id)?)?;
        if let Some(prior_run_id) = prior_run_id {
            let remaining = self.catalog_generation(prior_run_id)?;
            anyhow::ensure!(
                remaining.is_empty(),
                "prior catalog generation {prior_run_id} still has {} managed documents",
                remaining.len()
            );
        }
        Ok(())
    }

    fn validate(
        &mut self,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()> {
        for dataset in &desired.plan.datasets {
            self.es.refresh(&dataset.index)?;
        }
        self.es.refresh(crate::catalog::CATALOG_INDEX)?;
        for group in &desired.groups {
            anyhow::ensure!(
                self.exact_group_count(group, &desired.plan)? == group.expected_records,
                "live record count disagrees with desired group {}",
                group.group_id
            );
            let semantic = self.exact_semantic_count(group, &desired.plan)?;
            anyhow::ensure!(
                semantic == group.expected_passages && semantic == group.expected_vectors,
                "live semantic count disagrees with desired group {}",
                group.group_id
            );
            // Scoped to this generation's `run_id`, the same way the run-summary
            // read-back is (`lib.rs`). `file_key` is derived from CONTENT alone
            // (`content::full_digest`) and the catalog is one global index that no
            // `--prefix` namespaces, so an unscoped count also sees the
            // canonical and alias documents that ANOTHER corpus on this node
            // published for byte-identical content — two Apache-2.0 checkouts
            // sharing a LICENSE is enough. Those documents are that run's, not
            // this one's: counting them aborted a generation whose own
            // publication was exactly right (#360). What this barrier is for is
            // "this generation published one canonical document and its aliases",
            // and that is what it now asks.
            let catalog = self.es.search(
                crate::catalog::CATALOG_INDEX,
                &serde_json::json!({
                    "size": 0,
                    "track_total_hits": true,
                    "query": {"bool": {"filter": [
                        {"term": {"file_key": &group.content_id}},
                        {"term": {"run_id": &snapshot.tx_id}}
                    ]}}
                }),
            )?;
            anyhow::ensure!(
                catalog.pointer("/hits/total/value").and_then(Value::as_u64)
                    == Some(1 + group.aliases.len() as u64),
                "catalog canonical/alias count disagrees with desired group {}",
                group.group_id
            );
        }
        let desired_ids: std::collections::HashSet<&str> = desired
            .groups
            .iter()
            .map(|group| group.content_id.as_str())
            .collect();
        for old in &base.groups {
            if !desired_ids.contains(old.content_id.as_str()) {
                anyhow::ensure!(
                    self.exact_group_count(old, &base.plan)? == 0,
                    "replaced or deleted content {} remains live",
                    old.content_id
                );
            }
        }
        Ok(())
    }
}

/// Resume a journaled generation strictly from its verified snapshot.
///
/// The durable order is Started -> convergent remote apply -> Committed.
/// Therefore a crash after an accepted response repeats the same operation;
/// a crash after Committed skips it. Only an exact backend validation allows
/// `sync_validated` and the minimal authority switch in `sync_commit`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn replay_pending_operations(
    state_dir: &Path,
    journal: &mut Journal,
    backend: &mut impl SyncOperationBackend,
) -> Result<()> {
    let pending = journal
        .pending_sync
        .clone()
        .context("cannot replay without a pending corpus generation")?;
    let base = journal
        .committed_manifest
        .clone()
        .context("pending corpus generation has no committed base")?;
    // Same contract as the journal-replay wrap in `state.rs` (#283): a pending
    // generation that fails its own re-validation is unrepairable by
    // re-running, and the internal invariant alone is not actionable.
    pending.validate_against(&base).with_context(|| {
        format!(
            "the pending corpus generation {} no longer re-validates against committed \
             generation {}; the generation journal is not internally consistent, and re-running \
             will not repair it. No remote data was changed. Rebuild with a new --state-dir and \
             a new --prefix",
            pending.desired.generation, base.generation
        )
    })?;
    let snapshot = open_snapshot(state_dir, &pending.tx_id)?;
    verify_snapshot_binding(&pending, &snapshot)?;
    backend.provision_generation(&pending.desired)?;

    for operation in &pending.operations {
        let state = journal
            .pending_sync
            .as_ref()
            .and_then(|sync| sync.operation_states.get(&operation.operation_id))
            .cloned();
        if state == Some(SyncOperationState::Committed) {
            continue;
        }
        if state.is_none() {
            journal.sync_operation_state(&operation.operation_id, SyncOperationState::Started)?;
        }
        backend.apply(operation, &base, &pending.desired, &snapshot)?;
        replay_fail_after_apply()?;
        journal.sync_operation_state(&operation.operation_id, SyncOperationState::Committed)?;
    }
    backend.publish_generation_catalog(&base, &pending.desired, &snapshot)?;
    backend.validate(&base, &pending.desired, &snapshot)?;
    journal.sync_validated()?;
    journal.sync_commit()?;
    gc_snapshots(state_dir, journal).context(
        "generation committed durably, but snapshot cleanup failed; inspect the attached cause \
         (remove a refused symlink or repair the reported filesystem permission/I/O problem), \
         then retry the same command to continue bounded cleanup without republishing data",
    )
}

const SNAPSHOT_GC_BATCH_SIZE: usize = 4096;

fn protected_snapshot(
    state_dir: &Path,
    execution: &crate::sync::ExecutionIdentity,
) -> Result<String> {
    let SourceExecutionPolicy::DurableSnapshot {
        reference,
        snapshot_digest,
    } = &execution.source_policy
    else {
        anyhow::bail!("generated authority does not reference a durable snapshot");
    };
    let tx_id = reference
        .strip_prefix("sync-snapshots/")
        .context("protected snapshot reference is not state-relative")?;
    validate_tx_id(tx_id)?;
    let snapshot = open_snapshot(state_dir, tx_id)?;
    anyhow::ensure!(
        snapshot.snapshot_digest == *snapshot_digest,
        "protected snapshot digest disagrees with journal authority"
    );
    Ok(tx_id.to_owned())
}

/// Reclaim only snapshots proven unreferenced by replayed journal authority.
/// The caller owns the journal lock for the complete validation/rename/fsync
/// sequence.
pub fn gc_snapshots(state_dir: &Path, journal: &Journal) -> Result<()> {
    let root = state_dir.join("sync-snapshots");
    let mut protected = std::collections::HashSet::new();
    if let Some(execution) = journal
        .committed_manifest
        .as_ref()
        .and_then(|manifest| manifest.execution.as_ref())
    {
        protected.insert(protected_snapshot(state_dir, execution)?);
    }
    if let Some(execution) = journal
        .pending_sync
        .as_ref()
        .and_then(|pending| pending.desired.execution.as_ref())
    {
        protected.insert(protected_snapshot(state_dir, execution)?);
    }
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut entries = entries.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    // Validate the complete directory before the first mutation. In
    // particular, a late symlink must not be discovered after earlier
    // snapshots have already been deleted.
    for entry in &entries {
        anyhow::ensure!(
            !entry.file_type()?.is_symlink(),
            "snapshot cleanup refuses symlink {}",
            entry.path().display()
        );
    }
    let directory = File::open(&root)?;
    for entry in entries.into_iter().take(SNAPSHOT_GC_BATCH_SIZE) {
        let metadata = entry.file_type()?;
        if !metadata.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if protected.contains(&name) {
            continue;
        }
        let tombstone = if name.ends_with(".gc") {
            entry.path()
        } else {
            let tombstone = root.join(format!(".{name}.gc"));
            anyhow::ensure!(
                !tombstone.exists(),
                "snapshot tombstone already exists: {}",
                tombstone.display()
            );
            std::fs::rename(entry.path(), &tombstone)?;
            directory.sync_all()?;
            #[cfg(test)]
            if GC_FAIL_AFTER_RENAME.swap(false, std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("injected snapshot GC crash after durable tombstone rename");
            }
            tombstone
        };
        std::fs::remove_dir_all(&tombstone)?;
        directory.sync_all()?;
    }
    Ok(())
}

/// Test helper proving a pending generation binds before mutable discovery.
/// Production calls `replay_pending_operations` at this boundary.
#[cfg(test)]
pub fn require_resumable_pending_source(
    state_dir: &Path,
    pending: Option<&PendingSync>,
) -> Result<()> {
    let Some(pending) = pending else {
        return Ok(());
    };
    let snapshot = open_snapshot(state_dir, &pending.tx_id).with_context(|| {
        format!(
            "pending corpus generation {} must resume before source discovery",
            pending.tx_id
        )
    })?;
    verify_snapshot_binding(pending, &snapshot)?;
    anyhow::bail!(
        "corpus generation {} is pending with verified durable source snapshot {} ({} files); \
         incremental operation replay is not enabled by this executor slice, so no source \
         discovery or remote mutation was attempted",
        pending.tx_id,
        snapshot.snapshot_digest,
        snapshot.files.len()
    )
}

fn verify_snapshot_binding(pending: &PendingSync, snapshot: &SourceSnapshot) -> Result<()> {
    let execution = pending
        .desired
        .execution
        .as_ref()
        .context("pending generation has no execution identity")?;
    let SourceExecutionPolicy::DurableSnapshot {
        reference,
        snapshot_digest,
    } = &execution.source_policy
    else {
        anyhow::bail!("pending generation is not bound to a durable source snapshot");
    };
    anyhow::ensure!(
        reference == &format!("sync-snapshots/{}", pending.tx_id)
            && snapshot_digest == &snapshot.snapshot_digest,
        "pending generation source snapshot binding does not match verified snapshot"
    );
    Ok(())
}

fn open_committed_snapshot(
    state_dir: &Path,
    committed: &CommittedManifest,
) -> Result<SourceSnapshot> {
    let execution = committed
        .execution
        .as_ref()
        .context("committed generation has no execution identity")?;
    let SourceExecutionPolicy::DurableSnapshot {
        reference,
        snapshot_digest,
    } = &execution.source_policy
    else {
        anyhow::bail!("committed generation is not bound to a durable snapshot");
    };
    let tx_id = reference
        .strip_prefix("sync-snapshots/")
        .context("committed snapshot reference is not state-relative")?;
    let snapshot = open_snapshot(state_dir, tx_id)?;
    anyhow::ensure!(
        &snapshot.snapshot_digest == snapshot_digest,
        "committed snapshot digest mismatch"
    );
    Ok(snapshot)
}

fn prepared_for<'a>(
    snapshot: &'a SourceSnapshot,
    content_id: &str,
) -> Result<&'a PreparedArtifact> {
    snapshot
        .files
        .iter()
        .find(|file| file.content_id == content_id)
        .and_then(|file| file.prepared.as_ref())
        .with_context(|| format!("content {content_id} has no sealed prepared artifact"))
}

fn group_indices(group: &ManifestGroup, plan: &Plan) -> Result<Vec<String>> {
    let by_slug: HashMap<&str, &str> = plan
        .datasets
        .iter()
        .map(|dataset| (dataset.slug.as_str(), dataset.index.as_str()))
        .collect();
    let mut indices = group
        .dataset_slugs
        .iter()
        .map(|slug| {
            by_slug
                .get(slug.as_str())
                .map(|index| (*index).to_string())
                .with_context(|| format!("group references absent dataset {slug}"))
        })
        .collect::<Result<Vec<_>>>()?;
    indices.sort();
    indices.dedup();
    Ok(indices)
}

fn checked_bulk(es: &crate::esclient::Es, body: Vec<u8>) -> Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    let outcome = es.bulk(body)?;
    anyhow::ensure!(
        outcome.item_errors == 0,
        "prepared bulk contained {} rejected items: {}",
        outcome.item_errors,
        outcome
            .first_error
            .unwrap_or_else(|| "unknown error".into())
    );
    Ok(())
}

fn append_index_action(index: &str, id: &str, doc: &Value, body: &mut Vec<u8>) -> Result<()> {
    serde_json::to_writer(
        &mut *body,
        &serde_json::json!({"index": {"_index": index, "_id": id}}),
    )?;
    body.push(b'\n');
    serde_json::to_writer(&mut *body, doc)?;
    body.push(b'\n');
    Ok(())
}

fn stream_ndjson_pairs(
    mut reader: impl std::io::BufRead,
    bulk_bytes: usize,
    mut send: impl FnMut(Vec<u8>) -> Result<()>,
) -> Result<()> {
    let mut body = Vec::with_capacity(bulk_bytes);
    loop {
        let mut action = Vec::new();
        if reader.read_until(b'\n', &mut action)? == 0 {
            break;
        }
        let mut document = Vec::new();
        anyhow::ensure!(
            reader.read_until(b'\n', &mut document)? > 0,
            "prepared NDJSON ended without a document"
        );
        if !body.is_empty() && body.len() + action.len() + document.len() > bulk_bytes {
            send(std::mem::take(&mut body))?;
            body.reserve(bulk_bytes);
        }
        body.extend_from_slice(&action);
        body.extend_from_slice(&document);
    }
    send(body)
}

fn stream_metadata_updates(
    mut reader: impl std::io::BufRead,
    bulk_bytes: usize,
    canonical: &str,
    paths: &[Value],
    mut send: impl FnMut(Vec<u8>) -> Result<()>,
) -> Result<()> {
    let mut body = Vec::with_capacity(bulk_bytes);
    loop {
        let mut action_line = String::new();
        if reader.read_line(&mut action_line)? == 0 {
            break;
        }
        let mut ignored_document = String::new();
        anyhow::ensure!(
            reader.read_line(&mut ignored_document)? > 0,
            "prepared NDJSON ended without a document"
        );
        let action: Value = serde_json::from_str(&action_line)?;
        let index = action
            .pointer("/index/_index")
            .and_then(Value::as_str)
            .context("prepared action has no index")?;
        let id = action
            .pointer("/index/_id")
            .and_then(Value::as_str)
            .context("prepared action has no ID")?;
        let update =
            serde_json::to_vec(&serde_json::json!({"update": {"_index": index, "_id": id}}))?;
        let patch = serde_json::to_vec(&serde_json::json!({"doc": {
            "ax_path": canonical,
            "ax_paths": paths
        }}))?;
        let required = update.len() + patch.len() + 2;
        if !body.is_empty() && body.len() + required > bulk_bytes {
            send(std::mem::take(&mut body))?;
            body.reserve(bulk_bytes);
        }
        body.extend_from_slice(&update);
        body.push(b'\n');
        body.extend_from_slice(&patch);
        body.push(b'\n');
    }
    send(body)
}

/// Build desired groups without guessing assignments. Output counts survive
/// only when the committed content identity is byte-identical.
#[cfg_attr(not(test), allow(dead_code))]
pub fn groups_from_inventory(
    inventory: &Inventory,
    plan: &Plan,
    previous: &[ManifestGroup],
) -> Result<Vec<ManifestGroup>> {
    ensure_inventory_lengths(inventory)?;
    let previous_by_content: HashMap<&str, &ManifestGroup> = previous
        .iter()
        .map(|group| (group.content_id.as_str(), group))
        .collect();
    let mut aliases_by_key: HashMap<&str, Vec<ManifestPath>> = HashMap::new();
    for alias in &inventory.duplicates {
        aliases_by_key
            .entry(alias.file_key.as_str())
            .or_default()
            .push(ManifestPath {
                path_id: alias.path_id.clone(),
                rel: alias.rel.clone(),
                is_symlink: alias.is_symlink.with_context(|| {
                    format!("alias {} has no persisted symlink rank", alias.rel)
                })?,
            });
    }
    let desired = inventory
        .files
        .iter()
        .zip(&inventory.keys)
        .zip(&inventory.digests)
        .map(|((file, content_id), content_digest)| {
            let assignment = plan.files.get(content_id).with_context(|| {
                format!("typed plan has no assignment for content {content_id}")
            })?;
            anyhow::ensure!(
                assignment.rel == file.rel
                    && assignment.path_id == file.rel_id
                    && assignment.content_digest.as_deref() == Some(content_digest.as_str()),
                "typed plan canonical projection disagrees with inventory for {}",
                file.rel
            );
            let mut dataset_slugs: Vec<String> = assignment
                .assignments
                .iter()
                .map(|(_, slug)| slug.clone())
                .collect();
            dataset_slugs.sort();
            dataset_slugs.dedup();
            let prior = previous_by_content.get(content_id.as_str()).copied();
            Ok(DesiredContentGroup {
                content_id: content_id.clone(),
                content_digest: content_digest.clone(),
                content_size: file.size,
                paths: std::iter::once(ManifestPath {
                    path_id: file.rel_id.clone(),
                    rel: file.rel.clone(),
                    is_symlink: file.is_symlink,
                })
                .chain(
                    aliases_by_key
                        .get(content_id.as_str())
                        .into_iter()
                        .flatten()
                        .cloned(),
                )
                .collect(),
                dataset_slugs,
                expected_records: prior.map_or(0, |group| group.expected_records),
                expected_passages: prior.map_or(0, |group| group.expected_passages),
                expected_vectors: prior.map_or(0, |group| group.expected_vectors),
                expected_junk_records: prior.map_or(0, |group| group.expected_junk_records),
                expected_records_by_dataset: prior
                    .map(|group| group.expected_records_by_dataset.clone())
                    .unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    sync::reconcile_groups(previous, desired)
}

/// Bind exact prepared output cardinalities into the desired manifest before
/// `sync_begin`. Content without a prepared artifact is rejected; unchanged
/// groups should retain their already committed counts instead.
#[cfg_attr(not(test), allow(dead_code))]
pub fn bind_prepared_counts(
    groups: &mut [ManifestGroup],
    snapshot: &SourceSnapshot,
    prepared_content: &[String],
) -> Result<()> {
    let prepared_content: std::collections::HashSet<&str> =
        prepared_content.iter().map(String::as_str).collect();
    let by_content: HashMap<&str, &PreparedArtifact> = snapshot
        .files
        .iter()
        .filter_map(|file| {
            file.prepared
                .as_ref()
                .map(|prepared| (file.content_id.as_str(), prepared))
        })
        .collect();
    for group in groups {
        if !prepared_content.contains(group.content_id.as_str()) {
            continue;
        }
        let prepared = by_content
            .get(group.content_id.as_str())
            .context("desired content has no prepared artifact")?;
        group.expected_records = prepared.records;
        group.expected_passages = prepared.passages;
        group.expected_vectors = prepared.vectors;
        group.expected_junk_records = prepared.junk;
        group.expected_records_by_dataset = prepared.records_by_dataset.clone();
    }
    Ok(())
}

/// Fsync blobs and manifest in a private directory, then atomically publish
/// the transaction snapshot. Retries reuse a verified final snapshot or
/// replace only that transaction's incomplete staging directory.
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_snapshot(
    state_dir: &Path,
    tx_id: &str,
    inventory: &Inventory,
) -> Result<SourceSnapshot> {
    create_snapshot_inner(
        state_dir,
        tx_id,
        inventory,
        None,
        "source-snapshot-v1",
        u64::MAX,
    )
}

/// Seal deterministic bulk actions together with source bytes. Extraction and
/// coercion happen before `sync_begin`; retries stream this artifact and never
/// re-extract or re-embed unchanged content.
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_prepared_snapshot(
    state_dir: &Path,
    tx_id: &str,
    inventory: &Inventory,
    plan: &Plan,
    preparation_contract_digest: &str,
    hard_budget_bytes: u64,
) -> Result<SourceSnapshot> {
    create_snapshot_inner(
        state_dir,
        tx_id,
        inventory,
        Some(plan),
        preparation_contract_digest,
        hard_budget_bytes,
    )
}

fn create_snapshot_inner(
    state_dir: &Path,
    tx_id: &str,
    inventory: &Inventory,
    plan: Option<&Plan>,
    preparation_contract_digest: &str,
    hard_budget_bytes: u64,
) -> Result<SourceSnapshot> {
    validate_tx_id(tx_id)?;
    ensure_inventory_lengths(inventory)?;
    let root = state_dir.join("sync-snapshots");
    std::fs::create_dir_all(&root)?;
    let final_dir = root.join(tx_id);
    if final_dir.exists() {
        let existing = open_snapshot(state_dir, tx_id)?;
        anyhow::ensure!(
            existing.preparation_contract_digest == preparation_contract_digest,
            "existing final snapshot was prepared under a different contract"
        );
        let requested = inventory
            .keys
            .iter()
            .zip(&inventory.digests)
            .zip(&inventory.files)
            .map(|((content_id, digest), file)| (content_id.as_str(), digest.as_str(), file.size))
            .collect::<Vec<_>>();
        let sealed = existing
            .files
            .iter()
            .map(|file| {
                (
                    file.content_id.as_str(),
                    file.content_digest.as_str(),
                    file.content_size,
                )
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            sealed == requested,
            "existing final snapshot inventory differs from this preparation attempt"
        );
        anyhow::ensure!(
            existing.footprint.total_bytes <= hard_budget_bytes,
            "existing final snapshot exceeds the current logical payload limit"
        );
        return Ok(existing);
    }
    let staging = root.join(format!(".{tx_id}.partial"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir(&staging)?;
    let mut cleanup = StagingCleanup {
        path: staging.clone(),
        armed: true,
    };
    let blobs = staging.join("blobs");
    std::fs::create_dir(&blobs)?;
    let prepared_dir = staging.join("prepared");
    if plan.is_some() {
        std::fs::create_dir(&prepared_dir)?;
    }
    let source_bytes = inventory.files.iter().try_fold(0u64, |total, file| {
        total
            .checked_add(file.size)
            .context("snapshot source byte overflow")
    })?;
    if source_bytes > hard_budget_bytes {
        anyhow::bail!(
            "snapshot source footprint {source_bytes} bytes exceeds logical payload limit \
             {hard_budget_bytes} bytes before preparation"
        );
    }
    let mut budget = PayloadBudget {
        used: 0,
        limit: hard_budget_bytes,
    };
    let mut files = Vec::with_capacity(inventory.files.len());
    for (ordinal, ((source, content_id), content_digest)) in inventory
        .files
        .iter()
        .zip(&inventory.keys)
        .zip(&inventory.digests)
        .enumerate()
    {
        crate::content::verify(&source.path, source.size, content_digest)?;
        let relative_blob = format!("blobs/{ordinal:08}");
        let destination = staging.join(&relative_blob);
        copy_synced(&source.path, &destination, &mut budget)?;
        crate::content::verify(&destination, source.size, content_digest)?;
        #[cfg(test)]
        apply_post_seal_source_replacement(&source.path)?;
        // A junk/skipped file has no `plan.files` entry *by construction* — it
        // lives in `plan.junk_files` and is never indexed — so demanding one
        // here made `--no-graph` fail outright on any folder holding a single
        // unreadable, empty or unrecognised file. Its bytes still belong in the
        // sealed snapshot (a resume replays from the snapshot, not from the
        // mutable tree, and the inventory it is verified against lists the
        // file), but there is nothing to prepare for it: `prepared: None`.
        let prepared = match plan {
            Some(plan) if plan.files.contains_key(content_id.as_str()) => Some(prepare_artifact(
                &staging,
                ordinal,
                tx_id,
                source,
                content_id,
                &destination,
                plan,
                &mut budget,
            )?),
            _ => None,
        };
        files.push(SnapshotFile {
            content_id: content_id.clone(),
            content_digest: content_digest.clone(),
            content_size: source.size,
            relative_blob,
            prepared,
        });
    }
    let prepared_bytes = files.iter().try_fold(0u64, |total, file| {
        total
            .checked_add(file.prepared.as_ref().map_or(0, |artifact| artifact.bytes))
            .context("prepared snapshot byte overflow")
    })?;
    let total_bytes = source_bytes
        .checked_add(prepared_bytes)
        .context("total snapshot byte overflow")?;
    anyhow::ensure!(
        total_bytes == budget.used && total_bytes <= hard_budget_bytes,
        "snapshot logical payload accounting mismatch"
    );
    let footprint = SnapshotFootprint {
        source_bytes,
        prepared_bytes,
        total_bytes,
        hard_budget_bytes,
    };
    snapshot_failpoint(1)?;
    let snapshot = SourceSnapshot {
        version: SNAPSHOT_VERSION,
        tx_id: tx_id.to_string(),
        preparation_contract_digest: preparation_contract_digest.to_owned(),
        footprint: footprint.clone(),
        started: chrono::Utc::now().to_rfc3339(),
        snapshot_digest: String::new(),
        files,
    };
    let mut snapshot = snapshot;
    snapshot.snapshot_digest = snapshot_digest(
        tx_id,
        &snapshot.started,
        preparation_contract_digest,
        &footprint,
        &snapshot.files,
    )?;
    write_synced_json(&staging.join("manifest.json"), &snapshot)?;
    sync_dir(&blobs)?;
    if plan.is_some() {
        sync_dir(&prepared_dir)?;
    }
    sync_dir(&staging)?;
    snapshot_failpoint(2)?;
    std::fs::rename(&staging, &final_dir)?;
    cleanup.armed = false;
    sync_dir(&root)?;
    snapshot_failpoint(3)?;
    open_snapshot(state_dir, tx_id)
}

pub fn open_snapshot(state_dir: &Path, tx_id: &str) -> Result<SourceSnapshot> {
    validate_tx_id(tx_id)?;
    let dir = state_dir.join("sync-snapshots").join(tx_id);
    let snapshot: SourceSnapshot =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
    anyhow::ensure!(
        snapshot.version == SNAPSHOT_VERSION && snapshot.tx_id == tx_id,
        "source snapshot identity mismatch"
    );
    let source_bytes = snapshot.files.iter().try_fold(0u64, |sum, file| {
        sum.checked_add(file.content_size)
            .context("snapshot source footprint overflow")
    })?;
    let prepared_bytes = snapshot.files.iter().try_fold(0u64, |sum, file| {
        sum.checked_add(file.prepared.as_ref().map_or(0, |artifact| artifact.bytes))
            .context("snapshot prepared footprint overflow")
    })?;
    let total_bytes = source_bytes
        .checked_add(prepared_bytes)
        .context("snapshot total footprint overflow")?;
    anyhow::ensure!(
        snapshot.footprint.source_bytes == source_bytes
            && snapshot.footprint.prepared_bytes == prepared_bytes
            && snapshot.footprint.total_bytes == total_bytes
            && total_bytes <= snapshot.footprint.hard_budget_bytes,
        "source snapshot footprint does not match its sealed artifacts"
    );
    anyhow::ensure!(
        snapshot.snapshot_digest
            == snapshot_digest(
                tx_id,
                &snapshot.started,
                &snapshot.preparation_contract_digest,
                &snapshot.footprint,
                &snapshot.files,
            )?,
        "source snapshot manifest digest mismatch"
    );
    for file in &snapshot.files {
        anyhow::ensure!(
            file.relative_blob.starts_with("blobs/")
                && !file.relative_blob.contains("..")
                && !Path::new(&file.relative_blob).is_absolute(),
            "invalid source snapshot blob path"
        );
        crate::content::verify(
            &dir.join(&file.relative_blob),
            file.content_size,
            &file.content_digest,
        )?;
        if let Some(prepared) = &file.prepared {
            validate_relative_path(&prepared.relative_ndjson, "prepared/")?;
            let path = dir.join(&prepared.relative_ndjson);
            let metadata = std::fs::metadata(&path)?;
            anyhow::ensure!(
                metadata.len() == prepared.bytes
                    && stream_digest(&path, "axp1")? == prepared.digest,
                "prepared artifact digest or size mismatch"
            );
        }
    }
    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
fn prepare_artifact(
    staging: &Path,
    ordinal: usize,
    generation_identity: &str,
    source: &crate::walk::FileEntry,
    content_id: &str,
    snapshot_blob: &Path,
    plan: &Plan,
    budget: &mut PayloadBudget,
) -> Result<PreparedArtifact> {
    let assignment = plan
        .files
        .get(content_id)
        .with_context(|| format!("plan has no assignment for {}", source.rel))?;
    let datasets: HashMap<&str, &crate::state::PlanDataset> = plan
        .datasets
        .iter()
        .map(|dataset| (dataset.slug.as_str(), dataset))
        .collect();
    let coercions: HashMap<&str, HashMap<String, crate::coerce::Coerce>> = plan
        .datasets
        .iter()
        .map(|dataset| {
            (
                dataset.slug.as_str(),
                crate::coerce::plan_from_specs(&dataset.specs),
            )
        })
        .collect();
    let mut paths = vec![assignment.rel.clone()];
    paths.extend(
        plan.duplicate_files
            .iter()
            .filter(|alias| alias.file_key == content_id)
            .map(|alias| alias.rel.clone()),
    );
    paths.sort();
    paths.dedup();
    let assignments: HashMap<Option<String>, &str> = assignment
        .assignments
        .iter()
        .map(|(group, slug)| (group.clone(), slug.as_str()))
        .collect();
    let sniffed = crate::sniff::sniff_with_name(snapshot_blob, &source.path)
        .with_context(|| format!("sniff {} for durable preparation", source.rel))?;
    let relative_ndjson = format!("prepared/{ordinal:08}.ndjson");
    let path = staging.join(&relative_ndjson);
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    let mut writer = BudgetWriter {
        inner: BufWriter::new(file),
        budget,
    };
    let mut records = 0u64;
    let mut records_by_dataset: BTreeMap<String, u64> = BTreeMap::new();
    let mut passages = 0u64;
    let mut vectors = 0u64;
    let mut sink_error: Option<anyhow::Error> = None;
    let mut sink = |record: crate::extract::RawRecord| -> bool {
        let Some(slug) = assignments
            .get(&record.group)
            .or_else(|| assignments.get(&None))
            .copied()
        else {
            sink_error = Some(anyhow::anyhow!(
                "record {} has no stable dataset assignment",
                record.locator
            ));
            return false;
        };
        let Some(dataset) = datasets.get(slug).copied() else {
            sink_error = Some(anyhow::anyhow!(
                "dataset {slug} is absent from prepared plan"
            ));
            return false;
        };
        let mut fields: Map<String, Value> = record.fields;
        let Some(coercion) = coercions.get(slug) else {
            sink_error = Some(anyhow::anyhow!("dataset {slug} has no coercion plan"));
            return false;
        };
        crate::coerce::coerce_record(&mut fields, coercion);
        fields.insert("ax_path".into(), Value::String(assignment.rel.clone()));
        fields.insert(
            "ax_paths".into(),
            Value::Array(paths.iter().cloned().map(Value::String).collect()),
        );
        fields.insert("ax_file".into(), Value::String(content_id.to_string()));
        fields.insert("ax_locator".into(), Value::String(record.locator.clone()));
        fields.insert("ax_dataset".into(), Value::String(slug.to_string()));
        fields.insert(
            "ax_run".into(),
            Value::String(generation_identity.to_owned()),
        );
        fields.insert(
            "ax_format".into(),
            Value::String(source.path.extension().map_or_else(
                || "unknown".into(),
                |extension| extension.to_string_lossy().to_ascii_lowercase(),
            )),
        );
        let id = crate::ids::doc_id(slug, content_id, &record.locator);
        let action = serde_json::json!({"index": {"_index": dataset.index, "_id": id}});
        if let Err(error) = writeln!(writer, "{action}")
            .and_then(|_| writeln!(writer, "{}", Value::Object(fields.clone())))
        {
            sink_error = Some(anyhow::Error::new(error).context("write prepared NDJSON"));
            return false;
        }
        records += 1;
        // Seal the count under the same dataset identity the record was written
        // under, so a file that fans out over several datasets stays reconcilable
        // against each one separately.
        *records_by_dataset.entry(slug.to_string()).or_default() += 1;
        if dataset
            .semantic_field
            .as_ref()
            .is_some_and(|field| fields.get(field).is_some())
        {
            passages += 1;
            vectors += 1;
        }
        true
    };
    // Junk records are recorded, never fatal — that is the documented contract
    // (`cli.rs` EXIT CODES: "3 completed-with-junk (junk recorded, never
    // fatal)") and what the legacy path has always done. Aborting the whole
    // generation because one line of one log file did not parse would make a
    // realistic corpus unindexable. The count is sealed into the artifact so
    // the generation, and then the catalog, can report it.
    //
    // #722: `assignment.as_document` (a demoted one-off config file, #173)
    // decides the record *shape*, the same precedence the legacy path's own
    // phase-B dispatch gives it (`lib.rs`, `if fa.as_document { … }`) — the
    // frozen dataset's mapping was built by re-sampling through
    // `extract_as_document`, so that is the only extractor whose output may
    // be compared against it. Before this, the durable/generated pipeline
    // never checked the flag at all: a demoted file was correctly ROUTED
    // into the docs dataset (`reconcile_plan`/`dataset::cluster` agree it
    // belongs there) but its published fields still came from its raw
    // family extractor, silently disagreeing with the mapping. `sniffed` is
    // already sniffed from `snapshot_blob`, a real path on the sealed
    // snapshot (not a byte blob), so the branch is exactly the legacy
    // path's — no format change needed.
    let stats = if assignment.as_document {
        // `extract_as_document_with_name`, not `extract_as_document`:
        // `snapshot_blob` is a real path, but to the SEALED SNAPSHOT under
        // its own ordinal name, not the source file's — the title must come
        // from `source.path` (the same split `sniff_with_name` above makes
        // for the same reason).
        crate::extract::extract_as_document_with_name(
            snapshot_blob,
            &source.path,
            sniffed.gzip,
            &mut sink,
        )
    } else {
        crate::extract::extract(snapshot_blob, &sniffed, None, &mut sink)
    }
    .with_context(|| format!("extract {} into durable preparation", source.rel))?;
    if let Some(error) = sink_error {
        return Err(error);
    }
    writer.flush()?;
    writer.inner.get_ref().sync_all()?;
    drop(writer);
    let bytes = std::fs::metadata(&path)?.len();
    Ok(PreparedArtifact {
        relative_ndjson,
        records,
        passages,
        vectors,
        junk: stats.junk,
        records_by_dataset,
        bytes,
        digest: stream_digest(&path, "axp1")?,
    })
}

fn validate_relative_path(path: &str, prefix: &str) -> Result<()> {
    anyhow::ensure!(
        path.starts_with(prefix) && !path.contains("..") && !Path::new(path).is_absolute(),
        "invalid snapshot artifact path"
    );
    Ok(())
}

fn stream_digest(path: &Path, prefix: &str) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hash = xxhash_rust::xxh3::Xxh3::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{prefix}-{:016x}", hash.digest()))
}

fn ensure_inventory_lengths(inventory: &Inventory) -> Result<()> {
    anyhow::ensure!(
        inventory.files.len() == inventory.keys.len()
            && inventory.keys.len() == inventory.digests.len(),
        "inventory vectors have inconsistent lengths"
    );
    Ok(())
}

fn snapshot_digest(
    tx_id: &str,
    started: &str,
    preparation_contract_digest: &str,
    footprint: &SnapshotFootprint,
    files: &[SnapshotFile],
) -> Result<String> {
    let mut files = files.to_vec();
    files.sort_by(|left, right| {
        left.content_id
            .cmp(&right.content_id)
            .then_with(|| left.relative_blob.cmp(&right.relative_blob))
    });
    let encoded = serde_json::to_vec(&(
        SNAPSHOT_VERSION,
        tx_id,
        started,
        preparation_contract_digest,
        footprint,
        files,
    ))?;
    Ok(format!(
        "axs1-{:032x}",
        xxhash_rust::xxh3::xxh3_128(&encoded)
    ))
}

fn validate_tx_id(tx_id: &str) -> Result<()> {
    anyhow::ensure!(
        !tx_id.is_empty()
            && tx_id.len() <= 128
            && tx_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "invalid source snapshot transaction ID"
    );
    Ok(())
}

fn copy_synced(source: &Path, destination: &Path, budget: &mut PayloadBudget) -> Result<()> {
    let mut input = File::open(source)?;
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut output = BudgetWriter {
        inner: output,
        budget,
    };
    std::io::copy(&mut input, &mut output)?;
    output.inner.sync_all()?;
    Ok(())
}

fn write_synced_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
fn arm_source_mutation_after_seal(path: &Path, replacement: &[u8]) {
    *POST_SEAL_SOURCE_REPLACEMENT.lock().unwrap() =
        Some((path.to_path_buf(), replacement.to_vec()));
}

#[cfg(test)]
fn apply_post_seal_source_replacement(path: &Path) -> Result<()> {
    let replacement = {
        let mut armed = POST_SEAL_SOURCE_REPLACEMENT.lock().unwrap();
        if armed.as_ref().is_some_and(|(expected, _)| expected == path) {
            armed.take().map(|(_, bytes)| bytes)
        } else {
            None
        }
    };
    if let Some(replacement) = replacement {
        std::fs::write(path, replacement)
            .with_context(|| format!("inject post-seal source replacement {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
fn fail_next_snapshot(boundary: u8) {
    SNAPSHOT_FAILPOINT.with(|failpoint| failpoint.set(boundary));
}

#[cfg(test)]
fn snapshot_failpoint(boundary: u8) -> Result<()> {
    let armed = SNAPSHOT_FAILPOINT.with(|failpoint| {
        if failpoint.get() == boundary {
            failpoint.set(0);
            true
        } else {
            false
        }
    });
    if armed {
        anyhow::bail!("injected snapshot failure at boundary {boundary}");
    }
    Ok(())
}

#[cfg(not(test))]
fn snapshot_failpoint(_boundary: u8) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn fail_replay_after_next_apply() {
    REPLAY_FAIL_AFTER_APPLY.with(|failpoint| failpoint.set(true));
}

#[cfg(test)]
fn replay_fail_after_apply() -> Result<()> {
    if REPLAY_FAIL_AFTER_APPLY.with(|failpoint| failpoint.replace(false)) {
        anyhow::bail!("injected crash after accepted operation");
    }
    Ok(())
}

#[cfg(not(test))]
fn replay_fail_after_apply() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FileAssignment, PlanDataset};
    use crate::sync::{
        ExecutionIdentity, GenerationManifest, SourceExecutionPolicy, SyncOperationKind,
        EXECUTION_IDENTITY_VERSION,
    };
    use crate::walk;
    use std::collections::BTreeMap;

    fn inventory(root: &Path) -> Inventory {
        crate::content::resolve_reporting(walk::walk(root, false).unwrap(), &|_| {}).unwrap()
    }

    fn plan_for(inventory: &Inventory) -> Plan {
        let key = inventory.keys[0].clone();
        let file = &inventory.files[0];
        Plan {
            datasets: vec![PlanDataset {
                slug: "docs".into(),
                index: "ax-docs".into(),
                family: "text".into(),
                group: None,
                specs: vec![],
                time_field: None,
                semantic_field: None,
                sampled_records: 1,
                file_count: 1,
            }],
            files: HashMap::from([(
                key,
                FileAssignment {
                    rel: file.rel.clone(),
                    path_id: file.rel_id.clone(),
                    is_symlink: Some(file.is_symlink),
                    family: "text".into(),
                    gzip: false,
                    content_digest: Some(inventory.digests[0].clone()),
                    assignments: vec![(None, "docs".into())],
                    as_document: false,
                },
            )]),
            alias_paths_indexed: false,
            ..Plan::default()
        }
    }

    /// One file, two tables, two datasets — the ordinary shape of a SQL dump.
    fn sql_plan_for(inventory: &Inventory) -> Plan {
        let key = inventory.keys[0].clone();
        let file = &inventory.files[0];
        let dataset = |slug: &str| PlanDataset {
            slug: slug.into(),
            index: format!("ax-{slug}"),
            family: "sqldump".into(),
            group: Some(slug.into()),
            specs: vec![],
            time_field: None,
            semantic_field: None,
            sampled_records: 2,
            file_count: 1,
        };
        Plan {
            datasets: vec![dataset("orders"), dataset("users")],
            files: HashMap::from([(
                key,
                FileAssignment {
                    rel: file.rel.clone(),
                    path_id: file.rel_id.clone(),
                    is_symlink: Some(file.is_symlink),
                    family: "sqldump".into(),
                    gzip: false,
                    content_digest: Some(inventory.digests[0].clone()),
                    assignments: vec![
                        (Some("orders".into()), "orders".into()),
                        (Some("users".into()), "users".into()),
                    ],
                    as_document: false,
                },
            )]),
            alias_paths_indexed: false,
            ..Plan::default()
        }
    }

    fn two_table_dump() -> &'static str {
        "CREATE TABLE `users` (`id` int, `name` varchar(64));\n\
         INSERT INTO `users` VALUES (1,'ann'),(2,'bob');\n\
         CREATE TABLE `orders` (`id` int, `total` int);\n\
         INSERT INTO `orders` VALUES (10,100),(11,200);\n"
    }

    /// #360: the seal has to say which dataset each record went to. A flat
    /// per-file total is comparable to no single dataset's read-back once the
    /// file fans out, and the executor reconciled it against every one of them.
    #[test]
    fn preparation_seals_records_under_each_dataset_a_file_feeds() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("dump.sql"), two_table_dump()).unwrap();
        let inventory = inventory(corpus.path());
        let plan = sql_plan_for(&inventory);
        let snapshot = create_prepared_snapshot(
            state.path(),
            "tx-fan-out",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
        )
        .unwrap();

        let prepared = snapshot.files[0].prepared.as_ref().unwrap();
        assert_eq!(prepared.records, 4);
        assert_eq!(
            prepared.records_by_dataset,
            BTreeMap::from([("orders".to_string(), 2), ("users".to_string(), 2)])
        );

        let mut groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        bind_prepared_counts(&mut groups, &snapshot, &inventory.keys).unwrap();
        assert_eq!(groups[0].expected_records, 4);
        assert_eq!(groups[0].expected_records_for("users"), Some(2));
        assert_eq!(groups[0].expected_records_for("orders"), Some(2));
        assert_eq!(groups[0].expected_records_for("absent"), Some(0));
    }

    /// #722: `assignment.as_document` was never read here at all — a demoted
    /// one-off config file (#173) was correctly ROUTED into the docs dataset
    /// (`reconcile_plan`/`dataset::cluster` agree) but published through its
    /// raw family extractor, not `extract_as_document`, silently disagreeing
    /// with the docs mapping the run itself built. A first, file-path-only
    /// fix used `extract_as_document(snapshot_blob, …)` directly and derived
    /// the title from `snapshot_blob`'s own name — the SEALED SNAPSHOT's
    /// ordinal filename (`prepared/00000000`), not the source file's, so
    /// every demoted document titled itself `"00000000"`. Pins both halves:
    /// the record is document-shaped (`title`/`body`, not the source's raw
    /// JSON keys) AND the title is the real source name.
    #[test]
    fn preparation_of_a_demoted_file_publishes_a_real_document_with_the_source_title() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            corpus.path().join("config.json"),
            br#"{"host": "example.com", "port": 8080}"#,
        )
        .unwrap();
        let inventory = inventory(corpus.path());
        let mut plan = plan_for(&inventory);
        let key = inventory.keys[0].clone();
        plan.files.get_mut(&key).unwrap().as_document = true;

        let snapshot = create_prepared_snapshot(
            state.path(),
            "tx-demoted-config",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
        )
        .unwrap();

        let prepared = snapshot.files[0].prepared.as_ref().unwrap();
        assert_eq!(prepared.records, 1);
        let root = state.path().join("sync-snapshots/tx-demoted-config");
        let ndjson = std::fs::read_to_string(root.join(&prepared.relative_ndjson)).unwrap();
        let document: Value = serde_json::from_str(ndjson.lines().nth(1).unwrap()).unwrap();
        assert_eq!(
            document["title"], "config",
            "title must be the source file's own stem, not the sealed snapshot's ordinal \
             filename: {document}"
        );
        assert!(
            document["body"]
                .as_str()
                .unwrap()
                .contains(r#""host": "example.com""#),
            "body must be the decoded document text, not raw extracted JSON fields: {document}"
        );
        assert!(
            document.get("host").is_none() && document.get("port").is_none(),
            "a demoted file must not publish its raw family-extractor fields: {document}"
        );
    }

    /// `snapshot_digest` covers the serialized `PreparedArtifact` and
    /// `open_snapshot` re-verifies it on every resume, so a snapshot sealed
    /// before the per-dataset ledger existed has to keep hashing to the value
    /// it was sealed with. Otherwise this fix turns a bug affecting SQL corpora
    /// into "protected snapshot digest disagrees" for everyone mid-generation.
    #[test]
    fn a_snapshot_sealed_without_the_per_dataset_ledger_still_resumes() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("dump.sql"), two_table_dump()).unwrap();
        let inventory = inventory(corpus.path());
        let plan = sql_plan_for(&inventory);
        let snapshot = create_prepared_snapshot(
            state.path(),
            "tx-legacy-ledger",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
        )
        .unwrap();
        assert!(!snapshot.files[0]
            .prepared
            .as_ref()
            .unwrap()
            .records_by_dataset
            .is_empty());

        // Rewrite the manifest exactly as a pre-upgrade binary wrote it: no
        // `records_by_dataset` key anywhere, and a digest computed over that
        // form. `skip_serializing_if` is what makes the two byte-identical.
        let manifest = state
            .path()
            .join("sync-snapshots/tx-legacy-ledger/manifest.json");
        let raw = std::fs::read_to_string(&manifest).unwrap();
        assert!(raw.contains("records_by_dataset"));
        let mut legacy: SourceSnapshot =
            serde_json::from_str(&raw.replace("\"records_by_dataset\"", "\"ignored_by_serde\""))
                .unwrap();
        assert!(legacy.files[0]
            .prepared
            .as_ref()
            .unwrap()
            .records_by_dataset
            .is_empty());
        assert!(!serde_json::to_string(&legacy)
            .unwrap()
            .contains("records_by_dataset"));
        legacy.snapshot_digest = snapshot_digest(
            &legacy.tx_id,
            &legacy.started,
            &legacy.preparation_contract_digest,
            &legacy.footprint,
            &legacy.files,
        )
        .unwrap();
        std::fs::write(&manifest, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let resumed = open_snapshot(state.path(), "tx-legacy-ledger").unwrap();
        assert_eq!(resumed, legacy);
        assert_eq!(resumed.files[0].prepared.as_ref().unwrap().records, 4);
    }

    fn execution(tx_id: &str, digest: &str) -> ExecutionIdentity {
        ExecutionIdentity {
            version: EXECUTION_IDENTITY_VERSION,
            root_identity: "root".into(),
            url: "http://engine".into(),
            prefix: "ax".into(),
            follow_symlinks: false,
            chunker_identity: "chunker-v1".into(),
            embedding_identity_sha256: "a".repeat(64),
            embedding_backend: "lexical".into(),
            embedding_dimension: Some(384),
            embedding_semantic_contract: "semantic_text-derived-vector.v1".into(),
            embedding_resumable: true,
            graph_enabled: false,
            brain: "disabled".into(),
            detector_identity: "disabled".into(),
            schema_identity: "schema-v1".into(),
            index_identity: "index-v1".into(),
            source_policy: SourceExecutionPolicy::DurableSnapshot {
                reference: format!("sync-snapshots/{tx_id}"),
                snapshot_digest: digest.into(),
            },
        }
    }

    fn desired(
        generation: u64,
        tx_id: &str,
        snapshot: &SourceSnapshot,
        plan: Plan,
        groups: Vec<ManifestGroup>,
    ) -> GenerationManifest {
        GenerationManifest {
            generation,
            execution: Some(execution(tx_id, &snapshot.snapshot_digest)),
            plan,
            groups,
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct LiveGroup {
        content_id: String,
        canonical: String,
        aliases: Vec<String>,
    }

    #[derive(Default)]
    struct FakeBackend {
        live: BTreeMap<String, LiveGroup>,
        applications: Vec<String>,
        provisions: usize,
        validations: usize,
    }

    impl SyncOperationBackend for FakeBackend {
        fn provision_generation(&mut self, _desired: &GenerationManifest) -> Result<()> {
            self.provisions += 1;
            Ok(())
        }

        fn apply(
            &mut self,
            operation: &SyncOperation,
            _base: &CommittedManifest,
            desired: &GenerationManifest,
            _snapshot: &SourceSnapshot,
        ) -> Result<()> {
            self.applications.push(operation.operation_id.clone());
            match operation.kind {
                SyncOperationKind::Delete => {
                    self.live.remove(&operation.group_id);
                }
                SyncOperationKind::Upsert | SyncOperationKind::Metadata => {
                    let group = desired
                        .groups
                        .iter()
                        .find(|group| group.group_id == operation.group_id)
                        .context("desired operation group is absent")?;
                    self.live.insert(
                        group.group_id.clone(),
                        LiveGroup {
                            content_id: group.content_id.clone(),
                            canonical: group.canonical.path_id.clone(),
                            aliases: group
                                .aliases
                                .iter()
                                .map(|alias| alias.path_id.clone())
                                .collect(),
                        },
                    );
                }
            }
            Ok(())
        }

        fn validate(
            &mut self,
            _base: &CommittedManifest,
            desired: &GenerationManifest,
            _snapshot: &SourceSnapshot,
        ) -> Result<()> {
            self.validations += 1;
            let expected: BTreeMap<String, LiveGroup> = desired
                .groups
                .iter()
                .map(|group| {
                    (
                        group.group_id.clone(),
                        LiveGroup {
                            content_id: group.content_id.clone(),
                            canonical: group.canonical.path_id.clone(),
                            aliases: group
                                .aliases
                                .iter()
                                .map(|alias| alias.path_id.clone())
                                .collect(),
                        },
                    )
                })
                .collect();
            anyhow::ensure!(self.live == expected, "fake live generation mismatch");
            Ok(())
        }
    }

    fn begin(
        journal: &mut Journal,
        tx_id: &str,
        snapshot: &SourceSnapshot,
        plan: Plan,
        groups: Vec<ManifestGroup>,
    ) {
        let base = journal.committed_manifest.as_ref().unwrap();
        let pending = PendingSync::new(
            tx_id.into(),
            base,
            desired(base.generation + 1, tx_id, snapshot, plan, groups),
        )
        .unwrap();
        journal.sync_begin(&pending).unwrap();
    }

    #[test]
    fn inventory_bridge_retains_counts_only_for_identical_content() {
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for(&inventory);
        let first = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        assert_eq!(first[0].expected_records, 0);
        let mut committed = first;
        committed[0].expected_records = 7;
        committed[0].expected_passages = 3;
        committed[0].expected_vectors = 3;
        let resumed = groups_from_inventory(&inventory, &plan, &committed).unwrap();
        assert_eq!(
            (
                resumed[0].expected_records,
                resumed[0].expected_passages,
                resumed[0].expected_vectors
            ),
            (7, 3, 3)
        );
    }

    #[test]
    fn snapshot_is_immutable_and_detects_blob_corruption() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
        let inventory = inventory(corpus.path());
        let snapshot = create_snapshot(state.path(), "tx-1", &inventory).unwrap();
        std::fs::write(corpus.path().join("a.txt"), "changed source").unwrap();
        assert_eq!(open_snapshot(state.path(), "tx-1").unwrap(), snapshot);
        let blob = state
            .path()
            .join("sync-snapshots/tx-1")
            .join(&snapshot.files[0].relative_blob);
        std::fs::write(blob, "corrupt").unwrap();
        assert!(open_snapshot(state.path(), "tx-1").is_err());
    }

    #[test]
    fn prepared_snapshot_streams_exact_actions_counts_and_digest() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            corpus.path().join("a.jsonl"),
            "{\"message\":\"alpha\"}\n{\"message\":\"beta\"}\n",
        )
        .unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for(&inventory);
        let snapshot = create_prepared_snapshot(
            state.path(),
            "tx-prepared",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
        )
        .unwrap();
        let prepared = snapshot.files[0].prepared.as_ref().unwrap();
        assert_eq!(prepared.records, 2);
        assert!(prepared.bytes > 0);
        let ndjson = std::fs::read_to_string(
            state
                .path()
                .join("sync-snapshots/tx-prepared")
                .join(&prepared.relative_ndjson),
        )
        .unwrap();
        assert_eq!(ndjson.lines().count(), 4);
        assert!(ndjson.contains("\"ax_file\""));
        assert!(ndjson.contains("\"_index\":\"ax-docs\""));

        let mut groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        bind_prepared_counts(&mut groups, &snapshot, &inventory.keys).unwrap();
        assert_eq!(groups[0].expected_records, 2);
        let artifact_path = state
            .path()
            .join("sync-snapshots/tx-prepared")
            .join(&prepared.relative_ndjson);
        std::fs::write(artifact_path, "corrupt").unwrap();
        assert!(open_snapshot(state.path(), "tx-prepared").is_err());
    }

    #[test]
    fn preparation_sniffs_and_extracts_only_the_sealed_blob_after_live_mutation() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let source = corpus.path().join("records.jsonl");
        let sealed = b"{\"message\":\"sealed alpha\"}\n{\"message\":\"sealed beta\"}\n";
        let replacement = b"id,value\n9,live mutation\n";
        std::fs::write(&source, sealed).unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for(&inventory);
        arm_source_mutation_after_seal(&source, replacement);

        let snapshot = create_prepared_snapshot(
            state.path(),
            "tx-post-seal-mutation",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
        )
        .unwrap();

        assert_eq!(std::fs::read(&source).unwrap(), replacement);
        let prepared = snapshot.files[0].prepared.as_ref().unwrap();
        assert_eq!(prepared.records, 2);
        let root = state.path().join("sync-snapshots/tx-post-seal-mutation");
        let ndjson = std::fs::read_to_string(root.join(&prepared.relative_ndjson)).unwrap();
        assert!(ndjson.contains("sealed alpha"), "{ndjson}");
        assert!(ndjson.contains("sealed beta"), "{ndjson}");
        assert!(!ndjson.contains("live mutation"), "{ndjson}");
        for document in ndjson
            .lines()
            .skip(1)
            .step_by(2)
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
        {
            assert_eq!(document["ax_format"], "jsonl");
            assert!(document.get("message").is_some(), "{document}");
            assert!(document.get("id").is_none(), "{document}");
            assert!(document.get("value").is_none(), "{document}");
        }
        assert_eq!(
            std::fs::read(root.join(&snapshot.files[0].relative_blob)).unwrap(),
            sealed
        );
    }

    /// #294: snapshot blobs are content-addressed and extensionless
    /// (`blobs/00000000`), and the code extractor keyed its grammar lookup on
    /// the CONTENT path instead of the logical one carried by `Sniffed`. Every
    /// source file on the durable path therefore prepared as junk — zero
    /// documents — while the generation still committed and reported success.
    #[test]
    fn preparation_extracts_code_from_extensionless_snapshot_blobs() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            corpus.path().join("app.py"),
            "def alpha_helper():\n    return 1\n",
        )
        .unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for(&inventory);
        let snapshot = create_prepared_snapshot(
            state.path(),
            "tx-code",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
        )
        .unwrap();
        let prepared = snapshot.files[0].prepared.as_ref().unwrap();
        // #500: a code file prepares the file-level document PLUS one document
        // per declaration — so records is now ≥1 (was exactly 1). The invariant
        // the test guards is "documents, not silent junk": junk stays 0.
        assert!(
            prepared.records >= 1 && prepared.junk == 0,
            "a code file must prepare documents, not silent junk: records={} junk={}",
            prepared.records,
            prepared.junk
        );
        let ndjson = std::fs::read_to_string(
            state
                .path()
                .join("sync-snapshots/tx-code")
                .join(&prepared.relative_ndjson),
        )
        .unwrap();
        let document: Value = serde_json::from_str(ndjson.lines().nth(1).unwrap()).unwrap();
        assert_eq!(document["language"], "python", "{document}");
        // The title must be the logical file name, not the blob ordinal.
        assert_eq!(document["title"], "app.py", "{document}");
        assert!(
            document["defs"]
                .as_str()
                .unwrap_or("")
                .contains("function alpha_helper"),
            "{document}"
        );
    }

    #[test]
    fn prepared_snapshot_budget_aborts_and_removes_staging() {
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "budgeted source bytes").unwrap();
        let inventory = crate::content::resolve_reporting(
            crate::walk::walk(corpus.path(), false).unwrap(),
            &|_| {},
        )
        .unwrap();
        let plan = plan_for(&inventory);
        let state = tempfile::tempdir().unwrap();
        let error = create_prepared_snapshot(
            state.path(),
            "tx-budget",
            &inventory,
            &plan,
            "test-preparation-v1",
            inventory.files[0].size + 1,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("logical payload"));
        assert!(!state
            .path()
            .join("sync-snapshots/.tx-budget.partial")
            .exists());
        assert!(!state.path().join("sync-snapshots/tx-budget").exists());
    }

    #[test]
    fn payload_writer_refuses_bytes_before_they_reach_disk() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("payload");
        let file = File::create(&path).unwrap();
        let mut budget = PayloadBudget { used: 0, limit: 3 };
        let mut writer = BudgetWriter {
            inner: file,
            budget: &mut budget,
        };
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        writer.flush().unwrap();
        drop(writer);
        assert_eq!(std::fs::metadata(path).unwrap().len(), 3);
        assert_eq!(budget.used, 3);
    }

    #[test]
    fn gc_tombstone_crash_is_retryable_and_symlinks_are_refused() {
        let state = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(state.path(), "root", "url", "prefix", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        let snapshots = state.path().join("sync-snapshots");
        std::fs::create_dir_all(snapshots.join("orphan")).unwrap();
        std::fs::write(snapshots.join("orphan/blob"), b"x").unwrap();
        GC_FAIL_AFTER_RENAME.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(gc_snapshots(state.path(), &journal).is_err());
        assert!(!snapshots.join("orphan").exists());
        assert!(snapshots.join(".orphan.gc").exists());
        gc_snapshots(state.path(), &journal).unwrap();
        assert!(!snapshots.join(".orphan.gc").exists());

        #[cfg(unix)]
        {
            std::fs::create_dir(snapshots.join("a-orphan")).unwrap();
            std::os::unix::fs::symlink(state.path(), snapshots.join("hostile")).unwrap();
            assert!(gc_snapshots(state.path(), &journal).is_err());
            assert!(snapshots.join("a-orphan").exists());
            assert!(snapshots.join("hostile").exists());
        }
    }

    #[test]
    fn gc_validates_every_protected_snapshot_before_deleting_orphans() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "protected bytes").unwrap();
        let inventory = crate::content::resolve_reporting(
            crate::walk::walk(corpus.path(), false).unwrap(),
            &|_| {},
        )
        .unwrap();
        let plan = plan_for(&inventory);
        let state = tempfile::tempdir().unwrap();
        let snapshot = create_snapshot(state.path(), "tx-protected", &inventory).unwrap();
        let mut journal = Journal::open(state.path(), "root", "url", "prefix", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        let groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        begin(&mut journal, "tx-protected", &snapshot, plan, groups);
        let orphan = state.path().join("sync-snapshots/orphan");
        std::fs::create_dir(&orphan).unwrap();
        std::fs::write(orphan.join("blob"), b"orphan").unwrap();
        let protected_blob = state
            .path()
            .join("sync-snapshots/tx-protected")
            .join(&snapshot.files[0].relative_blob);
        std::fs::write(protected_blob, b"corrupt").unwrap();
        assert!(gc_snapshots(state.path(), &journal).is_err());
        assert!(orphan.exists(), "validation must precede every deletion");
        assert!(state.path().join("sync-snapshots/tx-protected").exists());
    }

    #[test]
    fn prepared_and_metadata_streams_preserve_pairs_under_tiny_budgets() {
        let input = concat!(
            "{\"index\":{\"_index\":\"docs\",\"_id\":\"a\"}}\n",
            "{\"body\":\"alpha\"}\n",
            "{\"index\":{\"_index\":\"docs\",\"_id\":\"b\"}}\n",
            "{\"body\":\"beta\"}\n"
        );
        let mut index_chunks = Vec::new();
        stream_ndjson_pairs(input.as_bytes(), 1, |body| {
            index_chunks.push(String::from_utf8(body).unwrap());
            Ok(())
        })
        .unwrap();
        assert_eq!(index_chunks.len(), 2);
        assert!(index_chunks.iter().all(|chunk| chunk.lines().count() == 2));

        let mut update_chunks = Vec::new();
        stream_metadata_updates(
            input.as_bytes(),
            1,
            "renamed.txt",
            &[Value::String("renamed.txt".into())],
            |body| {
                update_chunks.push(String::from_utf8(body).unwrap());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(update_chunks.len(), 2);
        assert!(update_chunks.iter().all(|chunk| {
            chunk.lines().count() == 2
                && chunk.contains("\"update\"")
                && chunk.contains("\"ax_path\":\"renamed.txt\"")
                && !chunk.contains("\"body\"")
        }));

        let es = crate::esclient::Es::new("http://127.0.0.1:1", None).unwrap();
        let state = tempfile::tempdir().unwrap();
        let _backend = EsSyncBackend::new(&es, state.path(), 1);
    }

    #[test]
    fn every_snapshot_crash_boundary_restarts_deterministically() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for boundary in 1..=3 {
            let corpus = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
            let inventory = inventory(corpus.path());
            fail_next_snapshot(boundary);
            assert!(create_snapshot(state.path(), "tx-restart", &inventory).is_err());
            let recovered = create_snapshot(state.path(), "tx-restart", &inventory).unwrap();
            assert_eq!(
                open_snapshot(state.path(), "tx-restart").unwrap(),
                recovered
            );
        }
    }

    #[test]
    fn snapshot_failpoint_cannot_be_stolen_by_an_unrelated_thread() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (armed_tx, armed_rx) = std::sync::mpsc::channel();
        let (thief_done_tx, thief_done_rx) = std::sync::mpsc::channel();

        let owner = std::thread::spawn(move || {
            fail_next_snapshot(2);
            armed_tx.send(()).unwrap();
            thief_done_rx.recv().unwrap();
            (
                snapshot_failpoint(2).is_err(),
                snapshot_failpoint(2).is_ok(),
            )
        });

        armed_rx.recv().unwrap();
        let thief_result = snapshot_failpoint(2);
        thief_done_tx.send(()).unwrap();
        let (owner_consumed_once, owner_second_probe_succeeded) = owner.join().unwrap();

        assert!(
            thief_result.is_ok(),
            "a thread that did not arm the snapshot failpoint consumed it"
        );
        assert!(
            owner_consumed_once,
            "the thread that armed the snapshot failpoint did not consume it"
        );
        assert!(
            owner_second_probe_succeeded,
            "the snapshot failpoint must retain one-shot semantics"
        );
    }

    #[test]
    fn replay_failpoint_cannot_be_stolen_by_an_unrelated_thread() {
        let _guard = REPLAY_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (armed_tx, armed_rx) = std::sync::mpsc::channel();
        let (thief_done_tx, thief_done_rx) = std::sync::mpsc::channel();

        let owner = std::thread::spawn(move || {
            fail_replay_after_next_apply();
            armed_tx.send(()).unwrap();
            thief_done_rx.recv().unwrap();
            (
                replay_fail_after_apply().is_err(),
                replay_fail_after_apply().is_ok(),
            )
        });

        armed_rx.recv().unwrap();
        let thief_result = replay_fail_after_apply();
        thief_done_tx.send(()).unwrap();
        let (owner_consumed_once, owner_second_probe_succeeded) = owner.join().unwrap();

        assert!(
            thief_result.is_ok(),
            "a thread that did not arm the replay failpoint consumed it"
        );
        assert!(
            owner_consumed_once,
            "the thread that armed the replay failpoint did not consume it"
        );
        assert!(
            owner_second_probe_succeeded,
            "the replay failpoint must retain one-shot semantics"
        );
    }

    #[test]
    fn pending_generation_is_bound_before_mutable_source_replanning() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = tempfile::tempdir().unwrap();
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
        let inventory = inventory(corpus.path());
        let snapshot = create_snapshot(state.path(), "tx-pending", &inventory).unwrap();
        let pending: PendingSync = serde_json::from_value(serde_json::json!({
            "tx_id": "tx-pending",
            "base_generation": 0,
            "base_manifest_digest": "base",
            "desired_manifest_digest": "desired",
            "operation_hash": "operations",
            "desired": {
                "generation": 1,
                "execution": {
                    "version": 1,
                    "root_identity": "root",
                    "url": "http://engine",
                    "prefix": "ax",
                    "follow_symlinks": false,
                    "chunker_identity": "chunker",
                    "embedding_identity_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "embedding_backend": "lexical",
                    "embedding_dimension": 384,
                    "embedding_semantic_contract": "semantic_text-derived-vector.v1",
                    "embedding_resumable": true,
                    "graph_enabled": false,
                    "brain": "disabled",
                    "detector_identity": "disabled",
                    "schema_identity": "schema",
                    "index_identity": "index",
                    "source_policy": {
                        "policy": "durable_snapshot",
                        "reference": "sync-snapshots/tx-pending",
                        "snapshot_digest": snapshot.snapshot_digest
                    }
                },
                "plan": {
                    "datasets": [],
                    "files": {},
                    "junk_files": [],
                    "duplicate_files": [],
                    "alias_paths_indexed": false
                },
                "groups": []
            },
            "operations": []
        }))
        .unwrap();
        let error = require_resumable_pending_source(state.path(), Some(&pending))
            .unwrap_err()
            .to_string();
        std::fs::write(corpus.path().join("a.txt"), "mutated after sync_begin").unwrap();
        let repeated = require_resumable_pending_source(state.path(), Some(&pending))
            .unwrap_err()
            .to_string();
        assert_eq!(error, repeated);
        assert!(error.contains("verified durable source snapshot"));
    }

    #[test]
    fn accepted_upsert_replays_without_duplicate_live_state() {
        let _journal_guard = crate::state::SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for(&inventory);
        let groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        let snapshot = create_snapshot(state.path(), "tx-add", &inventory).unwrap();
        let mut journal =
            Journal::open(state.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        begin(&mut journal, "tx-add", &snapshot, plan, groups);
        let mut backend = FakeBackend::default();

        fail_replay_after_next_apply();
        assert!(replay_pending_operations(state.path(), &mut journal, &mut backend).is_err());
        assert_eq!(backend.live.len(), 1);
        drop(journal);

        let mut journal =
            Journal::open(state.path(), "root", "http://engine", "ax", 300, false).unwrap();
        replay_pending_operations(state.path(), &mut journal, &mut backend).unwrap();
        assert_eq!(backend.live.len(), 1);
        assert_eq!(backend.applications.len(), 2);
        assert!(journal.pending_sync.is_none());
        assert_eq!(journal.committed_manifest.as_ref().unwrap().generation, 1);
    }

    #[test]
    fn replay_converges_change_metadata_and_delete_without_resurrection() {
        let _journal_guard = crate::state::SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = tempfile::tempdir().unwrap();
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
        let first_inventory = inventory(corpus.path());
        let first_plan = plan_for(&first_inventory);
        let first_groups = groups_from_inventory(&first_inventory, &first_plan, &[]).unwrap();
        let first_snapshot = create_snapshot(state.path(), "tx-first", &first_inventory).unwrap();
        let mut journal =
            Journal::open(state.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        begin(
            &mut journal,
            "tx-first",
            &first_snapshot,
            first_plan,
            first_groups,
        );
        let mut backend = FakeBackend::default();
        replay_pending_operations(state.path(), &mut journal, &mut backend).unwrap();
        let stable_group_id = journal.committed_manifest.as_ref().unwrap().groups[0]
            .group_id
            .clone();

        std::fs::write(corpus.path().join("a.txt"), "beta").unwrap();
        let changed_inventory = inventory(corpus.path());
        let changed_plan = plan_for(&changed_inventory);
        let changed_groups = groups_from_inventory(
            &changed_inventory,
            &changed_plan,
            &journal.committed_manifest.as_ref().unwrap().groups,
        )
        .unwrap();
        assert_eq!(changed_groups[0].group_id, stable_group_id);
        let changed_snapshot =
            create_snapshot(state.path(), "tx-change", &changed_inventory).unwrap();
        begin(
            &mut journal,
            "tx-change",
            &changed_snapshot,
            changed_plan,
            changed_groups,
        );
        fail_replay_after_next_apply();
        assert!(replay_pending_operations(state.path(), &mut journal, &mut backend).is_err());
        replay_pending_operations(state.path(), &mut journal, &mut backend).unwrap();
        let changed_content = backend.live[&stable_group_id].content_id.clone();

        let mut renamed_plan = journal.committed_manifest.as_ref().unwrap().plan.clone();
        let content_id = renamed_plan.files.keys().next().unwrap().clone();
        let assignment = renamed_plan.files.get_mut(&content_id).unwrap();
        assignment.rel = "renamed.txt".into();
        assignment.path_id = "unix:72656e616d65642e747874".into();
        let mut renamed_groups = journal.committed_manifest.as_ref().unwrap().groups.clone();
        renamed_groups[0].canonical.rel = "renamed.txt".into();
        renamed_groups[0].canonical.path_id = "unix:72656e616d65642e747874".into();
        let renamed_snapshot =
            create_snapshot(state.path(), "tx-metadata", &changed_inventory).unwrap();
        begin(
            &mut journal,
            "tx-metadata",
            &renamed_snapshot,
            renamed_plan,
            renamed_groups,
        );
        replay_pending_operations(state.path(), &mut journal, &mut backend).unwrap();
        assert_eq!(backend.live[&stable_group_id].content_id, changed_content);
        assert_eq!(
            backend.live[&stable_group_id].canonical,
            "unix:72656e616d65642e747874"
        );

        let empty = Inventory {
            files: vec![],
            keys: vec![],
            digests: vec![],
            duplicates: vec![],
        };
        let empty_snapshot = create_snapshot(state.path(), "tx-delete", &empty).unwrap();
        let mut empty_plan = journal.committed_manifest.as_ref().unwrap().plan.clone();
        empty_plan.files.clear();
        empty_plan.datasets[0].file_count = 0;
        begin(
            &mut journal,
            "tx-delete",
            &empty_snapshot,
            empty_plan,
            vec![],
        );
        fail_replay_after_next_apply();
        assert!(replay_pending_operations(state.path(), &mut journal, &mut backend).is_err());
        assert!(backend.live.is_empty());
        replay_pending_operations(state.path(), &mut journal, &mut backend).unwrap();
        assert!(backend.live.is_empty());
        assert!(journal
            .committed_manifest
            .as_ref()
            .unwrap()
            .groups
            .is_empty());
    }

    #[test]
    fn serialized_graph_identity_with_zero_operations_fails_before_backend_or_commit() {
        let state = tempfile::tempdir().unwrap();
        let inventory = Inventory {
            files: vec![],
            keys: vec![],
            digests: vec![],
            duplicates: vec![],
        };
        let snapshot = create_snapshot(state.path(), "tx-hostile", &inventory).unwrap();
        let mut journal =
            Journal::open(state.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        let base = journal.committed_manifest.as_ref().unwrap().clone();
        let desired = desired(1, "tx-hostile", &snapshot, Plan::default(), vec![]);
        let valid = PendingSync::new("tx-hostile".into(), &base, desired).unwrap();
        assert!(valid.operations.is_empty());
        journal.sync_begin(&valid).unwrap();

        let mut encoded = serde_json::to_value(journal.pending_sync.as_ref().unwrap()).unwrap();
        encoded["desired"]["execution"]["graph_enabled"] = Value::Bool(true);
        encoded["desired"]["execution"]["brain"] = Value::String("hostile".into());
        encoded["desired"]["execution"]["detector_identity"] =
            Value::String("hostile-detectors".into());
        journal.pending_sync = Some(serde_json::from_value(encoded).unwrap());

        let mut backend = FakeBackend::default();
        assert!(replay_pending_operations(state.path(), &mut journal, &mut backend).is_err());
        assert_eq!(backend.provisions, 0);
        assert!(backend.applications.is_empty());
        assert_eq!(backend.validations, 0);
        assert_eq!(journal.committed_manifest.as_ref().unwrap().generation, 0);
        assert!(journal.pending_sync.is_some());
    }
}
