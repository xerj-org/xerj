//! macOS File Provider placeholders (iCloud Drive and other cloud volumes).
//!
//! A dataless file has a directory entry and a logical size, but its bytes are
//! not on this machine. `stat` reports that without downloading anything.
//! `open` / `read` does download, and the call blocks until the transfer
//! finishes — which is how `xerj autoindex` hashing stalled with no path named
//! (#2058, field reports #2042 and #2051).
//!
//! The flag is Apple's `SF_DATALESS` (`0x40000000` in `bsd/sys/stat.h`, "file
//! is dataless object"). It is synthetic and read-only, so a test cannot set
//! it with `chflags`. libc 0.2.191 exposes `stat.st_flags` on macOS and does
//! not export the constant; the value below is the header's. Detection is
//! macOS-only. On every other target [`split`] returns the input unchanged and
//! does not stat.
//!
//! Linux compiles the accounting so the unit tests run in CI. Production on
//! that target calls [`Split::unchanged`] and does not stat; the rest of this
//! module is macOS-only and is allowed to look unused there.

#![cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]

use crate::state::Plan;
use crate::walk::FileEntry;
use std::collections::HashMap;
use std::path::Path;

/// Apple `SF_DATALESS` (`bsd/sys/stat.h`).
pub(crate) const SF_DATALESS: u32 = 0x4000_0000;

/// Skip reason recorded in the run summary. Stable for anything that counts
/// skips by name.
pub(crate) const SKIP_REASON: &str = "dataless-placeholder";

/// `st_flags` has `SF_DATALESS` set. Other flags do not matter: a placeholder
/// can also be hidden or compressed.
pub(crate) fn flags_indicate_dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

/// Human line for files left out of this run. `None` when nothing was skipped,
/// so a run with no placeholders prints the lines it always printed.
pub(crate) fn skip_note(count: u64) -> Option<String> {
    if count == 0 {
        return None;
    }
    let files = if count == 1 { "file" } else { "files" };
    Some(format!(
        "skipped {count} {files} — {SKIP_REASON} (macOS File Provider / iCloud placeholder; \
         the bytes are not local, and reading them blocks on a download, so they were not \
         hashed or indexed). Materialize them and re-run to include them"
    ))
}

/// Human line for placeholders this run did not read because a previous pass
/// already hashed them. They stay in the inventory under that digest, so an
/// incremental re-run does not treat an evicted file as deleted.
pub(crate) fn carry_note(count: u64) -> Option<String> {
    if count == 0 {
        return None;
    }
    let files = if count == 1 { "file" } else { "files" };
    Some(format!(
        "left {count} dataless placeholder {files} on their previous content \
         ({SKIP_REASON}); not re-read, so the indexed copy stays"
    ))
}

/// What hashing should do with one walked tree.
pub(crate) struct Split {
    /// Files to resolve. Dataless files with no prior digest are absent.
    pub files: Vec<FileEntry>,
    /// `path_id` → digest for dataless files that must not be opened.
    pub carry: HashMap<String, String>,
    /// Dataless files removed from [`Self::files`].
    pub skipped_files: Vec<FileEntry>,
    /// Files [`Self::files`] that will actually be read. Carried placeholders
    /// are not included. The hash phase recomputes this after the budget
    /// filter, from the files still admitted, so production does not read
    /// these fields — the unit tests do.
    #[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
    pub hash_files: u64,
    #[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
    pub hash_bytes: u64,
}

impl Split {
    /// No stat, no filtering. The non-macOS result.
    ///
    /// macOS goes through `split_with` instead, so this helper is unused there.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(crate) fn unchanged(files: Vec<FileEntry>) -> Self {
        let hash_files = files.len() as u64;
        let hash_bytes = files.iter().map(|file| file.size).sum();
        Self {
            files,
            carry: HashMap::new(),
            skipped_files: Vec::new(),
            hash_files,
            hash_bytes,
        }
    }
}

/// Split `files` into "hash", "carry the previous digest", and "skip".
///
/// On macOS a file whose `st_flags` include `SF_DATALESS` is not opened. If
/// `prior` has a digest for its `path_id`, that digest is carried and the file
/// stays in the inventory (an evicted file is not a deletion). Otherwise the
/// file is skipped and counted. A failed `stat` is not dataless: the later
/// open reports the real error instead of hiding a readable file.
///
/// On every other target this is [`Split::unchanged`].
pub(crate) fn split(files: Vec<FileEntry>, prior: &HashMap<String, String>) -> Split {
    #[cfg(target_os = "macos")]
    {
        split_with(files, prior, is_dataless_placeholder)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prior;
        Split::unchanged(files)
    }
}

