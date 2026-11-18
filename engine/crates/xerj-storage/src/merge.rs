//! Segment merging: size-tiered strategy, tombstone purge, rate-limited I/O.
//!
//! ## Design
//!
//! Merging consolidates several small segments into one larger segment.  This:
//!
//! - Reduces the number of files to scan per query.
//! - Purges tombstones (deleted doc records) that would otherwise stay around.
//! - Improves cache locality for frequently-accessed data.
//!
//! ### Size-tiered merge policy
//!
//! Segments are grouped into "tiers" by size.  When a tier accumulates more
//! than `min_merge_count` segments, the smallest ones are merged.  The policy
//! favours merging small segments first — expensive I/O is amortised because
//! small segments have fewer bytes to rewrite.
//!
//! ### Rate limiting
//!
//! Background merges are rate-limited to avoid starving foreground indexing I/O.
//! The rate limiter tracks bytes written and sleeps the merge thread when the
//! instantaneous rate exceeds `io_rate_mb_per_sec`.  Setting the rate to `0`
//! disables limiting (useful in tests).
//!
//! ### Tombstone purge
//!
//! During merge, documents whose `doc_id` is flagged as deleted in the version
//! map are **omitted** from the output segment.  The tombstone sections of the
//! input segments are discarded.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use tracing::{debug, info, instrument, warn};

use crate::index_store::IndexStore;
use crate::segment::{SectionType, SegmentId, SegmentMeta, SegmentReader, SegmentWriter};
use crate::{Result, StorageError};

// ── MergePolicy trait ─────────────────────────────────────────────────────────

/// Decides which segments should be merged together.
pub trait MergePolicy: Send + Sync + 'static {
    /// Given the current set of segments, return the groups that should be
    /// merged.  Each inner `Vec` is one merge candidate set; the executor will
    /// merge them in the order returned.
    fn select_merges(&self, segments: &[SegmentMeta]) -> Vec<Vec<SegmentId>>;
}

// ── SizeTieredMergePolicy ─────────────────────────────────────────────────────

/// Merge segments within the same size tier when enough have accumulated.
///
/// Tiers are defined exponentially: tier 0 covers `[0, tier_size_mb)`, tier 1
/// covers `[tier_size_mb, tier_size_mb^2)`, etc.
#[derive(Debug, Clone)]
pub struct SizeTieredMergePolicy {
    /// Minimum number of segments in a tier before a merge is triggered.
    pub min_merge_count: usize,
    /// Maximum number of segments merged at once (to limit I/O spike).
    pub max_merge_count: usize,
    /// Tier boundary base in bytes (default: 5 MiB).
    pub tier_floor_bytes: u64,
    /// Maximum size a segment can reach before it is excluded from merges
    /// (already "large enough").
    pub max_merged_segment_bytes: u64,
    /// Upper bound on the SUM of input segment bytes in one merge batch.
    /// Issue #948: the merge executor materialises decoded + re-encoded
    /// copies of every input doc in memory (measured 5-9x the input bytes
    /// on mailbox-shaped docs), so an unbounded count-chunked batch of
    /// large segments is an unbounded heap spike.  A batch may exceed the
    /// cap only when even the tier's `min_merge_count` smallest segments
    /// already exceed it (otherwise no merge in that tier could ever run).
    /// 0 disables the bound (previous behaviour).
    pub max_batch_input_bytes: u64,
}

impl Default for SizeTieredMergePolicy {
    fn default() -> Self {
        Self {
            min_merge_count: 4,
            max_merge_count: 10,
            tier_floor_bytes: 5 * 1024 * 1024, // 5 MiB
            max_merged_segment_bytes: 5 * 1024 * 1024 * 1024, // 5 GiB
            max_batch_input_bytes: 512 * 1024 * 1024, // 512 MiB
        }
    }
}

impl SizeTieredMergePolicy {
    fn tier_for(&self, size_bytes: u64) -> u32 {
        if size_bytes == 0 {
            return 0;
        }
        let floor = self.tier_floor_bytes.max(1) as f64;
        let ratio = (size_bytes as f64 / floor).log2().max(0.0);
        ratio as u32
    }
}

