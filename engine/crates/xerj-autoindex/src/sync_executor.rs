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
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    /// #971: identity of every input `prepare_artifact` consumes for THIS file
    /// (chunker identity, document-id identity, the file's assignment, its
    /// alias paths, the datasets it routes to). A later generation whose file
    /// carries the same content digest AND the same identity reuses the sealed
    /// blob and prepared artifact via hardlink instead of re-copying,
    /// re-verifying and re-extracting, which is what makes a one-file change
    /// cost O(changed) rather than O(corpus). Skip-serialized so a manifest
    /// sealed before this field existed keeps hashing to the digest the journal
    /// recorded — the same rule `records_by_dataset` follows above. `None` on
    /// this file therefore also means "a pre-#971 snapshot: never reuse".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_identity: Option<String>,
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
    /// The per-file record cap (#381) stopped this file's section stream before
    /// the whole body was emitted. Skip-serialized when false so a non-truncated
    /// artifact hashes exactly as before (a truncating file already differs by
    /// design — it seals fewer records).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
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

/// One operation this replay attempt still owes, with the name and byte
/// weight the progress surface reports for it. Precomputed once by
/// [`replay_pending_operations`] so the serial loop and the parallel
/// scheduler (#933) describe every operation identically — and so a worker
/// thread never needs the group maps the fields were derived from.
pub struct ReplayItem<'o> {
    pub operation: &'o SyncOperation,
    pub rel: String,
    pub bytes: u64,
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

    /// Progress hooks for the replay loop (#931). The loop is where a
    /// generated run spends its indexing time, and it used to report nothing:
    /// the stream kept printing the PREVIOUS phase — `scan`, at 100%, with a
    /// `since_progress_s` that only climbed — for as long as documents were
    /// landing, which is exactly what a real hang looks like. A backend with a
    /// progress surface overrides these; the defaults keep test backends silent.
    ///
    /// `items` is the number of operations still to apply and `bytes` the
    /// sealed NDJSON they will send, so the phase has an honest denominator in
    /// the unit the ETA is derived from.
    fn replay_begins(&mut self, _items: u64, _bytes: u64) {}

    /// One operation is about to be applied; `rel` names its source file so the
    /// surface can say what a quiet tail is waiting on.
    fn operation_begins(&mut self, _rel: &str, _bytes: u64) {}

    /// The operation started by [`Self::operation_begins`] has been applied.
    fn operation_applied(&mut self) {}

    /// Apply `items` — the operations this attempt still owes, in operation
    /// order — journaling each one through `journal`.
    ///
    /// The default is the historical serial loop: `Started`, apply,
    /// `Committed`, one operation at a time. [`EsSyncBackend`] overrides it
    /// (#933) with a windowed scheduler when the run asked for more than one
    /// worker: one plan gives every group at most one operation
    /// (`plan_operations` emits at most one per group id), and an operation's
    /// writes are keyed by its group's content id, so two operations' writes
    /// never touch the same documents. The per-operation durable order is
    /// exactly the serial contract, overlapped — `Started` is journaled
    /// before the operation is dispatched, `Committed` after its apply
    /// returned, so a crash mid-window repeats precisely the operations
    /// whose accepted apply was not recorded, and `apply` stays convergent
    /// for exactly that retry.
    fn replay_operations(
        &mut self,
        items: &[ReplayItem<'_>],
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
        journal: &mut Journal,
    ) -> Result<()> {
        replay_serial(self, items, base, desired, snapshot, journal)
    }
}

/// The historical serial replay: one operation at a time, journaling
/// `Started` before its apply and `Committed` after it.
fn replay_serial<B: SyncOperationBackend + ?Sized>(
    backend: &mut B,
    items: &[ReplayItem<'_>],
    base: &CommittedManifest,
    desired: &GenerationManifest,
    snapshot: &SourceSnapshot,
    journal: &mut Journal,
) -> Result<()> {
    for item in items {
        let operation = item.operation;
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
        backend.operation_begins(&item.rel, item.bytes);
        backend.apply(operation, base, desired, snapshot)?;
        replay_fail_after_apply()?;
        journal.sync_operation_state(&operation.operation_id, SyncOperationState::Committed)?;
        backend.operation_applied();
    }
    Ok(())
}

/// The journal-side callbacks of [`replay_windowed`], one object so the
/// caller's mutable state — the journal — is borrowed once, not once per
/// closure.
pub(crate) trait ReplayHooks<T> {
    /// Called on the scheduling thread before the item is dispatched.
    fn begin(&mut self, item: &T) -> Result<()>;
    /// Called on the scheduling thread, in dispatch order, as each item's
    /// apply finishes. Returning an error stops all further dispatch.
    fn applied(&mut self, item: &T, outcome: Result<()>) -> Result<()>;
}

/// #933: apply `items` through a window at most `width` items wide.
///
/// This is the whole concurrency policy of the parallel replay, kept free of
/// ES, journal and progress detail so it can be unit-tested without a
/// server. [`ReplayHooks::begin`] runs on the calling thread before an item
/// is dispatched — where the serial loop journals `Started`; `apply` runs on
/// worker threads and may overlap with other items' `apply`;
/// [`ReplayHooks::applied`] runs on the calling thread, in dispatch order,
/// as each item finishes — where the serial loop journals `Committed`.
/// Completions are joined head-first, so `applied` sees items in exactly the
/// order they were begun, which is what keeps the journal a serial reader
/// can follow.
///
/// Failure semantics are the serial loop's plus one honest addition the
/// overlap forces, and they run through `applied`: the first `applied` that
/// returns an error stops all further dispatch, everything already
/// dispatched is drained to completion, and `applied` is still called for
/// each drained item so the caller can journal the applies the server did
/// accept. On the serial loop nothing else was in flight when an apply
/// failed, so its behaviour is unchanged; overlapped, skipping the
/// `Committed` write for an apply that already landed would only force a
/// convergent redo of it on resume. A caller that deliberately accepts a
/// failed outcome by returning `Ok` keeps the run going — the scheduler
/// trusts `applied`, not the outcome. The first error is the return value;
/// later errors are handed to `applied` and otherwise dropped, because the
/// first failure is the one a retry of the same command will meet again.
pub(crate) fn replay_windowed<T: Sync>(
    items: &[T],
    width: usize,
    hooks: &mut dyn ReplayHooks<T>,
    apply: &(dyn Fn(&T) -> Result<()> + Sync),
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    if width <= 1 || items.len() == 1 {
        // No overlap is possible; same callbacks, same order, no threads.
        for item in items {
            hooks.begin(item)?;
            hooks.applied(item, apply(item))?;
        }
        return Ok(());
    }
    /// One dispatched operation: on its own thread, or — when that thread
    /// could not be started (#1122) — already applied inline.
    enum Dispatched<'scope> {
        Thread(xerj_common::thread_spawn::ScopedJoinHandle<'scope, Result<()>>),
        Inline(Result<()>),
    }
    // #1122: `Builder::spawn_scoped` can abort the process after returning
    // `Ok`, when the child cannot map its alternate signal stack. This
    // scope uses `pthread_create`. A refusal applies the item on this
    // thread; the closure that would have run it is dropped unrun.
    let failure = xerj_common::thread_spawn::scope(|scope| -> Result<Option<anyhow::Error>> {
        let mut next = items.iter();
        let mut in_flight: std::collections::VecDeque<(&T, Dispatched<'_>)> =
            std::collections::VecDeque::new();
        let mut failure: Option<anyhow::Error> = None;
        loop {
            while failure.is_none() && in_flight.len() < width {
                // `begin` is the Started write: it happens on THIS thread,
                // before the operation exists remotely, so an operation that
                // was never dispatched is also never recorded as begun.
                let Some(item) = next.next() else { break };
                hooks.begin(item)?;
                let dispatched = item;
                let slot = match scope.spawn_returning("xerj-sync-apply", move || apply(dispatched))
                {
                    Ok(handle) => Dispatched::Thread(handle),
                    Err(_) => Dispatched::Inline(apply(item)),
                };
                in_flight.push_back((item, slot));
            }
            let Some((item, slot)) = in_flight.pop_front() else {
                break;
            };
            let outcome = match slot {
                Dispatched::Thread(handle) => handle.join().unwrap_or_else(|panic| {
                    Err(anyhow::anyhow!(
                        "replay apply panicked: {}",
                        panic_message(panic)
                    ))
                }),
                Dispatched::Inline(outcome) => outcome,
            };
            let reported = hooks.applied(item, outcome);
            if let (None, Err(error)) = (&failure, reported) {
                // First failure stops dispatch; the drain still reports.
                failure = Some(error);
            }
        }
        Ok(failure)
    })?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The message of a panicking worker, when it carried one — a panic is not an
/// ES answer and must not look like one.
fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        "no message".to_owned()
    }
}

/// Per-replay id→position caches for the manifests and snapshot a replay
/// walks (#1224).
///
/// `apply_shared` resolves every operation's group in the base and desired
/// manifests and its content in the snapshot, and the linear `iter().find()`
/// each resolution used is quadratic over a corpus run — the 402,744-operation
/// cve-records build paid milliseconds of scanning per file. Built once per
/// [`EsSyncBackend::replay_operations`] call from that call's arguments, so
/// inside a replay every hit is exact. A lookup outside a replay (or against
/// differently-shaped data than the cache was built from — guarded by the
/// recorded lengths) falls back to the scan the cache replaced, and every hit
/// is verified against the slice it answers from, so a stale cache can never
/// return a group the scan would not have.
#[derive(Debug, Clone, Default)]
struct ReplayLookup {
    base_len: usize,
    base: std::collections::HashMap<String, usize>,
    desired_len: usize,
    desired: std::collections::HashMap<String, usize>,
    files_len: usize,
    files: std::collections::HashMap<String, usize>,
}

impl ReplayLookup {
    fn build(
        base: &crate::sync::CommittedManifest,
        desired: &crate::sync::GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Self {
        Self {
            base_len: base.groups.len(),
            base: base
                .groups
                .iter()
                .enumerate()
                .map(|(position, group)| (group.group_id.clone(), position))
                .collect(),
            desired_len: desired.groups.len(),
            desired: desired
                .groups
                .iter()
                .enumerate()
                .map(|(position, group)| (group.group_id.clone(), position))
                .collect(),
            files_len: snapshot.files.len(),
            files: snapshot
                .files
                .iter()
                .enumerate()
                .map(|(position, file)| (file.content_id.clone(), position))
                .collect(),
        }
    }

    /// `groups` by id through a cache side, falling back to the scan when the
    /// cache does not describe this exact slice.
    fn group<'m, G>(
        side: &std::collections::HashMap<String, usize>,
        cached_len: usize,
        groups: &'m [G],
        group_id: &str,
        id_of: impl Fn(&G) -> &str,
    ) -> Option<&'m G> {
        if cached_len != groups.len() {
            return groups.iter().find(|group| id_of(group) == group_id);
        }
        side.get(group_id)
            .copied()
            .and_then(|position| groups.get(position))
            .filter(|group| id_of(group) == group_id)
    }

    fn base_group<'m>(
        &self,
        base: &'m crate::sync::CommittedManifest,
        group_id: &str,
    ) -> Option<&'m crate::sync::ManifestGroup> {
        Self::group(&self.base, self.base_len, &base.groups, group_id, |group| {
            &group.group_id
        })
    }

    fn desired_group<'m>(
        &self,
        desired: &'m crate::sync::GenerationManifest,
        group_id: &str,
    ) -> Option<&'m crate::sync::ManifestGroup> {
        Self::group(
            &self.desired,
            self.desired_len,
            &desired.groups,
            group_id,
            |group| &group.group_id,
        )
    }

    fn file<'m>(&self, snapshot: &'m SourceSnapshot, content_id: &str) -> Option<&'m SnapshotFile> {
        if self.files_len != snapshot.files.len() {
            return snapshot
                .files
                .iter()
                .find(|file| file.content_id == content_id);
        }
        self.files
            .get(content_id)
            .copied()
            .and_then(|position| snapshot.files.get(position))
            .filter(|file| file.content_id == content_id)
    }
}

/// How many per-digest verify windows to issue for one semantic field, and
/// how many to tick without asking (#1267).
///
/// A field whose `exists` total is zero cannot match any digest. Issuing the
/// `term` + `exists` window anyway is the pre-#1260 brute scan: one window
/// per [`EsSyncBackend::VERIFY_WINDOW`] digests, each walking the index, each
/// returning no hits. `window == 0` asks for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExistsProbePlan {
    issue_windows: u64,
    skip_windows: u64,
}

fn plan_exists_probes(values: usize, window: usize, exists_total: u64) -> ExistsProbePlan {
    if window == 0 {
        return ExistsProbePlan {
            issue_windows: 0,
            skip_windows: 0,
        };
    }
    let windows = values.div_ceil(window) as u64;
    if exists_total == 0 {
        ExistsProbePlan {
            issue_windows: 0,
            skip_windows: windows,
        }
    } else {
        ExistsProbePlan {
            issue_windows: windows,
            skip_windows: 0,
        }
    }
}

/// Catalog documents whose live `_source` is not already the sealed projection.
///
/// A resume of the same generation re-enters publication with the same
/// `run_id` and the same bytes (#1267). Those ids are not rewritten: the bulk
/// that used to send every projection document is what made the following
/// `run_id` walk match the whole catalog.
fn catalog_docs_to_write(
    expected: &BTreeMap<String, Value>,
    live: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    expected
        .iter()
        .filter(|(id, document)| live.get(*id) != Some(*document))
        .map(|(id, document)| ((*id).clone(), (*document).clone()))
        .collect()
}

/// How many of the prior generation's published ids this generation kept.
///
/// Kept documents still carry the prior `run_id`. The live count of that
/// `run_id` equals this number when nothing stray remains and every kept
/// document is still there. A smaller count is the documented cross-journal
/// collision (a kept document was overwritten elsewhere). A larger count is a
/// stray. Equal is the fast path that does not walk.
fn prior_run_expected_remainder(prior_written: &BTreeSet<String>, kept: &BTreeSet<String>) -> u64 {
    prior_written.intersection(kept).count() as u64
}

/// Ids carrying one generation's `run_id`, one per line, inside that
/// generation's already-sealed snapshot directory.
///
/// The manifest digest does not cover this file. Missing means "published by
/// an older client": the next generation walks that `run_id` once. Present
/// means the next generation's stray check is a `size: 0` count.
const CATALOG_RUN_IDS_FILE: &str = "catalog-run.ids";

fn catalog_run_ids_path(state_dir: &Path, run_id: &str) -> Result<std::path::PathBuf> {
    validate_tx_id(run_id)?;
    Ok(state_dir
        .join("sync-snapshots")
        .join(run_id)
        .join(CATALOG_RUN_IDS_FILE))
}

fn write_catalog_run_ids<'a>(
    state_dir: &Path,
    run_id: &str,
    ids: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    let path = catalog_run_ids_path(state_dir, run_id)?;
    let mut lines: Vec<&str> = Vec::new();
    for id in ids {
        anyhow::ensure!(
            !id.contains('\n') && !id.contains('\r'),
            "catalog document id {id:?} contains a newline and cannot be stored one-per-line"
        );
        lines.push(id.as_str());
    }
    lines.sort_unstable();
    let parent = path
        .parent()
        .context("catalog run id file has no parent directory")?;
    let tmp = parent.join(format!("{CATALOG_RUN_IDS_FILE}.tmp"));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        for id in &lines {
            writeln!(file, "{id}")?;
        }
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    sync_dir(parent)?;
    Ok(())
}

fn read_catalog_run_ids(state_dir: &Path, run_id: &str) -> Result<Option<BTreeSet<String>>> {
    let path = catalog_run_ids_path(state_dir, run_id)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", path.display()));
        }
    };
    let mut ids = BTreeSet::new();
    for line in text.lines() {
        if !line.is_empty() {
            ids.insert(line.to_owned());
        }
    }
    Ok(Some(ids))
}

/// `_id -> _source` for every `found: true` entry of an `_mget` response.
/// `found: false` is an absent id, not an error. A duplicate id is.
fn mget_sources(response: &Value) -> Result<BTreeMap<String, Value>> {
    let docs = response
        .pointer("/docs")
        .and_then(Value::as_array)
        .context("catalog mget response has no docs")?;
    let mut sources = BTreeMap::new();
    for doc in docs {
        let id = doc
            .get("_id")
            .and_then(Value::as_str)
            .context("catalog mget doc has no _id")?;
        if doc.get("found").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let source = doc
            .get("_source")
            .cloned()
            .with_context(|| format!("catalog mget doc {id} is found but has no _source"))?;
        anyhow::ensure!(
            sources.insert(id.to_owned(), source).is_none(),
            "catalog mget returned duplicate ID {id}"
        );
    }
    Ok(sources)
}

