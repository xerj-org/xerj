//! Corpus Hub v2 — project lockfile, freshness, and `status` (plan Step 8).
//!
//! These are the node-free, network-free pieces of Step 8: everything here is a
//! pure function or a graceful filesystem read, so it is unit-testable without a
//! running node, a hub server, or the network.
//!
//! What lives here (plan §8, `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`):
//! - the `xerj.lock` TOML reader/writer (`read_lock`, `write_lock`,
//!   `append_entry`, `project_root`, `lock_path`),
//! - the `--locked` check ([`locked_check`] → [`LockedCheck`], which the CLI maps
//!   to exit code 2 on [`LockedCheck::Mismatch`] — the library never calls
//!   `std::process::exit`),
//! - the freshness-warning helper ([`freshness_warning`]),
//! - the `xerj hub status` data model + plain-text renderer
//!   ([`gather_status`] / [`render_status`]).
//!
//! `xerj hub sync` is not here: it drives the pull path, so it lives next to the
//! other subcommands that do (`crate::sync`). This module only owns the on-disk
//! layout names it shares (`packs/`, `pack.json`, `last_used`, `last_sync`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use xerj_common::hub::{Catalogue, CatalogueEntry, PackManifest};

// ---------------------------------------------------------------------------
// xerj.lock
// ---------------------------------------------------------------------------

/// The lockfile's basename, written at the project root.
pub const LOCK_FILE_NAME: &str = "xerj.lock";

/// The only `version` this reader/writer emits and understands.
pub const LOCK_VERSION: u32 = 1;

fn default_lock_version() -> u32 {
    LOCK_VERSION
}

/// One `[[corpus]]` table in `xerj.lock`: the exact pin an answer was served
/// from in this project, so another machine with the same lock and `--locked`
/// resolves the identical pack.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    pub slug: String,
    pub pin_sha: String,
    pub pin_date: String,
    pub pack_sha256: String,
}

impl LockEntry {
    /// Build the lock entry for a corpus from the catalogue entry an answer was
    /// served from — the four fields plan §8 records (`slug`, `pin_sha`,
    /// `pin_date`, `pack_sha256`).
    pub fn from_catalogue(entry: &CatalogueEntry) -> Self {
        Self {
            slug: entry.slug.clone(),
            pin_sha: entry.pin_sha.clone(),
            pin_date: entry.pin_date.clone(),
            pack_sha256: entry.pack_sha256.clone(),
        }
    }
}

/// The whole `xerj.lock`: `version = 1` then a `[[corpus]]` table per corpus.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Lockfile {
    #[serde(default = "default_lock_version")]
    pub version: u32,
    /// Serialised as `[[corpus]]` tables.
    #[serde(default, rename = "corpus")]
    pub corpus: Vec<LockEntry>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: LOCK_VERSION,
            corpus: Vec::new(),
        }
    }
}

impl Lockfile {
    /// The locked entry for `slug`, if the project has pinned that corpus.
    pub fn lookup(&self, slug: &str) -> Option<&LockEntry> {
        self.corpus.iter().find(|e| e.slug == slug)
    }
}

/// The project root: the first ancestor of `start` (inclusive) that contains a
/// `.git` entry, else `start` itself. `.git` is matched whether it is a
/// directory (a normal clone) or a file (a worktree gitlink).
pub fn project_root(start: &Path) -> PathBuf {
    let mut dir: Option<&Path> = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.to_path_buf()
}

/// The path to `xerj.lock` for the project that owns `start`.
pub fn lock_path(start: &Path) -> PathBuf {
    project_root(start).join(LOCK_FILE_NAME)
}

/// Read and parse a lockfile. `Ok(None)` when the file does not exist; an error
/// only when it exists but cannot be read or parsed (a corrupt lock must not be
/// silently discarded).
pub fn read_lock(path: &Path) -> Result<Option<Lockfile>> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let lock: Lockfile = toml::from_str(&s)
                .with_context(|| format!("parsing lockfile {}", path.display()))?;
            Ok(Some(lock))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading lockfile {}", path.display())),
    }
}

/// Write the whole lockfile, replacing any existing file. This is the explicit
/// full rewrite (`xerj hub update --write`); for the common "serve an answer,
/// record its pin" path use [`append_entry`], which never clobbers an existing
/// corpus entry.
pub fn write_lock(path: &Path, lock: &Lockfile) -> Result<()> {
    let body = toml::to_string(lock).context("serialising lockfile")?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating lockfile dir {}", parent.display()))?;
        }
    }
    fs::write(path, body).with_context(|| format!("writing lockfile {}", path.display()))?;
    Ok(())
}

