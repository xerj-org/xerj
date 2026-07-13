//! Lock-free document version tracking.
//!
//! Every indexed document has an entry in the version map that records the
//! highest sequence number seen for that `doc_id` and the segment (or
//! "in-memory" sentinel) that owns the latest version.
//!
//! ## Design
//!
//! The core data structure is a [`DashMap`], which is a concurrent hash map
//! that shards its lock space — reads and writes on disjoint key ranges do not
//! block each other.
//!
//! ### Conflict detection (optimistic concurrency)
//!
//! When a document arrives with `if_seq_no` / `if_primary_term` constraints
//! (Elasticsearch's optimistic concurrency), [`VersionMap::check_and_set`]
//! performs an atomic compare-and-swap:
//!
//! 1. Acquire the shard lock for the key.
//! 2. Read the current `(seq_no, segment_id)`.
//! 3. If the caller's expected seq_no matches, update and return `Ok(new_seq)`.
//! 4. Otherwise return `Err(VersionConflict)`.
//!
//! Because `DashMap::entry` holds the shard guard for the duration of the
//! closure, steps 2-3 are atomic with respect to other writers on the same key.
//!
//! ## Cleanup
//!
//! After a merge, call [`VersionMap::remove_segment`] to drop all entries whose
//! `segment_id` points to one of the now-deleted segments.  The merged segment's
//! ID is already written into the map during the merge process, so this removes
//! only stale / duplicate references.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tracing::debug;

use crate::segment::SegmentId;
use crate::{Result, SeqNo, StorageError};

/// Sentinel segment ID used for documents that are in the in-memory write buffer
/// (not yet flushed to any segment file).
pub const IN_MEMORY_SEGMENT_ID: &str = "__memtable__";

// ── VersionEntry ──────────────────────────────────────────────────────────────

/// Per-document version record.
///
/// `segment_id` is `Arc<str>` rather than `String` so that `VersionMap::get`
/// returns a cheap-to-clone view (one atomic increment) — the entry is read on
/// every doc lookup in the search/index hot path, and the same handful of
/// segment IDs (`__memtable__` + active segment IDs) is shared by millions of
/// entries.  Hoisting an `Arc::from(IN_MEMORY_SEGMENT_ID)` once per ingest
/// batch and `Arc::clone`-ing per doc replaces a per-doc `String` allocation
/// with one atomic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionEntry {
    /// The latest sequence number for this document.
    pub seq_no: SeqNo,
    /// The segment that owns the latest version (`IN_MEMORY_SEGMENT_ID` while
    /// the document is buffered in memory).
    pub segment_id: Arc<str>,
    /// `true` if the document has been deleted (tombstoned).
    pub deleted: bool,
    /// ES `_version`: per-document write counter, starting at 1 on first
    /// index and incremented by every subsequent index or delete of the
    /// same `doc_id` (deletes bump it too — the tombstone carries the
    /// bumped version, matching ES delete responses).
    ///
    /// Physical repoints (flush moving a doc from the memtable to its
    /// segment, merges repointing surviving docs) do NOT bump it: they
    /// re-record the same logical write.
    ///
    /// Restart caveat: the map is rebuilt from segments + WAL tail on
    /// open, so the counter restarts from the recovered history rather
    /// than the true lifetime write count (CAS is unaffected — it
    /// compares `seq_no`, which is durable).
    pub version: u64,
}

// ── VersionMap ───────────────────────────────────────────────────────────────

