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
    // A counter, not the thread id. One thread stages every side-car of one
    // field and then the next field. On a case-folding volume two destinations
    // that differ only by case (`Page.post` and `page.post`) produced the same
    // temp name, the second `File::create` truncated the first, and the second
    // rename failed with `ERROR_FILE_NOT_FOUND` (os error 2) — the rc.98
    // "publishing FTS side-cars" abort. The thread-id nonce did not save it:
    // both creates ran on that one thread.
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    path.with_file_name(format!("{file_name}.tmp.{:x}.{nonce}", std::process::id()))
}

/// Write `bytes` to a temp file beside `path` for a later
/// [`publish_staged`], which makes it durable before it renames it (on
/// Linux the sync itself happens there, see [`DEFER_STAGED_SYNC`]). Nothing
/// is visible at `path` until then.
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
    sync_staged_at_stage(&f)?;
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
    sync_staged_at_stage(file)?;
    Ok(staged)
}

/// Linux defers each staged file's `fsync` to [`publish_staged`], which
/// issues them all at once (#3324).
///
/// Staging used to `fsync` every temp file the moment it was written. The FTS
/// writer stages four files per field (postings, FST, meta, norms) one after
/// another, so a segment paid four sequential journal commits per field, and
/// the flush finalize held its `flush_finalize_gate` permit through all of
/// them. On ext4 under I/O pressure a commit took 0.3 s at the median and
/// 0.9 s at p90 (measured on the shared 32-core box, load average ~70), so
/// the permit was held for seconds of disk wait, not CPU, and every other
/// flush in the process queued behind it until `flush_admit_timeout_secs`.
///
/// Concurrent `fsync` calls on ext4 join the same journal transaction, so the
/// batch costs about one commit instead of one per file. The guarantee is
/// unchanged: every staged file's bytes are synced before the first rename,
/// and the directory is synced after the last one. Other targets keep the
/// per-file sync at stage time (Apple pairs it with one `F_FULLFSYNC`).
const DEFER_STAGED_SYNC: bool = cfg!(target_os = "linux");

/// Upper bound on the threads [`sync_staged_batch`] uses for one batch.
const STAGED_SYNC_WORKERS: usize = 16;

fn sync_staged_at_stage(file: &std::fs::File) -> std::io::Result<()> {
    if DEFER_STAGED_SYNC {
        Ok(())
    } else {
        sync_file_for_batch(file)
    }
}

/// Open an existing file with the access rights needed to sync it to stable
/// storage. Unix accepts a read-only descriptor (`fsync(2)` / `F_FULLFSYNC`
/// writes back the inode's dirty pages whichever descriptor asks); Windows
/// `FlushFileBuffers` (`File::sync_all`) requires `GENERIC_WRITE` and fails
/// with `ERROR_ACCESS_DENIED` on a read-only handle.
fn open_for_sync(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        std::fs::OpenOptions::new().write(true).open(path)
    }
    #[cfg(not(windows))]
    {
        std::fs::File::open(path)
    }
}

