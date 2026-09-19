//! Object storage as an autoindex source: `xerj autoindex s3://bucket/prefix`.
//!
//! # What this does, and what it deliberately does not
//!
//! The listing, the change detection and the byte transfer live here. The
//! discovery pipeline — sniffing, the 34 tree-sitter extractors, CSV/JSON/PDF/
//! DOCX/mbox handling, planning, the resume journal, reconciliation — is
//! untouched and sees exactly what it sees for a folder: local files. An object
//! source *materialises* the objects it needs into a mirror directory under the
//! state dir and then the ordinary walk runs over that mirror.
//!
//! That is a real design decision, not a shortcut, and it has costs worth
//! stating plainly:
//!
//! - **Why not pipe the stream straight into the extractors.** Several of them
//!   are not sequential readers. A PDF is read from its trailer backwards, a
//!   DOCX is a ZIP whose central directory is at the END of the file, SQLite
//!   seeks by page, and the content-identity contract reads every file twice
//!   (hash, then extract) plus a third time on verify. Over a network stream
//!   each of those becomes either a re-GET — more requests, more money — or an
//!   in-memory copy of the whole object, which is the thing we must not do.
//! - **The cost is local disk.** The mirror holds the bytes of every object the
//!   run admits, so `autoindex s3://…` needs as much free disk as the prefix
//!   it indexes. The path is printed on every run, and `--state-dir` moves it.
//! - **The win is that a second run is nearly free.** An object whose ETag and
//!   size match what the manifest recorded is not downloaded at all.
//!
//! The INDEX itself still lives on local disk, in the XERJ node `--url` points
//! at. Nothing in this module puts an index in a bucket; that is separate work.
//!
//! # Requests, and why the count is printed
//!
//! Object stores bill per request, and R2's free tier is 1,000,000 class-A
//! (LIST/PUT) and 10,000,000 class-B (GET/HEAD) operations a month. One run over
//! a prefix costs `ceil(objects / 1000)` class-A LIST requests plus one class-B
//! GET per changed object — it never writes, so it never spends a class-A
//! operation on anything but listing. Because the dangerous shape is a
//! *schedule* rather than a single run, [`MaterializeReport::cost_lines`] prints
//! what this run cost and what re-running it hourly or every five minutes would
//! cost against that allowance. See `docs/autoindex-object-storage.md`.
//!
//! Reference: the SDK wiring (endpoint override, path-style addressing, region
//! resolution, streaming a GET body to a writer) follows quickwit's
//! `quickwit/quickwit-storage/src/object_storage/s3_compatible_storage.rs:138-175`
//! and `:919-925` (Apache-2.0) — adapted, not copied: quickwit is async
//! throughout and never enumerates a bucket, so pagination, the blocking
//! adapter and the mirror are ours.

use crate::cli::IndexCfg;
use crate::progress::Progress;
use crate::source::{
    classify_key, rules, DocSource, KeyVerdict, ObjectScheme, ObjectSpec, SourceEntry,
    SourceListing, SourceOps, SourceSpec,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Keys per LIST request. 1000 is the protocol maximum and therefore the
/// cheapest: asking for fewer multiplies the class-A cost of every run.
const LIST_PAGE_KEYS: i32 = 1000;

/// Refuse to keep listing past this many pages (10 million keys).
///
/// Not a performance guard — a money and sanity guard. A mistyped prefix that
/// resolves to a bucket with tens of millions of objects would otherwise spend
/// tens of thousands of class-A requests and fill the disk before anybody looked
/// at the output. Ten thousand pages is already 1% of R2's monthly free
/// allowance in a single run, which is the point at which an operator should be
/// asked rather than billed.
const MAX_LIST_PAGES: u64 = 10_000;

/// Copy buffer per in-flight download. Bounded on purpose: this constant times
/// the fetch concurrency is the whole memory cost of the transfer phase,
/// whatever the objects weigh.
const COPY_BUFFER_BYTES: usize = 256 * 1024;

/// Class-A free allowance a month on Cloudflare R2, used only to turn the
/// request count into a fraction an operator can act on.
const CLASS_A_FREE_PER_MONTH: u64 = 1_000_000;

/// An object the store listed and then did not have. Distinguished because it
/// means "deleted between the LIST and the GET", which is a normal race on a
/// live bucket and must not fail a run.
#[derive(Debug)]
pub struct MissingObject {
    pub key: String,
}

impl std::fmt::Display for MissingObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "object {} is gone from the store", self.key)
    }
}

impl std::error::Error for MissingObject {}

