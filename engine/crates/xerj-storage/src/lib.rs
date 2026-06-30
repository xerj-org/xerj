//! # xerj-storage
//!
//! Storage engine for the xerj search engine.
//!
//! ## Design principles
//!
//! - **2 files per segment** (not ES's 12-16): `.seg` (data) + `.sidx` (skip index).
//! - **One durability system**: Write-Ahead Log (WAL) only — no dual translog/Lucene commit.
//! - **Pluggable backends**: local filesystem, S3 with range reads, in-memory for tests.
//! - **`mmap` for segment reads**: data is served from OS page cache, not app heap.
//! - **`ArcSwap<IndexSnapshot>`**: atomically swap active segment lists without locks.
//! - **`DashMap` version map**: lock-free per-document version tracking.
//!
//! ## Module layout
//!
//! | Module            | Responsibility                                              |
//! |-------------------|-------------------------------------------------------------|
//! | [`backend`]       | [`StorageBackend`] trait + [`LocalFsBackend`] / S3 stub    |
//! | [`wal`]           | Write-Ahead Log — append, sync, replay, generation rotate  |
//! | [`segment`]       | `.seg` / `.sidx` format, [`SegmentWriter`] / [`SegmentReader`] |
//! | [`version_map`]   | Lock-free doc-id → (seq_no, segment_id) map                |
//! | [`index_store`]   | Per-index orchestration: WAL + segments + flush            |
//! | [`merge`]         | Size-tiered merge, tombstone purge, rate-limited I/O       |

pub mod backend;
pub mod cache;
pub mod doc_values;
pub mod index_store;
pub mod merge;
pub mod segment;
pub mod stored_codec;
pub mod version_map;
pub mod wal;

// ── Public re-exports ────────────────────────────────────────────────────────

pub use backend::{FileMetadata, LocalFsBackend, S3Backend, StorageBackend};
pub use cache::SegmentCache;
pub use index_store::{
    DrainedMemtable, FsckReport, FsckSectionReport, FsckSegmentReport, IndexSnapshot, IndexStore,
    IndexStoreConfig, StorageMode,
};
pub use merge::{MergeExecutor, MergePolicy, SizeTieredMergePolicy};
pub use segment::{SectionType, SegmentId, SegmentMeta, SegmentReader, SegmentWriter};
pub use version_map::VersionMap;
pub use wal::{WalEntry, WalReader, WalWriter};

// ── Crate-wide type aliases ──────────────────────────────────────────────────

/// Sequence number — monotonically increasing, globally unique within an index.
pub type SeqNo = u64;

/// Result alias using the storage crate's own error type.
pub type Result<T> = std::result::Result<T, StorageError>;

// ── Error type ───────────────────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Checksum mismatch (expected {expected:#010x}, got {actual:#010x})")]
    ChecksumMismatch { expected: u32, actual: u32 },

    #[error("Invalid magic bytes: expected {expected:?}, got {actual:?}")]
    InvalidMagic { expected: &'static [u8], actual: Vec<u8> },

    #[error("Unsupported format version {0}")]
    UnsupportedVersion(u16),

    #[error("Segment {0} not found")]
    SegmentNotFound(String),

    #[error("WAL is corrupt at offset {0}: {1}")]
    WalCorrupt(u64, String),

    #[error("Version conflict: doc {doc_id} expected seq {expected}, found {actual}")]
    VersionConflict { doc_id: String, expected: SeqNo, actual: SeqNo },

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Merge aborted: {0}")]
    MergeAborted(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
