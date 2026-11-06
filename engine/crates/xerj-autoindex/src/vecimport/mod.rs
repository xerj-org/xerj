//! `xerj autoindex <vector-db-url>`: import an existing vector collection into
//! XERJ (rc.102, lane C).
//!
//! Four sources, one pipeline:
//!
//! | source   | positional URL                                   | read API                        |
//! |----------|--------------------------------------------------|---------------------------------|
//! | Qdrant   | `qdrant://host:6333/collection` (`qdrants://`)    | `points/scroll` (REST)          |
//! | Chroma   | `chroma://host:8000/collection` (`chromas://`)    | v2 `collections/{id}/get`       |
//! | Pinecone | `pinecone://index-name[?namespace=ns]`           | `vectors/list` + `vectors/fetch`|
//! | pgvector | `postgres://…/db --pgvector-table <table>`        | keyset `SELECT … ORDER BY id`   |
//!
//! What the pipeline does, in order:
//!
//! 1. **Describe** the source: vector dimensions, distance, point count.
//! 2. **Map**: create the XERJ index with a `dense_vector` field of those
//!    dimensions and the matching similarity (see [`Similarity`]). A source
//!    distance XERJ cannot express is refused, never silently swapped.
//! 3. **Copy**: page through the source on a reader thread, write `_bulk`
//!    batches on the caller's thread. Every point keeps its source id as the
//!    XERJ `_id`, so a re-run overwrites rather than duplicates.
//! 4. **Journal**: after every acknowledged batch the source cursor is written
//!    to a small JSON journal, so an interrupted import resumes where the last
//!    acknowledged batch ended (`--fresh` starts over).
//! 5. **Verify**: for a sample of imported points, ask the source and XERJ the
//!    same nearest-neighbour question and report how often the top-1 ids
//!    agree (`--verify N`, default 100, `0` turns it off).
//!
//! Every network client is the workspace's blocking `reqwest` (rustls) or the
//! pure-Rust `postgres` crate, so the 8-target release matrix needs no C.

mod chroma;
mod cli;
mod journal;
mod pgvector;
mod pinecone;
mod qdrant;
#[cfg(test)]
mod tests;

pub use cli::{looks_like_vector_source, parse, VecImportCfg};

/// `xerj autoindex --help-vector`.
pub fn cli_help() {
    cli::print_help();
}

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::esclient::Es;

/// One point read from a source.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// The source's id, as text (Qdrant ints and UUIDs, Chroma/Pinecone
    /// strings, Postgres keys of any type all become strings).
    pub id: String,
    pub vector: Vec<f32>,
    /// Source payload / metadata / row columns, flattened one level.
    pub payload: Map<String, Value>,
    /// Chroma stores the original text next to the vector; kept as `document`.
    pub document: Option<String>,
}

/// One page and where the next one starts. `next == None` ends the copy.
#[derive(Debug, Clone)]
pub struct Page {
    pub points: Vec<Point>,
    pub next: Option<String>,
}

/// Vector similarity, in XERJ's (Elasticsearch-compatible) names.
///
/// Mapping from the sources, documented once here:
///
/// * cosine → `cosine` (all four sources).
/// * dot / inner product → `max_inner_product`. Not `dot_product`: that name
///   requires unit-length vectors and a source that stores raw inner-product
///   vectors does not promise them; `max_inner_product` ranks the same way
///   without the constraint.
/// * euclidean / L2 → `l2_norm`.
/// * Qdrant `Manhattan` has no XERJ equivalent and is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Similarity {
    Cosine,
    MaxInnerProduct,
    L2,
}

impl Similarity {
    pub fn es_name(self) -> &'static str {
        match self {
            Similarity::Cosine => "cosine",
            Similarity::MaxInnerProduct => "max_inner_product",
            Similarity::L2 => "l2_norm",
        }
    }

    /// `--similarity` values a user may type. The ES names and the common
    /// source spellings are all accepted.
    pub fn parse_user(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cosine" | "cos" => Some(Similarity::Cosine),
            "dot" | "dot_product" | "dotproduct" | "ip" | "inner_product" | "max_inner_product" => {
                Some(Similarity::MaxInnerProduct)
            }
            "l2" | "l2_norm" | "euclid" | "euclidean" => Some(Similarity::L2),
            _ => None,
        }
    }
}

/// What the source says about itself before any point is read.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub kind: &'static str,
    pub name: String,
    pub dims: usize,
    pub similarity: Similarity,
    /// Total points, when the source can count cheaply. Drives the ETA.
    pub count: Option<u64>,
    /// Where the similarity came from ("collection config", "index opclass",
    /// "default: no index found", …), printed so a guess is visible as one.
    pub similarity_basis: String,
}

