//! Corpus Hub v2 — `xerj hub sync` and `xerj hub sync --install-timer` (plan §8).
//!
//! `sync` keeps the pack cache (`~/.xerj/hub/packs/<slug>/<pin8>/`) in step with
//! the catalogue, in this order:
//!
//! 1. **Refresh the catalogue** ([`crate::catalogue::refresh_catalogue`]): a
//!    conditional GET that ignores the 24 h cache window, with the detached
//!    signature verified before the cached copy is replaced. If the hub cannot be
//!    reached or the signature does not verify, sync stops and changes nothing.
//! 2. **Update every cached pack whose catalogue pin moved.** The new pin is
//!    pulled by `xerj hub pull <slug>` — the existing pull path, run as a child
//!    process of this same binary — so the archive's sha256 and its ed25519
//!    signature are verified before a byte is extracted, exactly as for a first
//!    pull. The OLD pin directory is removed only after the pull exited 0 and
//!    the new directory is complete and recorded in the state ledger; a failed
//!    pull (network, verification, restore) leaves the old pin in place and
//!    serving.
//! 3. **Prune to the disk budget**, least recently used first. Packs updated in
//!    this run are not candidates (they were just fetched), and an update keeps
//!    the old pin's `last_used` recency: refreshing a pack is not using it.
//! 4. **Write `last_sync`** (only when nothing failed), the file `xerj hub status`
//!    reads for "last sync".
//!
//! Only extracted pack directories are removed. The restored indices live in the
//! node's own data directory and are not touched here — deleting indices from a
//! node is not sync's business (the same rule the pull-time eviction follows).
//!
//! The pure parts — [`scan_cache`], [`plan_slug`], [`prune_to_budget`], the unit
//! renderers — take plain arguments, and the parts that touch the outside world
//! (pulling, reading the ledger) sit behind [`SyncOps`], so the tests drive the
//! whole flow with throwaway directories and no network, and `tests/sync_e2e.rs`
//! drives it against real nodes and the real pull.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;
use xerj_common::hub::{
    Catalogue, CatalogueEntry, LicenceClass, PackManifest, AUTO_PULL_MAX_BYTES, DEFAULT_HUB_URL,
    DISK_BUDGET_BYTES,
};
use xerj_common::xccode::state;

use crate::catalogue::refresh_catalogue;
use crate::lockfile::{
    default_hub_root, dir_size, hub_root_from, human_bytes, LAST_SYNC_FILE, LAST_USED_FILE,
    PACKS_DIR, PACK_MANIFEST_FILE,
};

/// Exit codes of `xerj hub sync`. 3 is "a pack failed verification", kept apart
/// from 1 ("the machine or network got in the way") for the same reason
/// `xerj hub pull` keeps them apart.
const EXIT_OK: i32 = 0;
const EXIT_INFRA: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_VERIFY: i32 = 3;

/// `xerj hub sync -h`. A function, not a constant, so the budget it names is the
/// one the code enforces.
fn sync_usage() -> String {
    format!(
        "usage: xerj hub sync [--url <node>]
       xerj hub sync --install-timer

Refresh the catalogue, then bring every cached pack up to the catalogue's pin:
the new pin is pulled by `xerj hub pull` (sha256 and signature are verified
before anything is extracted), the old pin's directory is removed only after the
new one is restored, and the cache is pruned to the {budget} disk budget, least
recently used first. `~/.xerj/hub/last_sync` is written when nothing failed.

  --url <node>      the node the packs are restored into (passed to `hub pull`)
  --install-timer   write a systemd user timer (Linux) or a launchd agent
                    (macOS) that runs `xerj hub sync` every 6 hours, and print
                    its path. Nothing is enabled, loaded or started: the
                    comment at the top of the written file says how.

exit codes: 0 synced (or nothing to do); 1 the catalogue could not be refreshed
or a pack could not be updated (its old pin is kept); 2 usage error; 3 a pack
failed verification (its old pin is kept)",
        budget = human_bytes(DISK_BUDGET_BYTES)
    )
}

// ── the cache on disk ────────────────────────────────────────────────────────

/// One extracted pack: `<hub>/packs/<slug>/<pin8>/`.
#[derive(Debug, Clone)]
pub struct CachedPin {
    pub slug: String,
    /// The directory name: the first 8 hex characters of the pack's `pin_sha`.
    pub pin8: String,
    pub dir: PathBuf,
    /// `pin_date` from the pack's own `pack.json`, when it parses.
    pub pin_date: Option<String>,
    /// `pack.json` parses and every file `SHA256SUMS` names is on disk. A
    /// directory that fails this is a half-extracted leftover, not a pack.
    pub complete: bool,
    /// Recency for LRU: the mtime of the pack's `last_used` file, else of the
    /// directory itself (the same rule the pull-time eviction uses).
    pub last_used: SystemTime,
}

