//! Unit tests for `xerj hub sync` (plan §8): every one runs on throwaway
//! directories with a scripted [`SyncOps`], so no node, network or port is
//! involved. `tests/sync_e2e.rs` covers the same flow against real nodes and the
//! real `hub pull`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use tempfile::tempdir;
use xerj_common::hub::{LicenceClass, PackSource, Sketch, PACK_FORMAT_VERSION};

use super::*;

// ── fixtures ─────────────────────────────────────────────────────────────────

fn full_sha(pin8: &str) -> String {
    format!("{pin8}{}", "0".repeat(32))
}

/// A complete extracted pack: `pack.json`, a payload file, a snapshot member and
/// the `SHA256SUMS` that lists them.
fn make_pack(hub: &Path, slug: &str, pin8: &str, pin_date: &str) -> PathBuf {
    let dir = hub.join("packs").join(slug).join(pin8);
    fs::create_dir_all(dir.join("snapshot")).unwrap();
    let manifest = PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: slug.to_string(),
        pin_sha: full_sha(pin8),
        pin_date: pin_date.to_string(),
        built_at: "2026-10-04T00:00:00Z".to_string(),
        builder_version: "test".to_string(),
        index_prefix: format!("xc-{slug}-b1"),
        indices: vec![],
        snapshot_name: format!("{slug}-{pin8}"),
        licence_spdx: "MIT".to_string(),
        licence_class: LicenceClass::AdaptWithAttribution,
        sources: vec![PackSource {
            repo: slug.to_string(),
            url: format!("https://example.test/{slug}"),
            sha: full_sha(pin8),
            licence: "MIT".to_string(),
        }],
        defs_count: 0,
        files_count: 0,
    };
    fs::write(
        dir.join("pack.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("defs.jsonl"), b"{}\n").unwrap();
    fs::write(dir.join("snapshot").join("manifest.json"), b"{}").unwrap();
    fs::write(
        dir.join("SHA256SUMS"),
        "00  defs.jsonl\n00  pack.json\n00  snapshot/manifest.json\n",
    )
    .unwrap();
    dir
}

fn entry(slug: &str, pin8: &str, pin_date: &str) -> CatalogueEntry {
    CatalogueEntry {
        slug: slug.to_string(),
        domain: "NET".to_string(),
        topics: vec![],
        ask_for: "x".to_string(),
        upstream: String::new(),
        pin_sha: full_sha(pin8),
        pin_date: pin_date.to_string(),
        refresh_cadence_days: 7,
        licence_spdx: "MIT".to_string(),
        licence_class: LicenceClass::AdaptWithAttribution,
        pack_bytes: 1_000,
        pack_url: format!("https://example.test/{slug}-{pin8}.hubpack.tar.zst"),
        pack_sha256: "ab".repeat(32),
        sig_url: format!("https://example.test/{slug}-{pin8}.hubpack.tar.zst.sig"),
        dependency_names: vec![],
        quality_grade: None,
        term_sketch: Sketch {
            m_bits: 64,
            k_hashes: 7,
            bits_base64: "AAAAAAAAAAA=".to_string(),
        },
    }
}

fn catalogue(entries: Vec<CatalogueEntry>) -> Catalogue {
    Catalogue {
        format_version: 1,
        generated_at: "2026-10-14T00:00:00Z".to_string(),
        corpora: entries,
    }
}

/// What a scripted pull of one slug does.
#[derive(Clone)]
enum Script {
    /// Land `pin8` the way a real pull does: extract the pin directory, then
    /// stamp the ledger.
    Land { pin8: String, pin_date: String },
    /// Exit non-zero.
    Fail { code: i32, message: String },
    /// Exit 0 without having extracted anything.
    SucceedWithoutADirectory,
    /// Extract the pin directory but exit 0 without ever stamping the ledger.
    SucceedWithoutALedger { pin8: String, pin_date: String },
}

struct Fake<'a> {
    hub: &'a Path,
    ledger: RefCell<BTreeMap<String, Ledger>>,
    script: BTreeMap<String, Script>,
    pulls: RefCell<Vec<String>>,
    /// The pin directories that existed under `packs/<slug>/` at the moment the
    /// pull ran — proof the old pin outlives the pull.
    present_at_pull: RefCell<BTreeMap<String, Vec<String>>>,
}

impl<'a> Fake<'a> {
    fn new(hub: &'a Path) -> Self {
        Fake {
            hub,
            ledger: RefCell::new(BTreeMap::new()),
            script: BTreeMap::new(),
            pulls: RefCell::new(Vec::new()),
            present_at_pull: RefCell::new(BTreeMap::new()),
        }
    }
    fn with_ledger(self, slug: &str, pin8: &str) -> Self {
        self.ledger.borrow_mut().insert(
            slug.to_string(),
            Ledger::Hubpack {
                pin8: Some(pin8.to_string()),
            },
        );
        self
    }
    fn with_script(mut self, slug: &str, script: Script) -> Self {
        self.script.insert(slug.to_string(), script);
        self
    }
    fn pulled(&self) -> Vec<String> {
        self.pulls.borrow().clone()
    }
}

impl SyncOps for Fake<'_> {
    fn pull(&self, slug: &str) -> PullOutcome {
        self.pulls.borrow_mut().push(slug.to_string());
        let listing = scan_cache(self.hub)
            .into_iter()
            .filter(|c| c.slug == slug)
            .map(|c| c.pin8)
            .collect();
        self.present_at_pull
            .borrow_mut()
            .insert(slug.to_string(), listing);
        match self.script.get(slug).cloned() {
            Some(Script::Land { pin8, pin_date }) => {
                make_pack(self.hub, slug, &pin8, &pin_date);
                self.ledger.borrow_mut().insert(
                    slug.to_string(),
                    Ledger::Hubpack {
                        pin8: Some(pin8.clone()),
                    },
                );
                PullOutcome::Done
            }
            Some(Script::Fail { code, message }) => PullOutcome::Failed { code, message },
            Some(Script::SucceedWithoutADirectory) => PullOutcome::Done,
            Some(Script::SucceedWithoutALedger { pin8, pin_date }) => {
                make_pack(self.hub, slug, &pin8, &pin_date);
                PullOutcome::Done
            }
            None => PullOutcome::Failed {
                code: 1,
                message: format!("the test scripted no pull of {slug}"),
            },
        }
    }

    fn ledger(&self, slug: &str) -> Ledger {
        self.ledger
            .borrow()
            .get(slug)
            .cloned()
            .unwrap_or(Ledger::Missing)
    }
}

