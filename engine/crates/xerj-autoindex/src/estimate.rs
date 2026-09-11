//! How long the rest of this run will take, measured on **this** machine.
//!
//! The rule this module exists to obey: never state a number we did not
//! measure here. A constant calibrated on the maintainers' box is worthless on
//! an Apple Silicon laptop with a cold page cache, and a single confident
//! figure is worse than no figure at all — so this produces a *range* with its
//! basis attached, or it produces nothing and says why.
//!
//! # What is measured
//!
//! Phase A already reads and parses every file to sniff and sample it. That is
//! free evidence, and [`Meter`] collects it: for each file whose extraction
//! phase A **provably ran to completion over the whole file**, it records
//! (family, bytes, elapsed). Files that phase A only partially read contribute
//! nothing to the rate — a partial read timed against a full size would invent
//! throughput the machine never demonstrated.
//!
//! "Provably complete" is decided by [`exact_scan_bytes`], not by assumption:
//!
//! * whole-file extractors (`json`, `html`, `yaml`, `txt-prose`, `code`,
//!   `docx`, `pdf`) call `read_whole`/parse the entire container before they
//!   emit anything, so a non-junk scan read every byte;
//! * streaming extractors (`jsonl`, `csv`, `logs`, `txt-lines`, `xml`,
//!   `sqldump`) stop at whichever of the sampling byte cap or the record cap
//!   comes first, so they count only when the file was under the byte cap
//!   *and* produced fewer records than the cap — i.e. hit EOF;
//! * `sqlite` is row-capped per table with no byte relationship at all, and
//!   gzipped files spend their time on the decompressed stream while `size` is
//!   the compressed one, so neither is ever measured.
//!
//! # What the range means
//!
//! Per family, `rate = measured_bytes / measured_seconds`. Per planned file,
//! `work_i = bytes_i / rate_family`. With `W` phase-B workers the makespan of
//! that work is bounded by the classical list-scheduling result (Graham, 1969):
//!
//! ```text
//! low  = max(Σ work_i / W, max work_i)      // nothing can beat this
//! high = Σ work_i / W + (1 - 1/W) · max work_i
//! ```
//!
//! Both ends therefore come from measured throughput plus arithmetic; neither
//! is a fudge factor.
//!
//! # This is a FLOOR, and it is labelled as one everywhere
//!
//! The range covers **client-side extraction only**. The server's own
//! indexing, embedding and merge time, the network, the NDJSON rendering and
//! relationship detection are not modelled, because autoindex cannot measure
//! any of them without writing to the index — which is exactly the thing the
//! estimate exists to ask permission for. A probe write would buy a better
//! number at the cost of the property that makes stopping safe ("nothing of
//! yours has been touched"), so it is not taken.
//!
//! Two consequences, both stated in the output rather than buried here:
//!
//! * The number is a **lower bound**, not a prediction. Measured on a
//!   68 MB source tree (120 Rust files, 40 Markdown, 15 YAML, 6 log files) on
//!   a 32-core box: extraction floor 0.1 s, real wall clock 8.9 s. The floor
//!   was ~2% of the truth, because for text families the run is dominated by
//!   transport and by the server. It is far closer for PDF/DOCX/SQLite
//!   corpora, where client-side parsing genuinely is the job.
//! * So the gate **under-asks and never over-asks**. When it fires, the run is
//!   certainly too long; when it does not fire, that is not a promise the run
//!   is short — and [`Estimate::headline`] and the decision payload say so in
//!   those words. Inventing a multiplier to close the gap would make the gate
//!   fire on a number nobody measured, which is the one thing this file exists
//!   to prevent.
//!
//! Prior art consulted: quickwit's `ThroughputCalculator`
//! (`quickwit/quickwit-cli/src/tool.rs:977-1012`, Apache-2.0) keeps a windowed
//! `VecDeque<(Instant, u64)>` and reports bytes/s — and stops there, offering
//! no ETA. ClickHouse's `ProgressIndication`
//! (`ClickHouse/src/Common/ProgressIndication.cpp:118-119,223-227`,
//! Apache-2.0) computes `bytes × 1e9 / elapsed_ns` and refuses to draw a bar
//! until 500 ms have passed, so a number is never shown before it is earned.
//! meilisearch's `milli::progress` reports named steps and no ETA whatsoever.
//! None of the three estimates a job *before* running it; that part is ours,
//! and the conservatism above is the price of doing it at all.

