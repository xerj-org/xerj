//! `--watch`: keep an index current from filesystem events.
//!
//! The shape of this module is set by one rule: **there is exactly one indexing
//! path**. A burst of filesystem events does not index anything itself — it
//! decides which files are allowed to skip the hash, and then runs the ordinary
//! incremental pass ([`crate::run_index_report`]'s body) against the same
//! journal, the same plan projection and the same publish machinery a manual
//! re-run uses. Everything a re-run guarantees therefore still holds; what the
//! events buy is (a) not having to re-run, and (b) not re-reading the corpus.
//!
//! What the events are trusted for, precisely
//! -----------------------------------------
//! A plain `xerj autoindex` hashes every byte on every run, deliberately: size
//! and mtime cannot prove byte identity on every filesystem we support, so a
//! metadata-only shortcut could leave a stale document live forever after a
//! same-size rewrite with a restored timestamp (`content.rs`).
//!
//! `--watch` does not weaken that into "trust mtime". It keeps a digest cache
//! **in memory, in this process only**, built by hashing the corpus in full on
//! the first pass, and a file may skip its re-hash only when **both** hold:
//!
//! 1. no event in any burst since that hash named the file, the directory it
//!    lives in, or any ancestor directory; and
//! 2. its `(size, mtime, inode)` fingerprint is byte-for-byte what it was when
//!    the digest was taken.
//!
//! (1) is the kernel telling us the file was not written; (2) is the safety net
//! for a write the kernel never reported (a missed event, a watch not yet in
//! place, a network filesystem that does not report). The cache is never
//! persisted, so a restart — or a crash — always re-hashes in full, exactly like
//! a plain run.
//!
//! The one hole, stated plainly: a write that both produces no event and leaves
//! size, mtime and inode identical is not detected until something else touches
//! the file. `touch -r` and a same-size rewrite with a restored timestamp can do
//! that. A plain `xerj autoindex` re-run (which hashes everything) is the
//! ground truth and repairs it.
//!
//! Why watches are placed per directory
//! ------------------------------------
//! One recursive watch on the root would watch what the index does *not* hold:
//! `.git/`, `node_modules/`, an ignored `target/`. A single `cargo build` then
//! produces thousands of events for files no pass would ever index. The watch
//! set here comes from [`crate::walk::walk_dirs_opts`] — the same traversal,
//! the same hidden-name rule and the same `.gitignore`/`.xerjignore` stack the
//! indexing walk uses — so the rule in item 3 of the feature request ("respect
//! the ignore files exactly as a normal run does") holds by construction rather
//! than by a second implementation that can drift. It also keeps the descriptor
//! count at "one per indexed directory" instead of "one per directory on disk".

use crate::cli::IndexCfg;
use crate::ignore_rules::{is_hidden_name, GITIGNORE, XERJIGNORE};
use crate::progress::{self, Progress};
use crate::walk::FileEntry;
use anyhow::{Context, Result};
use notify::{ErrorKind as NotifyErrorKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Default quiet period. An editor's "save" is several syscalls (truncate,
/// write, rename, chmod) and a formatter-on-save is several more, so a pass
/// started on the first of them would index a half-written file and then index
/// it again. A few hundred milliseconds is below human notice and above every
/// save dance measured here.
pub const DEFAULT_DEBOUNCE_MS: u64 = 400;

/// Floor for how long a burst may keep growing before a pass runs anyway.
/// Without a cap, a tree that never goes quiet — a build writing into a watched
/// directory, a log being appended to — would hold the pass off forever, which
/// is the one way a debounce can turn into "never index again".
const MAX_HOLD_FLOOR: Duration = Duration::from_secs(5);

/// The cap for a given quiet period. A `--debounce` longer than the floor is an
/// explicit instruction to wait, so the cap follows it instead of silently
/// turning `--debounce 30000` into a five-second one.
fn max_hold(debounce: Duration) -> Duration {
    MAX_HOLD_FLOOR.max(debounce.saturating_mul(2))
}

/// Above this many distinct paths a burst stops tracking individuals and
/// becomes a full re-hash. Bounds memory when something rewrites a whole tree,
/// and is cheaper than it looks: at that size the pass is re-reading most of
/// the corpus anyway.
const MAX_TRACKED_PATHS: usize = 20_000;

/// `(size, mtime, inode)`. Never used to decide "unchanged" on its own — see
/// the module docs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fingerprint {
    size: u64,
    mtime_ns: i128,
    ino: u64,
}

fn fingerprint_of(path: &Path) -> Option<Fingerprint> {
    let md = std::fs::metadata(path).ok()?;
    let mtime_ns = md
        .modified()
        .ok()
        .map(|t| match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            // Pre-1970 mtimes exist on restored archives; negative is the
            // honest answer, and equality is all this value is ever used for.
            Err(e) => -(e.duration().as_nanos() as i128),
        })?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        md.ino()
    };
    // Windows has a file index, but reading it needs a handle open with
    // FILE_FLAG_BACKUP_SEMANTICS; size+mtime plus the event set is what the
    // fingerprint is there, and a rename dance changes the mtime.
    #[cfg(not(unix))]
    let ino = 0u64;
    Some(Fingerprint {
        size: md.len(),
        mtime_ns,
        ino,
    })
}

/// Digests this process hashed itself, keyed by the path they were read from.
#[derive(Default)]
pub(crate) struct Carry {
    by_path: HashMap<PathBuf, (Fingerprint, String)>,
}

impl Carry {
    pub(crate) fn len(&self) -> usize {
        self.by_path.len()
    }
}

