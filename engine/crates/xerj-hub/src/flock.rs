//! A cross-process lock that belongs to the process holding it.
//!
//! The hub serialises two things between processes: starting the automatic node
//! ([`crate::node`]) and pulling one pack ([`crate::cli`]). Both used to create a
//! lock *directory* and treat one older than a threshold as abandoned. That cannot
//! be made right: a lock with no owner is either stolen while its owner is still
//! working (two starters, two pulls) or kept for the whole threshold after its
//! owner was killed (a corpus nobody can pull for half an hour, a stuck starter
//! that spins forever). The threshold is a guess about a process nobody asked.
//!
//! This is the operating system's own answer: an advisory lock on a file
//! (`flock(2)` / `LockFileEx`, through [`File::try_lock`] — the call the engine
//! takes `<data-dir>/node.lock` with). The kernel releases it when the holder's
//! file descriptor closes, which includes the holder being killed with SIGKILL, so:
//!
//! - there is no "stale lock" and nothing to steal, so no steal race;
//! - a killed holder never blocks the next caller;
//! - a waiter's loop always reaches its deadline, because the only ways out of
//!   it are "locked", "deadline", and "the lock cannot be taken at all" (a real
//!   I/O error is returned at once, never retried).
//!
//! The lock file itself is never deleted: removing a flock file lets a waiter
//! lock the unlinked inode while a newcomer locks a fresh one. The holder writes
//! its PID into the file purely so a caller that gives up can say who it was
//! waiting for; exclusivity comes from the OS lock, never from the file's content.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a waiter retries.
const POLL: Duration = Duration::from_millis(100);

/// Why a lock was not taken.
#[derive(Debug)]
pub(crate) enum LockError {
    /// Somebody else still held it when the wait ran out. `holder` is the PID
    /// that process wrote into the lock file, if it could be read.
    Timeout { holder: Option<u32> },
    /// The lock file could not be opened or locked at all. Not retried: a retry
    /// would only repeat the failure until the deadline.
    Io(io::Error),
}

/// An exclusive lock on a file, held until this value is dropped (or the process
/// dies).
#[derive(Debug)]
pub(crate) struct FileLock {
    file: File,
    path: PathBuf,
}

impl FileLock {
    /// Take the lock on `path` (creating the file), waiting up to `wait` for the
    /// current holder to let go. `wait` of zero tries exactly once.
    pub(crate) fn acquire(path: &Path, wait: Duration) -> Result<FileLock, LockError> {
        let file = open_lock_file(path).map_err(LockError::Io)?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    stamp(&file);
                    return Ok(FileLock {
                        file,
                        path: path.to_path_buf(),
                    });
                }
                Err(TryLockError::WouldBlock) => {
                    // Reached on every pass that does not get the lock, so the
                    // wait is bounded no matter what else happens.
                    if Instant::now() >= deadline {
                        return Err(LockError::Timeout {
                            holder: read_holder(path),
                        });
                    }
                    std::thread::sleep(POLL);
                }
                Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
            }
        }
    }

    /// The lock file.
    #[allow(dead_code)] // read by tests and by callers that name the lock in a message
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Closing the descriptor would release it too; unlocking first just makes
        // the hand-over immediate when the handle is kept alive by a clone.
        let _ = self.file.unlock();
    }
}

/// Open (creating) the lock file. A build that used a lock *directory* at this
/// path may have left one behind: an empty one is removed so the file can take
/// its place; anything else is the caller's error to report.
fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
    };
    match open() {
        Ok(f) => Ok(f),
        Err(first) => {
            if path.is_dir() {
                fs::remove_dir(path)?;
                open()
            } else {
                Err(first)
            }
        }
    }
}

/// Record the holder's PID in the lock file. Best effort: it is a diagnostic.
fn stamp(file: &File) {
    let _ = file.set_len(0);
    let mut f = file;
    let _ = writeln!(f, "{}", std::process::id());
}