/// Change identity for one object.
///
/// **What we use, and what we refuse to assume.** The token is the ETag
/// exactly as the store returned it, plus the size. We treat the ETag as an
/// OPAQUE string: a value that changes when the object changes. We never
/// compare it to a locally computed MD5, and a multipart ETag (the `-N` suffix
/// form, e.g. `"a1b2…-7"`) is used exactly like any other. That is the whole
/// reason incremental runs are cheap — the alternative is downloading every
/// object on every run to hash it, which is the cost this feature exists to
/// avoid.
///
/// The size is included because an ETag is only promised to be unique per
/// object *within a store's own scheme*; a store that recycles or truncates
/// ETags still cannot produce the same (etag, size) pair for different bytes in
/// practice, and including the size costs nothing.
///
/// When the store reports no ETag at all, last-modified plus size is used and
/// the caller is told, because that pair can miss a same-size rewrite inside
/// the timestamp's resolution. When neither is available the object is always
/// re-downloaded, which is correct and expensive rather than cheap and wrong.
pub fn change_token(etag: Option<&str>, size: u64, last_modified: Option<&str>) -> Option<String> {
    match (etag, last_modified) {
        (Some(etag), _) if !etag.trim().is_empty() => {
            Some(format!("etag:{}|size:{size}", etag.trim()))
        }
        (_, Some(modified)) if !modified.trim().is_empty() => {
            Some(format!("lastmod:{}|size:{size}", modified.trim()))
        }
        _ => None,
    }
}

/// What the run recorded about one object, so the next run can skip it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    /// [`change_token`] at the time the bytes were fetched.
    pub change_token: String,
    pub size: u64,
    /// The raw ETag, kept for the operator's benefit (so `-N` multipart tags are
    /// visible in the state file) and never parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// When this object's bytes were last fetched, RFC 3339.
    pub fetched: String,
}

/// The per-source sidecar that makes an unchanged re-run free.
///
/// It sits beside `journal.ndjson` in the state directory rather than inside it:
/// the journal is an append-only record of *index* operations with replay
/// invariants of its own, and this is a cache index for *transfer* decisions.
/// Losing it is never a correctness problem — the next run re-downloads and
/// re-records — so it is written atomically and read best-effort.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectManifest {
    pub version: u32,
    /// [`ObjectSpec::identity`] of the source these records describe.
    pub source: String,
    /// Root-relative path → what was fetched.
    pub objects: BTreeMap<String, ObjectRecord>,
}

pub const MANIFEST_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "object-source.json";

impl ObjectManifest {
    fn empty(source: &str) -> Self {
        Self {
            version: MANIFEST_VERSION,
            source: source.to_string(),
            objects: BTreeMap::new(),
        }
    }

    /// Read the manifest for `source`, or an empty one.
    ///
    /// A manifest for a different source, an unreadable file or an unknown
    /// version all yield "empty" plus a reason for the caller to print: the
    /// consequence is a full re-fetch, which must be explained rather than
    /// silently paid for.
    pub fn load(path: &Path, source: &str) -> (Self, Option<String>) {
        let raw = match std::fs::read(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Self::empty(source), None)
            }
            Err(e) => {
                return (
                    Self::empty(source),
                    Some(format!(
                        "could not read {} ({e}); every object will be fetched again",
                        path.display()
                    )),
                )
            }
        };
        match serde_json::from_slice::<Self>(&raw) {
            Ok(manifest) if manifest.version != MANIFEST_VERSION => (
                Self::empty(source),
                Some(format!(
                    "{} was written by a different format version ({}); every object will be \
                     fetched again",
                    path.display(),
                    manifest.version
                )),
            ),
            Ok(manifest) if manifest.source != source => (
                Self::empty(source),
                Some(format!(
                    "{} was last used for {}; every object under {source} will be fetched again",
                    path.display(),
                    manifest.source
                )),
            ),
            Ok(manifest) => (manifest, None),
            Err(e) => (
                Self::empty(source),
                Some(format!(
                    "could not parse {} ({e}); every object will be fetched again",
                    path.display()
                )),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        xerj_common::fsio::write_file_durable(path, &bytes)
            .with_context(|| format!("write object manifest {}", path.display()))
    }
}

/// Everything about an object-store run that is decided before any state is
/// opened: where its bytes land locally, and what the journal knows it as.
#[derive(Debug, Clone)]
pub struct ObjectRun {
    pub spec: ObjectSpec,
    /// [`ObjectSpec::identity`] — the run's root identity, in place of a path.
    pub identity: String,
    /// Local mirror the rest of the run walks.
    pub mirror: PathBuf,
    pub manifest_path: PathBuf,
}

/// Turn an `s3://`/`r2://` positional argument into a local mirror the ordinary
/// run can walk, or return `None` for a folder (the unchanged path).
///
/// Mutates `cfg.root` to the mirror directory. The mirror is named after the
/// bucket and prefix, which is also what `derive_brain_name` reads, so the
/// default brain for `s3://acme-docs/handbook` is `acme-docs-handbook` rather
/// than the name of a cache directory.
pub fn prepare(cfg: &mut IndexCfg) -> Result<Option<ObjectRun>> {
    let spec = match crate::source::parse_source(&cfg.root, cfg.endpoint_url.as_deref())? {
        SourceSpec::LocalDir(path) => {
            // Accepted-and-ignored is the failure shape this repo refuses on
            // purpose (#204): an operator who passes an endpoint has an object
            // store in mind, and running a folder scan instead is not what they
            // asked for.
            if cfg.endpoint_url.is_some() {
                bail!(
                    "--endpoint-url applies only to an s3:// or r2:// source, and {} is a folder. \
                     Drop the flag, or point the run at s3://<bucket>/<prefix>",
                    path.display()
                );
            }
            return Ok(None);
        }
        SourceSpec::Object(spec) => spec,
    };
    if spec.scheme == ObjectScheme::R2 && spec.endpoint.is_none() {
        bail!(
            "r2://{} needs Cloudflare's account endpoint, which has no default: pass \
             --endpoint-url https://<account-id>.r2.cloudflarestorage.com (or set \
             AWS_ENDPOINT_URL_S3). The account id is on the R2 page of the Cloudflare dashboard",
            spec.bucket
        );
    }
    let identity = spec.identity();
    let state_dir = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| crate::state::default_state_dir(&identity, &cfg.url, &cfg.prefix));
    let mirror = state_dir.join("object-cache").join(spec.slug());
    std::fs::create_dir_all(&mirror)
        .with_context(|| format!("create object mirror {}", mirror.display()))?;
    cfg.root = mirror.clone();
    Ok(Some(ObjectRun {
        spec,
        identity,
        mirror,
        manifest_path: state_dir.join(MANIFEST_FILE),
    }))
}