/// One debounced burst of filesystem events, reduced to "what may not be
/// carried".
#[derive(Debug, Default)]
pub(crate) struct ChangeSet {
    /// Paths named by an event.
    files: HashSet<PathBuf>,
    /// Directory prefixes whose whole subtree is suspect: a directory event, or
    /// a vanished path that may have been a directory, or an ignore-file edit.
    dirs: Vec<PathBuf>,
    /// Paths whose ATTRIBUTES changed and whose contents may not have (a chmod,
    /// or the atime update the kernel makes when a pass reads the file). Worth a
    /// pass — a file that just became unreadable stops being indexable — but not
    /// worth invalidating a digest by itself: a content write always moves mtime
    /// as well, and `PassPlan` checks the fingerprint regardless.
    meta: HashSet<PathBuf>,
    /// Distrust the entire cache. Set by a platform rescan notice (inotify
    /// queue overflow, FSEvents' own rescan flag), by the first pass, and by a
    /// burst too large to track.
    full: bool,
    /// Raw events folded in, for the operator-facing count.
    pub(crate) events: u64,
    /// Paths dropped because no pass could ever index them (hidden names).
    pub(crate) filtered: u64,
}

impl ChangeSet {
    pub(crate) fn full() -> Self {
        ChangeSet {
            full: true,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn is_full(&self) -> bool {
        self.full
    }

    /// A burst naming exactly these paths. The session builds one from events;
    /// the convergence tests build one from the changes they just made, which is
    /// what a working watcher would have reported.
    #[cfg(test)]
    pub(crate) fn from_paths<P: AsRef<Path>>(paths: &[P]) -> Self {
        let mut change = ChangeSet::default();
        for path in paths {
            change.mark(path.as_ref());
            change.events += 1;
        }
        change
    }

    /// Something worth a pass happened. An empty, non-full change set is how
    /// the loop knows a burst was entirely noise.
    pub(crate) fn is_empty(&self) -> bool {
        !self.full && self.files.is_empty() && self.dirs.is_empty() && self.meta.is_empty()
    }

    pub(crate) fn paths(&self) -> usize {
        self.files.len() + self.dirs.len() + self.meta.len()
    }

    /// Fold in an attribute-only event.
    fn mark_metadata(&mut self, path: &Path) {
        if self.full || self.meta.len() >= MAX_TRACKED_PATHS {
            return;
        }
        self.meta.insert(path.to_path_buf());
    }

    /// Fold one path in, deciding by what is on disk NOW.
    fn mark(&mut self, path: &Path) {
        if self.full {
            return;
        }
        if self.files.len() + self.dirs.len() >= MAX_TRACKED_PATHS {
            self.files.clear();
            self.dirs.clear();
            self.meta.clear();
            self.full = true;
            return;
        }
        // An ignore file governs what the whole subtree below it contains, so
        // editing one invalidates that subtree, not one file.
        let ignore_file = path
            .file_name()
            .is_some_and(|name| name == GITIGNORE || name == XERJIGNORE);
        match std::fs::symlink_metadata(path) {
            Ok(md) if md.is_dir() => self.dirs.push(path.to_path_buf()),
            Ok(_) if ignore_file => {
                if let Some(parent) = path.parent() {
                    self.dirs.push(parent.to_path_buf());
                }
            }
            Ok(_) => {
                self.files.insert(path.to_path_buf());
            }
            // Gone. It may have been a file (delete: nothing to carry, the walk
            // will not find it) or a directory (its children are suspect, and a
            // directory can be replaced by a file or moved back). Both, then:
            // the cost of being wrong is a re-hash, the cost of guessing wrong
            // the other way is a stale document.
            Err(_) => {
                self.files.insert(path.to_path_buf());
                self.dirs.push(path.to_path_buf());
                if ignore_file {
                    if let Some(parent) = path.parent() {
                        self.dirs.push(parent.to_path_buf());
                    }
                }
            }
        }
    }

    /// Does this burst forbid carrying `path`'s digest?
    fn touches(&self, path: &Path) -> bool {
        if self.full {
            return true;
        }
        if self.files.contains(path) {
            return true;
        }
        self.dirs.iter().any(|dir| path.starts_with(dir))
    }
}

/// Is this path one a pass could ever index, cheaply decided?
///
/// Only the walker's absolute rule is applied: a hidden name below the root is
/// never indexed and a hidden directory is never descended, whatever the ignore
/// flags say (`walk.rs`). That makes dropping it safe — it cannot hide a change
/// the index should have seen. `.gitignore`/`.xerjignore` are hidden names and
/// are deliberately NOT dropped: editing one changes what the tree contains.
///
/// Everything else is kept and decided by the pass's own walk, which is the
/// only thing that applies the full ignore stack. Ignored directories cost
/// nothing here because they are never watched in the first place.
fn is_interesting(root: &Path, path: &Path) -> bool {
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        // Outside the root: a followed symlink's target, or a stale event.
        // Keep it; the pass decides.
        Err(_) => return true,
    };
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        if !is_hidden_name(component.as_os_str()) {
            continue;
        }
        let last = components.peek().is_none();
        let ignore_file =
            last && (component.as_os_str() == GITIGNORE || component.as_os_str() == XERJIGNORE);
        if !ignore_file {
            return false;
        }
    }
    true
}

/// What one event kind means for the digest cache.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Signal {
    /// The bytes may have changed: re-read the file.
    Changed,
    /// Only attributes moved. Worth a pass, not worth a re-read on its own.
    Attributes,
    /// Not a change at all — and dropping these is not an optimisation, it is
    /// what stops the watcher feeding itself. See [`classify`].
    Ignored,
}

