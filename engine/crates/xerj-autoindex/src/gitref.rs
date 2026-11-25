//! Read-only git commit lookup for an indexed folder (#3268).
//!
//! `xerj status` and `xerj_map` compare the commit a folder was at when it
//! was indexed with the commit it is at now. Neither the git binary nor a
//! git library is needed: `HEAD`, the ref it names (loose or packed), and
//! the HEAD reflog are plain text files. The commit at index time is the
//! last HEAD reflog entry written at or before the index stamp. That is the
//! commit HEAD pointed at then, as long as the reflog still holds that
//! entry (git expires reflog entries after 90 days by default). When it
//! does not, the answer is `None`, not a guess.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Largest reflog read. A HEAD reflog is ~150 bytes per checkout/commit.
const REFLOG_CAP: u64 = 16 << 20;
/// Ancestors walked looking for `.git`.
const MAX_ANCESTORS: usize = 64;

/// The git directory for `dir`, and the common directory that holds shared
/// refs (the same directory outside a linked worktree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirs {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// Find the repository `dir` belongs to: the nearest ancestor with a `.git`
/// directory, or a `.git` file naming one (`gitdir: …`, a linked worktree or
/// a submodule).
pub fn git_dirs(dir: &Path) -> Option<GitDirs> {
    let mut cur = Some(dir);
    for _ in 0..MAX_ANCESTORS {
        let here = cur?;
        let dot_git = here.join(".git");
        let git_dir = if dot_git.is_dir() {
            Some(dot_git)
        } else if dot_git.is_file() {
            let text = fs::read_to_string(&dot_git).ok()?;
            let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
            let target = Path::new(target);
            Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                here.join(target)
            })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            let common_dir = fs::read_to_string(git_dir.join("commondir"))
                .ok()
                .map(|c| {
                    let c = Path::new(c.trim());
                    if c.is_absolute() {
                        c.to_path_buf()
                    } else {
                        git_dir.join(c)
                    }
                })
                .unwrap_or_else(|| git_dir.clone());
            return Some(GitDirs {
                git_dir,
                common_dir,
            });
        }
        cur = here.parent();
    }
    None
}

fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn resolve_ref(dirs: &GitDirs, name: &str) -> Option<String> {
    for base in [&dirs.git_dir, &dirs.common_dir] {
        if let Ok(text) = fs::read_to_string(base.join(name)) {
            let text = text.trim();
            if is_sha(text) {
                return Some(text.to_string());
            }
        }
    }
    let packed = fs::read_to_string(dirs.common_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (sha, refname) = line.split_once(' ')?;
        (refname.trim() == name && is_sha(sha)).then(|| sha.to_string())
    })
}

/// The commit `HEAD` names now, or `None` outside a repository, on an
/// unborn branch, or when the ref cannot be read.
pub fn head_now(dir: &Path) -> Option<String> {
    let dirs = git_dirs(dir)?;
    let head = fs::read_to_string(dirs.git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if is_sha(head) {
        return Some(head.to_string());
    }
    let name = head.strip_prefix("ref:")?.trim();
    if name.contains("..") {
        return None;
    }
    resolve_ref(&dirs, name)
}

/// The commit `HEAD` pointed at `when`, from the HEAD reflog. `None` when
/// there is no reflog, or its oldest entry is newer than `when`.
pub fn head_at(dir: &Path, when: SystemTime) -> Option<String> {
    let dirs = git_dirs(dir)?;
    let path = dirs.git_dir.join("logs").join("HEAD");
    if fs::metadata(&path).ok()?.len() > REFLOG_CAP {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let cutoff = when.duration_since(UNIX_EPOCH).ok()?.as_secs();
    head_from_reflog(&text, cutoff)
}

/// Parse a HEAD reflog: `<old> <new> <name> <<email>> <unix> <tz>\t<msg>`.
/// Entries are appended in time order, so the last one at or before
/// `cutoff` wins.
fn head_from_reflog(text: &str, cutoff: u64) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let head = line.split('\t').next().unwrap_or(line);
        let mut parts = head.split(' ');
        let (Some(_old), Some(new)) = (parts.next(), parts.next()) else {
            continue;
        };
        let mut tail = head.rsplit(' ');
        let (Some(_tz), Some(ts)) = (tail.next(), tail.next()) else {
            continue;
        };
        let Ok(ts) = ts.parse::<u64>() else {
            continue;
        };
        if ts <= cutoff && is_sha(new) && !new.bytes().all(|b| b == b'0') {
            found = Some(new.to_string());
        }
    }
    found
}

/// The first 12 characters of a commit id, for display.
pub fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const C: &str = "3333333333333333333333333333333333333333";

    #[test]
    fn reflog_picks_the_last_entry_at_or_before_the_stamp() {
        let zero = "0".repeat(40);
        let log = format!(
            "{zero} {A} Dev Name <d@example.com> 1000 +0000\tclone\n\
             {A} {B} Dev Name <d@example.com> 2000 +0100\tcommit: two\n\
             {B} {C} Dev <d@example.com> 3000 -0500\tcommit: three\n"
        );
        assert_eq!(head_from_reflog(&log, 999), None);
        assert_eq!(head_from_reflog(&log, 1000).as_deref(), Some(A));
        assert_eq!(head_from_reflog(&log, 2500).as_deref(), Some(B));
        assert_eq!(head_from_reflog(&log, 9999).as_deref(), Some(C));
    }

    #[test]
    fn head_resolves_loose_packed_and_worktree_refs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let git = repo.join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::create_dir_all(git.join("logs")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(git.join("refs/heads/main"), format!("{B}\n")).unwrap();
        fs::write(
            git.join("logs/HEAD"),
            format!("{A} {B} D <d@e> 5 +0000\tcommit\n"),
        )
        .unwrap();
        let sub = repo.join("src/deep");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(head_now(&sub).as_deref(), Some(B));
        assert_eq!(
            head_at(&sub, UNIX_EPOCH + Duration::from_secs(6)).as_deref(),
            Some(B)
        );

        fs::remove_file(git.join("refs/heads/main")).unwrap();
        fs::write(
            git.join("packed-refs"),
            format!("# pack-refs with: peeled\n{C} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(head_now(&repo).as_deref(), Some(C));

        // A linked worktree: `.git` is a file, refs live in the common dir.
        let wt_git = git.join("worktrees/wt");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = dir.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(head_now(&wt).as_deref(), Some(C));

        let outside = tempfile::tempdir().unwrap();
        assert_ne!(head_now(outside.path()).as_deref(), Some(C));
    }
}
