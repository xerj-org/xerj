//! Exclusive lock on `<data_dir>/node.lock` (RC4 blocker 13).
//!
//! A second process must not open a data directory that a live node is
//! serving: `Index::open` replays the WAL and can flush segments. The
//! exclusion itself is `File::try_lock` — `flock(2)` on Unix and
//! `LockFileEx` on Windows — the same call the hub starter uses
//! (`engine/crates/xerj-hub/src/flock.rs` lines 11–25). That comment is
//! right about `flock`: the kernel drops it when the holder exits, including
//! `kill -9`, so the file left behind is only a PID note.
//!
//! `LockFileEx` is a mandatory byte-range lock, and a Windows process that
//! dies during ingest can leave it stuck on `node.lock` until the file is
//! replaced (issue #2233: the next start refused until someone deleted the
//! file by hand). Recovery therefore also asks whether the PID recorded in
//! the file is still a process, the question `xerj-hub` already asks in
//! `proc_state` (`engine/crates/xerj-hub/src/node.rs` lines 1313–1320 and
//! 1439–1458). The engine uses `kill(pid, 0)` and `OpenProcess` rather than
//! the hub's `ps` / `tasklist` spawn: startup should not pay for a process
//! to decide if it may start.
//!
//! A live PID, or a PID that cannot be checked, still fails the second
//! start. The recorded PID is a diagnostic; a running holder is excluded by
//! the OS lock, not by the bytes in the file.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::warn;

use crate::{EngineError, Result};

const LOCK_NAME: &str = "node.lock";

/// How many times [`acquire`] will replace a stale lock file before giving up.
const ACQUIRE_ATTEMPTS: u32 = 4;

/// Pause before treating a busy lock whose recorded PID is dead as stale.
///
/// A live holder writes its PID immediately after `try_lock` returns. The
/// file can still contain the previous (dead) PID for that short window.
/// Replacing the file in the window would drop a running node's lock. A
/// holder that has not stamped within this long is not the process the file
/// names.
const STALE_CONFIRM: Duration = Duration::from_millis(100);

/// Whether a PID names a running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// The operating system still has this process.
    Alive,
    /// The operating system reports no such process.
    Dead,
    /// The check failed. Treated as alive so a lock is never stolen.
    Unknown,
}

/// Exclusive `<data_dir>/node.lock` held for the life of one engine.
///
/// Drop from the acquiring process issues `LOCK_UN` before the descriptor is
/// closed. `flock` lives on the open-file description, and `fork` duplicates
/// that description into the child: closing the parent's fd leaves the lock
/// held until the child execs and `O_CLOEXEC` closes the inherited fd. A concurrent
/// `Command::spawn` in the same process (the node-lock tests do this) hits
/// that window, and the next `Engine::new` on the directory fails even
/// though the holder is gone. `LOCK_UN` releases the description immediately.
///
/// The child must not unlock. Its copy of this value shares the description,
/// and `LOCK_UN` there would drop a parent that is still serving the
/// directory. Same rule as the autoindex journal lock
/// (`engine/crates/xerj-autoindex/src/state.rs` lines 809–827).
#[derive(Debug)]
pub(crate) struct NodeLock {
    file: File,
    owner_pid: u32,
}

impl NodeLock {
    fn new(file: File) -> Self {
        Self {
            file,
            owner_pid: std::process::id(),
        }
    }

    #[cfg(test)]
    fn as_file(&self) -> &File {
        &self.file
    }
}

impl Drop for NodeLock {
    fn drop(&mut self) {
        if std::process::id() == self.owner_pid {
            let _ = self.file.unlock();
        }
    }
}

