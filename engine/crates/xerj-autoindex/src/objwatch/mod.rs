//! `--watch` for object storage: keep a bucket-backed index current.
//!
//! A bucket has no inotify. There are exactly two ways to learn that an object
//! changed — **poll** the key space, or subscribe to the store's **event
//! notifications** — and they trade the same way everywhere:
//!
//! | | poll (`ListObjectsV2`) | events (SQS / Cloudflare Queues) |
//! |---|---|---|
//! | works on | S3, R2, MinIO, Ceph RGW, any S3 gateway | only where configured, per-vendor plumbing |
//! | latency | one poll interval | seconds |
//! | cost | **Class A per 1,000 keys per cycle** | per message, on the queue's own bill |
//! | completeness | authoritative: the listing IS the truth | at-least-once, and a missed event is invisible |
//! | setup | credentials and a prefix | bucket config + queue + consumer + IAM |
//!
//! This module implements polling, because polling is the portable one and
//! because it is self-correcting: every cycle re-derives the truth from the
//! bucket, so a dropped event cannot leave the index permanently wrong. What an
//! event-driven path would take is written down in
//! `docs/WATCHING_OBJECT_STORAGE.md` rather than guessed at here; it is NOT
//! implemented.
//!
//! The cost model is not an afterthought: `ListObjectsV2` is a Class A
//! operation, one call per 1,000 keys, so the poll interval spends money on
//! every cycle whether anything changed or not. [`cost`] holds the arithmetic,
//! the guard refuses an interval that cannot stay inside the budget, and every
//! cycle reports what it spent.
//!
//! WHAT THIS MODULE DOES NOT DO: it does not index. It produces a change feed —
//! added / changed / deleted, with content digests — and hands each event to a
//! [`ChangeSink`]. The JSONL sink shipped here is what an operator or an agent
//! can consume today; the object-storage indexer plugs into the same trait.

pub mod cost;
pub mod journal;
pub mod run;
pub mod s3;

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use cost::{CostTotals, Projection};
pub use journal::{WatchJournal, WatchLock, WatchedObject};

/// One object as the store described it in a listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub last_modified: String,
}

/// One `ListObjectsV2` call's worth of arguments.
#[derive(Debug, Clone)]
pub struct ListRequest<'a> {
    pub prefix: &'a str,
    pub continuation_token: Option<&'a str>,
    /// `start-after`: the store returns only keys greater than this one. The
    /// whole of the append-only optimisation.
    pub start_after: Option<&'a str>,
    pub delimiter: Option<&'a str>,
    pub max_keys: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ListPage {
    pub objects: Vec<ObjectMeta>,
    /// `Some` only when the store said the listing is truncated.
    pub next_token: Option<String>,
    pub common_prefixes: Vec<String>,
}

pub struct FetchedObject {
    pub bytes: Vec<u8>,
    /// ETag of the bytes we actually read, when the store sent one.
    pub etag: Option<String>,
    /// The object was longer than the byte cap and the bytes are a prefix.
    pub truncated: bool,
}

/// The minimum an object store must do for the watcher: list, and read.
///
/// Deliberately two methods. A richer object source (streaming reads,
/// multipart-aware fetch, a shared credential resolver) can implement this
/// without the watcher changing, which is how this stream stays independent of
/// the one adding `xerj autoindex s3://…`.
pub trait ObjectSource {
    /// Human-readable location. Must never contain credentials.
    fn describe(&self) -> String;
    fn list(&self, req: &ListRequest) -> Result<ListPage>;
    fn get(&self, key: &str, max_bytes: u64) -> Result<FetchedObject>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Changed,
    Deleted,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Changed => "changed",
            ChangeKind::Deleted => "deleted",
        }
    }
}

/// One thing that happened to one object.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub kind: ChangeKind,
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub last_modified: String,
    /// xxh3 of the fetched bytes. `None` for a delete and in `--no-fetch` mode.
    pub digest: Option<String>,
    pub bytes_fetched: u64,
    /// Why the watcher considers this changed. Printed so an operator can tell
    /// a real edit from a store that rewrote an ETag.
    pub reason: &'static str,
    pub truncated: bool,
}

impl ChangeEvent {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "xerj": "objwatch-change",
            "kind": self.kind.as_str(),
            "key": self.key,
            "size": self.size,
            "etag": self.etag,
            "last_modified": self.last_modified,
            "digest": self.digest,
            "bytes_fetched": self.bytes_fetched,
            "reason": self.reason,
            "truncated": self.truncated,
        })
    }
}

