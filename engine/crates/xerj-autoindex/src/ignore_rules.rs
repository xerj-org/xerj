//! Ignore rules for the inventory walk (#276).
//!
//! The cheapest file to index is the one that is never read. A repository's
//! `.gitignore` already says which files are build output; honouring it means
//! `target/`, `node_modules/` and friends are pruned before the walk descends
//! into them, so they cost nothing at all — not a stat, not a hash, not a bulk
//! body.
//!
//! # Where the semantics come from
//!
//! gitignore matching is easy to get 80% right and very hard to get right:
//! nested ignore files, negation (`!keep.log`), directory-only patterns
//! (`build/`), anchoring (`/only-here`) and precedence between an ignore file
//! in a subdirectory and one at the root. We do not hand-roll any of that. The
//! matcher is ripgrep's `ignore` crate (`Unlicense OR MIT`, compatible with
//! XERJ's Apache-2.0), used for its **matching** only — traversal stays on
//! `walkdir` so the existing symlink, hidden-file and unreadable-entry
//! behaviour of [`crate::walk`] is unchanged, and so we can report *which*
//! rule discarded *what*, which `ignore`'s own walker does not expose.
//!
//! Two behaviours are copied deliberately from that reference implementation
//! (`ignore-0.4.32/src/dir.rs`, read at `~/.cargo/registry/src/*/ignore-0.4.32`):
//!
//! * **Deepest ignore file wins, per kind.** `dir.rs:563-594` walks matchers
//!   from the current directory outward and keeps the first decision each kind
//!   produces (`if m_gi.is_none() { m_gi = … }`).
//! * **Kinds are ranked, and the rank beats depth.** `dir.rs:678-684` combines
//!   them `m_custom_ignore.or(m_ignore).or(m_gi).or(m_gi_exclude)`. So a
//!   `.xerjignore` at the root outranks a `.gitignore` in a deep subdirectory.
//!   That is what makes `.xerjignore` useful as the stated "exclude things git
//!   tracks" (and re-include things git ignores) override rather than just a
//!   second `.gitignore`.
//!
//! Within one file the last matching pattern wins; that is `Gitignore::matched`
//! and we do not reimplement it.
//!
//! # What is deliberately not honoured
//!
//! The user's *global* gitignore (`core.excludesFile`) is not read: a machine-
//! wide preference silently deciding what is in someone's search index is a
//! surprise, and it is not visible in the folder being indexed. `.gitignore`,
//! `.xerjignore` and `.git/info/exclude` all live in the tree the user pointed
//! at, so they are.

use ignore::gitignore::{Gitignore, GitignoreBuilder, Glob};
use ignore::Match;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Ignore file honoured in every directory, ranked below `.xerjignore`.
pub const GITIGNORE: &str = ".gitignore";
/// XERJ's own ignore file. Same syntax as `.gitignore`, higher precedence, so
/// a user can exclude something git tracks (or re-include something git
/// ignores) without editing `.gitignore`.
pub const XERJIGNORE: &str = ".xerjignore";

/// Build output that is junk in practice but is *not* reliably gitignored —
/// `vendor/` is committed by Go projects, `dist/` and `build/` are gitignored
/// in one repo and checked in the next, and a folder full of downloaded
/// packages is very often not a git repository at all.
///
/// These are defaults, not law: `--no-default-ignores` turns them off, any
/// ignore file in the tree outranks them, and `--dry-run` names them as the
/// reason a directory disappeared.
pub const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    "node_modules/",
    "vendor/",
    "target/",
    "dist/",
    "build/",
    ".venv/",
    "__pycache__/",
];

/// Rule label used for the pre-existing dotfile skip. It is not one of the
/// ignore rules and `--no-ignore` does not disable it (it is what keeps `.env`
/// and `.ssh` out of a queryable brain), but a user whose file vanished still
/// needs it named, so it shares the reporting.
pub const HIDDEN_RULE: &str = "hidden:dotfile";

