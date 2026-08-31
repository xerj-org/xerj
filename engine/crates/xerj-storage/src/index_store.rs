//! Index-level storage: WAL + segments + atomic snapshot swap.
//!
//! [`IndexStore`] is the entry point for all read and write operations on a
//! single index.  It owns:
//!
//! - A [`WalWriter`] (behind a `Mutex`) for durable mutation recording.
//! - An [`ArcSwap<IndexSnapshot>`] that holds the current set of active
//!   segments — swapped atomically on flush so readers never block writers.
//! - A [`VersionMap`] for lock-free optimistic concurrency.
//!
//! ## Flush lifecycle
//!
//! 1. The caller accumulates mutations in memory (a simple `Vec` here; a real
//!    implementation would use a sorted skip-list / BTreeMap memtable).
//! 2. [`IndexStore::flush`] is called (manually or by a background thread when
//!    the memtable exceeds a configurable threshold).
//! 3. Flush:
//!    a. Freezes the memtable — subsequent writes go to a new buffer.
//!    b. Writes a new `.seg` file via [`SegmentWriter`].
//!    c. Atomically swaps the snapshot (old list + new segment).
//!    d. Writes a WAL checkpoint covering all flushed seq_nos.
//!    e. Prunes WAL generations that are now covered.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use crate::backend::StorageBackend;
use crate::segment::{SectionType, SegmentId, SegmentMeta, SegmentReader, SegmentWriter};
use crate::version_map::{VersionMap, VersionRepointTransaction, IN_MEMORY_SEGMENT_ID};
use crate::wal::{SyncMode, WalEntry, WalWriter};
use crate::{Result, SeqNo, StorageError};

// ── Data-directory format marker (RC4 W3 #10) ─────────────────────────────────

/// Highest on-disk data-directory format this binary can read.
///
/// Version 2 reserves digest-derived FTS field filename components. A v2
/// binary can read v1 raw side-cars, but a v1 binary must refuse a directory
/// after the first v2 side-car can be created.
const DATA_DIR_FORMAT_VERSION: u32 = 2;

/// Baseline written for fresh and pre-marker directories.
///
/// Merely opening a directory with a newer binary must not make rollback
/// impossible. The marker advances to v2 only at the durable preflight for a
/// writer that will create an encoded FTS field filename.
const DATA_DIR_BASE_FORMAT_VERSION: u32 = 1;

/// First layout version that permits digest-derived FTS field components.
const DATA_DIR_FTS_ENCODED_FIELD_COMPONENT_VERSION: u32 = 2;

#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataDirFormatWriteFailpoint {
    BeforeTempWrite = 1,
    BeforeRename = 2,
    BeforeParentFsync = 3,
}

/// Name of the format-marker file written at the data-dir root.
const DATA_DIR_META_FILE: &str = "xerj_meta.json";

/// Contents of `xerj_meta.json`.
///
/// `format_version` is REQUIRED — a marker file that lacks it is treated as
/// corrupt (deserialization fails → refuse to open). Provenance fields carry
/// `#[serde(default)]` so the marker tolerates field additions across
/// versions the same way the manifest does.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DataDirMeta {
    /// Data-dir layout version this directory was last written with.
    format_version: u32,
    /// Best-effort provenance for operators; ignored by the version gate.
    #[serde(default)]
    xerj_version: String,
}

// ── IndexSnapshot ─────────────────────────────────────────────────────────────

/// Immutable snapshot of the active segments at a point in time.
///
/// Stored inside `ArcSwap<IndexSnapshot>`.  Readers load a copy of the `Arc`
/// (cheap, no lock) and can iterate the segment list without holding any mutex.
/// Writers create a new `IndexSnapshot` with the updated list and swap it in.
///
/// ## Upgrade hygiene (RC4 W3 #10)
///
/// `segments` is REQUIRED with no serde default: it is the core of the
/// manifest, so a `snapshot.json` that lacks it (e.g. a truncated `{}` or a
/// wrong-shaped file) fails to deserialize and is refused rather than
/// silently loaded as an empty snapshot — which would orphan (and then GC)
/// every segment on disk. The other fields carry `#[serde(default)]` so a
/// manifest written by a different xerj version, missing one of them, still
/// loads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexSnapshot {
    /// Ordered list of active segments (oldest first).
    pub segments: Vec<SegmentMeta>,
    /// Snapshot generation — incremented on every flush/merge.
    #[serde(default)]
    pub generation: u64,
    /// The highest seq_no covered by segments in this snapshot.
    #[serde(default)]
    pub max_seq_no: SeqNo,
}

impl IndexSnapshot {
    fn empty() -> Self {
        Self {
            segments: Vec::new(),
            generation: 0,
            max_seq_no: 0,
        }
    }

    fn with_new_segment(&self, meta: SegmentMeta) -> Self {
        let max_seq_no = self.max_seq_no.max(meta.max_seq_no);
        let mut segments = self.segments.clone();
        segments.push(meta);
        Self {
            segments,
            generation: self.generation + 1,
            max_seq_no,
        }
    }

    fn replace_segments(&self, remove_ids: &[SegmentId], add: SegmentMeta) -> Self {
        let remove_set: std::collections::HashSet<&str> =
            remove_ids.iter().map(String::as_str).collect();
        let mut segments: Vec<SegmentMeta> = self
            .segments
            .iter()
            .filter(|s| !remove_set.contains(s.id.as_str()))
            .cloned()
            .collect();
        segments.push(add);
        let max_seq_no = segments.iter().map(|s| s.max_seq_no).max().unwrap_or(0);
        Self {
            segments,
            generation: self.generation + 1,
            max_seq_no,
        }
    }
}

// ── Memtable entry ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemEntry {
    pub seq_no: SeqNo,
    pub doc_id: String,
    /// `None` = tombstone (delete).
    pub source: Option<std::sync::Arc<serde_json::Value>>,
    /// Raw JSON bytes of the source document. When non-empty, the segment
    /// writer uses these directly instead of re-serializing the Value —
    /// saving ~500ns/doc on the flush hot path.
    pub source_bytes: std::sync::Arc<[u8]>,
}

/// Opaque handle holding a drained memtable.
///
/// Returned by `IndexStore::take_memtable_for_flush` and consumed by
/// `IndexStore::finalize_flush_with_publisher`.  The engine layer uses this
/// two-step drain/finalise split to drop its FTS write lock before the
/// expensive segment + side-car I/O — unblocking ingest during the flush.
pub struct DrainedMemtable {
    pub entries: Vec<MemEntry>,
}

#[derive(Debug)]
pub enum FlushFinalizeOutcome {
    Empty,
    Published {
        meta: SegmentMeta,
        maintenance_deferred: bool,
    },
}

// ── Fsck report types ─────────────────────────────────────────────────────────
//
// ── SnapshotReadGuard ─────────────────────────────────────────────────────────

/// A loaded [`IndexSnapshot`] plus a **read lease** on its segment files.
///
/// Returned by [`IndexStore::snapshot`].  While any guard is alive, files
/// of segments retired by a concurrent merge are parked in a graveyard
/// instead of being unlinked (`IndexStore::retire_segment_files`), so a
/// scan iterating this snapshot's segment list can always open every
/// segment it references.  Dropping the last guard sweeps the graveyard.
///
/// Derefs to `Arc<IndexSnapshot>` exactly like the
/// `arc_swap::Guard<Arc<IndexSnapshot>>` it wraps, so call sites are
/// source-compatible with the pre-lease `snapshot()`.
pub struct SnapshotReadGuard<'a> {
    snap: arc_swap::Guard<Arc<IndexSnapshot>>,
    store: &'a IndexStore,
}

impl std::ops::Deref for SnapshotReadGuard<'_> {
    type Target = Arc<IndexSnapshot>;
    fn deref(&self) -> &Arc<IndexSnapshot> {
        &self.snap
    }
}

impl Drop for SnapshotReadGuard<'_> {
    fn drop(&mut self) {
        // Last lease out sweeps the graveyard.  `fetch_sub` returning 1
        // means this was the final outstanding lease.
        if self
            .store
            .read_leases
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.store.sweep_retired_segments();
        }
    }
}

// Returned by `IndexStore::fsck_segments()`. Per-section CRC32C is
// computed at write time and validated on every section_checked()
// call. The fast `section()` read path skips revalidation for perf;
// fsck goes back over every section to prove the bytes haven't been
// corrupted at rest.

/// One section's fsck result inside a segment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FsckSectionReport {
    /// `Stored` / `FtsPostings` / `DocValues` / etc. (Debug-stringified
    /// to avoid leaking the SectionType repr to JSON consumers).
    pub kind: String,
    pub ok: bool,
    /// Reason on failure (`section_checked` Err).
    pub error: Option<String>,
}

/// One segment's fsck result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FsckSegmentReport {
    pub segment_id: String,
    pub sections: Vec<FsckSectionReport>,
    /// `Some` if the segment couldn't be opened at all (mmap fail,
    /// missing file, etc.). When present the `sections` vec is empty.
    pub open_error: Option<String>,
}

/// Aggregate fsck report — what `_admin/segments/fsck` returns.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FsckReport {
    pub segments: Vec<FsckSegmentReport>,
    pub total_segments_checked: usize,
    pub total_sections_checked: usize,
    /// Count of sections where the recomputed CRC32C disagreed with
    /// the stored one — i.e. on-disk bit rot. A non-zero value here
    /// is an immediate operator-action signal.
    pub corrupt_sections: usize,
}

// ── StorageMode ───────────────────────────────────────────────────────────────

/// Controls where flushed segments are written.
///
/// - `Local`: segments are written to `data_dir/segments/` (current default).
/// - `ObjectStore`: segments are written to a pluggable backend (S3/GCS/local-sim).
///   Local NVMe is used as a read-through cache: if a segment is present locally
///   it is served from disk, otherwise it is fetched from the backend and cached.
pub enum StorageMode {
    /// All segment data lives in `data_dir` on the local filesystem.
    Local,
    /// Segment data is durably stored in the object-store backend.
    /// The local cache directory is used for read-through caching.
    ObjectStore {
        backend: std::sync::Arc<dyn StorageBackend>,
        /// Local directory used as an NVMe read-through cache.
        cache_dir: PathBuf,
    },
}

impl std::fmt::Debug for StorageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageMode::Local => write!(f, "StorageMode::Local"),
            StorageMode::ObjectStore { cache_dir, .. } => {
                write!(
                    f,
                    "StorageMode::ObjectStore {{ cache_dir: {:?} }}",
                    cache_dir
                )
            }
        }
    }
}

// ── IndexStoreConfig ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct IndexStoreConfig {
    /// Flush the memtable when it exceeds this many bytes.
    pub memtable_max_bytes: usize,
    /// Maximum WAL file size before rotation.
    pub wal_max_size_bytes: u64,
    /// WAL sync mode.
    ///
    /// RC4 W1 #9 — this is now honored EVERYWHERE, including the bulk
    /// paths (`wal_append_batch` / `wal_append_batch_raw`), which
    /// previously forced Batched behaviour and only fsynced via the
    /// `XERJ_STRICT_SYNC` env var — silently ignoring an operator's
    /// explicit `wal_sync = "sync"` opt-in.
    ///
    /// - `Strict`  (`wal_sync = "sync"`): fsync before every ack.  On the
    ///   bulk paths this is one fsync per bulk request (group commit) —
    ///   the same granularity as ES's per-request translog fsync.
    /// - `Batched` (`wal_sync = "batched"`): writes reach the kernel page
    ///   cache before ack (process-crash durable); every dirty WAL shard
    ///   is fsynced within `wal_batch_ms` of the write that dirtied it
    ///   (power-loss window bounded to `wal_batch_ms`) by the shared
    ///   [`crate::wal_fsync`] scheduler.
    pub sync_mode: SyncMode,
    /// RC4 W1 #9 — batched-fsync deadline (milliseconds) applied to a WAL
    /// shard when a write dirties it, `sync_mode == Batched`.  `0` opts the
    /// store out of scheduled fsyncs entirely (used by `wal_sync = "async"`:
    /// never fsync, OS decides — and by unit tests that don't want them).
    pub wal_batch_ms: u64,
    /// Schema version for new segments.
    pub schema_version: u32,
    /// Storage destination for flushed segments.
    pub storage_mode: StorageMode,
    /// Number of independent WAL shards (default: 1 for backward compat).
    /// When > 1, each shard gets its own WAL file (`wal_s{N}/`) for
    /// parallel writes without cross-shard mutex contention.
    pub num_wal_shards: usize,
    /// Rotated WAL generations kept per shard even once every entry in them
    /// is durable in a segment (default: `0` — prune as soon as it is safe).
    ///
    /// This is the retention floor a WAL consumer needs: with the default,
    /// `prune_verified` deletes a rotated generation the moment a flush makes
    /// it redundant, so a #320 tap whose target is down for longer than one
    /// flush interval loses entries. Raising it buys the consumer that much
    /// slack at a cost bounded by `n * wal_max_size_bytes` per shard — a
    /// floor, never a lease, so a stalled consumer still cannot fill the disk.
    pub wal_min_retained_generations: u64,
}

/// A raw JSON batch that has been completely validated before publication.
///
/// Fields are private so callers cannot bypass validation and hand malformed
/// bytes to the WAL writer.  The parsed form is retained only when requested
/// by an engine path that will reuse it for schema/FTS work.
pub type RawJsonDoc = (String, std::sync::Arc<[u8]>);
pub type ParsedRawSources = Vec<std::sync::Arc<serde_json::Value>>;

#[derive(Debug)]
pub struct ValidatedRawBatch {
    docs: Vec<RawJsonDoc>,
    parsed: Option<ParsedRawSources>,
}

impl ValidatedRawBatch {
    pub fn docs(&self) -> &[RawJsonDoc] {
        &self.docs
    }

    pub fn parsed(&self) -> Option<&[std::sync::Arc<serde_json::Value>]> {
        self.parsed.as_deref()
    }

    pub fn into_parts(self) -> (Vec<RawJsonDoc>, Option<ParsedRawSources>) {
        (self.docs, self.parsed)
    }
}

const MAX_RAW_JSON_NESTING: usize = 128;

fn validate_raw_json_nesting(bytes: &[u8]) -> std::result::Result<(), String> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_RAW_JSON_NESTING {
                    return Err(format!(
                        "JSON nesting exceeds the supported limit of {MAX_RAW_JSON_NESTING}"
                    ));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

impl Default for IndexStoreConfig {
    fn default() -> Self {
        Self {
            memtable_max_bytes: 32 * 1024 * 1024,  // 32 MiB
            wal_max_size_bytes: 128 * 1024 * 1024, // 128 MiB
            sync_mode: SyncMode::Batched,
            wal_batch_ms: 0,
            schema_version: 1,
            storage_mode: StorageMode::Local,
            num_wal_shards: 1,
            wal_min_retained_generations: 0,
        }
    }
}

/// RC4 W1 #9 — one-time loud warning when `XERJ_SKIP_WAL` disables the
/// write-ahead log entirely.  Pre-fix the env var was honored silently;
/// an operator (or stray benchmark script) could run a production node
/// with ZERO durability and nothing in the logs saying so.
fn warn_skip_wal_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        warn!(
            "XERJ_SKIP_WAL is set: the write-ahead log is DISABLED. \
             Acknowledged writes exist only in memory until a segment \
             flush — ANY crash loses them. Never use this outside \
             throwaway benchmarks."
        );
        eprintln!(
            "WARNING: XERJ_SKIP_WAL is set — WAL disabled, acked writes are \
             NOT crash-durable. Never use this outside throwaway benchmarks."
        );
    });
}

// ── IndexStore ────────────────────────────────────────────────────────────────

/// Per-index storage engine.
///
/// All public methods are safe to call from multiple threads.  WAL writes are
/// serialized through the internal `Mutex<WalWriter>`; snapshot reads are
/// completely lock-free via `ArcSwap`.
//
// Sharding note: the memtable shard count is not a compile-time constant. It
// lives in `IndexStore.num_shards`, derived at construction from
// `IndexStoreConfig.num_wal_shards.max(1).next_power_of_two()` (usually
// plumbed from `Config.engine.ingest_shards`). Each shard has its own
// `Mutex<Vec<MemEntry>>` — so concurrent bulk paths on different shards don't
// contend on one global lock — and must be a power of two so the shard index
// is `hash & (N-1)`. The previous `pub const MEMTABLE_SHARDS: usize = 16;`
// was a footgun: the static `shard_for_doc_id` helper used it as the modulus
// while the instance `shard_for(&self)` used `self.num_shards`, producing
// inconsistent routing on any deployment that didn't land on 16.
pub struct IndexStore {
    /// Root directory for this index's data files.
    pub data_dir: PathBuf,
    config: IndexStoreConfig,
    /// Serializes monotonic data-directory marker advancement. The durable
    /// writer itself is atomic, but without this latch concurrent future
    /// version transitions could let a lower version overwrite a higher one.
    data_dir_format_lock: Mutex<()>,
    /// Process-local proof that the v2 marker has crossed the platform's
    /// durable-publication boundary: parent-directory fsync on Unix, or
    /// same-directory Win32 write-through replacement on Windows. Merely
    /// observing v2 is insufficient: Unix may expose a rename before its
    /// parent fsync, and an older Windows build may have used the non-durable
    /// directory-sync shim. The format lock serializes the false -> true
    /// transition so concurrent flush/merge retries cannot bypass confirmation.
    fts_v2_marker_durability_confirmed: AtomicBool,
    #[cfg(any(test, feature = "test-hooks"))]
    data_dir_format_write_failpoint: std::sync::atomic::AtomicU8,
    #[cfg(any(test, feature = "test-hooks"))]
    fts_v2_marker_durability_confirmations: AtomicU64,
    /// Actual memtable shard count, derived at open time from
    /// `IndexStoreConfig.num_wal_shards.max(1).next_power_of_two()`.
    num_shards: usize,
    /// Current active segment snapshot.
    snapshot: ArcSwap<IndexSnapshot>,
    /// Sharded WAL writers — each shard has its own WAL file and mutex.
    /// Batches route to a shard by `xxh3(doc_id) & (num_shards - 1)`.
    /// When num_wal_shards=1, this is equivalent to the old single-WAL path.
    ///
    /// Issue #334 — each shard is behind its own `Arc` so the process-wide
    /// batched-fsync scheduler ([`crate::wal_fsync`]) can hold a `Weak` to
    /// exactly the shards that have un-fsynced bytes, instead of this store
    /// owning a thread that polls all of them.
    wal_shards: Vec<Arc<Mutex<WalWriter>>>,
    /// Per-document version map.
    pub version_map: Arc<VersionMap>,
    /// Monotonically increasing sequence number.
    seq_counter: Arc<AtomicU64>,
    /// Pending (un-flushed) memtable entries, sharded by doc_id hash.
    ///
    /// Each bulk ingest batch is routed to exactly ONE shard — all of a
    /// batch's documents live in the same shard so that a single shard
    /// lock protects both the WAL-seq ordering and the memtable push.
    /// Sharding lets N concurrent bulk clients hit N different shards
    /// without serialising on a single global mutex — measured ~3-4×
    /// ingest scaling on multi-client benchmarks.
    memtable_shards: Vec<Mutex<Vec<MemEntry>>>,
    /// Aggregate estimated byte size across all shards.
    memtable_bytes: AtomicU64,
    /// M5.20 — hold-open SegmentReader cache.
    ///
    /// Pre-M5.20 `open_segment` re-opened (File::open + mmap + full-file
    /// CRC validation) every segment on every query.  With 197 segments
    /// and 32 concurrent clients the concurrent QPS bench collapsed to
    /// ~1 QPS / 7.6 s p50 because of repeated mmap syscalls and
    /// gigabytes of redundant CRC work per second.
    ///
    /// Segments are immutable once flushed — we keep one `Arc<SegmentReader>`
    /// per segment_id in a DashMap.  The reader owns its mmap and does
    /// CRC validation exactly once at open time.  Querying threads
    /// only do an `Arc::clone`, no file I/O.
    seg_reader_cache: dashmap::DashMap<String, Arc<crate::segment::SegmentReader>>,
    /// Millis-since-epoch of the last WAL maintenance (checkpoint +
    /// rotate + prune) call.  `finalize_flush_with_publisher` used to
    /// run this loop for ALL 16 WAL shards on EVERY segment flush.
    /// With 16 concurrent shard flushes that's 16 × 16 = 256 lock
    /// acquires + 16 file writes per flush cycle — the dominant cost
    /// once the sync-path refactor eliminated async overhead.  Now we
    /// gate the work with a CAS + time window: at most one caller
    /// every `WAL_MAINTENANCE_INTERVAL_MS` runs it on behalf of all
    /// concurrent flushers.
    last_wal_maintenance_ms: AtomicU64,
    /// Merge-race fix (2026-07) — number of outstanding
    /// [`SnapshotReadGuard`]s (read leases).  Every reader that obtains a
    /// segment list via [`IndexStore::snapshot`] holds a lease for as long
    /// as it keeps the guard, and retired (merged-away) segment files are
    /// only unlinked once this count reaches zero.  See
    /// `retire_segment_files` for the full race description.
    read_leases: std::sync::atomic::AtomicUsize,
    /// Graveyard of segment ids retired by `apply_merge` whose on-disk
    /// files could not be deleted immediately because a read lease was
    /// outstanding.  Swept (files unlinked, reader-cache entries evicted)
    /// by the last lease drop; crash leftovers are handled by the on-open
    /// `cleanup_orphaned_segment_files` (their `.ids` resurrection marker
    /// is already unlinked at retire time, so `recover_orphaned_segments`
    /// can never resurrect them as duplicates).
    retired_segments: Mutex<Vec<SegmentId>>,
    /// #871 — fired after every `self.snapshot` swap that changes the
    /// segment set: flush publication, merge application, tombstone-only
    /// segment persistence, orphan recovery. These are the only events
    /// that can create a merge candidate, so the engine installs a hook
    /// here that debounce-schedules a merge-policy check — mirroring the
    /// #334 wal_fsync design (work is scheduled by the event that creates
    /// it; an idle index costs no timer and no wakeup). `None` (e.g.
    /// storage-crate unit tests) is a no-op. The hook is cloned out of the
    /// mutex before it runs, so it never executes under a store lock.
    segments_changed_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Delete-durability fix (2026-07): `doc_id → (delete seq_no, wal
    /// shard)` for every acknowledged delete whose ONLY durable record
    /// is still its `WalEntry::Delete` in the WAL.
    ///
    /// Background: a delete is expressed as (a) a WAL entry, (b) an
    /// in-RAM version-map tombstone, and (c) the FTS/storage memtables
    /// dropping the doc.  Segment flushes carry NO tombstones the
    /// reopen path can see (`rebuild_version_map_from_segments` loads
    /// every segment-resident doc as live), so until a background merge
    /// physically drops the doc from all segments, the WAL entry is the
    /// only thing standing between an acked delete and resurrection on
    /// restart.  Pre-fix, every flush's WAL maintenance (checkpoint +
    /// force-rotate + prune) destroyed those entries — `prune()` deletes
    /// any rotated generation that has a checkpoint file, regardless of
    /// the checkpoint's `max_seq_no` — so `DELETE → ack → _flush/_refresh
    /// /shutdown → restart` brought the docs back (batch-5 adversarial
    /// verifier, 2026-07-09).
    ///
    /// Invariant: WAL maintenance MUST NOT checkpoint/rotate/prune a WAL
    /// shard that appears in this map ("pinned").  Entries are recorded
    /// BEFORE the WAL append (so a maintenance pass that rechecks under
    /// the WAL shard mutex can never miss a racing delete) and removed
    /// by `sweep_pending_deletes` once the delete is subsumed — i.e. the
    /// doc was re-indexed with a newer seq_no AND that newer version has
    /// been flushed into a real segment.  Deletes that are never
    /// subsumed pin their WAL shard (bounded retention growth on
    /// delete-heavy workloads) — the accepted RC trade-off; the durable
    /// design is segment-level tombstones (see SectionType::Tombstones
    /// note in the flush path).
    pending_deletes: Mutex<std::collections::HashMap<String, (SeqNo, usize)>>,
    /// RC4 W1 #8 follow-up — per-generation verification verdict cache for
    /// the verified WAL prune, keyed by `(wal_shard, generation)`.
    ///
    /// Without it, every 1 s maintenance tick re-decoded EVERY retained
    /// rotated generation end-to-end (LZ4 + serde_json per entry) just to
    /// re-discover that some entries were still unflushed — O(retained WAL
    /// bytes) of parse work per tick, i.e. potentially many seconds' worth
    /// of ingest re-parsed every second on a busy shard between flushes.
    /// Verdicts are stable (durability proofs are monotone: seqs only
    /// grow, a doc's segment residency never reverts to `__memtable__`
    /// for the same-or-older seq), so a generation is decoded ONCE; later
    /// ticks re-check only its remaining unproven `(doc_id, seq)` pairs
    /// against the version map and prune once the list drains.
    wal_prune_cache: Mutex<std::collections::HashMap<(usize, u64), WalGenVerdict>>,
    #[cfg(test)]
    fail_next_snapshot_save: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_snapshot_save_after: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    snapshot_save_failures_remaining: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    fail_next_wal_maintenance: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    publication_failpoint: std::sync::atomic::AtomicU8,
    #[cfg(test)]
    orphan_disarm_fail_after: std::sync::atomic::AtomicUsize,
}

/// Authoritative publication state returned by transactional merge apply.
#[derive(Debug)]
pub enum MergePublicationOutcome {
    Published { maintenance_deferred: bool },
}

/// Failure classification for transactional merge publication.
#[derive(Debug)]
pub enum MergePublicationError {
    NotPublished(StorageError),
    Indeterminate {
        publication: StorageError,
        rollback: StorageError,
    },
}

/// Cached verification state of one rotated WAL generation.
enum WalGenVerdict {
    /// Every entry proven durable-or-superseded — prunable now.
    Durable,
    /// Entries still unproven at last check: `(is_delete, doc_id, seq)`.
    /// Re-verified against the version map on each maintenance tick;
    /// drained-to-empty ⇒ Durable.
    Unproven(Vec<(bool, String, SeqNo)>),
    /// The file failed to decode end-to-end (torn tail from a crash) —
    /// never prunable this process lifetime; skipped without re-decoding.
    Undecodable,
}

const WAL_MAINTENANCE_INTERVAL_MS: u64 = 1_000;

impl IndexStore {
    /// Open (or create) an index at `data_dir`.
    ///
    /// If WAL files exist, they are replayed to rebuild the in-memory state.
    pub fn open(data_dir: impl AsRef<Path>, config: IndexStoreConfig) -> Result<Arc<Self>> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;

        // RC4 W3 #10 — gate on the data-dir format marker BEFORE any
        // destructive step (WAL open, snapshot load, orphan GC). A dir
        // written by a newer xerj, or one with a corrupt marker, is refused
        // here with all data still intact on disk.
        Self::check_data_dir_version(&data_dir)?;

        let wal_dir = data_dir.join("wal");
        let segments_dir = data_dir.join("segments");
        std::fs::create_dir_all(&wal_dir)?;
        std::fs::create_dir_all(&segments_dir)?;

        let seq_counter = Arc::new(AtomicU64::new(1));
        let num_wal_shards = config.num_wal_shards.max(1);
        let mut wal_shards = Vec::with_capacity(num_wal_shards);
        for shard_idx in 0..num_wal_shards {
            let shard_dir = if num_wal_shards == 1 {
                wal_dir.clone()
            } else {
                let d = wal_dir.join(format!("s{shard_idx}"));
                std::fs::create_dir_all(&d)?;
                d
            };
            let mut w = WalWriter::open(
                &shard_dir,
                config.wal_max_size_bytes,
                config.sync_mode,
                Arc::clone(&seq_counter),
            )?;
            // Retention floor for WAL consumers (#320). Zero by default, so
            // this is a no-op unless an operator has asked for slack.
            w.set_min_retained_generations(config.wal_min_retained_generations);
            let shard = Arc::new(Mutex::new(w));
            // Issue #334 — register with the shared, event-driven fsync
            // scheduler instead of spawning a thread per store.  Nothing is
            // scheduled and no thread starts until a write actually dirties
            // this shard.
            if config.sync_mode == SyncMode::Batched && config.wal_batch_ms > 0 {
                let handle = crate::wal_fsync::register(
                    &shard,
                    std::time::Duration::from_millis(config.wal_batch_ms),
                );
                shard.lock().unwrap().set_sync_handle(Some(handle));
            }
            wal_shards.push(shard);
        }

        // Load the persisted snapshot (segment registry). A PRESENT-but-
        // unparseable manifest is refused (Err propagates out of open) rather
        // than silently treated as empty — see `load_snapshot`. A genuinely
        // absent manifest (fresh index) yields an empty snapshot.
        let snapshot = Self::load_snapshot(&data_dir)?.unwrap_or_else(IndexSnapshot::empty);

        let version_map = Arc::new(VersionMap::new());

        let num_shards = config.num_wal_shards.max(1).next_power_of_two();
        let memtable_shards: Vec<Mutex<Vec<MemEntry>>> =
            (0..num_shards).map(|_| Mutex::new(Vec::new())).collect();
        let store = Arc::new(Self {
            data_dir: data_dir.clone(),
            config,
            data_dir_format_lock: Mutex::new(()),
            fts_v2_marker_durability_confirmed: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-hooks"))]
            data_dir_format_write_failpoint: std::sync::atomic::AtomicU8::new(0),
            #[cfg(any(test, feature = "test-hooks"))]
            fts_v2_marker_durability_confirmations: AtomicU64::new(0),
            num_shards,
            snapshot: ArcSwap::from_pointee(snapshot),
            wal_shards,
            version_map: Arc::clone(&version_map),
            seq_counter,
            memtable_shards,
            memtable_bytes: AtomicU64::new(0),
            seg_reader_cache: dashmap::DashMap::new(),
            last_wal_maintenance_ms: AtomicU64::new(0),
            read_leases: std::sync::atomic::AtomicUsize::new(0),
            retired_segments: Mutex::new(Vec::new()),
            segments_changed_hook: Mutex::new(None),
            pending_deletes: Mutex::new(std::collections::HashMap::new()),
            wal_prune_cache: Mutex::new(std::collections::HashMap::new()),
            #[cfg(test)]
            fail_next_snapshot_save: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_snapshot_save_after: std::sync::atomic::AtomicUsize::new(usize::MAX),
            #[cfg(test)]
            snapshot_save_failures_remaining: std::sync::atomic::AtomicUsize::new(1),
            #[cfg(test)]
            fail_next_wal_maintenance: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            publication_failpoint: std::sync::atomic::AtomicU8::new(0),
            #[cfg(test)]
            orphan_disarm_fail_after: std::sync::atomic::AtomicUsize::new(usize::MAX),
        });