/// `^[0-9a-f]{8}$` — what a pin directory is called. Anything else under a slug
/// directory (`<pin8>.tmp`, `<pin8>.lock`, stray files) is not a pack.
fn is_pin8(name: &str) -> bool {
    name.len() == 8 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// First 8 hex characters of a commit, lower-cased — the `pin8` in a pack name.
/// `None` for anything that is not at least 8 hex characters.
fn pin8_of(pin_sha: &str) -> Option<String> {
    let s = pin_sha.trim();
    (s.len() >= 8 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s[..8].to_ascii_lowercase())
}

/// Every pin directory under `<hub>/packs`, sorted by slug then pin. Cheap: no
/// directory is walked for its size (only the files each `SHA256SUMS` names are
/// stat'ed). Symlinks are never followed.
pub fn scan_cache(hub_home: &Path) -> Vec<CachedPin> {
    let mut out = Vec::new();
    let Ok(slugs) = fs::read_dir(hub_home.join(PACKS_DIR)) else {
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
            let pin8 = pin_ent.file_name().to_string_lossy().into_owned();
            if !pin_ent.file_type().is_ok_and(|t| t.is_dir()) || !is_pin8(&pin8) {
                continue;
            }
            let dir = pin_ent.path();
            let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
            let manifest = read_pack_manifest(&dir);
            out.push(CachedPin {
                slug: slug.clone(),
                complete: manifest.is_some() && every_listed_file_exists(&dir),
                pin_date: manifest.map(|m| m.pin_date),
                last_used: mtime(&dir.join(LAST_USED_FILE))
                    .or_else(|| mtime(&dir))
                    .unwrap_or(UNIX_EPOCH),
                pin8,
                dir,
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.pin8.cmp(&b.pin8)));
    out
}

fn read_pack_manifest(dir: &Path) -> Option<PackManifest> {
    let raw = fs::read(dir.join(PACK_MANIFEST_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Every path `SHA256SUMS` lists exists under `dir`. The extractor writes
/// `pack.json` before the snapshot files, so a parsing `pack.json` alone does not
/// prove the extraction finished; the listing does. Existence only — re-hashing
/// hundreds of MB every six hours would cost more than it protects. A path that
/// is absolute or climbs out of `dir` makes the listing, and so the pack, invalid.
fn every_listed_file_exists(dir: &Path) -> bool {
    let Ok(sums) = fs::read_to_string(dir.join("SHA256SUMS")) else {
        return false;
    };
    sums.lines().filter(|l| !l.trim().is_empty()).all(|line| {
        let Some((_, rel)) = line.split_once("  ") else {
            return false;
        };
        let rel = Path::new(rel);
        rel.components().all(|c| matches!(c, Component::Normal(_))) && dir.join(rel).is_file()
    })
}

// ── what to do about one slug ────────────────────────────────────────────────

/// What the state ledger (`<code root>/state/<slug>.json`) says about a slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ledger {
    /// No readable ledger: the pack was never fully restored, or the ledger was
    /// deleted.
    Missing,
    /// The slug is served from a LOCAL index (`xerj corpus index`), not a hub
    /// pack. Sync does not flip that choice back behind the user's back.
    Local,
    /// A hub pack was restored; `pin8` is the first 8 hex of its `pin_sha`.
    Hubpack { pin8: Option<String> },
}

/// What [`run_sync_with`] decided for one slug.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// The catalogue pin is cached, complete and restored. `superseded` are
    /// older pin directories still lying around, now safe to remove.
    Current { superseded: Vec<CachedPin> },
    /// Pull `target`; remove `old` afterwards, and only then.
    Pull {
        target: String,
        old: Vec<CachedPin>,
        why: PullWhy,
    },
    /// Leave the slug alone and say why.
    Keep(KeepWhy),
}

/// Why a pull is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullWhy {
    /// The catalogue's pin is not cached.
    NewPin,
    /// The pin directory is there but the ledger does not record it as restored
    /// (an interrupted pull, a deleted ledger).
    NotRestored,
    /// The pin directory is there but incomplete.
    Damaged,
}

/// Why a slug was left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepWhy {
    NotInCatalogue,
    /// The catalogue's `pin_date` is older than the newest cached pin's: a
    /// replayed or rolled-back catalogue must not downgrade a cache.
    CatalogueOlder {
        catalogue: String,
        cached: String,
    },
    /// The catalogue now classes the corpus `excluded`; it is never pulled.
    Excluded,
    /// The new pack is over `AUTO_PULL_MAX_BYTES`; sync never auto-pulls those.
    OverCap {
        bytes: u64,
    },
    /// The catalogue's `pin_sha` is not a hex commit.
    BadPin(String),
    /// Served from a local index ([`Ledger::Local`]).
    LocalIndex,
}

impl KeepWhy {
    fn describe(&self) -> String {
        match self {
            KeepWhy::NotInCatalogue => "not in the catalogue".into(),
            KeepWhy::CatalogueOlder { catalogue, cached } => {
                format!("the catalogue's pin ({catalogue}) is older than the cached one ({cached})")
            }
            KeepWhy::Excluded => {
                "the catalogue now classes it excluded, so no new pin is pulled".into()
            }
            KeepWhy::OverCap { bytes } => format!(
                "its new pack is {} bytes, over the {}-byte auto-pull cap",
                bytes, AUTO_PULL_MAX_BYTES
            ),
            KeepWhy::BadPin(p) => format!("the catalogue's pin '{p}' is not a hex commit"),
            KeepWhy::LocalIndex => {
                "it is served from a local index (`xerj corpus index`), not a hub pack".into()
            }
        }
    }
}

fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Decide what to do about one slug. Pure: `entry` is the catalogue's entry for
/// the slug (if any), `cached` its pin directories on disk (never empty), and
/// `ledger` what the state ledger records.
pub fn plan_slug(entry: Option<&CatalogueEntry>, cached: &[CachedPin], ledger: &Ledger) -> Verdict {
    let Some(entry) = entry else {
        return Verdict::Keep(KeepWhy::NotInCatalogue);
    };
    let Some(target) = pin8_of(&entry.pin_sha) else {
        return Verdict::Keep(KeepWhy::BadPin(entry.pin_sha.clone()));
    };
    if *ledger == Ledger::Local {
        return Verdict::Keep(KeepWhy::LocalIndex);
    }

    let others: Vec<CachedPin> = cached
        .iter()
        .filter(|c| c.pin8 != target)
        .cloned()
        .collect();
    let have = cached.iter().find(|c| c.pin8 == target);
    let restored = matches!(ledger, Ledger::Hubpack { pin8: Some(p) } if *p == target);

    if let Some(have) = have {
        if have.complete && restored {
            return Verdict::Current { superseded: others };
        }
    }

    // A pull is needed. Refuse what sync must never auto-pull, and refuse to go
    // BACKWARDS in time, before spending a download on it.
    if entry.licence_class == LicenceClass::Excluded {
        return Verdict::Keep(KeepWhy::Excluded);
    }
    if let (Some(cat_day), Some(cached_day)) = (
        parse_day(&entry.pin_date),
        others
            .iter()
            .filter_map(|c| c.pin_date.as_deref().and_then(parse_day))
            .max(),
    ) {
        if cat_day < cached_day {
            return Verdict::Keep(KeepWhy::CatalogueOlder {
                catalogue: entry.pin_date.clone(),
                cached: cached_day.format("%Y-%m-%d").to_string(),
            });
        }
    }
    if entry.pack_bytes > AUTO_PULL_MAX_BYTES {
        return Verdict::Keep(KeepWhy::OverCap {
            bytes: entry.pack_bytes,
        });
    }

    let why = match have {
        None => PullWhy::NewPin,
        Some(h) if !h.complete => PullWhy::Damaged,
        Some(_) => PullWhy::NotRestored,
    };
    Verdict::Pull {
        target,
        old: others,
        why,
    }
}

// ── the outside world ────────────────────────────────────────────────────────

/// What a pull attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    Done,
    /// The pull failed with `code` (the exit code of `xerj hub pull`: 3 means
    /// the pack failed verification, anything else is the machine or network).
    Failed {
        code: i32,
        message: String,
    },
}

