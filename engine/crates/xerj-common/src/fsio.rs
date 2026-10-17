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
//! [`write_file_durable`] or [`replace_file_durable`], or, for many files in
//! one directory, [`stage_file`] plus one [`publish_staged`]. Unix makes the rename
//! durable with parent-directory fsync; Windows uses a same-directory Win32
//! write-through replacement because it has no equivalent directory-fsync
//! contract.

use std::io::Write as _;
use std::path::Path;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;

/// fsync a directory so previously-renamed/created entries in it survive
/// power loss. No-op errors are surfaced to the caller; on filesystems
/// where directories cannot be fsynced (rare), callers may choose to
/// ignore the error.
#[cfg(not(windows))]
pub fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    let d = std::fs::File::open(dir)?;
    sync_file_full(&d)
}

/// How many full flushes ([`sync_file_full`]) this process has issued.
static FULL_FLUSHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number of full flushes this process has issued through this module.
///
/// A full flush is `File::sync_all`, which on Apple targets is
/// `fcntl(F_FULLFSYNC)`: it drains the whole device write cache and costs
/// milliseconds each. Exposed so tests can bound how many a publish costs.
#[doc(hidden)]
pub fn full_flush_count() -> u64 {
    FULL_FLUSHES.load(std::sync::atomic::Ordering::Relaxed)
}

/// `file.sync_all()`, counted (see [`full_flush_count`]).
pub fn sync_file_full(file: &std::fs::File) -> std::io::Result<()> {
    FULL_FLUSHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    file.sync_all()
}

/// Sync a staged file's bytes ahead of a later device flush.
///
/// On Apple targets this is plain `fsync(2)`: it hands the data to the
/// device but leaves it in the device's write cache, which the one
/// `F_FULLFSYNC` in [`publish_staged`] then drains for every staged file at
/// once. Everywhere else it is the same full `sync_all` a durable write
/// always used, because there `fsync` is already the per-file barrier and
/// there is no cheaper device-wide one.
fn sync_file_for_batch(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `fsync` on a descriptor this `File` owns for the call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        sync_file_full(file)
    }
}

/// A file written to a same-directory temp name and synced, waiting for
/// [`publish_staged`] to move it into place. Dropping it unpublished removes
/// the temp file.
#[derive(Debug)]
pub struct StagedFile {
    tmp: std::path::PathBuf,
    dest: std::path::PathBuf,
    published: bool,
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

fn temp_sibling(path: &Path) -> std::path::PathBuf {
    let nonce = format!("{:x}-{:?}", std::process::id(), std::thread::current().id());
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    path.with_file_name(format!("{file_name}.tmp.{nonce}"))
}

/// Write `bytes` to a temp file beside `path` and sync it, for a later
/// [`publish_staged`]. Nothing is visible at `path` until then.
pub fn stage_file(path: &Path, bytes: &[u8]) -> std::io::Result<StagedFile> {
    let tmp = temp_sibling(path);
    require_same_directory(&tmp, path)?;
    let staged = StagedFile {
        tmp,
        dest: path.to_path_buf(),
        published: false,
    };
    let mut f = std::fs::File::create(&staged.tmp)?;
    f.write_all(bytes)?;
    sync_file_for_batch(&f)?;
    Ok(staged)
}

/// Stage a temp file the caller wrote itself (streamed output). `file` is the
/// open handle to `tmp`, fully written and flushed by the caller; this syncs
/// it like [`stage_file`] does.
pub fn stage_written_file(
    tmp: &Path,
    dest: &Path,
    file: &std::fs::File,
) -> std::io::Result<StagedFile> {
    let staged = StagedFile {
        tmp: tmp.to_path_buf(),
        dest: dest.to_path_buf(),
        published: false,
    };
    require_same_directory(&staged.tmp, &staged.dest)?;
    sync_file_for_batch(file)?;
    Ok(staged)
}

/// Move every staged file in `dir` into place, durably, with the same
/// guarantee as one [`write_file_durable`] per file:
///
/// 1. every staged file's bytes reach stable storage before any rename (on
///    Apple targets, one `F_FULLFSYNC`, which drains the device cache for
///    every earlier `fsync`; elsewhere each file was already fully synced
///    when it was staged);
/// 2. each file replaces its target (rename on Unix, write-through
///    `MoveFileExW` on Windows);
/// 3. one parent-directory fsync makes every rename durable (Unix).
///
/// A crash at any point leaves each target either absent, old or complete,
/// never torn, exactly as per-file durable writes did. What changes is the
/// cost: two device flushes for the batch instead of two per file, which on
/// Apple targets is the difference between ~250 cache drains per segment and
/// two. If a rename fails, the files already moved stay moved and the rest are
/// removed, which is also what a failure part-way through a loop of
/// [`write_file_durable`] calls left behind.
pub fn publish_staged(dir: &Path, files: Vec<StagedFile>) -> std::io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    if let Some(stray) = files.iter().find(|f| f.dest.parent() != Some(dir)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("staged file {:?} is not in {:?}", stray.dest, dir),
        ));
    }
    #[cfg(target_vendor = "apple")]
    sync_file_full(&std::fs::File::open(dir)?)?;
    for mut file in files {
        replace_file_for_durable_write(&file.tmp, &file.dest)?;
        file.published = true;
    }
    #[cfg(not(windows))]
    fsync_dir(dir)?;
    Ok(())
}

