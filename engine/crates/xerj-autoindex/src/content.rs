//! Full-content identity, duplicate grouping, and mutation detection.

use crate::state::DuplicateFile;
use crate::walk::FileEntry;
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;

/// Why a walked file could not be opened for the hash (#2352).
///
/// `Absent` is cross-platform (`ErrorKind::NotFound`). `Locked` is the Windows
/// sharing violation (`ERROR_SHARING_VIOLATION` 32 and `ERROR_LOCK_VIOLATION`
/// 33). Unix errno 32 is `EPIPE`, so the raw code is only a lock on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFailureKind {
    Absent,
    Locked,
}

/// A file the walk listed and the hash could not open (#2352).
///
/// It is not a member of [`Inventory::files`]: there is no content digest, so
/// it must not become a content group. The run records one `skipped` catalog
/// row and continues.
#[derive(Debug, Clone)]
pub struct UnreadableSource {
    pub file: FileEntry,
    pub detail: String,
    pub kind: ReadFailureKind,
}

pub struct Inventory {
    pub files: Vec<FileEntry>,
    /// Durable document identity. New plans use the full-content key.
    pub keys: Vec<String>,
    /// Full digest used to detect mutations even when a legacy key is retained.
    pub digests: Vec<String>,
    pub duplicates: Vec<DuplicateFile>,
    /// Walked files that disappeared or were locked before the hash could read
    /// them (#2352). Ordered by relative path.
    pub unreadable: Vec<UnreadableSource>,
}

/// Classify an open/read failure that should skip one file instead of aborting.
///
/// Anything else (permission denied without a Windows lock code, `EIO`, a
/// directory) stays fatal. `ErrorKind::PermissionDenied` alone is not a lock:
/// Rust has moved the sharing-violation kind between releases, and the raw
/// Win32 codes are the stable contract.
pub fn io_read_failure_kind(error: &std::io::Error) -> Option<ReadFailureKind> {
    if error.kind() == std::io::ErrorKind::NotFound {
        return Some(ReadFailureKind::Absent);
    }
    #[cfg(windows)]
    {
        match error.raw_os_error() {
            Some(32 | 33) => return Some(ReadFailureKind::Locked),
            _ => {}
        }
    }
    None
}

/// [`io_read_failure_kind`] over an `anyhow` chain (`with_context` hides the
/// `io::Error` one level down).
pub fn read_failure_kind(error: &anyhow::Error) -> Option<ReadFailureKind> {
    for cause in error.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if let Some(kind) = io_read_failure_kind(io) {
                return Some(kind);
            }
        }
    }
    None
}

/// Catalog `reason` for one skipped unreadable file. Must not contain the
/// stale-resume marker (`not in the frozen resume plan`).
pub fn unreadable_skip_reason(detail: &str, kind: ReadFailureKind) -> String {
    match kind {
        ReadFailureKind::Absent => format!(
            "no longer exists (disappeared after it was enumerated: {detail}); skipped and \
             nothing from it was indexed. Rerun to index it if it comes back"
        ),
        ReadFailureKind::Locked => format!(
            "could not be opened because another process holds it locked ({detail}); skipped \
             and nothing from it was indexed. Rerun to index it once it can be opened"
        ),
    }
}

/// Plan key for a file that never produced a content digest.
///
/// `axf2-` keys are content hashes. This prefix cannot collide with one, and
/// it is stable for a path so the same skip is not a new catalog row on the
/// next run. It is not in `inventory.keys`, so the stale-junk sweep must be
/// told to keep it for this run.
pub fn unreadable_file_key(rel_id: &str) -> String {
    format!(
        "axabsent-{:032x}",
        xxhash_rust::xxh3::xxh3_128(rel_id.as_bytes())
    )
}

/// Sniff/read failures that are the same skip as the hash (#2352).
///
/// Other sniff errors stay `junk` with the caller's existing reason.
pub fn unreadable_junk_fields(error: &anyhow::Error) -> Option<(&'static str, String)> {
    read_failure_kind(error).map(|kind| {
        (
            "skipped",
            unreadable_skip_reason(&format!("{error:#}"), kind),
        )
    })
}