impl MergePolicy for SizeTieredMergePolicy {
    fn select_merges(&self, segments: &[SegmentMeta]) -> Vec<Vec<SegmentId>> {
        // Group segments by tier
        let mut tiers: std::collections::BTreeMap<u32, Vec<&SegmentMeta>> =
            std::collections::BTreeMap::new();

        for seg in segments {
            // Skip segments that are already large
            if seg.size_bytes >= self.max_merged_segment_bytes {
                continue;
            }
            let tier = self.tier_for(seg.size_bytes);
            tiers.entry(tier).or_default().push(seg);
        }

        let mut merges = Vec::new();
        for (_tier, mut segs) in tiers {
            if segs.len() < self.min_merge_count {
                continue;
            }
            // Sort by size ascending — merge the smallest first.
            segs.sort_by_key(|s| s.size_bytes);

            // Emit multiple batches per tier so a single `select_merges`
            // call can schedule *all* the merges needed to collapse a
            // tier, instead of only the first `max_merge_count` segments.
            // Without this, a 700-segment tier-0 would need ~700 /
            // max_merge_count passes to converge.  The caller runs
            // batches sequentially and the spawn_blocking hand-off keeps
            // the runtime responsive, so there's no reason to
            // artificially hold batches back.
            //
            // #948 — batches are additionally cut on cumulative INPUT
            // bytes (`max_batch_input_bytes`): the executor holds decoded
            // + re-encoded copies of every input doc, so batch heap cost
            // scales with bytes, not segment count.  A batch only
            // overruns the cap when `min_merge_count` smallest segments
            // of the tier already exceed it by themselves.
            let id_sizes: Vec<(SegmentId, u64)> = segs
                .into_iter()
                .map(|s| (s.id.clone(), s.size_bytes))
                .collect();
            let mut start = 0usize;
            while start < id_sizes.len() {
                let mut batch: Vec<SegmentId> = Vec::new();
                let mut batch_bytes = 0u64;
                let mut i = start;
                while i < id_sizes.len() && batch.len() < self.max_merge_count {
                    let (id, size) = &id_sizes[i];
                    if self.max_batch_input_bytes > 0
                        && batch.len() >= self.min_merge_count
                        && batch_bytes + size > self.max_batch_input_bytes
                    {
                        break;
                    }
                    batch_bytes = batch_bytes.saturating_add(*size);
                    batch.push(id.clone());
                    i += 1;
                }
                if batch.len() >= self.min_merge_count {
                    merges.push(batch);
                }
                start = i;
            }
        }
        merges
    }
}

// ── RateLimiter ───────────────────────────────────────────────────────────────

/// Token-bucket rate limiter for merge I/O.
struct RateLimiter {
    /// Bytes per second budget.  0 = unlimited.
    bytes_per_sec: u64,
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    fn new(mb_per_sec: u64) -> Self {
        Self {
            bytes_per_sec: mb_per_sec * 1024 * 1024,
            tokens: (mb_per_sec * 1024 * 1024) as f64,
            last_refill: Instant::now(),
        }
    }

    /// Consume `bytes` tokens; sleep if the bucket is exhausted.
    fn consume(&mut self, bytes: u64) {
        if self.bytes_per_sec == 0 {
            return; // unlimited
        }
        // Refill tokens proportional to elapsed time
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens += elapsed * self.bytes_per_sec as f64;
        self.tokens = self.tokens.min(self.bytes_per_sec as f64); // cap at 1s of budget
        self.last_refill = now;

        self.tokens -= bytes as f64;
        if self.tokens < 0.0 {
            // Sleep until the bucket refills enough for the deficit
            let deficit = -self.tokens;
            let sleep_secs = deficit / self.bytes_per_sec as f64;
            std::thread::sleep(Duration::from_secs_f64(sleep_secs));
            self.tokens = 0.0;
            self.last_refill = Instant::now();
        }
    }
}

// ── MergeConfig ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MergeConfig {
    /// Merge I/O rate limit in MiB/s.  0 = unlimited.
    pub io_rate_mb_per_sec: u64,
    /// How often the background merge thread checks for merge candidates.
    pub check_interval_ms: u64,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            io_rate_mb_per_sec: 50,
            check_interval_ms: 5_000,
        }
    }
}