/// Concurrent, lock-free document version map.
pub struct VersionMap {
    /// doc_id → VersionEntry
    inner: DashMap<String, VersionEntry>,
    /// Maintained count of live (non-deleted) entries.
    ///
    /// `live_count()` used to iterate the whole map (`O(entries)`), which
    /// put an ~80 ms fixed cost on EVERY search over a 1 M-doc index — the
    /// query path calls it for the `deletes_present` gate and the shortcut
    /// guards.  Each mutating method applies its delta from the entry state
    /// it atomically displaced (the `DashMap` shard lock linearises
    /// per-key updates, so concurrent writers each observe a distinct
    /// old-value and the deltas sum exactly).  `i64` (not `u64`) so a
    /// transiently-interleaved reader can never underflow the type;
    /// `live_count()` clamps at 0.
    live: AtomicI64,
    /// Monotonic count of "ghost-producing" events: overwrites of an
    /// existing doc id and deletes.  Each such event leaves a physically
    /// present but superseded/tombstoned copy in the memtable or a
    /// segment until a merge purges it.
    ///
    /// The search fast paths (`deletes_present` gates in `search_inner` /
    /// `fast_aggs`) used to INFER this state from
    /// `live_count() < Σ segment doc_count + memtable doc_count` — but
    /// that arithmetic also trips on any unrelated physical-count drift
    /// (live-verified: two duplicate docs per merged segment flipped the
    /// gate permanently on a pure append-only workload, forcing every
    /// size>0 term/range/bool query into a full O(N) stored scan — the
    /// core of the read-under-write collapse).  This counter is the
    /// exact signal: zero ⇔ no update/delete has ever produced a ghost.
    ///
    /// Monotonic by design (never decremented on merge): once an index
    /// has seen updates, the delete-aware slow paths stay on.  That is
    /// conservative but always correct.
    ghost_events: std::sync::atomic::AtomicU64,
}

impl VersionMap {
    /// Create an empty version map.
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
            live: AtomicI64::new(0),
            ghost_events: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Apply a live-count delta for an entry transition.
    #[inline]
    fn live_delta(&self, old_live: bool, new_live: bool) {
        let delta = new_live as i64 - old_live as i64;
        if delta != 0 {
            self.live.fetch_add(delta, Ordering::Relaxed);
        }
    }

    /// Return the current version entry for `doc_id`, if any.
    pub fn get(&self, doc_id: &str) -> Option<VersionEntry> {
        self.inner.get(doc_id).map(|e| e.value().clone())
    }

    /// Unconditionally record a new version for `doc_id`.
    ///
    /// Used when indexing without optimistic concurrency control (most writes).
    ///
    /// `segment_id` accepts anything convertible to `Arc<str>` — a `&str`
    /// allocates a fresh shared buffer, a `String` is consumed in place, and
    /// an existing `Arc<str>` performs a single atomic increment with no
    /// allocation.  Hot-path callers (per-doc loops in `wal_append_batch_*`)
    /// should hoist the conversion above the loop and pass `Arc::clone` per
    /// iteration.
    pub fn set(
        &self,
        doc_id: impl Into<String>,
        seq_no: SeqNo,
        segment_id: impl Into<Arc<str>>,
        deleted: bool,
    ) -> u64 {
        let doc_id = doc_id.into();
        debug!(?doc_id, seq_no, "version_map::set");
        use dashmap::mapref::entry::Entry;
        // Per-doc `_version`: a STRICTLY newer seq is a new logical write
        // (bump); an equal seq is a physical repoint of the same write
        // (keep); an older seq only occurs on replay/recovery re-sets of
        // an already-superseded copy (keep — the newer write already
        // counted itself).  Computed and written under the dashmap shard
        // guard so racing writers on the same key each derive from the
        // exact entry they displace.
        let (version, was_overwrite, old_was_live) = match self.inner.entry(doc_id) {
            Entry::Occupied(mut occ) => {
                let old_seq = occ.get().seq_no;
                let old_live = !occ.get().deleted;
                let version = if seq_no > old_seq {
                    occ.get().version + 1
                } else {
                    occ.get().version
                };
                occ.insert(VersionEntry {
                    seq_no,
                    segment_id: segment_id.into(),
                    deleted,
                    version,
                });
                (version, old_seq != seq_no, old_live)
            }
            Entry::Vacant(vac) => {
                vac.insert(VersionEntry {
                    seq_no,
                    segment_id: segment_id.into(),
                    deleted,
                    version: 1,
                });
                (1, false, false)
            }
        };
        // Overwrite or delete — a superseded/tombstoned physical copy now
        // exists somewhere until merged away.  A same-seq_no re-set is NOT
        // an overwrite: the flush path repoints every drained doc's entry
        // from `__memtable__` to its segment id with the seq_no unchanged
        // (live-verified: counting those flipped the gate ON for every
        // flushed doc and re-disabled all fast paths under pure appends).
        if was_overwrite || deleted {
            self.ghost_events.fetch_add(1, Ordering::Relaxed);
        }
        self.live_delta(old_was_live, !deleted);
        version
    }