/// What [`append_entry`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// A new `[[corpus]]` table was added and the lock written (creating the
    /// file if it did not exist).
    Appended,
    /// A table with this slug already existed; the lock was left **untouched**
    /// (plan §8: "never rewritten silently").
    AlreadyPresent,
}

/// Append one corpus's pin to `xerj.lock`, creating the file (with
/// `version = 1`) the first time. If the slug is already locked the file is left
/// exactly as it was and [`AppendOutcome::AlreadyPresent`] is returned — an
/// existing pin is never silently rewritten.
pub fn append_entry(path: &Path, entry: LockEntry) -> Result<AppendOutcome> {
    let mut lock = read_lock(path)?.unwrap_or_default();
    if lock.lookup(&entry.slug).is_some() {
        return Ok(AppendOutcome::AlreadyPresent);
    }
    lock.corpus.push(entry);
    write_lock(path, &lock)?;
    Ok(AppendOutcome::Appended)
}

// ---------------------------------------------------------------------------
// --locked
// ---------------------------------------------------------------------------

/// The result of the `--locked` check. The CLI maps [`LockedCheck::Mismatch`] to
/// process exit code 2 (plan §8); the library itself never exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedCheck {
    /// The project has no locked pin for this corpus — nothing to enforce.
    Absent,
    /// The locked pin equals the catalogue's current pin: proceed.
    Match,
    /// The catalogue's pin differs from the locked pin: the CLI exits 2.
    Mismatch,
}

/// Compare a corpus's locked entry (from the project `xerj.lock`) against the
/// catalogue's current entry for that slug. With no locked entry the result is
/// [`LockedCheck::Absent`]; otherwise it is [`LockedCheck::Match`] or
/// [`LockedCheck::Mismatch`] on the `pin_sha`.
pub fn locked_check(locked: Option<&LockEntry>, current: &CatalogueEntry) -> LockedCheck {
    match locked {
        None => LockedCheck::Absent,
        Some(e) if e.pin_sha == current.pin_sha => LockedCheck::Match,
        Some(_) => LockedCheck::Mismatch,
    }
}

// ---------------------------------------------------------------------------
// freshness warning
// ---------------------------------------------------------------------------

/// The freshness warning (plan §8): returned when the question asks for the
/// current source **and** the pin is older than its refresh cadence. `None`
/// otherwise. Pure over its inputs.
///
/// The question "asks for the current source" when it contains one of
/// `current`, `latest`, `newest`, `now` as a standalone word — the literal
/// regex `\b(current|latest|newest|now)\b`, implemented without the `regex`
/// crate (see [`question_asks_for_current`]).
pub fn freshness_warning(
    question: &str,
    age_days: u32,
    refresh_cadence_days: u32,
) -> Option<String> {
    if age_days > refresh_cadence_days && question_asks_for_current(question) {
        Some(format!(
            "pin is {age_days} days old and the question asks for the current source"
        ))
    } else {
        None
    }
}

/// `\b(current|latest|newest|now)\b`. Word boundaries are honoured by splitting
/// on every character that is not a word character (`[A-Za-z0-9_]`, extended to
/// Unicode alphanumerics to mirror the `regex` crate's default `\b`) and testing
/// each token for an exact, case-sensitive match — so "currently" and
/// "recurrent" do not fire, while "now?" and "the latest API" do. Case follows
/// the literal regex (no `i` flag).
fn question_asks_for_current(question: &str) -> bool {
    const TRIGGERS: [&str; 4] = ["current", "latest", "newest", "now"];
    question
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| TRIGGERS.contains(&word))
}

// ---------------------------------------------------------------------------
// xerj hub status
// ---------------------------------------------------------------------------

pub(crate) const PACKS_DIR: &str = xerj_common::hub::HUB_PACKS_DIR;
pub(crate) const PACK_MANIFEST_FILE: &str = "pack.json";
pub(crate) const LAST_USED_FILE: &str = "last_used";
const NODE_PID_FILE: &str = "node.pid";
const NODE_PORT_FILE: &str = "node.port";
const CONFIG_FILE: &str = "config.toml";
pub(crate) const LAST_SYNC_FILE: &str = "last_sync";
const CATALOGUE_FILE: &str = "catalogue.json";