// ── MergeExecutor ─────────────────────────────────────────────────────────────

/// **Not part of the supported public API — the server does NOT use this.**
///
/// The running engine merges through its own path
/// (`xerj-engine::index::Index::merge_pass_locked`, driven by the
/// event-driven debounced checks of `Index::request_merge_check`, #871);
/// `MergeExecutor` is a legacy, storage-crate-only
/// helper kept solely as a convenience for in-crate unit tests. It is
/// `#[doc(hidden)]` and no longer re-exported at the crate root because it
/// has two footguns that make it unsafe to call on a real index:
///
/// 1. **It emits Stored-only segments.** `execute_merge` copies only the
///    `Stored` section of its inputs into the output segment — it drops the
///    `Fts` postings, `Columns` doc-values, `Points` BKD trees, `Vectors`,
///    and `Schema` sections entirely. A segment produced this way is
///    unqueryable by full-text / aggregation / kNN paths; the engine's own
///    merge rebuilds every section from the live in-memory index instead.
///
/// 2. **It repoints in the wrong order.** It calls
///    [`IndexStore::apply_merge`] (which atomically swaps the snapshot to
///    reference the new segment and retires the inputs) *before* it updates
///    the version map to point the merged docs at the new segment id. A
///    concurrent reader in that window can resolve a doc to an input segment
///    that has already been retired → a spurious miss. The engine path
///    publishes the version-map repoint and the snapshot swap in the safe
///    order.
///
/// Do not wire this into any production path. If you need the merge policy,
/// use [`SizeTieredMergePolicy`] (which *is* public and shared with the
/// engine); the execution belongs to the engine.
#[doc(hidden)]
pub struct MergeExecutor {
    store: Arc<IndexStore>,
    config: MergeConfig,
    /// Set to `true` by the background thread when shutdown is requested.
    shutdown: Arc<AtomicBool>,
}

