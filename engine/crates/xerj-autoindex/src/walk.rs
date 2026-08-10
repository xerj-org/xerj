//! Inventory: recursive walk of the target folder.
//! Symlinks are NOT followed by default; with --follow-symlinks walkdir's
//! ancestor-loop detection keeps the traversal loop-safe.
//!
//! Ignore rules (`.gitignore`, `.xerjignore`, built-in build-output defaults)
//! are applied *during* the walk, not after it: an ignored directory is never
//! descended, so its contents cost nothing — no stat, no hash, no bulk body.
//! The matching itself lives in [`crate::ignore_rules`].

use crate::ignore_rules::{IgnoreOptions, IgnoreReport, IgnoreStack};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    /// Root-relative path with forward slashes — the stable `ax_path` value.
    pub rel: String,
    /// Reversible platform-native identity; unlike `rel`, never lossy.
    pub rel_id: String,
    /// True when the discovered path itself is a symlink (target metadata follows).
    pub is_symlink: bool,
    pub size: u64,
}

/// Generated/cache directories whose NAMES are too common for the blunt
/// [`crate::ignore_rules::DEFAULT_IGNORE_PATTERNS`] (a folder named `Logs/`
/// or `Library/` is often real data), pruned at any depth ONLY when a sibling
/// marker file proves the parent directory is that kind of project. Part of
/// the built-in defaults: `--no-ignore` / `--no-default-ignores` disable it,
/// and any ignore file in the tree can re-include (`!Library/`) as usual —
/// these run after the ignore files have had their say.
///
/// (dir name, marker path relative to the dir's PARENT, report label)
const MARKER_GENERATED_DIRS: &[(&str, &str, &str)] = &[
    (
        "Library",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "Temp",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "obj",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "Logs",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "UserSettings",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
];

/// The marker-gated verdict for one directory entry.
fn marker_generated_dir(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?;
    for (dir, marker, label) in MARKER_GENERATED_DIRS {
        if name == *dir && path.parent().is_some_and(|p| p.join(marker).is_file()) {
            return Some(label);
        }
    }
    None
}

/// Walk with the default ignore rules on and the report discarded.
pub fn walk(root: &Path, follow_symlinks: bool) -> Result<Vec<FileEntry>> {
    walk_reporting(root, follow_symlinks, IgnoreOptions::default()).map(|(files, _)| files)
}

/// Walk, and say what was left out and why.
///
/// The returned [`IgnoreReport`] is the answer to "my files vanished" — it
/// names every rule that discarded something and how much. It is reported on
/// every run and, under `--dry-run`, also counts what was inside each pruned
/// directory.
pub fn walk_reporting(
    root: &Path,
    follow_symlinks: bool,
    ignore: IgnoreOptions,
) -> Result<(Vec<FileEntry>, IgnoreReport)> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("resolve root folder {}", root.display()))?;
    if !root_canon.is_dir() {
        anyhow::bail!("{} is not a directory", root_canon.display());
    }
    let mut out = Vec::new();
    let marker_defaults = ignore.enabled && ignore.defaults;
    let mut stack = IgnoreStack::new(&root_canon, ignore);
    // Manual iteration rather than `filter_entry`, because the ignore stack has
    // to load a directory's ignore files *after* that directory is admitted and
    // *before* its children are judged. `skip_current_dir` on a just-yielded
    // directory prunes its contents — walkdir's own documented pattern, and the
    // same effect `filter_entry` had.
    let mut it = walkdir::WalkDir::new(&root_canon)
        .follow_links(follow_symlinks)
        .into_iter();
    while let Some(next) = it.next() {
        let entry = match next {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk: skipping unreadable entry: {e}");
                continue;
            }
        };
        let is_dir = entry.file_type().is_dir();
        // Retire the layers of directories this entry is no longer inside; see
        // `IgnoreStack::truncate_to` for why every entry needs it, not just
        // directories.
        stack.truncate_to(entry.depth());
        // SECURITY / hygiene: never index hidden files or descend into hidden
        // directories. Without this the walker happily indexed `.env` (secrets,
        // API tokens), `.git`, `.ssh`, `.aws`, and other dotfiles into a
        // queryable brain with no per-brain authorization — a real exposure for
        // the "point it at my project folder" use case. A hidden directory is
        // pruned before descending, so `.git/` is skipped whole. The root
        // itself (depth 0) is exempt so a brain over a dot-named folder still
        // works. `--no-ignore` does NOT turn this off: it is not one of the
        // ignore rules, it is the reason secrets stay out.
        if entry.depth() > 0
            && entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with('.'))
        {
            stack.record_hidden(entry.path(), is_dir);
            if is_dir {
                it.skip_current_dir();
            }
            continue;
        }
        if is_dir {
            // The root is never judged by the rules below it: pointing
            // autoindex at a folder is an explicit instruction, and the note
            // explaining that it *would* have been ignored is on the report.
            if entry.depth() > 0 && stack.skip_dir(entry.path()).is_some() {
                it.skip_current_dir();
                continue;
            }
            // Marker-gated generated dirs (part of the built-in defaults, so
            // `--no-ignore` / `--no-default-ignores` disable it): names like
            // Unity's `Library/` or `Logs/` are too common for the blunt
            // DEFAULT_IGNORE_PATTERNS, so they are pruned only when a sibling
            // marker file proves the parent is that kind of project.
            if entry.depth() > 0 && marker_defaults {
                if let Some(label) = marker_generated_dir(entry.path()) {
                    stack.record_marker_dir(entry.path(), label);
                    it.skip_current_dir();
                    continue;
                }
            }
            stack.enter_dir(entry.path(), entry.depth());
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if stack.skip_file(entry.path()).is_some() {
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let p = entry.path().to_path_buf();
        let rel = p
            .strip_prefix(&root_canon)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let rel_id = stable_path_id(p.strip_prefix(&root_canon).unwrap_or(&p));
        out.push(FileEntry {
            path: p,
            rel,
            rel_id,
            is_symlink: entry
                .path()
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink()),
            size: md.len(),
        });
    }
    // Deterministic order for clustering / naming.
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((out, stack.report))
}

