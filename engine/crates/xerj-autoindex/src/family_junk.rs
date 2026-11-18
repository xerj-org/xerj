//! Per-family junk warnings for the autoindex run summary (#2370).
//!
//! Exit 3 already says some files were junk. It does not say that a whole
//! file type disappeared: four PDFs that all failed to parse print the same
//! "completed-with-junk" summary as one unreadable side file next to a healthy
//! corpus. The catalog lists each junked path, and that is the only place the
//! loss used to be visible.
//!
//! A family warns when its junked files are a **strict majority** of the files
//! classified as indexed or junk (`junk_files * 2 > files`). That includes
//! every file of the family (`extent: all`). A tie (1 of 2) stays on the
//! ordinary junk count. Stale-plan skips are neither indexed nor junk; they
//! already have their own summary and are not part of this ratio.
//!
//! `top_reason` is the junk reason shared by the most files of that family.
//! Equal counts break by Unicode code-point order of the reason, smaller
//! first — not by hash-map iteration. An empty reason is published as
//! `(no reason recorded)`.
//!
//! The array is omitted when nothing tripped it, so a clean run document keeps
//! the fields it had. Existing summary fields are not renamed. Exit codes are
//! not changed: ordinary junk stays exit 3, and a family-wide PDF worker
//! startup failure stays exit 1.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::state::{JunkFile, Plan};
use crate::sync::ManifestGroup;

/// `all` — every indexed-or-junk file of the family was junked.
/// `most` — junked files are a strict majority, and at least one file was indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FamilyJunkExtent {
    All,
    Most,
}

/// One family the run summary should name. Field names are the `--json` contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FamilyJunkWarning {
    pub family: String,
    pub files: u64,
    pub junk_files: u64,
    pub indexed_files: u64,
    pub extent: FamilyJunkExtent,
    pub top_reason: String,
    pub top_reason_files: u64,
}

/// What an empty junk reason becomes in the summary. The catalog file row can
/// still carry an empty `reason`; the summary does not print a blank one.
pub(crate) const NO_REASON: &str = "(no reason recorded)";

impl FamilyJunkWarning {
    /// One human line. Starts with `warning:` so it is visible in the stdout
    /// summary and on the progress stream. Names the family, the counts, and
    /// the top reason.
    pub(crate) fn line(&self) -> String {
        let verb = if self.junk_files == 1 { "was" } else { "were" };
        let body = match self.extent {
            FamilyJunkExtent::All => format!(
                "{} family: all {} {verb} junked and not indexed",
                self.family,
                crate::estimate::count_phrase(self.files, "file", "files"),
            ),
            FamilyJunkExtent::Most => format!(
                "{} family: {} of {} {verb} junked and not indexed",
                self.family,
                crate::estimate::count_phrase(self.junk_files, "file", "files"),
                crate::estimate::count_phrase(self.files, "file", "files"),
            ),
        };
        format!(
            "warning: {body}; top reason ({}): {}",
            crate::estimate::count_phrase(self.top_reason_files, "file", "files"),
            self.top_reason,
        )
    }
}

/// Family key for a catalog format string. `pdf` and `pdf(gzip)` are one family.
pub(crate) fn family_of_format(format: &str) -> &str {
    let bare = format.strip_suffix("(gzip)").unwrap_or(format);
    if bare.is_empty() {
        "unknown"
    } else {
        bare
    }
}

#[derive(Default)]
struct Acc {
    indexed: u64,
    junk: u64,
    reasons: BTreeMap<String, u64>,
}

/// Warnings for one finished run.
///
/// `indexed_formats` are files that reached the index (catalog status
/// `indexed`), format strings with the optional `(gzip)` suffix.
/// `junk` is the junk/skipped list after shadowed entries have been dropped.
/// Stale-plan skips are ignored here.
pub(crate) fn warnings<'a>(
    indexed_formats: impl IntoIterator<Item = &'a str>,
    junk: impl IntoIterator<Item = &'a JunkFile>,
) -> Vec<FamilyJunkWarning> {
    let mut by_family: BTreeMap<String, Acc> = BTreeMap::new();
    for format in indexed_formats {
        let acc = by_family
            .entry(family_of_format(format).to_string())
            .or_default();
        acc.indexed = acc.indexed.saturating_add(1);
    }
    for junk_file in junk {
        if crate::is_stale_resume_skip(junk_file) {
            continue;
        }
        let acc = by_family
            .entry(family_of_format(&junk_file.format).to_string())
            .or_default();
        acc.junk = acc.junk.saturating_add(1);
        let reason = if junk_file.reason.is_empty() {
            NO_REASON.to_string()
        } else {
            junk_file.reason.clone()
        };
        *acc.reasons.entry(reason).or_default() += 1;
    }

    let mut out = Vec::new();
    for (family, acc) in by_family {
        let Some(extent) = extent_for(acc.indexed, acc.junk) else {
            continue;
        };
        let (top_reason, top_reason_files) = top_reason(&acc.reasons);
        let files = acc.indexed.saturating_add(acc.junk);
        out.push(FamilyJunkWarning {
            family,
            files,
            junk_files: acc.junk,
            indexed_files: acc.indexed,
            extent,
            top_reason,
            top_reason_files,
        });
    }
    out
}

