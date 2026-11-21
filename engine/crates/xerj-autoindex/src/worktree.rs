//! Git worktree resolution and index isolation/sharing modes (`--worktree shared|isolated`, #2967).
//!
//! Parallel AI agents increasingly run in linked Git worktrees of the same
//! repository (`git worktree add`, `worktrunk`, Copilot/Codex worktrees).
//! Without an explicit worktree policy, two failure modes arise:
//!
//! 1. **Unintended prefix collisions (`default --prefix ax`)**: two agents in
//!    `wt-a` and `wt-b` both default to `--prefix ax`; the second run hits the
//!    `foreign_prefix_roots` refusal (`prefix 'ax' already holds datasets from
//!    wt-a`) or, with `--replace-prefix`, clobbers `wt-a`'s index.
//! 2. **Redundant full-repo indexes when a shared baseline is wanted**: teams
//!    that want every worktree agent to share one index rooted at the main
//!    checkout must manually discover the `.git/worktrees/<name>/commondir`
//!    target and pass matching `--root` and `--prefix` flags.
//!
//! `--worktree shared|isolated` solves both deterministically on local disk:
//! - `--worktree shared`: resolves a linked Git worktree back to its main
//!   repository checkout via `.git` (`gitdir:` + `commondir`), rewrites the
//!   index root to the main checkout, and (when `--prefix` was not explicitly
//!   given) derives the shared prefix `ax-<main-repo-slug>`. Concurrent runs
//!   against the shared root serialize through the existing `StateLock` in
//!   `~/.xerj/autoindex/<key>/state.lock`.
//! - `--worktree isolated`: keeps the worktree directory itself as the index
//!   root and (when `--prefix` was not explicitly given) derives an isolated
//!   per-worktree prefix `ax-<main-repo-slug>-<worktree-slug>` (or
//!   `ax-<folder-slug>` for a standalone checkout), giving each worktree its
//!   own prefix and resume journal with zero lock contention.

use std::fs;
use std::path::{Path, PathBuf};

/// Worktree index policy selected by `--worktree <shared|isolated>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeMode {
    /// Share one index and resume journal rooted at the main Git checkout.
    Shared,
    /// Keep an isolated index and resume journal per worktree with a
    /// worktree-scoped default prefix.
    Isolated,
}

impl WorktreeMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "shared" => Ok(WorktreeMode::Shared),
            "isolated" => Ok(WorktreeMode::Isolated),
            other => Err(format!(
                "--worktree {other} is not valid; expected `shared` or `isolated`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WorktreeMode::Shared => "shared",
            WorktreeMode::Isolated => "isolated",
        }
    }
}

/// Resolved root, prefix, and startup note after applying [`WorktreeMode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeResolution {
    pub root: PathBuf,
    pub prefix: String,
    pub note: String,
}

/// Inspect `dir/.git` (or an ancestor) to determine whether `dir` is a linked
/// Git worktree, returning `Some(main_checkout_root)` when `.git` is a
/// `gitdir:` pointer file whose `commondir` resolves to a main `.git` directory.
pub fn resolve_main_checkout(dir: &Path) -> Option<PathBuf> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let dot_git = canon.join(".git");
    if !dot_git.is_file() {
        return None;
    }
    let content = fs::read_to_string(&dot_git).ok()?;
    let gitdir_line = content
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if gitdir_line.is_empty() {
        return None;
    }
    let gitdir_raw = PathBuf::from(gitdir_line);
    let gitdir = if gitdir_raw.is_absolute() {
        gitdir_raw
    } else {
        canon.join(gitdir_raw)
    };
    let commondir_file = gitdir.join("commondir");
    let common_git_dir = if commondir_file.is_file() {
        let rel = fs::read_to_string(&commondir_file).ok()?;
        let rel = rel.lines().next()?.trim();
        if rel.is_empty() {
            return None;
        }
        let p = PathBuf::from(rel);
        if p.is_absolute() {
            p
        } else {
            gitdir.join(p)
        }
    } else if gitdir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("worktrees")
    {
        gitdir.parent()?.parent()?.to_path_buf()
    } else {
        return None;
    };
    let common_canon = common_git_dir
        .canonicalize()
        .unwrap_or_else(|_| common_git_dir.clone());
    if common_canon.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common_canon.parent().map(Path::to_path_buf)
    } else {
        Some(common_canon)
    }
}

fn dir_slug(path: &Path) -> String {
    let raw = path.file_name().and_then(|n| n.to_str()).unwrap_or("repo");
    let s = crate::dataset::sanitize_slug(raw);
    if s.is_empty() {
        "repo".to_string()
    } else {
        s
    }
}

