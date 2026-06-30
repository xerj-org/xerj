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

use std::sync::Arc;

use dashmap::DashMap;
use tracing::debug;

use crate::{Result, SeqNo, StorageError};
use crate::segment::SegmentId;

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
}

// ── VersionMap ───────────────────────────────────────────────────────────────

/// Concurrent, lock-free document version map.
pub struct VersionMap {
    /// doc_id → VersionEntry
    inner: DashMap<String, VersionEntry>,
}

impl VersionMap {
    /// Create an empty version map.
    pub fn new() -> Self {
        Self { inner: DashMap::new() }
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
    ) {
        let doc_id = doc_id.into();
        debug!(?doc_id, seq_no, "version_map::set");
        self.inner.insert(doc_id, VersionEntry { seq_no, segment_id: segment_id.into(), deleted });
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
                occ.insert(VersionEntry { seq_no: new_seq_no, segment_id: new_segment_id, deleted });
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
                vac.insert(VersionEntry { seq_no: new_seq_no, segment_id: new_segment_id, deleted });
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
                *entry = VersionEntry {
                    seq_no,
                    segment_id: segment_id.into(),
                    deleted: true,
                };
                Ok(true)
            }
            None => Ok(false),
        }
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

        self.inner.retain(|_, v| !stale.contains(&*v.segment_id));
        debug!(stale_count = stale.len(), "version_map: cleaned up stale segments");
    }

    /// Bulk-load entries from a segment during recovery / replay.
    pub fn bulk_load<I>(&self, entries: I)
    where
        I: IntoIterator<Item = (String, SeqNo, SegmentId, bool)>,
    {
        for (doc_id, seq_no, segment_id, deleted) in entries {
            // Only update if the incoming entry is newer
            let should_insert = match self.inner.get(&doc_id) {
                Some(existing) => seq_no > existing.seq_no,
                None => true,
            };
            if should_insert {
                self.inner.insert(
                    doc_id,
                    VersionEntry { seq_no, segment_id: Arc::from(segment_id), deleted },
                );
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
    pub fn live_count(&self) -> usize {
        self.inner.iter().filter(|e| !e.value().deleted).count()
    }
}

impl Default for VersionMap {
    fn default() -> Self { Self::new() }
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
    }

    #[test]
    fn check_and_set_success() {
        let vm = VersionMap::new();
        vm.set("doc-1", 5, "seg-a", false);
        let new_seq = vm.check_and_set("doc-1", Some(5), 10, "seg-b", false).unwrap();
        assert_eq!(new_seq, 10);
        assert_eq!(vm.get("doc-1").unwrap().seq_no, 10);
    }

    #[test]
    fn check_and_set_conflict() {
        let vm = VersionMap::new();
        vm.set("doc-1", 5, "seg-a", false);
        let result = vm.check_and_set("doc-1", Some(3), 10, "seg-b", false);
        assert!(matches!(result, Err(StorageError::VersionConflict { actual: 5, expected: 3, .. })));
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
        for h in handles { h.join().unwrap(); }
        assert_eq!(vm.len(), 8);
    }
}