/// Take the exclusive `<data_dir>/node.lock`, creating the file if needed.
///
/// The returned lock holds the OS lock until it is dropped. On success the
/// file contains this process's PID.
pub(crate) fn acquire(data_dir: &Path) -> Result<NodeLock> {
    let lock_path = data_dir.join(LOCK_NAME);
    let mut reclaimed_from: Option<u32> = None;

    for _ in 0..ACQUIRE_ATTEMPTS {
        let file = open_lock(&lock_path).map_err(|e| {
            config_err(format!(
                "failed to open node lock '{}': {e}",
                lock_path.display()
            ))
        })?;
        match file.try_lock() {
            Ok(()) => {
                if !path_names_this_file(&file, &lock_path) {
                    // Locked an inode that is no longer the directory entry —
                    // a concurrent reclaim renamed it away. Dropping releases
                    // that inode; the next pass opens whatever is there now.
                    drop(file);
                    continue;
                }
                let previous = read_pid_from_file(&file);
                stamp(&file).map_err(|e| {
                    config_err(format!(
                        "failed to record the holder pid in '{}': {e}",
                        lock_path.display()
                    ))
                })?;
                if !path_names_this_file(&file, &lock_path) {
                    drop(file);
                    continue;
                }
                if let Some(pid) = reclaimed_from.or_else(|| dead_other(previous)) {
                    log_reclaim(&lock_path, pid);
                }
                return Ok(NodeLock::new(file));
            }
            Err(TryLockError::WouldBlock) => {
                drop(file);
                match read_holder(&lock_path) {
                    Some(pid) if pid_liveness(pid) == Liveness::Dead => {
                        if confirm_still_dead(&lock_path, pid)
                            && still_blocked(&lock_path).map_err(|e| {
                                config_err(format!(
                                    "failed to acquire node lock '{}': {e}",
                                    lock_path.display()
                                ))
                            })?
                        {
                            reclaimed_from = Some(pid);
                            move_aside(&lock_path, pid).map_err(|e| {
                                config_err(format!(
                                    "data dir '{}' has a stale node.lock for pid {pid}, which is not running, but replacing '{}' failed: {e}. Remove the lock file and start again.",
                                    data_dir.display(),
                                    lock_path.display(),
                                ))
                            })?;
                        }
                        continue;
                    }
                    holder => {
                        return Err(in_use(data_dir, &lock_path, &holder_label(holder)));
                    }
                }
            }
            Err(TryLockError::Error(e)) => {
                return Err(config_err(format!(
                    "failed to acquire node lock '{}': {e}",
                    lock_path.display()
                )));
            }
        }
    }

    Err(config_err(format!(
        "failed to acquire node lock '{}': the lock stayed busy across {ACQUIRE_ATTEMPTS} attempts. If no xerj process is running, remove the lock file and start again.",
        lock_path.display(),
    )))
}

/// Is `dir/node.lock` held by a live process?
///
/// Used by `--auto-port` to pick a free sibling directory. A missing file,
/// a free OS lock, or a busy lock whose recorded PID is not running is not
/// held — the engine reclaims the last of those when it opens the directory.
/// A PID that cannot be read or checked counts as held, so a live node is
/// never treated as free.
pub fn data_dir_lock_held(dir: &Path) -> bool {
    let path = dir.join(LOCK_NAME);
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    match file.try_lock() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => {
            let recorded_dead = matches!(
                read_holder(&path),
                Some(pid) if pid_liveness(pid) == Liveness::Dead
            );
            !recorded_dead
        }
        Err(TryLockError::Error(_)) => false,
    }
}

fn config_err(reason: String) -> EngineError {
    EngineError::Common(xerj_common::XerjError::config(reason))
}

fn in_use(data_dir: &Path, lock_path: &Path, holder: &str) -> EngineError {
    config_err(format!(
        "data dir '{}' is already in use by another running xerj process \
         (pid {holder}, lock file '{}') — refusing to start. Two processes \
         serving one data dir would replay each other's WAL and corrupt \
         segments; stop the other process or point this one at its own \
         server.data_dir.",
        data_dir.display(),
        lock_path.display(),
    ))
}