/// What one materialisation did, in the numbers an operator and a reviewer both
/// need: what was listed, what was actually transferred, and what it cost.
#[derive(Debug, Clone, Default)]
pub struct MaterializeReport {
    pub objects_listed: u64,
    pub list_requests: u64,
    pub admitted: u64,
    pub skipped: BTreeMap<String, u64>,
    pub downloaded: u64,
    pub bytes_downloaded: u64,
    pub unchanged: u64,
    pub vanished: u64,
    pub removed: u64,
    pub read_requests: u64,
    /// Objects with no ETag, which fall back to last-modified + size.
    pub without_etag: u64,
    /// Objects whose bytes are NOT already in the mirror. Equal to
    /// `downloaded + vanished` after a [`MaterializeMode::Fetch`] pass, and the
    /// whole point of a [`MaterializeMode::PlanOnly`] one.
    pub pending: u64,
    pub pending_bytes: u64,
    pub elapsed_ms: u64,
}

/// Whether a materialisation may spend money.
///
/// `--dry-run` must not download: a preview that transfers gigabytes and bills
/// for the GETs is not a preview. So a dry run over an object source LISTS
/// (which it cannot avoid — the listing is the inventory) and then reports what
/// it would transfer. When the mirror is already up to date there is nothing to
/// transfer, and the ordinary dry-run projection runs over the mirror for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeMode {
    Fetch,
    PlanOnly,
}

impl MaterializeReport {
    /// One line per fact, for the progress surface.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "object source: {} object(s) listed, {} admitted, {} downloaded ({} MB), {} unchanged \
             (not downloaded), {} removed locally, {} ms",
            self.objects_listed,
            self.admitted,
            self.downloaded,
            self.bytes_downloaded / (1 << 20),
            self.unchanged,
            self.removed,
            self.elapsed_ms
        )];
        for (rule, count) in &self.skipped {
            lines.push(format!("object source: {count} key(s) skipped by {rule}"));
        }
        if self.vanished > 0 {
            lines.push(format!(
                "object source: {} object(s) disappeared between LIST and GET and were treated as \
                 deleted",
                self.vanished
            ));
        }
        if self.without_etag > 0 {
            lines.push(format!(
                "object source: {} object(s) had no ETag; change detection for those falls back to \
                 last-modified + size, which can miss a same-size rewrite inside the timestamp's \
                 resolution",
                self.without_etag
            ));
        }
        lines
    }

    /// The cost of this run, and of running it on a schedule.
    ///
    /// Printed on every object-store run because the number that matters is not
    /// this run's — it is what a cron entry would spend. Class-A requests are
    /// the scarce ones (R2 gives 1,000,000 a month free), and listing is the
    /// only class-A operation an autoindex run performs.
    pub fn cost_lines(&self) -> Vec<String> {
        let class_a = self.list_requests;
        let hourly = class_a.saturating_mul(720);
        let five_min = class_a.saturating_mul(8_640);
        let daily = class_a.saturating_mul(30);
        let pct = |n: u64| format!("{:.1}%", (n as f64) * 100.0 / (CLASS_A_FREE_PER_MONTH as f64));
        vec![
            format!(
                "object store requests this run: {class_a} LIST (class A) + {} GET (class B), {} MB \
                 transferred",
                self.read_requests,
                self.bytes_downloaded / (1 << 20)
            ),
            format!(
                "re-running this at the same size costs {class_a} class-A request(s) each time: \
                 {daily}/month daily ({}), {hourly}/month hourly ({}), {five_min}/month every 5 \
                 minutes ({}) of a 1,000,000/month free allowance",
                pct(daily),
                pct(hourly),
                pct(five_min)
            ),
        ]
    }
}

/// Bounded concurrency for the transfer phase.
///
/// Network transfer is not CPU work, so it does not take the scan pool's width:
/// more sockets do not need more cores, and 16 in flight already saturates an
/// ordinary link while costing `16 × COPY_BUFFER_BYTES` of memory.
pub fn fetch_concurrency(scan_workers: usize) -> usize {
    scan_workers.clamp(1, 16)
}

/// Connect, list, fetch what changed, drop what disappeared.
pub fn materialize(
    run: &ObjectRun,
    pr: &Progress,
    scan_workers: usize,
    mode: MaterializeMode,
) -> Result<MaterializeReport> {
    let source = ObjectStoreSource::connect(&run.spec)?;
    materialize_from(run, &source, pr, scan_workers, mode)
}