use crate::sniff::Family;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// Sampling byte cap for the line-oriented streaming families (mirrors
/// `crate::SAMPLE_LIMIT_BYTES`).
const SAMPLE_LIMIT_BYTES: u64 = 4 << 20;
/// Sampling byte cap for SQL dumps (mirrors `crate::SQLDUMP_SAMPLE_LIMIT`).
const SQLDUMP_SAMPLE_LIMIT: u64 = 64 << 20;
/// Sampling byte cap for Unity assets (mirrors `crate::UNITY_SAMPLE_LIMIT`).
const UNITY_SAMPLE_LIMIT: u64 = 512 << 20;
/// Whole-file cap for `.meta` sidecars (mirrors `extract::unity::META_CAP`).
const UNITY_META_CAP: u64 = 16 << 20;

/// Bytes phase A **provably** read in full for this file, or `None` when the
/// read was or may have been partial.
///
/// `records` is the number of records the sampler accepted, `sample` the
/// per-group cap it stops at, `gzip` whether the source is compressed.
pub fn exact_scan_bytes(
    family: Family,
    gzip: bool,
    size: u64,
    records: u64,
    sample: usize,
) -> Option<u64> {
    if gzip {
        // `size` is compressed bytes; the parser spent its time on the
        // inflated stream. There is no honest rate to derive from that pair.
        return None;
    }
    let hit_eof = records < sample as u64;
    match family {
        // read_whole()/full-container parsers: a non-junk scan read all of it.
        Family::Json
        | Family::Html
        | Family::Yaml
        | Family::TxtProse
        | Family::Code
        | Family::Docx
        | Family::Eml
        | Family::Pdf => Some(size),
        // Streaming, byte-capped and record-capped.
        Family::Jsonl | Family::Csv | Family::Logs | Family::TxtLines => {
            (size <= SAMPLE_LIMIT_BYTES && hit_eof).then_some(size)
        }
        // Streaming, record-capped only (no byte limit is passed).
        Family::Xml => hit_eof.then_some(size),
        // Grouped: the sink never stops it early, so only the byte cap applies.
        Family::SqlDump => (size <= SQLDUMP_SAMPLE_LIMIT).then_some(size),
        // Grouped like SqlDump — the sink reads on so every Unity class gets
        // sampled — so again only the byte cap can have truncated the read.
        Family::UnityYaml => (size <= UNITY_SAMPLE_LIMIT).then_some(size),
        // `read_whole` up to META_CAP: under the cap the read is complete,
        // over it the file is junked after reading only cap+1 bytes.
        Family::UnityMeta => (size <= UNITY_META_CAP).then_some(size),
        // Row-capped per table; bytes read bear no fixed relation to `size`.
        Family::Sqlite => None,
        // Deliberately partial: `bvh::extract` stops at `Frame Time:` and
        // never pulls the motion block off disk, so `size` is not the number
        // of bytes this machine demonstrated it can chew through.
        Family::Bvh => None,
        // `--stub` files are never opened. Zero bytes read for a non-zero
        // `size` would fabricate an unbounded rate.
        Family::Stub => None,
        // Never extracted at all.
        Family::Binary => None,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Acc {
    files: u64,
    bytes: u64,
    nanos: u128,
}

/// Thread-safe collector for phase-A throughput evidence.
///
/// One short lock per scanned file. Phase A spends milliseconds to seconds per
/// file, so the contention is not measurable next to the work it is timing.
#[derive(Debug, Default)]
pub struct Meter {
    inner: Mutex<BTreeMap<&'static str, Acc>>,
}

impl Meter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one file whose extraction phase A ran end to end.
    pub fn record(&self, family: Family, bytes: u64, elapsed: Duration) {
        if bytes == 0 {
            // A zero-byte file times the syscall, not the throughput.
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let acc = inner.entry(family.as_str()).or_default();
        acc.files += 1;
        acc.bytes += bytes;
        acc.nanos += elapsed.as_nanos();
    }

    /// Measured throughput per family. Families with no usable evidence are
    /// simply absent — never present with a guessed rate.
    pub fn rates(&self) -> BTreeMap<&'static str, MeasuredRate> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(family, acc)| {
                if acc.nanos == 0 || acc.bytes == 0 {
                    return None;
                }
                let seconds = acc.nanos as f64 / 1e9;
                Some((
                    *family,
                    MeasuredRate {
                        files: acc.files,
                        bytes: acc.bytes,
                        bytes_per_sec: acc.bytes as f64 / seconds,
                    },
                ))
            })
            .collect()
    }
}