fn holder_label(holder: Option<u32>) -> String {
    holder
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A recorded PID that is not this process and is not running.
fn dead_other(previous: Option<u32>) -> Option<u32> {
    let pid = previous?;
    if pid == std::process::id() {
        return None;
    }
    (pid_liveness(pid) == Liveness::Dead).then_some(pid)
}

fn log_reclaim(lock_path: &Path, pid: u32) {
    warn!(
        "reclaimed stale node.lock left by pid {pid} (process is not running) at {}",
        lock_path.display()
    );
}

fn open_lock(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Record our PID without shrinking the file.
///
/// `LockFileEx` locks a byte range. Truncating to zero (the old stamp) drops
/// every range that ends past the new end of the file, which on Windows is
/// the whole lock. The record is a fixed 10-digit line, overwritten in
/// place, so a later stamp does not shorten the file. A longer tail, if a
/// previous writer left one, sits past the newline and is ignored.
fn stamp(file: &File) -> io::Result<()> {
    let mut out = file.try_clone()?;
    out.seek(SeekFrom::Start(0))?;
    let pid = std::process::id();
    let record = format!("{pid:010}\n");
    out.write_all(record.as_bytes())?;
    out.flush()?;
    out.sync_all()?;
    Ok(())
}

fn confirm_still_dead(path: &Path, pid: u32) -> bool {
    if pid_liveness(pid) != Liveness::Dead {
        return false;
    }
    std::thread::sleep(STALE_CONFIRM);
    read_holder(path) == Some(pid) && pid_liveness(pid) == Liveness::Dead
}

/// `true` when a fresh open still cannot take the OS lock.
fn still_blocked(path: &Path) -> io::Result<bool> {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Rename `node.lock` aside when it still records `pid` and that PID is dead.
///
/// Removing the directory entry is what clears a `LockFileEx` that survived
/// its holder: the stuck range belongs to the old file object, and the next
/// `open` creates a different one. The rename is skipped when the contents
/// changed, so a holder that stamped during [`STALE_CONFIRM`] keeps its file.
fn move_aside(path: &Path, pid: u32) -> io::Result<()> {
    if read_holder(path) != Some(pid) || pid_liveness(pid) != Liveness::Dead {
        return Ok(());
    }
    let dest = reclaim_dest(path, pid);
    match fs::rename(path, &dest) {
        Ok(()) => {
            let _ = fs::remove_file(&dest);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(_) if !path.exists() => Ok(()),
            Err(e) => Err(e),
        },
    }
}

fn reclaim_dest(path: &Path, pid: u32) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        "node.lock.reclaim-{pid}-{}-{seq}",
        std::process::id()
    ))
}

fn read_holder(path: &Path) -> Option<u32> {
    let file = File::open(path).ok()?;
    read_pid_from_file(&file)
}

fn read_pid_from_file(file: &File) -> Option<u32> {
    let mut cloned = file.try_clone().ok()?;
    cloned.seek(SeekFrom::Start(0)).ok()?;
    let mut buf = [0_u8; 64];
    let n = cloned.read(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf[..n]).ok()?;
    parse_pid(text)
}

fn parse_pid(text: &str) -> Option<u32> {
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let pid: u32 = line.parse().ok()?;
    (pid != 0).then_some(pid)
}

fn path_names_this_file(file: &File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), fs::metadata(path)) {
            (Ok(ours), Ok(named)) => ours.dev() == named.dev() && ours.ino() == named.ino(),
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        windows_same_file(file, path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, path);
        true
    }
}

#[cfg(unix)]
fn pid_liveness(pid: u32) -> Liveness {
    // `kill` takes a `pid_t`. A value that does not fit is not a Unix PID;
    // refusing to signal it also avoids casting it to a negative pid.
    let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
        return Liveness::Unknown;
    };
    // SAFETY: signal 0 does not deliver a signal. `kill` only checks that
    // `pid` names a process the caller may signal, and writes the result to
    // the return value and errno. No other state is modified.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        // Includes zombies: the process entry still exists. A zombie has
        // already closed its descriptors, so it does not hold `flock`;
        // treating it as alive only refuses a steal, which is the safe side.
        return Liveness::Alive;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::ESRCH => Liveness::Dead,
        // The process exists, but it belongs to another user.
        Some(code) if code == libc::EPERM => Liveness::Alive,
        _ => Liveness::Unknown,
    }
}

