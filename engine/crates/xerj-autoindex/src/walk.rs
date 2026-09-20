//! Inventory: recursive walk of the target folder.
//! Symlinks are NOT followed by default; with --follow-symlinks walkdir's
//! ancestor-loop detection keeps the traversal loop-safe.
//!
//! Ignore rules (`.gitignore`, `.xerjignore`, built-in build-output defaults)
//! are applied *during* the walk, not after it: an ignored directory is never
//! descended, so its contents cost nothing — no stat, no hash, no bulk body.
//! The matching itself lives in [`crate::ignore_rules`].

use crate::ignore_rules::{is_hidden_name, IgnoreOptions, IgnoreReport, IgnoreStack};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    /// Root-relative path with forward slashes — the stable `ax_path` value.
    pub rel: String,
    /// Reversible platform-native identity; unlike `rel`, never lossy.
    pub rel_id: String,
    /// True when the discovered path itself is a symlink (target metadata follows).
    pub is_symlink: bool,
    pub size: u64,
}

/// Generated/cache directories whose NAMES are too common for the blunt
/// [`crate::ignore_rules::DEFAULT_IGNORE_PATTERNS`] (a folder named `Logs/`
/// or `Library/` is often real data), pruned at any depth ONLY when a sibling
/// marker file proves the parent directory is that kind of project. Part of
/// the built-in defaults: `--no-ignore` / `--no-default-ignores` disable it,
/// and any ignore file in the tree can re-include (`!Library/`) as usual —
/// these run after the ignore files have had their say.
///
/// (dir name, marker path relative to the dir's PARENT, report label)
const MARKER_GENERATED_DIRS: &[(&str, &str, &str)] = &[
    (
        "Library",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "Temp",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "obj",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "Logs",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
    (
        "UserSettings",
        "ProjectSettings/ProjectVersion.txt",
        "default:unity-generated",
    ),
];

/// The marker-gated verdict for one directory entry.
fn marker_generated_dir(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?;
    for (dir, marker, label) in MARKER_GENERATED_DIRS {
        if name == *dir && path.parent().is_some_and(|p| p.join(marker).is_file()) {
            return Some(label);
        }
    }
    None
}

/// The file Google Takeout writes at the top of every export: an HTML table of
/// contents for the archive. Its presence is what makes a directory a Takeout
/// root — the `Takeout` folder NAME is not tested, because people rename it,
/// merge several parts into one folder, or point autoindex at the inside of it.
pub const TAKEOUT_MARKER: &str = "archive_browser.html";

/// The export's own table of contents. It is not the user's data: it lists
/// every product and file name in the archive, so indexing it puts one large
/// document in front of every search that names a file.
pub const TAKEOUT_INDEX_RULE: &str = "default:takeout-index";

/// Google Keep exports EVERY note twice — `Note.json` (structured: labels,
/// timestamps, checklist state) and `Note.html` (the same note rendered). The
/// JSON is indexed as a document by `extract::json::keep_note`; indexing the
/// HTML twin as well answers every Keep search twice.
pub const TAKEOUT_KEEP_TWIN_RULE: &str = "default:takeout-keep-html-twin";

/// Is `dir` the root of a Google Takeout export?
fn is_takeout_root(dir: &Path) -> bool {
    dir.join(TAKEOUT_MARKER).is_file()
}

/// The marker-gated verdict for one FILE of a Google Takeout export: `Some`
/// names the rule that makes it noise. Part of the built-in defaults, exactly
/// like [`MARKER_GENERATED_DIRS`], and deliberately short — two rules, each
/// about a file that DUPLICATES something else in the export. Everything else
/// in a Takeout is the user's data and is left to the ordinary sniffing:
/// dropping someone's data on a guess about a product folder is worse than
/// indexing a little noise.
///
/// * `archive_browser.html` in a Takeout root → [`TAKEOUT_INDEX_RULE`].
/// * `X.html` directly inside a product folder of a Takeout root, with a
///   sibling `X.json` that is a Keep note → [`TAKEOUT_KEEP_TWIN_RULE`]. The
///   product folder's NAME is not tested (it is localised: `Keep`, `Notizen`,
///   …); the sibling JSON's content is, by the same predicate the extractor
///   uses, so an unrelated `report.html` beside a `report.json` is untouched.
fn takeout_noise(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?;
    let dir = path.parent()?;
    if name == TAKEOUT_MARKER {
        // `dir` holds this very file, so it is a Takeout root by definition.
        return Some(TAKEOUT_INDEX_RULE);
    }
    let is_html = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("html"));
    if is_html && dir.parent().is_some_and(is_takeout_root) {
        let twin = path.with_extension("json");
        if twin.is_file() && crate::extract::json::is_keep_note_file(&twin) {
            return Some(TAKEOUT_KEEP_TWIN_RULE);
        }
    }
    None
}

/// Why a followed symlink was refused. See the call site in `walk_reporting`.
enum SymlinkVerdict {
    /// The resolved target is not under the indexed root. Carries the
    /// resolution so the waiver arm can store it: an admitted entry must read
    /// from the path that was judged rather than from the link it arrived by,
    /// and out-of-root entries are the only ones that arm ever admits.
    OutsideRoot(PathBuf),
    /// Outside the root AND through a dotted component. Separate from
    /// `OutsideRoot` because `--follow-symlinks-outside-root` must not reach
    /// it, and separate from `HiddenTarget` because the operator has no dotfile
    /// in their own folder to go looking for — the actionable fact is that a
    /// link left it.
    HiddenOutsideRoot,
    /// The resolved target is under the root but its real path runs through a
    /// hidden component — the link's visible name was hiding a dotfile.
    HiddenTarget,
    /// The path could not be resolved at all, so neither question could be
    /// answered. Refused rather than admitted; see `escaped_or_hidden_target`.
    Unresolvable,
}

