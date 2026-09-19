//! Pluggable storage backends.
//!
//! Every storage operation goes through [`StorageBackend`]. Two
//! implementations live here:
//!
//! - [`LocalFsBackend`] — the production local-filesystem backend.
//! - [`SimulatedObjectStore`] — a **test double**. It mirrors an object
//!   store's key hierarchy onto a local directory so the object-store code
//!   paths can be exercised in unit tests with no network and no credentials.
//!   It is not, and never was, a real object store.
//!
//! The real S3-compatible backend (Cloudflare R2, MinIO, AWS S3) lives in
//! [`crate::s3`] and talks to the service over HTTP via `aws-sdk-s3`.
//!
//! ## Operation accounting
//!
//! Object stores bill per request, and the tightest budget XERJ is expected
//! to run inside — Cloudflare R2's free tier — allows 1,000,000 Class A and
//! 10,000,000 Class B operations *per account, per month*. A backend that
//! cannot say how many requests it made cannot be operated safely inside that
//! allowance, so [`ObjectStoreOps`] counts every billed attempt and
//! [`OpBudget`] can refuse to make more. See `docs/OBJECT_STORAGE.md` for the
//! arithmetic.

use async_trait::async_trait;
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Entity tag, when the backend has one. Object stores return an ETag from
    /// `HeadObject`; local filesystems have no equivalent, so the local
    /// backends leave this `None`. Never parse it — S3 only guarantees an
    /// opaque, comparable string (it is an MD5 for single-part uploads and
    /// something else entirely for multipart ones).
    pub etag: Option<String>,
}

// ── Billing-relevant operation accounting ────────────────────────────────────

/// Which billing class an object-store request falls into.
///
/// The class names are Cloudflare R2's, because R2's free tier is the tightest
/// budget XERJ is expected to run inside. AWS S3 and MinIO price differently
/// (S3 charges per 1,000 requests; MinIO charges nothing) but the shape is the
/// same everywhere: mutating and listing calls cost several times a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpClass {
    /// `PutObject`, `ListObjectsV2`, `CopyObject`, `CreateMultipartUpload`,
    /// `UploadPart`, `CompleteMultipartUpload`. The scarce one: 1,000,000 per
    /// month on R2's free tier, which is ~23 per minute for a whole account.
    ClassA,
    /// `GetObject`, `HeadObject`. 10,000,000 per month on R2's free tier.
    ClassB,
    /// `DeleteObject`, `AbortMultipartUpload` — free on R2.
    Free,
}

/// Live counters for the requests a backend has actually sent.
///
/// Counted per **billed attempt**, not per logical call: a `PutObject` that
/// fails once and succeeds on retry is billed twice by the provider and
/// counted twice here. That is why [`crate::s3::S3Backend`] disables the AWS
/// SDK's own retry layer and retries in its own code — an SDK-internal retry
/// is invisible to a counter wrapped around the call, and an undercount is
/// exactly the kind of number that produces a surprise bill.
#[derive(Debug, Default)]
pub struct ObjectStoreOps {
    class_a: AtomicU64,
    class_b: AtomicU64,
    free: AtomicU64,
    retried: AtomicU64,
    refused: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
}

/// A point-in-time copy of [`ObjectStoreOps`], safe to log or serialise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ObjectStoreOpsSnapshot {
    /// Billed Class A attempts (put / list / copy / multipart).
    pub class_a: u64,
    /// Billed Class B attempts (get / head).
    pub class_b: u64,
    /// Free attempts (delete / abort-multipart).
    pub free: u64,
    /// Attempts that were retries of an earlier failed attempt. Already
    /// included in the class counters above — this is how many of them were
    /// paid for twice.
    pub retried: u64,
    /// Requests the [`OpBudget`] refused to send.
    pub refused: u64,
    /// Object bytes actually transferred in (response bodies).
    pub bytes_read: u64,
    /// Object bytes actually transferred out (request bodies).
    pub bytes_written: u64,
}

