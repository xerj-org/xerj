//! Neural ingest time estimate.
//!
//! The number comes from a probe the server measured (`GET /v1/embedding/identity`)
//! and the bytes of source that will be embedded. This module never substitutes
//! a constant rate. No probe, a zero probe, a non-neural backend, or zero
//! source bytes produces no estimate.
//!
//! The probe is one cold batch of code-dense text. Scaled linearly, it
//! overstated a real prose ingest about 7x (#2822). The note therefore leads
//! with the probe time divided by [`PROBE_OVER_STEADY_STATE`] and gives the
//! unscaled probe time as the upper bound. Both come from the measured rate.

use std::collections::{HashMap, HashSet};

use crate::esclient::EmbeddingExecutionIdentity;
use crate::estimate::{human_bytes, human_secs};
use crate::state::{FileAssignment, Plan};

/// How far the linear probe estimate overstated a real neural ingest (#2822).
///
/// From an rc.100 field report: a 13 MB markdown archive (4,530 records,
/// 9 datasets, linux x86_64) printed 42.8 min (2,568 s), and the ingest took
/// 348 s, a ratio of 7.38. The probe embeds 32 copies of a code-like line, which
/// tokenizes to roughly twice as many tokens per byte as prose. It also times
/// a single batch after a one-passage warmup, so it pays first-full-batch
/// costs that a long ingest amortizes. Code-dense corpora sit between the
/// calibrated time and the probe time, so the note prints both.
pub const PROBE_OVER_STEADY_STATE: f64 = 7.4;

/// What `source_bytes` counts, so the sentence does not call a whole NDJSON
/// file "the embedded text".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBytes {
    /// Files whose assignment feeds a dataset with a `semantic_text` field.
    SemanticSource,
    /// A `xerj index` NDJSON file. Embedded fields may be a subset of it.
    NdjsonFileUpperBound,
}

/// The note to print before a neural ingest, or `None` when there is nothing
/// honest to say (not neural, or no source bytes).
pub fn note(
    identity: &EmbeddingExecutionIdentity,
    source_bytes: u64,
    kind: SourceBytes,
) -> Option<String> {
    if identity.backend != "neural" || source_bytes == 0 {
        return None;
    }
    let probe = match (
        identity.ingest_probe_bytes,
        identity.ingest_probe_passages,
        identity.ingest_probe_millis,
    ) {
        (Some(bytes), Some(passages), Some(millis)) if bytes > 0 && passages > 0 && millis > 0 => {
            Some((bytes, passages, millis))
        }
        _ => None,
    };
    let subject = match kind {
        SourceBytes::SemanticSource => format!(
            "{} of source that feeds a semantic_text dataset",
            human_bytes(source_bytes)
        ),
        SourceBytes::NdjsonFileUpperBound => format!(
            "{} of NDJSON (an upper bound on embedded text, not a count of semantic_text fields)",
            human_bytes(source_bytes)
        ),
    };
    let sticky = "The embedding mode is fixed per index at ingest: changing --embed-mode does not \
                  re-embed documents already written; re-index into a new index. The faster default \
                  is lexical (`xerj --embed-mode lexical`, or omit the flag).";
    let body = match probe {
        Some((bytes, passages, millis)) => {
            let probe_seconds = estimate_seconds(source_bytes, bytes, millis);
            let seconds = calibrated_seconds(probe_seconds);
            let per_passage = bytes / u64::from(passages);
            let rate = (bytes as f64) * 1000.0 / (millis as f64);
            format!(
                "neural ingest: about {} to embed {subject} at the steady-state rate, and up to {} \
                 at the probe's own rate (code-dense text can approach that bound). From a measured \
                 probe of {passages} passages / {} ({per_passage} bytes each) in {millis} ms ({} of \
                 that probe text per second, timed after one untimed warmup passage); a cold \
                 code-dense probe overstated a measured prose ingest about \
                 {PROBE_OVER_STEADY_STATE}x, so the first figure divides by that. Embedding time \
                 only: chunk overlap is not included. {sticky}",
                human_secs(seconds),
                human_secs(probe_seconds),
                human_bytes(bytes),
                human_bytes(rate.round() as u64),
            )
        }
        None => format!(
            "neural ingest: {subject}. The embedding rate was not measured on this process, so \
             no time estimate is printed. Neural embedding is CPU-bound and much slower than the \
             default lexical embedder. {sticky}"
        ),
    };
    Some(body)
}

