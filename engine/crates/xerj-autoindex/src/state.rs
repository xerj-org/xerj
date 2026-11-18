//! Resume journal — append-only NDJSON living OUTSIDE the scanned folder
//! (default ~/.xerj/autoindex/<hash>/journal.ndjson). A torn last line is
//! discarded; worst case one file is fully reprocessed and idempotent _ids
//! dedupe it.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
static FILE_DONE_IO_FAILPOINT: std::sync::Mutex<Option<(u8, PathBuf)>> =
    std::sync::Mutex::new(None);
#[cfg(test)]
static SYNC_IO_FAILPOINT: std::sync::Mutex<Option<(u8, PathBuf)>> = std::sync::Mutex::new(None);
#[cfg(test)]
pub(crate) static FILE_DONE_IO_FAILPOINT_TEST_LOCK: std::sync::Mutex<()> =
    std::sync::Mutex::new(());
#[cfg(test)]
pub(crate) static SYNC_IO_FAILPOINT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn fail_next_file_done_io(boundary: u8, journal_path: &Path) {
    *FILE_DONE_IO_FAILPOINT.lock().unwrap() = Some((boundary, journal_path.to_path_buf()));
}

#[cfg(test)]
fn fail_next_sync_io(boundary: u8, journal_path: &Path) {
    *SYNC_IO_FAILPOINT.lock().unwrap() = Some((boundary, journal_path.to_path_buf()));
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
    /// The analyzer this dataset's index declares at CREATE time for its text
    /// fields (#1059): `Some("stemmer")` when the profiler marked at least one
    /// of the dataset's text fields as natural-language prose, else `None`.
    ///
    /// It rides the PLAN and not [`crate::infer::FieldSpec`] on purpose: the
    /// specs are hashed into every committed generation's `schema_identity`
    /// (`generation_contract_identities` serializes `dataset.specs` verbatim),
    /// so a new serialized spec field would move that digest for every
    /// prose-carrying dataset and abort the next run of every existing state
    /// dir on the incremental no-change arm — the exact upgrade-abort class
    /// the frozen-contract work removed. This struct is serialized into the
    /// journal but the identity digests read it through named keys only, so a
    /// new field here is invisible to them.
    ///
    /// `#[serde(default)]` so a plan frozen by an older build deserializes
    /// with `None` — which is also the honest value: an index that an older
    /// build created was written with the `standard` analyzer, and the
    /// analysis settings of an existing index are immutable server-side, so
    /// the declaration only ever lands on an index THIS build creates
    /// (`install_dataset_mappings`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_analyzer: Option<String>,
    /// Repository scope this dataset was clustered under (#1230): the
    /// root-relative, "/"-separated path of the nearest enclosing `.git`
    /// ancestor at genesis, or `Some("")` for the autoindex root. A `docs`
    /// dataset is one-per-scope (`dataset::cluster`), so this is what tells two
    /// sibling forks with byte-identical schemas apart — without it the
    /// `--no-graph` incremental reconciler cannot say which frozen `docs`
    /// dataset a newly added file belongs to and refuses the whole run as an
    /// ambiguous assignment.
    ///
    /// Like [`Self::text_analyzer`] it rides the PLAN and NOT
    /// [`crate::infer::FieldSpec`]: `generation_contract_identities` serializes
    /// the schema contract through named keys (`slug`/`family`/`group`/`specs`/
    /// `time_field`/`semantic_field`) only, so a new field here is invisible to
    /// the identity digests and does not move any committed generation's
    /// `schema_identity`/`index_identity` — adding it never aborts an existing
    /// state dir on the no-change arm.
    ///
    /// `#[serde(default)]` so a plan frozen by a pre-#1230 build deserializes
    /// with `None`. `None` means "scope was never recorded"; the reconciler
    /// then falls back to its prior schema-only classification (an older plan
    /// that hit the ambiguity simply keeps hitting it), so the fix is additive
    /// and changes nothing for single-scope trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub sampled_records: u64,
    pub file_count: usize,
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_watch_ownership, journal_continues_folder, run_id_in_second, Journal, StateLock,
        WATCH_OWNER_LOCK,
    };

    /// `flock` ownership follows the open file description, so an inherited
    /// descriptor can unlock the acquiring parent's lock. Model that post-fork
    /// relationship deterministically with `try_clone`: a guard whose recorded
    /// owner is a different PID must close only, while the acquiring owner's
    /// guard must still release the lock.
    #[cfg(unix)]
    #[test]
    fn foreign_pid_drop_preserves_lock_until_owner_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.lock");
        let owner_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&owner_file).unwrap();
        let inherited_file = owner_file.try_clone().unwrap();
        let owner_pid = std::process::id();
        let foreign_pid = if owner_pid == u32::MAX {
            owner_pid - 1
        } else {
            owner_pid + 1
        };
        let owner = StateLock {
            file: owner_file,
            owner_pid,
        };
        let inherited = StateLock {
            file: inherited_file,
            owner_pid: foreign_pid,
        };

        drop(inherited);

        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&contender)
            .expect_err("a foreign-PID drop must not unlock the live owner's flock");

        drop(owner);
        fs2::FileExt::try_lock_exclusive(&contender)
            .expect("the acquiring owner's drop must release its flock");
        fs2::FileExt::unlock(&contender).unwrap();
    }

    /// #1232: the per-pass lock is released between passes, so a second
    /// `--watch` for the same journal/destination used to park beside the
    /// first. A session-long ownership lock makes a second owner impossible: the
    /// second acquisition is a bounded refusal while the first is alive, and
    /// ownership frees as soon as the first guard drops (as it does on process
    /// death, by the kernel).
    #[test]
    fn a_second_watch_owner_is_refused_until_the_first_releases() {
        let dir = tempfile::tempdir().unwrap();

        let first = acquire_watch_ownership(dir.path()).expect("first watcher takes ownership");

        let refusal = match acquire_watch_ownership(dir.path()) {
            Ok(_) => panic!("a second watcher for the same state dir must be refused"),
            Err(error) => error,
        };
        let message = format!("{refusal:#}");
        assert!(
            message.contains("already owns") && message.contains(&dir.path().display().to_string()),
            "refusal must name the contended state dir: {message}"
        );

        drop(first);
        let _second = acquire_watch_ownership(dir.path())
            .expect("ownership must be reacquirable once the first owner releases");
    }

    /// The ownership lock lives in its OWN file so a watch session's own passes,
    /// which take the per-pass lock on `.autoindex.lock`, are never blocked by
    /// the session lock the same process already holds. Sharing one file would
    /// deadlock the first pass against the session lock.
    #[test]
    fn watch_ownership_does_not_block_the_sessions_own_per_pass_lock() {
        let dir = tempfile::tempdir().unwrap();
        let owner = acquire_watch_ownership(dir.path()).expect("take watch ownership");

        // A pass inside the same process must still take `.autoindex.lock`.
        let preflight = Journal::preflight(dir.path(), "root", "url", "prefix", false)
            .expect("the per-pass lock must be free while watch ownership is held");

        assert!(dir.path().join(WATCH_OWNER_LOCK).exists());
        assert!(dir.path().join(".autoindex.lock").exists());
        drop(preflight);
        drop(owner);
    }

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

    /// `interrupted` is what tells a resuming run that end-of-run work (the
    /// corpus-wide graph pass) may never have happened for files that are
    /// already journaled done. It is true exactly when a journal is resumed
    /// and its newest record is not a `finish`.
    #[test]
    fn a_resumed_journal_knows_whether_the_previous_run_reached_its_summary() {
        let dir = tempfile::tempdir().unwrap();
        let open = || Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        let mut first = open();
        assert!(!first.interrupted, "a new journal has no previous run");
        first
            .file_replace_start("k1", "g1")
            .expect("publication intent");
        drop(first); // killed before `finish`

        let mut resumed = open();
        assert!(resumed.resumed && resumed.interrupted);
        resumed.finish(&serde_json::json!({"ok": true})).unwrap();
        drop(resumed);

        let after_finish = open();
        assert!(after_finish.resumed && !after_finish.interrupted);
        drop(after_finish); // wrote only its `resume` record, then died

        assert!(
            open().interrupted,
            "a run that resumed and never finished is an interrupted run too"
        );
    }

    /// Run identity must stay unique inside one process even when the
    /// one-second timestamp component cannot distinguish two runs. The
    /// generation catalog keys managed documents on `run_id`, so a duplicate
    /// makes a run read another run's documents back as its own generation.
    #[test]
    fn run_ids_minted_in_the_same_second_and_process_are_distinct() {
        let second = "20260804T000000Z";
        let first = run_id_in_second(second);
        let next = run_id_in_second(second);
        assert_ne!(first, next);
        assert!(first.starts_with(&format!("run-{second}-")), "{first}");
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
            .pin_embedding_identity(&"a".repeat(64), true, None, None)
            .unwrap();
        drop(first);

        let mut resumed = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        resumed
            .pin_embedding_identity(&"a".repeat(64), true, None, None)
            .unwrap();
        let error = resumed
            .pin_embedding_identity(&"b".repeat(64), true, None, None)
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
            .pin_embedding_identity(&"a".repeat(64), false, None, Some("remote alias can drift"))
            .unwrap();
        drop(journal);
        let mut resumed = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        let error = resumed
            .pin_embedding_identity(&"a".repeat(64), false, None, Some("remote alias can drift"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("remote alias can drift"), "{error}");
        assert!(error.contains("--fresh"), "{error}");
        assert!(error.contains("--prefix"), "{error}");
        assert!(error.contains("delete and recreate"), "{error}");
    }

    /// #2306: a journal pinned while the proxy width was still unconfirmed
    /// (`resumable: false`, no width) resumes once the same digest is reported
    /// as resumable. A later width that disagrees with a pinned width does not.
    #[test]
    fn confirmed_width_resumes_a_journal_pinned_before_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let digest = "a".repeat(64);
        let mut journal = Journal::open(dir.path(), "root", "url", "prefix", 300, true).unwrap();
        journal
            .pin_embedding_identity(&digest, false, None, Some("width was not confirmed"))
            .unwrap();
        drop(journal);

        let mut resumed = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        resumed
            .pin_embedding_identity(&digest, true, Some(384), None)
            .expect("same digest becomes resumable once the width is confirmed");
        // The pre-confirmation record did not carry a width, and resume does
        // not append a second identity line (a flipped resumable flag would
        // fail replay). A width is therefore not yet pinned on this journal.
        assert_eq!(resumed.embedding_identity_dimensions, None);
    }

    #[test]
    fn a_pinned_width_is_durable_and_a_missing_width_is_not_drift() {
        let dir = tempfile::tempdir().unwrap();
        let digest = "c".repeat(64);
        let mut first = Journal::open(dir.path(), "root", "url", "prefix", 300, true).unwrap();
        first
            .pin_embedding_identity(&digest, true, Some(384), None)
            .unwrap();
        drop(first);
        let text = std::fs::read_to_string(dir.path().join("journal.ndjson")).unwrap();
        assert!(text.contains("\"dimensions\":384"), "{text}");

        let mut resumed = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert_eq!(resumed.embedding_identity_dimensions, Some(384));
        let lost = resumed
            .pin_embedding_identity(&digest, true, None, None)
            .unwrap_err()
            .to_string();
        assert!(lost.contains("no longer reported"), "{lost}");
        assert!(lost.contains("--fresh"), "{lost}");
        let drifted = resumed
            .pin_embedding_identity(&digest, true, Some(768), None)
            .unwrap_err()
            .to_string();
        assert!(
            drifted.contains("refusing to mix vector spaces"),
            "{drifted}"
        );
        assert!(drifted.contains("384"), "{drifted}");
        assert!(drifted.contains("768"), "{drifted}");
        assert!(drifted.contains("--prefix"), "{drifted}");
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
            .pin_embedding_identity(&"a".repeat(64), true, None, None)
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
                "\",\"resumable\":true,\"dimensions\":0}\n"
            ),
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

    /// The prefix guard skips its catalog read only when the journal already
    /// belongs to this folder. A different root, a different prefix, and a
    /// missing journal all stay on the catalog-check path.
    #[test]
    fn journal_continues_folder_matches_only_the_recorded_run() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!journal_continues_folder(
            dir.path(),
            "root",
            "url",
            "prefix"
        ));
        drop(Journal::open(dir.path(), "root", "url", "prefix", 300, true).unwrap());
        assert!(journal_continues_folder(
            dir.path(),
            "root",
            "url",
            "prefix"
        ));
        assert!(!journal_continues_folder(
            dir.path(),
            "other-root",
            "url",
            "prefix"
        ));
        assert!(!journal_continues_folder(
            dir.path(),
            "root",
            "url",
            "other-prefix"
        ));
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
    /// Canonical path rank input. `None` identifies a legacy plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_symlink: Option<bool>,
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

