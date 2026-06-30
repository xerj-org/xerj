//! Segment read-through cache for object-store backends.
//!
//! [`SegmentCache`] sits between the engine's segment readers and a remote
//! [`StorageBackend`] (e.g. the simulated S3 backend).  It keeps a local copy
//! of frequently-accessed segments on NVMe so that subsequent reads are served
//! at local-disk speed without a network round-trip.
//!
//! ## Cache policy
//!
//! - **Read-through**: on a cache miss the full segment is fetched from the
//!   backend and written to the cache directory before being returned to the
//!   caller.
//! - **Write-through**: the cache is populated by [`IndexStore::flush`] at
//!   write time so that the first read after a flush is always a cache hit.
//! - **LRU eviction**: [`maybe_evict`] scans the cache directory and removes
//!   the oldest files (by `mtime`) until the total size is below `max_size_bytes`.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use tracing::{debug, info, warn};

use crate::backend::StorageBackend;
use crate::{Result, StorageError};

// ── SegmentCache ──────────────────────────────────────────────────────────────

/// Local NVMe read-through cache for segments stored in an object-store backend.
pub struct SegmentCache {
    /// Directory where cached segment files are stored.
    cache_dir: PathBuf,
    /// Maximum total byte size of the cache before eviction triggers.
    max_size_bytes: u64,
    /// Remote backend to fetch segments from on a cache miss.
    backend: Arc<dyn StorageBackend>,
}

impl SegmentCache {
    /// Create a new [`SegmentCache`].
    ///
    /// - `cache_dir`: local directory for cached segments (created if absent).
    /// - `max_size_bytes`: eviction threshold.
    /// - `backend`: object-store to fetch from on cache misses.
    pub fn new(
        cache_dir: impl Into<PathBuf>,
        max_size_bytes: u64,
        backend: Arc<dyn StorageBackend>,
    ) -> Self {
        let cache_dir = cache_dir.into();
        std::fs::create_dir_all(&cache_dir).ok();
        Self { cache_dir, max_size_bytes, backend }
    }

    /// Return the data for `path`.
    ///
    /// Checks the local cache first.  On a miss, fetches from the backend,
    /// writes the result to the cache, and returns the data.
    pub async fn get(&self, path: &str) -> Result<Bytes> {
        let local_path = self.cache_dir.join(path.trim_start_matches('/'));

        if local_path.exists() {
            debug!(?local_path, "cache hit");
            let data = tokio::fs::read(&local_path).await?;
            return Ok(Bytes::from(data));
        }

        // Cache miss — fetch from backend.
        debug!(?local_path, path, "cache miss, fetching from backend");
        let data = self.backend.read_range(path, 0, u64::MAX).await?;

        // Cache locally — best-effort (failure must not block the caller).
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        if let Err(e) = tokio::fs::write(&local_path, &data).await {
            warn!(?local_path, error = %e, "failed to cache segment locally");
        } else {
            debug!(?local_path, bytes = data.len(), "segment cached");
        }

        Ok(data)
    }

    /// Remove the cached copy of `path`, if present.
    pub async fn invalidate(&self, path: &str) -> Result<()> {
        let local_path = self.cache_dir.join(path.trim_start_matches('/'));
        match tokio::fs::remove_file(&local_path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Evict the oldest cached segments when the total cache size exceeds
    /// `max_size_bytes`.
    ///
    /// Files are sorted by `mtime` (oldest first) and removed until the cache
    /// is below the threshold.  This is a simple LRU approximation — a
    /// production implementation would maintain an in-memory access log.
    pub async fn maybe_evict(&self) -> Result<()> {
        let cache_dir = self.cache_dir.clone();
        let max_size = self.max_size_bytes;

        // Walk the cache directory in a blocking task (synchronous FS calls).
        let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> =
            tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                collect_files(&cache_dir, &mut out);
                out
            })
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        let total_bytes: u64 = entries.iter().map(|(_, sz, _)| sz).sum();
        if total_bytes <= max_size {
            return Ok(()); // Nothing to evict.
        }

        // Sort oldest-first.
        entries.sort_by_key(|(_, _, mtime)| *mtime);

        let mut reclaimed = 0u64;
        let target = total_bytes.saturating_sub(max_size);

        for (path, size, _) in &entries {
            if reclaimed >= target {
                break;
            }
            match tokio::fs::remove_file(path).await {
                Ok(()) => {
                    reclaimed += size;
                    info!(?path, size, "evicted cached segment");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!(?path, error = %e, "failed to evict cached segment");
                }
            }
        }

        Ok(())
    }

