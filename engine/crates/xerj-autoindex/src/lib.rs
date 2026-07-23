//! `xerj autoindex` — point it at ANY folder and it makes the contents
//! AI-searchable with ZERO configuration. Pure ES-compat HTTP client feature:
//! it does NOT link xerj-engine, works against any endpoint, and cannot
//! destabilize the server.

pub mod catalog;
pub mod cli;
pub mod coerce;
pub mod correlate;
pub mod dataset;
pub mod esclient;
pub mod extract;
pub mod ids;
pub mod infer;
pub mod sniff;
pub mod state;
pub mod walk;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use cli::{Cmd, IndexCfg, MapCfg, StatusCfg};
use esclient::Es;
use sniff::{Family, Sniffed};
use state::{FileAssignment, FileDone, JunkFile, Plan, PlanDataset};

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

// ─── Phase A: per-file scan (sniff + bounded sampling) ───────────────────

struct FileScan {
    sniffed: Option<Sniffed>,
    /// group → (field accs, sampled record count)
    sketches: Vec<(Option<String>, HashMap<String, infer::FieldAcc>, u64)>,
    junk: Option<(String, String)>, // (status, reason)
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
    let mut groups: HashMap<Option<String>, (HashMap<String, infer::FieldAcc>, u64)> =
        HashMap::new();
    let grouped_family = matches!(sn.family, Family::SqlDump | Family::Sqlite);
    let mut sink = |rec: extract::RawRecord| -> bool {
        let entry = groups.entry(rec.group.clone()).or_default();
        if (entry.1 as usize) < sample {
            for (k, v) in &rec.fields {
                entry.0.entry(k.clone()).or_default().add(v);
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
    out.sketches = groups.into_iter().map(|(g, (f, n))| (g, f, n)).collect();
    out.sketches.sort_by(|a, b| a.0.cmp(&b.0));
    out.sniffed = Some(sn);
    out
}

// ─── mapping builder ─────────────────────────────────────────────────────

pub const PROVENANCE_FIELDS: &[&str] = &[
    "ax_path",
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

fn run_index(cfg: IndexCfg) -> Result<i32> {
    let t0 = Instant::now();
    let es = Es::new(&cfg.url, cfg.api_key.clone())?;
    es.ping()?;

    let files = walk::walk(&cfg.root, cfg.follow_symlinks)?;
    if files.is_empty() {
        println!("no files found under {}", cfg.root.display());
        return Ok(0);
    }
    let root_str = cfg
        .root
        .canonicalize()
        .unwrap_or_else(|_| cfg.root.clone())
        .to_string_lossy()
        .to_string();
    if !cfg.quiet {
        eprintln!(
            "autoindex: {} files ({} MB) under {}",
            files.len(),
            files.iter().map(|f| f.size).sum::<u64>() / (1 << 20),
            root_str
        );
    }

    // content-based file keys (parallel)
    use rayon::prelude::*;
    let keys: Vec<String> = files
        .par_iter()
        .map(|f| ids::file_key(&f.path, f.size).unwrap_or_default())
        .collect();

    let state_dir = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| state::default_state_dir(&root_str, &cfg.url, &cfg.prefix));
    let mut journal =
        state::Journal::open(&state_dir, &root_str, &cfg.url, &cfg.prefix, cfg.fresh)?;
    let run_id = journal.run_id.clone();
    if journal.resumed && !cfg.quiet {
        eprintln!(
            "resuming from journal {} ({} files already done)",
            journal.path().display(),
            journal.done.len()
        );
    }

    // ── Phase A: inference (skipped when a frozen plan exists) ──────────
    let mut clusters_rt: Option<Vec<dataset::Cluster>> = None;
    let plan: Plan = if let Some(p) = journal.plan.clone() {
        p
    } else {
        if !cfg.quiet {
            eprintln!("phase A: sniffing + sampling {} files…", files.len());
        }
        let scans: Vec<FileScan> = files
            .par_iter()
            .map(|f| scan_file(&f.path, f.size, cfg.sample, cfg.max_file_gb))
            .collect();

        let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
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
            for (group, fields, n) in sc.sketches {
                sketches.push(dataset::Sketch {
                    file_idx: i,
                    group,
                    family,
                    fields,
                    records: n,
                });
            }
        }
        let clusters = dataset::cluster(sketches, &rels);
        if !cfg.quiet {
            eprintln!(
                "phase A: {} datasets inferred, {} junk/skipped files",
                clusters.len(),
                junk_files.len()
            );
        }

        // per-file assignments
        let mut file_assignments: HashMap<String, FileAssignment> = HashMap::new();
        for (ci, c) in clusters.iter().enumerate() {
            for &m in &c.members {
                let key = &keys[m];
                let sn = sniff::sniff(&files[m].path).ok();
                let fa = file_assignments
                    .entry(key.clone())
                    .or_insert_with(|| FileAssignment {
                        rel: files[m].rel.clone(),
                        family: c.family.as_str().to_string(),
                        gzip: sn.map(|s| s.gzip).unwrap_or(false),
                        assignments: Vec::new(),
                    });
                fa.assignments
                    .push((c.group.clone(), clusters[ci].slug.clone()));
            }
        }

        let mut datasets = Vec::new();
        for c in &clusters {
            let specs = infer::infer_fields(&c.fields, c.records, cfg.no_semantic);
            let time_field = infer::elect_time_field(&specs);
            let semantic_field = specs
                .iter()
                .find(|s| s.es_type == "semantic_text")
                .map(|s| s.name.clone());
            datasets.push(PlanDataset {
                slug: c.slug.clone(),
                index: format!("{}-{}", cfg.prefix, c.slug),
                family: c.family.as_str().to_string(),
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
        };
        clusters_rt = Some(clusters);
        plan
    };

    if cfg.dry_run {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        eprintln!("(dry run — nothing indexed)");
        return Ok(0);
    }

    // ── create indices with explicit mappings ────────────────────────────
    for d in &plan.datasets {
        es.ensure_index(&d.index, &build_mapping(&d.specs))
            .with_context(|| format!("create index {}", d.index))?;
    }
    es.ensure_index(catalog::CATALOG_INDEX, &catalog::catalog_mapping())?;
    if journal.plan.is_none() {
        journal.write_plan(&plan)?;
    }

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
        if keys[i].is_empty() || done0.contains(&keys[i]) {
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
    // ascending by size — workers pop() from the tail, so the BIGGEST files
    // start first and can't serialize the end of the run.
    todo.sort_by_key(|&i| files[i].size);
    let n_todo = todo.len();
    if !cfg.quiet {
        eprintln!(
            "phase B: indexing {} files with {} workers → {}",
            n_todo, cfg.workers, cfg.url
        );
    }

    let queue = Mutex::new(todo);
    let journal_mx = Mutex::new(&mut journal);
    let files_done = AtomicU64::new(0);
    let records_total = AtomicU64::new(0);
    let junk_records = AtomicU64::new(0);
    let bulk_errors = Mutex::new(Vec::<String>::new());
    let extra_junk = Mutex::new(Vec::<JunkFile>::new());
    let bulk_cut = cfg.bulk_mb << 20;

    std::thread::scope(|scope| {
        for _ in 0..cfg.workers.min(n_todo.max(1)) {
            scope.spawn(|| {
                let mut buf: Vec<u8> = Vec::with_capacity(bulk_cut + (1 << 20));
                let mut buf_docs = 0usize;
                loop {
                    let i = match queue.lock().unwrap().pop() {
                        Some(i) => i,
                        None => break,
                    };
                    let f = &files[i];
                    let key = &keys[i];
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
                            fields.insert("ax_file".into(), Value::String(key.clone()));
                            fields.insert("ax_locator".into(), Value::String(rec.locator.clone()));
                            fields.insert("ax_dataset".into(), Value::String(slug.clone()));
                            fields.insert("ax_run".into(), Value::String(run_id.clone()));
                            fields.insert("ax_format".into(), Value::String(format_str(Some(&sn))));
                            let id = ids::doc_id(slug, key, &rec.locator);
                            let action = json!({"index": {"_index": rt.index, "_id": id}});
                            buf.extend_from_slice(action.to_string().as_bytes());
                            buf.push(b'\n');
                            buf.extend_from_slice(
                                serde_json::to_string(&Value::Object(fields))
                                    .unwrap_or_else(|_| "{}".into())
                                    .as_bytes(),
                            );
                            buf.push(b'\n');
                            buf_docs += 1;
                            rt.records.fetch_add(1, Ordering::Relaxed);
                            file_records += 1;
                            if buf.len() >= bulk_cut || buf_docs >= 5000 {
                                match es.bulk(std::mem::take(&mut buf)) {
                                    Ok(o) => {
                                        if o.server_errors > 0 {
                                            send_err = Some(format!(
                                                "bulk backend failed for {} item(s): {}. \
                                                 Source file was not journaled complete; fix \
                                                 the server/embedding configuration and rerun \
                                                 autoindex to resume",
                                                o.server_errors,
                                                o.first_server_error
                                                    .as_deref()
                                                    .unwrap_or("unknown server error")
                                            ));
                                            return false;
                                        }
                                        if o.item_errors > 0 {
                                            junk_records
                                                .fetch_add(o.item_errors, Ordering::Relaxed);
                                            if let Some(e) = o.first_error {
                                                let mut be = bulk_errors.lock().unwrap();
                                                if be.len() < 5 {
                                                    be.push(e);
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        send_err = Some(format!("{e:#}"));
                                        return false;
                                    }
                                }
                                buf_docs = 0;
                                buf.reserve(bulk_cut);
                            }
                            true
                        };
                        let res = extract::extract(&f.path, &sn, None, &mut sink);
                        match res {
                            Ok(stats) => {
                                file_junk += stats.junk;
                            }
                            Err(e) => {
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
                    // flush the remainder for THIS file before journaling
                    if !buf.is_empty() && send_err.is_none() {
                        match es.bulk(std::mem::take(&mut buf)) {
                            Ok(o) => {
                                if o.server_errors > 0 {
                                    send_err = Some(format!(
                                        "bulk backend failed for {} item(s): {}. Source file \
                                         was not journaled complete; fix the server/embedding \
                                         configuration and rerun autoindex to resume",
                                        o.server_errors,
                                        o.first_server_error
                                            .as_deref()
                                            .unwrap_or("unknown server error")
                                    ));
                                }
                                if o.item_errors > 0 {
                                    junk_records.fetch_add(
                                        o.item_errors.saturating_sub(o.server_errors),
                                        Ordering::Relaxed,
                                    );
                                }
                            }
                            Err(e) => send_err = Some(format!("{e:#}")),
                        }
                        buf_docs = 0;
                    }
                    if let Some(e) = send_err {
                        // endpoint trouble: record, do NOT journal file_done
                        let mut be = bulk_errors.lock().unwrap();
                        if be.len() < 5 {
                            be.push(e);
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
                    journal_mx
                        .lock()
                        .unwrap()
                        .file_done(&FileDone {
                            file_key: key.clone(),
                            path: f.rel.clone(),
                            records: file_records,
                            junk: file_junk,
                            bytes: f.size,
                        })
                        .ok();
                    let dn = files_done.fetch_add(1, Ordering::Relaxed) + 1;
                    if !cfg.quiet && (dn.is_multiple_of(200) || f.size > 5 * (1 << 20)) {
                        eprintln!("  [{dn}/{n_todo}] {} ({} records)", f.rel, file_records);
                    }
                }
            });
        }
    });

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
    es.refresh(&format!("{}-*", cfg.prefix)).ok();

    // live per-dataset counts + time ranges (every claim traces to a run)
    let mut ds_counts: HashMap<String, u64> = HashMap::new();
    let mut ds_timerange: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for d in &plan.datasets {
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
        for c in key_corrs.iter_mut() {
            correlate::confirm(&es, c, 20).ok();
        }
        // keep only live-confirmed overlaps in the report
        key_corrs.retain(|c| c.confirmed.map(|(n, _)| n > 0).unwrap_or(false));
    } else if !cfg.quiet {
        eprintln!("(resumed run: key-overlap correlations kept from the original run's catalog)");
    }

    let mut series = Vec::new();
    for d in &plan.datasets {
        if let Some(t) = &d.time_field {
            if let Ok(Some(s)) = correlate::fetch_histogram(&es, &d.slug, &d.index, t) {
                series.push(s);
            }
        }
    }
    let time_corrs = correlate::time_alignment(&series);

    // ── catalog write ────────────────────────────────────────────────────
    let mut cat_buf: Vec<u8> = Vec::new();
    let push_doc = |id: &str, doc: &Value, buf: &mut Vec<u8>| {
        let action = json!({"index": {"_index": catalog::CATALOG_INDEX, "_id": id}});
        buf.extend_from_slice(action.to_string().as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(doc.to_string().as_bytes());
        buf.push(b'\n');
    };

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
                &fd.path,
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
    let mut all_junk: Vec<&JunkFile> = plan.junk_files.iter().collect();
    all_junk.extend(extra.iter());
    all_junk.extend(new_unplanned.iter());
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
    let run_doc = json!({
        "doc_kind": "run",
        "run_id": run_id,
        "root": root_str,
        "url": cfg.url,
        "prefix": cfg.prefix,
        "started": chrono::Utc::now().to_rfc3339(),
        "files_total": files.len(),
        "files_indexed": journal_mx.lock().unwrap().done.len(),
        "files_junk": all_junk.len(),
        "records_total": total_records,
        "junk_records_total": junk_records_by_run,
        "wall_seconds": (wall * 10.0).round() / 10.0,
        "workers": cfg.workers,
        "semantic": !cfg.no_semantic,
    });
    push_doc(&format!("run:{run_id}"), &run_doc, &mut cat_buf);

    if !cat_buf.is_empty() {
        es.bulk(cat_buf).context("write catalog")?;
    }
    es.refresh(catalog::CATALOG_INDEX).ok();
    journal_mx.lock().unwrap().finish(&run_doc)?;

    // ── summary ──────────────────────────────────────────────────────────
    let junk_total_records = junk_records.load(Ordering::Relaxed);
    if cfg.json {
        println!("{run_doc}");
    } else if !cfg.quiet {
        println!("\ndone in {wall:.1}s — {} datasets, {} records live, {} junk records, {} junk/skipped files",
            plan.datasets.len(), total_records, junk_total_records, all_junk.len());
        let mut rows: Vec<(&String, u64)> = plan
            .datasets
            .iter()
            .map(|d| (&d.index, *ds_counts.get(&d.slug).unwrap_or(&0)))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        for (idx, cnt) in rows {
            println!("  {idx:<40} {cnt:>10} docs");
        }
        println!(
            "\nnext: `xerj autoindex map --url {}` for the data map; search via GET /{}-*/_search",
            cfg.url, cfg.prefix
        );
    }
    Ok(if junk_total_records > 0 || !all_junk.is_empty() {
        3
    } else {
        0
    })
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
    let junk_files = fetch(
        json!({"bool": {"must": [{"term": {"doc_kind": "file"}}],
            "must_not": [{"term": {"status": "indexed"}}]}}),
        500,
        None,
    )?;
    if cfg.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "run": runs.first(),
                "datasets": datasets,
                "correlations": correlations,
                "junk_files": junk_files,
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
                junk_files.len() as u64
            )
        );
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
                        Some("finish") => finished = true,
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
