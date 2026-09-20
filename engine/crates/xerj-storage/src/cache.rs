//! Segment read-through cache for object-store backends.
//!
//! [`SegmentCache`] sits between the engine's segment readers and a remote
//! [`StorageBackend`] — in production [`crate::s3::S3Backend`], in tests
//! [`crate::backend::SimulatedObjectStore`]. It keeps a local copy of
//! frequently-accessed segments on NVMe so subsequent reads are served at
//! local-disk speed without a network round-trip, and without a billed request.
//!
//! ## Why the counters matter
//!
//! On a per-request-billed store the cache hit rate *is* the cost model: every
//! miss is a `GetObject`. [`SegmentCache::stats`] reports hits, misses, bytes
//! fetched and bytes served locally so that number is observable rather than
//! assumed. See `docs/OBJECT_STORAGE.md` for measured figures.
//!
//! ## Cache policy
//!
//! - **Read-through**: on a cache miss the full segment is fetched from the
//!   backend and written to the cache directory before being returned to the
//!   caller.
//! - **Write-through**: the cache is populated by [`IndexStore::flush`] at
//!   write time so that the first read after a flush is always a cache hit.
//! - **LRU eviction**: [`SegmentCache::maybe_evict`] scans the cache directory
//!   and removes the oldest files (by `mtime`) until the total size is below
//!   `max_size_bytes`.
//! - **Whole-object granularity**: a miss fetches the whole object, so a
//!   4 KiB range read of an uncached 40 MiB segment transfers 40 MiB. That is
//!   the right trade when the next read of the same segment is likely (the
//!   engine's reads are `mmap` over a whole segment) and the wrong one for a
//!   single cold probe. [`SegmentCache::get_range_uncached`] exists for the
//!   latter: it fetches just the range and caches nothing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tracing::{debug, info, warn};

use crate::backend::StorageBackend;
use crate::{Result, StorageError};

// ── SegmentCache ──────────────────────────────────────────────────────────────

/// Counters describing how much of the read traffic the cache absorbed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheStats {
    /// Reads served from the local cache directory. No billed request.
    pub hits: u64,
    /// Reads that had to go to the backend.
    pub misses: u64,
    /// Bytes fetched from the backend (what egress and request billing see).
    pub bytes_fetched: u64,
    /// Bytes served out of the local cache.
    pub bytes_served_local: u64,
    /// Fetches whose local write failed, so the next read will miss again.
    pub cache_write_failures: u64,
}

impl CacheStats {
    /// Hit rate over hits + misses, or `None` before the first read.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        (total > 0).then(|| self.hits as f64 / total as f64)
    }
}

#[derive(Debug, Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    bytes_fetched: AtomicU64,
    bytes_served_local: AtomicU64,
    cache_write_failures: AtomicU64,
}

