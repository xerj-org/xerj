//! Index directories that a delete could not remove at once (issue #3274).
//!
//! `DELETE /<index>` used to be one `remove_dir_all` on the index directory.
//! Two field reports showed where that fails while the engine still holds
//! the bytes:
//!
//! * a client killed mid-bulk leaves a flush or merge running, and it keeps
//!   writing segment files into the directory while the removal walks it
//!   (`ENOTEMPTY`);
//! * on NFS an mmap'd segment that is unlinked is silly-renamed to `.nfs*`,
//!   and the directory cannot be removed until the last mapping goes
//!   (`ENOTEMPTY`, then `EBUSY`).
//!
//! Either way the operator got `500 store_exception` and only a restart
//! freed the name. The delete now renames the directory into
//! `<data_dir>/_trash/<name>-<uuid>` (one atomic step: the name is free the
//! moment it returns), then removes the renamed copy. When that removal fails,
//! the delete still succeeds and a reaper thread keeps retrying; whatever it
//! does not finish is swept at the next boot.
//!
//! The same retry-later shape is what tantivy's managed directory does for
//! segment GC: a file that fails to delete (on Windows, because it is
//! mmap'd) is logged and kept on the managed list for the next pass, and a
//! file that is already gone counts as deleted
//! (`tantivy/src/directory/managed_directory.rs:109-200`, MIT; approach
//! adapted, no code copied).
//!
//! `_trash` cannot collide with an index: an index name must start with a
//! lowercase letter or `.` (`IndexName::validate`), and boot discovery only
//! opens directories that hold a `wal/` subdirectory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};

/// Directory under `data_dir` that holds retired index directories.
pub const INDEX_TRASH_DIR: &str = "_trash";

/// What [`retire_dir`] managed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retired {
    /// Nothing is left on disk, under the old name or in the trash.
    Removed,
    /// The name is free, but the bytes now sit at this trash path and a
    /// reaper is retrying their removal.
    Deferred(PathBuf),
}

/// `<data_dir>/_trash`.
pub fn trash_root(data_dir: &Path) -> PathBuf {
    data_dir.join(INDEX_TRASH_DIR)
}

/// Free `dir` (a directory directly under `data_dir`) at once and remove its
/// bytes as far as the filesystem allows.
///
/// Returns `Err` only when the name could not be freed at all: the rename
/// into the trash failed and so did an in-place removal (read-only mount,
/// `EACCES`). The caller keeps the index in service in that case, which is
/// the #206 contract.
pub fn retire_dir(data_dir: &Path, dir: &Path, name: &str) -> std::io::Result<Retired> {
    if !dir.exists() {
        return Ok(Retired::Removed);
    }
    let trash = trash_root(data_dir);
    let dest = trash.join(format!("{name}-{}", uuid::Uuid::new_v4().simple()));
    let renamed = std::fs::create_dir_all(&trash).and_then(|()| std::fs::rename(dir, &dest));
    match renamed {
        Ok(()) => match remove_tree(&dest) {
            Ok(()) => Ok(Retired::Removed),
            Err(e) => {
                warn!(
                    index = name,
                    path = %dest.display(),
                    error = %e,
                    "index directory renamed into the trash but not yet removable; \
                     retrying in the background"
                );
                spawn_reaper(dest.clone());
                Ok(Retired::Deferred(dest))
            }
        },
        Err(rename_err) => {
            // The rename can fail where an in-place removal still works (a
            // trash directory we may not create). Try that before giving up.
            match remove_tree(dir) {
                Ok(()) => Ok(Retired::Removed),
                Err(e) => {
                    warn!(
                        index = name,
                        rename_error = %rename_err,
                        error = %e,
                        "index directory could neither be moved to the trash nor removed"
                    );
                    Err(e)
                }
            }
        }
    }
}

/// `remove_dir_all`, with an already-absent tree counted as removed.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Retry delays for the reaper: 50 ms doubling to a 5 s cap, about ten
/// minutes in all. Past that the boot sweep finishes the job.
fn reaper_delays() -> impl Iterator<Item = Duration> {
    (0..130u32).map(|i| Duration::from_millis((50u64 << i.min(7)).min(5_000)))
}

/// Retry removing `path` on a plain thread. A thread rather than a tokio
/// task: the work is blocking filesystem calls, and it must not depend on a
/// runtime being around (the engine is also driven from sync tests and CLI
/// paths).
fn spawn_reaper(path: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("xerj-trash-reaper".into())
        .spawn(move || {
            for delay in reaper_delays() {
                std::thread::sleep(delay);
                if remove_tree(&path).is_ok() {
                    info!(path = %path.display(), "trashed index directory removed");
                    return;
                }
            }
            warn!(
                path = %path.display(),
                "trashed index directory still not removable; the next boot sweeps it"
            );
        });
    if let Err(e) = spawned {
        warn!(error = %e, "could not start the trash reaper; the next boot sweeps the trash");
    }
}

/// Remove everything under `<data_dir>/_trash`. Runs at boot, before any
/// index is opened, so nothing in it can still be mapped by this process.
/// Returns how many entries could not be removed (each is logged).
pub fn sweep(data_dir: &Path) -> usize {
    let trash = trash_root(data_dir);
    let Ok(entries) = std::fs::read_dir(&trash) else {
        return 0;
    };
    let mut left = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let removed = if path.is_dir() {
            remove_tree(&path)
        } else {
            std::fs::remove_file(&path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })
        };
        match removed {
            Ok(()) => info!(path = %path.display(), "swept trashed index directory"),
            Err(e) => {
                left += 1;
                warn!(path = %path.display(), error = %e, "could not sweep trashed index directory");
            }
        }
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retire_frees_the_name_and_removes_the_bytes() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("idx");
        std::fs::create_dir_all(dir.join("segments")).unwrap();
        std::fs::write(dir.join("segments").join("a.seg"), b"x").unwrap();
        assert_eq!(
            retire_dir(root.path(), &dir, "idx").unwrap(),
            Retired::Removed
        );
        assert!(!dir.exists());
        let left: Vec<_> = std::fs::read_dir(trash_root(root.path()))
            .unwrap()
            .collect();
        assert!(left.is_empty(), "nothing left in the trash: {left:?}");
    }

    #[test]
    fn retire_of_an_absent_directory_is_a_no_op() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("gone");
        assert_eq!(
            retire_dir(root.path(), &dir, "gone").unwrap(),
            Retired::Removed
        );
        assert!(!trash_root(root.path()).exists());
    }

    /// A tree whose removal fails still frees the name; the bytes move to
    /// the trash and the boot sweep removes them once they can go.
    #[cfg(unix)]
    #[test]
    fn an_unremovable_tree_is_deferred_to_the_trash_and_swept_later() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("idx");
        let held = dir.join("held");
        std::fs::create_dir_all(&held).unwrap();
        std::fs::write(held.join("seg"), b"x").unwrap();
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(held.join(".probe"), b"x").is_ok() {
            std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: directory permissions are not enforced for this user");
            return;
        }
        let Retired::Deferred(dest) = retire_dir(root.path(), &dir, "idx").unwrap() else {
            panic!("an unremovable tree must be deferred");
        };
        assert!(!dir.exists(), "the name is free at once");
        assert!(dest.starts_with(trash_root(root.path())));
        std::fs::set_permissions(dest.join("held"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        // The reaper may win the race; either way the sweep leaves nothing.
        assert_eq!(sweep(root.path()), 0);
        assert!(!dest.exists());
    }
}