/// Where change events go. The indexer for object storage is a `ChangeSink`;
/// so is `tee`.
pub trait ChangeSink {
    fn accept(&mut self, ev: &ChangeEvent) -> Result<()>;
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// JSON Lines, one event per line, flushed per event so a `tail -f` sees it.
pub struct JsonlSink<W: Write> {
    out: W,
    pub written: u64,
}

impl<W: Write> JsonlSink<W> {
    pub fn new(out: W) -> JsonlSink<W> {
        JsonlSink { out, written: 0 }
    }
}

impl<W: Write> ChangeSink for JsonlSink<W> {
    fn accept(&mut self, ev: &ChangeEvent) -> Result<()> {
        writeln!(self.out, "{}", ev.to_json()).context("write change event")?;
        self.out.flush().context("flush change event")?;
        self.written += 1;
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        self.out.flush().context("flush change feed")?;
        Ok(())
    }
}

/// Counts and drops. Used by `--dry-run`, which measures the poll cost of a
/// bucket without fetching or indexing anything.
#[derive(Default)]
pub struct CountingSink {
    pub accepted: u64,
}

impl ChangeSink for CountingSink {
    fn accept(&mut self, _ev: &ChangeEvent) -> Result<()> {
        self.accepted += 1;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub poll_interval: Duration,
    /// `None` = run until stopped. `Some(1)` is `--once`.
    pub max_cycles: Option<u64>,
    /// Fetch the bytes of added/changed objects (Class B GETs) to compute a
    /// content digest. `false` is metadata-only: zero GETs, ever.
    pub fetch: bool,
    pub max_object_bytes: u64,
    /// Ask only for keys above the highest one seen. Turns a 1,000-page scan
    /// into a 1-page scan on an append-only key space — and cannot detect
    /// deletes or edits to older keys, which is why it is opt-in.
    pub append_only: bool,
    pub max_monthly_class_a: u64,
    pub allow_cost: bool,
    pub page_size: u64,
    /// Where to write the operator-readable running count.
    pub status_path: Option<PathBuf>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        WatchOptions {
            poll_interval: Duration::from_secs(cost::DEFAULT_POLL_INTERVAL_SECS),
            max_cycles: None,
            fetch: true,
            max_object_bytes: 64 << 20,
            append_only: false,
            max_monthly_class_a: cost::DEFAULT_MAX_MONTHLY_CLASS_A,
            allow_cost: false,
            page_size: cost::MAX_KEYS_PER_LIST,
            status_path: None,
        }
    }
}

/// The projected poll cost exceeded the budget and nothing was polled further.
///
/// A distinct type so the CLI can answer with the same "needs a decision"
/// contract the indexing gate uses (exit 4) instead of a generic failure.
#[derive(Debug)]
pub struct PollCostRefused {
    pub projection: Projection,
    pub location: String,
}

impl std::fmt::Display for PollCostRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to poll {} every {}s: {}. That is over the budget of {} Class A \
             operations/month. Poll every {}s or more, scope the watch with a narrower prefix, \
             pass --append-only if the key space only grows, or pass --allow-cost to accept the \
             spend.",
            self.location,
            self.projection.interval_secs,
            self.projection.line(),
            self.projection.budget,
            self.projection.min_safe_interval_secs
        )
    }
}

impl std::error::Error for PollCostRefused {}

impl PollCostRefused {
    /// Same shape as the indexing gate's decision request, so one agent-side
    /// branch handles both.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "xerj": "objwatch-decision-request",
            "exit_code": crate::gate::EXIT_NEEDS_DECISION,
            "reason": "poll_cost_over_budget",
            "location": self.location,
            "projection": self.projection.to_json(),
            "message": self.to_string(),
            "answers": ["--poll-interval <secs>", "--append-only", "--allow-cost"],
        })
    }
}

/// What one poll cycle did, and what it cost.
#[derive(Debug, Clone)]
pub struct CycleReport {
    pub cycle: u64,
    pub started_at: String,
    pub wall: Duration,
    pub list_calls: u64,
    pub keys_listed: u64,
    pub gets: u64,
    pub bytes_fetched: u64,
    pub added: u64,
    pub changed: u64,
    pub deleted: u64,
    pub unchanged: u64,
    /// Objects whose metadata said "changed" but whose bytes hashed identical to
    /// what was already recorded — ETag churn, not an edit. Counted, fetched,
    /// and deliberately NOT sent to the sink.
    pub content_identical: u64,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub projection: Projection,
    pub totals: CostTotals,
}