    /// Record `doc_id → (seq_no, segment_id)` only if `seq_no` is >= the
    /// currently recorded seq_no (or the doc is unknown).
    ///
    /// Used by the merge task to repoint surviving docs at the merged
    /// segment with their REAL per-doc seq_nos:
    /// - `>=` (not `>`): the surviving doc's entry currently points at a
    ///   merged-away INPUT segment with the SAME seq_no — equality must
    ///   win or `apply_merge → remove_segment` would drop the entry (and
    ///   the doc) entirely.
    /// - guarded (not unconditional `set`): a doc updated concurrently
    ///   while the merge ran has a NEWER entry pointing at the memtable /
    ///   another segment — clobbering it with the merged copy would
    ///   resurrect the stale version.
    ///
    /// Atomic per key via the dashmap entry API.
    pub fn set_if_latest(
        &self,
        doc_id: impl Into<String>,
        seq_no: SeqNo,
        segment_id: impl Into<Arc<str>>,
        deleted: bool,
    ) {
        let doc_id = doc_id.into();
        use dashmap::mapref::entry::Entry;
        match self.inner.entry(doc_id) {
            Entry::Occupied(mut occ) => {
                if seq_no >= occ.get().seq_no {
                    // Physical repoint of an already-counted write — the
                    // per-doc `_version` is carried over unchanged.
                    let version = occ.get().version;
                    occ.insert(VersionEntry {
                        seq_no,
                        segment_id: segment_id.into(),
                        deleted,
                        version,
                    });
                }
            }
            Entry::Vacant(vac) => {
                vac.insert(VersionEntry {
                    seq_no,
                    segment_id: segment_id.into(),
                    deleted,
                    version: 1,
                });
            }
        }
    }

    /// Attempt an atomic compare-and-set.
    ///
    /// - If `expected_seq_no` is `None`, succeeds only if the document does NOT
    ///   exist yet (create-only semantics).
    /// - If `expected_seq_no` is `Some(n)`, succeeds only if the current seq_no
    ///   equals `n`.
    ///
    /// On success, updates the entry and returns the new seq_no.
    /// On failure, returns [`StorageError::VersionConflict`].
    pub fn check_and_set(
        &self,
        doc_id: impl Into<String>,
        expected_seq_no: Option<SeqNo>,
        new_seq_no: SeqNo,
        new_segment_id: impl Into<Arc<str>>,
        deleted: bool,
    ) -> Result<SeqNo> {
        let doc_id = doc_id.into();
        let new_segment_id = new_segment_id.into();

        use dashmap::mapref::entry::Entry;

        match self.inner.entry(doc_id.clone()) {
            Entry::Occupied(mut occ) => {
                let current = occ.get().seq_no;
                match expected_seq_no {
                    None => {
                        // create-only: document must not already exist (or must be deleted)
                        if !occ.get().deleted {
                            return Err(StorageError::VersionConflict {
                                doc_id,
                                expected: 0,
                                actual: current,
                            });
                        }
                    }
                    Some(expected) => {
                        if current != expected {
                            return Err(StorageError::VersionConflict {
                                doc_id,
                                expected,
                                actual: current,
                            });
                        }
                    }
                }
                let was_live = !occ.get().deleted;
                // A CAS write is a genuine new logical write.
                let version = occ.get().version + 1;
                occ.insert(VersionEntry {
                    seq_no: new_seq_no,
                    segment_id: new_segment_id,
                    deleted,
                    version,
                });
                self.ghost_events.fetch_add(1, Ordering::Relaxed);
                self.live_delta(was_live, !deleted);
                Ok(new_seq_no)
            }
            Entry::Vacant(vac) => {
                if let Some(expected) = expected_seq_no {
                    // Caller expected a specific version but the doc doesn't exist
                    return Err(StorageError::VersionConflict {
                        doc_id,
                        expected,
                        actual: 0,
                    });
                }
                vac.insert(VersionEntry {
                    seq_no: new_seq_no,
                    segment_id: new_segment_id,
                    deleted,
                    version: 1,
                });
                if deleted {
                    self.ghost_events.fetch_add(1, Ordering::Relaxed);
                }
                self.live_delta(false, !deleted);
                Ok(new_seq_no)
            }
        }
    }

