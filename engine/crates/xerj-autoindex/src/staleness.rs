//! Hint when a searched autoindex is older than the folder it came from.
//!
//! `xerj code` refuses a ledger older than 30 days
//! (`xerj_common::xccode::state::check_fresh`). This path does not: a search
//! still returns its passages, and says when a file under the recorded root
//! is newer than the last run or an indexed path is gone (#2345). An agent
//! that sees zero hits can then tell "not in the corpus" from "not in the
//! index yet".
//!
//! # Cost
//!
//! One catalog search (2s timeout, no retry) loads the newest run for the
//! prefix. The freshness stamp is `summary_generated_at` when that parses,
//! otherwise `started`. `started` is the beginning of a run, so a file saved
//! while a long generation was in flight can look newer than a stamp that
//! has no summary time; the hint names a reindex, which is safe.
//!
//! The walk then stats at most [`STAT_CAP`] entries and stops after
//! [`WALK_WALL`]. Children of a directory are stat'd, then admitted
//! subdirectories are entered newest-mtime first, so a new page is visited
//! before an old sibling tree spends the cap. Hidden names, symlinks,
//! `.gitignore` / `.xerjignore`, the built-in ignore defaults, and
//! marker-gated generated directories are skipped the same way `xerj
//! autoindex` skips them — a `target/` or `.git` rewrite is not a stale
//! corpus. A root that is not a directory on this machine (a remote node,
//! an object-store source) produces no hint.
//!
//! A directory mtime newer than the stamp, with no newer child, is a
//! removal. That case confirms against catalog `indexed` / `duplicate`
//! paths, and only those, so deleting `target/` does not warn. The
//! confirmation shares the same stat cap and wall clock. A check that hits
//! either cap without a proven change stays silent.

use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::catalog::CORPUS_SCOPE_FIELD;
use crate::ignore_rules::{is_hidden_name, IgnoreOptions, IgnoreStack};
use crate::walk::marker_generated_dir;
use serde_json::{json, Value};

/// Metadata reads for one check: the root, each listed child, and catalog
/// paths confirmed for deletion.
pub const STAT_CAP: u64 = 4096;
/// A slow disk stops here instead of turning search into a tree walk.
pub const WALK_WALL: Duration = Duration::from_millis(100);
/// mtime must be newer than the index stamp by more than this. Covers
/// one-second filesystem resolution around the moment the run finished.
pub const MTIME_GRACE: Duration = Duration::from_secs(2);
/// Catalog read cap. The search has already talked to the node; a stuck
/// catalog must not hold the answer.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(2);
const RUN_PAGE: usize = 20;
const FILE_PAGE: usize = 4096;
const MAX_DEPTH: usize = 64;
const HINT_CODE: &str = "index_stale";

/// What `xerj search` prints, and what MCP appends under `_xerj.hints`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessHint {
    pub code: &'static str,
    pub reason: String,
    pub indexed_at: String,
    pub root: String,
    pub run_id: String,
    pub prefix: String,
    pub path: String,
    /// `newer` (added or changed) or `deleted`.
    pub change: &'static str,
}

impl StalenessHint {
    /// The line `xerj search` prints before its passages.
    pub fn cli_line(&self) -> String {
        format!("xerj search: {}", self.reason)
    }
}

/// One indexed path from the catalog, used only to confirm a deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFile {
    pub path: String,
    pub status: Option<String>,
}

/// Caps for one walk. Tests shrink them; search uses [`WalkLimits::default`].
#[derive(Debug, Clone, Copy)]
pub struct WalkLimits {
    pub stat_cap: u64,
    pub wall: Duration,
    pub grace: Duration,
}

impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            stat_cap: STAT_CAP,
            wall: WALK_WALL,
            grace: MTIME_GRACE,
        }
    }
}

/// The run this check compares the folder to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStamp {
    pub prefix: String,
    pub run_id: String,
    pub root: String,
    pub indexed_at: SystemTime,
    pub indexed_at_raw: String,
}