/// Judge a followed entry by where it actually resolves.
///
/// `path` is the path the walk arrived by, so under `--follow-symlinks` it can
/// name a link anywhere along its length. Canonicalising collapses every link
/// in it at once, which is what makes a chain of links (`a -> b -> .secret/c`)
/// and a link partway up a directory path behave the same as a direct one.
///
/// `Ok(Some(real))` is the resolved path, which the caller stores on the
/// `FileEntry` so every later `open()` reads the file that was judged.
/// `Ok(None)` means there is nothing to resolve and the ordinary rules govern
/// the entry.
///
/// A resolution FAILURE is not `None`. An earlier version of this function
/// wrote `path.canonicalize().ok()?`, which collapsed every error into "the
/// entry is where it appears" and justified it with "a broken link has no
/// target to leak". That is true of `NotFound` and of nothing else, and the
/// difference is a bypass rather than a nicety: `canonicalize` is `realpath(3)`
/// and is bounded by `PATH_MAX`, while the kernel's own resolution is not, so an
/// entry can be unresolvable HERE and perfectly readable by every later stage —
/// `stat`, `open`, hashing, extraction. The guard was then skipped for exactly
/// the entries it could not vet, which is the wrong direction for a rule whose
/// job is to keep `.ssh` out of a queryable index. `ELOOP` and `EACCES` land in
/// the same arm.
///
/// So only `NotFound` is treated as "nothing here to leak" — walkdir already
/// reports a dangling link through its error arm. Everything else is refused
/// and named in the report, because an answer that could not be computed is not
/// an answer of "yes".
fn escaped_or_hidden_target(
    root_canon: &Path,
    path: &Path,
) -> Result<Option<PathBuf>, SymlinkVerdict> {
    let real = match path.canonicalize() {
        Ok(real) => real,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SymlinkVerdict::Unresolvable),
    };
    let Some(rel) = real.strip_prefix(root_canon).ok() else {
        // Outside the root — but the hidden-name question is asked FIRST and
        // against the whole resolved path, because `OutsideRoot` is the only
        // verdict `--follow-symlinks-outside-root` waives. Returning it before
        // looking for a dotted component let that flag admit
        // `keys.txt -> ../outside/.ssh/id_rsa` unexamined, which is #438
        // reopened through the escape hatch built to be safe.
        //
        // Judged from where the target DIVERGES from the root, not from `/`.
        // Checking every component would refuse any target whose absolute path
        // passes through a dotted ancestor — `/home/u/.cache/...`, and every
        // path under a `tempfile` directory, which is named `.tmpXXXXXX`. Those
        // ancestors are shared with the root the operator pointed at, so they
        // are as consented-to as the root itself; the components BELOW the
        // divergence are the ones nobody asked for.
        let shared = root_canon
            .components()
            .zip(real.components())
            .take_while(|(a, b)| a == b)
            .count();
        if real
            .components()
            .skip(shared)
            .any(|c| is_hidden_name(c.as_os_str()))
        {
            return Err(SymlinkVerdict::HiddenOutsideRoot);
        }
        return Err(SymlinkVerdict::OutsideRoot(real));
    };
    // In-root: judge only the part below the root. A brain over a dot-named
    // folder still works — that exemption is the same one the walker gives the
    // root itself at depth 0.
    if rel.components().any(|c| is_hidden_name(c.as_os_str())) {
        return Err(SymlinkVerdict::HiddenTarget);
    }
    Ok(Some(real))
}

/// Walk with the default ignore rules on and the report discarded.
pub fn walk(root: &Path, follow_symlinks: bool) -> Result<Vec<FileEntry>> {
    walk_reporting(root, follow_symlinks, IgnoreOptions::default()).map(|(files, _)| files)
}

/// Walk, and say what was left out and why.
///
/// The returned [`IgnoreReport`] is the answer to "my files vanished" — it
/// names every rule that discarded something and how much. It is reported on
/// every run and, under `--dry-run`, also counts what was inside each pruned
/// directory.
pub fn walk_reporting(
    root: &Path,
    follow_symlinks: bool,
    ignore: IgnoreOptions,
) -> Result<(Vec<FileEntry>, IgnoreReport)> {
    walk_reporting_opts(root, follow_symlinks, false, ignore)
}

/// As [`walk_reporting`], with the out-of-root escape hatch.
///
/// `allow_outside_root` restores the pre-#438 behaviour of following a link
/// wherever it points. It exists because refusing every out-of-root target is
/// not a narrowing of `--follow-symlinks` — for a vendored sibling checkout, a
/// monorepo package link or a mounted notes volume it IS the flag, and without
/// an opt-in those runs silently index what they would index with the flag off.
/// The hidden-name rule still applies to the resolved path either way; only the
/// root boundary is waived, and only when the operator says so.
pub fn walk_reporting_opts(
    root: &Path,
    follow_symlinks: bool,
    allow_outside_root: bool,
    ignore: IgnoreOptions,
) -> Result<(Vec<FileEntry>, IgnoreReport)> {
    walk_impl(root, follow_symlinks, allow_outside_root, ignore, None)
}

/// The directories this walk ADMITS, and nothing else.
///
/// `--watch` places one filesystem watch per admitted directory instead of one
/// recursive watch on the root, and this is where that set comes from: the same
/// traversal, the same hidden-name rule, the same `.gitignore`/`.xerjignore`
/// stack, the same marker-gated prunes. Deriving the watch set any other way is
/// how a watched run and a re-run start disagreeing about what is indexed — an
/// ignored `target/` would be watched (thousands of events per build), and a
/// re-included directory would not be watched at all.
///
/// Files are not stat'ed on this route: the caller wants watch targets, not an
/// inventory, and the root is always included (it is admitted at depth 0
/// exactly like the file walk admits it).
pub fn walk_dirs_opts(
    root: &Path,
    follow_symlinks: bool,
    allow_outside_root: bool,
    ignore: IgnoreOptions,
) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    walk_impl(
        root,
        follow_symlinks,
        allow_outside_root,
        ignore,
        Some(&mut dirs),
    )?;
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