/// Classify an event kind.
///
/// This exists because of a measured bug, and the bug is worth recording: the
/// Linux backend asks the kernel for `IN_OPEN` and `IN_CLOSE` as well as writes
/// (`notify-8.2.0/src/inotify.rs:418` — the watch mask includes
/// `WatchMask::OPEN`), so **every file a pass reads reports an event**. A pass
/// hashes the corpus and copies it into the generation snapshot, so treating
/// `Access` events as changes made every pass trigger the next one: on a
/// 3-file tree the session ran 48 passes over a tree nobody touched, with the
/// digest cache invalidated every time (`carried=0` on every line).
///
/// So reads are dropped, and only what can actually change bytes or
/// indexability is kept:
///
/// * `Create`, `Remove`, `Modify(Data | Name | Any | Other)` → changed.
/// * `Access(Close(Write))` → changed. A finished write, not a read.
/// * `Modify(Metadata)` → attributes. A chmod, or the atime update the kernel
///   makes for the pass's own reads; a content write always moves mtime too, so
///   nothing is lost by not invalidating a digest here.
/// * `Access(Open | Read | Close(Read) | Any | Other)` → ignored.
/// * `Any` / `Other` → changed. Unknown means conservative; the rescan flag is
///   handled before this is ever called.
fn classify(kind: &notify::EventKind) -> Signal {
    use notify::event::{AccessKind, AccessMode, EventKind, ModifyKind};
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => Signal::Changed,
        EventKind::Modify(ModifyKind::Metadata(_)) => Signal::Attributes,
        EventKind::Modify(_) => Signal::Changed,
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => Signal::Changed,
        EventKind::Access(_) => Signal::Ignored,
        EventKind::Any | EventKind::Other => Signal::Changed,
    }
}

/// Fold one watcher message into the burst.
///
/// `notify` errors are not fatal here: a watch on a directory that was deleted
/// mid-burst reports an error whose only correct handling is "run the pass and
/// re-sync the watch set". A rescan notice is the loud case — it means events
/// were dropped, so nothing in the cache can be trusted.
fn fold(
    change: &mut ChangeSet,
    message: notify::Result<notify::Event>,
    root: &Path,
    pr: &Progress,
) {
    change.events += 1;
    match message {
        Ok(event) => {
            if event.need_rescan() {
                pr.warn(
                    "watch: the platform reported dropped events (watch queue overflow); \
                     re-hashing the whole tree for this pass",
                );
                change.full = true;
                return;
            }
            match classify(&event.kind) {
                Signal::Ignored => change.filtered += event.paths.len() as u64,
                signal => {
                    for path in &event.paths {
                        if !is_interesting(root, path) {
                            change.filtered += 1;
                            continue;
                        }
                        match signal {
                            Signal::Changed => change.mark(path),
                            Signal::Attributes => change.mark_metadata(path),
                            Signal::Ignored => unreachable!("handled above"),
                        }
                    }
                }
            }
        }
        Err(error) if matches!(error.kind, NotifyErrorKind::MaxFilesWatch) => {
            pr.warn(&format!("watch: {}", watch_limit_message(None, &error)));
            change.full = true;
        }
        Err(error) => {
            // A path-carrying error still tells us where to look.
            for path in &error.paths {
                change.mark(path);
            }
            pr.note(&format!("watch: watcher reported {error}"));
        }
    }
}

/// The actionable form of "the OS will not give me any more watches".
///
/// A half-watched tree is the worst outcome available: the index looks live and
/// silently is not. So this is a hard failure with the numbers and the exact
/// command, not a warning — and it names the alternatives, because raising a
/// sysctl is not always the reader's to do.
fn watch_limit_message(dirs: Option<usize>, error: &notify::Error) -> String {
    let mut out = String::from("the operating system refused another filesystem watch");
    if let Some(dirs) = dirs {
        out.push_str(&format!(
            " ({dirs} directories need one watch each in this tree)"
        ));
    }
    out.push_str(&format!(": {error}. "));
    #[cfg(target_os = "linux")]
    {
        let read = |name: &str| {
            std::fs::read_to_string(format!("/proc/sys/fs/inotify/{name}"))
                .ok()
                .map(|value| value.trim().to_string())
        };
        match (read("max_user_watches"), read("max_user_instances")) {
            (Some(watches), Some(instances)) => out.push_str(&format!(
                "This user's limits are fs.inotify.max_user_watches={watches} and \
                 fs.inotify.max_user_instances={instances}, shared with every other watcher \
                 running as this user (editors and language servers hold thousands). "
            )),
            _ => out.push_str(
                "The limit is fs.inotify.max_user_watches, shared with every other watcher \
                 running as this user. ",
            ),
        }
        out.push_str(
            "Raise it with `sudo sysctl fs.inotify.max_user_watches=524288` (add it to \
             /etc/sysctl.d/ to survive a reboot). ",
        );
    }
    out.push_str(
        "Without a watch per indexed directory this run would report a live index while \
         silently missing changes, so it stops instead. Alternatives that need no root: point \
         --watch at a subdirectory, exclude directories you do not search with .xerjignore, or \
         keep re-running `xerj autoindex` (which needs no watches at all) on a timer.",
    );
    out
}

/// What one pass may carry, sized before the hash phase starts.
///
/// Built by one `stat` per discovered file — 10k stats is tens of milliseconds
/// against tens of seconds of hashing — so the hash phase's totals can be the
/// files this pass will actually read. Sizing them to the whole corpus instead
/// would report a percentage of work the pass is not doing.
pub(crate) struct PassPlan {
    /// The canonical root. Held because [`carryable`] has to be asked on the
    /// way OUT of the cache as well as on the way in: with `--follow-symlinks` a
    /// link and its target are two entries with one `path`, so a lookup keyed on
    /// the path alone would hand the target's digest to the link (caught by
    /// `a_followed_symlink_is_never_carried`).
    root: PathBuf,
    reuse: HashMap<PathBuf, String>,
    pre: HashMap<PathBuf, Fingerprint>,
    next: Mutex<HashMap<PathBuf, (Fingerprint, String)>>,
    pub(crate) hash_files: u64,
    pub(crate) hash_bytes: u64,
    pub(crate) carried_files: u64,
    pub(crate) carried_bytes: u64,
}