/// Throughput actually observed for one family during phase A.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MeasuredRate {
    /// Files this rate was measured over (whole-file reads only).
    pub files: u64,
    /// Bytes those files contained.
    pub bytes: u64,
    pub bytes_per_sec: f64,
}

/// One planned phase-B file, as the estimator sees it.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub family: String,
    pub bytes: u64,
}

/// Per-family arithmetic behind the range.
#[derive(Debug, Clone, Serialize)]
pub struct FamilyEstimate {
    pub family: String,
    pub planned_files: u64,
    pub planned_bytes: u64,
    pub measured_files: u64,
    pub measured_bytes: u64,
    pub bytes_per_sec: f64,
    pub seconds_of_work: f64,
}

/// A family with planned bytes and no usable measurement.
#[derive(Debug, Clone, Serialize)]
pub struct UnmeasuredFamily {
    pub family: String,
    pub planned_files: u64,
    pub planned_bytes: u64,
    pub reason: &'static str,
}

/// The estimate, or the honest absence of one.
#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    /// What kind of number this is. A constant, present so an agent parsing
    /// the payload cannot read `low_seconds` as a prediction of wall clock.
    pub kind: &'static str,
    /// Lower bound in seconds — `None` when nothing could be measured.
    pub low_seconds: Option<f64>,
    /// Upper bound in seconds — `None` when nothing could be measured.
    pub high_seconds: Option<f64>,
    /// One sentence naming what the numbers came from, or why there are none.
    pub basis: String,
    pub workers: usize,
    pub planned_files: u64,
    pub planned_bytes: u64,
    /// Planned bytes belonging to a family with a measured rate.
    pub covered_bytes: u64,
    /// `covered_bytes / planned_bytes`, or `None` when there are no bytes.
    pub coverage: Option<f64>,
    pub families: Vec<FamilyEstimate>,
    pub unmeasured_families: Vec<UnmeasuredFamily>,
    /// Costs deliberately outside the model, named so nobody has to guess.
    pub excludes: Vec<&'static str>,
}