/// One traversal, two callers: the file inventory and (`dirs_out`) the watch
/// set. With `dirs_out` set, every admitted directory is collected and files are
/// not examined at all.
fn walk_impl(
    root: &Path,
    follow_symlinks: bool,
    allow_outside_root: bool,
    ignore: IgnoreOptions,
    mut dirs_out: Option<&mut Vec<PathBuf>>,
) -> Result<(Vec<FileEntry>, IgnoreReport)> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("resolve root folder {}", root.display()))?;
    if !root_canon.is_dir() {
        anyhow::bail!("{} is not a directory", root_canon.display());
    }
    let mut out = Vec::new();
    // Shallowest depth from which the walk is inside a symlink; see the guard.
    let mut link_depth: Option<usize> = None;
    let marker_defaults = ignore.enabled && ignore.defaults;
    let mut stack = IgnoreStack::new(&root_canon, ignore);
    // Manual iteration rather than `filter_entry`, because the ignore stack has
    // to load a directory's ignore files *after* that directory is admitted and
    // *before* its children are judged. `skip_current_dir` on a just-yielded
    // directory prunes its contents — walkdir's own documented pattern, and the
    // same effect `filter_entry` had.
    let mut it = walkdir::WalkDir::new(&root_canon)
        .follow_links(follow_symlinks)
        .into_iter();
    while let Some(next) = it.next() {
        let entry = match next {
            Ok(e) => e,
            Err(e) => {
                // This message renders the offending PATH, and it goes to the
                // same stderr an agent parses for `xerj-progress` /
                // `xerj-done` records. An unreadable directory whose name
                // carries a newline would otherwise start a line here — the
                // #278 forgery, through a surface the progress module does not
                // own. Sanitised for the same reason and by the same rule.
                //
                // Routing this through `Progress` as well (so `--quiet` is
                // silent and `--progress json` stays one object per line) needs
                // a handle the walker does not have; that is a separate change.
                eprintln!(
                    "walk: skipping unreadable entry: {}",
                    crate::progress::sanitize(&e.to_string(), crate::progress::SAFE_PATH_MAX)
                );
                continue;
            }
        };
        let is_dir = entry.file_type().is_dir();
        // Retire the layers of directories this entry is no longer inside; see
        // `IgnoreStack::truncate_to` for why every entry needs it, not just
        // directories.
        stack.truncate_to(entry.depth());
        // SECURITY / hygiene: do not index hidden files or descend into hidden
        // directories. Without this the walker happily indexed `.env` (secrets,
        // API tokens), `.git`, `.ssh`, `.aws`, and other dotfiles into a
        // queryable brain with no per-brain authorization — a real exposure for
        // the "point it at my project folder" use case. A hidden directory is
        // pruned before descending, so `.git/` is skipped whole. The root
        // itself (depth 0) is exempt so a brain over a dot-named folder still
        // works. `--no-ignore` does NOT turn this off: it is not one of the
        // ignore rules, it is the main reason secrets stay out.
        //
        // The limit of that claim, stated because the rest of this file used to
        // assert it without one: this is a rule about NAMES and, under
        // `--follow-symlinks`, about where a link RESOLVES. A hard link has
        // neither — it is a second directory entry for the same inode, so
        // `ln .secretdir/k.txt notes.txt` reads the secret under a visible name
        // and nothing here can tell. Defending that needs `(dev, ino)` of every
        // hidden-rule exclusion, which a walk that prunes hidden directories
        // without descending does not have. `cp -al`, `rsync --link-dest` and
        // hard-linked package stores all produce that shape without an attacker.
        if entry.depth() > 0 && is_hidden_name(entry.file_name()) {
            stack.record_hidden(is_dir);
            if is_dir {
                it.skip_current_dir();
            }
            continue;
        }
        // The rule above judges the NAME the walk arrived by, which is the
        // whole rule when links are not followed. Under `--follow-symlinks` it
        // is not: a link is free to be called `notes.txt` and point at
        // `.secretdir/k.txt`, and walkdir then yields the target's contents
        // under the link's visible name. Both halves of the guarantee above
        // fail that way — the secret is indexed, and a link to a directory
        // outside the root drags in a tree the operator never pointed at
        // (`shared -> /etc` indexes `/etc/shadow` as `shared/shadow`). The
        // report made it worse by still saying the hidden directories were
        // pruned, which was true of the traversal and false of the outcome.
        //
        // So under the flag the same two questions are asked of the RESOLVED
        // path instead of the name. Loop-safety is walkdir's and unrelated;
        // this is the secret rule and the root boundary.
        // Resolving is only necessary once the walk has actually gone through a
        // link. `canonicalize` is `realpath(3)` and issues one `readlinkat` per
        // path component, so doing it for every entry costs 8-14 failing
        // syscalls per file on an ordinary tree and made the walk 4.2x slower
        // with 7.6x the syscalls — all of it wasted on paths where no link
        // exists. `link_depth` is the shallowest depth at or below which some
        // ancestor (or the entry itself) is a symlink; below that the walk path
        // and the real path cannot differ.
        if entry.path_is_symlink() {
            link_depth = Some(link_depth.map_or(entry.depth(), |d: usize| d.min(entry.depth())));
        } else if is_dir && link_depth.is_some_and(|d| entry.depth() <= d) {
            link_depth = None;
        }
        let through_link = entry.path_is_symlink() || link_depth.is_some_and(|d| entry.depth() > d);
        // The path every later stage should open. The guard resolves the entry
        // to decide on it; keeping that resolution is what stops the decision
        // being made about one file and the bytes being read from another.
        // `content.rs` hashes and `lib.rs` extracts from `FileEntry::path`
        // minutes after the walk on a large corpus, each a fresh `open()` that
        // re-follows the link — so a link re-pointed in between handed the
        // refused content straight into a document body. `rel` still comes from
        // the walk path: that is the name the operator pointed at and the
        // identity the index is keyed on.
        let mut resolved: Option<PathBuf> = None;
        if follow_symlinks && entry.depth() > 0 && through_link {
            match escaped_or_hidden_target(&root_canon, entry.path()) {
                Ok(real) => resolved = real,
                // Admitted by the waiver — but with the RESOLVED path, so the
                // bytes read later are the bytes that were judged. This arm was
                // empty, which stored the walk path and re-followed the link at
                // read time for everything the flag ingests.
                Err(SymlinkVerdict::OutsideRoot(real)) if allow_outside_root => {
                    resolved = Some(real);
                }
                Err(SymlinkVerdict::HiddenOutsideRoot) => {
                    stack.record_symlink_hidden_escape(is_dir);
                    if is_dir {
                        it.skip_current_dir();
                    }
                    continue;
                }
                Err(SymlinkVerdict::OutsideRoot(_)) => {
                    stack.record_symlink_escape(is_dir);
                    if is_dir {
                        it.skip_current_dir();
                    }
                    continue;
                }
                Err(SymlinkVerdict::HiddenTarget) => {
                    stack.record_hidden(is_dir);
                    if is_dir {
                        it.skip_current_dir();
                    }
                    continue;
                }
                Err(SymlinkVerdict::Unresolvable) => {
                    stack.record_symlink_unresolved(is_dir);
                    if is_dir {
                        it.skip_current_dir();
                    }
                    continue;
                }
            }
        }
        if is_dir {
            // The root is never judged by the rules below it: pointing
            // autoindex at a folder is an explicit instruction, and the note
            // explaining that it *would* have been ignored is on the report.
            if entry.depth() > 0 && stack.skip_dir(entry.path()).is_some() {
                it.skip_current_dir();
                continue;
            }
            // Marker-gated generated dirs (part of the built-in defaults, so
            // `--no-ignore` / `--no-default-ignores` disable it): names like
            // Unity's `Library/` or `Logs/` are too common for the blunt
            // DEFAULT_IGNORE_PATTERNS, so they are pruned only when a sibling
            // marker file proves the parent is that kind of project.
            if entry.depth() > 0 && marker_defaults {
                if let Some(label) = marker_generated_dir(entry.path()) {
                    stack.record_marker_dir(entry.path(), label);
                    it.skip_current_dir();
                    continue;
                }
            }
            stack.enter_dir(entry.path(), entry.depth());
            if let Some(dirs) = dirs_out.as_mut() {
                // The path the watcher must watch is the path the walk arrived
                // by — the same one `rel` is derived from — so a followed link
                // is watched where the operator pointed, not where it resolves.
                dirs.push(entry.path().to_path_buf());
            }
            continue;
        }
        // Watch-set mode wants directories only; a file's metadata is a syscall
        // per entry that nothing on this route reads.
        if dirs_out.is_some() {
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if stack.skip_file(entry.path()).is_some() {
            continue;
        }
        // Marker-gated Takeout noise (built-in defaults, so `--no-ignore` /
        // `--no-default-ignores` disable it). After the ignore files, and
        // yielding to them: a `!archive_browser.html` line in `.xerjignore`
        // is an explicit instruction and wins.
        if marker_defaults {
            if let Some(label) = takeout_noise(entry.path()) {
                if !stack.is_reincluded(entry.path(), false) {
                    stack.record_marker_file(label);
                    continue;
                }
            }
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let p = entry.path().to_path_buf();
        let rel = p
            .strip_prefix(&root_canon)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let rel_id = stable_path_id(p.strip_prefix(&root_canon).unwrap_or(&p));
        out.push(FileEntry {
            path: resolved.unwrap_or(p),
            rel,
            rel_id,
            is_symlink: entry
                .path()
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink()),
            size: md.len(),
        });
    }
    // Deterministic order for clustering / naming.
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((out, stack.report))
}

