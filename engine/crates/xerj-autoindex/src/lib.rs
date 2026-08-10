//! `xerj autoindex` — point it at ANY folder and it makes the contents
//! AI-searchable with ZERO configuration. Pure ES-compat HTTP client feature:
//! it does NOT link xerj-engine, works against any endpoint, and cannot
//! destabilize the server.

pub mod catalog;
pub mod cli;
pub mod coerce;
mod content;
pub mod correlate;
pub mod dataset;
pub mod detect;
pub mod esclient;
pub mod extract;
pub mod ids;
pub mod infer;
pub mod pool;
pub mod progress;
pub mod resources;
pub mod sniff;
pub mod state;
pub mod walk;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use cli::{Cmd, IndexCfg, MapCfg, StatusCfg};
// Trait must be in scope for `href_raw.counters()` (a concrete `Href`, not a
// `Box<dyn EdgeDetector>` like the registry entries).
use detect::EdgeDetector as _;
use esclient::Es;
use progress::Progress;
use sniff::{Family, Sniffed};
use state::{DuplicateFile, FileAssignment, FileDone, JunkFile, Plan, PlanDataset};

/// Entry point for the `xerj autoindex` subcommand (blocking; the server
/// binary calls this via spawn_blocking). Returns the process exit code.
pub fn run_cli() -> i32 {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let cmd = match cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n");
            cli::print_help();
            return 2;
        }
    };
    let res = match cmd {
        Cmd::Help => {
            cli::print_help();
            return 0;
        }
        Cmd::Index(cfg) => run_index(cfg),
        Cmd::Map(cfg) => run_map(cfg),
        Cmd::Status(cfg) => run_status(cfg),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

const GB: u64 = 1 << 30;
const SAMPLE_LIMIT_BYTES: u64 = 4 << 20;
const SQLDUMP_SAMPLE_LIMIT: u64 = 64 << 20;

#[cfg(test)]
static REPLACEMENT_FAILPOINT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
fn replacement_failpoint(boundary: u8) -> Result<()> {
    if REPLACEMENT_FAILPOINT
        .compare_exchange(boundary, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        anyhow::bail!("injected replacement crash boundary {boundary}");
    }
    Ok(())
}

#[cfg(not(test))]
#[inline]
fn replacement_failpoint(_boundary: u8) -> Result<()> {
    Ok(())
}

fn record_bulk_outcome(
    es: &Es,
    body: Vec<u8>,
    junk_records: &AtomicU64,
    bulk_errors: &Mutex<Vec<String>>,
    send_err: &mut Option<String>,
) -> bool {
    match es.bulk(body) {
        Ok(outcome) => {
            if outcome.server_errors > 0 {
                *send_err = Some(format!(
                    "bulk backend failed for {} item(s): {}. Source file was not journaled \
                     complete; fix the server/embedding configuration and rerun autoindex",
                    outcome.server_errors,
                    outcome
                        .first_server_error
                        .as_deref()
                        .unwrap_or("unknown server error")
                ));
                return true;
            }
            if outcome.item_errors > 0 {
                junk_records.fetch_add(outcome.item_errors, Ordering::Relaxed);
                if let Some(error) = outcome.first_error {
                    let mut errors = bulk_errors.lock().unwrap();
                    if errors.len() < 5 {
                        errors.push(error);
                    }
                }
            }
            false
        }
        Err(error) => {
            *send_err = Some(format!("{error:#}"));
            true
        }
    }
}

// ─── second-brain graph runtime ──────────────────────────────────────────

/// Per-run graph state, shared read-only with the Phase B workers
/// (SECOND_BRAIN_SPEC §6.6). Built after the plan is final because the
/// detectors resolve links against the FULL corpus — a per-file view could
/// not tell "dangling" from "not walked yet".
struct GraphRt {
    corpus: detect::CorpusIndex,
    detectors: Vec<Box<dyn detect::EdgeDetector>>,
    /// Raw-source href pass handle. Lives outside the registry because the
    /// HTML extractor strips markup before sectioning, so anchors only exist
    /// in the raw bytes — a source the `EdgeDetector` trait deliberately
    /// never sees (see `detect::href` module docs).
    href_raw: detect::href::Href,
    edges_index: String,
    brain: String,
    /// ONE wall-clock stamp per run: `created_at` is the single
    /// non-deterministic edge field (§6.4); per-worker clocks would make two
    /// halves of one run disagree about when it happened.
    created_at_ms: i64,
    /// detector tag → edges written this run (run-summary honesty §6.6.4).
    written: Mutex<std::collections::BTreeMap<&'static str, u64>>,
    self_dropped: AtomicU64,
    /// Prior-generation edges soft-invalidated before this run's writes.
    invalidated: u64,
}

/// Text-section locator → human label ("section 3", "page 2 section 0").
/// `emit_document` section locators are `s{i}`; PDF sections are
/// `p{page}-s{sec}` (extract/pdf.rs — page-major, so stream order IS the
/// lexicographic (page, sec) reading order). Everything else (row/line/byte/
/// table locators) is not a text section, returns None, and must not reach
/// `detect_text`. The label is used verbatim in sequence evidence rationales.
fn section_label(locator: &str) -> Option<String> {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    if let Some(rest) = locator.strip_prefix('s') {
        return digits(rest).then(|| format!("section {rest}"));
    }
    let rest = locator.strip_prefix('p')?;
    let (page, sec) = rest.split_once("-s")?;
    (digits(page) && digits(sec)).then(|| format!("page {page} section {sec}"))
}

// ─── Phase A: per-file scan (sniff + bounded sampling) ───────────────────

struct FileScan {
    sniffed: Option<Sniffed>,
    sketches: Vec<GroupSketch>,
    junk: Option<(String, String)>, // (status, reason)
}

/// One sampled group within a file: every field it produced, plus the names
/// that came from the file rather than from the extractor (`FieldOrigin`).
/// Only the latter may decide which dataset the file joins — see `dataset`.
struct GroupSketch {
    group: Option<String>,
    fields: HashMap<String, infer::FieldAcc>,
    key_fields: std::collections::HashSet<String>,
    records: u64,
}

fn scan_file(path: &Path, size: u64, sample: usize, max_file_gb: u64) -> FileScan {
    let mut out = FileScan {
        sniffed: None,
        sketches: Vec::new(),
        junk: None,
    };
    let sn = match sniff::sniff(path) {
        Ok(s) => s,
        Err(e) => {
            out.junk = Some(("junk".into(), format!("unreadable: {e}")));
            return out;
        }
    };
    if sn.family == Family::Binary {
        out.junk = Some((
            "junk".into(),
            format!(
                "binary content ({})",
                sn.binary_kind.clone().unwrap_or_else(|| "unknown".into())
            ),
        ));
        out.sniffed = Some(sn);
        return out;
    }
    // whole-file families get a size cap; streaming families don't need one
    let whole_file = matches!(
        sn.family,
        Family::Json | Family::Html | Family::Yaml | Family::TxtProse | Family::Pdf | Family::Docx
    );
    if whole_file && size > max_file_gb * GB {
        out.junk = Some((
            "skipped".into(),
            format!(
                "oversized for non-streaming family {} (> {max_file_gb} GB)",
                sn.family.as_str()
            ),
        ));
        out.sniffed = Some(sn);
        return out;
    }
    let limit = match sn.family {
        Family::SqlDump => Some(SQLDUMP_SAMPLE_LIMIT),
        Family::Jsonl | Family::Logs | Family::Csv | Family::TxtLines => Some(SAMPLE_LIMIT_BYTES),
        Family::Sqlite => Some(1), // signals per-table row cap inside the extractor
        _ => None,                 // whole-file extractors cap themselves
    };
    type GroupAcc = (
        HashMap<String, infer::FieldAcc>,
        u64,
        std::collections::HashSet<String>,
    );
    let mut groups: HashMap<Option<String>, GroupAcc> = HashMap::new();
    let grouped_family = matches!(sn.family, Family::SqlDump | Family::Sqlite);
    let mut sink = |rec: extract::RawRecord| -> bool {
        let entry = groups.entry(rec.group.clone()).or_default();
        if (entry.1 as usize) < sample {
            // Every field feeds type inference; only the ones the FILE named
            // feed the clustering key, so an extractor that starts emitting a
            // new field cannot re-home the file (#178).
            let from_file = rec.origin == extract::FieldOrigin::Data;
            for (k, v) in &rec.fields {
                entry.0.entry(k.clone()).or_default().add(v);
                if from_file {
                    entry.2.insert(k.clone());
                }
            }
        }
        entry.1 += 1;
        if grouped_family {
            true // read on — later tables still need sampling
        } else {
            (entry.1 as usize) < sample
        }
    };
    match extract::extract(path, &sn, limit, &mut sink) {
        Ok(stats) => {
            if groups.is_empty() {
                out.junk = Some((
                    "junk".into(),
                    format!(
                        "no records extracted ({} candidate family, {} junk lines)",
                        sn.family.as_str(),
                        stats.junk
                    ),
                ));
            }
        }
        Err(e) => {
            if groups.is_empty() {
                out.junk = Some(("junk".into(), format!("extract failed: {e}")));
            }
        }
    }
    out.sketches = groups
        .into_iter()
        .map(|(group, (fields, records, key_fields))| GroupSketch {
            group,
            fields,
            key_fields,
            records,
        })
        .collect();
    out.sketches.sort_by(|a, b| a.group.cmp(&b.group));
    out.sniffed = Some(sn);
    out
}

#[cfg(test)]
mod clustering_key_tests {
    use super::*;

    fn scan(dir: &Path, name: &str, body: &str) -> FileScan {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        scan_file(&path, size, 500, 2)
    }

    /// The #178 mechanism, from the extractor to the clustering key: a source
    /// file that yields symbols and one that yields none produce the SAME
    /// (empty) key, so no extractor improvement can move a file between
    /// datasets. `defs` still reaches the mapping — it is indexed, just not
    /// used to decide identity.
    #[test]
    fn symbols_never_enter_the_clustering_key() {
        let dir = std::env::temp_dir().join("xerj-ax-178-key");
        std::fs::create_dir_all(&dir).unwrap();

        // #170 now captures the `const`, so this file parses to one symbol and
        // GAINS a `defs` field — the case #170 improves. The point of this test
        // survives that change: the newly-captured extractor name must still be
        // kept out of the clustering key (#180), so a const-only file and a
        // fn/struct file still land in ONE dataset rather than re-homing apart.
        let table = scan(
            &dir,
            "table.rs",
            "const BYTE_FREQUENCIES: [u8; 2] = [1, 2];\n",
        );
        let code = scan(&dir, "code.rs", "fn main() {}\nstruct S;\n");
        let _ = std::fs::remove_dir_all(&dir);

        for s in [&table, &code] {
            assert_eq!(s.sketches.len(), 1, "one code record per file");
            assert!(
                s.sketches[0].key_fields.is_empty(),
                "extractor-invented names leaked into the clustering key: {:?}",
                s.sketches[0].key_fields
            );
        }
        // #170: the const is now captured, so the table file has `defs` too.
        assert!(table.sketches[0].fields.contains_key("defs"));
        assert!(code.sketches[0].fields.contains_key("defs"));

        // …so the two land in one dataset instead of one dataset each.
        let rels = vec!["table.rs".to_string(), "code.rs".to_string()];
        let sketches: Vec<dataset::Sketch> = [&table, &code]
            .iter()
            .enumerate()
            .map(|(i, s)| dataset::Sketch {
                file_idx: i,
                group: s.sketches[0].group.clone(),
                family: s.sniffed.as_ref().unwrap().family,
                fields: s.sketches[0].fields.clone(),
                key_fields: s.sketches[0].key_fields.clone(),
                records: s.sketches[0].records,
            })
            .collect();
        let scopes = vec![String::new(); rels.len()];
        let clusters = dataset::cluster(sketches, &rels, &scopes);
        assert_eq!(clusters.len(), 1, "{clusters:#?}");
    }

    /// The other half: a data file's own column names ARE the key, so real
    /// schemas still drive clustering.
    #[test]
    fn data_field_names_are_the_clustering_key() {
        let dir = std::env::temp_dir().join("xerj-ax-178-data");
        std::fs::create_dir_all(&dir).unwrap();
        let csv = scan(&dir, "t.csv", "id,email\n1,a@b.c\n2,d@e.f\n");
        let _ = std::fs::remove_dir_all(&dir);
        let mut names: Vec<&str> = csv.sketches[0]
            .key_fields
            .iter()
            .map(String::as_str)
            .collect();
        names.sort();
        assert_eq!(names, ["email", "id"]);
    }
}

#[cfg(test)]
mod phase_a_grouping_tests {
    use super::*;

    fn cfg_for(root: &Path) -> IndexCfg {
        IndexCfg {
            root: root.to_path_buf(),
            url: "http://unused.invalid".into(),
            api_key: None,
            workers: 1,
            scan_workers: 1,
            pdf_workers: 1,
            resource_notes: Vec::new(),
            pdf_timeout_secs: 10,
            bulk_mb: 1,
            bulk_timeout_secs: 10,
            prefix: "t".into(),
            state_dir: None,
            fresh: true,
            follow_symlinks: false,
            max_file_gb: 2,
            sample: 500,
            no_semantic: false,
            brain: None,
            no_graph: true,
            dry_run: true,
            json: false,
            quiet: true,
            progress: crate::progress::ProgressMode::None,
            progress_interval: None,
        }
    }

    fn plan_for(root: &Path) -> Plan {
        let files = walk::walk(root, false).unwrap();
        let keys: Vec<String> = files
            .iter()
            .map(|f| ids::file_key(&f.path, f.size).unwrap())
            .collect();
        let digests: Vec<String> = (0..files.len()).map(|i| format!("d{i}")).collect();
        let (plan, _) = build_phase_a(
            root,
            &files,
            &keys,
            &digests,
            Vec::new(),
            &cfg_for(root),
            &Progress::silent(),
        );
        plan
    }

    const CODE: &str = "// The event loop dispatches every ready connection to a worker.\n\
        // Each worker drains its queue before polling for more sockets.\n\
        static void dispatch_ready_connections(struct event_loop *loop) {\n\
            for (int index = 0; index < loop->ready_count; index++) {\n\
                worker_submit(loop->workers, loop->ready[index]);\n\
            }\n\
        }\n";

    const PROSE: &str = "# Overview\n\nThis server accepts client connections and stores \
        keys in memory. Every command travels through the same parser before the \
        dispatcher routes it to the matching handler function.\n";

    /// #173 end to end through the real planner: a tree holding two
    /// repositories of source, prose and one-off config JSON yields one
    /// document dataset per repository — with `body` elected `semantic_text`
    /// — plus the genuine data dataset, instead of one dataset per incidental
    /// config schema with no vector arm.
    #[test]
    fn a_two_repo_tree_plans_one_document_dataset_per_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for repo in ["valkey", "memcached"] {
            std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
            std::fs::write(root.join(repo).join(".git").join("HEAD"), "ref: x").unwrap();
        }
        std::fs::create_dir_all(root.join("valkey/src")).unwrap();
        std::fs::create_dir_all(root.join("valkey/commands")).unwrap();
        std::fs::create_dir_all(root.join("memcached/data")).unwrap();
        std::fs::write(root.join("valkey/src/server.c"), CODE).unwrap();
        std::fs::write(root.join("valkey/README.md"), PROSE).unwrap();
        // one-off configs: single-record JSON, each with its own key set
        std::fs::write(
            root.join("valkey/commands/get.json"),
            r#"{"GET": {"summary": "Return the string value stored at the given key.", "arity": 2}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("valkey/commands/set.json"),
            r#"{"SET": {"summary": "Store the given string value under the given key.", "arity": 3}}"#,
        )
        .unwrap();
        // distinct bytes — byte-identical files alias into one canonical copy
        std::fs::write(root.join("memcached/proto.c"), format!("{CODE}\n// v2\n")).unwrap();
        // a genuine data file: recurring rows, real schema
        std::fs::write(
            root.join("memcached/data/events.csv"),
            "id,email,level\n1,a@b.example,info\n2,c@d.example,warn\n3,e@f.example,info\n",
        )
        .unwrap();

        let plan = plan_for(root);
        let mut slugs: Vec<&str> = plan.datasets.iter().map(|d| d.slug.as_str()).collect();
        slugs.sort();
        assert_eq!(
            slugs,
            ["memcached-data", "memcached-docs", "valkey-docs"],
            "{:#?}",
            plan.datasets
        );

        // every docs dataset carries the vector arm on body
        for d in plan.datasets.iter().filter(|d| d.family == "docs") {
            let body = d.specs.iter().find(|s| s.name == "body").unwrap();
            assert_eq!(body.es_type, "semantic_text", "{}: {:#?}", d.slug, d.specs);
            assert_eq!(d.semantic_field.as_deref(), Some("body"), "{}", d.slug);
        }

        // the one-off configs were demoted: document-rendered, and their
        // config keys never reached the docs mapping
        let by_rel: HashMap<&str, &FileAssignment> =
            plan.files.values().map(|f| (f.rel.as_str(), f)).collect();
        assert!(by_rel["valkey/commands/get.json"].as_document);
        assert!(by_rel["valkey/commands/set.json"].as_document);
        assert!(!by_rel["valkey/src/server.c"].as_document);
        assert!(!by_rel["memcached/data/events.csv"].as_document);
        let vdocs = plan
            .datasets
            .iter()
            .find(|d| d.slug == "valkey-docs")
            .unwrap();
        assert!(
            !vdocs.specs.iter().any(|s| s.name.contains("summary")),
            "config keys leaked into the docs mapping: {:#?}",
            vdocs.specs
        );
        assert_eq!(vdocs.file_count, 4, "code + prose + 2 demoted configs");

        // the CSV kept its real schema
        let data = plan
            .datasets
            .iter()
            .find(|d| d.slug == "memcached-data")
            .unwrap();
        assert!(data.specs.iter().any(|s| s.name == "email"), "{data:#?}");
    }

    /// #196: the same tree WITHOUT nested `.git` markers (or with one at the
    /// root — a workspace) is ONE scope: a single document corpus.
    #[test]
    fn a_single_workspace_plans_one_document_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("wasm/examples")).unwrap();
        std::fs::write(root.join("src/a.c"), CODE).unwrap();
        std::fs::write(root.join("src/b.c"), CODE).unwrap();
        std::fs::write(root.join("README.md"), PROSE).unwrap();
        std::fs::write(root.join("wasm/examples/demo.md"), PROSE).unwrap();

        let plan = plan_for(root);
        assert_eq!(
            plan.datasets.len(),
            1,
            "one workspace, one corpus: {:#?}",
            plan.datasets
        );
        assert_eq!(plan.datasets[0].slug, "docs");
        assert_eq!(plan.datasets[0].file_count, 4);
    }
}

