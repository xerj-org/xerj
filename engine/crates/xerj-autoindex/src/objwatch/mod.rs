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
//! operation, one call per page of up to 1,000 keys, so the poll interval
//! spends money on every cycle whether anything changed or not. [`cost`] holds
//! the arithmetic. Two breakers enforce it, on EVERY cycle, not only the first:
//! the projection (does this interval fit the monthly budget at the size the
//! bucket is now?) and the spend ledger (has this month's budget already been
//! spent, across restarts?). Either one stops the watch with a decision request
//! (exit 4) unless the operator passed `--allow-cost`. A shipped default must
//! never be able to spend an account's free tier by itself.
//!
//! WHAT THIS MODULE DOES NOT DO: it does not index. It produces a change feed —
//! added / changed / deleted, with content digests — and hands each event to a
//! [`ChangeSink`]. The JSONL sink shipped here is what an operator or an agent
//! can consume today. One-shot indexing of a bucket is `crate::objsource`, a
//! different command (`xerj autoindex s3://bucket/prefix`, no `--watch`);
//! wiring this feed into it plugs into the same trait and has not landed.

pub mod cost;
pub mod journal;
pub mod run;
pub mod s3;

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use cost::{CostTotals, MonthSpend, Projection, SpendLedger};
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
/// `Sync` because changed objects are fetched with bounded concurrency
/// ([`FETCH_CONCURRENCY`]): a first scan of 10,000 objects over a
/// 50-100 ms-latency link is 8-17 minutes serially.
///
/// Deliberately two methods. A richer object source (streaming reads,
/// multipart-aware fetch, a shared credential resolver) can implement this
/// without the watcher changing, which is how this stream stays independent of
/// the one adding `xerj autoindex s3://…`.
pub trait ObjectSource: Sync {
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
    /// Class B (GET) budget per calendar month.
    pub max_monthly_class_b: u64,
    pub allow_cost: bool,
    pub page_size: u64,
    /// Where to write the operator-readable running count.
    pub status_path: Option<PathBuf>,
    /// Price the poll and report the change set WITHOUT recording anything:
    /// no GETs, no sink, and the journal is neither changed nor saved. A dry
    /// run that recorded what it saw would make the next real run believe the
    /// whole bucket was already indexed.
    pub dry_run: bool,
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
            max_monthly_class_b: cost::DEFAULT_MAX_MONTHLY_CLASS_B,
            allow_cost: false,
            page_size: cost::MAX_KEYS_PER_LIST,
            status_path: None,
            dry_run: false,
        }
    }
}

/// Which breaker stopped the watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The projection at the bucket's current size is over the Class A budget.
    ProjectedOverBudget,
    /// This calendar month's Class A budget is spent (the spend ledger).
    ClassABudgetSpent,
    /// This calendar month's Class B (GET) budget would be exceeded.
    ClassBBudgetSpent,
}

impl RefusalReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefusalReason::ProjectedOverBudget => "poll_cost_over_budget",
            RefusalReason::ClassABudgetSpent => "monthly_class_a_budget_spent",
            RefusalReason::ClassBBudgetSpent => "monthly_class_b_budget_spent",
        }
    }
}

/// A cost breaker tripped and nothing further was polled.
///
/// A distinct type so the CLI can answer with the same "needs a decision"
/// contract the indexing gate uses (exit 4) instead of a generic failure. It is
/// returned on ANY cycle, not only the first: a bucket that grows past the
/// budget while it is being watched stops the watch. Warning and carrying on is
/// what spends a free tier while nobody is reading the log.
#[derive(Debug)]
pub struct PollCostRefused {
    pub reason: RefusalReason,
    pub projection: Projection,
    pub location: String,
    /// The cycle that tripped. 0 = before anything was emitted.
    pub cycle: u64,
    /// Month-to-date spend when it tripped.
    pub spent: MonthSpend,
    pub budget_class_b: u64,
    /// Operations the refused step needed (list calls, or GETs).
    pub needed: u64,
}