#[cfg(unix)]
fn stable_path_id(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("unix:");
    for byte in path.as_os_str().as_bytes() {
        use std::fmt::Write;
        write!(out, "{byte:02x}").expect("write string");
    }
    out
}

#[cfg(windows)]
fn stable_path_id(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    let mut out = String::from("windows:");
    for unit in path.as_os_str().encode_wide() {
        use std::fmt::Write;
        write!(out, "{unit:04x}").expect("write string");
    }
    out
}

#[cfg(not(any(unix, windows)))]
fn stable_path_id(path: &Path) -> String {
    format!("other:{}", path.to_string_lossy())
}

#[cfg(test)]
mod hidden_skip_tests {
    use super::{escaped_or_hidden_target, SymlinkVerdict};

    /// The dispatch this file was refuted for. `canonicalize` failing is not
    /// evidence that the entry is where it appears; only `NotFound` is.
    ///
    /// A path with an interior NUL cannot be canonicalised and reports
    /// `InvalidInput`, which stands in here for the errno that actually
    /// motivates the arm: `realpath(3)` is `PATH_MAX`-bounded while the kernel
    /// is not, so a canonical path over that limit gives `ENAMETOOLONG` here
    /// while `open` on the walk path still succeeds. That asymmetry is what
    /// made `.ok()?` a bypass — the guard was skipped for precisely the entries
    /// it could not vet.
    #[test]
    fn an_unresolvable_entry_is_refused_not_admitted() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");