/// The two things sync needs from the outside world. The production
/// implementation is [`CliOps`]; tests substitute their own.
pub trait SyncOps {
    /// Pull `slug`'s catalogue pin through the verified pull path.
    fn pull(&self, slug: &str) -> PullOutcome;
    /// What the state ledger records for `slug`.
    fn ledger(&self, slug: &str) -> Ledger;
}

/// Runs `xerj hub pull <slug> --quiet [--url <node>]` as a child process of this
/// same binary and reads the ledger the pull writes.
///
/// A child process, not a function call, so sync gets whatever the pull
/// subcommand does — node selection, locks, the size cap, signature
/// verification — without re-implementing any of it, and so a change to the
/// pull path cannot silently diverge from what sync runs.
pub struct CliOps {
    program: PathBuf,
    /// Arguments placed before `hub pull …` (empty for the real binary).
    pre_args: Vec<OsString>,
    url: Option<String>,
    code_root: PathBuf,
}

impl CliOps {
    /// The running binary, with the ledger at `XERJ_CODE_HOME` (else
    /// `~/.xerj-code`) — the same place `hub pull` writes it.
    pub fn current_exe(url: Option<String>) -> Result<CliOps> {
        let program = std::env::current_exe().context("cannot locate the running xerj binary")?;
        Ok(CliOps {
            program,
            pre_args: Vec::new(),
            url,
            code_root: xerj_autoindex::xc::code_root(),
        })
    }