/// A readable vector source.
pub trait Source: Send {
    fn describe(&mut self) -> Result<SourceInfo>;
    /// Read up to `limit` points starting at `cursor` (`None` = the start).
    fn page(&mut self, cursor: Option<&str>, limit: usize) -> Result<Page>;
    /// The source's own nearest neighbour for `query`: the id it ranks first.
    fn top1(&mut self, query: &[f32]) -> Result<Option<String>>;
}

/// Build the source a parsed config names.
pub fn open_source(cfg: &VecImportCfg) -> Result<Box<dyn Source>> {
    Ok(match &cfg.source {
        cli::SourceSpec::Qdrant { base, collection } => Box::new(qdrant::Qdrant::new(
            base,
            collection,
            cfg.source_api_key.clone(),
            cfg.vector_name.clone(),
        )?),
        cli::SourceSpec::Chroma {
            base,
            collection,
            tenant,
            database,
        } => Box::new(chroma::Chroma::new(
            base,
            collection,
            tenant,
            database,
            cfg.source_api_key.clone(),
        )?),
        cli::SourceSpec::Pinecone { index, namespace } => Box::new(pinecone::Pinecone::new(
            index,
            namespace.clone(),
            cfg.source_api_key.clone(),
            cfg.pinecone_host.clone(),
            cfg.pinecone_controller.clone(),
        )?),
        cli::SourceSpec::PgVector { conn, table } => Box::new(pgvector::PgVector::new(
            conn,
            table,
            cfg.id_column.clone(),
            cfg.vector_column.clone(),
        )?),
    })
}

/// The index body for a source: one `dense_vector` field plus a text field for
/// Chroma documents. Payload fields are left to dynamic mapping, exactly as a
/// `_bulk` of the same JSON would get them.
pub fn index_body(vector_field: &str, dims: usize, sim: Similarity) -> Value {
    json!({
        "mappings": {
            "properties": {
                vector_field: {
                    "type": "dense_vector",
                    "dims": dims,
                    "index": true,
                    "similarity": sim.es_name()
                },
                "document": { "type": "text" }
            }
        }
    })
}

/// The XERJ document for one point. Payload keys are kept at the top level so
/// they filter like any other field; a key that would collide with the vector
/// field or `document` is renamed `payload_<key>` rather than dropped.
pub fn point_doc(p: &Point, vector_field: &str) -> Value {
    let mut doc = Map::new();
    for (k, v) in &p.payload {
        let key = if k == vector_field || k == "document" {
            format!("payload_{k}")
        } else {
            k.clone()
        };
        doc.insert(key, v.clone());
    }
    if let Some(d) = &p.document {
        doc.insert("document".into(), Value::String(d.clone()));
    }
    doc.insert(
        vector_field.to_string(),
        Value::Array(p.vector.iter().map(|x| json!(x)).collect()),
    );
    Value::Object(doc)
}

/// NDJSON `_bulk` body for a batch.
pub fn bulk_body(index: &str, points: &[Point], vector_field: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * (64 + 8 * 16));
    for p in points {
        let action = json!({ "index": { "_index": index, "_id": p.id } });
        serde_json::to_writer(&mut out, &action).expect("json");
        out.push(b'\n');
        serde_json::to_writer(&mut out, &point_doc(p, vector_field)).expect("json");
        out.push(b'\n');
    }
    out
}

/// Reservoir sample of points kept for verification (Algorithm R, seeded so a
/// run is reproducible).
struct Reservoir {
    cap: usize,
    seen: u64,
    items: Vec<(String, Vec<f32>)>,
    rng: u64,
}