/// Run a sync, returning the report and every line it printed.
fn sync(
    hub: &Path,
    cat: &Catalogue,
    ops: &dyn SyncOps,
    budget: u64,
) -> (SyncReport, Vec<(Level, String)>) {
    let mut lines = Vec::new();
    let report = run_sync_with(hub, cat, ops, budget, &mut |level, line| {
        lines.push((level, line.to_string()))
    });
    (report, lines)
}

fn has_line(lines: &[(Level, String)], needle: &str) -> bool {
    lines.iter().any(|(_, l)| l.contains(needle))
}

fn set_mtime(path: &Path, when: SystemTime) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn days_ago(n: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(n * 86_400)
}

// ── §8: sync with a newer pin replaces the pack and the old dir is gone ──────

#[test]
fn a_newer_pin_replaces_the_pack_and_the_old_dir_is_gone() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let old = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111").with_script(
        "quinn",
        Script::Land {
            pin8: "bbbb2222".into(),
            pin_date: "2026-10-09".into(),
        },
    );

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(!old.exists(), "the old pin's directory is gone");
    let new = hub.join("packs/quinn/bbbb2222");
    assert!(new.join("pack.json").is_file(), "the new pin is in place");
    assert_eq!(ops.pulled(), vec!["quinn"], "exactly one pull");
    assert_eq!(
        ops.present_at_pull.borrow()["quinn"],
        vec!["aaaa1111"],
        "the old pin was still on disk while the new one was pulled — it is removed \
         only AFTER the new pin restores"
    );
    assert_eq!(report.updated, vec!["quinn"]);
    assert!(report.failed.is_empty());
    assert_eq!(report.exit_code(), 0);
    assert!(
        has_line(
            &lines,
            "hub sync: quinn aaaa1111 -> bbbb2222: updated; old pin removed"
        ),
        "lines were: {lines:?}"
    );
    // `status` lists exactly what is on disk: only the new pin.
    let on_disk: Vec<_> = crate::lockfile::gather_status(hub)
        .packs
        .iter()
        .map(|p| (p.slug.clone(), p.pin8.clone()))
        .collect();
    assert_eq!(on_disk, vec![("quinn".to_string(), "bbbb2222".to_string())]);
}

#[test]
fn every_old_pin_of_the_slug_goes_once_the_new_one_has_restored() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let a = make_pack(hub, "quinn", "aaaa1111", "2026-09-01");
    let b = make_pack(hub, "quinn", "bbbb2222", "2026-09-20");
    let cat = catalogue(vec![entry("quinn", "cccc3333", "2026-10-09")]);
    let ops = Fake::new(hub).with_ledger("quinn", "bbbb2222").with_script(
        "quinn",
        Script::Land {
            pin8: "cccc3333".into(),
            pin_date: "2026-10-09".into(),
        },
    );

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(!a.exists() && !b.exists());
    assert!(hub.join("packs/quinn/cccc3333").is_dir());
    assert_eq!(report.updated, vec!["quinn"]);
    assert!(
        has_line(&lines, "quinn aaaa1111,bbbb2222 -> cccc3333: updated"),
        "lines were: {lines:?}"
    );
}

// ── a pull that does not complete never costs the old pin ────────────────────

#[test]
fn a_failed_pull_keeps_the_old_pin_and_exits_1() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let old = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111").with_script(
        "quinn",
        Script::Fail {
            code: 1,
            message: "connect refused".into(),
        },
    );

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(old.join("pack.json").is_file(), "the old pin is untouched");
    assert!(!hub.join("packs/quinn/bbbb2222").exists());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].slug, "quinn");
    assert_eq!(report.exit_code(), 1);
    assert!(report.updated.is_empty());
    let failed: Vec<_> = lines.iter().filter(|(_, l)| l.contains("FAILED")).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].0, Level::Warn, "failures go to stderr");
    assert!(failed[0].1.contains("connect refused"));
    assert!(failed[0].1.contains("old pin aaaa1111 kept"));
}

#[test]
fn a_pack_that_fails_verification_exits_3_and_keeps_the_old_pin() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let old = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let lz4 = make_pack(hub, "lz4", "dddd4444", "2026-10-02");
    let cat = catalogue(vec![
        entry("quinn", "bbbb2222", "2026-10-09"),
        entry("lz4", "eeee5555", "2026-10-09"),
    ]);
    let ops = Fake::new(hub)
        .with_ledger("quinn", "aaaa1111")
        .with_ledger("lz4", "dddd4444")
        .with_script(
            "quinn",
            Script::Fail {
                code: 3,
                message: "pack verification failed: signature does not verify".into(),
            },
        )
        .with_script(
            "lz4",
            Script::Land {
                pin8: "eeee5555".into(),
                pin_date: "2026-10-09".into(),
            },
        );

    let (report, _lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(
        old.is_dir(),
        "the unverifiable pack did not displace the old pin"
    );
    assert!(!lz4.exists() && hub.join("packs/lz4/eeee5555").is_dir());
    assert_eq!(
        report.updated,
        vec!["lz4"],
        "one slug failing does not stop the others"
    );
    assert_eq!(
        report.exit_code(),
        3,
        "verification failure outranks a plain failure"
    );
}

