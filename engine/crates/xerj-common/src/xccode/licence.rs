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

/// Find the licence without guessing: read the file(s) the project actually
/// ships. Dual-licensed crates (very common in Rust) ship LICENSE-MIT +
/// LICENSE-APACHE and no plain LICENSE, so every candidate is collected
/// rather than taking the first. Results are de-duplicated and joined with
/// "/" — "Apache-2.0/MIT" is the honest answer for a dual-licensed project;
/// collapsing it to one licence would be a false record. No candidate file
/// at all -> "NONE-FOUND".
pub fn detect_licence(repo_dir: &std::path::Path) -> String {
    let mut found: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(repo_dir) else {
        return "NONE-FOUND".to_string();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            n.starts_with("LICENSE") || n.starts_with("LICENCE") || n.starts_with("COPYING")
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
        if let Ok(text) = std::fs::read_to_string(repo_dir.join(&name)) {
            found.push(classify_one(&text).to_string());
        }
    }
    if found.is_empty() {
        return "NONE-FOUND".to_string();
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
    if from_title == "BSD" && (from_body == "UNKNOWN" || from_body == "BSD") {
        return "BSD";
    }
    if from_title == "UNKNOWN" {
        return from_body;
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