impl MergeExecutor {
    pub fn new(store: Arc<IndexStore>, config: MergeConfig) -> Self {
        Self {
            store,
            config,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Merge a specific set of segments.  Called by the background thread or
    /// by tests directly.
    #[instrument(skip(self), fields(count = segment_ids.len()))]
    pub fn execute_merge(&self, segment_ids: &[SegmentId]) -> Result<SegmentMeta> {
        if segment_ids.len() < 2 {
            return Err(StorageError::MergeAborted(
                "need at least 2 segments to merge".into(),
            ));
        }

        let snap = self.store.snapshot();
        let segments_dir = self.store.data_dir.join("segments");

        let mut rate_limiter = RateLimiter::new(self.config.io_rate_mb_per_sec);

        // Collect metadata for each segment to merge
        let metas: Vec<&SegmentMeta> = segment_ids
            .iter()
            .filter_map(|id| snap.segments.iter().find(|s| &s.id == id))
            .collect();

        if metas.len() != segment_ids.len() {
            let missing: Vec<_> = segment_ids
                .iter()
                .filter(|id| !metas.iter().any(|m| &m.id == *id))
                .collect();
            return Err(StorageError::MergeAborted(format!(
                "segments not found: {missing:?}"
            )));
        }

        let total_docs: u64 = metas.iter().map(|m| m.doc_count).sum();
        let min_seq = metas.iter().map(|m| m.min_seq_no).min().unwrap_or(0);
        let max_seq = metas.iter().map(|m| m.max_seq_no).max().unwrap_or(0);

        info!(
            merge_segments = segment_ids.len(),
            input_docs = total_docs,
            min_seq,
            max_seq,
            "starting merge"
        );

        // Build merged stored-fields section, skipping tombstoned docs
        let mut merged_docs: Vec<Json> = Vec::new();
        let version_map = &self.store.version_map;

        for meta in &metas {
            let seg_path = segments_dir.join(&meta.seg_path);
            let reader = match SegmentReader::open(&seg_path) {
                Ok(r) => r,
                Err(e) => {
                    warn!(?seg_path, "failed to open segment for merge: {e}");
                    return Err(e);
                }
            };

            // Read and filter stored docs
            if let Some(stored_bytes_raw) = reader.section(SectionType::Stored)? {
                rate_limiter.consume(stored_bytes_raw.len() as u64);

                // #1122/#2433: re-encoded below; stripped rows stay stripped
                // unless XERJ_STORE_SEMANTIC_VECTORS asks for full vectors.
                let stored_bytes =
                    crate::stored_codec::decode_stored_for_rewrite(stored_bytes_raw)?;
                let docs: Vec<Json> = serde_json::from_slice(&stored_bytes)?;
                for doc in docs {
                    let doc_id = doc.get("_id").and_then(|v| v.as_str()).unwrap_or("");
                    // Skip if deleted in the version map
                    if let Some(entry) = version_map.get(doc_id) {
                        if entry.deleted {
                            debug!(?doc_id, "purging tombstoned doc during merge");
                            continue;
                        }
                        // Skip if there's a newer version in a different segment
                        // (this doc is a stale copy from an earlier flush)
                        let doc_seq = doc.get("_seq_no").and_then(|v| v.as_u64()).unwrap_or(0);
                        if entry.seq_no > doc_seq {
                            debug!(?doc_id, "skipping stale copy during merge");
                            continue;
                        }
                    }
                    merged_docs.push(doc);
                }
            }
        }

        // Emit the merged segment in global insertion (_seq_no) order.  Inputs
        // are per-memtable-shard segments (hash-partitioned by _id), so plain
        // concatenation scrambles physical row order vs arrival order; ES
        // tie-breaks equal scores by internal doc id == arrival order, and the
        // engine-side merge (index.rs merge_pass_locked) applies the same sort.
        merged_docs
            .sort_unstable_by_key(|doc| doc.get("_seq_no").and_then(|v| v.as_u64()).unwrap_or(0));

        let live_doc_count = merged_docs.len() as u64;

        // Write merged segment — inherit schema version from the snapshot if available
        let schema_version = snap.segments.first().map(|_| 1u32).unwrap_or(1);
        let mut writer = SegmentWriter::new(&segments_dir, schema_version, snap.generation, 0)?;

        if !merged_docs.is_empty() {
            let stored_bytes = serde_json::to_vec(&merged_docs)?;
            rate_limiter.consume(stored_bytes.len() as u64);
            let encoded = crate::stored_codec::encode_stored_lz4(&stored_bytes);
            writer.add_section(SectionType::Stored, &encoded)?;
        }

        let merged_meta = writer.finish(live_doc_count, min_seq, max_seq)?;

        // Atomically apply the merge to the index store
        self.store.apply_merge(segment_ids, merged_meta.clone())?;

        // Update version map for all merged docs · hoist the Arc once.
        let merged_id_arc: std::sync::Arc<str> = std::sync::Arc::from(merged_meta.id.as_str());
        for doc in &merged_docs {
            if let Some(doc_id) = doc.get("_id").and_then(|v| v.as_str()) {
                if let Some(seq_no) = doc.get("_seq_no").and_then(|v| v.as_u64()) {
                    version_map.set(doc_id, seq_no, std::sync::Arc::clone(&merged_id_arc), false);
                }
            }
        }

        info!(
            merged_id = merged_meta.id,
            live_docs = live_doc_count,
            purged_docs = total_docs.saturating_sub(live_doc_count),
            "merge complete"
        );

        Ok(merged_meta)
    }

    /// Spawn a background thread that periodically checks for merge candidates.
    ///
    /// The thread runs until [`MergeExecutor::shutdown`] is called.  Returns a
    /// handle that can be joined on shutdown. If the thread cannot be created
    /// (#1122) the handle is inert — `join` returns immediately — and merges
    /// are not scheduled until the next successful spawn. The loop is not run
    /// inline: it does not return until shutdown.
    pub fn spawn_background<P>(
        self: Arc<Self>,
        policy: Arc<P>,
    ) -> xerj_common::thread_spawn::SpawnedThread
    where
        P: MergePolicy,
    {
        let shutdown = Arc::clone(&self.shutdown);
        let interval = Duration::from_millis(self.config.check_interval_ms);

        // #1122: `.expect` on `std::thread::Builder::spawn` aborts the process
        // both when `pthread_create` returns an error and when the child
        // panics mapping its alternate signal stack (`panic = "abort"`).
        // Background merges are maintenance; failing to start the thread
        // skips them instead of taking the node down.
        match xerj_common::thread_spawn::try_spawn_unguarded("xerj-merge", move || {
            info!("merge background thread started");
            while !shutdown.load(Ordering::Relaxed) {
                std::thread::sleep(interval);

                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                let snap = self.store.snapshot();
                let candidates = policy.select_merges(&snap.segments);
                drop(snap); // release guard before I/O

                if candidates.is_empty() {
                    debug!("no merge candidates");
                    continue;
                }

                for batch in candidates {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    match self.execute_merge(&batch) {
                        Ok(meta) => info!(merged_id = meta.id, "background merge completed"),
                        Err(e) => warn!("background merge failed: {e}"),
                    }
                }
            }
            info!("merge background thread stopped");
        }) {
            Ok(handle) => handle,
            Err(failure) => {
                static LOGGED: AtomicBool = AtomicBool::new(false);
                xerj_common::thread_spawn::warn_best_effort_degraded(
                    &LOGGED,
                    "xerj-merge",
                    &failure.error,
                );
                drop(failure.job);
                xerj_common::thread_spawn::SpawnedThread::inert()
            }
        }
    }

    /// Signal the background thread to stop at the next check.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_store::IndexStoreConfig;
    use crate::wal::SyncMode;

    fn test_store(dir: &std::path::Path) -> Arc<IndexStore> {
        IndexStore::open(
            dir,
            IndexStoreConfig {
                sync_mode: SyncMode::Batched,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn merge_two_segments() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());

        // Create two segments
        store.index("doc-1", serde_json::json!({"v": 1})).unwrap();
        store.flush().unwrap();
        store.index("doc-2", serde_json::json!({"v": 2})).unwrap();
        store.flush().unwrap();

        assert_eq!(store.snapshot().segments.len(), 2);

        let ids: Vec<SegmentId> = store
            .snapshot()
            .segments
            .iter()
            .map(|s| s.id.clone())
            .collect();

        let executor = Arc::new(MergeExecutor::new(
            Arc::clone(&store),
            MergeConfig {
                io_rate_mb_per_sec: 0,
                ..Default::default()
            },
        ));

        let meta = executor.execute_merge(&ids).unwrap();
        assert_eq!(meta.doc_count, 2);

        // After merge there should be exactly one segment
        assert_eq!(store.snapshot().segments.len(), 1);
        assert_eq!(store.snapshot().segments[0].id, meta.id);
    }

    #[test]
    fn merge_purges_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());

        store.index("doc-1", serde_json::json!({"v": 1})).unwrap();
        store.flush().unwrap();

        // Delete doc-1, then create a second segment
        store.delete("doc-1").unwrap();
        store.index("doc-2", serde_json::json!({"v": 2})).unwrap();
        store.flush().unwrap();

        let ids: Vec<SegmentId> = store
            .snapshot()
            .segments
            .iter()
            .map(|s| s.id.clone())
            .collect();

        let executor = Arc::new(MergeExecutor::new(
            Arc::clone(&store),
            MergeConfig {
                io_rate_mb_per_sec: 0,
                ..Default::default()
            },
        ));

        let meta = executor.execute_merge(&ids).unwrap();
        // doc-1 was deleted, so only doc-2 should survive
        assert_eq!(meta.doc_count, 1);
    }

    #[test]
    fn size_tiered_policy_groups_correctly() {
        let policy = SizeTieredMergePolicy {
            min_merge_count: 2,
            max_merge_count: 10,
            ..Default::default()
        };

        let make_meta = |id: &str, size: u64| SegmentMeta {
            id: id.to_string(),
            doc_count: 1,
            size_bytes: size,
            min_seq_no: 1,
            max_seq_no: 1,
            created_at_ms: 0,
            has_tombstones: false,
            seg_path: format!("{id}.seg"),
            sidx_path: format!("{id}.sidx"),
        };

        let segments = vec![
            make_meta("a", 1_000),
            make_meta("b", 1_500),
            make_meta("c", 50_000_000),
            make_meta("d", 55_000_000),
        ];

        let merges = policy.select_merges(&segments);
        // Should have at least one group (the small ones or large ones)
        assert!(!merges.is_empty());
        // None of the groups should mix different tiers drastically
        for group in &merges {
            assert!(group.len() >= 2);
        }
    }

    #[test]
    fn merge_requires_at_least_two_segments() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        store.index("doc-1", serde_json::json!({})).unwrap();
        store.flush().unwrap();

        let ids: Vec<SegmentId> = store
            .snapshot()
            .segments
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let executor = Arc::new(MergeExecutor::new(
            Arc::clone(&store),
            MergeConfig {
                io_rate_mb_per_sec: 0,
                ..Default::default()
            },
        ));
        assert!(matches!(
            executor.execute_merge(&ids),
            Err(StorageError::MergeAborted(_))
        ));
    }