/// `source_bytes / (probe_bytes / (probe_millis / 1000))`.
///
/// Both inputs are measurements. The division is floating point only so a
/// sub-second estimate can be printed; it does not introduce a rate the probe
/// did not supply.
pub fn estimate_seconds(source_bytes: u64, probe_bytes: u64, probe_millis: u64) -> f64 {
    if probe_bytes == 0 || probe_millis == 0 {
        return f64::NAN;
    }
    (source_bytes as f64) * (probe_millis as f64) / ((probe_bytes as f64) * 1000.0)
}

/// The probe-linear estimate scaled to steady-state ingest (#2822).
pub fn calibrated_seconds(probe_seconds: f64) -> f64 {
    probe_seconds / PROBE_OVER_STEADY_STATE
}

/// Sum of file sizes that feed a dataset with `semantic_field` set.
///
/// A file is counted once. A missing size is skipped, not replaced with a guess.
pub fn semantic_source_bytes(plan: &Plan, sizes: &HashMap<&str, u64>) -> u64 {
    let semantic = semantic_slugs(plan);
    let mut total = 0u64;
    for (key, assignment) in &plan.files {
        if !feeds(&semantic, assignment) {
            continue;
        }
        if let Some(size) = sizes.get(key.as_str()) {
            total = total.saturating_add(*size);
        }
    }
    total
}

/// Bytes of semantic-feeding files a reconcile will embed again.
///
/// Unchanged content (equal digests, already feeding a semantic dataset) is
/// excluded: the generated path reuses it. A missing digest cannot prove
/// reuse, so that file is counted.
pub fn changed_semantic_source_bytes(
    previous: &HashMap<String, FileAssignment>,
    plan: &Plan,
    sizes: &HashMap<&str, u64>,
) -> u64 {
    let semantic = semantic_slugs(plan);
    let mut total = 0u64;
    for (key, assignment) in &plan.files {
        if !feeds(&semantic, assignment) {
            continue;
        }
        let changed = match previous.get(key) {
            None => true,
            Some(prior) => match (&prior.content_digest, &assignment.content_digest) {
                (Some(before), Some(after)) => before != after || !feeds(&semantic, prior),
                _ => true,
            },
        };
        if !changed {
            continue;
        }
        if let Some(size) = sizes.get(key.as_str()) {
            total = total.saturating_add(*size);
        }
    }
    total
}

fn semantic_slugs(plan: &Plan) -> HashSet<&str> {
    plan.datasets
        .iter()
        .filter(|dataset| dataset.semantic_field.is_some())
        .map(|dataset| dataset.slug.as_str())
        .collect()
}