/// Substring of a PDF junk reason that means the isolated parser process
/// never started. Kept here so the run summary and the spawn error cannot
/// drift: a family-wide failure is this reason on every PDF, with none indexed.
pub(crate) const PDF_WORKER_START_FAILURE: &str = "could not start the isolated PDF parser";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JunkFile {
    pub file_key: String,
    pub rel: String,
    pub format: String,
    pub status: String, // junk | skipped | stale-plan
    pub reason: String,
    pub bytes: u64,
}

fn is_pdf_format(format: &str) -> bool {
    format.strip_suffix("(gzip)").unwrap_or(format) == "pdf"
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DuplicateFile {
    /// Content identity of the canonical file.
    pub file_key: String,
    /// Root-relative alias path which was not indexed separately.
    pub rel: String,
    #[serde(default)]
    pub path_id: String,
    /// Whether this alias was reached through a followed symlink. `None`
    /// identifies legacy plans which cannot prove canonical rank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_symlink: Option<bool>,
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
    /// Datasets the server refused to map when the first generation was
    /// provisioned (#929). See [`RefusedDataset`].
    ///
    /// Skipped when empty ON PURPOSE: the serialized plan is hashed into the
    /// preparation contract and compared for equality on every no-op re-run, so
    /// a plan with nothing refused has to serialize exactly as it did before
    /// this field existed, or upgrading would turn every committed corpus into
    /// a spurious "plan changed" generation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_datasets: Vec<RefusedDataset>,
    /// Scalar fields installed on a frozen dataset after the first pass (#2176).
    ///
    /// Keyed by dataset slug. Empty — and omitted from the serialized plan —
    /// until a watched file grows a field the frozen `specs` do not name.
    /// Those specs stay frozen: `schema_identity`, `index_identity`, and the
    /// generation-to-generation dataset equality check all read them, and
    /// changing them aborts the next generation. The mapping update sends
    /// this list on top of the frozen specs. The server accepts the additive
    /// `PUT /_mapping`. A no-op re-run copies the list forward, so the plan
    /// compares equal and commits nothing.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub incremental_fields: BTreeMap<String, Vec<crate::infer::FieldSpec>>,
}

/// A dataset the plan inferred and the server refused to map (#929).
///
/// The refusal is part of the durable plan rather than a line in a log because
/// a generation that lacks a dataset must never be able to look complete. It
/// rides in the committed manifest, is republished in the catalog's run
/// document by every later generation, and keeps the run's exit code at 3 for
/// as long as the corpus still holds the files it cost.
///
/// `dataset` is the whole frozen definition, not just the slug: an incremental
/// run never re-clusters, so it needs the refused dataset's schema to recognise
/// a NEW file of the same shape and record it as refused too — instead of
/// aborting the whole run with "no frozen dataset accepts …".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefusedDataset {
    pub dataset: PlanDataset,
    /// The server's refusal, verbatim (`PUT /<index>/_mapping failed: 400 …`).
    pub reason: String,
    /// Content ids recorded in `junk_files` because of this refusal, sorted.
    pub file_keys: Vec<String>,
}

impl Plan {
    /// `Some(n)` when every PDF in this plan failed because the isolated
    /// parser could not start, and none was indexed (#2336).
    ///
    /// One corrupt PDF stays ordinary junk. A platform that cannot spawn the
    /// worker fails the whole family the same way, and exit 3 would report
    /// that as success. `n` is the number of PDF files.
    pub(crate) fn pdf_worker_unavailable_count(&self) -> Option<u64> {
        let indexed = self
            .files
            .values()
            .filter(|file| file.family == "pdf")
            .count();
        if indexed > 0 {
            return None;
        }
        let mut pdfs = 0u64;
        let mut start_failures = 0u64;
        for junk in &self.junk_files {
            if !is_pdf_format(&junk.format) {
                continue;
            }
            pdfs += 1;
            if junk.reason.contains(PDF_WORKER_START_FAILURE) {
                start_failures += 1;
            }
        }
        (pdfs > 0 && start_failures == pdfs).then_some(pdfs)
    }

    /// The run-document fields that say a corpus lacks a dataset (#929); empty
    /// when nothing was refused, so a run document with nothing to report stays
    /// byte-identical to the one earlier releases published — the generated
    /// path reads its catalog back and compares whole `_source`s.
    ///
    /// The detail travels as a JSON string, like `fields_json` on a dataset
    /// document: the catalog mapping is frozen, and an array of objects would
    /// have the engine infer a nested mapping for a field no release declared.
    pub fn refused_run_fields(&self) -> Vec<(&'static str, serde_json::Value)> {
        if self.refused_datasets.is_empty() {
            return Vec::new();
        }
        let refused: Vec<serde_json::Value> = self
            .refused_datasets
            .iter()
            .map(|refusal| {
                serde_json::json!({
                    "slug": refusal.dataset.slug,
                    "index": refusal.dataset.index,
                    "files": refusal.file_keys.len(),
                    "reason": refusal.reason,
                })
            })
            .collect();
        let files: usize = self
            .refused_datasets
            .iter()
            .map(|refusal| refusal.file_keys.len())
            .sum();
        vec![
            ("datasets_refused", serde_json::json!(refused.len())),
            ("files_refused", serde_json::json!(files)),
            (
                "refused_datasets_json",
                serde_json::Value::String(
                    serde_json::to_string(&refused).expect("a JSON value always serializes"),
                ),
            ),
        ]
    }
}