#[test]
fn a_pull_that_claims_success_without_a_new_directory_keeps_the_old_pin() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let old = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub)
        .with_ledger("quinn", "aaaa1111")
        .with_script("quinn", Script::SucceedWithoutADirectory);

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(old.is_dir());
    assert_eq!(report.failed.len(), 1);
    assert!(
        has_line(&lines, "missing or incomplete"),
        "lines: {lines:?}"
    );
}

#[test]
fn a_pull_that_never_stamped_the_ledger_keeps_the_old_pin() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let old = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111").with_script(
        "quinn",
        Script::SucceedWithoutALedger {
            pin8: "bbbb2222".into(),
            pin_date: "2026-10-09".into(),
        },
    );

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(
        old.is_dir(),
        "the new pin is extracted but not recorded as restored, so the old pin stays"
    );
    assert_eq!(report.failed.len(), 1);
    assert!(
        has_line(&lines, "does not record bbbb2222 as restored"),
        "lines: {lines:?}"
    );
}

// ── nothing to do ────────────────────────────────────────────────────────────

#[test]
fn a_pack_already_at_the_catalogue_pin_is_not_pulled() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "aaaa1111", "2026-10-02")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111");

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(ops.pulled().is_empty(), "no pull, so no download");
    assert!(dir.is_dir());
    assert_eq!(report.current, vec!["quinn"]);
    assert_eq!(report.exit_code(), 0);
    assert!(has_line(&lines, "hub sync: quinn aaaa1111: current"));
}

#[test]
fn an_empty_cache_is_a_clean_no_op() {
    let tmp = tempdir().unwrap();
    let cat = catalogue(vec![entry("quinn", "aaaa1111", "2026-10-02")]);
    let ops = Fake::new(tmp.path());

    let (report, lines) = sync(tmp.path(), &cat, &ops, DISK_BUDGET_BYTES);

    assert_eq!(report, SyncReport::default());
    assert!(has_line(&lines, "no cached packs"));
    assert!(ops.pulled().is_empty());
}

#[test]
fn superseded_pins_beside_a_restored_current_pin_are_removed() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let stale = make_pack(hub, "quinn", "aaaa1111", "2026-09-01");
    let current = make_pack(hub, "quinn", "bbbb2222", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-02")]);
    let ops = Fake::new(hub).with_ledger("quinn", "bbbb2222");

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(ops.pulled().is_empty());
    assert!(!stale.exists(), "a leftover older pin is housekeeping");
    assert!(current.is_dir());
    assert_eq!(report.current, vec!["quinn"]);
    assert!(
        has_line(&lines, "removed superseded pin aaaa1111"),
        "{lines:?}"
    );
}

// ── "restored", not just "extracted" ─────────────────────────────────────────

#[test]
fn a_cached_pin_the_ledger_does_not_record_is_pulled_again_and_kept() {
    // The catalogue pin's directory exists (an interrupted pull extracted it)
    // but the ledger still names the old pin: it was never restored.
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let old = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let target = make_pack(hub, "quinn", "bbbb2222", "2026-10-09");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111").with_script(
        "quinn",
        Script::Land {
            pin8: "bbbb2222".into(),
            pin_date: "2026-10-09".into(),
        },
    );

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert_eq!(
        ops.pulled(),
        vec!["quinn"],
        "present is not the same as restored"
    );
    assert!(target.is_dir(), "the pin being repaired is never removed");
    assert!(!old.exists());
    assert_eq!(report.updated, vec!["quinn"]);
    assert!(has_line(
        &lines,
        "re-pulled (the pin was cached but not restored)"
    ));
}

#[test]
fn a_half_extracted_current_pin_is_pulled_again() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    // The extractor writes pack.json before the snapshot: lose a listed file.
    fs::remove_file(dir.join("snapshot").join("manifest.json")).unwrap();
    let cat = catalogue(vec![entry("quinn", "aaaa1111", "2026-10-02")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111").with_script(
        "quinn",
        Script::Land {
            pin8: "aaaa1111".into(),
            pin_date: "2026-10-02".into(),
        },
    );

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert_eq!(ops.pulled(), vec!["quinn"]);
    assert!(
        dir.join("snapshot").join("manifest.json").is_file(),
        "the pull rewrote the directory in place"
    );
    assert_eq!(report.updated, vec!["quinn"]);
    assert!(has_line(
        &lines,
        "re-pulled (the cached pin was incomplete)"
    ));
}

#[test]
fn scan_cache_sees_packs_and_nothing_else() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    // In-flight pull artefacts and strays sit in the same directory.
    let slug_dir = hub.join("packs/quinn");
    fs::create_dir(slug_dir.join("bbbb2222.tmp")).unwrap();
    fs::create_dir(slug_dir.join("bbbb2222.lock")).unwrap();
    fs::create_dir(slug_dir.join("NOTHEX00")).unwrap();
    fs::write(slug_dir.join("cccc3333"), b"a file, not a pack").unwrap();
    fs::write(hub.join("packs/stray-file"), b"x").unwrap();
    // A pin directory with no pack.json is listed, but not complete.
    fs::create_dir_all(hub.join("packs/lz4/dddd4444")).unwrap();

    let cached = scan_cache(hub);

    let seen: Vec<_> = cached
        .iter()
        .map(|c| (c.slug.as_str(), c.pin8.as_str(), c.complete))
        .collect();
    assert_eq!(
        seen,
        vec![("lz4", "dddd4444", false), ("quinn", "aaaa1111", true)]
    );
    assert_eq!(cached[1].pin_date.as_deref(), Some("2026-10-02"));
}

#[test]
fn a_sums_entry_that_climbs_out_of_the_pack_dir_makes_it_incomplete() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    fs::write(hub.join("outside"), b"x").unwrap();
    fs::write(
        dir.join("SHA256SUMS"),
        "00  pack.json\n00  ../../../outside\n",
    )
    .unwrap();
    assert!(!scan_cache(hub)[0].complete);
}