#[cfg(unix)]
fn stable_path_id(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("unix:");
    for byte in path.as_os_str().as_bytes() {
        use std::fmt::Write;
        write!(out, "{byte:02x}").expect("write string");
    }
    out
}

#[cfg(windows)]
fn stable_path_id(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    let mut out = String::from("windows:");
    for unit in path.as_os_str().encode_wide() {
        use std::fmt::Write;
        write!(out, "{unit:04x}").expect("write string");
    }
    out
}

#[cfg(not(any(unix, windows)))]
fn stable_path_id(path: &Path) -> String {
    format!("other:{}", path.to_string_lossy())
}

#[cfg(test)]
mod hidden_skip_tests {
    use super::walk;
    use std::fs;

    #[test]
    fn hidden_files_and_dirs_are_never_indexed() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join(".env"), "SECRET=pat-abc123").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("config"), "[core]").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src").join(".secret"), "nope").unwrap();

        let rels: Vec<String> = walk(root, false)
            .unwrap()
            .into_iter()
            .map(|e| e.rel)
            .collect();
        assert!(rels.contains(&"README.md".to_string()));
        assert!(rels.contains(&"src/main.rs".to_string()));
        // secrets and VCS internals must not be present
        assert!(!rels.iter().any(|r| r == ".env"), "indexed .env: {rels:?}");
        assert!(
            !rels.iter().any(|r| r.starts_with(".git")),
            "descended .git: {rels:?}"
        );
        assert!(
            !rels.iter().any(|r| r.ends_with(".secret")),
            "indexed a nested dotfile: {rels:?}"
        );
    }

    /// #276: the walk itself must drop gitignored junk, not a later stage —
    /// an ignored directory is never descended, so it never reaches hashing.
    #[test]
    fn the_walk_honours_gitignore_and_the_built_in_defaults() {
        use super::walk_reporting;
        use crate::ignore_rules::IgnoreOptions;

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "*.tmp\ndocs/private/\n").unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join("scratch.tmp"), "junk").unwrap();
        fs::create_dir_all(root.join("docs/private")).unwrap();
        fs::write(root.join("docs/private/salary.md"), "secret").unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/huge.rlib"), "bytes").unwrap();

        let (files, report) = walk_reporting(root, false, IgnoreOptions::default()).unwrap();
        let rels: Vec<String> = files.into_iter().map(|e| e.rel).collect();
        assert_eq!(rels, vec!["README.md".to_string()], "{rels:?}");
        // 2 = scratch.tmp, plus `.gitignore` itself under the (separate,
        // non-optional) hidden-file rule.
        assert_eq!(report.files_skipped, 2, "{report:?}");
        assert_eq!(report.dirs_pruned, 2, "{report:?}");
        assert_eq!(report.ignore_files_read, 1, "{report:?}");

        // --no-ignore brings all of it back, except the dotfiles.
        let (all, off) = walk_reporting(root, false, IgnoreOptions::off()).unwrap();
        let mut rels: Vec<String> = all.into_iter().map(|e| e.rel).collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "README.md".to_string(),
                "docs/private/salary.md".to_string(),
                "scratch.tmp".to_string(),
                "target/debug/huge.rlib".to_string(),
            ],
            "{rels:?}"
        );
        assert_eq!(off.dirs_pruned, 0, "{off:?}");
    }

    #[test]
    fn a_brain_over_a_dot_named_root_still_works() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join(".notes");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.md"), "x").unwrap();
        let rels: Vec<String> = walk(&root, false)
            .unwrap()
            .into_iter()
            .map(|e| e.rel)
            .collect();
        assert_eq!(
            rels,
            vec!["a.md".to_string()],
            "root exemption failed: {rels:?}"
        );
    }
}