    /// Any program that answers `hub pull` — the unit tests point this at a
    /// shell script.
    pub fn with_program(
        program: impl Into<PathBuf>,
        pre_args: Vec<OsString>,
        url: Option<String>,
        code_root: PathBuf,
    ) -> CliOps {
        CliOps {
            program: program.into(),
            pre_args,
            url,
            code_root,
        }
    }
}

impl SyncOps for CliOps {
    fn pull(&self, slug: &str) -> PullOutcome {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.pre_args)
            .args(["hub", "pull"])
            .arg(slug)
            .arg("--quiet");
        if let Some(url) = &self.url {
            cmd.arg("--url").arg(url);
        }
        cmd.stdin(Stdio::null());
        match cmd.output() {
            Ok(out) if out.status.success() => PullOutcome::Done,
            Ok(out) => PullOutcome::Failed {
                // No code means the child was killed by a signal.
                code: out.status.code().unwrap_or(EXIT_INFRA),
                message: one_line(&String::from_utf8_lossy(&out.stderr), 400),
            },
            Err(e) => PullOutcome::Failed {
                code: EXIT_INFRA,
                message: format!("cannot run {}: {e}", self.program.display()),
            },
        }
    }

    fn ledger(&self, slug: &str) -> Ledger {
        match state::load_state(&self.code_root, slug) {
            Err(_) => Ledger::Missing,
            Ok(st) if !st.is_hubpack() => Ledger::Local,
            Ok(st) => Ledger::Hubpack {
                pin8: st.pin_sha.as_deref().and_then(pin8_of),
            },
        }
    }
}

/// Collapse whitespace (including newlines) to single spaces and cap the length,
/// so a child's stderr becomes one readable line.
fn one_line(s: &str, max: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max {
        joined
    } else {
        let mut cut: String = joined.chars().take(max).collect();
        cut.push('…');
        cut
    }
}

// ── the sync itself ──────────────────────────────────────────────────────────

/// Where a progress line goes: stdout (`Info`) or stderr (`Warn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

/// One pack that could not be updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    pub slug: String,
    pub code: i32,
    pub message: String,
}

/// A pack directory the budget pass removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    pub slug: String,
    pub pin8: String,
    pub bytes: u64,
}

/// The outcome of one sync, slug by slug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub updated: Vec<String>,
    pub current: Vec<String>,
    pub kept: Vec<String>,
    pub failed: Vec<SyncFailure>,
    pub pruned: Vec<Pruned>,
}

impl SyncReport {
    /// 3 when any pack failed verification, else 1 when any pack failed, else 0.
    pub fn exit_code(&self) -> i32 {
        if self.failed.iter().any(|f| f.code == EXIT_VERIFY) {
            EXIT_VERIFY
        } else if !self.failed.is_empty() {
            EXIT_INFRA
        } else {
            EXIT_OK
        }
    }
}