impl RefusedDataset {
    /// The `reason` every file this refusal cost is recorded under, in the plan
    /// and therefore in the catalog. One function so the genesis demotion and
    /// the incremental projection cannot drift apart: the two plans are compared
    /// byte for byte to decide whether a re-run changed anything.
    pub fn junk_reason(&self) -> String {
        format!(
            "dataset {} was refused by the server, so this file was not indexed: {}",
            self.dataset.slug, self.reason
        )
    }
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
    _state_lock: StateLock,
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
    /// Vector width pinned with the identity, when the record carried one.
    ///
    /// Older journals omit it. A resume that now reports a width is not drift
    /// for that reason alone (#2306); two recorded widths that disagree are.
    pub embedding_identity_dimensions: Option<usize>,
    /// Newest corpus reconciliation transaction without a durable
    /// `sync_commit`. Its desired plan is deliberately not authoritative yet.
    pub committed_manifest: Option<crate::sync::CommittedManifest>,
    pub pending_sync: Option<crate::sync::PendingSync>,
    pub legacy_migration_reasons: Vec<String>,
    /// The journal was resumed and its newest record was NOT a `finish`: the
    /// previous invocation was killed, or bailed, somewhere between its first
    /// publication and its summary. Anything a run only does at its very end
    /// — the corpus-wide graph pass — may therefore never have happened for
    /// files that are already journaled done, and the resuming run must not
    /// assume it did (email-thread carry-over, `lib.rs`).
    pub interrupted: bool,
}

/// Owns the process-wide state-directory exclusion lock.
///
/// Closing the file normally releases an `flock`, but a concurrently forked
/// child can temporarily retain the same open-file description until it
/// execs. The acquiring process explicitly unlocks before close so its
/// completed Journal cannot be retained by that child. An inherited copy must
/// only close: `LOCK_UN` on the shared open-file description would also release
/// the still-live parent's lock.
struct StateLock {
    file: std::fs::File,
    owner_pid: u32,
}

impl Drop for StateLock {
    fn drop(&mut self) {
        if std::process::id() == self.owner_pid {
            let _ = fs2::FileExt::unlock(&self.file);
        }
    }
}

/// File name of the `--watch` session ownership lock, beside `.autoindex.lock`.
const WATCH_OWNER_LOCK: &str = ".autoindex.watch.lock";

/// Session-long ownership of a `--watch` run's state directory (#1232).
///
/// The per-pass [`StateLock`] on `.autoindex.lock` only protects a single pass:
/// a pass's [`Journal`] drops when the pass ends, and the watch loop then parks
/// in its idle wait holding no lock at all. That window let a second
/// `xerj autoindex --watch` for the same source root, endpoint, prefix and
/// state directory complete its own initial pass and stay parked beside the
/// first — two live owners with duplicate filesystem subscriptions doing
/// redundant reconciliation.
///
/// A watch session holds this lock for its whole lifetime so only one native
/// watcher can own a journal and destination at a time. Keep the guard alive
/// for as long as the session runs.
pub struct WatchOwnership {
    _lock: StateLock,
}

/// Take session-long `--watch` ownership of `state_dir`, or refuse if another
/// live watcher already holds it (#1232).
///
/// The lock lives in a SEPARATE file from the per-pass `.autoindex.lock` on
/// purpose: the session's own passes still take that per-pass lock, and `flock`
/// treats two open file descriptions for one path as independent even within a
/// single process, so sharing the file would make the first pass block forever
/// against the session lock it already holds.
///
/// `flock` ownership is released by the kernel on process death and by the
/// explicit unlock on graceful drop, so a crashed or killed watcher frees
/// ownership without leaving a stale marker and without touching any persistent
/// data — exactly the lifecycle the report asked for. A `WouldBlock` contention
/// becomes a clear, bounded refusal; any other error is returned as-is.
pub fn acquire_watch_ownership(state_dir: &Path) -> Result<WatchOwnership> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("create state dir {}", state_dir.display()))?;
    let lock_path = state_dir.join(WATCH_OWNER_LOCK);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open watch ownership lock {}", lock_path.display()))?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(WatchOwnership {
            _lock: StateLock {
                file,
                owner_pid: std::process::id(),
            },
        }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Err(anyhow::anyhow!(
            "another `xerj autoindex --watch` already owns {} (same source root, endpoint, \
             prefix and state directory). Only one native watcher may own a journal and its \
             destination at a time — stop the other watcher, or choose a different --state-dir. \
             Ownership frees automatically when that process exits.",
            state_dir.display()
        )),
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("lock watch ownership {}", lock_path.display()))),
    }
}

/// Per-process monotonic suffix for [`new_run_id`].
///
/// The timestamp component only has one-second resolution, so two runs started
/// in the same second by the same process used to produce the same `run_id`.
/// That is reachable from library consumers — `xerj brain` calls
/// `run_index_report` more than once in one process — and a collision is not
/// benign: the generation catalog keys managed documents on `run_id`, so the
/// second run inherits the first run's documents as its own generation.
static RUN_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Identity for a brand-new run: UTC second, process id, and a monotonic
/// per-process sequence number that makes same-second runs distinct.
fn new_run_id() -> String {
    run_id_in_second(&chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string())
}

/// Split out of [`new_run_id`] so the same-second case is testable without
/// depending on where a test happens to land inside a wall-clock second.
fn run_id_in_second(second: &str) -> String {
    format!(
        "run-{second}-{:04x}-{:04x}",
        std::process::id() & 0xffff,
        RUN_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff
    )
}

pub fn default_state_dir(root: &str, url: &str, prefix: &str) -> PathBuf {
    xerj_common::home::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".xerj")
        .join("autoindex")
        .join(crate::ids::state_key(root, url, prefix))
}

/// Whether `state_dir` already holds a journal run for this exact root, url,
/// and prefix.
///
/// A same-folder resume, an incremental re-run, and a locally refused
/// `--fresh` all re-enter through that journal. The prefix-collision guard
/// must not query the catalog on that path: those refusals are local, and
/// the HTTP fixtures require that a refused attempt perform only the
/// readiness GET and the read-only flood-stage probe (#2077). A different
/// folder does not satisfy this — the default state directory includes the
/// root, and an explicit state directory recorded for another root or
/// prefix compares unequal — so the catalog check still runs for it.
///
/// An absent, unreadable, or corrupt journal is not a continuation, and
/// neither is a journal whose `run` record names something else. This reads
/// the file and does not create the directory or take the state lock.
pub fn journal_continues_folder(state_dir: &Path, root: &str, url: &str, prefix: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(state_dir.join("journal.ndjson")) else {
        return false;
    };
    let mut saw_match = false;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        if value.get("kind").and_then(|kind| kind.as_str()) != Some("run") {
            continue;
        }
        let recorded_root = value.get("root").and_then(|v| v.as_str()).unwrap_or("");
        let recorded_url = value.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let recorded_prefix = value.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
        if recorded_root != root || recorded_url != url || recorded_prefix != prefix {
            return false;
        }
        saw_match = true;
    }
    saw_match
}

fn apply_legacy_plan(
    plan: Plan,
    committed: &mut Option<crate::sync::CommittedManifest>,
    migration_reasons: &mut Vec<String>,
) -> Result<()> {
    anyhow::ensure!(
        committed
            .as_ref()
            .is_none_or(|manifest| manifest.generation == 0),
        "legacy plan record cannot follow a committed generated manifest"
    );
    match crate::sync::CommittedManifest::bootstrap_legacy(plan)? {
        crate::sync::LegacyBootstrap::Ready(manifest) => {
            *committed = Some(*manifest);
            migration_reasons.clear();
        }
        crate::sync::LegacyBootstrap::MigrationRequired { reasons } => {
            *committed = None;
            *migration_reasons = reasons;
        }
    }
    Ok(())
}

#[cfg(test)]
fn consume_io_failpoint(what: &str, boundary: u8, journal_path: &Path) -> bool {
    let failpoint = if what == "file_done" {
        &FILE_DONE_IO_FAILPOINT
    } else if what.starts_with("sync_") {
        &SYNC_IO_FAILPOINT
    } else {
        return false;
    };
    let mut armed = failpoint.lock().unwrap();
    if armed
        .as_ref()
        .is_some_and(|(expected_boundary, expected_path)| {
            *expected_boundary == boundary && expected_path == journal_path
        })
    {
        *armed = None;
        true
    } else {
        false
    }
}

type PreflightReplay = (
    Option<Plan>,
    Option<crate::sync::CommittedManifest>,
    Option<crate::sync::PendingSync>,
    Vec<String>,
    bool,
);