        // Exists and resolves inside the root: admitted.
        std::fs::write(root.join("plain.txt"), "x").unwrap();
        assert!(
            matches!(
                escaped_or_hidden_target(&root, &root.join("plain.txt")),
                Ok(Some(_))
            ),
            "an ordinary in-root file must resolve and be governed by the ordinary rules"
        );

        // Does not exist: nothing to leak, so also admitted — walkdir reports it.
        assert!(
            matches!(
                escaped_or_hidden_target(&root, &root.join("gone.txt")),
                Ok(None)
            ),
            "a dangling link has no target and must not be reported as a refusal"
        );

        // Cannot be resolved for any OTHER reason: refused.
        let bad = root.join(OsStr::from_bytes(b"nul\0inside"));
        assert!(
            matches!(
                escaped_or_hidden_target(&root, &bad),
                Err(SymlinkVerdict::Unresolvable)
            ),
            "an unresolvable path must be refused — `could not compute` is not `yes`"
        );
    }

    use super::walk;
    use std::fs;

    #[test]
    fn hidden_files_and_dirs_are_never_indexed() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join(".env"), "SECRET=pat-abc123").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("config"), "[core]").unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src").join(".secret"), "nope").unwrap();

        let rels: Vec<String> = walk(root, false)
            .unwrap()
            .into_iter()
            .map(|e| e.rel)
            .collect();
        assert!(rels.contains(&"README.md".to_string()));
        assert!(rels.contains(&"src/main.rs".to_string()));
        // secrets and VCS internals must not be present
        assert!(!rels.iter().any(|r| r == ".env"), "indexed .env: {rels:?}");
        assert!(
            !rels.iter().any(|r| r.starts_with(".git")),
            "descended .git: {rels:?}"
        );
        assert!(
            !rels.iter().any(|r| r.ends_with(".secret")),
            "indexed a nested dotfile: {rels:?}"
        );
    }

    /// A name whose first byte is '.' but is not valid UTF-8 must still be
    /// treated as hidden. `file_name().to_str()` is `None` for that name, so
    /// the UTF-8 `starts_with('.')` check misses it.
    #[cfg(unix)]
    #[test]
    fn hidden_non_utf8_dot_name_is_detected() {
        use crate::ignore_rules::is_hidden_name;
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let hidden = OsStr::from_bytes(b".\x80");
        assert!(
            hidden.to_str().is_none(),
            "fixture must be non-UTF-8 so to_str() cannot save us"
        );
        assert!(
            is_hidden_name(hidden),
            "first byte is '.', even when to_str() is None"
        );
        assert!(is_hidden_name(OsStr::from_bytes(b".env")));
        assert!(!is_hidden_name(OsStr::from_bytes(b"README.md")));
        assert!(!is_hidden_name(OsStr::from_bytes(b"\x80env")));
    }

    /// Same name on disk. APFS/HFS reject non-UTF-8 names; Linux does not.
    #[cfg(target_os = "linux")]
    #[test]
    fn hidden_non_utf8_dotfile_is_never_indexed() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join(OsStr::from_bytes(b".\x80")), "SECRET=pat-abc123").unwrap();

        let files = walk(root, false).unwrap();
        let rels: Vec<String> = files.iter().map(|e| e.rel.clone()).collect();
        assert!(
            rels.contains(&"README.md".to_string()),
            "visible file must still be indexed: {rels:?}"
        );
        assert!(
            !files.iter().any(|e| {
                e.path
                    .file_name()
                    .is_some_and(|n| n.as_encoded_bytes().first() == Some(&b'.'))
            }),
            "indexed a hidden non-UTF-8 name: {rels:?}"
        );
    }

    /// #276: the walk itself must drop gitignored junk, not a later stage —
    /// an ignored directory is never descended, so it never reaches hashing.
    #[test]
    fn the_walk_honours_gitignore_and_the_built_in_defaults() {
        use super::walk_reporting;
        use crate::ignore_rules::IgnoreOptions;

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "*.tmp\ndocs/private/\n").unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::write(root.join("scratch.tmp"), "junk").unwrap();
        fs::create_dir_all(root.join("docs/private")).unwrap();
        fs::write(root.join("docs/private/salary.md"), "secret").unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/huge.rlib"), "bytes").unwrap();

        let (files, report) = walk_reporting(root, false, IgnoreOptions::default()).unwrap();
        let rels: Vec<String> = files.into_iter().map(|e| e.rel).collect();
        assert_eq!(rels, vec!["README.md".to_string()], "{rels:?}");
        // 2 = scratch.tmp, plus `.gitignore` itself under the (separate,
        // non-optional) hidden-file rule.
        assert_eq!(report.files_skipped, 2, "{report:?}");
        assert_eq!(report.dirs_pruned, 2, "{report:?}");
        assert_eq!(report.ignore_files_read, 1, "{report:?}");

        // --no-ignore brings all of it back, except the dotfiles.
        let (all, off) = walk_reporting(root, false, IgnoreOptions::off()).unwrap();
        let mut rels: Vec<String> = all.into_iter().map(|e| e.rel).collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "README.md".to_string(),
                "docs/private/salary.md".to_string(),
                "scratch.tmp".to_string(),
                "target/debug/huge.rlib".to_string(),
            ],
            "{rels:?}"
        );
        assert_eq!(off.dirs_pruned, 0, "{off:?}");
    }

    /// Create a real checkout at `path`. Returns false when the `git` binary
    /// is unavailable, in which case the caller still gets the on-disk shape
    /// that matters (`.git/HEAD`) but skips the parity cross-check.
    #[cfg(test)]
    fn git_init(path: &std::path::Path) -> bool {
        fs::create_dir_all(path).unwrap();
        let ok = std::process::Command::new("git")
            // A developer's global gitignore or an enclosing repo's config
            // must not decide what this test sees.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            fs::create_dir_all(path.join(".git")).unwrap();
            fs::write(path.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        }
        ok
    }

    /// Files a repository considers its own and does not ignore, as git itself
    /// reports them. Dotfiles are dropped because XERJ's separate, non-optional
    /// hidden-file rule removes them before any ignore rule is consulted.
    #[cfg(test)]
    fn git_untracked_visible_files(repo: &std::path::Path) -> Vec<String> {
        let out = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(repo)
            .output()
            .expect("git status");
        let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("?? "))
            .map(|p| p.trim_matches('"').to_string())
            .filter(|p| !p.split('/').any(|c| c.starts_with('.')))
            .collect();
        files.sort();
        files
    }

    /// #279 (follow-up to #276). An outer repository's `.gitignore` must not
    /// judge files inside a nested checkout.
    ///
    /// This is git's authority model, not a nicety: the repository that owns a
    /// file decides whether it is ignored. `git status` in the outer repo does
    /// not descend into a nested checkout at all, and `git status` inside the
    /// nested one never consults the outer's `.gitignore`. The first cut of
    /// this feature walked every layer with no boundary stop, so a root `*.md`
    /// hid the README of every vendored dependency below it.
    ///
    /// The fixture is a real nested checkout — the boundary is a `.git` on
    /// disk, so a matcher-level unit test could not have caught this.
    #[test]
    fn an_outer_repository_does_not_judge_a_nested_checkout() {
        use super::walk_reporting;
        use crate::ignore_rules::IgnoreOptions;

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("outer");
        let have_git = git_init(&root);
        fs::write(root.join(".gitignore"), "*.md\n").unwrap();
        fs::write(root.join("OUTER.md"), "outer, and outer's to ignore").unwrap();
        fs::write(root.join("keep.txt"), "x").unwrap();

        let inner = root.join("vendored");
        git_init(&inner);
        fs::write(inner.join(".gitignore"), "local-junk.txt\n").unwrap();
        fs::write(inner.join("README.md"), "the vendored project's own README").unwrap();
        fs::write(inner.join("local-junk.txt"), "inner's own rule drops this").unwrap();
        fs::create_dir_all(inner.join("src")).unwrap();
        fs::write(inner.join("src").join("guide.md"), "also the inner repo's").unwrap();

        let (files, _) = walk_reporting(&root, false, IgnoreOptions::default()).unwrap();
        let mut rels: Vec<String> = files.into_iter().map(|e| e.rel).collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "keep.txt".to_string(),
                "vendored/README.md".to_string(),
                "vendored/src/guide.md".to_string(),
            ],
            "the outer *.md must stop at the nested checkout, and the inner \
             repo's own rule must still apply inside it: {rels:?}"
        );

        // Parity with the tool whose semantics we claim to reproduce: the set
        // XERJ keeps under `vendored/` is the set the nested repository itself
        // reports as its own, unignored, visible files.
        if have_git {
            let from_git = git_untracked_visible_files(&inner);
            assert_eq!(
                from_git,
                vec!["README.md".to_string(), "src/guide.md".to_string()],
                "fixture drifted from git's own answer"
            );
            let from_xerj: Vec<String> = rels
                .iter()
                .filter_map(|r| r.strip_prefix("vendored/").map(str::to_string))
                .collect();
            assert_eq!(from_xerj, from_git, "XERJ and git must agree");
        }
    }

    /// The other half of the boundary: `.xerjignore` is XERJ's own file, not
    /// git's, and its stated job is to govern the folder you pointed at. It
    /// deliberately keeps its authority across a nested checkout, so one file
    /// at the root can say "not this, anywhere below here".
    #[test]
    fn a_root_xerjignore_still_reaches_into_a_nested_checkout() {
        use super::walk_reporting;
        use crate::ignore_rules::IgnoreOptions;

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("outer");
        git_init(&root);
        fs::write(root.join(".xerjignore"), "*.md\n").unwrap();
        fs::write(root.join("keep.txt"), "x").unwrap();

        let inner = root.join("vendored");
        git_init(&inner);
        fs::write(inner.join("README.md"), "x").unwrap();
        fs::write(inner.join("code.rs"), "x").unwrap();

        let (files, _) = walk_reporting(&root, false, IgnoreOptions::default()).unwrap();
        let mut rels: Vec<String> = files.into_iter().map(|e| e.rel).collect();
        rels.sort();
        assert_eq!(
            rels,
            vec!["keep.txt".to_string(), "vendored/code.rs".to_string()],
            "{rels:?}"
        );
    }

    #[test]
    fn a_brain_over_a_dot_named_root_still_works() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join(".notes");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.md"), "x").unwrap();
        let rels: Vec<String> = walk(&root, false)
            .unwrap()
            .into_iter()
            .map(|e| e.rel)
            .collect();
        assert_eq!(
            rels,
            vec!["a.md".to_string()],
            "root exemption failed: {rels:?}"
        );
    }
}