fn pin_list(pins: &[CachedPin]) -> String {
    pins.iter()
        .map(|p| p.pin8.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Sync `hub_home` against `cat`: update, then prune to `budget` bytes. Progress
/// goes to `emit` line by line as it happens, so a long sync is never silent.
pub fn run_sync_with(
    hub_home: &Path,
    cat: &Catalogue,
    ops: &dyn SyncOps,
    budget: u64,
    emit: &mut dyn FnMut(Level, &str),
) -> SyncReport {
    let mut report = SyncReport::default();
    let mut by_slug: BTreeMap<String, Vec<CachedPin>> = BTreeMap::new();
    for pin in scan_cache(hub_home) {
        by_slug.entry(pin.slug.clone()).or_default().push(pin);
    }
    if by_slug.is_empty() {
        emit(Level::Info, "hub sync: no cached packs to update");
    }

    // Slugs this run fetched: not candidates for the budget pass below.
    let mut fresh: BTreeSet<String> = BTreeSet::new();

    for (slug, pins) in &by_slug {
        let entry = cat.corpora.iter().find(|c| &c.slug == slug);
        match plan_slug(entry, pins, &ops.ledger(slug)) {
            Verdict::Current { superseded } => {
                let target = entry.and_then(|e| pin8_of(&e.pin_sha)).unwrap_or_default();
                if superseded.is_empty() {
                    emit(Level::Info, &format!("hub sync: {slug} {target}: current"));
                } else if remove_pins(&superseded).len() == superseded.len() {
                    emit(
                        Level::Info,
                        &format!(
                            "hub sync: {slug} {target}: current; removed superseded pin {}",
                            pin_list(&superseded)
                        ),
                    );
                } else {
                    emit(
                        Level::Warn,
                        &format!(
                            "hub sync: {slug} {target}: current; could not remove superseded pin {}",
                            pin_list(&superseded)
                        ),
                    );
                }
                report.current.push(slug.clone());
            }
            Verdict::Keep(why) => {
                emit(
                    Level::Info,
                    &format!(
                        "hub sync: {slug} {}: kept ({})",
                        pin_list(pins),
                        why.describe()
                    ),
                );
                report.kept.push(slug.clone());
            }
            Verdict::Pull { target, old, why } => {
                let from = if old.is_empty() {
                    String::new()
                } else {
                    format!("{} -> ", pin_list(&old))
                };
                match pull_and_replace(hub_home, slug, &target, &old, ops) {
                    Ok(()) => {
                        let what = match why {
                            PullWhy::NewPin => "updated",
                            PullWhy::NotRestored => {
                                "re-pulled (the pin was cached but not restored)"
                            }
                            PullWhy::Damaged => "re-pulled (the cached pin was incomplete)",
                        };
                        let tail = if old.is_empty() {
                            String::new()
                        } else {
                            "; old pin removed".to_string()
                        };
                        emit(
                            Level::Info,
                            &format!("hub sync: {slug} {from}{target}: {what}{tail}"),
                        );
                        fresh.insert(slug.clone());
                        report.updated.push(slug.clone());
                    }
                    Err((code, message)) => {
                        let kept = if old.is_empty() {
                            String::new()
                        } else {
                            format!("; old pin {} kept", pin_list(&old))
                        };
                        emit(
                            Level::Warn,
                            &format!(
                                "hub sync: {slug} {from}{target}: FAILED (exit {code}: {message}){kept}"
                            ),
                        );
                        report.failed.push(SyncFailure {
                            slug: slug.clone(),
                            code,
                            message,
                        });
                    }
                }
            }
        }
    }

    let (pruned, total) = prune_to_budget(hub_home, budget, &fresh);
    for p in &pruned {
        emit(
            Level::Info,
            &format!(
                "hub sync: pruned {} {} ({}, least recently used) to stay under the {} disk budget",
                p.slug,
                p.pin8,
                human_bytes(p.bytes),
                human_bytes(budget)
            ),
        );
    }
    if total > budget {
        emit(
            Level::Warn,
            &format!(
                "hub sync: the cache is {} over the {} disk budget and nothing more can be \
                 pruned (what is left was updated in this run, or could not be removed)",
                human_bytes(total - budget),
                human_bytes(budget)
            ),
        );
    }
    report.pruned = pruned;

    emit(
        Level::Info,
        &format!(
            "hub sync: {} updated, {} current, {} kept, {} failed, {} pruned",
            report.updated.len(),
            report.current.len(),
            report.kept.len(),
            report.failed.len(),
            report.pruned.len()
        ),
    );
    report
}

/// Pull `slug`'s catalogue pin and, only if that left the new pin complete and
/// recorded as restored, remove the `old` pin directories. On any failure the old
/// pins are untouched. The error is `(exit code, message)`.
fn pull_and_replace(
    hub_home: &Path,
    slug: &str,
    target: &str,
    old: &[CachedPin],
    ops: &dyn SyncOps,
) -> std::result::Result<(), (i32, String)> {
    if let PullOutcome::Failed { code, message } = ops.pull(slug) {
        return Err((code, message));
    }

    // `hub pull` exited 0. Trust, but check what "restored" is supposed to mean
    // before the only irreversible step: the new pin's directory is complete,
    // and the ledger — written only after the restore — names it.
    let landed = scan_cache(hub_home)
        .into_iter()
        .find(|c| c.slug == slug && c.pin8 == target);
    if !landed.as_ref().is_some_and(|c| c.complete) {
        return Err((
            EXIT_INFRA,
            format!("`hub pull` reported success but {slug}/{target} is missing or incomplete"),
        ));
    }
    match ops.ledger(slug) {
        Ledger::Hubpack { pin8: Some(p) } if p == target => {}
        other => {
            return Err((
                EXIT_INFRA,
                format!(
                    "`hub pull` reported success but the ledger does not record {target} as \
                     restored ({other:?})"
                ),
            ))
        }
    }

    carry_last_used(old, landed.as_ref().map(|c| c.dir.as_path()));
    let stuck: Vec<String> = old
        .iter()
        .filter_map(|p| {
            remove_pin_dir(&p.dir)
                .err()
                .map(|e| format!("{}: {e}", p.pin8))
        })
        .collect();
    if stuck.is_empty() {
        Ok(())
    } else {
        Err((
            EXIT_INFRA,
            format!(
                "the new pin is in place but the old pin could not be removed ({})",
                stuck.join("; ")
            ),
        ))
    }
}

/// Remove one pin directory. One that is already gone — a pull-time eviction
/// running beside this sync can take another corpus's pack — counts as removed.
fn remove_pin_dir(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Remove pin directories, returning the pin8s that went. Failures are skipped
/// (the next sync tries again).
fn remove_pins(pins: &[CachedPin]) -> Vec<String> {
    pins.iter()
        .filter(|p| remove_pin_dir(&p.dir).is_ok())
        .map(|p| p.pin8.clone())
        .collect()
}

/// An update is not a use: give the new pin directory the newest recency of the
/// pins it replaces, so a refresh does not make an idle pack look hot.
fn carry_last_used(old: &[CachedPin], new_dir: Option<&Path>) {
    let (Some(new_dir), Some(when)) = (new_dir, old.iter().map(|p| p.last_used).max()) else {
        return;
    };
    let secs = when
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let marker = new_dir.join(LAST_USED_FILE);
    // Best effort: LRU order is a courtesy, never a reason to fail an update.
    let _ = fs::write(&marker, secs.to_string());
    let _ = fs::OpenOptions::new()
        .write(true)
        .open(&marker)
        .and_then(|f| f.set_modified(when));
}

/// Remove whole pack directories, least recently used first (incomplete ones
/// before any complete one), until the pin directories under `<hub>/packs` fit in
/// `budget` bytes. Slugs in `protect` are never candidates. Returns what was
/// removed and the size that remains.
///
/// The budget counts the pin directories — what this can remove — and not the
/// `<pin8>.tmp` / `<pin8>.lock` entries of pulls in flight (or abandoned by a
/// crash), which belong to the pull that made them: counting them would evict
/// good packs to make room for files this cannot free.
pub fn prune_to_budget(
    hub_home: &Path,
    budget: u64,
    protect: &BTreeSet<String>,
) -> (Vec<Pruned>, u64) {
    let mut sized: Vec<(CachedPin, u64)> = scan_cache(hub_home)
        .into_iter()
        .map(|c| {
            let bytes = dir_size(&c.dir);
            (c, bytes)
        })
        .collect();
    let mut total: u64 = sized.iter().map(|(_, b)| *b).sum();
    let mut pruned = Vec::new();
    if total <= budget {
        return (pruned, total);
    }
    sized.retain(|(c, _)| !protect.contains(&c.slug));
    sized.sort_by(|(a, _), (b, _)| {
        a.complete
            .cmp(&b.complete)
            .then_with(|| a.last_used.cmp(&b.last_used))
            .then_with(|| a.slug.cmp(&b.slug))
            .then_with(|| a.pin8.cmp(&b.pin8))
    });
    for (c, bytes) in sized {
        if total <= budget {
            break;
        }
        if fs::remove_dir_all(&c.dir).is_ok() {
            total = total.saturating_sub(bytes);
            // Drop the slug directory once its last pin is gone.
            if let Some(parent) = c.dir.parent() {
                let _ = fs::remove_dir(parent);
            }
            pruned.push(Pruned {
                slug: c.slug,
                pin8: c.pin8,
                bytes,
            });
        }
    }
    (pruned, total)
}

/// Stamp `<hub>/last_sync` with the current UTC time (RFC 3339); `xerj hub
/// status` reads its age.
pub fn write_last_sync(hub_home: &Path) -> Result<()> {
    fs::create_dir_all(hub_home).with_context(|| format!("create {}", hub_home.display()))?;
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let path = hub_home.join(LAST_SYNC_FILE);
    fs::write(&path, format!("{stamp}\n")).with_context(|| format!("write {}", path.display()))
}

// ── the timer ────────────────────────────────────────────────────────────────

/// The systemd unit pair's basename (`<name>.service` + `<name>.timer`).
pub const SYSTEMD_UNIT_BASENAME: &str = "xerj-hub-sync";
/// The launchd agent's label, and the plist's basename.
pub const LAUNCHD_LABEL: &str = "org.xerj.hub-sync";
/// `StartInterval` for launchd: six hours, in seconds.
const LAUNCHD_INTERVAL_SECS: u32 = 6 * 60 * 60;

/// Which scheduler a timer is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    /// A systemd user timer (`~/.config/systemd/user/`).
    SystemdUser,
    /// A launchd agent (`~/Library/LaunchAgents/`).
    Launchd,
}

impl TimerKind {
    /// The scheduler of the OS this binary was built for; `None` where neither
    /// exists (Windows).
    pub fn for_this_os() -> Option<TimerKind> {
        if cfg!(target_os = "linux") {
            Some(TimerKind::SystemdUser)
        } else if cfg!(target_os = "macos") {
            Some(TimerKind::Launchd)
        } else {
            None
        }
    }
}

/// Where the timer files go. Injectable so the tests write into a temp dir.
#[derive(Debug, Clone)]
pub struct TimerDirs {
    /// `$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`.
    pub systemd_user: PathBuf,
    /// `~/Library/LaunchAgents`.
    pub launch_agents: PathBuf,
    /// The hub home: launchd's log lives here (`sync.log`).
    pub hub_home: PathBuf,
}

impl TimerDirs {
    pub fn from_env(env: &dyn Fn(&str) -> Option<OsString>) -> Result<TimerDirs> {
        // One home rule with `hub_root_from` (`HOME`, else `USERPROFILE`), so the
        // timer's paths and the hub home it syncs never come from different
        // variables.
        let home = xerj_common::home::home_dir_from(env).ok_or_else(|| {
            anyhow!("neither HOME nor USERPROFILE is set; cannot place a user timer")
        })?;
        // XDG says a relative XDG_CONFIG_HOME is invalid and must be ignored.
        let config = env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() || p.has_root())
            .unwrap_or_else(|| home.join(".config"));
        Ok(TimerDirs {
            systemd_user: config.join("systemd").join("user"),
            launch_agents: home.join("Library").join("LaunchAgents"),
            hub_home: hub_root_from(env),
        })
    }
}

/// A path as one `ExecStart=` token: verbatim when it is plainly safe, else
/// double-quoted with systemd's escapes (`\\`, `\"`, `%%` for the specifier
/// character, `$$` for the variable one).
fn systemd_exec_token(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", path.display()))?;
    if s.chars().any(|c| c.is_control()) {
        bail!("{s:?} contains a control character; refusing to write it into a unit");
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+:@,=".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        return Ok(s.to_string());
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// `<name>.service`: one `xerj hub sync` run. `Type=oneshot` so systemd never
/// overlaps two runs, a timeout so a wedged run cannot block every later one.
pub fn render_systemd_service(exe: &Path) -> Result<String> {
    Ok(format!(
        "# Written by `xerj hub sync --install-timer`. Enable the schedule with:\n\
         #   systemctl --user daemon-reload && systemctl --user enable --now {SYSTEMD_UNIT_BASENAME}.timer\n\
         [Unit]\n\
         Description=Refresh the cached XERJ hub corpus packs\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} hub sync\n\
         TimeoutStartSec=2h\n\
         Nice=10\n",
        systemd_exec_token(exe)?
    ))
}

/// `<name>.timer`: every six hours on the wall clock. `Persistent=true` runs a
/// missed slot at the next boot or wake (a laptop that slept through 06:00 still
/// syncs), and the randomised delay keeps every client from asking the hub in
/// the same second.
pub fn render_systemd_timer() -> String {
    format!(
        "# Written by `xerj hub sync --install-timer`; runs {SYSTEMD_UNIT_BASENAME}.service.\n\
         [Unit]\n\
         Description=Run `xerj hub sync` every 6 hours\n\
         \n\
         [Timer]\n\
         OnCalendar=*-*-* 00/6:00:00\n\
         RandomizedDelaySec=15min\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The launchd agent: `xerj hub sync` every 6 hours (missed intervals during
/// sleep coalesce into one run at wake), output appended to `log`.
pub fn render_launchd_plist(exe: &Path, log: &Path) -> Result<String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", exe.display()))?;
    let log = log
        .to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", log.display()))?;
    let (exe, log) = (xml_escape(exe), xml_escape(log));
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Written by `xerj hub sync --install-timer`. Load it with: launchctl bootstrap gui/$(id -u) {LAUNCHD_LABEL}.plist -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LAUNCHD_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{exe}</string>\n\
         \t\t<string>hub</string>\n\
         \t\t<string>sync</string>\n\
         \t</array>\n\
         \t<key>StartInterval</key>\n\
         \t<integer>{LAUNCHD_INTERVAL_SECS}</integer>\n\
         \t<key>RunAtLoad</key>\n\
         \t<false/>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Background</string>\n\
         \t<key>LowPriorityIO</key>\n\
         \t<true/>\n\
         \t<key>StandardOutPath</key>\n\
         \t<string>{log}</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t<string>{log}</string>\n\
         </dict>\n\
         </plist>\n"
    ))
}

/// Write `contents` to `path` through a sibling file and a rename, so a reader
/// (systemd, launchd) never sees a half-written unit.
fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let tmp = path.with_file_name(name);
    fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))
}