/// Advance the current phase by `n` items. Finalize-verify and the catalog
/// read-back call this once per window (or once per skipped window) so the
/// ticker moves while a request is in flight only for that one window, not
/// for the whole phase (#1267).
fn credit_verify_items(pr: &crate::progress::Progress, n: u64) {
    for _ in 0..n {
        pr.item_done(0);
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
    /// #1224: per-replay lookup caches; empty outside
    /// [`Self::replay_operations`], where every lookup falls back to the
    /// linear scan this field exists to avoid. See [`ReplayLookup`].
    lookup: ReplayLookup,
    /// #755: the run's progress surface, so a legacy-catalog mapping warning
    /// reaches the operator through the surface that owns stderr (#241) rather
    /// than a bare `eprintln!` that `--progress none` cannot silence and
    /// `--progress json` cannot parse.
    pr: &'a crate::progress::Progress,
    /// `index_identity` of a plan whose dataset mappings this same process has
    /// already installed (`preflight_generation_mappings`). Provisioning that
    /// exact generation then skips the per-dataset loop — two round trips per
    /// dataset, ~3,000 requests on the 1,526-dataset corpus that produced
    /// #929 — and installs only the catalog mapping. A replayed generation
    /// (any other identity, or none) is always provisioned in full.
    installed_index_identity: Option<String>,
    /// The operation the replay loop is inside; dropping it counts the
    /// operation done and clears it from the surface's in-flight table.
    in_flight: Option<crate::progress::FileGuard<'a>>,
    /// #933: how many operations [`Self::replay_operations`] may have in
    /// flight at once. `1` — the default — is the historical serial loop,
    /// and what every run without `--workers` beyond it configures.
    replay_workers: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<'a> EsSyncBackend<'a> {
    pub fn new(
        es: &'a crate::esclient::Es,
        state_dir: &'a Path,
        bulk_bytes: usize,
        pr: &'a crate::progress::Progress,
    ) -> Self {
        Self {
            es,
            state_dir,
            bulk_bytes: bulk_bytes.max(64 * 1024),
            lookup: ReplayLookup::default(),
            pr,
            installed_index_identity: None,
            in_flight: None,
            replay_workers: 1,
        }
    }

    /// See `installed_index_identity`.
    pub fn with_installed_mappings(mut self, index_identity: String) -> Self {
        self.installed_index_identity = Some(index_identity);
        self
    }

    /// Set the replay window width (#933). Production passes the run's
    /// `--workers` — the same number that already bounds the run's bulk
    /// admission window, so the operations share one AIMD gate with the
    /// bulks they send and a 429 shrinks what THIS path offers too.
    pub fn with_replay_workers(mut self, workers: usize) -> Self {
        self.replay_workers = workers.max(1);
        self
    }

    fn delete_group(&self, group: &ManifestGroup, plan: &Plan) -> Result<()> {
        for index in group_indices(group, plan)? {
            // The delete only has work when the index already holds documents
            // of this content identity. Every upsert used to fire it
            // unconditionally, and a one-record-per-file corpus pays one
            // upsert per FILE — cvelistV5 is 402,744 groups, each a defensive
            // `?refresh=true` delete_by_query that matched nothing and still
            // cost 5-16 s of server refresh under concurrent write load:
            // ~1 op/s measured, days projected (#1224). A first-time group
            // cannot hold partials, and a retried op replays the SAME
            // prepared artifact over the same deterministic `_id`s, so an
            // unrefreshed partial is overwritten by the replay itself. A
            // size:0 term search answers "is anything visible" in <1 ms; a
            // missing index (404) holds nothing by definition (#1173's
            // rule). A probe the node did not answer is an error, never a
            // skip — the delete path stays exactly as strict as before.
            if !self.index_holds_content(&index, &group.content_id)? {
                continue;
            }
            self.es.delete_by_query(
                &index,
                &serde_json::json!({"term": {
                    "ax_file": &group.content_id
                }}),
            )?;
        }
        Ok(())
    }

    /// Does this index hold any visible document of this content identity?
    /// The [#1224] gate in front of [`Self::delete_group`]'s
    /// `delete_by_query`: `None` (index absent) and 0 hits both mean "the
    /// delete would remove nothing", every other outcome propagates.
    fn index_holds_content(&self, index: &str, content_id: &str) -> Result<bool> {
        let Some(v) = self.es.search_present(
            index,
            &serde_json::json!({
                "size": 0,
                "track_total_hits": true,
                "query": {"term": {"ax_file": content_id}},
            }),
        )?
        else {
            return Ok(false);
        };
        let total = v
            .pointer("/hits/total/value")
            .and_then(Value::as_u64)
            .or_else(|| v.pointer("/hits/total").and_then(Value::as_u64))
            .with_context(|| format!("no total in probe of {index} for {content_id}"))?;
        Ok(total > 0)
    }

    /// The remote work of one operation, through a shared reference: the
    /// windowed replay (#933) runs this from several worker threads at once,
    /// over operations whose groups — and therefore whose documents, selected
    /// by `ax_file` content id — are disjoint. The body is the serial loop's
    /// `apply`, unchanged; only the receiver changed.
    fn apply_shared(
        &self,
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
        let old = self.lookup.base_group(base, &operation.group_id);
        let new = self.lookup.desired_group(desired, &operation.group_id);
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

    fn replay_prepared(&self, snapshot: &SourceSnapshot, content_id: &str) -> Result<()> {
        // Cache-first resolution of the sealed artifact (#1224): a linear
        // scan of the snapshot's files per upsert is quadratic over a corpus
        // run. Falls back to `prepared_for`'s scan when no replay cache
        // describes this snapshot.
        let prepared = self
            .lookup
            .file(snapshot, content_id)
            .and_then(|file| file.prepared.as_ref())
            .with_context(|| format!("content {content_id} has no sealed prepared artifact"))?;
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
        let old_group = self
            .lookup
            .base_group(base, &desired_group.group_id)
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

    /// Digests carried per read-back window in the batched finalize-verify.
    /// One window is one `terms` filter plus one `terms` aggregation, so a
    /// corpus with 400k changed groups asks ~400 round trips instead of
    /// ~1.2 M serial searches (#1183's count lane). Window size trades server
    /// work per request against round-trip count; the aggregation must be
    /// allowed as many buckets as the window can hold distinct values, which
    /// `windowed_value_counts` always requests.
    const VERIFY_WINDOW: usize = 1024;

    /// Exact live-row count per value of a keyword content-digest field
    /// (`ax_file` on data indices, `file_key` on the catalog), read windowed:
    /// one `terms` filter + one `terms` aggregation per [`Self::VERIFY_WINDOW`]
    /// values. The engine's terms `doc_count` is exact — no probabilistic
    /// sketch sits in the metric path — so each bucket is the number a
    /// per-value `term` search returned as `hits.total`. A value with no live
    /// row appears in no bucket and keeps the 0 it was seeded with.
    ///
    /// Filter context, `terms` first: the engine's columnar filter executor
    /// checks leaves in order, so the digest set narrows the walk before an
    /// `exists` leaf parses any stored source. In scoring context (`must`)
    /// this shape source-scanned every row — 9.6 s per call on a 91k-doc
    /// segment in the serial era (#1183).
    ///
    /// `extra_filters` are appended verbatim to the window query's
    /// `bool.filter`: the semantic leg's `exists`, the catalog leg's
    /// `run_id` term.
    ///
    /// `credit` ticks the current phase once per window after that window's
    /// answer is in. The capability probe passes `false`: it is one sample,
    /// not one of the phase's windows, and a tick there would walk the bar
    /// past its denominator. A `timed_out` window is re-asked, bounded, and
    /// only its exhaustion bails before the tick, so a partial window is
    /// not counted as zero and is not credited (#1761, #2405).
    fn windowed_value_counts(
        &self,
        index: &str,
        field: &str,
        values: &[String],
        extra_filters: &[Value],
        credit: bool,
    ) -> Result<HashMap<String, u64>> {
        let mut counts: HashMap<String, u64> =
            values.iter().map(|value| (value.clone(), 0u64)).collect();
        for window in values.chunks(Self::VERIFY_WINDOW) {
            let mut filter = Vec::with_capacity(1 + extra_filters.len());
            filter.push(serde_json::json!({"terms": {field: window}}));
            filter.extend(extra_filters.iter().cloned());
            // #2222 + #2405: `search_page_for_verify`, not `search`. A
            // DELETE of this index while the window is in flight used to
            // leave the caller parked on it (`waiting_on` the deleted name)
            // until the transport budget ran out, and the generation never
            // committed; and a `timed_out` answer used to exit 1 on the
            // first partial page. The call re-asks the same window, bounded,
            // and still fails fast on a deleted index. A deadline page is
            // HTTP 200 with `timed_out` and an empty aggregation — counting
            // it as "every digest is absent" would abort a generation whose
            // rows are on the node, so exhaustion stays a loud error and is
            // never counted as zero (#1761).
            let response = self.es.search_page_for_verify(
                index,
                &serde_json::json!({
                    "size": 0,
                    "query": {"bool": {"filter": filter}},
                    // As many buckets as the window can hold distinct values,
                    // so no digest's count is silently dropped by a bucket cap.
                    "aggs": {"values": {"terms": {"field": field, "size": window.len()}}}
                }),
            )?;
            for bucket in response
                .pointer("/aggregations/values/buckets")
                .and_then(Value::as_array)
                .context("batched validation response has no terms aggregation")?
            {
                let key = bucket
                    .get("key")
                    .and_then(Value::as_str)
                    .context("batched validation bucket has no key")?;
                let doc_count = bucket
                    .get("doc_count")
                    .and_then(Value::as_u64)
                    .context("batched validation bucket has no doc_count")?;
                counts.insert(key.to_owned(), doc_count);
            }
            if credit {
                credit_verify_items(self.pr, 1);
            }
        }
        Ok(counts)
    }

    /// The catalog leg of the batched finalize-verify: per-digest counts read
    /// by FETCHING the matched documents, never by aggregation. The catalog
    /// is one global index that holds every corpus's file documents (402,814
    /// on the cve-records node) and has usually taken updates (dataset
    /// documents are rewritten by every finalize-catalog), so its version map
    /// carries delete events — and the engine's columnar agg fast path
    /// refuses exactly that (#1260), falling back to the brute agg corpus that
    /// deep-clones the whole index against `max_query_memory_mb` (measured
    /// 786.7 MB vs the 512 MB default on that catalog → 429, the #1183
    /// breaker shape). A `_source`-restricted fetch of the matched set has no
    /// such gate: 31 ms cold / 14 ms warm for a 1,024-digest window against
    /// that same catalog. Each digest expects `1 + aliases.len()` documents,
    /// so a window's matched set is small; pages advance by `from` until the
    /// exact `total` is collected, a total that moves mid-window fails loud,
    /// and the per-window sum of the counts must equal the query's own total
    /// — the same number the serial per-group count trusted.
    fn windowed_catalog_counts(
        &self,
        values: &[String],
        run_id: &str,
    ) -> Result<HashMap<String, u64>> {
        const PAGE: usize = 8192;
        const MAX_PAGES: usize = 64;
        let mut counts: HashMap<String, u64> =
            values.iter().map(|value| (value.clone(), 0u64)).collect();
        for window in values.chunks(Self::VERIFY_WINDOW) {
            let mut from = 0usize;
            let mut window_total: Option<u64> = None;
            let mut pages = 0usize;
            loop {
                anyhow::ensure!(
                    pages < MAX_PAGES,
                    "catalog read-back window needed more than {MAX_PAGES} pages"
                );
                pages += 1;
                let response = self.es.search_page(
                    crate::catalog::CATALOG_INDEX,
                    &serde_json::json!({
                        "size": PAGE,
                        "from": from,
                        "track_total_hits": true,
                        "_source": ["file_key"],
                        "query": {"bool": {"filter": [
                            {"terms": {"file_key": window}},
                            {"term": {"run_id": run_id}}
                        ]}}
                    }),
                )?;
                let page_total = response
                    .pointer("/hits/total/value")
                    .and_then(Value::as_u64)
                    .context("catalog read-back response has no total hit count")?;
                if let Some(seen) = window_total {
                    anyhow::ensure!(
                        seen == page_total,
                        "catalog read-back total moved from {seen} to {page_total} mid-window"
                    );
                }
                window_total = Some(page_total);
                let hits = response
                    .pointer("/hits/hits")
                    .and_then(Value::as_array)
                    .context("catalog read-back response has no hits array")?;
                if hits.is_empty() {
                    break;
                }
                for hit in hits {
                    if let Some(key) = hit.pointer("/_source/file_key").and_then(Value::as_str) {
                        if let Some(count) = counts.get_mut(key) {
                            *count += 1;
                        }
                    }
                }
                from += hits.len();
                if from as u64 >= page_total {
                    break;
                }
            }
            let counted: u64 = window.iter().filter_map(|value| counts.get(value)).sum();
            anyhow::ensure!(
                counted == window_total.unwrap_or(0),
                "catalog read-back counted {counted} of {} matched documents in a window",
                window_total.unwrap_or(0)
            );
            // One tick per window, after the window's pages have all come
            // back. The phase denominator is windows, not pages (#1267).
            // `search_page` re-asks a `timed_out` page and bails only on
            // exhaustion, so a partial catalog window is not counted as
            // zero and the phase does not sit at 0/N while another corpus
            // publishes to this shared index (#1761, #2405).
            credit_verify_items(self.pr, 1);
        }
        Ok(counts)
    }

    /// How many times [`Self::catalog_generation`] may be re-walked when the
    /// observation falls short of the sealed projection. #1212's second live
    /// shape: under load the engine can answer a short or empty page with
    /// `timed_out` absent, so neither the deadline retry nor the flag can
    /// detect it — only observed-count-versus-expected can. One intermittent
    /// answer must not cost the whole finalize again.
    const READBACK_WALK_ATTEMPTS: usize = 3;

    /// The read-back barrier's walk, retried as a whole while it comes back
    /// short of `expected`. A page-level retry cannot catch an unflagged
    /// truncation (#1212), but a re-walk can: the answer is intermittent, and
    /// a fresh walk after a refresh re-reads the full set. Returns the last
    /// observation when every attempt falls short — the caller's
    /// exact-equality check then produces the loud, precise mismatch error.
    fn catalog_generation_complete(
        &self,
        run_id: &str,
        expected: usize,
    ) -> Result<BTreeMap<String, Value>> {
        Self::rewalk_while_short(
            self.pr,
            expected,
            || self.catalog_generation(run_id),
            || self.es.refresh(crate::catalog::CATALOG_INDEX),
        )
    }

    /// [`Self::catalog_generation_complete`]'s loop, isolated so its contract
    /// is pinnable without a node: walk once, and while the observation is
    /// short of `expected` and attempts remain, refresh and walk again.
    fn rewalk_while_short(
        pr: &crate::progress::Progress,
        expected: usize,
        mut walk: impl FnMut() -> Result<BTreeMap<String, Value>>,
        mut refresh: impl FnMut() -> Result<()>,
    ) -> Result<BTreeMap<String, Value>> {
        let mut observation = walk()?;
        for attempt in 2..=Self::READBACK_WALK_ATTEMPTS {
            if observation.len() >= expected {
                return Ok(observation);
            }
            pr.note(&format!(
                "autoindex: catalog read-back walk {} of {} saw {} of {} expected documents — \
                 the node answered a short page without timed_out (#1212); refreshing and \
                 walking again",
                attempt - 1,
                Self::READBACK_WALK_ATTEMPTS,
                observation.len(),
                expected,
            ));
            refresh()?;
            observation = walk()?;
        }
        Ok(observation)
    }

    /// Page size of the `run_id`-scoped read-back walk. The engine reports the
    /// full match count on every page, so a page shorter than this is the walk
    /// reaching the end of the generation's documents.
    const CATALOG_WALK_PAGE: usize = 1000;

    /// The phase the catalog read-back surfaces itself under.
    ///
    /// Publication used to rewrite every projection document and then walk
    /// `term run_id` + `sort _id` + `search_after`. A resume of the same
    /// generation rewrote the catalog, so that term matched every document
    /// (402,814 on the cve-records node): 403 sorted pages at ~5.2 s each,
    /// behind a finished `finalize-catalog` ticker (#1267). The happy path
    /// now opens this phase on the projection's own length and reads those
    /// ids with `_mget`. [`Self::catalog_generation`] still opens it when a
    /// count does not match and the sorted walk is the way to name the
    /// difference.
    const CATALOG_READBACK_PHASE: &'static str = "finalize-catalog-readback";

    fn catalog_generation(&self, run_id: &str) -> Result<BTreeMap<String, Value>> {
        Self::walk_run_id_pages(
            self.pr,
            Self::CATALOG_READBACK_PHASE,
            Self::CATALOG_WALK_PAGE,
            |search_after| {
                let mut body = serde_json::json!({
                    "size": Self::CATALOG_WALK_PAGE,
                    "sort": [{"_id": "asc"}],
                    // #1267: without this the engine caps `hits.total.value` at
                    // the ES 8.x default 10,000 (relation `gte`), so the walk's
                    // progress denominator froze at 10k on the 402,814-doc
                    // catalog — the exact "stuck ticker" this phase exists to
                    // fix. Ask for the true match count so the bar and ETA are
                    // real. The walk still terminates on a short page, not on
                    // the total, so this only corrects the denominator.
                    "track_total_hits": true,
                    "query": {"term": {"run_id": run_id}}
                });
                if let Some(after) = search_after {
                    body["search_after"] = after.clone();
                }
                // #1212: a `search_page`, not a `search` — the engine answers a
                // missed 30 s default deadline with a partial 200 (`timed_out`),
                // and this walk's short-page break read one partial page as the
                // end of the catalog: 56,441 documents observed against 58,568
                // that were all there, on a node whose sorted pages took 11-40 s
                // while its breaker drained. The page variant retries a timed-out
                // page at the same continuation key instead of returning it.
                self.es.search_page(crate::catalog::CATALOG_INDEX, &body)
            },
        )
    }

    /// Drive a `run_id`-scoped, `_id asc` + `search_after` generation walk to
    /// completion, collecting `_id -> _source` for every matched document and
    /// surfacing the walk as its own progress phase (#1267). `fetch_page` is
    /// handed the previous page's continuation sort key (`None` for the first
    /// page) and returns the raw search response; isolating it this way lets
    /// the paging contract — the duplicate-id guard, the short-page terminator,
    /// the continuation-key check, and the per-page progress ticks — be pinned
    /// without a node, the way [`Self::rewalk_while_short`] is. `page` is the
    /// request `size`: a page shorter than it terminates the walk, so a test
    /// drives real multi-page paging with a handful of documents.
    ///
    /// The phase opens on the first page using the engine's reported match
    /// total as the item denominator, then ticks once per collected document,
    /// so the surface moves and carries an ETA across a walk that can run for
    /// over half an hour. A total the engine does not report leaves the phase
    /// without a denominator (`pct=unknown`) rather than inventing one.
    fn walk_run_id_pages(
        pr: &crate::progress::Progress,
        phase: &'static str,
        page: usize,
        mut fetch_page: impl FnMut(Option<&Value>) -> Result<Value>,
    ) -> Result<BTreeMap<String, Value>> {
        let mut documents = BTreeMap::new();
        let mut search_after: Option<Value> = None;
        let mut phase_opened = false;
        loop {
            let response = fetch_page(search_after.as_ref())?;
            if !phase_opened {
                let total = response
                    .pointer("/hits/total/value")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                pr.phase(phase, total, 0);
                phase_opened = true;
            }
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
                pr.item_done(0);
            }
            if hits.len() < page {
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

    /// A warm verify window slower than this means the server is answering
    /// the window shape on its brute path. Measured on the #1267 resume: the
    /// `term ax_file` + `exists` probe took 3.4 s warm on a server without the
    /// #1260 exists arm and 2.7 ms on one with it — three orders of magnitude
    /// either side of this line, so it separates the two generations without
    /// being sensitive to node load.
    const SERVER_PROBE_SLOW: std::time::Duration = std::time::Duration::from_secs(1);

    /// The operational half of #1267: before `finalize-verify` issues its
    /// windows, time ONE representative window and, if it is slow, say so.
    ///
    /// The resume protocol cannot otherwise tell that the server predates the
    /// client's fast-agg fix generation (#1260/#1270). On the cve-records
    /// resume that cost four hours of ~3.4 s probes before anyone looked —
    /// the client was a #1259 binary, the server an older measure build, and
    /// nothing in the handshake differs between them. `GET /` cannot help:
    /// its `version.number` is the ES-compat version (`8.13.0` or the
    /// `--compat-version` override), not XERJ's. `/v1/health` does carry the
    /// XERJ version and is quoted in the warning when it differs from the
    /// client's, but a branch or measure build reports the same string as
    /// the release it forked from — exactly the #1267 shape — so latency is
    /// the signal and the version only context.
    ///
    /// `window_query` runs the window and returns how long it took (the caller
    /// times it, which keeps this pinnable without a node or a clock). A slow
    /// first answer is re-asked once before warning: the first touch after a
    /// server boot pages the index in cold (8.8 s on the FIXED binary in
    /// #1267), and that must not read as an old server. A probe that fails is
    /// not this function's business — the verify window that follows asks the
    /// same thing and fails loud — so it returns `None` without a warning.
    /// It never aborts the run: an old server is slow, not wrong.
    fn probe_server_generation(
        pr: &crate::progress::Progress,
        windows: u64,
        threshold: std::time::Duration,
        mut window_query: impl FnMut() -> Result<std::time::Duration>,
        server_version: impl FnOnce() -> Option<String>,
    ) -> Option<std::time::Duration> {
        let cold = window_query().ok()?;
        if cold <= threshold {
            return Some(cold);
        }
        let warm = window_query().ok()?;
        if warm <= threshold {
            return Some(warm);
        }
        let client = env!("CARGO_PKG_VERSION");
        let version = match server_version() {
            Some(server) if server != client => {
                format!(" The server reports XERJ {server}; this client is {client}.")
            }
            Some(_) => format!(
                " The server reports the same version as this client ({client}), so it is \
                 likely a branch or development build cut before the fix."
            ),
            None => String::new(),
        };
        let projected_s = warm.as_secs_f64() * windows as f64;
        pr.warn(&format!(
            "autoindex: warning: one finalize-verify window took {:.1}s (twice; under {}ms is \
             expected). The server is likely older than this client and lacks the columnar \
             term+exists count lane (#1260/#1270), so every verify window brute-scans its \
             index: {windows} windows at this rate is ~{:.1}h.{version} Continuing — the \
             result will be correct, only slow. To avoid the wait, stop this run, upgrade the \
             server to this client's version, and re-run the same command: the journal resumes \
             where it stopped.",
            warm.as_secs_f64(),
            threshold.as_millis(),
            projected_s / 3600.0,
        ));
        Some(warm)
    }

    /// The XERJ version the server reports on `/v1/health` (`data.version`),
    /// or `None` when the endpoint is absent or unreadable.
    fn server_xerj_version(&self) -> Option<String> {
        self.es
            .get_json("/v1/health")
            .ok()?
            .pointer("/data/version")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Ids fetched per `_mget`. Under the engine's default
    /// `limits.max_mget_docs` (10_000), so one window is one request.
    const MGET_WINDOW: usize = 1000;

    /// Read catalog `_source`s by id. `tick` credits one phase item per id
    /// after that id's window returns, so a multi-window read-back moves the
    /// bar between requests instead of once at the end.
    fn mget_catalog(&self, ids: &[String], tick: bool) -> Result<BTreeMap<String, Value>> {
        let mut observed = BTreeMap::new();
        if ids.is_empty() {
            return Ok(observed);
        }
        let _watching = self.pr.watch_file(crate::catalog::CATALOG_INDEX, 0);
        for window in ids.chunks(Self::MGET_WINDOW) {
            let response = self.es.mget(crate::catalog::CATALOG_INDEX, window)?;
            observed.extend(mget_sources(&response)?);
            if tick {
                credit_verify_items(self.pr, window.len() as u64);
            }
        }
        Ok(observed)
    }

    /// `hits.total` of `exists` on `field`. One query per semantic
    /// `(index, field)`, in place of a per-digest `term` + `exists` window
    /// that cannot match when this total is zero (#1267).
    fn field_exists_total(&self, index: &str, field: &str) -> Result<u64> {
        let response = self.es.search_for_verify(
            index,
            &serde_json::json!({
                "size": 0,
                "track_total_hits": true,
                "query": {"bool": {"filter": [{"exists": {"field": field}}]}}
            }),
        )?;
        response
            .pointer("/hits/total/value")
            .and_then(Value::as_u64)
            .context("exists preflight has no total hit count")
    }

    /// How many live catalog documents carry `run_id`. `size: 0`, so the
    /// answer is a count, not a page of stored sources.
    fn catalog_run_count(&self, run_id: &str) -> Result<u64> {
        let response = self.es.search(
            crate::catalog::CATALOG_INDEX,
            &serde_json::json!({
                "size": 0,
                "track_total_hits": true,
                "query": {"bool": {"filter": [{"term": {"run_id": run_id}}]}}
            }),
        )?;
        response
            .pointer("/hits/total/value")
            .and_then(Value::as_u64)
            .context("catalog run_id count has no total hit count")
    }

    /// Live documents still carrying the prior generation's `run_id` must be
    /// a subset of the ids this generation kept.
    ///
    /// When this process published that generation, `catalog-run.ids` is the
    /// exact id set that carried the `run_id`. The live count equals the
    /// intersection of that set with the ids kept now, or the sorted walk
    /// names the strays. A missing file (a generation published before this
    /// file existed) takes the walk once.
    fn assert_no_prior_strays(
        &self,
        prior_run_id: &str,
        projection: &crate::generation_catalog::CatalogProjection,
    ) -> Result<()> {
        let written: BTreeSet<String> = projection.documents.keys().cloned().collect();
        let kept: BTreeSet<String> = projection
            .managed_ids
            .difference(&written)
            .cloned()
            .collect();
        match read_catalog_run_ids(self.state_dir, prior_run_id)? {
            Some(prior_written) => {
                let expected = prior_run_expected_remainder(&prior_written, &kept);
                let live = self.catalog_run_count(prior_run_id)?;
                if live == expected {
                    return Ok(());
                }
                self.pr.note(&format!(
                    "autoindex: prior catalog generation {prior_run_id} still counts {live} \
                     document(s) under its run_id against {expected} kept; walking them to name \
                     any stray (#1267)"
                ));
            }
            None => {
                self.pr.note(&format!(
                    "autoindex: prior catalog generation {prior_run_id} has no saved id list; \
                     walking documents that still carry its run_id (#1267)"
                ));
            }
        }
        let remaining = self.catalog_generation(prior_run_id)?;
        let strays: Vec<&String> = remaining.keys().filter(|id| !kept.contains(*id)).collect();
        anyhow::ensure!(
            strays.is_empty(),
            "prior catalog generation {prior_run_id} still has {} document(s) this generation \
             neither rewrote, deleted nor kept: {:?}",
            strays.len(),
            strays
        );
        Ok(())
    }

    /// `exists` total for one semantic field.
    ///
    /// The first field of the phase is the capability probe (#1267): a slow
    /// answer is re-asked once, and a slow warm answer warns, but only when
    /// the field actually matches documents — those are the windows about to
    /// brute-scan. A zero total skips the windows, so a slow empty answer
    /// must not project that one query across the whole phase. Later fields
    /// ask once and do not warn again.
    fn semantic_exists_total(
        &self,
        index: &str,
        field: &str,
        probe: bool,
        windows_if_matched: u64,
    ) -> Result<u64> {
        let started = std::time::Instant::now();
        let mut total = self.field_exists_total(index, field)?;
        let first = started.elapsed();
        if !probe || total == 0 || first <= Self::SERVER_PROBE_SLOW {
            return Ok(total);
        }
        let started = std::time::Instant::now();
        total = self.field_exists_total(index, field)?;
        let warm = started.elapsed();
        if total > 0 && warm > Self::SERVER_PROBE_SLOW {
            let samples = std::cell::RefCell::new(vec![first, warm]);
            Self::probe_server_generation(
                self.pr,
                windows_if_matched,
                Self::SERVER_PROBE_SLOW,
                || {
                    let mut samples = samples.borrow_mut();
                    anyhow::ensure!(!samples.is_empty(), "probe timing exhausted");
                    Ok(samples.remove(0))
                },
                || self.server_xerj_version(),
            );
        }
        Ok(total)
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
            //
            // #1183: this read-back must never carry `aggs`. A `size:0 + aggs`
            // query whose agg has no columnar fast path makes the server
            // deep-clone every matching document into owned Values before the
            // agg runs (the "aggregation corpus materialisation" charge in the
            // engine's search path, ~2 KB a doc against
            // `limits.max_query_memory_mb`). On the xerj-search rebuild one
            // ~570 k-record dataset estimated 1.1 GB against the 512 MB
            // default, the server answered 429 circuit_breaking_exception, the
            // client retried for its full 600 s budget, and the run aborted —
            // the "finalize-catalog deadlock". The count needs no corpus; the
            // time bounds come from two size-1 searches sorted on the time
            // field (`extreme_time` below), which sort from doc values and
            // clone nothing.
            let filter = serde_json::json!([
                {"terms": {"ax_file": content_ids}},
                {"term": {"ax_dataset": dataset.slug}}
            ]);
            let body = serde_json::json!({
                "size": 0,
                "track_total_hits": true,
                "query": {"bool": {"filter": filter}}
            });
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
            let time_bound = |order: &'static str| -> Result<Option<String>> {
                match &dataset.time_field {
                    Some(field) => self.extreme_time(&dataset.index, &filter, field, order),
                    None => Ok(None),
                }
            };
            out.insert(
                dataset.slug.clone(),
                crate::generation_catalog::DatasetCatalogStats {
                    record_count,
                    junk_records,
                    bytes,
                    formats,
                    time_min: time_bound("asc")?,
                    time_max: time_bound("desc")?,
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
            self.pr.item_done(0);
        }
        Ok(out)
    }

    /// One end of a dataset's time range, via a size-1 search sorted on the
    /// time field under the dataset's own filter (#1183). This replaced a
    /// `min`/`max` aggregation on the same field: the agg made the server
    /// materialise the aggregation corpus for the whole dataset and refuse at
    /// the query-memory breaker on large ones, while a sort reads doc values
    /// and clones nothing. Exact for the same reason the agg was: the filter
    /// is the dataset's own identity, and the sort's first hit IS the extreme.
    ///
    /// The returned string matches the agg's `value_as_string`: the stored
    /// `_source` value verbatim when the field holds strings (autoindex date
    /// fields are ISO strings), and epoch-milliseconds rendered by the same
    /// ISO helper the engine's `min`/`max` aggs use when it holds numbers —
    /// so the catalog document does not change shape between corpus builds.
    fn extreme_time(
        &self,
        index: &str,
        filter: &Value,
        field: &str,
        order: &str,
    ) -> Result<Option<String>> {
        let body = serde_json::json!({
            "size": 1,
            "query": {"bool": {"filter": filter}},
            "sort": [{field: {"order": order}}],
            "_source": [field]
        });
        let response = self.es.search(index, &body)?;
        Ok(response
            .pointer("/hits/hits/0/_source")
            .and_then(|src| src.get(field))
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => n.as_i64().map(xerj_common::schema::epoch_ms_to_iso8601_utc),
                _ => None,
            }))
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
        if self.installed_index_identity.as_deref() == Some(index_identity.as_str()) {
            return crate::ensure_generation_catalog_mapping(self.es, self.pr);
        }
        crate::ensure_generation_mappings(self.es, &desired.plan, self.pr)
    }

    fn replay_begins(&mut self, items: u64, bytes: u64) {
        self.pr.phase("index", items, bytes);
    }

    fn operation_begins(&mut self, rel: &str, bytes: u64) {
        let pr = self.pr;
        self.in_flight = Some(pr.file(rel, bytes));
    }

    fn operation_applied(&mut self) {
        self.in_flight = None;
    }

    fn apply(
        &mut self,
        operation: &SyncOperation,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()> {
        self.apply_shared(operation, base, desired, snapshot)
    }

    fn replay_operations(
        &mut self,
        items: &[ReplayItem<'_>],
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
        journal: &mut Journal,
    ) -> Result<()> {
        // #1224: resolve per-operation groups and prepared artifacts through
        // id indexes built once here, not a linear scan per operation.
        self.lookup = ReplayLookup::build(base, desired, snapshot);
        let width = self.replay_workers;
        if width <= 1 || items.len() <= 1 {
            return replay_serial(self, items, base, desired, snapshot, journal);
        }
        // The durability argument of the overlap rests on disjoint groups:
        // one plan gives a group at most one operation (`plan_operations`
        // walks base and desired by group id), so no two in-flight
        // operations ever write the same documents. Refuse here rather
        // than silently corrupt if a future planner breaks that.
        let mut groups = std::collections::BTreeSet::new();
        anyhow::ensure!(
            items
                .iter()
                .all(|item| groups.insert(item.operation.group_id.as_str())),
            "parallel replay requires at most one operation per group; this plan needs the \
             serial loop"
        );
        let this: &EsSyncBackend<'_> = self;
        let pr = this.pr;
        let apply = |item: &ReplayItem<'_>| {
            // The in-flight entry IS this worker's file: the guard counts the
            // item done on every exit path, exactly as the serial loop's
            // `operation_begins`/`operation_applied` pair does (#931).
            let _guard = pr.file(&item.rel, item.bytes);
            this.apply_shared(item.operation, base, desired, snapshot)
        };
        /// The serial loop's two journal writes, verbatim: Started before
        /// dispatch, Committed after the apply returned — the durable order
        /// #933 must not change, only overlap it.
        struct JournalHooks<'j> {
            journal: &'j mut Journal,
        }
        impl ReplayHooks<ReplayItem<'_>> for JournalHooks<'_> {
            fn begin(&mut self, item: &ReplayItem<'_>) -> Result<()> {
                let state = self
                    .journal
                    .pending_sync
                    .as_ref()
                    .and_then(|sync| sync.operation_states.get(&item.operation.operation_id))
                    .cloned();
                if state.is_none() {
                    self.journal.sync_operation_state(
                        &item.operation.operation_id,
                        SyncOperationState::Started,
                    )?;
                }
                Ok(())
            }

            fn applied(&mut self, item: &ReplayItem<'_>, outcome: Result<()>) -> Result<()> {
                let () = outcome?;
                replay_fail_after_apply()?;
                self.journal.sync_operation_state(
                    &item.operation.operation_id,
                    SyncOperationState::Committed,
                )
            }
        }
        let mut hooks = JournalHooks { journal };
        replay_windowed(items, width, &mut hooks, &apply)
    }

    fn publish_generation_catalog(
        &mut self,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()> {
        // #931: one exact read-back query per dataset. Named like the legacy
        // path's phase so a reader of either route sees the same vocabulary.
        self.pr
            .phase("finalize-catalog", desired.plan.datasets.len() as u64, 0);
        // #1183: the refreshes used to run one index per request, serially —
        // 1,479 round-trips on the xerj-search corpus, each taking seconds
        // against a node whose memory breaker was draining between them, so
        // the phase read as `stalled` for the better part of an hour before
        // the read-back behind it even started. The refresh path takes a
        // comma-list (the server resolves it through the same selector as a
        // wildcard), so one request per window of indexes, with one progress
        // tick per index so the phase denominator keeps its meaning.
        let indexes: Vec<&str> = desired
            .plan
            .datasets
            .iter()
            .map(|dataset| dataset.index.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        const REFRESH_WINDOW: usize = 50;
        for batch in indexes.chunks(REFRESH_WINDOW) {
            // #2234: one attempt. `Es::refresh` would retry a timed-out window
            // five more times (300s each) with no new information.
            self.es.refresh_once(&batch.join(","))?;
            for _ in batch {
                self.pr.item_done(0);
            }
        }
        self.es.refresh_once(crate::catalog::CATALOG_INDEX)?;
        let prior_run_id =
            base.execution
                .as_ref()
                .and_then(|execution| match &execution.source_policy {
                    SourceExecutionPolicy::DurableSnapshot { reference, .. } => {
                        reference.strip_prefix("sync-snapshots/")
                    }
                    SourceExecutionPolicy::AbortOnSourceChange { .. } => None,
                });
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
            &BTreeSet::new(),
        )?;
        // #1267: read the projection's ids before writing. A resume of this
        // same generation already stored these bytes under this `run_id`;
        // sending them again is what made the old `run_id` walk match the
        // whole catalog. The phase denominator is the projection, which is
        // the changed set (#971) plus the documents a same-generation resume
        // still has to confirm.
        let ids: Vec<String> = projection.documents.keys().cloned().collect();
        self.pr
            .phase(Self::CATALOG_READBACK_PHASE, ids.len() as u64, 0);
        let mut observed = self.mget_catalog(&ids, true)?;
        let to_write = catalog_docs_to_write(&projection.documents, &observed);
        let mut body = Vec::new();
        for id in &projection.stale_ids {
            let action =
                serde_json::json!({"delete": {"_index": crate::catalog::CATALOG_INDEX, "_id": id}});
            body.extend_from_slice(serde_json::to_string(&action)?.as_bytes());
            body.push(b'\n');
        }
        for (id, document) in &to_write {
            append_index_action(crate::catalog::CATALOG_INDEX, id, document, &mut body)?;
        }
        // #955: one document per file — this body grows with the corpus. On
        // the 48,533-file reference corpus it was 51,129 actions in 31.9 MB,
        // sent as ONE request under an 8 MB `--bulk-mb`, and the engine's
        // 50,000-action limit ended the run here after every operation had
        // been applied. Windowed like every other bulk; deletes stay first.
        // #971: the projection is now incremental — a one-file change sends
        // that file's document (plus datasets and the run document), not one
        // document per file in the corpus. #1267: a document whose live
        // source already equals that projection is not in `to_write`.
        if !body.is_empty() {
            checked_bulk_windowed(self.es, body, self.bulk_bytes)?;
            // #2234: one attempt. `Es::refresh` would retry a timed-out
            // catalog refresh five more times (300s each).
            self.es.refresh_once(crate::catalog::CATALOG_INDEX)?;
        }
        if !to_write.is_empty() {
            let written_ids: Vec<String> = to_write.keys().cloned().collect();
            for (id, source) in self.mget_catalog(&written_ids, false)? {
                observed.insert(id, source);
            }
        }
        projection.validate_observed(&observed)?;
        // The mget above sees only the projection's ids. A document this
        // generation did not project that still carries its `run_id` would
        // pass that check and inflate the next generation's kept set. The
        // count is that extra. When it disagrees, the sorted walk — retried
        // while short (#1212) — is what names the missing and unexpected ids.
        // A walk that then matches the projection is the publish: the count
        // was the optimization, and a capped total must not fail a complete
        // read-back.
        let live = self.catalog_run_count(&snapshot.tx_id)?;
        if live != projection.documents.len() as u64 {
            self.pr.note(&format!(
                "autoindex: catalog run_id {} counts {live} live documents against {} in the \
                 projection; reading them back by the sorted walk (#1267)",
                snapshot.tx_id,
                projection.documents.len()
            ));
            projection.validate_observed(
                &self.catalog_generation_complete(&snapshot.tx_id, projection.documents.len())?,
            )?;
        }
        write_catalog_run_ids(self.state_dir, &snapshot.tx_id, projection.documents.keys())?;
        if let Some(prior_run_id) = prior_run_id {
            // #971 sweep. Documents still carrying the prior generation's
            // run_id are legitimate — they are the intentionally kept
            // (unchanged) ones — but NOTHING ELSE may. The check itself is
            // unchanged (subset, not equality; a kept document may have been
            // overwritten by another journal). What changed (#1267) is that
            // the common case is a count against the ids the prior generation
            // saved, not another sorted walk of the catalog.
            self.assert_no_prior_strays(prior_run_id, &projection)?;
        }
        Ok(())
    }

    fn validate(
        &mut self,
        base: &CommittedManifest,
        desired: &GenerationManifest,
        snapshot: &SourceSnapshot,
    ) -> Result<()> {
        // #931: the read-back barrier is only exact against refreshed indices,
        // so every dataset is refreshed first — one request each, serially. On
        // the 1,526-dataset corpus that produced #931 that measured ~106 ms a
        // request, i.e. close to three minutes. Folded into `finalize-verify`
        // it would hold that phase at `0/N` with `since_progress_s` climbing
        // until it read `stalled`: the same lie this change removes, at a
        // smaller scale. So it is a phase of its own, named like the legacy
        // path's, counted in indices, with the index it is waiting on named.
        self.pr.phase(
            "finalize-refresh",
            desired.plan.datasets.len() as u64 + 1,
            0,
        );
        let mut refresh_failures: Vec<String> = Vec::new();
        for dataset in &desired.plan.datasets {
            let _refreshing = self.pr.file(&dataset.index, 0);
            // #2234: one attempt per index. The file guard names the index
            // on the progress line for as long as this request is in flight.
            if let Err(error) = self.es.refresh_once(&dataset.index) {
                refresh_failures.push(format!("{}: {error:#}", dataset.index));
            }
        }
        {
            let _refreshing = self.pr.file(crate::catalog::CATALOG_INDEX, 0);
            if let Err(error) = self.es.refresh_once(crate::catalog::CATALOG_INDEX) {
                refresh_failures.push(format!("{}: {error:#}", crate::catalog::CATALOG_INDEX));
            }
        }
        if !refresh_failures.is_empty() {
            anyhow::bail!(
                "finalize-refresh failed for {} index(es): {}",
                refresh_failures.len(),
                refresh_failures.join(" | ")
            );
        }
        // The generation-wide barrier reads changed groups back. Every
        // question it asks has the same shape — "how many rows in this index
        // carry one of these content digests" — so it is asked windowed
        // instead of the three serial searches per file this barrier used to
        // issue. That serial shape is #1183's count lane: measured 2.2
        // groups/s on cve-records (402,695 changed groups, ~50 h projected)
        // against an index phase that itself takes hours. Measured on the
        // live crawl node (2026-10-08): the record windows answer in
        // 6–63 ms per 1,024 digests while the index is append-only (the
        // engine's columnar agg fast path), the semantic windows pay the
        // brute path — the fast path does not yet columnarize `exists`
        // (#1260) — at ~16 s per window on a 53k-doc index, and the catalog leg
        // cannot aggregate at all: its version map carries delete events
        // (dataset documents are rewritten by every finalize-catalog),
        // which the fast path refuses, and the brute agg corpus trips the
        // query-memory breaker (786.7 MB vs 512 MB → 429). The catalog
        // therefore fetch-counts its small matched sets instead
        // (`windowed_catalog_counts`, 14–31 ms a window). The phase counts
        // read-back windows and ticks as each one resolves, so the line
        // never sits at `0/N` for the length of the walk (#931's rule).
        //
        // #971: a group wholly equal to its committed self is skipped. That
        // equality is exactly the condition under which `plan_operations`
        // planned no operation — nothing this generation wrote can have moved
        // its live counts, and its read-back was exact at the barrier of the
        // generation that last touched it. Verifying it again every run is
        // what made a one-file change cost O(corpus) queries.
        let base_group_by_id: BTreeMap<&str, &ManifestGroup> = base
            .groups
            .iter()
            .map(|group| (group.group_id.as_str(), group))
            .collect();
        let changed_groups: Vec<&ManifestGroup> = desired
            .groups
            .iter()
            .filter(|group| base_group_by_id.get(group.group_id.as_str()).copied() != Some(*group))
            .collect();
        let desired_ids: std::collections::HashSet<&str> = desired
            .groups
            .iter()
            .map(|group| group.content_id.as_str())
            .collect();
        let deleted_groups: Vec<&ManifestGroup> = base
            .groups
            .iter()
            .filter(|old| !desired_ids.contains(old.content_id.as_str()))
            .collect();

        // What each leg needs, gathered before anything is asked so the phase
        // opens with an honest denominator. Record counts are keyed by index:
        // a changed group asks in every index its datasets touch, a deleted
        // group asks for zero in every index the BASE plan touched — a renamed
        // dataset yields two entries, exactly the two names that must be read.
        // Duplicate digests inside one index's list are harmless: the count map
        // is keyed by digest and each group sums its own lookups.
        let mut record_wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for group in &changed_groups {
            for index in group_indices(group, &desired.plan)? {
                record_wanted
                    .entry(index)
                    .or_default()
                    .push(group.content_id.clone());
            }
        }
        for old in &deleted_groups {
            for index in group_indices(old, &base.plan)? {
                record_wanted
                    .entry(index)
                    .or_default()
                    .push(old.content_id.clone());
            }
        }
        // Semantic counts are keyed by (index, semantic field): a changed
        // group asks once per dataset that carries one.
        let by_slug: HashMap<&str, &crate::state::PlanDataset> = desired
            .plan
            .datasets
            .iter()
            .map(|dataset| (dataset.slug.as_str(), dataset))
            .collect();
        let mut semantic_wanted: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for group in &changed_groups {
            for slug in &group.dataset_slugs {
                let dataset = by_slug
                    .get(slug.as_str())
                    .copied()
                    .with_context(|| format!("group references absent dataset {slug}"))?;
                if let Some(field) = &dataset.semantic_field {
                    semantic_wanted
                        .entry((dataset.index.clone(), field.clone()))
                        .or_default()
                        .push(group.content_id.clone());
                }
            }
        }
        let windows = |values: usize| values.div_ceil(Self::VERIFY_WINDOW) as u64;
        let mut readbacks = 0u64;
        for values in record_wanted.values() {
            readbacks += windows(values.len());
        }
        for values in semantic_wanted.values() {
            // One exists preflight per field, then its digest windows — issued
            // when the field matches anything, ticked and skipped when it
            // matches nothing (#1267).
            readbacks += 1 + windows(values.len());
        }
        let catalog_wanted: Vec<String> = changed_groups
            .iter()
            .map(|group| group.content_id.clone())
            .collect();
        if !catalog_wanted.is_empty() {
            readbacks += windows(catalog_wanted.len());
        }
        self.pr.phase("finalize-verify", readbacks, 0);

        // #1267 operational: a generation with no semantic field still times
        // one record window, so an old server is named before the walk. The
        // semantic leg's probe is the exists preflight below — a per-digest
        // `term` + `exists` sample is the query this change exists to stop
        // issuing when the field matches nothing. `watch_file` names the
        // index and does not credit an item: the probe is not one of
        // `readbacks`.
        if semantic_wanted.is_empty() {
            if let Some((index, value)) = record_wanted
                .iter()
                .find_map(|(index, values)| values.first().map(|value| (index.as_str(), value)))
            {
                let _probing = self.pr.watch_file(index, 0);
                Self::probe_server_generation(
                    self.pr,
                    readbacks,
                    Self::SERVER_PROBE_SLOW,
                    || {
                        let started = std::time::Instant::now();
                        self.windowed_value_counts(
                            index,
                            "ax_file",
                            std::slice::from_ref(value),
                            &[],
                            false,
                        )?;
                        Ok(started.elapsed())
                    },
                    || self.server_xerj_version(),
                );
            }
        }

        let mut record_counts: HashMap<String, HashMap<String, u64>> = HashMap::new();
        for (index, values) in &record_wanted {
            let _reading = self.pr.watch_file(index, 0);
            let counts = self.windowed_value_counts(index, "ax_file", values, &[], true)?;
            record_counts.insert(index.clone(), counts);
        }
        let mut semantic_counts: HashMap<(String, String), HashMap<String, u64>> = HashMap::new();
        let mut first_semantic = true;
        for ((index, field), values) in &semantic_wanted {
            let _reading = self.pr.watch_file(index, 0);
            let probe = first_semantic;
            first_semantic = false;
            let total = self.semantic_exists_total(index, field, probe, windows(values.len()))?;
            credit_verify_items(self.pr, 1);
            let plan = plan_exists_probes(values.len(), Self::VERIFY_WINDOW, total);
            if plan.skip_windows > 0 {
                self.pr.note(&format!(
                    "autoindex: semantic field {field} on {index} matches no documents; skipped \
                     {} verify window(s) (#1267)",
                    plan.skip_windows
                ));
                credit_verify_items(self.pr, plan.skip_windows);
                semantic_counts.insert(
                    (index.clone(), field.clone()),
                    values.iter().cloned().map(|value| (value, 0)).collect(),
                );
            } else {
                let exists = serde_json::json!({"exists": {"field": field}});
                let counts =
                    self.windowed_value_counts(index, "ax_file", values, &[exists], true)?;
                semantic_counts.insert((index.clone(), field.clone()), counts);
            }
        }
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
        // and that is what it now asks. For the same #971 reason as the
        // record counts above, only changed groups ask — a kept group's
        // canonical/alias documents intentionally keep the run_id of the
        // generation that last wrote them.
        let mut catalog_counts = if catalog_wanted.is_empty() {
            HashMap::new()
        } else {
            let _reading = self.pr.watch_file(crate::catalog::CATALOG_INDEX, 0);
            self.windowed_catalog_counts(&catalog_wanted, &snapshot.tx_id)?
        };

        // The checks are in-memory lookups in the same per-group order the
        // serial walk used, so the first disagreement still names the same
        // group it always did. A key missing from a map here is a bookkeeping
        // bug in the gathering above, not a count of zero — the catalog check
        // makes it fail loud either way.
        for group in changed_groups {
            let mut records = 0u64;
            for index in group_indices(group, &desired.plan)? {
                records += record_counts[&index][&group.content_id];
            }
            anyhow::ensure!(
                records == group.expected_records,
                "live record count disagrees with desired group {}",
                group.group_id
            );
            let mut semantic = 0u64;
            for slug in &group.dataset_slugs {
                let dataset = by_slug
                    .get(slug.as_str())
                    .copied()
                    .with_context(|| format!("group references absent dataset {slug}"))?;
                if let Some(field) = &dataset.semantic_field {
                    semantic +=
                        semantic_counts[&(dataset.index.clone(), field.clone())][&group.content_id];
                }
            }
            anyhow::ensure!(
                semantic == group.expected_passages && semantic == group.expected_vectors,
                "live semantic count disagrees with desired group {}: \
                 live_semantic_records={}, expected_passages={}, expected_vectors={}, datasets={:?}",
                group.group_id,
                semantic,
                group.expected_passages,
                group.expected_vectors,
                group.dataset_slugs
            );
            // #2314: the equality used to be one shot. A folder of identical
            // empty files published the canonical doc and its aliases, then
            // lost the race with catalog visibility and aborted; the same
            // command retried clean. Recount after a refresh, the same bound
            // as the short read-back walk, before calling it a disagreement.
            let expected = 1 + group.aliases.len() as u64;
            let mut observed = catalog_counts.get(&group.content_id).copied().unwrap_or(0);
            let mut attempt = 1usize;
            while observed != expected && attempt < Self::READBACK_WALK_ATTEMPTS {
                self.pr.note(&format!(
                    "autoindex: catalog canonical/alias count for {} saw {observed} of \
                     {expected} document(s) — refreshing and recounting (attempt {attempt} of {})",
                    group.group_id,
                    Self::READBACK_WALK_ATTEMPTS
                ));
                self.es.refresh_once(crate::catalog::CATALOG_INDEX)?;
                catalog_counts = self.windowed_catalog_counts(&catalog_wanted, &snapshot.tx_id)?;
                observed = catalog_counts.get(&group.content_id).copied().unwrap_or(0);
                attempt += 1;
            }
            anyhow::ensure!(
                observed == expected,
                "catalog canonical/alias count disagrees with desired group {}: \
                 observed {observed}, expected {expected}",
                group.group_id
            );
        }
        for old in &deleted_groups {
            let mut live = 0u64;
            for index in group_indices(old, &base.plan)? {
                live += record_counts[&index][&old.content_id];
            }
            anyhow::ensure!(
                live == 0,
                "replaced or deleted content {} remains live",
                old.content_id
            );
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

    // #931: what is left to apply, in the units the progress surface reports.
    // An operation already `Committed` by an earlier attempt is not work this
    // run will do, so it is in neither the numerator nor the denominator — a
    // resumed replay starts at 0% of what REMAINS, not at a percentage that
    // credits this invocation with a previous one's writes.
    let committed = |journal: &Journal, operation: &SyncOperation| {
        journal
            .pending_sync
            .as_ref()
            .and_then(|sync| sync.operation_states.get(&operation.operation_id))
            == Some(&SyncOperationState::Committed)
    };
    let desired_by_group: HashMap<&str, &ManifestGroup> = pending
        .desired
        .groups
        .iter()
        .map(|group| (group.group_id.as_str(), group))
        .collect();
    let base_by_group: HashMap<&str, &ManifestGroup> = base
        .groups
        .iter()
        .map(|group| (group.group_id.as_str(), group))
        .collect();
    let prepared_bytes: HashMap<&str, u64> = snapshot
        .files
        .iter()
        .filter_map(|file| {
            file.prepared
                .as_ref()
                .map(|artifact| (file.content_id.as_str(), artifact.bytes))
        })
        .collect();
    // Only an upsert sends the sealed NDJSON; a delete or a metadata rewrite
    // moves no prepared bytes, so it counts as an item and as zero bytes.
    let operation_bytes = |operation: &SyncOperation| -> u64 {
        if operation.kind != crate::sync::SyncOperationKind::Upsert {
            return 0;
        }
        desired_by_group
            .get(operation.group_id.as_str())
            .and_then(|group| prepared_bytes.get(group.content_id.as_str()))
            .copied()
            .unwrap_or(0)
    };
    let remaining: Vec<&SyncOperation> = pending
        .operations
        .iter()
        .filter(|operation| !committed(journal, operation))
        .collect();
    // #931: what the loop reports per operation — its source path and its
    // sealed bytes — resolved once here, so the serial loop and the windowed
    // scheduler (#933) name and measure every operation identically.
    let items: Vec<ReplayItem> = remaining
        .iter()
        .map(|operation| ReplayItem {
            operation,
            rel: desired_by_group
                .get(operation.group_id.as_str())
                .or_else(|| base_by_group.get(operation.group_id.as_str()))
                .map_or(operation.group_id.as_str(), |group| {
                    group.canonical.rel.as_str()
                })
                .to_owned(),
            bytes: operation_bytes(operation),
        })
        .collect();
    backend.replay_begins(
        items.len() as u64,
        items.iter().map(|item| item.bytes).sum(),
    );
    backend.replay_operations(&items, &base, &pending.desired, &snapshot, journal)?;
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

/// [`replay_pending_operations`] for a run with a progress surface: when the
/// server's back-pressure outlasts the client's patience, say THAT on the way
/// out instead of `reason=aborted`.
///
/// #944 made a throttled server slow the run down rather than end it; this is
/// the one case left where it still ends — the node accepted nothing for the
/// whole patience (the node behind #950 never accepts again until it is
/// restarted). Such a stop is not a crash and not a bad corpus: every applied
/// operation is journaled, the same command resumes, and the only thing the
/// reader needs is how much is left and what it was doing. So the terminal
/// line names the cause and the two counts, and a note names the operation in
/// flight. The exit code stays 1: `3` is published as "a finished run, retry
/// nothing", and this run did not finish — an agent told to retry nothing
/// would report a half-applied generation as searchable.
pub fn replay_pending_operations_reporting(
    state_dir: &Path,
    journal: &mut Journal,
    backend: &mut impl SyncOperationBackend,
    pr: &crate::progress::Progress,
) -> Result<()> {
    let result = replay_pending_operations(state_dir, journal, backend);
    let Err(error) = &result else {
        return result;
    };
    if crate::esclient::BackpressureExhausted::in_chain(error).is_none() {
        return result;
    }
    let Some(pending) = journal.pending_sync.as_ref() else {
        return result;
    };
    let committed = |operation: &SyncOperation| {
        pending.operation_states.get(&operation.operation_id)
            == Some(&SyncOperationState::Committed)
    };
    let applied = pending.operations.iter().filter(|op| committed(op)).count() as u64;
    let remaining = pending.operations.len() as u64 - applied;
    // The operation in flight is the one journaled Started and not Committed.
    let in_flight = pending
        .operations
        .iter()
        .find(|operation| {
            pending.operation_states.get(&operation.operation_id)
                == Some(&SyncOperationState::Started)
        })
        .and_then(|operation| {
            pending
                .desired
                .groups
                .iter()
                .find(|group| group.group_id == operation.group_id)
                .map(|group| group.canonical.rel.clone())
        });
    pr.note(&match (remaining, in_flight) {
        (0, _) => format!(
            "autoindex: stopped by server back-pressure after all {applied} operation(s) were \
             applied; the catalog write and the read-back barrier are what remain — the same \
             command resumes there"
        ),
        (_, Some(rel)) => format!(
            "autoindex: stopped by server back-pressure while applying {rel}: {applied} \
             operation(s) are journaled applied, {remaining} are not (this one first) — the \
             same command resumes from here once the node accepts writes again"
        ),
        (_, None) => format!(
            "autoindex: stopped by server back-pressure: {applied} operation(s) are journaled \
             applied, {remaining} are not — the same command resumes from here once the node \
             accepts writes again"
        ),
    });
    pr.finish(
        false,
        1,
        "server-backpressure",
        &[("ops_applied", applied), ("ops_remaining", remaining)],
    );
    result
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
        Err(error) => {
            return Err(anyhow::Error::from(error)
                .context(format!("list snapshot directory {}", root.display())));
        }
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
            std::fs::rename(entry.path(), &tombstone).with_context(|| {
                format!(
                    "rename snapshot {} to tombstone {}",
                    entry.path().display(),
                    tombstone.display()
                )
            })?;
            sync_dir(&root)?;
            #[cfg(test)]
            if GC_FAIL_AFTER_RENAME.swap(false, std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("injected snapshot GC crash after durable tombstone rename");
            }
            tombstone
        };
        std::fs::remove_dir_all(&tombstone)
            .with_context(|| format!("remove snapshot tombstone {}", tombstone.display()))?;
        sync_dir(&root)?;
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

pub(crate) fn open_committed_snapshot(
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
    check_bulk_outcome(es, es.bulk(body)?)
}

/// [`checked_bulk`] for a body nobody windowed — the catalog projection, which
/// holds one document per file and so grows with the corpus (#955). Sent as
/// requests of at most `bulk_bytes` bytes, in order.
fn checked_bulk_windowed(es: &crate::esclient::Es, body: Vec<u8>, bulk_bytes: usize) -> Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    check_bulk_outcome(es, es.bulk_windowed(body, bulk_bytes)?)
}

fn check_bulk_outcome(
    es: &crate::esclient::Es,
    outcome: crate::esclient::BulkOutcome,
) -> Result<()> {
    // `Es::bulk` has already re-sent per-item 429s, and only those, for the
    // whole of its patience (`THROTTLE_PATIENCE`, #944/#949, shortened to
    // `ZERO_PROGRESS_PATIENCE` when a parent memory breaker accepts nothing,
    // #2238). What is left is either a server condition that did not clear —
    // fatal, and resumable, because a sealed operation is journaled applied
    // only after this returns — or a record the server refused outright.
    if outcome.throttled_out > 0 {
        let patience = outcome
            .exhausted_after
            .unwrap_or_else(|| es.throttle_patience());
        let reason = outcome
            .first_server_error
            .unwrap_or_else(|| "unknown server error".into());
        return Err(anyhow::Error::new(crate::esclient::backpressure_exhausted(
            outcome.server_errors,
            patience,
            &reason,
        )));
    }
    if outcome.server_errors > 0 {
        let reason = outcome
            .first_server_error
            .as_deref()
            .map(crate::esclient::memory_cap_hint)
            .unwrap_or_else(|| "unknown server error".into());
        anyhow::bail!(
            "the server failed {} of a prepared bulk's items: {reason}. Nothing from this bulk was \
             journaled applied; fix the reported server condition and rerun the same command — \
             the run resumes from its last committed operation",
            outcome.server_errors,
        );
    }
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
        &crate::progress::Progress::silent(),
        None,
        "",
        // The source-snapshot wrapper has no plan and therefore never runs
        // `prepare_artifact`; a labeler would have nothing to vote on.
        None,
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
    create_prepared_snapshot_reporting(
        state_dir,
        tx_id,
        inventory,
        plan,
        preparation_contract_digest,
        hard_budget_bytes,
        &crate::progress::Progress::silent(),
        // Test-only wrapper: these snapshots are never handed back as a reuse
        // source, so the chunker identity they seal is a fixed label.
        None,
        "prepared-records-v1",
        // Test-only wrapper: no `--label` in these snapshots' contracts.
        None,
    )
}

/// [`create_prepared_snapshot`], reporting through the run's progress surface.
///
/// Sealing is the generated path's extraction pass — every file is verified,
/// copied, verified again and parsed into sealed NDJSON, one at a time — and on
/// the corpus that produced #931 it is minutes of work. It used to run with no
/// phase of its own, so the stream kept describing the `scan` that had already
/// finished. It is now the `snapshot` phase, with a byte denominator.
///
/// #971: `reuse` is the prior *committed* generation's snapshot. A file whose
/// content digest and preparation identity both match its entry there is
/// hardlinked from it instead of being re-copied and re-extracted, so a
/// one-file change seals O(changed) new bytes. `chunker_identity` is the run's
/// `prepared_records_identity` and becomes part of each file's sealed
/// preparation identity.
#[allow(clippy::too_many_arguments)]
pub fn create_prepared_snapshot_reporting(
    state_dir: &Path,
    tx_id: &str,
    inventory: &Inventory,
    plan: &Plan,
    preparation_contract_digest: &str,
    hard_budget_bytes: u64,
    pr: &crate::progress::Progress,
    reuse: Option<&SourceSnapshot>,
    chunker_identity: &str,
    labeler: Option<&crate::label::Labeler>,
) -> Result<SourceSnapshot> {
    create_snapshot_inner(
        state_dir,
        tx_id,
        inventory,
        Some(plan),
        preparation_contract_digest,
        hard_budget_bytes,
        pr,
        reuse,
        chunker_identity,
        labeler,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_snapshot_inner(
    state_dir: &Path,
    tx_id: &str,
    inventory: &Inventory,
    plan: Option<&Plan>,
    preparation_contract_digest: &str,
    hard_budget_bytes: u64,
    pr: &crate::progress::Progress,
    reuse: Option<&SourceSnapshot>,
    chunker_identity: &str,
    labeler: Option<&crate::label::Labeler>,
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
    // Built once per snapshot: per-file preparation used to rebuild the
    // dataset/coercion maps, rescan every duplicate alias, and re-serialize
    // each assigned dataset for every file — O(files × (aliases + datasets)).
    let lookup = plan.map(PreparationLookup::new).transpose()?;
    // #971: the reuse index. Sealing is O(corpus) because every file is
    // re-copied, re-verified and re-extracted per generation — a one-file
    // change in a 10,000-file corpus sealed 10,000 blobs twice over (verify,
    // copy+fsync, verify, extract). A file whose content digest and size match
    // the prior committed snapshot's entry is hardlinked from it instead. A
    // hardlink, not a symlink and not a copy: `gc_snapshots` removes the prior
    // snapshot's directory entries after this generation commits, and a
    // hardlinked inode survives that while a symlink would dangle — the same
    // manifest-referenced-GC property tantivy's `list_segment_files` relies on
    // to share unchanged segment files across commits. The blob needs no
    // re-verify either: `open_snapshot` (which the caller ran to hand us a
    // digest-checked `reuse`) proved the prior blob matches its manifest, and
    // this inventory's digest for the file is the same value — so the sealed
    // bytes equal the scan-time bytes by construction.
    let reuse_root = reuse.map(|prior| state_dir.join("sync-snapshots").join(&prior.tx_id));
    let reuse_by_content: HashMap<&str, &SnapshotFile> = reuse
        .map(|prior| {
            prior
                .files
                .iter()
                .map(|file| (file.content_id.as_str(), file))
                .collect()
        })
        .unwrap_or_default();
    let mut files = Vec::with_capacity(inventory.files.len());
    // Entered only when there is work to report: a retry that reuses a verified
    // final snapshot returned above and never claims a phase it did not run.
    pr.phase("snapshot", inventory.files.len() as u64, source_bytes);
    // #2314: a test grows one source in the window after the hash and before
    // this seal. Production runs leave the hook empty.
    snapshot_source_hook(state_dir);
    for (ordinal, ((source, content_id), content_digest)) in inventory
        .files
        .iter()
        .zip(&inventory.keys)
        .zip(&inventory.digests)
        .enumerate()
    {
        // Counted done on every exit from this iteration, including the `?`s:
        // the phase measures files drained, and the guard names the file a
        // quiet tail is inside.
        let _sealing = pr.file(&source.rel, source.size);
        let relative_blob = format!("blobs/{ordinal:08}");
        let destination = staging.join(&relative_blob);
        // A junk/skipped file has no `plan.files` entry *by construction* — it
        // lives in `plan.junk_files` and is never indexed — so demanding one
        // here made `--no-graph` fail outright on any folder holding a single
        // unreadable, empty or unrecognised file. Its bytes still belong in the
        // sealed snapshot (a resume replays from the snapshot, not from the
        // mutable tree, and the inventory it is verified against lists the
        // file), but there is nothing to prepare for it: `prepared: None`.
        let assignment = plan.and_then(|plan| plan.files.get(content_id.as_str()));
        let prepared_identity = match (lookup.as_ref(), assignment) {
            (Some(lookup), Some(assignment)) => Some(prepared_artifact_identity(
                chunker_identity,
                lookup,
                content_id.as_str(),
                assignment,
            )?),
            _ => None,
        };
        let reusable = reuse_by_content
            .get(content_id.as_str())
            .copied()
            .filter(|prior| {
                prior.content_digest == *content_digest && prior.content_size == source.size
            });
        match reusable {
            Some(prior) => {
                let prior_blob = reuse_root
                    .as_ref()
                    .expect("reuse root exists whenever reuse_by_content is non-empty")
                    .join(&prior.relative_blob);
                std::fs::hard_link(&prior_blob, &destination).with_context(|| {
                    format!(
                        "hardlink unchanged snapshot blob {} -> {}",
                        prior_blob.display(),
                        destination.display()
                    )
                })?;
                budget.charge(source.size as usize)?;
                let prepared = match &prior.prepared {
                    Some(prior_artifact)
                        if prior.prepared_identity.as_deref().is_some_and(|identity| {
                            Some(identity) == prepared_identity.as_deref()
                        }) =>
                    {
                        // Extraction inputs are byte-identical to the prior
                        // generation's, so the sealed NDJSON is too (the only
                        // run-varying field it carries is `ax_run`, which the
                        // prior generation stamped — and the live records this
                        // artifact replays still carry that stamp, so reusing
                        // it is consistent with the ax_run provenance rule:
                        // only an Upsert, which always re-prepares, may move
                        // it). Link it under this snapshot's ordinal and keep
                        // the verified artifact metadata verbatim.
                        let relative_ndjson = format!("prepared/{ordinal:08}.ndjson");
                        let prior_path = reuse_root
                            .as_ref()
                            .expect("reuse root exists whenever reuse_by_content is non-empty")
                            .join(&prior_artifact.relative_ndjson);
                        std::fs::hard_link(&prior_path, staging.join(&relative_ndjson))
                            .with_context(|| {
                                format!(
                                    "hardlink unchanged prepared artifact {}",
                                    prior_path.display()
                                )
                            })?;
                        budget.charge(prior_artifact.bytes as usize)?;
                        Some(PreparedArtifact {
                            relative_ndjson,
                            ..prior_artifact.clone()
                        })
                    }
                    _ => match assignment {
                        Some(assignment) => Some(prepare_artifact(
                            &staging,
                            ordinal,
                            tx_id,
                            source,
                            content_id,
                            &destination,
                            assignment,
                            lookup.as_ref().expect("assignment implies a plan"),
                            &mut budget,
                            labeler,
                        )?),
                        None => None,
                    },
                };
                files.push(SnapshotFile {
                    content_id: content_id.clone(),
                    content_digest: content_digest.clone(),
                    content_size: source.size,
                    relative_blob,
                    prepared,
                    prepared_identity,
                });
            }
            None => {
                // #2314: a file that changes size (or disappears) between the
                // hash and this copy used to abort the generation on the `?`.
                // The error names the live source so the caller can drop that
                // one file and seal the rest.
                verify_live_source(source, content_digest)?;
                if let Err(error) = copy_synced(&source.path, &destination, &mut budget) {
                    if verify_live_source(source, content_digest).is_err() {
                        return Err(source_changed(
                            &source.path,
                            format!(
                                "{} changed while it was being copied into the snapshot ({error:#})",
                                source.path.display()
                            ),
                        ));
                    }
                    return Err(error);
                }
                verify_sealed_blob(source, &destination, content_digest)?;
                #[cfg(test)]
                apply_post_seal_source_replacement(&source.path)?;
                let prepared = match assignment {
                    Some(assignment) => Some(prepare_artifact(
                        &staging,
                        ordinal,
                        tx_id,
                        source,
                        content_id,
                        &destination,
                        assignment,
                        lookup.as_ref().expect("assignment implies a plan"),
                        &mut budget,
                        labeler,
                    )?),
                    None => None,
                };
                files.push(SnapshotFile {
                    content_id: content_id.clone(),
                    content_digest: content_digest.clone(),
                    content_size: source.size,
                    relative_blob,
                    prepared,
                    prepared_identity,
                });
            }
        }
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

/// Per-snapshot index over the plan, built once by `create_snapshot_inner`
/// so preparing F files costs O(F + aliases + datasets) instead of
/// O(F × (aliases + datasets)). Duplicate slugs resolve last-wins, exactly
/// as the per-file `collect()`/`insert` loops it replaces did, so
/// `prepared_artifact_identity` stays byte-identical (it is a persisted
/// reuse key).
struct PreparationLookup<'a> {
    datasets: HashMap<&'a str, &'a crate::state::PlanDataset>,
    dataset_values: HashMap<&'a str, Value>,
    coercions: HashMap<&'a str, HashMap<String, crate::coerce::Coerce>>,
    /// `file_key` -> sorted `(rel, path_id)` of its duplicate aliases.
    aliases_by_key: HashMap<&'a str, Vec<(&'a str, &'a str)>>,
}

impl<'a> PreparationLookup<'a> {
    fn new(plan: &'a Plan) -> Result<Self> {
        let mut datasets = HashMap::with_capacity(plan.datasets.len());
        let mut dataset_values = HashMap::with_capacity(plan.datasets.len());
        let mut coercions = HashMap::with_capacity(plan.datasets.len());
        for dataset in &plan.datasets {
            let slug = dataset.slug.as_str();
            datasets.insert(slug, dataset);
            dataset_values.insert(slug, serde_json::to_value(dataset)?);
            coercions.insert(slug, crate::coerce::plan_from_specs(&dataset.specs));
        }
        let mut aliases_by_key: HashMap<&'a str, Vec<(&'a str, &'a str)>> = HashMap::new();
        for alias in &plan.duplicate_files {
            aliases_by_key
                .entry(alias.file_key.as_str())
                .or_default()
                .push((alias.rel.as_str(), alias.path_id.as_str()));
        }
        for aliases in aliases_by_key.values_mut() {
            aliases.sort_unstable();
        }
        Ok(Self {
            datasets,
            dataset_values,
            coercions,
            aliases_by_key,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_artifact(
    staging: &Path,
    ordinal: usize,
    generation_identity: &str,
    source: &crate::walk::FileEntry,
    content_id: &str,
    snapshot_blob: &Path,
    assignment: &crate::state::FileAssignment,
    lookup: &PreparationLookup<'_>,
    budget: &mut PayloadBudget,
    // #1062: `--label` runs each record's payload through the node's
    // `/_decide` HERE, before the record is sealed, so the labels become part
    // of the durable artifact — replays and hardlink reuse carry them without
    // a second vote. The question-set bytes are in the run's
    // `chunker_identity` (`lib.rs::prepared_records_identity`), which is what
    // makes a changed set re-prepare instead of hardlinking unlabelled (or
    // differently-labelled) NDJSON. `None` — the default, and every caller
    // without `--label` — seals unlabelled exactly as before.
    labeler: Option<&crate::label::Labeler>,
) -> Result<PreparedArtifact> {
    let datasets = &lookup.datasets;
    let coercions = &lookup.coercions;
    let mut paths = vec![assignment.rel.clone()];
    if let Some(aliases) = lookup.aliases_by_key.get(content_id) {
        paths.extend(aliases.iter().map(|(rel, _)| (*rel).to_owned()));
    }
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
        // #1062: the decide vote runs after coercion and provenance stamping
        // and before the record is written, on the same `fields` map that is
        // about to be sealed — the question templates see the record exactly
        // as the index will. A FAILED vote fails the seal (sink_error), which
        // fails the run: sealing the record unlabelled would hand the index a
        // document the operator believes was judged. An abstention is an
        // answer and stamps `label: null` with the raw p (see `label.rs`).
        if let Some(labeler) = labeler {
            match labeler.label_fields(&fields) {
                Ok(pairs) => {
                    for (name, value) in pairs {
                        fields.insert(name, value);
                    }
                }
                Err(error) => {
                    sink_error = Some(error.context(format!(
                        "--label {} (record {})",
                        source.rel, record.locator
                    )));
                    return false;
                }
            }
        }
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
        truncated: stats.truncated,
        records_by_dataset,
        bytes,
        digest: stream_digest(&path, "axp1")?,
    })
}

/// #971: the identity of every input `prepare_artifact` consumes for one file.
///
/// Equal identity + equal content digest (checked separately against the
/// inventory) means the sealed NDJSON this run would produce is byte-identical
/// to the prior generation's, so the prior artifact is hardlinked instead of
/// re-extracted. The enumeration deliberately mirrors what `prepare_artifact`
/// reads: the chunker-level settings and their version label, the document-id
/// scheme, the file's whole assignment (rel drives `ax_path`, the sniff name
/// and `ax_format`; `assignments` drives record routing; `as_document` switches
/// the extractor), the alias paths folded into `ax_paths`, and the complete
/// `PlanDataset` records for every dataset a record can land in (index name,
/// specs/coercions, semantic field). **If `prepare_artifact` ever grows an
/// input, it must be added here** — the chunker and document-id labels are the
/// existing contract-digest versioning points and change with any extractor
/// behaviour change, which is what makes this safe across builds.
fn prepared_artifact_identity(
    chunker_identity: &str,
    lookup: &PreparationLookup<'_>,
    content_id: &str,
    assignment: &crate::state::FileAssignment,
) -> Result<String> {
    let aliases: &[(&str, &str)] = lookup
        .aliases_by_key
        .get(content_id)
        .map_or(&[], Vec::as_slice);
    let mut datasets: BTreeMap<&str, &Value> = BTreeMap::new();
    for (_, slug) in &assignment.assignments {
        if let Some(value) = lookup.dataset_values.get(slug.as_str()) {
            datasets.insert(slug.as_str(), value);
        }
    }
    let encoded = serde_json::to_vec(&serde_json::json!({
        "chunker_identity": chunker_identity,
        "document_ids": crate::DOCUMENT_IDS_IDENTITY,
        "assignment": serde_json::to_value(assignment)?,
        "aliases": aliases,
        "datasets": datasets,
    }))?;
    Ok(format!(
        "axfi1-{:032x}",
        xxhash_rust::xxh3::xxh3_128(&encoded)
    ))
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

/// fsync a directory so the entries created or renamed inside it survive
/// power loss.
///
/// #482: this used to be `File::open(path)?.sync_all()?` — a Unix-only idiom.
/// On Windows `File::open` on a *directory* fails with `ERROR_ACCESS_DENIED`
/// (os error 5) on **every** call, because `std` cannot pass
/// `FILE_FLAG_BACKUP_SEMANTICS`, so sealing a source snapshot aborted the
/// whole `--no-graph` run before a single document was indexed. Exactly the
/// same mistake had already been found and fixed once in
/// [`xerj_common::fsio::fsync_dir`]; route through it instead of re-deriving
/// it here. Its Windows body is a documented no-op that makes no durability
/// claim — which costs Windows nothing, because the code it replaces flushed
/// nothing either: it returned `Err` and killed the run.
fn sync_dir(path: &Path) -> Result<()> {
    xerj_common::fsio::fsync_dir(path)
        .with_context(|| format!("fsync snapshot directory {}", path.display()))
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

/// A live source no longer matches the bytes this generation hashed.
///
/// Stat failures are included: a file deleted between the hash and the seal
/// is the same skip, not a reason to abort every other file.
fn verify_live_source(source: &crate::walk::FileEntry, digest: &str) -> Result<()> {
    match crate::content::verify(&source.path, source.size, digest) {
        Ok(()) => Ok(()),
        Err(error) => Err(source_changed(&source.path, format!("{error:#}"))),
    }
}

/// The staged blob does not match the hashed source. A size or content miss
/// is reported against the live path, which is the file the caller can drop.
fn verify_sealed_blob(source: &crate::walk::FileEntry, sealed: &Path, digest: &str) -> Result<()> {
    match crate::content::verify(sealed, source.size, digest) {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .downcast_ref::<crate::content::SourceChanged>()
                .is_some() =>
        {
            Err(source_changed(&source.path, format!("{error:#}")))
        }
        Err(error) => Err(error),
    }
}

fn source_changed(path: &Path, detail: String) -> anyhow::Error {
    crate::content::SourceChanged {
        path: path.to_path_buf(),
        detail,
        locked: false,
    }
    .into()
}

/// Test-only: run once at the start of a snapshot seal, keyed by state dir
/// so a parallel test cannot grow another run's files (#2314).
#[cfg(test)]
type SnapshotSourceHook = (std::path::PathBuf, Box<dyn FnOnce() + Send>);
#[cfg(test)]
static SNAPSHOT_SOURCE_HOOK: std::sync::Mutex<Option<SnapshotSourceHook>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn arm_snapshot_source_hook(state_dir: &Path, action: impl FnOnce() + Send + 'static) {
    *SNAPSHOT_SOURCE_HOOK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) =
        Some((state_dir.to_path_buf(), Box::new(action)));
}

#[cfg(test)]
fn snapshot_source_hook(state_dir: &Path) {
    let action = {
        let mut armed = SNAPSHOT_SOURCE_HOOK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match armed.as_ref() {
            Some((dir, _)) if dir == state_dir => armed.take().map(|(_, action)| action),
            _ => None,
        }
    };
    if let Some(action) = action {
        action();
    }
}

#[cfg(not(test))]
fn snapshot_source_hook(_state_dir: &Path) {}

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
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

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
                text_analyzer: None,
                scope: None,
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

    /// `prepared_artifact_identity` is a persisted reuse key: the
    /// once-per-snapshot `PreparationLookup` must hash exactly what the old
    /// per-file plan scan hashed, including unordered aliases, aliases of other
    /// files, a duplicate slug (last wins) and an assigned slug with no dataset.
    #[test]
    fn preparation_lookup_identity_matches_the_per_file_plan_scan() {
        fn legacy_identity(
            chunker_identity: &str,
            plan: &Plan,
            content_id: &str,
            assignment: &FileAssignment,
        ) -> String {
            let mut aliases: Vec<(&str, &str)> = plan
                .duplicate_files
                .iter()
                .filter(|alias| alias.file_key == content_id)
                .map(|alias| (alias.rel.as_str(), alias.path_id.as_str()))
                .collect();
            aliases.sort_unstable();
            let slugs: BTreeSet<&str> = assignment
                .assignments
                .iter()
                .map(|(_, slug)| slug.as_str())
                .collect();
            let mut datasets: BTreeMap<&str, Value> = BTreeMap::new();
            for dataset in plan
                .datasets
                .iter()
                .filter(|dataset| slugs.contains(&dataset.slug.as_str()))
            {
                datasets.insert(
                    dataset.slug.as_str(),
                    serde_json::to_value(dataset).unwrap(),
                );
            }
            let encoded = serde_json::to_vec(&serde_json::json!({
                "chunker_identity": chunker_identity,
                "document_ids": crate::DOCUMENT_IDS_IDENTITY,
                "assignment": serde_json::to_value(assignment).unwrap(),
                "aliases": aliases,
                "datasets": datasets,
            }))
            .unwrap();
            format!("axfi1-{:032x}", xxhash_rust::xxh3::xxh3_128(&encoded))
        }
        let dataset = |slug: &str, index: &str| PlanDataset {
            slug: slug.into(),
            index: index.into(),
            family: "text".into(),
            group: None,
            specs: vec![],
            time_field: None,
            semantic_field: None,
            text_analyzer: None,
            scope: None,
            sampled_records: 1,
            file_count: 1,
        };
        let alias = |file_key: &str, rel: &str| crate::state::DuplicateFile {
            file_key: file_key.into(),
            rel: rel.into(),
            path_id: format!("id-{rel}"),
            is_symlink: Some(false),
            duplicate_of: "canon".into(),
            bytes: 1,
        };
        let assignment = FileAssignment {
            rel: "canon".into(),
            path_id: "id-canon".into(),
            is_symlink: Some(false),
            family: "text".into(),
            gzip: false,
            content_digest: None,
            assignments: vec![
                (None, "docs".into()),
                (Some("g".into()), "logs".into()),
                (Some("h".into()), "missing".into()),
            ],
            as_document: false,
        };
        let plan = Plan {
            datasets: vec![
                dataset("docs", "ax-docs"),
                dataset("logs", "ax-logs-old"),
                dataset("other", "ax-other"),
                dataset("logs", "ax-logs"),
            ],
            files: HashMap::from([("k1".to_string(), assignment.clone())]),
            duplicate_files: vec![
                alias("k1", "z/copy"),
                alias("k2", "elsewhere"),
                alias("k1", "a/copy"),
                alias("k1", "m/copy"),
            ],
            ..Plan::default()
        };
        let lookup = PreparationLookup::new(&plan).unwrap();
        for content_id in ["k1", "k2", "k3"] {
            assert_eq!(
                prepared_artifact_identity("chunker-v1", &lookup, content_id, &assignment).unwrap(),
                legacy_identity("chunker-v1", &plan, content_id, &assignment),
                "identity drifted for {content_id}"
            );
        }
    }

    /// #971: like `plan_for` but with an assignment for EVERY file in the
    /// inventory — the shape a real corpus plan has.
    fn plan_for_all(inventory: &Inventory) -> Plan {
        let files: HashMap<String, FileAssignment> = inventory
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                (
                    inventory.keys[index].clone(),
                    FileAssignment {
                        rel: file.rel.clone(),
                        path_id: file.rel_id.clone(),
                        is_symlink: Some(file.is_symlink),
                        family: "text".into(),
                        gzip: false,
                        content_digest: Some(inventory.digests[index].clone()),
                        assignments: vec![(None, "docs".into())],
                        as_document: false,
                    },
                )
            })
            .collect();
        Plan {
            datasets: vec![PlanDataset {
                slug: "docs".into(),
                index: "ax-docs".into(),
                family: "text".into(),
                group: None,
                specs: vec![],
                time_field: None,
                semantic_field: None,
                text_analyzer: None,
                scope: None,
                sampled_records: 1,
                file_count: files.len(),
            }],
            files,
            alias_paths_indexed: false,
            ..Plan::default()
        }
    }

    /// #971 fail-before shape: a one-file change in a three-file corpus must
    /// seal new bytes ONLY for the changed file. Unchanged files' blobs and
    /// prepared artifacts are hardlinked from the prior committed snapshot —
    /// same inode, no copy, no re-extraction, no per-file fsync.
    #[cfg(unix)]
    #[test]
    fn one_changed_file_hardlinks_unchanged_blobs_and_prepared_artifacts() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("a.md", "alpha contents one\n"),
            ("b.md", "bravo contents two\n"),
            ("c.md", "charlie contents three\n"),
        ] {
            std::fs::write(corpus.path().join(name), body).unwrap();
        }
        let first_inventory = inventory(corpus.path());
        let first_plan = plan_for_all(&first_inventory);
        let first = create_prepared_snapshot_reporting(
            state.path(),
            "tx-g1",
            &first_inventory,
            &first_plan,
            "test-preparation-v1",
            u64::MAX,
            &crate::progress::Progress::silent(),
            None,
            "chunker-v1",
            None,
        )
        .unwrap();
        assert_eq!(first.files.len(), 3);
        assert!(first
            .files
            .iter()
            .all(|file| file.prepared.is_some() && file.prepared_identity.is_some()));

        // One file changes; the tree is re-scanned; generation two seals.
        std::fs::write(corpus.path().join("b.md"), "bravo contents CHANGED\n").unwrap();
        let second_inventory = inventory(corpus.path());
        let second_plan = plan_for_all(&second_inventory);
        let changed_key = second_inventory
            .keys
            .iter()
            .zip(&second_inventory.digests)
            .find(|(key, digest)| {
                first
                    .files
                    .iter()
                    .find(|file| file.content_id == **key)
                    .is_none_or(|prior| &prior.content_digest != *digest)
            })
            .map(|(key, _)| key.clone())
            .unwrap();
        let second = create_prepared_snapshot_reporting(
            state.path(),
            "tx-g2",
            &second_inventory,
            &second_plan,
            "test-preparation-v1",
            u64::MAX,
            &crate::progress::Progress::silent(),
            Some(&first),
            "chunker-v1",
            None,
        )
        .unwrap();

        // Content ids are content-derived, so the changed file's id is new in
        // generation two — gen1 holds it under the OLD id. Classify by content
        // identity instead: a gen2 entry whose (content_id, digest) has no
        // gen1 match is the changed file.
        let prior_by_content: HashMap<&str, &SnapshotFile> = first
            .files
            .iter()
            .map(|file| (file.content_id.as_str(), file))
            .collect();
        let fresh: Vec<&SnapshotFile> = second
            .files
            .iter()
            .filter(|file| {
                prior_by_content
                    .get(file.content_id.as_str())
                    .is_none_or(|prior| prior.content_digest != file.content_digest)
            })
            .collect();
        assert_eq!(
            fresh.len(),
            1,
            "exactly the changed file seals fresh bytes: {:?}",
            fresh
                .iter()
                .map(|file| file.content_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(fresh[0].content_id, changed_key);

        let artifact_inode = |snapshot: &SourceSnapshot, relative: &str| {
            std::fs::metadata(
                state
                    .path()
                    .join("sync-snapshots")
                    .join(&snapshot.tx_id)
                    .join(relative),
            )
            .unwrap()
            .ino()
        };
        let prior_blob_inodes: std::collections::HashSet<_> = first
            .files
            .iter()
            .map(|file| artifact_inode(&first, &file.relative_blob))
            .collect();

        for file in &second.files {
            let new_prepared = file.prepared.as_ref().unwrap();
            match prior_by_content
                .get(file.content_id.as_str())
                .filter(|prior| prior.content_digest == file.content_digest)
            {
                None => {
                    // The changed file: everything sealed fresh, under the new
                    // generation's identity.
                    assert!(
                        !prior_blob_inodes.contains(&artifact_inode(&second, &file.relative_blob)),
                        "the changed file's blob must be a fresh inode"
                    );
                    let ndjson = std::fs::read_to_string(
                        state
                            .path()
                            .join("sync-snapshots/tx-g2")
                            .join(&new_prepared.relative_ndjson),
                    )
                    .unwrap();
                    assert!(
                        ndjson.contains("\"ax_run\":\"tx-g2\""),
                        "the changed file's artifact must carry the new generation: {ndjson}"
                    );
                }
                Some(prior) => {
                    assert_eq!(
                        artifact_inode(&first, &prior.relative_blob),
                        artifact_inode(&second, &file.relative_blob),
                        "an unchanged file's blob must be the prior generation's inode"
                    );
                    let prior_prepared = prior.prepared.as_ref().unwrap();
                    assert_eq!(
                        artifact_inode(&first, &prior_prepared.relative_ndjson),
                        artifact_inode(&second, &new_prepared.relative_ndjson),
                        "an unchanged file's prepared artifact must be the prior generation's \
                         inode"
                    );
                    let ndjson = std::fs::read_to_string(
                        state
                            .path()
                            .join("sync-snapshots/tx-g2")
                            .join(&new_prepared.relative_ndjson),
                    )
                    .unwrap();
                    assert!(
                        !ndjson.contains("tx-g2"),
                        "a reused artifact keeps the generation that extracted it: {ndjson}"
                    );
                    assert_eq!(new_prepared.digest, prior_prepared.digest);
                }
            }
        }
        // The sealed snapshot still opens and re-verifies every artifact,
        // hardlinked or not.
        let reopened = open_snapshot(state.path(), "tx-g2").unwrap();
        assert_eq!(reopened, second);
    }

    /// #971: gc removes the prior snapshot's directory entries after a new
    /// generation commits (`gc_snapshots`); the new generation's hardlinked
    /// inodes must survive that, and the sealed snapshot must still verify.
    /// This is the property that made a hardlink the right sharing primitive —
    /// a symlink would dangle here.
    #[test]
    fn reused_blobs_survive_removal_of_the_prior_snapshot_directory() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("a.md", "alpha contents one\n"),
            ("b.md", "bravo contents two\n"),
        ] {
            std::fs::write(corpus.path().join(name), body).unwrap();
        }
        let first_inventory = inventory(corpus.path());
        let first_plan = plan_for_all(&first_inventory);
        let first = create_prepared_snapshot_reporting(
            state.path(),
            "tx-g1",
            &first_inventory,
            &first_plan,
            "test-preparation-v1",
            u64::MAX,
            &crate::progress::Progress::silent(),
            None,
            "chunker-v1",
            None,
        )
        .unwrap();
        std::fs::write(corpus.path().join("b.md"), "bravo contents CHANGED\n").unwrap();
        let second_inventory = inventory(corpus.path());
        let second_plan = plan_for_all(&second_inventory);
        let second = create_prepared_snapshot_reporting(
            state.path(),
            "tx-g2",
            &second_inventory,
            &second_plan,
            "test-preparation-v1",
            u64::MAX,
            &crate::progress::Progress::silent(),
            Some(&first),
            "chunker-v1",
            None,
        )
        .unwrap();

        // What gc does to the prior generation once g2 is authority.
        std::fs::remove_dir_all(state.path().join("sync-snapshots/tx-g1")).unwrap();
        // Every file re-verifies: blobs by content digest, prepared artifacts
        // by size and digest — none of that consults the removed directory.
        let reopened = open_snapshot(state.path(), "tx-g2").unwrap();
        assert_eq!(reopened, second);
        assert_eq!(reopened.files.len(), 2);
    }

    /// #971: a preparation-input change (chunker identity) busts prepared
    /// reuse even when the bytes are identical — the artifact is re-extracted
    /// under the new contract, while the blob itself is still shared.
    #[cfg(unix)]
    #[test]
    fn changed_preparation_identity_reextracts_but_still_shares_the_blob() {
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let corpus = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.md"), "alpha contents one\n").unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for_all(&inventory);
        let first = create_prepared_snapshot_reporting(
            state.path(),
            "tx-g1",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
            &crate::progress::Progress::silent(),
            None,
            "chunker-v1",
            None,
        )
        .unwrap();
        let second = create_prepared_snapshot_reporting(
            state.path(),
            "tx-g2",
            &inventory,
            &plan,
            "test-preparation-v1",
            u64::MAX,
            &crate::progress::Progress::silent(),
            Some(&first),
            "chunker-v2",
            None,
        )
        .unwrap();
        let key = &inventory.keys[0];
        let blob = |snapshot: &SourceSnapshot| {
            state
                .path()
                .join("sync-snapshots")
                .join(&snapshot.tx_id)
                .join(
                    &snapshot
                        .files
                        .iter()
                        .find(|file| file.content_id == *key)
                        .unwrap()
                        .relative_blob,
                )
        };
        let inode = |path: std::path::PathBuf| std::fs::metadata(path).unwrap().ino();
        assert_eq!(
            inode(blob(&first)),
            inode(blob(&second)),
            "identical content still shares the blob"
        );
        // The artifacts live in different snapshot directories under the same
        // ordinal name, so compare inodes, not relative paths.
        let prepared = |snapshot: &SourceSnapshot| {
            state
                .path()
                .join("sync-snapshots")
                .join(&snapshot.tx_id)
                .join(
                    &snapshot
                        .files
                        .iter()
                        .find(|file| file.content_id == *key)
                        .unwrap()
                        .prepared
                        .as_ref()
                        .unwrap()
                        .relative_ndjson,
                )
        };
        assert_ne!(
            inode(prepared(&first)),
            inode(prepared(&second)),
            "a changed preparation identity must re-extract, not hardlink"
        );
        let ndjson = std::fs::read_to_string(
            state
                .path()
                .join("sync-snapshots/tx-g2")
                .join(&second.files[0].prepared.as_ref().unwrap().relative_ndjson),
        )
        .unwrap();
        assert!(
            ndjson.contains("\"ax_run\":\"tx-g2\""),
            "a re-extracted artifact carries the new generation: {ndjson}"
        );
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
            text_analyzer: None,
            scope: None,
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

    /// The `windows:` native identity `stable_path_id` writes for `rel` on a
    /// Windows walk: each UTF-16 unit of the NATIVE path (separator `\`) as
    /// four hex digits. `native_path_id_supported` accepts it on every
    /// platform, so a Windows walk can be reproduced from a macOS/Linux one.
    fn windows_path_id(rel: &str) -> String {
        let mut out = String::from("windows:");
        for unit in rel.replace('/', "\\").encode_utf16() {
            use std::fmt::Write;
            write!(out, "{unit:04x}").unwrap();
        }
        out
    }

    /// #1723: `corpus index` on Windows aborted every generation — even with
    /// `--fresh` — with "plan canonical projection disagrees with manifest
    /// group" once the corpus held byte-identical files at `Pill/x.scss` and
    /// `PillS/x.scss` (any pair whose paths first differ at a separator on
    /// one side and a digit, `A-Z`, or `:;<=>?@[` on the other).
    ///
    /// The inventory picked the canonical copy by `rel`, where the separator
    /// is `/` (0x2F) and sorts first; the manifest ranks by `path_id`, the
    /// native identity, where the separator is `\` (0x5C) and sorts after
    /// those characters. The plan projected one path, the manifest group
    /// another, and `PendingSync::new` refused the generation.
    #[test]
    fn windows_duplicate_canonical_agrees_between_plan_and_manifest() {
        let corpus = tempfile::tempdir().unwrap();
        for dir in ["Pill", "PillS", "Pill1"] {
            std::fs::create_dir_all(corpus.path().join(dir)).unwrap();
            std::fs::write(corpus.path().join(dir).join("x.scss"), ".pill{}\n").unwrap();
        }
        std::fs::write(corpus.path().join("other.txt"), "unrelated").unwrap();
        let mut files = walk::walk(corpus.path(), false).unwrap();
        for file in &mut files {
            file.rel_id = windows_path_id(&file.rel);
        }
        let inventory = crate::content::resolve_reporting(files, &|_| {}).unwrap();
        assert_eq!(inventory.files.len(), 2, "three copies, one content file");
        let canonical = inventory
            .files
            .iter()
            .find(|file| file.rel.ends_with("x.scss"))
            .unwrap();
        assert_eq!(
            canonical.rel, "Pill1/x.scss",
            "the canonical copy is the lowest native identity: `1` (0x31) and \
             `S` (0x53) both sort below `\\` (0x5C)"
        );

        let mut plan = plan_for_all(&inventory);
        plan.duplicate_files = inventory.duplicates.clone();
        let groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        let base = CommittedManifest::genesis().unwrap();
        PendingSync::new(
            "tx-1723".into(),
            &base,
            GenerationManifest {
                generation: 1,
                execution: Some(execution("tx-1723", "digest")),
                plan,
                groups,
            },
        )
        .expect("#1723: plan and manifest must agree on the canonical duplicate");
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
        let (pr, _sink) = crate::progress::Progress::capture(
            crate::progress::Surface::Silent,
            std::time::Duration::from_secs(3600),
        );
        let _backend = EsSyncBackend::new(&es, state.path(), 1, &pr);
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

    /// The concurrency a window of applies actually reached, counted from
    /// inside `apply` — shared with the worker threads through an `Arc`.
    #[derive(Default)]
    struct WindowProbe {
        in_flight: std::sync::atomic::AtomicUsize,
        max_in_flight: std::sync::atomic::AtomicUsize,
    }

    impl WindowProbe {
        fn enter(&self) {
            let entered = self
                .in_flight
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.max_in_flight
                .fetch_max(entered, std::sync::atomic::Ordering::SeqCst);
        }

        fn leave(&self) {
            self.in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }

        fn max_seen(&self) -> usize {
            self.max_in_flight.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// What the scheduling thread saw, in the order it saw it.
    #[derive(Default)]
    struct WindowHooks {
        begun: Vec<usize>,
        applied: Vec<(usize, bool)>,
    }

    impl ReplayHooks<usize> for WindowHooks {
        fn begin(&mut self, item: &usize) -> Result<()> {
            self.begun.push(*item);
            Ok(())
        }

        fn applied(&mut self, item: &usize, outcome: Result<()>) -> Result<()> {
            self.applied.push((*item, outcome.is_ok()));
            outcome
        }
    }

    #[test]
    fn replay_windowed_overlaps_up_to_the_width_and_reports_in_dispatch_order() {
        let items: Vec<usize> = (0..12).collect();
        let probe = std::sync::Arc::new(WindowProbe::default());
        let apply_probe = std::sync::Arc::clone(&probe);
        let apply = move |_item: &usize| {
            apply_probe.enter();
            // Long enough that the whole first window is inside `apply` at
            // once; short enough that the test stays instant.
            std::thread::sleep(std::time::Duration::from_millis(25));
            apply_probe.leave();
            Ok(())
        };
        let mut hooks = WindowHooks::default();
        replay_windowed(&items, 4, &mut hooks, &apply).unwrap();
        assert_eq!(hooks.begun, items, "every item begun, in dispatch order");
        assert_eq!(
            hooks
                .applied
                .iter()
                .map(|(item, _)| *item)
                .collect::<Vec<_>>(),
            items,
            "completions reported in dispatch order"
        );
        assert!(hooks.applied.iter().all(|(_, ok)| *ok));
        let max = probe.max_seen();
        assert!(
            (2..=4).contains(&max),
            "a 4-wide window over 25 ms applies must overlap; max concurrent was {max}"
        );
    }

    #[test]
    fn replay_windowed_stops_dispatch_on_failure_and_drains_what_was_dispatched() {
        let items: Vec<usize> = (0..6).collect();
        let mut hooks = WindowHooks::default();
        let apply = |item: &usize| {
            if *item == 1 {
                // Inside the first window, slow enough that items 0, 2 and
                // the next dispatch (3) are already in flight when it lands.
                std::thread::sleep(std::time::Duration::from_millis(30));
                Err(anyhow::anyhow!("injected apply failure"))
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
                Ok(())
            }
        };
        let error = replay_windowed(&items, 3, &mut hooks, &apply)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "injected apply failure");
        assert_eq!(
            hooks.begun,
            vec![0, 1, 2, 3],
            "dispatch stops at the failure"
        );
        assert_eq!(
            hooks.applied,
            vec![(0, true), (1, false), (2, true), (3, true)],
            "in-flight successes after the failure are still reported, so their \
             Committed writes are not lost; undispatched items are untouched"
        );
    }

    #[test]
    fn replay_windowed_width_one_never_spawns_and_keeps_order() {
        let items: Vec<usize> = (0..3).collect();
        let mut hooks = WindowHooks::default();
        let seen_threads =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let apply_of = std::sync::Arc::clone(&seen_threads);
        let apply = move |_item: &usize| {
            apply_of.lock().unwrap().insert(std::thread::current().id());
            Ok(())
        };
        replay_windowed(&items, 1, &mut hooks, &apply).unwrap();
        assert_eq!(hooks.begun, items);
        assert_eq!(hooks.applied, vec![(0, true), (1, true), (2, true)]);
        assert_eq!(
            seen_threads.lock().unwrap().len(),
            1,
            "width 1 runs every apply on the calling thread"
        );
    }

    #[test]
    fn replay_windowed_converts_a_worker_panic_into_the_run_error() {
        let items: Vec<usize> = (0..4).collect();
        let mut hooks = WindowHooks::default();
        let apply = |item: &usize| {
            if *item == 0 {
                std::panic::panic_any("worker exploded");
            }
            Ok(())
        };
        // Keep the injected panic out of the test output; it is the input,
        // not a failure to report.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let error = replay_windowed(&items, 4, &mut hooks, &apply).unwrap_err();
        std::panic::set_hook(previous_hook);
        assert_eq!(error.to_string(), "replay apply panicked: worker exploded");
        assert_eq!(hooks.begun, vec![0, 1, 2, 3], "the window was dispatched");
        assert_eq!(
            hooks.applied,
            vec![(0, false), (1, true), (2, true), (3, true)],
            "the drain still reports the applies that did land"
        );
    }

    /// A backend whose Nth `apply` is the server still answering 429 after
    /// the client's patience — the one way a throttled node still ends a run.
    struct ThrottledOutBackend {
        inner: FakeBackend,
        fail_apply_number: usize,
        applies: usize,
    }

    impl SyncOperationBackend for ThrottledOutBackend {
        fn apply(
            &mut self,
            operation: &SyncOperation,
            base: &CommittedManifest,
            desired: &GenerationManifest,
            snapshot: &SourceSnapshot,
        ) -> Result<()> {
            self.applies += 1;
            if self.applies == self.fail_apply_number {
                return Err(anyhow::Error::new(crate::esclient::BackpressureExhausted {
                    items: 7,
                    patience: std::time::Duration::from_secs(120),
                    reason: "breaker".into(),
                }))
                .context("replay sealed bulk");
            }
            self.inner.apply(operation, base, desired, snapshot)
        }

        fn validate(
            &mut self,
            base: &CommittedManifest,
            desired: &GenerationManifest,
            snapshot: &SourceSnapshot,
        ) -> Result<()> {
            self.inner.validate(base, desired, snapshot)
        }
    }

    /// #944, the terminal case: the node accepted nothing for the client's
    /// whole patience. The run still ends — nothing on this side can make a
    /// pinned node (#950) accept — but it ends NAMED: the terminal line says
    /// `reason=server-backpressure` with what is applied and what is left, a
    /// note names the file it was on, and the same journal resumes to a commit.
    /// Any other failure keeps `reason=aborted`.
    #[test]
    fn a_backpressure_stop_names_itself_and_what_is_left_then_resumes() {
        let _journal_guard = crate::state::SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = tempfile::tempdir().unwrap();
        let corpus = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(corpus.path().join(name), format!("content of {name}")).unwrap();
        }
        let inventory = inventory(corpus.path());
        // `plan_for` assigns the first file only; this test needs three
        // operations, so every file gets the same one-dataset assignment.
        let mut plan = plan_for(&inventory);
        for ((key, file), digest) in inventory
            .keys
            .iter()
            .zip(&inventory.files)
            .zip(&inventory.digests)
        {
            plan.files.insert(
                key.clone(),
                FileAssignment {
                    rel: file.rel.clone(),
                    path_id: file.rel_id.clone(),
                    is_symlink: Some(file.is_symlink),
                    family: "text".into(),
                    gzip: false,
                    content_digest: Some(digest.clone()),
                    assignments: vec![(None, "docs".into())],
                    as_document: false,
                },
            );
        }
        let groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        let snapshot = create_snapshot(state.path(), "tx-throttled", &inventory).unwrap();
        let mut journal =
            Journal::open(state.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        begin(&mut journal, "tx-throttled", &snapshot, plan, groups);
        let second_rel = {
            let pending = journal.pending_sync.as_ref().unwrap();
            let operation = &pending.operations[1];
            pending
                .desired
                .groups
                .iter()
                .find(|group| group.group_id == operation.group_id)
                .unwrap()
                .canonical
                .rel
                .clone()
        };

        let mut backend = ThrottledOutBackend {
            inner: FakeBackend::default(),
            fail_apply_number: 2,
            applies: 0,
        };
        let (pr, sink) = crate::progress::Progress::capture(
            crate::progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        let error =
            replay_pending_operations_reporting(state.path(), &mut journal, &mut backend, &pr)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("the server kept rejecting 7 of a prepared bulk's items"),
            "{error:#}"
        );
        let stream = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        let done = stream
            .lines()
            .find(|line| line.starts_with("xerj-done "))
            .unwrap_or_else(|| panic!("{stream}"));
        assert!(
            done.starts_with("xerj-done ok=false exit=1 reason=server-backpressure ")
                && done.ends_with(" ops_applied=1 ops_remaining=2"),
            "{done}"
        );
        assert!(
            stream.lines().any(|line| line.contains(&format!(
                "stopped by server back-pressure while applying {second_rel}: 1 operation(s) \
                 are journaled applied, 2 are not"
            ))),
            "{stream}"
        );

        // The same journal resumes: two operations left, then a commit.
        backend.fail_apply_number = usize::MAX;
        let (pr, sink) = crate::progress::Progress::capture(
            crate::progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        replay_pending_operations_reporting(state.path(), &mut journal, &mut backend, &pr).unwrap();
        assert_eq!(backend.inner.applications.len(), 3);
        assert!(journal.pending_sync.is_none(), "the generation committed");
        let stream = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        assert!(
            !stream.contains("xerj-done"),
            "a successful replay leaves the terminal line to its caller: {stream}"
        );
    }

    /// Only back-pressure earns the named reason. Anything else — here an
    /// injected failure after an apply — leaves the stream to close itself
    /// `reason=aborted`, exactly as before.
    #[test]
    fn a_failure_that_is_not_backpressure_is_not_renamed() {
        let _journal_guard = crate::state::SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = SNAPSHOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = tempfile::tempdir().unwrap();
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.txt"), "alpha").unwrap();
        let inventory = inventory(corpus.path());
        let plan = plan_for(&inventory);
        let groups = groups_from_inventory(&inventory, &plan, &[]).unwrap();
        let snapshot = create_snapshot(state.path(), "tx-other", &inventory).unwrap();
        let mut journal =
            Journal::open(state.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        begin(&mut journal, "tx-other", &snapshot, plan, groups);
        let mut backend = FakeBackend::default();
        let (pr, sink) = crate::progress::Progress::capture(
            crate::progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        fail_replay_after_next_apply();
        assert!(
            replay_pending_operations_reporting(state.path(), &mut journal, &mut backend, &pr)
                .is_err()
        );
        let stream = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        assert!(!stream.contains("xerj-done"), "{stream}");
        assert!(!stream.contains("back-pressure"), "{stream}");
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
            unreadable: vec![],
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
            unreadable: vec![],
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

    // ── #1212 shape 2: the read-back barrier re-walks an unflagged short page ──
    //
    // The live failure: finalize's run_id-scoped sorted walk read 9 full
    // 1,000-hit pages, then an EMPTY page with `timed_out` absent — 9,000 of
    // 58,568, no flag, so neither the transport retry nor #1213's page-level
    // deadline check could see it. Only observed-count-versus-expected can,
    // and the answer was intermittent: the same walk on an idle node read all
    // 59 pages clean. These pin the re-walk loop's contract without a node.

    fn doc_map(ids: &[&str]) -> BTreeMap<String, Value> {
        ids.iter()
            .map(|id| ((*id).to_string(), Value::String((*id).into())))
            .collect()
    }

    #[test]
    fn a_complete_first_read_back_walk_is_returned_without_refreshing() {
        let pr = crate::progress::Progress::silent();
        let walks = std::cell::Cell::new(0u32);
        let refreshes = std::cell::Cell::new(0u32);
        let observed = EsSyncBackend::rewalk_while_short(
            &pr,
            2,
            || {
                walks.set(walks.get() + 1);
                Ok(doc_map(&["a", "b"]))
            },
            || {
                refreshes.set(refreshes.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(walks.get(), 1, "the happy path is exactly one walk");
        assert_eq!(refreshes.get(), 0, "a complete walk never refreshes");
    }

    #[test]
    fn a_short_walk_is_refreshed_and_rewalked_until_complete() {
        let pr = crate::progress::Progress::silent();
        let walks = std::cell::Cell::new(0u32);
        let refreshes = std::cell::Cell::new(0u32);
        let observed = EsSyncBackend::rewalk_while_short(
            &pr,
            2,
            || {
                walks.set(walks.get() + 1);
                // walk 1 is the #1212 shape: an unflagged truncated answer;
                // walk 2 sees the whole set, the way the idle-node probe did
                Ok(if walks.get() == 1 {
                    doc_map(&["a"])
                } else {
                    doc_map(&["a", "b"])
                })
            },
            || {
                refreshes.set(refreshes.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(observed.len(), 2, "the complete observation is returned");
        assert_eq!(walks.get(), 2);
        assert_eq!(refreshes.get(), 1, "one refresh between the two walks");
    }

    #[test]
    fn a_walk_short_every_time_costs_all_attempts_and_returns_the_last_observation() {
        let pr = crate::progress::Progress::silent();
        let walks = std::cell::Cell::new(0u32);
        let observed = EsSyncBackend::rewalk_while_short(
            &pr,
            58_568,
            || {
                walks.set(walks.get() + 1);
                // every walk truncates differently short — never complete
                let id = format!("only-{0}", walks.get());
                Ok(BTreeMap::from([(id.clone(), Value::String(id))]))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            walks.get(),
            EsSyncBackend::READBACK_WALK_ATTEMPTS as u32,
            "bounded: no infinite re-walk over a persistently truncating node"
        );
        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed.keys().next().map(String::as_str),
            Some(format!("only-{0}", EsSyncBackend::READBACK_WALK_ATTEMPTS).as_str()),
            "the LAST observation is what validate_observed judges — the error \
             names the final walk's counts, not a stale one"
        );
    }

    #[test]
    fn a_transport_error_in_the_walk_propagates_without_a_retry() {
        let pr = crate::progress::Progress::silent();
        let refreshes = std::cell::Cell::new(0u32);
        let verdict = EsSyncBackend::rewalk_while_short(
            &pr,
            2,
            || Err(anyhow::anyhow!("connection reset")),
            || {
                refreshes.set(refreshes.get() + 1);
                Ok(())
            },
        );
        assert!(verdict.is_err(), "an error is not a short walk");
        assert_eq!(refreshes.get(), 0, "no refresh over a dead transport");
    }

    // #1267: the `run_id`-scoped catalog read-back walk. On a resume that
    // rewrote the whole catalog its `run_id` matches every document, so it
    // runs for ~35 min — and used to do so invisibly, behind a finished
    // `finalize-catalog` ticker that read as a hang. These pin the walk as its
    // own phase with the engine's match total as the denominator, plus the
    // paging contract, without a node.

    /// One page of a `run_id` walk: every hit carries a `sort` key (the
    /// continuation the next page is fetched from) and the engine reports the
    /// full match `total` on every page, not just the first.
    fn walk_page(total: u64, ids: &[&str]) -> Value {
        let hits: Vec<Value> = ids
            .iter()
            .map(|id| serde_json::json!({"_id": id, "_source": {"run": id}, "sort": [id]}))
            .collect();
        serde_json::json!({"hits": {"total": {"value": total}, "hits": hits}})
    }

    #[test]
    fn a_run_id_walk_collects_every_page_and_opens_an_honest_phase() {
        let (pr, buffer) = crate::progress::Progress::capture(
            crate::progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        // Page size 2: two full pages, then a short page ends the walk. The
        // engine reports total=5 on every page.
        let pages = std::cell::RefCell::new(vec![
            walk_page(5, &["a", "b"]),
            walk_page(5, &["c", "d"]),
            walk_page(5, &["e"]),
        ]);
        let calls = std::cell::Cell::new(0usize);
        let observed = EsSyncBackend::walk_run_id_pages(
            &pr,
            "finalize-catalog-readback",
            2,
            |_search_after| {
                calls.set(calls.get() + 1);
                Ok(pages.borrow_mut().remove(0))
            },
        )
        .unwrap();
        assert_eq!(
            calls.get(),
            3,
            "two full pages then one short page ends the walk"
        );
        assert_eq!(
            observed.len(),
            5,
            "every document across every page is collected"
        );
        assert!(
            observed.contains_key("e"),
            "the short final page is not dropped"
        );

        let out = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(
            out.contains("phase=finalize-catalog-readback"),
            "the walk is its own phase, not a frozen finalize-catalog ticker: {out}"
        );
        // The denominator is the engine's own match total from page one — not
        // the page size, not the running count — so the bar is honest from the
        // first page instead of after 35 min of silence (#1267).
        assert!(
            out.contains("items=0/5"),
            "the phase opens with the full match total as its denominator: {out}"
        );
    }

    #[test]
    fn the_read_back_walk_advances_its_surface_per_document() {
        // The whole point of #1267: the walk must TICK, so a long read-back is
        // never a stalled line. A ticker on a short cadence proves the surface
        // reaches `items=3/3` without any item needing to "complete" a file.
        let (pr, buffer) = crate::progress::Progress::capture(
            crate::progress::Surface::Plain,
            std::time::Duration::from_millis(40),
        );
        let ticker = pr.spawn_ticker();
        // One short page (size 1000 default; 3 < 1000 ends the walk).
        let observed = EsSyncBackend::walk_run_id_pages(
            &pr,
            "finalize-catalog-readback",
            1000,
            |_search_after| Ok(walk_page(3, &["a", "b", "c"])),
        )
        .unwrap();
        assert_eq!(observed.len(), 3);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let out = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
            if out.contains("phase=finalize-catalog-readback") && out.contains("items=3/3") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the ticker never showed the walk advance to items=3/3: {out}"
            );
            std::thread::yield_now();
        }
        drop(ticker);
    }

    // #1267 operational: the server-generation latency probe that runs before
    // `finalize-verify`. Timings are injected, so these pin the decision
    // without a node or a clock.

    fn run_probe(
        surface: crate::progress::Surface,
        timings: &[std::time::Duration],
        version: Option<&str>,
    ) -> (Option<std::time::Duration>, usize, String) {
        let (pr, buffer) =
            crate::progress::Progress::capture(surface, std::time::Duration::from_secs(3600));
        let queue = std::cell::RefCell::new(timings.to_vec());
        let calls = std::cell::Cell::new(0usize);
        let verdict = EsSyncBackend::probe_server_generation(
            &pr,
            400,
            EsSyncBackend::SERVER_PROBE_SLOW,
            || {
                calls.set(calls.get() + 1);
                let mut queue = queue.borrow_mut();
                anyhow::ensure!(!queue.is_empty(), "transport down");
                Ok(queue.remove(0))
            },
            || version.map(str::to_owned),
        );
        let out = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        (verdict, calls.get(), out)
    }

    #[test]
    fn a_fast_verify_window_probes_once_and_stays_silent() {
        let ms = std::time::Duration::from_millis;
        let (verdict, calls, out) =
            run_probe(crate::progress::Surface::Plain, &[ms(3)], Some("0.0.0-old"));
        assert_eq!(verdict, Some(ms(3)));
        assert_eq!(calls, 1, "a fast first answer is not re-asked");
        assert!(
            !out.contains("warning"),
            "a fixed server is not warned about: {out}"
        );
    }

    #[test]
    fn a_cold_first_window_is_reasked_and_not_mistaken_for_an_old_server() {
        // #1267's own numbers: 8.8 s cold page-in on the FIXED binary, then 2.7 ms.
        let ms = std::time::Duration::from_millis;
        let (verdict, calls, out) =
            run_probe(crate::progress::Surface::Plain, &[ms(8800), ms(3)], None);
        assert_eq!(verdict, Some(ms(3)));
        assert_eq!(calls, 2);
        assert!(
            !out.contains("warning"),
            "cold page-in is not an old server: {out}"
        );
    }

    #[test]
    fn a_slow_warm_window_warns_on_the_plain_surface_and_does_not_abort() {
        let ms = std::time::Duration::from_millis;
        let (verdict, calls, out) = run_probe(
            crate::progress::Surface::Plain,
            &[ms(8800), ms(3400)],
            Some("1.0.0-rc.80"),
        );
        assert_eq!(
            verdict,
            Some(ms(3400)),
            "the probe reports and the run continues"
        );
        assert_eq!(calls, 2);
        for needle in [
            "warning",
            "3.4s",
            "older than this client",
            "#1260",
            "400 windows",
            "~0.4h",
            "XERJ 1.0.0-rc.80",
            env!("CARGO_PKG_VERSION"),
            "upgrade the server",
            "Continuing",
        ] {
            assert!(out.contains(needle), "warning lacks {needle:?}: {out}");
        }
    }

    #[test]
    fn a_slow_server_with_the_same_version_is_named_a_development_build() {
        let ms = std::time::Duration::from_millis;
        let (_, _, out) = run_probe(
            crate::progress::Surface::Plain,
            &[ms(3400), ms(3400)],
            Some(env!("CARGO_PKG_VERSION")),
        );
        assert!(out.contains("same version as this client"), "{out}");
        assert!(out.contains("development build"), "{out}");
    }

    #[test]
    fn the_slow_server_warning_is_one_json_event_on_the_machine_surface() {
        let ms = std::time::Duration::from_millis;
        let (_, _, out) = run_probe(crate::progress::Surface::Json, &[ms(3400), ms(3400)], None);
        let events: Vec<Value> = out
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        assert_eq!(events.len(), 1, "{out}");
        assert_eq!(events[0]["event"], "warning");
        assert!(events[0]["message"]
            .as_str()
            .unwrap()
            .contains("older than this client"));
    }

    #[test]
    fn the_slow_server_warning_survives_the_silent_surface() {
        // `--quiet` drops routine notes; a four-hour wait is not routine.
        let ms = std::time::Duration::from_millis;
        let (_, _, out) = run_probe(
            crate::progress::Surface::Silent,
            &[ms(3400), ms(3400)],
            None,
        );
        assert!(out.contains("older than this client"), "{out}");
    }

    #[test]
    fn a_failing_probe_neither_warns_nor_propagates() {
        let (verdict, calls, out) = run_probe(crate::progress::Surface::Plain, &[], None);
        assert_eq!(verdict, None);
        assert_eq!(calls, 1);
        assert!(out.is_empty(), "{out}");
    }

    #[test]
    fn a_duplicate_id_across_pages_fails_the_walk_loudly() {
        let pr = crate::progress::Progress::silent();
        let pages =
            std::cell::RefCell::new(vec![walk_page(4, &["a", "b"]), walk_page(4, &["b", "c"])]);
        let verdict = EsSyncBackend::walk_run_id_pages(
            &pr,
            "finalize-catalog-readback",
            2,
            |_search_after| Ok(pages.borrow_mut().remove(0)),
        );
        let err = verdict.unwrap_err().to_string();
        assert!(
            err.contains("duplicate ID b"),
            "a document seen twice is a loud failure, never a silent overwrite: {err}"
        );
    }

    #[test]
    fn a_full_page_without_a_continuation_key_cannot_advance() {
        let pr = crate::progress::Progress::silent();
        // A full page (size 2) whose hits carry no `sort`: the walk cannot form
        // the next `search_after` and must refuse rather than loop or truncate.
        let page = serde_json::json!({
            "hits": {"total": {"value": 9}, "hits": [
                {"_id": "a", "_source": {}},
                {"_id": "b", "_source": {}}
            ]}
        });
        let verdict = EsSyncBackend::walk_run_id_pages(
            &pr,
            "finalize-catalog-readback",
            2,
            |_search_after| Ok(page.clone()),
        );
        let err = verdict.unwrap_err().to_string();
        assert!(
            err.contains("continuation sort key"),
            "a full page with no sort key is a loud failure: {err}"
        );
    }

    /// #2222: deleting a finalize-verify target while its window search is in
    /// flight must end the wait. The search connection is held open on purpose
    /// — that is the in-flight request whose handle the node already resolved,
    /// so it does not become a 404 — and `HEAD` starts answering 404 once the
    /// index is "deleted". Verify has to return the deleted-index error while
    /// that search is still unanswered.
    #[test]
    fn finalize_verify_stops_when_its_target_index_is_deleted() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;

        const INDEX: &str = "xc-elastic-charts-cpu-profile-tree-mock";

        fn read_http(stream: &mut std::net::TcpStream) -> String {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            String::from_utf8_lossy(&buf).into_owned()
        }

        fn write_status(stream: &mut std::net::TcpStream, status: &str) {
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // Accepted sockets inherit non-blocking from the listener. Put each
        // one back into blocking mode inside its handler. The accept loop
        // itself stays non-blocking so a parked `_search` — handled on its
        // own thread — cannot stop a later `HEAD` from being answered.
        listener.set_nonblocking(true).unwrap();
        let deleted = std::sync::Arc::new(AtomicBool::new(false));
        let release = std::sync::Arc::new(AtomicBool::new(false));
        let heads = std::sync::Arc::new(AtomicUsize::new(0));
        let deleted_srv = std::sync::Arc::clone(&deleted);
        let release_srv = std::sync::Arc::clone(&release);
        let heads_srv = std::sync::Arc::clone(&heads);
        let server = std::thread::spawn(move || {
            // Hold the search well past the test's own deadline so a client
            // that waits for the response fails the test instead of passing
            // by accident. `release` lets the passing path close the socket.
            let hold_until = std::time::Instant::now() + Duration::from_secs(45);
            let started = std::time::Instant::now();
            let mut handlers = Vec::new();
            while started.elapsed() < Duration::from_secs(60) {
                if release_srv.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        let deleted_one = std::sync::Arc::clone(&deleted_srv);
                        let release_one = std::sync::Arc::clone(&release_srv);
                        let heads_one = std::sync::Arc::clone(&heads_srv);
                        handlers.push(std::thread::spawn(move || {
                            let mut stream = stream;
                            stream.set_nonblocking(false).unwrap();
                            let request = read_http(&mut stream);
                            if request.starts_with("HEAD ") {
                                heads_one.fetch_add(1, Ordering::SeqCst);
                                let status = if deleted_one.load(Ordering::SeqCst) {
                                    "404 Not Found"
                                } else {
                                    "200 OK"
                                };
                                write_status(&mut stream, status);
                            } else if request.contains("_search") {
                                while !release_one.load(Ordering::SeqCst)
                                    && std::time::Instant::now() < hold_until
                                {
                                    std::thread::sleep(Duration::from_millis(20));
                                }
                                write_status(&mut stream, "200 OK");
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });

        let deleted_watch = std::sync::Arc::clone(&deleted);
        let heads_watch = std::sync::Arc::clone(&heads);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while heads_watch.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            deleted_watch.store(true, Ordering::SeqCst);
        });

        let url = format!("http://{address}");
        let (tx, rx) = mpsc::channel();
        // Release the parked search even if an assertion panics, so the
        // server thread does not sit out the rest of its hold.
        struct ReleaseOnDrop(std::sync::Arc<AtomicBool>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _release_on_drop = ReleaseOnDrop(std::sync::Arc::clone(&release));
        std::thread::spawn(move || {
            let es = crate::esclient::Es::new(&url, None).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let pr = crate::progress::Progress::silent();
            let backend = EsSyncBackend::new(&es, dir.path(), 8 * 1024 * 1024, &pr);
            let result =
                backend.windowed_value_counts(INDEX, "ax_file", &["digest".into()], &[], false);
            let _ = tx.send(result.map_err(|error| format!("{error:#}")));
        });

        let message = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("finalize-verify hung on an index that was deleted during verify")
            .expect_err("a deleted verify target must fail that item");
        assert!(
            !release.load(Ordering::SeqCst),
            "verify returned only after the hung search was released"
        );
        assert!(message.contains("was deleted during verify"), "{message}");
        assert!(message.contains(INDEX), "{message}");

        release.store(true, Ordering::SeqCst);
        // Unblock `accept` if it is parked, then let the held search finish.
        let _ = std::net::TcpStream::connect(address);
        server.join().unwrap();
    }

    /// The same window, when the index stays and the search answers, still
    /// returns the bucket counts. The deletion check must not fire on a live
    /// target.
    #[test]
    fn finalize_verify_window_counts_a_live_index() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let body =
                br#"{"aggregations":{"values":{"buckets":[{"key":"digest-a","doc_count":3}]}}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let es = crate::esclient::Es::new(&format!("http://{address}"), None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pr = crate::progress::Progress::silent();
        let backend = EsSyncBackend::new(&es, dir.path(), 8 * 1024 * 1024, &pr);
        let counts = backend
            .windowed_value_counts(
                "xc-live",
                "ax_file",
                &["digest-a".into(), "digest-b".into()],
                &[],
                false,
            )
            .unwrap();
        assert_eq!(counts.get("digest-a"), Some(&3));
        assert_eq!(counts.get("digest-b"), Some(&0));
        server.join().unwrap();
    }

    /// A scripted node for the finalize-verify retry tests (#2405). Every
    /// connection gets its own handler thread so a `_search` in flight
    /// cannot starve the `HEAD` probe `search_for_verify` may send while a
    /// search is unanswered. `HEAD` always answers 200 (the index lives);
    /// each `_search` records its request body and answers with the next
    /// scripted response — the last one repeats when the script runs out,
    /// so a caller that keeps re-asking past the script is still answered.
    struct ScriptedSearch {
        address: std::net::SocketAddr,
        search_bodies: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        server: Option<std::thread::JoinHandle<()>>,
    }

    impl ScriptedSearch {
        fn start(script: &[&str]) -> Self {
            use std::io::{Read, Write};
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::{Arc, Mutex};
            use std::time::Duration;

            fn read_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                                let length = head
                                    .lines()
                                    .find_map(|line| {
                                        line.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .and_then(|v| v.trim().parse::<usize>().ok())
                                    })
                                    .unwrap_or(0);
                                let have = buf.len() - (end + 4);
                                if have >= length {
                                    return (head, buf[end + 4..end + 4 + length].to_vec());
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                (head, Vec::new())
            }

            fn write_json(stream: &mut std::net::TcpStream, body: &str) {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body.as_bytes()).unwrap();
            }

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let search_bodies = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let script: Arc<Mutex<std::collections::VecDeque<String>>> =
                Arc::new(Mutex::new(script.iter().map(|s| s.to_string()).collect()));
            let bodies_srv = Arc::clone(&search_bodies);
            let stop_srv = Arc::clone(&stop);
            let script_srv = Arc::clone(&script);
            let server = std::thread::spawn(move || {
                let mut handlers = Vec::new();
                while !stop_srv.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let bodies_one = Arc::clone(&bodies_srv);
                            let script_one = Arc::clone(&script_srv);
                            handlers.push(std::thread::spawn(move || {
                                let mut stream = stream;
                                stream.set_nonblocking(false).unwrap();
                                let (head, body) = read_request(&mut stream);
                                if head.starts_with("HEAD ") {
                                    write_json(&mut stream, "{}");
                                } else if head.contains("_search") {
                                    let parsed =
                                        serde_json::from_slice(&body).unwrap_or(Value::Null);
                                    bodies_one.lock().unwrap().push(parsed);
                                    let answer = {
                                        let mut scripted = script_one.lock().unwrap();
                                        if scripted.len() > 1 {
                                            scripted.pop_front().unwrap()
                                        } else {
                                            scripted
                                                .front()
                                                .cloned()
                                                .unwrap_or_else(|| "{}".to_string())
                                        }
                                    };
                                    write_json(&mut stream, &answer);
                                } else {
                                    write_json(&mut stream, "{}");
                                }
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
                for handler in handlers {
                    let _ = handler.join();
                }
            });
            ScriptedSearch {
                address,
                search_bodies,
                stop,
                server: Some(server),
            }
        }

        fn searches(&self) -> Vec<Value> {
            self.search_bodies.lock().unwrap().clone()
        }

        fn es(&self) -> crate::esclient::Es {
            crate::esclient::Es::new(&format!("http://{}", self.address), None).unwrap()
        }

        fn shutdown(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            // Unblock `accept` if it is parked.
            let _ = std::net::TcpStream::connect(self.address);
            if let Some(server) = self.server.take() {
                server.join().unwrap();
            }
        }
    }

    impl Drop for ScriptedSearch {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    /// #2405: a `timed_out` window used to exit 1 on the spot. The window
    /// is re-asked — with the page timeout the body lacked — and the second
    /// answer counts.
    #[test]
    fn finalize_verify_window_retries_a_timed_out_answer() {
        let mut server = ScriptedSearch::start(&[
            r#"{"timed_out":true,"hits":{"hits":[]}}"#,
            r#"{"aggregations":{"values":{"buckets":[{"key":"digest-a","doc_count":3}]}}}"#,
        ]);
        let es = server.es();
        let dir = tempfile::tempdir().unwrap();
        let pr = crate::progress::Progress::silent();
        let backend = EsSyncBackend::new(&es, dir.path(), 8 * 1024 * 1024, &pr);
        let counts = backend
            .windowed_value_counts(
                "xc-retry",
                "ax_file",
                &["digest-a".into(), "digest-b".into()],
                &[],
                false,
            )
            .unwrap();
        assert_eq!(counts.get("digest-a"), Some(&3));
        assert_eq!(counts.get("digest-b"), Some(&0));
        let searches = server.searches();
        assert_eq!(
            searches.len(),
            2,
            "one retry, not a page storm: {searches:?}"
        );
        for body in &searches {
            assert_eq!(
                body.get("timeout").and_then(Value::as_str),
                Some("120s"),
                "the retried window must carry the page timeout: {body}"
            );
        }
        server.shutdown();
    }

    /// The retry is bounded: a node that answers every attempt partial
    /// gets the loud error, not an infinite wait, and the window was never
    /// counted.
    #[test]
    fn finalize_verify_window_fails_loud_when_every_attempt_times_out() {
        let mut server = ScriptedSearch::start(&[r#"{"timed_out":true,"hits":{"hits":[]}}"#]);
        let es = server.es();
        let dir = tempfile::tempdir().unwrap();
        let pr = crate::progress::Progress::silent();
        let backend = EsSyncBackend::new(&es, dir.path(), 8 * 1024 * 1024, &pr);
        let error = backend
            .windowed_value_counts("xc-wedged", "ax_file", &["digest-a".into()], &[], false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("xc-wedged"), "{error}");
        assert!(error.contains("timed_out"), "{error}");
        assert!(error.contains("consecutive attempts"), "{error}");
        assert!(error.contains("rerun the same command"), "{error}");
        assert_eq!(
            server.searches().len(),
            crate::esclient::PARTIAL_PAGE_ATTEMPTS,
        );
        server.shutdown();
    }

    /// The catalog read-back's `search_page` leg retries the same way.
    #[test]
    fn catalog_read_back_window_retries_a_timed_out_page() {
        let mut server = ScriptedSearch::start(&[
            r#"{"timed_out":true,"hits":{"hits":[]}}"#,
            r#"{"timed_out":false,"hits":{"total":{"value":1,"relation":"eq"},"hits":[{"_id":"x","_source":{"file_key":"k1"}}]}}"#,
        ]);
        let es = server.es();
        let dir = tempfile::tempdir().unwrap();
        let pr = crate::progress::Progress::silent();
        let backend = EsSyncBackend::new(&es, dir.path(), 8 * 1024 * 1024, &pr);
        let counts = backend
            .windowed_catalog_counts(&["k1".into()], "run-1")
            .unwrap();
        assert_eq!(counts.get("k1"), Some(&1));
        assert_eq!(server.searches().len(), 2);
        server.shutdown();
    }

    #[test]
    fn a_zero_exists_total_skips_the_per_digest_windows() {
        // #1267 measured 2,619 term+exists probes, each a brute scan with zero
        // hits. After the windowed verify those are three windows of 1,024.
        // A field that matches nothing issues none of them.
        let skipped = plan_exists_probes(2619, EsSyncBackend::VERIFY_WINDOW, 0);
        assert_eq!(skipped.issue_windows, 0);
        assert_eq!(skipped.skip_windows, 3);
        let issued = plan_exists_probes(2619, EsSyncBackend::VERIFY_WINDOW, 1);
        assert_eq!(issued.issue_windows, 3);
        assert_eq!(issued.skip_windows, 0);
        assert_eq!(
            plan_exists_probes(0, EsSyncBackend::VERIFY_WINDOW, 0),
            ExistsProbePlan {
                issue_windows: 0,
                skip_windows: 0
            }
        );
        assert_eq!(plan_exists_probes(5, 0, 0).issue_windows, 0);
    }

    #[test]
    fn skipped_exists_windows_advance_the_verify_phase() {
        let (pr, buffer) = crate::progress::Progress::capture(
            crate::progress::Surface::Plain,
            std::time::Duration::from_millis(40),
        );
        let ticker = pr.spawn_ticker();
        let plan = plan_exists_probes(2619, EsSyncBackend::VERIFY_WINDOW, 0);
        pr.phase("finalize-verify", plan.skip_windows, 0);
        credit_verify_items(&pr, plan.skip_windows);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let out = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
            if out.contains("phase=finalize-verify") && out.contains("items=3/3") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the ticker never showed the skipped windows advance to items=3/3: {out}"
            );
            std::thread::yield_now();
        }
        drop(ticker);
    }

    #[test]
    fn an_unchanged_catalog_source_is_not_rewritten() {
        let same = serde_json::json!({"run_id": "g1", "path": "a"});
        let expected = BTreeMap::from([
            ("file:same".to_owned(), same.clone()),
            (
                "file:changed".to_owned(),
                serde_json::json!({"run_id": "g2", "path": "b"}),
            ),
            (
                "file:missing".to_owned(),
                serde_json::json!({"run_id": "g2", "path": "c"}),
            ),
        ]);
        let live = BTreeMap::from([
            ("file:same".to_owned(), same),
            (
                "file:changed".to_owned(),
                serde_json::json!({"run_id": "g1", "path": "b"}),
            ),
        ]);
        let write = catalog_docs_to_write(&expected, &live);
        assert!(
            !write.contains_key("file:same"),
            "a resume that already stored this projection must not bulk it again"
        );
        assert_eq!(write["file:changed"]["run_id"], "g2");
        assert!(write.contains_key("file:missing"));
        assert_eq!(write.len(), 2);
    }

    #[test]
    fn the_prior_run_remainder_is_the_kept_intersection() {
        let prior = BTreeSet::from([
            "kept".to_owned(),
            "rewritten".to_owned(),
            "not-kept".to_owned(),
        ]);
        let kept = BTreeSet::from(["kept".to_owned(), "born-kept".to_owned()]);
        assert_eq!(prior_run_expected_remainder(&prior, &kept), 1);
    }

    #[test]
    fn catalog_run_ids_round_trip_and_a_missing_file_is_an_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("sync-snapshots").join("tx-1267");
        std::fs::create_dir_all(&snap).unwrap();
        assert!(
            read_catalog_run_ids(dir.path(), "tx-1267")
                .unwrap()
                .is_none(),
            "a generation published before this file existed walks once"
        );
        let ids = ["b".to_owned(), "a".to_owned()];
        write_catalog_run_ids(dir.path(), "tx-1267", [&ids[0], &ids[1]]).unwrap();
        assert_eq!(
            read_catalog_run_ids(dir.path(), "tx-1267")
                .unwrap()
                .unwrap(),
            BTreeSet::from(["a".to_owned(), "b".to_owned()])
        );
        let bad = "a\nb".to_owned();
        let err = write_catalog_run_ids(dir.path(), "tx-1267", [&bad]).unwrap_err();
        assert!(
            err.to_string().contains("newline"),
            "a newline id must not split into two lines: {err}"
        );
        assert!(read_catalog_run_ids(dir.path(), "../tx").is_err());
    }

    #[test]
    fn mget_sources_keep_found_documents_and_reject_a_duplicate_id() {
        let body = serde_json::json!({"docs": [
            {"_id": "a", "found": true, "_source": {"k": 1}},
            {"_id": "b", "found": false}
        ]});
        let got = mget_sources(&body).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got["a"]["k"], 1);
        let dup = serde_json::json!({"docs": [
            {"_id": "a", "found": true, "_source": {}},
            {"_id": "a", "found": true, "_source": {}}
        ]});
        let err = mget_sources(&dup).unwrap_err().to_string();
        assert!(err.contains("duplicate ID a"), "{err}");
    }
}

/// Issue #482 regression guard.
///
/// The durable-snapshot path (`--no-graph`) fsyncs four directories per seal
/// plus two per GC step. `File::open` applied to a *directory* is a Unix-only
/// idiom: on Windows it returns `ERROR_ACCESS_DENIED` (os error 5) for every
/// call, because `std` cannot pass `FILE_FLAG_BACKUP_SEMANTICS`. Each of
/// those sites was therefore an unconditional abort of the whole run on that
/// platform. The engine had already learned this once —
/// `xerj_common::fsio::fsync_dir` carries the `#[cfg(windows)]` shim and the
/// write-up — and this module re-derived the broken form anyway.
///
/// The fix is `#[cfg(windows)]`-shaped, so no behavioural test compiled for
/// Unix can tell fixed code from unfixed. The behavioural coverage is the
/// windows-latest `autoindex-fd-smoke` job, which since this change runs the
/// reported `--no-graph` flow. This guard is what keeps the idiom from coming
/// back on the platform the unit suite is never compiled for.
#[cfg(test)]
mod windows_directory_open_guard {
    /// The body of the `fn` whose signature line starts with `header`,
    /// brace-matched out of this file's own source at compile time.
    fn function_body(source: &str, header: &str) -> String {
        let start = source
            .find(header)
            .unwrap_or_else(|| panic!("signature moved: {header}"))
            + header.len();
        let mut depth = 1usize;
        for (offset, ch) in source[start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return source[start..start + offset].to_string();
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces after {header}");
    }

    /// Every directory this module makes durable, named by the binding the
    /// code passes around. None of them may reach `File::open`.
    const DIRECTORY_BINDINGS: [&str; 6] = [
        "&root",
        "&staging",
        "&blobs",
        "&prepared_dir",
        "&final_dir",
        "&snapshot_dir",
    ];

    #[test]
    fn no_directory_is_opened_as_a_file() {
        const SRC: &str = include_str!("sync_executor.rs");
        for binding in DIRECTORY_BINDINGS {
            // Assembled, never written out literally, so this test's own
            // source can never satisfy the search it performs.
            let forbidden = format!("File::open({binding})");
            assert!(
                !SRC.contains(&forbidden),
                "#482: `{forbidden}` opens a directory as a file. That call returns \
                 ERROR_ACCESS_DENIED (os error 5) on Windows every single time and \
                 aborts the run. Use `sync_dir`, which routes through \
                 `xerj_common::fsio::fsync_dir`."
            );
        }
    }

    #[test]
    fn sync_dir_delegates_to_the_platform_aware_shim() {
        const SRC: &str = include_str!("sync_executor.rs");
        let body = function_body(SRC, "fn sync_dir(path: &Path) -> Result<()> {");
        assert!(
            body.contains("xerj_common::fsio::fsync_dir(path)"),
            "#482: `sync_dir` must delegate to `xerj_common::fsio::fsync_dir`, whose \
             Windows body is a documented no-op; body was:\n{body}"
        );
        assert!(
            !body.contains("File::open"),
            "#482: `sync_dir` re-derived the Unix-only `File::open(dir).sync_all()` \
             idiom, which fails with os error 5 on every Windows call; body was:\n{body}"
        );
    }
}
