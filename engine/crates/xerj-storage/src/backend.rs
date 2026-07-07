//! Pluggable storage backends.
//!
//! Every storage operation goes through [`StorageBackend`].  The local
//! filesystem implementation is production-ready; the S3 stub documents the
//! interface needed for range-read support and is ready for wiring up to the
//! AWS SDK.

use async_trait::async_trait;
use bytes::Bytes;
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{debug, instrument};

use crate::{Result, StorageError};

// ── FileMetadata ─────────────────────────────────────────────────────────────

/// Lightweight metadata about a stored object.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub size: u64,
    pub modified: SystemTime,
    pub created: Option<SystemTime>,
}

// ── StorageBackend trait ─────────────────────────────────────────────────────

/// Abstraction over any byte-addressable storage medium.
///
/// All methods are async so they compose naturally with Tokio.  Implementations
/// **must** be `Send + Sync` so they can be shared across threads behind an
/// `Arc`.
///
/// ## Range reads
///
/// [`read_range`] is the hot path for segment access.  Local-FS reads use
/// `seek + read_exact`; S3 reads use HTTP Range headers so only the needed
/// bytes cross the network.
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    /// Read `length` bytes from `path` starting at `offset`.
    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Bytes>;

    /// Atomically write `data` to `path` (tmp-file + rename).
    async fn write(&self, path: &str, data: &[u8]) -> Result<()>;

    /// Delete the file at `path`.  Not an error if the file does not exist.
    async fn delete(&self, path: &str) -> Result<()>;

    /// Return `true` if `path` exists.
    async fn exists(&self, path: &str) -> Result<bool>;

    /// List all paths with the given `prefix`.
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Return metadata for `path`.
    async fn metadata(&self, path: &str) -> Result<FileMetadata>;
}

// ── LocalFsBackend ───────────────────────────────────────────────────────────

/// Production-ready local-filesystem backend.
///
/// Writes are atomic: data is written to a `.tmp` file in the same directory,
/// then `rename`d into place so a crash never leaves a partial file.
#[derive(Debug, Clone)]
pub struct LocalFsBackend {
    root: PathBuf,
}

impl LocalFsBackend {
    /// Create a backend rooted at `root`.  The directory is created if absent.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn abs(&self, path: &str) -> PathBuf {
        // Strip any leading slash so join works correctly
        self.root.join(path.trim_start_matches('/'))
    }
}