/// Look up `prefix` in the catalog and, when the recorded root is a local
/// directory, compare it to the files there.
///
/// `None` on every failure: a catalog the node does not have, a transport
/// error, an unreadable folder. The caller prints the search either way.
pub fn check(url: &str, api_key: Option<&str>, prefix: &str) -> Option<StalenessHint> {
    let prefix = prefix.trim();
    let url = url.trim().trim_end_matches('/');
    if prefix.is_empty() || url.is_empty() {
        return None;
    }
    let client = CatalogClient::new(url, api_key)?;
    let runs = fetch_runs(&client, prefix)?;
    let stamp = latest_stamp(&runs, prefix)?;
    hint_for_stamp(&stamp, &WalkLimits::default(), || {
        fetch_files(&client, prefix).unwrap_or_default()
    })
}

/// Decide from an already-loaded stamp. `files` runs only when a directory
/// mtime says something was removed and the walk did not already find a
/// newer admitted file.
pub fn hint_for_stamp(
    stamp: &IndexStamp,
    limits: &WalkLimits,
    files: impl FnOnce() -> Vec<CatalogFile>,
) -> Option<StalenessHint> {
    let root = Path::new(stamp.root.trim());
    if !root.is_dir() {
        return None;
    }
    let root = root.canonicalize().ok()?;
    if !root.is_dir() {
        return None;
    }
    let walked = walk_tree(&root, stamp.indexed_at, limits);
    let (change, rel) = match walked.kind {
        WalkKind::Newer(rel) => ("newer", rel),
        WalkKind::Suspect => {
            if walked.stats >= limits.stat_cap || walked.started.elapsed() >= limits.wall {
                return None;
            }
            let rel = find_deleted(&root, &files(), limits, walked.stats, walked.started)?;
            ("deleted", rel)
        }
        WalkKind::Fresh => return None,
    };
    Some(compose(stamp, &root, &rel, change))
}

/// Append `hint` to `_xerj.hints`, leaving an existing hint list in place.
/// A response that is not a JSON object is left alone.
pub fn attach(resp: &mut Value, hint: &StalenessHint) {
    let Some(obj) = resp.as_object_mut() else {
        return;
    };
    let xerj = obj.entry("_xerj").or_insert_with(|| json!({}));
    let Some(xerj) = xerj.as_object_mut() else {
        return;
    };
    let hints = xerj.entry("hints").or_insert_with(|| json!([]));
    let Some(hints) = hints.as_array_mut() else {
        return;
    };
    if hints
        .iter()
        .any(|h| h.get("code").and_then(Value::as_str) == Some(hint.code))
    {
        return;
    }
    hints.push(json!({
        "code": hint.code,
        "reason": hint.reason,
        "indexed_at": hint.indexed_at,
        "root": hint.root,
        "run_id": hint.run_id,
        "prefix": hint.prefix,
        "path": hint.path,
        "change": hint.change,
    }));
}

/// Merge `hint` into a search JSON body. Non-JSON text is returned unchanged
/// so a truncated budget response is not rewritten into something else.
pub fn merge_response_json(text: &str, hint: &StalenessHint) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    attach(&mut value, hint);
    serde_json::to_string(&value).unwrap_or_else(|_| text.to_string())
}

/// Newest run for `prefix`, by the freshness stamp.
///
/// Run documents are filtered here as well as in the query: a `prefix`
/// inferred as `text` can make a `match_phrase` hit a neighbor, and `term`
/// on that field misses. `summary_generated_at` is the end of the run.
/// `started` is the fallback the generation catalog writes.
pub fn latest_stamp(runs: &[Value], prefix: &str) -> Option<IndexStamp> {
    let mut best: Option<IndexStamp> = None;
    for run in runs {
        if run.get("doc_kind").and_then(Value::as_str) != Some("run") {
            continue;
        }
        if !scope_matches(run, prefix) {
            continue;
        }
        let root = run.get("root").and_then(Value::as_str)?.trim();
        if root.is_empty() {
            continue;
        }
        let (raw, when) = freshness(run)?;
        let run_id = run
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let stamp = IndexStamp {
            prefix: prefix.to_string(),
            run_id,
            root: root.to_string(),
            indexed_at: when,
            indexed_at_raw: raw,
        };
        let replace = match &best {
            None => true,
            Some(prev) => {
                stamp.indexed_at > prev.indexed_at
                    || (stamp.indexed_at == prev.indexed_at && stamp.run_id > prev.run_id)
            }
        };
        if replace {
            best = Some(stamp);
        }
    }
    best
}