impl std::fmt::Display for PollCostRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let when = if self.cycle == 0 {
            "refusing to poll".to_string()
        } else {
            format!(
                "circuit breaker: stopping after cycle {} instead of polling",
                self.cycle
            )
        };
        match self.reason {
            RefusalReason::ProjectedOverBudget => write!(
                f,
                "{when} {} every {}s: {}. That is over the budget of {} Class A \
                 operations/month. Poll every {}s or more, scope the watch with a narrower prefix, \
                 pass --append-only if the key space only grows, or pass --allow-cost to accept the \
                 spend.",
                self.location,
                self.projection.interval_secs,
                self.projection.line(),
                self.projection.budget,
                self.projection.min_safe_interval_secs
            ),
            RefusalReason::ClassABudgetSpent => write!(
                f,
                "{when} {}: {} of the {} Class A operations budgeted for {} are already spent \
                 (ledger: {}), and the next listing needs {} more. Nothing more is listed until \
                 next month. Raise --max-monthly-ops, or pass --allow-cost to accept the spend.",
                self.location,
                self.spent.class_a,
                self.projection.budget,
                self.spent.month,
                cost::SpendLedger::FILE,
                self.needed
            ),
            RefusalReason::ClassBBudgetSpent => write!(
                f,
                "{when} {}: this cycle needs {} GET(s) and {} of the {} Class B operations \
                 budgeted for {} are already spent. Nothing was fetched or emitted. Pass --no-fetch \
                 (metadata-only, zero GETs), raise --max-monthly-gets, or pass --allow-cost.",
                self.location,
                self.needed,
                self.spent.class_b,
                self.budget_class_b,
                self.spent.month
            ),
        }
    }
}

impl std::error::Error for PollCostRefused {}

