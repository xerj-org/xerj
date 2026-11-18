//! `xerj status [--prefix P]` — how far an autoindex is behind its folder
//! (#3268).
//!
//! `xerj status` used to fail with "unknown argument": index freshness was
//! only reachable as the MCP `xerj_status` tool, which reports node health,
//! not when a prefix was built. This prints the catalog's newest run for the
//! prefix (time, run id, root), the git commit at that time and now when the
//! root is a git checkout, and how many files are newer than the run. It is
//! read-only: one catalog search, metadata reads under the root, and the
//! git `HEAD`, ref and reflog files.

use std::time::{Duration, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::staleness::{self, WalkLimits};

const USAGE: &str = "\
xerj status — when an autoindex prefix was built and how far its folder has moved since

USAGE:
    xerj status [OPTIONS]

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --prefix <P>    autoindex prefix (default \"ax\")
    --api-key <K>   Authorization (or $XERJ_API_KEY; --insecure nodes need none)
    --json          print one JSON object

Exit 0 with the report, 1 when the node has no run for the prefix, 2 on a usage
or connection error.
";

/// Changed files counted before the scan stops and says \"at least\".
const MAX_COUNTED: usize = 100_000;
/// Changed paths printed as examples.
const EXAMPLES: usize = 5;
const STATUS_WALK: WalkLimits = WalkLimits {
    stat_cap: 200_000,
    wall: Duration::from_secs(5),
    grace: staleness::MTIME_GRACE,
};

/// Everything `xerj status` reports for one prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub prefix: String,
    pub indexed_at: String,
    pub run_id: String,
    pub root: String,
    pub root_present: bool,
    pub commit_at_index: Option<String>,
    pub commit_now: Option<String>,
    pub changed_files: usize,
    pub changed_complete: bool,
    pub changed_examples: Vec<String>,
}

impl StatusReport {
    /// Build from a catalog stamp by reading the folder it names.
    pub fn from_stamp(stamp: &staleness::IndexStamp) -> Self {
        let root = std::path::Path::new(stamp.root.trim());
        let root_present = root.is_dir();
        let (commit_at_index, commit_now, scan) = if root_present {
            (
                crate::gitref::head_at(root, stamp.indexed_at),
                crate::gitref::head_now(root),
                staleness::changed_files(root, stamp.indexed_at, &STATUS_WALK, MAX_COUNTED),
            )
        } else {
            (None, None, staleness::ChangedScan::default())
        };
        Self {
            prefix: stamp.prefix.clone(),
            indexed_at: stamp.indexed_at_raw.clone(),
            run_id: stamp.run_id.clone(),
            root: stamp.root.clone(),
            root_present,
            commit_at_index,
            commit_now,
            changed_files: scan.files.len(),
            changed_complete: root_present && scan.complete,
            changed_examples: scan.files.iter().take(EXAMPLES).cloned().collect(),
        }
    }

    /// `fresh`, `stale`, or `unknown` (the root is not on this machine).
    pub fn verdict(&self) -> &'static str {
        if !self.root_present {
            "unknown"
        } else if self.changed_files > 0
            || matches!((&self.commit_at_index, &self.commit_now), (Some(a), Some(b)) if a != b)
        {
            "stale"
        } else {
            "fresh"
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "prefix": self.prefix,
            "indexed_at": self.indexed_at,
            "run_id": self.run_id,
            "root": self.root,
            "root_present": self.root_present,
            "commit_at_index": self.commit_at_index,
            "commit_now": self.commit_now,
            "changed_files": self.changed_files,
            "changed_files_complete": self.changed_complete,
            "changed_examples": self.changed_examples,
            "verdict": self.verdict(),
        })
    }

    pub fn lines(&self) -> Vec<String> {
        let commit = |c: &Option<String>, missing: &str| {
            c.as_deref()
                .map(|c| crate::gitref::short(c).to_string())
                .unwrap_or_else(|| missing.to_string())
        };
        let mut out = vec![
            format!("prefix:          {}", self.prefix),
            format!(
                "indexed_at:      {}{}",
                self.indexed_at,
                age(&self.indexed_at)
            ),
        ];
        if !self.run_id.is_empty() {
            out.push(format!("run:             {}", self.run_id));
        }
        out.push(format!(
            "root:            {}{}",
            crate::search::display_root(&self.root),
            if self.root_present {
                ""
            } else {
                "  (not a folder on this machine)"
            }
        ));
        if self.root_present {
            out.push(format!(
                "commit at index: {}",
                commit(
                    &self.commit_at_index,
                    "unknown (not a git checkout, or no reflog entry that old)"
                )
            ));
            out.push(format!(
                "commit now:      {}",
                commit(&self.commit_now, "unknown (not a git checkout)")
            ));
            out.push(format!(
                "changed files:   {}{} newer than the index",
                if self.changed_complete {
                    ""
                } else {
                    "at least "
                },
                self.changed_files
            ));
            for example in &self.changed_examples {
                out.push(format!("                 {example}"));
            }
        }
        let verdict = self.verdict();
        out.push(format!("verdict:         {verdict}"));
        if verdict == "stale" {
            out.push(format!(
                "`xerj def` re-anchors moved lines and finds definitions in changed files; \
                 search can still miss new text. Reindex with: xerj autoindex {} --prefix {}",
                crate::search::display_root(&self.root),
                self.prefix
            ));
        }
        out
    }
}