#[cfg(test)]
mod marker_generated_dir_tests {
    use super::{walk, walk_reporting};
    use crate::ignore_rules::IgnoreOptions;
    use std::fs;
    use std::path::Path;

    fn make_unity_project(root: &Path, with_marker: bool) {
        fs::create_dir_all(root.join("Assets")).unwrap();
        fs::write(root.join("Assets/Player.cs"), "class Player {}").unwrap();
        fs::create_dir_all(root.join("Library/Artifacts")).unwrap();
        fs::write(root.join("Library/Artifacts/blob.bin"), b"\x00\x01").unwrap();
        fs::create_dir_all(root.join("Logs")).unwrap();
        fs::write(root.join("Logs/editor.log"), "x").unwrap();
        if with_marker {
            fs::create_dir_all(root.join("ProjectSettings")).unwrap();
            fs::write(
                root.join("ProjectSettings/ProjectVersion.txt"),
                "m_EditorVersion: 2022.3.10f1\n",
            )
            .unwrap();
        }
    }

    /// Monorepo case: the Unity project is a SUBFOLDER of the walk root; the
    /// sibling marker decides at any depth.
    #[test]
    fn a_nested_unity_project_is_pruned_and_reported() {
        let dir = tempfile::TempDir::new().unwrap();
        let unity = dir.path().join("platform/unity");
        fs::create_dir_all(&unity).unwrap();
        make_unity_project(&unity, true);
        let (files, report) =
            walk_reporting(dir.path(), false, IgnoreOptions::default()).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"platform/unity/Assets/Player.cs"));
        assert!(
            !rels.iter().any(|r| r.contains("/Library/")),
            "Library/ must be pruned whole: {rels:?}"
        );
        assert!(
            report
                .by_rule
                .keys()
                .any(|k| k == "default:unity-generated"),
            "the prune must be named on the report: {report:?}"
        );
    }

    /// Without the marker, a folder merely NAMED Library or Logs is data.
    #[test]
    fn without_the_marker_nothing_is_pruned() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_project(dir.path(), false);
        let files = walk(dir.path(), false).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"Library/Artifacts/blob.bin"), "{rels:?}");
        assert!(rels.contains(&"Logs/editor.log"), "{rels:?}");
    }

    /// The marker rule is one of the built-in defaults, so both switches
    /// disable it.
    #[test]
    fn no_ignore_and_no_default_ignores_both_disable_the_marker_rule() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_project(dir.path(), true);
        let no_defaults = IgnoreOptions {
            defaults: false,
            ..IgnoreOptions::default()
        };
        for opts in [IgnoreOptions::off(), no_defaults] {
            let (files, _) = walk_reporting(dir.path(), false, opts).unwrap();
            let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
            assert!(
                rels.contains(&"Library/Artifacts/blob.bin"),
                "opts {opts:?} must walk generated dirs: {rels:?}"
            );
        }
    }

    #[test]
    fn a_file_named_like_a_generated_dir_is_not_pruned() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_project(dir.path(), true);
        fs::write(dir.path().join("obj"), "a FILE named obj").unwrap();
        let files = walk(dir.path(), false).unwrap();
        assert!(
            files.iter().any(|e| e.rel == "obj"),
            "the prune applies to directories only"
        );
    }
}