/// Write the timer files for `kind` and return the path to the one the user
/// enables: the `.timer` (systemd; its `.service` is written beside it) or the
/// `.plist` (launchd). Enables, loads and starts nothing. Re-running overwrites,
/// so it is also how a moved binary's path is refreshed.
pub fn install_timer(kind: TimerKind, exe: &Path, dirs: &TimerDirs) -> Result<PathBuf> {
    match kind {
        TimerKind::SystemdUser => {
            let service = dirs
                .systemd_user
                .join(format!("{SYSTEMD_UNIT_BASENAME}.service"));
            let timer = dirs
                .systemd_user
                .join(format!("{SYSTEMD_UNIT_BASENAME}.timer"));
            // Render both before writing either: a bad exe path leaves nothing behind.
            let service_body = render_systemd_service(exe)?;
            let timer_body = render_systemd_timer();
            write_file_atomic(&service, &service_body)?;
            write_file_atomic(&timer, &timer_body)?;
            Ok(timer)
        }
        TimerKind::Launchd => {
            let plist = dirs.launch_agents.join(format!("{LAUNCHD_LABEL}.plist"));
            let log = dirs.hub_home.join("sync.log");
            let body = render_launchd_plist(exe, &log)?;
            // launchd opens the log path at job start and will not create its directory.
            fs::create_dir_all(&dirs.hub_home)
                .with_context(|| format!("create {}", dirs.hub_home.display()))?;
            write_file_atomic(&plist, &body)?;
            Ok(plist)
        }
    }
}