/// Local NVMe read-through cache for segments stored in an object-store backend.
pub struct SegmentCache {
    /// Directory where cached segment files are stored.
    cache_dir: PathBuf,
    /// Maximum total byte size of the cache before eviction triggers.
    max_size_bytes: u64,
    /// Remote backend to fetch segments from on a cache miss.
    backend: Arc<dyn StorageBackend>,
    /// Hit/miss accounting — see [`CacheStats`].
    counters: Counters,
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
        Self {
            cache_dir,
            max_size_bytes,
            backend,
            counters: Counters::default(),
        }
    }

    /// A snapshot of the hit/miss counters.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            bytes_fetched: self.counters.bytes_fetched.load(Ordering::Relaxed),
            bytes_served_local: self.counters.bytes_served_local.load(Ordering::Relaxed),
            cache_write_failures: self.counters.cache_write_failures.load(Ordering::Relaxed),
        }
    }

    /// Billed-request counters of the backend underneath, when it has any.
    pub fn backend_ops(&self) -> Option<crate::backend::ObjectStoreOpsSnapshot> {
        self.backend.ops()
    }

    /// The local path a cached object occupies.
    ///
    /// `path` is a flat object key, so a key containing `..` would otherwise
    /// escape the cache directory. Each component is checked and a traversal
    /// attempt is refused rather than normalised away.
    ///
    /// A backslash anywhere in the key is refused outright, not split on.
    /// `PathBuf::join` treats `\` as a separator on Windows, so a key like
    /// `a..\..\..\etc\x` has no `/`-delimited `..` component and would escape
    /// there while looking like one harmless filename on Linux — a guard that
    /// holds on the developer's machine and not on the shipped Windows binary
    /// is the worst kind. `xerj-autoindex`'s key classifier takes the same
    /// position (`source.rs`, `classify_key`).
    fn cache_path(&self, path: &str) -> Result<PathBuf> {
        let rel = path.trim_start_matches('/');
        if rel.is_empty() {
            return Err(StorageError::Backend("empty object key".into()));
        }
        if rel.contains('\\') {
            return Err(StorageError::Backend(format!(
                "object key contains a backslash, which is a path separator on Windows: {path}"
            )));
        }
        for component in rel.split('/') {
            if component == ".." {
                return Err(StorageError::Backend(format!(
                    "object key escapes the cache directory: {path}"
                )));
            }
        }
        Ok(self.cache_dir.join(rel))
    }

    /// Read `length` bytes at `offset`, serving from the cache when the object
    /// is already local and fetching the whole object when it is not.
    ///
    /// This is the read-through path a segment reader wants: the first range
    /// read of a segment pays one `GetObject` for the whole segment, and every
    /// later range read of it is local. Use
    /// [`get_range_uncached`](Self::get_range_uncached) when the object will not
    /// be read again and the transfer, not the round-trip, is the cost.
    pub async fn get_range(&self, path: &str, offset: u64, length: u64) -> Result<Bytes> {
        let local_path = self.cache_path(path)?;

        if local_path.exists() {
            let bytes = read_local_range(&local_path, offset, length).await?;
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            self.counters
                .bytes_served_local
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            debug!(?local_path, offset, bytes = bytes.len(), "range cache hit");
            return Ok(bytes);
        }

        // Miss: populate the cache with the whole object, then slice it.
        let whole = self.get(path).await?;
        Ok(slice_range(&whole, offset, length))
    }

    /// Fetch one byte range straight from the backend, caching nothing.
    ///
    /// For a cold probe of a large object — a footer, a header, one skip-list
    /// block — this transfers kilobytes where [`get_range`](Self::get_range)
    /// would transfer the whole object. It costs the same single billed request
    /// either way, so the saving is bandwidth and latency, not operations.
    /// Counted as a miss, because it is one.
    pub async fn get_range_uncached(&self, path: &str, offset: u64, length: u64) -> Result<Bytes> {
        let bytes = self.backend.read_range(path, offset, length).await?;
        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_fetched
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Return the data for `path`.
    ///
    /// Checks the local cache first.  On a miss, fetches from the backend,
    /// writes the result to the cache, and returns the data.
    pub async fn get(&self, path: &str) -> Result<Bytes> {
        let local_path = self.cache_path(path)?;

        if local_path.exists() {
            debug!(?local_path, "cache hit");
            let data = tokio::fs::read(&local_path).await?;
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            self.counters
                .bytes_served_local
                .fetch_add(data.len() as u64, Ordering::Relaxed);
            return Ok(Bytes::from(data));
        }

        // Cache miss — fetch from backend. One billed GetObject.
        debug!(?local_path, path, "cache miss, fetching from backend");
        let data = self.backend.read_range(path, 0, u64::MAX).await?;
        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_fetched
            .fetch_add(data.len() as u64, Ordering::Relaxed);

        // Cache locally. Best-effort: a cache that cannot be written is a
        // performance problem, not a correctness one, so the caller still gets
        // its bytes — but the failure is counted, because a cache silently
        // missing every time looks exactly like a cold cache.
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        if let Err(e) = write_atomically(&local_path, &data).await {
            warn!(?local_path, error = %e, "failed to cache segment locally");
            self.counters
                .cache_write_failures
                .fetch_add(1, Ordering::Relaxed);
        } else {
            debug!(?local_path, bytes = data.len(), "segment cached");
        }

        Ok(data)
    }

    /// Remove the cached copy of `path`, if present.
    pub async fn invalidate(&self, path: &str) -> Result<()> {
        let local_path = self.cache_path(path)?;
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

/// Write `data` to `path` via a temp file + rename.
///
/// A plain `fs::write` leaves a half-written cache entry behind if the process
/// dies mid-write, and the next read finds a file that `exists()` and is
/// corrupt — a cache that serves short segments is worse than no cache. The
/// temp name carries the process id so two processes sharing a cache directory
/// cannot collide on it.
async fn write_atomically(path: &PathBuf, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    tokio::fs::write(&tmp, data).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

/// Read `length` bytes at `offset` from a local file, `u64::MAX` meaning
/// "to the end".
async fn read_local_range(path: &PathBuf, offset: u64, length: u64) -> Result<Bytes> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    if length == 0 {
        return Ok(Bytes::new());
    }
    let mut file = tokio::fs::File::open(path).await?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset)).await?;
    }
    if length == u64::MAX {
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        Ok(Bytes::from(buf))
    } else {
        let mut buf = vec![0u8; length as usize];
        file.read_exact(&mut buf).await?;
        Ok(Bytes::from(buf))
    }
}

/// Slice `[offset, offset+length)` out of an already-fetched object.
fn slice_range(whole: &Bytes, offset: u64, length: u64) -> Bytes {
    let start = (offset as usize).min(whole.len());
    let end = if length == u64::MAX {
        whole.len()
    } else {
        start.saturating_add(length as usize).min(whole.len())
    };
    whole.slice(start..end)
}

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
    use crate::backend::{LocalFsBackend, SimulatedObjectStore};

    #[tokio::test]
    async fn cache_miss_fetches_from_backend() {
        let backend_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(backend_dir.path()).unwrap());
        backend
            .write("segments/seg-001.seg", b"segment data here")
            .await
            .unwrap();

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

        let cache = SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

        // Populate cache.
        cache.get("seg.seg").await.unwrap();

        // Modify the backend — cache should still serve the old version.
        backend.write("seg.seg", b"modified").await.unwrap();

        let data = cache.get("seg.seg").await.unwrap();
        assert_eq!(
            &data[..],
            b"original",
            "cache should serve stale-but-local copy"
        );
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
        assert!(
            size_after <= 15,
            "expected <= 15 bytes after eviction, got {size_after}"
        );
    }

    #[tokio::test]
    async fn invalidate_removes_cached_file() {
        let backend_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        let backend: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(backend_dir.path()).unwrap());
        backend.write("inv.seg", b"data").await.unwrap();

        let cache = SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

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

        let backend: Arc<dyn StorageBackend> = Arc::new(SimulatedObjectStore::new(
            s3_dir.path(),
            "test-bucket",
            "xerj/",
        ));
        backend
            .write("segments/s3-seg.seg", b"s3 segment bytes")
            .await
            .unwrap();

        let cache = SegmentCache::new(cache_dir.path(), 100 * 1024 * 1024, Arc::clone(&backend));

        // Cache miss path through simulated S3.
        let data = cache.get("segments/s3-seg.seg").await.unwrap();
        assert_eq!(&data[..], b"s3 segment bytes");

        // Second call should be served from cache.
        let data2 = cache.get("segments/s3-seg.seg").await.unwrap();
        assert_eq!(&data2[..], b"s3 segment bytes");
    }
}
