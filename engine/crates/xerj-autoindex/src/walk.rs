//! Inventory: recursive walk of the target folder.
//! Symlinks are NOT followed by default; with --follow-symlinks walkdir's
//! ancestor-loop detection keeps the traversal loop-safe.

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

/// A directory pruned whole by the generated-dir hygiene rule, recorded so
/// the run can surface it (junk report / map) instead of skipping silently.
#[derive(Debug, Clone)]
pub struct SkippedDir {
    pub rel: String,
    pub reason: String,
}

/// Well-known generated/cache directories, pruned at ANY depth but ONLY when
/// a sibling marker file proves the parent directory is that kind of project
/// (a monorepo may nest e.g. `apps/game/Library`, `apps/web/node_modules`).
/// Marker-gated like the dotfile rule (hygiene, evidence-based) — never
/// extension guessing, never a per-corpus code path.
/// `--no-default-excludes` disables the rule.
///
/// (dir name, marker path relative to the dir's PARENT, reason)
const GENERATED_DIR_RULES: &[(&str, &str, &str)] = &[
    (
        "Library",
        "ProjectSettings/ProjectVersion.txt",
        "Unity generated/cache directory (marker: sibling ProjectSettings/ProjectVersion.txt)",
    ),
    (
        "Temp",
        "ProjectSettings/ProjectVersion.txt",
        "Unity generated/cache directory (marker: sibling ProjectSettings/ProjectVersion.txt)",
    ),
    (
        "obj",
        "ProjectSettings/ProjectVersion.txt",
        "Unity generated/cache directory (marker: sibling ProjectSettings/ProjectVersion.txt)",
    ),
    (
        "Logs",
        "ProjectSettings/ProjectVersion.txt",
        "Unity generated/cache directory (marker: sibling ProjectSettings/ProjectVersion.txt)",
    ),
    (
        "UserSettings",
        "ProjectSettings/ProjectVersion.txt",
        "Unity generated/cache directory (marker: sibling ProjectSettings/ProjectVersion.txt)",
    ),
    (
        "node_modules",
        "package.json",
        "installed npm/yarn dependency tree (marker: sibling package.json)",
    ),
    (
        "target",
        "Cargo.toml",
        "Cargo build directory (marker: sibling Cargo.toml)",
    ),
];

pub fn walk(
    root: &Path,
    follow_symlinks: bool,
    default_excludes: bool,
) -> Result<(Vec<FileEntry>, Vec<SkippedDir>)> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("resolve root folder {}", root.display()))?;
    if !root_canon.is_dir() {
        anyhow::bail!("{} is not a directory", root_canon.display());
    }
    let skipped: std::rc::Rc<std::cell::RefCell<Vec<SkippedDir>>> = Default::default();
    let mut out = Vec::new();
    // SECURITY / hygiene: never index hidden files or descend into hidden
    // directories. Without this the walker happily indexed `.env` (secrets,
    // API tokens), `.git`, `.ssh`, `.aws`, and other dotfiles into a queryable
    // brain with no per-brain authorization — a real exposure for the
    // "point it at my project folder" use case. `filter_entry` prunes a hidden
    // directory before descending, so `.git/` is skipped whole. The root itself
    // (depth 0) is exempt so a brain over a dot-named folder still works.
    let sk = std::rc::Rc::clone(&skipped);
    let filter_root = root_canon.clone();
    let walker = walkdir::WalkDir::new(&root_canon)
        .follow_links(follow_symlinks)
        .into_iter()
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            if e.file_name().to_str().is_some_and(|n| n.starts_with('.')) {
                return false;
            }
            if default_excludes && e.file_type().is_dir() {
                for (name, marker, reason) in GENERATED_DIR_RULES {
                    if e.file_name() == *name
                        && e.path()
                            .parent()
                            .is_some_and(|p| p.join(marker).is_file())
                    {
                        let rel = e
                            .path()
                            .strip_prefix(&filter_root)
                            .unwrap_or(e.path())
                            .to_string_lossy()
                            .replace('\\', "/");
                        sk.borrow_mut().push(SkippedDir {
                            rel,
                            reason: (*reason).to_string(),
                        });
                        return false;
                    }
                }
            }
            true
        });
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk: skipping unreadable entry: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
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
    let skipped = std::rc::Rc::try_unwrap(skipped)
        .expect("walker iterator consumed; sole owner remains")
        .into_inner();
    Ok((out, skipped))
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

        let rels: Vec<String> = walk(root, false, true)
            .unwrap()
            .0
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

    #[test]
    fn a_brain_over_a_dot_named_root_still_works() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join(".notes");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.md"), "x").unwrap();
        let rels: Vec<String> = walk(&root, false, true)
            .unwrap()
            .0
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
mod generated_dir_tests {
    use super::walk;
    use std::fs;
    use std::path::Path;