impl Reservoir {
    fn new(cap: usize) -> Self {
        Reservoir {
            cap,
            seen: 0,
            items: Vec::new(),
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }
    fn next_u64(&mut self) -> u64 {
        // xorshift64*: deterministic, dependency-free.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn offer(&mut self, p: &Point) {
        if self.cap == 0 {
            return;
        }
        self.seen += 1;
        if self.items.len() < self.cap {
            self.items.push((p.id.clone(), p.vector.clone()));
        } else {
            let j = self.next_u64() % self.seen;
            if (j as usize) < self.cap {
                self.items[j as usize] = (p.id.clone(), p.vector.clone());
            }
        }
    }
}

/// A query near `v` but not equal to it: each component moved by up to 5% of
/// the vector's RMS magnitude. Asking for `v` itself would make agreement
/// trivially 100% (both sides return the point), which tests copying but not
/// ranking; the perturbed query tests that both engines rank the neighbourhood
/// the same way.
pub fn perturb(v: &[f32], seed: u64) -> Vec<f32> {
    let n = v.len().max(1) as f32;
    let rms = (v.iter().map(|x| x * x).sum::<f32>() / n).sqrt().max(1e-6);
    let mut s = seed | 1;
    v.iter()
        .map(|x| {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            let u = (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f32 / (1u64 << 53) as f32;
            x + (u * 2.0 - 1.0) * 0.05 * rms
        })
        .collect()
}

/// Shared progress of the parallel writers (see `run`).
struct Watermark {
    next_seq: u64,
    done: BTreeMap<u64, (u64, Option<String>)>,
    state: journal::State,
    imported: u64,
    new_points: u64,
    refused: u64,
    failed: Option<String>,
    last_note: Instant,
}

impl Watermark {
    /// Fold every contiguous acknowledged page into the journal.
    fn advance(&mut self, jpath: &std::path::Path) -> Result<()> {
        let mut moved = false;
        while let Some((n, next)) = self.done.remove(&self.next_seq) {
            self.next_seq += 1;
            self.imported += n;
            self.new_points += n;
            self.state.imported = self.imported;
            self.state.cursor = next;
            moved = true;
        }
        if moved {
            journal::save(jpath, &self.state)?;
        }
        Ok(())
    }

    fn progress(&mut self, started: Instant, count: Option<u64>, every: u64) {
        if self.last_note.elapsed() < Duration::from_secs(every) {
            return;
        }
        self.last_note = Instant::now();
        let el = started.elapsed().as_secs_f64().max(1e-3);
        let rate = self.new_points as f64 / el;
        let (pct, eta) = match count {
            Some(c) if c > 0 => (
                format!(" ({:.1}%)", 100.0 * self.imported as f64 / c as f64),
                format!(
                    " · ETA {}",
                    fmt_eta(c.saturating_sub(self.imported) as f64 / rate.max(1e-9))
                ),
            ),
            _ => (String::new(), String::new()),
        };
        eprintln!(
            "progress: {}{} imported · {:.0} points/s{}",
            self.imported, pct, rate, eta
        );
    }
}

/// Result of a finished import, printed and returned for tests.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub imported: u64,
    pub seconds: f64,
    pub verified: usize,
    pub self_hits: usize,
    pub agree: usize,
    pub refused: u64,
    /// (source count before the copy, imported) when they differ.
    pub count_mismatch: Option<(u64, u64)>,
}

fn fmt_eta(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "?".into();
    }
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Entry point once the CLI said "this positional is a vector source".
pub fn run_cli(args: Vec<String>) -> i32 {
    let cfg = match cli::parse(args) {
        Ok(Some(c)) => c,
        Ok(None) => {
            cli::print_help();
            return 0;
        }
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("run `xerj autoindex --help-vector` for vector-source usage.");
            return 2;
        }
    };
    match run(&cfg) {
        Ok(r) => {
            if cfg.dry_run {
                return 0;
            }
            if r.refused > 0 || r.count_mismatch.is_some() {
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

/// The import itself. Public for the integration tests.
pub fn run(cfg: &VecImportCfg) -> Result<Report> {
    let mut src = open_source(cfg)?;
    let mut info = src
        .describe()
        .context("reading the source's collection description")?;
    if let Some(s) = cfg.similarity {
        info.similarity = s;
        info.similarity_basis = "--similarity".into();
    }
    let index = cfg
        .index
        .clone()
        .unwrap_or_else(|| cli::default_index_name(&info.name));
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "source: {} `{}` · {} dims · similarity {} ({}) · {} points",
        info.kind,
        info.name,
        info.dims,
        info.similarity.es_name(),
        info.similarity_basis,
        info.count
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    let _ = writeln!(
        err,
        "target: {} index `{}`, vector field `{}`",
        cfg.url, index, cfg.vector_field
    );

    if cfg.dry_run {
        let n = cfg.limit.unwrap_or(5).max(1);
        let page = src.page(None, n.min(1000))?;
        let body = index_body(&cfg.vector_field, info.dims, info.similarity);
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run": true,
                "source": { "kind": info.kind, "name": info.name, "dims": info.dims,
                            "similarity": info.similarity.es_name(),
                            "similarity_basis": info.similarity_basis, "count": info.count },
                "index": index,
                "index_body": body,
                "sample_documents": page.points.iter().take(3)
                    .map(|p| json!({"_id": p.id, "_source": point_doc(p, &cfg.vector_field)}))
                    .collect::<Vec<_>>(),
                "points_read": page.points.len(),
            }))?
        );
        for p in &page.points {
            if p.vector.len() != info.dims {
                bail!(
                    "point `{}` has {} dims, the collection says {}",
                    p.id,
                    p.vector.len(),
                    info.dims
                );
            }
        }
        let _ = writeln!(
            err,
            "dry run: read {} point(s), wrote nothing",
            page.points.len()
        );
        return Ok(Report::default());
    }