// ── the subcommand ───────────────────────────────────────────────────────────

/// `XERJ_HUB_URL`, else the public hub.
fn hub_url() -> String {
    std::env::var("XERJ_HUB_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_HUB_URL.to_string())
}

/// `xerj hub sync [--url <node>]` and `xerj hub sync --install-timer`.
pub fn run_sync(args: &[String]) -> i32 {
    run_sync_using(args, &|url| {
        Ok(Box::new(CliOps::current_exe(url)?) as Box<dyn SyncOps>)
    })
}

/// [`run_sync`] with the pull/ledger side injectable, so the whole command —
/// argument handling, the catalogue refresh and its signature check, the update,
/// the exit code, `last_sync` — can be driven end to end by a test that has no
/// `xerj` binary to spawn. `make_ops` receives `--url` and is called only after
/// the catalogue has been refreshed, so a usage error, `--install-timer` or a
/// failed refresh never builds one.
pub fn run_sync_using(
    args: &[String],
    make_ops: &dyn Fn(Option<String>) -> Result<Box<dyn SyncOps>>,
) -> i32 {
    let usage = sync_usage();
    let mut url: Option<String> = None;
    let mut install = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{usage}");
                return EXIT_OK;
            }
            "--install-timer" => install = true,
            "--url" => match args.get(i + 1) {
                Some(v) => {
                    url = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("xerj hub sync: --url needs a value\n\n{usage}");
                    return EXIT_USAGE;
                }
            },
            a if a.starts_with('-') => {
                eprintln!("xerj hub sync: unknown flag '{a}'\n\n{usage}");
                return EXIT_USAGE;
            }
            a => {
                eprintln!("xerj hub sync: unexpected argument '{a}'\n\n{usage}");
                return EXIT_USAGE;
            }
        }
        i += 1;
    }
    if install {
        if url.is_some() {
            eprintln!("xerj hub sync: --install-timer takes no other flag\n\n{usage}");
            return EXIT_USAGE;
        }
        return run_install_timer();
    }

    let hub_home = default_hub_root();
    let cat = match refresh_catalogue(&hub_url(), &hub_home) {
        Ok(c) => c,
        Err(_) if !xerj_common::hub::hub_key_configured() => {
            eprintln!(
                "xerj hub sync: {} Nothing was changed.",
                xerj_common::hub::NO_HUB_KEY_MESSAGE
            );
            return crate::ask::EXIT_NO_KEY;
        }
        Err(e) => {
            eprintln!("xerj hub sync: cannot refresh the catalogue; nothing was changed: {e:#}");
            return EXIT_INFRA;
        }
    };
    println!(
        "hub sync: catalogue refreshed (generated {}, {} corpora)",
        cat.generated_at,
        cat.corpora.len()
    );
    let ops = match make_ops(url) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("xerj hub sync: {e:#}");
            return EXIT_INFRA;
        }
    };
    let report = run_sync_with(
        &hub_home,
        &cat,
        ops.as_ref(),
        DISK_BUDGET_BYTES,
        &mut |level, line| match level {
            Level::Info => println!("{line}"),
            Level::Warn => eprintln!("{line}"),
        },
    );
    let code = report.exit_code();
    if code == EXIT_OK {
        if let Err(e) = write_last_sync(&hub_home) {
            eprintln!("xerj hub sync: synced, but could not write last_sync: {e:#}");
            return EXIT_INFRA;
        }
    }
    code
}

/// `--install-timer`: write the units for this OS, print the path, nothing else.
fn run_install_timer() -> i32 {
    let Some(kind) = TimerKind::for_this_os() else {
        eprintln!(
            "xerj hub sync --install-timer: only systemd (Linux) and launchd (macOS) are \
             supported; schedule `xerj hub sync` with your platform's scheduler instead"
        );
        return EXIT_INFRA;
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("xerj hub sync --install-timer: cannot locate the running binary: {e}");
            return EXIT_INFRA;
        }
    };
    let dirs = match TimerDirs::from_env(&|k| std::env::var_os(k)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("xerj hub sync --install-timer: {e:#}");
            return EXIT_INFRA;
        }
    };
    match install_timer(kind, &exe, &dirs) {
        Ok(path) => {
            println!("{}", path.display());
            EXIT_OK
        }
        Err(e) => {
            eprintln!("xerj hub sync --install-timer: {e:#}");
            EXIT_INFRA
        }
    }
}

#[cfg(test)]
mod tests;