impl CycleReport {
    /// One line an operator can read in a terminal or a log.
    pub fn line(&self) -> String {
        let mut line = format!(
            "xerj-watch cycle={} added={} changed={} deleted={} unchanged={} same_bytes={} \
             list_calls={} gets={} fetched={} wall={:.2}s | month-to-date: \
             list_calls={} gets={} ({:.1}% of free-tier Class A if sustained)",
            self.cycle,
            self.added,
            self.changed,
            self.deleted,
            self.unchanged,
            self.content_identical,
            self.list_calls,
            self.gets,
            human_bytes(self.bytes_fetched),
            self.wall.as_secs_f64(),
            self.totals.list_calls,
            self.totals.gets,
            self.projection.free_tier_percent,
        );
        if self.totals.skipped_deadlines > 0 {
            // An overrun means the real interval is longer than the one the
            // budget was computed from, so it belongs on the line an operator
            // reads and not only in the JSON.
            line.push_str(&format!(
                " | {} poll deadline(s) passed while a cycle was running \
                 (cycles never overlap; the interval is effectively longer than {}s)",
                self.totals.skipped_deadlines, self.projection.interval_secs
            ));
        }
        line
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "xerj": "objwatch-cycle",
            "cycle": self.cycle,
            "started_at": self.started_at,
            "wall_ms": self.wall.as_millis() as u64,
            "list_calls_class_a": self.list_calls,
            "keys_listed": self.keys_listed,
            "gets_class_b": self.gets,
            "bytes_fetched": self.bytes_fetched,
            "added": self.added,
            "changed": self.changed,
            "deleted": self.deleted,
            "unchanged": self.unchanged,
            "content_identical": self.content_identical,
            "errors": self.errors,
            "warnings": self.warnings,
            "projection": self.projection.to_json(),
            "totals": self.totals.to_json(),
        })
    }
}

/// The change set one listing implies.
#[derive(Debug, Clone, Default)]
pub struct PollPlan {
    pub added: Vec<ObjectMeta>,
    pub changed: Vec<(ObjectMeta, &'static str)>,
    pub deleted: Vec<String>,
    pub unchanged: u64,
}

impl PollPlan {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.deleted.is_empty()
    }
    pub fn len(&self) -> usize {
        self.added.len() + self.changed.len() + self.deleted.len()
    }
}

/// Compare a listing against the journal.
///
/// `detect_deletes` must be false whenever the listing did not cover the whole
/// key space (append-only mode), because "absent from a partial listing" is not
/// "deleted" — acting on it would drop live records.
pub fn diff(journal: &WatchJournal, listed: &[ObjectMeta], detect_deletes: bool) -> PollPlan {
    let mut plan = PollPlan::default();
    for meta in listed {
        match journal.get(&meta.key) {
            None => plan.added.push(meta.clone()),
            Some(known) => {
                // ETag first: it is the store's own content identity, and it is
                // the only one of the three that changes when content changes
                // but size and mtime do not. Size and last-modified are checked
                // too because some gateways reuse an ETag across a rewrite.
                let reason = if known.etag != meta.etag {
                    Some("etag")
                } else if known.size != meta.size {
                    Some("size")
                } else if known.last_modified != meta.last_modified {
                    Some("last_modified")
                } else if known.etag_unverified {
                    Some("etag_unverified")
                } else {
                    None
                };
                match reason {
                    Some(r) => plan.changed.push((meta.clone(), r)),
                    None => plan.unchanged += 1,
                }
            }
        }
    }
    if detect_deletes {
        let present: std::collections::HashSet<&str> =
            listed.iter().map(|m| m.key.as_str()).collect();
        for key in journal.objects.keys() {
            if !present.contains(key.as_str()) {
                plan.deleted.push(key.clone());
            }
        }
    }
    plan
}

/// Flush the journal after every accepted object while it is small; batch once
/// it is big, because a save rewrites the whole file.
const JOURNAL_FLUSH_BATCH_ABOVE: usize = 10_000;
const JOURNAL_FLUSH_EVERY_WHEN_BIG: u64 = 64;