/// Sync `len` paths returned by `path_at(i)` concurrently across up to
/// [`STAGED_SYNC_WORKERS`] threads.
///
/// The calling thread participates as one worker so a refused helper-thread
/// spawn (`EAGAIN` under tight `RLIMIT_NPROC`) still completes the batch
/// without panicking. On Apple targets, each worker issues plain `fsync(2)`
/// and records one index per distinct filesystem (`st_dev`), and this function
/// finishes with one `F_FULLFSYNC` ([`sync_file_full`]) per filesystem so
/// every file's bytes reach stable storage before returning.
fn sync_paths_with<'a>(
    len: usize,
    path_at: impl Fn(usize) -> &'a Path + Sync,
) -> std::io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    if len == 1 {
        return sync_file_full(&open_for_sync(path_at(0))?);
    }

    #[cfg(target_vendor = "apple")]
    type WorkerOutcome = std::io::Result<Vec<(u64, usize)>>;
    #[cfg(not(target_vendor = "apple"))]
    type WorkerOutcome = std::io::Result<()>;

    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicBool::new(false);
    let run_worker = || -> WorkerOutcome {
        #[cfg(target_vendor = "apple")]
        let mut devs: Vec<(u64, usize)> = Vec::new();
        loop {
            if failed.load(std::sync::atomic::Ordering::Relaxed) {
                #[cfg(target_vendor = "apple")]
                return Ok(devs);
                #[cfg(not(target_vendor = "apple"))]
                return Ok(());
            }
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i >= len {
                #[cfg(target_vendor = "apple")]
                return Ok(devs);
                #[cfg(not(target_vendor = "apple"))]
                return Ok(());
            }
            let sync_result = open_for_sync(path_at(i)).and_then(|file| {
                sync_file_for_batch(&file)?;
                #[cfg(target_vendor = "apple")]
                {
                    use std::os::unix::fs::MetadataExt as _;
                    let dev = file.metadata()?.dev();
                    if !devs.iter().any(|&(seen, _)| seen == dev) {
                        devs.push((dev, i));
                    }
                }
                Ok(())
            });
            if let Err(error) = sync_result {
                failed.store(true, std::sync::atomic::Ordering::Relaxed);
                return Err(error);
            }
        }
    };

    let helper_count = len.min(STAGED_SYNC_WORKERS).saturating_sub(1);
    #[cfg(target_vendor = "apple")]
    let mut all_devs: Vec<(u64, usize)> = Vec::new();
    let mut first_error: Option<std::io::Error> = None;

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(helper_count);
        for _ in 0..helper_count {
            // If the OS refuses a helper thread, the calling thread and any
            // already-spawned helpers still drain the full batch.
            if let Ok(handle) = std::thread::Builder::new().spawn_scoped(scope, run_worker) {
                handles.push(handle);
            }
        }
        let local_outcome = run_worker();
        match local_outcome {
            #[cfg(target_vendor = "apple")]
            Ok(devs) => {
                for (dev, idx) in devs {
                    if !all_devs.iter().any(|&(seen, _)| seen == dev) {
                        all_devs.push((dev, idx));
                    }
                }
            }
            #[cfg(not(target_vendor = "apple"))]
            Ok(()) => {}
            Err(error) => {
                first_error = Some(error);
            }
        }
        for handle in handles {
            let outcome = handle
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("batch-file sync thread panicked")));
            match outcome {
                #[cfg(target_vendor = "apple")]
                Ok(devs) => {
                    for (dev, idx) in devs {
                        if !all_devs.iter().any(|&(seen, _)| seen == dev) {
                            all_devs.push((dev, idx));
                        }
                    }
                }
                #[cfg(not(target_vendor = "apple"))]
                Ok(()) => {}
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
    });

    if let Some(error) = first_error {
        return Err(error);
    }
    #[cfg(target_vendor = "apple")]
    {
        for (_, idx) in all_devs {
            sync_file_full(&open_for_sync(path_at(idx))?)?;
        }
    }
    Ok(())
}

/// Sync every file in `paths` to stable storage, issuing per-file syncs
/// concurrently across up to [`STAGED_SYNC_WORKERS`] threads (#3449).
///
/// Use this when a staging phase writes many independent files whose
/// durability barrier is a later manifest write or directory rename:
///
/// - On Linux, concurrent `fsync` calls join one ext4 journal commit instead
///   of paying one commit per file (the #3324 pattern).
/// - On Apple targets, workers issue `fsync(2)` concurrently and one
///   `F_FULLFSYNC` ([`sync_file_full`]) per filesystem drains the drive write
///   cache for the whole batch before returning.
/// - On Windows and other targets, workers issue [`sync_file_full`]
///   concurrently.
///
/// Returns `Ok(())` only after every file in `paths` has been synced; on any
/// error, stops scheduling new files and returns the first error.
pub fn sync_paths_batch<P: AsRef<Path> + Sync>(paths: &[P]) -> std::io::Result<()> {
    sync_paths_with(paths.len(), |i| paths[i].as_ref())
}

/// `fsync` every staged temp file, concurrently, before any rename.
fn sync_staged_batch(files: &[StagedFile]) -> std::io::Result<()> {
    sync_paths_with(files.len(), |i| files[i].tmp.as_path())
}