/// [`materialize`] against any [`DocSource`], so the cache contract can be
/// tested without a network.
pub fn materialize_from(
    run: &ObjectRun,
    source: &dyn DocSource,
    pr: &Progress,
    scan_workers: usize,
    mode: MaterializeMode,
) -> Result<MaterializeReport> {
    let started = std::time::Instant::now();
    let mut report = MaterializeReport::default();
    let (mut manifest, reset_reason) = ObjectManifest::load(&run.manifest_path, &run.identity);
    if let Some(reason) = reset_reason {
        pr.warn(&format!("object source: {reason}"));
    }

    pr.phase("list", 0, 0);
    let listing = source.list()?;
    report.objects_listed = listing.seen;
    for (rule, count) in &listing.skipped {
        *report.skipped.entry(rule.clone()).or_default() += count;
    }

    // Two keys that differ only in case are one file on macOS and Windows, so
    // each run would overwrite the other's bytes and the index would flip
    // between them. Decided here, deterministically (first in sorted order
    // wins), rather than discovered as a corrupted mirror.
    let mut entries = listing.entries;
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut seen_lowercase: BTreeMap<String, String> = BTreeMap::new();
    let mut admitted: Vec<SourceEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let folded = entry.rel.to_lowercase();
        match seen_lowercase.get(&folded) {
            Some(first) => {
                *report
                    .skipped
                    .entry(rules::CASE_COLLISION.to_string())
                    .or_default() += 1;
                pr.warn(&format!(
                    "object source: {} differs from {} only in case and would be the same file on \
                     a case-insensitive filesystem; skipping it",
                    crate::progress::sanitize(&entry.rel, crate::progress::SAFE_PATH_MAX),
                    crate::progress::sanitize(first, crate::progress::SAFE_PATH_MAX)
                ));
            }
            None => {
                seen_lowercase.insert(folded, entry.rel.clone());
                if entry.change_token.is_none() {
                    report.without_etag += 1;
                }
                admitted.push(entry);
            }
        }
    }
    report.admitted = admitted.len() as u64;

    // Decide, per object, whether its bytes are already on disk. Two conditions,
    // both required: the store says the object has not changed, and the mirror
    // file is present at the size the store reports. The second is what makes a
    // half-finished earlier run self-heal instead of indexing a truncated file.
    let mut fetch: Vec<&SourceEntry> = Vec::new();
    let mut fetch_bytes = 0u64;
    for entry in &admitted {
        let recorded = manifest.objects.get(&entry.rel);
        let unchanged = match (recorded, entry.change_token.as_deref()) {
            (Some(recorded), Some(token)) => {
                recorded.change_token == token && mirror_file_ready(&run.mirror, &entry.rel, entry.size)
            }
            _ => false,
        };
        if unchanged {
            report.unchanged += 1;
        } else {
            fetch_bytes += entry.size;
            fetch.push(entry);
        }
    }

    report.pending = fetch.len() as u64;
    report.pending_bytes = fetch_bytes;
    if mode == MaterializeMode::PlanOnly {
        // Nothing is written: not the mirror, not the manifest. The LIST
        // requests are already spent and are reported.
        let ops = source.ops();
        report.list_requests = ops.list_requests;
        report.read_requests = ops.read_requests;
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(report);
    }
    if !fetch.is_empty() {
        pr.note(&format!(
            "object source: fetching {} object(s), {} MB into {}",
            fetch.len(),
            fetch_bytes / (1 << 20),
            run.mirror.display()
        ));
    }
    pr.phase("fetch", fetch.len() as u64, fetch_bytes);
    let fetched = fetch_objects(run, source, &fetch, pr, fetch_concurrency(scan_workers))?;
    let mut vanished: BTreeSet<&str> = BTreeSet::new();
    for outcome in &fetched {
        match outcome {
            FetchOutcome::Fetched { rel, bytes } => {
                report.downloaded += 1;
                report.bytes_downloaded += bytes;
                let entry = admitted
                    .iter()
                    .find(|e| &e.rel == rel)
                    .context("fetched an object that was not admitted")?;
                manifest.objects.insert(
                    rel.clone(),
                    ObjectRecord {
                        change_token: entry.change_token.clone().unwrap_or_else(|| {
                            // No token at all: record one that can never match,
                            // so the object is fetched again next run rather
                            // than assumed fresh.
                            format!("unversioned:{}", chrono::Utc::now().to_rfc3339())
                        }),
                        size: entry.size,
                        etag: entry
                            .change_token
                            .as_deref()
                            .and_then(|t| t.strip_prefix("etag:"))
                            .and_then(|t| t.split("|size:").next())
                            .map(str::to_string),
                        last_modified: entry.last_modified.clone(),
                        fetched: chrono::Utc::now().to_rfc3339(),
                    },
                );
            }
            FetchOutcome::Vanished { rel } => {
                report.vanished += 1;
                vanished.insert(rel.as_str());
            }
        }
    }

    // Reconcile the mirror with the listing. Anything local that the store no
    // longer has — deleted objects, keys that a rule now skips, leftovers from
    // an interrupted run, `.part` files — is removed, which is what makes the
    // existing local reconcile delete those documents from the index on this
    // same run. The mirror is walked rather than the manifest diffed, so a lost
    // manifest cannot leave a deleted object searchable forever.
    let keep: BTreeSet<&str> = admitted
        .iter()
        .map(|e| e.rel.as_str())
        .filter(|rel| !vanished.contains(rel))
        .collect();
    report.removed = prune_mirror(&run.mirror, &keep)?;
    manifest
        .objects
        .retain(|rel, _| keep.contains(rel.as_str()));
    manifest.save(&run.manifest_path)?;

    let ops = source.ops();
    report.list_requests = ops.list_requests;
    report.read_requests = ops.read_requests;
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// True when the mirror already holds this object's bytes at the right size.
fn mirror_file_ready(mirror: &Path, rel: &str, size: u64) -> bool {
    std::fs::metadata(mirror.join(rel)).is_ok_and(|m| m.is_file() && m.len() == size)
}