// ── refusals ─────────────────────────────────────────────────────────────────

#[test]
fn a_catalogue_older_than_the_cache_never_downgrades_it() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let cached = make_pack(hub, "quinn", "bbbb2222", "2026-10-09");
    // A replayed (validly signed, but old) catalogue still lists yesterday's pin.
    let cat = catalogue(vec![entry("quinn", "aaaa1111", "2026-10-02")]);
    let ops = Fake::new(hub).with_ledger("quinn", "bbbb2222");

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(ops.pulled().is_empty());
    assert!(cached.is_dir());
    assert_eq!(report.kept, vec!["quinn"]);
    assert_eq!(report.exit_code(), 0, "a refusal is not a failure");
    assert!(
        has_line(&lines, "older than the cached one (2026-10-09)"),
        "lines: {lines:?}"
    );
}

#[test]
fn a_slug_the_catalogue_no_longer_lists_is_kept() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = make_pack(hub, "gone", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub);

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(dir.is_dir());
    assert!(ops.pulled().is_empty());
    assert_eq!(report.kept, vec!["gone"]);
    assert!(has_line(
        &lines,
        "gone aaaa1111: kept (not in the catalogue)"
    ));
}

#[test]
fn excluded_corpora_and_oversize_packs_are_never_pulled() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let a = make_pack(hub, "closed", "aaaa1111", "2026-10-02");
    let b = make_pack(hub, "huge", "bbbb2222", "2026-10-02");
    let mut closed = entry("closed", "cccc3333", "2026-10-09");
    closed.licence_class = LicenceClass::Excluded;
    let mut huge = entry("huge", "dddd4444", "2026-10-09");
    huge.pack_bytes = AUTO_PULL_MAX_BYTES + 1;
    let cat = catalogue(vec![closed, huge]);
    let ops = Fake::new(hub)
        .with_ledger("closed", "aaaa1111")
        .with_ledger("huge", "bbbb2222");

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(ops.pulled().is_empty(), "neither is downloaded");
    assert!(a.is_dir() && b.is_dir(), "both cached copies are kept");
    assert_eq!(report.kept, vec!["closed", "huge"]);
    assert_eq!(report.exit_code(), 0);
    assert!(has_line(&lines, "classes it excluded"));
    assert!(has_line(&lines, "over the 52428800-byte auto-pull cap"));
}

#[test]
fn a_current_pack_stays_current_when_the_catalogue_later_calls_it_excluded() {
    // `excluded` stops a NEW pin being pulled; it is not a reason to report a
    // pack that is already at the catalogue's pin as something to refuse.
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = make_pack(hub, "closed", "aaaa1111", "2026-10-02");
    let mut same_pin = entry("closed", "aaaa1111", "2026-10-02");
    same_pin.licence_class = LicenceClass::Excluded;
    let ops = Fake::new(hub).with_ledger("closed", "aaaa1111");

    let (report, lines) = sync(hub, &catalogue(vec![same_pin]), &ops, DISK_BUDGET_BYTES);

    assert!(dir.is_dir() && ops.pulled().is_empty());
    assert_eq!(report.current, vec!["closed"]);
    assert!(
        has_line(&lines, "hub sync: closed aaaa1111: current"),
        "{lines:?}"
    );
}

#[test]
fn a_slug_served_from_a_local_index_is_left_alone() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub);
    ops.ledger
        .borrow_mut()
        .insert("quinn".into(), Ledger::Local);

    let (report, lines) = sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    assert!(
        ops.pulled().is_empty(),
        "a pull would flip the ledger back to the pack"
    );
    assert!(dir.is_dir());
    assert_eq!(report.kept, vec!["quinn"]);
    assert!(has_line(&lines, "served from a local index"));
}

#[test]
fn a_catalogue_pin_that_is_not_a_commit_is_refused() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    make_pack(hub, "quinn", "aaaa1111", "2026-10-02");
    let mut bad = entry("quinn", "bbbb2222", "2026-10-09");
    bad.pin_sha = "../../etc".into();
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111");

    let (report, lines) = sync(hub, &catalogue(vec![bad]), &ops, DISK_BUDGET_BYTES);

    assert!(ops.pulled().is_empty());
    assert_eq!(report.kept, vec!["quinn"]);
    assert!(has_line(&lines, "is not a hex commit"));
}

// ── the disk budget ──────────────────────────────────────────────────────────

/// A complete pack padded to `size` bytes with a given recency.
fn sized_pack(hub: &Path, slug: &str, pin8: &str, size: usize, used: SystemTime) -> PathBuf {
    let dir = make_pack(hub, slug, pin8, "2026-10-02");
    fs::write(dir.join("snapshot").join("segment.bin"), vec![0u8; size]).unwrap();
    let marker = dir.join("last_used");
    fs::write(&marker, b"x").unwrap();
    set_mtime(&marker, used);
    dir
}

#[test]
fn the_budget_pass_removes_least_recently_used_first_until_it_fits() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let oldest = sized_pack(hub, "alpha", "aaaa1111", 10_000, days_ago(30));
    let middle = sized_pack(hub, "beta", "bbbb2222", 10_000, days_ago(10));
    let newest = sized_pack(hub, "gamma", "cccc3333", 10_000, days_ago(1));
    let total = dir_size(&hub.join("packs"));
    // Room for two of the three packs.
    let budget = total - 10_000;

    let (pruned, left) = prune_to_budget(hub, budget, &BTreeSet::new());

    assert_eq!(
        pruned.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["alpha"],
        "only the least recently used pack had to go"
    );
    assert!(left <= budget);
    assert!(!oldest.exists() && middle.is_dir() && newest.is_dir());
    assert!(
        !hub.join("packs/alpha").exists(),
        "an emptied slug directory is dropped too"
    );
}