/// Windows has no supported directory-flush primitive to call here:
/// `File::open` on a
/// directory fails with `ERROR_ACCESS_DENIED` (os error 5) because std
/// cannot pass `FILE_FLAG_BACKUP_SEMANTICS`, and `FlushFileBuffers` is
/// not defined for directory handles. This compatibility shim makes no
/// durability claim. Code whose correctness requires a durable namespace
/// replacement must use [`write_file_durable`] or [`replace_file_durable`],
/// whose Windows implementation uses `MoveFileExW` with write-through.
///
/// This is not a cosmetic cfg: the Unix body returned `Err` for *every*
/// call on Windows, and `save_snapshot` propagates that error, so the
/// server could not create an index — it aborted at boot with
/// `create .xerj_users: I/O error: Access is denied. (os error 5)`.
#[cfg(windows)]
pub fn fsync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_file_for_durable_write(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

fn require_same_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    if source.parent() == destination.parent() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "durable file replacement requires source and destination in the same directory",
        ))
    }
}

/// Atomically replace one same-directory file and wait for the move to reach
/// disk on Windows.
///
/// Windows cannot use the Unix rename + parent-directory-fsync sequence:
/// `std::fs::File` cannot open a directory with `FILE_FLAG_BACKUP_SEMANTICS`,
/// and `FlushFileBuffers` does not define a directory-handle contract. Win32's
/// `MoveFileExW`, however, defines `MOVEFILE_WRITE_THROUGH` to wait until the
/// move is on disk. Callers create the temporary file beside `destination`, so
/// this remains a same-volume namespace replacement rather than the API's
/// optional copy/delete fallback.
#[cfg(windows)]
fn replace_file_for_durable_write(source: &Path, destination: &Path) -> std::io::Result<()> {
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
        const BACKSLASH: u16 = b'\\' as u16;
        const QUESTION: u16 = b'?' as u16;
        const UPPER_U: u16 = b'U' as u16;
        const UPPER_N: u16 = b'N' as u16;
        const UPPER_C: u16 = b'C' as u16;

        let absolute = std::path::absolute(path)?;
        let original: Vec<u16> = absolute.as_os_str().encode_wide().collect();
        if original.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path contains an embedded NUL",
            ));
        }

        // MoveFileExW does not get std's automatic long-path conversion.
        // Supply an absolute verbatim path so the durable replacement keeps
        // working beyond MAX_PATH. Preserve an existing verbatim prefix and
        // translate ordinary UNC paths to `\\?\UNC\...`.
        let mut wide = if original.starts_with(&[BACKSLASH, BACKSLASH, QUESTION, BACKSLASH]) {
            original
        } else if original.starts_with(&[BACKSLASH, BACKSLASH]) {
            let mut verbatim = vec![
                BACKSLASH, BACKSLASH, QUESTION, BACKSLASH, UPPER_U, UPPER_N, UPPER_C, BACKSLASH,
            ];
            verbatim.extend_from_slice(&original[2..]);
            verbatim
        } else {
            let mut verbatim = vec![BACKSLASH, BACKSLASH, QUESTION, BACKSLASH];
            verbatim.extend_from_slice(&original);
            verbatim
        };
        wide.push(0);
        Ok(wide)
    }

    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    // SAFETY: both buffers are NUL-terminated and live through the call.
    // The flags request replacement plus synchronous persistence; no
    // COPY_ALLOWED flag is present, and our temp file is same-directory.
    let succeeded = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Replace `destination` with an already-written, already-synced sibling file
