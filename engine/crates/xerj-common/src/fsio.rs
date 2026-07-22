//! Power-loss-ordered file I/O primitives (RC4 Wave-1 blocker #10).
//!
//! The segment publish chain (`.seg` → `.ids`/`.dv`/FTS side-cars →
//! `snapshot.json` → WAL prune) is only crash-safe if every link is
//! durable **before** the WAL entries it supersedes are destroyed.
//! `fs::write` + `rename` alone leaves both the file bytes and the
//! directory entry in the volatile page cache: a power loss after the
//! WAL was pruned can then GC a fully-flushed segment as an orphan —
//! acked-data loss.
//!
//! Every side-car / manifest write on the publish chain must go through
//! [`write_file_durable`], and every `rename` that publishes a file must
//! be followed by [`fsync_dir`] on the parent (rename durability is a
//! property of the *directory*, not the file).

use std::io::Write as _;
use std::path::Path;

/// fsync a directory so previously-renamed/created entries in it survive
/// power loss. No-op errors are surfaced to the caller; on filesystems
/// where directories cannot be fsynced (rare), callers may choose to
/// ignore the error.
pub fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    let d = std::fs::File::open(dir)?;
    d.sync_all()
}

/// Write `bytes` to `path` atomically **and durably**:
///
/// 1. write to a same-directory temp file,
/// 2. `fsync` the temp file (data + metadata),
/// 3. `rename` over the target,
/// 4. `fsync` the parent directory (makes the rename itself durable).
///
/// A crash or power loss at any point leaves either the old file or the
/// complete new file — never a torn one, and never a "file that
/// evaporates on power loss because only the page cache had it".
///
/// The temp name embeds a uuid + thread id so concurrent writers to the
/// same target never clobber each other's temp file (see the
/// `save_snapshot` race note in `xerj-storage`).
pub fn write_file_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let nonce = format!("{:x}-{:?}", std::process::id(), std::thread::current().id());
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp.{nonce}"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_file_durable_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        write_file_durable(&p, b"hello").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
        // Overwrite is atomic.
        write_file_durable(&p, b"world").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"world");
        // No stray temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn fsync_dir_works() {
        let dir = tempfile::tempdir().unwrap();
        fsync_dir(dir.path()).unwrap();
    }
}