impl Estimate {
    /// Build the estimate from the planned work and the phase-A evidence.
    ///
    /// `workers` is the phase-B worker count the resource policy (#240)
    /// settled on — this module does not re-derive parallelism.
    pub fn compute(
        planned: &[PlannedFile],
        rates: &BTreeMap<&'static str, MeasuredRate>,
        workers: usize,
    ) -> Self {
        let workers = workers.max(1);
        let planned_bytes: u64 = planned.iter().map(|f| f.bytes).sum();
        let planned_files = planned.len() as u64;

        let mut per_family: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for file in planned {
            let entry = per_family.entry(file.family.as_str()).or_default();
            entry.0 += 1;
            entry.1 += file.bytes;
        }

        let mut families = Vec::new();
        let mut unmeasured_families = Vec::new();
        let mut covered_bytes = 0u64;
        let mut total_work = 0f64;
        for (family, (files, bytes)) in &per_family {
            match rates.get(family) {
                Some(rate) if rate.bytes_per_sec > 0.0 => {
                    let seconds = *bytes as f64 / rate.bytes_per_sec;
                    covered_bytes += *bytes;
                    total_work += seconds;
                    families.push(FamilyEstimate {
                        family: (*family).to_string(),
                        planned_files: *files,
                        planned_bytes: *bytes,
                        measured_files: rate.files,
                        measured_bytes: rate.bytes,
                        bytes_per_sec: rate.bytes_per_sec,
                        seconds_of_work: seconds,
                    });
                }
                _ => unmeasured_families.push(UnmeasuredFamily {
                    family: (*family).to_string(),
                    planned_files: *files,
                    planned_bytes: *bytes,
                    reason: "phase A never read a file of this family end to end on this run, so \
                             no throughput was measured for it",
                }),
            }
        }
        families.sort_by(|a, b| {
            b.seconds_of_work
                .total_cmp(&a.seconds_of_work)
                .then_with(|| a.family.cmp(&b.family))
        });
        unmeasured_families.sort_by(|a, b| {
            b.planned_bytes
                .cmp(&a.planned_bytes)
                .then_with(|| a.family.cmp(&b.family))
        });

        // The longest single job — no amount of parallelism divides it.
        let longest = planned
            .iter()
            .filter_map(|file| {
                rates
                    .get(file.family.as_str())
                    .filter(|rate| rate.bytes_per_sec > 0.0)
                    .map(|rate| file.bytes as f64 / rate.bytes_per_sec)
            })
            .fold(0f64, f64::max);

        let coverage = (planned_bytes > 0).then(|| covered_bytes as f64 / planned_bytes as f64);

        if families.is_empty() {
            return Self {
                kind: ESTIMATE_KIND,
                low_seconds: None,
                high_seconds: None,
                basis: if planned_files == 0 {
                    "nothing to index".to_string()
                } else {
                    format!(
                        "no estimate: phase A did not read any of the {planned_files} planned \
                         file(s) end to end, so no throughput was measured on this machine. \
                         Families seen: {}",
                        unmeasured_families
                            .iter()
                            .map(|f| f.family.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                workers,
                planned_files,
                planned_bytes,
                covered_bytes,
                coverage,
                families,
                unmeasured_families,
                excludes: EXCLUDES.to_vec(),
            };
        }

        let parallel = total_work / workers as f64;
        let low = parallel.max(longest);
        let high = parallel + longest * (1.0 - 1.0 / workers as f64);

        let measured_files: u64 = families.iter().map(|f| f.measured_files).sum();
        let measured_bytes: u64 = families.iter().map(|f| f.measured_bytes).sum();
        let basis = format!(
            "measured on this machine during phase A: {measured_files} file(s) / {} read end to \
             end across {} family/families, scheduled over {workers} worker(s). Covers {} of the \
             {} planned ({}). Client-side extraction only.",
            human_bytes(measured_bytes),
            families.len(),
            human_bytes(covered_bytes),
            human_bytes(planned_bytes),
            match coverage {
                Some(fraction) => format!("{:.0}% of planned bytes", fraction * 100.0),
                None => "no planned bytes".to_string(),
            }
        );

        Self {
            kind: ESTIMATE_KIND,
            low_seconds: Some(low),
            high_seconds: Some(high),
            basis,
            workers,
            planned_files,
            planned_bytes,
            covered_bytes,
            coverage,
            families,
            unmeasured_families,
            excludes: EXCLUDES.to_vec(),
        }
    }

    /// The bound the gate compares against: the top of the range. The whole
    /// range is a floor, so taking its upper end is the least
    /// under-conservative reading available.
    pub fn gate_seconds(&self) -> Option<f64> {
        self.high_seconds
    }

    /// `14.2 min–19.8 min`, or an honest sentence when there is no number.
    /// Bare — always print it through [`Estimate::headline`] where a reader
    /// could mistake it for a prediction.
    pub fn range_text(&self) -> String {
        match (self.low_seconds, self.high_seconds) {
            (Some(low), Some(high)) => format!("{}–{}", human_secs(low), human_secs(high)),
            _ => "no estimate (see basis)".to_string(),
        }
    }

    /// The range with what it is stapled to it. Every surface that shows a
    /// user or an agent a number uses this, because "14 min" and "at least
    /// 14 min" are different promises and only the second one is true.
    pub fn headline(&self) -> String {
        match self.low_seconds {
            Some(_) => format!(
                "at least {} — a MEASURED FLOOR for client-side extraction, not a prediction of \
                 the whole run: server indexing, embedding and network time are not in it",
                self.range_text()
            ),
            None => format!("no estimate — {}", self.basis),
        }
    }

    /// The estimate re-run over a subset of the planned work, so an option
    /// like "index only this subdirectory" can be costed with the same
    /// measured rates instead of a new guess.
    pub fn recompute_subset(
        &self,
        planned: &[PlannedFile],
        rates: &BTreeMap<&'static str, MeasuredRate>,
    ) -> Self {
        Self::compute(planned, rates, self.workers)
    }
}

/// Stamped on every [`Estimate`]: this is a lower bound on client-side
/// extraction, never a prediction of the run's wall clock.
pub const ESTIMATE_KIND: &str = "client-side-extraction-floor";

const EXCLUDES: &[&str] = &[
    "server-side indexing, embedding and merge time (autoindex cannot measure it without writing)",
    "network round trips to the endpoint",
    "relationship detection and edge writes (--no-graph removes these)",
    "families with no end-to-end read in phase A, listed under unmeasured_families",
];

/// `1.4 GB`, `812.0 MB`, `4.0 KB`, `37 B`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

/// `3.2 s`, `4.7 min`, `1.9 h`.
pub fn human_secs(seconds: f64) -> String {
    if seconds < 90.0 {
        format!("{seconds:.1} s")
    } else if seconds < 5400.0 {
        format!("{:.1} min", seconds / 60.0)
    } else {
        format!("{:.1} h", seconds / 3600.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(family: &'static str, bytes_per_sec: f64) -> (&'static str, MeasuredRate) {
        (
            family,
            MeasuredRate {
                files: 10,
                bytes: 1 << 20,
                bytes_per_sec,
            },
        )
    }

    fn planned(family: &str, bytes: u64, count: usize) -> Vec<PlannedFile> {
        (0..count)
            .map(|_| PlannedFile {
                family: family.to_string(),
                bytes,
            })
            .collect()
    }

    /// The two bounds are Graham's, computed from measured throughput and
    /// nothing else.
    #[test]
    fn the_range_is_the_scheduling_bound_on_measured_work() {
        // 4 files × 100 bytes at 100 B/s = 1 s each, 4 s of work, 2 workers.
        let rates = BTreeMap::from([rate("code", 100.0)]);
        let estimate = Estimate::compute(&planned("code", 100, 4), &rates, 2);
        let low = estimate.low_seconds.unwrap();
        let high = estimate.high_seconds.unwrap();
        // parallel = 4/2 = 2; longest = 1.
        assert!((low - 2.0).abs() < 1e-9, "{low}");
        assert!((high - 2.5).abs() < 1e-9, "{high}");
        assert!(low <= high);
        assert_eq!(estimate.coverage, Some(1.0));
        assert!(estimate.basis.contains("measured on this machine"));
    }

    /// One dominant file cannot be divided by the worker count. The lower
    /// bound has to say so, or the gate under-asks on exactly the corpus that
    /// most deserves a question.
    #[test]
    fn one_enormous_file_sets_the_floor_however_many_workers_there_are() {
        let rates = BTreeMap::from([rate("sqldump", 1.0)]);
        let mut work = planned("sqldump", 1_000, 1);
        work.extend(planned("sqldump", 1, 100));
        let estimate = Estimate::compute(&work, &rates, 64);
        // total work 1100 s over 64 workers = 17.2 s, but one job is 1000 s.
        assert!(estimate.low_seconds.unwrap() >= 1000.0);
        assert!(estimate.high_seconds.unwrap() >= estimate.low_seconds.unwrap());
    }

    /// The rule that keeps this module honest: no measurement, no number.
    #[test]
    fn an_unmeasurable_corpus_produces_no_number_at_all() {
        let estimate = Estimate::compute(&planned("pdf", 5_000, 3), &BTreeMap::new(), 8);
        assert_eq!(estimate.low_seconds, None);
        assert_eq!(estimate.high_seconds, None);
        assert!(estimate.gate_seconds().is_none());
        assert!(
            estimate.basis.starts_with("no estimate:"),
            "{}",
            estimate.basis
        );
        assert!(estimate.basis.contains("pdf"));
        assert_eq!(estimate.coverage, Some(0.0));
        assert_eq!(estimate.unmeasured_families.len(), 1);
        assert_eq!(estimate.range_text(), "no estimate (see basis)");
    }

    /// Partial coverage is reported as partial, not silently extrapolated onto
    /// the families nobody timed.
    #[test]
    fn unmeasured_families_are_named_and_excluded_from_the_arithmetic() {
        let rates = BTreeMap::from([rate("code", 1_000.0)]);
        let mut work = planned("code", 1_000, 1);
        work.extend(planned("sqlite", 9_000, 1));
        let estimate = Estimate::compute(&work, &rates, 1);
        assert_eq!(estimate.covered_bytes, 1_000);
        assert_eq!(estimate.planned_bytes, 10_000);
        assert_eq!(estimate.coverage, Some(0.1));
        assert_eq!(estimate.unmeasured_families[0].family, "sqlite");
        assert!(estimate.basis.contains("10%"));
        // The sqlite bytes contribute NO seconds — an estimate that quietly
        // priced them at the code rate would be a fabrication.
        assert!((estimate.low_seconds.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_plan_says_so_instead_of_dividing_by_zero() {
        let estimate = Estimate::compute(&[], &BTreeMap::new(), 8);
        assert_eq!(estimate.basis, "nothing to index");
        assert_eq!(estimate.coverage, None);
        assert_eq!(estimate.low_seconds, None);
    }

    #[test]
    fn a_subset_is_costed_with_the_same_measured_rates() {
        let rates = BTreeMap::from([rate("code", 100.0)]);
        let full = Estimate::compute(&planned("code", 100, 10), &rates, 1);
        let half = full.recompute_subset(&planned("code", 100, 5), &rates);
        assert_eq!(half.workers, full.workers);
        assert!((half.low_seconds.unwrap() * 2.0 - full.low_seconds.unwrap()).abs() < 1e-9);
    }

    /// Only reads phase A can prove were complete may be timed.
    #[test]
    fn exact_scan_bytes_admits_only_provably_complete_reads() {
        // Whole-file parsers: always the full size.
        for family in [
            Family::Json,
            Family::Html,
            Family::Yaml,
            Family::TxtProse,
            Family::Code,
            Family::Docx,
            Family::Pdf,
        ] {
            assert_eq!(exact_scan_bytes(family, false, 900, 500, 500), Some(900));
        }
        // Streaming + byte-capped: under the cap AND short of the record cap.
        assert_eq!(
            exact_scan_bytes(Family::Jsonl, false, 1 << 10, 10, 500),
            Some(1 << 10)
        );
        // Hit the record cap → the tail of the file was never read.
        assert_eq!(
            exact_scan_bytes(Family::Jsonl, false, 1 << 10, 500, 500),
            None
        );
        // Over the sampling byte cap → truncated read.
        assert_eq!(exact_scan_bytes(Family::Csv, false, 8 << 20, 3, 500), None);
        // XML has no byte cap, only the record cap.
        assert_eq!(
            exact_scan_bytes(Family::Xml, false, 8 << 20, 3, 500),
            Some(8 << 20)
        );
        // SQL dumps are grouped: the sink never stops them, only the byte cap.
        assert_eq!(
            exact_scan_bytes(Family::SqlDump, false, 1 << 20, 9_999, 500),
            Some(1 << 20)
        );
        assert_eq!(
            exact_scan_bytes(Family::SqlDump, false, 128 << 20, 1, 500),
            None
        );
        // Unity assets are grouped like SQL dumps: byte cap only.
        assert_eq!(
            exact_scan_bytes(Family::UnityYaml, false, 1 << 20, 9_999, 500),
            Some(1 << 20)
        );
        assert_eq!(
            exact_scan_bytes(Family::UnityYaml, false, 1024 << 20, 1, 500),
            None
        );
        // `.meta` sidecars are read whole up to META_CAP.
        assert_eq!(
            exact_scan_bytes(Family::UnityMeta, false, 4 << 10, 1, 500),
            Some(4 << 10)
        );
        assert_eq!(
            exact_scan_bytes(Family::UnityMeta, false, 32 << 20, 1, 500),
            None
        );
        // Never measurable.
        assert_eq!(exact_scan_bytes(Family::Sqlite, false, 100, 1, 500), None);
        assert_eq!(exact_scan_bytes(Family::Binary, false, 100, 1, 500), None);
        // BVH stops at the motion header by design, so `size` is never the
        // number of bytes it read; `--stub` files are never opened at all.
        // Either one returning `Some(size)` would invent throughput.
        assert_eq!(
            exact_scan_bytes(Family::Bvh, false, 500 << 20, 1, 500),
            None
        );
        assert_eq!(
            exact_scan_bytes(Family::Stub, false, 500 << 20, 1, 500),
            None
        );
        // Compressed: `size` is not the number of bytes the parser worked on.
        assert_eq!(exact_scan_bytes(Family::Json, true, 100, 1, 500), None);
    }

    /// `band` and `band_from_family_str` are the SAME decision made from a
    /// live enum and from the string the durable plan persisted. They drifted
    /// silently once already: the string form's `_ => Bulk` catch-all meant a
    /// resumed run demoted every Unity asset out of the source band. Every
    /// family must agree across the two.
    #[test]
    fn the_two_band_functions_agree_for_every_family() {
        use crate::order::{band, band_from_family_str};
        for family in [
            Family::Jsonl,
            Family::Json,
            Family::Csv,
            Family::Logs,
            Family::Xml,
            Family::Html,
            Family::Yaml,
            Family::TxtProse,
            Family::TxtLines,
            Family::Pdf,
            Family::Docx,
            Family::Sqlite,
            Family::SqlDump,
            Family::Code,
            Family::UnityYaml,
            Family::UnityMeta,
            Family::Bvh,
            Family::Stub,
            Family::Binary,
        ] {
            let rel = "a/b.dat";
            assert_eq!(
                band(rel, family),
                band_from_family_str(rel, family.as_str()),
                "{} disagrees between the enum and string band functions",
                family.as_str()
            );
        }
    }

    #[test]
    fn the_meter_reports_only_families_it_actually_timed() {
        let meter = Meter::new();
        meter.record(Family::Code, 1_000, Duration::from_millis(10));
        meter.record(Family::Code, 1_000, Duration::from_millis(10));
        // Zero-byte files time a syscall, not throughput.
        meter.record(Family::Json, 0, Duration::from_millis(5));
        let rates = meter.rates();
        assert_eq!(rates.len(), 1);
        let code = rates["code"];
        assert_eq!((code.files, code.bytes), (2, 2_000));
        // 2000 bytes in 20 ms = 100_000 B/s.
        assert!((code.bytes_per_sec - 100_000.0).abs() < 1.0, "{code:?}");
        assert!(Meter::new().rates().is_empty());
    }

    #[test]
    fn human_renderings_are_stable() {
        assert_eq!(human_bytes(37), "37 B");
        assert_eq!(human_bytes(4 << 10), "4.0 KB");
        assert_eq!(human_bytes(3 << 20), "3.0 MB");
        assert_eq!(human_bytes(2 << 30), "2.0 GB");
        assert_eq!(human_secs(3.24), "3.2 s");
        assert_eq!(human_secs(282.0), "4.7 min");
        assert_eq!(human_secs(6840.0), "1.9 h");
    }
}