impl PollCostRefused {
    /// Same shape as the indexing gate's decision request, so one agent-side
    /// branch handles both.
    pub fn to_json(&self) -> serde_json::Value {
        let answers: &[&str] = match self.reason {
            RefusalReason::ProjectedOverBudget => {
                &["--poll-interval <secs>", "--append-only", "--allow-cost"]
            }
            RefusalReason::ClassABudgetSpent => &["--max-monthly-ops <n>", "--allow-cost"],
            RefusalReason::ClassBBudgetSpent => {
                &["--no-fetch", "--max-monthly-gets <n>", "--allow-cost"]
            }
        };
        serde_json::json!({
            "xerj": "objwatch-decision-request",
            "exit_code": crate::gate::EXIT_NEEDS_DECISION,
            "reason": self.reason.as_str(),
            "location": self.location,
            "cycle": self.cycle,
            "projection": self.projection.to_json(),
            "month_to_date": {
                "month": self.spent.month,
                "class_a": self.spent.class_a,
                "class_b": self.spent.class_b,
                "budget_class_a": self.projection.budget,
                "budget_class_b": self.budget_class_b,
            },
            "needed": self.needed,
            "message": self.to_string(),
            "answers": answers,
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
    /// Objects fetched only up to `--max-object-mb`. Their digest covers a
    /// prefix, so it can never prove "same bytes": a metadata change on one is
    /// always emitted, and counted here so the cap is visible.
    pub truncated: u64,
    /// Month-to-date spend after this cycle, from the ledger.
    pub month_to_date: MonthSpend,
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
             list_calls={} gets={} fetched={} wall={:.2}s | this process: list_calls={} gets={} \
             | month-to-date {}: Class A {}/{} budget, Class B {} | projected {:.1}% of the \
             free-tier Class A if sustained",
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
            self.month_to_date.month,
            self.month_to_date.class_a,
            self.projection.budget,
            self.month_to_date.class_b,
            self.projection.free_tier_percent,
        );
        if self.truncated > 0 {
            line.push_str(&format!(" truncated={}", self.truncated));
        }
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
            "truncated": self.truncated,
            "month_to_date": {
                "month": self.month_to_date.month,
                "class_a": self.month_to_date.class_a,
                "class_b": self.month_to_date.class_b,
            },
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

/// Journal saves are batched: at most this many recorded objects, or this long,
/// between two saves.
///
/// A save rewrites the whole file and fsyncs it, so saving after EVERY object
/// made a first scan quadratic: 10,000 objects meant 10,000 rewrites of a file
/// growing to ~2 MB. Measured against a local MinIO, one host, 10,000 objects,
/// `--no-fetch`: 17.95 s with the journal on ext4 and 6.08 s on tmpfs, against
/// 0.19 s either way after this change. Batching bounds what a crash
/// can cost to re-emitting the last window of events (the feed is
/// at-least-once; the object-storage indexer's ids are idempotent) and turns
/// the save cost from O(n^2) into O(n).
const JOURNAL_SAVE_EVERY_OBJECTS: u64 = 256;
const JOURNAL_SAVE_EVERY: Duration = Duration::from_secs(2);

/// GETs in flight at once when fetching changed objects. Bounded, as every
/// object-store client bounds bulk operations (quickwit bounds its bulk delete
/// with `buffer_unordered(100)`, quickwit-storage s3_compatible_storage.rs:726;
/// this is the same idea on std threads, far lower because a watcher shares the
/// account with production traffic).
pub const FETCH_CONCURRENCY: usize = 8;
/// Objects fetched per batch before their events are emitted in listing order.
const FETCH_BATCH_OBJECTS: usize = 32;
/// Upper bound on the bytes one batch may hold in memory, estimated from the
/// listed sizes capped at `--max-object-mb`. A batch always holds at least one
/// object.
const FETCH_BATCH_BYTES: u64 = 256 << 20;

/// A cycle that fails (a 503, a timeout, a dropped connection) is retried at the
/// next scheduled poll — never immediately, so a failing store is not hammered
/// with Class A calls. This many failures IN A ROW end the watch.
pub const MAX_CONSECUTIVE_FAILED_CYCLES: u64 = 5;

struct JournalSaver {
    pending: u64,
    last: Instant,
}

impl JournalSaver {
    fn new() -> JournalSaver {
        JournalSaver {
            pending: 0,
            last: Instant::now(),
        }
    }

    fn note(&mut self, journal: &mut WatchJournal) -> Result<()> {
        self.pending += 1;
        if self.pending >= JOURNAL_SAVE_EVERY_OBJECTS || self.last.elapsed() >= JOURNAL_SAVE_EVERY {
            journal.save()?;
            self.pending = 0;
            self.last = Instant::now();
        }
        Ok(())
    }
}

enum ScanOutcome {
    Complete(Vec<ObjectMeta>),
    /// The Class A allowance left this month ran out before the listing did.
    /// A partial listing is not the truth, so nothing is derived from it.
    BudgetExhausted,
}

/// List the whole watched key space (or, in append-only mode, its tail).
///
/// `calls` is incremented per request SENT, including one that then fails, so a
/// failing listing is still charged to the ledger. `max_calls` is the Class A
/// allowance left this month; the scan stops before exceeding it.
///
/// **The ledger is written before the calls it pays for.** A scan can issue up
/// to `max_calls` requests — 200,000 on a fresh month at the default budget —
/// and charging the whole cycle after the loop meant a process killed inside it
/// recorded nothing at all: the #968 verification killed a 2,001-call scan five
/// times, the store served 241 list calls, and `objwatch-spend.json` was never
/// created. A supervisor restarting a crashing watcher then got a fresh budget
/// every time, which is exactly what the ledger exists to prevent.
///
/// So the loop reserves [`cost::CLASS_A_CHARGE_BATCH`] operations at a time,
/// persisting the ledger first, and refunds the unused tail of the last
/// reservation when it ends. A crash over-records by at most a batch (the safe
/// direction); a completed scan records exactly what the store served.
#[allow(clippy::too_many_arguments)]
fn scan(
    src: &dyn ObjectSource,
    prefix: &str,
    start_after: Option<&str>,
    page_size: u64,
    max_calls: u64,
    calls: &mut u64,
    ledger: &mut SpendLedger,
    stop: &dyn Fn() -> bool,
) -> Result<ScanOutcome> {
    let mut out: Vec<ObjectMeta> = Vec::new();
    let mut token: Option<String> = None;
    let mut made = 0u64;
    // Charged to the ledger and not yet spent. Refunded on every exit path.
    let mut reserved = 0u64;
    let outcome = (|| -> Result<ScanOutcome> {
        loop {
            if made >= max_calls {
                return Ok(ScanOutcome::BudgetExhausted);
            }
            if reserved == 0 {
                let want = cost::CLASS_A_CHARGE_BATCH.min(max_calls - made);
                ledger.reserve_class_a(want)?;
                reserved = want;
            }
            made += 1;
            reserved -= 1;
            *calls += 1;
            let page = src.list(&ListRequest {
                prefix,
                continuation_token: token.as_deref(),
                start_after: if token.is_some() { None } else { start_after },
                delimiter: None,
                max_keys: page_size,
            })?;
            out.extend(page.objects);
            match page.next_token {
                Some(t) => token = Some(t),
                None => break,
            }
            if stop() {
                // A cancelled scan is a partial listing: the caller must not
                // treat it as authoritative, so say so rather than returning a
                // truncated "truth".
                bail!("listing cancelled after {made} call(s); no change set was derived");
            }
        }
        Ok(ScanOutcome::Complete(std::mem::take(&mut out)))
    })();
    ledger.refund_class_a(reserved);
    outcome
}

/// Fetch `keys` with at most [`FETCH_CONCURRENCY`] GETs in flight. Results come
/// back in the order of `keys`, so events are still emitted in listing order.
fn fetch_batch(
    src: &dyn ObjectSource,
    keys: &[&str],
    max_bytes: u64,
) -> Vec<Result<FetchedObject>> {
    if keys.len() <= 1 {
        return keys.iter().map(|k| src.get(k, max_bytes)).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Result<FetchedObject>>>> =
        keys.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..FETCH_CONCURRENCY.min(keys.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= keys.len() {
                    break;
                }
                let r = src.get(keys[i], max_bytes);
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            });
        }
    });
    slots
        .into_iter()
        .map(|m| {
            m.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_else(|| Err(anyhow::anyhow!("fetch was never attempted")))
        })
        .collect()
}