fn freshness(run: &Value) -> Option<(String, SystemTime)> {
    if let Some(raw) = field_str(run, "summary_generated_at") {
        if let Some(when) = parse_time(raw) {
            return Some((raw.to_string(), when));
        }
    }
    let raw = field_str(run, "started")?;
    Some((raw.to_string(), parse_time(raw)?))
}

fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn parse_time(raw: &str) -> Option<SystemTime> {
    let dt = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::new(secs as u64, dt.timestamp_subsec_nanos()))
}

fn scope_matches(doc: &Value, prefix: &str) -> bool {
    field_str(doc, "prefix") == Some(prefix) || field_str(doc, CORPUS_SCOPE_FIELD) == Some(prefix)
}

fn compose(stamp: &IndexStamp, root: &Path, rel: &str, change: &str) -> StalenessHint {
    let rel_shown = sanitize_display(rel);
    let root_shown = sanitize_display(&root.display().to_string());
    let run = if stamp.run_id.is_empty() {
        String::new()
    } else {
        format!(" (run {})", sanitize_display(&stamp.run_id))
    };
    let what = if change == "deleted" {
        format!(
            "`{rel_shown}` was indexed and is no longer in that folder, so these results can name a page that was removed"
        )
    } else {
        format!(
            "`{rel_shown}` is newer than that index, so a page on disk can be missing from these results"
        )
    };
    let reason = format!(
        "The index for prefix `{}` was last built at {}{run}. {what}. Reindex with: xerj autoindex {} --prefix {}",
        sanitize_display(&stamp.prefix),
        sanitize_display(&stamp.indexed_at_raw),
        shell_quote(&root_shown),
        shell_quote(&stamp.prefix),
    );
    StalenessHint {
        code: HINT_CODE,
        reason,
        indexed_at: stamp.indexed_at_raw.clone(),
        root: root_shown,
        run_id: stamp.run_id.clone(),
        prefix: stamp.prefix.clone(),
        path: rel_shown,
        change: if change == "deleted" {
            "deleted"
        } else {
            "newer"
        },
    }
}

fn sanitize_display(value: &str) -> String {
    value
        .chars()
        .take(240)
        .map(|ch| {
            if ch.is_control() || ch == '`' {
                '?'
            } else {
                ch
            }
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '\\'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

enum WalkKind {
    Newer(String),
    Suspect,
    Fresh,
}

struct Walked {
    kind: WalkKind,
    stats: u64,
    started: Instant,
}

fn walk_tree(root: &Path, indexed_at: SystemTime, limits: &WalkLimits) -> Walked {
    let started = Instant::now();
    let mut walker = Walker {
        root: root.to_path_buf(),
        indexed_at,
        limits: *limits,
        started,
        stats: 0,
        suspect: false,
        stack: IgnoreStack::new(root, IgnoreOptions::default()),
    };
    if started.elapsed() >= limits.wall || !walker.charge() {
        return Walked {
            kind: WalkKind::Fresh,
            stats: walker.stats,
            started,
        };
    }
    let mtime = fs::symlink_metadata(root)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let kind = match walker.visit(root, 0, mtime) {
        WalkControl::Newer(rel) => WalkKind::Newer(rel),
        WalkControl::Stop if walker.suspect => WalkKind::Suspect,
        WalkControl::Stop => WalkKind::Fresh,
        WalkControl::Continue if walker.suspect => WalkKind::Suspect,
        WalkControl::Continue => WalkKind::Fresh,
    };
    Walked {
        kind,
        stats: walker.stats,
        started,
    }
}

enum WalkControl {
    Newer(String),
    Stop,
    Continue,
}

struct Walker {
    root: PathBuf,
    indexed_at: SystemTime,
    limits: WalkLimits,
    started: Instant,
    stats: u64,
    suspect: bool,
    stack: IgnoreStack,
}

impl Walker {
    fn charge(&mut self) -> bool {
        if self.stats >= self.limits.stat_cap || self.started.elapsed() >= self.limits.wall {
            return false;
        }
        self.stats += 1;
        true
    }

    fn newer(&self, mtime: SystemTime) -> bool {
        match mtime.duration_since(self.indexed_at) {
            Ok(age) => age > self.limits.grace,
            Err(_) => false,
        }
    }

    fn visit(&mut self, dir: &Path, depth: usize, dir_mtime: SystemTime) -> WalkControl {
        if depth >= MAX_DEPTH {
            return WalkControl::Continue;
        }
        self.stack.truncate_to(depth);
        self.stack.enter_dir(dir, depth);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return WalkControl::Continue,
        };
        let mut complete = true;
        let mut child_newer = false;
        let mut subdirs: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                complete = false;
                break;
            };
            if !self.charge() {
                complete = false;
                break;
            }
            let path = entry.path();
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if self.newer(mtime) {
                child_newer = true;
            }
            let name = match path.file_name() {
                Some(name) => name,
                None => continue,
            };
            if is_hidden_name(name) {
                continue;
            }
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if self.stack.skip_dir(&path).is_some() || marker_generated_dir(&path).is_some() {
                    continue;
                }
                subdirs.push((mtime, path));
                continue;
            }
            if !file_type.is_file() || self.stack.skip_file(&path).is_some() {
                continue;
            }
            if self.newer(mtime) {
                return WalkControl::Newer(rel_of(&self.root, &path));
            }
        }
        if complete && self.newer(dir_mtime) && !child_newer {
            self.suspect = true;
        }
        if !complete {
            return WalkControl::Stop;
        }
        subdirs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (mtime, path) in subdirs {
            match self.visit(&path, depth + 1, mtime) {
                WalkControl::Continue => {}
                other => return other,
            }
        }
        WalkControl::Continue
    }
}