    // ── #948: merge batch input-byte bound ──────────────────────────────────

    fn sized_segment(id: &str, size_bytes: u64) -> SegmentMeta {
        SegmentMeta {
            id: id.to_string(),
            doc_count: 1,
            size_bytes,
            min_seq_no: 0,
            max_seq_no: 1,
            created_at_ms: 0,
            has_tombstones: false,
            seg_path: String::new(),
            sidx_path: String::new(),
        }
    }

    const MI: u64 = 1024 * 1024;

    #[test]
    fn size_tiered_batches_respect_input_byte_cap() {
        // #948 — the merge executor materialises decoded + re-encoded copies
        // of every input document (measured 5-9x input bytes on mailbox
        // docs), so batch heap cost scales with INPUT BYTES, not segment
        // count.  A count-chunked batch of large segments is an unbounded
        // heap spike.  The default policy caps a batch at 512 MiB.
        let policy = SizeTieredMergePolicy::default();
        let cap = policy.max_batch_input_bytes;
        assert!(cap > 0, "default policy must bound batch input bytes");

        // Ten 100 MiB segments in the same size tier: 1000 MiB of input —
        // one count-chunked batch pre-#948 (max_merge_count = 10).  The cap
        // must split them into <= 512 MiB batches.
        let segments: Vec<SegmentMeta> = (0..10)
            .map(|i| sized_segment(&format!("seg-{i}"), 100 * MI))
            .collect();
        let size_of =
            |id: &str| -> u64 { segments.iter().find(|s| s.id == id).unwrap().size_bytes };

        let batches = policy.select_merges(&segments);
        assert!(
            batches.len() >= 2,
            "1000 MiB of input must not land in one batch: {:?}",
            batches
        );
        for batch in &batches {
            let input_bytes: u64 = batch.iter().map(|id| size_of(id)).sum();
            assert!(
                input_bytes <= cap || batch.len() <= policy.min_merge_count,
                "batch of {} segments carries {} MiB > {} MiB cap",
                batch.len(),
                input_bytes / MI,
                cap / MI
            );
        }
        // The bound must not stop the tier converging: every segment that
        // made it into a batch is merged exactly once.
        let merged: std::collections::HashSet<&str> = batches
            .iter()
            .flat_map(|b| b.iter().map(|id| id.as_str()))
            .collect();
        assert_eq!(merged.len(), 10, "all ten segments must still merge");
    }

    #[test]
    fn size_tiered_batch_cap_floor_is_min_merge_count() {
        // When even the tier's `min_merge_count` smallest segments exceed
        // the cap by themselves, the floor wins: a batch of exactly
        // `min_merge_count` is allowed to overrun, otherwise the tier
        // could never merge at all.
        let policy = SizeTieredMergePolicy {
            max_batch_input_bytes: 10 * MI,
            ..Default::default()
        };
        let segments: Vec<SegmentMeta> = (0..8)
            .map(|i| sized_segment(&format!("seg-{i}"), 4 * MI))
            .collect();

        let batches = policy.select_merges(&segments);
        assert!(!batches.is_empty());
        for batch in &batches {
            assert!(
                batch.len() == policy.min_merge_count,
                "cap smaller than min_merge_count inputs must yield exact-floor batches, got {:?}",
                batch
            );
        }
    }
}