/// The default value reported for the `hosted` setting when `config.toml` is
/// absent or does not set it.
const DEFAULT_HOSTED: &str = "auto";

/// The default hub root: `XERJ_HUB_HOME` when set (the override every `xerj hub`
/// subcommand honours), else `~/.xerj/hub`. `status` takes the root as a
/// parameter (so tests inject a temp dir); callers that want the real location
/// pass this.
pub fn default_hub_root() -> PathBuf {
    hub_root_from(&|key| std::env::var_os(key))
}

/// [`default_hub_root`] over an injectable environment, so the rule is testable
/// without mutating the process-global one.
pub(crate) fn hub_root_from(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    // One rule with the server's hub-pack restore root (`xerj_common::hub`):
    // `XERJ_HUB_HOME`, else `HOME`/`USERPROFILE` + `.xerj/hub`. With no home at
    // all the CLI falls back to the current directory; the server allows nothing.
    xerj_common::hub::hub_root_from(env)
        .unwrap_or_else(|| PathBuf::from(".").join(".xerj").join("hub"))
}

/// One cached pack on disk (`<hub>/packs/<slug>/<pin8>/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackStatus {
    pub slug: String,
    /// The pin's 8-char prefix — the directory name itself.
    pub pin8: String,
    /// Pin age in days (from the pack manifest's `pin_date`, falling back to the
    /// directory mtime). `None` when neither is readable.
    pub age_days: Option<i64>,
    /// Total bytes of the restored pack directory.
    pub bytes: u64,
    /// Days since the pack's `last_used` file was last touched; `None` when the
    /// file is absent (never used since it was pulled).
    pub last_used_age_days: Option<i64>,
}

/// The local hub node, as read from its pid/port files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    /// True when a `node.pid` file is present with a parseable PID.
    pub running: bool,
    pub pid: Option<u32>,
    pub port: Option<u16>,
}

/// Everything `xerj hub status` reports, gathered gracefully from the hub root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubStatus {
    pub packs: Vec<PackStatus>,
    pub node: NodeStatus,
    /// The `hosted` setting from `config.toml`, or `"auto"` when unset.
    pub hosted: String,
    /// Days since `last_sync` was written; `None` when it has never run.
    pub last_sync_age_days: Option<i64>,
    /// Age in days of the cached catalogue; `None` when none is cached.
    pub catalogue_age_days: Option<i64>,
}

/// Read the whole hub status from `hub_root`. Every read is best-effort: a
/// missing file yields a "none"/"not running"/"auto" value, never an error or a
/// panic. `hub_root` is injectable so tests point it at a temp dir.
pub fn gather_status(hub_root: &Path) -> HubStatus {
    HubStatus {
        packs: gather_packs(hub_root),
        node: gather_node(hub_root),
        hosted: gather_hosted(hub_root),
        last_sync_age_days: path_age_days(&hub_root.join(LAST_SYNC_FILE)),
        catalogue_age_days: gather_catalogue_age(hub_root),
    }
}