#[async_trait]
impl StorageBackend for LocalFsBackend {
    #[instrument(skip(self), fields(path, offset, length))]
    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Bytes> {
        let abs = self.abs(path);
        debug!(?abs, offset, length, "read_range");
        let mut file = tokio::fs::File::open(&abs).await?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }
        if length == u64::MAX {
            // Read to end of file.
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut file, &mut buf).await?;
            Ok(Bytes::from(buf))
        } else {
            let mut buf = vec![0u8; length as usize];
            file.read_exact(&mut buf).await?;
            Ok(Bytes::from(buf))
        }
    }

    #[instrument(skip(self, data), fields(path, bytes = data.len()))]
    async fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let abs = self.abs(path);
        // Ensure parent directory exists
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = abs.with_extension("tmp");
        {
            let mut file = tokio::fs::File::create(&tmp).await?;
            file.write_all(data).await?;
            file.flush().await?;
            file.sync_all().await?;
        }
        tokio::fs::rename(&tmp, &abs).await?;
        debug!(?abs, bytes = data.len(), "write complete");
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let abs = self.abs(path);
        match tokio::fs::remove_file(&abs).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn exists(&self, path: &str) -> Result<bool> {
        let abs = self.abs(path);
        Ok(tokio::fs::try_exists(&abs).await?)
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let abs = self.abs(prefix);
        // Walk the directory; return paths relative to self.root
        let root = self.root.clone();
        let entries = tokio::task::spawn_blocking(move || {
            let search_dir = if abs.is_dir() {
                abs
            } else {
                abs.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| root.clone())
            };
            let mut results = Vec::new();
            if search_dir.exists() {
                for entry in walkdir::WalkDir::new(&search_dir)
                    .min_depth(1)
                    .into_iter()
                    .flatten()
                {
                    if entry.file_type().is_file() {
                        if let Ok(rel) = entry.path().strip_prefix(&root) {
                            results.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            results
        })
        .await
        .map_err(|e| StorageError::Backend(e.to_string()))?;

        Ok(entries)
    }

    async fn metadata(&self, path: &str) -> Result<FileMetadata> {
        let abs = self.abs(path);
        let meta = tokio::fs::metadata(&abs).await?;
        Ok(FileMetadata {
            size: meta.len(),
            modified: meta.modified()?,
            created: meta.created().ok(),
        })
    }
}

// ── S3Backend ────────────────────────────────────────────────────────────────

/// S3-backed storage with HTTP Range read support.
///
/// In this implementation the backend is simulated with a local filesystem so
/// it is fully testable without AWS credentials.  The public API matches what a
/// real `aws-sdk-s3` integration would expose, so swapping the implementation
/// later requires only changing the method bodies.
///
/// # Production wiring
///
/// ```rust,ignore
/// use aws_sdk_s3::Client;
/// use aws_config::BehaviorVersion;
///
/// let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
/// let client = Client::new(&config);
/// // Replace S3Backend::new(base_dir) with a real S3 client variant.
/// ```
///
/// Range reads map directly to `GetObject` with `Range: bytes=<offset>-<end>`.
/// Writes use `PutObject` (atomic at the object level on S3).
/// List uses `ListObjectsV2` with `prefix`.
#[derive(Debug, Clone)]
pub struct S3Backend {
    /// Simulated bucket: local directory that mirrors the S3 key hierarchy.
    /// In production replace with `aws_sdk_s3::Client` + bucket/prefix fields.
    base_dir: PathBuf,
    /// Virtual bucket name (informational; used for logging / future real S3).
    pub bucket: String,
    /// Key prefix prepended to every object path.
    pub prefix: String,
}

impl S3Backend {
    /// Create a new S3 backend backed by a local directory simulation.
    ///
    /// `base_dir` acts as the S3 bucket root.  The directory is created if it
    /// does not already exist.  `bucket` and `prefix` are stored for future use
    /// when the real AWS SDK is wired in.
    pub fn new(
        base_dir: impl Into<PathBuf>,
        bucket: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Self {
        let dir = base_dir.into();
        std::fs::create_dir_all(&dir).ok();
        Self {
            base_dir: dir,
            bucket: bucket.into(),
            prefix: prefix.into(),
        }
    }

    /// Build the full local path for a given object `path`, applying the
    /// configured prefix so key layout mirrors what a real S3 bucket would have.
    fn abs(&self, path: &str) -> PathBuf {
        let key = self.object_key(path);
        self.base_dir.join(key.trim_start_matches('/'))
    }

    /// Prepend `self.prefix` to `path` to form the full S3 object key.
    pub fn object_key(&self, path: &str) -> String {
        if self.prefix.is_empty() {
            path.trim_start_matches('/').to_owned()
        } else {
            format!(
                "{}/{}",
                self.prefix.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        }
    }
}

#[async_trait]
impl StorageBackend for S3Backend {
    /// Read `length` bytes from `path` starting at `offset`.
    ///
    /// When `length == u64::MAX` the entire file is returned (equivalent to
    /// an S3 `GetObject` without a Range header).
    #[instrument(skip(self), fields(path, offset, length))]
    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Bytes> {
        let abs = self.abs(path);
        debug!(?abs, offset, length, "s3_sim read_range");
        let mut file = tokio::fs::File::open(&abs).await?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }
        if length == u64::MAX {
            // Read to end of file — mirrors S3 GetObject without Range header.
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut file, &mut buf).await?;
            Ok(Bytes::from(buf))
        } else {
            let mut buf = vec![0u8; length as usize];
            file.read_exact(&mut buf).await?;
            Ok(Bytes::from(buf))
        }
    }

    /// Write `data` to `path`.
    ///
    /// Uses a tmp-file + rename for atomicity, mirroring S3's atomic PutObject
    /// semantics (S3 PUT is atomic at the object level).
    #[instrument(skip(self, data), fields(path, bytes = data.len()))]
    async fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let abs = self.abs(path);
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = abs.with_extension("s3tmp");
        {
            let mut file = tokio::fs::File::create(&tmp).await?;
            file.write_all(data).await?;
            file.flush().await?;
            file.sync_all().await?;
        }
        tokio::fs::rename(&tmp, &abs).await?;
        debug!(?abs, bytes = data.len(), "s3_sim write complete");
        Ok(())
    }

    /// Delete the object at `path`.  Not an error if the object does not exist.
    async fn delete(&self, path: &str) -> Result<()> {
        let abs = self.abs(path);
        match tokio::fs::remove_file(&abs).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Return `true` if the object at `path` exists.
    async fn exists(&self, path: &str) -> Result<bool> {
        let abs = self.abs(path);
        Ok(tokio::fs::try_exists(&abs).await?)
    }

    /// List all object keys that share the given `prefix`.
    ///
    /// Returns paths relative to `self.base_dir`, matching the format that
    /// `read_range`/`write`/`delete` accept.
    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let abs = self.abs(prefix);
        let base = self.base_dir.clone();
        let entries = tokio::task::spawn_blocking(move || {
            let search_dir = if abs.is_dir() {
                abs
            } else {
                abs.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| base.clone())
            };
            let mut results = Vec::new();
            if search_dir.exists() {
                for entry in walkdir::WalkDir::new(&search_dir)
                    .min_depth(1)
                    .into_iter()
                    .flatten()
                {
                    if entry.file_type().is_file() {
                        if let Ok(rel) = entry.path().strip_prefix(&base) {
                            results.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            results
        })
        .await
        .map_err(|e| StorageError::Backend(e.to_string()))?;

        Ok(entries)
    }

    /// Return metadata for the object at `path`.
    async fn metadata(&self, path: &str) -> Result<FileMetadata> {
        let abs = self.abs(path);
        let meta = tokio::fs::metadata(&abs).await?;
        Ok(FileMetadata {
            size: meta.len(),
            modified: meta.modified()?,
            created: meta.created().ok(),
        })
    }
}

// ── walkdir (needed by list()) ────────────────────────────────────────────────
// We declare walkdir as an inline dependency; add it to Cargo.toml if not present.
// For now provide a fallback that uses std::fs::read_dir recursively.
mod walkdir {
    use std::path::{Path, PathBuf};

    pub struct WalkDir {
        root: PathBuf,
        min_depth: usize,
    }

    pub struct Entry {
        path: PathBuf,
        file_type: std::fs::FileType,
        // Recorded during the walk but not currently read by any consumer;
        // kept to mirror the real walkdir::DirEntry shape.
        #[allow(dead_code)]
        depth: usize,
    }

    impl Entry {
        pub fn path(&self) -> &Path {
            &self.path
        }
        pub fn file_type(&self) -> &std::fs::FileType {
            &self.file_type
        }
    }

    impl WalkDir {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self {
                root: root.into(),
                min_depth: 0,
            }
        }
        pub fn min_depth(mut self, d: usize) -> Self {
            self.min_depth = d;
            self
        }
    }

    impl IntoIterator for WalkDir {
        type Item = Result<Entry, std::io::Error>;
        type IntoIter = Box<dyn Iterator<Item = Self::Item>>;

        fn into_iter(self) -> Self::IntoIter {
            let mut entries = Vec::new();
            collect(&self.root, &self.root, self.min_depth, 0, &mut entries);
            Box::new(entries.into_iter())
        }
    }

    // `root` is threaded through the recursion to mirror walkdir's API but is
    // only forwarded to nested calls, never read directly in this body.
    #[allow(clippy::only_used_in_recursion)]
    fn collect(
        root: &Path,
        dir: &Path,
        min_depth: usize,
        depth: usize,
        out: &mut Vec<Result<Entry, std::io::Error>>,
    ) {
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                out.push(Err(e));
                return;
            }
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(e) => {
                    out.push(Err(e));
                    continue;
                }
            };
            if depth + 1 >= min_depth {
                out.push(Ok(Entry {
                    path: path.clone(),
                    file_type: ft,
                    depth: depth + 1,
                }));
            }
            if ft.is_dir() {
                collect(root, &path, min_depth, depth + 1, out);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_fs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalFsBackend::new(dir.path()).unwrap();

        let data = b"hello xerj storage";
        backend.write("test/hello.bin", data).await.unwrap();

        assert!(backend.exists("test/hello.bin").await.unwrap());

        let got = backend.read_range("test/hello.bin", 6, 4).await.unwrap();
        assert_eq!(&got[..], b"xerj");

        let meta = backend.metadata("test/hello.bin").await.unwrap();
        assert_eq!(meta.size, data.len() as u64);

        backend.delete("test/hello.bin").await.unwrap();
        assert!(!backend.exists("test/hello.bin").await.unwrap());
    }

    #[tokio::test]
    async fn local_fs_list() {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalFsBackend::new(dir.path()).unwrap();

        backend.write("idx/a.seg", b"A").await.unwrap();
        backend.write("idx/b.seg", b"B").await.unwrap();

        let mut paths = backend.list("idx/").await.unwrap();
        paths.sort();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].contains("a.seg"));
        assert!(paths[1].contains("b.seg"));
    }

    #[tokio::test]
    async fn delete_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalFsBackend::new(dir.path()).unwrap();
        backend.delete("no/such/file.seg").await.unwrap();
    }

    // ── S3Backend (simulated) tests ───────────────────────────────────────────

    #[tokio::test]
    async fn s3_backend_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = S3Backend::new(dir.path(), "test-bucket", "xerj/");

        let data = b"hello s3 simulated storage";
        backend.write("segments/seg-001.seg", data).await.unwrap();

        assert!(backend.exists("segments/seg-001.seg").await.unwrap());

        // Range read: "s3 si" starting at offset 6
        let got = backend
            .read_range("segments/seg-001.seg", 6, 5)
            .await
            .unwrap();
        assert_eq!(&got[..], b"s3 si");

        let meta = backend.metadata("segments/seg-001.seg").await.unwrap();
        assert_eq!(meta.size, data.len() as u64);

        backend.delete("segments/seg-001.seg").await.unwrap();
        assert!(!backend.exists("segments/seg-001.seg").await.unwrap());
    }

    #[tokio::test]
    async fn s3_backend_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let backend = S3Backend::new(dir.path(), "test-bucket", "");

        let data = b"full read test data";
        backend.write("full.bin", data).await.unwrap();

        // read_range with u64::MAX reads the whole file
        let got = backend.read_range("full.bin", 0, u64::MAX).await.unwrap();
        assert_eq!(&got[..], data);
    }

    #[tokio::test]
    async fn s3_backend_list() {
        let dir = tempfile::tempdir().unwrap();
        let backend = S3Backend::new(dir.path(), "test-bucket", "xerj/");

        backend.write("segments/a.seg", b"A").await.unwrap();
        backend.write("segments/b.seg", b"B").await.unwrap();

        let mut paths = backend.list("segments/").await.unwrap();
        paths.sort();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].contains("a.seg"), "got: {:?}", paths);
        assert!(paths[1].contains("b.seg"), "got: {:?}", paths);
    }

    #[tokio::test]
    async fn s3_backend_delete_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let backend = S3Backend::new(dir.path(), "test-bucket", "xerj/");
        backend.delete("no/such/object.seg").await.unwrap();
    }

    #[tokio::test]
    async fn s3_backend_object_key_prefix() {
        let backend = S3Backend::new("/tmp", "my-bucket", "xerj/v1");
        assert_eq!(
            backend.object_key("segments/foo.seg"),
            "xerj/v1/segments/foo.seg"
        );
        // No double-slash
        assert!(!backend.object_key("/segments/foo.seg").contains("//"));
    }

    #[tokio::test]
    async fn s3_backend_empty_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let backend = S3Backend::new(dir.path(), "test-bucket", "");

        backend.write("plain/key.seg", b"data").await.unwrap();
        assert!(backend.exists("plain/key.seg").await.unwrap());

        let key = backend.object_key("plain/key.seg");
        assert_eq!(key, "plain/key.seg");
    }
}
