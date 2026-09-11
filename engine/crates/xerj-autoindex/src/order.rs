//! In what order phase B drains the corpus — and why that order is what it is.
//!
//! Before this module the queue was sorted by size alone (biggest first, so
//! one enormous file could not serialise the tail of the run). That is a good
//! *scheduling* rule and a useless *value* rule: a user who stops a long run
//! early, or searches while it is still going, got whatever happened to be
//! largest — very often `node_modules`.
//!
//! So the queue is now ordered on two axes, in this priority:
//!
//! 1. **Value band** ([`Band`]) — source and prose first, then configuration,
//!    then structured data, then bulk line/log files, and vendored, generated
//!    and minified paths last. The band is decided from the sniffed family and
//!    the path, never the extension alone.
//! 2. **Biggest first inside a band** — the original rule, kept, because with
//!    `W` workers the largest file in a band runs *concurrently* with the rest
//!    of that band rather than ahead of it. Starting it last is what makes a
//!    run end with `W-1` idle workers. With `W == 1` there is no tail to hide,
//!    so a single-worker run is ordered smallest-first inside each band, which
//!    is what "searchable earliest" means when nothing is concurrent.
//!
//! One exception keeps axis 1 from costing wall clock it cannot buy back:
//! a file large enough that its own extraction outlasts everything ranked
//! above it — `size × workers > (all other planned bytes)` — is on the
//! critical path of the whole run and starts first regardless of band. At most
//! `workers` files can satisfy that, so the exception cannot swallow the
//! ordering.
//!
//! Prior art for the "sort by size descending, then bucket into levels"
//! shape: tantivy's `LogMergePolicy::compute_merge_candidates`
//! (`tantivy/src/indexer/log_merge_policy.rs:94-99`, MIT) sorts segments
//! `Reverse(max_doc)` and then chunks them into log-size levels. Adapted, not
//! copied: our levels are *value* bands, and size only orders within one.
//! The critical-path exception is the standard list-scheduling observation
//! (Graham, 1969) that makespan is bounded below by the longest single job.

use crate::sniff::Family;

/// Value bands, most valuable first. The discriminant IS the rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    /// Source code and human-authored documents: what a person actually asks
    /// questions about.
    SourceAndDocs = 0,
    /// Configuration and small structured trees — dense, high signal per byte.
    Config = 1,
    /// Structured data: rows, records, tables.
    Data = 2,
    /// Line-oriented bulk: logs and plain line files. Voluminous, low signal
    /// per byte, and rarely the reason someone indexed the folder.
    Bulk = 3,
    /// Vendored, generated or minified paths, whatever their family.
    Vendored = 4,
}

impl Band {
    /// Every band, in drain order.
    pub const ALL: [Band; 5] = [
        Band::SourceAndDocs,
        Band::Config,
        Band::Data,
        Band::Bulk,
        Band::Vendored,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Band::SourceAndDocs => "source-and-docs",
            Band::Config => "config",
            Band::Data => "data",
            Band::Bulk => "bulk",
            Band::Vendored => "vendored",
        }
    }

    /// The reason this band sits where it sits — printed with the plan so the
    /// order is never just an assertion.
    pub fn why(self) -> &'static str {
        match self {
            Band::SourceAndDocs => "source code and documents — what a person searches for first",
            Band::Config => {
                "configuration and small structured trees — dense, high signal per byte"
            }
            Band::Data => "structured data: rows, records and tables",
            Band::Bulk => "logs and plain line files — voluminous, low signal per byte",
            Band::Vendored => {
                "vendored, generated or minified paths — indexed last so stopping early costs \
                 nothing you care about"
            }
        }
    }
}

/// Directory names that mean "not written here": dependency trees, build
/// output, VCS internals, caches.
///
/// A false positive costs a file its place near the front of the queue and
/// nothing else — it is still indexed, still searchable, still complete. That
/// asymmetry is why the list can afford to include ambiguous names like
/// `build` and `out`.
const VENDORED_DIRS: &[&str] = &[
    ".cache",
    ".cargo",
    ".git",
    ".gradle",
    ".hg",
    ".mypy_cache",
    ".next",
    ".nuxt",
    ".pytest_cache",
    ".svelte-kit",
    ".svn",
    ".terraform",
    ".tox",
    ".venv",
    "Pods",
    "__pycache__",
    "bower_components",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "obj",
    "out",
    "site-packages",
    "target",
    "third_party",
    "thirdparty",
    "vendor",
    "venv",
];