/// Test-only witness: which pool the digest phase actually ran on. Phase A is
/// where `--workers` was silently disconnected (#240), and the pool a task
/// lands on is only observable from inside the task.
#[cfg(test)]
pub(crate) static OBSERVED_DIGEST_POOL_THREADS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Set when any digest ran on a thread that is not one of the scan pool's.
#[cfg(test)]
pub(crate) static DIGEST_RAN_OFF_THE_SCAN_POOL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Hash each file once, group equal digests, then byte-verify only digest peers.
///
/// This is linear in corpus bytes for adversarial common-prefix inputs rather
/// than pairwise O(n² × file_size).
///
/// `hashed` is called with each file's byte count as it completes.
/// Hashing reads the whole corpus, so on a large tree this is minutes of work
/// that reported nothing before #241. The callback runs on the scan pool's
/// workers and must therefore be `Sync`; it does no I/O of its own — it only
/// bumps the progress counters that the ticker thread reads.
pub fn resolve_reporting(
    files: Vec<FileEntry>,
    hashed: &(dyn Fn(u64) + Sync),
) -> Result<Inventory> {
    resolve_reporting_carried(files, &|_| None, &|_, _, _| {}, hashed, None)
}

/// As [`resolve_reporting`], with a caller-supplied digest cache.
///
/// `carried` is asked, per file, for a digest the caller can PROVE is still the
/// digest of the current bytes; when it answers, the file is not read. Only
/// `--watch` answers it (see [`crate::watch`]), and only for files the OS told
/// it did not change since it hashed them in this same process — a metadata
/// shortcut on its own is explicitly not good enough for this decision, which is
/// why the plain route above hashes everything on every run.
///
/// `observed` is then called for every file with its final digest and whether
/// that digest came from a fresh read, which is how the watcher's cache gets
/// rebuilt after each pass. Both run on the scan pool's workers, hence `Sync`.
///
/// Safety of the shortcut, stated where it is implemented: a carried digest that
/// is WRONG cannot corrupt the index silently. The incremental projection in
/// [`crate::reconcile_plan`] compares the digest against the committed plan and
/// carries an assignment forward only on a match, so a stale carried digest
/// looks like "unchanged" — the failure mode is a missed update, not a bad
/// document — and the next event or fingerprint change repairs it.
///
/// `blocking`, when set, names each file on the progress line for the duration
/// of its read. macOS passes the run's progress handle so a read that blocks
/// on a dataless placeholder says which path (#2058). The guard credits
/// nothing; `hashed` still does, once, on completion. Other targets ignore the
/// handle even if it is passed.
pub fn resolve_reporting_carried(
    files: Vec<FileEntry>,
    carried: &(dyn Fn(&FileEntry) -> Option<String> + Sync),
    observed: &(dyn Fn(&FileEntry, &str, bool) + Sync),
    hashed: &(dyn Fn(u64) + Sync),
    blocking: Option<&crate::progress::Progress>,
) -> Result<Inventory> {
    // Phase A belongs to the run's scan pool, not to rayon's global pool:
    // `--workers` has to bound the CPU-bound phase to mean anything (#240 §2).
    // The progress callback therefore fires from scan-pool threads — the ones
    // actually doing the hashing — rather than from rayon's global pool (#241).
    // A file that vanished or is locked after the walk is one skip, not a
    // reason to abort the corpus (#2352). Any other open/read error still
    // fails the whole resolve, as before.
    enum HashOutcome {
        Ready(String),
        Unreadable {
            detail: String,
            kind: ReadFailureKind,
        },
        Fatal(anyhow::Error),
    }
    let outcomes: Vec<HashOutcome> = crate::pool::install(|| {
        files
            .par_iter()
            .map(|file| {
                #[cfg(test)]
                {
                    OBSERVED_DIGEST_POOL_THREADS.fetch_max(
                        rayon::current_num_threads(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    let on_scan_pool = std::thread::current()
                        .name()
                        .is_some_and(|n| n.starts_with("xerj-scan-"));
                    if !on_scan_pool {
                        DIGEST_RAN_OFF_THE_SCAN_POOL
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                let (outcome, fresh) = match carried(file) {
                    // Cache hit: no read, and no byte credited to the hash
                    // phase either — the phase's totals were sized to the files
                    // this pass will actually read, so crediting a skipped file
                    // would push the percent past 100 and invent a rate.
                    Some(digest) => (HashOutcome::Ready(digest), false),
                    None => {
                        // Held across the read and dropped before the byte
                        // credit, so a blocked `read` is visible and a finished
                        // one is not counted twice.
                        let digest = {
                            let _blocking = crate::dataless::watch_blocking_hash(blocking, file);
                            full_digest(&file.path, file.size)
                        };
                        // Credit the attempt even when the open fails. The
                        // phase total counted this file; leaving it out parks
                        // the bar short of 100% for a skip that is success.
                        hashed(file.size);
                        let outcome = match digest {
                            Ok(digest) => HashOutcome::Ready(digest),
                            Err(error) => match read_failure_kind(&error) {
                                Some(kind) => HashOutcome::Unreadable {
                                    detail: format!("{error:#}"),
                                    kind,
                                },
                                None => HashOutcome::Fatal(error),
                            },
                        };
                        (outcome, true)
                    }
                };
                if let HashOutcome::Ready(digest) = &outcome {
                    observed(file, digest, fresh);
                }
                outcome
            })
            .collect()
    });
    let mut fatal: Option<anyhow::Error> = None;
    let mut kept_files = Vec::new();
    let mut digests = Vec::new();
    let mut unreadable = Vec::new();
    for (file, outcome) in files.into_iter().zip(outcomes) {
        match outcome {
            HashOutcome::Ready(digest) => {
                kept_files.push(file);
                digests.push(digest);
            }
            HashOutcome::Unreadable { detail, kind } => {
                unreadable.push(UnreadableSource { file, detail, kind });
            }
            HashOutcome::Fatal(error) => {
                if fatal.is_none() {
                    fatal = Some(error);
                }
            }
        }
    }
    if let Some(error) = fatal {
        return Err(error);
    }
    unreadable.sort_by(|left, right| left.file.rel.cmp(&right.file.rel));
    resolve_with_digests(kept_files, digests, unreadable)
}

fn resolve_with_digests(
    files: Vec<FileEntry>,
    digests: Vec<String>,
    unreadable: Vec<UnreadableSource>,
) -> Result<Inventory> {
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, digest) in digests.iter().enumerate() {
        groups.entry(digest).or_default().push(index);
    }

    let mut keep = vec![true; files.len()];
    let mut resolved_keys = digests.clone();
    let mut duplicates = Vec::new();
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        let mut indices = indices.clone();
        // Prefer a real in-tree path over a followed symlink as canonical,
        // then the lowest NATIVE identity — the exact `(is_symlink, path_id)`
        // rank the sync manifest re-derives and validates
        // (`sync::normalize_paths` / `validate_groups`). Ranking by `rel`
        // here was #1723: on Windows `rel` writes the separator as `/`
        // (0x2F) while `path_id` keeps the native `\` (0x5C), so for
        // `Pill/x.scss` vs `PillS/x.scss` the two orders disagree, the plan
        // projected one canonical and the manifest group another, and every
        // generation of such a corpus was refused ("plan canonical projection
        // disagrees with manifest group"), `--fresh` included. Unix `rel` is
        // the raw path, so the two orders agree there for UTF-8 names.
        indices.sort_by(|&left, &right| {
            (files[left].is_symlink, files[left].rel_id.as_str())
                .cmp(&(files[right].is_symlink, files[right].rel_id.as_str()))
        });
        // A digest match is still only a candidate: verify exact bytes.
        let mut representatives: Vec<usize> = Vec::new();
        for index in indices {
            let canonical = representatives.iter().copied().find(|&candidate| {
                same_file_identity(&files[candidate].path, &files[index].path)
                    || files_equal(&files[candidate].path, &files[index].path).unwrap_or(false)
            });
            if let Some(canonical) = canonical {
                keep[index] = false;
                duplicates.push(DuplicateFile {
                    file_key: resolved_keys[canonical].clone(),
                    rel: files[index].rel.clone(),
                    path_id: files[index].rel_id.clone(),
                    is_symlink: Some(files[index].is_symlink),
                    duplicate_of: files[canonical].rel.clone(),
                    bytes: files[index].size,
                });
            } else {
                if !representatives.is_empty() {
                    // A full-digest collision was byte-proven, not assumed.
                    // Give the unequal representative a deterministic,
                    // path-specific discriminator so documents cannot collide.
                    resolved_keys[index] = format!(
                        "{}-collision-{:032x}",
                        digests[index],
                        xxhash_rust::xxh3::xxh3_128(files[index].rel_id.as_bytes())
                    );
                }
                representatives.push(index);
            }
        }
    }

    let mut unique_files = Vec::new();
    let mut unique_keys = Vec::new();
    let mut unique_digests = Vec::new();
    for (index, file) in files.into_iter().enumerate() {
        if keep[index] {
            unique_files.push(file);
            unique_keys.push(resolved_keys[index].clone());
            unique_digests.push(digests[index].clone());
        }
    }
    duplicates.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(Inventory {
        files: unique_files,
        keys: unique_keys,
        digests: unique_digests,
        duplicates,
        unreadable,
    })
}

/// A source file moved under the run after it was hashed (#1031, #1911, #2314).
///
/// The display text is the operator message. `path` is the live source, so a
/// seal that noticed the change on the staged copy can still name the file
/// the caller has to skip.
#[derive(Debug, Clone)]
pub struct SourceChanged {
    pub path: std::path::PathBuf,
    pub detail: String,
    /// The file is still on disk and another process holds it. A no-graph seal
    /// can keep the previous generation's bytes instead of dropping the file.
    pub locked: bool,
}

impl std::fmt::Display for SourceChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for SourceChanged {}

fn disappeared_or_locked(path: &Path, kind: ReadFailureKind, detail: &str) -> SourceChanged {
    let detail = match kind {
        ReadFailureKind::Absent => format!(
            "{} no longer exists ({detail}); retry the run",
            path.display()
        ),
        ReadFailureKind::Locked => format!(
            "{} could not be opened because another process holds it locked ({detail}); retry \
             the run",
            path.display()
        ),
    };
    SourceChanged {
        path: path.to_path_buf(),
        detail,
        locked: kind == ReadFailureKind::Locked,
    }
}

pub fn verify(path: &Path, expected_size: u64, expected_digest: &str) -> Result<()> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            if let Some(kind) = io_read_failure_kind(&error) {
                return Err(disappeared_or_locked(path, kind, &format!("{error}")).into());
            }
            return Err(error).with_context(|| format!("stat {}", path.display()));
        }
    };
    let actual_size = metadata.len();
    if actual_size != expected_size {
        return Err(SourceChanged {
            path: path.to_path_buf(),
            detail: format!(
                "{} changed size during autoindex (expected {}, now {}); retry the run",
                path.display(),
                expected_size,
                actual_size
            ),
            locked: false,
        }
        .into());
    }
    let actual = match full_digest(path, actual_size) {
        Ok(digest) => digest,
        Err(error) => {
            if let Some(kind) = read_failure_kind(&error) {
                return Err(disappeared_or_locked(path, kind, &format!("{error:#}")).into());
            }
            return Err(error);
        }
    };
    if actual != expected_digest {
        return Err(SourceChanged {
            path: path.to_path_buf(),
            detail: format!(
                "{} changed content during autoindex; retry the run",
                path.display()
            ),
            locked: false,
        }
        .into());
    }
    Ok(())
}