// ─── Phase A plan building (pure: no server contact) ─────────────────────

/// Repository scope per file: the deepest ancestor directory (root-relative,
/// "/"-separated) containing a `.git` entry, or "" when the file is under
/// none. The walk never descends into `.git` itself, but the marker is still
/// on disk. A `.git` at the autoindex root yields "" for every file — the
/// whole tree is one scope — and so does a tree with no `.git` at all, which
/// is exactly the #173/#196 property: one autoindex of one folder yields a
/// corpus searchable as one corpus, split only at nested repository roots.
fn compute_scopes(root: &Path, rels: &[String]) -> Vec<String> {
    let mut cache: HashMap<String, bool> = HashMap::new();
    rels.iter()
        .map(|rel| {
            let mut scope = String::new();
            let mut prefix = String::new();
            let mut segs = rel.split('/').peekable();
            while let Some(seg) = segs.next() {
                if segs.peek().is_none() {
                    break; // final segment is the file name
                }
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(seg);
                let repo = *cache
                    .entry(prefix.clone())
                    .or_insert_with(|| root.join(&prefix).join(".git").exists());
                if repo {
                    scope = prefix.clone();
                }
            }
            scope
        })
        .collect()
}

/// Sniff + sample every file, cluster into datasets, and assemble the plan.
/// Pure planning: reads the tree, never the server — which is what makes the
/// #173/#196 grouping behaviour testable end-to-end without a cluster.
fn build_phase_a(
    root: &Path,
    files: &[walk::FileEntry],
    keys: &[String],
    digests: &[String],
    duplicate_files: Vec<DuplicateFile>,
    cfg: &IndexCfg,
    pr: &Progress,
) -> (Plan, Vec<dataset::Cluster>) {
    use rayon::prelude::*;
    // Same pool as the digest phase: sniffing and sampling are the other half
    // of the CPU-bound phase `--workers` has to bound (#240 §2). Progress is
    // reported from inside that pool, so the straggler the ticker names is the
    // file a scan-pool thread is genuinely sitting on (#241).
    let scans: Vec<FileScan> = crate::pool::install(|| {
        files
            .par_iter()
            .map(|f| {
                let _in_flight = pr.file(&f.rel, f.size);
                scan_file(&f.path, f.size, cfg.sample, cfg.max_file_gb)
            })
            .collect()
    });

    let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
    let scopes = compute_scopes(root, &rels);
    // (family, gzip) per file, from the scan's sniff — reused for assignments
    // and demoted-file re-sampling instead of re-sniffing every file.
    let file_meta: Vec<Option<(Family, bool)>> = scans
        .iter()
        .map(|sc| sc.sniffed.as_ref().map(|s| (s.family, s.gzip)))
        .collect();
    let mut sketches = Vec::new();
    let mut junk_files = Vec::new();
    for (i, sc) in scans.into_iter().enumerate() {
        let family = sc
            .sniffed
            .as_ref()
            .map(|s| s.family)
            .unwrap_or(Family::Binary);
        if let Some((status, reason)) = sc.junk {
            junk_files.push(JunkFile {
                file_key: keys[i].clone(),
                rel: files[i].rel.clone(),
                format: format_str(sc.sniffed.as_ref()),
                status,
                reason,
                bytes: files[i].size,
            });
            continue;
        }
        for gs in sc.sketches {
            sketches.push(dataset::Sketch {
                file_idx: i,
                group: gs.group,
                family,
                fields: gs.fields,
                key_fields: gs.key_fields,
                records: gs.records,
            });
        }
    }
    let mut clusters = dataset::cluster(sketches, &rels, &scopes);

    // Demoted one-off config files (#173) were sampled as flattened records;
    // phase B will index them as documents. Re-sample them through the same
    // document renderer so the docs dataset's mapping and stats describe what
    // actually gets indexed.
    for c in clusters.iter_mut().filter(|c| !c.demoted.is_empty()) {
        let demoted = c.demoted.clone();
        let fields = &mut c.fields;
        let mut sampled_total = 0u64;
        for m in demoted {
            let gzip = file_meta[m].map(|(_, g)| g).unwrap_or(false);
            let mut sampled = 0u64;
            let mut sink = |rec: extract::RawRecord| -> bool {
                if (sampled as usize) < cfg.sample {
                    for (k, v) in &rec.fields {
                        fields.entry(k.clone()).or_default().add(v);
                    }
                }
                sampled += 1;
                (sampled as usize) < cfg.sample
            };
            // An unreadable file junks at phase B like any other; the plan
            // keeps its membership either way.
            let _ = extract::extract_as_document(&files[m].path, gzip, &mut sink);
            sampled_total += sampled;
        }
        c.records += sampled_total;
    }

    // per-file assignments
    let mut file_assignments: HashMap<String, FileAssignment> = HashMap::new();
    for (ci, c) in clusters.iter().enumerate() {
        let demoted: std::collections::HashSet<usize> = c.demoted.iter().copied().collect();
        for &m in &c.members {
            let key = &keys[m];
            let (family, gzip) = file_meta[m].unwrap_or((c.family, false));
            let fa = file_assignments
                .entry(key.clone())
                .or_insert_with(|| FileAssignment {
                    rel: files[m].rel.clone(),
                    path_id: files[m].rel_id.clone(),
                    family: family.as_str().to_string(),
                    gzip,
                    content_digest: Some(digests[m].clone()),
                    assignments: Vec::new(),
                    as_document: false,
                });
            fa.as_document |= demoted.contains(&m);
            fa.assignments
                .push((c.group.clone(), clusters[ci].slug.clone()));
        }
    }

    let mut datasets = Vec::new();
    for c in &clusters {
        let specs =
            infer::infer_fields_with_policy(&c.fields, c.records, cfg.no_semantic, c.is_docs);
        let time_field = infer::elect_time_field(&specs);
        let semantic_field = specs
            .iter()
            .find(|s| s.es_type == "semantic_text")
            .map(|s| s.name.clone());
        datasets.push(PlanDataset {
            slug: c.slug.clone(),
            index: format!("{}-{}", cfg.prefix, c.slug),
            family: if c.is_docs {
                "docs".to_string()
            } else {
                c.family.as_str().to_string()
            },
            group: c.group.clone(),
            specs,
            time_field,
            semantic_field,
            sampled_records: c.records,
            file_count: c.members.len(),
        });
    }
    let plan = Plan {
        datasets,
        files: file_assignments,
        junk_files,
        duplicate_files,
        alias_paths_indexed: true,
    };
    (plan, clusters)
}

// ─── mapping builder ─────────────────────────────────────────────────────

pub const PROVENANCE_FIELDS: &[&str] = &[
    "ax_path",
    "ax_paths",
    "ax_file",
    "ax_locator",
    "ax_dataset",
    "ax_run",
    "ax_format",
];

fn build_mapping(specs: &[infer::FieldSpec]) -> Value {
    let mut props = Map::new();
    for s in specs {
        let m = match s.es_type.as_str() {
            "date" => json!({"type": "date", "format": "strict_date_optional_time||epoch_millis"}),
            t => json!({"type": t}),
        };
        props.insert(s.name.clone(), m);
    }
    for p in PROVENANCE_FIELDS {
        props.insert((*p).into(), json!({"type": "keyword"}));
    }
    json!({"mappings": {"properties": props}})
}

// ─── the main run ────────────────────────────────────────────────────────

fn select_resume_plan_keys(
    files: &[walk::FileEntry],
    content_keys: &[String],
    plan: &Plan,
    journal_path: &Path,
) -> Result<Vec<Option<String>>> {
    let mut planned_by_rel: HashMap<&str, &str> = HashMap::new();
    let mut planned_by_path_id: HashMap<&str, &str> = HashMap::new();
    for (key, assignment) in &plan.files {
        if let Some(previous) = planned_by_rel.insert(&assignment.rel, key) {
            anyhow::bail!(
                "resume plan assigns path {} to both {} and {}; use --fresh after verifying the \
                 existing index",
                assignment.rel,
                previous,
                key
            );
        }
        if !assignment.path_id.is_empty() {
            if let Some(previous) = planned_by_path_id.insert(&assignment.path_id, key) {
                anyhow::bail!(
                    "resume plan assigns one native path identity to both {} and {}; use --fresh \
                     after verifying the existing index",
                    previous,
                    key
                );
            }
        }
    }
    let current_rels: std::collections::HashSet<&str> =
        files.iter().map(|file| file.rel.as_str()).collect();
    let current_path_ids: std::collections::HashSet<&str> =
        files.iter().map(|file| file.rel_id.as_str()).collect();
    let mut claimed = std::collections::HashSet::new();
    let mut selected = Vec::with_capacity(files.len());
    for (file, content_key) in files.iter().zip(content_keys) {
        let exact_path = planned_by_path_id
            .get(file.rel_id.as_str())
            .or_else(|| planned_by_rel.get(file.rel.as_str()))
            .filter(|key| !claimed.contains(**key))
            .map(|key| (*key).to_string());
        let exact_content = plan
            .files
            .contains_key(content_key)
            .then(|| content_key.clone())
            .filter(|key| !claimed.contains(key.as_str()));
        let key = if let Some(key) = exact_path.or(exact_content) {
            Some(key)
        } else {
            // Computing the legacy prefix key is intentionally the final
            // fallback. Normal resumes are O(files), with no 64 KiB read.
            let legacy_key = ids::file_key(&file.path, file.size)?;
            if let Some(assignment) = plan.files.get(&legacy_key) {
                let has_exact_current_owner = current_rels.contains(assignment.rel.as_str())
                    || (!assignment.path_id.is_empty()
                        && current_path_ids.contains(assignment.path_id.as_str()));
                if claimed.contains(legacy_key.as_str()) || has_exact_current_owner {
                    anyhow::bail!(
                        "{} collides with legacy resume key {} already owned by {}. No documents \
                         were changed; remove or move one of these two files out of the corpus \
                         and rerun — every other file keeps its resume state. Deleting the \
                         journal at {} (or rerunning with --fresh) also clears the collision, \
                         but re-extracts and re-embeds the entire corpus",
                        file.rel,
                        legacy_key,
                        assignment.rel,
                        journal_path.display()
                    );
                }
                Some(legacy_key)
            } else if claimed.contains(content_key.as_str()) {
                // Another current file already owns this planned key. Ownership
                // must stay exclusive — two owners would each run the
                // replacement transaction on one ax_file key and delete each
                // other's freshly published documents. Divert this file to a
                // deterministic path-derived key, the same discriminator scheme
                // content::resolve_reporting uses for byte-proven digest
                // collisions.
                Some(format!(
                    "{content_key}-claimed-{:032x}",
                    xxhash_rust::xxh3::xxh3_128(file.rel_id.as_bytes())
                ))
            } else {
                None
            }
        };
        if let Some(key) = &key {
            claimed.insert(key.clone());
        }
        selected.push(key);
    }
    Ok(selected)
}