enum FetchOutcome {
    Fetched { rel: String, bytes: u64 },
    Vanished { rel: String },
}

/// Stream each pending object into the mirror.
///
/// A failure that is not "the object is gone" aborts the whole run. That is the
/// safe direction and not a convenience: materialisation happens before anything
/// is indexed, and skipping a failed download would present the object as
/// deleted to the reconcile step, which would then delete its documents because
/// of a transient 500.
fn fetch_objects(
    run: &ObjectRun,
    source: &dyn DocSource,
    pending: &[&SourceEntry],
    pr: &Progress,
    concurrency: usize,
) -> Result<Vec<FetchOutcome>> {
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(concurrency)
        .thread_name(|i| format!("xerj-fetch-{i}"))
        .build()
        .context("build the object-fetch pool")?;
    let results: Vec<Result<FetchOutcome>> = pool.install(|| {
        use rayon::prelude::*;
        pending
            .par_iter()
            .map(|entry| {
                let guard = pr.file(&entry.rel, entry.size);
                let outcome = fetch_one(run, source, entry);
                drop(guard);
                outcome
            })
            .collect()
    });
    let mut outcomes = Vec::with_capacity(results.len());
    for result in results {
        outcomes.push(result?);
    }
    Ok(outcomes)
}

fn fetch_one(run: &ObjectRun, source: &dyn DocSource, entry: &SourceEntry) -> Result<FetchOutcome> {
    let destination = run.mirror.join(&entry.rel);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create {} for object {}",
                parent.display(),
                crate::progress::sanitize(&entry.rel, crate::progress::SAFE_PATH_MAX)
            )
        })?;
    }
    let mut reader = match source.open(entry) {
        Ok(reader) => reader,
        Err(e) if e.downcast_ref::<MissingObject>().is_some() => {
            return Ok(FetchOutcome::Vanished {
                rel: entry.rel.clone(),
            })
        }
        Err(e) => return Err(e),
    };
    // A temporary beside the destination, then an atomic rename: a killed run
    // must never leave a half-written object looking like a whole one, because
    // the next run's size check would accept it.
    let temp = destination.with_file_name(format!(
        ".{}.{}.part",
        destination
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "object".into()),
        std::process::id()
    ));
    let bytes = {
        let file = std::fs::File::create(&temp)
            .with_context(|| format!("create {}", temp.display()))?;
        let mut writer = std::io::BufWriter::with_capacity(COPY_BUFFER_BYTES, file);
        // `io::copy` with a bounded buffer: the object is never in memory whole,
        // whatever its size. This is the streaming guarantee, and it is why a
        // 1 GB object costs a flat few hundred kilobytes of RSS.
        let copied = std::io::copy(&mut reader, &mut writer).with_context(|| {
            format!(
                "stream object {} into {}",
                crate::progress::sanitize(&entry.rel, crate::progress::SAFE_PATH_MAX),
                temp.display()
            )
        })?;
        use std::io::Write;
        writer.flush()?;
        copied
    };
    xerj_common::fsio::replace_file_durable(&temp, &destination)
        .with_context(|| format!("install {}", destination.display()))?;
    Ok(FetchOutcome::Fetched {
        rel: entry.rel.clone(),
        bytes,
    })
}

/// Delete every file under `mirror` whose relative path is not in `keep`, and
/// then every directory the deletions emptied. Returns how many files went.
fn prune_mirror(mirror: &Path, keep: &BTreeSet<&str>) -> Result<u64> {
    let mut removed = 0u64;
    let mut directories: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(mirror).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if entry.file_type().is_dir() {
            if path != mirror {
                directories.push(path.to_path_buf());
            }
            continue;
        }
        let Ok(rel) = path.strip_prefix(mirror) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !keep.contains(rel.as_str()) {
            if std::fs::remove_file(path).is_ok() {
                removed += 1;
            }
        }
    }
    // Deepest first, so a directory whose only contents were removed files goes
    // too. `remove_dir` on a non-empty directory fails and is ignored, which is
    // exactly the test for "did this become empty".
    directories.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for directory in directories {
        let _ = std::fs::remove_dir(&directory);
    }
    Ok(removed)
}

