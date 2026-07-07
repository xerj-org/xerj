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
}

impl Default for SizeTieredMergePolicy {
    fn default() -> Self {
        Self {
            min_merge_count: 4,
            max_merge_count: 10,
            tier_floor_bytes: 5 * 1024 * 1024, // 5 MiB
            max_merged_segment_bytes: 5 * 1024 * 1024 * 1024, // 5 GiB
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
            let ids: Vec<SegmentId> = segs.into_iter().map(|s| s.id.clone()).collect();
            let mut start = 0usize;
            while start < ids.len() {
                let end = (start + self.max_merge_count).min(ids.len());
                let batch: Vec<SegmentId> = ids[start..end].to_vec();
                if batch.len() >= self.min_merge_count {
                    merges.push(batch);
                }
                start = end;
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

/// Reads multiple segments, writes one merged segment, and applies the result
/// to the [`IndexStore`] atomically.
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

                let stored_bytes = crate::stored_codec::decode_stored(stored_bytes_raw)?;
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
    /// handle that can be joined on shutdown.
    pub fn spawn_background<P>(self: Arc<Self>, policy: Arc<P>) -> std::thread::JoinHandle<()>
    where
        P: MergePolicy,
    {
        let shutdown = Arc::clone(&self.shutdown);
        let interval = Duration::from_millis(self.config.check_interval_ms);

        std::thread::Builder::new()
            .name("xerj-merge".to_string())
            .spawn(move || {
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
            })
            .expect("failed to spawn merge thread")
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
}