/// Move every staged file in `dir` into place, durably, with the same
/// guarantee as one [`write_file_durable`] per file:
///
/// 1. every staged file's bytes reach stable storage before any rename (on
///    Apple targets, one `F_FULLFSYNC`, which drains the device cache for
///    every earlier `fsync`; on Linux all staged files are `fsync`ed here,
///    concurrently, see [`DEFER_STAGED_SYNC`]; elsewhere each file was
///    already fully synced when it was staged);
/// 2. each file replaces its target (rename on Unix, write-through
///    `MoveFileExW` on Windows);
/// 3. one parent-directory fsync makes every rename durable (Unix).
///
/// A crash at any point leaves each target either absent, old or complete,
/// never torn, exactly as per-file durable writes did. What changes is the
/// cost: two device flushes for the batch instead of two per file, which on
/// Apple targets is the difference between ~250 cache drains per segment and
/// two.
///
/// If a rename fails, destinations already replaced by *this* call are
/// removed and the remaining temps are dropped. FTS side-cars are new files
/// under a fresh segment id, so that rollback leaves the directory empty of
/// this batch instead of a half-published family. A later directory fsync
/// failure does not roll those names back: the bytes are already complete,
/// and removing them would turn a durability-confirmation error into missing
/// files. Callers that replace a pre-existing file they still need must not
/// use this batch helper.
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
    // Refuse before any rename. NTFS (and the default Windows volume) treats
    // the two names as one file: the second replace either returns
    // ERROR_FILE_NOT_FOUND once the shared temp is gone, or
    // MOVEFILE_REPLACE_EXISTING silently drops the first field. Layout v3
    // hashes the common pairs before they reach here; this is the backstop
    // for a pair the layout still stores as two literals (`µ`/`μ`, `ς`/`σ`).
    reject_case_folded_duplicate_destinations(&files)?;
    if DEFER_STAGED_SYNC {
        sync_staged_batch(&files)?;
    }
    #[cfg(target_vendor = "apple")]
    sync_file_full(&std::fs::File::open(dir)?)?;
    let mut published_dests = Vec::with_capacity(files.len());
    for mut file in files {
        if let Err(error) = replace_file_for_durable_write(&file.tmp, &file.dest) {
            for dest in &published_dests {
                let _ = std::fs::remove_file(dest);
            }
            return Err(error);
        }
        file.published = true;
        published_dests.push(file.dest.clone());
    }
    #[cfg(not(windows))]
    fsync_dir(dir)?;
    Ok(())
}

/// Windows `MoveFileExW` returns these when a scanner still has the temp
/// open at the moment of the replace. They are transient for the FTS publish
/// path (the handle is ours and already closed). `ERROR_FILE_NOT_FOUND` (2)
/// is not: retrying it hides a missing temp.
#[cfg(any(windows, test))]
fn replace_lock_should_retry(os_code: Option<i32>, retries_already: u32) -> bool {
    const REPLACE_LOCK_RETRIES: u32 = 6;
    retries_already < REPLACE_LOCK_RETRIES && matches!(os_code, Some(5 | 32 | 33))
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
fn replace_file_for_durable_write_raw(source: &Path, destination: &Path) -> std::io::Result<()> {
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
fn replace_file_for_durable_write_raw(source: &Path, destination: &Path) -> std::io::Result<()> {
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
    let mut retries_already = 0u32;
    loop {
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
        if succeeded != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if replace_lock_should_retry(error.raw_os_error(), retries_already) {
            retries_already += 1;
            // A filter driver (Defender, DLP) often locks a just-closed file
            // for a scan. Short backoff covers that without turning a
            // permanent access error into a long stall: six waits are
            // 10+20+…+60 ms.
            std::thread::sleep(std::time::Duration::from_millis(
                10 * u64::from(retries_already),
            ));
            continue;
        }
        return Err(error);
    }
}

fn replace_file_for_durable_write(source: &Path, destination: &Path) -> std::io::Result<()> {
    replace_file_for_durable_write_raw(source, destination).map_err(|error| {
        // The OS error's Display already ends with `(os error N)`. Naming both
        // paths is what the rc.98 report was missing: the chain stopped at
        // "publishing FTS side-cars" and the agent could not see which file
        // the volume refused.
        std::io::Error::new(
            error.kind(),
            format!(
                "replacing {} with {}: {error}",
                destination.display(),
                source.display()
            ),
        )
    })
}

/// Key under which a case-folding volume (NTFS, and the default Windows
/// drive) stores `file_name`. Unicode lowercase covers ASCII and the letters
/// whose lower form is the folded form. The extra arms are the NTFS upcase
/// equivalences lowercase does not collapse: U+00B5 `µ` and U+03BC `μ` both
/// upcase to U+039C, and U+03C2 `ς` and U+03C3 `σ` both upcase to U+03A3.
fn case_folding_publish_key(file_name: &str) -> String {
    let mut key = String::with_capacity(file_name.len());
    for character in file_name.chars() {
        push_case_folding_publish_char(character, &mut key);
    }
    key
}

fn push_case_folding_publish_char(character: char, out: &mut String) {
    match character {
        'µ' | 'μ' => out.push('μ'),
        'ς' | 'σ' => out.push('σ'),
        'ı' => out.push('i'),
        'ſ' => out.push('s'),
        'K' => out.push('k'),
        'ß' => out.push_str("ss"),
        other => {
            for lower in other.to_lowercase() {
                match lower {
                    'µ' | 'μ' => out.push('μ'),
                    'ς' | 'σ' => out.push('σ'),
                    'ı' => out.push('i'),
                    'ſ' => out.push('s'),
                    'K' => out.push('k'),
                    'ß' => out.push_str("ss"),
                    folded => out.push(folded),
                }
            }
        }
    }
}

fn reject_case_folded_duplicate_destinations(files: &[StagedFile]) -> std::io::Result<()> {
    let mut seen: std::collections::HashMap<String, &Path> = std::collections::HashMap::new();
    for file in files {
        let file_name = file
            .dest
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let key = case_folding_publish_key(&file_name);
        if let Some(previous) = seen.get(&key) {
            if *previous != file.dest.as_path() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "cannot publish {} and {}: a case-folding filesystem treats them as one file",
                        previous.display(),
                        file.dest.display()
                    ),
                ));
            }
        } else {
            seen.insert(key, file.dest.as_path());
        }
    }
    Ok(())
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
    hook: impl FnMut(DurableWriteStage) -> std::io::Result<()>,
) -> std::io::Result<()> {
    durable_publish(path, |f| f.write_all(bytes), hook)
}