/// List the whole watched key space (or, in append-only mode, its tail).
fn scan(
    src: &dyn ObjectSource,
    prefix: &str,
    start_after: Option<&str>,
    page_size: u64,
    stop: &dyn Fn() -> bool,
) -> Result<(Vec<ObjectMeta>, u64)> {
    let mut out: Vec<ObjectMeta> = Vec::new();
    let mut token: Option<String> = None;
    let mut calls = 0u64;
    loop {
        let page = src.list(&ListRequest {
            prefix,
            continuation_token: token.as_deref(),
            start_after: if token.is_some() { None } else { start_after },
            delimiter: None,
            max_keys: page_size,
        })?;
        calls += 1;
        out.extend(page.objects);
        match page.next_token {
            Some(t) => token = Some(t),
            None => break,
        }
        if stop() {
            // A cancelled scan is a partial listing: the caller must not treat
            // it as authoritative, so say so rather than returning a truncated
            // "truth".
            bail!("listing cancelled after {calls} call(s); no change set was derived");
        }
    }
    Ok((out, calls))
}

/// One poll cycle: list, diff, fetch what changed, feed the sink, record.
///
/// `cycle_index` is 0 for the first cycle of this process, which is the only one
/// that can refuse on cost (a later cycle warns instead: the bucket grew while
/// we were running, and stopping a live watcher is worse than telling its
/// operator).
#[allow(clippy::too_many_arguments)]
pub fn poll_once(
    src: &dyn ObjectSource,
    journal: &mut WatchJournal,
    opts: &WatchOptions,
    sink: &mut dyn ChangeSink,
    cycle_index: u64,
    totals: &mut CostTotals,
    stop: &dyn Fn() -> bool,
) -> Result<CycleReport> {
    let started = Instant::now();
    let started_at = journal::now_rfc3339();
    let prefix = journal.prefix.clone();
    let start_after = if opts.append_only {
        journal.max_key_seen.clone()
    } else {
        None
    };
    let (listed, list_calls) = scan(src, &prefix, start_after.as_deref(), opts.page_size, stop)?;
    totals.list_calls += list_calls;
    totals.keys_listed += listed.len() as u64;

    // The cost of a cycle is knowable only after the listing, and the listing
    // is the cheapest thing we do — so the guard runs here, before any GET and
    // before the sink is told anything.
    // What the NEXT cycle will list, which is what the recurring cost is.
    //
    // In full-scan mode that is the whole key space, and the journal is a better
    // estimate of it than one listing (a listing taken mid-delete is smaller than
    // the space it covers). In APPEND-ONLY mode it is only the tail above
    // `start-after`, and using the journal size there would be a straight bug:
    // `--append-only` is the documented answer to a bucket too large to scan, and
    // projecting it at journal size would make the guard refuse the very escape
    // hatch it recommends. A 1,000,000-key append-only journal costs ONE list
    // call per cycle, not 1,000.
    let known_keys = if opts.append_only {
        listed.len() as u64
    } else {
        (journal.len() as u64).max(listed.len() as u64)
    };
    let projection = Projection::from_millis(
        known_keys,
        cost::list_calls_for_keys(known_keys),
        // Milliseconds, not `as_secs()`: a sub-second interval truncates to 0 s,
        // which projects as unbounded and refuses for the wrong reason.
        opts.poll_interval.as_millis().min(u128::from(u64::MAX)) as u64,
        opts.max_monthly_class_a,
    );
    let mut warnings: Vec<String> = Vec::new();
    if projection.over_budget() {
        if cycle_index == 0 && !opts.allow_cost {
            return Err(PollCostRefused {
                projection,
                location: src.describe(),
            }
            .into());
        }
        warnings.push(format!(
            "poll cost is over budget and the watch is continuing: {}",
            projection.line()
        ));
    }

    let plan = diff(journal, &listed, !opts.append_only);
    let mut report = CycleReport {
        cycle: cycle_index,
        started_at,
        wall: Duration::ZERO,
        list_calls,
        keys_listed: listed.len() as u64,
        gets: 0,
        bytes_fetched: 0,
        added: 0,
        changed: 0,
        deleted: 0,
        unchanged: plan.unchanged,
        content_identical: 0,
        errors: Vec::new(),
        warnings,
        projection,
        totals: totals.clone(),
    };

    let flush_every = if journal.len() > JOURNAL_FLUSH_BATCH_ABOVE {
        JOURNAL_FLUSH_EVERY_WHEN_BIG
    } else {
        1
    };
    let mut since_flush = 0u64;

    // Deletes first: dropping a gone object's records before re-reading a
    // changed one keeps the index from briefly holding both.
    for key in &plan.deleted {
        if stop() {
            break;
        }
        let known = journal.get(key).cloned();
        let ev = ChangeEvent {
            kind: ChangeKind::Deleted,
            key: key.clone(),
            size: known.as_ref().map(|k| k.size).unwrap_or(0),
            etag: known.as_ref().map(|k| k.etag.clone()).unwrap_or_default(),
            last_modified: known.map(|k| k.last_modified).unwrap_or_default(),
            digest: None,
            bytes_fetched: 0,
            reason: "absent_from_listing",
            truncated: false,
        };
        match sink.accept(&ev) {
            Ok(()) => {
                journal.forget(key);
                report.deleted += 1;
                totals.events += 1;
                since_flush += 1;
            }
            Err(e) => record_error(&mut report, totals, format!("delete {key}: {e}")),
        }
        if since_flush >= flush_every {
            journal.save()?;
            since_flush = 0;
        }
    }

    let work: Vec<(ObjectMeta, ChangeKind, &'static str)> = plan
        .added
        .iter()
        .map(|m| (m.clone(), ChangeKind::Added, "new_key"))
        .chain(
            plan.changed
                .iter()
                .map(|(m, r)| (m.clone(), ChangeKind::Changed, *r)),
        )
        .collect();

    for (meta, kind, reason) in work {
        if stop() {
            break;
        }
        let prev_digest = journal.get(&meta.key).and_then(|k| k.digest.clone());
        let mut digest = None;
        let mut etag_used = meta.etag.clone();
        let mut fetch_had_no_etag = false;
        let mut truncated = false;
        let mut bytes_fetched = 0u64;
        if opts.fetch {
            match src.get(&meta.key, opts.max_object_bytes) {
                Ok(f) => {
                    report.gets += 1;
                    totals.gets += 1;
                    bytes_fetched = f.bytes.len() as u64;
                    report.bytes_fetched += bytes_fetched;
                    totals.bytes_fetched += bytes_fetched;
                    digest = Some(format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&f.bytes)));
                    truncated = f.truncated;
                    match f.etag {
                        // Record the ETag of the bytes we actually read, not the
                        // one the listing showed. If the object was replaced
                        // between the list and the get, the next cycle sees a
                        // difference and re-reads it — no lost update, and no
                        // second index of the same bytes.
                        Some(e) => etag_used = e,
                        None => fetch_had_no_etag = true,
                    }
                }
                Err(e) => {
                    record_error(&mut report, totals, format!("fetch {}: {e}", meta.key));
                    // Leave the journal entry alone so the next cycle retries.
                    continue;
                }
            }
        }
        // The bytes hash the same as what is already recorded, so the metadata
        // change was ETag or mtime churn (a copy, a lifecycle rewrite, a
        // gateway that re-stamps on read) and not an edit. Update the journal so
        // the next cycle is free again, and do NOT re-index: an unchanged
        // document rewritten is a bulk request, a merge and a refresh for
        // nothing.
        let content_identical = opts.fetch && prev_digest.is_some() && prev_digest == digest;
        // A GET that carried no ETag means the recorded ETag came from the
        // listing and might describe bytes we did not read. Flag it so the next
        // cycle re-reads once and confirms by digest — and clear the flag as
        // soon as that confirmation happens, so a store that never sends ETags
        // costs one extra GET per change rather than one per cycle forever.
        let etag_unverified = fetch_had_no_etag && !content_identical;
        if content_identical {
            journal.record(
                &meta.key,
                WatchedObject {
                    etag: etag_used,
                    size: meta.size,
                    last_modified: meta.last_modified.clone(),
                    digest,
                    etag_unverified,
                    seen_at: journal::now_rfc3339(),
                    truncated,
                },
            );
            report.content_identical += 1;
            report.unchanged += 1;
            since_flush += 1;
            if since_flush >= flush_every {
                journal.save()?;
                since_flush = 0;
            }
            continue;
        }
        let ev = ChangeEvent {
            kind,
            key: meta.key.clone(),
            size: meta.size,
            etag: etag_used.clone(),
            last_modified: meta.last_modified.clone(),
            digest: digest.clone(),
            bytes_fetched,
            reason,
            truncated,
        };
        match sink.accept(&ev) {
            Ok(()) => {
                journal.record(
                    &meta.key,
                    WatchedObject {
                        etag: etag_used,
                        size: meta.size,
                        last_modified: meta.last_modified.clone(),
                        digest,
                        etag_unverified,
                        seen_at: journal::now_rfc3339(),
                        truncated,
                    },
                );
                match kind {
                    ChangeKind::Added => report.added += 1,
                    ChangeKind::Changed => report.changed += 1,
                    ChangeKind::Deleted => {}
                }
                totals.events += 1;
                since_flush += 1;
            }
            Err(e) => record_error(&mut report, totals, format!("index {}: {e}", meta.key)),
        }
        if since_flush >= flush_every {
            journal.save()?;
            since_flush = 0;
        }
    }

    sink.flush()?;
    journal.note_cycle();
    journal.save()?;
    totals.cycles += 1;
    report.wall = started.elapsed();
    report.totals = totals.clone();
    Ok(report)
}

