//! Tier 1: the local XERJ node over loopback HTTP, definitions only. Off by
//! default until probe P0 (symbol docs + `--watch` on the hook's prefix)
//! passes. Never the CLI: its freshness check alone costs 430-800 ms.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct Tier1 {
    pub url: String,
    pub prefix: String,
    pub timeout: Duration,
}

pub struct Def {
    pub abs: PathBuf,
    pub line: usize,
    pub code: String,
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
        let prefix = std::env::var("XERJ_HOOK_PREFIX")
            .ok()
            .or_else(|| cfg.tier1_prefix.clone())
            .unwrap_or_else(|| "ax".into());
        Some(Self {
            url: url.trim_end_matches('/').to_string(),
            prefix,
            timeout: Duration::from_millis(200),
        })
    }

    /// Exact definitions for the chain's identifiers under `root`.
    pub fn defs(&self, idents: &[String], root: &Path) -> Vec<Def> {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .build()
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ident in idents.iter().take(2) {
            let body = crate::def::definition_query(ident, None, None, 5, None);
            let url = format!("{}/{}*/_search", self.url, self.prefix);
            let Ok(resp) = client.post(&url).json(&body).send() else {
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
                    .to_string();
                if !name.eq_ignore_ascii_case(ident)
                    && !name.to_ascii_lowercase().ends_with(ident.as_str())
                {
                    continue;
                }
                let loc = crate::search::hit_location(&h);
                let Some((path, line)) = loc.rsplit_once(':') else {
                    continue;
                };
                let line: usize = line.parse().unwrap_or(1);
                let abs = super::parse::normalize(root, path);
                if !abs.starts_with(root) {
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
                out.push(Def { abs, line, code });
            }
        }
        out
    }

    /// First trigger in a repo: index it in the background, idle priority,
    /// one worker, no embeddings. Runs once per repo (marker file).
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
                &self.prefix,
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
