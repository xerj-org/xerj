//! Licence policy, driven by DATA (the recorded `corpus.json` licence string)
//! — never by prose. The hub's own record once disagreed with a doc's prose
//! (`sonic` is recorded MPL-2.0; a stale CLAUDE.md said GPL), and hard-coding
//! repo names or licences is exactly how that mistake becomes load-bearing.
//!
//! Two surfaces share one tuple:
//! * QUERY time — [`is_restricted`] gates the per-hit warning printed
//!   directly under the passage it guards, so it cannot be separated from the
//!   code it applies to;
//! * CLONE time — the same tuple gates the one-line warning `xerj corpus add`
//!   prints per repo.
//!
//! A corpus the operator marked own ([`copy_warning_applies`]) is their own
//! first-party code. An unidentified licence stays recorded and is still
//! named in the hit header, but the do-not-copy banner is omitted for that
//! unidentified licence only. A licence the detector actually named is
//! unchanged, and a hub pack's licence class is not this tuple.
//!
//! The original scripts let these drift (the clone-time check was missing the
//! MPL arm, so `sonic` warned only at query time despite a comment saying the
//! two "MUST stay in step"). The port unifies on the FULLER tuple: strictly
//! more warning, never less.

/// Licences you must not copy from, as substrings of what the manifest
/// records. Substring is deliberate: the detector records multi-licence
/// strings ("AGPL" for elasticsearch's triple, "BUSL/MIT" for meilisearch's
/// EE split, "Apache-2.0/MIT" for dual-licensed crates), and the tuple
/// catches every restricted constituent.
///
/// Warning on only ("GPL","LGPL") — an exact match, as this once did — was
/// silent on every restricted repo the hub deliberately ships.
/// UNKNOWN and NONE-FOUND are restricted too: an unidentified licence must
/// over-report restriction, never under-report it.
pub const RESTRICTED: &[&str] = &[
    "AGPL",
    "SSPL",
    "Elastic",
    "BUSL",
    "GPL",
    "LGPL",
    "MPL",
    "UNKNOWN",
    "NONE-FOUND",
];

/// The per-hit warning line, printed directly under the passage so it cannot
/// be separated from the code it guards.
pub fn hit_warning_line(lic: &str) -> String {
    format!("    !! {lic}: adapt the APPROACH, do not copy the code")
}

/// The clone-time warning line (`xerj corpus add`). Same tuple as query time
/// — the unified rule the scripts' comment promised.
pub fn clone_warning_line(lic: &str) -> Option<String> {
    if is_restricted(lic) {
        Some(format!(
            "          ^ {lic}: adapt the APPROACH, do not copy the code"
        ))
    } else {
        None
    }
}

/// True when the recorded licence string carries any restricted constituent.
/// An empty string (repo missing from the licence map) is "no record", which
/// the renderer reports without a licence-specific line — there is no licence
/// to name in the warning.
pub fn is_restricted(lic: &str) -> bool {
    RESTRICTED.iter().any(|p| lic.contains(p))
}

/// The licence a human REVIEW recorded for a repo at its pin (the `review`
/// block a vetted hub manifest carries: `spdx`, `use`, `by`, `at`, `note`).
/// Only a completed review counts: `by` must name the reviewer and `use` must
/// not be `UNREVIEWED` (draft rows carry an `spdx` hint with an empty `by`).
/// The SPDX string must look like one — letters, digits and `.+-/ ()`, at
/// most 64 chars — so an untrusted manifest cannot put arbitrary text into
/// the terminal through it.
pub fn reviewed_spdx(review: Option<&serde_json::Value>) -> Option<String> {
    let r = review?.as_object()?;
    let field = |k: &str| {
        r.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
    };
    let spdx = field("spdx");
    let by = field("by");
    let used = field("use");
    let looks_spdx = !spdx.is_empty()
        && spdx.len() <= 64
        && spdx
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+-/ ()".contains(c));
    if !looks_spdx || by.is_empty() || used.is_empty() || used.eq_ignore_ascii_case("UNREVIEWED") {
        return None;
    }
    Some(spdx.to_string())
}

/// True when the detector could not name a licence at all (as opposed to
/// naming a restricted one).
pub fn is_unidentified(lic: &str) -> bool {
    matches!(lic.trim(), "" | "UNKNOWN" | "NONE-FOUND")
}