/// Whole-file names that are machine-written by construction.
const GENERATED_FILES: &[&str] = &[
    "Cargo.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
    "package-lock.json",
    "pnpm-lock.yaml",
    "poetry.lock",
    "yarn.lock",
];

/// Suffixes that mean minified or generated content.
const GENERATED_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    ".min.mjs",
    ".map",
    ".pb.go",
    "_pb2.py",
    "_pb2_grpc.py",
    ".g.dart",
    ".generated.ts",
    ".generated.go",
];

/// Does this root-relative path (forward slashes) live in vendored, generated
/// or minified territory? Returns the segment or suffix that decided it, so
/// the plan can name the rule rather than assert the verdict.
pub fn vendored_reason(rel: &str) -> Option<&'static str> {
    for segment in rel.split('/') {
        if let Some(hit) = VENDORED_DIRS.iter().find(|d| **d == segment) {
            return Some(hit);
        }
    }
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if let Some(hit) = GENERATED_FILES.iter().find(|f| **f == name) {
        return Some(hit);
    }
    GENERATED_SUFFIXES
        .iter()
        .find(|s| name.len() > s.len() && name.ends_with(**s))
        .copied()
}

/// Which band a planned file belongs to.
pub fn band(rel: &str, family: Family) -> Band {
    if vendored_reason(rel).is_some() {
        return Band::Vendored;
    }
    match family {
        Family::Code
        | Family::TxtProse
        | Family::Html
        | Family::Pdf
        | Family::Docx
        | Family::Eml => Band::SourceAndDocs,
        Family::Yaml | Family::Json | Family::Xml => Band::Config,
        Family::Csv | Family::Jsonl | Family::Sqlite | Family::SqlDump => Band::Data,
        Family::Logs | Family::TxtLines => Band::Bulk,
        // Unity assets are the project's own authored content — scenes,
        // prefabs and materials are to a Unity repo what source files are to
        // a Rust one, so they rank with source, not with bulk data.
        Family::UnityYaml => Band::SourceAndDocs,
        // `.meta` sidecars carry the guid join key and nothing a human reads;
        // they are plumbing for `UnityYaml`, so they rank as config.
        Family::UnityMeta => Band::Config,
        // A `.bvh` yields ONE metadata record for a file that is mostly a
        // numeric motion block — data, not documentation.
        Family::Bvh => Band::Data,
        // `--stub` files are opened for a name card only. The owner asked for
        // them to be referenceable, not read, so they must not displace real
        // content in the queue.
        Family::Stub => Band::Bulk,
        // Binary never reaches phase B (phase A junks it); ranked last so a
        // future caller that does pass one cannot jump the queue with it.
        Family::Binary => Band::Vendored,
    }
}

/// `band` from the family *string* the durable plan stores. An unrecognised
/// family is ranked with the bulk band rather than promoted — an unknown
/// thing is not evidence of value.
pub fn band_from_family_str(rel: &str, family: &str) -> Band {
    if vendored_reason(rel).is_some() {
        return Band::Vendored;
    }
    match family {
        "code" | "txt-prose" | "html" | "pdf" | "docx" | "unity" => Band::SourceAndDocs,
        "yaml" | "json" | "xml" | "unity-meta" => Band::Config,
        "csv" | "jsonl" | "sqlite" | "sqldump" | "bvh" => Band::Data,
        // Pre-existing drift, caught by
        // `estimate::tests::the_two_band_functions_agree_for_every_family`:
        // `band` ranks Binary `Vendored`, but the catch-all below ranked the
        // persisted string `Bulk`, so a resumed run ordered it differently
        // from the run that planned it.
        "binary" => Band::Vendored,
        _ => Band::Bulk,
    }
}

/// One planned unit of phase-B work, as the ordering sees it.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    /// Index into the caller's own file array — returned verbatim.
    pub index: usize,
    pub band: Band,
    pub bytes: u64,
}