fn rel_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn find_deleted(
    root: &Path,
    files: &[CatalogFile],
    limits: &WalkLimits,
    mut stats: u64,
    started: Instant,
) -> Option<String> {
    for file in files {
        if !counts_as_indexed(file.status.as_deref()) {
            continue;
        }
        let Some(rel) = safe_rel(&file.path) else {
            continue;
        };
        if stats >= limits.stat_cap || started.elapsed() >= limits.wall {
            return None;
        }
        stats += 1;
        let path = root.join(rel);
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == ErrorKind::NotFound => return Some(rel.to_string()),
            _ => {}
        }
    }
    None
}

fn counts_as_indexed(status: Option<&str>) -> bool {
    match status {
        None | Some("indexed" | "duplicate") => true,
        Some(_) => false,
    }
}

/// A catalog path that stays inside the root. Absolute paths and `..` are
/// ignored so a catalog row cannot point the stat at another directory.
fn safe_rel(rel: &str) -> Option<&str> {
    if rel.is_empty() || rel.len() > 1024 || rel.starts_with('/') || rel.starts_with('\\') {
        return None;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return None;
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(rel)
}

struct CatalogClient {
    http: reqwest::blocking::Client,
    base: String,
    api_key: Option<String>,
}

enum CatalogErr {
    BadRequest,
    Failed,
}

impl CatalogClient {
    fn new(url: &str, api_key: Option<&str>) -> Option<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(CATALOG_TIMEOUT)
            .connect_timeout(Duration::from_secs(1))
            .danger_accept_invalid_certs(true)
            .build()
            .ok()?;
        let api_key = api_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        Some(Self {
            http,
            base: url.to_string(),
            api_key,
        })
    }

    fn search(&self, body: &Value) -> Result<Value, CatalogErr> {
        let url = format!("{}/autoindex-catalog/_search", self.base);
        let mut req = self.http.post(url).json(body);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("ApiKey {key}"));
        }
        let resp = req.send().map_err(|_| CatalogErr::Failed)?;
        let status = resp.status();
        if status.as_u16() == 400 {
            return Err(CatalogErr::BadRequest);
        }
        if !status.is_success() {
            return Err(CatalogErr::Failed);
        }
        resp.json().map_err(|_| CatalogErr::Failed)
    }
}

fn scope_clause(prefix: &str) -> Value {
    json!({
        "bool": {
            "should": [
                {"term": {"prefix": prefix}},
                {"match_phrase": {"prefix": prefix}},
                {"term": {CORPUS_SCOPE_FIELD: prefix}}
            ],
            "minimum_should_match": 1
        }
    })
}