/// Can a digest for this entry be cached at all?
///
/// Only entries the walk reached directly. Under `--follow-symlinks` a file's
/// `path` is the RESOLVED target while the watch set holds the path the walk
/// arrived by, so an event on the link cannot be matched against the entry —
/// those are always re-hashed rather than silently carried.
fn carryable(root: &Path, entry: &FileEntry) -> bool {
    !entry.is_symlink && root.join(&entry.rel) == entry.path
}

impl PassPlan {
    fn build(root: &Path, files: &[FileEntry], carry: &Carry, change: &ChangeSet) -> Self {
        use rayon::prelude::*;
        // One stat per file, on the run's own scan pool (never rayon's global
        // pool — `--workers` bounds phase A, and this is phase A).
        let stats: Vec<Option<Fingerprint>> = crate::pool::install(|| {
            files
                .par_iter()
                .map(|entry| fingerprint_of(&entry.path))
                .collect()
        });
        let mut plan = PassPlan {
            root: root.to_path_buf(),
            reuse: HashMap::new(),
            pre: HashMap::new(),
            next: Mutex::new(HashMap::new()),
            hash_files: 0,
            hash_bytes: 0,
            carried_files: 0,
            carried_bytes: 0,
        };
        for (entry, stat) in files.iter().zip(stats) {
            if let Some(fingerprint) = stat {
                plan.pre.insert(entry.path.clone(), fingerprint);
            }
            let carried = if change.full || !carryable(root, entry) {
                None
            } else {
                match (carry.by_path.get(&entry.path), stat) {
                    (Some((recorded, digest)), Some(now))
                        if *recorded == now && !change.touches(&entry.path) =>
                    {
                        Some(digest.clone())
                    }
                    _ => None,
                }
            };
            match carried {
                Some(digest) => {
                    plan.carried_files += 1;
                    plan.carried_bytes += entry.size;
                    plan.reuse.insert(entry.path.clone(), digest);
                }
                None => {
                    plan.hash_files += 1;
                    plan.hash_bytes += entry.size;
                }
            }
        }
        plan
    }

    /// The digest this pass is allowed to skip reading, if any.
    pub(crate) fn carried(&self, entry: &FileEntry) -> Option<String> {
        if !carryable(&self.root, entry) {
            return None;
        }
        self.reuse.get(&entry.path).cloned()
    }

    /// Record what the pass ended up with, for the next pass to carry.
    ///
    /// A freshly hashed file is re-fingerprinted and only cached when the
    /// fingerprint it had BEFORE the read is the fingerprint it has after:
    /// otherwise the bytes moved under the hash, and pairing the new mtime with
    /// the old digest would make the change invisible for good.
    pub(crate) fn observe(&self, entry: &FileEntry, digest: &str, fresh: bool) {
        if !carryable(&self.root, entry) {
            return;
        }
        let Some(before) = self.pre.get(&entry.path).copied() else {
            return;
        };
        if fresh && fingerprint_of(&entry.path) != Some(before) {
            return;
        }
        self.next
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(entry.path.clone(), (before, digest.to_owned()));
    }

    fn into_carry(self) -> Carry {
        Carry {
            by_path: self
                .next
                .into_inner()
                .unwrap_or_else(|poison| poison.into_inner()),
        }
    }
}

/// The handle the indexing pass gets. Holds the cache the pass may read and
/// collects the cache the next pass will use.
pub(crate) struct Pass<'a> {
    root: PathBuf,
    carry: &'a Carry,
    change: &'a ChangeSet,
    next: std::cell::RefCell<Option<Carry>>,
    stats: std::cell::Cell<(u64, u64, u64, u64)>,
}

impl<'a> Pass<'a> {
    pub(crate) fn new(root: &Path, carry: &'a Carry, change: &'a ChangeSet) -> Self {
        Pass {
            root: root.to_path_buf(),
            carry,
            change,
            next: std::cell::RefCell::new(None),
            stats: std::cell::Cell::new((0, 0, 0, 0)),
        }
    }

    pub(crate) fn plan(&self, files: &[FileEntry]) -> PassPlan {
        PassPlan::build(&self.root, files, self.carry, self.change)
    }

    /// Hand back the cache the pass built. Called once, after the inventory is
    /// resolved and before anything is published: the cache holds facts about
    /// files, not about the index, so it is valid even if the publish later
    /// fails — the next pass still sees the same digests and retries.
    pub(crate) fn commit(&self, plan: PassPlan) {
        self.stats.set((
            plan.carried_files,
            plan.carried_bytes,
            plan.hash_files,
            plan.hash_bytes,
        ));
        *self.next.borrow_mut() = Some(plan.into_carry());
    }

    /// `(carried files, carried bytes, hashed files, hashed bytes)`.
    pub(crate) fn stats(&self) -> PassWork {
        self.stats.get()
    }

    fn take_carry(self) -> Option<Carry> {
        self.next.into_inner()
    }
}

/// `(carried files, carried bytes, hashed files, hashed bytes)` for one pass.
pub(crate) type PassWork = (u64, u64, u64, u64);

/// What one pass returns: the run's own result, and what it cost.
pub(crate) type PassResult = (Result<(i32, Option<serde_json::Value>)>, PassWork);

/// Run exactly one pass and adopt the digest cache it produced.
///
/// The only place a `--watch` pass is started, so the session loop and the
/// convergence tests drive identical code.
///
/// The cache is adopted even when the pass FAILED, as long as the pass got far
/// enough to produce one: it records digests of files on disk, not facts about
/// the index, so a failed publish does not make it wrong — the next pass
/// projects the same plan and retries. A pass that produced no cache at all
/// resets to empty, so the next pass hashes in full rather than trusting a
/// cache from two passes ago whose events have already been consumed.
pub(crate) fn one_pass(
    cfg: &IndexCfg,
    root: &Path,
    carry: &mut Carry,
    change: &ChangeSet,
) -> PassResult {
    let pass = Pass::new(root, carry, change);
    let outcome = crate::run_index_report_watched(cfg.clone(), &pass);
    let stats = pass.stats();
    *carry = pass.take_carry().unwrap_or_default();
    (outcome, stats)
}

