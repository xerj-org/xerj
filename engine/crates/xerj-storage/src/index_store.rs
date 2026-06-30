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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use arc_swap::ArcSwap;
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument, warn};

use crate::backend::StorageBackend;
use crate::segment::{SegmentId, SegmentMeta, SegmentReader, SegmentWriter, SectionType};
use crate::version_map::{VersionMap, IN_MEMORY_SEGMENT_ID};
use crate::wal::{wal_build_frames_lockfree, ReplayEntry, SyncMode, WalEntry, WalReader, WalWriter};
use crate::{Result, SeqNo, StorageError};

// ── IndexSnapshot ─────────────────────────────────────────────────────────────

/// Immutable snapshot of the active segments at a point in time.
///
/// Stored inside `ArcSwap<IndexSnapshot>`.  Readers load a copy of the `Arc`
/// (cheap, no lock) and can iterate the segment list without holding any mutex.
/// Writers create a new `IndexSnapshot` with the updated list and swap it in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSnapshot {
    /// Ordered list of active segments (oldest first).
    pub segments: Vec<SegmentMeta>,
    /// Snapshot generation — incremented on every flush/merge.
    pub generation: u64,
    /// The highest seq_no covered by segments in this snapshot.
    pub max_seq_no: SeqNo,
}

impl IndexSnapshot {
    fn empty() -> Self {
        Self { segments: Vec::new(), generation: 0, max_seq_no: 0 }
    }

    fn with_new_segment(&self, meta: SegmentMeta) -> Self {
        let max_seq_no = self.max_seq_no.max(meta.max_seq_no);
        let mut segments = self.segments.clone();
        segments.push(meta);
        Self { segments, generation: self.generation + 1, max_seq_no }
    }

    fn replace_segments(&self, remove_ids: &[SegmentId], add: SegmentMeta) -> Self {
        let remove_set: std::collections::HashSet<&str> =
            remove_ids.iter().map(String::as_str).collect();
        let mut segments: Vec<SegmentMeta> =
            self.segments.iter().filter(|s| !remove_set.contains(s.id.as_str())).cloned().collect();
        segments.push(add);
        let max_seq_no = segments.iter().map(|s| s.max_seq_no).max().unwrap_or(0);
        Self { segments, generation: self.generation + 1, max_seq_no }
    }
}

// ── Memtable entry ────────────────────────────────────────────────────────────

#[derive(Debug)]
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

// ── Fsck report types ─────────────────────────────────────────────────────────
//
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
                write!(f, "StorageMode::ObjectStore {{ cache_dir: {:?} }}", cache_dir)
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
    pub sync_mode: SyncMode,
    /// Schema version for new segments.
    pub schema_version: u32,
    /// Storage destination for flushed segments.
    pub storage_mode: StorageMode,
    /// Number of independent WAL shards (default: 1 for backward compat).
    /// When > 1, each shard gets its own WAL file (`wal_s{N}/`) for
    /// parallel writes without cross-shard mutex contention.
    pub num_wal_shards: usize,
}

impl Default for IndexStoreConfig {
    fn default() -> Self {
        Self {
            memtable_max_bytes: 32 * 1024 * 1024, // 32 MiB
            wal_max_size_bytes: 128 * 1024 * 1024, // 128 MiB
            sync_mode: SyncMode::Batched,
            schema_version: 1,
            storage_mode: StorageMode::Local,
            num_wal_shards: 1,
        }
    }
}

// ── IndexStore ────────────────────────────────────────────────────────────────

/// Per-index storage engine.
///
/// All public methods are safe to call from multiple threads.  WAL writes are
/// serialized through the internal `Mutex<WalWriter>`; snapshot reads are
/// completely lock-free via `ArcSwap`.
/// Number of memtable shards.  Each shard has its own `Mutex<Vec<MemEntry>>`
/// so that concurrent bulk ingest paths that land on different shards run
/// without contending on a single global lock.  Must be a power of two so
/// the shard index can be derived via `hash & (N-1)`.
// Memtable shard count is no longer a compile-time constant. The actual
// value lives in `IndexStore.num_shards`, derived at construction from
// `IndexStoreConfig.num_wal_shards.max(1).next_power_of_two()` (in turn
// usually plumbed from `Config.engine.ingest_shards`). The previous
// `pub const MEMTABLE_SHARDS: usize = 16;` was a footgun: the static
// `shard_for_doc_id` helper used it as the modulus while the instance
// `shard_for(&self)` used `self.num_shards`, producing inconsistent
// routing on any deployment that didn't happen to land on 16.