fn record_error(report: &mut CycleReport, totals: &mut CostTotals, msg: String) {
    totals.errors += 1;
    // Bounded: a bucket-wide outage must not turn one cycle's report into a
    // megabyte of identical lines.
    if report.errors.len() < 20 {
        report.errors.push(msg);
    }
}

/// What a whole `--watch` run did.
#[derive(Debug, Clone)]
pub struct WatchOutcome {
    pub totals: CostTotals,
    pub last: Option<CycleReport>,
}

/// Poll until `stop()`, `max_cycles`, or a fatal error.
///
/// A cycle that overruns the interval does not queue: the missed deadlines are
/// counted (`skipped_deadlines`) and the next cycle starts immediately. Cycles
/// never overlap, which is what makes "no double index" structural rather than
/// a race the sink has to defend against.
pub fn run_watch(
    src: &dyn ObjectSource,
    journal: &mut WatchJournal,
    opts: &WatchOptions,
    sink: &mut dyn ChangeSink,
    stop: &dyn Fn() -> bool,
    report: &mut dyn FnMut(&CycleReport),
) -> Result<WatchOutcome> {
    let mut totals = CostTotals::default();
    let mut last: Option<CycleReport>;
    let mut cycle = 0u64;
    // Cycle k is scheduled for t0 + k * interval. Fixing the schedule up front
    // is what lets an overrun be *reported* (`skipped_deadlines`) instead of
    // silently stretching the interval an operator budgeted with.
    let mut next_start = Instant::now() + opts.poll_interval;
    loop {
        let r = poll_once(src, journal, opts, sink, cycle, &mut totals, stop)?;
        if let Some(path) = &opts.status_path {
            // Best effort: a watcher must not die because a status file could
            // not be written, but the operator must be able to see that it
            // failed, so it is reported as a cycle error.
            if let Err(e) = write_status(path, src.describe().as_str(), &r) {
                totals.errors += 1;
                eprintln!(
                    "xerj-watch: could not write status file {}: {e}",
                    path.display()
                );
            }
        }
        report(&r);
        last = Some(r);
        cycle += 1;
        if let Some(max) = opts.max_cycles {
            if cycle >= max {
                break;
            }
        }
        if stop() {
            break;
        }
        let now = Instant::now();
        if now >= next_start {
            // The cycle outlasted its own slot. Count every whole interval that
            // elapsed inside it, resync the schedule to now rather than chasing
            // a backlog forever, and start the next cycle immediately. Cycles
            // never overlap and never queue, which is what makes "no double
            // index, no lost update" structural instead of a race the sink has
            // to defend against.
            let late_by = now.duration_since(next_start);
            let interval_nanos = opts.poll_interval.as_nanos().max(1);
            totals.skipped_deadlines += 1 + (late_by.as_nanos() / interval_nanos) as u64;
            next_start = now + opts.poll_interval;
        } else {
            // Sleep in slices so a stop request is answered in under a second
            // rather than after a five-minute interval.
            while Instant::now() < next_start {
                if stop() {
                    break;
                }
                let left = next_start.saturating_duration_since(Instant::now());
                std::thread::sleep(left.min(Duration::from_millis(250)));
            }
            next_start += opts.poll_interval;
        }
        if stop() {
            break;
        }
    }
    Ok(WatchOutcome { totals, last })
}

/// The running count, where an operator (or an agent) can read it without
/// attaching to the process.
pub fn write_status(path: &std::path::Path, location: &str, r: &CycleReport) -> Result<()> {
    let body = serde_json::json!({
        "xerj": "objwatch-status",
        "location": location,
        "updated_at": journal::now_rfc3339(),
        "last_cycle": r.to_json(),
        "totals": r.totals.to_json(),
        "projection": r.projection.to_json(),
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

pub fn human_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n}B")
    } else if f < K * K {
        format!("{:.1}KB", f / K)
    } else if f < K * K * K {
        format!("{:.1}MB", f / (K * K))
    } else {
        format!("{:.2}GB", f / (K * K * K))
    }
}

#[cfg(test)]
mod tests;