/// Wait for the next burst.
///
/// Blocks — with no timeout and no polling — until the first event, which is
/// what "near-free at idle" means concretely: one thread parked on a channel
/// and one kernel watch per indexed directory, no wakeups at all while nothing
/// changes. Then drains until the tree has been quiet for `debounce`, or
/// the hold cap ([`max_hold`]) has passed since the first event.
fn next_burst(
    rx: &Receiver<notify::Result<notify::Event>>,
    root: &Path,
    debounce: Duration,
    pr: &Progress,
) -> Option<ChangeSet> {
    let mut change = ChangeSet::default();
    fold(&mut change, rx.recv().ok()?, root, pr);
    let opened = Instant::now();
    let cap = max_hold(debounce);
    loop {
        let hold_left = cap.saturating_sub(opened.elapsed());
        if hold_left.is_zero() {
            return Some(change);
        }
        match rx.recv_timeout(debounce.min(hold_left)) {
            Ok(message) => fold(&mut change, message, root, pr),
            // Either the tree went quiet for a full debounce window (the save
            // dance is over) or the hold cap ran out; both mean "index now".
            Err(RecvTimeoutError::Timeout) => return Some(change),
            Err(RecvTimeoutError::Disconnected) => return Some(change),
        }
    }
}

/// Place one watch per admitted directory, and report what could not be watched.
fn sync_watches(
    watcher: &mut notify::RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    cfg: &IndexCfg,
) -> Result<(usize, usize)> {
    let wanted = crate::walk::walk_dirs_opts(
        &cfg.root,
        cfg.follow_symlinks,
        cfg.follow_symlinks_outside_root,
        cfg.ignore,
    )
    .context("list the directories to watch")?;
    let wanted: HashSet<PathBuf> = wanted.into_iter().collect();
    let mut added = 0usize;
    for dir in &wanted {
        if watched.contains(dir) {
            continue;
        }
        // Non-recursive on purpose: the set is derived from the walk, so a
        // recursive watch here would re-add every ignored subtree the walk just
        // excluded.
        match watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                watched.insert(dir.clone());
                added += 1;
            }
            Err(error) if matches!(error.kind, NotifyErrorKind::MaxFilesWatch) => {
                anyhow::bail!(watch_limit_message(Some(wanted.len()), &error));
            }
            // A directory that vanished between the walk and the watch is not a
            // failure; the next pass will not find it either.
            Err(error) if matches!(error.kind, NotifyErrorKind::PathNotFound) => {}
            Err(error) => {
                return Err(
                    anyhow::Error::new(error).context(format!("watch directory {}", dir.display()))
                );
            }
        }
    }
    let gone: Vec<PathBuf> = watched.difference(&wanted).cloned().collect();
    for dir in gone {
        // Unwatching a deleted directory fails on every backend; the kernel has
        // already dropped the watch. Only the bookkeeping matters.
        let _ = watcher.unwatch(&dir);
        watched.remove(&dir);
    }
    Ok((watched.len(), added))
}

/// `xerj autoindex <folder> --watch`.
///
/// Returns the exit code of the LAST pass, so a `--watch` run that is stopped
/// after a clean pass exits like the equivalent manual run. A pass that fails
/// does not end the session: a watcher that dies because the server restarted
/// is worse than useless, it is a silently stale index.
pub fn run(cfg: IndexCfg) -> Result<i32> {
    let surface = progress::detect(cfg.progress);
    let pr = Progress::new(
        surface,
        cfg.progress_interval
            .unwrap_or_else(|| progress::default_interval(surface)),
    );
    let root = cfg
        .root
        .canonicalize()
        .with_context(|| format!("resolve watched folder {}", cfg.root.display()))?;
    anyhow::ensure!(
        root.is_dir(),
        "--watch needs a folder; {} is not one",
        root.display()
    );

    // A journal inside the indexed tree is a feedback loop: every pass writes to
    // it, those writes are events, and the events start the next pass. The
    // default state directory is under ~/.xerj, but `--state-dir ./state` inside
    // the folder is an easy thing to type, and the failure it causes (a session
    // that reindexes forever and never settles) does not look like its cause.
    let state_dir = cfg.state_dir.clone().unwrap_or_else(|| {
        crate::state::default_state_dir(&root.to_string_lossy(), &cfg.url, &cfg.prefix)
    });
    let resolved_state = state_dir.canonicalize().unwrap_or(state_dir.clone());
    anyhow::ensure!(
        !resolved_state.starts_with(&root),
        "--watch refuses a --state-dir inside the folder it watches ({} is under {}). Every pass \
         writes the resume journal there, those writes are filesystem events, and the \
         events would start the next pass — the session would reindex forever. Put the \
         state directory outside the tree, or leave --state-dir off and let it default \
         under ~/.xerj/autoindex/.",
        resolved_state.display(),
        root.display()
    );

    let (tx, rx): (Sender<notify::Result<notify::Event>>, _) = channel();
    let mut watcher = notify::recommended_watcher(tx).map_err(|error| {
        anyhow::anyhow!(
            "this platform's filesystem watcher could not be started: {error}. \
             Re-run without --watch (a plain `xerj autoindex` needs no watcher), or run it on a \
             timer. Watching is not supported everywhere — some container bind mounts, NFS and \
             other network shares report no events at all."
        )
    })?;
    let mut watched: HashSet<PathBuf> = HashSet::new();
    // Watches go on BEFORE the first pass, or every change made while that pass
    // runs — minutes, on a large tree — would be invisible until the next one.
    let (watching, _) = sync_watches(&mut watcher, &mut watched, &cfg)?;
    pr.note(&format!(
        "watch: {watching} directories watched under {} (debounce {} ms); one watch per indexed \
         directory, no polling thread",
        root.display(),
        cfg.debounce.as_millis()
    ));

    let mut carry = Carry::default();
    // The first pass is a full one: the cache is empty, so this hashes the
    // corpus exactly like a plain run and is what every later pass carries from.
    let mut change = ChangeSet::full();
    let mut pass_number = 0u64;
    let mut last_code = 0i32;
    loop {
        let started = Instant::now();
        let (outcome, (carried_files, carried_bytes, hashed_files, hashed_bytes)) =
            one_pass(&cfg, &root, &mut carry, &change);
        match outcome {
            Ok((code, _)) => {
                last_code = code;
                pr.note(&format!(
                    "watch: pass {pass_number} finished in {:.1}s exit={code} events={} paths={} \
                     hashed={hashed_files}/{}MB carried={carried_files}/{}MB cache={} files",
                    started.elapsed().as_secs_f64(),
                    change.events,
                    change.paths(),
                    hashed_bytes >> 20,
                    carried_bytes >> 20,
                    carry.len(),
                ));
            }
            Err(error) => {
                // Keep watching. A watcher that exits because the endpoint
                // blipped leaves a stale index behind and says nothing; the
                // journal's resume contract means the next pass retries the
                // same work rather than losing it.
                pr.warn(&format!(
                    "watch: pass {pass_number} FAILED after {:.1}s: {error:#}. Still watching — \
                     the next change retries it. Stop the run (Ctrl-C) if this repeats.",
                    started.elapsed().as_secs_f64()
                ));
            }
        }
        // New directories only become visible once something has walked the
        // tree, so the watch set is re-synced after every pass.
        let (total, added) = sync_watches(&mut watcher, &mut watched, &cfg)?;
        if added > 0 {
            pr.note(&format!(
                "watch: {added} new directories watched ({total} total)"
            ));
        }
        pass_number += 1;
        match next_burst_or_idle(&rx, &root, cfg.debounce, &pr) {
            Some(next) => change = next,
            None => {
                pr.note("watch: the filesystem watcher stopped; exiting");
                return Ok(last_code);
            }
        }
    }
}