    fn make_unity_root(root: &Path, with_marker: bool) {
        fs::create_dir_all(root.join("Assets")).unwrap();
        fs::write(root.join("Assets/Player.cs"), "class Player {}").unwrap();
        fs::create_dir_all(root.join("Library/Artifacts")).unwrap();
        fs::write(root.join("Library/Artifacts/blob.bin"), b"\x00\x01").unwrap();
        fs::create_dir_all(root.join("Temp")).unwrap();
        fs::write(root.join("Temp/scratch"), "x").unwrap();
        if with_marker {
            fs::create_dir_all(root.join("ProjectSettings")).unwrap();
            fs::write(
                root.join("ProjectSettings/ProjectVersion.txt"),
                "m_EditorVersion: 2022.3.10f1\n",
            )
            .unwrap();
        }
    }

    #[test]
    fn unity_marker_prunes_generated_dirs_and_records_them() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_root(dir.path(), true);
        let (files, skipped) = walk(dir.path(), false, true).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"Assets/Player.cs"));
        assert!(
            !rels.iter().any(|r| r.starts_with("Library/")),
            "Library/ must be pruned whole: {rels:?}"
        );
        assert!(!rels.iter().any(|r| r.starts_with("Temp/")));
        let mut skipped_rels: Vec<&str> = skipped.iter().map(|s| s.rel.as_str()).collect();
        skipped_rels.sort();
        assert_eq!(skipped_rels, ["Library", "Temp"], "each pruned dir is recorded");
    }

    #[test]
    fn without_the_marker_nothing_is_pruned() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_root(dir.path(), false);
        let (files, skipped) = walk(dir.path(), false, true).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            rels.iter().any(|r| r.starts_with("Library/")),
            "a folder merely NAMED Library is not evidence: {rels:?}"
        );
        assert!(skipped.is_empty());
    }

    #[test]
    fn no_default_excludes_walks_everything() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_root(dir.path(), true);
        let (files, skipped) = walk(dir.path(), false, false).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.iter().any(|r| r.starts_with("Library/")));
        assert!(skipped.is_empty());
    }

    #[test]
    fn a_file_named_like_a_generated_dir_is_not_pruned() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_root(dir.path(), true);
        fs::write(dir.path().join("obj"), "a FILE named obj").unwrap();
        let (files, _) = walk(dir.path(), false, true).unwrap();
        assert!(
            files.iter().any(|e| e.rel == "obj"),
            "the prune applies to directories only"
        );
    }

    /// Monorepo case: the Unity project is a SUBFOLDER of the walk root. The
    /// marker is checked against each candidate's parent, so nesting depth is
    /// irrelevant.
    #[test]
    fn a_nested_unity_project_is_pruned_by_its_sibling_marker() {
        let dir = tempfile::TempDir::new().unwrap();
        let unity = dir.path().join("apps/game");
        fs::create_dir_all(&unity).unwrap();
        make_unity_root(&unity, true);
        fs::write(dir.path().join("README.md"), "monorepo").unwrap();
        let (files, skipped) = walk(dir.path(), false, true).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"apps/game/Assets/Player.cs"));
        assert!(!rels.iter().any(|r| r.contains("game/Library/")));
        assert!(skipped.iter().any(|s| s.rel == "apps/game/Library"));
    }

    #[test]
    fn node_modules_is_pruned_only_next_to_a_package_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let app = dir.path().join("web");
        fs::create_dir_all(app.join("node_modules/lodash")).unwrap();
        fs::write(app.join("node_modules/lodash/index.js"), "x").unwrap();
        fs::write(app.join("package.json"), "{}").unwrap();
        fs::write(app.join("index.js"), "x").unwrap();
        let bare = dir.path().join("data/node_modules");
        fs::create_dir_all(&bare).unwrap();
        fs::write(bare.join("notes.txt"), "not an npm tree").unwrap();
        let (files, skipped) = walk(dir.path(), false, true).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"web/index.js"));
        assert!(!rels.iter().any(|r| r.starts_with("web/node_modules")));
        assert!(
            rels.contains(&"data/node_modules/notes.txt"),
            "without the package.json marker there is no evidence: {rels:?}"
        );
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].rel, "web/node_modules");
    }

    #[test]
    fn cargo_target_is_pruned_only_next_to_a_cargo_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let crate_dir = dir.path().join("mycrate");
        fs::create_dir_all(crate_dir.join("target/debug")).unwrap();
        fs::write(crate_dir.join("target/debug/build.log"), "x").unwrap();
        fs::write(crate_dir.join("Cargo.toml"), "[package]").unwrap();
        fs::write(crate_dir.join("main.rs"), "fn main() {}").unwrap();
        let plain = dir.path().join("shooting/target");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("scores.csv"), "a,b\n1,2\n").unwrap();
        let (files, skipped) = walk(dir.path(), false, true).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"mycrate/main.rs"));
        assert!(!rels.iter().any(|r| r.starts_with("mycrate/target")));
        assert!(
            rels.contains(&"shooting/target/scores.csv"),
            "a folder merely NAMED target is data, not a build dir: {rels:?}"
        );
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].rel, "mycrate/target");
    }
}
