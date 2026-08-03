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
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const SNAPSHOT_VERSION: u32 = 1;

#[cfg(test)]
static SNAPSHOT_FAILPOINT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
static SNAPSHOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
static REPLAY_FAIL_AFTER_APPLY: std::sync::atomic::AtomicBool =
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
    pub bytes: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub version: u32,
    pub tx_id: String,
    pub files: Vec<SnapshotFile>,
    pub snapshot_digest: String,
}

/// Remote mutations must be convergent: calling `apply` twice for one
/// operation after an accepted-but-unrecorded response must produce exactly
/// the same live state. `validate` is the final generation-wide barrier.
#[cfg_attr(not(test), allow(dead_code))]
pub trait SyncOperationBackend {
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
}

impl SyncOperationBackend for EsSyncBackend<'_> {
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
            ),
            crate::sync::SyncOperationKind::Upsert => {
                if let Some(old) = old {
                    self.delete_group(old, &base.plan)?;
                }
                let new = new.context("upsert operation has no desired group")?;
                // Remove a partial prior retry of the desired identity too.
                self.delete_group(new, &desired.plan)?;
                self.replay_prepared(snapshot, &new.content_id)
            }
            crate::sync::SyncOperationKind::Metadata => self.replay_metadata(
                base,
                new.context("metadata operation has no desired group")?,
            ),
        }
    }

    fn validate(
        &mut self,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        _snapshot: &SourceSnapshot,
    ) -> Result<()> {
        for dataset in &desired.plan.datasets {
            self.es.refresh(&dataset.index)?;
        }
        for group in &desired.groups {
            anyhow::ensure!(
                self.exact_group_count(group, &desired.plan)? == group.expected_records,
                "live record count disagrees with desired group {}",
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
    pending.validate_against(&base)?;
    let snapshot = open_snapshot(state_dir, &pending.tx_id)?;
    verify_snapshot_binding(&pending, &snapshot)?;

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
    backend.validate(&base, &pending.desired, &snapshot)?;
    journal.sync_validated()?;
    journal.sync_commit()
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
    create_snapshot_inner(state_dir, tx_id, inventory, None)
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
) -> Result<SourceSnapshot> {
    create_snapshot_inner(state_dir, tx_id, inventory, Some(plan))
}

fn create_snapshot_inner(
    state_dir: &Path,
    tx_id: &str,
    inventory: &Inventory,
    plan: Option<&Plan>,
) -> Result<SourceSnapshot> {
    validate_tx_id(tx_id)?;
    ensure_inventory_lengths(inventory)?;
    let root = state_dir.join("sync-snapshots");
    std::fs::create_dir_all(&root)?;
    let final_dir = root.join(tx_id);
    if final_dir.exists() {
        return open_snapshot(state_dir, tx_id);
    }
    let staging = root.join(format!(".{tx_id}.partial"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir(&staging)?;
    let blobs = staging.join("blobs");
    std::fs::create_dir(&blobs)?;
    let prepared_dir = staging.join("prepared");
    if plan.is_some() {
        std::fs::create_dir(&prepared_dir)?;
    }
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
        copy_synced(&source.path, &destination)?;
        crate::content::verify(&destination, source.size, content_digest)?;
        let prepared = plan
            .map(|plan| prepare_artifact(&staging, ordinal, source, content_id, &destination, plan))
            .transpose()?;
        files.push(SnapshotFile {
            content_id: content_id.clone(),
            content_digest: content_digest.clone(),
            content_size: source.size,
            relative_blob,
            prepared,
        });
    }
    snapshot_failpoint(1)?;
    let snapshot = SourceSnapshot {
        version: SNAPSHOT_VERSION,
        tx_id: tx_id.to_string(),
        snapshot_digest: snapshot_digest(tx_id, &files)?,
        files,
    };
    write_synced_json(&staging.join("manifest.json"), &snapshot)?;
    sync_dir(&blobs)?;
    if plan.is_some() {
        sync_dir(&prepared_dir)?;
    }
    sync_dir(&staging)?;
    snapshot_failpoint(2)?;
    std::fs::rename(&staging, &final_dir)?;
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
    anyhow::ensure!(
        snapshot.snapshot_digest == snapshot_digest(tx_id, &snapshot.files)?,
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

fn prepare_artifact(
    staging: &Path,
    ordinal: usize,
    source: &crate::walk::FileEntry,
    content_id: &str,
    snapshot_blob: &Path,
    plan: &Plan,
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
    let sniffed = crate::sniff::sniff(&source.path)
        .with_context(|| format!("sniff {} for durable preparation", source.rel))?;
    let relative_ndjson = format!("prepared/{ordinal:08}.ndjson");
    let path = staging.join(&relative_ndjson);
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    let mut writer = BufWriter::new(file);
    let mut records = 0u64;
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
        fields.insert("ax_run".into(), Value::String("generation-pending".into()));
        fields.insert(
            "ax_format".into(),
            Value::String(source.path.extension().map_or_else(
                || "unknown".into(),
                |extension| extension.to_string_lossy().to_ascii_lowercase(),
            )),
        );
        let id = crate::ids::doc_id(slug, content_id, &record.locator);
        let action = serde_json::json!({"index": {"_index": dataset.index, "_id": id}});
        if writeln!(writer, "{action}")
            .and_then(|_| writeln!(writer, "{}", Value::Object(fields.clone())))
            .is_err()
        {
            sink_error = Some(anyhow::anyhow!("write prepared NDJSON"));
            return false;
        }
        records += 1;
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
    let stats = crate::extract::extract(snapshot_blob, &sniffed, None, &mut sink)
        .with_context(|| format!("extract {} into durable preparation", source.rel))?;
    anyhow::ensure!(
        stats.junk == 0,
        "durable preparation of {} produced {} junk records",
        source.rel,
        stats.junk
    );
    if let Some(error) = sink_error {
        return Err(error);
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    let bytes = std::fs::metadata(&path)?.len();
    Ok(PreparedArtifact {
        relative_ndjson,
        records,
        passages,
        vectors,
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

fn snapshot_digest(tx_id: &str, files: &[SnapshotFile]) -> Result<String> {
    let mut files = files.to_vec();
    files.sort_by(|left, right| {
        left.content_id
            .cmp(&right.content_id)
            .then_with(|| left.relative_blob.cmp(&right.relative_blob))
    });
    let encoded = serde_json::to_vec(&(SNAPSHOT_VERSION, tx_id, files))?;
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

fn copy_synced(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
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
fn fail_next_snapshot(boundary: u8) {
    SNAPSHOT_FAILPOINT.store(boundary, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn snapshot_failpoint(boundary: u8) -> Result<()> {
    if SNAPSHOT_FAILPOINT.compare_exchange(
        boundary,
        0,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
    ) == Ok(boundary)
    {
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
    REPLAY_FAIL_AFTER_APPLY.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn replay_fail_after_apply() -> Result<()> {
    if REPLAY_FAIL_AFTER_APPLY.swap(false, std::sync::atomic::Ordering::SeqCst) {
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
        crate::content::resolve(walk::walk(root, false).unwrap()).unwrap()
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
                },
            )]),
            alias_paths_indexed: false,
            ..Plan::default()
        }
    }

    fn execution(tx_id: &str, digest: &str) -> ExecutionIdentity {
        ExecutionIdentity {
            version: EXECUTION_IDENTITY_VERSION,
            root_identity: "root".into(),
            url: "http://engine".into(),
            prefix: "ax".into(),
            follow_symlinks: false,
            chunker_identity: "chunker-v1".into(),
            embedding_backend: "lexical".into(),
            embedding_model: "feature-hash".into(),
            embedding_tokenizer: "none".into(),
            embedding_dimension: 384,
            graph_enabled: false,
            brain: "brain".into(),
            detector_identity: "none".into(),
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
    }

    impl SyncOperationBackend for FakeBackend {
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

    fn empty_base_for(plan: &Plan) -> Plan {
        let mut base = plan.clone();
        base.files.clear();
        base.duplicate_files.clear();
        for dataset in &mut base.datasets {
            dataset.file_count = 0;
        }
        base
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
        let snapshot =
            create_prepared_snapshot(state.path(), "tx-prepared", &inventory, &plan).unwrap();
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
                    "embedding_backend": "lexical",
                    "embedding_model": "feature-hash",
                    "embedding_tokenizer": "none",
                    "embedding_dimension": 384,
                    "graph_enabled": false,
                    "brain": "brain",
                    "detector_identity": "none",
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
        journal.write_plan(&empty_base_for(&plan)).unwrap();
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
        journal.write_plan(&empty_base_for(&first_plan)).unwrap();
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
}