#[test]
fn the_budget_pass_keeps_removing_until_the_cache_fits() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    sized_pack(hub, "alpha", "aaaa1111", 10_000, days_ago(30));
    sized_pack(hub, "beta", "bbbb2222", 10_000, days_ago(10));
    let newest = sized_pack(hub, "gamma", "cccc3333", 10_000, days_ago(1));
    let one_pack = dir_size(&newest);

    let (pruned, left) = prune_to_budget(hub, one_pack + 1, &BTreeSet::new());

    assert_eq!(
        pruned.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["alpha", "beta"],
        "oldest first"
    );
    assert!(left <= one_pack + 1);
    assert!(newest.is_dir());
}

#[test]
fn a_cache_inside_the_budget_is_left_exactly_alone() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = sized_pack(hub, "alpha", "aaaa1111", 10_000, days_ago(300));

    let (pruned, _) = prune_to_budget(hub, DISK_BUDGET_BYTES, &BTreeSet::new());

    assert!(pruned.is_empty());
    assert!(dir.is_dir());
}

#[test]
fn leftovers_of_pulls_do_not_count_against_the_budget() {
    // The budget measures what sync can remove. A crashed pull's `.tmp` is the
    // pull's to clean up; counting it would evict a good pack to make room for
    // bytes this can never free.
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let dir = sized_pack(hub, "alpha", "aaaa1111", 10_000, days_ago(300));
    let junk = hub.join("packs/alpha/bbbb2222.tmp");
    fs::create_dir_all(&junk).unwrap();
    fs::write(junk.join("partial.tar.zst"), vec![0u8; 1_000_000]).unwrap();
    let budget = dir_size(&dir) + 100;

    let (pruned, left) = prune_to_budget(hub, budget, &BTreeSet::new());

    assert!(
        pruned.is_empty(),
        "the pack fits; a megabyte of junk elsewhere is not its problem"
    );
    assert!(left <= budget);
    assert!(dir.is_dir());
    assert!(
        junk.is_dir(),
        "and sync does not touch another process's temp directory"
    );
}

#[test]
fn incomplete_directories_are_pruned_before_any_pack() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let good = sized_pack(hub, "alpha", "aaaa1111", 10_000, days_ago(300));
    let junk = hub.join("packs/beta/bbbb2222");
    fs::create_dir_all(&junk).unwrap();
    fs::write(junk.join("partial.bin"), vec![0u8; 10_000]).unwrap();
    let budget = dir_size(&hub.join("packs")) - 5_000;

    let (pruned, _) = prune_to_budget(hub, budget, &BTreeSet::new());

    assert_eq!(pruned.len(), 1);
    assert_eq!(
        pruned[0].slug, "beta",
        "the half-extracted leftover goes first"
    );
    assert!(
        good.is_dir(),
        "even though the real pack was used less recently"
    );
}

#[test]
fn packs_updated_in_this_run_are_not_pruned() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    sized_pack(hub, "quinn", "aaaa1111", 10_000, days_ago(30));
    let other = sized_pack(hub, "lz4", "dddd4444", 10_000, days_ago(1));
    let cat = catalogue(vec![
        entry("quinn", "bbbb2222", "2026-10-09"),
        entry("lz4", "dddd4444", "2026-10-02"),
    ]);
    let ops = Fake::new(hub)
        .with_ledger("quinn", "aaaa1111")
        .with_ledger("lz4", "dddd4444")
        .with_script(
            "quinn",
            Script::Land {
                pin8: "bbbb2222".into(),
                pin_date: "2026-10-09".into(),
            },
        );
    // Far too small for either pack: lz4 (used yesterday) is the only candidate,
    // because quinn was just fetched — so it goes, and quinn stays over budget.
    let budget = 100;

    let (report, lines) = sync(hub, &cat, &ops, budget);

    assert!(
        hub.join("packs/quinn/bbbb2222").is_dir(),
        "the fresh pack stays"
    );
    assert!(!other.exists(), "the only candidate went");
    assert_eq!(report.pruned.len(), 1);
    assert_eq!(report.pruned[0].slug, "lz4");
    assert!(has_line(&lines, "pruned lz4 dddd4444"), "lines: {lines:?}");
    assert!(
        has_line(&lines, "nothing more can be pruned"),
        "still over budget, and it says so: {lines:?}"
    );
}

#[test]
fn an_update_keeps_the_old_pins_recency() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path();
    let long_idle = days_ago(40);
    sized_pack(hub, "quinn", "aaaa1111", 100, long_idle);
    let cat = catalogue(vec![entry("quinn", "bbbb2222", "2026-10-09")]);
    let ops = Fake::new(hub).with_ledger("quinn", "aaaa1111").with_script(
        "quinn",
        Script::Land {
            pin8: "bbbb2222".into(),
            pin_date: "2026-10-09".into(),
        },
    );

    sync(hub, &cat, &ops, DISK_BUDGET_BYTES);

    let new = scan_cache(hub);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].pin8, "bbbb2222");
    let (early, late) = if new[0].last_used < long_idle {
        (new[0].last_used, long_idle)
    } else {
        (long_idle, new[0].last_used)
    };
    let drift = late.duration_since(early).unwrap();
    assert!(
        drift < Duration::from_secs(2),
        "refreshing a pack is not using it: its recency must stay the old pin's, \
         but it moved by {drift:?}"
    );
}

// ── last_sync and the exit code ──────────────────────────────────────────────

#[test]
fn last_sync_is_an_rfc3339_stamp_that_status_can_read() {
    let tmp = tempdir().unwrap();
    let hub = tmp.path().join("fresh-hub-dir");

    write_last_sync(&hub).unwrap();

    let raw = fs::read_to_string(hub.join("last_sync")).unwrap();
    chrono::DateTime::parse_from_rfc3339(raw.trim()).expect("RFC 3339");
    assert_eq!(
        crate::lockfile::gather_status(&hub).last_sync_age_days,
        Some(0),
        "`xerj hub status` reads the file's age"
    );
}