#[cfg(windows)]
fn pid_liveness(pid: u32) -> Liveness {
    if pid == 0 {
        return Liveness::Unknown;
    }
    // PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE, and the wait/error codes.
    // `WaitForSingleObject` needs SYNCHRONIZE on the handle; query-limited
    // alone is not enough to observe exit.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 0x102;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(
            desired_access: u32,
            inherit_handle: i32,
            process_id: u32,
        ) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }

    // SAFETY: `OpenProcess` / `WaitForSingleObject` / `CloseHandle` /
    // `GetLastError` are the Win32 process-liveness calls. The handle is
    // either null or a process handle this function closes before returning.
    // `GetLastError` is read immediately after a failed `OpenProcess`, before
    // any other call can overwrite the thread's last-error code.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return match GetLastError() {
                ERROR_INVALID_PARAMETER => Liveness::Dead,
                ERROR_ACCESS_DENIED => Liveness::Alive,
                _ => Liveness::Unknown,
            };
        }
        let waited = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        match waited {
            WAIT_TIMEOUT => Liveness::Alive,
            // The process object is signaled once the process has exited,
            // even if a leftover handle has kept the PID from disappearing.
            WAIT_OBJECT_0 => Liveness::Dead,
            _ => Liveness::Unknown,
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn pid_liveness(_pid: u32) -> Liveness {
    Liveness::Unknown
}

#[cfg(windows)]
fn windows_same_file(file: &File, path: &Path) -> bool {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct Filetime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: Filetime,
        last_access_time: Filetime,
        last_write_time: Filetime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            info: *mut ByHandleFileInformation,
        ) -> i32;
    }

    fn identity(handle: *mut std::ffi::c_void) -> Option<(u32, u64)> {
        // SAFETY: `handle` is a live file handle. `info` points at a
        // zeroed `BY_HANDLE_FILE_INFORMATION` the caller owns. The call
        // writes that struct and returns zero on failure.
        unsafe {
            let mut info = std::mem::zeroed();
            if GetFileInformationByHandle(handle, &mut info) == 0 {
                return None;
            }
            let index = (u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low);
            Some((info.volume_serial_number, index))
        }
    }

    let Ok(named) = File::open(path) else {
        return false;
    };
    match (
        identity(file.as_raw_handle()),
        identity(named.as_raw_handle()),
    ) {
        (Some(ours), Some(theirs)) => ours == theirs,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn serialize_tests() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[derive(Clone)]
    struct LogBuffer(std::sync::Arc<Mutex<Vec<u8>>>);

    struct LogWriter(std::sync::Arc<Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = LogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(self.0.clone())
        }
    }

    fn capture_warns(body: impl FnOnce()) -> String {
        let buffer = LogBuffer(std::sync::Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(buffer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        body();
        let bytes = buffer.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn reaped_pid() -> u32 {
        for _ in 0..20 {
            let mut cmd = if cfg!(windows) {
                let mut cmd = std::process::Command::new("cmd");
                cmd.args(["/C", "exit", "0"]);
                cmd
            } else {
                std::process::Command::new("true")
            };
            let mut child = cmd.spawn().expect("spawn a process that exits");
            let pid = child.id();
            child.wait().unwrap();
            if pid_liveness(pid) == Liveness::Dead {
                return pid;
            }
        }
        panic!("could not observe a reaped pid");
    }

    fn hold(path: &Path) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        file.try_lock().expect("the test process locks the file");
        file
    }

    #[test]
    fn this_process_is_alive_and_a_reaped_child_is_dead() {
        assert_eq!(pid_liveness(std::process::id()), Liveness::Alive);
        assert_eq!(pid_liveness(0), Liveness::Unknown);
        let dead = reaped_pid();
        assert_eq!(pid_liveness(dead), Liveness::Dead);
    }

    #[test]
    fn a_second_acquire_fails_while_the_holder_is_alive_and_reopen_does_not_warn() {
        let _guard = serialize_tests();
        let dir = tempfile::tempdir().unwrap();
        let held = acquire(dir.path()).expect("first lock");
        let err = acquire(dir.path()).expect_err("a live holder must block");
        let err = err.to_string();
        assert!(
            err.contains("already in use"),
            "unexpected lock error: {err}"
        );
        assert!(
            err.contains(&std::process::id().to_string()),
            "lock error should name the holding pid: {err}"
        );
        drop(held);

        let logs = capture_warns(|| {
            let again = acquire(dir.path()).expect("reopen after the holder drops");
            drop(again);
        });
        assert!(
            !logs.contains("reclaimed stale node.lock"),
            "dropping our own lock is not a dead holder: {logs}"
        );
    }

    #[test]
    fn a_lock_file_left_by_a_dead_pid_is_reclaimed_and_logged() {
        let _guard = serialize_tests();
        let dir = tempfile::tempdir().unwrap();
        let dead = reaped_pid();
        fs::write(dir.path().join(LOCK_NAME), format!("{dead}\n")).unwrap();

        let logs = capture_warns(|| {
            let file = acquire(dir.path()).expect("a dead pid with no os holder is stale");
            let recorded = read_pid_from_file(file.as_file());
            assert_eq!(recorded, Some(std::process::id()));
            let err = acquire(dir.path()).expect_err("the reclaimed lock is exclusive");
            assert!(err.to_string().contains("already in use"));
            drop(file);
        });
        assert!(
            logs.contains(&format!(
                "reclaimed stale node.lock left by pid {dead} (process is not running)"
            )),
            "reclaim must be logged: {logs}"
        );
    }

    #[test]
    fn a_busy_lock_whose_recorded_pid_is_dead_is_reclaimed_when_that_pid_is_readable() {
        let _guard = serialize_tests();
        let dir = tempfile::tempdir().unwrap();
        let dead = reaped_pid();
        let path = dir.path().join(LOCK_NAME);
        fs::write(&path, format!("{dead}\n")).unwrap();
        // Simulates the Windows failure mode: the OS lock is still busy, but
        // the PID in the file is not a running process. On Unix the bytes
        // stay readable under `flock`. A mandatory lock that hides them
        // cannot be shown to be stale, and must keep blocking.
        let holder = hold(&path);
        let readable = read_holder(&path) == Some(dead);

        let logs = capture_warns(|| {
            let result = acquire(dir.path());
            if readable {
                let file =
                    result.expect("a readable dead pid is reclaimed even if try_lock is busy");
                assert_eq!(read_pid_from_file(file.as_file()), Some(std::process::id()));
                // Reclaim replaces the file. The same inode would mean
                // `try_lock` succeeded on the busy file and this did not
                // exercise the stuck-lock path.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let held_ino = holder.metadata().unwrap().ino();
                    let now_ino = fs::metadata(&path).unwrap().ino();
                    assert_ne!(
                        held_ino, now_ino,
                        "reclaim must replace the stuck lock file"
                    );
                }
                let err = acquire(dir.path()).expect_err("replacement lock is held");
                assert!(err.to_string().contains(&std::process::id().to_string()));
                drop(file);
            } else {
                let err = result.expect_err("an unreadable busy lock must block");
                assert!(
                    err.to_string().contains("already in use"),
                    "unexpected lock error: {err}"
                );
            }
        });
        if readable {
            assert!(
                logs.contains(&format!("reclaimed stale node.lock left by pid {dead}")),
                "stuck-lock reclaim must be logged: {logs}"
            );
        }
        drop(holder);
    }

    #[test]
    fn a_busy_lock_with_no_pid_still_blocks() {
        let _guard = serialize_tests();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_NAME);
        fs::write(&path, b"\n").unwrap();
        let _holder = hold(&path);
        let err = acquire(dir.path()).expect_err("no pid means the holder is unknown");
        let err = err.to_string();
        assert!(err.contains("already in use"), "{err}");
        assert!(err.contains("unknown"), "{err}");
    }

    #[test]
    fn data_dir_lock_held_ignores_a_dead_pid_and_honors_a_live_one() {
        let _guard = serialize_tests();
        let dir = tempfile::tempdir().unwrap();
        assert!(!data_dir_lock_held(dir.path()));

        let dead = reaped_pid();
        fs::write(dir.path().join(LOCK_NAME), format!("{dead}\n")).unwrap();
        assert!(
            !data_dir_lock_held(dir.path()),
            "a lock file from a dead pid with no os lock is not held"
        );

        // `acquire` logs the reclaim. A subscriber has to be installed first:
        // the callsite interest cache is process-global, and a first hit with
        // no subscriber would drop the warning the other tests assert.
        capture_warns(|| {
            let held = acquire(dir.path()).unwrap();
            assert!(
                data_dir_lock_held(dir.path()),
                "a lock held by this live process is held"
            );
            drop(held);
        });
        assert!(!data_dir_lock_held(dir.path()));
    }

    /// The integration test `second_engine_on_same_data_dir_fails_fast_with_holder_pid`
    /// failed under `--test-threads=2` because its sibling spawns `true` /
    /// `ps`. `fork` duplicates the held `node.lock` description into that
    /// child; `drop` of the engine only `close`d the parent's fd, so the
    /// reopen still saw the lock until the child exec'd.
    #[cfg(all(unix, not(miri)))]
    #[test]
    fn drop_unlocks_while_a_forked_child_holds_the_inherited_descriptor() {
        let _guard = serialize_tests();
        let dir = tempfile::tempdir().unwrap();
        let held = acquire(dir.path()).expect("first lock");

        let mut pipe_fds = [-1_i32; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            panic!("pipe failed: {}", std::io::Error::last_os_error());
        }
        let child = unsafe { libc::fork() };
        if child < 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(pipe_fds[0]);
                libc::close(pipe_fds[1]);
            }
            panic!("fork failed: {error}");
        }
        if child == 0 {
            unsafe {
                libc::close(pipe_fds[1]);
                let mut byte = 0_u8;
                loop {
                    let n = libc::read(
                        pipe_fds[0],
                        (&mut byte as *mut u8).cast::<libc::c_void>(),
                        1,
                    );
                    if n == 0 {
                        break;
                    }
                    if n < 0 && child_errno() == libc::EINTR {
                        continue;
                    }
                    libc::_exit(3);
                }
                libc::close(pipe_fds[0]);
                libc::_exit(0);
            }
        }

        // The child is blocked in `read` with the inherited descriptor open.
        // Reap it on every exit path so a failed assertion cannot leave it
        // waiting on the pipe.
        let _child = ForkedHolder {
            child,
            release_fd: pipe_fds[1],
        };
        unsafe { libc::close(pipe_fds[0]) };

        drop(held);
        let reopened = acquire(dir.path());
        drop(_child);
        let again = reopened.expect(
            "dropping the holder must unlock while a forked child still has the inherited fd",
        );
        drop(again);
    }

    #[cfg(all(unix, not(miri)))]
    fn child_errno() -> libc::c_int {
        // #2473 follow-up: `__errno_location` is glibc-only. Naming it made the
        // whole lib-test target fail to compile on macOS and the BSDs
        // (`cannot find function __errno_location in crate libc`), which is
        // every test in this crate, not just the fork ones. `last_os_error`
        // reads the same thread-local errno through the standard library's
        // per-platform accessor and allocates nothing.
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    #[cfg(all(unix, not(miri)))]
    struct ForkedHolder {
        child: libc::pid_t,
        release_fd: libc::c_int,
    }

    #[cfg(all(unix, not(miri)))]
    impl Drop for ForkedHolder {
        fn drop(&mut self) {
            if self.release_fd >= 0 {
                unsafe { libc::close(self.release_fd) };
                self.release_fd = -1;
            }
            if self.child > 0 {
                loop {
                    let mut status = 0;
                    let waited = unsafe { libc::waitpid(self.child, &mut status, 0) };
                    if waited == self.child {
                        break;
                    }
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        break;
                    }
                }
                self.child = -1;
            }
        }
    }
}