pub struct IndexStore {
    /// Root directory for this index's data files.
    pub data_dir: PathBuf,
    config: IndexStoreConfig,
    /// Actual memtable shard count, derived at open time from
    /// `IndexStoreConfig.num_wal_shards.max(1).next_power_of_two()`.
    num_shards: usize,
    /// Current active segment snapshot.
    snapshot: ArcSwap<IndexSnapshot>,
    /// Sharded WAL writers — each shard has its own WAL file and mutex.
    /// Batches route to a shard by `xxh3(doc_id) & (num_shards - 1)`.
    /// When num_wal_shards=1, this is equivalent to the old single-WAL path.
    wal_shards: Vec<Mutex<WalWriter>>,
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
}

const WAL_MAINTENANCE_INTERVAL_MS: u64 = 1_000;

impl IndexStore {
    /// Open (or create) an index at `data_dir`.
    ///
    /// If WAL files exist, they are replayed to rebuild the in-memory state.
    pub fn open(data_dir: impl AsRef<Path>, config: IndexStoreConfig) -> Result<Arc<Self>> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;

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
            let w = WalWriter::open(&shard_dir, config.wal_max_size_bytes, config.sync_mode, Arc::clone(&seq_counter))?;
            wal_shards.push(Mutex::new(w));
        }

        // Load the persisted snapshot (segment registry)
        let snapshot = Self::load_snapshot(&data_dir).unwrap_or_else(|_| IndexSnapshot::empty());

        let version_map = Arc::new(VersionMap::new());

        let num_shards = config.num_wal_shards.max(1).next_power_of_two();
        let memtable_shards: Vec<Mutex<Vec<MemEntry>>> = (0..num_shards)
            .map(|_| Mutex::new(Vec::new()))
            .collect();
        let store = Arc::new(Self {
            data_dir: data_dir.clone(),
            config,
            num_shards,
            snapshot: ArcSwap::from_pointee(snapshot),
            wal_shards,
            version_map: Arc::clone(&version_map),
            seq_counter,
            memtable_shards,
            memtable_bytes: AtomicU64::new(0),
            seg_reader_cache: dashmap::DashMap::new(),
            last_wal_maintenance_ms: AtomicU64::new(0),
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

        // Replay WAL to rebuild in-memory state (these override segment entries).
        store.replay_wal(&wal_dir)?;

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
    /// Recovery strategy: read the `.ids` sidecar (`ZID1`/`ZID2`, written
    /// at flush time as the very last side-car), which carries the
    /// canonical (doc_count, min_seq, max_seq) the snapshot needs.  If
    /// the sidecar is present and decodes cleanly, the segment was
    /// fully flushed — the only thing missing is the snapshot pointer,
    /// which we add here.  If the `.ids` sidecar is missing or corrupt,
    /// the flush was genuinely incomplete and the file falls through to
    /// `cleanup_orphaned_segment_files` for deletion.
    ///
    /// Returns the number of segments recovered.
    fn recover_orphaned_segments(&self) -> Result<usize> {
        let segments_dir = self.data_dir.join("segments");
        if !segments_dir.exists() {
            return Ok(0);
        }

        let snap = self.snapshot.load();
        let live_ids: std::collections::HashSet<String> = snap
            .segments
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        drop(snap);

        let mut recovered: Vec<SegmentMeta> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = match entry { Ok(e) => e, Err(_) => continue };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Only process the primary `.seg` file once per UUID.
            if !name_str.ends_with(".seg") { continue; }
            if name_str.len() < 40 { continue; } // 36 UUID + ".seg"
            let prefix = &name_str[..36];
            if prefix.as_bytes().get(8) != Some(&b'-') { continue; }
            if live_ids.contains(prefix) { continue; }
            if !seen_ids.insert(prefix.to_string()) { continue; }

            let seg_filename = format!("{prefix}.seg");
            let seg_path = segments_dir.join(&seg_filename);
            let ids_path = segments_dir.join(format!("{prefix}.ids"));

            // .ids sidecar must exist — it's the last write of the flush
            // sequence, so its presence implies the segment is complete.
            let ids_bytes = match std::fs::read(&ids_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if ids_bytes.len() < 8 {
                continue;
            }
            let magic = &ids_bytes[..4];
            if magic != b"ZID1" && magic != b"ZID2" {
                continue;
            }
            let num_docs = u32::from_le_bytes([
                ids_bytes[4], ids_bytes[5], ids_bytes[6], ids_bytes[7],
            ]) as u64;
            if num_docs == 0 {
                continue;
            }
            let body: Vec<u8> = if magic == b"ZID2" {
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
                pos += id_len;
                min_seq = min_seq.min(seq);
                max_seq = max_seq.max(seq);
                parsed += 1;
            }
            if parsed == 0 || min_seq == u64::MAX {
                continue;
            }

            // Sanity-check the segment file itself opens.
            if SegmentReader::open(&seg_path).is_err() {
                continue;
            }

            let seg_meta = match std::fs::metadata(&seg_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let created_at_ms = seg_meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            recovered.push(SegmentMeta {
                id: prefix.to_string(),
                doc_count: parsed,
                size_bytes: seg_meta.len(),
                min_seq_no: min_seq,
                max_seq_no: max_seq,
                created_at_ms,
                has_tombstones: false,
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
        if !segments_dir.exists() { return Ok(()); }

        // Build the set of live segment UUIDs from the current snapshot.
        let snap = self.snapshot.load();
        let live_ids: std::collections::HashSet<String> = snap
            .segments
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        drop(snap);

        let mut removed_files = 0usize;
        let mut removed_bytes = 0u64;
        for entry in std::fs::read_dir(&segments_dir)? {
            let entry = match entry { Ok(e) => e, Err(_) => continue };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Segment filenames look like "<36-char UUID>.<suffix>".
            // Skip anything that doesn't start with a UUID-shaped prefix.
            if name_str.len() < 37 { continue; }
            let prefix = &name_str[..36];
            if prefix.as_bytes().get(8) != Some(&b'-') { continue; }
            if live_ids.contains(prefix) { continue; }
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
    pub fn index(
        &self,
        doc_id: impl Into<String>,
        source: serde_json::Value,
    ) -> Result<SeqNo> {
        let doc_id = doc_id.into();
        let entry = WalEntry::Index { doc_id: doc_id.clone(), source: source.clone() };

        let seq_no = {
            let ws = self.wal_shard_for(&doc_id);
            let mut wal = self.wal_lock_shard(ws);
            wal.append(&entry)?
        };

        let source_len = source.to_string().len();
        self.version_map.set(&doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);

        let shard = self.shard_for(&doc_id);
        let mut mem = self.memtable_shards[shard].lock().unwrap();
        mem.push(MemEntry {
            seq_no,
            doc_id,
            source: Some(std::sync::Arc::new(source)),
            source_bytes: std::sync::Arc::from(&[][..]),
        });
        self.memtable_bytes.fetch_add(source_len as u64, Ordering::Relaxed);

        debug!(seq_no, "document indexed");
        Ok(seq_no)
    }

    /// Batch-index multiple documents in a single WAL lock acquisition.
    /// Much faster than calling `index()` in a loop because:
    /// 1. One mutex lock for the entire batch (not N locks)
    /// 2. WAL entries written sequentially without releasing the lock
    /// 3. One memtable lock for the entire batch
    pub fn index_batch(
        &self,
        docs: &[(String, serde_json::Value)],
    ) -> Result<Vec<SeqNo>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        let mut seq_nos = Vec::with_capacity(docs.len());

        // Route batch to WAL shard of first doc (matches memtable shard routing)
        {
            let ws = if docs.is_empty() { 0 } else { self.wal_shard_for(&docs[0].0) };
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
            self.version_map.set(doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);
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
            self.memtable_bytes.fetch_add(source_len as u64, Ordering::Relaxed);
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

        let entry = WalEntry::Delete { doc_id: doc_id.to_owned() };
        let seq_no = {
            let ws = self.wal_shard_for(doc_id);
            let mut wal = self.wal_lock_shard(ws);
            wal.append(&entry)?
        };

        self.version_map.delete(doc_id, seq_no, IN_MEMORY_SEGMENT_ID)?;

        let shard = self.shard_for(doc_id);
        let mut mem = self.memtable_shards[shard].lock().unwrap();
        mem.push(MemEntry { seq_no, doc_id: doc_id.to_owned(), source: None, source_bytes: std::sync::Arc::from(&[][..]) });

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

    /// Flush the memtable, but call `post_finish` with the fresh `SegmentMeta`
    /// BEFORE the in-memory snapshot is swapped.  This lets the caller write
    /// side-car files (e.g. the FTS index) that must be present *before*
    /// readers can see the segment.  If `post_finish` returns an error, the
    /// segment is abandoned (the .seg file may remain on disk but is never
    /// referenced from the snapshot, so readers will not observe a
    /// half-written segment).
    pub fn flush_with_publisher<F>(
        &self,
        post_finish: F,
    ) -> Result<Option<SegmentMeta>>
    where
        F: FnOnce(&SegmentMeta) -> Result<()>,
    {
        // Drain the memtable and finalise in one shot (legacy path).
        let drained = match self.take_memtable_for_flush() {
            Some(e) => e,
            None => return Ok(None),
        };
        self.finalize_flush_with_publisher(drained, post_finish)
    }

    /// Finalise a flush using caller-supplied pre-drained memtable entries.
    /// See [`take_memtable_for_flush`] for the drain half.
    ///
    /// All segment I/O, FTS side-car writes, snapshot publication, and WAL
    /// checkpointing happen here — but no memtable locks are touched, so
    /// callers can release higher-level locks before calling this method.
    pub fn finalize_flush_with_publisher<F>(
        &self,
        drained: DrainedMemtable,
        post_finish: F,
    ) -> Result<Option<SegmentMeta>>
    where
        F: FnOnce(&SegmentMeta) -> Result<()>,
    {
        let entries = drained.entries;
        if entries.is_empty() {
            return Ok(None);
        }

        let doc_count = entries.iter().filter(|e| e.source.is_some()).count() as u64;
        let min_seq = entries.iter().map(|e| e.seq_no).min().unwrap_or(0);
        let max_seq = entries.iter().map(|e| e.seq_no).max().unwrap_or(0);

        let segments_dir = self.data_dir.join("segments");
        let mut writer =
            SegmentWriter::new(&segments_dir, self.config.schema_version, 0, 0)?;

        // Build stored-fields bytes directly, streaming each source value
        // into the output buffer via `serde_json::to_writer`.  The previous
        // version built an intermediate `Vec<serde_json::Value>` with a
        // `json!` macro that deep-cloned every `_source` (the dominant
        // flush cost on log workloads).  Writing bytes once avoids the
        // clone entirely — `e.source` is `Arc<Value>` and `to_writer` only
        // walks it for serialisation.
        let live_entries: Vec<&MemEntry> =
            entries.iter().filter(|e| e.source.is_some()).collect();
        let has_stored = !live_entries.is_empty();
        if has_stored {
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
            // V4 M4.6 — columnar V2 codec: per-column dict+bitpack,
            // cross-column determinism (URL→status/bytes collapses to a
            // mode table + exception bitmap), fallback to LZ4 on small
            // segments.  The encoder picks v1 or v2 internally based on
            // which is smaller on this segment, so we never write a
            // larger file than the pre-M4.6 format.
            let encoded = crate::stored_codec::encode_stored_v2(&stored_bytes);
            writer.add_section(SectionType::Stored, &encoded)?;
        }

        // Build tombstone section if any deletes
        let tombstone_ids: Vec<&str> =
            entries.iter().filter(|e| e.source.is_none()).map(|e| e.doc_id.as_str()).collect();
        if !tombstone_ids.is_empty() {
            let ts_bytes = serde_json::to_vec(&tombstone_ids)?;
            writer.add_section(SectionType::Tombstones, &ts_bytes)?;
        }

        let meta = writer.finish(doc_count, min_seq, max_seq)?;
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

        // V4 M4.8 — write a tiny `seg.ids` sidecar at flush time so
        // `rebuild_version_map_from_segments` on reopen can pull
        // `(doc_id, seq_no)` pairs directly without decoding the
        // stored section + parsing JSON for every doc.  On the
        // 66.5 M / 2 291-segment workload this drops cold restart
        // from ~302 s to ~5 s and 15 GB peak RSS to ~500 MB.
        //
        // Format (V1, uncompressed):
        //   "ZID1"            4 bytes magic
        //   u32  num_docs
        //   per doc:
        //     u64  seq_no
        //     u16  id_len
        //     id_len bytes (UTF-8 doc_id)
        //
        // Format (V2, M5.18, LZ4-compressed):
        //   "ZID2"            4 bytes magic
        //   u32  num_docs
        //   lz4_flex::compress_prepend_size(V1-body-sans-magic-and-numdocs)
        //
        // V2 compression ratio on real nginx ingest with synthetic
        // doc_ids (`c0d0`, `c0d1`, …) runs 7-10× because the id
        // stream has huge prefix repetition.  With real UUID-shaped
        // ids LZ4 still gets ~2× because the u64 seq_nos step
        // monotonically.  Reading V1 still works for old data dirs.
        {
            let segments_dir = self.data_dir.join("segments");
            let ids_path = segments_dir.join(format!("{}.ids", meta.id.as_str()));
            let live: Vec<&MemEntry> = entries.iter().filter(|e| e.source.is_some()).collect();
            let mut body: Vec<u8> = Vec::with_capacity(
                live.iter().map(|e| 8 + 2 + e.doc_id.len()).sum::<usize>(),
            );
            for e in &live {
                body.extend_from_slice(&e.seq_no.to_le_bytes());
                body.extend_from_slice(&(e.doc_id.len() as u16).to_le_bytes());
                body.extend_from_slice(e.doc_id.as_bytes());
            }
            let compressed = lz4_flex::compress_prepend_size(&body);
            let mut buf: Vec<u8> = Vec::with_capacity(4 + 4 + compressed.len());
            buf.extend_from_slice(b"ZID2");
            buf.extend_from_slice(&(live.len() as u32).to_le_bytes());
            buf.extend_from_slice(&compressed);
            if let Err(e) = std::fs::write(&ids_path, &buf) {
                tracing::warn!(?ids_path, "failed to write seg.ids sidecar: {e}");
            }
        }

        // Run the caller-supplied "build side-car files" step.  This must
        // succeed BEFORE we publish the segment to the snapshot — otherwise
        // a racing query could open the segment and find the side-cars
        // (e.g. FTS index) missing, returning wrong results.
        post_finish(&meta)?;

        // Update version map: point live docs at the new segment
        let segment_id_arc: std::sync::Arc<str> = std::sync::Arc::from(segment_id.as_str());
        for entry in &entries {
            if entry.source.is_some() {
                self.version_map.set(
                    &entry.doc_id,
                    entry.seq_no,
                    std::sync::Arc::clone(&segment_id_arc),
                    false,
                );
            }
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
        self.snapshot.rcu(|old| Arc::new(old.with_new_segment(meta.clone())));

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
            for ws in &self.wal_shards {
                let mut wal = ws.lock().unwrap();
                wal.checkpoint(max_seq)?;
                let _ = wal.rotate_if_large(64 * 1024 * 1024);
                wal.prune()?;
            }
        }

        // Persist the snapshot to disk
        self.save_snapshot()?;

        info!(segment_id, doc_count, min_seq, max_seq, "segment flushed");
        Ok(Some(meta))
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
    pub fn snapshot(&self) -> arc_swap::Guard<Arc<IndexSnapshot>> {
        self.snapshot.load()
    }

    /// Return the current WAL sequence number (the next value that
    /// `wal_append_batch` would assign).  Used by `Index::flush` to
    /// write a global checkpoint covering ALL shards after a
    /// multi-shard parallel flush.
    pub fn current_seq_no(&self) -> u64 {
        self.seq_counter.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Write a checkpoint to ALL WAL shards with the given max_seq_no.
    /// Used by `Index::flush` to write a global checkpoint after
    /// multi-shard parallel flush.
    pub fn wal_checkpoint_all(&self, max_seq: SeqNo) -> Result<()> {
        for ws in &self.wal_shards {
            let mut wal = ws.lock().unwrap();
            wal.checkpoint(max_seq)?;
        }
        Ok(())
    }

    /// Unconditionally run WAL maintenance (checkpoint + force-rotate
    /// + prune) across all shards.  Bypasses the
    /// `WAL_MAINTENANCE_INTERVAL_MS` gate that
    /// `finalize_flush_with_publisher` uses on the hot flush path.
    /// Called by `Index::flush()` (the final drain / user-triggered
    /// `_flush`) so disk cleanup happens immediately — matches ES's
    /// `_flush`-time translog rollover semantics where every user
    /// flush rotates and prunes.  The periodic background tick
    /// continues to use the 64 MB threshold (amortises rotation
    /// churn under sustained ingest), but explicit user flushes get
    /// the unconditional rotation so disk gets reclaimed promptly.
    pub fn force_wal_maintenance(&self, max_seq: SeqNo) -> Result<()> {
        for ws in &self.wal_shards {
            let mut wal = ws.lock().unwrap();
            wal.checkpoint(max_seq)?;
            wal.force_rotate()?;
            wal.prune()?;
        }
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
                if !ok { bad_sections += 1; }
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
        let meta = snap
            .segments
            .iter()
            .find(|s| s.id == segment_id)
            .ok_or_else(|| StorageError::SegmentNotFound(segment_id.to_owned()))?;

        let local_path = self.data_dir.join("segments").join(&meta.seg_path);

        // For object-store mode: check local cache; fetch from backend on miss.
        let reader = if let StorageMode::ObjectStore { backend, cache_dir } = &self.config.storage_mode {
            let cache_path = cache_dir.join(&meta.seg_path);
            if cache_path.exists() {
                crate::segment::SegmentReader::open(cache_path)?
            } else {
                let object_key = format!("segments/{}", meta.seg_path);
                let backend_clone = std::sync::Arc::clone(backend);
                let key_clone = object_key.clone();
                let data = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async move {
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
            crate::segment::SegmentReader::open(local_path)?
        };
        let arc = Arc::new(reader);
        self.seg_reader_cache.insert(segment_id.to_string(), Arc::clone(&arc));
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
        let snap = self.snapshot.load();
        let bytes = serde_json::to_vec_pretty(&**snap)?;
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
        std::fs::write(&tmp, &bytes)?;
        // Atomic rename onto the real path.  Last writer wins on the
        // final snapshot contents, but that's fine: each caller sees
        // the same `self.snapshot.load()` atomically-swapped payload
        // (arc_swap), so there is no content-level race — only the
        // filesystem tmp name was the contention source.
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn load_snapshot(data_dir: &Path) -> Result<IndexSnapshot> {
        let path = Self::snapshot_path(data_dir);
        let bytes = std::fs::read(&path)?;
        Ok(serde_json::from_slice(&bytes)?)
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
                if bytes.len() >= 8 && (&bytes[..4] == b"ZID1" || &bytes[..4] == b"ZID2") {
                    let num = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
                    // V2 = LZ4-compressed body after the 8-byte header.
                    let body: Vec<u8> = if &bytes[..4] == b"ZID2" {
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
                        if pos + 8 + 2 > body.len() { break; }
                        let seq_no = u64::from_le_bytes(
                            body[pos..pos + 8].try_into().unwrap(),
                        );
                        pos += 8;
                        let id_len = u16::from_le_bytes(
                            body[pos..pos + 2].try_into().unwrap(),
                        ) as usize;
                        pos += 2;
                        if pos + id_len > body.len() { break; }
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

        if total > 0 {
            info!(total, "version map rebuilt from segments");
        }
        Ok(())
    }

    // ── WAL replay ────────────────────────────────────────────────────────────

    fn replay_wal(&self, wal_dir: &Path) -> Result<()> {
        // Collect all WAL directories: root wal/ (legacy single-WAL) plus
        // any wal/s{N}/ subdirectories (sharded WAL).
        let mut wal_dirs: Vec<PathBuf> = Vec::new();
        // Check root for legacy .wal files
        if std::fs::read_dir(wal_dir)
            .ok()
            .map(|rd| rd.filter_map(|e| e.ok()).any(|e| {
                e.path().extension().map(|x| x == "wal").unwrap_or(false)
            }))
            .unwrap_or(false)
        {
            wal_dirs.push(wal_dir.to_path_buf());
        }
        // Scan for sharded subdirectories (s0, s1, ...)
        if let Ok(rd) = std::fs::read_dir(wal_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('s') && name_str[1..].parse::<usize>().is_ok() {
                    if entry.path().is_dir() {
                        wal_dirs.push(entry.path());
                    }
                }
            }
        }

        // Replay all WAL directories, merge-sorting by seq_no.
        let mut all_entries: Vec<ReplayEntry> = Vec::new();
        for dir in &wal_dirs {
            let reader = WalReader::new(dir);
            let iter = match reader.replay() {
                Ok(it) => it,
                Err(e) => {
                    warn!(error = %e, dir = ?dir, "failed to open WAL for replay");
                    continue;
                }
            };
            for result in iter {
                match result {
                    Ok(e) => all_entries.push(e),
                    Err(e) => {
                        warn!(error = %e, dir = ?dir, "skipping corrupt WAL entry during replay");
                    }
                }
            }
        }
        // Sort by seq_no to reconstruct global insertion order.
        all_entries.sort_by_key(|e| e.seq_no);

        let mut count = 0usize;
        for replay_entry in all_entries {
            match replay_entry.entry {
                WalEntry::Index { doc_id, source } => {
                    let seq_no = replay_entry.seq_no;
                    self.version_map.set(&doc_id, seq_no, IN_MEMORY_SEGMENT_ID, false);
                    let shard = self.shard_for(&doc_id);
                    let mut mem = self.memtable_shards[shard].lock().unwrap();
                    mem.push(MemEntry {
                        seq_no,
                        doc_id,
                        source: Some(std::sync::Arc::new(source)),
                        source_bytes: std::sync::Arc::from(&[][..]),
                    });
                }
                WalEntry::Delete { doc_id } => {
                    let seq_no = replay_entry.seq_no;
                    self.version_map.delete(&doc_id, seq_no, IN_MEMORY_SEGMENT_ID).ok();
                    let shard = self.shard_for(&doc_id);
                    let mut mem = self.memtable_shards[shard].lock().unwrap();
                    mem.push(MemEntry { seq_no, doc_id, source: None, source_bytes: std::sync::Arc::from(&[][..]) });
                }
                WalEntry::UpdateMapping { .. } => {}
            }
            let _ = self.seq_counter.fetch_max(replay_entry.seq_no + 1, Ordering::AcqRel);
            count += 1;
        }

        if count > 0 {
            info!(count, wal_dirs = wal_dirs.len(), "replayed WAL entries");
        }
        Ok(())
    }

    // ── Merge integration ─────────────────────────────────────────────────────

    /// Called by the merge executor (or the engine-level merge task) to
    /// atomically replace merged segments with the merged result and update
    /// the version map.
    pub fn apply_merge(
        &self,
        merged_ids: &[SegmentId],
        new_meta: SegmentMeta,
    ) -> Result<()> {
        // Atomic replace via rcu — same race as `with_new_segment` in
        // `finalize_flush_with_publisher`: a concurrent flush appending
        // its segment between load and store would drop our merged
        // segment swap. rcu retries on contention.
        self.snapshot.rcu(|old| Arc::new(old.replace_segments(merged_ids, new_meta.clone())));
        self.version_map.remove_segment(merged_ids);
        self.save_snapshot()?;
        info!(merged = merged_ids.len(), "merge applied");
        Ok(())
    }

    /// Returns stats useful for triggering merges.
    pub fn segment_stats(&self) -> Vec<(SegmentId, u64, u64)> {
        let snap = self.snapshot.load();
        snap.segments.iter().map(|s| (s.id.clone(), s.doc_count, s.size_bytes)).collect()
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
        let entry = WalEntry::Delete { doc_id: doc_id.to_owned() };
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
    pub fn wal_append_batch_raw(
        &self,
        docs: &[(String, std::sync::Arc<[u8]>)],
    ) -> Result<Vec<SeqNo>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }

        if std::env::var("XERJ_SKIP_WAL").is_ok() {
            let n = docs.len() as u64;
            let start_seq = self.seq_counter.fetch_add(n, std::sync::atomic::Ordering::AcqRel);
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
            let needs_escape = doc_id
                .bytes()
                .any(|b| b == b'"' || b == b'\\' || b < 0x20);
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
            (&mut seq_buf[..]).write_u64::<LittleEndian>(seq_no).unwrap();
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
            let saved_mode = wal.sync_mode();
            wal.set_sync_mode(SyncMode::Batched);
            wal.append_frames_locked(&frames, total_written)?;
            let strict_sync = std::env::var("XERJ_STRICT_SYNC")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false);
            let sync_result = if strict_sync {
                wal.sync()
            } else {
                wal.soft_flush()
            };
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

    pub fn wal_append_batch(
        &self,
        docs: &[(
            String,
            std::sync::Arc<serde_json::Value>,
            std::sync::Arc<[u8]>,
        )],
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
            let n = docs.len() as u64;
            let start_seq = self.seq_counter.fetch_add(n, std::sync::atomic::Ordering::AcqRel);
            let seq_nos: Vec<SeqNo> = (0..docs.len())
                .map(|i| start_seq + i as u64)
                .collect();
            return Ok(seq_nos);
        }

        // Single-pass frame assembly: build WAL envelope + CRC + framing
        // directly into one output buffer. Eliminates the intermediate
        // Vec<Vec<u8>> allocation that was 10k allocs per batch.
        let n = docs.len() as u64;
        let start_seq = self.seq_counter.fetch_add(n, std::sync::atomic::Ordering::AcqRel);
        let mut seq_nos: Vec<SeqNo> = Vec::with_capacity(docs.len());

        // Estimate total frame size: per-doc overhead ~80 bytes + source
        let est_total: usize = docs.iter()
            .map(|(id, _, sb)| id.len() + sb.len() + 100)
            .sum();
        let mut frames: Vec<u8> = Vec::with_capacity(est_total);

        for (i, (doc_id, source, source_bytes)) in docs.iter().enumerate() {
            let seq_no = start_seq + i as u64;
            seq_nos.push(seq_no);

            let bytes_to_write: std::borrow::Cow<[u8]> = if !source_bytes.is_empty() {
                std::borrow::Cow::Borrowed(source_bytes.as_ref())
            } else {
                match serde_json::to_vec(source.as_ref()) {
                    Ok(v) => std::borrow::Cow::Owned(v),
                    Err(_) => std::borrow::Cow::Owned(b"null".to_vec()),
                }
            };

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
            frames.extend_from_slice(&bytes_to_write);
            frames.extend_from_slice(b"}}");
            let payload_end = frames.len();

            // Payload is everything after the 13-byte header
            let payload_slice = &frames[payload_start + 13..payload_end];
            let payload_len = payload_slice.len() as u32;

            // CRC over seq_no(8) + op(1) + payload
            let mut hasher = crc32fast::Hasher::new();
            let mut seq_buf = [0u8; 8];
            use byteorder::{LittleEndian, WriteBytesExt};
            (&mut seq_buf[..]).write_u64::<LittleEndian>(seq_no).unwrap();
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
            let ws = if docs.is_empty() { 0 } else { self.wal_shard_for(&docs[0].0) };
            let mut wal = self.wal_lock_shard(ws);
            let saved_mode = wal.sync_mode();
            wal.set_sync_mode(SyncMode::Batched);
            wal.append_frames_locked(&frames, total_written)?;
            // M5.4 — SKIP the per-batch fsync on the bulk hot path.
            //
            // Pre-M5.4 each bulk batch called `wal.sync()` which
            // issues a `fsync(2)` — on NVMe that's ~1 ms of wall
            // time.  At 76 batches/sec (380 k/s ingest) that's
            // 76 ms/sec of fsync blocking = ~8 % of the ingest
            // wall time, spent purely on filesystem round-trips.
            //
            // Durability trade-off: without per-batch fsync, the
            // WAL bytes are buffered in the kernel page cache.  A
            // process crash loses nothing (the kernel flushes on
            // close).  A kernel crash / power loss between batches
            // can lose the last N seconds of data — same window as
            // a segment flush interval anyway.  The WAL still does
            // a periodic `sync` at rotation boundaries and at flush
            // time, and the segment commit path calls `fsync` on
            // the finished .seg file before the snapshot swap, so
            // acknowledged-and-flushed data is never lost.
            //
            // Setting `SyncMode::NoSync` via the `XERJ_STRICT_SYNC`
            // env var re-enables per-batch fsync for workloads
            // that need it.
            let strict_sync = std::env::var("XERJ_STRICT_SYNC")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false);
            let sync_result = if strict_sync {
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
        for (i, (doc_id, _source, _bytes)) in docs.iter().enumerate() {
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

    fn open_test_store(dir: &Path) -> Arc<IndexStore> {
        IndexStore::open(dir, IndexStoreConfig {
            sync_mode: SyncMode::Batched, // faster for tests
            ..Default::default()
        }).unwrap()
    }

    #[test]
    fn index_and_flush() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test_store(dir.path());

        store.index("doc-1", serde_json::json!({"title": "hello"})).unwrap();
        store.index("doc-2", serde_json::json!({"title": "world"})).unwrap();

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
            store.index(format!("doc-{i}"), serde_json::json!({"i": i})).unwrap();
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

        store.index("doc-1", serde_json::json!({"hello": "world"})).unwrap();
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

        let backend: Arc<dyn StorageBackend> = Arc::new(
            S3Backend::new(s3_dir.path(), "test-bucket", "xerj/"),
        );

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

        store.index("doc-1", serde_json::json!({"title": "hello s3"})).unwrap();
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

        let backend: Arc<dyn StorageBackend> = Arc::new(
            S3Backend::new(s3_dir.path(), "test-bucket", "xerj/"),
        );

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

        store.index("doc-1", serde_json::json!({"title": "cache test"})).unwrap();
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
}