const MAX_WARNINGS: usize = 20;
/// Directory entries the `--dry-run` deep count may examine in total. A
/// pathological `node_modules` is not allowed to turn "tell me the plan" into
/// minutes of stat storm on a laptop; past the budget the count is reported as
/// a floor instead of a total.
const DEEP_COUNT_BUDGET: u64 = 1_000_000;
/// How far above the root we look for an ignore file that covers the root
/// itself. Bounded so a root near `/` cannot walk the whole filesystem.
const MAX_ANCESTOR_LEVELS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IgnoreOptions {
    /// `--no-ignore` clears this: no `.gitignore`, no `.xerjignore`, no
    /// defaults. The dotfile skip is separate and stays on.
    pub enabled: bool,
    /// `--no-default-ignores` clears this: ignore files still apply,
    /// [`DEFAULT_IGNORE_PATTERNS`] does not.
    pub defaults: bool,
    /// Count the files inside a pruned directory instead of only reporting
    /// that the directory was pruned. Worth its syscalls in `--dry-run`, whose
    /// whole job is explaining the plan, and not otherwise — the point of
    /// pruning is to never touch those paths.
    pub deep_count: bool,
}

impl Default for IgnoreOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            defaults: true,
            deep_count: false,
        }
    }
}

impl IgnoreOptions {
    /// `--no-ignore`.
    pub const fn off() -> Self {
        Self {
            enabled: false,
            defaults: false,
            deep_count: false,
        }
    }

