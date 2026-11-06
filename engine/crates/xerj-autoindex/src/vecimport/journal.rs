//! Resume journal for a vector import: one small JSON file per (source, index),
//! rewritten atomically after every acknowledged `_bulk` batch and removed when
//! the copy completes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::cli::VecImportCfg;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub source: String,
    pub index: String,
    /// Source cursor to read next; `None` = start.
    pub cursor: Option<String>,
    pub imported: u64,
}

impl State {
    pub fn fresh(source: &str, index: &str) -> Self {
        State {
            source: source.into(),
            index: index.into(),
            cursor: None,
            imported: 0,
        }
    }
}

pub fn path_for(cfg: &VecImportCfg, index: &str) -> PathBuf {
    let dir = cfg.state_dir.clone().unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".xerj")
            .join("vecimport")
    });
    let key = format!("{}\u{0}{}\u{0}{}", cfg.source_display(), cfg.url, index);
    let h = xxhash_rust::xxh3::xxh3_64(key.as_bytes());
    dir.join(format!("{h:016x}.json"))
}

pub fn load(path: &Path) -> Result<Option<State>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| {
            format!(
                "resume journal {} is not readable; pass --fresh",
                path.display()
            )
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, s: &State) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(s)?)?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn finish(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