/// Whether a passage or a clone line should carry the do-not-copy banner.
///
/// `own` is the corpus-level first-party marker (`xerj corpus add --own` /
/// `xerj corpus own`). It suppresses the banner only when the recorded
/// licence is unidentified — the owner's own tree often has no LICENSE file,
/// and "do not copy" is the wrong instruction for that code. The licence
/// string itself is not rewritten. A named restricted licence (GPL, AGPL,
/// MPL, …) still warns: marking a corpus own does not relicense a checkout
/// the detector could read.
pub fn copy_warning_applies(lic: &str, own: bool) -> bool {
    !lic.is_empty() && is_restricted(lic) && !(own && is_unidentified(lic))
}

/// The licence to label a repo's passages with: the recorded detector string,
/// unless the detector could not identify one and a completed human review
/// names it (issue #1893: the detector read UNKNOWN on a CC0-1.0 repo whose
/// review verified CC0-1.0, and every passage said "do not copy"). A review
/// never overrides a licence the detector DID identify — a restricted
/// detection still warns.
pub fn effective_licence(recorded: &str, review: Option<&serde_json::Value>) -> String {
    if is_unidentified(recorded) {
        if let Some(spdx) = reviewed_spdx(review) {
            return spdx;
        }
    }
    recorded.to_string()
}

/// Find the licence without guessing: read the file(s) the project actually
/// ships. Dual-licensed crates (very common in Rust) ship LICENSE-MIT +
/// LICENSE-APACHE and no plain LICENSE, so every candidate is collected
/// rather than taking the first. Results are de-duplicated and joined with
/// "/" — "Apache-2.0/MIT" is the honest answer for a dual-licensed project;
/// collapsing it to one licence would be a false record. No candidate file
/// at all -> "NONE-FOUND".
pub fn detect_licence(repo_dir: &std::path::Path) -> String {
    let Ok(entries) = std::fs::read_dir(repo_dir) else {
        return "NONE-FOUND".to_string();
    };
    let names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let found = classify_candidates(names, |name| {
        std::fs::read_to_string(repo_dir.join(name)).ok()
    });
    if !found.is_empty() {
        return found;
    }
    // A `paths` harvest (#1842) is a sparse checkout of only the named
    // directories, so the root LICENSE is in the pin but not on disk and the
    // corpus was recorded NONE-FOUND (restricted). Read the root licence
    // files from the checked-out commit instead of the worktree.
    let pinned = licence_from_git_head(repo_dir);
    if pinned.is_empty() {
        "NONE-FOUND".to_string()
    } else {
        pinned
    }
}