    /// `--dry-run` explains itself; every other run stays cheap.
    pub const fn with_deep_count(mut self, deep_count: bool) -> Self {
        self.deep_count = deep_count;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleCount {
    /// Files matched directly by this rule.
    pub files: u64,
    /// Directories pruned by this rule (their contents were never walked).
    pub dirs: u64,
    /// Files found inside those pruned directories. Only ever non-zero when
    /// the deep count ran (`--dry-run`).
    pub files_inside_pruned_dirs: u64,
}

#[derive(Debug, Clone)]
pub struct IgnoreReport {
    /// rule label → what it discarded.
    pub by_rule: BTreeMap<String, RuleCount>,
    pub files_skipped: u64,
    pub dirs_pruned: u64,
    pub files_inside_pruned_dirs: u64,
    /// True unless the deep count hit [`DEEP_COUNT_BUDGET`]. When false,
    /// `files_inside_pruned_dirs` is a floor and every line that prints it
    /// says "at least".
    pub deep_count_complete: bool,
    /// Whether the deep count ran at all. When it did not, no file count
    /// inside a pruned directory is printed — an unmeasured number is not
    /// printed as zero.
    pub deep_count_ran: bool,
    pub ignore_files_read: u64,
    /// Set when the folder the user pointed at is itself covered by an ignore
    /// rule. It is indexed anyway; this is the "and here is why you are seeing
    /// files you thought were ignored" line.
    pub root_exemption: Option<String>,
    /// Unreadable or unparseable ignore files. Never fatal: an ignore file we
    /// cannot read means we index more, not less.
    pub warnings: Vec<String>,
}

impl Default for IgnoreReport {
    fn default() -> Self {
        Self {
            by_rule: BTreeMap::new(),
            files_skipped: 0,
            dirs_pruned: 0,
            files_inside_pruned_dirs: 0,
            deep_count_complete: true,
            deep_count_ran: false,
            ignore_files_read: 0,
            root_exemption: None,
            warnings: Vec::new(),
        }
    }
}

impl IgnoreReport {
    pub fn nothing_to_report(&self) -> bool {
        self.by_rule.is_empty() && self.root_exemption.is_none() && self.warnings.is_empty()
    }

    /// Human-readable lines for the progress surface. Every number here was
    /// counted by this run; the ones that were deliberately not counted are
    /// named as such instead of being printed as zero.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for warning in &self.warnings {
            out.push(format!("ignore rules: {warning}"));
        }
        if let Some(reason) = &self.root_exemption {
            out.push(format!("ignore rules: {reason}"));
        }
        if self.by_rule.is_empty() {
            return out;
        }
        let mut headline = format!(
            "ignore rules: skipped {} file{} and pruned {} director{}",
            self.files_skipped,
            plural(self.files_skipped),
            self.dirs_pruned,
            if self.dirs_pruned == 1 { "y" } else { "ies" },
        );
        if self.deep_count_ran && self.dirs_pruned > 0 {
            headline.push_str(&format!(
                " ({}{} file{} inside them)",
                if self.deep_count_complete {
                    ""
                } else {
                    "at least "
                },
                self.files_inside_pruned_dirs,
                plural(self.files_inside_pruned_dirs),
            ));
        } else if self.dirs_pruned > 0 {
            headline.push_str(" (contents never walked, so not counted)");
        }
        if self.ignore_files_read > 0 {
            headline.push_str(&format!(
                "; {} ignore file{} read",
                self.ignore_files_read,
                plural(self.ignore_files_read)
            ));
        }
        out.push(headline);

        let mut ranked: Vec<(&String, &RuleCount)> = self.by_rule.iter().collect();
        ranked.sort_by(|a, b| {
            let weight = |c: &RuleCount| c.files + c.files_inside_pruned_dirs + c.dirs;
            weight(b.1).cmp(&weight(a.1)).then_with(|| a.0.cmp(b.0))
        });
        const SHOWN: usize = 10;
        for (label, count) in ranked.iter().take(SHOWN) {
            let mut parts = Vec::new();
            if count.files > 0 {
                parts.push(format!("{} file{}", count.files, plural(count.files)));
            }
            if count.dirs > 0 {
                let inside = if self.deep_count_ran {
                    format!(
                        " ({}{} file{} inside)",
                        if self.deep_count_complete {
                            ""
                        } else {
                            "at least "
                        },
                        count.files_inside_pruned_dirs,
                        plural(count.files_inside_pruned_dirs)
                    )
                } else {
                    String::new()
                };
                parts.push(format!(
                    "{} director{} pruned{inside}",
                    count.dirs,
                    if count.dirs == 1 { "y" } else { "ies" }
                ));
            }
            out.push(format!("ignore rules:   {label} — {}", parts.join(", ")));
        }
        if ranked.len() > SHOWN {
            out.push(format!(
                "ignore rules:   … and {} more rule{}",
                ranked.len() - SHOWN,
                plural((ranked.len() - SHOWN) as u64)
            ));
        }
        out
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[derive(Default)]
struct Layer {
    xerj: Option<Gitignore>,
    git: Option<Gitignore>,
    exclude: Option<Gitignore>,
}

/// A stack of ignore matchers, one entry per directory on the current
/// root-to-here path, plus the built-in defaults.
///
/// The caller drives it: [`IgnoreStack::enter_dir`] when a directory is
/// yielded, [`IgnoreStack::skip_dir`] / [`IgnoreStack::skip_file`] to ask
/// whether an entry survives.
pub struct IgnoreStack {
    opts: IgnoreOptions,
    root: PathBuf,
    layers: Vec<Layer>,
    defaults: Option<Gitignore>,
    budget: u64,
    pub report: IgnoreReport,
}

impl IgnoreStack {
    /// `root` must be the canonicalised root folder.
    pub fn new(root: &Path, opts: IgnoreOptions) -> Self {
        let mut stack = Self {
            opts,
            root: root.to_path_buf(),
            layers: Vec::new(),
            defaults: None,
            budget: DEEP_COUNT_BUDGET,
            report: IgnoreReport {
                deep_count_ran: opts.deep_count,
                ..IgnoreReport::default()
            },
        };
        if opts.enabled && opts.defaults {
            let defaults = stack.build_defaults(root);
            stack.defaults = defaults;
        }
        let exemption = stack.root_exemption(root);
        stack.report.root_exemption = exemption;
        stack
    }

    fn build_defaults(&mut self, root: &Path) -> Option<Gitignore> {
        let mut builder = GitignoreBuilder::new(root);
        for pattern in DEFAULT_IGNORE_PATTERNS {
            if let Err(err) = builder.add_line(None, pattern) {
                self.warn(format!("built-in rule `{pattern}` is not usable: {err}"));
            }
        }
        match builder.build() {
            Ok(gi) if gi.is_empty() => None,
            Ok(gi) => Some(gi),
            Err(err) => {
                self.warn(format!("built-in default rules are unusable: {err}"));
                None
            }
        }
    }

    /// Detect that the folder the user pointed at is itself ignored.
    ///
    /// We never act on it — pointing autoindex at `~/proj/target` is an
    /// explicit instruction and silently indexing nothing would be the worst
    /// possible answer. We only explain it, because the alternative is a user
    /// staring at a brain full of build output wondering why their ignore file
    /// did nothing.
    fn root_exemption(&self, root: &Path) -> Option<String> {
        if !self.opts.enabled {
            return None;
        }
        let name = root.file_name()?;
        let parent = root.parent()?;
        // The defaults are anchored at the root itself, so they cannot see the
        // root's own name. Re-anchor a copy one level up to ask the question.
        if self.opts.defaults {
            let mut builder = GitignoreBuilder::new(parent);
            for pattern in DEFAULT_IGNORE_PATTERNS {
                let _ = builder.add_line(None, pattern);
            }
            if let Ok(gi) = builder.build() {
                if let Match::Ignore(glob) = gi.matched(Path::new(name), true) {
                    return Some(format!(
                        "{} matches the built-in default rule `{}`, but you pointed autoindex at \
                         it, so it is being indexed — pass --no-default-ignores to silence this \
                         note",
                        root.display(),
                        glob.original()
                    ));
                }
            }
        }
        // Ignore files *above* the root are consulted for this note only, and
        // never to prune: git itself stops at the repository root, so we do
        // too (plus a hard level cap).
        let mut dir = parent.to_path_buf();
        for _ in 0..MAX_ANCESTOR_LEVELS {
            for file in [XERJIGNORE, GITIGNORE] {
                let path = dir.join(file);
                if !path.is_file() {
                    continue;
                }
                let mut builder = GitignoreBuilder::new(&dir);
                let _ = builder.add(&path);
                let Ok(gi) = builder.build() else { continue };
                if gi.is_empty() {
                    continue;
                }
                if let Match::Ignore(glob) = gi.matched_path_or_any_parents(root, true) {
                    return Some(format!(
                        "{} is ignored by `{}` in {}, but you pointed autoindex at it, so it is \
                         being indexed",
                        root.display(),
                        glob.original(),
                        path.display()
                    ));
                }
            }
            if dir.join(".git").exists() {
                break;
            }
            match dir.parent() {
                Some(up) => dir = up.to_path_buf(),
                None => break,
            }
        }
        None
    }

    /// Drop the layers of directories the walk has finished with.
    ///
    /// Must be called for **every** entry, not only directories. A depth-first
    /// walk interleaves files and subdirectories in readdir order, so after it
    /// descends into `a/` and comes back it can hand us `b.txt` at the shallower
    /// depth while `a/`'s layer is still on the stack. Without this, `a/`'s
    /// `.gitignore` would silently judge its parent's files.
    pub fn truncate_to(&mut self, depth: usize) {
        self.layers.truncate(depth);
    }

    /// Called for every directory the walk yields, root first, before any of
    /// its children. `depth` is walkdir's depth, so the stack is indexed by
    /// depth and a sibling directory replaces its predecessor's layer.
    pub fn enter_dir(&mut self, dir: &Path, depth: usize) {
        self.layers.truncate(depth);
        let layer = if self.opts.enabled {
            Layer {
                xerj: self.load(dir, dir.join(XERJIGNORE)),
                git: self.load(dir, dir.join(GITIGNORE)),
                // Per-repository excludes, which is where people put "junk
                // only I have". Loaded per directory so a nested checkout gets
                // its own.
                exclude: self.load(dir, dir.join(".git").join("info").join("exclude")),
            }
        } else {
            Layer::default()
        };
        self.layers.push(layer);
    }

    fn load(&mut self, dir: &Path, file: PathBuf) -> Option<Gitignore> {
        if !file.is_file() {
            return None;
        }
        let mut builder = GitignoreBuilder::new(dir);
        if let Some(err) = builder.add(&file) {
            self.warn(format!("{}: {err}", file.display()));
        }
        match builder.build() {
            Ok(gi) => {
                self.report.ignore_files_read += 1;
                if gi.is_empty() {
                    None
                } else {
                    Some(gi)
                }
            }
            Err(err) => {
                self.warn(format!("{} is not usable: {err}", file.display()));
                None
            }
        }
    }

    fn warn(&mut self, message: String) {
        if self.report.warnings.len() < MAX_WARNINGS {
            self.report.warnings.push(message);
        }
    }

    /// ripgrep's precedence, reproduced: the deepest match *per kind*, then
    /// the kinds ranked `.xerjignore` > `.gitignore` > `.git/info/exclude` >
    /// built-in defaults (`ignore-0.4.32/src/dir.rs:563-594` and `:678-684`).
    fn lookup(&self, path: &Path, is_dir: bool) -> Option<(bool, String)> {
        if !self.opts.enabled {
            return None;
        }
        let mut xerj = None;
        let mut git = None;
        let mut exclude = None;
        for layer in self.layers.iter().rev() {
            if xerj.is_none() {
                xerj = self.decide_with(layer.xerj.as_ref(), path, is_dir);
            }
            if git.is_none() {
                git = self.decide_with(layer.git.as_ref(), path, is_dir);
            }
            if exclude.is_none() {
                exclude = self.decide_with(layer.exclude.as_ref(), path, is_dir);
            }
            if xerj.is_some() {
                // Nothing ranked lower can change the answer.
                break;
            }
        }
        xerj.or(git)
            .or(exclude)
            .or_else(|| self.decide_with(self.defaults.as_ref(), path, is_dir))
    }

    fn decide_with(
        &self,
        matcher: Option<&Gitignore>,
        path: &Path,
        is_dir: bool,
    ) -> Option<(bool, String)> {
        match matcher?.matched(path, is_dir) {
            Match::None => None,
            Match::Ignore(glob) => Some((true, self.label(glob))),
            Match::Whitelist(glob) => Some((false, self.label(glob))),
        }
    }

    fn label(&self, glob: &Glob) -> String {
        let from = glob
            .from()
            .map(|p| self.rel(p))
            .unwrap_or_else(|| "<built-in>".to_string());
        format!("{from}:{}", glob.original())
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Ask whether a directory must be pruned. `Some(label)` means do not
    /// descend; the report has already been updated.
    pub fn skip_dir(&mut self, path: &Path) -> Option<String> {
        let (ignored, label) = self.lookup(path, true)?;
        if !ignored {
            return None;
        }
        self.record_dir(path, &label);
        Some(label)
    }

    /// Ask whether a file must be skipped.
    pub fn skip_file(&mut self, path: &Path) -> Option<String> {
        let (ignored, label) = self.lookup(path, false)?;
        if !ignored {
            return None;
        }
        self.record_file(&label);
        Some(label)
    }

    /// The dotfile skip is decided by the walker, not here, but it is reported
    /// through the same accounting so `--dry-run` explains every missing file
    /// with one mechanism.
    pub fn record_hidden(&mut self, path: &Path, is_dir: bool) {
        if is_dir {
            self.record_dir(path, HIDDEN_RULE);
        } else {
            self.record_file(HIDDEN_RULE);
        }
    }

    /// The marker-gated generated-dir prune (`walk::MARKER_GENERATED_DIRS`)
    /// is also decided by the walker — the verdict needs the SIBLING marker
    /// file, which the per-directory matcher stack cannot see — and reported
    /// through the same accounting.
    pub fn record_marker_dir(&mut self, path: &Path, label: &str) {
        self.record_dir(path, label);
    }

    fn record_file(&mut self, label: &str) {
        self.report.files_skipped += 1;
        self.report
            .by_rule
            .entry(label.to_string())
            .or_default()
            .files += 1;
    }

    fn record_dir(&mut self, path: &Path, label: &str) {
        self.report.dirs_pruned += 1;
        self.report
            .by_rule
            .entry(label.to_string())
            .or_default()
            .dirs += 1;
        let inside = self.count_inside(path);
        self.report.files_inside_pruned_dirs += inside;
        self.report
            .by_rule
            .entry(label.to_string())
            .or_default()
            .files_inside_pruned_dirs += inside;
    }

    /// Only under `--dry-run`. Symlinks are never followed here whatever the
    /// run's `--follow-symlinks` setting: this is an accounting pass over a
    /// directory we have already decided not to index, and it must not be able
    /// to wander outside it or loop.
    ///
    /// The dotfile rule is applied here too, so the number means "files that
    /// would otherwise have been indexed" and not "inodes under this path".
    /// Without it a pruned directory containing a nested checkout reports its
    /// whole `.git`, which was never indexable in the first place — a number
    /// that does not reconcile with the before/after file count is worse than
    /// no number.
    fn count_inside(&mut self, dir: &Path) -> u64 {
        if !self.opts.deep_count {
            return 0;
        }
        let mut files = 0;
        let walker = walkdir::WalkDir::new(dir)
            .follow_links(false)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| {
                !e.file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with('.'))
            });
        for entry in walker {
            if self.budget == 0 {
                self.report.deep_count_complete = false;
                break;
            }
            self.budget -= 1;
            if entry.is_ok_and(|e| e.file_type().is_file()) {
                files += 1;
            }
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Drive the stack the way `crate::walk` does, over a sorted listing, and
    /// return the surviving root-relative file paths.
    fn survivors(root: &Path, opts: IgnoreOptions) -> (Vec<String>, IgnoreReport) {
        let root = root.canonicalize().unwrap();
        let mut stack = IgnoreStack::new(&root, opts);
        let mut kept = Vec::new();
        let mut it = walkdir::WalkDir::new(&root).into_iter();
        while let Some(next) = it.next() {
            let Ok(entry) = next else { continue };
            let is_dir = entry.file_type().is_dir();
            if entry.depth() > 0
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with('.'))
            {
                stack.record_hidden(entry.path(), is_dir);
                if is_dir {
                    it.skip_current_dir();
                }
                continue;
            }
            stack.truncate_to(entry.depth());
            if is_dir {
                if entry.depth() > 0 && stack.skip_dir(entry.path()).is_some() {
                    it.skip_current_dir();
                    continue;
                }
                stack.enter_dir(entry.path(), entry.depth());
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            if stack.skip_file(entry.path()).is_some() {
                continue;
            }
            kept.push(
                entry
                    .path()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        kept.sort();
        (kept, stack.report)
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn root_gitignore_prunes_and_negation_re_includes() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "*.log\n!keep.log\nbuild/\n");
        write(root, "a.log", "x");
        write(root, "keep.log", "x");
        write(root, "src/main.rs", "x");
        write(root, "src/nested.log", "x");
        write(root, "build/out.bin", "x");

        let (kept, report) = survivors(root, IgnoreOptions::default());
        assert_eq!(kept, vec!["keep.log", "src/main.rs"], "{kept:?}");
        // `*.log` has no slash, so it matches at any depth — git's rule.
        assert!(
            report.by_rule.contains_key(".gitignore:*.log"),
            "{report:?}"
        );
        assert!(
            report.by_rule.contains_key(".gitignore:build/"),
            "{report:?}"
        );
        assert_eq!(report.dirs_pruned, 1);
        assert_eq!(
            report.by_rule[".gitignore:*.log"].files, 2,
            "a.log and src/nested.log"
        );
        // 3, not 2: `.gitignore` is itself a dotfile, so the pre-existing
        // hidden-file rule accounts for it in the same report.
        assert_eq!(report.files_skipped, 3, "{report:?}");
        assert_eq!(report.by_rule[HIDDEN_RULE].files, 1, "{report:?}");
    }

    #[test]
    fn a_nested_gitignore_outranks_the_root_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "*.log\n");
        write(root, "logs/.gitignore", "!*.log\n");
        write(root, "top.log", "x");
        write(root, "logs/kept.log", "x");

        let (kept, _) = survivors(root, IgnoreOptions::default());
        assert_eq!(kept, vec!["logs/kept.log"], "{kept:?}");
    }

    #[test]
    fn xerjignore_outranks_gitignore_at_any_depth() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        // First line: git tracks it, the user does not want it in the index.
        // Second: git ignores it, the user *does* want it — and a root
        // .xerjignore outranks a deep .gitignore, which is ripgrep's rule.
        write(
            root,
            ".xerjignore",
            "tracked-but-huge.csv\n!deep/wanted.bin\n",
        );
        write(root, "deep/.gitignore", "*.bin\n");
        write(root, "tracked-but-huge.csv", "x");
        write(root, "deep/wanted.bin", "x");
        write(root, "deep/other.bin", "x");
        write(root, "ok.txt", "x");

        let (kept, report) = survivors(root, IgnoreOptions::default());
        assert_eq!(kept, vec!["deep/wanted.bin", "ok.txt"], "{kept:?}");
        assert!(
            report
                .by_rule
                .contains_key(".xerjignore:tracked-but-huge.csv"),
            "{report:?}"
        );
    }

    #[test]
    fn defaults_prune_build_output_that_no_ignore_file_mentions() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, "index.js", "x");
        write(root, "node_modules/left-pad/index.js", "x");
        write(root, "target/debug/huge.rlib", "x");
        write(root, "__pycache__/mod.cpython-311.pyc", "x");