fn fetch_runs(client: &CatalogClient, prefix: &str) -> Option<Vec<Value>> {
    let source = json!([
        "prefix",
        CORPUS_SCOPE_FIELD,
        "root",
        "started",
        "summary_generated_at",
        "run_id",
        "doc_kind"
    ]);
    let query = json!({"bool": {"must": [
        {"term": {"doc_kind": "run"}},
        scope_clause(prefix)
    ]}});
    let sorted = json!({
        "size": RUN_PAGE,
        "sort": [{"_id": "desc"}],
        "_source": source,
        "query": query,
    });
    match client.search(&sorted) {
        Ok(body) => Some(hit_sources(&body)),
        Err(CatalogErr::BadRequest) => {
            let unsorted = json!({
                "size": 200,
                "_source": source,
                "query": query,
            });
            client.search(&unsorted).ok().map(|body| hit_sources(&body))
        }
        Err(CatalogErr::Failed) => None,
    }
}

fn fetch_files(client: &CatalogClient, prefix: &str) -> Option<Vec<CatalogFile>> {
    let body = json!({
        "size": FILE_PAGE,
        "_source": ["path", "status", "prefix", CORPUS_SCOPE_FIELD, "doc_kind"],
        "query": {"bool": {"must": [
            {"term": {"doc_kind": "file"}},
            scope_clause(prefix)
        ]}}
    });
    let sources = hit_sources(&client.search(&body).ok()?);
    let mut files = Vec::new();
    for source in sources {
        if source.get("doc_kind").and_then(Value::as_str) != Some("file") {
            continue;
        }
        if !scope_matches(&source, prefix) {
            continue;
        }
        let Some(path) = field_str(&source, "path") else {
            continue;
        };
        files.push(CatalogFile {
            path: path.to_string(),
            status: field_str(&source, "status").map(str::to_string),
        });
    }
    Some(files)
}