impl ObjectStoreOps {
    /// Record one billed attempt of `class`, or refuse it when `budget` is
    /// already spent.
    ///
    /// The check is "increment, then compare, then roll back if over", so two
    /// threads racing on the last unit of budget can both be allowed through.
    /// That is deliberate: this is a circuit breaker, not a ledger, and a
    /// mutex on the hot path would cost more than the one extra request.
    pub fn charge(&self, class: OpClass, budget: &OpBudget) -> Result<()> {
        let (counter, cap, label) = match class {
            OpClass::ClassA => (&self.class_a, budget.max_class_a, "Class A"),
            OpClass::ClassB => (&self.class_b, budget.max_class_b, "Class B"),
            OpClass::Free => (&self.free, None, "free"),
        };
        let used = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(cap) = cap {
            if used > cap {
                counter.fetch_sub(1, Ordering::Relaxed);
                self.refused.fetch_add(1, Ordering::Relaxed);
                return Err(StorageError::Backend(format!(
                    "object-store {label} operation budget exhausted: {cap} allowed, \
                     {cap} already sent by this process. Raise the budget or reduce the \
                     request rate; see docs/OBJECT_STORAGE.md for the free-tier arithmetic"
                )));
            }
        }
        Ok(())
    }

    /// Note that the attempt about to be charged is a retry of a failed one.
    pub fn note_retry(&self) {
        self.retried.fetch_add(1, Ordering::Relaxed);
    }

    /// Add to the inbound object-byte total.
    pub fn add_bytes_read(&self, n: u64) {
        self.bytes_read.fetch_add(n, Ordering::Relaxed);
    }

    /// Add to the outbound object-byte total.
    pub fn add_bytes_written(&self, n: u64) {
        self.bytes_written.fetch_add(n, Ordering::Relaxed);
    }

    /// Copy the current counters.
    pub fn snapshot(&self) -> ObjectStoreOpsSnapshot {
        ObjectStoreOpsSnapshot {
            class_a: self.class_a.load(Ordering::Relaxed),
            class_b: self.class_b.load(Ordering::Relaxed),
            free: self.free.load(Ordering::Relaxed),
            retried: self.retried.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
        }
    }
}

/// A ceiling on how many billed requests a backend may send.
///
/// **What this is not:** a monthly quota. The counters live in one process and
/// reset when it restarts, and they know nothing about the other clients on
/// the same bucket. It is a circuit breaker that stops one runaway loop — a
/// poller wedged at 5 ms, a retry storm — from spending a month's allowance in
/// an afternoon. Staying inside a monthly free tier is an operator's
/// arithmetic (see `docs/OBJECT_STORAGE.md`); this only bounds the blast
/// radius of a bug.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpBudget {
    /// Maximum Class A attempts for this process's lifetime. `None` = no cap.
    pub max_class_a: Option<u64>,
    /// Maximum Class B attempts for this process's lifetime. `None` = no cap.
    pub max_class_b: Option<u64>,
}

impl OpBudget {
    /// No ceiling. The default, because a ceiling that fires in the middle of
    /// a legitimate bulk ingest is worse than no ceiling at all — the operator
    /// has to choose the number that matches their own traffic.
    pub const fn unlimited() -> Self {
        Self {
            max_class_a: None,
            max_class_b: None,
        }
    }

    /// A ceiling of `class_a` Class A and `class_b` Class B attempts.
    pub const fn new(class_a: u64, class_b: u64) -> Self {
        Self {
            max_class_a: Some(class_a),
            max_class_b: Some(class_b),
        }
    }