/// Read the last durable plan without locking, repairing, truncating or
/// appending to the journal. This is a preflight hint only: `Journal::open`
/// remains the authority after the caller passes its fail-closed inventory
/// gate and acquires the state lock.
fn read_plan_for_preflight(
    path: &Path,
    root: &str,
    url: &str,
    prefix: &str,
) -> Result<PreflightReplay> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, None, None, Vec::new(), false));
        }
        Err(error) => return Err(error.into()),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut plan = None;
    let mut committed_manifest: Option<crate::sync::CommittedManifest> = None;
    let mut pending_sync: Option<crate::sync::PendingSync> = None;
    let mut legacy_migration_reasons = Vec::new();
    // #585 case 2: set (and never cleared) the moment a durable
    // `sync_bootstrap` record replays. `begin_non_graph_generation` is the
    // only caller of `sync_bootstrap_genesis`, and only under `cfg.no_graph`
    // — so this is the one durable fact that tells a generation-0,
    // empty-groups `committed_manifest` reached via generation-format
    // records (an interrupted --no-graph genesis) apart from the same shape
    // reached via a legacy graph-path `write_plan(Plan::default())` (an
    // empty legacy plan, never no-graph). Genesis itself cannot carry this:
    // `validate_genesis` requires `execution: None`.
    let mut no_graph_genesis = false;
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
        let record_bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes.as_slice());
        let value = crate::sync::decode_unique_durable_json(record_bytes).with_context(|| {
            format!(
                "journal corruption at byte {record_start} in {}: malformed \
                         newline-terminated record. Refusing to discard later records. Restore \
                         the journal from a backup, or truncate it to exactly {record_start} \
                         bytes to keep every completion recorded before the corruption. Deleting \
                         the whole journal (or rerunning with --fresh) also recovers, but \
                         re-extracts and re-embeds the entire corpus — and it discards any \
                         durable generation records in this journal along with it, which is \
                         why --fresh is reachable here at all. For an isolated rebuild use a \
                         new --state-dir, new --prefix, and new --brain when graph detection \
                         is enabled (or --no-graph); explicitly validate and clean the shared \
                         catalog and old target",
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
                anyhow::ensure!(
                    pending_sync.is_none(),
                    "legacy plan record cannot appear while a sync is pending"
                );
                if let Some(encoded) = value.get("plan") {
                    let decoded: Plan = serde_json::from_value(encoded.clone())
                        .with_context(|| format!("decode durable plan in {}", path.display()))?;
                    plan = Some(decoded.clone());
                    apply_legacy_plan(
                        decoded,
                        &mut committed_manifest,
                        &mut legacy_migration_reasons,
                    )?;
                }
            }
            Some(kind) if crate::sync::is_sync_record_kind(kind) => {
                // Same contract as the authoritative replay in
                // `open_after_preflight` (#283): this preflight runs first, so
                // an invariant that leaks here bare is the one the user sees.
                crate::sync::replay_durable_record(
                    &value,
                    record_bytes,
                    &mut committed_manifest,
                    &mut pending_sync,
                )
                .with_context(|| {
                    format!(
                        "the durable {kind} record at byte {record_start} in {} failed to \
                         replay; the generation journal is not internally consistent, \
                         and re-running will not repair it. No remote data was changed. \
                         Rebuild with a new --state-dir and a new --prefix",
                        path.display()
                    )
                })?;
                if matches!(kind, "sync_bootstrap" | "sync_commit") {
                    plan = committed_manifest
                        .as_ref()
                        .map(|manifest| manifest.plan.clone());
                }
                if kind == "sync_bootstrap" {
                    legacy_migration_reasons.clear();
                    no_graph_genesis = true;
                }
            }
            Some(kind) if kind.starts_with("sync_") => {
                anyhow::bail!("unknown durable sync record kind {kind}");
            }
            _ => {}
        }
    }
    Ok((
        plan,
        committed_manifest,
        pending_sync,
        legacy_migration_reasons,
        no_graph_genesis,
    ))
}

pub struct JournalPreflight {
    state_dir: PathBuf,
    state_lock: StateLock,
    pub plan: Option<Plan>,
    pub committed_manifest: Option<crate::sync::CommittedManifest>,
    pub pending_sync: Option<crate::sync::PendingSync>,
    pub legacy_migration_reasons: Vec<String>,
    pub journal_exists: bool,
    /// Set only under `--fresh`, when the durable plan could not be read and
    /// the run is about to discard the journal anyway. The caller owns the
    /// decision to print it (autoindex honours `--quiet`).
    pub unreadable_plan: Option<String>,
    /// #585 case 2: `true` once a durable `sync_bootstrap` record has
    /// replayed. The only caller of `sync_bootstrap_genesis` is
    /// `begin_non_graph_generation`, and only under `cfg.no_graph`, so this
    /// is what lets a caller tell a generation-0/empty-groups
    /// `committed_manifest` reached that shape via an interrupted --no-graph
    /// genesis apart from the identical shape a legacy graph-path
    /// `write_plan(Plan::default())` can also produce (never no-graph).
    /// Meaningless unless `committed_manifest` is itself genesis-shaped.
    pub no_graph_genesis: bool,
}