fn cap_prefix(mut p: String) -> String {
    const MAX_PREFIX_LEN: usize = 48;
    if p.len() > MAX_PREFIX_LEN {
        p.truncate(MAX_PREFIX_LEN);
        p = p.trim_end_matches('-').to_string();
    }
    p
}

/// Apply `mode` (`shared` or `isolated`) to `root` and `prefix`.
pub fn resolve_worktree(
    root: &Path,
    mode: WorktreeMode,
    prefix_explicit: bool,
    current_prefix: &str,
) -> WorktreeResolution {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let linked_main = resolve_main_checkout(&canon_root);
    match mode {
        WorktreeMode::Shared => {
            let effective_root = linked_main.clone().unwrap_or_else(|| canon_root.clone());
            let main_slug = dir_slug(&effective_root);
            let prefix = if prefix_explicit {
                current_prefix.to_string()
            } else {
                cap_prefix(format!("ax-{main_slug}"))
            };
            let note = if linked_main.is_some() {
                format!(
                    "worktree=shared: linked worktree {} shares main checkout {} (prefix '{prefix}')",
                    canon_root.display(),
                    effective_root.display()
                )
            } else {
                format!(
                    "worktree=shared: indexing checkout {} under shared prefix '{prefix}'",
                    effective_root.display()
                )
            };
            WorktreeResolution {
                root: effective_root,
                prefix,
                note,
            }
        }
        WorktreeMode::Isolated => {
            let wt_slug = dir_slug(&canon_root);
            let prefix = if prefix_explicit {
                current_prefix.to_string()
            } else if let Some(main) = &linked_main {
                let main_slug = dir_slug(main);
                if main_slug == wt_slug {
                    cap_prefix(format!("ax-{wt_slug}"))
                } else {
                    cap_prefix(format!("ax-{main_slug}-{wt_slug}"))
                }
            } else {
                cap_prefix(format!("ax-{wt_slug}"))
            };
            let note = format!(
                "worktree=isolated: indexing worktree {} under isolated prefix '{prefix}'",
                canon_root.display()
            );
            WorktreeResolution {
                root: root.to_path_buf(),
                prefix,
                note,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_linked_git_worktree_in_shared_and_isolated_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let main_repo = tmp.path().join("acme-engine");
        let main_git = main_repo.join(".git");
        let wt_gitdir = main_git.join("worktrees").join("agent-alpha");
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();

        let wt_dir = tmp.path().join("worktrees").join("agent-alpha");
        std::fs::create_dir_all(&wt_dir).unwrap();
        std::fs::write(
            wt_dir.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        let wt_beta_gitdir = main_git.join("worktrees").join("agent-beta");
        std::fs::create_dir_all(&wt_beta_gitdir).unwrap();
        std::fs::write(wt_beta_gitdir.join("commondir"), "../..\n").unwrap();
        let wt_beta = tmp.path().join("worktrees").join("agent-beta");
        std::fs::create_dir_all(&wt_beta).unwrap();
        std::fs::write(
            wt_beta.join(".git"),
            format!("gitdir: {}\n", wt_beta_gitdir.display()),
        )
        .unwrap();

        // Shared mode: both linked worktrees resolve to the same main repo root and prefix
        let shared_a = resolve_worktree(&wt_dir, WorktreeMode::Shared, false, "ax");
        let shared_b = resolve_worktree(&wt_beta, WorktreeMode::Shared, false, "ax");
        assert_eq!(
            shared_a.root,
            main_repo.canonicalize().unwrap(),
            "shared mode must redirect to main checkout"
        );
        assert_eq!(shared_a.root, shared_b.root);
        assert_eq!(shared_a.prefix, "ax-acme-engine");
        assert_eq!(shared_b.prefix, "ax-acme-engine");

        // Isolated mode: each worktree keeps its own root and gets a distinct prefix
        let iso_a = resolve_worktree(&wt_dir, WorktreeMode::Isolated, false, "ax");
        let iso_b = resolve_worktree(&wt_beta, WorktreeMode::Isolated, false, "ax");
        assert_eq!(iso_a.root, wt_dir);
        assert_eq!(iso_b.root, wt_beta);
        assert_eq!(iso_a.prefix, "ax-acme-engine-agent-alpha");
        assert_eq!(iso_b.prefix, "ax-acme-engine-agent-beta");
        assert_ne!(iso_a.prefix, iso_b.prefix);

        // Explicit --prefix is always respected
        let explicit = resolve_worktree(&wt_dir, WorktreeMode::Isolated, true, "custom-wt");
        assert_eq!(explicit.prefix, "custom-wt");
    }
}