/// One poll cycle: check the budget, list, diff, fetch what changed, feed the
/// sink, record.
///
/// Every cycle can refuse on cost, not only the first. A bucket that grows
/// past the budget while it is watched, or a month whose allowance is spent,
/// stops the watch with a [`PollCostRefused`] (exit 4) unless the operator
/// passed `--allow-cost`. `cycle_index` only changes the wording.
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
    let interval_millis = opts.poll_interval.as_millis().min(u128::from(u64::MAX)) as u64;
    let mut ledger = SpendLedger::open(journal.state_dir())?;

    // An append-only watch with nothing recorded yet has to list the whole key
    // space once to find its highest key. That backfill is a one-time cost; the
    // RECURRING cost is the tail above `start-after`.
    let append_backfill = opts.append_only && journal.max_key_seen.is_none();
    let start_after = if opts.append_only {
        journal.max_key_seen.clone()
    } else {
        None
    };

    let refuse = |reason: RefusalReason, projection: Projection, spent: MonthSpend, needed: u64| {
        anyhow::Error::from(PollCostRefused {
            reason,
            projection,
            location: src.describe(),
            cycle: cycle_index,
            spent,
            budget_class_b: opts.max_monthly_class_b,
            needed,
        })
    };

    // Breaker 1, before a single call: is there Class A allowance left this
    // month for the listing this cycle is about to make?
    let expected_calls = if opts.append_only && !append_backfill {
        1
    } else {
        cost::list_calls_for_keys_at(journal.len() as u64, opts.page_size)
    };
    let allowance_left = if opts.allow_cost {
        u64::MAX
    } else {
        opts.max_monthly_class_a
            .saturating_sub(ledger.spend.class_a)
    };
    let pre_projection = |keys: u64, calls: u64| {
        Projection::from_millis(keys, calls, interval_millis, opts.max_monthly_class_a)
    };
    if expected_calls > allowance_left {
        return Err(refuse(
            RefusalReason::ClassABudgetSpent,
            pre_projection(journal.len() as u64, expected_calls),
            ledger.spend.clone(),
            expected_calls,
        ));
    }

    let mut list_calls = 0u64;
    let scanned = scan(
        src,
        &prefix,
        start_after.as_deref(),
        opts.page_size,
        allowance_left,
        &mut list_calls,
        &mut ledger,
        stop,
    );
    totals.list_calls += list_calls;
    // `scan` already charged and persisted every call it made, batch by batch,
    // and refunded the unused tail in memory. This save writes the exact
    // figure — and runs before `scanned?` below, so a failed listing is still
    // recorded.
    ledger.save()?;
    let listed = match scanned? {
        ScanOutcome::Complete(listed) => listed,
        ScanOutcome::BudgetExhausted => {
            let needed = cost::list_calls_for_keys_at(journal.len() as u64, opts.page_size);
            return Err(refuse(
                RefusalReason::ClassABudgetSpent,
                pre_projection(journal.len() as u64, needed.max(list_calls + 1)),
                ledger.spend.clone(),
                needed.max(list_calls + 1),
            ));
        }
    };
    totals.keys_listed += listed.len() as u64;

    // Breaker 2: what the NEXT cycle will list, priced at the page size this
    // watch actually uses. That is the recurring cost.
    //
    // Full scan: the whole key space, for which the journal is a better
    // estimate than one listing (a listing taken mid-delete is smaller than
    // the space it covers).
    //
    // Append-only: only the tail above `start-after`. Pricing it at journal
    // size refused the very escape hatch the refusal recommends, and pricing
    // the one-time backfill as if it recurred did the same thing on cycle 0.
    let (projected_keys, projected_calls) = if opts.append_only {
        if append_backfill {
            (0, 1)
        } else {
            let tail = listed.len() as u64;
            (tail, cost::list_calls_for_keys_at(tail, opts.page_size))
        }
    } else {
        let keys = (journal.len() as u64).max(listed.len() as u64);
        // `.max(list_calls)`: `ceil(keys / page_size)` is the FLOOR, not the
        // bill. When a store stops paging is implementation-defined — it may
        // set `IsTruncated` on a page it has just filled and only reveal that
        // there is nothing after it on the next call. Measured against MinIO at
        // page size 1,000: 1,000 objects -> 1 call, 2,000 -> 2, but 10,000 ->
        // 11. So "the count divides exactly" is NOT the rule (1,000 and 2,000
        // divide exactly and cost no extra page); what the store actually
        // served this cycle is the truth, and the projection takes whichever is
        // larger.
        (
            keys,
            cost::list_calls_for_keys_at(keys, opts.page_size).max(list_calls),
        )
    };
    let projection = pre_projection(projected_keys, projected_calls);
    let mut warnings: Vec<String> = Vec::new();
    if append_backfill && list_calls > 1 {
        warnings.push(format!(
            "append-only backfill: this first cycle listed {} key(s) in {list_calls} call(s), \
             once; later cycles list only the keys above the highest one seen",
            listed.len()
        ));
    }
    if projection.over_budget() {
        if !opts.allow_cost {
            return Err(refuse(
                RefusalReason::ProjectedOverBudget,
                projection,
                ledger.spend.clone(),
                projected_calls,
            ));
        }
        warnings.push(format!(
            "poll cost is over budget and --allow-cost accepted it: {}",
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
        truncated: 0,
        month_to_date: ledger.spend.clone(),
        errors: Vec::new(),
        warnings,
        projection,
        totals: totals.clone(),
    };

    // A dry run reports the change set it WOULD emit and records nothing: no
    // GET, no sink, no journal write. Recording it would make the next real run
    // treat the whole bucket as already indexed.
    if opts.dry_run {
        report.added = plan.added.len() as u64;
        report.changed = plan.changed.len() as u64;
        report.deleted = plan.deleted.len() as u64;
        totals.cycles += 1;
        report.wall = started.elapsed();
        report.totals = totals.clone();
        return Ok(report);
    }

    // Breaker 3, before any GET: does this cycle's fetching fit the Class B
    // budget? Checked up front so a refused cycle has emitted nothing.
    let planned_gets = if opts.fetch {
        (plan.added.len() + plan.changed.len()) as u64
    } else {
        0
    };
    if !opts.allow_cost
        && planned_gets > 0
        && ledger.spend.class_b.saturating_add(planned_gets) > opts.max_monthly_class_b
    {
        return Err(refuse(
            RefusalReason::ClassBBudgetSpent,
            report.projection.clone(),
            ledger.spend.clone(),
            planned_gets,
        ));
    }

    let mut saver = JournalSaver::new();

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
                saver.note(journal)?;
            }
            Err(e) => record_error(&mut report, totals, format!("delete {key}: {e}")),
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

    let mut at = 0usize;
    while at < work.len() {
        if stop() {
            break;
        }
        // The next batch: bounded by count and by the bytes it may hold.
        let mut end = at;
        let mut batch_bytes = 0u64;
        while end < work.len() && end - at < FETCH_BATCH_OBJECTS {
            let est = work[end].0.size.min(opts.max_object_bytes);
            if end > at && batch_bytes.saturating_add(est) > FETCH_BATCH_BYTES {
                break;
            }
            batch_bytes = batch_bytes.saturating_add(est);
            end += 1;
        }
        let batch = &work[at..end];
        at = end;

        let mut fetched: Vec<Option<Result<FetchedObject>>> = if opts.fetch {
            let keys: Vec<&str> = batch.iter().map(|(m, _, _)| m.key.as_str()).collect();
            let results = fetch_batch(src, &keys, opts.max_object_bytes);
            // Every GET sent is counted, including one that failed: the
            // ledger assumes the provider bills it.
            let sent = results.len() as u64;
            report.gets += sent;
            totals.gets += sent;
            ledger.add(0, sent);
            results.into_iter().map(Some).collect()
        } else {
            batch.iter().map(|_| None).collect()
        };

        for ((meta, kind, reason), fetch) in batch.iter().zip(fetched.drain(..)) {
            let (kind, reason) = (*kind, *reason);
            let prev = journal.get(&meta.key);
            let prev_digest = prev.and_then(|k| k.digest.clone());
            let prev_truncated = prev.map(|k| k.truncated).unwrap_or(false);
            let mut digest = None;
            let mut etag_used = meta.etag.clone();
            let mut fetch_had_no_etag = false;
            let mut truncated = false;
            let mut bytes_fetched = 0u64;
            match fetch {
                None => {}
                Some(Ok(f)) => {
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
                Some(Err(e)) => {
                    record_error(&mut report, totals, format!("fetch {}: {e}", meta.key));
                    // Leave the journal entry alone so the next cycle retries.
                    continue;
                }
            }
            if truncated {
                report.truncated += 1;
            }
            // The bytes hash the same as what is already recorded, so the
            // metadata change was ETag or mtime churn (a copy, a lifecycle
            // rewrite, a gateway that re-stamps on read) and not an edit. Update
            // the journal so the next cycle is free again, and do NOT re-index.
            //
            // Only when BOTH digests cover the whole object. A digest of a
            // capped fetch covers a prefix, so "same prefix" says nothing about
            // the tail: an edit past `--max-object-mb` hashed identical and was
            // silently dropped (F4 of the #968 review). A capped object whose
            // metadata changed is always emitted, flagged `truncated`.
            let content_identical = opts.fetch
                && !truncated
                && !prev_truncated
                && prev_digest.is_some()
                && prev_digest == digest;
            // A GET that carried no ETag means the recorded ETag came from the
            // listing and might describe bytes we did not read. Flag it so the
            // next cycle re-reads once and confirms by digest — and clear the
            // flag as soon as that confirmation happens, so a store that never
            // sends ETags costs one extra GET per change rather than one per
            // cycle forever.
            let etag_unverified = fetch_had_no_etag && !content_identical;
            let entry = WatchedObject {
                etag: etag_used.clone(),
                size: meta.size,
                last_modified: meta.last_modified.clone(),
                digest: digest.clone(),
                etag_unverified,
                seen_at: journal::now_rfc3339(),
                truncated,
            };
            if content_identical {
                journal.record(&meta.key, entry);
                report.content_identical += 1;
                report.unchanged += 1;
                saver.note(journal)?;
                continue;
            }
            let ev = ChangeEvent {
                kind,
                key: meta.key.clone(),
                size: meta.size,
                etag: etag_used,
                last_modified: meta.last_modified.clone(),
                digest,
                bytes_fetched,
                reason,
                truncated,
            };
            match sink.accept(&ev) {
                Ok(()) => {
                    journal.record(&meta.key, entry);
                    match kind {
                        ChangeKind::Added => report.added += 1,
                        ChangeKind::Changed => report.changed += 1,
                        ChangeKind::Deleted => {}
                    }
                    totals.events += 1;
                    saver.note(journal)?;
                }
                Err(e) => record_error(&mut report, totals, format!("index {}: {e}", meta.key)),
            }
        }
        ledger.save()?;
    }

    if report.truncated > 0 {
        report.warnings.push(format!(
            "{} object(s) are larger than --max-object-mb ({}): their digest covers only the \
             first {}, so a change to one is detected from its ETag/size/mtime and always \
             emitted with truncated=true",
            report.truncated,
            human_bytes(opts.max_object_bytes),
            human_bytes(opts.max_object_bytes)
        ));
    }

    sink.flush()?;
    journal.note_cycle();
    journal.save()?;
    ledger.save()?;
    totals.cycles += 1;
    report.wall = started.elapsed();
    report.month_to_date = ledger.spend.clone();
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

/// Poll until `stop()`, `max_cycles`, a cost breaker, or a fatal error.
///
/// A cycle that overruns the interval does not queue: the missed deadlines are
/// counted (`skipped_deadlines`) and the next cycle starts immediately. Cycles
/// never overlap, which is what makes "no double index" structural rather than
/// a race the sink has to defend against.
///
/// A cycle that FAILS after the first one (a 503, a timeout) is reported and
/// retried at the next scheduled poll; [`MAX_CONSECUTIVE_FAILED_CYCLES`] in a
/// row end the watch. The first cycle's failure is returned at once, because on
/// a fresh start it is almost always configuration (wrong endpoint, key, or
/// bucket), which waiting will not fix. A cost refusal is never retried.
pub fn run_watch(
    src: &dyn ObjectSource,
    journal: &mut WatchJournal,
    opts: &WatchOptions,
    sink: &mut dyn ChangeSink,
    stop: &dyn Fn() -> bool,
    report: &mut dyn FnMut(&CycleReport),
) -> Result<WatchOutcome> {
    let mut totals = CostTotals::default();
    let mut last: Option<CycleReport> = None;
    let mut cycle = 0u64;
    let mut failed_in_a_row = 0u64;
    // Cycle k is scheduled for t0 + k * interval. Fixing the schedule up front
    // is what lets an overrun be *reported* (`skipped_deadlines`) instead of
    // silently stretching the interval an operator budgeted with.
    let mut next_start = Instant::now() + opts.poll_interval;
    loop {
        let (r, ok) = match poll_once(src, journal, opts, sink, cycle, &mut totals, stop) {
            Ok(r) => {
                failed_in_a_row = 0;
                (r, true)
            }
            Err(e) if stop() => {
                // A cancelled listing is how ^C looks mid-scan: not a failure.
                let _ = e;
                break;
            }
            Err(e) => {
                let retryable = e.downcast_ref::<PollCostRefused>().is_none();
                let Some(prev) = last.as_ref().filter(|_| retryable) else {
                    return Err(e);
                };
                failed_in_a_row += 1;
                totals.errors += 1;
                totals.failed_cycles += 1;
                if failed_in_a_row >= MAX_CONSECUTIVE_FAILED_CYCLES {
                    return Err(e.context(format!(
                        "{failed_in_a_row} poll cycles failed in a row; stopping the watch"
                    )));
                }
                let failed = CycleReport {
                    cycle,
                    started_at: journal::now_rfc3339(),
                    wall: Duration::ZERO,
                    list_calls: 0,
                    keys_listed: 0,
                    gets: 0,
                    bytes_fetched: 0,
                    added: 0,
                    changed: 0,
                    deleted: 0,
                    unchanged: 0,
                    content_identical: 0,
                    truncated: 0,
                    month_to_date: SpendLedger::open(journal.state_dir())
                        .map(|l| l.spend)
                        .unwrap_or_else(|_| prev.month_to_date.clone()),
                    errors: vec![format!(
                        "cycle failed ({failed_in_a_row} of {MAX_CONSECUTIVE_FAILED_CYCLES} \
                         allowed in a row); retrying at the next poll: {e:#}"
                    )],
                    warnings: Vec::new(),
                    projection: prev.projection.clone(),
                    totals: totals.clone(),
                };
                (failed, false)
            }
        };
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
        // A failed cycle's report is shown, but `last` keeps the last cycle
        // that actually listed, so the projection it carries is real.
        if ok {
            last = Some(r);
        }
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
            // a backlog forever, and start the next cycle immediately.
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