/// and wait for the namespace replacement to become durable.
///
/// Both paths must have the same lexical parent. Unix uses rename followed by
/// a parent-directory fsync. Windows uses a same-volume
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)` call and
/// does not claim that directory handles can be flushed.
pub fn replace_file_durable(source: &Path, destination: &Path) -> std::io::Result<()> {
    require_same_directory(source, destination)?;
    replace_file_for_durable_write(source, destination)?;
    #[cfg(not(windows))]
    if let Some(parent) = destination.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Write `bytes` to `path` atomically **and durably**:
///
/// 1. write to a same-directory temp file,
/// 2. `fsync` the temp file (data + metadata),
/// 3. replace the target with the temp file,
/// 4. wait for that replacement to become durable: parent-directory `fsync`
///    on Unix, or `MoveFileExW(MOVEFILE_WRITE_THROUGH)` on Windows.
///
/// A crash or power loss at any point leaves either the old file or the
/// complete new file — never a torn one, and never a "file that
/// evaporates on power loss because only the page cache had it".
///
/// The temp name embeds the process + thread ids so concurrent writers to the
/// same target never clobber each other's temp file (see the
/// `save_snapshot` race note in `xerj-storage`).
pub fn write_file_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_file_durable_with_hook(path, bytes, |_| Ok(()))
}

/// Durable-write boundaries exposed for deterministic cross-crate fault tests.
///
/// Production callers should use [`write_file_durable`]. The hook-capable form
/// exists so publication code can prove that every failure boundary prevents
/// later, format-dependent writes without relying on filesystem permissions.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableWriteStage {
    BeforeTempWrite,
    BeforeRename,
    /// The target name has switched. On Unix the parent-directory fsync has
    /// not run yet; on Windows the write-through replacement has completed,
    /// but callers can still inject a conservative post-replace failure before
    /// recording their process-local durability confirmation.
    BeforeParentFsync,
}

/// [`write_file_durable`] with a deterministic observer/failure hook.
#[doc(hidden)]
pub fn write_file_durable_with_hook(
    path: &Path,
    bytes: &[u8],
    mut hook: impl FnMut(DurableWriteStage) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let nonce = format!("{:x}-{:?}", std::process::id(), std::thread::current().id());
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp.{nonce}"));
    hook(DurableWriteStage::BeforeTempWrite)?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        sync_file_full(&f)?;
    }
    if let Err(error) = hook(DurableWriteStage::BeforeRename) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    require_same_directory(&tmp, path)?;
    if let Err(e) = replace_file_for_durable_write(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    hook(DurableWriteStage::BeforeParentFsync)?;
    #[cfg(not(windows))]
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

    #[cfg(not(windows))]
    #[test]
    fn fsync_dir_works() {
        let dir = tempfile::tempdir().unwrap();
        fsync_dir(dir.path()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn fsync_dir_is_only_a_compatibility_shim_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        fsync_dir(dir.path()).unwrap();
        // This return value intentionally carries no durability proof. The
        // durable replacement tests below exercise the write-through API that
        // publication code must use on Windows.
    }

    #[test]
    fn durable_replace_overwrites_synced_sibling() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("replacement.tmp");
        let destination = dir.path().join("marker.json");
        std::fs::write(&destination, b"old").unwrap();
        let mut staged = std::fs::File::create(&source).unwrap();
        staged.write_all(b"new").unwrap();
        staged.sync_all().unwrap();
        drop(staged);

        replace_file_durable(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!source.exists());
    }

    #[test]
    fn staged_batch_publishes_every_file_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.post");
        let b = dir.path().join("b.meta");
        std::fs::write(&b, b"old").unwrap();
        let staged = vec![
            stage_file(&a, b"alpha").unwrap(),
            stage_file(&b, b"beta").unwrap(),
        ];
        // Nothing is visible until the publish.
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"old");
        publish_staged(dir.path(), staged).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"alpha");
        assert_eq!(std::fs::read(&b).unwrap(), b"beta");
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp.")));
    }

    #[test]
    fn an_unpublished_stage_removes_its_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.post");
        drop(stage_file(&a, b"alpha").unwrap());
        assert!(!a.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn publish_staged_rejects_a_file_outside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let staged = vec![stage_file(&other.path().join("x"), b"x").unwrap()];
        let error = publish_staged(dir.path(), staged).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!other.path().join("x").exists());
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
    }

    #[test]
    fn durable_replace_rejects_cross_directory_move() {
        let source_dir = tempfile::tempdir().unwrap();
        let destination_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("replacement.tmp");
        let destination = destination_dir.path().join("marker.json");
        std::fs::write(&source, b"new").unwrap();
        let error = replace_file_durable(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn durable_write_hook_classifies_pre_and_post_rename_failures() {
        for (stage, expected) in [
            (DurableWriteStage::BeforeTempWrite, b"old".as_slice()),
            (DurableWriteStage::BeforeRename, b"old".as_slice()),
            (DurableWriteStage::BeforeParentFsync, b"new".as_slice()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("marker.json");
            std::fs::write(&path, b"old").unwrap();
            let result = write_file_durable_with_hook(&path, b"new", |boundary| {
                if boundary == stage {
                    Err(std::io::Error::other("injected durable-write failure"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), expected);
            assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp.")
            }));
        }
    }
}