#[cfg(test)]
mod marker_generated_dir_tests {
    use super::{walk, walk_reporting};
    use crate::ignore_rules::IgnoreOptions;
    use std::fs;
    use std::path::Path;

    fn make_unity_project(root: &Path, with_marker: bool) {
        fs::create_dir_all(root.join("Assets")).unwrap();
        fs::write(root.join("Assets/Player.cs"), "class Player {}").unwrap();
        fs::create_dir_all(root.join("Library/Artifacts")).unwrap();
        fs::write(root.join("Library/Artifacts/blob.bin"), b"\x00\x01").unwrap();
        fs::create_dir_all(root.join("Logs")).unwrap();
        fs::write(root.join("Logs/editor.log"), "x").unwrap();
        if with_marker {
            fs::create_dir_all(root.join("ProjectSettings")).unwrap();
            fs::write(
                root.join("ProjectSettings/ProjectVersion.txt"),
                "m_EditorVersion: 2022.3.10f1\n",
            )
            .unwrap();
        }
    }

    /// Monorepo case: the Unity project is a SUBFOLDER of the walk root; the
    /// sibling marker decides at any depth.
    #[test]
    fn a_nested_unity_project_is_pruned_and_reported() {
        let dir = tempfile::TempDir::new().unwrap();
        let unity = dir.path().join("platform/unity");
        fs::create_dir_all(&unity).unwrap();
        make_unity_project(&unity, true);
        let (files, report) = walk_reporting(dir.path(), false, IgnoreOptions::default()).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"platform/unity/Assets/Player.cs"));
        assert!(
            !rels.iter().any(|r| r.contains("/Library/")),
            "Library/ must be pruned whole: {rels:?}"
        );
        assert!(
            report
                .by_rule
                .keys()
                .any(|k| k == "default:unity-generated"),
            "the prune must be named on the report: {report:?}"
        );
    }

    /// Without the marker, a folder merely NAMED Library or Logs is data.
    #[test]
    fn without_the_marker_nothing_is_pruned() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_project(dir.path(), false);
        let files = walk(dir.path(), false).unwrap();
        let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"Library/Artifacts/blob.bin"), "{rels:?}");
        assert!(rels.contains(&"Logs/editor.log"), "{rels:?}");
    }

    /// The marker rule is one of the built-in defaults, so both switches
    /// disable it.
    #[test]
    fn no_ignore_and_no_default_ignores_both_disable_the_marker_rule() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_project(dir.path(), true);
        let no_defaults = IgnoreOptions {
            defaults: false,
            ..IgnoreOptions::default()
        };
        for opts in [IgnoreOptions::off(), no_defaults] {
            let (files, _) = walk_reporting(dir.path(), false, opts).unwrap();
            let rels: Vec<&str> = files.iter().map(|e| e.rel.as_str()).collect();
            assert!(
                rels.contains(&"Library/Artifacts/blob.bin"),
                "opts {opts:?} must walk generated dirs: {rels:?}"
            );
        }
    }

    #[test]
    fn a_file_named_like_a_generated_dir_is_not_pruned() {
        let dir = tempfile::TempDir::new().unwrap();
        make_unity_project(dir.path(), true);
        fs::write(dir.path().join("obj"), "a FILE named obj").unwrap();
        let files = walk(dir.path(), false).unwrap();
        assert!(
            files.iter().any(|e| e.rel == "obj"),
            "the prune applies to directories only"
        );
    }
}