fn alias_keys_to_reindex(
    previous: &[state::DuplicateFile],
    current: &[state::DuplicateFile],
    migration_keys: Option<&[String]>,
) -> std::collections::HashSet<String> {
    let paths_by_key = |aliases: &[state::DuplicateFile]| {
        let mut by_key: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
        for alias in aliases {
            by_key
                .entry(alias.file_key.clone())
                .or_default()
                .insert(alias.rel.clone());
        }
        by_key
    };
    let previous = paths_by_key(previous);
    let current = paths_by_key(current);
    let mut changed = std::collections::HashSet::new();
    if let Some(keys) = migration_keys {
        changed.extend(keys.iter().cloned());
    }
    for key in previous.keys().chain(current.keys()) {
        if previous.get(key) != current.get(key) {
            changed.insert(key.clone());
        }
    }
    changed
}

/// Default second-brain name for a corpus root: `sanitize_slug(basename)`
/// (SECOND_BRAIN_SPEC §6.1), falling back to `"brain"` when the basename
/// sanitizes to nothing (e.g. `/`). Public because `xerj brain` must know
/// the SAME name this pipeline will use — the console URL it prints and
/// opens embeds it — and two copies of this rule would drift.
pub fn derive_brain_name(root: &Path) -> String {
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let name = base
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = dataset::sanitize_slug(&name);
    if slug.is_empty() {
        "brain".into()
    } else {
        slug
    }
}

fn run_index(cfg: IndexCfg) -> Result<i32> {
    run_index_report(cfg).map(|(code, _)| code)
}