#[test]
fn the_exit_code_ranks_verification_over_infrastructure() {
    let mut r = SyncReport::default();
    assert_eq!(r.exit_code(), 0);
    r.failed.push(SyncFailure {
        slug: "a".into(),
        code: 1,
        message: String::new(),
    });
    assert_eq!(r.exit_code(), 1);
    r.failed.push(SyncFailure {
        slug: "b".into(),
        code: 3,
        message: String::new(),
    });
    assert_eq!(r.exit_code(), 3);
}

// ── the child-process pull ───────────────────────────────────────────────────

#[cfg(unix)]
mod child_process {
    use super::*;

    /// A `sh` script standing in for `xerj`: records its arguments, writes a
    /// line to stderr, exits with `code`. Run as `sh <script> hub pull …` so no
    /// freshly written executable is ever exec'd (no ETXTBSY race under the
    /// parallel test runner).
    fn fake_xerj(dir: &Path, code: i32, stderr: &str) -> (PathBuf, PathBuf) {
        let args = dir.join("args.txt");
        let script = dir.join("fake-xerj.sh");
        fs::write(
            &script,
            format!(
                "printf '%s\\n' \"$@\" > '{}'\necho '{stderr}' >&2\nexit {code}\n",
                args.display()
            ),
        )
        .unwrap();
        (script, args)
    }

    fn ops(script: &Path, url: Option<&str>, code_root: &Path) -> CliOps {
        CliOps::with_program(
            "sh",
            vec![script.as_os_str().to_owned()],
            url.map(str::to_string),
            code_root.to_path_buf(),
        )
    }

    #[test]
    fn pull_runs_the_existing_pull_subcommand_quietly() {
        let tmp = tempdir().unwrap();
        let (script, args) = fake_xerj(tmp.path(), 0, "");

        let out = ops(&script, Some("http://127.0.0.1:1"), tmp.path()).pull("quinn");

        assert_eq!(out, PullOutcome::Done);
        assert_eq!(
            fs::read_to_string(args)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec![
                "hub",
                "pull",
                "quinn",
                "--quiet",
                "--url",
                "http://127.0.0.1:1"
            ],
            "sync drives `xerj hub pull`, it does not reimplement it"
        );
    }

    #[test]
    fn pull_omits_url_when_none_was_given() {
        let tmp = tempdir().unwrap();
        let (script, args) = fake_xerj(tmp.path(), 0, "");

        ops(&script, None, tmp.path()).pull("quinn");

        assert_eq!(
            fs::read_to_string(args)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["hub", "pull", "quinn", "--quiet"]
        );
    }

    #[test]
    fn a_nonzero_exit_carries_its_code_and_stderr() {
        let tmp = tempdir().unwrap();
        let (script, _) = fake_xerj(
            tmp.path(),
            3,
            "xerj hub pull: pack verification failed: pack signature",
        );

        let out = ops(&script, None, tmp.path()).pull("quinn");

        assert_eq!(
            out,
            PullOutcome::Failed {
                code: 3,
                message: "xerj hub pull: pack verification failed: pack signature".into()
            }
        );
    }