fn full_digest(path: &Path, size: u64) -> Result<String> {
    let mut file =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut hash = Xxh3::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("axf2-{:032x}-{size:x}", hash.digest128()))
}

fn files_equal(a: &Path, b: &Path) -> Result<bool> {
    let mut a = BufReader::new(File::open(a).with_context(|| format!("open {}", a.display()))?);
    let mut b = BufReader::new(File::open(b).with_context(|| format!("open {}", b.display()))?);
    let mut ab = [0u8; 64 * 1024];
    let mut bb = [0u8; 64 * 1024];
    loop {
        let an = read_chunk(&mut a, &mut ab)?;
        let bn = read_chunk(&mut b, &mut bb)?;
        if an != bn || ab[..an] != bb[..bn] {
            return Ok(false);
        }
        if an == 0 {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
fn same_file_identity(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file_identity(_a: &Path, _b: &Path) -> bool {
    false
}

fn read_chunk(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        let n = reader.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// These tests are about identity resolution, not progress reporting.
    fn resolve(files: Vec<FileEntry>) -> Result<Inventory> {
        resolve_reporting(files, &|_| {})
    }

    /// #240 §2: the digest phase ran on rayon's default global pool — every
    /// core on the machine — so `--workers` never bounded it. It must run on
    /// the run's own scan pool, whose width the CLI sets.
    ///
    /// #241 rides on the same closure: moving phase A into a private pool must
    /// not cost the progress stream a single completion, or the percent the
    /// ticker prints would stall below 100 for the rest of the phase.
    #[test]
    fn the_digest_phase_runs_on_the_runs_scan_pool_and_still_reports_every_file() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..32 {
            fs::write(dir.path().join(format!("f{i}.txt")), format!("body {i}\n")).unwrap();
        }
        OBSERVED_DIGEST_POOL_THREADS.store(0, std::sync::atomic::Ordering::Relaxed);
        DIGEST_RAN_OFF_THE_SCAN_POOL.store(false, std::sync::atomic::Ordering::Relaxed);

        let files = crate::walk::walk(dir.path(), false).unwrap();
        let expected_files = files.len() as u64;
        let expected_bytes: u64 = files.iter().map(|f| f.size).sum();
        let reported_files = std::sync::atomic::AtomicU64::new(0);
        let reported_bytes = std::sync::atomic::AtomicU64::new(0);
        resolve_reporting(files, &|bytes| {
            reported_files.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            reported_bytes.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        })
        .unwrap();

        assert!(
            !DIGEST_RAN_OFF_THE_SCAN_POOL.load(std::sync::atomic::Ordering::Relaxed),
            "phase A must not run on rayon's global pool"
        );
        assert_eq!(
            OBSERVED_DIGEST_POOL_THREADS.load(std::sync::atomic::Ordering::Relaxed),
            crate::pool::width(),
            "the digest phase must see exactly the scan pool the run configured"
        );
        assert_eq!(
            reported_files.load(std::sync::atomic::Ordering::Relaxed),
            expected_files,
            "every hashed file must still be credited to the progress stream"
        );
        assert_eq!(
            reported_bytes.load(std::sync::atomic::Ordering::Relaxed),
            expected_bytes,
            "the bytes-based percent must see the whole corpus"
        );
    }

    #[test]
    fn identical_paths_become_one_content_file_and_an_alias() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"same records\n").unwrap();
        fs::write(dir.path().join("b.txt"), b"same records\n").unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false).unwrap()).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.files[0].rel, "a.txt");
        assert_eq!(inv.duplicates.len(), 1);
        assert_eq!(inv.duplicates[0].duplicate_of, "a.txt");
        assert_eq!(inv.keys[0], inv.duplicates[0].file_key);
    }

    #[test]
    fn same_prefix_and_size_but_different_tail_get_distinct_durable_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = vec![b'x'; 65_537];
        let mut second = first.clone();
        first[65_536] = b'a';
        second[65_536] = b'b';
        fs::write(dir.path().join("a.txt"), first).unwrap();
        fs::write(dir.path().join("b.txt"), second).unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false).unwrap()).unwrap();
        assert_eq!(inv.files.len(), 2);
        assert!(inv.duplicates.is_empty());
        assert_ne!(inv.keys[0], inv.keys[1]);
    }

    #[test]
    fn verification_detects_same_size_tail_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"content-a").unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false).unwrap()).unwrap();
        fs::write(&path, b"content-b").unwrap();
        assert!(verify(&path, 9, &inv.digests[0]).is_err());
    }

    #[test]
    fn byte_proven_full_digest_collision_gets_distinct_document_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"unequal-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"unequal-b").unwrap();
        let files = crate::walk::walk(dir.path(), false).unwrap();
        let forced = vec!["axf2-forced-9".to_string(); 2];
        let inv = resolve_with_digests(files, forced, Vec::new()).unwrap();
        assert_eq!(inv.files.len(), 2);
        assert!(inv.duplicates.is_empty());
        assert_ne!(inv.keys[0], inv.keys[1]);
        assert!(inv.keys.iter().any(|key| key.contains("-collision-")));
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_are_aliases_without_pairwise_reading() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"hardlinked").unwrap();
        fs::hard_link(&a, &b).unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false).unwrap()).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.duplicates.len(), 1);
        assert_eq!(inv.duplicates[0].is_symlink, Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn lossy_display_collisions_keep_distinct_alias_identities() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("canonical"), b"same").unwrap();
        fs::write(
            dir.path().join(OsString::from_vec(vec![b'a', 0x80])),
            b"same",
        )
        .unwrap();
        fs::write(
            dir.path().join(OsString::from_vec(vec![b'a', 0x81])),
            b"same",
        )
        .unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false).unwrap()).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.duplicates.len(), 2);
        assert_ne!(inv.duplicates[0].path_id, inv.duplicates[1].path_id);
    }

    #[cfg(unix)]
    #[test]
    fn followed_symlink_does_not_become_canonical_over_real_path() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("z-real");
        fs::write(&real, b"same").unwrap();
        symlink(&real, dir.path().join("a-link")).unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), true).unwrap()).unwrap();
        assert_eq!(inv.files[0].rel, "z-real");
        assert_eq!(inv.duplicates[0].rel, "a-link");
        assert_eq!(inv.duplicates[0].is_symlink, Some(true));
    }

    #[test]
    fn a_file_deleted_after_the_walk_is_skipped_and_the_rest_are_hashed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("keep.txt"), b"keep\n").unwrap();
        fs::write(dir.path().join("gone.txt"), b"gone\n").unwrap();
        let files = crate::walk::walk(dir.path(), false).unwrap();
        let gone = files
            .iter()
            .find(|file| file.rel.ends_with("gone.txt"))
            .unwrap()
            .path
            .clone();
        fs::remove_file(&gone).unwrap();
        let reported = std::sync::atomic::AtomicU64::new(0);
        let inv = resolve_reporting(files, &|bytes| {
            reported.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        })
        .unwrap();
        assert_eq!(inv.files.len(), 1);
        assert!(inv.files[0].rel.ends_with("keep.txt"));
        assert_eq!(inv.unreadable.len(), 1);
        assert_eq!(inv.unreadable[0].kind, ReadFailureKind::Absent);
        assert!(inv.unreadable[0].file.rel.ends_with("gone.txt"));
        let reason = unreadable_skip_reason(&inv.unreadable[0].detail, inv.unreadable[0].kind);
        assert!(reason.contains("no longer exists"), "{reason}");
        assert!(reason.contains("disappeared"), "{reason}");
        assert!(
            !reason.contains("not in the frozen resume plan"),
            "{reason}"
        );
        assert!(reported.load(std::sync::atomic::Ordering::Relaxed) > 0);
    }

    #[test]
    fn a_directory_handed_to_the_hash_still_aborts_the_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("subdir");
        fs::create_dir(&nested).unwrap();
        fs::write(dir.path().join("keep.txt"), b"keep\n").unwrap();
        let mut files = crate::walk::walk(dir.path(), false).unwrap();
        files.push(FileEntry {
            path: nested,
            rel: "subdir".into(),
            rel_id: "subdir".into(),
            is_symlink: false,
            size: 0,
        });
        assert!(resolve(files).is_err());
    }

    #[test]
    fn not_found_is_absent_and_a_sharing_violation_is_locked_only_on_windows() {
        let missing = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            io_read_failure_kind(&missing),
            Some(ReadFailureKind::Absent)
        );
        let sharing = std::io::Error::from_raw_os_error(32);
        let lock = std::io::Error::from_raw_os_error(33);
        #[cfg(windows)]
        {
            assert_eq!(
                io_read_failure_kind(&sharing),
                Some(ReadFailureKind::Locked)
            );
            assert_eq!(io_read_failure_kind(&lock), Some(ReadFailureKind::Locked));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(io_read_failure_kind(&sharing), None);
            assert_eq!(io_read_failure_kind(&lock), None);
        }
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        assert_eq!(io_read_failure_kind(&denied), None);
        let wrapped =
            anyhow::Error::from(std::io::Error::from_raw_os_error(2)).context("open gone.txt");
        assert_eq!(read_failure_kind(&wrapped), Some(ReadFailureKind::Absent));
    }

    #[test]
    fn verification_names_a_file_that_disappeared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"content-a").unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false).unwrap()).unwrap();
        fs::remove_file(&path).unwrap();
        let err = verify(&path, inv.files[0].size, &inv.digests[0]).unwrap_err();
        let changed = err.downcast_ref::<SourceChanged>().expect("source changed");
        assert!(!changed.locked);
        assert!(
            changed.detail.contains("no longer exists"),
            "{}",
            changed.detail
        );
    }
}