/// `run_index` plus the machine-readable run summary — the same JSON the
/// run writes to the catalog as `run:{run_id}` (datasets, `records_total`
/// as *live* per-dataset counts, `graph.edges_written` etc.). `xerj brain`
/// composes autoindex through this so it can be honest about what actually
/// got indexed without re-querying or parsing stdout. The summary is
/// `None` when the run ended before a plan produced one (empty folder,
/// `--dry-run`).
pub fn run_index_report(cfg: IndexCfg) -> Result<(i32, Option<Value>)> {
    // Fix the phase-A pool width BEFORE anything parallel starts: hashing and
    // sniffing are the CPU-bound phase, and they used to take every core no
    // matter what the caller asked for (#240 §2).
    pool::configure(cfg.scan_workers);
    // What phase A is *actually* running on. `pool::configure` is first-call-wins
    // (rayon pools cannot be resized), so in a process that already indexed once
    // — `xerj-server`'s brain endpoint does exactly that — the installed width
    // can differ from what this run's plan asked for. Progress must state the
    // number the policy got, not the one it requested (#240 + #241).
    let scan_threads = pool::scan_pool().current_num_threads();
    extract::pdf::configure_workers(cfg.pdf_workers);
    extract::pdf::configure_timeout(cfg.pdf_timeout_secs);
    let t0 = Instant::now();
    // The progress surface and its ticker are the FIRST things built: every
    // later phase reports through them, and the ticker guarantees the stream
    // closes with a terminal line even if this function bails (#241). The
    // resource plan is announced through the same surface rather than through a
    // bare `eprintln!`, so `--quiet` / `--progress none` still means silent and
    // `--progress json` still means one machine-readable stream (#240 + #241).
    let surface = progress::detect(cfg.progress);
    let pr = Progress::new(
        surface,
        cfg.progress_interval
            .unwrap_or_else(|| progress::default_interval(surface)),
    );
    let ticker = pr.spawn_ticker();
    // What this run decided to take from the machine, before it takes it.
    pr.note(&format!(
        "autoindex: {} scan threads, {} index workers, {} pdf workers, --bulk-mb {} [{}]",
        scan_threads,
        cfg.workers,
        cfg.pdf_workers,
        cfg.bulk_mb,
        xerj_common::resource::describe(),
    ));
    for note in &cfg.resource_notes {
        pr.note(&format!("autoindex: {note}"));
    }
    pr.note(&format!(
        "autoindex: bulk HTTP request timeout: {}s",
        cfg.bulk_timeout_secs
    ));
    // The run's bulk load is admitted through one window `--workers` wide, so
    // a 429 can shrink what the run offers instead of only delaying it
    // (#240 §8). Enabled here and nowhere else: probes have nothing to
    // throttle. Its shrink/recover announcements go to stderr, which the
    // progress surface owns — so they are emitted exactly when that surface is
    // enabled, not merely when `--quiet` is absent.
    let es = Es::with_bulk_timeout(&cfg.url, cfg.api_key.clone(), cfg.bulk_timeout_secs)?
        .with_bulk_concurrency(cfg.workers, pr.enabled());
    es.ping()?;

    // Totals are unknown until the walk returns, so this phase honestly
    // reports `pct=unknown` and proves liveness with the clock alone.
    pr.phase("walk", 0, 0);
    let discovered_files = walk::walk(&cfg.root, cfg.follow_symlinks)?;
    if discovered_files.is_empty() {
        println!("no files found under {}", cfg.root.display());
        pr.finish(true, 0, "no-files", &[]);
        return Ok((0, None));
    }
    let discovered_bytes: u64 = discovered_files.iter().map(|f| f.size).sum();
    let root_str = cfg
        .root
        .canonicalize()
        .unwrap_or_else(|_| cfg.root.clone())
        .to_string_lossy()
        .to_string();
    pr.note(&format!(
        "autoindex: {} files ({} MB) under {}",
        discovered_files.len(),
        discovered_bytes / (1 << 20),
        root_str
    ));

    let state_dir = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| state::default_state_dir(&root_str, &cfg.url, &cfg.prefix));
    let mut journal = state::Journal::open(
        &state_dir,
        &root_str,
        &cfg.url,
        &cfg.prefix,
        cfg.bulk_timeout_secs,
        cfg.fresh,
    )?;
    let resumed_with_plan = journal.plan.is_some();
    let run_id = journal.run_id.clone();
    if journal.resumed {
        pr.note(&format!(
            "resuming from journal {} ({} files already done)",
            journal.path().display(),
            journal.done.len()
        ));
    }
    // Full hashing on every run is deliberate: size/mtime/inode fingerprints
    // cannot prove byte identity across all supported local and network
    // filesystems. A metadata-only shortcut could leave stale live documents
    // forever after a same-size rewrite with restored or stale timestamps.
    // Hashing reads every byte of the corpus. On a large tree it is minutes of
    // real work, and before #241 it was minutes with no output at all.
    pr.phase("hash", discovered_files.len() as u64, discovered_bytes);
    let mut inventory = content::resolve_reporting(discovered_files, &|bytes| pr.item_done(bytes))?;
    let journal_path = journal.path().to_path_buf();
    let mut content_changed = std::collections::HashSet::new();
    let mut stale_alias_ids = Vec::new();
    let mut alias_paths_to_replace = std::collections::HashSet::new();
    let mut plan_changed = journal.plan.is_none();
    // Preserve legacy document IDs while upgrading old plans with full digests.
    // A later same-size/tail mutation is then detected and reindexed.
    if let Some(plan) = &mut journal.plan {
        let needs_alias_path_migration = !plan.alias_paths_indexed;
        let previous_aliases = plan.duplicate_files.clone();
        alias_paths_to_replace.extend(previous_aliases.iter().map(|alias| alias.rel.clone()));
        stale_alias_ids.extend(
            plan.duplicate_files
                .iter()
                .map(|old| catalog::duplicate_file_id(&old.file_key, &old.rel, &old.path_id)),
        );
        let selected_plan_keys =
            select_resume_plan_keys(&inventory.files, &inventory.keys, plan, &journal_path)?;
        for (index, planned_key) in selected_plan_keys.into_iter().enumerate() {
            let file = &inventory.files[index];
            if let Some(planned_key) = planned_key {
                if !plan.files.contains_key(&planned_key) {
                    // The file was diverted off a planned key exclusively
                    // owned by another current file. Record the divergence in
                    // the durable plan so every resume deterministically skips
                    // this path instead of racing one ax_file key.
                    if !plan
                        .junk_files
                        .iter()
                        .any(|junk| junk.file_key == planned_key)
                    {
                        let owner = plan
                            .files
                            .get(&inventory.keys[index])
                            .map(|assignment| assignment.rel.as_str())
                            .unwrap_or("another file");
                        plan.junk_files.push(JunkFile {
                            file_key: planned_key.clone(),
                            rel: file.rel.clone(),
                            format: "unknown".into(),
                            status: "skipped".into(),
                            reason: format!(
                                "content resolves to planned key {} owned by {owner}; skipped to \
                                 keep key ownership exclusive (remove one of the two files and \
                                 rerun to index the survivor)",
                                inventory.keys[index]
                            ),
                            bytes: file.size,
                        });
                        plan_changed = true;
                    }
                    inventory.keys[index] = planned_key;
                    continue;
                }
                let assignment = plan.files.get_mut(&planned_key).expect("planned key");
                if assignment.rel != file.rel {
                    content_changed.insert(planned_key.clone());
                    plan_changed = true;
                }
                if assignment
                    .content_digest
                    .as_deref()
                    .is_some_and(|digest| digest != inventory.digests[index])
                {
                    content_changed.insert(planned_key.clone());
                    plan_changed = true;
                }
                if assignment.path_id != file.rel_id
                    || assignment.content_digest.as_deref()
                        != Some(inventory.digests[index].as_str())
                {
                    plan_changed = true;
                }
                assignment.rel = file.rel.clone();
                assignment.path_id = file.rel_id.clone();
                assignment.content_digest = Some(inventory.digests[index].clone());
                inventory.keys[index] = planned_key;
            }
        }
        let key_by_path: HashMap<&str, &str> = inventory
            .files
            .iter()
            .zip(inventory.keys.iter())
            .map(|(file, key)| (file.rel.as_str(), key.as_str()))
            .collect();
        for duplicate in &mut inventory.duplicates {
            if let Some(key) = key_by_path.get(duplicate.duplicate_of.as_str()) {
                duplicate.file_key = (*key).to_string();
            }
        }
        let current_alias_ids: std::collections::HashSet<String> = inventory
            .duplicates
            .iter()
            .map(|alias| catalog::duplicate_file_id(&alias.file_key, &alias.rel, &alias.path_id))
            .collect();
        stale_alias_ids.retain(|id| !current_alias_ids.contains(id));
        // The historical global flag cannot identify which live documents
        // already carry ax_paths. Its one-time migration must rewrite every
        // canonical key; ordinary alias changes remain scoped per key.
        // A key whose entire duplicate group was deleted has no current file
        // to republish; scheduling it would strand a pending replacement that
        // every later run re-journals without ever committing.
        let current_keys: std::collections::HashSet<&str> =
            inventory.keys.iter().map(String::as_str).collect();
        content_changed.extend(
            alias_keys_to_reindex(
                &previous_aliases,
                &inventory.duplicates,
                needs_alias_path_migration.then_some(inventory.keys.as_slice()),
            )
            .into_iter()
            .filter(|key| current_keys.contains(key.as_str())),
        );
        if needs_alias_path_migration {
            plan.alias_paths_indexed = true;
            plan_changed = true;
        }
        if previous_aliases != inventory.duplicates {
            plan_changed = true;
        }
        plan.duplicate_files = inventory.duplicates.clone();
    }
    alias_paths_to_replace.extend(inventory.duplicates.iter().map(|alias| alias.rel.clone()));
    let files = inventory.files;
    let keys = inventory.keys;
    let digests = inventory.digests;
    let duplicate_files = inventory.duplicates;
    let paths_discovered = files.len() + duplicate_files.len();
    let planned_bytes: u64 = files.iter().map(|f| f.size).sum();
    if !duplicate_files.is_empty() {
        pr.note(&format!(
            "autoindex: {} byte-identical duplicate path(s) will reuse canonical content",
            duplicate_files.len()
        ));
        for duplicate in duplicate_files.iter().take(10) {
            pr.note(&format!(
                "  duplicate: {} → {}",
                duplicate.rel, duplicate.duplicate_of
            ));
        }
        if duplicate_files.len() > 10 {
            pr.note(&format!("  … and {} more", duplicate_files.len() - 10));
        }
    }

    // ── Phase A: inference (skipped when a frozen plan exists) ──────────
    let mut clusters_rt: Option<Vec<dataset::Cluster>> = None;
    let mut plan: Plan = if let Some(p) = journal.plan.clone() {
        p
    } else {
        pr.phase("scan", files.len() as u64, planned_bytes);
        // Named with the width phase A is really running at: `--workers` only
        // started governing this phase in #240, so "how wide is it right now"
        // is exactly the question the reader has.
        pr.note(&format!(
            "phase A: sniffing + sampling {} files with {scan_threads} threads…",
            files.len()
        ));
        let (plan, clusters) = build_phase_a(
            &cfg.root,
            &files,
            &keys,
            &digests,
            duplicate_files.clone(),
            &cfg,
            &pr,
        );
        pr.note(&format!(
            "phase A: {} datasets inferred, {} junk/skipped files",
            plan.datasets.len(),
            plan.junk_files.len()
        ));
        clusters_rt = Some(clusters);
        plan
    };

    // ── stale junk-catalog sweep (#238) ──────────────────────────────────
    //
    // A junk/skipped file is never indexed and never enters the graph corpus
    // — both walk `plan.files`, which by construction does not contain it. Its
    // ENTIRE live footprint is one `file:{key}` catalog document, so removing
    // that document is a complete removal, unlike an indexed file where
    // dropping the catalog entry while its records stay live would be a lie.
    //
    // The durable plan is the only thing that remembers the document exists:
    // nothing else in a run remembers a file it deliberately did not read. So
    // the sweep is driven from the plan, and the plan entry may be dropped
    // only after the delete has actually landed — the same order quickwit's
    // GC keeps, deleting split files first and removing metastore records
    // only for the deletes that succeeded (quickwit,
    // quickwit/quickwit-index-management/src/garbage_collection.rs:484-534,
    // Apache-2.0; approach only, no code taken). Dropping the record first
    // would strand the document exactly as before.
    //
    // A key that is somehow BOTH planned and junk is left alone: deleting
    // `file:{key}` would take out a live indexed file's catalog entry, and an
    // immortal junk row is the cheaper of those two failures.
    let live_keys: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
    let stale_junk_keys: std::collections::HashSet<String> = plan
        .junk_files
        .iter()
        .filter(|junk| {
            !live_keys.contains(junk.file_key.as_str()) && !plan.files.contains_key(&junk.file_key)
        })
        .map(|junk| junk.file_key.clone())
        .collect();

    if cfg.dry_run {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        pr.note("(dry run — nothing indexed)");
        pr.finish(true, 0, "dry-run", &[("files", files.len() as u64)]);
        return Ok((0, None));
    }

    if plan
        .datasets
        .iter()
        .any(|dataset| dataset.semantic_field.is_some())
    {
        let identity = es
            .embedding_execution_identity()
            .context("semantic autoindex could not pin the server embedding execution identity")?;
        journal.pin_embedding_identity(
            &identity.identity_sha256,
            identity.resumable,
            identity.non_resumable_reason.as_deref(),
        )?;
    }

    // ── create indices with explicit mappings ────────────────────────────
    // Two round trips per dataset; a 135-dataset plan is a real wait.
    pr.phase("prepare", plan.datasets.len() as u64, 0);
    for d in &plan.datasets {
        es.ensure_index(&d.index, &build_mapping(&d.specs))
            .with_context(|| format!("create index {}", d.index))?;
        es.update_mapping(
            &d.index,
            &json!({"properties": {"ax_paths": {"type": "keyword"}}}),
        )
        .with_context(|| format!("upgrade alias-path mapping for {}", d.index))?;
        pr.item_done(0);
    }
    es.ensure_index(catalog::CATALOG_INDEX, &catalog::catalog_mapping())?;
    es.update_mapping(
        catalog::CATALOG_INDEX,
        &json!({"properties": {"duplicate_of": {"type": "keyword"}}}),
    )
    .context("upgrade autoindex catalog mapping for duplicate aliases")?;
    // A replacement transaction starts before the effective new plan is
    // persisted and before live visibility changes. If the process dies at
    // any later boundary, journal replay removes the older file_done and
    // deterministically schedules a delete-before-replace repair.
    let generation_by_key: HashMap<&str, &str> = keys
        .iter()
        .zip(digests.iter())
        .map(|(key, digest)| (key.as_str(), digest.as_str()))
        .collect();
    // Snapshot whether live records may already exist before this run starts
    // any new publication intents. Fresh first publications can skip the
    // delete/refresh round trip; replacements and crash repairs cannot.
    let mut cleanup_required: std::collections::HashSet<String> = journal
        .done
        .keys()
        .chain(journal.pending_replacements.keys())
        .cloned()
        .collect();
    let mut replacements: Vec<&String> = content_changed
        .iter()
        .filter(|key| {
            let desired = generation_by_key.get(key.as_str()).copied();
            journal.done.contains_key(key.as_str())
                || journal
                    .pending_replacements
                    .get(key.as_str())
                    .is_some_and(|pending| Some(pending.as_str()) != desired)
        })
        .collect();
    replacements.sort();
    for key in replacements {
        journal.file_replace_start(
            key,
            generation_by_key
                .get(key.as_str())
                .copied()
                .unwrap_or("unknown"),
        )?;
    }
    // Persist only an effective plan change. Repeating the full plan on every
    // no-op resume caused journal growth proportional to plan_size × runs.
    if plan_changed {
        journal.write_plan(&plan)?;
    }
    replacement_failpoint(1).context("after durable replacement plan")?;

    // ── Phase B: full-stream extraction + bulk indexing ─────────────────
    struct DsRt {
        index: String,
        plan: HashMap<String, coerce::Coerce>,
        records: AtomicU64,
        junk: AtomicU64,
        dropped: AtomicU64,
        bytes: AtomicU64,
    }
    let mut ds_rt: HashMap<String, DsRt> = HashMap::new();
    for d in &plan.datasets {
        ds_rt.insert(
            d.slug.clone(),
            DsRt {
                index: d.index.clone(),
                plan: coerce::plan_from_specs(&d.specs),
                records: AtomicU64::new(0),
                junk: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                bytes: AtomicU64::new(0),
            },
        );
    }

    let done0 = journal.done_keys();
    let planned_junk: std::collections::HashSet<&str> = plan
        .junk_files
        .iter()
        .map(|j| j.file_key.as_str())
        .collect();
    let mut new_unplanned: Vec<JunkFile> = Vec::new();
    let mut todo: Vec<usize> = Vec::new();
    for i in 0..files.len() {
        if keys[i].is_empty() || done0.contains(&keys[i]) && !content_changed.contains(&keys[i]) {
            continue;
        }
        if plan.files.contains_key(&keys[i]) {
            todo.push(i);
        } else if !planned_junk.contains(keys[i].as_str()) {
            // file appeared after the plan was frozen — recorded, not fatal
            new_unplanned.push(JunkFile {
                file_key: keys[i].clone(),
                rel: files[i].rel.clone(),
                format: "unknown".into(),
                status: "skipped".into(),
                reason: "not in the frozen resume plan (re-run with --fresh to include new files)"
                    .into(),
                bytes: files[i].size,
            });
        }
    }
    if resumed_with_plan {
        // Legacy journals predate intent-before-publication and may have live
        // partial records without either file_done or file_replace_start.
        // Conservatively clean every resumed planned todo once. Only a plan
        // created in this process can prove that its first publication is
        // genuinely fresh and skip the delete round trip.
        cleanup_required.extend(todo.iter().map(|&i| keys[i].clone()));
    }
    // Every publication, including a fresh one, receives durable intent
    // before its first bulk. A failed fresh publication therefore skips the
    // unnecessary delete now but is recognized as pending and cleaned on the
    // next run.
    let mut intent_keys: Vec<&str> = todo.iter().map(|&i| keys[i].as_str()).collect();
    intent_keys.sort_unstable();
    intent_keys.dedup();
    for key in intent_keys {
        let generation = generation_by_key.get(key).copied().unwrap_or("unknown");
        if journal
            .pending_replacements
            .get(key)
            .is_none_or(|pending| pending != generation)
        {
            journal.file_replace_start(key, generation)?;
        }
    }
    // ── second-brain graph: corpus table, structural edges, invalidation ──
    // (SECOND_BRAIN_SPEC §6.6.1/§6.6.3.) Runs after plan finalization so the
    // detectors see the whole corpus, and BEFORE any Phase B publication so
    // replacement invalidation can only ever see prior-generation edges —
    // running it later would invalidate this run's own fresh edges.
    let bulk_cut = cfg.bulk_mb << 20;
    let junk_records = AtomicU64::new(0);
    let bulk_errors = Mutex::new(Vec::<String>::new());
    let graph: Option<GraphRt> = if cfg.no_graph {
        None
    } else {
        pr.phase("graph", files.len() as u64, 0);
        let brain = match &cfg.brain {
            Some(b) => b.clone(),
            None => derive_brain_name(&cfg.root),
        };
        if let Err(reason) = detect::validate_brain(&brain) {
            anyhow::bail!(
                "brain name '{brain}' is invalid: {reason}. Pass an explicit --brain <name> \
                 or disable relationship detection with --no-graph"
            );
        }
        // Corpus resolution table: every planned file's rel → identity +
        // anchor node. valid_at comes from the file mtime (§6.4) so an
        // unchanged corpus re-emits byte-identical edge_ids and re-runs
        // converge by overwrite, exactly like ids::doc_id does for nodes.
        let mut corpus_files = Vec::new();
        for (i, f) in files.iter().enumerate() {
            let key = &keys[i];
            if key.is_empty() {
                continue;
            }
            let Some(fa) = plan.files.get(key) else {
                continue; // junk or post-freeze files carry no node docs
            };
            let slug = fa
                .assignments
                .iter()
                .find(|(g, _)| g.is_none())
                .map(|(_, s)| s.clone())
                .or_else(|| fa.assignments.iter().map(|(_, s)| s.clone()).min());
            let Some(slug) = slug else { continue };
            let mtime_ms = std::fs::metadata(&f.path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            corpus_files.push(detect::corpus_file(
                &f.rel, key, &slug, &fa.family, mtime_ms,
            ));
            pr.item_done(0);
        }
        let corpus = detect::CorpusIndex::build(corpus_files);
        let detectors = detect::default_detectors();
        let edges_index = detect::edges_index_name(&brain);
        let created_at_ms = chrono::Utc::now().timestamp_millis();

        es.ensure_index(&edges_index, &detect::edge_index_mapping())
            .with_context(|| format!("create edges index {edges_index}"))?;
        let mut nodes_indices: Vec<&str> = plan.datasets.iter().map(|d| d.index.as_str()).collect();
        nodes_indices.sort_unstable();
        nodes_indices.dedup();
        detect::ensure_brain_meta(
            &es,
            &edges_index,
            &brain,
            &nodes_indices.join(","),
            created_at_ms,
        )?;

        // Replacement invalidation FIRST: soft-invalidate every live edge a
        // replaced file taught in earlier runs. The bi-temporal record stays
        // queryable (`as_of` time travel); nothing is deleted.
        let mut invalidated = 0u64;
        {
            let mut replaced_rels: Vec<&str> = todo
                .iter()
                .filter(|&&i| cleanup_required.contains(&keys[i]))
                .map(|&i| files[i].rel.as_str())
                .collect();
            replaced_rels.sort_unstable();
            replaced_rels.dedup();
            for rel in replaced_rels {
                invalidated +=
                    detect::invalidate_prior_edges(&es, &edges_index, rel, created_at_ms)
                        .with_context(|| format!("invalidate prior edges taught by {rel}"))?;
            }
        }

        // Structural detection (samedir chains) + bulk write, cut at the same
        // --bulk-mb threshold as node bulks.
        let mut structural = Vec::new();
        for det in &detectors {
            det.detect_structure(&corpus, &mut structural);
        }
        let assembled = detect::assemble(&structural, &edges_index, created_at_ms);
        let mut written: std::collections::BTreeMap<&'static str, u64> =
            std::collections::BTreeMap::new();
        {
            let mut send_err: Option<String> = None;
            let mut buf: Vec<u8> = Vec::new();
            for edge in &assembled.edges {
                buf.extend_from_slice(&edge.ndjson);
                *written.entry(edge.detector).or_default() += 1;
                if buf.len() >= bulk_cut
                    && record_bulk_outcome(
                        &es,
                        std::mem::take(&mut buf),
                        &junk_records,
                        &bulk_errors,
                        &mut send_err,
                    )
                {
                    break;
                }
            }
            if send_err.is_none() && !buf.is_empty() {
                record_bulk_outcome(&es, buf, &junk_records, &bulk_errors, &mut send_err);
            }
            if let Some(e) = send_err {
                anyhow::bail!("write structural graph edges to {edges_index}: {e}");
            }
        }
        pr.note(&format!(
            "graph: brain '{brain}' → {edges_index}; {} structural edges, {} prior edges \
             invalidated ({} detectors live)",
            assembled.edges.len(),
            invalidated,
            detectors.len()
        ));
        Some(GraphRt {
            corpus,
            detectors,
            href_raw: detect::href::Href::default(),
            edges_index,
            brain,
            created_at_ms,
            written: Mutex::new(written),
            self_dropped: AtomicU64::new(assembled.self_dropped),
            invalidated,
        })
    };

    // ascending by size — workers pop() from the tail, so the BIGGEST files
    // start first and can't serialize the end of the run.
    todo.sort_by_key(|&i| files[i].size);
    let n_todo = todo.len();
    // Percent and ETA are bytes-based, never file-count-based. The queue is
    // biggest-first, so a files-done percent races to ~100% and then sits
    // there for minutes on the one big file still in flight (#241 §5/§6).
    let todo_bytes: u64 = todo.iter().map(|&i| files[i].size).sum();
    pr.phase("index", n_todo as u64, todo_bytes);
    pr.note(&format!(
        "phase B: indexing {} files with {} workers → {}",
        n_todo, cfg.workers, cfg.url
    ));

    let queue = Mutex::new(todo);
    let mut paths_by_key: HashMap<String, Vec<String>> = files
        .iter()
        .zip(keys.iter())
        .map(|(file, key)| (key.clone(), vec![file.rel.clone()]))
        .collect();
    for duplicate in &plan.duplicate_files {
        paths_by_key
            .entry(duplicate.file_key.clone())
            .or_default()
            .push(duplicate.rel.clone());
    }
    for paths in paths_by_key.values_mut() {
        paths.sort();
        paths.dedup();
    }
    let journal_mx = Mutex::new(&mut journal);
    let files_done = AtomicU64::new(0);
    let records_total = AtomicU64::new(0);
    let extra_junk = Mutex::new(Vec::<JunkFile>::new());

    std::thread::scope(|scope| {
        for _ in 0..cfg.workers.min(n_todo.max(1)) {
            scope.spawn(|| {
                loop {
                    let i = match queue.lock().unwrap().pop() {
                        Some(i) => i,
                        None => break,
                    };
                    let f = &files[i];
                    // Counts this file done on EVERY exit path below, junk
                    // included: progress measures work drained from the queue,
                    // and a `continue` that skipped the count would park the
                    // bar short of 100% forever.
                    let _in_flight = pr.file(&f.rel, f.size);
                    let key = &keys[i];
                    let expected_digest = &digests[i];
                    let fa = plan.files.get(key).unwrap();
                    let asg: HashMap<Option<String>, String> =
                        fa.assignments.iter().cloned().collect();
                    let sn = match sniff::sniff(&f.path) {
                        Ok(s) => s,
                        Err(e) => {
                            extra_junk.lock().unwrap().push(JunkFile {
                                file_key: key.clone(),
                                rel: f.rel.clone(),
                                format: "unknown".into(),
                                status: "junk".into(),
                                reason: format!("unreadable at index time: {e}"),
                                bytes: f.size,
                            });
                            continue;
                        }
                    };
                    let mut file_records = 0u64;
                    let mut file_junk = 0u64;
                    let mut send_err: Option<String> = None;
                    // Edges this file teaches — buffered apart from the node
                    // staging file (different target index) and sent only
                    // after the node bulks are accepted (§6.7).
                    let mut edge_drafts: Vec<detect::EdgeDraft> = Vec::new();
                    // (doc id, label) of the last staged text section — the
                    // sequence detector's predecessor. Stream order is the
                    // only source that can name a PDF page boundary's
                    // predecessor (p2-s0 follows the LAST section of page 1).
                    let mut prev_section: Option<(String, String)> = None;
                    let mut staged = match tempfile::Builder::new()
                        .prefix(".autoindex-stage-")
                        .tempfile_in(&state_dir)
                    {
                        Ok(file) => file,
                        Err(error) => {
                            let mut errors = bulk_errors.lock().unwrap();
                            if errors.len() < 5 {
                                errors.push(format!(
                                    "create per-file staging area for {}: {error}",
                                    f.rel
                                ));
                            }
                            continue;
                        }
                    };
                    if let Err(error) = content::verify(&f.path, f.size, expected_digest) {
                        let mut errors = bulk_errors.lock().unwrap();
                        if errors.len() < 5 {
                            errors.push(format!("{error:#}"));
                        }
                        continue;
                    }
                    // File-card anchor node (§6.6.2a): one card doc per corpus
                    // file, staged BEFORE the file's records. Its
                    // deterministic id is `CorpusFile.anchor_doc_id` — the
                    // node every file-level edge (wikilink/mdlink/href/
                    // pathcite/cratecite/samedir dst, sequence opener src)
                    // terminates at. Row/line/page families have no `s0`
                    // section doc, so without the card those edges pointed at
                    // ghosts. Not counted as an extracted record: it is
                    // derived anchor infrastructure, not file content.
                    if let Some(gr) = graph.as_ref() {
                        if let Some(cf) = gr.corpus.files.get(&f.rel) {
                            if let Some(rt) = ds_rt.get(&cf.dataset_slug) {
                                let name = f.rel.rsplit('/').next().unwrap_or(&f.rel);
                                let mut fields = Map::new();
                                fields.insert("title".into(), Value::String(name.to_string()));
                                fields.insert("ax_path".into(), Value::String(f.rel.clone()));
                                fields.insert(
                                    "ax_paths".into(),
                                    Value::Array(
                                        paths_by_key
                                            .get(key)
                                            .into_iter()
                                            .flatten()
                                            .cloned()
                                            .map(Value::String)
                                            .collect(),
                                    ),
                                );
                                fields.insert("ax_file".into(), Value::String(key.clone()));
                                fields.insert(
                                    "ax_locator".into(),
                                    Value::String(detect::FILE_CARD_LOCATOR.into()),
                                );
                                fields.insert(
                                    "ax_dataset".into(),
                                    Value::String(cf.dataset_slug.clone()),
                                );
                                fields.insert("ax_run".into(), Value::String(run_id.clone()));
                                fields.insert(
                                    "ax_format".into(),
                                    Value::String(format_str(Some(&sn))),
                                );
                                let action = json!({"index": {
                                    "_index": rt.index, "_id": cf.anchor_doc_id}});
                                if let Err(error) = writeln!(
                                    staged.as_file_mut(),
                                    "{}\n{}",
                                    action,
                                    Value::Object(fields)
                                ) {
                                    send_err =
                                        Some(format!("stage file card for {}: {error}", f.rel));
                                }
                            }
                        }
                    }
                    {
                        let mut sink = |rec: extract::RawRecord| -> bool {
                            let Some(slug) = asg.get(&rec.group).or_else(|| asg.get(&None)) else {
                                file_junk += 1;
                                return true;
                            };
                            let Some(rt) = ds_rt.get(slug) else {
                                file_junk += 1;
                                return true;
                            };
                            let mut fields = rec.fields;
                            let dropped = coerce::coerce_record(&mut fields, &rt.plan);
                            if dropped > 0 {
                                rt.dropped.fetch_add(dropped as u64, Ordering::Relaxed);
                            }
                            fields.insert("ax_path".into(), Value::String(f.rel.clone()));
                            fields.insert(
                                "ax_paths".into(),
                                Value::Array(
                                    paths_by_key
                                        .get(key)
                                        .into_iter()
                                        .flatten()
                                        .cloned()
                                        .map(Value::String)
                                        .collect(),
                                ),
                            );
                            fields.insert("ax_file".into(), Value::String(key.clone()));
                            fields.insert("ax_locator".into(), Value::String(rec.locator.clone()));
                            fields.insert("ax_dataset".into(), Value::String(slug.clone()));
                            fields.insert("ax_run".into(), Value::String(run_id.clone()));
                            fields.insert("ax_format".into(), Value::String(format_str(Some(&sn))));
                            let id = ids::doc_id(slug, key, &rec.locator);
                            let action = json!({"index": {"_index": rt.index, "_id": id}});
                            let doc = Value::Object(fields);
                            if let Err(error) = writeln!(
                                staged.as_file_mut(),
                                "{}\n{}",
                                action,
                                serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into())
                            ) {
                                send_err =
                                    Some(format!("stage extracted records for {}: {error}", f.rel));
                                return false;
                            }
                            rt.records.fetch_add(1, Ordering::Relaxed);
                            file_records += 1;
                            // Textual edge detection (§6.6.2), after the node
                            // action is staged: `body` is the exact section
                            // string the node doc carries, and `id` is the
                            // section node the evidence lives in.
                            if let Some(gr) = graph.as_ref() {
                                if let Some(label) = section_label(&rec.locator) {
                                    if let (Some(cf), Some(body)) = (
                                        gr.corpus.files.get(&f.rel),
                                        doc.get("body").and_then(Value::as_str),
                                    ) {
                                        let ctx = detect::SectionCtx {
                                            corpus: &gr.corpus,
                                            file: cf,
                                            section_label: &label,
                                            prev_section: prev_section
                                                .as_ref()
                                                .map(|(pid, pl)| (pid.as_str(), pl.as_str())),
                                            section_doc_id: &id,
                                            text: body,
                                        };
                                        for det in &gr.detectors {
                                            det.detect_text(&ctx, &mut edge_drafts);
                                        }
                                        prev_section = Some((id.clone(), label));
                                    }
                                }
                            }
                            true
                        };
                        // Demoted one-off config files (#173) index as
                        // documents — their key sets are configuration, not a
                        // schema (see `dataset` module docs).
                        let res = if fa.as_document {
                            extract::extract_as_document(&f.path, sn.gzip, &mut sink)
                        } else {
                            extract::extract(&f.path, &sn, None, &mut sink)
                        };
                        match res {
                            Ok(stats) => {
                                file_junk += stats.junk;
                            }
                            Err(e) => {
                                send_err = Some(format!("extract {}: {e}", f.rel));
                                extra_junk.lock().unwrap().push(JunkFile {
                                    file_key: key.clone(),
                                    rel: f.rel.clone(),
                                    format: format_str(Some(&sn)),
                                    status: "junk".into(),
                                    reason: format!("extract failed at index time: {e}"),
                                    bytes: f.size,
                                });
                            }
                        }
                    }
                    // Raw-source href pass: the HTML extractor strips markup
                    // before sectioning, so `<a href>` evidence exists only in
                    // the raw bytes (detect::href module docs). The second
                    // content::verify below still covers this re-read.
                    if send_err.is_none() {
                        if let Some(gr) = graph.as_ref() {
                            if let Some(cf) =
                                gr.corpus.files.get(&f.rel).filter(|cf| cf.family == "html")
                            {
                                if let Ok(Some(bytes)) =
                                    extract::read_whole(&f.path, sn.gzip, extract::MAX_WHOLE_FILE)
                                {
                                    let (raw, _) = sniff::decode_text(&bytes);
                                    gr.href_raw.detect_raw_html(
                                        &gr.corpus,
                                        cf,
                                        &raw,
                                        &mut edge_drafts,
                                    );
                                }
                            }
                        }
                    }
                    if send_err.is_none() {
                        if let Err(error) = content::verify(&f.path, f.size, expected_digest) {
                            send_err = Some(format!(
                                "{error:#}; no records from this changing file were made visible"
                            ));
                        }
                    }
                    // Visibility begins only after extraction and the second
                    // full-content verification. Delete-before-replace makes
                    // a retry clean up any partial prior attempt.
                    if send_err.is_none() && cleanup_required.contains(key) {
                        let mut indices: Vec<&str> = fa
                            .assignments
                            .iter()
                            .filter_map(|(_, slug)| ds_rt.get(slug).map(|rt| rt.index.as_str()))
                            .collect();
                        indices.sort_unstable();
                        indices.dedup();
                        for index in indices {
                            if let Err(error) =
                                es.delete_by_query(index, &json!({"term": {"ax_file": key}}))
                            {
                                send_err = Some(format!(
                                    "remove prior records for {} before replacement: {error:#}",
                                    f.rel
                                ));
                                break;
                            }
                        }
                        if send_err.is_none() {
                            if let Err(error) = replacement_failpoint(2) {
                                send_err = Some(format!("{error:#}"));
                            }
                        }
                    }
                    if send_err.is_none() {
                        if let Err(error) = staged.as_file_mut().rewind() {
                            send_err =
                                Some(format!("rewind staged records for {}: {error}", f.rel));
                        }
                    }
                    if send_err.is_none() {
                        let mut reader = BufReader::new(staged.as_file_mut());
                        let mut buf = Vec::with_capacity(bulk_cut + (1 << 20));
                        let mut docs = 0usize;
                        loop {
                            let mut action = Vec::new();
                            match reader.read_until(b'\n', &mut action) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(error) => {
                                    send_err =
                                        Some(format!("read staged action for {}: {error}", f.rel));
                                    break;
                                }
                            }
                            let mut document = Vec::new();
                            match reader.read_until(b'\n', &mut document) {
                                Ok(0) => {
                                    send_err = Some(format!(
                                        "staged record for {} ended without a document",
                                        f.rel
                                    ));
                                    break;
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    send_err = Some(format!(
                                        "read staged document for {}: {error}",
                                        f.rel
                                    ));
                                    break;
                                }
                            }
                            buf.extend_from_slice(&action);
                            buf.extend_from_slice(&document);
                            docs += 1;
                            if (buf.len() >= bulk_cut || docs >= 5000)
                                && record_bulk_outcome(
                                    &es,
                                    std::mem::take(&mut buf),
                                    &junk_records,
                                    &bulk_errors,
                                    &mut send_err,
                                )
                            {
                                break;
                            }
                            if buf.is_empty() {
                                docs = 0;
                                buf.reserve(bulk_cut);
                            }
                        }
                        if !buf.is_empty() && send_err.is_none() {
                            record_bulk_outcome(
                                &es,
                                buf,
                                &junk_records,
                                &bulk_errors,
                                &mut send_err,
                            );
                        }
                    }
                    // Second-brain edges for this file (§6.7): only after the
                    // node bulks were accepted, so an edge never precedes its
                    // own src doc. A failed edge send leaves the file
                    // un-journaled — the whole file (nodes AND edges) is
                    // republished on the next run, which converges because
                    // both sides overwrite by deterministic _id.
                    if send_err.is_none() && !edge_drafts.is_empty() {
                        if let Some(gr) = graph.as_ref() {
                            let out =
                                detect::assemble(&edge_drafts, &gr.edges_index, gr.created_at_ms);
                            gr.self_dropped
                                .fetch_add(out.self_dropped, Ordering::Relaxed);
                            let mut ebuf: Vec<u8> = Vec::new();
                            for edge in &out.edges {
                                ebuf.extend_from_slice(&edge.ndjson);
                                if ebuf.len() >= bulk_cut
                                    && record_bulk_outcome(
                                        &es,
                                        std::mem::take(&mut ebuf),
                                        &junk_records,
                                        &bulk_errors,
                                        &mut send_err,
                                    )
                                {
                                    break;
                                }
                            }
                            if send_err.is_none() && !ebuf.is_empty() {
                                record_bulk_outcome(
                                    &es,
                                    ebuf,
                                    &junk_records,
                                    &bulk_errors,
                                    &mut send_err,
                                );
                            }
                            if send_err.is_none() {
                                let mut written = gr.written.lock().unwrap();
                                for edge in &out.edges {
                                    *written.entry(edge.detector).or_default() += 1;
                                }
                            }
                        }
                    }
                    if let Some(e) = send_err {
                        // endpoint trouble: record, do NOT journal file_done
                        let mut be = bulk_errors.lock().unwrap();
                        if be.len() < 5 {
                            be.push(e);
                        }
                        continue;
                    }
                    if let Err(error) = replacement_failpoint(4) {
                        let mut errors = bulk_errors.lock().unwrap();
                        if errors.len() < 5 {
                            errors.push(format!("{error:#}"));
                        }
                        continue;
                    }
                    records_total.fetch_add(file_records, Ordering::Relaxed);
                    junk_records.fetch_add(file_junk, Ordering::Relaxed);
                    if let Some(rt) = fa.assignments.first().and_then(|(_, slug)| ds_rt.get(slug)) {
                        rt.bytes.fetch_add(
                            f.size / fa.assignments.len().max(1) as u64,
                            Ordering::Relaxed,
                        );
                        if file_junk > 0 {
                            rt.junk.fetch_add(file_junk, Ordering::Relaxed);
                        }
                    }
                    let (commit_result, journal_path) = {
                        let mut journal = journal_mx.lock().unwrap();
                        let path = journal.path().display().to_string();
                        let result = journal.file_done(&FileDone {
                            file_key: key.clone(),
                            path: f.rel.clone(),
                            records: file_records,
                            junk: file_junk,
                            bytes: f.size,
                            generation: Some(expected_digest.clone()),
                        });
                        (result, path)
                    };
                    match commit_result {
                        Ok(()) => {}
                        Err(error) => {
                            let mut errors = bulk_errors.lock().unwrap();
                            if errors.len() < 5 {
                                errors.push(format!(
                                    "durably commit completed source {} to {}: {error:#}. \
                                     Live records may be present, but the file remains pending; \
                                     repair journal storage and rerun autoindex",
                                    f.rel, journal_path
                                ));
                            }
                            continue;
                        }
                    }
                    // Workers no longer print. A worker can block for minutes
                    // inside one file, so a worker-driven heartbeat cannot
                    // bound silence; the ticker thread can, and does.
                    files_done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    // Corpus-wide edges (§6.6.2, `EdgeDetector::detect_corpus`): the pass for
    // relationships that only exist once EVERY document has been read —
    // sharedterm cannot know which words are distinctive until it has seen the
    // whole run. It runs after Phase B for the same reason per-file edges are
    // written after their file's nodes: an edge must never precede the docs it
    // points at. Skipped when Phase B already failed — that run bails below,
    // and edges over a half-read corpus would be edges over a lie.
    if let Some(gr) = &graph {
        if bulk_errors.lock().unwrap().is_empty() {
            pr.phase("graph-corpus", gr.detectors.len() as u64, 0);
            let mut drafts = Vec::new();
            for det in &gr.detectors {
                det.detect_corpus(&gr.corpus, &mut drafts);
                pr.item_done(0);
            }
            if !drafts.is_empty() {
                let out = detect::assemble(&drafts, &gr.edges_index, gr.created_at_ms);
                gr.self_dropped
                    .fetch_add(out.self_dropped, Ordering::Relaxed);
                let mut send_err: Option<String> = None;
                let mut buf: Vec<u8> = Vec::new();
                for edge in &out.edges {
                    buf.extend_from_slice(&edge.ndjson);
                    if buf.len() >= bulk_cut
                        && record_bulk_outcome(
                            &es,
                            std::mem::take(&mut buf),
                            &junk_records,
                            &bulk_errors,
                            &mut send_err,
                        )
                    {
                        break;
                    }
                }
                if send_err.is_none() && !buf.is_empty() {
                    record_bulk_outcome(&es, buf, &junk_records, &bulk_errors, &mut send_err);
                }
                match send_err {
                    Some(e) => bulk_errors
                        .lock()
                        .unwrap()
                        .push(format!("write corpus-wide graph edges: {e}")),
                    None => {
                        let mut written = gr.written.lock().unwrap();
                        for edge in &out.edges {
                            *written.entry(edge.detector).or_default() += 1;
                        }
                    }
                }
            }
        }
    }

    let bulk_errs = bulk_errors.into_inner().unwrap();
    if !bulk_errs.is_empty() {
        anyhow::bail!(
            "autoindex stopped with bulk/backend failures: {}. Failed source files were not \
             journaled complete; fix the reported server or embedding configuration and rerun \
             the same command to resume safely",
            bulk_errs.join(" | ")
        );
    }

    // ── finalize: refresh, verify, correlate, catalog ────────────────────
    //
    // This block was 47-64% of every measured run and emitted NOTHING between
    // the last phase-B line and the final summary (#241 §1). Its work is fully
    // countable before each loop starts, so it is reported like any other
    // phase — split into named sub-phases because the mix of one-shot refreshes
    // and per-dataset round trips has no single honest denominator.
    pr.phase("finalize-refresh", 1 + graph.is_some() as u64, 0);
    es.refresh(&format!("{}-*", cfg.prefix)).ok();
    pr.item_done(0);
    // The dot-prefixed edges index is outside the {prefix}-* pattern.
    if let Some(gr) = &graph {
        es.refresh(&gr.edges_index).ok();
        pr.item_done(0);
    }

    // live per-dataset counts + time ranges (every claim traces to a run)
    pr.phase("finalize-count", plan.datasets.len() as u64, 0);
    let mut ds_counts: HashMap<String, u64> = HashMap::new();
    let mut ds_timerange: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for d in &plan.datasets {
        let _counted = pr.file(&d.index, 0);
        let cnt = es.count(&d.index).unwrap_or(0);
        ds_counts.insert(d.slug.clone(), cnt);
        if let Some(t) = &d.time_field {
            let body = json!({"size":0,"aggs":{
                "mn":{"min":{"field":t}},"mx":{"max":{"field":t}}}});
            if let Ok(v) = es.search(&d.index, &body) {
                let get = |k: &str| -> Option<String> {
                    let a = v.pointer(&format!("/aggregations/{k}"))?;
                    a.get("value_as_string")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            a.get("value").and_then(|f| f.as_f64()).and_then(|ms| {
                                chrono::DateTime::from_timestamp_millis(ms as i64)
                                    .map(|d| infer::dates::to_rfc3339_millis(&d))
                            })
                        })
                };
                ds_timerange.insert(d.slug.clone(), (get("mn"), get("mx")));
            }
        }
    }

    // ── zero-live verification gate (#195) ───────────────────────────────
    //
    // The journal's completed files claim records were made visible; the
    // live counts above are what the server actually answers with. When the
    // journal says records landed but ZERO documents are live across every
    // dataset index, the run must not report success: the user (or agent)
    // would be left with green, mapped, empty indices and no way to tell
    // "the corpus has no match" from "nothing was ever written". This is
    // the last-resort catch for any rejection path the per-bulk
    // classification does not recognise.
    let (files_done_journaled, journal_records) = {
        let journal = journal_mx.lock().unwrap();
        (
            journal.done.len(),
            journal.done.values().map(|fd| fd.records).sum::<u64>(),
        )
    };
    let live_records: u64 = ds_counts.values().sum();
    if journal_records > 0 && live_records == 0 {
        anyhow::bail!(
            "autoindex verification failed: the resume journal records {journal_records} \
             record(s) from {files_done_journaled} completed file(s), but 0 documents are \
             live across the {} dataset index(es). The indices exist and look healthy but \
             hold nothing — a server-side write rejection (e.g. a disk flood-stage or \
             index write block), deleted indices, or an unreadable server can all cause \
             this. Fix the server-side condition, then rerun with --fresh to re-index \
             from scratch",
            plan.datasets.len()
        );
    }

    // correlations
    let mut key_corrs: Vec<correlate::KeyCorr> = Vec::new();
    if let Some(clusters) = &clusters_rt {
        let mut cands = Vec::new();
        for (c, d) in clusters.iter().zip(plan.datasets.iter()) {
            for spec in &d.specs {
                let Some(acc) = c.fields.get(&spec.name) else {
                    continue;
                };
                if correlate::is_candidate(
                    &spec.es_type,
                    spec.semantic.as_deref(),
                    acc.distinct.len(),
                    acc.n,
                    acc.distinct_overflow,
                    (acc.long_ok > 0).then_some((acc.int_min, acc.int_max)),
                ) {
                    cands.push(correlate::Candidate {
                        slug: d.slug.clone(),
                        index: d.index.clone(),
                        field: spec.name.clone(),
                        kind: spec.es_type.clone(),
                        values: acc.raw_values.clone(),
                        sampled_n: acc.n,
                    });
                }
            }
        }
        key_corrs = correlate::key_overlaps(&cands);
        // One live query per candidate overlap, and the bound is known before
        // the loop starts — `phase=finalize-correlate items=37/135` needs no
        // new bookkeeping at all (#241 §8).
        pr.phase("finalize-correlate", key_corrs.len() as u64, 0);
        for c in key_corrs.iter_mut() {
            let _query = pr.file(
                &format!("{}.{} ~ {}.{}", c.a_slug, c.a_field, c.b_slug, c.b_field),
                0,
            );
            correlate::confirm(&es, c, 20).ok();
        }
        // keep only live-confirmed overlaps in the report
        key_corrs.retain(|c| c.confirmed.map(|(n, _)| n > 0).unwrap_or(false));
    } else {
        pr.note("(resumed run: key-overlap correlations kept from the original run's catalog)");
    }

    let timed: Vec<&PlanDataset> = plan
        .datasets
        .iter()
        .filter(|d| d.time_field.is_some())
        .collect();
    pr.phase("finalize-histogram", timed.len() as u64, 0);
    let mut series = Vec::new();
    for d in timed {
        if let Some(t) = &d.time_field {
            let _query = pr.file(&d.index, 0);
            if let Ok(Some(s)) = correlate::fetch_histogram(&es, &d.slug, &d.index, t) {
                series.push(s);
            }
        }
    }
    let time_corrs = correlate::time_alignment(&series);

    // ── catalog write ────────────────────────────────────────────────────
    // Alias IDs changed as identity evolved. Remove by logical path first so
    // catalogs created by any previous identity scheme cannot survive beside
    // the one current alias document.
    pr.phase(
        "finalize-catalog",
        alias_paths_to_replace.len() as u64 + 1,
        0,
    );
    for path in &alias_paths_to_replace {
        let _delete = pr.file(path, 0);
        es.delete_by_query(
            catalog::CATALOG_INDEX,
            &json!({
                "bool": {
                    "filter": [
                        {"term": {"status": "duplicate"}},
                        {"term": {"path": path}}
                    ]
                }
            }),
        )
        .with_context(|| format!("replace catalog alias for {path}"))?;
    }
    let mut cat_buf: Vec<u8> = Vec::new();
    let push_doc = |id: &str, doc: &Value, buf: &mut Vec<u8>| {
        let action = json!({"index": {"_index": catalog::CATALOG_INDEX, "_id": id}});
        buf.extend_from_slice(action.to_string().as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(doc.to_string().as_bytes());
        buf.push(b'\n');
    };
    for id in &stale_alias_ids {
        let action = json!({"delete": {"_index": catalog::CATALOG_INDEX, "_id": id}});
        cat_buf.extend_from_slice(action.to_string().as_bytes());
        cat_buf.push(b'\n');
    }
    // Junk/skipped files that left the corpus (#238). Deletes lead the buffer
    // so a key that is re-reported in the same run — a junk file whose bytes
    // changed, a legacy plan key rewritten under the current scheme — is
    // deleted before its replacement document is indexed, never after.
    let mut swept_junk_keys: Vec<&str> = stale_junk_keys.iter().map(String::as_str).collect();
    swept_junk_keys.sort_unstable();
    for key in &swept_junk_keys {
        let action = json!({"delete": {
            "_index": catalog::CATALOG_INDEX,
            "_id": catalog::file_id(key),
        }});
        cat_buf.extend_from_slice(action.to_string().as_bytes());
        cat_buf.push(b'\n');
    }

    // dataset docs
    let mut junk_records_by_run: u64 = junk_records.load(Ordering::Relaxed);
    for d in &plan.datasets {
        let rt = &ds_rt[&d.slug];
        let sample_queries = catalog::build_sample_queries(d, &key_corrs);
        let mut notes = Vec::new();
        let dropped = rt.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            notes.push(format!(
                "{dropped} field values could not be coerced to the inferred types and were dropped (records still indexed)"
            ));
        }
        if let Some(g) = &d.group {
            notes.push(format!("source table: {g}"));
        }
        for s in &d.specs {
            for n in &s.notes {
                notes.push(format!("{}: {}", s.name, n));
            }
        }
        // formats incl gz flag
        let mut formats: Vec<String> = plan
            .files
            .values()
            .filter(|fa| fa.assignments.iter().any(|(_, s)| s == &d.slug))
            .map(|fa| {
                if fa.gzip {
                    format!("{}(gzip)", fa.family)
                } else {
                    fa.family.clone()
                }
            })
            .collect();
        formats.sort();
        formats.dedup();
        let (tmin, tmax) = ds_timerange.get(&d.slug).cloned().unwrap_or((None, None));
        let (id, doc) = catalog::dataset_doc(&catalog::DatasetDocInput {
            pd: d,
            record_count: *ds_counts.get(&d.slug).unwrap_or(&0),
            junk_records: rt.junk.load(Ordering::Relaxed),
            bytes: rt.bytes.load(Ordering::Relaxed),
            file_count: d.file_count,
            formats,
            time_min: tmin,
            time_max: tmax,
            sample_queries,
            notes,
            run_id: &run_id,
        });
        push_doc(&id, &doc, &mut cat_buf);
    }

    // file docs — indexed (from journal) + junk/skipped (from plan + this run)
    {
        let j = journal_mx.lock().unwrap();
        for fd in j.done.values() {
            let current_path = plan
                .files
                .get(&fd.file_key)
                .map(|assignment| assignment.rel.as_str())
                .unwrap_or(&fd.path);
            let fmt = plan
                .files
                .get(&fd.file_key)
                .map(|fa| {
                    if fa.gzip {
                        format!("{}(gzip)", fa.family)
                    } else {
                        fa.family.clone()
                    }
                })
                .unwrap_or_else(|| "unknown".into());
            let (id, doc) = catalog::file_doc(
                &fd.file_key,
                current_path,
                &fmt,
                "indexed",
                None,
                fd.records,
                fd.junk,
                fd.bytes,
                &run_id,
            );
            push_doc(&id, &doc, &mut cat_buf);
        }
    }
    let extra = extra_junk.into_inner().unwrap();
    let mut all_junk: Vec<&JunkFile> = plan
        .junk_files
        .iter()
        .filter(|junk| !stale_junk_keys.contains(&junk.file_key))
        .collect();
    all_junk.extend(extra.iter());
    all_junk.extend(new_unplanned.iter());
    // Counted now: every later reader of this number outlives the borrows
    // `all_junk` holds on `plan` and `new_unplanned`, which the durable
    // junk-plan update below mutates.
    let junk_file_count = all_junk.len();
    for jf in &all_junk {
        let (id, doc) = catalog::file_doc(
            &jf.file_key,
            &jf.rel,
            &jf.format,
            &jf.status,
            Some(&jf.reason),
            0,
            0,
            jf.bytes,
            &run_id,
        );
        push_doc(&id, &doc, &mut cat_buf);
        junk_records_by_run += 0; // junk FILES tracked separately from junk records
    }
    for duplicate in &plan.duplicate_files {
        let (id, doc) = catalog::duplicate_file_doc(
            &duplicate.file_key,
            &duplicate.rel,
            &duplicate.path_id,
            &duplicate.duplicate_of,
            duplicate.bytes,
            &run_id,
        );
        push_doc(&id, &doc, &mut cat_buf);
    }

    for c in &key_corrs {
        let mut v = c.to_value();
        v["run_id"] = json!(run_id);
        push_doc(&c.id(), &v, &mut cat_buf);
    }
    for (i, tc) in time_corrs.iter().enumerate() {
        let id = format!(
            "tcorr:{}:{}",
            tc.get("a_dataset").and_then(|v| v.as_str()).unwrap_or(""),
            tc.get("b_dataset")
                .and_then(|v| v.as_str())
                .unwrap_or(&i.to_string())
        );
        let mut v = tc.clone();
        v["run_id"] = json!(run_id);
        push_doc(&id, &v, &mut cat_buf);
    }

    let wall = t0.elapsed().as_secs_f64();
    let total_records: u64 = ds_counts.values().sum();
    // Run-summary honesty (§6.6.4): what the detectors wrote AND what they
    // could not resolve — a dangling [[link]] is a fact about the corpus, not
    // something to swallow.
    let graph_summary = graph.as_ref().map(|gr| {
        let written = gr.written.lock().unwrap();
        let mut counters = detect::DetectorCounters::default();
        for det in &gr.detectors {
            let c = det.counters();
            counters.unresolved += c.unresolved;
            counters.ambiguous += c.ambiguous;
            counters.capped += c.capped;
        }
        let raw = gr.href_raw.counters();
        counters.unresolved += raw.unresolved;
        counters.ambiguous += raw.ambiguous;
        let by_detector: Map<String, Value> = written
            .iter()
            .map(|(tag, n)| ((*tag).to_string(), json!(n)))
            .collect();
        json!({
            "brain": gr.brain,
            "edges_index": gr.edges_index,
            "edges_written": written.values().sum::<u64>(),
            "by_detector": by_detector,
            "edges_unresolved": counters.unresolved,
            "edges_ambiguous": counters.ambiguous,
            "edges_capped": counters.capped,
            "edges_self_dropped": gr.self_dropped.load(Ordering::Relaxed),
            "edges_invalidated": gr.invalidated,
        })
    });
    let mut run_doc = json!({
        "doc_kind": "run",
        "run_id": run_id,
        "root": root_str,
        "url": cfg.url,
        "prefix": cfg.prefix,
        "started": chrono::Utc::now().to_rfc3339(),
        "files_total": paths_discovered,
        "unique_content_files": files.len(),
        "files_indexed": journal_mx.lock().unwrap().done.len(),
        "duplicate_files": plan.duplicate_files.len(),
        "files_junk": junk_file_count,
        "records_total": total_records,
        "junk_records_total": junk_records_by_run,
        // This-run submission accounting (#195): what THIS invocation sent
        // and had accepted, vs `records_total` above which is the live
        // server-side count. A healthy run keeps these consistent; a
        // mismatch is visible without reading a server log.
        "files_submitted_this_run": files_done.load(Ordering::Relaxed),
        "records_submitted_this_run": records_total.load(Ordering::Relaxed),
        "wall_seconds": (wall * 10.0).round() / 10.0,
        "workers": cfg.workers,
        // The whole resource decision, so a run can be explained after the
        // fact from its own summary rather than from the machine it ran on.
        // `scan_workers` is the width phase A actually ran at — see the
        // first-call-wins note at the top of this function; a requested width
        // that lost that race would make the summary a record of an intention
        // rather than of a run.
        "scan_workers": scan_threads,
        "pdf_workers": cfg.pdf_workers,
        "bulk_mb": cfg.bulk_mb,
        "cores_available": xerj_common::resource::cores(),
        // `null` on a platform with no RAM probe — the summary reports what the
        // run actually knew, never a stand-in number (#240).
        "memory_safe_zone_mb": xerj_common::resource::memory_safe_zone_bytes()
            .map(|b| b / (1024 * 1024)),
        // What the server's backpressure did to the offered load. A final
        // limit below `workers` means this run met real congestion and
        // answered it, which is the difference between a slow run and a run
        // that was making the machine worse (#240 §8).
        "bulk_concurrency_final": es.bulk_concurrency_limit(),
        "bulk_congestion_events": es.bulk_congestion_events(),
        "resource_notes": cfg.resource_notes,
        "semantic": !cfg.no_semantic,
    });
    if let Some(g) = &graph_summary {
        run_doc["graph"] = g.clone();
    }
    push_doc(&format!("run:{run_id}"), &run_doc, &mut cat_buf);

    if !cat_buf.is_empty() {
        let outcome = es.bulk(cat_buf).context("write catalog")?;
        // The catalog is the data map every later `map`/`status`/agent query
        // reads; a rejected catalog bulk (e.g. a write block that engaged
        // mid-run) must not be swallowed into a "success" exit (#195).
        if outcome.server_errors > 0 {
            anyhow::bail!(
                "write catalog: bulk backend failed for {} item(s): {}. Fix the reported \
                 server condition and rerun the same command",
                outcome.server_errors,
                outcome
                    .first_server_error
                    .as_deref()
                    .unwrap_or("unknown server error")
            );
        }
    }
    es.refresh(catalog::CATALOG_INDEX).ok();
    pr.item_done(0);
    // ── durable junk record, written last (#238) ─────────────────────────
    //
    // Only now is the plan allowed to claim what the catalog holds: the
    // documents above are written and the swept ones deleted. A failed
    // catalog bulk bailed out before this point with the plan untouched, so
    // the next run recomputes the identical additions and removals and
    // retries them — losing a record here is what makes a document immortal.
    //
    // Post-freeze skipped files are re-derived on every rerun, so this
    // converges: once they are in the plan they are `planned_junk`, the
    // additions are empty, and no further plan record is appended.
    if !new_unplanned.is_empty() || !stale_junk_keys.is_empty() {
        plan.junk_files
            .retain(|junk| !stale_junk_keys.contains(&junk.file_key));
        plan.junk_files.append(&mut new_unplanned);
        journal_mx
            .lock()
            .unwrap()
            .write_plan(&plan)
            .context("record junk/skipped catalog entries in the resume plan")?;
    }
    journal_mx.lock().unwrap().finish(&run_doc)?;

    // ── summary ──────────────────────────────────────────────────────────
    let junk_total_records = junk_records.load(Ordering::Relaxed);
    if cfg.json {
        println!("{run_doc}");
    } else if !cfg.quiet {
        println!("\ndone in {wall:.1}s — {} datasets, {} records live, {} duplicate aliases, {} junk records, {} junk/skipped files",
            plan.datasets.len(), total_records, plan.duplicate_files.len(), junk_total_records, junk_file_count);
        // Indexed-vs-submitted honesty line (#195): the live count against
        // what this run actually submitted, so a silent-rejection mismatch
        // is visible in the client output alone. Units differ on purpose: a
        // source record (e.g. one prose file) can expand to several section
        // documents, but submitted records with ZERO live documents is
        // always a defect (and fails above, before this line prints).
        println!(
            "indexed: {} documents live; this run submitted {} source records from {} files",
            total_records,
            records_total.load(Ordering::Relaxed),
            files_done.load(Ordering::Relaxed),
        );
        let mut rows: Vec<(&String, u64)> = plan
            .datasets
            .iter()
            .map(|d| (&d.index, *ds_counts.get(&d.slug).unwrap_or(&0)))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        for (idx, cnt) in rows {
            println!("  {idx:<40} {cnt:>10} docs");
        }
        if let Some(g) = &graph_summary {
            let by: Vec<String> = g["by_detector"]
                .as_object()
                .map(|m| m.iter().map(|(tag, n)| format!("{tag} {n}")).collect())
                .unwrap_or_default();
            println!(
                "graph: {} edges → {} ({}); {} unresolved, {} ambiguous, {} capped, {} self-dropped, {} invalidated",
                g["edges_written"],
                g["edges_index"].as_str().unwrap_or(""),
                if by.is_empty() { "no detections".to_string() } else { by.join(", ") },
                g["edges_unresolved"],
                g["edges_ambiguous"],
                g["edges_capped"],
                g["edges_self_dropped"],
                g["edges_invalidated"],
            );
        }
        println!(
            "\nnext: `xerj autoindex map --url {}` for the data map; search via GET /{}-*/_search",
            cfg.url, cfg.prefix
        );
    }
    let code = if junk_total_records > 0 || junk_file_count > 0 {
        3
    } else {
        0
    };
    // Terminal line, in every progress mode but `none` (which `--quiet`
    // selects, and which prints nothing by definition). Exit 3 means
    // "completed, some files were unparseable" — success — and an agent reading
    // a bare `3` off a silent stream reads failure (#241 §9). Say it in words.
    pr.finish(
        true,
        code,
        if code == 3 {
            "completed-with-junk"
        } else {
            "completed"
        },
        &[
            ("files", files_done.load(Ordering::Relaxed)),
            ("records", total_records),
            ("datasets", plan.datasets.len() as u64),
            ("junk_files", junk_file_count as u64),
        ],
    );
    drop(ticker);
    Ok((code, Some(run_doc)))
}

fn format_str(sn: Option<&Sniffed>) -> String {
    match sn {
        Some(s) if s.gzip => format!("{}(gzip)", s.family.as_str()),
        Some(s) => s.family.as_str().to_string(),
        None => "unknown".into(),
    }
}

// ─── map subcommand ──────────────────────────────────────────────────────

fn run_map(cfg: MapCfg) -> Result<i32> {
    let es = Es::new(&cfg.url, cfg.api_key.clone())?;
    es.ping()?;
    let fetch = |query: Value, size: usize, sort: Option<Value>| -> Result<Vec<Value>> {
        let mut body = json!({"query": query, "size": size});
        if let Some(s) = sort {
            body["sort"] = s;
        }
        let v = es.search(catalog::CATALOG_INDEX, &body)?;
        Ok(v.pointer("/hits/hits")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| h.get("_source").cloned())
                    .collect()
            })
            .unwrap_or_default())
    };
    let mut ds_query = json!({"term": {"doc_kind": "dataset"}});
    if let Some(slug) = &cfg.dataset {
        ds_query = json!({"bool": {"must": [
            {"term": {"doc_kind": "dataset"}},
            {"term": {"slug": slug}}
        ]}});
    }
    let datasets = fetch(ds_query, 500, Some(json!([{"record_count": "desc"}])))?;
    if datasets.is_empty() {
        eprintln!(
            "no autoindex catalog found at {} (index {}) — run `xerj autoindex <folder>` first",
            cfg.url,
            catalog::CATALOG_INDEX
        );
        return Ok(1);
    }
    let mut runs = fetch(json!({"term": {"doc_kind": "run"}}), 50, None)?;
    runs.sort_by_key(|r| {
        std::cmp::Reverse(
            r.get("started")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        )
    });
    let correlations = {
        let mut all = fetch(json!({"term": {"doc_kind": "correlation"}}), 200, None)?;
        // stale-correlation hygiene: catalog docs upsert by deterministic id,
        // so older runs' correlations linger — show only the latest run that
        // produced each corr_kind.
        for kind in ["key_overlap", "time_alignment"] {
            let latest = all
                .iter()
                .filter(|c| c.get("corr_kind").and_then(|k| k.as_str()) == Some(kind))
                .filter_map(|c| c.get("run_id").and_then(|r| r.as_str()))
                .max()
                .map(|s| s.to_string());
            all.retain(|c| {
                c.get("corr_kind").and_then(|k| k.as_str()) != Some(kind)
                    || c.get("run_id")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                        == latest
            });
        }
        all
    };
    let latest_run_filter = runs
        .first()
        .and_then(|run| run.get("run_id"))
        .and_then(|value| value.as_str())
        .map(|run_id| json!({"term": {"run_id": run_id}}));
    let mut junk_must = vec![json!({"term": {"doc_kind": "file"}})];
    if let Some(filter) = latest_run_filter.clone() {
        junk_must.push(filter);
    }
    let junk_files = fetch(
        json!({"bool": {"must": junk_must,
            "must_not": [
                {"term": {"status": "indexed"}},
                {"term": {"status": "duplicate"}}
        ]}}),
        500,
        None,
    )?;
    let mut duplicate_must = vec![
        json!({"term": {"doc_kind": "file"}}),
        json!({"term": {"status": "duplicate"}}),
    ];
    if let Some(filter) = latest_run_filter {
        duplicate_must.push(filter);
    }
    let duplicate_files = fetch(json!({"bool": {"must": duplicate_must}}), 500, None)?;
    if cfg.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "run": runs.first(),
                "datasets": datasets,
                "correlations": correlations,
                "junk_files": junk_files,
                "duplicate_files": duplicate_files,
                "gotchas": catalog::GOTCHAS,
            }))?
        );
    } else {
        print!(
            "{}",
            catalog::render_map(
                runs.first(),
                &datasets,
                &correlations,
                &junk_files,
                &duplicate_files,
                junk_files.len() as u64
            )
        );
        // Second-brain summary (§6.1): live edge count straight from the
        // edges index, scoped by `exists src` so the meta doc never counts.
        if let Some(g) = runs.first().and_then(|r| r.get("graph")) {
            if let (Some(brain), Some(edges_index)) = (
                g.get("brain").and_then(Value::as_str),
                g.get("edges_index").and_then(Value::as_str),
            ) {
                let live = es
                    .search(
                        edges_index,
                        &json!({
                            "size": 0,
                            "track_total_hits": true,
                            "query": {"bool": {
                                "filter": [{"exists": {"field": "src"}}],
                                "must_not": [{"exists": {"field": "invalid_at"}}]
                            }}
                        }),
                    )
                    .ok()
                    .and_then(|v| v.pointer("/hits/total/value").and_then(Value::as_u64));
                match live {
                    Some(n) => println!("\ngraph: {n} live edges in {edges_index} (brain {brain})"),
                    None => {
                        println!("\ngraph: brain {brain} — edges index {edges_index} unreachable")
                    }
                }
            }
        }
    }
    Ok(0)
}