/// [`write_file_durable`] for a body produced by a writer instead of a slice.
///
/// `write_body` writes the whole file content into the temp file; the temp
/// file is then fsynced, renamed over `path`, and the parent directory is
/// fsynced, exactly like [`write_file_durable`]. Use it when the content is
/// large enough that building it in memory first would double its footprint
/// (an HNSW graph snapshot is the size of the graph, #2427). On error the
/// temp file is removed and `path` is untouched. Buffer the writes yourself
/// (e.g. `std::io::BufWriter`) — the `File` is handed over unbuffered.
pub fn write_file_durable_streaming(
    path: &Path,
    write_body: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    durable_publish(path, write_body, |_| Ok(()))
}

fn durable_publish(
    path: &Path,
    write_body: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
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
        let written = write_body(&mut f).and_then(|()| sync_file_full(&f));
        if let Err(error) = written {
            drop(f);
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
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

    #[test]
    fn write_file_durable_streaming_round_trip_and_failure_keeps_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("graph.bin");
        write_file_durable_streaming(&p, |f| {
            f.write_all(b"hel")?;
            f.write_all(b"lo")
        })
        .unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
        // A body that fails part-way must leave the old file and no temp file.
        let err = write_file_durable_streaming(&p, |f| {
            f.write_all(b"torn")?;
            Err(std::io::Error::other("encode failed"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "encode failed");
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
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

    /// #3324: a wide batch (more files than sync workers) publishes every
    /// file, and on Linux, where the per-file sync moved from staging to the
    /// publish, a temp that cannot be synced fails the batch before any
    /// rename, so no destination appears.
    #[test]
    fn a_wide_staged_batch_syncs_every_file_before_any_rename() {
        let dir = tempfile::tempdir().unwrap();
        let n = STAGED_SYNC_WORKERS * 3 + 1;
        let paths: Vec<_> = (0..n)
            .map(|i| dir.path().join(format!("f{i}.post")))
            .collect();
        let staged: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| stage_file(p, format!("v{i}").as_bytes()).unwrap())
            .collect();
        publish_staged(dir.path(), staged).unwrap();
        for (i, p) in paths.iter().enumerate() {
            assert_eq!(std::fs::read(p).unwrap(), format!("v{i}").as_bytes());
        }

        if DEFER_STAGED_SYNC {
            let other = tempfile::tempdir().unwrap();
            let a = other.path().join("a.post");
            let b = other.path().join("b.post");
            let staged = vec![stage_file(&a, b"a").unwrap(), stage_file(&b, b"b").unwrap()];
            std::fs::remove_file(&staged[1].tmp).unwrap();
            assert!(publish_staged(other.path(), staged).is_err());
            assert!(!a.exists(), "no rename may run before every temp is synced");
            assert!(!b.exists());
        }
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
    fn replace_lock_retries_sharing_violations_but_not_a_missing_temp() {
        assert!(replace_lock_should_retry(Some(5), 0));
        assert!(replace_lock_should_retry(Some(32), 5));
        assert!(replace_lock_should_retry(Some(33), 0));
        assert!(!replace_lock_should_retry(Some(32), 6));
        assert!(!replace_lock_should_retry(Some(2), 0));
        assert!(!replace_lock_should_retry(None, 0));
    }

    #[test]
    fn publish_staged_rolls_back_a_partial_rename_and_a_later_publish_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let published = dir.path().join("a.post");
        let blocked = dir.path().join("b.meta");
        // A directory sitting on the destination makes the second replace fail
        // after the first has already moved into place.
        std::fs::create_dir(&blocked).unwrap();
        let staged = vec![
            stage_file(&published, b"alpha").unwrap(),
            stage_file(&blocked, b"beta").unwrap(),
        ];
        let error = publish_staged(dir.path(), staged).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !published.exists(),
            "a dest renamed earlier in the batch must be removed"
        );
        assert!(blocked.is_dir());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["b.meta".to_string()]);

        let staged = vec![stage_file(&published, b"alpha-again").unwrap()];
        publish_staged(dir.path(), staged).unwrap();
        assert_eq!(std::fs::read(&published).unwrap(), b"alpha-again");
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

    #[test]
    fn staging_one_destination_twice_on_one_thread_publishes_both_temps() {
        // Before the per-call nonce, the second stage truncated the first
        // temp and the second rename failed with not-found. That is the
        // Windows shape of two case-folded side-cars written by one thread;
        // the same destination reproduces the temp collision on Linux.
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("a.post");
        let first = stage_file(&destination, b"one").unwrap();
        let second = stage_file(&destination, b"two").unwrap();
        publish_staged(dir.path(), vec![first, second]).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"two");
    }

    #[test]
    fn publish_staged_refuses_names_a_case_folding_volume_would_merge() {
        let dir = tempfile::tempdir().unwrap();
        for (left_name, right_name) in [
            ("Page.fst", "page.fst"),
            ("field.µ.post", "field.μ.post"),
            ("sigma.ς.meta", "sigma.σ.meta"),
        ] {
            let left = dir.path().join(left_name);
            let right = dir.path().join(right_name);
            let staged = vec![
                stage_file(&left, b"left").unwrap(),
                stage_file(&right, b"right").unwrap(),
            ];
            let error = publish_staged(dir.path(), staged).unwrap_err();
            let message = error.to_string();
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{message}");
            assert!(message.contains(left_name), "{message}");
            assert!(message.contains(right_name), "{message}");
            assert!(!left.exists(), "{left_name} was renamed");
            assert!(!right.exists(), "{right_name} was renamed");
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_failed_replace_names_both_paths_and_the_os_error() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("body.post.tmp.1");
        let destination = dir.path().join("body.post");
        std::fs::write(&source, b"new").unwrap();
        // A directory sitting on the destination makes the replace fail
        // after the temp exists, which is the publish_staged failure mode.
        std::fs::create_dir(&destination).unwrap();
        let error = replace_file_durable(&source, &destination).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("body.post"), "{message}");
        assert!(message.contains("body.post.tmp.1"), "{message}");
        assert!(
            message.contains("os error"),
            "the OS code must be visible, got {message}"
        );
        assert!(source.exists(), "a failed replace must keep the temp");
    }

    /// #3449: `sync_paths_batch` handles empty, single, and wide batches
    /// (wider than the worker pool across subdirectories), and fails cleanly
    /// if any path cannot be opened/synced.
    #[test]
    fn sync_paths_batch_handles_empty_single_wide_and_missing_paths() {
        let empty: [&Path; 0] = [];
        sync_paths_batch(&empty).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let single = dir.path().join("only.ndjson");
        std::fs::write(&single, b"single-payload\n").unwrap();
        sync_paths_batch(&[single.as_path()]).unwrap();
        assert_eq!(std::fs::read(&single).unwrap(), b"single-payload\n");

        let blobs = dir.path().join("blobs");
        let prepared = dir.path().join("prepared");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&prepared).unwrap();
        let n = STAGED_SYNC_WORKERS * 3 + 1;
        let mut paths = Vec::with_capacity(n);
        for i in 0..n {
            let p = if i % 2 == 0 {
                prepared.join(format!("{i:08}.ndjson"))
            } else {
                blobs.join(format!("{i:08}"))
            };
            std::fs::write(&p, format!("payload-{i}\n")).unwrap();
            paths.push(p);
        }
        sync_paths_batch(&paths).unwrap();
        for (i, p) in paths.iter().enumerate() {
            assert_eq!(
                std::fs::read(p).unwrap(),
                format!("payload-{i}\n").as_bytes()
            );
        }

        // Remove one file beyond the first worker wave and confirm the batch
        // fails with NotFound rather than succeeding silently.
        let missing_idx = STAGED_SYNC_WORKERS + 2;
        std::fs::remove_file(&paths[missing_idx]).unwrap();
        let error = sync_paths_batch(&paths).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
    }
}