/// The order phase B should *start* the work in, most valuable first.
///
/// Returns caller indices. The caller's queue pops from the tail, so it must
/// reverse this before use — [`start_order_as_pop_queue`] does exactly that.
pub fn start_order(items: &[Item], workers: usize) -> Vec<usize> {
    let workers = workers.max(1);
    let total: u128 = items.iter().map(|i| i.bytes as u128).sum();

    let mut ranked: Vec<&Item> = items.iter().collect();
    // Band first; then biggest-first, except on a single worker where there is
    // no concurrency for a big file to hide inside.
    ranked.sort_by(|a, b| {
        a.band.cmp(&b.band).then_with(|| {
            if workers == 1 {
                a.bytes.cmp(&b.bytes)
            } else {
                b.bytes.cmp(&a.bytes)
            }
        })
    });

    // Critical path: a file whose own extraction outlasts everything the other
    // workers can do in parallel decides the wall clock by itself, so it must
    // start now whatever band it is in.
    let (mut critical, rest): (Vec<&Item>, Vec<&Item>) = ranked
        .into_iter()
        .partition(|item| is_critical_path(item.bytes, total, workers));
    critical.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.index.cmp(&b.index)));

    critical
        .into_iter()
        .chain(rest)
        .map(|item| item.index)
        .collect()
}

/// `size × workers > (everything else)` — this file cannot be hidden behind
/// the rest of the run. At most `workers` files can satisfy it.
pub fn is_critical_path(bytes: u64, total_bytes: u128, workers: usize) -> bool {
    let others = total_bytes.saturating_sub(bytes as u128);
    (bytes as u128).saturating_mul(workers.max(1) as u128) > others
}

/// [`start_order`] reversed, ready for a queue that is drained with `pop()`.
pub fn start_order_as_pop_queue(items: &[Item], workers: usize) -> Vec<usize> {
    let mut order = start_order(items, workers);
    order.reverse();
    order
}

/// Per-band totals, in drain order, for the plan output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BandSummary {
    pub band: &'static str,
    pub why: &'static str,
    pub files: u64,
    pub bytes: u64,
}