    /// Mark a document as deleted (tombstone) at `seq_no`.
    ///
    /// Returns `Ok(true)` if the document existed and was marked deleted.
    /// Returns `Ok(false)` if the document was not found (idempotent delete).
    pub fn delete(
        &self,
        doc_id: &str,
        seq_no: SeqNo,
        segment_id: impl Into<Arc<str>>,
    ) -> Result<bool> {
        match self.inner.get_mut(doc_id) {
            Some(mut entry) => {
                if entry.deleted {
                    return Ok(false); // already deleted
                }
                // ES bumps `_version` on delete: the tombstone carries the
                // bumped value (surfaced in the delete response, and a
                // subsequent re-index continues from it).
                let version = entry.version + 1;
                *entry = VersionEntry {
                    seq_no,
                    segment_id: segment_id.into(),
                    deleted: true,
                    version,
                };
                self.ghost_events.fetch_add(1, Ordering::Relaxed);
                self.live_delta(true, false);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Repoint a document's entry at a new segment WITHOUT counting a new
    /// logical write.  Used by the flush path: a drained memtable doc keeps
    /// its seq_no and `_version`, only its physical home changes.
    ///
    /// - Entry present with the SAME `seq_no` → swap `segment_id`, keep
    ///   `version` / `deleted` / counters untouched.
    /// - Entry present with a DIFFERENT `seq_no` → no-op.  Covers both a
    ///   superseded duplicate in the drained batch (an older copy of a doc
    ///   that was overwritten in the same memtable generation) and a doc
    ///   that was updated or deleted after the drain — clobbering either
    ///   would resurrect a stale version (the old unconditional `set` here
    ///   transiently did exactly that).
    /// - No entry → record it (recovery parity with the old `set` call).
    pub fn repoint(&self, doc_id: &str, seq_no: SeqNo, segment_id: impl Into<Arc<str>>) {
        use dashmap::mapref::entry::Entry;
        match self.inner.entry(doc_id.to_owned()) {
            Entry::Occupied(mut occ) => {
                if occ.get().seq_no == seq_no {
                    occ.get_mut().segment_id = segment_id.into();
                }
            }
            Entry::Vacant(vac) => {
                vac.insert(VersionEntry {
                    seq_no,
                    segment_id: segment_id.into(),
                    deleted: false,
                    version: 1,
                });
                self.live_delta(false, true);
            }
        }
    }

    /// Bump the `_version` of an EXISTING tombstone and return the bumped
    /// value.  ES semantics for `DELETE` of an already-deleted document:
    /// the delete reports `result: not_found` (404) but still increments
    /// the document's version, and a later re-index continues from it.
    /// Returns `None` when the doc is unknown or live.
    pub fn bump_tombstone_version(&self, doc_id: &str) -> Option<u64> {
        let mut entry = self.inner.get_mut(doc_id)?;
        if !entry.deleted {
            return None;
        }
        entry.version += 1;
        Some(entry.version)
    }

    /// Remove all entries whose `segment_id` is in `stale_segment_ids`.
    ///
    /// Called after a merge to clean up references to segments that no longer
    /// exist.  The merged segment's entries are already updated before this
    /// call, so this only removes genuinely stale entries (e.g. for documents
    /// that were deleted and tombstone-purged during the merge).
    pub fn remove_segment(&self, stale_segment_ids: &[SegmentId]) {
        let stale: std::collections::HashSet<&str> =
            stale_segment_ids.iter().map(String::as_str).collect();

        let mut removed_live: i64 = 0;
        self.inner.retain(|_, v| {
            let keep = !stale.contains(&*v.segment_id);
            if !keep && !v.deleted {
                removed_live += 1;
            }
            keep
        });
        if removed_live != 0 {
            self.live.fetch_sub(removed_live, Ordering::Relaxed);
        }
        debug!(
            stale_count = stale.len(),
            "version_map: cleaned up stale segments"
        );
    }

    /// Bulk-load entries from a segment during recovery / replay.
    pub fn bulk_load<I>(&self, entries: I)
    where
        I: IntoIterator<Item = (String, SeqNo, SegmentId, bool)>,
    {
        for (doc_id, seq_no, segment_id, deleted) in entries {
            // Only update if the incoming entry is newer
            let (should_insert, version) = match self.inner.get(&doc_id) {
                Some(existing) => (seq_no > existing.seq_no, existing.version + 1),
                None => (true, 1),
            };
            if should_insert {
                let old = self.inner.insert(
                    doc_id,
                    VersionEntry {
                        seq_no,
                        segment_id: Arc::from(segment_id),
                        deleted,
                        version,
                    },
                );
                self.live_delta(old.is_some_and(|e| !e.deleted), !deleted);
            }
        }
    }

    /// Return the total number of tracked documents (including deleted ones).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return `true` if the map is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return the count of live (non-deleted) documents.
    ///
    /// O(1) — reads the maintained counter instead of iterating the map
    /// (which cost ~80 ms per call at 1 M entries and was invoked on every
    /// search request via the `deletes_present` gate).
    pub fn live_count(&self) -> usize {
        self.live.load(Ordering::Relaxed).max(0) as usize
    }

    /// Number of overwrite/delete events ever seen (see `ghost_events`
    /// field docs).  Zero ⇔ append-only history ⇔ physical postings and
    /// stored copies are 1:1 with live docs (barring storage-level bugs,
    /// which corrupt the brute counting paths identically).
    pub fn ghost_events(&self) -> u64 {
        self.ghost_events.load(Ordering::Relaxed)
    }

    /// Conservatively mark the index as containing ghosts.  Called once at
    /// open time when the at-rest arithmetic (`live < physical`) indicates
    /// superseded copies from a pre-restart history the WAL replay cannot
    /// see.
    pub fn force_ghost_event(&self) {
        self.ghost_events.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for VersionMap {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_set_and_get() {
        let vm = VersionMap::new();
        vm.set("doc-1", 10, "seg-a", false);
        let e = vm.get("doc-1").unwrap();
        assert_eq!(e.seq_no, 10);
        assert_eq!(&*e.segment_id, "seg-a");
        assert!(!e.deleted);
        assert_eq!(e.version, 1);
    }

    #[test]
    fn version_bumps_on_newer_seq_only() {
        let vm = VersionMap::new();
        assert_eq!(vm.set("d", 1, "__memtable__", false), 1);
        assert_eq!(vm.set("d", 5, "__memtable__", false), 2); // overwrite
        // Same-seq repoint (flush) keeps the version.
        assert_eq!(vm.set("d", 5, "seg-a", false), 2);
        // Stale replay of an older copy keeps the version.
        assert_eq!(vm.set("d", 1, "seg-a", false), 2);
    }

    #[test]
    fn delete_bumps_version_and_reindex_continues() {
        let vm = VersionMap::new();
        vm.set("d", 1, "s", false); // v1
        vm.set("d", 2, "s", false); // v2
        vm.delete("d", 3, "s").unwrap(); // tombstone carries v3
        assert_eq!(vm.get("d").unwrap().version, 3);
        assert_eq!(vm.set("d", 4, "s", false), 4); // recreate → v4
    }

    #[test]
    fn bump_tombstone_version_only_on_tombstones() {
        let vm = VersionMap::new();
        assert_eq!(vm.bump_tombstone_version("ghost"), None);
        vm.set("d", 1, "s", false);
        assert_eq!(vm.bump_tombstone_version("d"), None); // live
        vm.delete("d", 2, "s").unwrap(); // v2 tombstone
        assert_eq!(vm.bump_tombstone_version("d"), Some(3));
        assert_eq!(vm.bump_tombstone_version("d"), Some(4));
        // A recreate continues from the bumped tombstone version.
        assert_eq!(vm.set("d", 3, "s", false), 5);
    }

    #[test]
    fn repoint_swaps_segment_without_bumping() {
        let vm = VersionMap::new();
        vm.set("d", 1, "__memtable__", false);
        vm.set("d", 2, "__memtable__", false); // v2 (overwrite in memtable)
        let ghosts_before = vm.ghost_events();

        // Flush replays BOTH drained copies; only the live seq repoints.
        vm.repoint("d", 1, "seg-a"); // superseded duplicate → no-op
        let e = vm.get("d").unwrap();
        assert_eq!((&*e.segment_id, e.seq_no, e.version), ("__memtable__", 2, 2));
        vm.repoint("d", 2, "seg-a");
        let e = vm.get("d").unwrap();
        assert_eq!((&*e.segment_id, e.seq_no, e.version), ("seg-a", 2, 2));
        assert_eq!(vm.ghost_events(), ghosts_before);

        // A doc deleted after the drain is NOT resurrected by the repoint.
        vm.delete("d", 3, "__memtable__").unwrap();
        vm.repoint("d", 2, "seg-b");
        assert!(vm.get("d").unwrap().deleted);
        assert_eq!(vm.get("d").unwrap().seq_no, 3);

        // Unknown doc → recorded live (recovery parity with the old set()).
        vm.repoint("x", 7, "seg-a");
        let e = vm.get("x").unwrap();
        assert_eq!((e.seq_no, e.version, e.deleted), (7, 1, false));
        assert_eq!(vm.live_count(), 1); // "x" live, "d" tombstoned
    }

    #[test]
    fn check_and_set_success() {
        let vm = VersionMap::new();
        vm.set("doc-1", 5, "seg-a", false);
        let new_seq = vm
            .check_and_set("doc-1", Some(5), 10, "seg-b", false)
            .unwrap();
        assert_eq!(new_seq, 10);
        assert_eq!(vm.get("doc-1").unwrap().seq_no, 10);
    }

    #[test]
    fn check_and_set_conflict() {
        let vm = VersionMap::new();
        vm.set("doc-1", 5, "seg-a", false);
        let result = vm.check_and_set("doc-1", Some(3), 10, "seg-b", false);
        assert!(matches!(
            result,
            Err(StorageError::VersionConflict {
                actual: 5,
                expected: 3,
                ..
            })
        ));
    }

    #[test]
    fn create_only_rejects_existing() {
        let vm = VersionMap::new();
        vm.set("doc-1", 1, "seg-a", false);
        let result = vm.check_and_set("doc-1", None, 2, "seg-b", false);
        assert!(matches!(result, Err(StorageError::VersionConflict { .. })));
    }

    #[test]
    fn create_only_on_deleted_succeeds() {
        let vm = VersionMap::new();
        vm.set("doc-1", 1, "seg-a", true); // deleted
        let result = vm.check_and_set("doc-1", None, 2, "seg-b", false);
        assert!(result.is_ok());
    }

    #[test]
    fn delete_marks_tombstone() {
        let vm = VersionMap::new();
        vm.set("doc-1", 1, "seg-a", false);
        let found = vm.delete("doc-1", 2, "seg-a").unwrap();
        assert!(found);
        assert!(vm.get("doc-1").unwrap().deleted);
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let vm = VersionMap::new();
        let found = vm.delete("ghost", 1, "seg-a").unwrap();
        assert!(!found);
    }

    #[test]
    fn remove_stale_segment() {
        let vm = VersionMap::new();
        vm.set("doc-1", 1, "seg-a", false);
        vm.set("doc-2", 2, "seg-b", false);
        vm.set("doc-3", 3, "seg-a", false);

        vm.remove_segment(&["seg-a".to_string()]);
        assert!(vm.get("doc-1").is_none());
        assert!(vm.get("doc-2").is_some());
        assert!(vm.get("doc-3").is_none());
    }

    #[test]
    fn live_count_excludes_deleted() {
        let vm = VersionMap::new();
        vm.set("a", 1, "s", false);
        vm.set("b", 2, "s", true);
        vm.set("c", 3, "s", false);
        assert_eq!(vm.live_count(), 2);
        assert_eq!(vm.len(), 3);
    }

    #[test]
    fn bulk_load_uses_newer_seq_no() {
        let vm = VersionMap::new();
        vm.set("doc-1", 5, "seg-a", false);
        vm.bulk_load(vec![
            ("doc-1".into(), 3, "seg-b".into(), false), // older — ignored
            ("doc-2".into(), 7, "seg-c".into(), false), // new
        ]);
        assert_eq!(vm.get("doc-1").unwrap().seq_no, 5); // unchanged
        assert_eq!(vm.get("doc-2").unwrap().seq_no, 7);
    }

    #[test]
    fn concurrent_set() {
        use std::sync::Arc;
        let vm = Arc::new(VersionMap::new());
        let mut handles = Vec::new();
        for i in 0..8u64 {
            let vm = Arc::clone(&vm);
            handles.push(std::thread::spawn(move || {
                vm.set(format!("doc-{i}"), i, "seg-x", false);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(vm.len(), 8);
    }
}
