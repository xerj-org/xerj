//! Resume journal — append-only NDJSON living OUTSIDE the scanned folder
//! (default ~/.xerj/autoindex/<hash>/journal.ndjson). A torn last line is
//! discarded; worst case one file is fully reprocessed and idempotent _ids
//! dedupe it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDataset {
    pub slug: String,
    pub index: String,
    pub family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub specs: Vec<crate::infer::FieldSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_field: Option<String>,
    pub sampled_records: u64,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAssignment {
    pub rel: String,
    pub family: String,
    pub gzip: bool,
    /// group (None = whole file) → dataset slug
    pub assignments: Vec<(Option<String>, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JunkFile {
    pub file_key: String,
    pub rel: String,
    pub format: String,
    pub status: String, // junk | skipped
    pub reason: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub datasets: Vec<PlanDataset>,
    /// file_key → assignment
    pub files: HashMap<String, FileAssignment>,
    /// junk/skipped files recorded at scan time (never fatal)
    #[serde(default)]
    pub junk_files: Vec<JunkFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDone {
    pub file_key: String,
    pub path: String,
    pub records: u64,
    pub junk: u64,
    pub bytes: u64,
}

pub struct Journal {
    path: PathBuf,
    file: std::fs::File,
    pub run_id: String,
    pub resumed: bool,
    pub done: HashMap<String, FileDone>,
    pub plan: Option<Plan>,
}

pub fn default_state_dir(root: &str, url: &str, prefix: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home)
        .join(".xerj")
        .join("autoindex")
        .join(crate::ids::state_key(root, url, prefix))
}

impl Journal {
    pub fn open(
        state_dir: &Path,
        root: &str,
        url: &str,
        prefix: &str,
        fresh: bool,
    ) -> Result<Journal> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create state dir {}", state_dir.display()))?;
        let jpath = state_dir.join("journal.ndjson");
        if fresh && jpath.exists() {
            std::fs::remove_file(&jpath).ok();
        }
        let mut done = HashMap::new();
        let mut plan = None;
        let mut run_id = None;
        let mut resumed = false;
        if jpath.exists() {
            let f = std::fs::File::open(&jpath)?;
            for line in std::io::BufReader::new(f).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    break; // torn tail line — stop replay here
                };
                match v.get("kind").and_then(|k| k.as_str()) {
                    Some("run") => {
                        let (jr, ju, jp) = (
                            v.get("root").and_then(|x| x.as_str()).unwrap_or(""),
                            v.get("url").and_then(|x| x.as_str()).unwrap_or(""),
                            v.get("prefix").and_then(|x| x.as_str()).unwrap_or(""),
                        );
                        if jr != root || ju != url || jp != prefix {
                            anyhow::bail!(
                                "journal at {} was created for root={jr} url={ju} prefix={jp}; \
                                 current run has root={root} url={url} prefix={prefix}. \
                                 Use --fresh to discard it or --state-dir for a separate state.",
                                jpath.display()
                            );
                        }
                        if run_id.is_none() {
                            run_id = v
                                .get("run_id")
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string());
                        }
                        resumed = true;
                    }
                    Some("plan") => {
                        if let Some(p) = v.get("plan") {
                            if let Ok(p) = serde_json::from_value::<Plan>(p.clone()) {
                                plan = Some(p);
                            }
                        }
                    }
                    Some("file_done") => {
                        if let Ok(fd) = serde_json::from_value::<FileDone>(v.clone()) {
                            done.insert(fd.file_key.clone(), fd);
                        }
                    }
                    _ => {}
                }
            }
        }
        let is_new = run_id.is_none();
        let run_id = run_id.unwrap_or_else(|| {
            format!(
                "run-{}-{:04x}",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
                std::process::id() & 0xffff
            )
        });
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jpath)?;
        let mut j = Journal {
            path: jpath,
            file,
            run_id: run_id.clone(),
            resumed: resumed && !is_new,
            done,
            plan,
        };
        if is_new {
            j.append(&serde_json::json!({
                "v": 1, "kind": "run", "root": root, "url": url, "prefix": prefix,
                "run_id": run_id, "started": chrono::Utc::now().to_rfc3339(),
            }))?;
        } else {
            j.append(&serde_json::json!({
                "kind": "resume", "at": chrono::Utc::now().to_rfc3339(),
            }))?;
        }
        Ok(j)
    }

    fn append(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        Ok(())
    }

    pub fn write_plan(&mut self, plan: &Plan) -> Result<()> {
        self.append(&serde_json::json!({"kind": "plan", "plan": plan}))?;
        self.file.sync_data().ok();
        self.plan = Some(plan.clone());
        Ok(())
    }

    pub fn file_done(&mut self, fd: &FileDone) -> Result<()> {
        let mut v = serde_json::to_value(fd)?;
        v["kind"] = Value::String("file_done".into());
        self.append(&v)?;
        self.done.insert(fd.file_key.clone(), fd.clone());
        // fsync batched: every 32 completions
        if self.done.len().is_multiple_of(32) {
            self.file.sync_data().ok();
        }
        Ok(())
    }

    pub fn finish(&mut self, summary: &Value) -> Result<()> {
        self.append(&serde_json::json!({"kind": "finish", "summary": summary,
            "at": chrono::Utc::now().to_rfc3339()}))?;
        self.file.sync_data().ok();
        Ok(())
    }

    pub fn done_keys(&self) -> HashSet<String> {
        self.done.keys().cloned().collect()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