/// The same classification [`warnings`] uses, read off a sealed generation:
/// groups are the indexed files, `plan.junk_files` is the junk list.
pub(crate) fn from_groups(plan: &Plan, groups: &[ManifestGroup]) -> Vec<FamilyJunkWarning> {
    let indexed: Vec<String> = groups
        .iter()
        .filter_map(|group| {
            let assignment = plan.files.get(&group.content_id)?;
            Some(if assignment.gzip {
                format!("{}(gzip)", assignment.family)
            } else {
                assignment.family.clone()
            })
        })
        .collect();
    warnings(indexed.iter().map(String::as_str), plan.junk_files.iter())
}

/// Lines for a run document that already carries `family_junk_warnings`.
/// Malformed entries are skipped; a missing field is no lines.
pub(crate) fn lines_from_summary(summary: &Value) -> Vec<String> {
    summary
        .get("family_junk_warnings")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| serde_json::from_value::<FamilyJunkWarning>(entry.clone()).ok())
                .map(|warning| warning.line())
                .collect()
        })
        .unwrap_or_default()
}

/// Strict majority, with the whole family called out separately.
/// `junk * 2 > indexed + junk`, and `junk > 0`.
fn extent_for(indexed: u64, junk: u64) -> Option<FamilyJunkExtent> {
    let files = indexed.saturating_add(junk);
    if junk == 0 || files == 0 {
        return None;
    }
    if junk == files {
        return Some(FamilyJunkExtent::All);
    }
    if junk.saturating_mul(2) > files {
        Some(FamilyJunkExtent::Most)
    } else {
        None
    }
}