/// [`split`] with the placeholder test injected. The production macOS path
/// passes [`is_dataless_placeholder`]; tests pass a predicate so the accounting
/// does not need a real iCloud file.
fn split_with(
    files: Vec<FileEntry>,
    prior: &HashMap<String, String>,
    is_dataless: impl Fn(&Path) -> bool,
) -> Split {
    let mut kept = Vec::with_capacity(files.len());
    let mut carry = HashMap::new();
    let mut skipped_files = Vec::new();
    let mut hash_files = 0u64;
    let mut hash_bytes = 0u64;
    for file in files {
        if is_dataless(&file.path) {
            if let Some(digest) = prior.get(&file.rel_id) {
                carry.insert(file.rel_id.clone(), digest.clone());
                kept.push(file);
            } else {
                skipped_files.push(file);
            }
        } else {
            hash_files += 1;
            hash_bytes += file.size;
            kept.push(file);
        }
    }
    Split {
        files: kept,
        carry,
        skipped_files,
        hash_files,
        hash_bytes,
    }
}

/// path_id → full-content digest for every file the previous plan actually
/// hashed. Empty path ids and assignments with no digest are omitted: there
/// is nothing safe to carry, and a placeholder in that state is skipped.
pub(crate) fn prior_digests(plan: Option<&Plan>) -> HashMap<String, String> {
    let Some(plan) = plan else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for assignment in plan.files.values() {
        remember_digest(
            &mut out,
            &assignment.path_id,
            assignment.content_digest.as_deref(),
        );
    }
    for duplicate in &plan.duplicate_files {
        let digest = plan
            .files
            .get(&duplicate.file_key)
            .and_then(|assignment| assignment.content_digest.as_deref());
        remember_digest(&mut out, &duplicate.path_id, digest);
    }
    out
}

fn remember_digest(out: &mut HashMap<String, String>, path_id: &str, digest: Option<&str>) {
    if path_id.is_empty() {
        return;
    }
    let Some(digest) = digest.filter(|digest| !digest.is_empty()) else {
        return;
    };
    out.entry(path_id.to_string())
        .or_insert_with(|| digest.to_string());
}

/// Register `file` on the progress in-flight list for the duration of the
/// guard, on macOS only.
///
/// Hashing reads the whole file in one worker. A placeholder the pre-pass
/// missed (it became dataless between `stat` and `open`, or `stat` failed and
/// the open blocks) otherwise stalls with `in_flight` empty, so the progress
/// line cannot name a path. The guard does not credit bytes — the completion
/// callback still does that, once, when the read returns.
///
/// Other targets return `None` and do not touch progress. A hash phase there
/// keeps the line it had before #2058.
pub(crate) fn watch_blocking_hash<'a>(
    progress: Option<&'a crate::progress::Progress>,
    file: &FileEntry,
) -> Option<crate::progress::WatchGuard<'a>> {
    #[cfg(target_os = "macos")]
    {
        progress.map(|progress| progress.watch_file(&file.rel, file.size))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (progress, file);
        None
    }
}

#[cfg(target_os = "macos")]
fn is_dataless_placeholder(path: &Path) -> bool {
    flags_of(path).is_some_and(flags_indicate_dataless)
}

