//! Resume journal — append-only NDJSON living OUTSIDE the scanned folder
//! (default ~/.xerj/autoindex/<hash>/journal.ndjson). A torn last line is
//! discarded; worst case one file is fully reprocessed and idempotent _ids
//! dedupe it.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
static FILE_DONE_IO_FAILPOINT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
pub(crate) static FILE_DONE_IO_FAILPOINT_TEST_LOCK: std::sync::Mutex<()> =
    std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn fail_next_file_done_io(boundary: u8) {
    FILE_DONE_IO_FAILPOINT.store(boundary, std::sync::atomic::Ordering::SeqCst);
}

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

#[cfg(test)]
mod tests {
    use super::Journal;

    #[test]
    fn legacy_journal_can_resume_with_a_custom_operational_timeout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("journal.ndjson"),
            concat!(
                "{\"v\":1,\"kind\":\"run\",\"root\":\"root\",\"url\":\"url\",",
                "\"prefix\":\"prefix\",\"run_id\":\"legacy\"}\n"
            ),
        )
        .unwrap();
        let journal = Journal::open(dir.path(), "root", "url", "prefix", 3600, false).unwrap();
        assert!(journal.resumed);
        drop(journal);
        let text = std::fs::read_to_string(dir.path().join("journal.ndjson")).unwrap();
        assert!(text.contains("\"kind\":\"resume\""));
        assert!(text.contains("\"bulk_timeout_secs\":3600"));
    }

    #[test]
    fn fresh_journal_records_each_runs_effective_operational_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path(), "root", "url", "prefix", 3600, true).unwrap();
        drop(journal);
        let text = std::fs::read_to_string(dir.path().join("journal.ndjson")).unwrap();
        assert!(text.contains("\"bulk_timeout_secs\":3600"));
        assert!(
            Journal::open(dir.path(), "root", "url", "prefix", 900, false)
                .unwrap()
                .resumed
        );
        let text = std::fs::read_to_string(dir.path().join("journal.ndjson")).unwrap();
        assert!(text.contains("\"kind\":\"run\""));
        assert!(text.contains("\"bulk_timeout_secs\":3600"));
        assert!(text.contains("\"kind\":\"resume\""));
        assert!(text.contains("\"bulk_timeout_secs\":900"));
    }

    #[test]
    fn semantic_identity_is_durable_and_drift_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = Journal::open(dir.path(), "root", "url", "prefix", 300, true).unwrap();
        first
            .pin_embedding_identity(&"a".repeat(64), true, None)
            .unwrap();
        drop(first);

        let mut resumed = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        resumed
            .pin_embedding_identity(&"a".repeat(64), true, None)
            .unwrap();
        let error = resumed
            .pin_embedding_identity(&"b".repeat(64), true, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to mix vector spaces"), "{error}");
        assert!(error.contains("Restore the original"), "{error}");
        assert!(error.contains("--fresh"), "{error}");
        assert!(error.contains("--prefix"), "{error}");
        assert!(error.contains("delete and recreate"), "{error}");
    }

    #[test]
    fn unpinned_backend_allows_fresh_run_but_not_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(dir.path(), "root", "url", "prefix", 300, true).unwrap();
        journal
            .pin_embedding_identity(&"a".repeat(64), false, Some("remote alias can drift"))
            .unwrap();
        drop(journal);
        let mut resumed = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        let error = resumed
            .pin_embedding_identity(&"a".repeat(64), false, Some("remote alias can drift"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("remote alias can drift"), "{error}");
        assert!(error.contains("--fresh"), "{error}");
        assert!(error.contains("--prefix"), "{error}");
        assert!(error.contains("delete and recreate"), "{error}");
    }

    #[test]
    fn completed_legacy_semantic_journal_requires_fresh_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("journal.ndjson"),
            concat!(
                "{\"v\":1,\"kind\":\"run\",\"root\":\"root\",\"url\":\"url\",",
                "\"prefix\":\"prefix\",\"run_id\":\"legacy\"}\n",
                "{\"kind\":\"file_done\",\"file_key\":\"f\",\"path\":\"report.txt\",",
                "\"records\":1,\"junk\":0,\"bytes\":10}\n"
            ),
        )
        .unwrap();
        let mut journal = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        let error = journal
            .pin_embedding_identity(&"a".repeat(64), true, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("predates embedding identity pinning"),
            "{error}"
        );
        assert!(error.contains("--fresh"), "{error}");
        assert!(error.contains("--prefix"), "{error}");
        assert!(error.contains("delete and recreate"), "{error}");
    }

    #[test]
    fn malformed_or_conflicting_identity_records_fail_during_replay() {
        for tail in [
            "{\"v\":1,\"kind\":\"embedding_identity\",\"identity_sha256\":\"bad\",\"resumable\":true}\n",
            concat!(
                "{\"v\":1,\"kind\":\"embedding_identity\",\"identity_sha256\":\"",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "\",\"resumable\":true}\n",
                "{\"v\":1,\"kind\":\"embedding_identity\",\"identity_sha256\":\"",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "\",\"resumable\":true}\n"
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("journal.ndjson"),
                format!(
                    "{{\"v\":1,\"kind\":\"run\",\"root\":\"root\",\"url\":\"url\",\
                     \"prefix\":\"prefix\",\"run_id\":\"legacy\"}}\n{tail}"
                ),
            )
            .unwrap();
            assert!(
                Journal::open(dir.path(), "root", "url", "prefix", 300, false).is_err(),
                "{tail}"
            );
        }
    }

    /// The preflight must never be more fatal than the open it precedes.
    /// `open_after_preflight` deletes the journal unread when `--fresh` is set,
    /// so a preflight that hard-errors while parsing that journal would make
    /// `--fresh` unreachable in exactly the states whose error text recommends
    /// it — a state directory no supported flag can recover.
    #[test]
    fn fresh_recovers_journals_that_the_preflight_refuses_to_resume() {
        for (label, journal, refusal_marker) in [
            (
                "malformed record",
                concat!(
                    "{\"v\":1,\"kind\":\"run\",\"root\":\"root\",\"url\":\"url\",",
                    "\"prefix\":\"prefix\",\"run_id\":\"legacy\"}\n",
                    "{not json at all\n"
                ),
                "--fresh",
            ),
            (
                "root/url/prefix mismatch",
                concat!(
                    "{\"v\":1,\"kind\":\"run\",\"root\":\"elsewhere\",\"url\":\"url\",",
                    "\"prefix\":\"prefix\",\"run_id\":\"legacy\"}\n"
                ),
                "was created for root=",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("journal.ndjson");
            std::fs::write(&path, journal).unwrap();

            let refused = Journal::preflight(dir.path(), "root", "url", "prefix", false)
                .err()
                .unwrap_or_else(|| panic!("{label}: a resume must still refuse this journal"));
            let refused = format!("{refused:#}");
            assert!(refused.contains(refusal_marker), "{label}: {refused}");

            let preflight = Journal::preflight(dir.path(), "root", "url", "prefix", true)
                .unwrap_or_else(|e| panic!("{label}: --fresh must pass the preflight: {e:#}"));
            assert!(preflight.plan.is_none(), "{label}");
            let reason = preflight
                .unreadable_plan
                .clone()
                .unwrap_or_else(|| panic!("{label}: the discarded plan must be reported"));
            assert!(!reason.is_empty(), "{label}");

            let journal =
                Journal::open_after_preflight(preflight, "root", "url", "prefix", 300, true)
                    .unwrap_or_else(|e| panic!("{label}: --fresh must open: {e:#}"));
            drop(journal);
            // And the rebuilt state directory is resumable again without --fresh.
            Journal::open(dir.path(), "root", "url", "prefix", 300, false)
                .unwrap_or_else(|e| panic!("{label}: the rebuilt journal must resume: {e:#}"));
        }
    }

    /// A readable journal must not be reported as unreadable just because the
    /// run asked for `--fresh`; the note is a discarded-state warning, not a
    /// `--fresh` banner.
    #[test]
    fn fresh_over_a_readable_journal_reports_nothing_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        drop(Journal::open(dir.path(), "root", "url", "prefix", 300, true).unwrap());
        let preflight = Journal::preflight(dir.path(), "root", "url", "prefix", true).unwrap();
        assert!(preflight.unreadable_plan.is_none());
    }

    #[test]
    fn preflight_holds_the_same_exclusive_lock_through_authoritative_open() {
        let dir = tempfile::tempdir().unwrap();
        let preflight = Journal::preflight(dir.path(), "root", "url", "prefix", false).unwrap();
        let error = Journal::open(dir.path(), "root", "url", "prefix", 300, false)
            .err()
            .expect("a second opener must not pass the held preflight lock");
        assert!(format!("{error:#}").contains("already in use"));

        let journal =
            Journal::open_after_preflight(preflight, "root", "url", "prefix", 300, false).unwrap();
        drop(journal);
        assert!(Journal::open(dir.path(), "root", "url", "prefix", 300, false).is_ok());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAssignment {
    pub rel: String,
    /// Reversible path identity used for deterministic resume matching.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path_id: String,
    pub family: String,
    pub gzip: bool,
    /// Full-content digest used to validate durable identity across resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
    /// group (None = whole file) → dataset slug
    pub assignments: Vec<(Option<String>, String)>,
    /// Demoted one-off config file (#173): index it through the document
    /// renderer (`extract::extract_as_document`), not its family extractor.
    /// Defaults false so frozen plans from earlier versions resume unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub as_document: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DuplicateFile {
    /// Content identity of the canonical file.
    pub file_key: String,
    /// Root-relative alias path which was not indexed separately.
    pub rel: String,
    #[serde(default)]
    pub path_id: String,
    /// Root-relative canonical path whose records represent this content.
    pub duplicate_of: String,
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
    /// Byte-identical paths represented by a canonical content file.
    #[serde(default)]
    pub duplicate_files: Vec<DuplicateFile>,
    /// Canonical records have been rewritten with the `ax_paths` alias list.
    #[serde(default)]
    pub alias_paths_indexed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDone {
    pub file_key: String,
    pub path: String,
    pub records: u64,
    pub junk: u64,
    pub bytes: u64,
    /// Field values dropped by coercion, grouped by the dataset whose
    /// inferred schema rejected them. Older journals predate this accounting.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub dropped_by_dataset: HashMap<String, u64>,
    /// Content generation committed by this record. Legacy completions have
    /// no generation and cannot commit a newer pending replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
}

pub struct Journal {
    path: PathBuf,
    file: std::fs::File,
    _state_lock: std::fs::File,
    pub run_id: String,
    pub resumed: bool,
    pub done: HashMap<String, FileDone>,
    /// Durable replacement transactions which have not reached file_done.
    /// Replay removes these keys from `done`, so a crash after destructive
    /// publication can never make resume skip a zero/partial generation.
    pub pending_replacements: HashMap<String, String>,
    pub plan: Option<Plan>,
    pub embedding_identity_sha256: Option<String>,
    pub embedding_identity_resumable: Option<bool>,
}

pub fn default_state_dir(root: &str, url: &str, prefix: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home)
        .join(".xerj")
        .join("autoindex")
        .join(crate::ids::state_key(root, url, prefix))
}

/// Read the last durable plan without locking, repairing, truncating or
/// appending to the journal. This is a preflight hint only: `Journal::open`
/// remains the authority after the caller passes its fail-closed inventory
/// gate and acquires the state lock.
fn read_plan_for_preflight(
    path: &Path,
    root: &str,
    url: &str,
    prefix: &str,
) -> Result<Option<Plan>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut plan = None;
    let mut offset = 0u64;
    loop {
        let record_start = offset;
        let mut bytes = Vec::new();
        let read = reader.read_until(b'\n', &mut bytes)?;
        if read == 0 {
            break;
        }
        offset += read as u64;
        if bytes.last() != Some(&b'\n') {
            // `Journal::open` will repair this torn final record after the
            // preflight accepts. It cannot be treated as durable here.
            break;
        }
        let value: Value =
            serde_json::from_slice(bytes.strip_suffix(b"\n").unwrap_or(bytes.as_slice()))
                .with_context(|| {
                    format!(
                        "journal corruption at byte {record_start} in {}: malformed \
                         newline-terminated record. Refusing to discard later records. Restore \
                         the journal from a backup, or truncate it to exactly {record_start} \
                         bytes to keep every completion recorded before the corruption. Deleting \
                         the whole journal (or rerunning with --fresh) also recovers, but \
                         re-extracts and re-embeds the entire corpus",
                        path.display()
                    )
                })?;
        match value.get("kind").and_then(Value::as_str) {
            Some("run") => {
                let recorded_root = value.get("root").and_then(Value::as_str).unwrap_or("");
                let recorded_url = value.get("url").and_then(Value::as_str).unwrap_or("");
                let recorded_prefix = value.get("prefix").and_then(Value::as_str).unwrap_or("");
                anyhow::ensure!(
                    recorded_root == root && recorded_url == url && recorded_prefix == prefix,
                    "journal at {} was created for root={} url={} prefix={}; current run has \
                     root={} url={} prefix={}",
                    path.display(),
                    recorded_root,
                    recorded_url,
                    recorded_prefix,
                    root,
                    url,
                    prefix
                );
            }
            Some("plan") => {
                if let Some(encoded) = value.get("plan") {
                    plan =
                        Some(serde_json::from_value(encoded.clone()).with_context(|| {
                            format!("decode durable plan in {}", path.display())
                        })?);
                }
            }
            _ => {}
        }
    }
    Ok(plan)
}