/// Highest count wins. Ties break to the code-point-smaller reason.
fn top_reason(counts: &BTreeMap<String, u64>) -> (String, u64) {
    counts
        .iter()
        .max_by(|(reason_a, count_a), (reason_b, count_b)| {
            count_a.cmp(count_b).then_with(|| reason_b.cmp(reason_a))
        })
        .map(|(reason, count)| (reason.clone(), *count))
        .unwrap_or_else(|| (NO_REASON.to_string(), 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::JunkFile;

    fn junk(format: &str, reason: &str) -> JunkFile {
        JunkFile {
            file_key: format!("k-{format}-{reason}"),
            rel: format!("f.{format}"),
            format: format.to_string(),
            status: "junk".into(),
            reason: reason.into(),
            bytes: 1,
        }
    }

    fn stale(format: &str) -> JunkFile {
        let mut file = junk(format, "not in the frozen resume plan");
        file.status = "stale-plan".into();
        file
    }

    #[test]
    fn all_junked_family_names_the_count_and_top_reason() {
        let junked = [
            junk("pdf", "could not start the isolated PDF parser"),
            junk("pdf", "could not start the isolated PDF parser"),
            junk("pdf(gzip)", "could not start the isolated PDF parser"),
            junk("pdf", "damaged trailer"),
        ];
        let warnings = warnings(["code", "code", "txt-prose"], junked.iter());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let pdf = &warnings[0];
        assert_eq!(pdf.family, "pdf");
        assert_eq!(pdf.extent, FamilyJunkExtent::All);
        assert_eq!(pdf.files, 4);
        assert_eq!(pdf.junk_files, 4);
        assert_eq!(pdf.indexed_files, 0);
        assert_eq!(pdf.top_reason, "could not start the isolated PDF parser");
        assert_eq!(pdf.top_reason_files, 3);
        let line = pdf.line();
        assert!(line.starts_with("warning: pdf family:"), "{line}");
        assert!(line.contains("all 4 files"), "{line}");
        assert!(line.contains("were junked and not indexed"), "{line}");
        assert!(
            line.contains("top reason (3 files): could not start the isolated PDF parser"),
            "{line}"
        );
        // The indexed families are clean and must not appear.
        assert!(!line.contains("code"));
        assert!(!line.contains("txt-prose"));
    }

    #[test]
    fn mostly_junked_family_warns_at_a_strict_majority() {
        // 3 junk + 1 indexed = 3/4, strict majority, not the whole family.
        let junked = [
            junk("docx", "encrypted"),
            junk("docx", "encrypted"),
            junk("docx", "truncated"),
        ];
        let warnings = warnings(["docx", "code", "code"], junked.iter());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let docx = &warnings[0];
        assert_eq!(docx.family, "docx");
        assert_eq!(docx.extent, FamilyJunkExtent::Most);
        assert_eq!((docx.files, docx.junk_files, docx.indexed_files), (4, 3, 1));
        assert_eq!(docx.top_reason, "encrypted");
        assert_eq!(docx.top_reason_files, 2);
        let line = docx.line();
        assert!(line.contains("docx family: 3 files of 4 files"), "{line}");
        assert!(line.contains("were junked"), "{line}");
        assert!(line.contains("top reason (2 files): encrypted"), "{line}");
    }

    #[test]
    fn a_clean_family_and_a_minority_of_junk_do_not_warn() {
        // code: 3 indexed, 0 junk. pdf: 1 junk of 3 (not a majority). html: 1 of 2 (tie).
        let junked = [junk("pdf", "damaged"), junk("html", "empty")];
        let warnings = warnings(
            ["code", "code", "code", "pdf", "pdf", "html"],
            junked.iter(),
        );
        assert!(
            warnings.is_empty(),
            "clean, minority, and tie families stay off the warning list: {warnings:?}"
        );
        assert!(lines_from_summary(&serde_json::json!({})).is_empty());
        assert!(lines_from_summary(&serde_json::json!({"files_junk": 2})).is_empty());
    }

    #[test]
    fn equal_top_reasons_break_by_code_point_order() {
        let junked = [
            junk("pdf", "zeta failure"),
            junk("pdf", "alpha failure"),
            junk("pdf", "zeta failure"),
            junk("pdf", "alpha failure"),
        ];
        let warnings = warnings(std::iter::empty(), junked.iter());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].extent, FamilyJunkExtent::All);
        assert_eq!(warnings[0].top_reason, "alpha failure");
        assert_eq!(warnings[0].top_reason_files, 2);
    }

    #[test]
    fn an_empty_reason_is_named_and_a_single_file_uses_the_singular() {
        let junked = [junk("png", "")];
        let warnings = warnings(std::iter::empty(), junked.iter());
        assert_eq!(warnings[0].top_reason, NO_REASON);
        assert_eq!(warnings[0].top_reason_files, 1);
        let line = warnings[0].line();
        assert!(line.contains("all 1 file was junked"), "{line}");
        assert!(
            line.contains("top reason (1 file): (no reason recorded)"),
            "{line}"
        );
    }

    #[test]
    fn stale_plan_skips_are_not_part_of_the_family() {
        // Three stale PDFs and no junk: nothing to warn about.
        let only_stale = [stale("pdf"), stale("pdf"), stale("pdf")];
        assert!(warnings(std::iter::empty(), only_stale.iter()).is_empty());

        // One real junk PDF beside stale skips: the stale files are not a
        // denominator, so the one junked PDF is the whole classified family.
        let mixed = [stale("pdf"), junk("pdf", "damaged")];
        let warnings = warnings(std::iter::empty(), mixed.iter());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].extent, FamilyJunkExtent::All);
        assert_eq!(warnings[0].files, 1);
        assert_eq!(warnings[0].top_reason, "damaged");
    }

    #[test]
    fn families_are_sorted_and_the_summary_array_round_trips() {
        let junked = [junk("pdf", "worker"), junk("csv", "header")];
        let warnings = warnings(["md"], junked.iter());
        assert_eq!(
            warnings
                .iter()
                .map(|warning| warning.family.as_str())
                .collect::<Vec<_>>(),
            vec!["csv", "pdf"]
        );
        let summary = serde_json::json!({
            "files_junk": 2,
            "family_junk_warnings": warnings,
        });
        // Existing fields stay put.
        assert_eq!(summary["files_junk"], 2);
        let lines = lines_from_summary(&summary);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("csv family"));
        assert!(lines[1].contains("pdf family"));
        // A non-object entry does not drop the well-formed ones.
        let mut dirty = summary;
        dirty["family_junk_warnings"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("not an object"));
        assert_eq!(lines_from_summary(&dirty).len(), 2);
    }

    #[test]
    fn two_of_three_is_most_and_two_of_four_is_not() {
        assert_eq!(extent_for(1, 2), Some(FamilyJunkExtent::Most));
        assert_eq!(extent_for(0, 2), Some(FamilyJunkExtent::All));
        assert_eq!(extent_for(2, 2), None, "a tie is not a majority");
        assert_eq!(extent_for(3, 1), None);
        assert_eq!(extent_for(5, 0), None);
    }
}