#[cfg(test)]
mod takeout_noise_tests {
    use super::{walk_reporting, TAKEOUT_INDEX_RULE, TAKEOUT_KEEP_TWIN_RULE};
    use crate::ignore_rules::IgnoreOptions;
    use std::fs;
    use std::path::Path;

    const KEEP_NOTE: &str = r#"{"isTrashed":false,"isPinned":false,"isArchived":false,
        "textContent":"call Dana","title":"todo",
        "userEditedTimestampUsec":1700000000000000,"createdTimestampUsec":1700000000000000}"#;

    /// `Takeout/` with mail, one Keep note (exported twice, as Takeout does),
    /// and an unrelated html/json pair that must survive.
    fn make_takeout(root: &Path, with_marker: bool) {
        let t = root.join("Takeout");
        fs::create_dir_all(t.join("Mail")).unwrap();
        fs::create_dir_all(t.join("Keep")).unwrap();
        fs::create_dir_all(t.join("Drive")).unwrap();
        if with_marker {
            fs::write(t.join("archive_browser.html"), "<html>index</html>").unwrap();
        }
        fs::write(t.join("Mail/All mail Including Spam and Trash.mbox"), "x").unwrap();
        fs::write(t.join("Keep/todo.json"), KEEP_NOTE).unwrap();
        fs::write(t.join("Keep/todo.html"), "<html>call Dana</html>").unwrap();
        // A note whose name is not ASCII: the twin lookup is path-based and
        // must never slice a name by bytes.
        fs::write(t.join("Keep/設計書 مرحبا.json"), KEEP_NOTE).unwrap();
        fs::write(t.join("Keep/設計書 مرحبا.html"), "<html>x</html>").unwrap();
        // Not a Keep note: an html beside a json that is ordinary data.
        fs::write(t.join("Drive/report.json"), r#"{"rows":[1,2,3]}"#).unwrap();
        fs::write(t.join("Drive/report.html"), "<html>report</html>").unwrap();
        // An html with no json twin at all.
        fs::write(t.join("Keep/orphan.html"), "<html>orphan</html>").unwrap();
    }

    fn rels(root: &Path, opts: IgnoreOptions) -> (Vec<String>, crate::ignore_rules::IgnoreReport) {
        let (files, report) = walk_reporting(root, false, opts).unwrap();
        (files.into_iter().map(|e| e.rel).collect(), report)
    }

    #[test]
    fn the_export_index_and_keep_html_twins_are_skipped_and_named() {
        let dir = tempfile::TempDir::new().unwrap();
        make_takeout(dir.path(), true);
        let (rels, report) = rels(dir.path(), IgnoreOptions::default());
        assert!(
            !rels.iter().any(|r| r.ends_with("archive_browser.html")),
            "{rels:?}"
        );
        assert!(
            !rels.contains(&"Takeout/Keep/todo.html".to_string()),
            "{rels:?}"
        );
        assert!(
            !rels.contains(&"Takeout/Keep/設計書 مرحبا.html".to_string()),
            "{rels:?}"
        );
        // The data itself is all still there.
        for kept in [
            "Takeout/Mail/All mail Including Spam and Trash.mbox",
            "Takeout/Keep/todo.json",
            "Takeout/Keep/設計書 مرحبا.json",
            "Takeout/Keep/orphan.html",
            "Takeout/Drive/report.html",
            "Takeout/Drive/report.json",
        ] {
            assert!(
                rels.contains(&kept.to_string()),
                "{kept} missing from {rels:?}"
            );
        }
        // Never silent: each rule is on the report with its count.
        assert_eq!(report.by_rule[TAKEOUT_INDEX_RULE].files, 1, "{report:?}");
        assert_eq!(
            report.by_rule[TAKEOUT_KEEP_TWIN_RULE].files, 2,
            "{report:?}"
        );
        assert_eq!(report.files_skipped, 3);
    }

    /// Pointing autoindex INSIDE the export (at `Takeout/` itself) is the same
    /// export: the marker is found in the root, not in a folder named Takeout.
    #[test]
    fn the_takeout_folder_itself_can_be_the_root() {
        let dir = tempfile::TempDir::new().unwrap();
        make_takeout(dir.path(), true);
        let (rels, report) = rels(&dir.path().join("Takeout"), IgnoreOptions::default());
        assert!(
            !rels.contains(&"archive_browser.html".to_string()),
            "{rels:?}"
        );
        assert!(!rels.contains(&"Keep/todo.html".to_string()), "{rels:?}");
        assert!(rels.contains(&"Keep/todo.json".to_string()), "{rels:?}");
        assert_eq!(report.by_rule[TAKEOUT_KEEP_TWIN_RULE].files, 2);
    }

    /// No marker, no Takeout: a folder that merely has a `Keep/` with paired
    /// files keeps every one of them.
    #[test]
    fn without_the_marker_nothing_is_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        make_takeout(dir.path(), false);
        let (rels, report) = rels(dir.path(), IgnoreOptions::default());
        assert!(
            rels.contains(&"Takeout/Keep/todo.html".to_string()),
            "{rels:?}"
        );
        assert_eq!(report.files_skipped, 0, "{report:?}");
    }

    /// An `archive_browser.html` deeper than a product folder does not make
    /// its grand-children Takeout products: only `<root>/<product>/X.html`.
    #[test]
    fn the_twin_rule_reaches_one_level_below_the_marker_only() {
        let dir = tempfile::TempDir::new().unwrap();
        make_takeout(dir.path(), true);
        let deep = dir.path().join("Takeout/Keep/sub");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("n.json"), KEEP_NOTE).unwrap();
        fs::write(deep.join("n.html"), "<html>n</html>").unwrap();
        let (rels, _) = rels(dir.path(), IgnoreOptions::default());
        assert!(
            rels.contains(&"Takeout/Keep/sub/n.html".to_string()),
            "{rels:?}"
        );
    }

    #[test]
    fn both_switches_disable_it_and_an_ignore_file_can_reinclude() {
        let dir = tempfile::TempDir::new().unwrap();
        make_takeout(dir.path(), true);
        let no_defaults = IgnoreOptions {
            defaults: false,
            ..IgnoreOptions::default()
        };
        for opts in [IgnoreOptions::off(), no_defaults] {
            let (rels, _) = rels(dir.path(), opts);
            assert!(
                rels.contains(&"Takeout/archive_browser.html".to_string()),
                "{opts:?}"
            );
            assert!(
                rels.contains(&"Takeout/Keep/todo.html".to_string()),
                "{opts:?}"
            );
        }
        // `!name` in .xerjignore outranks the built-in rule.
        fs::write(dir.path().join(".xerjignore"), "!archive_browser.html\n").unwrap();
        let (rels, report) = rels(dir.path(), IgnoreOptions::default());
        assert!(
            rels.contains(&"Takeout/archive_browser.html".to_string()),
            "{rels:?}"
        );
        assert!(
            !report.by_rule.contains_key(TAKEOUT_INDEX_RULE),
            "{report:?}"
        );
        assert!(
            !rels.contains(&"Takeout/Keep/todo.html".to_string()),
            "twins still skipped"
        );
    }

    /// A json twin that is broken, huge, or a directory is not a Keep note, and
    /// doubt means INDEX the html. None of these may panic (panic = abort).
    #[test]
    fn a_twin_that_is_not_a_readable_keep_note_skips_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        make_takeout(dir.path(), true);
        let keep = dir.path().join("Takeout/Keep");
        fs::write(keep.join("broken.json"), b"{\"isTrashed\": \xff\xfe").unwrap();
        fs::write(keep.join("broken.html"), "<html>b</html>").unwrap();
        fs::create_dir_all(keep.join("isdir.json")).unwrap();
        fs::write(keep.join("isdir.html"), "<html>d</html>").unwrap();
        let mut big = String::from(KEEP_NOTE.trim_end_matches('}'));
        big.push_str(&format!(",\"pad\":\"{}\"}}", "x".repeat((1 << 20) + 16)));
        fs::write(keep.join("big.json"), big).unwrap();
        fs::write(keep.join("big.html"), "<html>big</html>").unwrap();
        let (rels, _) = rels(dir.path(), IgnoreOptions::default());
        for kept in ["broken.html", "isdir.html", "big.html"] {
            assert!(
                rels.contains(&format!("Takeout/Keep/{kept}")),
                "{kept}: {rels:?}"
            );
        }
    }
}