fn hit_sources(body: &Value) -> Vec<Value> {
    body.pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|hit| hit.get("_source").cloned())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn limits(stat_cap: u64) -> WalkLimits {
        WalkLimits {
            stat_cap,
            wall: Duration::from_secs(5),
            grace: MTIME_GRACE,
        }
    }

    fn touch(path: &Path, when: SystemTime) {
        let file = fs::OpenOptions::new()
            .read(true)
            .open(path)
            .unwrap_or_else(|err| panic!("open {}: {err}", path.display()));
        file.set_modified(when)
            .unwrap_or_else(|err| panic!("mtime {}: {err}", path.display()));
    }

    fn stamp(root: &Path, indexed_at: SystemTime) -> IndexStamp {
        IndexStamp {
            prefix: "ok-knowledge".into(),
            run_id: "run-20261023T234652Z-2c98-0000".into(),
            root: root.display().to_string(),
            indexed_at,
            indexed_at_raw: "2026-10-23T23:46:52Z".into(),
        }
    }

    fn hint(
        root: &Path,
        indexed_at: SystemTime,
        cap: u64,
        files: &[CatalogFile],
    ) -> Option<StalenessHint> {
        hint_for_stamp(&stamp(root, indexed_at), &limits(cap), || files.to_vec())
    }

    #[test]
    fn a_page_added_after_the_index_is_named_and_the_search_is_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let indexed_at = SystemTime::now() - Duration::from_secs(24 * 3600);
        let old = indexed_at - Duration::from_secs(24 * 3600);
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/old.md"), "yesterday\n").unwrap();
        touch(&root.join("docs/old.md"), old);
        touch(&root.join("docs"), old);
        touch(root, old);
        fs::write(root.join("docs/page.md"), "created the next day\n").unwrap();

        let found = hint(root, indexed_at, STAT_CAP, &[]).expect("newer page");
        let line = found.cli_line();
        assert_eq!(found.change, "newer");
        assert_eq!(found.code, "index_stale");
        assert!(line.starts_with("xerj search: "), "{line}");
        assert!(line.contains("docs/page.md"), "{line}");
        assert!(line.contains("2026-10-23T23:46:52Z"), "{line}");
        assert!(line.contains("run-20261023T234652Z-2c98-0000"), "{line}");
        assert!(
            line.contains("xerj autoindex ") && line.contains("--prefix ok-knowledge"),
            "{line}"
        );
        assert!(
            !line.to_ascii_lowercase().contains("refus"),
            "a stale index is a hint, not a refusal: {line}"
        );
    }

    #[test]
    fn newest_directory_is_visited_before_the_cap_is_spent_on_old_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let indexed_at = SystemTime::now() - Duration::from_secs(24 * 3600);
        let old = indexed_at - Duration::from_secs(24 * 3600);
        fs::create_dir_all(root.join("old")).unwrap();
        for i in 0..30 {
            let path = root.join(format!("old/f{i}.txt"));
            fs::write(&path, "old\n").unwrap();
            touch(&path, old);
        }
        touch(&root.join("old"), old);
        fs::create_dir_all(root.join("fresh")).unwrap();
        fs::write(root.join("fresh/page.md"), "new\n").unwrap();

        let found = hint(root, indexed_at, 4, &[]).expect("cap 4 reaches fresh/page.md");
        assert_eq!(found.path, "fresh/page.md", "{found:?}");
        assert!(
            hint(root, indexed_at, 3, &[]).is_none(),
            "three stats cover the root and its two children and stop before the new page"
        );
    }

    #[test]
    fn an_unchanged_tree_and_ignored_or_hidden_writes_stay_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let indexed_at = SystemTime::now() - Duration::from_secs(24 * 3600);
        let old = indexed_at - Duration::from_secs(24 * 3600);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        touch(&root.join("src/lib.rs"), old);
        touch(&root.join(".gitignore"), old);
        touch(&root.join("src"), old);
        touch(root, old);

        assert!(hint(root, indexed_at, STAT_CAP, &[]).is_none());

        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/foo.rs"), "build output\n").unwrap();
        fs::write(root.join("debug.log"), "noise\n").unwrap();
        assert!(
            hint(root, indexed_at, STAT_CAP, &[]).is_none(),
            "hidden, default-ignored, and gitignored writes are not the corpus"
        );
    }

    #[test]
    fn a_marker_generated_directory_is_not_a_stale_page() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let indexed_at = SystemTime::now() - Duration::from_secs(24 * 3600);
        let old = indexed_at - Duration::from_secs(24 * 3600);
        fs::create_dir_all(root.join("ProjectSettings")).unwrap();
        fs::write(
            root.join("ProjectSettings/ProjectVersion.txt"),
            "m_EditorVersion: 1\n",
        )
        .unwrap();
        fs::write(root.join("README.md"), "notes\n").unwrap();
        touch(&root.join("ProjectSettings/ProjectVersion.txt"), old);
        touch(&root.join("README.md"), old);
        touch(&root.join("ProjectSettings"), old);
        touch(root, old);
        fs::create_dir_all(root.join("Library")).unwrap();
        fs::write(root.join("Library/cache.bin"), "generated\n").unwrap();
        assert!(hint(root, indexed_at, STAT_CAP, &[]).is_none());
    }

    #[test]
    fn a_removed_indexed_file_is_a_hint_and_a_removed_ignored_tree_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let indexed_at = SystemTime::now() - Duration::from_secs(24 * 3600);
        let old = indexed_at - Duration::from_secs(24 * 3600);
        let fresh = SystemTime::now();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/keep.md"), "stay\n").unwrap();
        touch(&root.join("docs/keep.md"), old);
        touch(&root.join("docs"), fresh);
        touch(root, old);
        let files = vec![
            CatalogFile {
                path: "docs/gone.md".into(),
                status: Some("indexed".into()),
            },
            CatalogFile {
                path: "docs/keep.md".into(),
                status: Some("indexed".into()),
            },
            CatalogFile {
                path: "target/foo.rs".into(),
                status: Some("junk".into()),
            },
            CatalogFile {
                path: "../outside.txt".into(),
                status: Some("indexed".into()),
            },
        ];
        let found = hint(root, indexed_at, STAT_CAP, &files).expect("deleted page");
        assert_eq!(found.change, "deleted");
        assert_eq!(found.path, "docs/gone.md");
        let line = found.cli_line();
        assert!(line.contains("no longer in that folder"), "{line}");
        assert!(line.contains("Reindex with:"), "{line}");
        assert!(!line.to_ascii_lowercase().contains("refus"), "{line}");

        // The directory change is explained by a newer ignored child, so a
        // missing junk path is not reported.
        let explained = tempfile::tempdir().unwrap();
        let root = explained.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
        touch(&root.join("src/lib.rs"), old);
        touch(&root.join("src"), old);
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/out.rlib"), "bin\n").unwrap();
        assert!(hint(root, indexed_at, STAT_CAP, &files).is_none());
    }

    #[test]
    fn a_mtime_inside_the_grace_window_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let indexed_at = SystemTime::now() - Duration::from_secs(60);
        fs::write(root.join("page.md"), "same second\n").unwrap();
        touch(&root.join("page.md"), indexed_at + Duration::from_secs(1));
        touch(root, indexed_at);
        assert!(hint(root, indexed_at, STAT_CAP, &[]).is_none());
    }

    #[test]
    fn a_missing_root_produces_no_hint() {
        let stamp = IndexStamp {
            prefix: "ok-knowledge".into(),
            run_id: "run".into(),
            root: "/no/such/xerj-staleness-root-2345".into(),
            indexed_at: SystemTime::now() - Duration::from_secs(3600),
            indexed_at_raw: "2026-10-23T23:46:52Z".into(),
        };
        assert!(hint_for_stamp(&stamp, &limits(8), Vec::new).is_none());
    }

    #[test]
    fn latest_stamp_prefers_the_summary_time_and_this_prefix() {
        let runs = vec![
            json!({
                "doc_kind": "run",
                "prefix": "ok-knowledge",
                "root": "/old",
                "started": "2026-10-22T00:00:00Z",
                "run_id": "run-old"
            }),
            json!({
                "doc_kind": "run",
                "prefix": "other",
                "root": "/other",
                "started": "2026-10-25T00:00:00Z",
                "run_id": "run-other"
            }),
            json!({
                "doc_kind": "run",
                "prefix": "ok-knowledge",
                "root": "/corpus",
                "started": "2026-10-23T23:00:00Z",
                "summary_generated_at": "2026-10-23T23:46:52Z",
                "run_id": "run-20261023T234652Z-2c98-0000"
            }),
            json!({
                "doc_kind": "run",
                "prefix": "ok-knowledge",
                "root": "",
                "started": "2026-10-24T00:00:00Z",
                "run_id": "run-blank-root"
            }),
        ];
        let stamp = latest_stamp(&runs, "ok-knowledge").unwrap();
        assert_eq!(stamp.root, "/corpus");
        assert_eq!(stamp.indexed_at_raw, "2026-10-23T23:46:52Z");
        assert_eq!(stamp.run_id, "run-20261023T234652Z-2c98-0000");
        assert!(latest_stamp(&runs, "missing").is_none());
    }

    #[test]
    fn attach_appends_to_existing_hints_and_leaves_hits_in_place() {
        let hint = StalenessHint {
            code: HINT_CODE,
            reason: "reindex please".into(),
            indexed_at: "2026-10-23T23:46:52Z".into(),
            root: "/corpus".into(),
            run_id: "run-1".into(),
            prefix: "ok-knowledge".into(),
            path: "docs/page.md".into(),
            change: "newer",
        };
        let mut resp = json!({
            "_xerj": {"hints": [{"code": "lexical_on_semantic_text", "reason": "lexical"}]},
            "hits": {"hits": [{"_id": "1"}]}
        });
        attach(&mut resp, &hint);
        attach(&mut resp, &hint);
        let hints = resp
            .pointer("/_xerj/hints")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[1]["code"], "index_stale");
        assert_eq!(hints[1]["path"], "docs/page.md");
        assert_eq!(
            resp.pointer("/hits/hits/0/_id").and_then(Value::as_str),
            Some("1")
        );

        let merged = merge_response_json(r#"{"hits":{"hits":[]}}"#, &hint);
        let parsed: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(
            parsed
                .pointer("/_xerj/hints/0/change")
                .and_then(Value::as_str),
            Some("newer")
        );
        assert_eq!(merge_response_json("not-json", &hint), "not-json");
    }

    #[test]
    fn a_closed_catalog_port_is_silence() {
        assert!(check("http://127.0.0.1:1", None, "ok-knowledge").is_none());
    }
}