fn embedding_resume_refused(reason: &str) -> String {
    format!(
        "server embedding backend cannot safely resume semantic indexing: {reason}. \
         No remote mutation was attempted. Documents already indexed were kept. \
         If the vector width was not confirmed, make the embedding endpoint reachable \
         and rerun the same command — already-embedded documents are reused once the \
         same width is confirmed. Restore the exact original embedding identity, or \
         rebuild all vectors with --fresh and a new --prefix only when the backend, \
         model, endpoint, or vector width changed. Before reusing the old prefix, \
         delete and recreate its prior autoindex indices"
    )
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
        let state_lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        state_lock_file.try_lock_exclusive().with_context(|| {
            format!(
                "autoindex state {} is already in use by another process; wait for it to \
                 finish or choose a different --state-dir",
                state_dir.display()
            )
        })?;
        // Capture the acquiring process before any later fallible work. A
        // forked child inherits this value and therefore closes without
        // unlocking the parent's shared open-file description.
        let state_lock = StateLock {
            file: state_lock_file,
            owner_pid: std::process::id(),
        };
        let journal_path = state_dir.join("journal.ndjson");
        let journal_exists = journal_path.exists();
        let (
            plan,
            committed_manifest,
            pending_sync,
            legacy_migration_reasons,
            no_graph_genesis,
            unreadable_plan,
        ) = match read_plan_for_preflight(&journal_path, root, url, prefix) {
            Ok((plan, committed, pending, reasons, no_graph_genesis)) => {
                (plan, committed, pending, reasons, no_graph_genesis, None)
            }
            // The journal is about to be deleted unread. Losing the plan here
            // costs the removed-file gate its comparison basis, which is
            // exactly what a full in-place rebuild accepts; the alternative is
            // a state directory that no supported flag can recover.
            //
            // Every generated record is discarded with it, so the durable
            // generation this run might otherwise have refused to overwrite is
            // reported as absent rather than half-parsed. That is what keeps
            // `--fresh` genuinely reachable here: the caller's generation
            // refusal reads `committed_manifest`/`pending_sync`, and firing it
            // off a partially-replayed journal would contradict the recovery
            // this very error recommends.
            Err(error) if fresh => (
                None,
                None,
                None,
                Vec::new(),
                false,
                Some(format!("{error:#}")),
            ),
            Err(error) => return Err(error),
        };
        Ok(JournalPreflight {
            state_dir: state_dir.to_owned(),
            state_lock,
            plan,
            committed_manifest,
            pending_sync,
            legacy_migration_reasons,
            journal_exists,
            unreadable_plan,
            no_graph_genesis,
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
        // `None` = no identity record yet. `Some(None)` = a record that omitted
        // the width. `Some(Some(n))` = a recorded width. Absence and a width
        // in the same journal disagree; two different widths disagree.
        let mut embedding_identity_dimensions_seen: Option<Option<usize>> = None;
        let mut committed_manifest: Option<crate::sync::CommittedManifest> = None;
        let mut pending_sync: Option<crate::sync::PendingSync> = None;
        let mut legacy_migration_reasons = Vec::new();
        let mut run_id = None;
        let mut resumed = false;
        let mut last_kind_was_finish = false;
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
                let record_bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes.as_slice());
                match crate::sync::decode_unique_durable_json(record_bytes) {
                    Ok(v) if newline_terminated => {
                        valid_end += read as u64;
                        last_kind_was_finish =
                            v.get("kind").and_then(|k| k.as_str()) == Some("finish");
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
                                anyhow::ensure!(
                                    pending_sync.is_none(),
                                    "legacy plan record at byte {record_start} cannot appear while \
                                     a sync is pending"
                                );
                                if let Some(p) = v.get("plan") {
                                    let decoded: Plan = serde_json::from_value(p.clone())
                                        .with_context(|| {
                                            format!(
                                                "decode durable plan record at byte {record_start} \
                                                 in {}",
                                                jpath.display()
                                            )
                                        })?;
                                    plan = Some(decoded.clone());
                                    apply_legacy_plan(
                                        decoded,
                                        &mut committed_manifest,
                                        &mut legacy_migration_reasons,
                                    )?;
                                } else {
                                    anyhow::bail!(
                                        "durable plan record at byte {record_start} in {} has no \
                                         plan payload",
                                        jpath.display()
                                    );
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
                                let has_dimensions = v
                                    .as_object()
                                    .is_some_and(|object| object.contains_key("dimensions"));
                                let keys_ok = v.as_object().is_some_and(|object| {
                                    let base = ["v", "kind", "identity_sha256", "resumable"]
                                        .iter()
                                        .all(|key| object.contains_key(*key));
                                    if has_dimensions {
                                        object.len() == 5 && base
                                    } else {
                                        object.len() == 4 && base
                                    }
                                });
                                let recorded_width = if has_dimensions {
                                    match v.get("dimensions").and_then(Value::as_u64) {
                                        Some(width)
                                            if width > 0 && usize::try_from(width).is_ok() =>
                                        {
                                            Some(width as usize)
                                        }
                                        _ => {
                                            anyhow::bail!(
                                                "journal at {} contains a malformed embedding \
                                                 identity; restore it from backup or re-run with \
                                                 --fresh",
                                                jpath.display()
                                            );
                                        }
                                    }
                                } else {
                                    None
                                };
                                if v.get("v").and_then(Value::as_u64) != Some(1)
                                    || digest.is_none()
                                    || resumable.is_none()
                                    || !keys_ok
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
                                    || embedding_identity_dimensions_seen
                                        .is_some_and(|existing| existing != recorded_width)
                                {
                                    anyhow::bail!(
                                        "journal at {} contains conflicting embedding identities; \
                                         restore it from backup or re-run with --fresh",
                                        jpath.display()
                                    );
                                }
                                embedding_identity_sha256 = digest.map(str::to_owned);
                                embedding_identity_resumable = resumable;
                                embedding_identity_dimensions_seen = Some(recorded_width);
                            }
                            Some(kind) if crate::sync::is_sync_record_kind(kind) => {
                                // A durable record that no longer re-validates
                                // is unrepairable by re-running; surface the
                                // rebuild route instead of a bare internal
                                // invariant, keeping the invariant as the
                                // cause (#283).
                                crate::sync::replay_durable_record(
                                    &v,
                                    record_bytes,
                                    &mut committed_manifest,
                                    &mut pending_sync,
                                )
                                .with_context(|| {
                                    format!(
                                        "the durable {kind} record at byte {record_start} in {} \
                                         failed to replay; the generation journal is not \
                                         internally consistent, and re-running will not repair \
                                         it. No remote data was changed. Rebuild with a new \
                                         --state-dir and a new --prefix",
                                        jpath.display()
                                    )
                                })?;
                                if matches!(kind, "sync_bootstrap" | "sync_commit") {
                                    plan = committed_manifest
                                        .as_ref()
                                        .map(|manifest| manifest.plan.clone());
                                }
                                if kind == "sync_bootstrap" {
                                    legacy_migration_reasons.clear();
                                }
                            }
                            Some(kind) if kind.starts_with("sync_") => {
                                anyhow::bail!(
                                    "unknown durable sync record kind {kind} at byte \
                                     {record_start} in {}",
                                    jpath.display()
                                );
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
                            // #2314: a file that left the folder is not a
                            // replacement. Drop its completion so a later
                            // catalog write does not advertise it again.
                            Some("file_removed") => {
                                if let Some(file_key) = v.get("file_key").and_then(Value::as_str) {
                                    done.remove(file_key);
                                    pending_replacements.remove(file_key);
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
                            newline-terminated record ({error:#}). Refusing to discard records \
                             after corruption. Restore the journal from a backup, or truncate it \
                             to exactly {record_start} bytes to keep every completion recorded \
                             before the corruption (files journaled after that point are \
                             re-verified, re-indexed and re-embedded on the next run). For an \
                             isolated rebuild use a new --state-dir, new --prefix, and new --brain \
                             when graph detection is enabled (or --no-graph); explicitly validate \
                             and clean the shared catalog and old target",
                            jpath.display()
                        );
                    }
                }
            }
        }
        let is_new = run_id.is_none();
        let run_id = run_id.unwrap_or_else(new_run_id);
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
            embedding_identity_dimensions: embedding_identity_dimensions_seen.flatten(),
            committed_manifest,
            pending_sync,
            legacy_migration_reasons,
            interrupted: resumed && !is_new && !last_kind_was_finish,
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
    ///
    /// `dimensions` is the confirmed vector width when the server has one.
    /// The key is omitted from the journal record when it is absent, so a
    /// journal written before widths were pinned still replays.
    pub fn pin_embedding_identity(
        &mut self,
        identity_sha256: &str,
        resumable: bool,
        dimensions: Option<usize>,
        non_resumable_reason: Option<&str>,
    ) -> Result<()> {
        if identity_sha256.len() != 64
            || !identity_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            anyhow::bail!("server returned an invalid embedding identity digest");
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
            match (self.embedding_identity_dimensions, dimensions) {
                (Some(stored), Some(current)) if stored != current => {
                    anyhow::bail!(
                        "embedding execution identity changed since this autoindex journal was \
                         created (vector width {stored} vs {current}); refusing to mix vector \
                         spaces. Restore the original embedding identity, or rebuild all vectors \
                         with --fresh and a new --prefix. Before reusing the old prefix, delete \
                         and recreate its prior autoindex indices"
                    );
                }
                (Some(stored), None) => {
                    anyhow::bail!(
                        "embedding execution identity changed since this autoindex journal was \
                         created (vector width {stored} is no longer reported); refusing to mix \
                         vector spaces. Restore the original embedding identity, or rebuild all \
                         vectors with --fresh and a new --prefix. Before reusing the old prefix, \
                         delete and recreate its prior autoindex indices"
                    );
                }
                _ => {}
            }
            if !resumable {
                // Same digest, still not safe to reuse vectors. A proxy whose
                // width was never confirmed lands here; the server's reason
                // says how to get a resume instead of a rebuild.
                anyhow::bail!(
                    "{}",
                    embedding_resume_refused(
                        non_resumable_reason.unwrap_or("embedding identity is not immutable")
                    )
                );
            }
            // Same digest, and the server can resume. Do not append another
            // identity record: replay treats a flipped `resumable` flag as
            // corruption. That is what lets a journal pinned before a proxy
            // width could be confirmed (#2306) continue once this process
            // confirms the same model and endpoint.
            return Ok(());
        }
        if self.resumed && !resumable {
            anyhow::bail!(
                "{}",
                embedding_resume_refused(
                    non_resumable_reason.unwrap_or("embedding identity is not immutable")
                )
            );
        }
        if self.resumed && !self.done.is_empty() {
            anyhow::bail!(
                "this semantic autoindex journal predates embedding identity pinning and cannot \
                 be resumed safely. Rebuild all vectors with --fresh and a new --prefix. Before \
                 reusing the old prefix, delete and recreate its prior autoindex indices"
            );
        }
        let mut record = serde_json::json!({
            "v": 1,
            "kind": "embedding_identity",
            "identity_sha256": identity_sha256,
            "resumable": resumable,
        });
        if let Some(width) = dimensions {
            record["dimensions"] = serde_json::json!(width);
        }
        self.append_transaction(&record, "embedding_identity")?;
        self.embedding_identity_sha256 = Some(identity_sha256.to_owned());
        self.embedding_identity_resumable = Some(resumable);
        self.embedding_identity_dimensions = dimensions;
        Ok(())
    }

    fn append_transaction(&mut self, v: &Value, what: &str) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let start = self.file.metadata()?.len();
        #[cfg(test)]
        if consume_io_failpoint(what, 1, &self.path) {
            anyhow::bail!("injected {what} append failure before any bytes");
        }
        #[cfg(test)]
        let partial_write = consume_io_failpoint(what, 3, &self.path);
        #[cfg(not(test))]
        let partial_write = false;
        let write_result = if partial_write {
            let written = line.len().min(23);
            self.file
                .write_all(&line.as_bytes()[..written])
                .and_then(|_| {
                    Err(std::io::Error::other(format!(
                        "injected partial {what} write after {written} bytes"
                    )))
                })
        } else {
            self.file.write_all(line.as_bytes())
        };
        if let Err(error) = write_result {
            return self.rollback_transaction(start, what, "append", error);
        }
        #[cfg(test)]
        let injected_sync_failure = consume_io_failpoint(what, 2, &self.path);
        #[cfg(not(test))]
        let injected_sync_failure = false;
        let sync_result = if injected_sync_failure {
            Err(std::io::Error::other(format!(
                "injected {what} fsync failure"
            )))
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
        anyhow::ensure!(
            self.pending_sync.is_none(),
            "legacy plan write cannot occur while a sync is pending"
        );
        anyhow::ensure!(
            self.committed_manifest
                .as_ref()
                .is_none_or(|manifest| manifest.generation == 0),
            "legacy plan write cannot follow a committed generated manifest"
        );
        let mut committed = self.committed_manifest.clone();
        let mut migration_reasons = self.legacy_migration_reasons.clone();
        apply_legacy_plan(plan.clone(), &mut committed, &mut migration_reasons)?;
        self.append_transaction(&serde_json::json!({"kind": "plan", "plan": plan}), "plan")?;
        self.plan = Some(plan.clone());
        self.committed_manifest = committed;
        self.legacy_migration_reasons = migration_reasons;
        Ok(())
    }

    pub fn sync_begin(&mut self, sync: &crate::sync::PendingSync) -> Result<()> {
        anyhow::ensure!(
            self.pending_sync.is_none(),
            "cannot begin sync {} while {} is still pending",
            sync.tx_id,
            self.pending_sync
                .as_ref()
                .map(|pending| pending.tx_id.as_str())
                .unwrap_or("another sync")
        );
        let committed = self
            .committed_manifest
            .as_ref()
            .context("cannot begin sync before a committed plan exists")?;
        let mut pending = None;
        crate::sync::apply_begin(committed, &mut pending, sync.clone())?;
        let mut value = serde_json::to_value(sync)?;
        value["kind"] = Value::String("sync_begin".into());
        self.append_transaction(&value, "sync_begin")?;
        self.pending_sync = pending;
        Ok(())
    }

    pub fn sync_bootstrap_genesis(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.pending_sync.is_none()
                && self.committed_manifest.is_none()
                && self.plan.is_none()
                && self.done.is_empty()
                && self.pending_replacements.is_empty(),
            "generation-zero bootstrap is only valid for an empty new journal"
        );
        let manifest = crate::sync::CommittedManifest::genesis()?;
        let mut committed = None;
        crate::sync::replay_record(
            &serde_json::json!({"kind": "sync_bootstrap", "manifest": manifest}),
            &mut committed,
            &mut None,
        )?;
        self.append_transaction(
            &serde_json::json!({"kind": "sync_bootstrap", "manifest": manifest}),
            "sync_bootstrap",
        )?;
        self.plan = Some(Plan::default());
        self.committed_manifest = committed;
        self.legacy_migration_reasons.clear();
        Ok(())
    }

    pub fn sync_operation_state(
        &mut self,
        operation_id: &str,
        state: crate::sync::SyncOperationState,
    ) -> Result<()> {
        // This runs twice per replayed operation (Started, then Committed).
        // The copy-on-write this used to do cloned the ENTIRE pending sync —
        // desired manifest, every group, every operation — per state write,
        // which on the 402,744-operation cve-records run was ~0.7 s of
        // client CPU per file and a 5.7 GB client RSS (#1224). The durable
        // journal line is the transaction boundary: validate the transition
        // first (pure), append durably, and only then mutate the in-memory
        // map. A crash between append and mutation loses nothing — the next
        // open replays the journal, which now carries the event; a failed
        // append returns before any mutation, same as the clone did.
        let pending = self
            .pending_sync
            .as_ref()
            .context("cannot record operation state without sync_begin")?;
        let tx_id = pending.tx_id.clone();
        pending.validate_operation_state(&tx_id, operation_id, &state)?;
        self.append_transaction(
            &serde_json::json!({
                "kind": "sync_operation_state",
                "tx_id": tx_id,
                "operation_id": operation_id,
                "state": state,
            }),
            "sync_operation_state",
        )?;
        if let Some(pending) = self.pending_sync.as_mut() {
            pending.record_operation_state(operation_id, state);
        }
        Ok(())
    }

    pub fn sync_validated(&mut self) -> Result<()> {
        let pending = self
            .pending_sync
            .as_ref()
            .context("cannot validate sync without sync_begin")?;
        let tx_id = pending.tx_id.clone();
        let digest = pending.desired_manifest_digest.clone();
        let mut candidate = pending.clone();
        candidate.apply_validated(&tx_id, &digest)?;
        self.append_transaction(
            &serde_json::json!({
                "kind": "sync_validated",
                "tx_id": tx_id,
                "desired_manifest_digest": digest,
            }),
            "sync_validated",
        )?;
        self.pending_sync = Some(candidate);
        Ok(())
    }

    pub fn sync_commit(&mut self) -> Result<()> {
        let pending = self
            .pending_sync
            .as_ref()
            .context("cannot commit sync without sync_begin")?;
        let tx_id = pending.tx_id.clone();
        let digest = pending.desired_manifest_digest.clone();
        let committed = pending.clone().apply_commit(&tx_id, &digest)?;
        self.append_transaction(
            &serde_json::json!({
                "kind": "sync_commit",
                "tx_id": tx_id,
                "desired_manifest_digest": digest,
            }),
            "sync_commit",
        )?;
        self.pending_sync = None;
        self.plan = Some(committed.plan.clone());
        self.committed_manifest = Some(committed);
        Ok(())
    }

    pub fn sync_abort(&mut self, reason: &str) -> Result<()> {
        let pending = self
            .pending_sync
            .as_ref()
            .context("cannot abort without pending sync")?;
        let tx_id = pending.tx_id.clone();
        let digest = pending.desired_manifest_digest.clone();
        self.append_transaction(
            &serde_json::json!({
                "kind": "sync_abort",
                "tx_id": tx_id,
                "desired_manifest_digest": digest,
                "reason": reason,
            }),
            "sync_abort",
        )?;
        self.pending_sync = None;
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

    /// A planned file left the folder and its documents were swept (#2314).
    ///
    /// Unlike [`Self::file_replace_start`], this does not schedule a
    /// replacement: there is no current file to republish.
    pub fn file_removed(&mut self, file_key: &str) -> Result<()> {
        self.append_transaction(
            &serde_json::json!({
                "kind": "file_removed",
                "file_key": file_key,
            }),
            "file_removed",
        )?;
        self.done.remove(file_key);
        self.pending_replacements.remove(file_key);
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
mod sync_journal_tests {
    use super::*;
    use crate::sync::{
        ExecutionIdentity, GenerationManifest, ManifestGroup, ManifestPath, PendingSync,
        SourceExecutionPolicy, SyncOperationState, EXECUTION_IDENTITY_VERSION,
    };

    fn journal_with_plan(dir: &Path) -> Journal {
        let mut journal = Journal::open(dir, "root", "url", "prefix", 300, false).unwrap();
        let mut plan = Plan::default();
        plan.datasets.push(PlanDataset {
            slug: "reports".into(),
            index: "prefix-reports".into(),
            family: "pdf".into(),
            group: None,
            specs: Vec::new(),
            time_field: None,
            semantic_field: None,
            text_analyzer: None,
            scope: None,
            sampled_records: 1,
            file_count: 0,
        });
        journal.sync_bootstrap_genesis().unwrap();
        journal
    }

    fn pending(journal: &Journal) -> PendingSync {
        let base = journal.committed_manifest.as_ref().unwrap();
        let mut plan = base.plan.clone();
        if plan.datasets.is_empty() {
            plan.datasets.push(PlanDataset {
                slug: "reports".into(),
                index: "prefix-reports".into(),
                family: "pdf".into(),
                group: None,
                specs: Vec::new(),
                time_field: None,
                semantic_field: None,
                text_analyzer: None,
                scope: None,
                sampled_records: 1,
                file_count: 1,
            });
        } else {
            plan.datasets[0].file_count = 1;
        }
        plan.files.insert(
            "content-a".into(),
            FileAssignment {
                rel: "a.pdf".into(),
                path_id: "unix:61".into(),
                is_symlink: Some(false),
                family: "pdf".into(),
                gzip: false,
                content_digest: Some("digest-a".into()),
                assignments: vec![(None, "reports".into())],
                as_document: false,
            },
        );
        PendingSync::new(
            "sync-1".into(),
            base,
            GenerationManifest {
                generation: base.generation + 1,
                execution: Some(ExecutionIdentity {
                    version: EXECUTION_IDENTITY_VERSION,
                    root_identity: "unix:root".into(),
                    url: "url".into(),
                    prefix: "prefix".into(),
                    follow_symlinks: false,
                    chunker_identity: "chunker-v1".into(),
                    embedding_identity_sha256: "a".repeat(64),
                    embedding_backend: "lexical".into(),
                    embedding_dimension: Some(384),
                    embedding_semantic_contract: "semantic_text-derived-vector.v1".into(),
                    embedding_resumable: true,
                    graph_enabled: false,
                    brain: "disabled".into(),
                    detector_identity: "disabled".into(),
                    schema_identity: "schema-v1".into(),
                    index_identity: "index-v1".into(),
                    source_policy: SourceExecutionPolicy::AbortOnSourceChange {
                        inventory_digest: "inventory-v1".into(),
                    },
                }),
                plan,
                groups: vec![ManifestGroup {
                    group_id: "group-a".into(),
                    content_id: "content-a".into(),
                    content_digest: "digest-a".into(),
                    content_size: 10,
                    canonical: ManifestPath {
                        path_id: "unix:61".into(),
                        rel: "a.pdf".into(),
                        is_symlink: false,
                    },
                    aliases: Vec::new(),
                    dataset_slugs: vec!["reports".into()],
                    expected_records: 1,
                    expected_passages: 1,
                    expected_vectors: 1,
                    expected_junk_records: 0,
                    expected_records_by_dataset: std::collections::BTreeMap::new(),
                }],
            },
        )
        .unwrap()
    }

    fn historical_issue_283_journal(sync_begin: &str) -> Vec<u8> {
        let run = serde_json::json!({
            "v": 1,
            "kind": "run",
            "root": "root",
            "url": "url",
            "prefix": "prefix",
            "bulk_timeout_secs": 300,
            "run_id": "run-issue-283-fixture",
            "started": "2026-08-10T00:00:00Z"
        });
        let bootstrap = serde_json::json!({
            "kind": "sync_bootstrap",
            "manifest": crate::sync::CommittedManifest::genesis().unwrap(),
        });
        format!("{run}\n{bootstrap}\n{sync_begin}\n").into_bytes()
    }

    fn historical_issue_283_begin() -> String {
        // Mechanical whitespace removal preserves the checked-in decimal
        // token; parsing it here would erase the historical boundary.
        include_str!("../tests/fixtures/issue_283_sync_begin.json")
            .lines()
            .collect()
    }

    fn assert_preflight_and_authoritative_open_reject(
        valid_journal: &[u8],
        invalid_journal: &[u8],
        marker: &str,
    ) {
        let preflight_dir = tempfile::tempdir().unwrap();
        std::fs::write(preflight_dir.path().join("journal.ndjson"), invalid_journal).unwrap();
        let error = Journal::preflight(preflight_dir.path(), "root", "url", "prefix", false)
            .err()
            .expect("preflight must reject the raw durable mutation");
        assert!(format!("{error:#}").contains(marker), "{error:#}");

        // Pass preflight over valid bytes, then mutate the journal while its
        // lock remains held. This directly exercises the authoritative replay
        // in open_after_preflight rather than failing in preflight twice.
        let open_dir = tempfile::tempdir().unwrap();
        let path = open_dir.path().join("journal.ndjson");
        std::fs::write(&path, valid_journal).unwrap();
        let preflight =
            Journal::preflight(open_dir.path(), "root", "url", "prefix", false).unwrap();
        std::fs::write(&path, invalid_journal).unwrap();
        let error = Journal::open_after_preflight(preflight, "root", "url", "prefix", 300, false)
            .err()
            .expect("authoritative open must reject the raw durable mutation");
        assert!(format!("{error:#}").contains(marker), "{error:#}");
    }

    #[test]
    fn raw_sync_begin_mutations_fail_at_preflight_and_authoritative_open() {
        let begin = historical_issue_283_begin();
        let valid = historical_issue_283_journal(&begin);
        let cases = [
            (
                "desired payload",
                begin.replacen("245.44444444444446", "245.44444444444449", 1),
                "desired manifest digest does not match sync_begin payload",
            ),
            (
                "operation list",
                begin.replacen(
                    "\"operations\": []",
                    "\"operations\": [{\"operation_id\":\"delete:ghost\",\"kind\":\"delete\",\"group_id\":\"ghost\"}]",
                    1,
                ),
                "sync operation list was not derived exactly",
            ),
            (
                "desired digest",
                begin.replacen(
                    "axm1-5e314eb29c9617433af240313b94e544",
                    "axm1-5e314eb29c9617433af240313b94e545",
                    1,
                ),
                "desired manifest digest does not match sync_begin payload",
            ),
            (
                "execution identity",
                begin.replacen("\"chunker_identity\": \"chunker-v1\"", "\"chunker_identity\": \"chunker-v2\"", 1),
                "desired manifest digest does not match sync_begin payload",
            ),
        ];
        for (label, mutated, marker) in cases {
            assert_ne!(mutated, begin, "mutation did not apply: {label}");
            assert_preflight_and_authoritative_open_reject(
                &valid,
                &historical_issue_283_journal(&mutated),
                marker,
            );
        }
    }

    #[test]
    fn raw_content_identity_mutation_fails_at_preflight_and_authoritative_open() {
        let source = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(source.path());
        journal.sync_begin(&pending(&journal)).unwrap();
        drop(journal);
        let valid = std::fs::read(source.path().join("journal.ndjson")).unwrap();
        let valid_text = String::from_utf8(valid.clone()).unwrap();
        // Target only the ManifestGroup field. The plan's FileAssignment has
        // the same digest text but a different following field.
        let mutated_text = valid_text.replacen(
            "\"content_digest\":\"digest-a\",\"content_size\":10,\"canonical\"",
            "\"content_digest\":\"digest-b\",\"content_size\":10,\"canonical\"",
            1,
        );
        assert_ne!(mutated_text, valid_text, "content mutation did not apply");
        assert_preflight_and_authoritative_open_reject(
            &valid,
            mutated_text.as_bytes(),
            "plan canonical projection disagrees with manifest group",
        );
    }

    #[test]
    fn duplicate_kind_is_rejected_before_last_key_can_bypass_sync_routing() {
        let valid = concat!(
            "{\"v\":1,\"kind\":\"run\",\"root\":\"root\",\"url\":\"url\",",
            "\"prefix\":\"prefix\",\"run_id\":\"valid\"}\n"
        );
        let duplicate = concat!(
            "{\"v\":1,\"kind\":\"sync_begin\",\"kind\":\"run\",",
            "\"root\":\"root\",\"url\":\"url\",\"prefix\":\"prefix\",",
            "\"run_id\":\"ambiguous\"}\n"
        );
        assert_preflight_and_authoritative_open_reject(
            valid.as_bytes(),
            duplicate.as_bytes(),
            "duplicate JSON object key \"kind\"",
        );
    }

    #[test]
    fn sync_begin_is_pending_and_never_promotes_the_committed_manifest() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(dir.path());
        let desired = pending(&journal);
        journal.sync_begin(&desired).unwrap();
        let before = std::fs::read(dir.path().join("journal.ndjson")).unwrap();
        assert!(journal.write_plan(&Plan::default()).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("journal.ndjson")).unwrap(),
            before
        );
        assert_eq!(journal.committed_manifest.as_ref().unwrap().generation, 0);
        assert!(!journal.plan.as_ref().unwrap().alias_paths_indexed);
        drop(journal);

        let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 0);
        assert!(!reopened.plan.as_ref().unwrap().alias_paths_indexed);
        assert_eq!(reopened.pending_sync.as_ref().unwrap().tx_id, "sync-1");
    }

    #[test]
    fn new_journal_can_bootstrap_empty_generation_authority_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        journal.sync_bootstrap_genesis().unwrap();
        let committed = journal.committed_manifest.as_ref().unwrap();
        assert_eq!(committed.generation, 0);
        assert!(committed.groups.is_empty());
        assert_eq!(
            serde_json::to_value(journal.plan.as_ref().unwrap()).unwrap(),
            serde_json::to_value(Plan::default()).unwrap()
        );
        assert!(journal.sync_bootstrap_genesis().is_err());
        drop(journal);

        let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert_eq!(reopened.committed_manifest.unwrap().generation, 0);
        assert_eq!(
            serde_json::to_value(reopened.plan.unwrap()).unwrap(),
            serde_json::to_value(Plan::default()).unwrap()
        );
    }

    #[test]
    fn replay_rejects_sync_begin_followed_by_legacy_plan_without_changing_authority() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(dir.path());
        journal.sync_begin(&pending(&journal)).unwrap();
        let committed_digest = journal
            .committed_manifest
            .as_ref()
            .unwrap()
            .manifest_digest
            .clone();
        drop(journal);
        let path = dir.path().join("journal.ndjson");
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(
            format!(
                "{}\n",
                serde_json::json!({"kind": "plan", "plan": Plan::default()})
            )
            .as_bytes(),
        )
        .unwrap();
        file.sync_data().unwrap();
        drop(file);
        let error = Journal::open(dir.path(), "root", "url", "prefix", 300, false)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("while a sync is pending"));
        let preflight = Journal::preflight(dir.path(), "root", "url", "prefix", false)
            .err()
            .unwrap();
        assert!(format!("{preflight:#}").contains("while a sync is pending"));
        // The rejected record cannot manufacture a sync_commit; the durable
        // committed digest remains present only in the earlier sync_begin base.
        let text = std::fs::read_to_string(dir.path().join("journal.ndjson")).unwrap();
        assert!(text.contains(&committed_digest));
        assert!(!text.contains("\"kind\":\"sync_commit\""));
    }

    #[test]
    fn only_validated_all_committed_sync_advances_authority_after_restart() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(dir.path());
        journal.sync_begin(&pending(&journal)).unwrap();
        assert!(journal.sync_validated().is_err());
        assert!(journal
            .sync_operation_state("upsert:group-a", SyncOperationState::Committed)
            .is_err());
        journal
            .sync_operation_state("upsert:group-a", SyncOperationState::Started)
            .unwrap();
        journal
            .sync_operation_state("upsert:group-a", SyncOperationState::Committed)
            .unwrap();
        journal.sync_validated().unwrap();
        journal.sync_commit().unwrap();
        assert_eq!(journal.committed_manifest.as_ref().unwrap().generation, 1);
        assert!(journal
            .plan
            .as_ref()
            .unwrap()
            .files
            .contains_key("content-a"));
        assert!(journal.pending_sync.is_none());
        let before = std::fs::read(dir.path().join("journal.ndjson")).unwrap();
        assert!(journal.write_plan(&Plan::default()).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("journal.ndjson")).unwrap(),
            before
        );
        drop(journal);

        let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 1);
        assert!(reopened
            .plan
            .as_ref()
            .unwrap()
            .files
            .contains_key("content-a"));
        assert!(reopened.pending_sync.is_none());
    }

    #[test]
    fn digest_bound_abort_discards_pending_without_advancing_authority() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(dir.path());
        journal.sync_begin(&pending(&journal)).unwrap();
        journal.sync_abort("source changed").unwrap();
        assert!(journal.pending_sync.is_none());
        assert_eq!(journal.committed_manifest.as_ref().unwrap().generation, 0);
        drop(journal);
        let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert!(reopened.pending_sync.is_none());
        assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 0);
    }

    #[test]
    fn unknown_sync_namespace_record_fails_closed_but_preflight_exposes_pending() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(dir.path());
        journal.sync_begin(&pending(&journal)).unwrap();
        drop(journal);
        let preflight = Journal::preflight(dir.path(), "root", "url", "prefix", false).unwrap();
        assert_eq!(preflight.committed_manifest.as_ref().unwrap().generation, 0);
        assert_eq!(preflight.pending_sync.as_ref().unwrap().tx_id, "sync-1");
        drop(preflight);

        let path = dir.path().join("journal.ndjson");
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"{\"kind\":\"sync_future_v99\"}\n").unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(Journal::open(dir.path(), "root", "url", "prefix", 300, false).is_err());
    }

    #[test]
    fn out_of_order_or_malformed_known_sync_records_fail_closed() {
        for record in [
            "{\"kind\":\"sync_commit\",\"tx_id\":\"none\",\"desired_manifest_digest\":\"none\"}\n",
            "{\"kind\":\"sync_begin\"}\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            drop(journal_with_plan(dir.path()));
            let path = dir.path().join("journal.ndjson");
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(record.as_bytes()).unwrap();
            file.sync_data().unwrap();
            let error = Journal::open(dir.path(), "root", "url", "prefix", 300, false)
                .err()
                .unwrap();
            let message = format!("{error:#}");
            assert!(
                message.contains("pending sync") || message.contains("sync_begin"),
                "{message}"
            );
        }
    }

    #[test]
    fn torn_sync_tail_is_removed_without_creating_a_pending_generation() {
        let dir = tempfile::tempdir().unwrap();
        drop(journal_with_plan(dir.path()));
        let path = dir.path().join("journal.ndjson");
        let durable_len = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(br#"{"kind":"sync_begin","tx_id":"torn""#)
            .unwrap();
        file.sync_data().unwrap();
        drop(file);

        let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert!(reopened.pending_sync.is_none());
        assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 0);
        // Reopen appends one resume record, but the torn bytes are absent.
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(!contents.contains("\"tx_id\":\"torn\""));
        assert!(contents.len() as u64 > durable_len);
    }

    #[test]
    fn complete_middle_corruption_is_not_truncated_or_skipped() {
        let dir = tempfile::tempdir().unwrap();
        drop(journal_with_plan(dir.path()));
        let path = dir.path().join("journal.ndjson");
        let before = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{malformed sync record}\n").unwrap();
        file.write_all(b"{\"kind\":\"unknown-after-corruption\"}\n")
            .unwrap();
        file.sync_data().unwrap();
        drop(file);
        assert!(Journal::open(dir.path(), "root", "url", "prefix", 300, false).is_err());
        assert!(std::fs::metadata(path).unwrap().len() > before);
    }

    #[test]
    fn unknown_future_records_do_not_change_legacy_completion_replay() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal_with_plan(dir.path());
        journal
            .file_done(&FileDone {
                file_key: "legacy".into(),
                path: "legacy.pdf".into(),
                records: 1,
                junk: 0,
                bytes: 10,
                dropped_by_dataset: std::collections::HashMap::new(),
                generation: None,
            })
            .unwrap();
        drop(journal);
        let path = dir.path().join("journal.ndjson");
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"{\"kind\":\"extension_future_v99\",\"payload\":{\"x\":1}}\n")
            .unwrap();
        file.sync_data().unwrap();
        drop(file);

        let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
        assert!(reopened.done.contains_key("legacy"));
        assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 0);
    }

    #[test]
    fn sync_append_and_fsync_failures_roll_back_without_phantom_pending_state() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for boundary in [1, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let mut journal = journal_with_plan(dir.path());
            let desired = pending(&journal);
            let before = std::fs::read(dir.path().join("journal.ndjson")).unwrap();
            fail_next_sync_io(boundary, journal.path());
            assert!(journal.sync_begin(&desired).is_err());
            assert!(journal.pending_sync.is_none());
            drop(journal);
            assert_eq!(
                std::fs::read(dir.path().join("journal.ndjson")).unwrap(),
                before
            );
            let reopened = Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
            assert!(reopened.pending_sync.is_none());
            assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 0);
        }
    }

    #[test]
    fn every_later_sync_record_rolls_back_append_partial_and_fsync_failures() {
        let _guard = SYNC_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for stage in ["operation", "validated", "commit", "abort"] {
            for boundary in [1, 2, 3] {
                let dir = tempfile::tempdir().unwrap();
                let mut journal = journal_with_plan(dir.path());
                journal.sync_begin(&pending(&journal)).unwrap();
                if stage != "operation" {
                    journal
                        .sync_operation_state("upsert:group-a", SyncOperationState::Started)
                        .unwrap();
                    journal
                        .sync_operation_state("upsert:group-a", SyncOperationState::Committed)
                        .unwrap();
                }
                if matches!(stage, "commit") {
                    journal.sync_validated().unwrap();
                }
                let before = std::fs::read(dir.path().join("journal.ndjson")).unwrap();
                fail_next_sync_io(boundary, journal.path());
                let result = match stage {
                    "operation" => {
                        journal.sync_operation_state("upsert:group-a", SyncOperationState::Started)
                    }
                    "validated" => journal.sync_validated(),
                    "commit" => journal.sync_commit(),
                    "abort" => journal.sync_abort("test abort"),
                    _ => unreachable!(),
                };
                assert!(result.is_err(), "{stage} boundary {boundary}");
                assert_eq!(
                    std::fs::read(dir.path().join("journal.ndjson")).unwrap(),
                    before,
                    "{stage} boundary {boundary}"
                );
                drop(journal);
                let reopened =
                    Journal::open(dir.path(), "root", "url", "prefix", 300, false).unwrap();
                assert_eq!(reopened.committed_manifest.as_ref().unwrap().generation, 0);
                assert!(reopened.pending_sync.is_some());
            }
        }
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
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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
        // keeps prior completions; an exact rebuild needs isolated state and
        // destination identities.
        assert!(message.contains("truncate it to exactly"));
        assert!(message.contains("new --state-dir, new --prefix, and new --brain"));
    }

    #[test]
    fn stale_generation_completion_cannot_clear_newer_pending_replacement() {
        // Reaches file_done → must hold the failpoint lock (see above).
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for boundary in [1, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let mut journal =
                Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
            journal.file_replace_start("file-key", "new").unwrap();
            fail_next_file_done_io(boundary, journal.path());
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
        let _guard = FILE_DONE_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            Journal::open(dir.path(), "root", "http://engine", "ax", 300, false).unwrap();
        journal
            .file_replace_start("file-c", "generation-c")
            .unwrap();
        fail_next_file_done_io(3, journal.path());
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

#[cfg(test)]
mod pdf_worker_unavailable_tests {
    use super::{FileAssignment, JunkFile, Plan, PDF_WORKER_START_FAILURE};

    fn junk(format: &str, reason: &str) -> JunkFile {
        JunkFile {
            file_key: format!("key-{format}"),
            rel: format!("f.{format}"),
            format: format.to_string(),
            status: "junk".into(),
            reason: reason.into(),
            bytes: 1,
        }
    }

    fn indexed_pdf() -> FileAssignment {
        FileAssignment {
            rel: "a.pdf".into(),
            path_id: "a.pdf".into(),
            is_symlink: Some(false),
            family: "pdf".into(),
            gzip: false,
            content_digest: None,
            assignments: Vec::new(),
            as_document: false,
        }
    }

    #[test]
    fn every_pdf_start_failure_counts_gzip_members_and_ignores_other_formats() {
        let mut plan = Plan::default();
        let reason = format!("extract failed: {PDF_WORKER_START_FAILURE} for quarterly.pdf");
        plan.junk_files.push(junk("pdf", &reason));
        plan.junk_files.push(junk("pdf(gzip)", &reason));
        plan.junk_files.push(junk("txt", "unreadable"));
        assert_eq!(plan.pdf_worker_unavailable_count(), Some(2));
    }

    #[test]
    fn a_parsed_pdf_or_a_different_failure_stays_ordinary_junk() {
        let mut mixed = Plan::default();
        mixed.junk_files.push(junk("pdf", PDF_WORKER_START_FAILURE));
        mixed.junk_files.push(junk("pdf", "damaged"));
        assert_eq!(mixed.pdf_worker_unavailable_count(), None);

        let mut indexed = Plan::default();
        indexed.files.insert("k".into(), indexed_pdf());
        indexed
            .junk_files
            .push(junk("pdf", PDF_WORKER_START_FAILURE));
        assert_eq!(indexed.pdf_worker_unavailable_count(), None);

        assert_eq!(Plan::default().pdf_worker_unavailable_count(), None);
    }
}