        let (kept, report) = survivors(root, IgnoreOptions::default());
        assert_eq!(kept, vec!["index.js"], "{kept:?}");
        assert_eq!(report.dirs_pruned, 3);
        assert!(
            report.by_rule.contains_key("<built-in>:node_modules/"),
            "{report:?}"
        );

        let (all, _) = survivors(
            root,
            IgnoreOptions {
                defaults: false,
                ..IgnoreOptions::default()
            },
        );
        assert_eq!(all.len(), 4, "--no-default-ignores must keep them: {all:?}");
    }

    #[test]
    fn an_ignore_file_outranks_a_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        // A Go project that commits its vendor tree.
        write(root, ".gitignore", "!vendor/\n");
        write(root, "vendor/lib/lib.go", "x");
        let (kept, _) = survivors(root, IgnoreOptions::default());
        assert_eq!(kept, vec!["vendor/lib/lib.go"], "{kept:?}");
    }

    #[test]
    fn no_ignore_disables_every_rule_but_not_the_dotfile_skip() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "*.log\n");
        write(root, "a.log", "x");
        write(root, "node_modules/x.js", "x");
        write(root, ".env", "SECRET=1");

        let (kept, report) = survivors(root, IgnoreOptions::off());
        assert_eq!(kept, vec!["a.log", "node_modules/x.js"], "{kept:?}");
        assert!(!kept.iter().any(|k| k == ".env"), "{kept:?}");
        assert_eq!(report.by_rule.get(HIDDEN_RULE).unwrap().files, 2);
    }

    #[test]
    fn git_info_exclude_is_honoured() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".git/info/exclude", "scratch.txt\n");
        write(root, "scratch.txt", "x");
        write(root, "real.txt", "x");
        let (kept, _) = survivors(root, IgnoreOptions::default());
        assert_eq!(kept, vec!["real.txt"], "{kept:?}");
    }

    #[test]
    fn an_ignored_root_is_indexed_anyway_and_says_why() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        write(&repo, ".gitignore", "generated/\n");
        write(&repo, "generated/out.o", "x");

        let root = repo.join("generated");
        let (kept, report) = survivors(&root, IgnoreOptions::default());
        assert_eq!(kept, vec!["out.o"], "an explicit root must be indexed");
        let reason = report.root_exemption.expect("must explain itself");
        assert!(reason.contains("generated/"), "{reason}");
        assert!(reason.contains(".gitignore"), "{reason}");
    }

    #[test]
    fn a_root_named_like_a_default_is_indexed_anyway_and_says_why() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("node_modules");
        fs::create_dir_all(&root).unwrap();
        write(&root, "left-pad/index.js", "x");
        let (kept, report) = survivors(&root, IgnoreOptions::default());
        assert_eq!(kept, vec!["left-pad/index.js"], "{kept:?}");
        let reason = report.root_exemption.expect("must explain itself");
        assert!(reason.contains("node_modules/"), "{reason}");
    }

    #[test]
    fn deep_count_is_off_by_default_and_counts_under_dry_run() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, "keep.txt", "x");
        for i in 0..7 {
            write(root, &format!("node_modules/pkg/f{i}.js"), "x");
        }

        let (_, cheap) = survivors(root, IgnoreOptions::default());
        assert_eq!(cheap.files_inside_pruned_dirs, 0);
        assert!(!cheap.deep_count_ran);
        assert!(
            cheap
                .summary_lines()
                .iter()
                .any(|l| l.contains("contents never walked")),
            "{:?}",
            cheap.summary_lines()
        );

        let (_, deep) = survivors(root, IgnoreOptions::default().with_deep_count(true));
        assert_eq!(deep.files_inside_pruned_dirs, 7);
        assert!(deep.deep_count_complete);
        assert!(
            deep.summary_lines().iter().any(|l| l.contains("7 files")),
            "{:?}",
            deep.summary_lines()
        );
    }

    /// Regression, order-independent: a depth-first walk interleaves files and
    /// subdirectories, so a nested `.gitignore` must be off the stack again by
    /// the time the walk hands back a file from the parent directory. Driven
    /// through the exact call sequence `crate::walk` makes, because readdir
    /// order is not something a test can pin.
    #[test]
    fn a_nested_ignore_file_never_judges_its_parents_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write(&root, "a/.gitignore", "*.txt\n");
        write(&root, "a/inside.txt", "x");
        write(&root, "b.txt", "x");

        let mut stack = IgnoreStack::new(&root, IgnoreOptions::default());
        stack.truncate_to(0);
        stack.enter_dir(&root, 0);
        // …descend into a/ …
        stack.truncate_to(1);
        stack.enter_dir(&root.join("a"), 1);
        stack.truncate_to(2);
        assert!(
            stack
                .skip_file(&root.join("a").join("inside.txt"))
                .is_some(),
            "a/.gitignore must apply inside a/"
        );
        // …and come back out to a sibling file of a/.
        stack.truncate_to(1);
        assert!(
            stack.skip_file(&root.join("b.txt")).is_none(),
            "a/.gitignore leaked onto the parent's files"
        );
    }

    /// A broken ignore file must never be able to make files disappear
    /// silently: it is reported, and its only possible effect is that we index
    /// MORE than the user asked for, never less.
    #[test]
    fn an_unreadable_ignore_file_warns_and_indexes_more_not_less() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), b"\xff\xfe not text\n*.txt\n").unwrap();
        fs::write(root.join("a.txt"), "x").unwrap();
        let (kept, report) = survivors(root, IgnoreOptions::default());
        assert_eq!(
            kept,
            vec!["a.txt"],
            "an ignore file we cannot read must not hide anything"
        );
        assert!(!report.warnings.is_empty(), "must warn: {report:?}");
        assert!(
            report
                .summary_lines()
                .iter()
                .any(|l| l.contains(".gitignore")),
            "the warning must reach the user: {:?}",
            report.summary_lines()
        );
    }
}