    let es = Es::new(&cfg.url, cfg.api_key.clone())?;
    es.ping()
        .with_context(|| format!("XERJ is not answering at {}", cfg.url))?;
    es.ensure_index(
        &index,
        &index_body(&cfg.vector_field, info.dims, info.similarity),
    )?;

    let jpath = journal::path_for(cfg, &index);
    let state = if cfg.fresh {
        journal::State::fresh(&cfg.source_display(), &index)
    } else {
        journal::load(&jpath)?
            .unwrap_or_else(|| journal::State::fresh(&cfg.source_display(), &index))
    };
    if state.imported > 0 {
        let _ = writeln!(
            err,
            "resuming: {} point(s) already imported (journal {})",
            state.imported,
            jpath.display()
        );
    }

    // Reader thread: pages the source ahead of the writer, two pages deep.
    let (tx, rx) = mpsc::sync_channel::<Result<Page>>(2);
    let batch = cfg.batch;
    let limit = cfg.limit;
    let start_cursor = state.cursor.clone();
    let already = state.imported;
    let reader = std::thread::spawn(move || {
        let mut cursor = start_cursor;
        let mut read: u64 = already;
        loop {
            let want = match limit {
                Some(l) if read >= l as u64 => break,
                Some(l) => batch.min(l - read as usize),
                None => batch,
            };
            let page = src.page(cursor.as_deref(), want);
            let stop = match &page {
                Ok(p) => {
                    read += p.points.len() as u64;
                    cursor = p.next.clone();
                    p.next.is_none() || p.points.is_empty()
                }
                Err(_) => true,
            };
            if tx.send(page).is_err() || stop {
                break;
            }
        }
        src
    });