// ─── status subcommand ───────────────────────────────────────────────────

fn run_status(cfg: StatusCfg) -> Result<i32> {
    // journals
    let dirs: Vec<std::path::PathBuf> = match &cfg.state_dir {
        Some(d) => vec![d.clone()],
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let base = Path::new(&home).join(".xerj").join("autoindex");
            std::fs::read_dir(&base)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default()
        }
    };
    for d in dirs {
        let jp = d.join("journal.ndjson");
        if !jp.exists() {
            continue;
        }
        let mut root = String::new();
        let mut done = 0u64;
        let mut records = 0u64;
        let mut finished = false;
        let mut graph_line: Option<String> = None;
        if let Ok(f) = std::fs::File::open(&jp) {
            use std::io::BufRead;
            for line in std::io::BufReader::new(f).lines().map_while(|l| l.ok()) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    match v.get("kind").and_then(|k| k.as_str()) {
                        Some("run") => {
                            root = v.get("root").and_then(|r| r.as_str()).unwrap_or("").into()
                        }
                        Some("file_done") => {
                            done += 1;
                            records += v.get("records").and_then(|r| r.as_u64()).unwrap_or(0);
                        }
                        Some("finish") => {
                            finished = true;
                            // Latest finish wins — the summary embeds the run
                            // doc, whose `graph` block is the edge count of
                            // record for this journal.
                            if let Some(g) = v.pointer("/summary/graph") {
                                graph_line = Some(format!(
                                    "graph: {} edges written to {} (brain {})",
                                    g.get("edges_written").and_then(Value::as_u64).unwrap_or(0),
                                    g.get("edges_index").and_then(Value::as_str).unwrap_or("?"),
                                    g.get("brain").and_then(Value::as_str).unwrap_or("?"),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        println!(
            "journal {} — root {} — {} files done, {} records, {}",
            jp.display(),
            root,
            done,
            records,
            if finished { "FINISHED" } else { "in progress" }
        );
        if let Some(line) = graph_line {
            println!("  {line}");
        }
    }
    // live indices
    if let Ok(es) = Es::new(&cfg.url, cfg.api_key.clone()) {
        if es.ping().is_ok() {
            let pat = format!("{}-", cfg.prefix);
            println!("\nlive indices at {}:", cfg.url);
            for (name, docs) in es.cat_indices().unwrap_or_default() {
                if name.starts_with(&pat) || name == catalog::CATALOG_INDEX {
                    println!("  {name:<40} {docs:>10} docs");
                }
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod section_label_tests {
    use super::section_label;

    /// The two text-section locator grammars (§6.6.2) and their labels; every
    /// other locator shape must be None so row/line/byte records never reach
    /// `detect_text`.
    #[test]
    fn labels_only_text_section_locators() {
        assert_eq!(section_label("s0").as_deref(), Some("section 0"));
        assert_eq!(section_label("s17").as_deref(), Some("section 17"));
        assert_eq!(section_label("p1-s0").as_deref(), Some("page 1 section 0"));
        assert_eq!(
            section_label("p12-s3").as_deref(),
            Some("page 12 section 3")
        );
        for not_a_section in [
            "s", "sx", "s1x", "p1", "p1-s", "p-s1", "px-s1", "b1024", "row7", "file", "line3",
            "p1-s2-x",
        ] {
            assert_eq!(section_label(not_a_section), None, "{not_a_section}");
        }
    }
}

#[cfg(test)]
mod duplicate_integration_tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::io::Write;

    fn legacy_assignment(rel: &str) -> FileAssignment {
        FileAssignment {
            rel: rel.to_string(),
            path_id: String::new(),
            family: "txt".to_string(),
            gzip: false,
            content_digest: None,
            assignments: vec![(None, "text".to_string())],
            as_document: false,
        }
    }

    #[test]
    fn legacy_prefix_collision_has_one_deterministic_owner() {
        let corpus = tempfile::tempdir().unwrap();
        let mut a = vec![b'x'; 65_537];
        let mut b = a.clone();
        a[65_536] = b'a';
        b[65_536] = b'b';
        fs::write(corpus.path().join("a.txt"), a).unwrap();
        fs::write(corpus.path().join("b.txt"), b).unwrap();
        let files = walk::walk(corpus.path(), false).unwrap();
        let inventory = content::resolve_reporting(files.clone(), &|_| {}).unwrap();
        let legacy = ids::file_key(&files[0].path, files[0].size).unwrap();
        assert_eq!(
            legacy,
            ids::file_key(&files[1].path, files[1].size).unwrap()
        );

        // The exact historical owner sorts second. It must retain the legacy
        // key; the earlier collision sibling must never steal or share it.
        let mut plan = Plan::default();
        plan.files
            .insert(legacy.clone(), legacy_assignment("b.txt"));
        let error = select_resume_plan_keys(
            &inventory.files,
            &inventory.keys,
            &plan,
            Path::new("/state/journal.ndjson"),
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("collides with legacy resume key"));
        // Recovery advice must stay scoped to the two colliding files and be
        // honest that discarding the journal re-embeds the whole corpus.
        assert!(message.contains("remove or move one of these two files"));
        assert!(message.contains("/state/journal.ndjson"));
        assert!(message.contains("re-extracts and re-embeds the entire corpus"));
    }

    #[test]
    fn planned_key_claimed_by_path_diverts_the_content_claimant_deterministically() {
        let corpus = tempfile::tempdir().unwrap();
        // a.txt was planned under its old digest; its content has since
        // changed, while b.txt now holds exactly the bytes a.txt was planned
        // with — so b.txt's content key IS the planned key a.txt claims by rel.
        fs::write(corpus.path().join("a.txt"), b"rewritten content\n").unwrap();
        fs::write(corpus.path().join("b.txt"), b"original planned content\n").unwrap();
        let inventory =
            content::resolve_reporting(walk::walk(corpus.path(), false).unwrap(), &|_| {}).unwrap();
        let planned_key = inventory.keys[1].clone();
        let mut plan = Plan::default();
        plan.files
            .insert(planned_key.clone(), legacy_assignment("a.txt"));

        let selected = select_resume_plan_keys(
            &inventory.files,
            &inventory.keys,
            &plan,
            Path::new("/state/journal.ndjson"),
        )
        .unwrap();
        assert_eq!(selected[0].as_deref(), Some(planned_key.as_str()));
        let diverted = selected[1].as_deref().expect("diverted key");
        assert_ne!(diverted, planned_key);
        assert!(diverted.starts_with(&format!("{planned_key}-claimed-")));
        // The divergence is a pure function of (digest, path identity):
        // resumes select the same exclusive owner and the same diverted key.
        let again = select_resume_plan_keys(
            &inventory.files,
            &inventory.keys,
            &plan,
            Path::new("/state/journal.ndjson"),
        )
        .unwrap();
        assert_eq!(selected, again);
    }

    #[test]
    fn one_alias_change_invalidates_only_its_content_key() {
        let alias = |file_key: &str, rel: &str| state::DuplicateFile {
            file_key: file_key.to_string(),
            rel: rel.to_string(),
            path_id: format!("id:{rel}"),
            duplicate_of: format!("{file_key}.txt"),
            bytes: 10,
        };
        let previous = vec![alias("a", "a-copy.txt"), alias("c", "c-copy.txt")];
        let current = vec![
            alias("a", "a-copy.txt"),
            alias("b", "b-copy.txt"),
            alias("c", "c-copy.txt"),
        ];
        assert_eq!(
            alias_keys_to_reindex(&previous, &current, None),
            HashSet::from(["b".to_string()])
        );
        assert_eq!(
            alias_keys_to_reindex(&current, &previous, None),
            HashSet::from(["b".to_string()])
        );
        assert_eq!(
            alias_keys_to_reindex(
                &previous,
                &current,
                Some(&["a".to_string(), "b".to_string(), "c".to_string()])
            ),
            HashSet::from(["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn duplicate_content_keeps_journal_and_live_id_cardinality_equal_on_resume() {
        let corpus = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let body = "quarterly revenue was 42\noperating income was 7\n";
        fs::write(corpus.path().join("report-original.txt"), body).unwrap();
        fs::write(corpus.path().join("report-copy.txt"), body).unwrap();

        let discovered = walk::walk(corpus.path(), false).unwrap();
        let inventory = content::resolve_reporting(discovered, &|_| {}).unwrap();
        assert_eq!(inventory.files.len(), 1);
        assert_eq!(inventory.duplicates.len(), 1);

        let mut live_ids = HashSet::new();
        let mut records = 0u64;
        let sniffed = sniff::sniff(&inventory.files[0].path).unwrap();
        extract::extract(&inventory.files[0].path, &sniffed, None, &mut |record| {
            live_ids.insert(ids::doc_id("text", &inventory.keys[0], &record.locator));
            records += 1;
            true
        })
        .unwrap();

        let mut journal = state::Journal::open(
            state_dir.path(),
            "corpus",
            "http://engine",
            "test",
            300,
            false,
        )
        .unwrap();
        journal
            .write_plan(&Plan {
                duplicate_files: inventory.duplicates.clone(),
                ..Plan::default()
            })
            .unwrap();
        journal
            .file_done(&FileDone {
                file_key: inventory.keys[0].clone(),
                path: inventory.files[0].rel.clone(),
                records,
                junk: 0,
                bytes: inventory.files[0].size,
                generation: Some(inventory.digests[0].clone()),
            })
            .unwrap();
        drop(journal);

        let resumed = state::Journal::open(
            state_dir.path(),
            "corpus",
            "http://engine",
            "test",
            300,
            false,
        )
        .unwrap();
        assert!(resumed.resumed);
        assert_eq!(
            resumed.done.values().map(|f| f.records).sum::<u64>(),
            records
        );
        assert_eq!(records as usize, live_ids.len());
        let done = resumed.done_keys();
        assert!(inventory.keys.iter().all(|key| done.contains(key)));
        let aliases = &resumed.plan.unwrap().duplicate_files;
        assert_eq!(aliases, &inventory.duplicates);
    }

    #[test]
    fn mutation_after_more_than_one_bulk_is_staged_and_retry_replaces_stale_locators() {
        let corpus = tempfile::tempdir().unwrap();
        let path = corpus.path().join("large.csv");
        let mut csv = String::from("id,value\n");
        for id in 0..6_001 {
            csv.push_str(&format!("{id},old-{id}\n"));
        }
        fs::write(&path, csv).unwrap();

        let inventory =
            content::resolve_reporting(walk::walk(corpus.path(), false).unwrap(), &|_| {}).unwrap();
        let expected_size = inventory.files[0].size;
        let expected_digest = inventory.digests[0].clone();
        let sniffed = sniff::sniff(&path).unwrap();
        let mut staged = tempfile::NamedTempFile::new().unwrap();
        let mut staged_docs = 0usize;
        extract::extract(&path, &sniffed, None, &mut |record| {
            writeln!(
                staged,
                "{}\n{}",
                record.locator,
                Value::Object(record.fields)
            )
            .unwrap();
            staged_docs += 1;
            if staged_docs == 5_001 {
                // A shorter source replaces the file while extraction is in
                // progress, after the production 5,000-document bulk cut.
                fs::write(&path, "id,value\n0,new-0\n1,new-1\n").unwrap();
            }
            true
        })
        .unwrap();
        assert!(staged_docs > 5_000);

        let mut live: HashSet<String> = (0..6_001).map(|id| format!("row:{id}")).collect();
        assert!(content::verify(&path, expected_size, &expected_digest).is_err());
        // Verification precedes delete/visibility, so a rejected attempt has
        // not mixed any staged records into the old live set.
        assert_eq!(live.len(), 6_001);

        // The retry's delete-before-replace removes every old locator before
        // the now-short source becomes visible.
        live.clear();
        let retry_sniffed = sniff::sniff(&path).unwrap();
        extract::extract(&path, &retry_sniffed, None, &mut |record| {
            live.insert(record.locator);
            true
        })
        .unwrap();
        assert_eq!(live, HashSet::from(["r0".into(), "r1".into()]));
    }
}

#[cfg(test)]
mod failure_resume_http_tests;