pub struct JournalPreflight {
    state_dir: PathBuf,
    state_lock: std::fs::File,
    pub plan: Option<Plan>,
    pub journal_exists: bool,
    /// Set only under `--fresh`, when the durable plan could not be read and
    /// the run is about to discard the journal anyway. The caller owns the
    /// decision to print it (autoindex honours `--quiet`).
    pub unreadable_plan: Option<String>,
}

impl Journal {
    /// `fresh` is not a formality here. `open_after_preflight` removes the
    /// journal before replaying it when `--fresh` is set, so nothing inside a
    /// journal can be fatal to that path — and a preflight that hard-errors on
    /// a corrupt record or a root/url/prefix mismatch would make `--fresh`
    /// unreachable in exactly the cases whose error text recommends it. The
    /// preflight must therefore never be more fatal than the open it precedes.
    pub fn preflight(
        state_dir: &Path,
        root: &str,
        url: &str,
        prefix: &str,
        fresh: bool,
    ) -> Result<JournalPreflight> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create state dir {}", state_dir.display()))?;
        let lock_path = state_dir.join(".autoindex.lock");
        let state_lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        state_lock.try_lock_exclusive().with_context(|| {
            format!(
                "autoindex state {} is already in use by another process; wait for it to \
                 finish or choose a different --state-dir",
                state_dir.display()
            )
        })?;
        let journal_path = state_dir.join("journal.ndjson");
        let journal_exists = journal_path.exists();
        let (plan, unreadable_plan) =
            match read_plan_for_preflight(&journal_path, root, url, prefix) {
                Ok(plan) => (plan, None),
                // The journal is about to be deleted unread. Losing the plan here
                // costs the removed-file gate its comparison basis, which is
                // exactly what a full in-place rebuild accepts; the alternative is
                // a state directory that no supported flag can recover.
                Err(error) if fresh => (None, Some(format!("{error:#}"))),
                Err(error) => return Err(error),
            };
        Ok(JournalPreflight {
            state_dir: state_dir.to_owned(),
            state_lock,
            plan,
            journal_exists,
            unreadable_plan,
        })
    }

    pub fn open(
        state_dir: &Path,
        root: &str,
        url: &str,
        prefix: &str,
        bulk_timeout_secs: u64,
        fresh: bool,
    ) -> Result<Journal> {
        let preflight = Self::preflight(state_dir, root, url, prefix, fresh)?;
        Self::open_after_preflight(preflight, root, url, prefix, bulk_timeout_secs, fresh)
    }

    pub fn open_after_preflight(
        preflight: JournalPreflight,
        root: &str,
        url: &str,
        prefix: &str,
        bulk_timeout_secs: u64,
        fresh: bool,
    ) -> Result<Journal> {
        let JournalPreflight {
            state_dir,
            state_lock,
            ..
        } = preflight;
        // Hard kills cannot run NamedTempFile::drop. Stages are owned by this
        // journal directory and use a reserved prefix, so removing only
        // regular files with that prefix is deterministic and scope-safe.
        for entry in std::fs::read_dir(&state_dir)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".autoindex-stage-")
                && entry.file_type()?.is_file()
            {
                std::fs::remove_file(entry.path()).with_context(|| {
                    format!("remove orphan autoindex stage {}", entry.path().display())
                })?;
            }
        }
        let jpath = state_dir.join("journal.ndjson");
        if fresh && jpath.exists() {
            std::fs::remove_file(&jpath).ok();
        }
        let mut done = HashMap::new();
        let mut pending_replacements: HashMap<String, String> = HashMap::new();
        let mut plan = None;
        let mut embedding_identity_sha256 = None;
        let mut embedding_identity_resumable = None;
        let mut run_id = None;
        let mut resumed = false;
        if jpath.exists() {
            let f = std::fs::File::open(&jpath)?;
            let file_len = f.metadata()?.len();
            let mut reader = std::io::BufReader::new(f);
            let mut valid_end = 0u64;
            loop {
                let record_start = valid_end;
                let mut bytes = Vec::new();
                let read = reader.read_until(b'\n', &mut bytes)?;
                if read == 0 {
                    break;
                }
                let newline_terminated = bytes.last() == Some(&b'\n');
                match serde_json::from_slice::<Value>(
                    bytes.strip_suffix(b"\n").unwrap_or(bytes.as_slice()),
                ) {
                    Ok(v) if newline_terminated => {
                        valid_end += read as u64;
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
                                 Use --state-dir for separate state, or --fresh to rebuild the \
                                 plan in place — note that --fresh never deletes documents \
                                 already published under the other root, url or prefix.",
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
                            Some("embedding_identity") => {
                                let digest = v
                                    .get("identity_sha256")
                                    .and_then(Value::as_str)
                                    .filter(|digest| {
                                        digest.len() == 64
                                            && digest.bytes().all(|byte| {
                                                byte.is_ascii_hexdigit()
                                                    && !byte.is_ascii_uppercase()
                                            })
                                    });
                                let resumable = v.get("resumable").and_then(Value::as_bool);
                                if v.get("v").and_then(Value::as_u64) != Some(1)
                                    || digest.is_none()
                                    || resumable.is_none()
                                    || v.as_object().is_none_or(|object| {
                                        object.len() != 4
                                            || !["v", "kind", "identity_sha256", "resumable"]
                                                .iter()
                                                .all(|key| object.contains_key(*key))
                                    })
                                {
                                    anyhow::bail!(
                                        "journal at {} contains a malformed embedding identity; \
                                         restore it from backup or re-run with --fresh",
                                        jpath.display()
                                    );
                                }
                                if embedding_identity_sha256
                                    .as_deref()
                                    .is_some_and(|existing| existing != digest.unwrap())
                                    || embedding_identity_resumable
                                        .is_some_and(|existing| existing != resumable.unwrap())
                                {
                                    anyhow::bail!(
                                        "journal at {} contains conflicting embedding identities; \
                                         restore it from backup or re-run with --fresh",
                                        jpath.display()
                                    );
                                }
                                embedding_identity_sha256 = digest.map(str::to_owned);
                                embedding_identity_resumable = resumable;
                            }
                            Some("file_done") => {
                                if let Ok(fd) = serde_json::from_value::<FileDone>(v.clone()) {
                                    let commits_pending = pending_replacements
                                        .get(&fd.file_key)
                                        .is_none_or(|pending| {
                                            fd.generation.as_deref() == Some(pending.as_str())
                                        });
                                    if commits_pending {
                                        pending_replacements.remove(&fd.file_key);
                                        done.insert(fd.file_key.clone(), fd);
                                    }
                                }
                            }
                            Some("file_replace_start") => {
                                if let (Some(file_key), Some(generation)) = (
                                    v.get("file_key").and_then(Value::as_str),
                                    v.get("generation").and_then(Value::as_str),
                                ) {
                                    done.remove(file_key);
                                    pending_replacements
                                        .insert(file_key.to_string(), generation.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(_) | Err(_)
                        if !newline_terminated && record_start + read as u64 == file_len =>
                    {
                        let repair = std::fs::OpenOptions::new().write(true).open(&jpath)?;
                        repair.set_len(record_start)?;
                        repair.sync_data().with_context(|| {
                            format!(
                                "sync repaired torn journal tail at byte {record_start} in {}",
                                jpath.display()
                            )
                        })?;
                        break;
                    }
                    Ok(_) => unreachable!("complete JSON without newline handled as torn tail"),
                    Err(error) => {
                        anyhow::bail!(
                            "journal corruption at byte {record_start} in {}: malformed \
                             newline-terminated record ({error}). Refusing to discard records \
                             after corruption. Restore the journal from a backup, or truncate it \
                             to exactly {record_start} bytes to keep every completion recorded \
                             before the corruption (files journaled after that point are \
                             re-verified, re-indexed and re-embedded on the next run). Deleting \
                             the whole journal (or rerunning with --fresh) also recovers, but \
                             re-extracts and re-embeds the entire corpus",
                            jpath.display()
                        );
                    }
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
            _state_lock: state_lock,
            run_id: run_id.clone(),
            resumed: resumed && !is_new,
            done,
            pending_replacements,
            plan,
            embedding_identity_sha256,
            embedding_identity_resumable,
        };
        if is_new {
            j.append_transaction(
                &serde_json::json!({
                    "v": 1, "kind": "run", "root": root, "url": url, "prefix": prefix,
                    "bulk_timeout_secs": bulk_timeout_secs,
                    "run_id": run_id, "started": chrono::Utc::now().to_rfc3339(),
                }),
                "run",
            )?;
        } else {
            j.append_transaction(
                &serde_json::json!({
                    "kind": "resume", "bulk_timeout_secs": bulk_timeout_secs,
                    "at": chrono::Utc::now().to_rfc3339(),
                }),
                "resume",
            )?;
        }
        Ok(j)
    }

    /// Pin the server-side vector-space identity before any semantic write.
    pub fn pin_embedding_identity(
        &mut self,
        identity_sha256: &str,
        resumable: bool,
        non_resumable_reason: Option<&str>,
    ) -> Result<()> {
        if identity_sha256.len() != 64
            || !identity_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            anyhow::bail!("server returned an invalid embedding identity digest");
        }
        if self.resumed && (!resumable || self.embedding_identity_resumable == Some(false)) {
            anyhow::bail!(
                "server embedding backend cannot safely resume semantic indexing: {}. \
                 Restore the exact original embedding identity, or rebuild all vectors with \
                 --fresh and a new --prefix. Before reusing the old prefix, delete and recreate \
                 its prior autoindex indices",
                non_resumable_reason.unwrap_or("embedding identity is not immutable")
            );
        }
        if let Some(existing) = &self.embedding_identity_sha256 {
            if existing != identity_sha256 {
                anyhow::bail!(
                    "embedding execution identity changed since this autoindex journal was \
                     created; refusing to mix vector spaces. Restore the original embedding \
                     identity, or rebuild all vectors with --fresh and a new --prefix. Before \
                     reusing the old prefix, delete and recreate its prior autoindex indices"
                );
            }
            return Ok(());
        }
        if self.resumed && !self.done.is_empty() {
            anyhow::bail!(
                "this semantic autoindex journal predates embedding identity pinning and cannot \
                 be resumed safely. Rebuild all vectors with --fresh and a new --prefix. Before \
                 reusing the old prefix, delete and recreate its prior autoindex indices"
            );
        }
        self.append_transaction(
            &serde_json::json!({
                "v": 1,
                "kind": "embedding_identity",
                "identity_sha256": identity_sha256,
                "resumable": resumable,
            }),
            "embedding_identity",
        )?;
        self.embedding_identity_sha256 = Some(identity_sha256.to_owned());
        self.embedding_identity_resumable = Some(resumable);
        Ok(())
    }

    fn append_transaction(&mut self, v: &Value, what: &str) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let start = self.file.metadata()?.len();
        #[cfg(test)]
        if what == "file_done"
            && FILE_DONE_IO_FAILPOINT
                .compare_exchange(
                    1,
                    0,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        {
            anyhow::bail!("injected file_done append failure before any bytes");
        }
        #[cfg(test)]
        let partial_write = what == "file_done"
            && FILE_DONE_IO_FAILPOINT
                .compare_exchange(
                    3,
                    0,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok();
        #[cfg(not(test))]
        let partial_write = false;
        let write_result = if partial_write {
            let written = line.len().min(23);
            self.file
                .write_all(&line.as_bytes()[..written])
                .and_then(|_| {
                    Err(std::io::Error::other(format!(
                        "injected partial file_done write after {written} bytes"
                    )))
                })
        } else {
            self.file.write_all(line.as_bytes())
        };
        if let Err(error) = write_result {
            return self.rollback_transaction(start, what, "append", error);
        }
        #[cfg(test)]
        let injected_sync_failure = what == "file_done"
            && FILE_DONE_IO_FAILPOINT
                .compare_exchange(
                    2,
                    0,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok();
        #[cfg(not(test))]
        let injected_sync_failure = false;
        let sync_result = if injected_sync_failure {
            Err(std::io::Error::other("injected file_done fsync failure"))
        } else {
            self.file.sync_data()
        };
        if let Err(error) = sync_result {
            return self.rollback_transaction(start, what, "fsync", error);
        }
        Ok(())
    }

    fn rollback_transaction(
        &mut self,
        start: u64,
        what: &str,
        operation: &str,
        error: std::io::Error,
    ) -> Result<()> {
        let rollback = self.file.set_len(start).and_then(|_| self.file.sync_data());
        match rollback {
            Ok(()) => Err(error).context(format!(
                "{operation} durable {what}; partial transaction was rolled back to byte {start}"
            )),
            Err(rollback_error) => anyhow::bail!(
                "{operation} durable {what} failed: {error}; rollback to byte {start} also \
                 failed: {rollback_error}. Treat journal state as ambiguous and rerun after \
                 repairing storage"
            ),
        }
    }

    pub fn write_plan(&mut self, plan: &Plan) -> Result<()> {
        self.append_transaction(&serde_json::json!({"kind": "plan", "plan": plan}), "plan")?;
        self.plan = Some(plan.clone());
        Ok(())
    }

    pub fn file_done(&mut self, fd: &FileDone) -> Result<()> {
        if let Some(pending) = self.pending_replacements.get(&fd.file_key) {
            anyhow::ensure!(
                fd.generation.as_deref() == Some(pending.as_str()),
                "refusing stale completion for {} generation {:?}; pending generation is {}",
                fd.file_key,
                fd.generation,
                pending
            );
        }
        let mut v = serde_json::to_value(fd)?;
        v["kind"] = Value::String("file_done".into());
        self.append_transaction(&v, "file_done")?;
        self.pending_replacements.remove(&fd.file_key);
        self.done.insert(fd.file_key.clone(), fd.clone());
        Ok(())
    }

    /// Durably invalidate an older completion before persisting a new plan or
    /// changing live visibility. Replaying this record schedules repair until
    /// a later durable file_done commits the generation.
    pub fn file_replace_start(&mut self, file_key: &str, generation: &str) -> Result<()> {
        self.append_transaction(
            &serde_json::json!({
                "kind": "file_replace_start",
                "file_key": file_key,
                "generation": generation,
            }),
            "file_replace_start",
        )?;
        self.done.remove(file_key);
        self.pending_replacements
            .insert(file_key.to_string(), generation.to_string());
        Ok(())
    }

    pub fn finish(&mut self, summary: &Value) -> Result<()> {
        self.append_transaction(
            &serde_json::json!({"kind": "finish", "summary": summary,
                "at": chrono::Utc::now().to_rfc3339()}),
            "finish",
        )?;
        Ok(())
    }

    pub fn done_keys(&self) -> HashSet<String> {
        self.done.keys().cloned().collect()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn pre_dedupe_plan_deserializes_with_empty_aliases_and_no_digest() {
        let plan: Plan = serde_json::from_value(serde_json::json!({
            "datasets": [],
            "files": {
                "legacy-key": {
                    "rel": "report.pdf",
                    "family": "pdf",
                    "gzip": false,
                    "assignments": []
                }
            },
            "junk_files": []
        }))
        .unwrap();
        assert!(plan.duplicate_files.is_empty());
        assert!(plan.files["legacy-key"].content_digest.is_none());
    }

    #[test]
    fn replacement_start_durably_invalidates_done_until_a_durable_commit() {
        // The failpoint is a global one-shot: any test that reaches
        // `file_done` while another test has it armed would consume the
        // injection meant for the armer (observed live 2026-07-30 as a
        // parallel-run flake + poison cascade). Every file_done-reaching
        // test holds this lock, armer or not.
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        let old = FileDone {
            file_key: "file-key".into(),
            path: "large.csv".into(),
            records: 6_001,
            junk: 0,
            bytes: 100,
            dropped_by_dataset: HashMap::new(),
            generation: Some("old-generation".into()),
        };
        journal.file_done(&old).unwrap();
        journal
            .file_replace_start("file-key", "new-generation")
            .unwrap();
        journal.write_plan(&Plan::default()).unwrap();
        drop(journal);

        let resumed = Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert!(!resumed.done.contains_key("file-key"));
        assert_eq!(
            resumed.pending_replacements.get("file-key"),
            Some(&"new-generation".to_string())
        );
        drop(resumed);

        let mut committing =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        committing
            .file_done(&FileDone {
                records: 2,
                bytes: 20,
                generation: Some("new-generation".into()),
                ..old
            })
            .unwrap();
        drop(committing);
        let committed =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert_eq!(committed.done["file-key"].records, 2);
        assert!(!committed.pending_replacements.contains_key("file-key"));

        // The commit really reached the append-only file, rather than only
        // changing the in-memory replay state.
        let text = fs::read_to_string(dir.path().join("journal.ndjson")).unwrap();
        assert!(text.contains("\"kind\":\"file_replace_start\""));
        assert_eq!(text.matches("\"kind\":\"file_done\"").count(), 2);
    }

    #[test]
    fn torn_file_done_tail_is_truncated_before_repair_commit_is_appended() {
        // Reaches file_done → must hold the failpoint lock (see above).
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal
            .file_replace_start("file-key", "generation-2")
            .unwrap();
        drop(journal);
        let path = dir.path().join("journal.ndjson");
        let valid_len = fs::metadata(&path).unwrap().len();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(br#"{"kind":"file_done","file_key":"file-key","generation":"gener"#)
            .unwrap();

        let mut repaired =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert!(repaired.done.is_empty());
        assert_eq!(repaired.pending_replacements["file-key"], "generation-2");
        assert!(fs::metadata(&path).unwrap().len() > valid_len);
        repaired
            .file_done(&FileDone {
                file_key: "file-key".into(),
                path: "records.csv".into(),
                records: 2,
                junk: 0,
                bytes: 20,
                dropped_by_dataset: HashMap::new(),
                generation: Some("generation-2".into()),
            })
            .unwrap();
        drop(repaired);

        let reopened =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert_eq!(reopened.done["file-key"].records, 2);
        assert!(reopened.pending_replacements.is_empty());
    }

    #[test]
    fn torn_replacement_start_and_plan_tails_are_repaired_before_append() {
        for torn in [
            br#"{"kind":"file_replace_start","file_key":"file-key","generation":"new""#.as_slice(),
            br#"{"kind":"plan","plan":{"datasets":[],"files":{"#.as_slice(),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let journal =
                Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
            drop(journal);
            let path = dir.path().join("journal.ndjson");
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(torn)
                .unwrap();

            let repaired =
                Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
            assert!(repaired.pending_replacements.is_empty());
            drop(repaired);
            // The resume appended after truncation must itself remain visible
            // on every later replay.
            let reopened =
                Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
            assert!(reopened.resumed);
        }
    }

    #[test]
    fn malformed_newline_terminated_middle_record_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.write_plan(&Plan::default()).unwrap();
        drop(journal);
        let path = dir.path().join("journal.ndjson");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{malformed middle}\n").unwrap();
        file.write_all(b"{\"kind\":\"resume\"}\n").unwrap();
        drop(file);

        let error = Journal::open(dir.path(), "root", "http://engine", "ax", 300, false)
            .err()
            .expect("corruption must fail closed");
        let message = format!("{error:#}");
        assert!(message.contains("journal corruption"));
        // Recovery guidance must stay scoped and honest: byte-exact truncation
        // keeps prior completions; discarding the journal re-embeds everything.
        assert!(message.contains("truncate it to exactly"));
        assert!(message.contains("re-extracts and re-embeds the entire corpus"));
    }

    #[test]
    fn stale_generation_completion_cannot_clear_newer_pending_replacement() {
        // Reaches file_done → must hold the failpoint lock (see above).
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal.file_replace_start("file-key", "new").unwrap();
        let error = journal
            .file_done(&FileDone {
                file_key: "file-key".into(),
                path: "records.csv".into(),
                records: 10,
                junk: 0,
                bytes: 10,
                dropped_by_dataset: HashMap::new(),
                generation: Some("old".into()),
            })
            .unwrap_err();
        assert!(error.to_string().contains("refusing stale completion"));
        assert_eq!(journal.pending_replacements["file-key"], "new");
        assert!(!journal.done.contains_key("file-key"));
    }

    #[test]
    fn append_and_fsync_commit_failures_leave_pending_and_replayable() {
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
        for boundary in [1, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let mut journal =
                Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
            journal.file_replace_start("file-key", "new").unwrap();
            fail_next_file_done_io(boundary);
            let error = journal
                .file_done(&FileDone {
                    file_key: "file-key".into(),
                    path: "records.csv".into(),
                    records: 2,
                    junk: 0,
                    bytes: 20,
                    dropped_by_dataset: HashMap::new(),
                    generation: Some("new".into()),
                })
                .unwrap_err();
            assert!(format!("{error:#}").contains(if boundary == 1 {
                "append failure"
            } else if boundary == 3 {
                "partial file_done write"
            } else {
                "fsync failure"
            }));
            assert!(journal.done.is_empty());
            assert_eq!(journal.pending_replacements["file-key"], "new");
            drop(journal);
            let replay =
                Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
            assert!(replay.done.is_empty());
            assert_eq!(replay.pending_replacements["file-key"], "new");
        }
    }

    #[test]
    fn partial_commit_is_rolled_back_before_another_worker_appends() {
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal
            .file_replace_start("file-c", "generation-c")
            .unwrap();
        fail_next_file_done_io(3);
        let error = journal
            .file_done(&FileDone {
                file_key: "file-c".into(),
                path: "c.csv".into(),
                records: 2,
                junk: 0,
                bytes: 20,
                dropped_by_dataset: HashMap::new(),
                generation: Some("generation-c".into()),
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("after 23 bytes"));

        // The mutex would now pass to another worker. Its complete record must
        // begin exactly at the rolled-back boundary, not after a torn prefix.
        journal
            .file_done(&FileDone {
                file_key: "file-d".into(),
                path: "d.csv".into(),
                records: 3,
                junk: 0,
                bytes: 30,
                dropped_by_dataset: HashMap::new(),
                generation: Some("generation-d".into()),
            })
            .unwrap();
        drop(journal);

        let mut replay =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert_eq!(replay.pending_replacements["file-c"], "generation-c");
        assert!(!replay.done.contains_key("file-c"));
        assert_eq!(replay.done["file-d"].records, 3);
        replay
            .file_done(&FileDone {
                file_key: "file-c".into(),
                path: "c.csv".into(),
                records: 2,
                junk: 0,
                bytes: 20,
                dropped_by_dataset: HashMap::new(),
                generation: Some("generation-c".into()),
            })
            .unwrap();
        drop(replay);
        let committed =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert_eq!(committed.done.len(), 2);
        assert!(committed.pending_replacements.is_empty());
    }

    #[test]
    fn state_lock_precedes_safe_direct_child_orphan_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".autoindex-stage-orphan"), b"staged").unwrap();
        fs::create_dir(dir.path().join(".autoindex-stage-directory")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            dir.path().join(".autoindex-stage-orphan-target"),
            dir.path().join(".autoindex-stage-symlink"),
        )
        .unwrap();

        let journal = Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        assert!(!dir.path().join(".autoindex-stage-orphan").exists());
        assert!(dir.path().join(".autoindex-stage-directory").is_dir());
        #[cfg(unix)]
        assert!(dir.path().join(".autoindex-stage-symlink").is_symlink());

        // A second opener is rejected before it can inspect or clean stages
        // belonging to the live first process.
        fs::write(dir.path().join(".autoindex-stage-live"), b"live stage").unwrap();
        let error = Journal::open(dir.path(), "root", "http://engine", "ax", 300, false)
            .err()
            .expect("second process must not share a state directory");
        assert!(error.to_string().contains("already in use"));
        assert!(dir.path().join(".autoindex-stage-live").exists());
        drop(journal);
    }
}