/// The S3 client, a runtime to drive it, and the request counters.
pub struct ObjectStoreSource {
    spec: ObjectSpec,
    runtime: tokio::runtime::Runtime,
    client: aws_sdk_s3::Client,
    list_requests: AtomicU64,
    read_requests: AtomicU64,
    bytes_read: AtomicU64,
}

impl ObjectStoreSource {
    /// Build the client from the standard credential chain.
    ///
    /// autoindex is synchronous — std threads and rayon — and the AWS SDK is
    /// async-only, so one runtime is created here and every call is driven
    /// through it. It is a *multi-thread* runtime with two workers rather than
    /// the cheaper current-thread one on purpose: a current-thread runtime is
    /// only driven while the thread that owns it sits in `block_on`, so the
    /// blocking reader below — called from the fetch pool's threads — would
    /// deadlock waiting for a runtime nobody is polling.
    pub fn connect(spec: &ObjectSpec) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("xerj-s3")
            .build()
            .context("start the runtime for object-store requests")?;
        let region = resolve_region(spec);
        let endpoint = spec.endpoint.clone();
        let client = runtime.block_on(async move {
            let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new(region));
            if let Some(endpoint) = endpoint.as_deref() {
                loader = loader.endpoint_url(endpoint);
            }
            let shared = loader.load().await;
            let mut builder = aws_sdk_s3::config::Builder::from(&shared);
            if endpoint.is_some() {
                // A custom endpoint means MinIO, Ceph, R2, localstack or an
                // in-process test: virtual-host addressing there needs DNS
                // nobody set up, so address the bucket in the path.
                // (quickwit does the same, s3_compatible_storage.rs:151.)
                builder = builder.force_path_style(true);
            }
            aws_sdk_s3::Client::from_conf(builder.build())
        });
        Ok(Self {
            spec: spec.clone(),
            runtime,
            client,
            list_requests: AtomicU64::new(0),
            read_requests: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
        })
    }

    fn key_for(&self, rel: &str) -> String {
        format!("{}{}", self.spec.prefix, rel)
    }
}

/// Region for the request.
///
/// R2 and every other S3-compatible store behind `--endpoint-url` use `auto`;
/// real S3 needs a real region, and `us-east-1` is the one that answers with a
/// redirect naming the right one when it is wrong. An explicit `AWS_REGION` /
/// `AWS_DEFAULT_REGION` always wins.
fn resolve_region(spec: &ObjectSpec) -> String {
    if let Ok(region) = std::env::var("AWS_REGION").or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
    {
        if !region.trim().is_empty() {
            return region;
        }
    }
    if spec.scheme == ObjectScheme::R2 || spec.endpoint.is_some() {
        "auto".into()
    } else {
        "us-east-1".into()
    }
}

impl DocSource for ObjectStoreSource {
    fn describe(&self) -> String {
        self.spec.identity()
    }

    fn list(&self) -> Result<SourceListing> {
        let mut listing = SourceListing::default();
        let mut continuation: Option<String> = None;
        let mut pages = 0u64;
        loop {
            pages += 1;
            if pages > MAX_LIST_PAGES {
                bail!(
                    "{} holds more than {} objects ({} LIST requests and counting). Refusing to \
                     continue: that is 1% of a 1,000,000/month free class-A allowance in one run, \
                     and the mirror would need the disk for all of it. Point the run at a narrower \
                     prefix",
                    self.spec.identity(),
                    MAX_LIST_PAGES * LIST_PAGE_KEYS as u64,
                    MAX_LIST_PAGES
                );
            }
            let prefix = (!self.spec.prefix.is_empty()).then(|| self.spec.prefix.clone());
            let page = self
                .runtime
                .block_on(
                    self.client
                        .list_objects_v2()
                        .bucket(&self.spec.bucket)
                        .set_prefix(prefix)
                        .max_keys(LIST_PAGE_KEYS)
                        .set_continuation_token(continuation.clone())
                        .send(),
                )
                .map_err(|e| self.explain("list", None, e))?;
            self.list_requests.fetch_add(1, Ordering::Relaxed);
            for object in page.contents() {
                let Some(key) = object.key() else { continue };
                listing.seen += 1;
                let rel = key.strip_prefix(self.spec.prefix.as_str()).unwrap_or(key);
                match classify_key(rel) {
                    KeyVerdict::Skip(rule) => {
                        *listing.skipped.entry(rule.to_string()).or_default() += 1;
                    }
                    KeyVerdict::Admit => {
                        let size = object.size().unwrap_or(0).max(0) as u64;
                        let last_modified = object
                            .last_modified()
                            .and_then(|t| t.fmt(aws_sdk_s3::primitives::DateTimeFormat::DateTime).ok());
                        listing.entries.push(SourceEntry {
                            rel: rel.to_string(),
                            size,
                            change_token: change_token(
                                object.e_tag(),
                                size,
                                last_modified.as_deref(),
                            ),
                            last_modified,
                        });
                    }
                }
            }
            let truncated = page.is_truncated().unwrap_or(false);
            continuation = page.next_continuation_token().map(str::to_string);
            if !truncated || continuation.is_none() {
                break;
            }
        }
        Ok(listing)
    }