fn gather_packs(hub_root: &Path) -> Vec<PackStatus> {
    let mut out = Vec::new();
    let Ok(slugs) = fs::read_dir(hub_root.join(PACKS_DIR)) else {
        return out;
    };
    for slug_ent in slugs.flatten() {
        if !slug_ent.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let slug = slug_ent.file_name().to_string_lossy().into_owned();
        let Ok(pins) = fs::read_dir(slug_ent.path()) else {
            continue;
        };
        for pin_ent in pins.flatten() {
            if !pin_ent.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let pin8 = pin_ent.file_name().to_string_lossy().into_owned();
            let dir = pin_ent.path();
            out.push(PackStatus {
                slug: slug.clone(),
                pin8,
                age_days: pack_age_days(&dir),
                bytes: dir_size(&dir),
                last_used_age_days: path_age_days(&dir.join(LAST_USED_FILE)),
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.pin8.cmp(&b.pin8)));
    out
}

/// Pin age for a pack: the manifest's `pin_date` when `pack.json` parses,
/// otherwise the directory's mtime age, otherwise `None`.
fn pack_age_days(dir: &Path) -> Option<i64> {
    if let Ok(s) = fs::read_to_string(dir.join(PACK_MANIFEST_FILE)) {
        if let Ok(manifest) = serde_json::from_str::<PackManifest>(&s) {
            if let Some(age) = date_age_days(&manifest.pin_date) {
                return Some(age);
            }
        }
    }
    path_age_days(dir)
}

fn gather_node(hub_root: &Path) -> NodeStatus {
    let pid = read_trimmed(&hub_root.join(NODE_PID_FILE)).and_then(|s| s.parse::<u32>().ok());
    let port = read_trimmed(&hub_root.join(NODE_PORT_FILE)).and_then(|s| s.parse::<u16>().ok());
    NodeStatus {
        running: pid.is_some(),
        pid,
        port,
    }
}

fn gather_hosted(hub_root: &Path) -> String {
    #[derive(Deserialize)]
    struct Cfg {
        hosted: Option<String>,
    }
    let Ok(s) = fs::read_to_string(hub_root.join(CONFIG_FILE)) else {
        return DEFAULT_HOSTED.to_string();
    };
    match toml::from_str::<Cfg>(&s) {
        Ok(cfg) => cfg.hosted.unwrap_or_else(|| DEFAULT_HOSTED.to_string()),
        Err(_) => DEFAULT_HOSTED.to_string(),
    }
}

fn gather_catalogue_age(hub_root: &Path) -> Option<i64> {
    let path = hub_root.join(CATALOGUE_FILE);
    let s = fs::read_to_string(&path).ok()?;
    if let Ok(cat) = serde_json::from_str::<Catalogue>(&s) {
        if let Some(age) = rfc3339_age_days(&cat.generated_at) {
            return Some(age);
        }
    }
    // Present but unparseable (or a format we do not know): fall back to mtime.
    path_age_days(&path)
}

/// Render [`HubStatus`] as plain text, one line per item (plan §8). Packs come
/// first (or a single `no packs` line), then node, hosted, last sync, catalogue.
pub fn render_status(status: &HubStatus) -> String {
    let mut lines: Vec<String> = Vec::new();

    if status.packs.is_empty() {
        lines.push("no packs".to_string());
    } else {
        for p in &status.packs {
            lines.push(format!(
                "pack: {} {}  {}  {}  last used {}",
                p.slug,
                p.pin8,
                days_old(p.age_days),
                human_bytes(p.bytes),
                days_ago(p.last_used_age_days),
            ));
        }
    }

    lines.push(match &status.node {
        NodeStatus { running: false, .. } => "node: not running".to_string(),
        NodeStatus {
            running: true,
            pid,
            port,
        } => format!(
            "node: running  pid {}  port {}",
            pid.map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string()),
            port.map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string()),
        ),
    });

    lines.push(format!("hosted: {}", status.hosted));
    lines.push(format!(
        "last sync: {}",
        days_ago(status.last_sync_age_days)
    ));
    lines.push(format!(
        "catalogue: {}",
        match status.catalogue_age_days {
            Some(d) => days_old(Some(d)),
            None => "none".to_string(),
        }
    ));

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Read a file to a trimmed string, or `None` when it is absent or blank.
fn read_trimmed(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Total size in bytes of a directory tree (files only; symlinks skipped so a
/// cycle cannot hang the walk).
pub(crate) fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for ent in entries.flatten() {
        match ent.file_type() {
            Ok(ft) if ft.is_dir() => total += dir_size(&ent.path()),
            Ok(ft) if ft.is_file() => {
                if let Ok(m) = ent.metadata() {
                    total += m.len();
                }
            }
            _ => {}
        }
    }
    total
}

/// Age in days from a path's modification time; `None` when it has no readable
/// mtime (e.g. the file does not exist).
fn path_age_days(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let dt: DateTime<Utc> = modified.into();
    Some((Utc::now() - dt).num_days())
}

/// Age in days from a `YYYY-MM-DD` pin date.
fn date_age_days(date: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    Some((Utc::now().date_naive() - d).num_days())
}

/// Age in days from an RFC 3339 timestamp.
fn rfc3339_age_days(ts: &str) -> Option<i64> {
    let dt = DateTime::parse_from_rfc3339(ts.trim()).ok()?;
    Some((Utc::now() - dt.with_timezone(&Utc)).num_days())
}

/// "`N days old`" / "`1 day old`" / "`age unknown`", pluralised.
fn days_old(age: Option<i64>) -> String {
    match age {
        Some(1) => "1 day old".to_string(),
        Some(d) => format!("{d} days old"),
        None => "age unknown".to_string(),
    }
}

/// "`today`" / "`1 day ago`" / "`N days ago`" / "`never`".
fn days_ago(age: Option<i64>) -> String {
    match age {
        None => "never".to_string(),
        Some(d) if d <= 0 => "today".to_string(),
        Some(1) => "1 day ago".to_string(),
        Some(d) => format!("{d} days ago"),
    }
}

/// Human-readable byte size, e.g. `0 B`, `512 B`, `2.0 MB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::tempdir;
    use xerj_common::hub::{LicenceClass, PackSource, Sketch, PACK_FORMAT_VERSION};

    // ---- xerj.lock -------------------------------------------------------

    fn entry(slug: &str, pin: &str) -> LockEntry {
        LockEntry {
            slug: slug.to_string(),
            pin_sha: pin.to_string(),
            pin_date: "2026-10-12".to_string(),
            pack_sha256: format!("sha-{slug}"),
        }
    }

    #[test]
    fn project_root_finds_dot_git_ancestor() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(project_root(&nested), root);
        assert_eq!(lock_path(&nested), root.join("xerj.lock"));
    }

    #[test]
    fn project_root_falls_back_to_start_without_git() {
        let tmp = tempdir().unwrap();
        let start = tmp.path().join("no-git-here");
        fs::create_dir_all(&start).unwrap();
        assert_eq!(project_root(&start), start);
    }

    #[test]
    fn lock_written_then_appended_leaves_first_untouched() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("xerj.lock");

        // First serve → file created with version 1 and one corpus.
        assert_eq!(
            append_entry(&path, entry("quinn", "aaaa1111")).unwrap(),
            AppendOutcome::Appended
        );
        let lock = read_lock(&path).unwrap().unwrap();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.corpus, vec![entry("quinn", "aaaa1111")]);

        // Second corpus appended; first table unchanged, order preserved.
        assert_eq!(
            append_entry(&path, entry("lz4", "bbbb2222")).unwrap(),
            AppendOutcome::Appended
        );
        let lock = read_lock(&path).unwrap().unwrap();
        assert_eq!(
            lock.corpus,
            vec![entry("quinn", "aaaa1111"), entry("lz4", "bbbb2222")]
        );

        // The TOML really is `version = 1` + `[[corpus]]` tables.
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("version = 1"), "raw was: {raw}");
        assert_eq!(raw.matches("[[corpus]]").count(), 2, "raw was: {raw}");
    }

    #[test]
    fn append_never_rewrites_an_existing_slug() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("xerj.lock");
        append_entry(&path, entry("quinn", "original0")).unwrap();

        // Same slug, different pin → left untouched, reported AlreadyPresent.
        assert_eq!(
            append_entry(&path, entry("quinn", "changed99")).unwrap(),
            AppendOutcome::AlreadyPresent
        );
        let lock = read_lock(&path).unwrap().unwrap();
        assert_eq!(lock.corpus.len(), 1);
        assert_eq!(lock.lookup("quinn").unwrap().pin_sha, "original0");
    }

    #[test]
    fn read_lock_absent_is_none() {
        let tmp = tempdir().unwrap();
        assert!(read_lock(&tmp.path().join("nope.lock")).unwrap().is_none());
    }

    #[test]
    fn lock_entry_from_catalogue_copies_the_four_fields() {
        let cat = sample_catalogue_entry("quinn", "cafed00d");
        let le = LockEntry::from_catalogue(&cat);
        assert_eq!(le.slug, "quinn");
        assert_eq!(le.pin_sha, "cafed00d");
        assert_eq!(le.pin_date, cat.pin_date);
        assert_eq!(le.pack_sha256, cat.pack_sha256);
    }

    // ---- --locked --------------------------------------------------------

    fn sample_catalogue_entry(slug: &str, pin: &str) -> CatalogueEntry {
        CatalogueEntry {
            slug: slug.to_string(),
            domain: "NET".to_string(),
            topics: vec!["compression".to_string()],
            ask_for: "x".to_string(),
            upstream: String::new(),
            pin_sha: pin.to_string(),
            pin_date: "2026-10-02".to_string(),
            refresh_cadence_days: 7,
            licence_spdx: "BSD-2-Clause".to_string(),
            licence_class: LicenceClass::AdaptWithAttribution,
            pack_bytes: 2_000_000,
            pack_url: "https://example/x.tar.zst".to_string(),
            pack_sha256: "deadbeef".to_string(),
            sig_url: "https://example/x.tar.zst.sig".to_string(),
            dependency_names: vec![],
            quality_grade: None,
            term_sketch: Sketch {
                m_bits: 64,
                k_hashes: 7,
                bits_base64: "AAAAAAAAAAA=".to_string(),
            },
        }
    }

    #[test]
    fn locked_check_match_mismatch_absent() {
        let current = sample_catalogue_entry("lz4", "pinAAAA");

        let same = entry("lz4", "pinAAAA");
        assert_eq!(locked_check(Some(&same), &current), LockedCheck::Match);

        let diff = entry("lz4", "pinBBBB");
        assert_eq!(locked_check(Some(&diff), &current), LockedCheck::Mismatch);

        assert_eq!(locked_check(None, &current), LockedCheck::Absent);
    }

    // ---- freshness -------------------------------------------------------

    #[test]
    fn freshness_fires_only_with_both_conditions() {
        // (regex matches, age > cadence) — all four combinations.
        assert_eq!(
            freshness_warning("what is the current API", 30, 7),
            Some("pin is 30 days old and the question asks for the current source".to_string())
        );
        // regex matches, age not older than cadence → None.
        assert_eq!(freshness_warning("what is the current API", 5, 7), None);
        // regex does not match, age older than cadence → None.
        assert_eq!(freshness_warning("how does the parser work", 30, 7), None);
        // neither → None.
        assert_eq!(freshness_warning("how does the parser work", 5, 7), None);
    }

    #[test]
    fn freshness_respects_word_boundaries_and_all_triggers() {
        // Each trigger as a standalone word fires (age 30 > cadence 7).
        for q in [
            "the current source",
            "give me the latest",
            "newest release?",
            "what is true now",
        ] {
            assert!(
                freshness_warning(q, 30, 7).is_some(),
                "expected a warning for {q:?}"
            );
        }
        // Substrings that are not whole words do not fire.
        for q in ["recurrent calls", "concurrently", "nowhere", "knowns"] {
            assert_eq!(
                freshness_warning(q, 30, 7),
                None,
                "did not expect a warning for {q:?}"
            );
        }
        // Boundary at exactly cadence: age must be strictly greater.
        assert_eq!(freshness_warning("current", 7, 7), None);
        assert!(freshness_warning("current", 8, 7).is_some());
    }

    // ---- status ----------------------------------------------------------

    fn make_pack(hub: &Path, slug: &str, pin8: &str, filler: &[u8]) -> PathBuf {
        let dir = hub.join("packs").join(slug).join(pin8);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data.bin"), filler).unwrap();
        dir
    }

    #[test]
    fn status_lists_exactly_the_packs_and_reports_node_down() {
        let tmp = tempdir().unwrap();
        let hub = tmp.path();
        make_pack(hub, "quinn", "0774d055", &[0u8; 1000]);
        make_pack(hub, "lz4", "deadbeef", &[0u8; 2048]);

        let status = gather_status(hub);

        // Exactly the two packs on disk, sorted by slug then pin8.
        let got: Vec<(String, String)> = status
            .packs
            .iter()
            .map(|p| (p.slug.clone(), p.pin8.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("lz4".to_string(), "deadbeef".to_string()),
                ("quinn".to_string(), "0774d055".to_string()),
            ]
        );
        // Bytes are the directory contents.
        assert_eq!(status.packs[0].bytes, 2048);
        assert_eq!(status.packs[1].bytes, 1000);
        // No last_used file yet.
        assert!(status.packs[0].last_used_age_days.is_none());

        // No pid file → node down; defaults for the rest.
        assert!(!status.node.running);
        assert_eq!(status.hosted, "auto");
        assert!(status.catalogue_age_days.is_none());

        let text = render_status(&status);
        assert!(text.contains("node: not running"), "text was:\n{text}");
        assert!(text.contains("hosted: auto"), "text was:\n{text}");
        assert!(text.contains("pack: quinn 0774d055"), "text was:\n{text}");
        assert!(text.contains("pack: lz4 deadbeef"), "text was:\n{text}");
        assert!(text.contains("catalogue: none"), "text was:\n{text}");
    }

    #[test]
    fn status_empty_hub_says_no_packs() {
        let tmp = tempdir().unwrap();
        let status = gather_status(tmp.path());
        assert!(status.packs.is_empty());
        let text = render_status(&status);
        assert!(text.contains("no packs"), "text was:\n{text}");
        assert!(text.contains("node: not running"));
        assert!(text.contains("last sync: never"));
    }

    #[test]
    fn status_reads_node_pid_port_and_hosted() {
        let tmp = tempdir().unwrap();
        let hub = tmp.path();
        fs::write(hub.join("node.pid"), "4242\n").unwrap();
        fs::write(hub.join("node.port"), "9410\n").unwrap();
        fs::write(hub.join("config.toml"), "hosted = \"always\"\n").unwrap();

        let status = gather_status(hub);
        assert!(status.node.running);
        assert_eq!(status.node.pid, Some(4242));
        assert_eq!(status.node.port, Some(9410));
        assert_eq!(status.hosted, "always");

        let text = render_status(&status);
        assert!(
            text.contains("node: running  pid 4242  port 9410"),
            "text was:\n{text}"
        );
        assert!(text.contains("hosted: always"));
    }

    #[test]
    fn status_pack_age_from_manifest_pin_date() {
        let tmp = tempdir().unwrap();
        let hub = tmp.path();
        let dir = make_pack(hub, "quinn", "0774d055", &[1u8; 16]);

        let pin_date = (Utc::now().date_naive() - Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let manifest = PackManifest {
            format_version: PACK_FORMAT_VERSION,
            slug: "quinn".to_string(),
            pin_sha: "0774d055ffffffffffffffffffffffffffffffff".to_string(),
            pin_date,
            built_at: "2026-10-04T00:00:00Z".to_string(),
            builder_version: "test".to_string(),
            index_prefix: "xc-quinn-b1".to_string(),
            indices: vec![],
            snapshot_name: "snap".to_string(),
            licence_spdx: "MIT".to_string(),
            licence_class: LicenceClass::AdaptWithAttribution,
            sources: vec![PackSource {
                repo: "quinn".to_string(),
                url: "https://example/quinn".to_string(),
                sha: "0774d055".to_string(),
                licence: "MIT".to_string(),
            }],
            defs_count: 0,
            files_count: 0,
        };
        fs::write(
            dir.join("pack.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        // last_used present → a fresh touch reads as "today".
        fs::write(dir.join("last_used"), "2026-10-14T00:00:00Z").unwrap();

        let status = gather_status(hub);
        assert_eq!(status.packs.len(), 1);
        assert_eq!(status.packs[0].age_days, Some(10));
        assert_eq!(status.packs[0].last_used_age_days, Some(0));
        assert!(render_status(&status).contains("10 days old"));
    }

    #[test]
    fn hub_root_honours_the_override_then_home() {
        use std::ffi::OsString;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| -> Option<OsString> {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| OsString::from(*v))
            }
        };
        // XERJ_HUB_HOME wins — the same override `xerj hub pull` honours, so a
        // sync and the pulls it drives always look at one cache.
        assert_eq!(
            hub_root_from(&env(&[("XERJ_HUB_HOME", "/srv/hub"), ("HOME", "/home/u")])),
            PathBuf::from("/srv/hub")
        );
        // An EMPTY override is not an override.
        assert_eq!(
            hub_root_from(&env(&[("XERJ_HUB_HOME", ""), ("HOME", "/home/u")])),
            PathBuf::from("/home/u/.xerj/hub")
        );
        assert_eq!(
            hub_root_from(&env(&[("HOME", "/home/u")])),
            PathBuf::from("/home/u/.xerj/hub")
        );
        // Native Windows: no HOME, so the profile dir is the home.
        assert_eq!(
            hub_root_from(&env(&[("USERPROFILE", "/Users/u")])),
            PathBuf::from("/Users/u/.xerj/hub")
        );
        // No HOME at all degrades to a relative path, never a panic.
        assert_eq!(hub_root_from(&env(&[])), PathBuf::from("./.xerj/hub"));
    }

    // ---- small helpers ---------------------------------------------------

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(2_000_000), "1.9 MB");
    }

    #[test]
    fn day_phrases_pluralise() {
        assert_eq!(days_old(Some(1)), "1 day old");
        assert_eq!(days_old(Some(5)), "5 days old");
        assert_eq!(days_old(None), "age unknown");
        assert_eq!(days_ago(None), "never");
        assert_eq!(days_ago(Some(0)), "today");
        assert_eq!(days_ago(Some(1)), "1 day ago");
        assert_eq!(days_ago(Some(3)), "3 days ago");
    }
}
