//! Tier 1: the local XERJ node over loopback HTTP, definitions only. Off by
//! default; probe P0 measures it. Never the CLI: its freshness check alone
//! costs 430-800 ms.
//!
//! v2: one index prefix per worktree root (`<base><hash10>`), so a hit's
//! repo-relative `ax_path` is joined onto the root it was indexed from. v1
//! queried `<prefix>*` across every indexed root and joined each relative
//! path onto the scan root, which attributes another repo's definition to
//! a path in this one. Both identifier lookups share one deadline.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Tier1 {
    pub url: String,
    pub base: String,
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct Def {
    pub abs: PathBuf,
    pub line: usize,
    pub code: String,
}

/// The index prefix for one worktree root.
pub fn prefix_for(base: &str, root: &Path) -> String {
    let h = super::profile::hash("t1", &root.display().to_string());
    format!("{base}{}", &h[..10])
}

impl Tier1 {
    pub fn from_env(cfg: &super::Config) -> Option<Self> {
        if cfg.tier1 == "off" {
            return None;
        }
        let url = std::env::var("XERJ_HOOK_URL")
            .ok()
            .or_else(|| cfg.tier1_url.clone())?;
        if !xerj_common::localauth::url_is_loopback(&url) {
            return None;
        }
        let base = std::env::var("XERJ_HOOK_PREFIX")
            .ok()
            .or_else(|| cfg.tier1_prefix.clone())
            .unwrap_or_else(|| "xh".into());
        Some(Self {
            url: url.trim_end_matches('/').to_string(),
            base,
            timeout: Duration::from_millis(200),
        })
    }

    /// Exact definitions for `idents`, indexed from `repo_root`, kept only
    /// when they lie under `search_root`.
    pub fn defs(&self, idents: &[String], repo_root: &Path, search_root: &Path) -> Vec<Def> {
        let deadline = Instant::now() + self.timeout;
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .build()
        else {
            return Vec::new();
        };
        let prefix = prefix_for(&self.base, repo_root);
        let url = format!("{}/{prefix}-*/_search", self.url);
        let mut out = Vec::new();
        for ident in idents.iter().filter(|i| i.len() >= 4).take(2) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left < Duration::from_millis(5) {
                break;
            }
            let body = crate::def::definition_query(ident, None, None, 5, None);
            let Ok(resp) = client.post(&url).timeout(left).json(&body).send() else {
                continue;
            };
            let Ok(mut v) = resp.json::<Value>() else {
                continue;
            };
            for h in crate::def::finalize_definition_response(&mut v, ident, 5) {
                let src = h.get("_source").cloned().unwrap_or(Value::Null);
                let name = src
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                // Exact symbol only: `name` is the symbol doc's identifier.
                if name != *ident && !name.ends_with(&format!("::{ident}")) {
                    continue;
                }
                let loc = crate::search::hit_location(&h);
                let Some((path, line)) = loc.rsplit_once(':') else {
                    continue;
                };
                let line: usize = line.parse().unwrap_or(1);
                let abs = super::parse::normalize(repo_root, path);
                if !abs.starts_with(search_root) || !abs.is_file() {
                    continue;
                }
                let code = src
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !out.iter().any(|d: &Def| d.abs == abs && d.line == line) {
                    out.push(Def { abs, line, code });
                }
            }
        }
        out
    }

    /// First trigger in a worktree: index it in the background, idle
    /// priority, one worker, no embeddings. Runs once per root (marker).
    pub fn index_once(&self, root: &Path, marker: &Path) {
        if marker.exists() || !xerj_common::localnode::url_serving(&self.url) {
            return;
        }
        let Some(exe) = xerj_common::localnode::xerj_executable() else {
            return;
        };
        let _ = super::profile::atomic(marker, b"started\n");
        let mut cmd = Command::new("nice");
        cmd.args(["-n", "19"])
            .arg(exe)
            .arg("autoindex")
            .arg(root)
            .args([
                "--url",
                &self.url,
                "--prefix",
                &prefix_for(&self.base, root),
                "--no-semantic",
                "--workers",
                "1",
            ]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = cmd.spawn();
    }
}