/// Licence files at the root of `HEAD` in a git checkout, read from the
/// object store, so a sparse worktree still reports the project's licence.
/// Empty when `repo_dir` is not a git checkout or the commit has none.
fn licence_from_git_head(repo_dir: &std::path::Path) -> String {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // Only when repo_dir is itself the top of the checkout: a subdirectory
    // of some unrelated repository must not borrow that repository's licence.
    let Some(top) = git(&["rev-parse", "--show-toplevel"]) else {
        return String::new();
    };
    let same = match (
        std::fs::canonicalize(top.trim()),
        std::fs::canonicalize(repo_dir),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same {
        return String::new();
    }
    let Some(listing) = git(&["ls-tree", "--name-only", "HEAD"]) else {
        return String::new();
    };
    let names: Vec<String> = listing.lines().map(str::to_string).collect();
    classify_candidates(names, |name| git(&["show", &format!("HEAD:{name}")]))
}

/// Classify every licence-named file in `names`, reading each with `read`.
/// Returns the sorted, de-duplicated licences joined with "/", or "" when
/// no candidate file was read.
fn classify_candidates(names: Vec<String>, read: impl Fn(&str) -> Option<String>) -> String {
    let mut found: Vec<String> = Vec::new();
    let mut names: Vec<String> = names
        .into_iter()
        // Case-insensitive: plenty of projects ship `license.txt`,
        // `License.md` or `copying` (jfreechart, ta4j; issue #2201), and a
        // missed file records NONE-FOUND, which is treated as restricted.
        .filter(|n| {
            let u = n.to_uppercase();
            u.starts_with("LICENSE") || u.starts_with("LICENCE") || u.starts_with("COPYING")
        })
        .collect();
    names.sort();
    for name in names {
        // LICENSE-3rdparty.csv (quickwit) is an inventory of DEPENDENCY
        // licences, not this project's licence. Classifying it produced a
        // spurious "UNKNOWN" arm on an otherwise plain Apache-2.0 repo.
        let lower = name.to_lowercase();
        if lower.contains("3rdparty")
            || lower.contains("third-party")
            || lower.contains("thirdparty")
        {
            continue;
        }
        if let Some(text) = read(&name) {
            found.push(classify_one(&text).to_string());
        }
    }
    found.sort();
    found.dedup();
    found.join("/")
}

/// Classify one licence FILE by its text: the TITLE (first two non-blank
/// lines — titles wrap) first, and only then the body (first 4000 chars).
///
/// A licence file names itself in its opening lines while its body may CITE
/// other licences, so the body scan is the fallback, not the primary.
fn classify_one(text: &str) -> &'static str {
    let title: String = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let from_title = classify_text(&title);
    // A generic "BSD" hit is the redistribution sentence, which 2-clause,
    // 3-clause and 0BSD files all share. Keep reading the body so the
    // clause title or the endorsement sentence can name the variant.
    // Anything more specific (Apache, MIT, MPL, GPL, BSD-3-Clause, …)
    // wins from the title and the body is not consulted.
    if from_title != "UNKNOWN" && from_title != "BSD" {
        return from_title;
    }
    let from_body = classify_text(text.chars().take(4000).collect::<String>().as_str());
    // A generic `BSD` title stays `BSD` when the body names nothing more
    // specific; otherwise (and for an UNKNOWN title) the body decides.
    if from_title == "BSD" && from_body == "UNKNOWN" {
        return "BSD";
    }
    from_body
}

/// Classify licence text. Order matters: check the most RESTRICTIVE phrases
/// first, and keep "bsd" last because Apache and MIT texts can both mention
/// BSD in passing.
///
/// The restrictive-first ordering is not stylistic. Elasticsearch's
/// LICENSE.txt is a triple licence (AGPL-3.0 / SSPL-1.0 / Elastic-2.0) whose
/// text contains the phrase «an "Apache License 2.0" compatible license»:
/// with Apache checked first, the repo was recorded "Apache-2.0" — the most
/// permissive possible reading of the most restrictive licence in the
/// corpus, on the one repo where being wrong matters most.
fn classify_text(text: &str) -> &'static str {
    let body: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    // Restrictive-first; keep in step with RESTRICTED above.
    if body.contains("affero general public license") {
        "AGPL"
    } else if body.contains("server side public license") {
        "SSPL"
    } else if body.contains("elastic license") {
        "Elastic"
    } else if body.contains("business source license") {
        "BUSL"
    } else if body.contains("gnu lesser general") {
        "LGPL"
    } else if body.contains("gnu general public license") {
        "GPL"
    } else if body.contains("mozilla public license") {
        "MPL-2.0"
    } else if body.contains("apache license") {
        "Apache-2.0"
    } else if body.contains("mit license")
        || body.contains("permission is hereby granted, free of charge")
    {
        "MIT"
    } else if body.contains("bsd 3-clause")
        || body.contains("bsd-3-clause")
        || body.contains("3-clause bsd")
        || body.contains("may be used to endorse or promote products derived from this software")
    {
        "BSD-3-Clause"
    } else if body.contains("bsd 2-clause")
        || body.contains("bsd-2-clause")
        || body.contains("simplified bsd")
    {
        "BSD-2-Clause"
    } else if body.contains("0bsd")
        || body.contains("zero-clause bsd")
        || body.contains("zero clause bsd")
    {
        "0BSD"
    } else if body.contains("redistribution and use in source and binary forms") {
        "BSD"
    } else {
        "UNKNOWN"
    }
}