pub fn summarize(items: &[Item]) -> Vec<BandSummary> {
    Band::ALL
        .iter()
        .filter_map(|band| {
            let (files, bytes) = items
                .iter()
                .filter(|i| i.band == *band)
                .fold((0u64, 0u64), |(files, bytes), item| {
                    (files + 1, bytes + item.bytes)
                });
            (files > 0).then_some(BandSummary {
                band: band.as_str(),
                why: band.why(),
                files,
                bytes,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(index: usize, band: Band, bytes: u64) -> Item {
        Item { index, band, bytes }
    }

    #[test]
    fn source_and_docs_drain_before_vendored_whatever_the_size() {
        // The pre-#242 rule was size alone, so the 100 MB vendored blob went
        // first and the source tree went last.
        let items = [
            item(0, Band::Vendored, 100 << 20),
            item(1, Band::SourceAndDocs, 4 << 10),
            item(2, Band::SourceAndDocs, 8 << 10),
            item(3, Band::Config, 1 << 10),
        ];
        // 8 workers: the blob is 100 MB against 13 KB of everything else, so it
        // IS the critical path and legitimately starts first.
        assert_eq!(start_order(&items, 8), vec![0, 2, 1, 3]);

        // Give the valuable bands real weight and the blob stops dominating:
        // now value order decides, biggest-first inside each band.
        let items = [
            item(0, Band::Vendored, 100 << 20),
            item(1, Band::SourceAndDocs, 400 << 20),
            item(2, Band::SourceAndDocs, 500 << 20),
            item(3, Band::Config, 300 << 20),
        ];
        assert_eq!(start_order(&items, 8), vec![2, 1, 3, 0]);
    }

    #[test]
    fn a_single_worker_runs_smallest_first_inside_a_band() {
        let items = [
            item(0, Band::SourceAndDocs, 9),
            item(1, Band::SourceAndDocs, 1),
            item(2, Band::SourceAndDocs, 5),
        ];
        // W=1: nothing is concurrent, so "searchable earliest" is literally
        // "shortest first". (9 is still critical-path — 9 > 6 — so it leads;
        // the rest is smallest-first.)
        assert_eq!(start_order(&items, 1), vec![0, 1, 2]);

        let items = [
            item(0, Band::SourceAndDocs, 4),
            item(1, Band::SourceAndDocs, 5),
            item(2, Band::SourceAndDocs, 6),
        ];
        assert_eq!(start_order(&items, 1), vec![0, 1, 2]);
        // W>1 flips it back to biggest-first inside the band.
        assert_eq!(start_order(&items, 4), vec![2, 1, 0]);
    }

    /// The exception exists so value-first ordering cannot make the run
    /// *longer*: a file that outlasts all the parallel work above it has to
    /// start at t=0 or it becomes the tail on its own.
    #[test]
    fn a_file_that_outlasts_everything_above_it_starts_first() {
        let mut items = vec![item(0, Band::Vendored, 10_000)];
        for i in 1..=20 {
            items.push(item(i, Band::SourceAndDocs, 10));
        }
        // 10_000 × 4 > 200, so the vendored blob is the critical path.
        assert_eq!(start_order(&items, 4)[0], 0);
        assert!(is_critical_path(10_000, 10_200, 4));

        // Make the source band big enough to hide it and it goes back to last.
        let mut items = vec![item(0, Band::Vendored, 10_000)];
        for i in 1..=200 {
            items.push(item(i, Band::SourceAndDocs, 10_000));
        }
        assert!(!is_critical_path(10_000, 2_010_000, 4));
        assert_eq!(*start_order(&items, 4).last().unwrap(), 0);
    }

    #[test]
    fn the_pop_queue_is_the_start_order_reversed() {
        let items = [
            item(0, Band::Bulk, 1),
            item(1, Band::SourceAndDocs, 2),
            item(2, Band::Config, 3),
        ];
        let mut queue = start_order_as_pop_queue(&items, 2);
        let mut popped = Vec::new();
        while let Some(index) = queue.pop() {
            popped.push(index);
        }
        assert_eq!(popped, start_order(&items, 2));
    }

    #[test]
    fn vendored_paths_are_recognised_by_segment_name_and_by_suffix() {
        assert_eq!(
            vendored_reason("node_modules/left-pad/index.js"),
            Some("node_modules")
        );
        assert_eq!(
            vendored_reason("engine/target/release/build.rs"),
            Some("target")
        );
        assert_eq!(vendored_reason("web/app.min.js"), Some(".min.js"));
        assert_eq!(vendored_reason("Cargo.lock"), Some("Cargo.lock"));
        assert_eq!(vendored_reason("src/main.rs"), None);
        assert_eq!(vendored_reason("docs/README.md"), None);
        // A file literally called `.map` is not a source map.
        assert_eq!(vendored_reason(".map"), None);
        // A directory that merely *contains* the word is not the word.
        assert_eq!(vendored_reason("src/vendorish/a.rs"), None);
    }

    #[test]
    fn a_vendored_path_outranks_its_family() {
        assert_eq!(band("src/main.rs", Family::Code), Band::SourceAndDocs);
        assert_eq!(
            band("node_modules/x/index.js", Family::Code),
            Band::Vendored
        );
        assert_eq!(band("app.yaml", Family::Yaml), Band::Config);
        assert_eq!(band("var/log/app.log", Family::Logs), Band::Bulk);
        assert_eq!(band("data/rows.csv", Family::Csv), Band::Data);
        // The durable plan stores the family as a string; both paths agree.
        assert_eq!(
            band_from_family_str("src/main.rs", "code"),
            Band::SourceAndDocs
        );
        assert_eq!(band_from_family_str("x/a.csv", "csv"), Band::Data);
        assert_eq!(band_from_family_str("vendor/a.rs", "code"), Band::Vendored);
        // Unknown families are not promoted on a guess.
        assert_eq!(band_from_family_str("a.weird", "martian"), Band::Bulk);
    }

    #[test]
    fn the_band_summary_covers_every_planned_byte_exactly_once() {
        let items = [
            item(0, Band::SourceAndDocs, 10),
            item(1, Band::SourceAndDocs, 20),
            item(2, Band::Vendored, 70),
        ];
        let summary = summarize(&items);
        assert_eq!(summary.len(), 2, "empty bands are not printed: {summary:?}");
        assert_eq!(summary[0].band, "source-and-docs");
        assert_eq!((summary[0].files, summary[0].bytes), (2, 30));
        assert_eq!((summary[1].files, summary[1].bytes), (1, 70));
        assert_eq!(summary.iter().map(|b| b.bytes).sum::<u64>(), 100);
    }

    #[test]
    fn an_empty_corpus_orders_without_panicking() {
        assert!(start_order(&[], 8).is_empty());
        assert!(start_order(&[], 0).is_empty());
        assert!(summarize(&[]).is_empty());
        // A zero-byte corpus must not divide by anything.
        let items = [item(0, Band::Config, 0), item(1, Band::Config, 0)];
        assert_eq!(start_order(&items, 4).len(), 2);
    }
}