    #[test]
    fn a_missing_program_is_an_infrastructure_failure() {
        let tmp = tempdir().unwrap();
        let ops = CliOps::with_program(
            tmp.path().join("no-such-xerj"),
            vec![],
            None,
            tmp.path().to_path_buf(),
        );

        match ops.pull("quinn") {
            PullOutcome::Failed { code, message } => {
                assert_eq!(code, 1);
                assert!(message.contains("cannot run"), "{message}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn the_ledger_is_read_from_the_code_root() {
        let tmp = tempdir().unwrap();
        let state = tmp.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let (script, _) = fake_xerj(tmp.path(), 0, "");
        let ops = ops(&script, None, tmp.path());

        assert_eq!(ops.ledger("quinn"), Ledger::Missing);

        fs::write(
            state.join("quinn.json"),
            format!(
                r#"{{"corpus":"quinn","source":"hubpack","pin_sha":"{}"}}"#,
                full_sha("bbbb2222")
            ),
        )
        .unwrap();
        assert_eq!(
            ops.ledger("quinn"),
            Ledger::Hubpack {
                pin8: Some("bbbb2222".into())
            }
        );

        fs::write(
            state.join("lz4.json"),
            r#"{"corpus":"lz4","index_prefix":"xc-lz4-b1700000000"}"#,
        )
        .unwrap();
        assert_eq!(ops.ledger("lz4"), Ledger::Local, "no `source: hubpack`");
    }
}

#[test]
fn one_line_flattens_and_caps_stderr() {
    assert_eq!(one_line("a\n  b\t c\n", 100), "a b c");
    assert_eq!(one_line("abcdefghij", 4), "abcd…");
}

// ── the timer ────────────────────────────────────────────────────────────────

fn timer_dirs(root: &Path) -> TimerDirs {
    TimerDirs {
        systemd_user: root.join("cfg/systemd/user"),
        launch_agents: root.join("Library/LaunchAgents"),
        hub_home: root.join("hub"),
    }
}

#[test]
fn install_timer_writes_a_six_hourly_systemd_user_timer_and_nothing_else() {
    let tmp = tempdir().unwrap();
    let dirs = timer_dirs(tmp.path());
    let exe = Path::new("/home/u/.local/bin/xerj");

    let printed = install_timer(TimerKind::SystemdUser, exe, &dirs).unwrap();

    // The path printed is the timer — the unit the user enables.
    assert_eq!(printed, dirs.systemd_user.join("xerj-hub-sync.timer"));
    let timer = fs::read_to_string(&printed).unwrap();
    let service = fs::read_to_string(dirs.systemd_user.join("xerj-hub-sync.service")).unwrap();

    let timer_lines: Vec<&str> = timer.lines().collect();
    for want in [
        "[Timer]",
        "OnCalendar=*-*-* 00/6:00:00",
        "Persistent=true",
        "[Install]",
        "WantedBy=timers.target",
    ] {
        assert!(
            timer_lines.contains(&want),
            "timer lacks {want:?}:\n{timer}"
        );
    }
    let service_lines: Vec<&str> = service.lines().collect();
    for want in [
        "[Service]",
        "Type=oneshot",
        "ExecStart=/home/u/.local/bin/xerj hub sync",
    ] {
        assert!(
            service_lines.contains(&want),
            "service lacks {want:?}:\n{service}"
        );
    }

    // Nothing else: two files in the systemd dir, no hub dir, no launchd dir.
    let mut written: Vec<String> = fs::read_dir(&dirs.systemd_user)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    written.sort();
    assert_eq!(
        written,
        vec!["xerj-hub-sync.service", "xerj-hub-sync.timer"]
    );
    assert!(!dirs.hub_home.exists() && !dirs.launch_agents.exists());
}

#[test]
fn install_timer_overwrites_so_a_moved_binary_is_picked_up() {
    let tmp = tempdir().unwrap();
    let dirs = timer_dirs(tmp.path());

    install_timer(TimerKind::SystemdUser, Path::new("/old/xerj"), &dirs).unwrap();
    install_timer(TimerKind::SystemdUser, Path::new("/new/xerj"), &dirs).unwrap();

    let service = fs::read_to_string(dirs.systemd_user.join("xerj-hub-sync.service")).unwrap();
    assert!(service.contains("ExecStart=/new/xerj hub sync"));
    assert!(!service.contains("/old/xerj"));
}

#[test]
fn a_binary_path_that_needs_quoting_is_quoted_for_systemd() {
    let tok = |p: &str| systemd_exec_token(Path::new(p)).unwrap();
    assert_eq!(tok("/usr/bin/xerj"), "/usr/bin/xerj");
    assert_eq!(tok("/opt/my tools/xerj"), "\"/opt/my tools/xerj\"");
    // `%` starts a specifier and `$` a variable even inside quotes.
    assert_eq!(tok("/opt/100%/xerj"), "\"/opt/100%%/xerj\"");
    assert_eq!(tok("/opt/$HOME/xerj"), "\"/opt/$$HOME/xerj\"");
    assert_eq!(tok("/opt/a\"b\\c/xerj"), "\"/opt/a\\\"b\\\\c/xerj\"");
    assert!(systemd_exec_token(Path::new("/opt/a\nb")).is_err());

    let service = render_systemd_service(Path::new("/opt/my tools/xerj")).unwrap();
    assert!(service.contains("ExecStart=\"/opt/my tools/xerj\" hub sync"));
}

#[test]
fn a_binary_path_that_cannot_be_written_safely_leaves_nothing_behind() {
    let tmp = tempdir().unwrap();
    let dirs = timer_dirs(tmp.path());

    let err = install_timer(TimerKind::SystemdUser, Path::new("/opt/a\nb/xerj"), &dirs)
        .expect_err("a newline cannot ride in a unit file");

    assert!(format!("{err:#}").contains("control character"));
    assert!(!dirs.systemd_user.exists(), "no half-installed units");
}

#[test]
fn install_timer_writes_a_six_hourly_launchd_agent() {
    let tmp = tempdir().unwrap();
    let dirs = timer_dirs(tmp.path());
    let exe = Path::new("/Users/u/bin/xerj & co/xerj");

    let printed = install_timer(TimerKind::Launchd, exe, &dirs).unwrap();

    assert_eq!(printed, dirs.launch_agents.join("org.xerj.hub-sync.plist"));
    let plist = fs::read_to_string(&printed).unwrap();
    for want in [
        "<key>Label</key>\n\t<string>org.xerj.hub-sync</string>",
        // argv in order: the binary, then `hub sync`.
        "<string>/Users/u/bin/xerj &amp; co/xerj</string>\n\t\t<string>hub</string>\n\t\t<string>sync</string>",
        "<key>StartInterval</key>\n\t<integer>21600</integer>",
        "<key>RunAtLoad</key>\n\t<false/>",
    ] {
        assert!(plist.contains(want), "plist lacks {want:?}:\n{plist}");
    }
    let log = dirs.hub_home.join("sync.log");
    assert!(
        plist.contains(&format!("<string>{}</string>", log.display())),
        "launchd output lands in the hub home"
    );
    assert!(
        dirs.hub_home.is_dir(),
        "launchd will not create the log's directory"
    );
    assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(plist.trim_end().ends_with("</plist>"));
    assert!(
        !dirs.systemd_user.exists(),
        "only the launchd file is written"
    );
}

#[test]
fn timer_dirs_follow_xdg_config_home_and_ignore_a_relative_one() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| -> Option<OsString> {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(*v))
        }
    };

    let dirs =
        TimerDirs::from_env(&env(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/cfg")])).unwrap();
    assert_eq!(dirs.systemd_user, PathBuf::from("/cfg/systemd/user"));
    assert_eq!(
        dirs.launch_agents,
        PathBuf::from("/home/u/Library/LaunchAgents")
    );
    assert_eq!(dirs.hub_home, PathBuf::from("/home/u/.xerj/hub"));

    // XDG: a relative XDG_CONFIG_HOME is invalid and ignored.
    let dirs =
        TimerDirs::from_env(&env(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "rel")])).unwrap();
    assert_eq!(
        dirs.systemd_user,
        PathBuf::from("/home/u/.config/systemd/user")
    );

    // The same hub-home override every other subcommand honours.
    let dirs =
        TimerDirs::from_env(&env(&[("HOME", "/home/u"), ("XERJ_HUB_HOME", "/srv/hub")])).unwrap();
    assert_eq!(dirs.hub_home, PathBuf::from("/srv/hub"));