/// Whether a manifest's declared licence and a checkout's detected licence
/// are the same reading. A coarse `BSD` agrees with `BSD-2-Clause`,
/// `BSD-3-Clause` and `0BSD` — the detector's generic redistribution hit is
/// not a disagreement with a clause the manifest already named. Two
/// different clauses (`BSD-2-Clause` vs `BSD-3-Clause`) do not agree.
/// Slash-joined dual licences are compared token by token after sorting.
pub fn licence_readings_agree(declared: &str, detected: &str) -> bool {
    fn tokens(s: &str) -> Vec<&str> {
        s.split('/')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect()
    }
    fn one(a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        let bsd_family = |s: &str| s == "BSD" || s.starts_with("BSD-") || s == "0BSD";
        (a == "BSD" && bsd_family(b)) || (b == "BSD" && bsd_family(a))
    }
    let mut declared_tokens = tokens(declared);
    let mut detected_tokens = tokens(detected);
    if declared_tokens.len() != detected_tokens.len() {
        return false;
    }
    declared_tokens.sort_unstable();
    detected_tokens.sort_unstable();
    declared_tokens
        .iter()
        .zip(detected_tokens.iter())
        .all(|(a, b)| one(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_substrings_catch_every_recorded_restricted_repo() {
        // Exactly what corpus.json records, per repo:
        assert!(
            is_restricted("AGPL"),
            "elasticsearch (triple, most-restrictive recorded)"
        );
        assert!(
            is_restricted("MPL-2.0"),
            "sonic — MPL, NOT GPL; drive from the record"
        );
        assert!(is_restricted("BUSL/MIT"), "meilisearch EE");
        assert!(
            !is_restricted("Apache-2.0/MIT"),
            "dual permissive: cite freely"
        );
        assert!(!is_restricted("Apache-2.0"));
        assert!(!is_restricted("MIT"));
        assert!(!is_restricted("BSD-3-Clause"));
        assert!(is_restricted("UNKNOWN"), "unidentified over-reports");
        assert!(is_restricted("NONE-FOUND"), "no licence file over-reports");
    }

    #[test]
    fn the_warning_line_is_verbatim() {
        assert_eq!(
            hit_warning_line("AGPL"),
            "    !! AGPL: adapt the APPROACH, do not copy the code"
        );
    }

    // ── clone-time warning: UNIFIED with the query tuple (the fix) ────────

    #[test]
    fn clone_time_warning_now_covers_mpl_like_query_time() {
        // The scripts' discrepancy: sonic (MPL-2.0) warned at query time but
        // NOT at clone time. Unified on the fuller tuple.
        assert!(
            clone_warning_line("MPL-2.0").is_some(),
            "sonic must warn at clone time too"
        );
        for lic in [
            "AGPL",
            "SSPL",
            "Elastic",
            "BUSL",
            "GPL",
            "LGPL",
            "UNKNOWN",
            "NONE-FOUND",
        ] {
            assert!(clone_warning_line(lic).is_some(), "{lic} must warn");
        }
        assert!(clone_warning_line("Apache-2.0/MIT").is_none());
    }

    /// #2339: own code keeps an unidentified licence on the record, and does
    /// not get the do-not-copy banner for that unidentified licence. A named
    /// restricted licence still warns.
    #[test]
    fn own_suppresses_only_an_unidentified_copy_warning() {
        assert!(!copy_warning_applies("UNKNOWN", true));
        assert!(!copy_warning_applies("NONE-FOUND", true));
        assert!(!copy_warning_applies("", true));
        assert!(copy_warning_applies("UNKNOWN", false));
        assert!(copy_warning_applies("NONE-FOUND", false));
        assert!(
            copy_warning_applies("AGPL", true),
            "own does not relicense AGPL"
        );
        assert!(copy_warning_applies("GPL-3.0", true));
        assert!(copy_warning_applies("MPL-2.0", true));
        assert!(!copy_warning_applies("Apache-2.0", true));
        assert!(!copy_warning_applies("Apache-2.0", false));
        assert!(
            copy_warning_applies("UNKNOWN/MIT", true),
            "a mixed string that names a constituent is not unidentified"
        );
    }

    // ── a completed human review names an unidentified licence (#1893) ──

    #[test]
    fn a_completed_review_names_an_unknown_licence() {
        let review = serde_json::json!({
            "spdx": "CC0-1.0", "use": "adapt-with-attribution", "by": "xerj-org",
            "at": "2026-10-13", "note": "LICENSE.md verified. Detector reported UNKNOWN."
        });
        assert_eq!(effective_licence("UNKNOWN", Some(&review)), "CC0-1.0");
        assert_eq!(effective_licence("NONE-FOUND", Some(&review)), "CC0-1.0");
        assert!(clone_warning_line(&effective_licence("UNKNOWN", Some(&review))).is_none());
        // A licence the detector DID identify is never overridden.
        assert_eq!(effective_licence("GPL-3.0", Some(&review)), "GPL-3.0");
        assert_eq!(effective_licence("UNKNOWN", None), "UNKNOWN");
    }

    #[test]
    fn a_draft_or_odd_review_does_not_count() {
        let draft = serde_json::json!({ "spdx": "MIT", "use": "UNREVIEWED", "by": "", "at": "" });
        assert_eq!(effective_licence("UNKNOWN", Some(&draft)), "UNKNOWN");
        let no_by = serde_json::json!({ "spdx": "MIT", "use": "adapt-with-attribution", "by": "" });
        assert_eq!(reviewed_spdx(Some(&no_by)), None);
        let hostile = serde_json::json!({ "spdx": "MIT\u{1b}[2J", "use": "x", "by": "y" });
        assert_eq!(reviewed_spdx(Some(&hostile)), None);
        assert_eq!(reviewed_spdx(Some(&serde_json::json!("MIT"))), None);
    }

    // ── the detector's two ordering traps ──────────────────────────────────

    #[test]
    fn elasticsearch_triple_reads_agpl_not_apache() {
        // A full AGPL triple body must not fall through to Apache: the phrase
        // «an "Apache License 2.0" compatible license» lives in the AGPL text.
        let triple = "GNU Affero General Public License\nVersion 3, 19 November 2007\n\
                      ...an \"Apache License 2.0\" compatible license...";
        assert_eq!(classify_one(triple), "AGPL");
        // The Elastic title names itself.
        assert_eq!(
            classify_one("Elastic License 2.0\n\nwww.elastic.co"),
            "Elastic"
        );
    }

    #[test]
    fn mpl_body_citing_gpl_still_reads_mpl_title_first() {
        // MPL-2.0 defines "Secondary License" as «the GNU General Public
        // License, Version 2.0» inside its own text: a body-only scan with
        // restrictive-first ordering reads sonic's plain MPL-2.0 as GPL. The
        // TITLE wins; the body is only the fallback.
        let mpl = "Mozilla Public License\nVersion 2.0\n\
                   ...\"Secondary Licenses\" means the GNU General Public License, Version 2.0...";
        assert_eq!(classify_one(mpl), "MPL-2.0");
        // Fallback: a "# License" heading (title says nothing) drops to the
        // body scan — the previous behaviour.
        assert_eq!(
            classify_one("# License\n\nGNU General Public License\nVersion 2"),
            "GPL"
        );
    }

    #[test]
    fn permissive_bodies_classify_their_own_licence() {
        assert_eq!(
            classify_one("Apache License\nVersion 2.0, January 2004"),
            "Apache-2.0"
        );
        assert_eq!(
            classify_one(
                "MIT License\n\nPermission is hereby granted, free of charge, to any person"
            ),
            "MIT"
        );
        assert_eq!(
            classify_one(
                " Redistribution and use in source and binary forms, with or without\nmodification"
            ),
            "BSD"
        );
        assert_eq!(
            classify_one(
                "BSD 3-Clause License\n\n\
                 Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:\n\n\
                 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission."
            ),
            "BSD-3-Clause"
        );
        // No title, just the redistribution sentence plus the endorsement
        // clause. The first lines classify as coarse BSD; the body names it.
        assert_eq!(
            classify_one(
                "Redistribution and use in source and binary forms,\n\
                 with or without modification.\n\n\
                 Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission."
            ),
            "BSD-3-Clause"
        );
        assert_eq!(
            classify_one(
                "BSD 2-Clause License\n\nRedistribution and use in source and binary forms."
            ),
            "BSD-2-Clause"
        );
        assert_eq!(
            classify_one("Zero-Clause BSD\n\nPermission to use, copy, modify."),
            "0BSD"
        );
        assert!(licence_readings_agree("BSD-3-Clause", "BSD"));
        assert!(licence_readings_agree("BSD", "BSD-3-Clause"));
        assert!(licence_readings_agree("BSD-3-Clause", "BSD-3-Clause"));
        assert!(licence_readings_agree("MIT/Apache-2.0", "Apache-2.0/MIT"));
        assert!(!licence_readings_agree("MIT", "BSD"));
        assert!(licence_readings_agree("BSD-2-Clause", "BSD"));
        assert!(!licence_readings_agree("BSD-2-Clause", "BSD-3-Clause"));
        assert_eq!(
            classify_one("Random README text with no licence phrases"),
            "UNKNOWN"
        );
    }

    // ── detect_licence over fixture directories ───────────────────────────

    #[test]
    fn detect_reads_all_candidates_skips_3rdparty_and_dedups() {
        let holder = tempfile::tempdir().unwrap();
        let repo = holder.path().join("dual");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("LICENSE-MIT"),
            "MIT License\n\nPermission is hereby granted, free of charge",
        )
        .unwrap();
        std::fs::write(
            repo.join("LICENSE-APACHE"),
            "Apache License\nVersion 2.0, January 2004",
        )
        .unwrap();
        std::fs::write(
            repo.join("LICENSE-3rdparty.csv"),
            "dependency,licence\nserde,MIT\nsomething,GNU General Public License",
        )
        .unwrap();
        assert_eq!(
            detect_licence(&repo),
            "Apache-2.0/MIT",
            "dual-licensed crates join both, sorted; the 3rdparty inventory is skipped"
        );

        let empty = holder.path().join("nolicense");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(detect_licence(&empty), "NONE-FOUND");
    }

    #[test]
    fn detect_licence_reads_root_licence_of_a_sparse_paths_checkout() {
        // A `paths` harvest checks out only the named directories, so the
        // root LICENSE is in the pin but not in the worktree. It was
        // recorded NONE-FOUND.
        let holder = tempfile::tempdir().unwrap();
        let repo = holder.path().join("sparse");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("LICENSE"),
            "MIT License\n\nPermission is hereby granted, free of charge",
        )
        .unwrap();
        std::fs::write(repo.join("src/lib.zig"), "pub fn f() void {}\n").unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            st.status.success()
        };
        if !git(&["init", "-q"]) {
            return; // no git on this machine
        }
        assert!(git(&["add", "-A"]));
        assert!(git(&["commit", "-q", "-m", "init"]));
        assert!(git(&["sparse-checkout", "set", "--no-cone", "--", "/src/"]));
        assert!(
            !repo.join("LICENSE").exists(),
            "sparse checkout drops LICENSE"
        );
        assert_eq!(detect_licence(&repo), "MIT");

        // A plain subdirectory of a repository does not borrow its licence.
        assert_eq!(detect_licence(&repo.join("src")), "NONE-FOUND");
    }

    #[test]
    fn detect_licence_file_name_is_case_insensitive() {
        // Issue #2201: jfreechart ships `license.txt` (LGPL-2.1) and was
        // recorded NONE-FOUND.
        let holder = tempfile::tempdir().unwrap();
        let repo = holder.path().join("jfreechart");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("license.txt"),
            "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 2.1, February 1999",
        )
        .unwrap();
        assert_eq!(detect_licence(&repo), "LGPL");

        let mixed = holder.path().join("mixed");
        std::fs::create_dir_all(&mixed).unwrap();
        std::fs::write(
            mixed.join("License.md"),
            "MIT License\n\nPermission is hereby granted, free of charge",
        )
        .unwrap();
        assert_eq!(detect_licence(&mixed), "MIT");
    }

    #[test]
    fn detect_reads_title_first_then_body() {
        let holder = tempfile::tempdir().unwrap();
        let repo = holder.path().join("sonic");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("LICENSE"),
            "Mozilla Public License\nVersion 2.0\n\n\
             1. Definitions...\n\"Secondary Licenses\" means the GNU General Public License, Version 2.0...",
        )
        .unwrap();
        assert_eq!(detect_licence(&repo), "MPL-2.0");
    }

    #[test]
    fn detect_licence_name_variants_all_count() {
        let holder = tempfile::tempdir().unwrap();
        let repo = holder.path().join("uk");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("LICENCE"), "Apache License\nVersion 2.0").unwrap();
        std::fs::write(
            repo.join("COPYING"),
            "GNU General Public License\nVersion 2",
        )
        .unwrap();
        // Results sort as strings then join: "Apache-2.0" < "GPL".
        assert_eq!(detect_licence(&repo), "Apache-2.0/GPL");
    }
}
