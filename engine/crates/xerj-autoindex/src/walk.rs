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
    pub size: u64,
}

pub fn walk(root: &Path, follow_symlinks: bool) -> Result<Vec<FileEntry>> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("resolve root folder {}", root.display()))?;
    if !root_canon.is_dir() {
        anyhow::bail!("{} is not a directory", root_canon.display());
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&root_canon).follow_links(follow_symlinks) {
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
        out.push(FileEntry {
            path: p,
            rel,
            size: md.len(),
        });
    }
    // Deterministic order for clustering / naming.
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}