    /// A fifth of Cloudflare R2's monthly free allowance: 200,000 Class A and
    /// 2,000,000 Class B.
    ///
    /// The arithmetic: R2's free tier is 1,000,000 Class A and 10,000,000
    /// Class B per month per *account*, and an account normally serves more
    /// than one process — other buckets, release downloads, other nodes. A
    /// fifth leaves four fifths for everything else, which is the point: a
    /// ceiling sized to the whole tier would let one process spend the account
    /// dry and still call itself compliant.
    ///
    /// **Read this before using it.** The counters are process-lifetime, not
    /// monthly, so this bound only approximates a monthly one for a process
    /// that lives about a month — a long-running server. For a CLI run that
    /// exits in a minute it will never fire, and for a server restarted daily
    /// it permits thirty times as much as its name suggests. It is a sane
    /// default for an operator-facing feature that would otherwise have no
    /// ceiling at all, not an accountant. The monthly arithmetic an operator
    /// actually needs is the table in `docs/OBJECT_STORAGE.md`.
    pub const fn r2_free_tier_slice() -> Self {
        Self::new(200_000, 2_000_000)
    }
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

    /// Billed-request counters, for backends that send billed requests.
    ///
    /// Local-filesystem backends return `None` — there is nothing to bill.
    /// Operators reading this for a remote backend get the number that
    /// matters: how many Class A operations this process has actually spent.
    fn ops(&self) -> Option<ObjectStoreOpsSnapshot> {
        None
    }
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
            etag: None,
        })
    }
}

// ── SimulatedObjectStore (test double) ───────────────────────────────────────

/// **Test double.** An object store simulated on the local filesystem.
///
/// This is not a backend to point production data at, and it is not the S3
/// client — that is [`crate::s3::S3Backend`]. What it is: a stand-in that
/// mirrors an object store's flat key hierarchy onto a local directory, so
/// every code path that talks to [`StorageBackend`] can be unit tested with no
/// network, no credentials, no container and no cost. It was itself called
/// `S3Backend` until the real client existed, which is the whole reason for
/// the rename.
///
/// It reproduces the parts of the object-store contract the engine relies on:
///
/// - keys are flat strings with the configured prefix prepended
///   ([`object_key`](Self::object_key));
/// - [`list`](StorageBackend::list) returns keys **without** the configured
///   prefix, so its output feeds straight back into `read_range`;
/// - writes are whole-object and atomic (tmp file + rename, matching S3's
///   atomic `PutObject`);
/// - `read_range` serves a byte range, as a ranged `GetObject` would.
///
/// It deliberately does **not** reproduce: request billing, eventual
/// consistency, network errors, throttling, multipart uploads, or the
/// 1,000-key page limit on `ListObjectsV2`. A test that needs any of those
/// needs a real endpoint — MinIO locally, R2 by hand.
#[derive(Debug, Clone)]
pub struct SimulatedObjectStore {
    /// Stand-in bucket: local directory mirroring the object key hierarchy.
    base_dir: PathBuf,
    /// Bucket name. Informational only — it appears in logs, nothing else.
    pub bucket: String,
    /// Key prefix prepended to every object path.
    pub prefix: String,
}

impl SimulatedObjectStore {
    /// Create a simulated object store rooted at `base_dir`.
    ///
    /// `base_dir` plays the part of the bucket root and is created if absent.
    /// `bucket` is informational; `prefix` is applied to every key exactly as
    /// the real backend applies it.
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

    /// The configured prefix in the form it appears at the head of a key
    /// (`"xerj/"`), or `None` when no prefix is configured.
    fn key_prefix(&self) -> Option<String> {
        if self.prefix.is_empty() {
            None
        } else {
            Some(format!("{}/", self.prefix.trim_end_matches('/')))
        }
    }

    /// Prepend `self.prefix` to `path` to form the full object key.
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
impl StorageBackend for SimulatedObjectStore {
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
    /// Returns keys with the configured object prefix **removed**, i.e. in
    /// exactly the form `read_range` / `write` / `delete` accept. Before this
    /// was fixed the simulation returned prefixed keys, so feeding `list`
    /// output back into `read_range` double-applied the prefix and went looking
    /// for `xerj/xerj/segments/...`. The real backend has the same contract and
    /// a shared test asserts the round-trip on both.
    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let abs = self.abs(prefix);
        let base = self.base_dir.clone();
        let strip = self.key_prefix();
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
                            let key = rel.to_string_lossy().into_owned();
                            match strip.as_deref() {
                                Some(p) => {
                                    if let Some(k) = key.strip_prefix(p) {
                                        results.push(k.to_owned());
                                    }
                                }
                                None => results.push(key),
                            }
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
            etag: None,
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