/// Keep waiting until a burst names something indexable, so a noisy editor
/// cannot make the loop spin on passes that have nothing to do.
fn next_burst_or_idle(
    rx: &Receiver<notify::Result<notify::Event>>,
    root: &Path,
    debounce: Duration,
    pr: &Progress,
) -> Option<ChangeSet> {
    loop {
        let burst = next_burst(rx, root, debounce, pr)?;
        if !burst.is_empty() {
            return Some(burst);
        }
        // `filtered` counts every path the burst discarded: the pass's own
        // reads, hidden names, and paths no run would index. Saying "hidden"
        // alone would misdescribe the common case, which is reads.
        pr.note(&format!(
            "watch: {} event(s) ignored ({} path(s): reads, hidden names or ignored paths); \
             no pass needed",
            burst.events, burst.filtered
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, EventKind, ModifyKind, RenameMode};
    use std::fs;

    fn silent() -> std::sync::Arc<Progress> {
        Progress::silent()
    }

    fn event(kind: EventKind, paths: &[&Path]) -> notify::Result<notify::Event> {
        let mut event = notify::Event::new(kind);
        for path in paths {
            event = event.add_path(path.to_path_buf());
        }
        Ok(event)
    }

    #[test]
    fn a_hidden_path_is_not_worth_a_pass_but_an_ignore_file_is() {
        let root = Path::new("/corpus");
        assert!(is_interesting(root, &root.join("notes/a.md")));
        // Editor scratch files live next to the file being edited and are the
        // single loudest event source there is.
        assert!(!is_interesting(root, &root.join("notes/.a.md.swp")));
        assert!(!is_interesting(root, &root.join(".git/index.lock")));
        assert!(!is_interesting(root, &root.join(".git/objects/ab/cdef")));
        // The rules themselves: editing one changes what the tree contains.
        assert!(is_interesting(root, &root.join(".gitignore")));
        assert!(is_interesting(root, &root.join("sub/.xerjignore")));
        // A dot-named ROOT is exempt, exactly as the walker exempts depth 0.
        let dotroot = Path::new("/home/me/.notes");
        assert!(is_interesting(dotroot, &dotroot.join("a.md")));
    }

    /// The bug this test exists for: the Linux backend reports `IN_OPEN`, so a
    /// pass's own reads arrive as events. Treating them as changes made the
    /// watcher feed itself — 48 passes over an untouched 3-file tree.
    #[test]
    fn the_passs_own_reads_are_not_changes() {
        use notify::event::{AccessKind, AccessMode, DataChange, MetadataKind};
        assert_eq!(
            classify(&EventKind::Access(AccessKind::Open(AccessMode::Any))),
            Signal::Ignored
        );
        assert_eq!(
            classify(&EventKind::Access(AccessKind::Read)),
            Signal::Ignored
        );
        assert_eq!(
            classify(&EventKind::Access(AccessKind::Close(AccessMode::Read))),
            Signal::Ignored
        );
        // A finished WRITE is not a read.
        assert_eq!(
            classify(&EventKind::Access(AccessKind::Close(AccessMode::Write))),
            Signal::Changed
        );
        assert_eq!(
            classify(&EventKind::Modify(ModifyKind::Data(DataChange::Content))),
            Signal::Changed
        );
        assert_eq!(
            classify(&EventKind::Modify(ModifyKind::Name(RenameMode::Both))),
            Signal::Changed
        );
        assert_eq!(
            classify(&EventKind::Create(CreateKind::File)),
            Signal::Changed
        );
        assert_eq!(
            classify(&EventKind::Remove(notify::event::RemoveKind::File)),
            Signal::Changed
        );
        // A chmod, or the atime update the kernel makes for our own reads.
        assert_eq!(
            classify(&EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any))),
            Signal::Attributes
        );
        // Unknown means conservative.
        assert_eq!(classify(&EventKind::Any), Signal::Changed);
    }

    #[test]
    fn a_burst_of_only_reads_asks_for_no_work_at_all() {
        let pr = silent();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let target = root.join("a.md");
        fs::write(
            &target, b"body
",
        )
        .unwrap();
        let (tx, rx) = channel();
        // Exactly what hashing one file looks like from the outside.
        for kind in [
            EventKind::Access(notify::event::AccessKind::Open(
                notify::event::AccessMode::Any,
            )),
            EventKind::Access(notify::event::AccessKind::Close(
                notify::event::AccessMode::Read,
            )),
        ] {
            tx.send(event(kind, &[&target])).unwrap();
        }
        let burst = next_burst(&rx, &root, Duration::from_millis(30), &pr).unwrap();
        assert!(
            burst.is_empty(),
            "a pass's own reads must not ask for another pass: {burst:?}"
        );
        assert_eq!(burst.filtered, 2);
    }

    #[test]
    fn an_attribute_only_event_costs_a_pass_but_not_a_rehash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(
            root.join("a.md"),
            b"body
",
        )
        .unwrap();
        let files = crate::walk::walk(&root, false).unwrap();
        let plan = PassPlan::build(&root, &files, &Carry::default(), &ChangeSet::full());
        plan.observe(&files[0], "digest-a", true);
        let carry = plan.into_carry();

        let mut change = ChangeSet::default();
        change.mark_metadata(&root.join("a.md"));
        assert!(!change.is_empty(), "a chmod can change what is indexable");
        let plan = PassPlan::build(&root, &files, &carry, &change);
        assert_eq!(
            plan.carried(&files[0]).as_deref(),
            Some("digest-a"),
            "an attribute change with an unchanged fingerprint must not force a re-read"
        );
    }

    #[test]
    fn a_rescan_notice_forces_a_full_rehash() {
        let pr = silent();
        let mut change = ChangeSet::default();
        let rescan = notify::Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        fold(&mut change, Ok(rescan), Path::new("/corpus"), &pr);
        assert!(change.is_full(), "dropped events must invalidate the cache");
        assert!(change.touches(Path::new("/corpus/anything")));
    }

    #[test]
    fn a_vanished_path_invalidates_it_as_both_file_and_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let gone = root.join("pkg");
        let mut change = ChangeSet::default();
        change.mark(&gone);
        // It may have been a directory: everything that used to be under it is
        // suspect, because a directory can be replaced or moved back.
        assert!(change.touches(&gone));
        assert!(change.touches(&gone.join("deep/file.md")));
    }

    #[test]
    fn a_directory_event_invalidates_its_subtree_and_a_file_event_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("moved")).unwrap();
        fs::write(root.join("moved/inner.md"), b"x").unwrap();
        fs::write(root.join("top.md"), b"y").unwrap();
        let mut change = ChangeSet::default();
        change.mark(&root.join("moved"));
        change.mark(&root.join("top.md"));
        assert!(change.touches(&root.join("moved/inner.md")));
        assert!(change.touches(&root.join("top.md")));
        assert!(!change.touches(&root.join("other.md")));
    }

    #[test]
    fn an_ignore_file_edit_invalidates_the_directory_it_governs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        let rules = root.join("sub/.gitignore");
        fs::write(&rules, b"*.log\n").unwrap();
        let mut change = ChangeSet::default();
        change.mark(&rules);
        assert!(
            change.touches(&root.join("sub/keep.md")),
            "new ignore rules change which files belong in the index"
        );
    }

    #[test]
    fn a_burst_larger_than_the_tracking_cap_becomes_a_full_rehash() {
        let mut change = ChangeSet::default();
        for i in 0..(MAX_TRACKED_PATHS + 1) {
            change.mark(Path::new(&format!("/corpus/f{i}")));
        }
        assert!(change.is_full());
        assert!(change.files.is_empty(), "the cap must bound memory");
    }

    #[test]
    fn the_debouncer_coalesces_a_save_dance_into_one_burst() {
        let pr = silent();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let target = root.join("a.md");
        fs::write(&target, b"body\n").unwrap();
        let (tx, rx) = channel();
        // The rename dance an editor performs on save: temp file created,
        // written, renamed over the target, then the target's mode touched.
        let temp = root.join("a.md.tmp");
        fs::write(&temp, b"body\n").unwrap();
        tx.send(event(EventKind::Create(CreateKind::File), &[&temp]))
            .unwrap();
        tx.send(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[&temp, &target],
        ))
        .unwrap();
        tx.send(event(
            EventKind::Modify(ModifyKind::Metadata(
                notify::event::MetadataKind::Permissions,
            )),
            &[&target],
        ))
        .unwrap();
        let burst = next_burst(&rx, &root, Duration::from_millis(30), &pr).unwrap();
        assert_eq!(burst.events, 3, "one burst, not three passes");
        assert!(burst.touches(&target));
        assert!(burst.touches(&temp));
    }

    #[test]
    fn the_debouncer_stops_holding_a_tree_that_never_goes_quiet() {
        let pr = silent();
        let root = PathBuf::from("/corpus");
        let (tx, rx) = channel();
        let noisy = std::thread::spawn(move || {
            let deadline = Instant::now() + MAX_HOLD_FLOOR + Duration::from_secs(2);
            while Instant::now() < deadline {
                if tx
                    .send(event(
                        EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
                        &[Path::new("/corpus/loud.log")],
                    ))
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let started = Instant::now();
        let burst = next_burst(&rx, &root, Duration::from_millis(400), &pr).unwrap();
        let waited = started.elapsed();
        assert!(
            waited < MAX_HOLD_FLOOR + Duration::from_millis(1500),
            "a never-quiet tree must still get a pass; waited {waited:?}"
        );
        assert!(burst.events > 1);
        drop(rx);
        let _ = noisy.join();
    }

    #[test]
    fn an_explicit_debounce_longer_than_the_floor_is_honoured() {
        // `--debounce 30000` is an instruction to wait 30 s, not a suggestion
        // the 5 s floor may overrule.
        assert_eq!(max_hold(Duration::from_millis(400)), MAX_HOLD_FLOOR);
        assert_eq!(
            max_hold(Duration::from_secs(30)),
            Duration::from_secs(60),
            "the hold cap must follow a debounce longer than the floor"
        );
    }

    #[test]
    fn the_watch_limit_message_names_the_limit_the_count_and_a_way_out() {
        let error = notify::Error::new(NotifyErrorKind::MaxFilesWatch);
        let message = watch_limit_message(Some(4096), &error);
        assert!(message.contains("4096 directories"), "{message}");
        #[cfg(target_os = "linux")]
        assert!(message.contains("fs.inotify.max_user_watches"), "{message}");
        assert!(message.contains(".xerjignore"), "{message}");
        assert!(
            message.contains("silently missing changes"),
            "a half-watched tree must be explained, not merely reported: {message}"
        );
    }

    #[test]
    fn a_carried_digest_needs_both_no_event_and_an_unchanged_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("a.md"), b"first\n").unwrap();
        fs::write(root.join("b.md"), b"second\n").unwrap();
        let files = crate::walk::walk(&root, false).unwrap();

        // Pass 1: nothing is cached, so everything is hashed.
        let empty = Carry::default();
        let full = ChangeSet::full();
        let plan = PassPlan::build(&root, &files, &empty, &full);
        assert_eq!(plan.hash_files, 2);
        assert_eq!(plan.carried_files, 0);
        for entry in &files {
            assert!(plan.carried(entry).is_none());
            plan.observe(entry, &format!("digest-{}", entry.rel), true);
        }
        let carry = plan.into_carry();
        assert_eq!(carry.len(), 2);

        // Pass 2: an event for a.md only. a.md is re-hashed, b.md is carried.
        let mut change = ChangeSet::default();
        change.mark(&root.join("a.md"));
        let plan = PassPlan::build(&root, &files, &carry, &change);
        assert_eq!(plan.hash_files, 1);
        assert_eq!(plan.carried_files, 1);
        let a = files.iter().find(|f| f.rel == "a.md").unwrap();
        let b = files.iter().find(|f| f.rel == "b.md").unwrap();
        assert!(plan.carried(a).is_none(), "the changed file must be read");
        assert_eq!(plan.carried(b).as_deref(), Some("digest-b.md"));

        // Pass 3: no event at all, but b.md was rewritten behind the watcher's
        // back. The fingerprint is the safety net.
        std::thread::sleep(Duration::from_millis(10));
        fs::write(root.join("b.md"), b"rewritten longer\n").unwrap();
        let plan = PassPlan::build(&root, &files, &carry, &ChangeSet::default());
        assert!(
            plan.carried(b).is_none(),
            "a changed fingerprint must force a re-hash even with no event"
        );
        assert_eq!(plan.carried(a).as_deref(), Some("digest-a.md"));
    }

    #[test]
    fn a_file_rewritten_while_it_was_being_hashed_is_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("a.md");
        fs::write(&path, b"before\n").unwrap();
        let files = crate::walk::walk(&root, false).unwrap();
        let plan = PassPlan::build(&root, &files, &Carry::default(), &ChangeSet::full());
        // The write lands between the fingerprint and the observation, which is
        // exactly the window a hash of a live file sits in.
        std::thread::sleep(Duration::from_millis(10));
        fs::write(&path, b"after, and a different length\n").unwrap();
        plan.observe(&files[0], "digest-of-the-old-bytes", true);
        assert_eq!(
            plan.into_carry().len(),
            0,
            "pairing a stale digest with the new mtime would hide the change for good"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_followed_symlink_is_never_carried() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let outside = dir.path().join("outside.md");
        fs::create_dir(root.join("tree")).unwrap();
        fs::write(root.join("tree/real.md"), b"real\n").unwrap();
        fs::write(&outside, b"linked\n").unwrap();
        symlink(root.join("tree/real.md"), root.join("tree/link.md")).unwrap();
        let files = crate::walk::walk(&root, true).unwrap();
        let plan = PassPlan::build(&root, &files, &Carry::default(), &ChangeSet::full());
        for entry in &files {
            plan.observe(entry, "digest", true);
        }
        let carry = plan.into_carry();
        let plan = PassPlan::build(&root, &files, &carry, &ChangeSet::default());
        for entry in files.iter().filter(|f| f.is_symlink) {
            assert!(
                plan.carried(entry).is_none(),
                "a link's own path is what events name, so its digest cannot be cached"
            );
        }
    }

    #[test]
    fn the_watch_set_is_the_directories_the_walk_admits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        fs::write(root.join(".gitignore"), b"target/\n").unwrap();
        fs::write(root.join("src/a.md"), b"a\n").unwrap();
        let dirs = crate::walk::walk_dirs_opts(
            &root,
            false,
            false,
            crate::ignore_rules::IgnoreOptions::default(),
        )
        .unwrap();
        assert!(dirs.contains(&root));
        assert!(dirs.contains(&root.join("src")));
        assert!(dirs.contains(&root.join("src/deep")));
        // An empty admitted directory must be watched too, or the first file
        // created in it is never seen.
        assert!(dirs.contains(&root.join("empty")));
        assert!(
            !dirs.iter().any(|d| d.starts_with(root.join("target"))),
            "an ignored directory must not cost a watch: {dirs:?}"
        );
        assert!(
            !dirs.iter().any(|d| d.starts_with(root.join(".git"))),
            "a hidden directory must not cost a watch: {dirs:?}"
        );
    }
}