    /// Return the total number of bytes currently in the cache.
    pub async fn cache_size_bytes(&self) -> Result<u64> {
        let cache_dir = self.cache_dir.clone();
        let total = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            collect_files(&cache_dir, &mut entries);
            entries.iter().map(|(_, sz, _)| sz).sum::<u64>()
        })
        .await
        .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(total)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Recursively collect (path, size, mtime) for all regular files under `dir`.
fn collect_files(dir: &PathBuf, out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                collect_files(&path, out);
            } else if meta.is_file() {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                out.push((path, meta.len(), mtime));
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{LocalFsBackend, S3Backend};

    #[tokio::test]
    async fn cache_miss_fetches_from_backend() {
        let backend_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(backend_dir.path()).unwrap());
        backend.write("segments/seg-001.seg", b"segment data here").await.unwrap();

        let cache = SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

        // First call — cache miss.
        let data = cache.get("segments/seg-001.seg").await.unwrap();
        assert_eq!(&data[..], b"segment data here");

        // File should now exist in cache.
        assert!(cache_dir.path().join("segments/seg-001.seg").exists());
    }

    #[tokio::test]
    async fn cache_hit_served_locally() {
        let backend_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(backend_dir.path()).unwrap());
        backend.write("seg.seg", b"original").await.unwrap();

        let cache =
            SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

        // Populate cache.
        cache.get("seg.seg").await.unwrap();

        // Modify the backend — cache should still serve the old version.
        backend.write("seg.seg", b"modified").await.unwrap();

        let data = cache.get("seg.seg").await.unwrap();
        assert_eq!(&data[..], b"original", "cache should serve stale-but-local copy");
    }

    #[tokio::test]
    async fn eviction_removes_oldest_files() {
        let backend_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(backend_dir.path()).unwrap());

        // Write two 10-byte segments to the backend.
        backend.write("a.seg", b"0123456789").await.unwrap();
        backend.write("b.seg", b"9876543210").await.unwrap();

        // Cache with max_size = 15 bytes (holds only one 10-byte segment after eviction).
        let cache = SegmentCache::new(cache_dir.path(), 15, Arc::clone(&backend));

        cache.get("a.seg").await.unwrap();
        // Small sleep to ensure mtime differs.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cache.get("b.seg").await.unwrap();

        // Total = 20 bytes > 15 — eviction should remove "a.seg" (oldest).
        cache.maybe_evict().await.unwrap();

        let size_after = cache.cache_size_bytes().await.unwrap();
        assert!(size_after <= 15, "expected <= 15 bytes after eviction, got {size_after}");
    }

    #[tokio::test]
    async fn invalidate_removes_cached_file() {
        let backend_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(backend_dir.path()).unwrap());
        backend.write("inv.seg", b"data").await.unwrap();

        let cache =
            SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

        cache.get("inv.seg").await.unwrap();
        assert!(cache_dir.path().join("inv.seg").exists());

        cache.invalidate("inv.seg").await.unwrap();
        assert!(!cache_dir.path().join("inv.seg").exists());

        // Invalidating a non-existent file is not an error.
        cache.invalidate("no-such-file.seg").await.unwrap();
    }

    #[tokio::test]
    async fn s3_backend_integration() {
        let s3_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(S3Backend::new(s3_dir.path(), "test-bucket", "xerj/"));
        backend.write("segments/s3-seg.seg", b"s3 segment bytes").await.unwrap();

        let cache =
            SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

        // Cache miss path through simulated S3.
        let data = cache.get("segments/s3-seg.seg").await.unwrap();
        assert_eq!(&data[..], b"s3 segment bytes");

        // Second call should be served from cache.
        let data2 = cache.get("segments/s3-seg.seg").await.unwrap();
        assert_eq!(&data2[..], b"s3 segment bytes");
    }
}