#[cfg(target_os = "macos")]
fn flags_of(path: &Path) -> Option<u32> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // `stat` is a C string. An interior NUL cannot be passed through; treat
    // that as "not proven dataless" and let the open fail on its own.
    if bytes.contains(&0) {
        return None;
    }
    let mut c_path = Vec::with_capacity(bytes.len() + 1);
    c_path.extend_from_slice(bytes);
    c_path.push(0);
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `c_path` is a NUL-terminated path this process may stat, and
    // `st` is written by `stat` on success before it is read.
    let rc = unsafe { libc::stat(c_path.as_ptr().cast(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let st = unsafe { st.assume_init() };
    Some(st.st_flags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DuplicateFile, FileAssignment};
    use std::fs;

    fn assignment(path_id: &str, digest: Option<&str>) -> FileAssignment {
        FileAssignment {
            rel: path_id.to_string(),
            path_id: path_id.to_string(),
            is_symlink: Some(false),
            family: "txt".into(),
            gzip: false,
            content_digest: digest.map(str::to_string),
            assignments: Vec::new(),
            as_document: false,
        }
    }

    fn entry(rel_id: &str, size: u64) -> FileEntry {
        FileEntry {
            path: Path::new(rel_id).to_path_buf(),
            rel: rel_id.to_string(),
            rel_id: rel_id.to_string(),
            is_symlink: false,
            size,
        }
    }

    #[test]
    fn sf_dataless_is_the_apple_bit_and_other_flags_are_not() {
        assert!(flags_indicate_dataless(SF_DATALESS));
        assert!(flags_indicate_dataless(SF_DATALESS | 0x0002_0000));
        assert!(!flags_indicate_dataless(0));
        // UF_HIDDEN, SF_IMMUTABLE, and the other synthetic high bit.
        assert!(!flags_indicate_dataless(0x0000_8000));
        assert!(!flags_indicate_dataless(0x0002_0000));
        assert!(!flags_indicate_dataless(0x8000_0000));
    }

    #[test]
    fn skip_and_carry_notes_name_the_reason_and_the_count() {
        assert!(skip_note(0).is_none());
        let one = skip_note(1).unwrap();
        assert!(
            one.contains("skipped 1 file — dataless-placeholder"),
            "{one}"
        );
        assert!(!one.contains("files"), "{one}");
        let two = skip_note(2).unwrap();
        assert!(
            two.contains("skipped 2 files — dataless-placeholder"),
            "{two}"
        );
        assert!(carry_note(0).is_none());
        let carried = carry_note(1).unwrap();
        assert!(
            carried.contains("1 dataless placeholder file") && carried.contains(SKIP_REASON),
            "{carried}"
        );
    }

    #[test]
    fn a_placeholder_with_no_prior_digest_is_skipped_and_counted() {
        let prior = HashMap::from([("kept-cloud".to_string(), "axf2-prior".to_string())]);
        let split = split_with(
            vec![
                entry("local", 10),
                entry("kept-cloud", 20),
                entry("new-cloud", 30),
            ],
            &prior,
            |path| path.to_str().is_some_and(|path| path.ends_with("cloud")),
        );
        let rels: Vec<&str> = split.files.iter().map(|file| file.rel.as_str()).collect();
        assert_eq!(rels, ["local", "kept-cloud"]);
        assert_eq!(
            split.carry.get("kept-cloud").map(String::as_str),
            Some("axf2-prior")
        );
        assert!(!split.carry.contains_key("local"));
        assert_eq!(split.skipped_files.len(), 1);
        assert_eq!(split.skipped_files[0].rel, "new-cloud");
        // The hash denominator is the file that will be read, not the one
        // whose digest is carried and not the one that was skipped.
        assert_eq!(split.hash_files, 1);
        assert_eq!(split.hash_bytes, 10);
    }

    #[test]
    fn a_non_placeholder_tree_is_unchanged() {
        let files = vec![entry("a", 4), entry("b", 6)];
        let split = split_with(files, &HashMap::new(), |_| false);
        assert!(split.carry.is_empty());
        assert!(split.skipped_files.is_empty());
        assert_eq!(split.hash_files, 2);
        assert_eq!(split.hash_bytes, 10);
        assert_eq!(split.files.len(), 2);
    }

    #[test]
    fn prior_digests_keep_hashed_paths_and_alias_targets() {
        let mut plan = Plan::default();
        plan.files
            .insert("axf2-body".into(), assignment("unix:61", Some("axf2-body")));
        plan.files
            .insert("legacy".into(), assignment("unix:62", None));
        plan.files
            .insert("blank".into(), assignment("", Some("axf2-blank")));
        plan.duplicate_files.push(DuplicateFile {
            file_key: "axf2-body".into(),
            rel: "alias".into(),
            path_id: "unix:63".into(),
            is_symlink: Some(false),
            duplicate_of: "body".into(),
            bytes: 4,
        });
        let prior = prior_digests(Some(&plan));
        assert_eq!(prior.get("unix:61").map(String::as_str), Some("axf2-body"));
        assert_eq!(prior.get("unix:63").map(String::as_str), Some("axf2-body"));
        assert!(!prior.contains_key("unix:62"), "no digest to carry");
        assert!(!prior.contains_key(""), "an empty path id is not a key");
        assert!(prior_digests(None).is_empty());
    }

    /// A real local file is not a placeholder. On macOS this stats it; elsewhere
    /// [`split`] does not stat and still keeps the file.
    #[test]
    fn a_local_file_is_not_a_dataless_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"hello").unwrap();
        let file = FileEntry {
            path,
            rel: "a.txt".into(),
            rel_id: "unix:61".into(),
            is_symlink: false,
            size: 5,
        };
        let split = split(vec![file], &HashMap::new());
        assert!(split.skipped_files.is_empty(), "{:?}", split.skipped_files);
        assert!(split.carry.is_empty());
        assert_eq!(split.files.len(), 1);
        assert_eq!(split.hash_files, 1);
        assert_eq!(split.hash_bytes, 5);
    }
}