fn feeds(semantic: &HashSet<&str>, assignment: &FileAssignment) -> bool {
    assignment
        .assignments
        .iter()
        .any(|(_, slug)| semantic.contains(slug.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PlanDataset;

    fn identity(backend: &str, probe: Option<(u64, u32, u64)>) -> EmbeddingExecutionIdentity {
        let (bytes, passages, millis) = match probe {
            Some((bytes, passages, millis)) => (Some(bytes), Some(passages), Some(millis)),
            None => (None, None, None),
        };
        EmbeddingExecutionIdentity {
            version: 1,
            backend: backend.into(),
            identity_sha256: "ab".repeat(32),
            dimensions: Some(384),
            semantic_contract: "semantic_text-derived-vector.v1".into(),
            resumable: true,
            non_resumable_reason: None,
            ingest_probe_bytes: bytes,
            ingest_probe_passages: passages,
            ingest_probe_millis: millis,
        }
    }

    #[test]
    fn estimate_is_source_bytes_over_the_measured_rate() {
        // 16_384 probe bytes in 1_000 ms is 16_384 B/s. 32_768 source bytes
        // is exactly 2 seconds. No other rate is in the formula.
        let seconds = estimate_seconds(32_768, 16_384, 1_000);
        assert!((seconds - 2.0).abs() < 1e-9, "{seconds}");
    }

    #[test]
    fn a_zero_probe_is_not_a_number() {
        assert!(estimate_seconds(1_000, 0, 1_000).is_nan());
        assert!(estimate_seconds(1_000, 1_000, 0).is_nan());
    }

    #[test]
    fn neural_note_quotes_the_probe_and_points_at_lexical() {
        let note = note(
            &identity("neural", Some((16_384, 32, 1_000))),
            32_768,
            SourceBytes::SemanticSource,
        )
        .expect("neural with a probe and source bytes");
        // 2.0 s at the probe rate; 2.0 / 7.4 = 0.27 s at steady state.
        assert!(note.contains("about 0.3 s"), "{note}");
        assert!(note.contains("up to 2.0 s"), "{note}");
        assert!(note.contains("32 passages"), "{note}");
        assert!(note.contains("16.0 KB"), "{note}");
        assert!(note.contains("re-index"), "{note}");
        assert!(note.contains("xerj --embed-mode lexical"), "{note}");
        assert!(note.contains("fixed per index"), "{note}");
        assert!(note.contains("warmup"), "{note}");
    }

    #[test]
    fn an_unmeasured_neural_probe_does_not_invent_a_duration() {
        for probe in [
            None,
            Some((0, 32, 1_000)),
            Some((16_384, 0, 1_000)),
            Some((16_384, 32, 0)),
        ] {
            let note = note(
                &identity("neural", probe),
                32_768,
                SourceBytes::SemanticSource,
            )
            .expect("still say the rate is unknown");
            assert!(note.contains("not measured"), "{note}");
            assert!(!note.contains("about "), "{note}");
            assert!(note.contains("xerj --embed-mode lexical"), "{note}");
        }
    }

    #[test]
    fn non_neural_and_empty_corpora_say_nothing() {
        let neural = identity("neural", Some((16_384, 32, 1_000)));
        assert!(note(&neural, 0, SourceBytes::SemanticSource).is_none());
        assert!(note(
            &identity("lexical", Some((16_384, 32, 1_000))),
            32_768,
            SourceBytes::SemanticSource
        )
        .is_none());
        assert!(note(
            &identity("proxy", None),
            32_768,
            SourceBytes::NdjsonFileUpperBound
        )
        .is_none());
    }

    #[test]
    fn ndjson_upper_bound_is_labeled_as_a_file_not_embedded_text() {
        let note = note(
            &identity("neural", Some((16_384, 32, 2_000))),
            16_384,
            SourceBytes::NdjsonFileUpperBound,
        )
        .unwrap();
        assert!(note.contains("upper bound"), "{note}");
        assert!(note.contains("about 0.3 s"), "{note}");
        assert!(note.contains("up to 2.0 s"), "{note}");
    }

    /// #2822: the reported run printed 42.8 min and took 348 s (5.8 min).
    /// The probe below reproduces the 42.8 min at the probe rate; the lead
    /// figure must land within 2x of the measured ingest.
    #[test]
    fn the_lead_estimate_matches_the_reported_prose_ingest() {
        let source_bytes = 13 * 1024 * 1024;
        let (probe_bytes, passages) = (16_384_u64, 32_u32);
        // Probe millis that make the linear estimate 2,568 s (42.8 min).
        let millis = (2_568.0 * 1000.0 * probe_bytes as f64 / source_bytes as f64).round() as u64;
        let probe = estimate_seconds(source_bytes, probe_bytes, millis);
        assert!((probe - 2_568.0).abs() < 2.0, "{probe}");
        let lead = calibrated_seconds(probe);
        let actual = 348.0;
        assert!(
            lead <= actual * 2.0 && lead >= actual / 2.0,
            "lead {lead} s vs actual {actual} s"
        );
        assert!((lead / actual - 1.0).abs() < 0.05, "{lead}");

        let note = note(
            &identity("neural", Some((probe_bytes, passages, millis))),
            source_bytes,
            SourceBytes::SemanticSource,
        )
        .unwrap();
        assert!(note.contains("about 5.8 min"), "{note}");
        assert!(note.contains("up to 42.8 min"), "{note}");
    }

    fn assignment(slug: &str, digest: Option<&str>) -> FileAssignment {
        FileAssignment {
            rel: format!("{slug}.txt"),
            path_id: String::new(),
            is_symlink: Some(false),
            family: "txtprose".into(),
            gzip: false,
            content_digest: digest.map(str::to_string),
            assignments: vec![(None, slug.into())],
            as_document: false,
        }
    }

    fn sample_plan(files: Vec<(&str, &str, Option<&str>)>, semantic: bool) -> Plan {
        Plan {
            datasets: vec![PlanDataset {
                slug: "docs".into(),
                index: "ax-docs".into(),
                family: "txtprose".into(),
                group: None,
                specs: Vec::new(),
                time_field: None,
                semantic_field: semantic.then(|| "body".into()),
                text_analyzer: None,
                scope: None,
                sampled_records: 0,
                file_count: files.len(),
            }],
            files: files
                .into_iter()
                .map(|(key, slug, digest)| (key.to_string(), assignment(slug, digest)))
                .collect(),
            ..Plan::default()
        }
    }

    #[test]
    fn semantic_bytes_count_each_feeding_file_once_and_skip_missing_sizes() {
        let plan = sample_plan(
            vec![
                ("a", "docs", Some("1")),
                ("b", "other", Some("2")),
                ("c", "docs", Some("3")),
            ],
            true,
        );
        let sizes = HashMap::from([("a", 100u64), ("b", 9_000u64)]);
        assert_eq!(semantic_source_bytes(&plan, &sizes), 100);
        let lexical = sample_plan(vec![("a", "docs", Some("1"))], false);
        assert_eq!(semantic_source_bytes(&lexical, &sizes), 0);
    }

    #[test]
    fn changed_bytes_skip_an_unchanged_digest() {
        let previous_plan = sample_plan(
            vec![("a", "docs", Some("same")), ("b", "docs", Some("old"))],
            true,
        );
        let next = sample_plan(
            vec![
                ("a", "docs", Some("same")),
                ("b", "docs", Some("new")),
                ("c", "docs", Some("fresh")),
            ],
            true,
        );
        let sizes = HashMap::from([("a", 10u64), ("b", 20u64), ("c", 30u64)]);
        assert_eq!(
            changed_semantic_source_bytes(&previous_plan.files, &next, &sizes),
            50
        );
    }

    #[test]
    fn a_missing_digest_is_counted_because_reuse_is_unproven() {
        let previous_plan = sample_plan(vec![("a", "docs", None)], true);
        let next = sample_plan(vec![("a", "docs", None)], true);
        let sizes = HashMap::from([("a", 40u64)]);
        assert_eq!(
            changed_semantic_source_bytes(&previous_plan.files, &next, &sizes),
            40
        );
    }

    #[test]
    fn identity_json_keeps_probe_fields_optional() {
        let bare = r#"{"version":1,"backend":"neural","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","dimensions":384,"semantic_contract":"semantic_text-derived-vector.v1","resumable":true}"#;
        let parsed: EmbeddingExecutionIdentity = serde_json::from_str(bare).unwrap();
        assert!(parsed.ingest_probe_millis.is_none());
        let with = r#"{"version":1,"backend":"neural","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","semantic_contract":"semantic_text-derived-vector.v1","resumable":true,"ingest_probe_bytes":16384,"ingest_probe_passages":32,"ingest_probe_millis":1000}"#;
        let parsed: EmbeddingExecutionIdentity = serde_json::from_str(with).unwrap();
        assert_eq!(parsed.ingest_probe_millis, Some(1000));
        let encoded = serde_json::to_string(&identity("lexical", None)).unwrap();
        assert!(!encoded.contains("ingest_probe"), "{encoded}");
    }
}