    // HOME unset or empty falls back to USERPROFILE, the same rule as the hub
    // home itself; both paths below come from the one variable.
    for e in [
        env(&[("USERPROFILE", "/Users/u")]),
        env(&[("HOME", ""), ("USERPROFILE", "/Users/u")]),
    ] {
        let dirs = TimerDirs::from_env(&e).unwrap();
        assert_eq!(
            dirs.launch_agents,
            PathBuf::from("/Users/u/Library/LaunchAgents")
        );
        assert_eq!(dirs.hub_home, PathBuf::from("/Users/u/.xerj/hub"));
    }

    assert!(
        TimerDirs::from_env(&env(&[])).is_err(),
        "no HOME and no USERPROFILE, no user timer"
    );
    assert!(
        TimerDirs::from_env(&env(&[("HOME", ""), ("USERPROFILE", "")])).is_err(),
        "empty values count as unset"
    );
}

/// systemd is the arbiter of what a unit means, so when the machine has
/// `systemd-analyze` ask it: the calendar spec must fire every six hours, and
/// `verify` must have no complaint about the files we wrote. (Skipped where it
/// is absent, or too old (< 254) to take `--base-time`: it is not a build
/// dependency.)
#[cfg(target_os = "linux")]
#[test]
fn systemd_reads_the_units_as_a_six_hourly_oneshot() {
    let have = Command::new("systemd-analyze").arg("--version").output();
    let Some(version) = have.ok().filter(|o| o.status.success()).and_then(|o| {
        // First line: "systemd 255 (255.4-1ubuntu8)".
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse::<u32>().ok())
    }) else {
        eprintln!("systemd-analyze not available; skipping");
        return;
    };
    if version < 254 {
        eprintln!("systemd-analyze {version} predates --base-time; skipping");
        return;
    }

    // The calendar spec in the timer we render, evaluated from a FIXED clock
    // in UTC so neither the host's zone nor the time of day the test runs at
    // moves the answer. 21:30 UTC sits between the 18:00 and 00:00 firings,
    // so the next five are 00, 06, 12, 18, 00 — on any day, at any hour.
    let spec = render_systemd_timer()
        .lines()
        .find_map(|l| l.strip_prefix("OnCalendar=").map(str::to_string))
        .expect("the timer has an OnCalendar line");
    let cal = Command::new("systemd-analyze")
        .env("TZ", "UTC")
        .args([
            "calendar",
            "--iterations=5",
            "--base-time=2026-01-01 21:30:00 UTC",
            &spec,
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&cal.stdout).into_owned();
    assert!(cal.status.success(), "calendar spec rejected:\n{text}");
    let firings: Vec<String> = text
        .lines()
        .filter(|l| l.contains("UTC") && !l.contains("From now"))
        .filter_map(|l| {
            // "    Next elapse: Fri 2026-01-02 00:00:00 UTC" -> "2026-01-02 00:00:00"
            let mut w = l.split_whitespace().rev().skip(1);
            let time = w.next()?;
            let date = w.next()?;
            Some(format!("{date} {time}"))
        })
        .collect();
    assert_eq!(
        firings,
        vec![
            "2026-01-02 00:00:00",
            "2026-01-02 06:00:00",
            "2026-01-02 12:00:00",
            "2026-01-02 18:00:00",
            "2026-01-03 00:00:00",
        ],
        "five successive firings are six hours apart:\n{text}"
    );

    // Whole-unit check. `/bin/true` exists everywhere systemd does.
    let tmp = tempdir().unwrap();
    let dirs = timer_dirs(tmp.path());
    install_timer(TimerKind::SystemdUser, Path::new("/bin/true"), &dirs).unwrap();
    let verify = Command::new("systemd-analyze")
        .arg("verify")
        .arg(dirs.systemd_user.join("xerj-hub-sync.service"))
        .arg(dirs.systemd_user.join("xerj-hub-sync.timer"))
        .output()
        .unwrap();
    let diag = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify.status.success() && !diag.contains("xerj-hub-sync"),
        "systemd-analyze verify complained:\n{diag}"
    );
}

// ── the subcommand's argument handling ───────────────────────────────────────

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn sync_rejects_what_it_does_not_understand_before_touching_anything() {
    assert_eq!(run_sync(&args(&["--bogus"])), 2);
    assert_eq!(run_sync(&args(&["some-slug"])), 2, "sync takes no slug");
    assert_eq!(run_sync(&args(&["--url"])), 2, "--url needs a value");
    assert_eq!(
        run_sync(&args(&["--install-timer", "--url", "http://x"])),
        2,
        "--install-timer takes no other flag"
    );
    assert_eq!(run_sync(&args(&["--help"])), 0);
    assert_eq!(run_sync(&args(&["-h"])), 0);
}

#[test]
fn the_hub_command_routes_sync_here() {
    // `--help` exits 0 only if `sync` is a known subcommand (an unknown one
    // exits 2), and it does so without touching the network or the cache.
    assert_eq!(crate::cli::run_hub_cli(&args(&["sync", "--help"])), 0);
    assert_eq!(crate::cli::run_hub_cli(&args(&["sync", "--bogus"])), 2);
}

#[test]
fn the_usage_names_every_flag_and_exit_code() {
    let usage = sync_usage();
    for needle in [
        "--url <node>",
        "--install-timer",
        "every 6 hours",
        "exit codes: 0",
        "3 a pack",
    ] {
        assert!(usage.contains(needle), "usage lacks {needle:?}");
    }
    // The budget it names is the one the code enforces, not a typed-in number.
    assert!(
        usage.contains(&format!(
            "the {} disk budget",
            human_bytes(DISK_BUDGET_BYTES)
        )),
        "{usage}"
    );
}