/// The PID last written into the lock file.
fn read_holder(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `" (held by pid N)"`, or nothing when the holder wrote no PID.
pub(crate) fn held_by(holder: Option<u32>) -> String {
    match holder {
        Some(pid) => format!(", held by pid {pid}"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_taker_waits_out_its_deadline_and_names_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let first = FileLock::acquire(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(first.path(), path);

        let t0 = Instant::now();
        let err = FileLock::acquire(&path, Duration::from_millis(300)).unwrap_err();
        let waited = t0.elapsed();
        match err {
            LockError::Timeout { holder } => {
                assert_eq!(
                    holder,
                    Some(std::process::id()),
                    "the holder stamps its pid"
                );
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(
            waited >= Duration::from_millis(300) && waited < Duration::from_secs(5),
            "the wait is bounded by the deadline, not unbounded: {waited:?}"
        );

        drop(first);
        FileLock::acquire(&path, Duration::from_millis(0)).expect("released on drop");
    }

    #[test]
    fn a_zero_wait_tries_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let _held = FileLock::acquire(&path, Duration::ZERO).unwrap();
        let t0 = Instant::now();
        assert!(matches!(
            FileLock::acquire(&path, Duration::ZERO),
            Err(LockError::Timeout { .. })
        ));
        assert!(t0.elapsed() < Duration::from_millis(90), "no sleep at all");
    }

    #[test]
    fn the_lock_file_is_not_deleted_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        drop(FileLock::acquire(&path, Duration::ZERO).unwrap());
        assert!(path.is_file(), "deleting it would let two holders coexist");
    }

    #[test]
    fn an_empty_lock_directory_from_an_older_build_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        fs::create_dir(&path).unwrap();
        let lock = FileLock::acquire(&path, Duration::ZERO).expect("replaced by a file");
        assert!(path.is_file());
        drop(lock);
    }

    #[test]
    fn a_lock_path_that_cannot_be_opened_fails_at_once_instead_of_spinning() {
        let dir = tempfile::tempdir().unwrap();
        // A non-empty directory cannot be replaced by a file. The old scheme
        // `continue`d straight back to create_dir here, with no deadline check
        // and no sleep: 100% CPU for as long as the thing stayed in the way.
        let path = dir.path().join("x.lock");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("occupied"), b"x").unwrap();
        let t0 = Instant::now();
        let err = FileLock::acquire(&path, Duration::from_secs(30)).unwrap_err();
        assert!(matches!(err, LockError::Io(_)), "{err:?}");
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "an unopenable lock is an immediate error, not a 30 s wait"
        );
    }

    #[test]
    fn held_by_renders_the_pid_or_nothing() {
        assert_eq!(held_by(Some(7)), ", held by pid 7");
        assert_eq!(held_by(None), "");
    }

    /// Not a test: the lock holder the next test kills. Started as
    /// `<this binary> --ignored --exact flock::tests::lock_holder_child --nocapture`
    /// with `XERJ_TEST_FLOCK_PATH` naming the lock; prints `locked`, then sleeps.
    #[test]
    #[ignore = "child-process entry point, started by a_killed_holder_never_blocks_the_next_taker"]
    fn lock_holder_child() {
        let Ok(path) = std::env::var("XERJ_TEST_FLOCK_PATH") else {
            return;
        };
        let _lock = FileLock::acquire(Path::new(&path), Duration::from_secs(10)).expect("lock");
        println!("locked");
        let _ = io::stdout().flush();
        std::thread::sleep(Duration::from_secs(120));
    }

    #[cfg(unix)]
    #[test]
    fn a_killed_holder_never_blocks_the_next_taker() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.lock");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "flock::tests::lock_holder_child",
                "--nocapture",
            ])
            .env("XERJ_TEST_FLOCK_PATH", &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let ready = lines.by_ref().map_while(Result::ok).any(|l| l == "locked");
        assert!(ready, "the child never took the lock");

        // While the child lives, the lock is really held.
        assert!(matches!(
            FileLock::acquire(&path, Duration::from_millis(200)),
            Err(LockError::Timeout { holder: Some(p) }) if p == child.id()
        ));

        // SIGKILL: no destructor, no signal handler — exactly an interrupted pull.
        child.kill().unwrap();
        child.wait().unwrap();

        let t0 = Instant::now();
        FileLock::acquire(&path, Duration::from_secs(5)).expect("the dead holder's lock is free");
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "no stale period: the kernel released it with the process"
        );
    }
}
