//! Durable state: `<data_dir>/_sources/<name>/{spec.json,state.json}`.
//!
//! `spec.json` is what the operator asked for; `state.json` is how far the
//! worker got (phase, confirmed LSN, whether it created the slot and the
//! publication, so `DELETE` can clean up exactly what it made). Both are
//! written atomically (temp file + rename) so a crash leaves the previous
//! version, never a torn one.

use crate::spec::SourceSpec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No consistent snapshot has completed. A restart in this phase starts
    /// the snapshot over (the exported snapshot dies with its connection).
    #[default]
    Snapshot,
    /// The snapshot completed; changes stream from `lsn`.
    Streaming,
    /// Watermark polling (no replication available).
    Polling,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PersistedState {
    #[serde(default)]
    pub phase: Phase,
    /// Last LSN whose changes are applied to the index and acknowledged to
    /// the server. Restart resumes here; the server never discards WAL past it.
    #[serde(default)]
    pub lsn: u64,
    #[serde(default)]
    pub slot_created: bool,
    #[serde(default)]
    pub publication_created: bool,
    /// Poll mode: per-index high-water mark of the watermark column (as text).
    #[serde(default)]
    pub watermarks: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

pub fn sources_root(data_dir: &Path) -> PathBuf {
    data_dir.join("_sources")
}

impl Store {
    pub fn new(data_dir: &Path, name: &str) -> Self {
        Self {
            dir: sources_root(data_dir).join(name),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn save_spec(&self, spec: &SourceSpec) -> Result<()> {
        write_atomic(
            &self.dir.join("spec.json"),
            &serde_json::to_vec_pretty(spec)?,
        )
    }

    pub fn load_spec(&self) -> Result<SourceSpec> {
        let p = self.dir.join("spec.json");
        let b = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
        Ok(serde_json::from_slice(&b)?)
    }

    pub fn load_state(&self) -> PersistedState {
        std::fs::read(self.dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, st: &PersistedState) -> Result<()> {
        write_atomic(
            &self.dir.join("state.json"),
            &serde_json::to_vec_pretty(st)?,
        )
    }

    pub fn remove(&self) -> Result<()> {
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir)?;
        }
        Ok(())
    }

    /// Every stored source, in name order.
    pub fn list(data_dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(sources_root(data_dir))
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().join("spec.json").exists())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_a_missing_file_is_the_default() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path(), "shop");
        assert_eq!(s.load_state(), PersistedState::default());
        let st = PersistedState {
            phase: Phase::Streaming,
            lsn: 0x16B3748,
            slot_created: true,
            ..Default::default()
        };
        s.save_state(&st).unwrap();
        assert_eq!(s.load_state(), st);
        s.remove().unwrap();
        assert_eq!(s.load_state(), PersistedState::default());
    }
}