    fn open(&self, entry: &SourceEntry) -> Result<Box<dyn Read + Send>> {
        let key = self.key_for(&entry.rel);
        let output = self
            .runtime
            .block_on(
                self.client
                    .get_object()
                    .bucket(&self.spec.bucket)
                    .key(&key)
                    .send(),
            )
            .map_err(|e| self.explain("get", Some(&key), e))?;
        self.read_requests.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(BlockingObjectReader {
            handle: self.runtime.handle().clone(),
            inner: Box::pin(output.body.into_async_read()),
        }))
    }

    fn ops(&self) -> SourceOps {
        SourceOps {
            list_requests: self.list_requests.load(Ordering::Relaxed),
            read_requests: self.read_requests.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
        }
    }
}

impl ObjectStoreSource {
    /// Turn an SDK failure into something an operator can act on.
    ///
    /// The SDK models one of these per operation (`NoSuchBucket`) and delivers
    /// the rest — access denied, a bad key, an unreachable endpoint, no
    /// credentials at all — as unmodelled service or dispatch errors whose type
    /// differs per operation. Classifying the RENDERED error chain therefore
    /// covers every case with one function; `DisplayErrorContext` is the SDK's
    /// own way of rendering that chain and includes the service error code.
    ///
    /// None of these messages include a credential: what is echoed back is the
    /// bucket, the key and the endpoint.
    fn explain<E, R>(
        &self,
        operation: &str,
        key: Option<&str>,
        error: aws_sdk_s3::error::SdkError<E, R>,
    ) -> anyhow::Error
    where
        E: std::error::Error + Send + Sync + 'static,
        R: std::fmt::Debug + Send + Sync + 'static,
    {
        let rendered = format!("{}", aws_sdk_s3::error::DisplayErrorContext(&error));
        let identity = self.spec.identity();
        let endpoint = self
            .spec
            .endpoint
            .clone()
            .unwrap_or_else(|| "the AWS endpoint for the region".into());
        let has = |needle: &str| rendered.to_lowercase().contains(&needle.to_lowercase());

        if operation == "get" && (has("NoSuchKey") || has("404") && has("not found")) {
            let key = key.unwrap_or_default().to_string();
            return anyhow::Error::new(MissingObject { key });
        }
        let advice = if has("NoSuchBucket") || has("bucket does not exist") {
            format!(
                "no bucket named {} at {endpoint}. Check the spelling, and check that the \
                 credentials in the environment belong to the account that owns it",
                self.spec.bucket
            )
        } else if has("InvalidAccessKeyId") || has("SignatureDoesNotMatch") {
            format!(
                "{endpoint} rejected the credentials. AWS_ACCESS_KEY_ID / \
                 AWS_SECRET_ACCESS_KEY (or the profile in AWS_PROFILE) do not match a key on that \
                 account — for R2 the pair comes from R2 → Manage API tokens, and the endpoint must \
                 be the account's own https://<account-id>.r2.cloudflarestorage.com"
            )
        } else if has("AccessDenied") || has("403") {
            format!(
                "access denied for {identity}. The credentials are valid but lack \
                 s3:ListBucket on {} (and s3:GetObject on its objects). For R2, give the API token \
                 'Object Read' on this bucket",
                self.spec.bucket
            )
        } else if has("no credentials")
            || has("credentials were not provided")
            || has("CredentialsNotLoaded")
            || has("credential")
        {
            "no credentials were found. Set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY in the \
             environment, or AWS_PROFILE for a profile in ~/.aws/credentials, or run on an instance \
             with a role. XERJ reads them through the standard AWS chain and never stores them"
                .to_string()
        } else if has("dispatch failure") || has("connect") || has("dns") || has("timeout") {
            format!(
                "could not reach {endpoint}. Check the endpoint URL and that this machine can \
                 reach it; an S3-compatible store needs --endpoint-url"
            )
        } else {
            format!("{operation} failed against {identity}")
        };
        let where_ = match key {
            Some(key) => format!("{identity} (key {key})"),
            None => identity,
        };
        anyhow::anyhow!("{advice}\n  while listing/reading {where_}\n  store said: {rendered}")
    }
}

/// A synchronous [`Read`] over an async S3 body.
///
/// One `read` call is one `block_on` of one async read, so the object is
/// streamed in whatever chunks the transport produces and nothing accumulates.
struct BlockingObjectReader {
    handle: tokio::runtime::Handle,
    inner: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>,
}