fn age(raw: &str) -> String {
    let Some(when) = staleness::parse_time(raw) else {
        return String::new();
    };
    let (Ok(then), Ok(now)) = (
        when.duration_since(UNIX_EPOCH),
        std::time::SystemTime::now().duration_since(UNIX_EPOCH),
    ) else {
        return String::new();
    };
    let secs = now.as_secs().saturating_sub(then.as_secs());
    let text = match secs {
        s if s < 120 => format!("{s} s"),
        s if s < 2 * 3600 => format!("{} min", s / 60),
        s if s < 2 * 86_400 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86_400),
    };
    format!("  ({text} ago)")
}

/// Entry point for `xerj status`. Returns a process exit code.
pub fn run_status_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut prefix = crate::search::DEFAULT_SEARCH_PREFIX.to_string();
    let mut as_json = false;
    let mut it = xerj_common::argv::args_after_subcommand().into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return 0;
            }
            "--url" => url = it.next().unwrap_or_default(),
            "--prefix" => prefix = it.next().unwrap_or(prefix),
            "--api-key" => api_key = it.next(),
            "--json" => as_json = true,
            other => {
                eprintln!("xerj status: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        }
    }
    let resolved = match crate::search::resolve_prefix(&prefix) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("xerj status: {e}\n\n{USAGE}");
            return 2;
        }
    };
    if let Some(note) = crate::search::prefix_rewrite_note(&prefix, &resolved) {
        eprintln!("xerj status: {note}");
    }
    let prefix = resolved.prefix;
    if url.is_empty() {
        url = "http://localhost:9200".to_string();
    }
    if api_key.is_none() && xerj_common::localauth::url_is_loopback(&url) {
        if let Some(found) =
            xerj_common::localauth::discover_local_admin_key_for_url_with_origin(&url)
        {
            eprintln!(
                "xerj status: no --api-key/XERJ_API_KEY given; using the admin key at {} ({})",
                found.path.display(),
                found.provenance()
            );
            api_key = Some(found.key);
        }
    }
    let es = match crate::esclient::Es::new(&url, api_key.clone()) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj status: could not build client for {url}: {e}");
            return 2;
        }
    };
    if let Err(e) = es.ping() {
        eprintln!("xerj status: no XERJ node reachable at {url} ({e}).");
        return 2;
    }
    let Some(stamp) = staleness::load_stamp(&url, api_key.as_deref(), &prefix) else {
        if as_json {
            println!(
                "{}",
                json!({"prefix": prefix, "indexed_at": null, "verdict": "not_indexed"})
            );
        } else {
            println!(
                "xerj status: no autoindex run is recorded for prefix `{prefix}` on {url}. \
                 Index a folder with: xerj autoindex <folder> --prefix {prefix}"
            );
        }
        return 1;
    };
    let report = StatusReport::from_stamp(&stamp);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_default()
        );
    } else {
        for line in report.lines() {
            println!("{line}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn report_counts_changed_files_and_reads_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.c"), "int a;\n").unwrap();
        let indexed_at = SystemTime::now() - Duration::from_secs(3600);
        let file = std::fs::File::options()
            .write(true)
            .open(dir.path().join("a.c"))
            .unwrap();
        file.set_modified(indexed_at - Duration::from_secs(10))
            .unwrap();
        let stamp = staleness::IndexStamp {
            prefix: "p".into(),
            run_id: "gen-1".into(),
            root: dir.path().display().to_string(),
            indexed_at,
            indexed_at_raw: "2026-11-18T00:00:00Z".into(),
        };
        let fresh = StatusReport::from_stamp(&stamp);
        assert_eq!(fresh.changed_files, 0);
        assert!(fresh.changed_complete);
        assert_eq!(fresh.verdict(), "fresh");

        std::fs::write(dir.path().join("b.c"), "int b;\n").unwrap();
        let stale = StatusReport::from_stamp(&stamp);
        assert_eq!(stale.changed_files, 1);
        assert_eq!(stale.changed_examples, vec!["b.c"]);
        assert_eq!(stale.verdict(), "stale");
        let text = stale.lines().join("\n");
        assert!(
            text.contains("indexed_at:      2026-11-18T00:00:00Z"),
            "{text}"
        );
        assert!(text.contains("changed files:   1 newer"), "{text}");
        assert_eq!(stale.to_json()["changed_files"], 1);

        let gone = StatusReport::from_stamp(&staleness::IndexStamp {
            root: "/definitely/not/here".into(),
            ..stamp
        });
        assert_eq!(gone.verdict(), "unknown");
    }
}