    let started = Instant::now();
    let mut res = Reservoir::new(cfg.verify);
    // Parallel `_bulk` writers. Pages complete out of order, so the journal
    // advances only over a contiguous prefix of acknowledged pages (the
    // watermark): a crash can re-send at most the in-flight pages, never skip
    // one, and re-sending is harmless because ids are kept.
    let wm = std::sync::Arc::new(std::sync::Mutex::new(Watermark {
        next_seq: 0,
        done: BTreeMap::new(),
        state: state.clone(),
        imported: state.imported,
        new_points: 0,
        refused: 0,
        failed: None,
        last_note: Instant::now(),
    }));
    let (work_tx, work_rx) =
        mpsc::sync_channel::<(u64, Vec<u8>, usize, Option<String>)>(cfg.workers * 2);
    let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));
    let mut writers = Vec::new();
    for _ in 0..cfg.workers {
        let es = es.clone();
        let work_rx = work_rx.clone();
        let wm = wm.clone();
        let jpath = jpath.clone();
        let count = info.count;
        let progress_secs = cfg.progress_secs;
        writers.push(std::thread::spawn(move || loop {
            let job = { work_rx.lock().unwrap().recv() };
            let Ok((seq, body, n, next)) = job else { break };
            if wm.lock().unwrap().failed.is_some() {
                break;
            }
            let outcome = es.bulk(body);
            let mut w = wm.lock().unwrap();
            match outcome {
                Err(e) => {
                    w.failed.get_or_insert(format!("{e:#}"));
                    break;
                }
                Ok(o) if o.server_errors > 0 => {
                    w.failed.get_or_insert(format!(
                        "XERJ refused {} item(s) for a server-side reason ({}); the journal points \
                         at the last contiguous acknowledged batch, re-run to resume",
                        o.server_errors,
                        o.first_server_error.unwrap_or_default()
                    ));
                    break;
                }
                Ok(o) => {
                    if o.item_errors > 0 {
                        eprintln!(
                            "warning: {} point(s) refused in a batch: {}",
                            o.item_errors,
                            o.first_error.unwrap_or_default()
                        );
                    }
                    w.refused += o.item_errors;
                    w.done.insert(seq, (n as u64, next));
                    if let Err(e) = w.advance(&jpath) {
                        w.failed.get_or_insert(format!("{e:#}"));
                        break;
                    }
                    w.progress(started, count, progress_secs);
                }
            }
        }));
    }
    let mut seq = 0u64;
    let mut read_err: Option<anyhow::Error> = None;
    for page in rx {
        let page = match page {
            Ok(p) => p,
            Err(e) => {
                read_err = Some(e.context("reading the source"));
                break;
            }
        };
        if page.points.is_empty() {
            break;
        }
        if let Some(p) = page.points.iter().find(|p| p.vector.len() != info.dims) {
            read_err = Some(anyhow!(
                "point `{}` has {} dims but the index was created with {}; nothing after the \
                 last acknowledged batch was journaled",
                p.id,
                p.vector.len(),
                info.dims
            ));
            break;
        }
        for p in &page.points {
            res.offer(p);
        }
        if wm.lock().unwrap().failed.is_some() {
            break;
        }
        let body = bulk_body(&index, &page.points, &cfg.vector_field);
        if work_tx
            .send((seq, body, page.points.len(), page.next.clone()))
            .is_err()
        {
            break;
        }
        seq += 1;
    }
    drop(work_tx);
    for w in writers {
        let _ = w.join();
    }
    let (failed, imported, new_points, refused) = {
        let w = wm.lock().unwrap();
        (w.failed.clone(), w.imported, w.new_points, w.refused)
    };
    if let Some(e) = read_err {
        return Err(e);
    }
    if let Some(f) = failed {
        bail!("{f}");
    }
    let mut report = Report {
        imported,
        refused,
        ..Default::default()
    };
    // The source's own count is the cross-check that paging covered the whole
    // collection. A mismatch is reported, not hidden: a source written to
    // during the copy legitimately differs, a paging bug must not pass quietly.
    if let (Some(c), None) = (info.count, cfg.limit) {
        if c != imported {
            report.count_mismatch = Some((c, imported));
            let _ = writeln!(
                err,
                "warning: the source reported {c} point(s) before the copy and {imported} were \
                 imported; if nothing wrote to the source meanwhile, re-run with --fresh and \
                 report it"
            );
        }
    }
    let mut src = reader
        .join()
        .map_err(|_| anyhow!("source reader thread panicked"))?;
    report.seconds = started.elapsed().as_secs_f64();
    journal::finish(&jpath)?;
    let _ = writeln!(
        err,
        "imported {} point(s) in {:.1}s ({:.0} points/s){}",
        report.imported,
        report.seconds,
        new_points as f64 / report.seconds.max(1e-3),
        if report.refused > 0 {
            format!(", {} refused", report.refused)
        } else {
            String::new()
        }
    );

    if cfg.verify > 0 && !res.items.is_empty() {
        es.refresh(&index)?;
        let mut seed = 0xD1B5_4A32_D192_ED03u64;
        let mut misses: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
        for (id, v) in &res.items {
            report.verified += 1;
            if xerj_top1(&es, &index, &cfg.vector_field, v)?.as_deref() == Some(id.as_str()) {
                report.self_hits += 1;
            }
            seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let q = perturb(v, seed);
            let ours = xerj_top1(&es, &index, &cfg.vector_field, &q)?;
            let theirs = src.top1(&q)?;
            if ours.is_some() && ours == theirs {
                report.agree += 1;
            } else if misses.len() < 5 {
                misses.insert(id.clone(), (ours, theirs));
            }
        }
        let _ =
            writeln!(
            err,
            "verify: {}/{} sampled points are their own XERJ top-1; perturbed-query top-1 agrees \
             with {} on {}/{} ({:.1}%)",
            report.self_hits, report.verified, info.kind, report.agree, report.verified,
            100.0 * report.agree as f64 / report.verified as f64
        );
        for (id, (o, t)) in misses {
            let _ = writeln!(err, "  near {id}: xerj {:?} vs {} {:?}", o, info.kind, t);
        }
    }
    println!(
        "{}",
        json!({
            "index": index, "imported": report.imported, "refused": report.refused,
            "source_count": info.count,
            "seconds": (report.seconds * 10.0).round() / 10.0,
            "verify": { "sampled": report.verified, "self_top1": report.self_hits,
                        "perturbed_top1_agree": report.agree }
        })
    );
    Ok(report)
}

/// XERJ's top-1 id for `q` via the ES-compatible `knn` search.
fn xerj_top1(es: &Es, index: &str, field: &str, q: &[f32]) -> Result<Option<String>> {
    let body = json!({
        "knn": { "field": field, "query_vector": q, "k": 1, "num_candidates": 100 },
        "size": 1,
        "_source": false
    });
    let v = es.search(index, &body)?;
    Ok(v.pointer("/hits/hits/0/_id")
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// Parse a JSON number array into `f32`s.
pub(crate) fn f32s(v: &Value) -> Option<Vec<f32>> {
    v.as_array()?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32))
        .collect()
}

/// Text id for a JSON id (number or string).
pub(crate) fn id_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