impl Read for BlockingObjectReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        self.handle.block_on(self.inner.read(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_is_an_opaque_token_including_multipart() {
        let single = change_token(Some("\"9bb58f26192e4ba00f01e2e7b136bbd8\""), 11, None).unwrap();
        let multipart = change_token(Some("\"a1b2c3d4e5f60718293a4b5c6d7e8f90-7\""), 11, None).unwrap();
        assert!(single.contains("etag:"));
        assert!(
            multipart.contains("-7"),
            "a multipart ETag is used as-is, not parsed: {multipart}"
        );
        assert_ne!(single, multipart);
        // Size participates, so a same-ETag different-size pair is a change.
        assert_ne!(
            change_token(Some("\"x\""), 1, None),
            change_token(Some("\"x\""), 2, None)
        );
    }

    #[test]
    fn last_modified_is_the_fallback_and_nothing_is_the_last_resort() {
        assert_eq!(
            change_token(None, 5, Some("2026-09-19T10:00:00Z")).unwrap(),
            "lastmod:2026-09-19T10:00:00Z|size:5"
        );
        assert_eq!(change_token(None, 5, None), None);
        assert_eq!(change_token(Some("  "), 5, None), None);
    }

    #[test]
    fn manifest_round_trips_and_rejects_a_foreign_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        let mut manifest = ObjectManifest::empty("s3://bucket/p/");
        manifest.objects.insert(
            "a.md".into(),
            ObjectRecord {
                change_token: "etag:\"x\"|size:3".into(),
                size: 3,
                etag: Some("\"x\"".into()),
                last_modified: None,
                fetched: "2026-09-19T00:00:00Z".into(),
            },
        );
        manifest.save(&path).unwrap();
        let (loaded, reason) = ObjectManifest::load(&path, "s3://bucket/p/");
        assert!(reason.is_none());
        assert_eq!(loaded.objects.len(), 1);
        let (other, reason) = ObjectManifest::load(&path, "s3://bucket/other/");
        assert!(other.objects.is_empty());
        assert!(reason.unwrap().contains("was last used for"));
    }

    #[test]
    fn a_missing_manifest_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (manifest, reason) =
            ObjectManifest::load(&dir.path().join("nope.json"), "s3://bucket/");
        assert!(manifest.objects.is_empty());
        assert!(reason.is_none(), "a first run has no manifest and no warning");
    }

    #[test]
    fn corrupt_manifest_explains_the_refetch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        std::fs::write(&path, b"{not json").unwrap();
        let (manifest, reason) = ObjectManifest::load(&path, "s3://bucket/");
        assert!(manifest.objects.is_empty());
        assert!(reason.unwrap().contains("fetched again"));
    }

    #[test]
    fn prune_removes_unlisted_files_and_their_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = dir.path();
        std::fs::create_dir_all(mirror.join("keep")).unwrap();
        std::fs::create_dir_all(mirror.join("drop/deep")).unwrap();
        std::fs::write(mirror.join("keep/a.txt"), b"a").unwrap();
        std::fs::write(mirror.join("drop/deep/b.txt"), b"b").unwrap();
        std::fs::write(mirror.join(".c.txt.1234.part"), b"junk").unwrap();
        let keep: BTreeSet<&str> = ["keep/a.txt"].into_iter().collect();
        let removed = prune_mirror(mirror, &keep).unwrap();
        assert_eq!(removed, 2, "the stale object and the leftover .part file");
        assert!(mirror.join("keep/a.txt").exists());
        assert!(!mirror.join("drop").exists(), "emptied directories go too");
    }

    #[test]
    fn region_defaults_are_store_appropriate() {
        // Set deliberately: the resolver reads the environment first, and a box
        // with AWS_REGION exported must not change what these assert.
        let spec = |scheme, endpoint: Option<&str>| ObjectSpec {
            scheme,
            bucket: "b".into(),
            prefix: String::new(),
            endpoint: endpoint.map(str::to_string),
        };
        let saved = std::env::var("AWS_REGION").ok();
        let saved_default = std::env::var("AWS_DEFAULT_REGION").ok();
        std::env::remove_var("AWS_REGION");
        std::env::remove_var("AWS_DEFAULT_REGION");
        assert_eq!(resolve_region(&spec(ObjectScheme::S3, None)), "us-east-1");
        assert_eq!(resolve_region(&spec(ObjectScheme::R2, Some("http://x"))), "auto");
        assert_eq!(
            resolve_region(&spec(ObjectScheme::S3, Some("http://127.0.0.1:9000"))),
            "auto"
        );
        std::env::set_var("AWS_REGION", "eu-central-1");
        assert_eq!(resolve_region(&spec(ObjectScheme::S3, None)), "eu-central-1");
        match saved {
            Some(v) => std::env::set_var("AWS_REGION", v),
            None => std::env::remove_var("AWS_REGION"),
        }
        if let Some(v) = saved_default {
            std::env::set_var("AWS_DEFAULT_REGION", v);
        }
    }

    #[test]
    fn cost_lines_state_the_schedule_arithmetic() {
        let report = MaterializeReport {
            list_requests: 3,
            read_requests: 12,
            bytes_downloaded: 5 << 20,
            ..Default::default()
        };
        let lines = report.cost_lines();
        assert!(lines[0].contains("3 LIST (class A)"), "{:?}", lines);
        assert!(lines[0].contains("12 GET (class B)"), "{:?}", lines);
        // 3 per run, hourly = 720 runs a month.
        assert!(lines[1].contains("2160/month hourly"), "{:?}", lines);
        assert!(lines[1].contains("25920/month every 5"), "{:?}", lines);
        assert!(lines[1].contains("1,000,000/month free allowance"), "{:?}", lines);
    }

    #[test]
    fn fetch_concurrency_is_bounded_both_ends() {
        assert_eq!(fetch_concurrency(0), 1);
        assert_eq!(fetch_concurrency(4), 4);
        assert_eq!(fetch_concurrency(64), 16);
    }
}