    // ── SimulatedObjectStore (test-double) tests ──────────────────────────────

    #[tokio::test]
    async fn simulated_object_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SimulatedObjectStore::new(dir.path(), "test-bucket", "xerj/");

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
    async fn simulated_object_store_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SimulatedObjectStore::new(dir.path(), "test-bucket", "");

        let data = b"full read test data";
        backend.write("full.bin", data).await.unwrap();

        // read_range with u64::MAX reads the whole file
        let got = backend.read_range("full.bin", 0, u64::MAX).await.unwrap();
        assert_eq!(&got[..], data);
    }

    #[tokio::test]
    async fn simulated_object_store_list() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SimulatedObjectStore::new(dir.path(), "test-bucket", "xerj/");

        backend.write("segments/a.seg", b"A").await.unwrap();
        backend.write("segments/b.seg", b"B").await.unwrap();

        let mut paths = backend.list("segments/").await.unwrap();
        paths.sort();
        // Keys come back WITHOUT the configured prefix, so list() output is
        // directly usable. The old behaviour returned "xerj/segments/a.seg",
        // which read_range() then resolved as "xerj/xerj/segments/a.seg".
        assert_eq!(paths, vec!["segments/a.seg", "segments/b.seg"]);
        for key in &paths {
            let got = backend
                .read_range(key, 0, u64::MAX)
                .await
                .unwrap_or_else(|e| {
                    panic!("list() output must feed straight into read_range(): {key}: {e}")
                });
            assert_eq!(got.len(), 1, "one byte per object");
        }
    }

    /// The same contract the real backend is held to in
    /// `tests/s3_object_store.rs::list_strips_the_prefix_so_its_output_round_trips`.
    /// Keeping it here too means the test double cannot drift away from the
    /// thing it stands in for without a test noticing.
    #[tokio::test]
    async fn simulated_object_store_list_is_scoped_to_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SimulatedObjectStore::new(dir.path(), "test-bucket", "tenant-a/");

        backend.write("segments/a.seg", b"A").await.unwrap();
        backend.write("other/c.seg", b"C").await.unwrap();

        assert_eq!(
            backend.list("segments/").await.unwrap(),
            vec!["segments/a.seg"]
        );
        let mut all = backend.list("").await.unwrap();
        all.sort();
        assert_eq!(all, vec!["other/c.seg", "segments/a.seg"]);

        // A second tenant sharing the same bucket must see nothing of the first.
        let other = SimulatedObjectStore::new(dir.path(), "test-bucket", "tenant-b/");
        assert!(other.list("").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn simulated_object_store_delete_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SimulatedObjectStore::new(dir.path(), "test-bucket", "xerj/");
        backend.delete("no/such/object.seg").await.unwrap();
    }

    #[tokio::test]
    async fn simulated_object_store_object_key_prefix() {
        let backend = SimulatedObjectStore::new("/tmp", "my-bucket", "xerj/v1");
        assert_eq!(
            backend.object_key("segments/foo.seg"),
            "xerj/v1/segments/foo.seg"
        );
        // No double-slash
        assert!(!backend.object_key("/segments/foo.seg").contains("//"));
    }

    #[tokio::test]
    async fn simulated_object_store_empty_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let backend = SimulatedObjectStore::new(dir.path(), "test-bucket", "");

        backend.write("plain/key.seg", b"data").await.unwrap();
        assert!(backend.exists("plain/key.seg").await.unwrap());

        let key = backend.object_key("plain/key.seg");
        assert_eq!(key, "plain/key.seg");
    }
}