        // Rebuild version map from flushed segments first (so WAL replay can
        // correctly override segment entries for recently re-indexed docs).
        store.rebuild_version_map_from_segments()?;

        // V4 M4.5 — snapshot GC on open.  Any file in the segments directory
        // whose UUID is not present in the snapshot is an orphan — either
        // from an incomplete merge (we wrote the output seg and its
        // side-cars but crashed before apply_merge) or from a pre-GC
        // version of xerj.  On the 20 M nginx battle these accumulated
        // to 2.70 GB of zero-value files.
        //
        // 2026-04-25 durability fix: orphans were also being created by
        // a race between `finalize_flush_with_publisher` writing the
        // segment file (step 1) and persisting the snapshot to disk
        // (step 5).  If the process exited between those two steps —
        // which happens on every CLI ingest because background flush
        // tasks aren't joined at exit — the segment file existed but
        // wasn't in the on-disk snapshot, and the next open's cleanup
        // deleted it.  On a 60.9 M-doc CLI ingest we lost 1.76 M docs
        // (2 894 segments × 116 MB) this way — 3 % data loss with no
        // error reported.  Now: BEFORE cleanup, try to recover orphans
        // by reading their `.ids` sidecar (which has doc_count + seq
        // range) and adding them back to the snapshot.  Only files that
        // can't be recovered (truly corrupt or partial) get cleaned.
        let recovered = match store.recover_orphaned_segments() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("orphan recovery failed: {e}");
                0
            }
        };
        if recovered > 0 {
            // Refresh the version map so recovered segments are queryable.
            store.rebuild_version_map_from_segments()?;
        }
        if let Err(e) = store.cleanup_orphaned_segment_files() {
            tracing::warn!("segment-dir GC failed: {e}");
        }

        // SEQ-COUNTER SEEDING (2026-07, S3 root cause): the counter starts at
        // 1 and, pre-fix, was only ever raised from seqs found in surviving
        // WAL files (`WalWriter::open`) and replayed entries.  After a flush
        // + WAL maintenance (checkpoint + rotate + prune) every WAL shard is
        // an empty active generation, so a restart RESET the counter to ~1
        // while segments held seqs up to X — and the stale checkpoint on the
        // active generation (max_seq_no = X) then made the NEXT replay
        // discard every post-restart acked op (seqs 1..K <= X): 100% loss of
        // the post-restart tail.  Seed the counter from the durable segment
        // metadata (snapshot.max_seq_no plus every registered/recovered
        // segment's max_seq_no) so global seq monotonicity holds across
        // restarts — the invariant every checkpoint and version-map
        // comparison silently assumes.
        {
            let snap = store.snapshot.load();
            let durable_max = snap
                .segments
                .iter()
                .map(|s| s.max_seq_no)
                .max()
                .unwrap_or(0)
                .max(snap.max_seq_no);
            drop(snap);
            if durable_max > 0 {
                store
                    .seq_counter
                    .fetch_max(durable_max + 1, Ordering::AcqRel);
            }
        }

        // Replay WAL to rebuild in-memory state (these override segment entries).
        store.replay_wal(&wal_dir)?;

        // RC4 W1 #9 — the `wal_batch_ms` fsync loop.  The config has
        // documented `wal_sync = "batched"` as "fsync every wal_batch_ms"
        // since 0.x, but nothing implemented it: the only fsyncs happened
        // at flush/rotate boundaries, so the real power-loss window was
        // unbounded (up to `flush_interval_secs`).
        //
        // Issue #334 — that first implementation spawned a dedicated OS
        // thread PER STORE which slept `wal_batch_ms` and polled its shards
        // for dirt.  On a node holding many indices (which is what `xerj
        // autoindex` produces — one index per inferred dataset per repo)
        // that is one thread and `1000 / wal_batch_ms` wakeups per second
        // per index, forever, even with zero clients and zero writes: 9 382
        // indices measured at 9 709 threads, ~197 k context switches/s and
        // 718-760 % CPU while completely idle.
        //
        // The registration now happens per WAL shard at construction above,
        // and the fsync itself is driven by the process-wide, event-driven
        // scheduler in `wal_fsync`: a shard is queued when a write dirties
        // it and dequeued when it has been fsynced, so an index that is not
        // being written to costs no thread, no timer and no wakeup.  Strict
        // mode still fsyncs inline per request and Async mode still opts out
        // entirely (`store_config_from` forces `wal_batch_ms = 0` for both),
        // so neither registers a handle.

        // RC4 W3 #10 — the open fully succeeded; stamp the data-dir format
        // marker so this and future opens are versioned. Fresh dirs and
        // upgraded rc3-vintage dirs (no prior marker) get stamped; an
        // existing marker is left untouched. This runs only after every
        // recovery/GC step above succeeded, so a refused open never leaves a
        // misleading marker behind.
        Self::stamp_data_dir_version(&data_dir)?;

        info!(data_dir = ?data_dir, "IndexStore opened");
        Ok(store)
    }

    /// Try to add orphan segment files back to the snapshot before
    /// `cleanup_orphaned_segment_files` deletes them.
    ///
    /// An orphan is a segment file (e.g. `<uuid>.seg`) whose UUID isn't
    /// in the current snapshot.  Pre-this-fix the cleanup deleted them
    /// unconditionally, which on CLI ingest workloads (where background
    /// flush tokio tasks aren't joined at process exit) lost segments
    /// that had been written to disk but hadn't yet reached the
    /// `save_snapshot()` step at line ~838 of `finalize_flush_with_publisher`.
    ///
    /// Recovery strategy: legacy `ZID1`/`ZID2` flushes use their `.ids`
    /// sidecar plus strict segment/header validation. New `ZID3` flushes
    /// additionally require a valid `.complete` manifest that binds the
    /// entire artifact family. If the applicable evidence is missing or
    /// corrupt, cleanup removes the orphan instead of publishing it.
    ///
    /// Returns the number of segments recovered.
    fn recover_orphaned_segments(&self) -> Result<usize> {
        let segments_dir = self.data_dir.join("segments");
        if !segments_dir.exists() {
            return Ok(0);
        }

        let snap = self.snapshot.load();
        let live_ids: std::collections::HashSet<String> =
            snap.segments.iter().map(|s| s.id.to_string()).collect();
        drop(snap);

        let mut recovered: Vec<SegmentMeta> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Only process the primary `.seg` file once per UUID.
            if !name_str.ends_with(".seg") {
                continue;
            }
            if name_str.len() < 40 {
                continue;
            } // 36 UUID + ".seg"
            let prefix = &name_str[..36];
            if prefix.as_bytes().get(8) != Some(&b'-') {
                continue;
            }
            if live_ids.contains(prefix) {
                continue;
            }
            if !seen_ids.insert(prefix.to_string()) {
                continue;
            }

            let seg_filename = format!("{prefix}.seg");
            let seg_path = segments_dir.join(&seg_filename);
            let ids_path = segments_dir.join(format!("{prefix}.ids"));

            // RC4 W2 #14 — TOMBSTONE-ONLY orphan (doc_count 0, ZTB2
            // section, no `.ids` — written by `persist_pending_tombstones`
            // whose crash-window is prune-before-snapshot-save).  The
            // `.seg` file itself is the durability marker: `finish()`
            // fsyncs file + dir before anything can reference it, and
            // `SegmentReader::open` re-validates the whole-file CRC here.
            // Without resurrection the on-open cleanup would delete it —
            // and with the delete's WAL entry already pruned, the doc
            // would come back from the dead.
            if let Ok(reader) = SegmentReader::open(&seg_path) {
                let hdr = reader.header();
                if hdr.doc_count == 0 && (hdr.flags & 0x0001) != 0 {
                    let seg_meta = match std::fs::metadata(&seg_path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    recovered.push(SegmentMeta {
                        id: prefix.to_string(),
                        doc_count: 0,
                        size_bytes: seg_meta.len(),
                        min_seq_no: hdr.min_seq_no,
                        max_seq_no: hdr.max_seq_no,
                        created_at_ms: hdr.created_at_ms,
                        has_tombstones: true,
                        seg_path: seg_filename,
                        sidx_path: format!("{prefix}.sidx"),
                    });
                    continue;
                }
            }

            // Every generation needs a valid ids payload. For legacy ZID1/2
            // it is recovery evidence only when the segment header agrees;
            // ZID3 additionally requires its completion manifest below.
            let ids_bytes = match std::fs::read(&ids_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if ids_bytes.len() < 8 {
                continue;
            }
            let magic = &ids_bytes[..4];
            if magic != b"ZID1" && magic != b"ZID2" && magic != b"ZID3" {
                continue;
            }
            if magic == b"ZID3" && !segments_dir.join(format!("{prefix}.complete")).exists() {
                continue;
            }
            let num_docs =
                u32::from_le_bytes([ids_bytes[4], ids_bytes[5], ids_bytes[6], ids_bytes[7]]) as u64;
            if num_docs == 0 {
                continue;
            }
            let body: Vec<u8> = if magic == b"ZID2" || magic == b"ZID3" {
                match lz4_flex::decompress_size_prepended(&ids_bytes[8..]) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            } else {
                ids_bytes[8..].to_vec()
            };
            let mut min_seq = u64::MAX;
            let mut max_seq = 0u64;
            let mut pos = 0usize;
            let mut parsed = 0u64;
            for _ in 0..num_docs {
                if pos + 10 > body.len() {
                    break;
                }
                let seq = u64::from_le_bytes(body[pos..pos + 8].try_into().unwrap());
                pos += 8;
                let id_len = u16::from_le_bytes(body[pos..pos + 2].try_into().unwrap()) as usize;
                pos += 2;
                if pos + id_len > body.len() {
                    break;
                }
                if std::str::from_utf8(&body[pos..pos + id_len]).is_err() {
                    break;
                }
                pos += id_len;
                min_seq = min_seq.min(seq);
                max_seq = max_seq.max(seq);
                parsed += 1;
            }
            if parsed != num_docs || pos != body.len() || min_seq == u64::MAX {
                continue;
            }

            // Sanity-check the segment file itself opens, and preserve
            // its tombstone flag (RC4 W2 #14 — a resurrected
            // delete-carrying segment must keep `has_tombstones` so the
            // rebuild applies its ZTB2 pairs and the deletes_present
            // gates stay on).
            let has_tombstones = match SegmentReader::open(&seg_path) {
                Ok(r)
                    if r.header().doc_count == num_docs
                        && r.header().min_seq_no == min_seq
                        && r.header().max_seq_no == max_seq =>
                {
                    let complete_path = segments_dir.join(format!("{prefix}.complete"));
                    if complete_path.exists()
                        && !self
                            .validate_flush_completion_manifest(prefix, num_docs, min_seq, max_seq)
                    {
                        continue;
                    }
                    (r.header().flags & 0x0001) != 0
                }
                Err(_) => continue,
                Ok(_) => continue,
            };

            let seg_meta = match std::fs::metadata(&seg_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let created_at_ms = SegmentReader::open(&seg_path)
                .map(|reader| reader.header().created_at_ms)
                .unwrap_or(0);

            recovered.push(SegmentMeta {
                id: prefix.to_string(),
                doc_count: num_docs,
                size_bytes: seg_meta.len(),
                min_seq_no: min_seq,
                max_seq_no: max_seq,
                created_at_ms,
                has_tombstones,
                seg_path: seg_filename,
                sidx_path: format!("{prefix}.sidx"),
            });
        }

        if recovered.is_empty() {
            return Ok(0);
        }

        // Build a new snapshot with all recovered segments and persist it.
        let mut new_snap: IndexSnapshot = (*self.snapshot.load()).as_ref().clone();
        let total_docs: u64 = recovered.iter().map(|m| m.doc_count).sum();
        let total_bytes: u64 = recovered.iter().map(|m| m.size_bytes).sum();
        for meta in &recovered {
            new_snap = new_snap.with_new_segment(meta.clone());
        }
        self.snapshot.store(Arc::new(new_snap));
        self.notify_segments_changed();
        // Persist immediately so a second restart doesn't need to re-recover.
        self.save_snapshot()?;

        info!(
            recovered_segments = recovered.len(),
            recovered_docs = total_docs,
            recovered_mb = total_bytes / 1_000_000,
            "orphaned segments recovered into snapshot (durability fix)"
        );
        Ok(recovered.len())
    }

    /// Delete every file in `segments/` whose UUID prefix isn't referenced
    /// by the current snapshot.  Called on `open()` after the snapshot has
    /// been loaded.
    fn cleanup_orphaned_segment_files(&self) -> Result<()> {
        let segments_dir = self.data_dir.join("segments");
        if !segments_dir.exists() {
            return Ok(());
        }

        // Build the set of live segment UUIDs from the current snapshot.
        let snap = self.snapshot.load();
        let live_ids: std::collections::HashSet<String> =
            snap.segments.iter().map(|s| s.id.to_string()).collect();
        drop(snap);

        let mut removed_files = 0usize;
        let mut removed_bytes = 0u64;
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Segment filenames look like "<36-char UUID>.<suffix>".
            // Skip anything that doesn't start with a UUID-shaped prefix.
            if name_str.len() < 37 {
                continue;
            }
            let prefix = &name_str[..36];
            if prefix.as_bytes().get(8) != Some(&b'-') {
                continue;
            }
            if live_ids.contains(prefix) {
                continue;
            }
            let path = entry.path();
            let sz = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(&path).is_ok() {
                removed_files += 1;
                removed_bytes += sz;
            }
        }
        if removed_files > 0 {
            info!(
                removed_files,
                removed_mb = removed_bytes / 1_000_000,
                "orphaned segment files cleaned up on open"
            );
        }
        Ok(())
    }

    /// Write the `<segment_id>.ids` side-car from `(seq_no, doc_id)` pairs
    /// (ZID2 format — see the format comment at the flush-time call in
    /// `finalize_flush_with_publisher`).  Shared by the flush path and the
    /// engine merge task: pre-2026-07 only flush wrote the side-car, so
    /// merge-output segments always fell back to the slow decode-stored
    /// path in `rebuild_version_map_from_segments` on reopen (the very
    /// path the side-car exists to avoid — ~302 s vs ~5 s cold restart on
    /// the 66.5 M-doc workload).
    pub fn write_ids_sidecar(
        &self,
        segment_id: &str,
        pairs: &[(u64, &str)],
    ) -> std::io::Result<()> {
        self.write_ids_sidecar_with_magic(segment_id, pairs, b"ZID2")
    }

    fn write_ids_sidecar_v3(&self, segment_id: &str, pairs: &[(u64, &str)]) -> std::io::Result<()> {
        self.write_ids_sidecar_with_magic(segment_id, pairs, b"ZID3")
    }

    fn write_ids_sidecar_with_magic(
        &self,
        segment_id: &str,
        pairs: &[(u64, &str)],
        magic: &[u8; 4],
    ) -> std::io::Result<()> {
        let mut body: Vec<u8> =
            Vec::with_capacity(pairs.iter().map(|(_, id)| 8 + 2 + id.len()).sum::<usize>());
        for (seq_no, id) in pairs {
            body.extend_from_slice(&seq_no.to_le_bytes());
            body.extend_from_slice(&(id.len() as u16).to_le_bytes());
            body.extend_from_slice(id.as_bytes());
        }
        let compressed = lz4_flex::compress_prepend_size(&body);
        let mut buf: Vec<u8> = Vec::with_capacity(8 + compressed.len());
        buf.extend_from_slice(magic);
        buf.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
        buf.extend_from_slice(&compressed);
        let ids_path = self
            .data_dir
            .join("segments")
            .join(format!("{segment_id}.ids"));
        // RC4 W1 #10 — durable write (tmp + fsync + rename + dir fsync).
        // The `.ids` side-car carries the durable id/sequence payload used by
        // recovery. ZID3 is not considered complete until the separately
        // durable `.complete` manifest validates the full artifact family.
        xerj_common::fsio::write_file_durable(&ids_path, &buf)
    }

    /// Persist the flush completion manifest after every required artifact.
    /// The manifest is an integrity envelope (IEEE CRC-32, not a cryptographic
    /// identity): it binds the segment header coordinates and the exact set
    /// of artifact names, sizes, and per-file CRCs visible at publication.
    fn write_flush_completion_manifest(&self, meta: &SegmentMeta) -> std::io::Result<()> {
        let segments_dir = self.data_dir.join("segments");
        let prefix = format!("{}.", meta.id);
        let mut artifacts = Vec::new();
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || name.ends_with(".complete") {
                continue;
            }
            let metadata = entry.metadata()?;
            let crc = Self::stream_file_crc32(&entry.path())?;
            artifacts.push((name, metadata.len(), crc));
        }
        artifacts.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        if !Self::valid_flush_artifact_set(&meta.id, &artifacts) {
            return Err(std::io::Error::other(
                "flush artifact set is incomplete or contains an unknown role",
            ));
        }
        let mut body = Vec::new();
        body.extend_from_slice(b"ZCM1");
        body.extend_from_slice(&(meta.id.len() as u16).to_le_bytes());
        body.extend_from_slice(meta.id.as_bytes());
        body.extend_from_slice(&meta.doc_count.to_le_bytes());
        body.extend_from_slice(&meta.min_seq_no.to_le_bytes());
        body.extend_from_slice(&meta.max_seq_no.to_le_bytes());
        body.extend_from_slice(&(artifacts.len() as u32).to_le_bytes());
        for (name, size, crc) in artifacts {
            body.extend_from_slice(&(name.len() as u16).to_le_bytes());
            body.extend_from_slice(name.as_bytes());
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(&crc.to_le_bytes());
        }
        let envelope_crc = crc32fast::hash(&body);
        body.extend_from_slice(&envelope_crc.to_le_bytes());
        xerj_common::fsio::write_file_durable(
            &segments_dir.join(format!("{}.complete", meta.id)),
            &body,
        )
    }

    fn stream_file_crc32(path: &Path) -> std::io::Result<u32> {
        Self::stream_crc32_from_reader(std::fs::File::open(path)?)
    }

    fn stream_crc32_from_reader(mut reader: impl std::io::Read) -> std::io::Result<u32> {
        let mut hasher = crc32fast::Hasher::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.finalize())
    }

    fn valid_flush_artifact_set(segment_id: &str, artifacts: &[(String, u64, u32)]) -> bool {
        let prefix = format!("{segment_id}.");
        let mut roles = std::collections::HashSet::new();
        let mut fts: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
            std::collections::HashMap::new();
        for (name, _, _) in artifacts {
            if !name.starts_with(&prefix) || name.contains('/') || name.contains('\\') {
                return false;
            }
            let rest = &name[prefix.len()..];
            if matches!(rest, "seg" | "sidx" | "ids" | "dv" | "fts-layout-v2") {
                if !roles.insert(rest) {
                    return false;
                }
                continue;
            }
            let Some((field, extension)) = rest.rsplit_once('.') else {
                return false;
            };
            if field.is_empty()
                || field.contains("..")
                || !matches!(extension, "fst" | "post" | "meta" | "norms")
                || !fts.entry(field).or_default().insert(extension)
            {
                return false;
            }
        }
        roles.contains("seg")
            && roles.contains("sidx")
            && roles.contains("ids")
            && fts.values().all(|extensions| {
                ["fst", "post", "meta", "norms"]
                    .iter()
                    .all(|extension| extensions.contains(extension))
            })
    }

    fn validate_flush_completion_manifest(
        &self,
        segment_id: &str,
        doc_count: u64,
        min_seq_no: u64,
        max_seq_no: u64,
    ) -> bool {
        let dir = self.data_dir.join("segments");
        const MAX_COMPLETION_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
        let path = dir.join(format!("{segment_id}.complete"));
        let Ok(metadata) = std::fs::metadata(&path) else {
            return false;
        };
        if metadata.len() > MAX_COMPLETION_MANIFEST_BYTES {
            return false;
        }
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        if bytes.len() < 4 + 2 + 8 * 3 + 4 + 4 || &bytes[..4] != b"ZCM1" {
            return false;
        }
        let payload_len = bytes.len() - 4;
        let expected_crc = u32::from_le_bytes(bytes[payload_len..].try_into().unwrap());
        if crc32fast::hash(&bytes[..payload_len]) != expected_crc {
            return false;
        }
        let mut pos = 4usize;
        let take_u16 = |bytes: &[u8], pos: &mut usize| -> Option<u16> {
            let value = u16::from_le_bytes(bytes.get(*pos..*pos + 2)?.try_into().ok()?);
            *pos += 2;
            Some(value)
        };
        let take_u32 = |bytes: &[u8], pos: &mut usize| -> Option<u32> {
            let value = u32::from_le_bytes(bytes.get(*pos..*pos + 4)?.try_into().ok()?);
            *pos += 4;
            Some(value)
        };
        let take_u64 = |bytes: &[u8], pos: &mut usize| -> Option<u64> {
            let value = u64::from_le_bytes(bytes.get(*pos..*pos + 8)?.try_into().ok()?);
            *pos += 8;
            Some(value)
        };
        let Some(id_len) = take_u16(&bytes, &mut pos).map(usize::from) else {
            return false;
        };
        let Some(id_bytes) = bytes.get(pos..pos + id_len) else {
            return false;
        };
        pos += id_len;
        if id_bytes != segment_id.as_bytes()
            || take_u64(&bytes, &mut pos) != Some(doc_count)
            || take_u64(&bytes, &mut pos) != Some(min_seq_no)
            || take_u64(&bytes, &mut pos) != Some(max_seq_no)
        {
            return false;
        }
        let Some(count) = take_u32(&bytes, &mut pos) else {
            return false;
        };
        const MAX_MANIFEST_ARTIFACTS: u32 = 4096;
        if count > MAX_MANIFEST_ARTIFACTS || count as usize > payload_len.saturating_sub(pos) / 14 {
            return false;
        }
        let mut declared = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let Some(name_len) = take_u16(&bytes, &mut pos).map(usize::from) else {
                return false;
            };
            if name_len == 0 || name_len > 512 {
                return false;
            }
            let Some(name_end) = pos.checked_add(name_len) else {
                return false;
            };
            let Some(name) = bytes.get(pos..name_end) else {
                return false;
            };
            pos = name_end;
            let Ok(name) = std::str::from_utf8(name) else {
                return false;
            };
            let Some(size) = take_u64(&bytes, &mut pos) else {
                return false;
            };
            let Some(crc) = take_u32(&bytes, &mut pos) else {
                return false;
            };
            declared.push((name.to_owned(), size, crc));
        }
        if pos != payload_len {
            return false;
        }
        let prefix = format!("{segment_id}.");
        let mut actual = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || name.ends_with(".complete") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                return false;
            };
            let Ok(crc) = Self::stream_file_crc32(&entry.path()) else {
                return false;
            };
            actual.push((name, metadata.len(), crc));
        }
        actual.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Self::valid_flush_artifact_set(segment_id, &declared) && declared == actual
    }

    /// Unlink every on-disk file belonging to the given segment ids — the
    /// primary `.seg` plus all side-cars (`.sidx`, `.ids`, `.dv`,
    /// `.<field>.post` / `.fst` / `.meta` / `.norms`) and the optional
    /// `.fts-layout-v2` discriminator.
    ///
    /// Disk-space fix (2026-07): called by the engine merge task right
    /// after `apply_merge` commits, so merged-away input segments are
    /// reclaimed immediately instead of lingering until the next process
    /// restart (`cleanup_orphaned_segment_files` only runs on `open()`;
    /// on the 1 M-doc benchmark that left ~137 MB of dead segment files
    /// on disk).  Deleting them at commit time also prevents
    /// `recover_orphaned_segments` from resurrecting stale pre-merge
    /// segments (they still carry a valid `.ids` side-car) on restart.
    ///
    /// Unlinking under a live mmap is safe on Linux: snapshot readers
    /// that already opened the segment keep their mappings; the blocks
    /// are freed once the last reader drops.  Errors are best-effort —
    /// anything left behind is picked up by the on-open cleanup.
    ///
    /// Returns `(files_removed, bytes_removed)` for logging.
    pub fn delete_segment_files(&self, segment_ids: &[SegmentId]) -> (usize, u64) {
        let segments_dir = self.data_dir.join("segments");
        let ids: std::collections::HashSet<&str> = segment_ids.iter().map(|s| s.as_str()).collect();
        if ids.is_empty() {
            return (0, 0);
        }
        let entries = match std::fs::read_dir(&segments_dir) {
            Ok(e) => e,
            Err(_) => return (0, 0),
        };
        let mut removed_files = 0usize;
        let mut removed_bytes = 0u64;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Segment filenames look like "<36-char UUID>.<suffix>".
            if name_str.len() < 37 {
                continue;
            }
            let prefix = &name_str[..36];
            if !ids.contains(prefix) {
                continue;
            }
            let sz = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(entry.path()).is_ok() {
                removed_files += 1;
                removed_bytes += sz;
            }
        }
        (removed_files, removed_bytes)
    }

    /// Durably disarm ZID3 orphan recovery for snapshot-listed merge inputs
    /// before the replacement snapshot is committed.
    ///
    /// Completion markers are removed before ids payloads. Removing ids is
    /// required for legacy ZID1/ZID2 inputs, where ids alone are recovery
    /// evidence. Snapshot-listed readers and restart rebuilds retain their
    /// exact stored-section fallback when ids are absent.
    pub fn disarm_orphan_recovery_for_segments(&self, segment_ids: &[SegmentId]) -> Result<()> {
        // The hot flush path may debounce snapshot persistence. Persist the
        // currently authoritative input set before removing its recovery
        // markers; otherwise an aborted merge could make a recently flushed
        // input neither snapshot-listed nor recoverable after restart.
        self.save_snapshot()?;
        self.unlink_recovery_markers(segment_ids, true)
    }

    /// Durably disarm every orphan-recovery generation for unpublished output.
    ///
    /// Completion markers are always attempted before ids. All requested
    /// removals are attempted even after an error, and the directory is
    /// fsynced before returning.
    fn disarm_unpublished_segments(&self, segment_ids: &[SegmentId]) -> Result<()> {
        self.unlink_recovery_markers(segment_ids, true)
    }

    fn rollback_unpublished_segment(&self, segment_id: &SegmentId) -> Result<()> {
        let disarm = self.disarm_unpublished_segments(std::slice::from_ref(segment_id));
        // Marker removal is the durability boundary. Remaining segment and
        // sidecar files are unreachable cleanup and can be removed afterward.
        self.delete_segment_files(std::slice::from_ref(segment_id));
        disarm
    }

    fn abandon_unpublished_segment(
        &self,
        segment_id: &SegmentId,
        publication_error: StorageError,
    ) -> StorageError {
        match self.rollback_unpublished_segment(segment_id) {
            Ok(()) => publication_error,
            Err(cleanup_error) => StorageError::Io(std::io::Error::other(format!(
                "publication failed ({publication_error}); orphan-recovery disarm also failed ({cleanup_error})"
            ))),
        }
    }

    fn unlink_recovery_markers(
        &self,
        segment_ids: &[SegmentId],
        remove_legacy_ids: bool,
    ) -> Result<()> {
        if segment_ids.is_empty() {
            return Ok(());
        }
        let segments_dir = self.data_dir.join("segments");
        let mut first_error: Option<std::io::Error> = None;
        let mut operation = 0usize;
        let suffixes: &[&str] = if remove_legacy_ids {
            &["complete", "ids"]
        } else {
            &["complete"]
        };
        for suffix in suffixes {
            for id in segment_ids {
                let path = segments_dir.join(format!("{}.{suffix}", id.as_str()));
                if let Err(error) = std::fs::remove_file(&path) {
                    if error.kind() != std::io::ErrorKind::NotFound && first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                #[cfg(test)]
                if self.orphan_disarm_fail_after.load(Ordering::Acquire) == operation
                    && first_error.is_none()
                {
                    first_error = Some(std::io::Error::other(format!(
                        "injected orphan-recovery disarm failure after {}",
                        path.display()
                    )));
                }
                operation = operation.saturating_add(1);
            }
        }
        if let Err(error) = xerj_common::fsio::fsync_dir(&segments_dir) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(StorageError::Io(std::io::Error::other(format!(
                "orphan-recovery disarm incomplete: {error}"
            )))),
            None => Ok(()),
        }
    }

    /// Retire merged-away segments: delete their files **as soon as it is
    /// safe**, i.e. once no in-flight reader can still be holding a
    /// pre-merge segment list that references them.
    ///
    /// Merge-race fix (2026-07): `run_merge_once` used to call
    /// [`delete_segment_files`](Self::delete_segment_files) directly after
    /// `apply_merge`.  A search that had already loaded the pre-merge
    /// snapshot would then hit `open_segment_arc` for a segment whose
    /// files had just been unlinked, get an error, and SILENTLY SKIP the
    /// segment — returning an undercounted `hits.total` (observed live:
    /// 798,281 instead of 932,037).  Now every reader holds a
    /// [`SnapshotReadGuard`] lease; if any lease is outstanding the ids
    /// are parked in `retired_segments` and swept by the last lease drop.
    ///
    /// Recovery evidence is unlinked IMMEDIATELY regardless of leases:
    /// `.complete` first, then `.ids`. The segments directory is fsynced
    /// before the ids enter the in-memory graveyard, so a crash while data
    /// deletion is deferred cannot resurrect merged-away inputs. Any other
    /// leftover files are removed by on-open orphan cleanup.
    ///
    /// Returns `(files_removed, bytes_removed)` — `(0, 0)` when deletion
    /// was deferred to the graveyard.
    pub fn retire_segment_files(&self, segment_ids: &[SegmentId]) -> Result<(usize, u64)> {
        if segment_ids.is_empty() {
            return Ok((0, 0));
        }
        // Merge publication already removed completion markers before
        // apply_merge. Remove ids as well before entering the graveyard.
        self.disarm_unpublished_segments(segment_ids)?;
        {
            let mut graveyard = self.retired_segments.lock().unwrap();
            graveyard.extend_from_slice(segment_ids);
        }
        // Opportunistic sweep: deletes right away when no reader is active
        // (the common case), otherwise the last lease drop sweeps.
        Ok(self.sweep_retired_segments())
    }

    /// Delete the files of every graveyard segment, provided no read
    /// lease is outstanding.  Called by [`retire_segment_files`] and by
    /// the last [`SnapshotReadGuard`] drop.
    ///
    /// The lease check happens while holding the graveyard lock, and
    /// `snapshot()` increments the lease count with a SeqCst RMW *before*
    /// loading the snapshot pointer (itself stored by `apply_merge`
    /// before retire).  So if this observes `read_leases == 0`, any
    /// reader that appears afterwards is guaranteed to load the
    /// post-merge snapshot and can never reference the ids being swept.
    fn sweep_retired_segments(&self) -> (usize, u64) {
        let ids: Vec<SegmentId> = {
            let mut graveyard = self.retired_segments.lock().unwrap();
            if graveyard.is_empty()
                || self.read_leases.load(std::sync::atomic::Ordering::SeqCst) != 0
            {
                return (0, 0);
            }
            graveyard.drain(..).collect()
        };
        // Evict any reader-cache entries a leased scan may have re-opened
        // for these ids so their mmaps (and the unlinked blocks) get
        // released.
        for id in &ids {
            self.seg_reader_cache.remove(id.as_str());
        }
        let (files, bytes) = self.delete_segment_files(&ids);
        debug!(
            segments = ids.len(),
            removed_files = files,
            removed_bytes = bytes,
            "retired segment files swept"
        );
        (files, bytes)
    }

    // ── Shard routing ─────────────────────────────────────────────────────────

    /// Route a doc_id to its memtable shard using the *runtime* shard
    /// count (configured via `IndexStoreConfig.num_wal_shards`). All
    /// operations on a given doc_id (index, delete, replay) target the
    /// same shard so per-doc write ordering is preserved without a
    /// global lock.
    ///
    /// The previous `pub fn shard_for_doc_id(doc_id) -> usize` was a
    /// static helper that hardcoded `MEMTABLE_SHARDS - 1` (=15). On any
    /// machine where `num_wal_shards != 16` it disagreed with this
    /// instance method, leading to either silent shard misrouting or an
    /// out-of-bounds panic in `memtable_shards[shard_idx]` when
    /// `num_wal_shards < 16`. Removed.
    #[inline]
    pub fn shard_for(&self, doc_id: &str) -> usize {
        let h = xxhash_rust::xxh3::xxh3_64(doc_id.as_bytes());
        (h as usize) & (self.num_shards - 1)
    }

    /// Number of memtable shards this store was opened with.
    pub fn num_memtable_shards(&self) -> usize {
        self.num_shards
    }

    /// Route a doc_id to its WAL shard index.
    #[inline]
    fn wal_shard_for(&self, doc_id: &str) -> usize {
        if self.wal_shards.len() == 1 {
            return 0;
        }
        let h = xxhash_rust::xxh3::xxh3_64(doc_id.as_bytes());
        (h as usize) & (self.wal_shards.len() - 1)
    }

    /// Lock a specific WAL shard.
    #[inline]
    fn wal_lock_shard(&self, shard: usize) -> std::sync::MutexGuard<'_, WalWriter> {
        self.wal_shards[shard].lock().unwrap()
    }

    // ── Write path ────────────────────────────────────────────────────────────

    /// Index a document.  Returns the assigned sequence number.
    pub fn index(&self, doc_id: impl Into<String>, source: serde_json::Value) -> Result<SeqNo> {
        let doc_id = doc_id.into();
        let entry = WalEntry::Index {
            doc_id: doc_id.clone(),
            source: source.clone(),
        };

        let seq_no = {
            let ws = self.wal_shard_for(&doc_id);
            let mut wal = self.wal_lock_shard(ws);
            wal.append(&entry)?
        };

        let source_len = source.to_string().len();
        self.version_map
            .set(&doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);

        let shard = self.shard_for(&doc_id);
        let mut mem = self.memtable_shards[shard].lock().unwrap();
        mem.push(MemEntry {
            seq_no,
            doc_id,
            source: Some(std::sync::Arc::new(source)),
            source_bytes: std::sync::Arc::from(&[][..]),
        });
        self.memtable_bytes
            .fetch_add(source_len as u64, Ordering::Relaxed);

        debug!(seq_no, "document indexed");
        Ok(seq_no)
    }

    /// Batch-index multiple documents in a single WAL lock acquisition.
    /// Much faster than calling `index()` in a loop because:
    /// 1. One mutex lock for the entire batch (not N locks)
    /// 2. WAL entries written sequentially without releasing the lock
    /// 3. One memtable lock for the entire batch
    pub fn index_batch(&self, docs: &[(String, serde_json::Value)]) -> Result<Vec<SeqNo>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        let mut seq_nos = Vec::with_capacity(docs.len());

        // Route batch to WAL shard of first doc (matches memtable shard routing)
        {
            let ws = if docs.is_empty() {
                0
            } else {
                self.wal_shard_for(&docs[0].0)
            };
            let mut wal = self.wal_lock_shard(ws);
            for (doc_id, source) in docs {
                let entry = WalEntry::Index {
                    doc_id: doc_id.clone(),
                    source: source.clone(),
                };
                let seq_no = wal.append(&entry)?;
                seq_nos.push(seq_no);
            }
        }

        // Version map + memtable updates — each doc routed to its
        // own shard.  We acquire each shard lock lazily so that most
        // pushes (small batches) only touch 1-2 shards.
        for (i, (doc_id, source)) in docs.iter().enumerate() {
            let seq_no = seq_nos[i];
            self.version_map
                .set(doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);
            let source_len = source.to_string().len();
            let shard = self.shard_for(doc_id);
            let mut mem = self.memtable_shards[shard].lock().unwrap();
            mem.push(MemEntry {
                seq_no,
                doc_id: doc_id.clone(),
                source: Some(std::sync::Arc::new(source.clone())),
                source_bytes: std::sync::Arc::from(&[][..]),
            });
            drop(mem);
            self.memtable_bytes
                .fetch_add(source_len as u64, Ordering::Relaxed);
        }

        Ok(seq_nos)
    }

    /// Delete a document.  Returns the assigned sequence number, or `None` if
    /// the document did not exist.
    pub fn delete(&self, doc_id: impl AsRef<str>) -> Result<Option<SeqNo>> {
        let doc_id = doc_id.as_ref();
        if self.version_map.get(doc_id).is_none() {
            return Ok(None);
        }

        let entry = WalEntry::Delete {
            doc_id: doc_id.to_owned(),
        };
        let ws = self.wal_shard_for(doc_id);
        // Delete-durability: pin this WAL shard BEFORE appending the
        // Delete entry.  WAL maintenance rechecks `pending_deletes`
        // under the WAL shard mutex, so ordering the map insert before
        // the append guarantees maintenance can never checkpoint+rotate+
        // prune a generation containing this Delete: if maintenance
        // acquired the shard mutex after our append, our insert is
        // already visible to its recheck.  The placeholder seq_no is
        // fixed up right after the append assigns the real one.
        self.pending_deletes
            .lock()
            .unwrap()
            .insert(doc_id.to_owned(), (SeqNo::MAX, ws));
        let seq_no = {
            let mut wal = self.wal_lock_shard(ws);
            match wal.append(&entry) {
                Ok(s) => s,
                Err(e) => {
                    // Nothing reached the WAL — unpin so the shard's
                    // maintenance isn't blocked forever by a failed op.
                    self.pending_deletes.lock().unwrap().remove(doc_id);
                    return Err(e);
                }
            }
        };
        if let Some(slot) = self.pending_deletes.lock().unwrap().get_mut(doc_id) {
            // Keep the larger seq if a concurrent re-delete raced us.
            if slot.0 == SeqNo::MAX || slot.0 < seq_no {
                slot.0 = seq_no;
            }
        }

        self.version_map
            .delete(doc_id, seq_no, IN_MEMORY_SEGMENT_ID)?;

        let shard = self.shard_for(doc_id);
        let mut mem = self.memtable_shards[shard].lock().unwrap();
        mem.push(MemEntry {
            seq_no,
            doc_id: doc_id.to_owned(),
            source: None,
            source_bytes: std::sync::Arc::from(&[][..]),
        });

        Ok(Some(seq_no))
    }

    // ── Flush ─────────────────────────────────────────────────────────────────

    /// Flush the memtable to a new segment and swap the snapshot.
    ///
    /// This is the only place where a new `IndexSnapshot` is created.  It is
    /// safe to call from multiple threads — the mutex on `memtable` ensures
    /// only one flush runs at a time.
    #[instrument(skip(self), name = "index_store::flush")]
    pub fn flush(&self) -> Result<Option<SegmentMeta>> {
        self.flush_with_publisher(|_| Ok(()))
    }

    /// Atomically take ownership of the current storage memtable entries,
    /// resetting the memtable to empty.  Returns `None` if the memtable is
    /// empty.
    ///
    /// This is the "drain only" half of `flush_with_publisher` so that the
    /// engine-level flush can release its FTS write lock before doing
    /// expensive segment + FTS side-car I/O.  Pair with
    /// [`finalize_flush_with_publisher`].
    pub fn take_memtable_for_flush(&self) -> Option<DrainedMemtable> {
        // Drain every shard under its own lock, then stitch the
        // per-shard vectors into one `Vec<MemEntry>` ordered by
        // global WAL seq_no.  Because WAL seq_no generation is
        // serialized by `wal.lock()`, two shards can never have
        // overlapping seq_no ranges — so a simple `sort_by_key`
        // yields the globally canonical insertion order.
        let mut entries: Vec<MemEntry> = Vec::new();
        for shard in &self.memtable_shards {
            let mut mem = shard.lock().unwrap();
            entries.append(&mut *mem);
        }
        if entries.is_empty() {
            return None;
        }
        entries.sort_by_key(|e| e.seq_no);
        self.memtable_bytes.store(0, Ordering::Relaxed);
        Some(DrainedMemtable { entries })
    }

    fn restore_internal_drain(&self, drained: &DrainedMemtable) {
        let mut restored_bytes = 0u64;
        for entry in &drained.entries {
            let Some(current) = self.version_map.get(&entry.doc_id) else {
                continue;
            };
            if current.seq_no != entry.seq_no || current.segment_id.as_ref() != IN_MEMORY_SEGMENT_ID
            {
                continue;
            }
            let shard = self.shard_for(&entry.doc_id);
            let mut mem = self.memtable_shards[shard].lock().unwrap();
            if mem.iter().any(|candidate| {
                candidate.doc_id == entry.doc_id && candidate.seq_no == entry.seq_no
            }) {
                continue;
            }
            restored_bytes = restored_bytes.saturating_add(if !entry.source_bytes.is_empty() {
                entry.source_bytes.len() as u64
            } else {
                entry
                    .source
                    .as_ref()
                    .map_or(0, |source| source.to_string().len() as u64)
            });
            mem.push(entry.clone());
        }
        self.memtable_bytes
            .fetch_add(restored_bytes, Ordering::Relaxed);
    }

    /// Flush the memtable, but call `post_finish` with the fresh `SegmentMeta`
    /// BEFORE the in-memory snapshot is swapped.  This lets the caller write
    /// side-car files (e.g. the FTS index) that must be present *before*
    /// readers can see the segment.  If `post_finish` returns an error, the
    /// segment is abandoned (the .seg file may remain on disk but is never
    /// referenced from the snapshot, so readers will not observe a
    /// half-written segment).
    pub fn flush_with_publisher<F>(&self, post_finish: F) -> Result<Option<SegmentMeta>>
    where
        F: FnOnce(&SegmentMeta) -> Result<()>,
    {
        // Drain the memtable and finalise in one shot (legacy path).
        let drained = match self.take_memtable_for_flush() {
            Some(e) => e,
            None => return Ok(None),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finalize_flush_with_publisher(&drained, post_finish)
        }));
        if !matches!(result, Ok(Ok(FlushFinalizeOutcome::Published { .. }))) {
            self.restore_internal_drain(&drained);
        }
        match result {
            Ok(result) => result.map(|outcome| match outcome {
                FlushFinalizeOutcome::Empty => None,
                FlushFinalizeOutcome::Published { meta, .. } => Some(meta),
            }),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Finalise a flush using caller-supplied pre-drained memtable entries.
    /// See [`take_memtable_for_flush`] for the drain half.
    ///
    /// All segment I/O, FTS side-car writes, snapshot publication, and WAL
    /// checkpointing happen here — but no memtable locks are touched, so
    /// callers can release higher-level locks before calling this method.
    pub fn finalize_flush_with_publisher<F>(
        &self,
        drained: &DrainedMemtable,
        post_finish: F,
    ) -> Result<FlushFinalizeOutcome>
    where
        F: FnOnce(&SegmentMeta) -> Result<()>,
    {
        let entries = &drained.entries;
        if entries.is_empty() {
            return Ok(FlushFinalizeOutcome::Empty);
        }

        // THROWAWAY prof (XERJ_PROF): finalize phase breakdown.
        let prof = std::env::var_os("XERJ_PROF").is_some();
        let t_fin_start = std::time::Instant::now();
        let mut prof_ser_us: u128 = 0;
        let mut prof_encode_us: u128 = 0;

        let doc_count = entries.iter().filter(|e| e.source.is_some()).count() as u64;
        let min_seq = entries.iter().map(|e| e.seq_no).min().unwrap_or(0);
        let max_seq = entries.iter().map(|e| e.seq_no).max().unwrap_or(0);

        let segments_dir = self.data_dir.join("segments");
        let mut writer = SegmentWriter::new(&segments_dir, self.config.schema_version, 0, 0)?;

        // Build stored-fields bytes directly, streaming each source value
        // into the output buffer via `serde_json::to_writer`.  The previous
        // version built an intermediate `Vec<serde_json::Value>` with a
        // `json!` macro that deep-cloned every `_source` (the dominant
        // flush cost on log workloads).  Writing bytes once avoids the
        // clone entirely — `e.source` is `Arc<Value>` and `to_writer` only
        // walks it for serialisation.
        let live_entries: Vec<&MemEntry> = entries.iter().filter(|e| e.source.is_some()).collect();
        let has_stored = !live_entries.is_empty();
        if has_stored {
            // P2.2 — when every live entry carries a parsed `source`
            // (the HTTP `_bulk` turbo path: engine memtable drained
            // parsed Values, `source_bytes` empty), feed the encoder the
            // Values directly instead of letting it re-parse a JSON
            // array (~10s background CPU per 1M docs).
            let all_parsed = live_entries
                .iter()
                .all(|e| e.source_bytes.is_empty() && e.source.is_some());
            let parity = std::env::var("XERJ_FLUSH_PARITY")
                .map(|v| v == "1")
                .unwrap_or(false);
            // Flush fast path: on large all-parsed segments skip the
            // canonical JSON-array serialisation entirely (it existed
            // only to feed the v1-LZ4 "never make things worse" size
            // net, which columnar V2 wins on every real segment this
            // size — ~90 ms serialise + ~25 ms LZ4 per 31k-doc flush,
            // ~3 s of background CPU per 1M ingested docs).  Small or
            // mixed segments keep the exact legacy behaviour.
            const SKIP_JSON_MIN_DOCS: usize = 4096;
            let skip_json = all_parsed && live_entries.len() >= SKIP_JSON_MIN_DOCS && !parity;

            let t_ser = std::time::Instant::now();
            let stored_bytes: Vec<u8> = if skip_json {
                Vec::new()
            } else {
                let mut stored_bytes: Vec<u8> = Vec::with_capacity(live_entries.len() * 512);
                stored_bytes.push(b'[');
                let mut first = true;
                for e in &live_entries {
                    if !first {
                        stored_bytes.push(b',');
                    }
                    first = false;
                    stored_bytes.extend_from_slice(br#"{"_id":"#);
                    serde_json::to_writer(&mut stored_bytes, &e.doc_id)?;
                    stored_bytes.extend_from_slice(br#","_seq_no":"#);
                    use std::io::Write;
                    write!(stored_bytes, "{}", e.seq_no)?;
                    stored_bytes.extend_from_slice(br#","_source":"#);
                    if !e.source_bytes.is_empty() {
                        // Raw bytes available — write directly, skip serde round-trip
                        stored_bytes.extend_from_slice(&e.source_bytes);
                    } else if let Some(src) = &e.source {
                        serde_json::to_writer(&mut stored_bytes, src.as_ref())?;
                    } else {
                        stored_bytes.extend_from_slice(b"null");
                    }
                    stored_bytes.push(b'}');
                }
                stored_bytes.push(b']');
                stored_bytes
            };
            // V4 M4.6 — columnar V2 codec: per-column dict+bitpack,
            // cross-column determinism (URL→status/bytes collapses to a
            // mode table + exception bitmap), fallback to LZ4 on small
            // segments.  Byte-identical output by contract between the
            // from-values and legacy encoders — see
            // `encode_stored_v2_from_values`; assert it live with
            // `XERJ_FLUSH_PARITY=1`.
            prof_ser_us = t_ser.elapsed().as_micros();
            let t_enc = std::time::Instant::now();
            let encoded = if all_parsed {
                let doc_refs: Vec<(&str, u64, &serde_json::Value)> = live_entries
                    .iter()
                    .map(|e| {
                        (
                            e.doc_id.as_str(),
                            e.seq_no,
                            e.source
                                .as_deref()
                                .expect("all_parsed checked source.is_some()"),
                        )
                    })
                    .collect();
                let enc = if skip_json {
                    crate::stored_codec::encode_stored_v2_from_values_nojson(&doc_refs)
                } else {
                    crate::stored_codec::encode_stored_v2_from_values(&stored_bytes, &doc_refs)
                };
                if parity {
                    let legacy = crate::stored_codec::encode_stored_v2(&stored_bytes);
                    assert_eq!(
                        legacy,
                        enc,
                        "XERJ_FLUSH_PARITY: encode_stored_v2_from_values diverged from \
                         encode_stored_v2 ({} live docs)",
                        doc_refs.len()
                    );
                    tracing::info!(
                        docs = doc_refs.len(),
                        bytes = enc.len(),
                        "XERJ_FLUSH_PARITY: stored-section bytes identical"
                    );
                }
                enc
            } else {
                crate::stored_codec::encode_stored_v2(&stored_bytes)
            };
            prof_encode_us = t_enc.elapsed().as_micros();
            writer.add_section(SectionType::Stored, &encoded)?;
        }

        // Build the SEQ-AWARE tombstone section (ZTB2) for any drained
        // deletes (RC4 W2 #14).  Reopen now APPLIES these pairs
        // (`rebuild_version_map_from_segments`, max-seq-wins), which makes
        // the delete segment-durable and lets `sweep_pending_deletes`
        // unpin the WAL shard once this segment lands — pre-fix the
        // pinning was FOREVER for a plain never-re-indexed delete (live
        // repro: one DELETE → WAL grew 866 KB → 3 MB across 6 flushed
        // rounds and was never pruned again).  The engine flush path
        // drains its own memtable (no delete entries here); its deletes
        // are persisted by `persist_pending_tombstones` on the
        // maintenance tick instead.  The pre-fix payload (JSON id array,
        // write-only, no seqs) is dropped; ZTB2 is self-describing and
        // old sections are ignored by the decoder.
        let tombstone_pairs: Vec<(u64, &str)> = entries
            .iter()
            .filter(|e| e.source.is_none())
            .map(|e| (e.seq_no, e.doc_id.as_str()))
            .collect();
        if !tombstone_pairs.is_empty() {
            let ts_bytes = crate::segment::encode_tombstones_v2(&tombstone_pairs);
            writer.add_section(SectionType::Tombstones, &ts_bytes)?;
        }

        let t_wfin = std::time::Instant::now();
        let meta = writer.finish(doc_count, min_seq, max_seq)?;
        let prof_wfin_us = t_wfin.elapsed().as_micros();
        let segment_id = meta.id.clone();

        // When using an object-store backend, upload the freshly-written segment
        // and also populate the local cache directory so subsequent reads can
        // be served locally (check-local-first strategy in SegmentCache).
        if let StorageMode::ObjectStore { backend, cache_dir } = &self.config.storage_mode {
            let seg_path = self.data_dir.join("segments").join(&meta.seg_path);
            let seg_data = std::fs::read(&seg_path)?;
            let object_key = format!("segments/{}", meta.seg_path);

            // Drive the async upload synchronously.  `flush` is a sync method so
            // we must not use `block_on` directly (it panics when called from inside
            // an existing Tokio runtime).  Instead we use `block_in_place` which
            // parks the current thread while the runtime schedules other work on
            // this thread's pool.  When flush is eventually made async this becomes
            // a plain `.await`.
            let backend_clone = std::sync::Arc::clone(backend);
            let key_clone = object_key.clone();
            let data_clone = seg_data.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async move { backend_clone.write(&key_clone, &data_clone).await })
            })
            .map_err(|e| StorageError::Backend(format!("object-store upload failed: {e}")))?;

            // Populate the local cache so the next read is served locally.
            let cache_path = cache_dir.join(&meta.seg_path);
            if let Some(parent) = cache_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Best-effort: if caching fails the next read will re-fetch from backend.
            let _ = std::fs::write(&cache_path, &seg_data);

            info!(segment_id, object_key, "segment uploaded to object store");
        }

        // Run the caller-supplied "build side-car files" step.  This must
        // succeed BEFORE we publish the segment to the snapshot — otherwise
        // a racing query could open the segment and find the side-cars
        // (e.g. FTS index) missing, returning wrong results.
        let t_pf = std::time::Instant::now();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| post_finish(&meta))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(self.abandon_unpublished_segment(&segment_id, error));
            }
            Err(payload) => {
                if let Err(cleanup_error) = self.rollback_unpublished_segment(&segment_id) {
                    return Err(StorageError::Io(std::io::Error::other(format!(
                        "publisher panicked; orphan-recovery disarm also failed ({cleanup_error})"
                    ))));
                }
                std::panic::resume_unwind(payload);
            }
        }
        let prof_pf_us = t_pf.elapsed().as_micros();

        // Write the ZID3 document-ID sidecar only after every required caller
        // sidecar succeeded. ZID3 itself is not evidence of completion:
        // orphan recovery requires the completion manifest written
        // immediately afterward. Keeping it after FTS/DV also avoids a
        // misleading near-complete family when sidecar construction fails.
        {
            let pairs: Vec<(u64, &str)> = entries
                .iter()
                .filter(|e| e.source.is_some())
                .map(|e| (e.seq_no, e.doc_id.as_str()))
                .collect();
            if let Err(error) = self.write_ids_sidecar_v3(meta.id.as_str(), &pairs) {
                return Err(self.abandon_unpublished_segment(&segment_id, error.into()));
            }
        }
        if let Err(error) = self.write_flush_completion_manifest(&meta) {
            return Err(self.abandon_unpublished_segment(&segment_id, error.into()));
        }

        // Update version map: point live docs at the new segment.
        //
        // `repoint` (not `set`): the drained entries include superseded
        // duplicates (a doc overwritten within the same memtable
        // generation drains once per write), and a doc can be updated or
        // deleted concurrently after the drain.  The old unconditional
        // `set` transiently clobbered the newer entry with the stale
        // copy's seq_no (and, with per-doc `_version` tracking, would
        // spuriously bump the version once per duplicate).  `repoint`
        // only swaps the segment id when the entry still carries exactly
        // this seq_no — same-generation duplicates and post-drain
        // updates/deletes are left untouched.
        let t_vm = std::time::Instant::now();
        let segment_id_arc: std::sync::Arc<str> = std::sync::Arc::from(segment_id.as_str());
        let rollback_journal: Vec<(String, u64, Option<crate::version_map::VersionEntry>)> =
            entries
                .iter()
                .map(|entry| {
                    (
                        entry.doc_id.clone(),
                        entry.seq_no,
                        self.version_map.get(&entry.doc_id),
                    )
                })
                .collect();
        let publication = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if self.publication_failpoint.load(Ordering::Acquire) == 1 {
                panic!("injected failure before version-map publication");
            }
            for entry in entries {
                if entry.source.is_some() {
                    self.version_map.repoint(
                        &entry.doc_id,
                        entry.seq_no,
                        std::sync::Arc::clone(&segment_id_arc),
                    );
                } else {
                    // RC4 W2 #14 — repoint the delete's tombstone off
                    // `__memtable__` onto the segment that now durably
                    // carries it (the ZTB2 section written above; the
                    // segment + section were fsynced by `writer.finish`).
                    // Guarded (`set_if_latest`): a doc re-indexed while this
                    // flush ran has a strictly newer live entry that must
                    // not be clobbered back to deleted.  Once
                    // segment-resident, `sweep_pending_deletes` unpins the
                    // WAL shard and `wal_pair_durable` lets the Delete
                    // entry prune.
                    self.version_map.set_if_latest(
                        &entry.doc_id,
                        entry.seq_no,
                        std::sync::Arc::clone(&segment_id_arc),
                        true,
                    );
                }
                #[cfg(test)]
                if self.publication_failpoint.load(Ordering::Acquire) == 2 {
                    panic!("injected failure during version-map publication");
                }
                #[cfg(test)]
                if self.publication_failpoint.load(Ordering::Acquire) == 5 {
                    self.version_map.set(
                        &entry.doc_id,
                        entry.seq_no + 1,
                        crate::version_map::IN_MEMORY_SEGMENT_ID,
                        false,
                    );
                    panic!("injected newer PUT during publication rollback");
                }
                #[cfg(test)]
                if self.publication_failpoint.load(Ordering::Acquire) == 6 {
                    self.version_map
                        .delete(
                            &entry.doc_id,
                            entry.seq_no + 1,
                            crate::version_map::IN_MEMORY_SEGMENT_ID,
                        )
                        .unwrap();
                    panic!("injected newer DELETE during publication rollback");
                }
            }
            #[cfg(test)]
            if self.publication_failpoint.load(Ordering::Acquire) == 3 {
                panic!("injected failure immediately before snapshot publication");
            }
            self.snapshot
                .rcu(|old| Arc::new(old.with_new_segment(meta.clone())));
            #[cfg(test)]
            if self.publication_failpoint.load(Ordering::Acquire) == 4 {
                panic!("injected failure immediately after snapshot publication");
            }
        }));
        if publication.is_err() {
            let published = self
                .snapshot
                .load()
                .segments
                .iter()
                .any(|candidate| candidate.id == segment_id);
            if published {
                warn!(
                    segment_id,
                    "post-publication panic caught; maintenance deferred"
                );
                // #871 — the segment IS in the snapshot; merge scheduling
                // must hear about it even on this degraded path.
                self.notify_segments_changed();
                return Ok(FlushFinalizeOutcome::Published {
                    meta,
                    maintenance_deferred: true,
                });
            }
            for (doc_id, seq_no, prior) in rollback_journal.into_iter().rev() {
                self.version_map
                    .rollback_repoint(&doc_id, seq_no, &segment_id, prior);
            }
            let publication_error = StorageError::Io(std::io::Error::other(
                "segment publication failed before snapshot commit",
            ));
            return Err(self.abandon_unpublished_segment(&segment_id, publication_error));
        }
        // #871 — the rcu above published a new segment: fire the segment-set
        // change hook so the engine debounce-schedules a merge check. This is
        // the flush-side analogue of Lucene's maybeMerge-on-flush
        // (IndexWriter.java:706, MergeTrigger.FULL_FLUSH).
        self.notify_segments_changed();
        if prof {
            eprintln!(
                "XERJ_PROF finalize docs={} ser_us={} encode_us={} writer_finish_us={} post_finish_us={} vm_us={} total_so_far_us={}",
                doc_count,
                prof_ser_us,
                prof_encode_us,
                prof_wfin_us,
                prof_pf_us,
                t_vm.elapsed().as_micros(),
                t_fin_start.elapsed().as_micros()
            );
        }

        // Publish the new segment via ArcSwap::rcu so concurrent shard
        // flushes (one per shard, run in parallel by `Index::flush`)
        // don't drop each other's segments. The previous load → modify
        // → store sequence wasn't atomic — two flushes finishing close
        // together would each load the same baseline snapshot, append
        // their own segment, and the second store would overwrite the
        // first, evicting the first segment from the snapshot. The
        // first segment's `version_map` entries still pointed at the
        // (now-unreachable) segment id, so the docs disappeared from
        // search even though their files were on disk. Reproduced as
        // ~30 % of `_refresh` calls losing 1-2 docs after 6-doc
        // sequential PUTs in the YAML suite (110_field_collapsing
        // setup, et al.).
        // V4 M4 — checkpoint + rotate + prune, NOW time-gated.
        //
        // Pre-gate: this loop ran for ALL 16 WAL shards on EVERY
        // segment flush.  With 16 concurrent shard flushes that's
        // 256 lock acquires + 16 checkpoint writes + 16 prune dirent
        // scans per flush tick — measurably the #1 cost once sync-path
        // refactor eliminated async overhead.
        //
        // Post-gate: at most one caller per
        // `WAL_MAINTENANCE_INTERVAL_MS` (1 s) wins the CAS and runs
        // the loop on behalf of all concurrent flushers.  Losers skip
        // the whole block.  On-disk WAL footprint is still bounded —
        // we just do the work less frequently.  `Index::flush()`
        // (final CLI drain / user flush) calls `force_wal_maintenance()`
        // to guarantee the last segment is checkpointed regardless of
        // timing window.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let last = self.last_wal_maintenance_ms.load(Ordering::Acquire);
        if now_ms.saturating_sub(last) >= WAL_MAINTENANCE_INTERVAL_MS
            && self
                .last_wal_maintenance_ms
                .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            // P2.3 — persist the snapshot HERE, coupled to the same 1 s
            // gate as WAL prune, and BEFORE pruning.  Pre-P2.3 this ran
            // `save_snapshot()` unconditionally on EVERY finalize: with
            // ~16 concurrent shard flushes per cycle that is 16 full
            // `serde_json` re-serialisations of the ENTIRE segment list
            // (O(total segments)) per flush tick — the mechanism behind
            // the ingest-throughput decay with corpus size.  We now do
            // it once per maintenance tick.
            //
            // Durability invariant preserved: the snapshot is persisted
            // immediately before the WAL is pruned, so every doc whose
            // WAL entry is dropped is already recorded in an on-disk
            // segment listed in the persisted snapshot.  Segments that
            // are published to the in-memory snapshot between ticks but
            // not yet persisted are recoverable on restart exactly like
            // today: a crash between the `rcu` publish above and this
            // save already left an "orphan" segment, and
            // `recover_orphaned_segments()` + WAL replay (deduped by the
            // version_map) reconstruct the live set.  Debouncing only
            // widens that already-handled window; it adds no new failure
            // mode.  Clean shutdown / explicit `_flush` persists via
            // `force_wal_maintenance()`.
            // RC4 W2 #14 — persist still-memtable-resident acked deletes
            // as a tombstone-only segment BEFORE the snapshot save, so
            // the save registers it and the prune below can release the
            // deletes' WAL pins.  Best-effort: on failure the deletes
            // simply stay WAL-pinned (retention, not loss).
            if let Err(e) = self.persist_pending_tombstones() {
                warn!(error = %e, "tombstone persistence failed — deletes stay WAL-pinned");
            }
            if let Err(error) = self.save_snapshot() {
                warn!(error = %error, segment_id, "snapshot persistence deferred after in-memory publication; WAL remains authoritative");
                info!(
                    segment_id,
                    doc_count, min_seq, max_seq, "segment published with deferred maintenance"
                );
                return Ok(FlushFinalizeOutcome::Published {
                    meta,
                    maintenance_deferred: true,
                });
            }
            // RC4 W1 #8 — verified maintenance (see
            // `wal_maintain_all_verified`).  The pre-fix loop here
            // checkpointed EVERY shard with THIS segment's `max_seq` and
            // the shard's full file offset, then force-rotated and pruned
            // — destroying acked-but-unflushed entries on sibling shards
            // (their memtable docs can carry seqs below a fresh segment's
            // max) and on THIS shard (docs appended between the drain and
            // this maintenance tick).  Live-verified as the 50/50 kill-9
            // acked-loss repro.  The durable watermark passed for the
            // compat checkpoint is the max seq registered in the snapshot
            // we just persisted — never a live counter.
            //
            // Disk-space behaviour is preserved: fully-durable generations
            // still rotate + prune on the same 1 s cadence; generations
            // holding unproven entries are retained (that retention IS the
            // fix) and reclaimed on a later tick once their docs flush.
            let durable_max = self.snapshot.load().max_seq_no;
            if let Err(error) = self.wal_maintain_all_verified(durable_max) {
                warn!(error = %error, segment_id, "WAL maintenance deferred after segment publication");
            }
        }

        info!(segment_id, doc_count, min_seq, max_seq, "segment flushed");
        Ok(FlushFinalizeOutcome::Published {
            meta,
            maintenance_deferred: false,
        })
    }

    /// Flush if the memtable is over the configured threshold.
    pub fn maybe_flush(&self) -> Result<Option<SegmentMeta>> {
        if self.memtable_bytes.load(Ordering::Relaxed) >= self.config.memtable_max_bytes as u64 {
            self.flush()
        } else {
            Ok(None)
        }
    }

    // ── Read path ─────────────────────────────────────────────────────────────

    /// Load the current snapshot.  Lock-free.
    ///
    /// Merge-race fix (2026-07): the returned guard is also a **read
    /// lease** — for as long as it is alive, the on-disk files of every
    /// segment it references are guaranteed to exist, even if a
    /// concurrent merge commits and retires some of them
    /// (`retire_segment_files` defers the unlink until the last lease
    /// drops).  Pre-fix, `run_merge_once` unlinked merged-away segment
    /// files immediately after `apply_merge`; a search that had already
    /// snapshotted the old segment list would then fail to open those
    /// segments mid-scan and silently skip them (observed live: 798,281
    /// hits returned instead of 932,037 during a background merge).
    ///
    /// IMPORTANT ordering: the lease count is incremented *before* the
    /// snapshot pointer is loaded (`fetch_add` is a full RMW barrier), so
    /// a retire that observes `read_leases == 0` can only race with a
    /// reader that will observe the *post-merge* snapshot — never one
    /// still holding the merged-away segment list.
    pub fn snapshot(&self) -> SnapshotReadGuard<'_> {
        self.read_leases
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SnapshotReadGuard {
            snap: self.snapshot.load(),
            store: self,
        }
    }

    /// #871 — install (or clear) the segment-set change hook. See the
    /// `segments_changed_hook` field docs; the engine wires this to its
    /// debounced merge-check scheduler right after constructing an index.
    pub fn set_segments_changed_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .segments_changed_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    /// Fire the #871 segment-set change hook, if installed. Called on every
    /// path that swaps `self.snapshot` with a different segment list. The
    /// hook is cloned out first so it never runs under this store's locks
    /// (it may spawn a tokio task).
    fn notify_segments_changed(&self) {
        let hook = self
            .segments_changed_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Return the current WAL sequence number (the next value that
    /// `wal_append_batch` would assign).  Used by `Index::flush` to
    /// write a global checkpoint covering ALL shards after a
    /// multi-shard parallel flush.
    pub fn current_seq_no(&self) -> u64 {
        self.seq_counter.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Delete-durability: drop `pending_deletes` entries whose delete has
    /// been SUBSUMED by a newer, segment-durable version of the same doc —
    /// the version map shows the doc live with a seq_no newer than the
    /// delete AND pointing at a real segment (the flush repointed it off
    /// `__memtable__`).  Once that newer copy is in a segment, reopen
    /// rebuilds the doc from it (max-seq-wins) and the old `WalEntry::
    /// Delete` is no longer load-bearing, so its WAL shard can resume
    /// checkpoint/rotate/prune.
    ///
    /// Deliberately conservative: tombstoned docs stay pinned even after
    /// a background merge physically drops them from the merged segments,
    /// because an older copy of the doc may still live in a segment that
    /// was NOT part of the merge — clearing on "merge purged it" alone
    /// would resurrect from that older segment.  Cost: WAL retention on
    /// delete-heavy indices (bounded by delete volume, zero on append-only
    /// workloads).  Runs only on the 1s-gated / explicit maintenance
    /// paths — never on the ingest hot path.
    fn sweep_pending_deletes(&self) {
        let mut pending = self.pending_deletes.lock().unwrap();
        if pending.is_empty() {
            return;
        }
        pending.retain(|doc_id, &mut (del_seq, _)| {
            match self.version_map.get(doc_id) {
                // Subsumed: live, strictly newer, and already flushed
                // into a real segment → safe to unpin.
                Some(e) if !e.deleted && e.seq_no > del_seq => {
                    &*e.segment_id == IN_MEMORY_SEGMENT_ID
                }
                // RC4 W2 #14 — tombstone persisted: the delete (or an
                // equal-or-newer one) is recorded in a segment's ZTB2
                // tombstone section (the repoint off `__memtable__`
                // happens only after the segment is durably on disk),
                // so reopen reconstructs it without the WAL entry →
                // unpin.  Pre-fix, a plain never-re-indexed delete
                // matched the `_ => true` arm FOREVER and its WAL shard
                // never checkpointed/rotated/pruned again.
                Some(e) if e.deleted && e.seq_no >= del_seq => {
                    &*e.segment_id == IN_MEMORY_SEGMENT_ID
                }
                // Missing / older live seq: the WAL Delete entry is
                // still the only durable record.
                _ => true,
            }
        });
    }

    /// RC4 W2 #14 — make every still-memtable-resident acked delete
    /// segment-durable by writing a dedicated TOMBSTONE-ONLY segment
    /// (doc_count = 0, single ZTB2 `Tombstones` section).
    ///
    /// The engine flush path drains its own memtable, so plain deletes
    /// (which live only in `pending_deletes` + the version map) never
    /// reach `finalize_flush_with_publisher`'s tombstone section.  This
    /// runs on the WAL-maintenance paths (1 s gated tick + explicit
    /// `force_wal_maintenance`) right before the snapshot is persisted:
    /// collect → write segment (fsynced by `SegmentWriter::finish`) →
    /// repoint the version-map tombstones onto it → publish to the
    /// snapshot.  The subsequent `save_snapshot()` + verified prune then
    /// see a segment-resident tombstone and release the WAL pin.
    ///
    /// Crash-window: a crash after the prune but before `save_snapshot`
    /// is covered by `recover_orphaned_segments`, which resurrects
    /// orphan tombstone-only segments from their (CRC-validated) header.
    fn persist_pending_tombstones(&self) -> Result<()> {
        // Collect deletes whose tombstone is still memtable-resident.
        let pairs: Vec<(u64, String)> = {
            let pending = self.pending_deletes.lock().unwrap();
            if pending.is_empty() {
                return Ok(());
            }
            pending
                .keys()
                .filter_map(|doc_id| match self.version_map.get(doc_id) {
                    Some(e) if e.deleted && &*e.segment_id == IN_MEMORY_SEGMENT_ID => {
                        Some((e.seq_no, doc_id.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        if pairs.is_empty() {
            return Ok(());
        }

        let min_seq = pairs.iter().map(|(s, _)| *s).min().unwrap_or(0);
        let max_seq = pairs.iter().map(|(s, _)| *s).max().unwrap_or(0);
        let pair_refs: Vec<(u64, &str)> = pairs.iter().map(|(s, id)| (*s, id.as_str())).collect();

        let segments_dir = self.data_dir.join("segments");
        let mut writer = SegmentWriter::new(&segments_dir, self.config.schema_version, 0, 0)?;
        writer.add_section(
            SectionType::Tombstones,
            crate::segment::encode_tombstones_v2(&pair_refs),
        )?;
        // doc_count 0: tombstone-only.  finish() fsyncs file + dir.
        let meta = writer.finish(0, min_seq, max_seq)?;

        // Segment is durable — repoint the tombstones onto it (guarded:
        // a doc re-indexed since collection keeps its newer live entry).
        let seg_arc: std::sync::Arc<str> = std::sync::Arc::from(meta.id.as_str());
        for (seq, doc_id) in &pairs {
            self.version_map.set_if_latest(
                doc_id.as_str(),
                *seq,
                std::sync::Arc::clone(&seg_arc),
                true,
            );
        }

        // Publish so the caller's save_snapshot() registers it on disk.
        self.snapshot
            .rcu(|old| Arc::new(old.with_new_segment(meta.clone())));
        // #871 — a tombstone-only segment is a segment-set change (and a
        // merge candidate: merges are what fold tombstones away).
        self.notify_segments_changed();

        info!(
            segment_id = meta.id.as_str(),
            tombstones = pairs.len(),
            "tombstone-only segment persisted (acked deletes now segment-durable)"
        );
        Ok(())
    }

    /// Delete-durability: true if WAL shard `shard_idx` still holds an
    /// unpersisted acked delete and therefore MUST NOT be checkpointed,
    /// rotated, or pruned.  Callers hold the shard's WAL mutex when
    /// they consult this, which combined with the insert-before-append
    /// ordering in [`IndexStore::delete`] makes the check race-free.
    fn wal_shard_pinned_by_pending_delete(&self, shard_idx: usize) -> bool {
        let pending = self.pending_deletes.lock().unwrap();
        pending.values().any(|&(_, ws)| ws == shard_idx)
    }

    /// RC4 W1 #8 — the per-entry durability proof used by WAL maintenance.
    ///
    /// Returns true iff destroying this WAL entry cannot lose data:
    ///
    /// - `Index`: the doc is tombstoned at an equal-or-newer seq (the
    ///   tombstoning delete is itself WAL-pinned until subsumed, so replay
    ///   reconstructs the deletion); OR a strictly newer version of the doc
    ///   exists (whose own WAL entry is retained until IT is durable — the
    ///   proof chains); OR this exact version has been flushed into a real
    ///   segment (version map repointed off `__memtable__`, which happens
    ///   only after the segment + its side-cars are durably on disk — see
    ///   the blocker-#10 fsync barrier in `finalize_flush_with_publisher`).
    /// - `Delete`: subsumed — the doc was re-indexed strictly newer AND
    ///   that version is segment-resident (mirrors
    ///   `sweep_pending_deletes`).  A load-bearing tombstone is never
    ///   prunable.
    /// - `UpdateMapping`: always — `schema.json` is written atomically at
    ///   update time and replaying the entry is a no-op.
    ///
    /// `None` from the version map is conservatively NOT durable: it can
    /// mean "version map not yet updated for a just-appended doc" (the
    /// batch paths append to the WAL before the version-map set).
    fn wal_entry_durable(&self, entry: &WalEntry, seq: SeqNo) -> bool {
        match entry {
            WalEntry::Index { doc_id, .. } => self.wal_pair_durable(false, doc_id, seq),
            WalEntry::Delete { doc_id } => self.wal_pair_durable(true, doc_id, seq),
            WalEntry::UpdateMapping { .. } => true,
        }
    }

    /// Core of [`wal_entry_durable`](Self::wal_entry_durable), operating on
    /// the `(is_delete, doc_id, seq)` shape the prune cache stores.
    fn wal_pair_durable(&self, is_delete: bool, doc_id: &str, seq: SeqNo) -> bool {
        if is_delete {
            match self.version_map.get(doc_id) {
                Some(e) if !e.deleted => e.seq_no > seq && &*e.segment_id != IN_MEMORY_SEGMENT_ID,
                // RC4 W2 #14 — the tombstone itself (this delete or a
                // newer one) is segment-resident: reopen rebuilds the
                // deletion from the ZTB2 section, the WAL entry is no
                // longer load-bearing.
                Some(e) if e.deleted => e.seq_no >= seq && &*e.segment_id != IN_MEMORY_SEGMENT_ID,
                _ => false,
            }
        } else {
            match self.version_map.get(doc_id) {
                Some(e) if e.deleted => e.seq_no >= seq,
                Some(e) => {
                    e.seq_no > seq || (e.seq_no == seq && &*e.segment_id != IN_MEMORY_SEGMENT_ID)
                }
                None => false,
            }
        }
    }

    /// RC4 W1 #8 — verified WAL maintenance across all shards.
    ///
    /// Replaces the pre-fix `checkpoint(global_max_seq) + force_rotate +
    /// prune` loop, which destroyed acked-but-unflushed entries two ways:
    ///
    /// 1. The checkpoint was written with a GLOBAL max_seq
    ///    (`current_seq_no()-1` from `Index::flush`, or a sibling shard's
    ///    segment max) and the shard's FULL current offset — covering
    ///    entries whose docs still lived only in a memtable.  Replay then
    ///    skipped them (loss channel closed by making replay ignore
    ///    checkpoints), and
    /// 2. `prune()` deleted any rotated generation that had a checkpoint
    ///    file, destroying those same entries outright (loss channel
    ///    closed by per-entry verified pruning).
    ///
    /// New per-shard flow (under the shard's WAL mutex):
    /// - skip entirely if the shard is pinned by an unpersisted delete;
    /// - decode the ACTIVE generation once and check every entry against
    ///   [`wal_entry_durable`](Self::wal_entry_durable);
    /// - if fully durable: write a checkpoint (safe values — kept for
    ///   data-dir compatibility with older binaries) and force-rotate so
    ///   the generation becomes prunable;
    /// - if it holds any unproven entry: force-rotate WITHOUT a
    ///   checkpoint (freezes the generation; per-pair re-verification
    ///   reclaims it on a later tick once everything in it flushed);
    /// - the rotated generation's verdict (durable / unproven pairs /
    ///   undecodable) is recorded in `wal_prune_cache` so no frozen file
    ///   is ever decoded twice — later ticks only re-check the cached
    ///   unproven pairs against the version map (see the cache's doc
    ///   comment for the O(retained WAL bytes)/tick problem this solves);
    /// - prune every rotated generation whose verdict has drained to
    ///   Durable.
    ///
    /// The caller must persist the snapshot (`save_snapshot`) BEFORE this
    /// runs so every segment the proofs point at is registered on disk.
    fn wal_maintain_all_verified(&self, durable_max_seq: SeqNo) -> Result<()> {
        #[cfg(test)]
        if self.fail_next_wal_maintenance.swap(false, Ordering::AcqRel) {
            return Err(StorageError::Io(std::io::Error::other(
                "injected post-publication WAL maintenance failure",
            )));
        }
        self.sweep_pending_deletes();
        for (ws_idx, ws) in self.wal_shards.iter().enumerate() {
            let mut wal = ws.lock().unwrap();
            if self.wal_shard_pinned_by_pending_delete(ws_idx) {
                debug!(
                    shard = ws_idx,
                    "WAL maintenance skipped: shard pinned by unpersisted delete"
                );
                continue;
            }
            // Drain the userspace buffer so the on-disk active generation
            // is complete before we decode it.
            wal.soft_flush()?;
            let active_gen = wal.active_generation();
            let (entries, clean) = wal.read_generation_entries(active_gen);
            let unproven = self.collect_unproven(&entries);
            if clean && unproven.is_empty() {
                if !entries.is_empty() {
                    wal.checkpoint(durable_max_seq)?;
                }
                wal.force_rotate()?;
            } else {
                wal.force_rotate()?;
            }
            // Record the verdict for the generation we just froze (if the
            // rotate was a no-op — empty generation — there is nothing to
            // record).
            if wal.active_generation() != active_gen {
                let verdict = if !clean {
                    WalGenVerdict::Undecodable
                } else if unproven.is_empty() {
                    WalGenVerdict::Durable
                } else {
                    WalGenVerdict::Unproven(unproven)
                };
                self.wal_prune_cache
                    .lock()
                    .unwrap()
                    .insert((ws_idx, active_gen), verdict);
            }

            // Prune pass over all rotated generations, cache-first.
            //
            // #320 — the WAL-consumer retention floor is applied HERE, not
            // only in `WalWriter::prune_verified`. `prune_verified` has no
            // production caller (grep: `wal.rs` and its own unit tests are the
            // only hits); this loop is the prune the engine actually runs, so
            // a floor enforced only there was fully unit-tested and still had
            // zero effect on a live node.
            //
            // `rotated_generations` is sorted ascending, so the newest are the
            // tail and the prunable prefix is `len - keep`. Same arithmetic,
            // and the same "enforce it inside the deletion pass rather than in
            // its callers" placement, as Lucene's
            // `KeepLastNCommitsDeletionPolicy.onCommit`
            // (lucene/core/src/java/org/apache/lucene/index/KeepLastNCommitsDeletionPolicy.java:51-58,
            // Apache-2.0): "The commits list is already sorted from oldest to
            // newest / for (i = 0; i < size - numCommitsToKeep; i++)
            // commits.get(i).delete()". Adapted, not copied.
            //
            // At the default floor of 0 this is `keep = 0` and the loop is
            // byte-for-byte the old one: no extra syscall, no behaviour change.
            let rotated = wal.rotated_generations()?;
            let keep = (wal.min_retained_generations() as usize).min(rotated.len());
            let prunable_prefix = rotated.len() - keep;
            for (pos, gen) in rotated.into_iter().enumerate() {
                if pos >= prunable_prefix {
                    // Held for a WAL consumer. Deliberately skipped BEFORE the
                    // cache lookup: decoding a generation we are not going to
                    // delete is the O(retained WAL bytes)/tick cost the prune
                    // cache exists to avoid.
                    debug!(
                        gen,
                        shard = ws_idx,
                        keep,
                        "WAL generation retained: consumer retention floor"
                    );
                    continue;
                }
                let mut cache = self.wal_prune_cache.lock().unwrap();
                let verdict = cache.entry((ws_idx, gen)).or_insert_with(|| {
                    // Cache miss: a generation rotated by the size-based
                    // append path, or retained across a restart.  Decode
                    // it exactly once.
                    let (gen_entries, gen_clean) = wal.read_generation_entries(gen);
                    if !gen_clean {
                        WalGenVerdict::Undecodable
                    } else {
                        let pairs = self.collect_unproven(&gen_entries);
                        if pairs.is_empty() {
                            WalGenVerdict::Durable
                        } else {
                            WalGenVerdict::Unproven(pairs)
                        }
                    }
                });
                let prunable = match verdict {
                    WalGenVerdict::Durable => true,
                    WalGenVerdict::Undecodable => {
                        debug!(gen, shard = ws_idx, "WAL generation retained: undecodable");
                        false
                    }
                    WalGenVerdict::Unproven(pairs) => {
                        // Cheap re-check: version-map lookups only.
                        pairs.retain(|(is_delete, doc_id, seq)| {
                            !self.wal_pair_durable(*is_delete, doc_id, *seq)
                        });
                        if pairs.is_empty() {
                            *verdict = WalGenVerdict::Durable;
                            true
                        } else {
                            debug!(
                                gen,
                                shard = ws_idx,
                                unproven = pairs.len(),
                                "WAL generation retained: acked-but-unflushed entries"
                            );
                            false
                        }
                    }
                };
                if prunable {
                    wal.delete_generation(gen)?;
                    cache.remove(&(ws_idx, gen));
                }
            }
        }
        Ok(())
    }

    /// Collect the `(is_delete, doc_id, seq)` pairs of every entry NOT yet
    /// provable durable (mapping updates are always durable and never
    /// collected).
    fn collect_unproven(&self, entries: &[crate::wal::ReplayEntry]) -> Vec<(bool, String, SeqNo)> {
        entries
            .iter()
            .filter(|e| !self.wal_entry_durable(&e.entry, e.seq_no))
            .filter_map(|e| match &e.entry {
                WalEntry::Index { doc_id, .. } => Some((false, doc_id.clone(), e.seq_no)),
                WalEntry::Delete { doc_id } => Some((true, doc_id.clone(), e.seq_no)),
                WalEntry::UpdateMapping { .. } => None,
            })
            .collect()
    }

    /// Change the WAL-consumer retention floor on every shard of this store,
    /// **live**.
    ///
    /// #320. `IndexStoreConfig.wal_min_retained_generations` is read once, at
    /// open, from the boot config file — and `Engine.config` is an
    /// `Arc<Config>` that is never mutated, so without this a floor set
    /// through `PUT /_xerj/wal_tap` reached no writer at all: not the ones
    /// already open, and not the ones opened after a restart either. The knob
    /// exists to stop a target outage turning into data loss, so "acknowledged
    /// but inert" is the one failure mode it must not have.
    ///
    /// Live reconfiguration of an already-open writer is Lucene's
    /// `LiveIndexWriterConfig`
    /// (lucene/core/src/java/org/apache/lucene/index/LiveIndexWriterConfig.java:39-126,
    /// Apache-2.0): "Holds all the configuration used by IndexWriter with few
    /// setters for settings that can be changed on an IndexWriter instance
    /// 'live'." Everything else there is explicitly documented as "Only takes
    /// effect when IndexWriter is first created"
    /// (`IndexWriterConfig.setCodec`, :297-305). A setting is one or the
    /// other, and says which; it is never claimed live and implemented at
    /// create.
    pub fn set_wal_min_retained_generations(&self, n: u64) {
        for shard in &self.wal_shards {
            shard.lock().unwrap().set_min_retained_generations(n);
        }
    }

    /// The retention floor in force on each WAL shard, in shard order.
    ///
    /// Instrumentation for the property that
    /// [`set_wal_min_retained_generations`](Self::set_wal_min_retained_generations)
    /// exists to provide: it must be assertable that a runtime change reached
    /// every writer, not just the first.
    pub fn wal_min_retained_generations(&self) -> Vec<u64> {
        self.wal_shards
            .iter()
            .map(|s| s.lock().unwrap().min_retained_generations())
            .collect()
    }

    /// Unconditionally run verified WAL maintenance across all shards.
    /// Bypasses the `WAL_MAINTENANCE_INTERVAL_MS` gate that
    /// `finalize_flush_with_publisher` uses on the hot flush path.
    /// Called by `Index::flush()` (the final drain / user-triggered
    /// `_flush`) so disk cleanup happens immediately — matches ES's
    /// `_flush`-time translog rollover semantics.
    ///
    /// RC4 W1 #8 — signature change: the old `max_seq: SeqNo` parameter
    /// is gone.  `Index::flush` passed `current_seq_no() - 1`, which
    /// covered every acked-but-unflushed doc in existence and was the
    /// direct trigger of the 50/50 kill-9 loss repro.  The durable
    /// watermark is now computed internally from the persisted snapshot
    /// (max seq actually resident in flushed segments).
    pub fn force_wal_maintenance(&self) -> Result<()> {
        // RC4 W2 #14 — see the gated call site: segment-persist acked
        // deletes so their WAL pins can be released by the prune below.
        if let Err(e) = self.persist_pending_tombstones() {
            warn!(error = %e, "tombstone persistence failed — deletes stay WAL-pinned");
        }
        // P2.3 — persist the (possibly debounced) snapshot before pruning
        // so an explicit `_flush` / clean shutdown always leaves the
        // on-disk snapshot covering every segment whose WAL is about to
        // be dropped.  Mirrors the coupling in the gated flush path.
        self.save_snapshot()?;
        let durable_max = self.snapshot.load().max_seq_no;
        self.wal_maintain_all_verified(durable_max)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_wal_maintenance_ms
            .store(now_ms, Ordering::Release);
        Ok(())
    }

    /// Acquire WAL shard 0 mutex for legacy single-WAL callers.
    pub fn wal_lock(&self) -> std::sync::MutexGuard<'_, WalWriter> {
        self.wal_shards[0].lock().unwrap()
    }

    /// Open a reader for a specific segment by ID.
    ///
    /// When [`StorageMode::ObjectStore`] is active the read-through cache is
    /// consulted first.  If the segment is not cached locally it is fetched from
    /// the backend and written to the cache before the reader is opened.
    pub fn open_segment(&self, segment_id: &str) -> Result<SegmentReader> {
        // Delegate to the cached Arc path then clone the inner reader.
        // `SegmentReader` doesn't impl Clone directly, so callers that
        // already use `open_segment_arc` avoid this clone path.
        let arc = self.open_segment_arc(segment_id)?;
        // Re-open from the same mmap that the cached reader holds —
        // zero disk I/O, only a few field copies.
        crate::segment::SegmentReader::from_mmap_arc(Arc::clone(arc.mmap_arc()))
    }

    /// Walk every segment in the current snapshot and re-validate
    /// every section's stored CRC32C. Returns a structured report
    /// with per-segment + per-section status. Use this from the
    /// `_admin/segments/fsck` endpoint or a scheduled job.
    ///
    /// Whole-file CRC is already validated at `from_mmap` (open
    /// time); per-section CRC is normally skipped on the read hot
    /// path for perf (see segment.rs::section docs). This method
    /// goes back over every section and proves the bytes haven't
    /// changed since the segment was written.
    pub fn fsck_segments(&self) -> FsckReport {
        let snap = self.snapshot.load();
        let mut segs = Vec::with_capacity(snap.segments.len());
        let mut total_sections = 0usize;
        let mut bad_sections = 0usize;
        for meta in snap.segments.iter() {
            let reader = match self.open_segment_arc(meta.id.as_str()) {
                Ok(r) => r,
                Err(e) => {
                    segs.push(FsckSegmentReport {
                        segment_id: meta.id.to_string(),
                        sections: Vec::new(),
                        open_error: Some(e.to_string()),
                    });
                    continue;
                }
            };
            let mut section_results = Vec::new();
            for kind in reader.section_types() {
                total_sections += 1;
                let result = reader.section_checked(kind);
                let ok = result.is_ok();
                if !ok {
                    bad_sections += 1;
                }
                section_results.push(FsckSectionReport {
                    kind: format!("{kind:?}"),
                    ok,
                    error: result.err().map(|e| e.to_string()),
                });
            }
            segs.push(FsckSegmentReport {
                segment_id: meta.id.to_string(),
                sections: section_results,
                open_error: None,
            });
        }
        FsckReport {
            segments: segs,
            total_segments_checked: snap.segments.len(),
            total_sections_checked: total_sections,
            corrupt_sections: bad_sections,
        }
    }

    /// M5.20 — cached-by-segment-id SegmentReader accessor.
    ///
    /// Callers that can use `Arc<SegmentReader>` directly (e.g. the
    /// query path) should prefer this over `open_segment` — a cache
    /// hit is a DashMap lookup + Arc::clone, no mmap syscall and
    /// no CRC work.
    pub fn open_segment_arc(&self, segment_id: &str) -> Result<Arc<crate::segment::SegmentReader>> {
        if let Some(entry) = self.seg_reader_cache.get(segment_id) {
            return Ok(Arc::clone(entry.value()));
        }
        let snap = self.snapshot.load();
        // Merge-race fix (2026-07): a miss against the CURRENT snapshot no
        // longer means "gone".  The caller may hold a `SnapshotReadGuard`
        // on an OLDER snapshot whose segment was merged away after the
        // caller loaded it — its files are then still on disk (retire
        // defers deletion until the last read lease drops), just no longer
        // registered.  Fall back to the id-derived filename (`{id}.seg` —
        // the invariant name set by SegmentWriter) so an in-flight scan
        // stays consistent with ITS snapshot instead of silently skipping
        // the segment (the merge-race undercount bug).  For a genuinely
        // unknown id the open below fails and the error propagates.
        // RC4 W2 #15 — no `exists()` pre-check on the fallback path.  The
        // old `exists()`-then-open was a TOCTOU: a retire sweep unlinking
        // the file between the two calls turned a benign "segment gone"
        // into a raw io::Error surfacing as a 500 (the mixed-RUW
        // `store_exception` file race), while an `exists() == false` for
        // a file mid-rename produced a spurious SegmentNotFound.  The
        // open itself is now the single authority: a NotFound from it is
        // mapped to `SegmentNotFound` below.
        let seg_path: String = match snap.segments.iter().find(|s| s.id == segment_id) {
            Some(m) => m.seg_path.clone(),
            None => format!("{segment_id}.seg"),
        };
        drop(snap);

        let local_path = self.data_dir.join("segments").join(&seg_path);

        // For object-store mode: check local cache; fetch from backend on miss.
        let reader = if let StorageMode::ObjectStore { backend, cache_dir } =
            &self.config.storage_mode
        {
            let cache_path = cache_dir.join(&seg_path);
            if cache_path.exists() {
                crate::segment::SegmentReader::open(cache_path)?
            } else {
                let object_key = format!("segments/{seg_path}");
                let backend_clone = std::sync::Arc::clone(backend);
                let key_clone = object_key.clone();
                let data = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async move {
                        backend_clone.read_range(&key_clone, 0, u64::MAX).await
                    })
                })
                .map_err(|e| StorageError::Backend(format!("object-store fetch failed: {e}")))?;

                if let Some(parent) = cache_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&cache_path, &data)?;
                debug!(segment_id, ?cache_path, "segment cached from object store");
                crate::segment::SegmentReader::open(cache_path)?
            }
        } else {
            match crate::segment::SegmentReader::open(local_path) {
                Ok(r) => r,
                // Typed not-found (RC4 W2 #15): the file was already
                // retired/unlinked — callers treat SegmentNotFound as
                // "skip / stale snapshot", not as an internal error.
                Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(StorageError::SegmentNotFound(segment_id.to_owned()));
                }
                Err(e) => return Err(e),
            }
        };
        let arc = Arc::new(reader);
        self.seg_reader_cache
            .insert(segment_id.to_string(), Arc::clone(&arc));
        Ok(arc)
    }

    /// Evict a cached SegmentReader (called by `apply_merge` so
    /// replaced segments are removed immediately and their mmap
    /// pages can be reclaimed).
    pub fn evict_segment_reader_cache(&self, segment_id: &str) {
        self.seg_reader_cache.remove(segment_id);
    }

    // ── Snapshot persistence ──────────────────────────────────────────────────

    fn snapshot_path(data_dir: &Path) -> PathBuf {
        data_dir.join("snapshot.json")
    }

    fn save_snapshot(&self) -> Result<()> {
        #[cfg(test)]
        if self.fail_next_snapshot_save.swap(false, Ordering::AcqRel) {
            return Err(StorageError::Io(std::io::Error::other(
                "injected post-publication snapshot save failure",
            )));
        }
        #[cfg(test)]
        {
            let remaining = self.fail_snapshot_save_after.load(Ordering::Acquire);
            if remaining != usize::MAX {
                if remaining == 0 {
                    let failures = self
                        .snapshot_save_failures_remaining
                        .fetch_sub(1, Ordering::AcqRel);
                    if failures > 0 {
                        if failures == 1 {
                            self.fail_snapshot_save_after
                                .store(usize::MAX, Ordering::Release);
                            self.snapshot_save_failures_remaining
                                .store(1, Ordering::Release);
                        }
                        return Err(StorageError::Io(std::io::Error::other(
                            "injected delayed snapshot save failure",
                        )));
                    }
                }
                self.fail_snapshot_save_after
                    .store(remaining - 1, Ordering::Release);
            }
        }
        let snap = self.snapshot.load();
        // P2.3 — `to_vec` (compact) not `to_vec_pretty`: the snapshot is a
        // machine-read manifest (loaded via `from_slice`), never
        // human-edited, and pretty-printing an O(total-segments) list
        // wastes serialize CPU + disk bytes on the flush path.
        let bytes = serde_json::to_vec(&**snap)?;
        let path = Self::snapshot_path(&self.data_dir);
        // Unique tmp filename per caller.  Concurrent shard flushes both
        // call `save_snapshot` from `finalize_flush_with_publisher`; pre-
        // fix we used `snapshot.tmp` for everyone and two racing writers
        // would clobber each other's tmp, leaving one of the `rename`
        // calls to fail with ENOENT.  That aborted the whole shard flush
        // — the shard's docs stayed in memtable until the next tick.
        // Uuid v4 + thread id makes collision essentially impossible.
        let nonce = format!(
            "{}-{:?}",
            Uuid::new_v4().simple(),
            std::thread::current().id(),
        );
        let tmp = path.with_extension(format!("tmp.{nonce}"));
        // RC4 W1 #10 — the snapshot is the manifest that makes flushed
        // segments discoverable on restart, and WAL maintenance prunes the
        // covered entries immediately after this returns.  Both the file
        // bytes and the rename must therefore be durable BEFORE the prune
        // barrier: write + fsync the tmp, rename, fsync the directory.
        // Pre-fix (`fs::write` + `rename`, no fsync anywhere) a power loss
        // within the writeback window could leave an old/absent
        // snapshot.json next to an already-pruned WAL — flushed segments
        // then got GC'd as orphans on reopen (acked-data loss).
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        // Atomic rename onto the real path.  Last writer wins on the
        // final snapshot contents, but that's fine: each caller sees
        // the same `self.snapshot.load()` atomically-swapped payload
        // (arc_swap), so there is no content-level race — only the
        // filesystem tmp name was the contention source.
        std::fs::rename(&tmp, &path)?;
        xerj_common::fsio::fsync_dir(&self.data_dir)?;
        Ok(())
    }

    /// Load the persisted segment manifest (`snapshot.json`).
    ///
    /// Return contract (RC4 W3 #10):
    /// - `Ok(None)` — the manifest is genuinely ABSENT (a fresh index that
    ///   has never flushed). The caller starts from an empty snapshot; the
    ///   WAL replay that follows reconstructs any un-flushed docs.
    /// - `Ok(Some(snap))` — the manifest is present and parsed.
    /// - `Err(IncompatibleDataDir)` — the manifest is PRESENT but could not
    ///   be parsed. This is refused LOUDLY instead of being silently mapped
    ///   to an empty snapshot. Pre-fix, `open()` did
    ///   `load_snapshot(..).unwrap_or_else(|_| IndexSnapshot::empty())`, so a
    ///   single unreadable byte in `snapshot.json` made every `.seg` on disk
    ///   an orphan — which `cleanup_orphaned_segment_files` then DELETED:
    ///   total, silent data loss on one bad read. Refusing keeps the data on
    ///   disk for an operator to recover.
    fn load_snapshot(data_dir: &Path) -> Result<Option<IndexSnapshot>> {
        let path = Self::snapshot_path(data_dir);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match serde_json::from_slice::<IndexSnapshot>(&bytes) {
            Ok(snap) => Ok(Some(snap)),
            Err(e) => Err(StorageError::IncompatibleDataDir(format!(
                "index manifest {} is present but could not be parsed ({e}). \
                 Refusing to open: treating it as empty would orphan and then \
                 delete every segment on disk. Restore snapshot.json from a \
                 backup, or from an interrupted-write `.tmp.*` sibling in the \
                 same directory.",
                path.display()
            ))),
        }
    }

    /// Path of the data-dir format marker.
    fn data_dir_meta_path(data_dir: &Path) -> PathBuf {
        data_dir.join(DATA_DIR_META_FILE)
    }

    /// Verify the data-dir format marker is compatible with this binary,
    /// BEFORE any potentially-destructive open step runs (snapshot load,
    /// orphan GC). Called first thing in `open()`.
    ///
    /// - Marker ABSENT → OK. Either a brand-new data dir or one written by a
    ///   pre-marker (rc3-vintage) xerj; both are safe to open, and `open()`
    ///   stamps a marker on success so subsequent opens are versioned.
    /// - Marker present, parses, `format_version <= DATA_DIR_FORMAT_VERSION`
    ///   → OK.
    /// - Marker present, parses, `format_version` GREATER than ours → REFUSE
    ///   (a newer xerj wrote this dir; we may not understand its layout).
    /// - Marker present but UNPARSEABLE → REFUSE (corrupt marker).
    fn check_data_dir_version(data_dir: &Path) -> Result<()> {
        Self::check_data_dir_version_with_max(data_dir, DATA_DIR_FORMAT_VERSION)
    }

    /// Version-parameterized form used by the compatibility regression to
    /// exercise exactly what a v1 binary does when it sees a v2 marker.
    fn check_data_dir_version_with_max(data_dir: &Path, max_supported: u32) -> Result<()> {
        let path = Self::data_dir_meta_path(data_dir);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let meta: DataDirMeta = serde_json::from_slice(&bytes).map_err(|e| {
            StorageError::IncompatibleDataDir(format!(
                "data-dir format marker {} is present but unparseable ({e}). \
                 Refusing to open so segments are not GC'd as orphans. Restore \
                 a backup or run the matching xerj version.",
                path.display()
            ))
        })?;
        if meta.format_version > max_supported {
            return Err(StorageError::IncompatibleDataDir(format!(
                "data dir {} was written by a newer xerj (data-dir format \
                 version {}); this binary supports up to {}. Refusing to open \
                 — upgrade xerj to a build that understands format {}.",
                data_dir.display(),
                meta.format_version,
                max_supported,
                meta.format_version
            )));
        }
        Ok(())
    }

    /// Stamp the current format marker at the data-dir root if none exists.
    /// Called at the END of a successful `open()`, so fresh dirs and upgraded
    /// rc3-vintage dirs (which had no marker) both become versioned. Never
    /// overwrites an existing (already-compatible) marker, and a failed open
    /// never reaches here — so a dir we refused to open is not left with a
    /// misleading marker.
    fn stamp_data_dir_version(data_dir: &Path) -> Result<()> {
        let path = Self::data_dir_meta_path(data_dir);
        if path.exists() {
            return Ok(());
        }
        let meta = DataDirMeta {
            format_version: DATA_DIR_BASE_FORMAT_VERSION,
            xerj_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let bytes = serde_json::to_vec(&meta)?;
        // Durable write — the marker gates future opens; a torn/absent marker
        // must not silently re-appear as "no marker" and skip the version gate.
        xerj_common::fsio::write_file_durable(&path, &bytes)?;
        Ok(())
    }

    /// Durably fence a writer before it creates digest-derived FTS side-cars.
    ///
    /// The marker reaches disk before the first v2 filename. A v1 process
    /// therefore either sees a v1-only directory or refuses the complete v2
    /// directory; it can never open v2 filenames as if they were v1. The
    /// transition is intentionally monotonic: if later segment publication
    /// fails, retaining v2 is the safe crash/retry outcome.
    ///
    /// Product-level cross-process exclusion is provided by Engine's
    /// `<server-data-dir>/node.lock`. The mutex here covers concurrent flush
    /// and merge callers inside that one supported writer process; opening
    /// multiple raw `IndexStore`s on the same directory remains unsupported.
    pub fn ensure_fts_encoded_field_component_format(&self) -> Result<()> {
        let _format_guard = self.data_dir_format_lock.lock().unwrap();
        if self
            .fts_v2_marker_durability_confirmed
            .load(Ordering::Acquire)
        {
            return Ok(());
        }
        let path = Self::data_dir_meta_path(&self.data_dir);
        let mut document = match std::fs::read(&path) {
            Ok(bytes) => Some(serde_json::from_slice::<serde_json::Value>(&bytes).map_err(
                |error| {
                    StorageError::IncompatibleDataDir(format!(
                        "data-dir format marker {} is present but unparseable ({error}). Refusing to write encoded FTS side-cars.",
                        path.display()
                    ))
                },
            )?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };

        if let Some(value) = document.as_ref() {
            let meta: DataDirMeta = serde_json::from_value(value.clone()).map_err(|error| {
                StorageError::IncompatibleDataDir(format!(
                    "data-dir format marker {} has an incompatible shape ({error}). Refusing to write encoded FTS side-cars.",
                    path.display()
                ))
            })?;
            if meta.format_version > DATA_DIR_FORMAT_VERSION {
                return Err(StorageError::IncompatibleDataDir(format!(
                    "data dir {} records unsupported format version {}; this binary supports up to {}",
                    self.data_dir.display(),
                    meta.format_version,
                    DATA_DIR_FORMAT_VERSION
                )));
            }
            if meta.format_version >= DATA_DIR_FTS_ENCODED_FIELD_COMPONENT_VERSION {
                // Do not trust visibility as proof of durability. A previous
                // attempt may have renamed v2 into place and then failed its
                // durability confirmation. Re-establish that proof now, while
                // still holding the transition lock, before permitting any
                // encoded FTS filename to be created.
                #[cfg(not(windows))]
                if let Some(parent) = path.parent() {
                    xerj_common::fsio::fsync_dir(parent)?;
                }
                // Windows has no parent-directory fsync contract. Rewrite the
                // exact visible marker through the Win32 write-through replace
                // path instead. Besides confirming current writes, this safely
                // upgrades a marker left visible by an older build whose
                // Windows directory-sync shim was a no-op.
                #[cfg(windows)]
                xerj_common::fsio::write_file_durable(&path, &std::fs::read(&path)?)?;
                self.note_fts_v2_marker_durability_confirmed();
                return Ok(());
            }
        }

        let document = document.get_or_insert_with(|| serde_json::json!({}));
        let object = document.as_object_mut().ok_or_else(|| {
            StorageError::IncompatibleDataDir(format!(
                "data-dir format marker {} must be a JSON object",
                path.display()
            ))
        })?;
        object.insert(
            "format_version".to_string(),
            serde_json::Value::from(DATA_DIR_FTS_ENCODED_FIELD_COMPONENT_VERSION),
        );
        object.insert(
            "xerj_version".to_string(),
            serde_json::Value::from(env!("CARGO_PKG_VERSION")),
        );
        let bytes = serde_json::to_vec(document)?;
        #[cfg(any(test, feature = "test-hooks"))]
        {
            let failpoint = self
                .data_dir_format_write_failpoint
                .swap(0, Ordering::AcqRel);
            xerj_common::fsio::write_file_durable_with_hook(&path, &bytes, |stage| {
                use xerj_common::fsio::DurableWriteStage;
                let injected_stage = match failpoint {
                    x if x == DataDirFormatWriteFailpoint::BeforeTempWrite as u8 => {
                        Some(DurableWriteStage::BeforeTempWrite)
                    }
                    x if x == DataDirFormatWriteFailpoint::BeforeRename as u8 => {
                        Some(DurableWriteStage::BeforeRename)
                    }
                    x if x == DataDirFormatWriteFailpoint::BeforeParentFsync as u8 => {
                        Some(DurableWriteStage::BeforeParentFsync)
                    }
                    _ => None,
                };
                if injected_stage == Some(stage) {
                    Err(std::io::Error::other(
                        "injected data-directory marker persistence failure",
                    ))
                } else {
                    Ok(())
                }
            })?;
        }
        #[cfg(not(any(test, feature = "test-hooks")))]
        xerj_common::fsio::write_file_durable(&path, &bytes)?;
        self.note_fts_v2_marker_durability_confirmed();
        Ok(())
    }

    fn note_fts_v2_marker_durability_confirmed(&self) {
        #[cfg(any(test, feature = "test-hooks"))]
        self.fts_v2_marker_durability_confirmations
            .fetch_add(1, Ordering::AcqRel);
        self.fts_v2_marker_durability_confirmed
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_data_dir_format_write_failpoint_for_test(
        &self,
        failpoint: DataDirFormatWriteFailpoint,
    ) {
        self.data_dir_format_write_failpoint
            .store(failpoint as u8, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn fts_v2_marker_durability_confirmations_for_test(&self) -> u64 {
        self.fts_v2_marker_durability_confirmations
            .load(Ordering::Acquire)
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn clear_fts_v2_marker_durability_confirmation_for_test(&self) {
        self.fts_v2_marker_durability_confirmed
            .store(false, Ordering::Release);
        self.fts_v2_marker_durability_confirmations
            .store(0, Ordering::Release);
    }

    // ── Segment version map rebuild ───────────────────────────────────────────

    /// Rebuild the version map from all flushed segments on disk.
    ///
    /// Called once at startup, before WAL replay, so that docs that were
    /// flushed and whose WAL entries were subsequently pruned are still
    /// discoverable via `get_document`.
    fn rebuild_version_map_from_segments(&self) -> Result<()> {
        let snap = self.snapshot.load();
        let segments_dir = self.data_dir.join("segments");
        let mut total = 0usize;

        for meta in &snap.segments {
            // Hoist segment-id Arc once per segment — the per-doc loops
            // below would otherwise do `Arc::from(&meta.id)` per doc, which
            // allocates a fresh shared buffer every time.
            let seg_id_arc: std::sync::Arc<str> = std::sync::Arc::from(meta.id.as_str());
            // V4 M4.8 — fast path via `seg.ids` sidecar written at flush
            // time.  Reads (seq_no, doc_id) pairs directly without
            // touching the stored section.  Falls back to the stored-
            // decode path for legacy segments without the sidecar.
            let ids_path = segments_dir.join(format!("{}.ids", meta.id.as_str()));
            if let Ok(bytes) = std::fs::read(&ids_path) {
                if bytes.len() >= 8
                    && (&bytes[..4] == b"ZID1" || &bytes[..4] == b"ZID2" || &bytes[..4] == b"ZID3")
                {
                    let num = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
                    // V2 = LZ4-compressed body after the 8-byte header.
                    let body: Vec<u8> = if &bytes[..4] == b"ZID2" || &bytes[..4] == b"ZID3" {
                        match lz4_flex::decompress_size_prepended(&bytes[8..]) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!(segment = %meta.id, error = %e, "ZID2 decompress failed, falling back");
                                continue;
                            }
                        }
                    } else {
                        bytes[8..].to_vec()
                    };
                    let mut pos = 0usize;
                    let mut loaded = 0usize;
                    for _ in 0..num {
                        if pos + 8 + 2 > body.len() {
                            break;
                        }
                        let seq_no = u64::from_le_bytes(body[pos..pos + 8].try_into().unwrap());
                        pos += 8;
                        let id_len =
                            u16::from_le_bytes(body[pos..pos + 2].try_into().unwrap()) as usize;
                        pos += 2;
                        if pos + id_len > body.len() {
                            break;
                        }
                        let id_bytes = &body[pos..pos + id_len];
                        pos += id_len;
                        if let Ok(id) = std::str::from_utf8(id_bytes) {
                            self.version_map.set(
                                id,
                                seq_no,
                                std::sync::Arc::clone(&seg_id_arc),
                                false,
                            );
                            loaded += 1;
                        }
                    }
                    total += loaded;
                    continue;
                }
            }

            // Legacy path: decode stored section to extract (id, seq_no).
            let seg_path = segments_dir.join(&meta.seg_path);
            let reader = match SegmentReader::open(&seg_path) {
                Ok(r) => r,
                Err(e) => {
                    warn!(segment = %meta.id, error = %e, "cannot open segment for version map rebuild");
                    continue;
                }
            };

            let stored_bytes_raw = match reader.section(SectionType::Stored) {
                Ok(Some(b)) => b,
                _ => continue,
            };
            let stored_bytes = match crate::stored_codec::decode_stored(stored_bytes_raw) {
                Ok(b) => b,
                Err(e) => {
                    warn!(segment = %meta.id, error = %e, "cannot decode stored section");
                    continue;
                }
            };

            let docs: Vec<serde_json::Value> = match serde_json::from_slice(&stored_bytes) {
                Ok(d) => d,
                Err(e) => {
                    warn!(segment = %meta.id, error = %e, "cannot decode stored docs for version map rebuild");
                    continue;
                }
            };

            for (ordinal, doc) in docs.iter().enumerate() {
                let doc_id = match doc.get("_id").and_then(serde_json::Value::as_str) {
                    Some(id) => id,
                    None => continue,
                };
                let seq_no = (meta.max_seq_no.saturating_sub(docs.len() as u64))
                    .saturating_add(ordinal as u64);
                self.version_map
                    .set(doc_id, seq_no, std::sync::Arc::clone(&seg_id_arc), false);
                total += 1;
            }
        }

        // RC4 W2 #14 — apply SEGMENT-DURABLE tombstones (ZTB2 sections),
        // max-seq-wins against the doc entries loaded above.  This is
        // what lets an acked delete survive a restart WITHOUT its WAL
        // entry (whose retention used to pin the shard's prune forever).
        // Runs after ALL doc entries are loaded so ordering across
        // segments cannot matter: a tombstone only lands if no
        // strictly-newer live version exists anywhere
        // (`set_if_latest`, and delete-vs-doc seqs are never equal).
        // Legacy id-only JSON tombstone sections decode to `None` and
        // are skipped — those deletes are still WAL-pinned as before.
        let mut tombstones_applied = 0usize;
        for meta in &snap.segments {
            if !meta.has_tombstones {
                continue;
            }
            let seg_path = segments_dir.join(&meta.seg_path);
            let reader = match SegmentReader::open(&seg_path) {
                Ok(r) => r,
                Err(e) => {
                    warn!(segment = %meta.id, error = %e, "cannot open segment for tombstone rebuild");
                    continue;
                }
            };
            let ts_bytes = match reader.section(SectionType::Tombstones) {
                Ok(Some(b)) => b,
                _ => continue,
            };
            let Some(pairs) = crate::segment::decode_tombstones_v2(ts_bytes) else {
                continue; // legacy id-only payload — no seqs, skip
            };
            let seg_id_arc: std::sync::Arc<str> = std::sync::Arc::from(meta.id.as_str());
            for (seq, doc_id) in pairs {
                self.version_map.set_if_latest(
                    doc_id,
                    seq,
                    std::sync::Arc::clone(&seg_id_arc),
                    true,
                );
                tombstones_applied += 1;
            }
        }

        if total > 0 || tombstones_applied > 0 {
            info!(
                total,
                tombstones_applied, "version map rebuilt from segments"
            );
        }
        Ok(())
    }

    // ── WAL replay ────────────────────────────────────────────────────────────

    fn replay_wal(&self, wal_dir: &Path) -> Result<()> {
        // Discover legacy + sharded WAL streams and merge-sort by seq_no
        // (shared with the engine-level FTS memtable rebuild so the two
        // replay passes can never diverge on directory layout).
        let all_entries = crate::wal::replay_all_sorted(wal_dir);

        let mut count = 0usize;
        for replay_entry in all_entries {
            match replay_entry.entry {
                WalEntry::Index { doc_id, source } => {
                    let seq_no = replay_entry.seq_no;
                    // Replay idempotence (2026-07, S2): if the version map —
                    // rebuilt from segments BEFORE replay — already shows this
                    // doc live in a real segment at seq_no >= this op, the
                    // exact same op (equal seq: the shutdown flush persisted
                    // it) or a newer version is already segment-durable.
                    // Re-materialising it in the memtable created a SECOND
                    // copy of the same (id, seq_no): the strict `doc_seq <
                    // ver.seq_no` stale-copy predicates on the count paths
                    // don't skip an equal-seq segment copy, so counts were
                    // inflated after a SIGTERM restart whose WAL shard was
                    // pinned by an unpersisted delete (batch-6 pinning
                    // correctly preserved the shard, preserving the already-
                    // flushed overwrite entries with it).  Skip the memtable
                    // push and version_map set; the seq counter is still
                    // fetch_max'd below.
                    //
                    // Caveat: legacy segments without a `.ids` sidecar
                    // rebuild version-map seqs by approximation
                    // (`rebuild_version_map_from_segments`); with the seq
                    // counter now seeded from segment metadata on open, any
                    // post-flush update carries a seq strictly greater than
                    // its segment's max_seq_no, so the approximation cannot
                    // shadow a genuinely newer WAL-only version.
                    let already_persisted = match self.version_map.get(&doc_id) {
                        Some(e) => {
                            !e.deleted
                                && e.seq_no >= seq_no
                                && &*e.segment_id != IN_MEMORY_SEGMENT_ID
                        }
                        None => false,
                    };
                    if !already_persisted {
                        self.version_map
                            .set(&doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);
                        let shard = self.shard_for(&doc_id);
                        let mut mem = self.memtable_shards[shard].lock().unwrap();
                        mem.push(MemEntry {
                            seq_no,
                            doc_id,
                            source: Some(std::sync::Arc::new(source)),
                            source_bytes: std::sync::Arc::from(&[][..]),
                        });
                    }
                }
                WalEntry::Delete { doc_id } => {
                    let seq_no = replay_entry.seq_no;
                    // RC4 W2 #14 — seq-aware replay:
                    //
                    // (a) STALE delete (the map — rebuilt from segments,
                    //     including ZTB2 tombstones — already holds a
                    //     strictly newer state for the doc): skip.
                    //     `VersionMap::delete` is seq-blind; pre-fix a
                    //     retained old Delete entry could tombstone a
                    //     NEWER segment-resident version whose own WAL
                    //     entry had already been pruned.
                    // (b) Tombstone already SEGMENT-resident at >= seq
                    //     (rebuilt from a ZTB2 section): the deletion is
                    //     durable without this WAL entry — don't re-apply,
                    //     don't re-pin, don't re-materialise a memtable
                    //     tombstone (which would re-flush it forever).
                    let cur = self.version_map.get(&doc_id);
                    let stale = cur.as_ref().is_some_and(|e| e.seq_no > seq_no);
                    let already_durable = cur.as_ref().is_some_and(|e| {
                        e.deleted && e.seq_no >= seq_no && &*e.segment_id != IN_MEMORY_SEGMENT_ID
                    });
                    if stale || already_durable {
                        let _ = self
                            .seq_counter
                            .fetch_max(replay_entry.seq_no + 1, Ordering::AcqRel);
                        count += 1;
                        continue;
                    }
                    let applied = self
                        .version_map
                        .delete(&doc_id, seq_no, IN_MEMORY_SEGMENT_ID)
                        .unwrap_or(false);
                    // Delete-durability: re-pin the WAL shard whenever the
                    // tombstone applied to a doc that still exists (it may
                    // be live in a segment) — this Delete entry remains the
                    // only durable record of the delete, so maintenance
                    // after THIS restart must keep refusing to prune it;
                    // otherwise the delete survives one restart and the doc
                    // resurrects on the next.  A delete that applied to
                    // nothing (doc already merge-purged from every segment,
                    // or already tombstoned by an earlier pinned entry) is
                    // vacuous and must not pin the shard forever.
                    // Superseded pins (doc re-indexed later in the replay
                    // stream) are cleared by the next
                    // `sweep_pending_deletes` once the newer version is
                    // segment-resident (or once the tombstone itself is
                    // persisted by `persist_pending_tombstones`).
                    if applied {
                        self.pending_deletes
                            .lock()
                            .unwrap()
                            .insert(doc_id.clone(), (seq_no, self.wal_shard_for(&doc_id)));
                    }
                    let shard = self.shard_for(&doc_id);
                    let mut mem = self.memtable_shards[shard].lock().unwrap();
                    mem.push(MemEntry {
                        seq_no,
                        doc_id,
                        source: None,
                        source_bytes: std::sync::Arc::from(&[][..]),
                    });
                }
                WalEntry::UpdateMapping { .. } => {}
            }
            let _ = self
                .seq_counter
                .fetch_max(replay_entry.seq_no + 1, Ordering::AcqRel);
            count += 1;
        }

        if count > 0 {
            info!(count, "replayed WAL entries");
        }
        Ok(())
    }

    // ── Merge integration ─────────────────────────────────────────────────────

    /// Called by the merge executor (or the engine-level merge task) to
    /// atomically replace merged segments with the merged result and update
    /// the version map.
    pub fn apply_merge(&self, merged_ids: &[SegmentId], new_meta: SegmentMeta) -> Result<()> {
        self.apply_merge_inner(merged_ids, new_meta, None, || {})
            .map(|_| ())
            .map_err(|error| match error {
                MergePublicationError::NotPublished(error) => error,
                MergePublicationError::Indeterminate {
                    publication,
                    rollback,
                } => StorageError::MergeAborted(format!(
                    "merge publication is indeterminate: publication failed ({publication}); rollback persistence failed ({rollback})"
                )),
            })
    }

    /// Publish a merge and take ownership of its provisional version-map
    /// repoints. The transaction commits at the exact durable manifest
    /// boundary, before any fallible or panic-capable post-publication work.
    pub fn apply_merge_with_repoints(
        &self,
        merged_ids: &[SegmentId],
        new_meta: SegmentMeta,
        version_repoints: VersionRepointTransaction,
    ) -> std::result::Result<MergePublicationOutcome, MergePublicationError> {
        self.apply_merge_inner(merged_ids, new_meta, Some(version_repoints), || {})
    }

    /// Test seam for exercising cancellation/panic immediately after durable
    /// publication. Production callers use [`Self::apply_merge_with_repoints`].
    #[doc(hidden)]
    pub fn apply_merge_with_repoints_and_post_publish<F: FnOnce()>(
        &self,
        merged_ids: &[SegmentId],
        new_meta: SegmentMeta,
        version_repoints: VersionRepointTransaction,
        post_publish: F,
    ) -> std::result::Result<MergePublicationOutcome, MergePublicationError> {
        self.apply_merge_inner(merged_ids, new_meta, Some(version_repoints), post_publish)
    }

    fn apply_merge_inner<F: FnOnce()>(
        &self,
        merged_ids: &[SegmentId],
        new_meta: SegmentMeta,
        version_repoints: Option<VersionRepointTransaction>,
        post_publish: F,
    ) -> std::result::Result<MergePublicationOutcome, MergePublicationError> {
        // Inputs still belong to the authoritative snapshot at this point.
        // Durably remove their ZID3 completion markers before committing the
        // replacement, so no crash after snapshot publication can resurrect
        // a partially retired input. The ids sidecars stay readable if this
        // step fails and the merge is aborted.
        self.disarm_orphan_recovery_for_segments(merged_ids)
            .map_err(MergePublicationError::NotPublished)?;
        // Sum the doc counts of the segments we're about to replace, so we can
        // tell whether this merge actually dropped any documents.
        let (merged_total, previous_metas): (u64, Vec<SegmentMeta>) = {
            let snap = self.snapshot.load();
            let previous: Vec<_> = snap
                .segments
                .iter()
                .filter(|s| merged_ids.contains(&s.id))
                .cloned()
                .collect();
            (previous.iter().map(|meta| meta.doc_count).sum(), previous)
        };
        // Atomic replace via rcu — same race as `with_new_segment` in
        // `finalize_flush_with_publisher`: a concurrent flush appending
        // its segment between load and store would drop our merged
        // segment swap. rcu retries on contention.
        self.snapshot
            .rcu(|old| Arc::new(old.replace_segments(merged_ids, new_meta.clone())));
        // #871 — merge application changed the segment set: fire the hook so
        // a cascading follow-up merge schedules itself (Lucene's
        // MergeTrigger.MERGE_FINISHED, IndexWriter.java:2452). Fired before
        // the fallible persistence below because the in-memory set HAS
        // changed on every path from here — including the rollback, which
        // swaps it twice more; a spurious debounced policy check is cheap,
        // a missed one strands merge debt until the next event.
        self.notify_segments_changed();
        if let Err(error) = self.save_snapshot() {
            // Publication changed memory before manifest persistence. Restore
            // the exact inputs only while this output remains authoritative;
            // the RCU closure preserves any concurrent flush append.
            let output_id = new_meta.id.clone();
            self.snapshot.rcu(|current| {
                let output_is_current = current.segments.iter().any(|meta| meta.id == output_id)
                    && previous_metas.iter().all(|previous| {
                        !current.segments.iter().any(|meta| meta.id == previous.id)
                    });
                if !output_is_current {
                    return Arc::clone(current);
                }
                let mut segments: Vec<_> = current
                    .segments
                    .iter()
                    .filter(|meta| meta.id != output_id)
                    .cloned()
                    .collect();
                segments.extend(previous_metas.iter().cloned());
                let max_seq_no = segments
                    .iter()
                    .map(|meta| meta.max_seq_no)
                    .max()
                    .unwrap_or(0);
                Arc::new(IndexSnapshot {
                    segments,
                    generation: current.generation + 1,
                    max_seq_no,
                })
            });
            // The initial error can occur after rename. Best-effort persist
            // the restored snapshot so memory and restart state converge.
            return match self.save_snapshot() {
                Ok(()) => Err(MergePublicationError::NotPublished(error)),
                Err(rollback) => Err(MergePublicationError::Indeterminate {
                    publication: error,
                    rollback,
                }),
            };
        }
        // The replacement is now durable. Commit provisional repoints here,
        // in the same synchronous ownership scope, so cancellation or panic
        // after publication cannot roll them back to retired inputs.
        if let Some(version_repoints) = version_repoints {
            version_repoints.commit();
        }
        post_publish();
        // `remove_segment` does a full O(N) `DashMap::retain` over the ENTIRE
        // version map, holding each shard's write lock — a >1s read-collapse
        // under merge pressure once the map holds millions of entries (reads
        // take the same shard locks via `version_map.get`).  It is only needed
        // to purge stale entries left by documents that were DELETED and
        // tombstone-dropped during the merge: every SURVIVING doc already had
        // its entry repointed to the merged segment (`set_if_latest` in
        // `merge_pass_locked`), so no live doc references the merged-away ids.
        // When the merge dropped nothing — append-only: the new segment's
        // doc_count equals the sum of its inputs — there are zero stale
        // entries, so we skip the sweep entirely and the merge-correlated read
        // stall disappears.  (Skipping can at worst leave a deleted-doc
        // tombstone entry pointing at a gone segment, which reads treat as
        // not-found — harmless; the sweep runs whenever doc_count shrank.)
        if new_meta.doc_count < merged_total {
            self.version_map.remove_segment(merged_ids);
        }
        info!(merged = merged_ids.len(), "merge applied");
        Ok(MergePublicationOutcome::Published {
            maintenance_deferred: false,
        })
    }

    /// Returns stats useful for triggering merges.
    pub fn segment_stats(&self) -> Vec<(SegmentId, u64, u64)> {
        let snap = self.snapshot.load();
        snap.segments
            .iter()
            .map(|s| (s.id.clone(), s.doc_count, s.size_bytes))
            .collect()
    }

    /// Returns the path to the WAL directory for this index store.
    ///
    /// Callers that need to replay WAL entries into their own in-memory
    /// structures (e.g. the FTS memtable in `xerj-engine`) can open a
    /// [`WalReader`] against this directory.
    pub fn wal_dir(&self) -> PathBuf {
        self.data_dir.join("wal")
    }

    /// Append a WAL entry for an indexed document.
    ///
    /// This is a thin wrapper that lets the engine layer write directly to the
    /// WAL without going through the full `IndexStore::index` path.  Useful
    /// when the engine has already applied the mutation to its own in-memory
    /// structures and just needs durability.
    pub fn wal_append_index(&self, doc_id: &str, source: &serde_json::Value) -> Result<SeqNo> {
        let entry = WalEntry::Index {
            doc_id: doc_id.to_owned(),
            source: source.clone(),
        };
        let ws = self.wal_shard_for(doc_id);
        let mut wal = self.wal_lock_shard(ws);
        wal.append(&entry)
    }

    /// Append a WAL entry for a deleted document.
    pub fn wal_append_delete(&self, doc_id: &str) -> Result<SeqNo> {
        let entry = WalEntry::Delete {
            doc_id: doc_id.to_owned(),
        };
        let ws = self.wal_shard_for(doc_id);
        let mut wal = self.wal_lock_shard(ws);
        wal.append(&entry)
    }

    /// Batch-append WAL entries for multiple documents in a single lock acquisition.
    ///
    /// Unlike `index_batch`, this method writes **only** to the WAL — it does
    /// not touch the store's internal memtable.  This is the correct path for
    /// the turbo ingest pipeline, where the engine maintains its own FTS
    /// memtable and does not need the store's storage-layer memtable.
    ///
    /// Returns the assigned sequence numbers in the same order as `docs`.
    /// Batch-append to WAL using `Arc<Value>` sources shared with the caller.
    ///
    /// The caller typically owns an `Arc<Value>` already (from the turbo
    /// ingest pipeline).  Passing an Arc instead of `&Value` means the
    /// memtable push at the end of this method is a pointer bump — not a
    /// deep clone of the JSON tree — and the WAL bytes are written from
    /// the same allocation.  Three per-doc deep clones become zero.
    ///
    /// Each tuple also carries `source_bytes: Arc<[u8]>` — the
    /// **already-serialized** JSON bytes that came in over the wire on
    /// the NDJSON bulk line.  When non-empty, the WAL writes those
    /// bytes verbatim and completely skips the per-doc
    /// `serde_json::to_writer` round-trip.  Empty `source_bytes`
    /// means the caller didn't have the raw payload handy; the WAL
    /// falls back to serializing from the `Value`.
    /// Fast-path WAL append that skips the `Arc<Value>` slot entirely.
    /// Intended for the CLI bulk-ingest `index_batch_sync_raw` path where
    /// we only ever carry raw bytes — the pre-refactor `wal_append_batch`
    /// required callers to synthesize `Arc<Value::Null>` per doc and
    /// allocate a full `Vec<(String, Arc<Value>, Arc<[u8]>)>` per batch,
    /// which at 400 batches/s × 5k docs = 2 M allocs/s of pure overhead.
    ///
    /// All on-disk framing is byte-identical to `wal_append_batch`; the
    /// two entries interleave freely in the WAL.
    /// Validate raw JSON without materializing a DOM.  The returned sealed
    /// value is the only input accepted by [`Self::wal_append_batch_raw`].
    pub fn validate_raw_batch(docs: Vec<RawJsonDoc>) -> Result<ValidatedRawBatch> {
        use rayon::prelude::*;
        docs.par_iter()
            .enumerate()
            .map(|(position, (doc_id, source_bytes))| {
                validate_raw_json_nesting(source_bytes).map_err(|reason| {
                    StorageError::RawBatchValidation {
                        doc_id: doc_id.clone(),
                        position: position + 1,
                        reason,
                    }
                })?;
                serde_json::from_slice::<serde::de::IgnoredAny>(source_bytes)
                    .map(|_| ())
                    .map_err(|error| StorageError::RawBatchValidation {
                        doc_id: doc_id.clone(),
                        position: position + 1,
                        reason: error.to_string(),
                    })
            })
            .collect::<Result<()>>()?;
        Ok(ValidatedRawBatch { docs, parsed: None })
    }

    /// Validate and retain parsed values for callers that need both WAL bytes
    /// and a JSON DOM.  This keeps the async raw ingest path to one parse.
    pub fn parse_raw_batch(docs: Vec<RawJsonDoc>) -> Result<ValidatedRawBatch> {
        use rayon::prelude::*;
        let parsed = docs
            .par_iter()
            .enumerate()
            .map(|(position, (doc_id, source_bytes))| {
                validate_raw_json_nesting(source_bytes).map_err(|reason| {
                    StorageError::RawBatchValidation {
                        doc_id: doc_id.clone(),
                        position: position + 1,
                        reason,
                    }
                })?;
                serde_json::from_slice(source_bytes)
                    .map(std::sync::Arc::new)
                    .map_err(|error| StorageError::RawBatchValidation {
                        doc_id: doc_id.clone(),
                        position: position + 1,
                        reason: error.to_string(),
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ValidatedRawBatch {
            docs,
            parsed: Some(parsed),
        })
    }

    /// Materialize parsed values from a batch whose complete JSON syntax was
    /// already validated. This consumes the sealed proof and retains the
    /// original immutable bytes alongside the DOM.
    pub fn parse_validated_raw_batch(batch: ValidatedRawBatch) -> Result<ValidatedRawBatch> {
        debug_assert!(batch.parsed.is_none());
        Self::parse_raw_batch(batch.docs)
    }

    pub fn wal_append_batch_raw(&self, batch: &ValidatedRawBatch) -> Result<Vec<SeqNo>> {
        let docs = batch.docs();
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        if std::env::var("XERJ_SKIP_WAL").is_ok() {
            warn_skip_wal_once();
            let n = docs.len() as u64;
            let start_seq = self
                .seq_counter
                .fetch_add(n, std::sync::atomic::Ordering::AcqRel);
            let seq_nos: Vec<SeqNo> = (0..docs.len()).map(|i| start_seq + i as u64).collect();
            // Hoist the segment-id Arc once: per-doc cost in the loop is one
            // Arc::clone (single atomic increment) instead of a String alloc.
            let in_memory: std::sync::Arc<str> = std::sync::Arc::from(IN_MEMORY_SEGMENT_ID);
            for (i, (doc_id, _)) in docs.iter().enumerate() {
                self.version_map
                    .set(doc_id, seq_nos[i], std::sync::Arc::clone(&in_memory), false);
            }
            return Ok(seq_nos);
        }

        let n = docs.len() as u64;
        let start_seq = self
            .seq_counter
            .fetch_add(n, std::sync::atomic::Ordering::AcqRel);
        let mut seq_nos: Vec<SeqNo> = Vec::with_capacity(docs.len());

        let est_total: usize = docs.iter().map(|(id, sb)| id.len() + sb.len() + 100).sum();
        let mut frames: Vec<u8> = Vec::with_capacity(est_total);

        for (i, (doc_id, source_bytes)) in docs.iter().enumerate() {
            let seq_no = start_seq + i as u64;
            seq_nos.push(seq_no);

            let payload_start = frames.len();
            // Reserve space for entry_len (4) + seq_no (8) + op (1)
            frames.extend_from_slice(&[0u8; 13]);
            frames.extend_from_slice(br#"{"Index":{"doc_id":""#);
            let needs_escape = doc_id.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20);
            if needs_escape {
                for &b in doc_id.as_bytes() {
                    match b {
                        b'"' => frames.extend_from_slice(br#"\""#),
                        b'\\' => frames.extend_from_slice(br#"\\"#),
                        b'\n' => frames.extend_from_slice(br"\n"),
                        b'\r' => frames.extend_from_slice(br"\r"),
                        b'\t' => frames.extend_from_slice(br"\t"),
                        0x00..=0x1f => {
                            frames.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
                        }
                        _ => frames.push(b),
                    }
                }
            } else {
                frames.extend_from_slice(doc_id.as_bytes());
            }
            frames.extend_from_slice(br#"","source":"#);
            frames.extend_from_slice(source_bytes);
            frames.extend_from_slice(b"}}");
            let payload_end = frames.len();

            let payload_slice = &frames[payload_start + 13..payload_end];
            let payload_len = payload_slice.len() as u32;

            let mut hasher = crc32fast::Hasher::new();
            let mut seq_buf = [0u8; 8];
            use byteorder::{LittleEndian, WriteBytesExt};
            (&mut seq_buf[..])
                .write_u64::<LittleEndian>(seq_no)
                .unwrap();
            hasher.update(&seq_buf);
            hasher.update(&[0x01]); // OP_INDEX
            hasher.update(payload_slice);
            let crc = hasher.finalize();

            frames[payload_start..payload_start + 4].copy_from_slice(&payload_len.to_le_bytes());
            frames[payload_start + 4..payload_start + 12].copy_from_slice(&seq_buf);
            frames[payload_start + 12] = 0x01; // OP_INDEX
            frames.extend_from_slice(&crc.to_le_bytes());
        }
        let total_written = frames.len() as u64;

        {
            let ws = self.wal_shard_for(&docs[0].0);
            let mut wal = self.wal_lock_shard(ws);
            // Suppress the writer's per-append fsync while the pre-framed
            // batch is emitted, then issue at most ONE sync for the whole
            // batch below (group commit).
            let saved_mode = wal.sync_mode();
            wal.set_sync_mode(SyncMode::Batched);
            wal.append_frames_locked(&frames, total_written)?;
            // RC4 W1 #9 — honor the operator's configured durability.
            // Pre-fix this path fsynced ONLY when the undocumented
            // XERJ_STRICT_SYNC env var was set; `wal_sync = "sync"` in the
            // config was silently ignored on every bulk request.
            let strict = self.config.sync_mode == SyncMode::Strict
                || std::env::var("XERJ_STRICT_SYNC")
                    .map(|v| !v.is_empty() && v != "0")
                    .unwrap_or(false);
            let sync_result = if strict { wal.sync() } else { wal.soft_flush() };
            wal.set_sync_mode(saved_mode);
            sync_result?;
        }

        // Hoist the segment-id Arc once per batch: per-doc cost in the loop
        // becomes one Arc::clone (single atomic increment) instead of the
        // previous String allocation that came from `IN_MEMORY_SEGMENT_ID`'s
        // implicit `Into<String>` conversion.
        let in_memory: std::sync::Arc<str> = std::sync::Arc::from(IN_MEMORY_SEGMENT_ID);
        for (i, (doc_id, _)) in docs.iter().enumerate() {
            self.version_map
                .set(doc_id, seq_nos[i], std::sync::Arc::clone(&in_memory), false);
        }

        Ok(seq_nos)
    }

    /// Append parsed documents to the WAL.
    ///
    /// The parsed `Value` is the sole authority and is serialized here.
    /// Accepting a second caller-provided byte representation made it possible
    /// for live indexing and replay to observe different documents; verifying
    /// that representation required a second full DOM parse on the hot path.
    pub fn wal_append_batch(
        &self,
        docs: &[(String, std::sync::Arc<serde_json::Value>)],
    ) -> Result<Vec<SeqNo>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        // M5.5 — build envelopes OUTSIDE the WAL lock in parallel.
        //
        // Pre-M5.5 the per-doc `Vec::with_capacity + doc_id escape loop +
        // extend_from_slice(source_bytes) + 5×BufWriter::write` was all
        // executed while holding the global WAL mutex.  At 32 concurrent
        // workers and 5000 docs/batch that's ~10 ms of mutex hold per
        // batch — 80 batches/sec × 10 ms = 80% lock utilization, capping
        // effective concurrency to ~1.25×.  Pidstat confirmed only
        // ~8/32 worker threads were genuinely busy (30-48% CPU); the
        // remaining ~24 cores sat idle waiting on the mutex.
        //
        // The work of building each doc's JSON envelope is 100%
        // CPU-bound and independent across docs, so we do it with
        // rayon::par_iter outside the lock.  Inside the lock we then
        // only do CRC32 + a single `write_all` of the pre-framed
        // batch buffer.
        if std::env::var("XERJ_SKIP_WAL").is_ok() {
            warn_skip_wal_once();
            let n = docs.len() as u64;
            let start_seq = self
                .seq_counter
                .fetch_add(n, std::sync::atomic::Ordering::AcqRel);
            let seq_nos: Vec<SeqNo> = (0..docs.len()).map(|i| start_seq + i as u64).collect();
            return Ok(seq_nos);
        }

        // Single-pass frame assembly: build WAL envelope + CRC + framing
        // directly into one output buffer. Eliminates the intermediate
        // Vec<Vec<u8>> allocation that was 10k allocs per batch.
        let n = docs.len() as u64;
        let start_seq = self
            .seq_counter
            .fetch_add(n, std::sync::atomic::Ordering::AcqRel);
        let mut seq_nos: Vec<SeqNo> = Vec::with_capacity(docs.len());

        // Estimate total frame size: per-doc overhead ~80 bytes + source
        let est_total: usize = docs.iter().map(|(id, _source)| id.len() + 600).sum();
        let mut frames: Vec<u8> = Vec::with_capacity(est_total);

        for (i, (doc_id, source)) in docs.iter().enumerate() {
            let seq_no = start_seq + i as u64;
            seq_nos.push(seq_no);

            // Build JSON envelope directly
            let payload_start = frames.len();
            // Reserve space for entry_len (4 bytes) + seq_no (8) + op (1)
            frames.extend_from_slice(&[0u8; 13]);
            // Write the payload
            frames.extend_from_slice(br#"{"Index":{"doc_id":""#);
            // Fast path: most doc_ids are alphanumeric + underscore
            let needs_escape = doc_id.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20);
            if needs_escape {
                for &b in doc_id.as_bytes() {
                    match b {
                        b'"' => frames.extend_from_slice(br#"\""#),
                        b'\\' => frames.extend_from_slice(br#"\\"#),
                        b'\n' => frames.extend_from_slice(br"\n"),
                        b'\r' => frames.extend_from_slice(br"\r"),
                        b'\t' => frames.extend_from_slice(br"\t"),
                        0x00..=0x1f => {
                            frames.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
                        }
                        _ => frames.push(b),
                    }
                }
            } else {
                frames.extend_from_slice(doc_id.as_bytes());
            }
            frames.extend_from_slice(br#"","source":"#);
            serde_json::to_writer(&mut frames, source.as_ref())?;
            frames.extend_from_slice(b"}}");
            let payload_end = frames.len();

            // Payload is everything after the 13-byte header
            let payload_slice = &frames[payload_start + 13..payload_end];
            let payload_len = payload_slice.len() as u32;

            // CRC over seq_no(8) + op(1) + payload
            let mut hasher = crc32fast::Hasher::new();
            let mut seq_buf = [0u8; 8];
            use byteorder::{LittleEndian, WriteBytesExt};
            (&mut seq_buf[..])
                .write_u64::<LittleEndian>(seq_no)
                .unwrap();
            hasher.update(&seq_buf);
            hasher.update(&[0x01]); // OP_INDEX
            hasher.update(payload_slice);
            let crc = hasher.finalize();

            // Fill in the header (entry_len + seq_no + op)
            frames[payload_start..payload_start + 4].copy_from_slice(&payload_len.to_le_bytes());
            frames[payload_start + 4..payload_start + 12].copy_from_slice(&seq_buf);
            frames[payload_start + 12] = 0x01; // OP_INDEX

            // Append CRC
            frames.extend_from_slice(&crc.to_le_bytes());
        }
        let total_written = frames.len() as u64;

        {
            let ws = if docs.is_empty() {
                0
            } else {
                self.wal_shard_for(&docs[0].0)
            };
            let mut wal = self.wal_lock_shard(ws);
            let saved_mode = wal.sync_mode();
            wal.set_sync_mode(SyncMode::Batched);
            wal.append_frames_locked(&frames, total_written)?;
            // M5.4 — skip the per-batch fsync on the DEFAULT (batched)
            // bulk hot path: each fsync(2) is ~1 ms on NVMe, ~8 % of
            // ingest wall time at 76 batches/s.  Without it the WAL
            // bytes are in the kernel page cache — a process crash
            // loses nothing; power loss is bounded by the `wal_batch_ms`
            // background fsync loop (RC4 W1 #9).
            //
            // RC4 W1 #9 — `wal_sync = "sync"` (SyncMode::Strict) is now
            // HONORED here: one fsync per bulk request before the ack
            // (group commit — the same granularity as ES's per-request
            // translog fsync).  Pre-fix the config was silently ignored
            // and only the undocumented XERJ_STRICT_SYNC env var (kept
            // as an override) forced the fsync.
            let strict = self.config.sync_mode == SyncMode::Strict
                || std::env::var("XERJ_STRICT_SYNC")
                    .map(|v| !v.is_empty() && v != "0")
                    .unwrap_or(false);
            let sync_result = if strict {
                wal.sync()
            } else {
                // Just flush the BufWriter to the kernel, skip
                // `fsync(2)`.  This costs ~100 ns vs ~1 ms.
                wal.soft_flush()
            };
            wal.set_sync_mode(saved_mode);
            sync_result?;
        }

        // Populate the storage memtable so `flush()` has data to drain —
        // otherwise the memtable would be empty at flush time and the segment
        // would contain no stored fields.  This is the critical link between
        // turbo ingest and durable storage.
        //
        // V4 M4.7 — dropped the per-doc `source.to_string().len()` call.
        // It was a full JSON re-serialisation **per document** whose only
        // purpose was computing the memtable byte accounting.  On the
        // 60 k-doc/s hot path that was burning ~40 % of per-doc CPU
        // allocating JSON strings and then throwing them away.  The
        // `memtable_bytes` counter only drives back-pressure, which
        // needs a ballpark — 500 bytes/doc is a fine approximation for
        // log data and keeps the back-pressure math within 2× of truth.
        // M5.2 — `wal_append_batch` is now WAL-ONLY.  The engine
        // memtable (sharded, authoritative) is populated by the
        // caller under its own shard lock; the storage memtable is
        // no longer pushed to on the live ingest path so the two
        // memtables can't desync at flush time.
        //
        // The version_map still needs to learn about the new docs so
        // lookups before flush resolve to `IN_MEMORY_SEGMENT_ID`.
        // This is the only per-doc side effect this method has
        // outside the WAL itself.
        for (i, (doc_id, _source)) in docs.iter().enumerate() {
            let seq_no = seq_nos[i];
            self.version_map
                .set(doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);
        }

        Ok(seq_nos)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Structural bound: 16 empty indices × 8 WAL shards each must retain
    /// exactly `128 × 64 KiB` of userspace buffer capacity — not the old
    /// `128 × 8 MiB` reservation.  Asserts `buffer_capacity()` (deterministic)
    /// rather than process RSS.
    #[test]
    fn empty_multi_index_wal_buffers_have_a_bounded_total_capacity() {
        let root = tempfile::tempdir().unwrap();
        let mut stores = Vec::new();
        let mut total_capacity = 0usize;

        for index in 0..16 {
            let store = IndexStore::open(
                root.path().join(format!("index-{index}")),
                IndexStoreConfig {
                    sync_mode: SyncMode::Batched,
                    wal_batch_ms: 0,
                    num_wal_shards: 8,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(store.wal_shards.len(), 8);
            total_capacity += store
                .wal_shards
                .iter()
                .map(|writer| writer.lock().unwrap().buffer_capacity())
                .sum::<usize>();
            stores.push(store);
        }

        assert_eq!(stores.len(), 16);
        assert_eq!(total_capacity, 16 * 8 * 64 * 1024);
        assert!(
            total_capacity <= 8 * 1024 * 1024,
            "128 idle WAL shards retained {total_capacity} bytes of buffers"
        );
    }

    fn open_test_store(dir: &Path) -> Arc<IndexStore> {
        IndexStore::open(
            dir,
            IndexStoreConfig {
                sync_mode: SyncMode::Batched, // faster for tests
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn wal_bytes(store: &IndexStore) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        for entry in std::fs::read_dir(store.wal_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                files.push((
                    path.file_name().unwrap().into(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }

    #[test]
    fn completion_manifest_rejects_corruption_and_noncanonical_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let segment_id = "12345678-1234-1234-1234-123456789abc";
        let segments = dir.path().join("segments");
        let seg_name = format!("{segment_id}.seg");
        let sidx_name = format!("{segment_id}.sidx");
        let ids_name = format!("{segment_id}.ids");
        std::fs::write(segments.join(&seg_name), b"segment").unwrap();
        std::fs::write(segments.join(&sidx_name), b"sidx").unwrap();
        std::fs::write(segments.join(&ids_name), b"ids").unwrap();
        let meta = SegmentMeta {
            id: segment_id.to_owned(),
            doc_count: 2,
            size_bytes: 7,
            min_seq_no: 10,
            max_seq_no: 11,
            created_at_ms: 0,
            has_tombstones: false,
            seg_path: seg_name.clone(),
            sidx_path: format!("{segment_id}.sidx"),
        };
        store.write_flush_completion_manifest(&meta).unwrap();
        assert!(store.validate_flush_completion_manifest(segment_id, 2, 10, 11));

        let complete = segments.join(format!("{segment_id}.complete"));
        let canonical = std::fs::read(&complete).unwrap();
        let write_records = |records: &[(&str, u64, u32)]| {
            let mut body = Vec::new();
            body.extend_from_slice(b"ZCM1");
            body.extend_from_slice(&(segment_id.len() as u16).to_le_bytes());
            body.extend_from_slice(segment_id.as_bytes());
            body.extend_from_slice(&2u64.to_le_bytes());
            body.extend_from_slice(&10u64.to_le_bytes());
            body.extend_from_slice(&11u64.to_le_bytes());
            body.extend_from_slice(&(records.len() as u32).to_le_bytes());
            for (name, size, crc) in records {
                body.extend_from_slice(&(name.len() as u16).to_le_bytes());
                body.extend_from_slice(name.as_bytes());
                body.extend_from_slice(&size.to_le_bytes());
                body.extend_from_slice(&crc.to_le_bytes());
            }
            body.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
            std::fs::write(&complete, body).unwrap();
        };
        let seg_record = (seg_name.as_str(), 7, crc32fast::hash(b"segment"));
        let sidx_record = (sidx_name.as_str(), 4, crc32fast::hash(b"sidx"));
        let ids_record = (ids_name.as_str(), 3, crc32fast::hash(b"ids"));

        let rewrite_crc = |bytes: &mut Vec<u8>| {
            let payload_len = bytes.len() - 4;
            let crc = crc32fast::hash(&bytes[..payload_len]);
            bytes[payload_len..].copy_from_slice(&crc.to_le_bytes());
        };
        let mut unknown_version = canonical.clone();
        unknown_version[..4].copy_from_slice(b"ZCM9");
        rewrite_crc(&mut unknown_version);
        std::fs::write(&complete, unknown_version).unwrap();
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));

        let mut huge_count = canonical.clone();
        let count_offset = 4 + 2 + segment_id.len() + 8 * 3;
        huge_count[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        rewrite_crc(&mut huge_count);
        std::fs::write(&complete, huge_count).unwrap();
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));

        let mut corrupt_envelope = canonical.clone();
        *corrupt_envelope.last_mut().unwrap() ^= 1;
        std::fs::write(&complete, corrupt_envelope).unwrap();
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));

        let mut corrupt_payload = canonical.clone();
        corrupt_payload[6] ^= 1;
        std::fs::write(&complete, corrupt_payload).unwrap();
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));

        write_records(&[seg_record, sidx_record]);
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));
        write_records(&[
            (seg_name.as_str(), 8, seg_record.2),
            sidx_record,
            ids_record,
        ]);
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));
        write_records(&[(seg_name.as_str(), 7, 0), sidx_record, ids_record]);
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));
        write_records(&[seg_record, sidx_record, ids_record]);
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));
        write_records(&[seg_record, seg_record, sidx_record, ids_record]);
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));
        write_records(&[("../escape", 7, seg_record.2), sidx_record, ids_record]);
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));

        std::fs::write(&complete, canonical).unwrap();
        std::fs::remove_file(segments.join(&ids_name)).unwrap();
        assert!(!store.validate_flush_completion_manifest(segment_id, 2, 10, 11));
        store.cleanup_orphaned_segment_files().unwrap();
        assert!(!complete.exists(), "orphan cleanup must remove `.complete`");
    }

    #[test]
    fn fts_layout_discriminator_is_manifested_and_removed_with_its_segment() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        // `delete_segment_files` intentionally recognizes only the engine's
        // canonical UUID-shaped segment prefix. Use a real-shaped id so this
        // assertion exercises production cleanup rather than a no-op fixture.
        let segment_id = "12345678-1234-1234-1234-123456789abc";
        let segments = dir.path().join("segments");
        for (role, bytes) in [
            ("seg", b"segment".as_slice()),
            ("sidx", b"sidx".as_slice()),
            ("ids", b"ids".as_slice()),
            ("fts-layout-v2", b"XERJ_FTS_FILENAME_LAYOUT_V2\n".as_slice()),
        ] {
            std::fs::write(segments.join(format!("{segment_id}.{role}")), bytes).unwrap();
        }
        let meta = SegmentMeta {
            id: segment_id.to_owned(),
            doc_count: 1,
            size_bytes: 7,
            min_seq_no: 1,
            max_seq_no: 1,
            created_at_ms: 0,
            has_tombstones: false,
            seg_path: format!("{segment_id}.seg"),
            sidx_path: format!("{segment_id}.sidx"),
        };
        store.write_flush_completion_manifest(&meta).unwrap();
        assert!(store.validate_flush_completion_manifest(segment_id, 1, 1, 1));

        let (removed, _) = store.delete_segment_files(&[segment_id.to_owned()]);
        assert_eq!(removed, 5, "segment family includes the discriminator");
        assert!(std::fs::read_dir(&segments)
            .unwrap()
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{segment_id}."))));
    }

    #[test]
    fn artifact_crc_streams_large_inputs_in_bounded_chunks() {
        struct TrackingReader {
            remaining: usize,
            max_request: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl std::io::Read for TrackingReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.max_request.fetch_max(buffer.len(), Ordering::Relaxed);
                let count = self.remaining.min(buffer.len());
                buffer[..count].fill(0x5a);
                self.remaining -= count;
                Ok(count)
            }
        }
        let max_request = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let total = 8 * 1024 * 1024 + 17;
        let crc = IndexStore::stream_crc32_from_reader(TrackingReader {
            remaining: total,
            max_request: Arc::clone(&max_request),
        })
        .unwrap();
        let mut expected = crc32fast::Hasher::new();
        for _ in 0..128 {
            expected.update(&[0x5a; 64 * 1024]);
        }
        expected.update(&[0x5a; 17]);
        assert_eq!(crc, expected.finalize());
        assert_eq!(max_request.load(Ordering::Relaxed), 64 * 1024);
    }

    #[test]
    fn orphan_recovery_rejects_incomplete_v3_and_malformed_legacy_ids() {
        enum Mutation {
            V3WithoutComplete,
            OversizedComplete,
            LegacyPartial,
            LegacyTrailing,
            LegacyHeaderMismatch,
            LegacyInvalidUtf8,
        }
        for mutation in [
            Mutation::V3WithoutComplete,
            Mutation::OversizedComplete,
            Mutation::LegacyPartial,
            Mutation::LegacyTrailing,
            Mutation::LegacyHeaderMismatch,
            Mutation::LegacyInvalidUtf8,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let segment_id;
            {
                let store = open_test_store(dir.path());
                store.save_snapshot().unwrap();
                let empty_snapshot = std::fs::read(dir.path().join("snapshot.json")).unwrap();
                store.index("doc", serde_json::json!({"v": 1})).unwrap();
                segment_id = store.flush().unwrap().unwrap().id;
                std::fs::write(dir.path().join("snapshot.json"), empty_snapshot).unwrap();
                let segments = dir.path().join("segments");
                let ids = segments.join(format!("{segment_id}.ids"));
                let complete = segments.join(format!("{segment_id}.complete"));
                if !matches!(mutation, Mutation::OversizedComplete) {
                    std::fs::remove_file(&complete).unwrap();
                }
                match mutation {
                    Mutation::V3WithoutComplete => {}
                    Mutation::OversizedComplete => {
                        std::fs::write(complete, vec![0u8; 4 * 1024 * 1024 + 1]).unwrap();
                    }
                    Mutation::LegacyPartial => {
                        let mut bytes = Vec::from(&b"ZID1"[..]);
                        bytes.extend_from_slice(&1u32.to_le_bytes());
                        bytes.extend_from_slice(&1u64.to_le_bytes());
                        bytes.extend_from_slice(&4u16.to_le_bytes());
                        bytes.extend_from_slice(b"do");
                        std::fs::write(ids, bytes).unwrap();
                    }
                    Mutation::LegacyTrailing => {
                        let mut bytes = Vec::from(&b"ZID1"[..]);
                        bytes.extend_from_slice(&1u32.to_le_bytes());
                        bytes.extend_from_slice(&1u64.to_le_bytes());
                        bytes.extend_from_slice(&3u16.to_le_bytes());
                        bytes.extend_from_slice(b"doc");
                        bytes.push(0xff);
                        std::fs::write(ids, bytes).unwrap();
                    }
                    Mutation::LegacyHeaderMismatch => {
                        store
                            .write_ids_sidecar(&segment_id, &[(999, "doc")])
                            .unwrap();
                    }
                    Mutation::LegacyInvalidUtf8 => {
                        let mut bytes = Vec::from(&b"ZID1"[..]);
                        bytes.extend_from_slice(&1u32.to_le_bytes());
                        bytes.extend_from_slice(&1u64.to_le_bytes());
                        bytes.extend_from_slice(&1u16.to_le_bytes());
                        bytes.push(0xff);
                        std::fs::write(ids, bytes).unwrap();
                    }
                }
                drop(store);
            }
            let reopened = open_test_store(dir.path());
            assert!(
                reopened
                    .snapshot()
                    .segments
                    .iter()
                    .all(|segment| segment.id != segment_id),
                "malformed/incomplete orphan was recovered"
            );
            assert!(
                !dir.path()
                    .join("segments")
                    .join(format!("{segment_id}.seg"))
                    .exists(),
                "rejected orphan family must be cleaned"
            );
        }
    }

    #[test]
    fn completed_zid3_orphan_recovers_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let empty_snapshot;
        let segment_id;
        {
            let store = open_test_store(dir.path());
            store.save_snapshot().unwrap();
            empty_snapshot = std::fs::read(dir.path().join("snapshot.json")).unwrap();
            store.index("doc", serde_json::json!({"v": 1})).unwrap();
            segment_id = store.flush().unwrap().unwrap().id;
            assert!(dir
                .path()
                .join("segments")
                .join(format!("{segment_id}.complete"))
                .exists());
        }
        std::fs::write(dir.path().join("snapshot.json"), empty_snapshot).unwrap();
        let reopened = open_test_store(dir.path());
        assert_eq!(reopened.snapshot().segments.len(), 1);
        assert_eq!(reopened.snapshot().segments[0].id, segment_id);
        assert_eq!(
            reopened.version_map.get("doc").unwrap().segment_id.as_ref(),
            segment_id
        );
        drop(reopened);
        let reopened_again = open_test_store(dir.path());
        assert_eq!(reopened_again.snapshot().segments.len(), 1);
        assert_eq!(reopened_again.snapshot().segments[0].id, segment_id);
    }

    #[test]
    fn direct_flush_failure_restores_for_retry_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store
            .index("direct-survivor", serde_json::json!({"value": 7}))
            .unwrap();
        let failed = store.flush_with_publisher(|_| {
            Err(StorageError::Io(std::io::Error::other(
                "injected direct publisher failure",
            )))
        });
        assert!(failed.is_err());
        assert!(store.snapshot().segments.is_empty());
        assert_eq!(
            store
                .memtable_shards
                .iter()
                .map(|shard| shard.lock().unwrap().len())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            store
                .version_map
                .get("direct-survivor")
                .unwrap()
                .segment_id
                .as_ref(),
            IN_MEMORY_SEGMENT_ID
        );

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = store.flush_with_publisher(|_| -> Result<()> {
                panic!("injected direct publisher panic")
            });
        }));
        assert!(panicked.is_err());
        assert_eq!(
            store
                .memtable_shards
                .iter()
                .map(|shard| shard.lock().unwrap().len())
                .sum::<usize>(),
            1,
            "direct panic must restore exactly one retry owner"
        );

        let meta = store.flush().unwrap().expect("restored drain must retry");
        assert_eq!(meta.doc_count, 1);
        drop(store);

        let reopened = open_test_store(dir.path());
        assert_eq!(reopened.snapshot().segments.len(), 1);
        assert_eq!(
            reopened
                .version_map
                .get("direct-survivor")
                .unwrap()
                .segment_id
                .as_ref(),
            meta.id
        );
    }

    #[test]
    fn prepublication_rollback_cannot_recover_restored_nonmarker_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store
            .index("rollback-doc", serde_json::json!({"v": 1}))
            .unwrap();
        let drained = store.take_memtable_for_flush().unwrap();
        store.publication_failpoint.store(1, Ordering::Release);
        assert!(store
            .finalize_flush_with_publisher(&drained, |_| Ok(()))
            .is_err());
        store.publication_failpoint.store(0, Ordering::Release);

        let segments = dir.path().join("segments");
        let segment_id = std::fs::read_dir(&segments)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".seg").map(str::to_owned)
            });
        assert!(segment_id.is_none(), "ordinary cleanup should remove data");

        // Model a crash image that retained non-marker files after the
        // directory fsync. No complete/ids evidence may accompany them.
        let fake_id = uuid::Uuid::new_v4().to_string();
        std::fs::write(segments.join(format!("{fake_id}.seg")), b"leftover").unwrap();
        std::fs::write(segments.join(format!("{fake_id}.sidx")), b"leftover").unwrap();
        assert!(!segments.join(format!("{fake_id}.complete")).exists());
        assert!(!segments.join(format!("{fake_id}.ids")).exists());
        drop(store);

        let reopened = open_test_store(dir.path());
        assert!(reopened
            .snapshot()
            .segments
            .iter()
            .all(|segment| segment.id != fake_id));
        assert!(!segments.join(format!("{fake_id}.seg")).exists());
    }

    #[test]
    fn partial_multi_input_disarm_aborts_merge_and_restarts_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store.index("merge-a", serde_json::json!({"v": 1})).unwrap();
        store.flush().unwrap();
        store.index("merge-b", serde_json::json!({"v": 2})).unwrap();
        store.flush().unwrap();
        let ids: Vec<_> = store
            .snapshot()
            .segments
            .iter()
            .map(|segment| segment.id.clone())
            .collect();
        assert_eq!(ids.len(), 2);
        store.orphan_disarm_fail_after.store(0, Ordering::Release);
        let executor = crate::merge::MergeExecutor::new(
            Arc::clone(&store),
            crate::merge::MergeConfig {
                io_rate_mb_per_sec: 0,
                ..Default::default()
            },
        );
        assert!(executor.execute_merge(&ids).is_err());
        assert_eq!(store.snapshot().segments.len(), 2);
        for id in &ids {
            assert!(!dir
                .path()
                .join("segments")
                .join(format!("{id}.complete"))
                .exists());
            assert!(store.open_segment(id).is_ok());
        }
        store
            .orphan_disarm_fail_after
            .store(usize::MAX, Ordering::Release);
        assert_eq!(
            IndexStore::load_snapshot(dir.path())
                .unwrap()
                .unwrap()
                .segments
                .len(),
            2,
            "failed merge must not change the persisted snapshot"
        );
        drop(store);

        let reopened = open_test_store(dir.path());
        assert_eq!(reopened.snapshot().segments.len(), 2);
        for id in &ids {
            assert!(reopened.open_segment(id).is_ok());
        }
        assert!(reopened.version_map.get("merge-a").is_some());
        assert!(reopened.version_map.get("merge-b").is_some());
    }

    fn rewrite_ids_as_legacy(segments: &Path, segment_id: &str, magic: &[u8; 4]) {
        let path = segments.join(format!("{segment_id}.ids"));
        let current = std::fs::read(&path).unwrap();
        assert_eq!(&current[..4], b"ZID3");
        let mut legacy = Vec::from(&magic[..]);
        legacy.extend_from_slice(&current[4..8]);
        if magic == b"ZID1" {
            legacy.extend_from_slice(&lz4_flex::decompress_size_prepended(&current[8..]).unwrap());
        } else {
            legacy.extend_from_slice(&current[8..]);
        }
        std::fs::write(path, legacy).unwrap();
        std::fs::remove_file(segments.join(format!("{segment_id}.complete"))).unwrap();
    }

    #[test]
    fn valid_zid1_and_zid2_orphans_recover_exactly_once() {
        for magic in [b"ZID1", b"ZID2"] {
            let dir = tempfile::tempdir().unwrap();
            let empty_snapshot;
            let segment_id;
            {
                let store = open_test_store(dir.path());
                store.save_snapshot().unwrap();
                empty_snapshot = std::fs::read(dir.path().join("snapshot.json")).unwrap();
                store
                    .index("legacy-doc", serde_json::json!({"v": 1}))
                    .unwrap();
                segment_id = store.flush().unwrap().unwrap().id;
            }
            rewrite_ids_as_legacy(&dir.path().join("segments"), &segment_id, magic);
            std::fs::write(dir.path().join("snapshot.json"), &empty_snapshot).unwrap();

            let reopened = open_test_store(dir.path());
            assert_eq!(reopened.snapshot().segments.len(), 1);
            assert_eq!(reopened.snapshot().segments[0].id, segment_id);
            assert_eq!(
                reopened
                    .version_map
                    .get("legacy-doc")
                    .unwrap()
                    .segment_id
                    .as_ref(),
                segment_id
            );
            drop(reopened);

            let reopened_again = open_test_store(dir.path());
            assert_eq!(reopened_again.snapshot().segments.len(), 1);
            assert_eq!(reopened_again.snapshot().segments[0].id, segment_id);
        }
    }

    #[test]
    fn snapshot_listed_legacy_segments_open_after_upgrade_without_complete_manifest() {
        for magic in [b"ZID1", b"ZID2"] {
            let dir = tempfile::tempdir().unwrap();
            let segment_id;
            {
                let store = open_test_store(dir.path());
                store
                    .index("legacy-doc", serde_json::json!({"v": 1}))
                    .unwrap();
                segment_id = store.flush().unwrap().unwrap().id;
            }
            rewrite_ids_as_legacy(&dir.path().join("segments"), &segment_id, magic);

            let reopened = open_test_store(dir.path());
            assert_eq!(reopened.snapshot().segments.len(), 1);
            assert_eq!(reopened.snapshot().segments[0].id, segment_id);
            assert_eq!(
                reopened
                    .open_segment(&segment_id)
                    .unwrap()
                    .header()
                    .doc_count,
                1
            );
            assert_eq!(
                reopened
                    .version_map
                    .get("legacy-doc")
                    .unwrap()
                    .segment_id
                    .as_ref(),
                segment_id
            );
        }
    }

    #[test]
    fn postpublication_maintenance_failures_retain_wal_and_never_duplicate() {
        for fail_snapshot in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let segment_id;
            {
                let store = open_test_store(dir.path());
                store.index("only", serde_json::json!({"v": 1})).unwrap();
                if fail_snapshot {
                    store.fail_next_snapshot_save.store(true, Ordering::Release);
                } else {
                    store
                        .fail_next_wal_maintenance
                        .store(true, Ordering::Release);
                }
                let meta = store
                    .flush()
                    .expect("post-publication maintenance is deferred, not a flush error")
                    .expect("one segment published");
                segment_id = meta.id;
                assert_eq!(store.snapshot().segments.len(), 1);
                assert_eq!(
                    store.version_map.get("only").unwrap().segment_id.as_ref(),
                    segment_id
                );
                assert!(
                    wal_bytes(&store).iter().any(|(_, bytes)| !bytes.is_empty()),
                    "deferred maintenance must retain a replayable WAL"
                );
                assert!(store.flush().unwrap().is_none(), "retry must not duplicate");
                assert_eq!(store.snapshot().segments.len(), 1);
            }

            let reopened = open_test_store(dir.path());
            let snap = reopened.snapshot();
            assert_eq!(snap.segments.len(), 1, "restart must recover exactly once");
            assert_eq!(snap.segments[0].id, segment_id);
            let live = reopened.version_map.get("only").unwrap();
            assert!(!live.deleted);
            assert_eq!(live.segment_id.as_ref(), segment_id);
            drop(snap);
            assert!(reopened.flush().unwrap().is_none());
            assert_eq!(reopened.snapshot().segments.len(), 1);
        }
    }

    #[test]
    fn publication_journal_rolls_back_before_rcu_and_commits_after_rcu() {
        for stage in 1u8..=4 {
            let dir = tempfile::tempdir().unwrap();
            let store = open_test_store(dir.path());
            store.index("only", serde_json::json!({"v": 1})).unwrap();
            let drained = store.take_memtable_for_flush().unwrap();
            store.publication_failpoint.store(stage, Ordering::Release);
            let outcome = store.finalize_flush_with_publisher(&drained, |_| Ok(()));

            if stage <= 3 {
                assert!(outcome.is_err(), "stage {stage} is pre-publication");
                assert!(store.snapshot().segments.is_empty());
                let current = store.version_map.get("only").unwrap();
                assert_eq!(
                    current.segment_id.as_ref(),
                    crate::version_map::IN_MEMORY_SEGMENT_ID
                );
                assert!(
                    std::fs::read_dir(dir.path().join("segments"))
                        .unwrap()
                        .flatten()
                        .next()
                        .is_none(),
                    "precommit rollback must remove the complete artifact family"
                );
            } else {
                let FlushFinalizeOutcome::Published {
                    meta,
                    maintenance_deferred,
                } = outcome.unwrap()
                else {
                    panic!("post-RCU panic must return Published")
                };
                assert!(maintenance_deferred);
                assert_eq!(store.snapshot().segments.len(), 1);
                assert_eq!(store.snapshot().segments[0].id, meta.id);
                assert_eq!(
                    store.version_map.get("only").unwrap().segment_id.as_ref(),
                    meta.id
                );
            }
            assert!(
                wal_bytes(&store).iter().any(|(_, bytes)| !bytes.is_empty()),
                "publication failpoint must retain WAL"
            );
            drop(store);
            let reopened = open_test_store(dir.path());
            assert!(reopened
                .version_map
                .get("only")
                .filter(|entry| !entry.deleted)
                .is_some());
            assert!(reopened.snapshot().segments.len() <= 1);
        }
    }

    #[test]
    fn publication_rollback_never_overwrites_newer_put_or_delete() {
        for stage in [5u8, 6u8] {
            let dir = tempfile::tempdir().unwrap();
            let store = open_test_store(dir.path());
            store.index("same", serde_json::json!({"v": 1})).unwrap();
            let drained = store.take_memtable_for_flush().unwrap();
            let old_seq = drained.entries[0].seq_no;
            store.publication_failpoint.store(stage, Ordering::Release);
            assert!(store
                .finalize_flush_with_publisher(&drained, |_| Ok(()))
                .is_err());
            let current = store.version_map.get("same").unwrap();
            assert_eq!(current.seq_no, old_seq + 1);
            assert_eq!(current.deleted, stage == 6);
            assert_eq!(
                current.segment_id.as_ref(),
                crate::version_map::IN_MEMORY_SEGMENT_ID
            );
            assert!(store.snapshot().segments.is_empty());
            assert!(std::fs::read_dir(dir.path().join("segments"))
                .unwrap()
                .flatten()
                .next()
                .is_none());
        }
    }

    #[test]
    fn raw_wal_batch_rejects_entire_invalid_batch_without_side_effects() {
        let invalid_payloads: &[&[u8]] = &[
            br#"{"truncated":"#,
            br#"{"valid":true} trailing"#,
            &[0xff, 0xfe, 0xfd],
        ];

        for (case, invalid) in invalid_payloads.iter().enumerate() {
            for bad_at in 0..3 {
                let dir = tempfile::tempdir().unwrap();
                let store = open_test_store(dir.path());
                let before_seq = store.current_seq_no();
                let before_wal = wal_bytes(&store);
                let mut docs = vec![
                    (
                        "first".to_owned(),
                        Arc::<[u8]>::from(br#"{"v":1}"#.as_slice()),
                    ),
                    (
                        "middle".to_owned(),
                        Arc::<[u8]>::from(br#"{"v":2}"#.as_slice()),
                    ),
                    (
                        "last".to_owned(),
                        Arc::<[u8]>::from(br#"{"v":3}"#.as_slice()),
                    ),
                ];
                docs[bad_at].1 = Arc::<[u8]>::from(*invalid);

                let error = IndexStore::validate_raw_batch(docs.clone()).unwrap_err();
                assert!(
                    matches!(
                        error,
                        StorageError::RawBatchValidation {
                            ref doc_id,
                            position,
                            ..
                        } if doc_id == &docs[bad_at].0 && position == bad_at + 1
                    ),
                    "case {case}, position {bad_at}: {error:?}"
                );
                let rendered = error.to_string();
                assert!(rendered.contains("Fix:"));
                assert!(rendered.contains("Related help:"));
                assert_eq!(store.current_seq_no(), before_seq);
                assert_eq!(wal_bytes(&store), before_wal);
                for (id, _) in &docs {
                    assert!(store.version_map.get(id).is_none(), "{id} became visible");
                }

                drop(store);
                let reopened = open_test_store(dir.path());
                assert_eq!(reopened.current_seq_no(), before_seq);
                for (id, _) in &docs {
                    assert!(
                        reopened.version_map.get(id).is_none(),
                        "{id} appeared after replay"
                    );
                }
            }
        }
    }

    #[test]
    fn raw_wal_batch_accepts_complete_scalar_object_and_array_values() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let docs = vec![
            ("scalar".to_owned(), Arc::<[u8]>::from(b"42".as_slice())),
            (
                "object".to_owned(),
                Arc::<[u8]>::from(br#"{"answer":42}"#.as_slice()),
            ),
            (
                "array".to_owned(),
                Arc::<[u8]>::from(br#"[1,2,3]"#.as_slice()),
            ),
        ];
        let start_seq = store.current_seq_no();
        let validated = IndexStore::validate_raw_batch(docs.clone()).unwrap();
        assert_eq!(
            store.wal_append_batch_raw(&validated).unwrap(),
            vec![start_seq, start_seq + 1, start_seq + 2]
        );
        for (id, _) in &docs {
            assert!(store.version_map.get(id).is_some(), "{id} not visible");
        }
        drop(store);

        let reopened = open_test_store(dir.path());
        for (id, _) in &docs {
            assert!(
                reopened.version_map.get(id).is_some(),
                "{id} missing after replay"
            );
        }
    }

    /// RC4 W1 #8 regression — bulk-during-flush + kill -9 must lose ZERO
    /// acked writes.
    ///
    /// Live repro this encodes (stream C evidence, 2026-07-12): 50 000-doc
    /// bulk A, `_flush` dispatched, 50-doc bulk B acked while A's flush
    /// finalize was in flight, kill -9 after the flush returned, restart →
    /// `total=50000`, bulk-B survivors **0/50**.  Root cause: flush-time
    /// WAL maintenance checkpointed every shard with a global max_seq
    /// (`current_seq_no()-1` / the fresh segment's max) + full file
    /// offset, then `prune()` deleted any checkpointed generation — B's
    /// WAL entries (memtable-only) were destroyed while B sat in RAM.
    ///
    /// In-process kill simulation: `drop(store)` without a flush is
    /// equivalent to SIGKILL for durability purposes — the memtable (RAM)
    /// is gone, and the WAL bytes that survive are exactly those the
    /// appends already pushed to the kernel (every batched append
    /// soft-flushes, so the userspace buffer is empty at kill time).
    #[test]
    fn bulk_during_flush_kill9_loses_zero_acked_writes() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = open_test_store(dir.path());

            // Bulk A: acked, then drained for a flush (segment write "in
            // flight" — this is the `_flush` racing point).
            for i in 0..100 {
                store
                    .index(format!("a{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            let drained = store.take_memtable_for_flush().unwrap();

            // Bulk B: 50 docs acked AFTER the drain — they miss the
            // in-flight segment and live only in WAL + memtable.
            for i in 0..50 {
                store
                    .index(format!("b{i}"), serde_json::json!({"v": 100000 + i}))
                    .unwrap();
            }

            // Finalize A's flush — runs the gated WAL maintenance
            // (pre-fix: checkpoint covering B + rotate + prune = B's WAL
            // destroyed).
            store
                .finalize_flush_with_publisher(&drained, |_| Ok(()))
                .unwrap();
            // The user-visible flush boundary forces maintenance again
            // (pre-fix with `current_seq_no() - 1`).
            store.force_wal_maintenance().unwrap();

            // kill -9.
            drop(store);
        }

        // Restart: every acked write must be recoverable.
        let store2 = open_test_store(dir.path());
        for i in 0..100 {
            let e = store2.version_map.get(&format!("a{i}"));
            assert!(
                e.map(|e| !e.deleted).unwrap_or(false),
                "flushed doc a{i} lost after kill+restart"
            );
        }
        let mut lost = Vec::new();
        for i in 0..50 {
            let alive = store2
                .version_map
                .get(&format!("b{i}"))
                .map(|e| !e.deleted)
                .unwrap_or(false);
            if !alive {
                lost.push(i);
            }
        }
        assert!(
            lost.is_empty(),
            "acked bulk-during-flush docs lost after kill -9: {}/50 (ids {:?})",
            lost.len(),
            lost
        );
    }

    /// RC4 W1 #8 — after everything is flushed and maintenance runs, the
    /// verified prune must still reclaim the WAL (no retention leak from
    /// the new conservatism).
    #[test]
    fn verified_prune_still_reclaims_fully_flushed_wal() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        for i in 0..100 {
            store
                .index(format!("d{i}"), serde_json::json!({"v": i}))
                .unwrap();
        }
        store.flush().unwrap().expect("segment");
        store.force_wal_maintenance().unwrap();

        // All docs are segment-resident → every generation must be gone
        // except the fresh empty active one.
        let wal_dir = store.wal_dir();
        let mut wal_bytes = 0u64;
        let mut wal_files = 0usize;
        for entry in walk_wal_files(&wal_dir) {
            wal_files += 1;
            wal_bytes += entry;
        }
        // Each surviving file may only be an empty active generation
        // (16-byte header).
        assert!(
            wal_bytes <= wal_files as u64 * 16,
            "fully-flushed WAL not reclaimed: {wal_files} files, {wal_bytes} bytes"
        );
    }

    /// RC4 W1 #8 follow-up — a generation frozen with unproven entries is
    /// retained via the verdict cache (no re-decode on later ticks) and
    /// pruned by the cached-pairs recheck once a later flush makes its
    /// entries durable.
    #[test]
    fn prune_cache_reclaims_after_late_flush() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        for i in 0..20 {
            store
                .index(format!("a{i}"), serde_json::json!({"v": i}))
                .unwrap();
        }
        let drained = store.take_memtable_for_flush().unwrap();
        // Acked while the flush is in flight — unproven at maintenance #1.
        for i in 0..10 {
            store
                .index(format!("late{i}"), serde_json::json!({"v": i}))
                .unwrap();
        }
        store
            .finalize_flush_with_publisher(&drained, |_| Ok(()))
            .unwrap();
        store.force_wal_maintenance().unwrap();

        // The late docs' generation must be retained (bytes on disk).
        let retained: u64 = walk_wal_files(&store.wal_dir()).iter().sum();
        assert!(
            retained > walk_wal_files(&store.wal_dir()).len() as u64 * 16,
            "late docs' WAL generation must be retained while unflushed"
        );

        // A later flush makes them durable; the cached-pairs recheck must
        // then reclaim everything.
        store.flush().unwrap().expect("late docs flush");
        store.force_wal_maintenance().unwrap();
        let files = walk_wal_files(&store.wal_dir());
        let bytes: u64 = files.iter().sum();
        assert!(
            bytes <= files.len() as u64 * 16,
            "WAL not reclaimed after late flush: {} files, {bytes} bytes",
            files.len()
        );

        // And nothing was lost.
        drop(store);
        let store2 = open_test_store(dir.path());
        for i in 0..10 {
            assert!(
                store2
                    .version_map
                    .get(&format!("late{i}"))
                    .map(|e| !e.deleted)
                    .unwrap_or(false),
                "late{i} lost"
            );
        }
    }

    /// #320 — the WAL-consumer retention floor must be honoured by the prune
    /// pass the ENGINE runs, which is `wal_maintain_all_verified`, not
    /// `WalWriter::prune_verified`.
    ///
    /// `prune_verified` has no production caller anywhere in the tree
    /// (`rg 'prune_verified\('` hits `wal.rs` and its own unit tests only), so
    /// a floor implemented and unit-tested there alone was decoration: on a
    /// live node every rotated generation was still deleted the moment a flush
    /// made it redundant, and a tap whose target was down for longer than one
    /// flush interval still lost entries.
    ///
    /// Fails before the fix with `rotated after maintenance: 0`.
    #[test]
    fn the_retention_floor_holds_generations_through_engine_wal_maintenance() {
        fn rotated_wal_files(store: &IndexStore) -> usize {
            // One file per shard is the ACTIVE generation and is never a prune
            // candidate; everything beyond that is retained-rotated.
            walk_wal_files(&store.wal_dir()).len() - store.wal_min_retained_generations().len()
        }

        // Control: the default floor of 0 reclaims everything it can prove
        // durable, exactly as before this change.
        let bare_dir = tempfile::tempdir().unwrap();
        let bare = open_test_store(bare_dir.path());
        for round in 0..4 {
            for i in 0..5 {
                bare.index(format!("b{round}_{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            bare.flush().unwrap();
            bare.force_wal_maintenance().unwrap();
        }
        assert_eq!(
            rotated_wal_files(&bare),
            0,
            "floor 0 must keep pruning to the active generation, files: {:?}",
            walk_wal_files(&bare.wal_dir())
        );

        // Floor 2: the newest two rotated generations survive every
        // maintenance pass, so a consumer two generations behind still finds
        // its entries.
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store.set_wal_min_retained_generations(2);
        for round in 0..4 {
            for i in 0..5 {
                store
                    .index(format!("d{round}_{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            store.flush().unwrap();
            store.force_wal_maintenance().unwrap();
        }
        let retained = rotated_wal_files(&store);
        assert_eq!(
            retained,
            2 * store.wal_min_retained_generations().len(),
            "the floor must hold 2 rotated generations per shard through the maintenance \
             path the engine actually runs; files on disk: {:?}",
            walk_wal_files(&store.wal_dir())
        );

        // And it is a floor, not a lease: it holds the same two however many
        // more passes run, so a stalled consumer cannot grow the WAL without
        // bound.
        for _ in 0..3 {
            store.force_wal_maintenance().unwrap();
        }
        assert_eq!(
            rotated_wal_files(&store),
            retained,
            "a floor must not grow with the number of maintenance passes"
        );

        // Lowering it back to 0 releases them on the next pass — the knob is
        // live in both directions.
        store.set_wal_min_retained_generations(0);
        store.force_wal_maintenance().unwrap();
        assert_eq!(
            rotated_wal_files(&store),
            0,
            "clearing the floor must release the held generations, files: {:?}",
            walk_wal_files(&store.wal_dir())
        );
    }

    /// RC4 W2 #14 regression — a plain, never-re-indexed DELETE must not
    /// pin its WAL shard forever.  Pre-fix: the delete's tombstone lived
    /// only in RAM + WAL, `sweep_pending_deletes` never unpinned it, and
    /// maintenance skipped the shard for the life of the process (live
    /// repro: single-shard WAL grew 866 KB → 3.06 MB across 6 flushed
    /// rounds, control index pruned to 16 B).  Post-fix the flush writes
    /// a seq-aware ZTB2 tombstone, the version map repoints it
    /// segment-resident, sweep unpins, prune reclaims — and the delete
    /// STAYS durable across BOTH the first and second restart (the
    /// second restart is the resurrection trap: no WAL entry left).
    #[test]
    fn plain_delete_unpins_wal_and_stays_deleted_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = open_test_store(dir.path());
            for i in 0..50 {
                store
                    .index(format!("d{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            store.flush().unwrap().expect("segment");

            // The plain delete — tombstone only, never re-indexed.
            store.delete("d7").unwrap().expect("d7 existed");

            // Flush drains the delete into a ZTB2 tombstone section;
            // maintenance must then be able to unpin + reclaim the WAL.
            for i in 50..60 {
                store
                    .index(format!("d{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            store.flush().unwrap().expect("segment 2");
            store.force_wal_maintenance().unwrap();

            let files = walk_wal_files(&store.wal_dir());
            let bytes: u64 = files.iter().sum();
            assert!(
                bytes <= files.len() as u64 * 16,
                "delete-bearing WAL must be reclaimed once the tombstone is \
                 segment-resident (pre-fix: pinned forever): {} files, {bytes} bytes",
                files.len()
            );
            drop(store);
        }

        // Restart 1: the delete must hold WITHOUT its WAL entry.
        {
            let store2 = open_test_store(dir.path());
            let e = store2.version_map.get("d7");
            assert!(
                e.map(|e| e.deleted).unwrap_or(true),
                "d7 resurrected after restart 1 (tombstone not rebuilt from segment)"
            );
            assert!(
                store2
                    .version_map
                    .get("d8")
                    .map(|e| !e.deleted)
                    .unwrap_or(false),
                "sibling doc d8 must stay live"
            );
            drop(store2);
        }
        // Restart 2 (the resurrection trap when durability leaned on a
        // replayed-then-repinned WAL entry).
        let store3 = open_test_store(dir.path());
        assert!(
            store3
                .version_map
                .get("d7")
                .map(|e| e.deleted)
                .unwrap_or(true),
            "d7 resurrected after restart 2"
        );
        for i in [0usize, 8, 49, 59] {
            assert!(
                store3
                    .version_map
                    .get(&format!("d{i}"))
                    .map(|e| !e.deleted)
                    .unwrap_or(false),
                "live doc d{i} lost"
            );
        }
    }

    /// RC4 W2 #14 — engine-path shape: the delete's tombstone never flows
    /// through a data flush (the engine drains its own memtable), so
    /// maintenance itself must persist it (tombstone-only segment) and
    /// then reclaim the WAL.  Survives restart with the WAL gone.
    #[test]
    fn maintenance_persists_pending_tombstones_without_data_flush() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = open_test_store(dir.path());
            for i in 0..20 {
                store
                    .index(format!("e{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            store.flush().unwrap().expect("segment");
            store.force_wal_maintenance().unwrap();

            // Delete with NO subsequent data flush — only maintenance runs
            // (the engine flush path never hands deletes to finalize).
            store.delete("e3").unwrap().expect("e3 existed");
            store.force_wal_maintenance().unwrap();

            let files = walk_wal_files(&store.wal_dir());
            let bytes: u64 = files.iter().sum();
            assert!(
                bytes <= files.len() as u64 * 16,
                "maintenance must persist the pending tombstone and reclaim \
                 the WAL: {} files, {bytes} bytes",
                files.len()
            );
            // A tombstone-only segment must be registered.
            let snap = store.snapshot.load();
            assert!(
                snap.segments
                    .iter()
                    .any(|m| m.doc_count == 0 && m.has_tombstones),
                "tombstone-only segment missing from snapshot"
            );
            drop(store);
        }

        let store2 = open_test_store(dir.path());
        assert!(
            store2
                .version_map
                .get("e3")
                .map(|e| e.deleted)
                .unwrap_or(true),
            "e3 resurrected after restart (tombstone-only segment not applied)"
        );
        assert!(
            store2
                .version_map
                .get("e4")
                .map(|e| !e.deleted)
                .unwrap_or(false),
            "sibling doc e4 must stay live"
        );
    }

    /// RC4 W2 #14 — crash-window coverage: a tombstone-only segment left
    /// ORPHANED (crash between the WAL prune and `save_snapshot`) must be
    /// resurrected on reopen, not GC'd — with the WAL entry pruned it is
    /// the only remaining record of the delete.
    #[test]
    fn orphan_tombstone_only_segment_is_resurrected() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_backup;
        {
            let store = open_test_store(dir.path());
            for i in 0..10 {
                store
                    .index(format!("o{i}"), serde_json::json!({"v": i}))
                    .unwrap();
            }
            store.flush().unwrap().expect("segment");
            store.force_wal_maintenance().unwrap();
            // Snapshot state BEFORE the delete becomes segment-durable.
            snapshot_backup = std::fs::read(dir.path().join("snapshot.json")).unwrap();

            store.delete("o5").unwrap().expect("o5 existed");
            store.force_wal_maintenance().unwrap(); // persists tombstone + prunes WAL
            drop(store);
        }
        // Simulate the crash window: the persisted snapshot never saw the
        // tombstone-only segment (WAL already pruned).
        std::fs::write(dir.path().join("snapshot.json"), &snapshot_backup).unwrap();

        let store2 = open_test_store(dir.path());
        assert!(
            store2
                .version_map
                .get("o5")
                .map(|e| e.deleted)
                .unwrap_or(true),
            "o5 resurrected: orphan tombstone-only segment was not recovered"
        );
        assert!(
            store2
                .version_map
                .get("o4")
                .map(|e| !e.deleted)
                .unwrap_or(false),
            "sibling doc o4 must stay live"
        );
    }

    fn walk_wal_files(root: &Path) -> Vec<u64> {
        let mut sizes = Vec::new();
        let mut dirs = vec![root.to_path_buf()];
        while let Some(d) = dirs.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().map(|x| x == "wal").unwrap_or(false) {
                    sizes.push(e.metadata().map(|m| m.len()).unwrap_or(0));
                }
            }
        }
        sizes
    }

    #[test]
    fn index_and_flush() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        store
            .index("doc-1", serde_json::json!({"title": "hello"}))
            .unwrap();
        store
            .index("doc-2", serde_json::json!({"title": "world"}))
            .unwrap();

        let meta = store.flush().unwrap().expect("flush produced a segment");
        assert_eq!(meta.doc_count, 2);

        let snap = store.snapshot();
        assert_eq!(snap.segments.len(), 1);
        assert_eq!(snap.generation, 1);
    }

    #[test]
    fn empty_flush_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        assert!(store.flush().unwrap().is_none());
    }

    #[test]
    fn delete_tombstones_segment() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        store.index("doc-1", serde_json::json!({})).unwrap();
        store.delete("doc-1").unwrap();

        let meta = store.flush().unwrap().unwrap();
        assert!(meta.has_tombstones);
    }

    #[test]
    fn version_map_updated_after_flush() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        store.index("doc-1", serde_json::json!({})).unwrap();
        let meta = store.flush().unwrap().unwrap();

        let entry = store.version_map.get("doc-1").unwrap();
        assert_eq!(&*entry.segment_id, meta.id.as_str());
    }

    #[test]
    fn multiple_flushes_accumulate_segments() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        for i in 0..3 {
            store
                .index(format!("doc-{i}"), serde_json::json!({"i": i}))
                .unwrap();
            store.flush().unwrap();
        }

        let snap = store.snapshot();
        assert_eq!(snap.segments.len(), 3);
        assert_eq!(snap.generation, 3);
    }

    #[test]
    fn snapshot_persisted_and_loaded() {
        let dir = tempfile::tempdir().unwrap();

        {
            let store = open_test_store(dir.path());
            store.index("doc-1", serde_json::json!({"x": 1})).unwrap();
            store.flush().unwrap();
        }

        // Re-open
        let store2 = open_test_store(dir.path());
        let snap = store2.snapshot();
        // Segments from the persisted snapshot should be loaded back
        assert_eq!(snap.segments.len(), 1);
    }

    #[test]
    fn open_segment_reader() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        store
            .index("doc-1", serde_json::json!({"hello": "world"}))
            .unwrap();
        let meta = store.flush().unwrap().unwrap();

        let reader = store.open_segment(&meta.id).unwrap();
        assert_eq!(reader.header().doc_count, 1);
    }

    // ── Object-store backed flush tests ───────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn object_store_flush_uploads_segment() {
        use crate::backend::S3Backend;
        use std::sync::Arc;

        let data_dir = tempfile::tempdir().unwrap();
        let s3_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(S3Backend::new(s3_dir.path(), "test-bucket", "xerj/"));

        let store = IndexStore::open(
            data_dir.path(),
            IndexStoreConfig {
                sync_mode: SyncMode::Batched,
                storage_mode: StorageMode::ObjectStore {
                    backend: Arc::clone(&backend),
                    cache_dir: cache_dir.path().to_path_buf(),
                },
                ..Default::default()
            },
        )
        .unwrap();

        store
            .index("doc-1", serde_json::json!({"title": "hello s3"}))
            .unwrap();
        let meta = store.flush().unwrap().expect("should produce a segment");

        // Segment must exist in the simulated S3 bucket.
        let object_key = format!("segments/{}", meta.seg_path);
        assert!(
            backend.exists(&object_key).await.unwrap(),
            "segment not found in object store: {object_key}"
        );

        // Segment should also be in local cache.
        let cached = cache_dir.path().join(&meta.seg_path);
        assert!(cached.exists(), "segment not cached locally: {:?}", cached);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn object_store_read_through_cache() {
        use crate::backend::S3Backend;
        use std::sync::Arc;

        let data_dir = tempfile::tempdir().unwrap();
        let s3_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(S3Backend::new(s3_dir.path(), "test-bucket", "xerj/"));

        let store = IndexStore::open(
            data_dir.path(),
            IndexStoreConfig {
                sync_mode: SyncMode::Batched,
                storage_mode: StorageMode::ObjectStore {
                    backend: Arc::clone(&backend),
                    cache_dir: cache_dir.path().to_path_buf(),
                },
                ..Default::default()
            },
        )
        .unwrap();

        store
            .index("doc-1", serde_json::json!({"title": "cache test"}))
            .unwrap();
        let meta = store.flush().unwrap().unwrap();

        // Remove local segment file to force a cache miss on first open.
        let local_seg = data_dir.path().join("segments").join(&meta.seg_path);
        std::fs::remove_file(&local_seg).ok();
        // Also clear the warm cache so the read-through path is exercised.
        let cached = cache_dir.path().join(&meta.seg_path);
        std::fs::remove_file(&cached).ok();

        // open_segment should fetch from the object store and cache locally.
        let reader = store.open_segment(&meta.id).unwrap();
        assert_eq!(reader.header().doc_count, 1);

        // Subsequent open should be served from cache.
        let reader2 = store.open_segment(&meta.id).unwrap();
        assert_eq!(reader2.header().doc_count, 1);
    }

    // ── Merge-race read-lease tests (2026-07) ────────────────────────────────

    /// Build two flushed segments and merge them, returning
    /// (store, input_ids, merged_meta).  The merge is applied
    /// (snapshot swapped) but the input files are NOT yet retired.
    fn two_segments_merged(dir: &Path) -> (Arc<IndexStore>, Vec<SegmentId>, SegmentMeta) {
        let store = open_test_store(dir);
        store.index("doc-1", serde_json::json!({"v": 1})).unwrap();
        store.flush().unwrap();
        store.index("doc-2", serde_json::json!({"v": 2})).unwrap();
        store.flush().unwrap();
        let ids: Vec<SegmentId> = store
            .snapshot()
            .segments
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(ids.len(), 2);
        let executor = crate::merge::MergeExecutor::new(
            Arc::clone(&store),
            crate::merge::MergeConfig {
                io_rate_mb_per_sec: 0,
                ..Default::default()
            },
        );
        let merged = executor.execute_merge(&ids).unwrap();
        (store, ids, merged)
    }

    #[test]
    fn retire_without_lease_deletes_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let (store, ids, merged) = two_segments_merged(dir.path());
        let segments_dir = dir.path().join("segments");

        for id in &ids {
            assert!(segments_dir.join(format!("{id}.seg")).exists());
        }
        let (files, _bytes) = store.retire_segment_files(&ids).unwrap();
        assert!(
            files >= 2,
            "expected immediate deletion, removed {files} files"
        );
        for id in &ids {
            assert!(
                !segments_dir.join(format!("{id}.seg")).exists(),
                "input segment file should be gone with no lease outstanding"
            );
        }
        assert!(segments_dir.join(format!("{}.seg", merged.id)).exists());
    }

    #[test]
    fn retire_defers_deletion_while_lease_held_and_scan_stays_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store.index("doc-1", serde_json::json!({"v": 1})).unwrap();
        store.flush().unwrap();
        store.index("doc-2", serde_json::json!({"v": 2})).unwrap();
        store.flush().unwrap();

        // A "query" snapshots the segment list BEFORE the merge commits.
        let query_snap = store.snapshot();
        let ids: Vec<SegmentId> = query_snap.segments.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids.len(), 2);

        // Merge commits and retires the inputs while the query is in flight
        // (mirrors run_merge_once: evict reader cache, then retire).
        let executor = crate::merge::MergeExecutor::new(
            Arc::clone(&store),
            crate::merge::MergeConfig {
                io_rate_mb_per_sec: 0,
                ..Default::default()
            },
        );
        executor.execute_merge(&ids).unwrap();
        for id in &ids {
            store.evict_segment_reader_cache(id.as_str());
        }
        let (files, _bytes) = store.retire_segment_files(&ids).unwrap();
        assert_eq!(files, 0, "deletion must be deferred while a lease is held");

        let segments_dir = dir.path().join("segments");
        for id in &ids {
            assert!(
                segments_dir.join(format!("{id}.seg")).exists(),
                "retired segment file must survive until the last lease drops"
            );
            assert!(
                !segments_dir.join(format!("{id}.ids")).exists(),
                ".ids resurrection marker must be unlinked at retire time"
            );
            // The in-flight query can still open every segment of ITS
            // snapshot (fallback open path — the ids are no longer in the
            // current snapshot and the reader cache was evicted).
            let reader = store.open_segment_arc(id.as_str()).unwrap();
            assert_eq!(reader.header().doc_count, 1);
        }

        // Query finishes → last lease drops → graveyard swept.
        drop(query_snap);
        for id in &ids {
            assert!(
                !segments_dir.join(format!("{id}.seg")).exists(),
                "retired segment file must be deleted once the last lease drops"
            );
        }
    }

    #[test]
    fn crash_with_deferred_retire_does_not_resurrect_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let (store, ids, merged) = two_segments_merged(dir.path());
        let segments_dir = dir.path().join("segments");

        // Hold a lease so retire defers, then "crash" (leak the lease and
        // drop the store). apply_merge already persisted the post-merge
        // snapshot and durably disarmed every recovery marker before commit.
        let leaked = store.snapshot();
        let (files, _bytes) = store.retire_segment_files(&ids).unwrap();
        assert_eq!(files, 0);
        for id in &ids {
            assert!(!segments_dir.join(format!("{id}.complete")).exists());
            assert!(!segments_dir.join(format!("{id}.ids")).exists());
        }

        // The leased readers keep non-marker segment data alive. Model a crash
        // at exactly this deferred-retirement point: data remains, while both
        // recovery marker families are durably absent.
        for id in &ids {
            assert!(segments_dir.join(format!("{id}.seg")).exists());
            assert!(!segments_dir.join(format!("{id}.complete")).exists());
            assert!(!segments_dir.join(format!("{id}.ids")).exists());
        }
        std::mem::forget(leaked);
        drop(store);

        // Reopen: recover_orphaned_segments must NOT resurrect the
        // merged-away inputs, and on-open cleanup must reclaim their leftover
        // non-marker files.
        let store2 = open_test_store(dir.path());
        let snap = store2.snapshot();
        assert_eq!(snap.segments.len(), 1, "only the merged segment survives");
        assert_eq!(snap.segments[0].id, merged.id);
        for id in &ids {
            assert!(
                !segments_dir.join(format!("{id}.seg")).exists(),
                "crash leftovers must be cleaned on open"
            );
        }
    }

    // ── RC4 W3 #10 — data-dir format marker + refuse-on-corrupt-snapshot ──────

    /// Ingest `n` docs and flush, so the dir has a real `snapshot.json`
    /// manifest plus segment files. Returns the doc-ids written.
    fn flush_n_docs(dir: &Path, n: usize) -> Vec<String> {
        let ids: Vec<String> = (0..n).map(|i| format!("doc-{i}")).collect();
        {
            let store = open_test_store(dir);
            for id in &ids {
                store
                    .index(id.clone(), serde_json::json!({"v": id}))
                    .unwrap();
            }
            store.flush().unwrap().expect("a segment was written");
            drop(store);
        }
        ids
    }

    fn assert_all_served(store: &Arc<IndexStore>, ids: &[String]) {
        for id in ids {
            let entry = store.version_map.get(id);
            assert!(
                entry.as_ref().map(|e| !e.deleted).unwrap_or(false),
                "doc {id} must be live and served after reopen"
            );
        }
    }

    /// A successful open stamps the data-dir format marker.
    #[test]
    fn open_stamps_data_dir_format_marker() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(DATA_DIR_META_FILE);
        assert!(!marker.exists(), "fresh dir has no marker yet");

        let store = open_test_store(dir.path());
        drop(store);

        assert!(marker.exists(), "open() must stamp {DATA_DIR_META_FILE}");
        let meta: DataDirMeta = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        assert_eq!(meta.format_version, DATA_DIR_BASE_FORMAT_VERSION);
    }

    #[test]
    fn ordinary_reopen_does_not_advance_the_baseline_format_marker() {
        let dir = tempfile::tempdir().unwrap();
        drop(open_test_store(dir.path()));
        drop(open_test_store(dir.path()));

        let marker = dir.path().join(DATA_DIR_META_FILE);
        let meta: DataDirMeta = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
        assert_eq!(meta.format_version, DATA_DIR_BASE_FORMAT_VERSION);
    }

    #[test]
    fn encoded_fts_preflight_advances_marker_and_v1_refuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let marker = dir.path().join(DATA_DIR_META_FILE);
        let mut before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        before["future_provenance"] = serde_json::json!({"keep": true});
        std::fs::write(&marker, serde_json::to_vec(&before).unwrap()).unwrap();

        store.ensure_fts_encoded_field_component_format().unwrap();
        // The transition is idempotent and may be called by concurrent flush
        // or merge preflights without changing its result.
        store.ensure_fts_encoded_field_component_format().unwrap();

        let marker_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        let meta: DataDirMeta = serde_json::from_value(marker_value.clone()).unwrap();
        assert_eq!(
            meta.format_version,
            DATA_DIR_FTS_ENCODED_FIELD_COMPONENT_VERSION
        );
        assert_eq!(marker_value["future_provenance"]["keep"], true);
        assert!(IndexStore::check_data_dir_version_with_max(
            dir.path(),
            DATA_DIR_BASE_FORMAT_VERSION
        )
        .is_err());
        assert!(IndexStore::check_data_dir_version(dir.path()).is_ok());

        drop(store);
        drop(open_test_store(dir.path()));
    }

    #[test]
    fn failed_encoded_fts_marker_persistence_is_conservative_at_every_boundary() {
        for (failpoint, expected_version) in [
            (DataDirFormatWriteFailpoint::BeforeTempWrite, 1),
            (DataDirFormatWriteFailpoint::BeforeRename, 1),
            (DataDirFormatWriteFailpoint::BeforeParentFsync, 2),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = open_test_store(dir.path());
            store.set_data_dir_format_write_failpoint_for_test(failpoint);

            assert!(store.ensure_fts_encoded_field_component_format().is_err());
            assert_eq!(store.fts_v2_marker_durability_confirmations_for_test(), 0);
            let marker = dir.path().join(DATA_DIR_META_FILE);
            let meta: DataDirMeta =
                serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
            assert_eq!(meta.format_version, expected_version, "{failpoint:?}");
            assert!(IndexStore::check_data_dir_version(dir.path()).is_ok());
            if expected_version == 2 {
                assert!(IndexStore::check_data_dir_version_with_max(dir.path(), 1).is_err());
            }

            // In the post-rename case the v2 bytes are already visible, but
            // retry must not treat visibility as durability. It crosses one
            // platform-appropriate durable replacement confirmation before
            // the store records success.
            store.ensure_fts_encoded_field_component_format().unwrap();
            assert_eq!(store.fts_v2_marker_durability_confirmations_for_test(), 1);
            store.ensure_fts_encoded_field_component_format().unwrap();
            assert_eq!(store.fts_v2_marker_durability_confirmations_for_test(), 1);
        }
    }

    #[test]
    fn concurrent_encoded_fts_preflights_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.ensure_fts_encoded_field_component_format()
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        let marker = dir.path().join(DATA_DIR_META_FILE);
        let meta: DataDirMeta = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
        assert_eq!(
            meta.format_version,
            DATA_DIR_FTS_ENCODED_FIELD_COMPONENT_VERSION
        );
        assert_eq!(store.fts_v2_marker_durability_confirmations_for_test(), 1);
    }

    #[test]
    fn concurrent_retries_confirm_visible_unconfirmed_v2_marker_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store.set_data_dir_format_write_failpoint_for_test(
            DataDirFormatWriteFailpoint::BeforeParentFsync,
        );
        assert!(store.ensure_fts_encoded_field_component_format().is_err());
        assert_eq!(store.fts_v2_marker_durability_confirmations_for_test(), 0);

        let marker = dir.path().join(DATA_DIR_META_FILE);
        let meta: DataDirMeta = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        assert_eq!(
            meta.format_version, DATA_DIR_FTS_ENCODED_FIELD_COMPONENT_VERSION,
            "the injected post-rename failure must leave v2 visible but unconfirmed"
        );

        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.ensure_fts_encoded_field_component_format()
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        assert_eq!(
            store.fts_v2_marker_durability_confirmations_for_test(),
            1,
            "the format mutex must serialize visible-marker durability confirmation"
        );
    }

    /// THE UPGRADE PATH: an rc3-vintage data dir has NO format marker (rc3
    /// never wrote one). Reopening under this (rc4) binary must serve every
    /// doc AND stamp a marker so subsequent opens are versioned. This is the
    /// "open an rc3-vintage fixture and serve it" test.
    #[test]
    fn rc3_vintage_dir_without_marker_opens_and_serves() {
        let dir = tempfile::tempdir().unwrap();
        let ids = flush_n_docs(dir.path(), 12);

        // Simulate rc3 vintage: delete the marker this binary stamped.
        let marker = dir.path().join(DATA_DIR_META_FILE);
        std::fs::remove_file(&marker).unwrap();
        assert!(!marker.exists());

        // Reopen — must succeed and serve every doc.
        let store2 = open_test_store(dir.path());
        assert!(
            !store2.snapshot().segments.is_empty(),
            "rc3 segments must be loaded, not orphaned"
        );
        assert_all_served(&store2, &ids);

        // The upgrade re-stamps the marker.
        assert!(marker.exists(), "reopen must re-stamp the format marker");
    }

    /// serde(default) hygiene: a manifest written by a different xerj version
    /// that OMITS optional segment/snapshot fields still loads (missing
    /// fields take their defaults) rather than failing to deserialize.
    #[test]
    fn snapshot_missing_optional_fields_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let ids = flush_n_docs(dir.path(), 8);

        // Rewrite snapshot.json stripping fields we marked #[serde(default)]:
        // per-segment `sidx_path` / `has_tombstones` / `created_at_ms`, and
        // the snapshot-level `generation`.
        let snap_path = dir.path().join("snapshot.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&snap_path).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("generation");
        for seg in v["segments"].as_array_mut().unwrap() {
            let o = seg.as_object_mut().unwrap();
            o.remove("sidx_path");
            o.remove("has_tombstones");
            o.remove("created_at_ms");
        }
        std::fs::write(&snap_path, serde_json::to_vec(&v).unwrap()).unwrap();

        let store2 = open_test_store(dir.path());
        assert!(!store2.snapshot().segments.is_empty());
        assert_all_served(&store2, &ids);
    }

    /// THE DATA-LOSS GUARD: a PRESENT-but-corrupt `snapshot.json` must make
    /// open REFUSE loudly (Err), NOT silently return an empty store. And the
    /// refusal must happen before the orphan GC, so every segment file is
    /// still on disk afterwards. Then restoring a valid manifest reopens
    /// cleanly and serves the data — proving the refusal preserved it.
    #[test]
    fn corrupt_snapshot_is_refused_and_segments_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let ids = flush_n_docs(dir.path(), 10);

        let snap_path = dir.path().join("snapshot.json");
        let good_bytes = std::fs::read(&snap_path).unwrap();

        // Count the segment files that exist before the bad open.
        let segments_dir = dir.path().join("segments");
        let seg_files_before: Vec<_> = std::fs::read_dir(&segments_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".seg"))
            .map(|e| e.path())
            .collect();
        assert!(!seg_files_before.is_empty());

        // Corrupt the manifest (a torn write — invalid JSON).
        std::fs::write(&snap_path, b"{\"segments\": [ {\"id\": \"deadbeef\", tr").unwrap();

        // Open must REFUSE, not return an empty store.
        let err = match IndexStore::open(
            dir.path(),
            IndexStoreConfig {
                sync_mode: SyncMode::Batched,
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("corrupt snapshot must refuse to open, not silently empty"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StorageError::IncompatibleDataDir(_)),
            "expected IncompatibleDataDir, got {err:?}"
        );

        // Every segment file must survive the refused open (no orphan GC ran).
        for p in &seg_files_before {
            assert!(
                p.exists(),
                "segment file {p:?} must NOT be deleted when the manifest is corrupt"
            );
        }

        // Restore the good manifest → opens cleanly and serves everything.
        std::fs::write(&snap_path, &good_bytes).unwrap();
        let store2 = open_test_store(dir.path());
        assert_all_served(&store2, &ids);
    }

    /// The version gate refuses a data dir written by a NEWER xerj (marker
    /// format_version greater than this binary supports), and accepts it
    /// again once the marker is compatible.
    #[test]
    fn newer_format_marker_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ids = flush_n_docs(dir.path(), 5);

        let marker = dir.path().join(DATA_DIR_META_FILE);
        let future = serde_json::json!({
            "format_version": DATA_DIR_FORMAT_VERSION + 1,
            "xerj_version": "9.9.9-from-the-future",
        });
        std::fs::write(&marker, serde_json::to_vec(&future).unwrap()).unwrap();

        let err = match IndexStore::open(
            dir.path(),
            IndexStoreConfig {
                sync_mode: SyncMode::Batched,
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("a newer-format data dir must be refused"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StorageError::IncompatibleDataDir(_)),
            "expected IncompatibleDataDir, got {err:?}"
        );

        // Segments preserved (refusal is before any GC).
        let segments_dir = dir.path().join("segments");
        assert!(std::fs::read_dir(&segments_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".seg")));

        // Compatible marker → opens and serves.
        let ok = serde_json::json!({ "format_version": DATA_DIR_FORMAT_VERSION });
        std::fs::write(&marker, serde_json::to_vec(&ok).unwrap()).unwrap();
        let store2 = open_test_store(dir.path());
        assert_all_served(&store2, &ids);
    }

    /// A genuinely ABSENT manifest (a fresh index that never flushed) must
    /// still open as empty — the refuse-on-corrupt change must not break the
    /// happy path of a brand-new data dir.
    #[test]
    fn absent_snapshot_opens_empty_without_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("snapshot.json").exists());
        let store = open_test_store(dir.path());
        assert_eq!(store.snapshot().segments.len(), 0);
        // And ingest still works on the fresh store.
        store.index("x", serde_json::json!({"a": 1})).unwrap();
        assert!(store.version_map.get("x").is_some());
    }

    #[test]
    fn apply_merge_manifest_failure_restores_exact_input_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store.index("a", serde_json::json!({"value": "a"})).unwrap();
        store.flush().unwrap().unwrap();
        store.index("b", serde_json::json!({"value": "b"})).unwrap();
        store.flush().unwrap().unwrap();
        let before = store.snapshot();
        assert_eq!(before.segments.len(), 2);
        let before_ids: std::collections::HashSet<_> =
            before.segments.iter().map(|meta| meta.id.clone()).collect();

        let mut output = before.segments[0].clone();
        output.id = "injected-unpublished-output".into();
        output.seg_path = "segments/injected-unpublished-output.seg".into();
        output.sidx_path = "segments/injected-unpublished-output.sidx".into();
        let input_ids: Vec<_> = before.segments.iter().map(|meta| meta.id.clone()).collect();
        // apply_merge first persists the authoritative inputs before it
        // disarms recovery markers; fail the following publication save.
        store.fail_snapshot_save_after.store(1, Ordering::Release);
        assert!(store.apply_merge(&input_ids, output).is_err());

        let after_ids: std::collections::HashSet<_> = store
            .snapshot()
            .segments
            .iter()
            .map(|meta| meta.id.clone())
            .collect();
        assert_eq!(after_ids, before_ids);
        drop(before);
        drop(store);

        let reopened = open_test_store(dir.path());
        let reopened_ids: std::collections::HashSet<_> = reopened
            .snapshot()
            .segments
            .iter()
            .map(|meta| meta.id.clone())
            .collect();
        assert_eq!(reopened_ids, before_ids);
        assert_eq!(reopened.version_map.live_count(), 2);
    }

    #[test]
    fn apply_merge_reports_indeterminate_when_publication_and_rollback_saves_fail() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());
        store.index("a", serde_json::json!({"value": "a"})).unwrap();
        store.flush().unwrap().unwrap();
        store.index("b", serde_json::json!({"value": "b"})).unwrap();
        store.flush().unwrap().unwrap();
        let before = store.snapshot();
        let mut output = before.segments[0].clone();
        output.id = "indeterminate-output".into();
        let input_ids: Vec<_> = before.segments.iter().map(|meta| meta.id.clone()).collect();
        let transaction =
            VersionRepointTransaction::with_capacity(Arc::clone(&store.version_map), 0);
        // The pre-disarm input save succeeds; publication and rollback saves
        // then both fail. The API must not claim exact rollback durability.
        store.fail_snapshot_save_after.store(1, Ordering::Release);
        store
            .snapshot_save_failures_remaining
            .store(2, Ordering::Release);
        let error = store
            .apply_merge_with_repoints(&input_ids, output, transaction)
            .unwrap_err();
        assert!(matches!(error, MergePublicationError::Indeterminate { .. }));
    }
}
