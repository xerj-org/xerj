//! Process-wide resource governor — the parent circuit breaker for the
//! ingest and search paths.
//!
//! Per-index back-pressure (the memtable soft/hard block in
//! [`crate::index`]) only ever bounds ONE index at `~3×flush_size_mb`. With
//! `N` indices there was no global ceiling: `N × ~1.5 GiB` of memtable could
//! accumulate until the kernel OOM-killed the process — the structural cause
//! of the 112 GiB incident. This module is the missing ceiling. It adds:
//!
//!   * **item 1** — a process-wide memtable byte budget, plus an RSS
//!     admission watermark measured against the cgroup/system memory limit.
//!     Crossing either rejects writes with HTTP 429
//!     `circuit_breaking_exception` (so a 429 beats the OOM-killer), and
//!     wires the hitherto-inert `max_query_memory_mb` into a per-query
//!     allocation guard. An analyze reservation charges in-flight token
//!     spikes before the next RSS sample, so several bulks cannot all pass
//!     one sample and allocate together (#1122).
//!   * **item 2** — a global search-concurrency pool sized from
//!     `max_concurrent_searches` (previously a hardcoded per-index
//!     `Semaphore::new(64)`, i.e. no global cap).
//!   * **item 3** — a disk flood-stage write block driven by a background
//!     `statvfs` poll, mirroring Elasticsearch's
//!     `disk.watermark.flood_stage`.
//!
//! A single process-wide [`OnceLock`] holds the governor. [`init`]
//! initialises it from config; [`Engine::spawn_resource_sampler`] refreshes
//! the RSS / memtable / disk atomics every ~250 ms, so the hot-path
//! admission checks are relaxed atomic loads — never syscalls.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use xerj_common::config::Config;
use xerj_common::XerjError;

use crate::segment_cache_budget::{SegmentHydrationBudget, SegmentHydrationBudgetSnapshot};

/// The process-wide governor singleton.
static GOVERNOR: OnceLock<Arc<ResourceGovernor>> = OnceLock::new();

/// Decides when the sampler may request another memory-pressure drain.
///
/// The engage edge fires immediately. While the breaker stays engaged, the
/// next request waits `interval`, which starts at the 2 s floor and doubles
/// up to [`MEMORY_DRAIN_MAX_INTERVAL`] whenever a request did not lower RSS.
/// A drop resets the spacing, and a climb past the noise band requests a
/// drain as soon as the minimum spacing allows (#2427). Disengaging resets everything, so the next
/// real engage is immediate again.
#[derive(Debug)]
pub(crate) struct MemoryDrainSchedule {
    interval: Duration,
    last_request: Option<Instant>,
    rss_at_request: Option<u64>,
    min: Duration,
}

impl MemoryDrainSchedule {
    pub(crate) fn new(min: Duration) -> Self {
        Self {
            interval: min,
            last_request: None,
            rss_at_request: None,
            min,
        }
    }

    /// `true` when this sample should request a drain.
    pub(crate) fn observe(&mut self, now: Instant, rss: u64, engaged: bool) -> bool {
        if !engaged {
            self.interval = self.min;
            self.last_request = None;
            self.rss_at_request = None;
            return false;
        }
        // #2427: the latch no longer releases above the watermark, so a
        // footprint that CLIMBS while engaged must not sit out a backed-off
        // gap (up to 15 min) — the release gate climbed from 5.1 GB to an
        // 8.4 GB OOM in 84 s. A climb past the noise band since the last
        // request is new pressure: drain again at the minimum spacing.
        let climbed = self
            .rss_at_request
            .is_some_and(|prev| rss.saturating_sub(prev) > MEMORY_RSS_NOISE_BYTES);
        let due = match self.last_request {
            None => true,
            Some(at) => {
                let since = now.saturating_duration_since(at);
                since >= self.interval || (climbed && since >= self.min)
            }
        };
        if !due {
            return false;
        }
        if climbed {
            self.interval = self.min;
            self.rss_at_request = Some(rss);
            self.last_request = Some(now);
            return true;
        }
        if let Some(prev) = self.rss_at_request {
            // A one-page dip is compressor noise, not a drain that worked.
            // Resetting on it would pin the spacing at 2 s for a footprint
            // that never actually shrinks (#1981).
            if prev.saturating_sub(rss) > MEMORY_RSS_NOISE_BYTES {
                self.interval = self.min;
            } else {
                self.interval = self
                    .interval
                    .checked_mul(2)
                    .unwrap_or(MEMORY_DRAIN_MAX_INTERVAL)
                    .min(MEMORY_DRAIN_MAX_INTERVAL);
            }
        }
        self.rss_at_request = Some(rss);
        self.last_request = Some(now);
        true
    }
}

/// Interval at which the background sampler refreshes the RSS / memtable /
/// disk atomics. Kept short so the RSS admission has a tight overshoot margin
/// under a runaway ingest — the sampler runs on a dedicated OS thread (see
/// `Engine::spawn_resource_sampler`), so this cadence is honoured even when the
/// tokio pool is saturated.
pub const SAMPLE_INTERVAL_MS: u64 = 100;

/// Ceiling on the automatic drain spacing once a drain stops lowering RSS.
/// The floor is the sampler's 2 s minimum; doubling from there reaches this
/// in a few minutes instead of staying at 2 s for an 18-hour uptime.
pub const MEMORY_DRAIN_MAX_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// RSS movement at or below this is not a falling drain and not new
/// pressure (#1981).
///
/// macOS reports `ri_phys_footprint`, which moves by pages while the
/// compressor runs even when the process is idle. An exact-byte clock never
/// stays flat on that signal, and a one-byte dip would also reset the drain
/// backoff, so the 2 s flush loop would survive the latch fix. A move past
/// this band is a real working-set change.
pub const MEMORY_RSS_NOISE_BYTES: u64 = 4 * 1024 * 1024;

/// Default release point of the RSS breaker, as a percentage of the
/// watermark that tripped it (#2427).
///
/// The old band was 95% and released on the first sample that did not fall.
/// A 3,000 MB autoindex run engaged at 2,918 MB and flapped 20 times in 16
/// minutes, often releasing 100-500 ms after it engaged: each release
/// re-admitted the write burst that tripped it. Releasing at 85% leaves a
/// band the drain has to clear before writes come back.
/// `XERJ_MEMORY_BREAKER_RELEASE_PCT` overrides it (50..=99).
pub const DEFAULT_MEMORY_RELEASE_PERCENT: u8 = 85;

/// Share of the process memory limit held back for searches, in percent
/// (#2427 read-budget isolation).
///
/// Background merges and their output writes are charged to a merge pool
/// whose ceiling is the limit minus this share (or the watermark, if that is
/// lower). They are refused, and an in-flight batch yields, once process
/// memory reaches that ceiling. Searches are only rejected at the full limit,
/// so the top of the budget stays free for reads instead of being the first
/// thing a merge takes. On the 1 GB gate (3,072 MB cap) the merge pool stops
/// at about 2,458 MB and reads keep the last 614 MB.
pub const SEARCH_READ_RESERVE_PERCENT: u64 = 20;

/// How long a search that arrives with process memory at or above the hard
/// limit waits for the breaker's drain before it is rejected with a 429
/// (#2427 read-budget isolation).
///
/// The drain (flush, cache release, allocator purge) takes RSS from the limit
/// back to the live set in one to two seconds. In the 1 GB gate the raw and
/// MCP searches arrived within 300 ms of a heavy search and were all
/// rejected in 4-14 ms, although RSS was back to about 1.5 GB a second later.
pub const SEARCH_HEADROOM_WAIT: Duration = Duration::from_secs(3);

/// Poll interval while a search waits for headroom. The sampler writes RSS
/// every 100 ms, so polling faster only re-reads the same sample.
const SEARCH_HEADROOM_POLL: Duration = Duration::from_millis(25);

/// Lowest accepted `XERJ_MEMORY_BREAKER_RELEASE_PCT`. Below this, a footprint
/// whose live set is over half the watermark could never release.
pub const MIN_MEMORY_RELEASE_PERCENT: u8 = 50;

/// Highest accepted `XERJ_MEMORY_BREAKER_RELEASE_PCT`.
pub const MAX_MEMORY_RELEASE_PERCENT: u8 = 99;

/// Default minimum time the RSS breaker stays engaged before a release below
/// the watermark (#2427). `XERJ_MEMORY_BREAKER_MIN_ENGAGED_MS` overrides it.
///
/// This stops a 100 ms engage/release cycle: a release needs the drain to
/// have run at least once. A live retune (#1323) or Windows commit recovery
/// are not subject to it. No release ever happens at or above the watermark.
pub const DEFAULT_MEMORY_MIN_ENGAGED: Duration = Duration::from_secs(2);

/// Ceiling on `XERJ_MEMORY_BREAKER_MIN_ENGAGED_MS`.
pub const MAX_MEMORY_MIN_ENGAGED: Duration = Duration::from_secs(60);

/// How long RSS may sit without falling past [`MEMORY_RSS_NOISE_BYTES`]
/// inside the release band `[release, watermark)` before the breaker stops
/// waiting for the drain and releases (#2427).
///
/// Without this the band could hold writes forever on a live set between
/// 85% and 100% of the watermark that no drain can shrink (the #1250
/// concern). Thirty seconds is fifteen drain intervals, and bounds a
/// band-stall re-engage cycle to at most one every 30 s.
pub const MEMORY_BAND_STALL_RELEASE: Duration = Duration::from_secs(30);

/// Bytes at which an engaged RSS breaker releases: `release_pct` percent of
/// `watermark`. `0` for a disabled watermark.
pub fn memory_release_bytes(watermark: u64, release_pct: u8) -> u64 {
    ((watermark as u128 * u128::from(release_pct.min(100))) / 100) as u64
}

/// Parse `XERJ_MEMORY_BREAKER_RELEASE_PCT`. Unset, unparsable, or out of
/// `MIN..=MAX` falls back to [`DEFAULT_MEMORY_RELEASE_PERCENT`].
pub fn resolve_memory_release_percent(raw: Option<&str>) -> u8 {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_MEMORY_RELEASE_PERCENT;
    };
    match raw.trim_end_matches('%').parse::<u8>() {
        Ok(pct) if (MIN_MEMORY_RELEASE_PERCENT..=MAX_MEMORY_RELEASE_PERCENT).contains(&pct) => pct,
        _ => {
            tracing::warn!(
                value = raw,
                default = DEFAULT_MEMORY_RELEASE_PERCENT,
                "XERJ_MEMORY_BREAKER_RELEASE_PCT must be {MIN_MEMORY_RELEASE_PERCENT}..={MAX_MEMORY_RELEASE_PERCENT}; using the default"
            );
            DEFAULT_MEMORY_RELEASE_PERCENT
        }
    }
}

/// Parse `XERJ_MEMORY_BREAKER_MIN_ENGAGED_MS`. Unset or unparsable falls back
/// to [`DEFAULT_MEMORY_MIN_ENGAGED`]; larger values are clamped to
/// [`MAX_MEMORY_MIN_ENGAGED`]. `0` turns the minimum off.
pub fn resolve_memory_min_engaged(raw: Option<&str>) -> Duration {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_MEMORY_MIN_ENGAGED;
    };
    match raw.parse::<u64>() {
        Ok(ms) => Duration::from_millis(ms).min(MAX_MEMORY_MIN_ENGAGED),
        Err(_) => {
            tracing::warn!(
                value = raw,
                "XERJ_MEMORY_BREAKER_MIN_ENGAGED_MS is not a whole number of milliseconds; using the default"
            );
            DEFAULT_MEMORY_MIN_ENGAGED
        }
    }
}

/// Bytes of analyze-time reservation charged per raw JSON input byte (#1122).
///
/// `document_limit` measured about 56 bytes of RSS per token for a
/// `Vec<String>` of the word `word ` (2026-10-23, 8,388,605 input bytes).
/// The densest input that still produces one token per two bytes is `"a "`,
/// so 28 bytes of reservation per input byte covers that spike. One token
/// per byte is outside this factor; the 8 MiB document ceiling bounds that
/// input. Eight such bulks are admitted only while the sum of reservations
/// stays under the watermark.
pub const ANALYZE_RESERVE_PER_INPUT_BYTE: u64 = 28;

/// Reservation for a raw JSON batch of `raw_json_bytes`.
pub fn analyze_reserve_bytes(raw_json_bytes: u64) -> u64 {
    raw_json_bytes.saturating_mul(ANALYZE_RESERVE_PER_INPUT_BYTE)
}

/// Bytes of analyze-time reservation charged per estimated token (#2427).
///
/// The same `document_limit` measurement as
/// [`ANALYZE_RESERVE_PER_INPUT_BYTE`]: about 56 bytes of RSS per token. A
/// token is a `Token` (a `String` plus three `u32`s) in a growing `Vec`, plus
/// the token text on the heap.
pub const ANALYZE_RESERVE_PER_TOKEN: u64 = 56;

/// Bytes of analyze-time reservation charged per raw JSON input byte on top
/// of the per-token charge (#2427): the parsed `serde_json::Value` (string
/// bodies, map keys and nodes) and the copy of each text field the analyzer
/// reads.
pub const ANALYZE_RESERVE_PER_INPUT_BYTE_BASE: u64 = 4;

/// Upper bound on the analyzer tokens `raw_json` can produce (#2427).
///
/// Counts runs of ASCII letters and digits, plus one token per non-ASCII
/// character (the standard tokenizer emits one token per CJK ideograph). Map
/// keys, numbers, `_` and `.` all start or split runs here, which the
/// analyzer joins or never analyzes, so the count errs high. A packed
/// `xerj:f32:` vector is one run of hex, matching the one string the parser
/// keeps for it.
pub fn estimated_analyzer_tokens(raw_json: &[u8]) -> u64 {
    let mut tokens = 0u64;
    let mut in_run = false;
    for &b in raw_json {
        if b >= 0xC0 {
            // UTF-8 lead byte: count every non-ASCII character.
            tokens += 1;
            in_run = true;
        } else if b >= 0x80 {
            // UTF-8 continuation byte: same character.
            in_run = true;
        } else if b.is_ascii_alphanumeric() {
            if !in_run {
                tokens += 1;
            }
            in_run = true;
        } else {
            in_run = false;
        }
    }
    tokens
}

/// Reservation for one raw JSON document (#1122, #2427).
///
/// [`ANALYZE_RESERVE_PER_TOKEN`] per [`estimated_analyzer_tokens`] plus
/// [`ANALYZE_RESERVE_PER_INPUT_BYTE_BASE`] per byte, capped at the old flat
/// [`ANALYZE_RESERVE_PER_INPUT_BYTE`] rate. The flat rate assumed the densest
/// input (one token per two bytes) for every document, so a bulk of prose
/// reserved about 3x and a bulk of code about 4x what parsing and analyzing
/// it allocates (`tests/analyze_reservation_matches_usage.rs`). Four 8 MiB
/// bulks in flight reserved 1.4 GB while RSS was 1.46 GB, and the gate's
/// bulks were refused long before real memory was short. Dense input still
/// reserves the flat rate.
pub fn analyze_reserve_bytes_for_doc(raw_json: &[u8]) -> u64 {
    let len = raw_json.len() as u64;
    estimated_analyzer_tokens(raw_json)
        .saturating_mul(ANALYZE_RESERVE_PER_TOKEN)
        .saturating_add(len.saturating_mul(ANALYZE_RESERVE_PER_INPUT_BYTE_BASE))
        .min(analyze_reserve_bytes(len))
}

/// `true` when `request` bytes of analyze reservation still fit.
///
/// A watermark of `0` is the disabled breaker: every request fits. Otherwise
/// the request fails when `observed + already_reserved + request` reaches
/// the watermark. The comparison is `>=`, matching RSS admission.
pub fn analyze_reservation_fits(
    watermark: u64,
    observed: u64,
    already_reserved: u64,
    request: u64,
) -> bool {
    if watermark == 0 {
        return true;
    }
    observed
        .saturating_add(already_reserved)
        .saturating_add(request)
        < watermark
}

/// Process-wide resource governor. See the module docs.
pub struct ResourceGovernor {
    /// One admission authority shared by every index in this process.
    segment_hydration_budget: Arc<SegmentHydrationBudget>,
    segment_hydration_budget_source: SegmentHydrationBudgetSource,
    // ── item 1: process-wide memtable budget ────────────────────────────
    /// Ceiling on summed memtable bytes across ALL indices. `0` = disabled.
    memtable_budget_bytes: u64,
    /// Last sampled sum of every index's memtable footprint.
    memtable_used_bytes: AtomicU64,
    /// Latched: last sample crossed the memtable budget.
    memtable_tripped: AtomicBool,

    // ── item 1: RSS admission watermark ─────────────────────────────────
    /// Effective process memory limit (cgroup limit, else system RAM).
    memory_limit_bytes: u64,
    /// Uncapped machine/cgroup memory the process can see — the base a
    /// percentage override is taken against and the hard ceiling an absolute
    /// override is clamped to. Distinct from `memory_limit_bytes`, which is the
    /// AUTO-tier *capped* value (e.g. 16 GiB on a 119 GiB box): a live override
    /// exists precisely to climb past that conservative cap, so it must be able
    /// to reach up to real machine memory, never just a fraction of the cap.
    /// On a commit-bound Windows box this stops at the post-reserve commit
    /// ceiling instead: a raise cannot grant commit the system will not give.
    machine_memory_bytes: u64,
    /// Boot-derived RSS admission threshold = `memory_limit_bytes * pct/100`.
    /// `0` = the watermark was disabled at startup and stays disabled; a runtime
    /// override is then a no-op (see [`Self::set_memory_watermark_override`]).
    memory_watermark_bytes_default: u64,
    /// Effective RSS admission threshold engaging the parent breaker —
    /// `memory_watermark_bytes_default` unless overridden at runtime via the
    /// `indices.breaker.total.limit` persistent cluster setting (see
    /// [`Self::set_memory_watermark_override`]), which lets an operator raise
    /// the watermark on a big box without a restart — the live equivalent of
    /// the boot-only `XERJ_MAX_PROCESS_MEMORY_MB`. `0` = off.
    memory_watermark_bytes: AtomicU64,
    /// Last sampled resident set size of this process.
    rss_bytes: AtomicU64,
    /// Bytes reserved for in-flight analysis spikes that the RSS sample has
    /// not observed yet (#1122). Released once those tokens exist, so the
    /// next admission sees them in live RSS instead of counting them twice.
    analyze_reserved: Arc<AtomicU64>,
    /// Latched: last sample crossed the RSS watermark.
    memory_tripped: AtomicBool,
    /// The effective watermark in force when the breaker last engaged (#1250).
    /// The release hysteresis is only meaningful against the watermark that
    /// tripped; when an operator re-tunes the watermark live
    /// (`indices.breaker.total.limit`, #1323) the latch is re-evaluated
    /// against the new value without the band, so a live raise releases at
    /// once instead of waiting for RSS to fall below the release point of
    /// the NEW watermark.
    memory_trip_watermark: AtomicU64,
    /// When RSS last stopped falling inside the release band while the
    /// breaker was engaged (#2427). `None` outside the band or while
    /// disengaged. Aged by tests; the sampler's wall clock fills it in
    /// production.
    memory_flat_since: Mutex<Option<Instant>>,
    /// Release point of the RSS breaker, percent of the tripped watermark
    /// (#2427). See [`DEFAULT_MEMORY_RELEASE_PERCENT`].
    memory_release_percent: u8,
    /// Minimum time engaged before an in-band or below-band release (#2427).
    memory_min_engaged: Duration,
    /// When the RSS breaker last engaged. `None` while released.
    memory_engaged_since: Mutex<Option<Instant>>,
    /// RSS at which the in-band stall clock started (#2427). A sample more
    /// than [`MEMORY_RSS_NOISE_BYTES`] under it is a falling drain and
    /// restarts the clock. `0` = no band clock.
    memory_band_anchor: AtomicU64,
    /// Process-wide ledger of in-flight segment merges and their estimated
    /// working set (#2427). See [`ResourceGovernor::try_reserve_merge`].
    merge_ledger: Arc<Mutex<MergeLedger>>,

    // ── item 1: per-query memory guard (`max_query_memory_mb`) ──────────
    /// Maximum bytes a single query may be estimated to allocate. `0` = off.
    max_query_memory_bytes: u64,

    // ── item 2: global search pool (`max_concurrent_searches`) ──────────
    /// Global search-concurrency permits.
    search_pool: Arc<Semaphore>,
    /// Configured permit count (for observability / stats).
    max_concurrent_searches: usize,
    /// Live count of in-flight search permits (proof of the cap). `Arc` so a
    /// [`SearchPermit`] guard can decrement it safely on drop.
    search_inflight: Arc<AtomicU64>,
    /// High-water mark of concurrent searches observed (proof of the cap).
    search_inflight_peak: Arc<AtomicU64>,
    /// #2427: one permit. While the RSS breaker is engaged every search
    /// also holds this, so searches run one at a time instead of
    /// `max_concurrent_searches` wide.
    search_pressure: Arc<Semaphore>,
    /// How long a search over the hard limit waits for the drain (#2427).
    /// [`SEARCH_HEADROOM_WAIT`] in production; tests shorten it.
    search_headroom_wait: Duration,
    /// Bound on a lone background merge's wait for headroom (#2427).
    /// `merge_budget::MERGE_HEADROOM_MAX_WAIT` in production.
    merge_headroom_max_wait: Duration,

    // ── item 2b: global bulk pool (concurrent bulk parse cap) ─────────
    /// Global bulk-concurrency permits. Caps the number of concurrent
    /// `_bulk` requests in the parse phase, preventing memory amplification
    /// from N concurrent bulks each materializing ~300 B/action before the
    /// memtable admission check fires.
    bulk_pool: Arc<Semaphore>,
    /// Configured bulk permit count (for observability / stats).
    max_concurrent_bulks: usize,

    // ── item 3: disk flood-stage write block ────────────────────────────
    /// Configured used-percentage watermark from startup config. `0` = the
    /// watermark is disabled and stays disabled — [`Self::set_disk_flood_pct_override`]
    /// is a no-op in that case, so a runtime cluster-settings override can
    /// only re-tune an already-active watermark, never turn the feature on.
    disk_flood_pct_default: u8,
    /// Effective used-percentage watermark that engages the write block —
    /// `disk_flood_pct_default` unless overridden at runtime via the
    /// `cluster.routing.allocation.disk.watermark.flood_stage` persistent
    /// cluster setting (see [`Self::set_disk_flood_pct_override`]), which
    /// lets an operator clear/relax the block without a restart, matching
    /// ES's `PUT _cluster/settings` recovery flow.
    disk_flood_pct: AtomicU8,
    /// `false` while the `cluster.routing.allocation.disk.threshold_enabled`
    /// cluster setting is `false`: the flood stage never engages, and an
    /// engaged block is released on the next sample, as in ES.
    disk_threshold_enabled: AtomicBool,
    /// Latched: the data-dir filesystem is at/over the flood-stage watermark.
    disk_blocked: AtomicBool,
    /// Last sampled used-percentage of the data-dir filesystem.
    disk_used_pct: AtomicU64,

    // ── Windows commit ceiling (#1122) ──────────────────────────────────
    // Physical RAM is the AUTO tier's input on every platform. On Windows the
    // allocator aborts when commit charge hits the commit limit, which with
    // no pagefile is about RAM and is already partly spent by other processes.
    // These fields are zero off Windows (and when the probe fails), and the
    // admission path below then behaves exactly as it did before.
    /// `MEMORYSTATUSEX::ullTotalPageFile`. `0` = no commit probe.
    commit_limit_bytes: u64,
    /// Available commit below this engages the breaker even when RSS is under
    /// the watermark. `0` disables the check.
    commit_reserve_bytes: u64,
    /// Last sampled `ullAvailPageFile`.
    available_commit_bytes: AtomicU64,
    /// Latched: available commit is under [`Self::commit_reserve_bytes`].
    commit_starved: AtomicBool,
    /// The process cap was lowered to the commit ceiling at startup.
    commit_capped: bool,
}

impl ResourceGovernor {
    // ── Admission checks (hot path — relaxed atomic loads only) ─────────

    /// Ingest admission. Returns a 429 `circuit_breaking_exception` when the
    /// process is at/over the memtable budget or the RSS watermark. This is
    /// the parent breaker that turns the OOM into a survivable 429.
    pub fn check_ingest_admission(&self) -> Result<(), XerjError> {
        if self.memtable_tripped.load(Ordering::Relaxed) {
            let used = self.memtable_used_bytes.load(Ordering::Relaxed);
            return Err(XerjError::circuit_breaking(format!(
                "[parent] memtable byte budget exceeded: used={}MB, limit={}MB across all \
                 indices; writes rejected to prevent an out-of-memory kill (raise \
                 limits.max_process_memory_mb or XERJ_MAX_PROCESS_MEMORY_MB — the auto \
                 tier derives this budget from that cap — or set limits.max_total_memtable_mb)",
                used / (1024 * 1024),
                self.memtable_budget_bytes / (1024 * 1024),
            )));
        }
        if self.memory_tripped.load(Ordering::Relaxed) {
            let rss = self.rss_bytes.load(Ordering::Relaxed);
            let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
            let starved = self.commit_starved.load(Ordering::Relaxed);
            // Inside the release band (#1250) RSS is already under the
            // watermark while the drain finishes; say so rather than print a
            // false `>=`. A commit-starvation trip with RSS still under the
            // watermark is neither of those: the process has not crossed the
            // RSS line, Windows has run out of commit.
            let relation = if starved && rss < watermark {
                "under"
            } else if rss >= watermark {
                ">="
            } else {
                "draining below"
            };
            let commit = (self.commit_capped || starved).then(|| CommitBreakerDetail {
                limit: self.commit_limit_bytes,
                available: self.available_commit_bytes.load(Ordering::Relaxed),
                reserve: self.commit_reserve_bytes,
            });
            return Err(XerjError::circuit_breaking(memory_breaker_reason(
                rss,
                relation,
                watermark,
                self.memory_limit_bytes,
                commit,
            )));
        }
        Ok(())
    }

    /// Reserve `bytes` of upcoming analysis allocations (#1122).
    ///
    /// Callers pass [`analyze_reserve_bytes_for_doc`] summed over the raw JSON
    /// documents, before the
    /// batch is parsed. The check uses the max of the sampled RSS and a live
    /// probe, plus reservations already held, and rechecks after the atomic
    /// add so two bulks cannot both pass the same snapshot. A watermark of
    /// `0`, or a zero request, reserves nothing. The guard releases on drop.
    pub(crate) fn reserve_analyze(&self, bytes: u64) -> Result<AnalyzeReservation, XerjError> {
        if bytes == 0 || self.memory_watermark_bytes.load(Ordering::Relaxed) == 0 {
            return Ok(AnalyzeReservation::none());
        }
        if self.memory_tripped.load(Ordering::Relaxed) {
            return Err(self.analyze_reservation_refused(bytes));
        }
        let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
        let observed = self.observed_process_memory();
        let already = self.analyze_reserved.load(Ordering::Acquire);
        if !analyze_reservation_fits(watermark, observed, already, bytes) {
            return Err(self.analyze_reservation_refused(bytes));
        }
        let prev = self.analyze_reserved.fetch_add(bytes, Ordering::AcqRel);
        let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
        if watermark == 0 {
            self.analyze_reserved.fetch_sub(bytes, Ordering::AcqRel);
            return Ok(AnalyzeReservation::none());
        }
        let observed = self.observed_process_memory();
        if analyze_reservation_fits(watermark, observed, prev, bytes) {
            return Ok(AnalyzeReservation {
                slot: Some(Arc::clone(&self.analyze_reserved)),
                bytes,
            });
        }
        self.analyze_reserved.fetch_sub(bytes, Ordering::AcqRel);
        Err(self.analyze_reservation_refused(bytes))
    }

    /// Admit one `_bulk` request before its body is split and parsed (#2427).
    ///
    /// Every write path checks the breaker, but only after the bulk parse
    /// has already built its per-action line, pair and document-copy vectors.
    /// A request that arrives while a breaker is latched still did all of that
    /// work, and concurrent bulks between two sampler ticks were admitted
    /// on a sample that predates each other's parse buffers. This refuses the
    /// request with the same 429 when either latch is engaged, and otherwise
    /// reserves `body_bytes` against live RSS plus every other in-flight
    /// reservation, so parse buffers are counted before they are allocated.
    /// The caller drops the reservation when the parse ends; the per-index
    /// write path then takes its own analyze reservation for the tokens.
    pub(crate) fn admit_bulk_parse(
        &self,
        body_bytes: u64,
    ) -> Result<AnalyzeReservation, XerjError> {
        self.check_ingest_admission()?;
        self.reserve_analyze(body_bytes)
    }

    fn observed_process_memory(&self) -> u64 {
        self.rss_bytes
            .load(Ordering::Relaxed)
            .max(current_process_memory_bytes())
    }

    fn analyze_reservation_refused(&self, request: u64) -> XerjError {
        const MIB: u64 = 1024 * 1024;
        let observed = self.observed_process_memory();
        let reserved = self.analyze_reserved.load(Ordering::Relaxed);
        let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
        XerjError::circuit_breaking(format!(
            "[parent] real memory circuit breaker tripped: rss={}MB plus {}MB already reserved \
             for analysis plus {}MB for this batch would reach watermark={}MB ({}% of \
             limit={}MB); writes rejected to prevent an out-of-memory kill (raise \
             limits.max_process_memory_mb or XERJ_MAX_PROCESS_MEMORY_MB, or live with \
             PUT _cluster/settings {{\"persistent\":{{\"indices.breaker.total.limit\":\"<N>gb\"}}}})",
            observed / MIB,
            reserved / MIB,
            request / MIB,
            watermark / MIB,
            pct_of(watermark, self.memory_limit_bytes),
            self.memory_limit_bytes / MIB,
        ))
    }

    /// Resolved process-memory ceiling in bytes. `0` is not used: an
    /// uncapped config stores the machine size here. Merge planning reads
    /// this so a small host's batches follow the AUTO cap instead of a
    /// fixed 512 MiB.
    pub fn process_memory_limit_bytes(&self) -> u64 {
        self.memory_limit_bytes
    }

    /// RSS admission watermark in bytes. `0` means the check is off.
    pub fn memory_watermark_bytes(&self) -> u64 {
        self.memory_watermark_bytes.load(Ordering::Relaxed)
    }

    /// Larger of the last RSS sample and a live read. `0` when this
    /// platform cannot read process memory.
    pub fn observed_rss_bytes(&self) -> u64 {
        self.observed_process_memory()
    }

    /// Whether the parent RSS memory breaker is currently engaged (issue
    /// #948). Admission (429) is only half of the contract: the resource
    /// sampler uses this flag, together with [`Self::memtable_breaker_engaged`],
    /// to request a memory-pressure DRAIN of rebuildable state (flush + cache
    /// release). A breaker that only rejects never lets a workload whose
    /// caches or memtables hold the bytes make progress.
    pub fn memory_breaker_engaged(&self) -> bool {
        self.memory_tripped.load(Ordering::Relaxed)
    }

    /// Start or read the flat-footprint clock. The first sample inside the
    /// noise band records `now` and returns zero; later samples return how
    /// long RSS has stayed there. Cleared when RSS leaves that band, the
    /// breaker releases, or the watermark is retuned.
    fn note_memory_flat(&self) -> Duration {
        let mut slot = self
            .memory_flat_since
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match *slot {
            Some(at) => at.elapsed(),
            None => {
                *slot = Some(Instant::now());
                Duration::ZERO
            }
        }
    }

    fn clear_memory_flat(&self) {
        *self
            .memory_flat_since
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Test hook: age the flat-footprint clock without sleeping, so a test
    /// can cross [`MEMORY_BAND_STALL_RELEASE`] on a paused schedule.
    #[cfg(test)]
    fn age_memory_flat_for_test(&self, by: Duration) {
        let mut slot = self
            .memory_flat_since
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(at) = *slot {
            *slot = Some(at.checked_sub(by).unwrap_or(at));
        }
    }

    /// Release point of the RSS breaker as a percentage of the watermark.
    pub fn memory_release_percent(&self) -> u8 {
        self.memory_release_percent
    }

    /// Minimum time the RSS breaker stays engaged before an in-band release.
    pub fn memory_min_engaged(&self) -> Duration {
        self.memory_min_engaged
    }

    /// How long the RSS breaker has been engaged. A latch with no recorded
    /// edge (the startup commit trip) counts as having met any minimum.
    fn memory_engaged_for(&self) -> Duration {
        match *self
            .memory_engaged_since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            Some(at) => at.elapsed(),
            None => Duration::MAX,
        }
    }

    fn set_memory_engaged_since(&self, at: Option<Instant>) {
        *self
            .memory_engaged_since
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = at;
    }

    /// #2427 test hook: age the engaged clock without sleeping.
    #[cfg(test)]
    fn age_memory_engaged_for_test(&self, by: Duration) {
        let mut slot = self
            .memory_engaged_since
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(at) = *slot {
            *slot = Some(at.checked_sub(by).unwrap_or(at));
        }
    }

    /// Bytes reserved by in-flight segment merges (#2427).
    pub fn merge_reserved_bytes(&self) -> u64 {
        self.merge_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserved
    }

    /// Segment merges currently holding a [`MergeReservation`] (#2427).
    pub fn merges_in_flight(&self) -> u64 {
        self.merge_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_flight
    }

    /// Ceiling of the merge pool (#2427 read-budget isolation): the process
    /// limit minus [`SEARCH_READ_RESERVE_PERCENT`], or the watermark if that
    /// is lower. Merge admission, the merge yield check and the merge
    /// output-write check stop at this line, so the reserve above it stays
    /// for searches. `0` when no watermark is live.
    pub fn merge_pool_ceiling_bytes(&self) -> u64 {
        merge_pool_ceiling(self.memory_watermark_bytes(), self.memory_limit_bytes)
    }

    /// Reserve the estimated working set of one merge batch whose inputs
    /// total `input_bytes` against the merge pool (#2427).
    ///
    /// Before this, every index started its first merge batch whatever the
    /// process footprint, and each batch was checked against an RSS snapshot
    /// that did not see other indices' merges that had started but not yet
    /// allocated. Under autoindex (hundreds of indices) that put many merge
    /// and FST working sets on top of a node already at the watermark: RSS
    /// climbed about 1 GB past a 3,000 MB cap while writes were getting 429s.
    ///
    /// The reservation is `input_bytes * MERGE_WORKING_SET_NUMERATOR` and is
    /// held until the guard drops. See `merge_budget::decide_merge_admission`
    /// for the rule.
    /// `None` means defer: the caller leaves the batch for a later pass.
    pub fn try_reserve_merge(&self, input_bytes: u64, forced: bool) -> Option<MergeReservation> {
        self.try_reserve_merge_with_footprint(input_bytes, 0, forced)
    }

    /// [`Self::try_reserve_merge`] for a batch whose inputs also occupy
    /// `on_disk_bytes` on disk, side-cars included (#2427). The reservation
    /// is `merge_budget::merge_reservation_bytes`: the planned-input
    /// estimate or `on_disk_bytes * MERGE_ON_DISK_WORKING_SET_NUMERATOR`,
    /// whichever is larger. `0` means unknown.
    pub fn try_reserve_merge_with_footprint(
        &self,
        input_bytes: u64,
        on_disk_bytes: u64,
        forced: bool,
    ) -> Option<MergeReservation> {
        let bytes = crate::merge_budget::merge_reservation_bytes(input_bytes, on_disk_bytes);
        // Concurrent merges are admitted against the merge pool, not the
        // watermark: the read reserve above it is never handed to a merge.
        let watermark = self.merge_pool_ceiling_bytes();
        let observed = self.observed_process_memory();
        let engaged = self.memory_breaker_engaged();
        let mut ledger = self.merge_ledger.lock().unwrap_or_else(|e| e.into_inner());
        // #2427 ingest overshoot: a lone background batch does not start
        // inside the read reserve, and does not start when its own
        // reservation would carry RSS into it (the 1 GB gate's ingest node
        // peaked at 3.5 GB against a 3,072 MB cap this way). It waits,
        // bounded, for the drain; meanwhile the next batches are planned
        // to fit the headroom. See `merge_budget::lone_merge_waits_for_headroom`
        // and `merge_budget::pressure_cap_for_headroom`.
        let lone_background = !forced && ledger.in_flight == 0 && watermark > 0 && observed > 0;
        let mut headroom_wait_expired = false;
        if lone_background && observed.saturating_add(bytes) >= watermark {
            let since = *ledger
                .headroom_wait_since
                .get_or_insert_with(std::time::Instant::now);
            if crate::merge_budget::lone_merge_waits_for_headroom(
                observed,
                watermark,
                bytes,
                ledger.in_flight,
                forced,
                since.elapsed(),
                self.merge_headroom_max_wait,
            ) {
                if observed < watermark {
                    ledger.pressure_cap = crate::merge_budget::pressure_cap_for_headroom(
                        ledger.pressure_cap,
                        input_bytes,
                        bytes,
                        watermark - observed,
                    );
                }
                return None;
            }
            // Waited the full bound: start it anyway so merging progresses.
            headroom_wait_expired = true;
        } else if observed < watermark {
            // Headroom came back: the next wait starts its own clock. Only
            // when the batch fits, not merely when RSS is under the
            // ceiling; otherwise a batch that never fits restarts the clock
            // on every pass and never runs.
            ledger.headroom_wait_since = None;
        }
        if !crate::merge_budget::decide_merge_admission(
            watermark,
            observed,
            ledger.reserved,
            ledger.in_flight,
            engaged,
            bytes,
            forced,
            headroom_wait_expired,
        ) {
            return None;
        }
        ledger.reserved = ledger.reserved.saturating_add(bytes);
        ledger.in_flight += 1;
        ledger.headroom_wait_since = None;
        Some(MergeReservation {
            ledger: Arc::clone(&self.merge_ledger),
            bytes,
        })
    }

    /// Whether an in-flight background merge batch must stop now (#2427).
    /// See `merge_budget::merge_must_yield`.
    pub fn merge_must_yield(&self, forced: bool) -> bool {
        let observed = self.observed_process_memory();
        crate::merge_budget::merge_must_yield(
            observed,
            self.merge_pool_ceiling_bytes(),
            self.merge_memory_pressure(observed),
            forced,
        )
    }

    /// Whether merges see memory pressure at `observed` bytes (#2427): the
    /// breaker is engaged, or `observed` is already at the watermark and the
    /// 100 ms sampler has not latched it yet. A merge checks live process
    /// memory, so it need not run on for up to a sampler tick past the
    /// watermark.
    fn merge_memory_pressure(&self, observed: u64) -> bool {
        let watermark = self.memory_watermark_bytes();
        self.memory_breaker_engaged() || (watermark > 0 && observed >= watermark)
    }

    /// [`Self::merge_must_yield`] for a batch of `input_bytes`: after a
    /// streak of yields a small batch completes, so compaction cannot stall
    /// for good. See `merge_budget::merge_must_yield_after`.
    pub fn merge_must_yield_for(&self, forced: bool, input_bytes: u64) -> bool {
        let streak = self
            .merge_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .yield_streak;
        let observed = self.observed_process_memory();
        crate::merge_budget::merge_must_yield_after(
            observed,
            self.merge_pool_ceiling_bytes(),
            self.merge_memory_pressure(observed),
            forced,
            input_bytes,
            streak,
        )
    }

    /// Record a background batch of `input_bytes` that stopped under memory
    /// pressure: the next batches are planned under half of it (#2427).
    pub fn note_merge_yield(&self, input_bytes: u64) -> u64 {
        let mut ledger = self.merge_ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.pressure_cap =
            crate::merge_budget::pressure_cap_after_yield(ledger.pressure_cap, input_bytes);
        ledger.yield_streak = ledger.yield_streak.saturating_add(1);
        ledger.pressure_cap
    }

    /// Record a background batch of `input_bytes` that completed: a batch
    /// that used the adaptive cap widens it again (#2427).
    pub fn note_merge_completed(&self, input_bytes: u64) {
        let mut ledger = self.merge_ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.pressure_cap =
            crate::merge_budget::pressure_cap_after_success(ledger.pressure_cap, input_bytes);
        ledger.yield_streak = 0;
    }

    /// The adaptive merge batch input cap, `0` when none is set (#2427).
    pub fn merge_pressure_cap_bytes(&self) -> u64 {
        self.merge_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pressure_cap
    }

    /// Whether a batch's output-write step is exempt from
    /// [`Self::admit_merge_write`]'s hard-limit refusal (#2427, #2468).
    ///
    /// True exactly when #2468's streak rule is what keeps this batch
    /// running: the plain rule ([`Self::merge_must_yield`]) says yield, and
    /// the streak-aware rule ([`Self::merge_must_yield_for`]) waives it for
    /// a small batch after a streak of yields with nothing merged. Refusing
    /// that batch's write would bring back the stall the rule exists to
    /// break.
    pub fn merge_write_exempt(&self, forced: bool, input_bytes: u64) -> bool {
        self.merge_must_yield(forced) && !self.merge_must_yield_for(forced, input_bytes)
    }

    /// Charge a merge batch's output-write step to the merge budget before
    /// it runs (#2427).
    ///
    /// The step comes after the last yield checkpoint: the doc-values
    /// build, the stored encode and the FTS side-car build. It allocates
    /// `extra` bytes (`merge_budget::merge_write_phase_bytes`) on top of the
    /// `held` bytes the batch already has. Before this, neither admission
    /// nor the yield checks saw it, and large pg-docs merges took a
    /// 3,072 MB search node to 4.4–4.9 GB in their last seconds.
    ///
    /// Returns `false` when the step would take observed process memory to
    /// the hard limit (`merge_budget::decide_merge_write`). The caller then
    /// yields the batch, keeping its inputs. On `true`, `reservation` grows
    /// to at least `held + extra`, so admission for other merges sees the
    /// step until the batch drops its reservation. A refused step leaves
    /// the ledger as it was. `exempt` covers batches that #2468's streak
    /// rule lets run to completion.
    pub fn admit_merge_write(
        &self,
        reservation: Option<&mut MergeReservation>,
        held: u64,
        extra: u64,
        forced: bool,
        exempt: bool,
    ) -> bool {
        self.admit_merge_write_for(reservation, held, extra, forced, exempt, 0)
    }

    /// [`Self::admit_merge_write`] for a batch of `input_bytes`: after a
    /// streak of yields with nothing merged, a small background batch's
    /// write is checked against the hard process limit instead of the
    /// merge-pool ceiling, so a node resting between the two still merges
    /// (#2427). See `merge_budget::merge_write_limit_after`.
    pub fn admit_merge_write_for(
        &self,
        reservation: Option<&mut MergeReservation>,
        held: u64,
        extra: u64,
        forced: bool,
        exempt: bool,
        input_bytes: u64,
    ) -> bool {
        let observed = self.observed_process_memory();
        let streak = self
            .merge_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .yield_streak;
        let line = crate::merge_budget::merge_write_limit_after(
            self.merge_pool_ceiling_bytes(),
            self.memory_limit_bytes,
            forced,
            input_bytes,
            streak,
        );
        if !crate::merge_budget::decide_merge_write(observed, line, extra, forced, exempt) {
            return false;
        }
        if let Some(reservation) = reservation {
            let wanted = held.saturating_add(extra);
            if wanted > reservation.bytes {
                let mut ledger = self.merge_ledger.lock().unwrap_or_else(|e| e.into_inner());
                ledger.reserved = ledger.reserved.saturating_add(wanted - reservation.bytes);
                reservation.bytes = wanted;
            }
        }
        true
    }

    /// Whether the summed-memtable budget breaker is currently engaged.
    pub fn memtable_breaker_engaged(&self) -> bool {
        self.memtable_tripped.load(Ordering::Relaxed)
    }

    /// Whether the sampler must request a memory-pressure drain (#948, #2238).
    ///
    /// Both parent latches answer writes with HTTP 429, and a 429 frees
    /// nothing by itself. The RSS latch has requested a drain since #948.
    /// The summed-memtable latch did not. On the 8 GiB auto tier that budget
    /// is 2 GiB (`auto_memtable_budget`: 25% floored at 2 GiB) while each
    /// index flushes at `storage.flush_size_mb` (default 512 MiB), so a
    /// corpus spread across dozens of indices crosses the process budget
    /// with every index still under its own flush line. Writes then stay
    /// 429 and the indexed count stays 0 unless this latch requests the
    /// same drain.
    ///
    /// The two latches do not share a timer. The RSS latch backs off when a
    /// drain does not lower RSS (#1981). The memtable latch stays on the
    /// fixed window, because that flush is what frees this budget and a
    /// stable RSS is the normal shape of a memtable-only trip.
    pub fn ingest_pressure_needs_drain(&self) -> bool {
        self.memory_breaker_engaged() || self.memtable_breaker_engaged()
    }

    /// Whether it is safe to spawn a best-effort helper thread right now (#1122).
    ///
    /// `std::thread::Builder::spawn` panics inside the new thread when the
    /// alternate signal stack cannot be mmap'd, and the shipped profile is
    /// `panic = "abort"`, so that panic aborts the node. The engine's helper
    /// spawner does not use that path (a failed `pthread_create` is an
    /// `Err`), and this gate skips the attempt entirely once the breaker is
    /// engaged so a flush storm does not keep reserving stacks. Callers on
    /// flush/publish paths that have an inline fallback must check this and
    /// take the fallback instead of spawning.
    ///
    /// Reads the *live* RSS (`current_rss_bytes`), not the sampler's last
    /// value, so an RSS spike between samples cannot sneak a spawn through;
    /// the engaged breaker flag is the second, sampled signal. With no RSS
    /// probe or no configured watermark this returns `true` — spawning is
    /// unrestricted, exactly the pre-#1122 behaviour.
    pub fn thread_spawn_safe(&self) -> bool {
        if self.memory_breaker_engaged() {
            return false;
        }
        let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
        if watermark == 0 {
            return true;
        }
        current_rss_bytes() < watermark
    }

    /// Disk flood-stage admission. Returns an ES-shaped
    /// `read_only_allow_delete` cluster block (HTTP 429) when the data-dir
    /// filesystem is over the flood-stage watermark. `index` names the
    /// blocked index for the ES `root_cause`.
    pub fn check_disk_block(&self, index: &str) -> Result<(), XerjError> {
        if self.disk_blocked.load(Ordering::Relaxed) {
            // The `read_only_allow_delete` substring drives the 429 status in
            // the ES error mapper (flood-stage rejections are 429, unlike an
            // explicit 403 write block). Mirrors ES's flood-stage message.
            return Err(XerjError::index_blocked(
                index,
                format!(
                    "read_only_allow_delete (disk usage {}% exceeded flood-stage watermark [{}%])",
                    self.disk_used_pct.load(Ordering::Relaxed),
                    self.disk_flood_pct.load(Ordering::Relaxed),
                ),
            ));
        }
        Ok(())
    }

    /// Effective flood-stage gate for `/_cat/allocation` (#2077).
    ///
    /// `flood_pct` is `limits.disk_flood_stage_percent` after a live
    /// `cluster.routing.allocation.disk.watermark.flood_stage` override.
    /// `0` means the watermark is off. `threshold_enabled` mirrors
    /// `cluster.routing.allocation.disk.threshold_enabled` (default true).
    /// `blocked` is the latched write block, including the 1% release
    /// hysteresis, so a volume that has dipped just under the threshold
    /// still reports that writes will be refused.
    pub fn disk_flood_admission(&self) -> DiskFloodAdmission {
        DiskFloodAdmission {
            flood_pct: self.disk_flood_pct.load(Ordering::Relaxed),
            threshold_enabled: self.disk_threshold_enabled.load(Ordering::Relaxed),
            blocked: self.disk_blocked.load(Ordering::Relaxed),
        }
    }

    /// Applies (or clears) a runtime override of the disk flood-stage
    /// percent, read each sampler tick from the
    /// `cluster.routing.allocation.disk.watermark.flood_stage` persistent
    /// cluster setting (parsed by [`parse_flood_stage_pct`]) — see
    /// `Engine::spawn_resource_sampler`. `None` reverts to the config
    /// default. A no-op when the watermark was disabled at startup
    /// (`disk_flood_stage_percent = 0`): this only re-tunes an
    /// already-active watermark without a restart, the way ES's
    /// `PUT _cluster/settings` does — it doesn't turn the feature on.
    pub fn set_disk_flood_pct_override(&self, override_pct: Option<u8>) {
        if self.disk_flood_pct_default == 0 {
            return;
        }
        let effective = override_pct.unwrap_or(self.disk_flood_pct_default).min(100);
        self.disk_flood_pct.store(effective, Ordering::Relaxed);
    }

    /// Applies the `cluster.routing.allocation.disk.threshold_enabled` cluster
    /// setting (`None` = unset = enabled).
    pub fn set_disk_threshold_enabled(&self, enabled: Option<bool>) {
        self.disk_threshold_enabled
            .store(enabled.unwrap_or(true), Ordering::Relaxed);
    }

    /// Applies (or clears) a runtime override of the parent RSS watermark, read
    /// each sampler tick from the `indices.breaker.total.limit` persistent
    /// cluster setting (parsed by [`parse_breaker_total_limit`]) — see
    /// `Engine::spawn_resource_sampler`. `None` reverts to the boot default.
    ///
    /// This is the live fix for the multi-index ingest oscillation (#1250): on
    /// a big box the AUTO tier caps the watermark conservatively (16 GiB on a
    /// 119 GiB host → it trips at ~15.5 GiB and sheds ALL writes), and the only
    /// prior remedy was `XERJ_MAX_PROCESS_MEMORY_MB` + a restart — which forced
    /// the indexing client into a full source-snapshot reconciliation. Raising
    /// the watermark via `PUT _cluster/settings` now needs no restart.
    ///
    /// `override_bytes` is clamped to `machine_memory_bytes` (a watermark above
    /// physical RAM cannot protect anything) and may be *below* the boot default
    /// too. A no-op when the watermark was disabled at startup
    /// (`memory_watermark_percent = 0`): like the disk override, this only
    /// re-tunes an already-active watermark, it never turns the feature on.
    pub fn set_memory_watermark_override(&self, override_bytes: Option<u64>) {
        if self.memory_watermark_bytes_default == 0 {
            return;
        }
        let effective = match override_bytes {
            Some(bytes) => bytes.min(self.machine_memory_bytes).max(1),
            None => self.memory_watermark_bytes_default,
        };
        self.memory_watermark_bytes
            .store(effective, Ordering::Relaxed);
    }

    /// Per-query memory guard for `max_query_memory_mb` (item 1). Rejects a
    /// query whose *estimated* peak allocation (`bytes`) exceeds the budget,
    /// before the allocation is made, with a 429 `circuit_breaking_exception`.
    /// `label` names the allocation site (e.g. "hydrate", "terms-agg").
    pub fn check_query_alloc(&self, bytes: u64, label: &str) -> Result<(), XerjError> {
        if self.max_query_memory_bytes != 0 && bytes > self.max_query_memory_bytes {
            return Err(XerjError::circuit_breaking(format!(
                "[request] query allocation ({}) would exceed limits.max_query_memory_mb={}MB \
                 at [{label}]; reduce size/aggregation cardinality",
                human_bytes(bytes),
                self.max_query_memory_bytes / (1024 * 1024),
            )));
        }
        Ok(())
    }

    /// Whether the per-query memory guard is active (non-zero budget).
    pub fn query_memory_enabled(&self) -> bool {
        self.max_query_memory_bytes != 0
    }

    // ── item 2: global search pool ──────────────────────────────────────

    /// Acquire one global search permit. Bounds process-wide search
    /// concurrency to `max_concurrent_searches`; the returned guard releases
    /// the permit (and decrements the in-flight gauge) on drop. Excess
    /// searches queue on the semaphore, exactly like ES's search thread
    /// pool bounds active workers.
    ///
    /// Under memory pressure (#2427) searches are non-write memory the RSS
    /// breaker never saw: the 1 GB release gate kept climbing from 5.1 GB to
    /// an 8.4 GB OOM with writes blocked, and its search phase ran 30 s
    /// multi-index searches against a node already over its watermark.
    /// While the breaker is engaged each search also takes the single
    /// `search_pressure` permit, so they run one at a time. At or above the
    /// hard process limit a new search is rejected with a 429
    /// `circuit_breaking_exception` once the drain has had
    /// [`SEARCH_HEADROOM_WAIT`] to bring RSS back under it.
    ///
    /// Read-budget isolation (#2427): bytes held by in-flight merge
    /// reservations do not count against a search (see
    /// [`Self::search_admission_rss`]); those merges yield at the merge-pool
    /// ceiling, below the limit. A search that still finds process memory at
    /// or above the limit waits up to [`SEARCH_HEADROOM_WAIT`] for the drain
    /// before it is rejected, instead of failing in milliseconds against a
    /// transient peak.
    pub async fn acquire_search(&self) -> Result<SearchPermit, XerjError> {
        let permit = Arc::clone(&self.search_pool)
            .acquire_owned()
            .await
            .map_err(|_| XerjError::internal("global search pool closed — shutting down"))?;
        let pressure = if self.memory_breaker_engaged() || self.check_search_hard_limit().is_err() {
            let p = Arc::clone(&self.search_pressure)
                .acquire_owned()
                .await
                .map_err(|_| XerjError::internal("search pressure gate closed — shutting down"))?;
            // RSS may have crossed the hard limit while this search queued.
            self.wait_for_search_headroom().await?;
            Some(p)
        } else {
            None
        };
        let now = self.search_inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.search_inflight_peak.fetch_max(now, Ordering::Relaxed);
        Ok(SearchPermit {
            _permit: permit,
            _pressure: pressure,
            inflight: Arc::clone(&self.search_inflight),
        })
    }

    /// Wait, holding the `search_pressure` permit, until the hard-limit check
    /// passes or [`Self::search_headroom_wait`] runs out (#2427). The sampler
    /// drains rebuildable state while the breaker is engaged; this gives that
    /// drain time to work before the search is turned away.
    async fn wait_for_search_headroom(&self) -> Result<(), XerjError> {
        let deadline = tokio::time::Instant::now() + self.search_headroom_wait;
        loop {
            match self.check_search_hard_limit() {
                Ok(()) => return Ok(()),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(SEARCH_HEADROOM_POLL).await,
            }
        }
    }

    /// Process memory a search is admitted against (#2427): the last RSS
    /// sample less the bytes in-flight merges hold in the merge ledger. Those
    /// merges must yield at the merge-pool ceiling, so their bytes are
    /// already on their way out and are not the read path's to pay for.
    fn search_admission_rss(&self) -> u64 {
        let rss = self.rss_bytes.load(Ordering::Relaxed);
        rss.saturating_sub(self.merge_reserved_bytes())
    }

    /// RSS line at which new searches are rejected (#2427): the process
    /// memory limit, or the live watermark if an operator raised it past
    /// that (#1323). `0` when no limit is known.
    fn search_hard_limit_bytes(&self) -> u64 {
        let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
        if watermark == 0 {
            return 0;
        }
        self.memory_limit_bytes.max(watermark)
    }

    fn check_search_hard_limit(&self) -> Result<(), XerjError> {
        let limit = self.search_hard_limit_bytes();
        let rss = self.search_admission_rss();
        if limit != 0 && rss >= limit {
            return Err(XerjError::circuit_breaking(format!(
                "[parent] Data too large: process memory [{rss}/{}] is at or above the memory limit [{limit}/{}] after waiting for the memory breaker to drain; search rejected",
                human_bytes(rss),
                human_bytes(limit)
            )));
        }
        Ok(())
    }

    /// Current in-flight search count (for stats / proof).
    pub fn search_inflight(&self) -> u64 {
        self.search_inflight.load(Ordering::Relaxed)
    }

    /// Peak concurrent search count observed since boot (for proof).
    pub fn search_inflight_peak(&self) -> u64 {
        self.search_inflight_peak.load(Ordering::Relaxed)
    }

    /// Configured global search-concurrency cap.
    pub fn max_concurrent_searches(&self) -> usize {
        self.max_concurrent_searches
    }

    // ── item 2b: global bulk pool ──────────────────────────────────────

    /// Acquire one global bulk permit. Bounds process-wide bulk-parse
    /// concurrency to `max_concurrent_bulks` (default 8), preventing N
    /// concurrent bulks from each materializing ~300 B/action of parse-phase
    /// heap before the memtable admission check fires.
    pub async fn acquire_bulk(&self) -> Result<tokio::sync::OwnedSemaphorePermit, XerjError> {
        self.bulk_pool
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| XerjError::internal("global bulk pool closed — shutting down"))
    }

    /// Configured global bulk-concurrency cap.
    pub fn max_concurrent_bulks(&self) -> usize {
        self.max_concurrent_bulks
    }

    // ── Sampler surface (called by the background task) ─────────────────

    /// Refresh every sampled atomic in one call (memtable, RSS, disk).
    /// Convenience wrapper used by tests; the live sampler calls
    /// [`Self::refresh_memory_disk`] and [`Self::refresh_memtable`]
    /// SEPARATELY so a contended memtable read can never delay the memory
    /// admission update (see `Engine::spawn_resource_sampler`).
    pub fn refresh(&self, memtable_used: u64, rss: u64, disk_used_pct: u64) {
        self.refresh_memory_disk(rss, disk_used_pct, 0);
        self.refresh_memtable(memtable_used);
    }

    /// Update the RSS + disk atomics and their latched trip flags. Depends on
    /// NOTHING but two syscalls — never blocks on an engine lock — so the
    /// parent memory breaker stays responsive even while a turbo batch holds
    /// every memtable shard's write lock.
    ///
    /// `disk_avail_bytes` is the raw free-space figure from the same
    /// `statvfs` sample as `disk_used_pct` — carried through purely so the
    /// crossing WARN below can print an absolute free-space figure
    /// alongside the percentage, matching ES's flood-stage log line. Pass
    /// `0` when it isn't available (e.g. from the [`Self::refresh`] test
    /// wrapper); it only affects that one log line, never admission.
    pub fn refresh_memory_disk(&self, rss: u64, disk_used_pct: u64, disk_avail_bytes: u64) {
        self.refresh_memory_observing(rss, disk_used_pct, disk_avail_bytes, None);
    }

    /// [`Self::refresh_memory_disk`] plus an optional Windows available-commit
    /// sample. `None` leaves the commit latch as it was (Linux, and tests
    /// that only move RSS). A `Some` below the commit reserve engages the
    /// breaker even when `rss` is under the watermark (#1122). Once commit is
    /// back above the reserve the RSS latch alone decides, and that latch
    /// never releases at or above the watermark (#2427).
    pub fn refresh_memory_observing(
        &self,
        rss: u64,
        disk_used_pct: u64,
        disk_avail_bytes: u64,
        available_commit: Option<u64>,
    ) {
        // ── RSS watermark, with release hysteresis (#1250, #2427) ──
        //
        // Engage at `rss >= watermark`. Once engaged, the latch releases below
        // `memory_release_percent` of the watermark (85% by default) after the
        // breaker has been engaged for `memory_min_engaged`. Inside the band
        // `[release, watermark)` it holds while the drain is still lowering
        // RSS. A 95% band that released on the first sample that did not fall
        // flapped 20 times in 16 minutes under autoindex (#2427), each release
        // re-admitting the write burst that tripped it. If RSS does not fall
        // past [`MEMORY_RSS_NOISE_BYTES`] inside the band for
        // [`MEMORY_BAND_STALL_RELEASE`], the drain has nothing more to free:
        // holding would only reject writes the node can take and keep the
        // sampler re-draining forever, so the latch releases.
        //
        // At or above the watermark the latch never releases (#2427). The
        // #1981 flat-footprint release that used to apply there is gone: it
        // released a node at 5,121MB against a 2,918MB watermark. A live
        // watermark change (#1323) is judged against the operator's new
        // value on that sample.
        self.rss_bytes.store(rss, Ordering::Relaxed);
        let watermark = self.memory_watermark_bytes.load(Ordering::Relaxed);
        let cur_mem = self.memory_tripped.load(Ordering::Relaxed);
        let retuned = self.memory_trip_watermark.load(Ordering::Relaxed) != watermark;
        // #2427: the release point is `memory_release_percent` of the
        // watermark (85% by default, was a fixed 95%).
        let release_bytes = memory_release_bytes(watermark, self.memory_release_percent);
        // `None` keeps the previous latch so an RSS-only refresh (Linux, and
        // the test helper) does not clear a commit trip it did not sample.
        // Reserve 0 means this platform has no commit probe: starved stays
        // false and the RSS formula below is the pre-#1122 check.
        let starved = if self.commit_reserve_bytes == 0 {
            false
        } else if let Some(available) = available_commit {
            self.available_commit_bytes
                .store(available, Ordering::Relaxed);
            available < self.commit_reserve_bytes
        } else {
            self.commit_starved.load(Ordering::Relaxed)
        };
        self.commit_starved.store(starved, Ordering::Relaxed);
        // RSS latch (#1250, #2427). Commit starvation is applied after this
        // so a pagefileless box stays engaged while available commit is
        // under the reserve.
        let rss_next = watermark != 0
            && if rss >= watermark {
                // #2427: at or above the watermark the RSS latch ALWAYS holds
                // (or engages). There is no release up here, whatever the
                // footprint is doing. The #1981 flat-footprint release that
                // used to live in this arm let a 1 GB release-gate node
                // release at rss=5,121MB against a 2,918MB watermark, re-engage
                // 100 ms later at 5,125MB, and get OOM-killed at 8.4 GB. The
                // drain backoff (`MemoryDrainSchedule`) is what keeps a
                // footprint that will not fall from re-draining every 2 s.
                self.memory_band_anchor.store(0, Ordering::Relaxed);
                self.clear_memory_flat();
                true
            } else if cur_mem && !retuned {
                // #2427 release hysteresis, strictly below the watermark.
                // Below the release point, release once the minimum engaged
                // time has passed. Inside the band, hold while the drain is
                // still lowering RSS; if RSS has not fallen past the noise
                // band for MEMORY_BAND_STALL_RELEASE, the drain has nothing
                // left to free, so release rather than reject writes forever
                // (#1250).
                let min_met = self.memory_engaged_for() >= self.memory_min_engaged;
                if rss < release_bytes {
                    self.clear_memory_flat();
                    self.memory_band_anchor.store(0, Ordering::Relaxed);
                    !min_met
                } else {
                    let anchor = self.memory_band_anchor.load(Ordering::Relaxed);
                    if anchor == 0 || rss.saturating_add(MEMORY_RSS_NOISE_BYTES) < anchor {
                        // Entered the band, or fell past the noise band:
                        // the drain is working. Restart the stall clock.
                        self.memory_band_anchor.store(rss, Ordering::Relaxed);
                        self.clear_memory_flat();
                        self.note_memory_flat();
                        true
                    } else {
                        let stalled = self.note_memory_flat() >= MEMORY_BAND_STALL_RELEASE;
                        if stalled && min_met {
                            self.clear_memory_flat();
                            self.memory_band_anchor.store(0, Ordering::Relaxed);
                            false
                        } else {
                            true
                        }
                    }
                }
            } else {
                // Disengaged under the watermark, or a live retune (#1323)
                // that put RSS under the operator's new value.
                self.clear_memory_flat();
                self.memory_band_anchor.store(0, Ordering::Relaxed);
                false
            };
        // #2427 invariant: the RSS latch never reports released while RSS is
        // at or above the live watermark.
        debug_assert!(rss_next || watermark == 0 || rss < watermark);
        let mem_next = starved || rss_next;
        if mem_next {
            self.memory_trip_watermark
                .store(watermark, Ordering::Relaxed);
        }
        if mem_next != self.memory_tripped.swap(mem_next, Ordering::Relaxed) {
            // Only an RSS engage starts the minimum-engaged clock: a Windows
            // commit trip releases as soon as commit recovers.
            self.set_memory_engaged_since((mem_next && rss_next).then(Instant::now));
            if mem_next && starved {
                tracing::warn!(
                    available_commit_mb = self.available_commit_bytes.load(Ordering::Relaxed)
                        / (1024 * 1024),
                    commit_limit_mb = self.commit_limit_bytes / (1024 * 1024),
                    reserve_mb = self.commit_reserve_bytes / (1024 * 1024),
                    "available Windows commit is under the reserve — engaging the parent memory circuit breaker (writes → 429)"
                );
            } else if mem_next {
                tracing::warn!(
                    rss_mb = rss / (1024 * 1024),
                    watermark_mb = watermark / (1024 * 1024),
                    "RSS crossed the memory watermark — engaging the parent memory circuit breaker (writes → 429)"
                );
            } else {
                tracing::info!(
                    rss_mb = rss / (1024 * 1024),
                    release_mb = release_bytes / (1024 * 1024),
                    "RSS back below the memory watermark — releasing the memory circuit breaker"
                );
            }
        }

        // ── disk flood stage (1% release hysteresis to avoid flapping) ──
        let flood_pct = self.disk_flood_pct.load(Ordering::Relaxed);
        if flood_pct != 0 {
            self.disk_used_pct.store(disk_used_pct, Ordering::Relaxed);
            let cur = self.disk_blocked.load(Ordering::Relaxed);
            let release_pct = (flood_pct as u64).saturating_sub(1);
            let enabled = self.disk_threshold_enabled.load(Ordering::Relaxed);
            let next = if !enabled {
                false
            } else if cur {
                disk_used_pct >= release_pct
            } else {
                disk_used_pct >= flood_pct as u64
            };
            if next != cur {
                if next {
                    tracing::warn!(
                        used_pct = disk_used_pct,
                        flood_pct = flood_pct,
                        free = %human_bytes(disk_avail_bytes),
                        "disk flood-stage watermark crossed — engaging read_only_allow_delete write block"
                    );
                } else if !enabled {
                    tracing::info!(
                        used_pct = disk_used_pct,
                        "disk threshold disabled by cluster setting — releasing write block"
                    );
                } else {
                    tracing::info!(
                        used_pct = disk_used_pct,
                        "disk usage back below flood-stage watermark — releasing write block"
                    );
                }
            }
            self.disk_blocked.store(next, Ordering::Relaxed);
        }
    }

    /// Update the summed-memtable atomic + its trip flag. Called AFTER
    /// [`Self::refresh_memory_disk`] in the sampler, because computing the sum
    /// reads a lock on every memtable shard and a turbo batch can hold those
    /// write-locked for the whole batch — blocking here must never stall the
    /// memory/disk update above.
    pub fn refresh_memtable(&self, memtable_used: u64) {
        self.memtable_used_bytes
            .store(memtable_used, Ordering::Relaxed);
        let next = self.memtable_budget_bytes != 0 && memtable_used >= self.memtable_budget_bytes;
        if next != self.memtable_tripped.swap(next, Ordering::Relaxed) {
            if next {
                tracing::warn!(
                    used_mb = memtable_used / (1024 * 1024),
                    budget_mb = self.memtable_budget_bytes / (1024 * 1024),
                    "summed memtable crossed the process budget — engaging the parent circuit breaker (writes → 429)"
                );
            } else {
                tracing::info!(
                    used_mb = memtable_used / (1024 * 1024),
                    "summed memtable back below the process budget — releasing the parent circuit breaker"
                );
            }
        }
    }

    // ── Observability ───────────────────────────────────────────────────

    /// Snapshot of the current governor state for `_nodes/stats`-style
    /// surfaces.
    pub fn snapshot(&self) -> GovernorSnapshot {
        GovernorSnapshot {
            segment_hydration: self.segment_hydration_budget.snapshot(),
            segment_hydration_source: self.segment_hydration_budget_source,
            memtable_used_bytes: self.memtable_used_bytes.load(Ordering::Relaxed),
            memtable_budget_bytes: self.memtable_budget_bytes,
            memtable_tripped: self.memtable_tripped.load(Ordering::Relaxed),
            rss_bytes: self.rss_bytes.load(Ordering::Relaxed),
            memory_limit_bytes: self.memory_limit_bytes,
            memory_watermark_bytes: self.memory_watermark_bytes.load(Ordering::Relaxed),
            memory_tripped: self.memory_tripped.load(Ordering::Relaxed),
            disk_used_pct: self.disk_used_pct.load(Ordering::Relaxed),
            disk_flood_pct: self.disk_flood_pct.load(Ordering::Relaxed),
            disk_blocked: self.disk_blocked.load(Ordering::Relaxed),
            max_concurrent_searches: self.max_concurrent_searches,
            search_inflight: self.search_inflight.load(Ordering::Relaxed),
            search_inflight_peak: self.search_inflight_peak.load(Ordering::Relaxed),
            max_concurrent_bulks: self.max_concurrent_bulks,
        }
    }

    pub fn segment_hydration_budget(&self) -> Arc<SegmentHydrationBudget> {
        Arc::clone(&self.segment_hydration_budget)
    }
}

/// RAII guard for a held global search permit. Decrements the in-flight
/// gauge on drop; the underlying semaphore permit is released with it.
pub struct SearchPermit {
    _permit: OwnedSemaphorePermit,
    /// The memory-pressure gate's single permit, held when the search was
    /// admitted while the RSS breaker was engaged (#2427).
    _pressure: Option<OwnedSemaphorePermit>,
    inflight: Arc<AtomicU64>,
}

impl Drop for SearchPermit {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The disk flood-stage gate `/_cat/allocation` reports to clients (#2077).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskFloodAdmission {
    pub flood_pct: u8,
    pub threshold_enabled: bool,
    pub blocked: bool,
}

/// A cheap, `Copy`-able snapshot of governor state.
#[derive(Debug, Clone, Copy)]
pub struct GovernorSnapshot {
    pub segment_hydration: SegmentHydrationBudgetSnapshot,
    pub segment_hydration_source: SegmentHydrationBudgetSource,
    pub memtable_used_bytes: u64,
    pub memtable_budget_bytes: u64,
    pub memtable_tripped: bool,
    pub rss_bytes: u64,
    pub memory_limit_bytes: u64,
    pub memory_watermark_bytes: u64,
    pub memory_tripped: bool,
    pub disk_used_pct: u64,
    pub disk_flood_pct: u8,
    pub disk_blocked: bool,
    pub max_concurrent_searches: usize,
    pub search_inflight: u64,
    pub search_inflight_peak: u64,
    pub max_concurrent_bulks: usize,
}

/// Auto-derive the memtable byte ceiling from the effective (cgroup-aware)
/// memory limit: 25% of the limit, floored at 2 GiB, but never past 50% of
/// the same limit. The 50% cap keeps the floor itself from exceeding the
/// container under a small enough effective limit.
///
/// `effective_limit` must already be the cgroup-aware value (see
/// `effective_memory_limit_bytes`) — this function has no way to detect a
/// host-RAM value passed in by mistake, so getting that argument right is
/// the caller's responsibility.
/// Apply the configured process-memory ceiling to the machine-derived limit.
///
/// `cap_mb == 0` disables the ceiling. The result is never larger than the
/// machine/cgroup limit — this lowers a budget, it never invents headroom that
/// the host does not have.
fn cap_memory_limit(machine_limit: u64, cap_mb: u64) -> u64 {
    if cap_mb == 0 {
        return machine_limit;
    }
    machine_limit.min(cap_mb.saturating_mul(1024 * 1024))
}

/// Lower a tiered cap to the Windows commit ceiling. `None` (Linux, macOS, a
/// failed probe) returns `tiered_limit` unchanged.
fn apply_windows_commit_ceiling(
    tiered_limit: u64,
    budget: Option<&xerj_common::resource::WindowsCommitBudget>,
) -> u64 {
    match budget {
        Some(budget) if budget.commit_bound && budget.effective_bytes < tiered_limit => {
            budget.effective_bytes.max(1)
        }
        _ => tiered_limit,
    }
}

/// Hard clamp for a live `indices.breaker.total.limit` override. Off Windows
/// this is physical/cgroup memory. On a commit-bound Windows box it stops at
/// the post-reserve ceiling, so raising the breaker cannot grant commit
/// Windows will not give the process.
fn windows_override_ceiling(
    physical: u64,
    budget: Option<&xerj_common::resource::WindowsCommitBudget>,
) -> u64 {
    match budget {
        Some(budget) if budget.commit_bound => budget.effective_bytes.min(physical).max(1),
        _ => physical,
    }
}

/// The AUTO process-memory cap, in MiB, chosen from the effective memory the
/// process can actually see:
///
///   effective < 16 GiB             → half of that RAM
///   16 GiB ≤ effective < 64 GiB    → 8 GiB
///   64 GiB ≤ effective < 128 GiB   → 16 GiB
///   effective ≥ 128 GiB            → 32 GiB
///
/// Tier off the effective (cgroup-aware) limit — see
/// `effective_memory_limit_bytes` — not raw host RAM, so a container smaller
/// than its host is sized for the container. This only *chooses* a ceiling;
/// `cap_memory_limit` still mins it with the machine, so the cap only ever
/// LOWERS a budget: on a 40 GiB box the 8 GiB tier yields min(40, 8) = 8.
///
/// Hosts under 16 GiB used to take the 8 GiB step and then `min` with the
/// machine, so an 8 GiB box's process budget was all of its RAM. A merge
/// storm on that budget died; the same box stayed up at a 4 GiB cap
/// (#1122). Half of RAM is that cap. At 16 GiB, half is 8 GiB, so the
/// steps above 16 GiB — and the memtable budget they derive — do not move.
/// A sub-mebibyte reading rounds up to 1 MiB and is still min'd with the
/// machine, so the floor does not invent RAM.
fn tiered_cap_mb(effective_bytes: u64) -> u64 {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if effective_bytes < 16 * GIB {
        (effective_bytes / 2 / MIB).max(1)
    } else if effective_bytes < 64 * GIB {
        8 * 1024
    } else if effective_bytes < 128 * GIB {
        16 * 1024
    } else {
        32 * 1024
    }
}

/// Startup warning when the machine has less memory free right now than
/// the cap this node just chose (#2574). The cap is sized from total RAM (or
/// the cgroup limit), so the friendly "detected N usable" line reads as if
/// availability had been checked. On a box where another process already
/// holds most of the RAM (a local LLM engine, say) the node will be swapped
/// or OOM-killed before its own breaker trips. `None` when availability is
/// unknown, when there is no cap, or when there is room for it.
fn available_memory_startup_line(
    available: Option<u64>,
    effective: u64,
    limit: u64,
) -> Option<String> {
    let available = available?;
    if limit == 0 || available >= limit {
        return None;
    }
    Some(format!(
        "memory: only {} of {} is available right now — other processes hold the rest — \
         which is less than the {} cap; if they keep it, lower limits.max_process_memory_mb \
         or XERJ_MAX_PROCESS_MEMORY_MB to fit what is left",
        format_gib(available),
        format_gib(effective),
        format_gib(limit),
    ))
}

/// Human-friendly binary size for the startup line: whole GiB when exact, one
/// decimal when not, and MiB below 1 GiB. Presentation only.
fn format_gib(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        let whole = bytes / GIB;
        let tenths = (bytes % GIB) * 10 / GIB;
        if tenths == 0 {
            format!("{whole} GiB")
        } else {
            format!("{whole}.{tenths} GiB")
        }
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

/// Where the resolved process-memory cap came from — logged in the friendly
/// startup line so a first run states plainly whether the ceiling was auto,
/// from config, or from the environment.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProcessMemoryCapSource {
    /// Chosen by the AUTO tier from the machine size (the no-config default).
    Auto,
    /// An explicit `limits.max_process_memory_mb` (a fixed cap, or `0` = whole machine).
    Config,
    /// `XERJ_MAX_PROCESS_MEMORY_MB` in the environment (wins over config).
    Env,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolvedProcessMemoryCap {
    /// MiB ceiling to hand to `cap_memory_limit`; `0` = uncapped (whole machine).
    cap_mb: u64,
    /// True when `cap_mb` came from the AUTO tier (drives the log wording).
    auto_tier: bool,
    source: ProcessMemoryCapSource,
    warning: Option<String>,
}

/// Resolve the process-memory cap from config + optional env override, mirroring
/// the `XERJ_SEGMENT_HYDRATION_CACHE_MB` grammar (`auto` | `off` | `unlimited` | `<MiB>`).
///
/// Precedence: env wins over config. Semantics preserved from the flat-cap era —
/// the AUTO sentinel means "tier it", `0` means UNCAPPED (whole machine), and a
/// positive `N` means a fixed `N` MiB ceiling. A bare env `0` is ambiguous
/// (uncapped? auto?) and falls back to config with a warning, exactly as the
/// hydration resolver does.
fn resolve_process_memory_cap(
    effective_limit: u64,
    configured_mb: u64,
    env_value: Option<&str>,
) -> ResolvedProcessMemoryCap {
    use ProcessMemoryCapSource as Src;

    let auto = |source| ResolvedProcessMemoryCap {
        cap_mb: tiered_cap_mb(effective_limit),
        auto_tier: true,
        source,
        warning: None,
    };
    let from_config = || {
        if configured_mb == xerj_common::config::AUTO_PROCESS_MEMORY_MB {
            auto(Src::Auto)
        } else {
            // 0 = uncapped, N = a fixed ceiling — both explicit config choices.
            ResolvedProcessMemoryCap {
                cap_mb: configured_mb,
                auto_tier: false,
                source: Src::Config,
                warning: None,
            }
        }
    };

    match env_value.map(str::trim) {
        Some("auto") => auto(Src::Env),
        Some("off") | Some("unlimited") => ResolvedProcessMemoryCap {
            cap_mb: 0,
            auto_tier: false,
            source: Src::Env,
            warning: None,
        },
        Some(value) => match value.parse::<u64>() {
            Ok(0) => {
                let mut fallback = from_config();
                fallback.warning = Some(
                    "XERJ_MAX_PROCESS_MEMORY_MB=0 is ambiguous; use auto, off, or a positive MiB value; falling back to config"
                        .to_owned(),
                );
                fallback
            }
            Ok(mb) => ResolvedProcessMemoryCap {
                cap_mb: mb,
                auto_tier: false,
                source: Src::Env,
                warning: None,
            },
            Err(_) => {
                let mut fallback = from_config();
                fallback.warning = Some(format!(
                    "invalid XERJ_MAX_PROCESS_MEMORY_MB={value:?}; expected auto, off, unlimited, or a positive MiB value; falling back to config"
                ));
                fallback
            }
        },
        None => from_config(),
    }
}

fn auto_memtable_budget(effective_limit: u64) -> u64 {
    // The 2 GiB floor is clamped by the 50% ceiling on the SAME effective
    // limit, so an 8 GiB cap yields 2 GiB (25% = 2 GiB) and a 2 GiB effective
    // limit yields 1 GiB rather than the floor's 2 GiB. The floor raises a
    // budget on a roomy host; it must never hand out more than half of what
    // the host — or the configured cap — actually allows.
    (effective_limit / 4)
        .max(2 * 1024 * 1024 * 1024)
        .min(effective_limit / 2)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SegmentHydrationBudgetSource {
    Auto,
    Config,
    Env,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolvedSegmentHydrationBudget {
    bytes: u64,
    source: SegmentHydrationBudgetSource,
    warning: Option<String>,
}

fn resolve_segment_hydration_budget(
    effective_limit: u64,
    configured_mb: u64,
    env_value: Option<&str>,
) -> ResolvedSegmentHydrationBudget {
    const MIB: u64 = 1024 * 1024;
    let automatic = effective_limit / 5;
    let explicit = |mb: u64, source| {
        let requested = mb.saturating_mul(MIB);
        let maximum = effective_limit / 2;
        let bytes = requested.min(maximum);
        let warning = (requested > maximum).then(|| {
            format!(
                "requested segment hydration cache {} MiB exceeds 50% of effective memory; clamped to {} MiB",
                mb,
                bytes / MIB
            )
        });
        ResolvedSegmentHydrationBudget {
            bytes,
            source,
            warning,
        }
    };

    match env_value.map(str::trim) {
        Some("auto") => ResolvedSegmentHydrationBudget {
            bytes: automatic,
            source: SegmentHydrationBudgetSource::Env,
            warning: None,
        },
        Some("off") | Some("unlimited") => ResolvedSegmentHydrationBudget {
            bytes: 0,
            source: SegmentHydrationBudgetSource::Env,
            warning: None,
        },
        Some(value) => match value.parse::<u64>() {
            Ok(0) => {
                let mut fallback =
                    resolve_segment_hydration_budget(effective_limit, configured_mb, None);
                fallback.warning = Some(
                    "XERJ_SEGMENT_HYDRATION_CACHE_MB=0 is ambiguous; use auto or off; falling back to config"
                        .to_owned(),
                );
                fallback
            }
            Ok(mb) => explicit(mb, SegmentHydrationBudgetSource::Env),
            Err(_) => {
                let mut fallback =
                    resolve_segment_hydration_budget(effective_limit, configured_mb, None);
                fallback.warning = Some(format!(
                    "invalid XERJ_SEGMENT_HYDRATION_CACHE_MB={value:?}; expected auto, off, unlimited, or a positive MiB value; falling back to config"
                ));
                fallback
            }
        },
        None if configured_mb > 0 => explicit(configured_mb, SegmentHydrationBudgetSource::Config),
        None => ResolvedSegmentHydrationBudget {
            bytes: automatic,
            source: SegmentHydrationBudgetSource::Auto,
            warning: None,
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Initialisation / access
// ─────────────────────────────────────────────────────────────────────────

/// Initialise the process-wide governor from config. Idempotent: the first
/// call wins (subsequent calls return the already-installed governor, so a
/// second `Engine` in-process — e.g. a test — does not re-key the budgets).
pub fn init(config: &Config) -> Arc<ResourceGovernor> {
    let governor = Arc::clone(GOVERNOR.get_or_init(|| Arc::new(build(config))));
    // #1122: every crate that spawns through `xerj_common::thread_spawn`
    // (storage, server, …) consults this same watermark check, so a spawn
    // under memory exhaustion is refused instead of aborting the node.
    xerj_common::thread_spawn::install_spawn_gate(thread_spawn_safe);
    governor
}

/// The installed governor, if [`init`] has run. Engine-only unit tests that
/// construct an [`crate::index::Index`] directly (never calling
/// `Engine::new`) get `None` and skip all admission checks — behaviour is
/// unchanged for them.
pub fn global() -> Option<Arc<ResourceGovernor>> {
    GOVERNOR.get().map(Arc::clone)
}

/// Global form of [`ResourceGovernor::thread_spawn_safe`] (#1122). Without an
/// initialised governor (unit tests, library embedding) there is no watermark
/// to protect, so spawning is unrestricted.
pub fn thread_spawn_safe() -> bool {
    global().map(|g| g.thread_spawn_safe()).unwrap_or(true)
}

fn build(config: &Config) -> ResourceGovernor {
    let limits = &config.limits;

    // ── RSS watermark against the effective memory limit ──
    //
    // Capped by `limits.max_process_memory_mb`, resolved through
    // `resolve_process_memory_cap`: the AUTO default tiers the cap off the
    // machine (half of RAM under 16 GiB, then 8/16/32 GiB), `0` means uncapped
    // (whole machine), an explicit
    // `N` forces a fixed MiB ceiling, and `XERJ_MAX_PROCESS_MEMORY_MB` can
    // override any of it. Every budget below derives from this one number, so
    // capping HERE is what makes the ceiling coherent — capping each dependent
    // budget separately would let their sum exceed the ceiling nobody set.
    //
    // The cap only lowers: a smaller machine, or a smaller cgroup limit, still
    // wins. Users reported ~20 GiB resident for two projects because with no
    // cgroup the derivation base was the whole machine, so a 64 GiB host asked
    // for 16 GiB of memtables and a 12.8 GiB hydration cache before anything
    // else allocated a byte — the AUTO tier is what stops that on a laptop
    // while still letting a big server climb to 16 or 32 GiB.
    let effective_memory_bytes = effective_memory_limit_bytes();
    let resolved_process_cap = resolve_process_memory_cap(
        effective_memory_bytes,
        limits.max_process_memory_mb,
        std::env::var("XERJ_MAX_PROCESS_MEMORY_MB").ok().as_deref(),
    );
    if let Some(warning) = &resolved_process_cap.warning {
        tracing::warn!("{warning}");
    }
    let tiered_memory_limit = cap_memory_limit(effective_memory_bytes, resolved_process_cap.cap_mb);
    // Windows only: the tier above is still physical RAM / cgroup. A no-pagefile
    // box's commit limit is about that RAM, and other processes have already
    // charged most of it. Lower the cap to what this process can still commit,
    // minus a reserve, so the watermark trips before the allocator aborts.
    // Linux and macOS pass `None` and keep `tiered_memory_limit` byte for byte.
    let commit_budget = xerj_common::resource::windows_commit_sample()
        .map(xerj_common::resource::windows_commit_budget);
    let memory_limit_bytes =
        apply_windows_commit_ceiling(tiered_memory_limit, commit_budget.as_ref());
    let commit_capped = memory_limit_bytes < tiered_memory_limit;
    let machine_memory_bytes =
        windows_override_ceiling(effective_memory_bytes, commit_budget.as_ref());
    let startup_starved = commit_budget.as_ref().is_some_and(|budget| {
        budget.reserve_bytes > 0 && budget.available_commit < budget.reserve_bytes
    });

    // ── memtable budget: 0 = auto-derive from the effective (cgroup-aware)
    // memory limit — see `auto_memtable_budget`. Must be derived from the
    // same effective limit as every sibling budget in this function, not
    // raw host RAM: `/proc/meminfo` is not namespace-virtualized, so a
    // host-RAM-based ceiling silently ignores any cgroup / `systemd-run -p
    // MemoryMax=` cap smaller than the host's physical RAM. The 2 GiB floor
    // needs its own ceiling for the same reason — it can exceed the whole
    // effective limit under a small enough cap on its own.
    let memtable_budget_bytes = if limits.max_total_memtable_mb != 0 {
        limits.max_total_memtable_mb.saturating_mul(1024 * 1024)
    } else {
        auto_memtable_budget(memory_limit_bytes)
    };
    let resolved_segment_hydration = resolve_segment_hydration_budget(
        memory_limit_bytes,
        limits.max_segment_hydration_cache_mb,
        std::env::var("XERJ_SEGMENT_HYDRATION_CACHE_MB")
            .ok()
            .as_deref(),
    );
    if let Some(warning) = &resolved_segment_hydration.warning {
        tracing::warn!("{warning}");
    }
    let segment_hydration_budget = SegmentHydrationBudget::new(resolved_segment_hydration.bytes);
    let memory_watermark_bytes = if limits.memory_watermark_percent == 0 {
        0
    } else {
        let pct = limits.memory_watermark_percent.min(100) as u64;
        ((memory_limit_bytes as u128 * pct as u128) / 100) as u64
    };

    // A configured watermark that cannot be enforced is an accepted-and-ignored
    // setting (#204): say so, rather than letting an operator believe the
    // parent circuit breaker is armed when this platform has no RSS probe.
    if memory_watermark_bytes > 0 && xerj_common::resource::current_rss_bytes().is_none() {
        tracing::warn!(
            "limits.memory_watermark_percent={} is configured, but this build cannot read \
             process RSS on {} — the RSS admission watermark is DISABLED (memtable budget and \
             disk flood stage still apply)",
            limits.memory_watermark_percent,
            std::env::consts::OS
        );
    }

    let max_query_memory_bytes = limits.max_query_memory_mb.saturating_mul(1024 * 1024);

    let max_concurrent_searches = (limits.max_concurrent_searches.max(1)) as usize;

    // Bulk concurrency cap: default 8. Bounds the number of concurrent
    // `_bulk` requests in the parse phase. Each bulk materializes ~300 B
    // per action before the memtable admission check fires, so N concurrent
    // 50 k-action bulks would allocate ~N × 15 MiB of parse-phase heap.
    let max_concurrent_bulks = 8usize;

    // One friendly, non-alarming line stating what RAM was detected, the cap
    // chosen and WHY, and how to override it. This is the first-run experience:
    // a laptop user should see a sane 8 GiB cap explained, not a silent 20 GiB.
    let cap_reason = match (
        resolved_process_cap.source,
        resolved_process_cap.auto_tier,
        resolved_process_cap.cap_mb,
    ) {
        (ProcessMemoryCapSource::Auto, _, _) => "auto tier",
        (ProcessMemoryCapSource::Config, _, 0) => {
            "uncapped: whole machine, limits.max_process_memory_mb = 0"
        }
        (ProcessMemoryCapSource::Config, _, _) => "explicit limits.max_process_memory_mb",
        (ProcessMemoryCapSource::Env, true, _) => "auto tier via XERJ_MAX_PROCESS_MEMORY_MB",
        (ProcessMemoryCapSource::Env, _, 0) => "uncapped via XERJ_MAX_PROCESS_MEMORY_MB",
        (ProcessMemoryCapSource::Env, _, _) => "explicit XERJ_MAX_PROCESS_MEMORY_MB",
    };
    if let Some(budget) = commit_budget.as_ref().filter(|_| commit_capped) {
        tracing::info!(
            "{}",
            commit_ceiling_startup_line(effective_memory_bytes, memory_limit_bytes, budget)
        );
    } else if resolved_process_cap.cap_mb == 0 {
        tracing::info!(
            "memory: detected {} usable, using the whole machine (no process cap — {}); \
             set limits.max_process_memory_mb or XERJ_MAX_PROCESS_MEMORY_MB to add one",
            format_gib(effective_memory_bytes),
            cap_reason,
        );
    } else {
        tracing::info!(
            "memory: detected {} usable, using a {} cap ({}); change with \
             limits.max_process_memory_mb or XERJ_MAX_PROCESS_MEMORY_MB (0 = whole machine)",
            format_gib(effective_memory_bytes),
            format_gib(memory_limit_bytes),
            cap_reason,
        );
    }
    if let Some(line) = available_memory_startup_line(
        xerj_common::resource::available_memory_bytes(),
        effective_memory_bytes,
        memory_limit_bytes,
    ) {
        tracing::warn!("{line}");
    }
    if startup_starved {
        tracing::warn!(
            "Windows has no commit left under the reserve — writes will be rejected until \
             commit is free. Enlarge the pagefile or close other programs"
        );
    }

    let memory_release_percent = resolve_memory_release_percent(
        std::env::var("XERJ_MEMORY_BREAKER_RELEASE_PCT")
            .ok()
            .as_deref(),
    );
    let memory_min_engaged = resolve_memory_min_engaged(
        std::env::var("XERJ_MEMORY_BREAKER_MIN_ENGAGED_MS")
            .ok()
            .as_deref(),
    );
    tracing::info!(
        memtable_budget_mb = memtable_budget_bytes / (1024 * 1024),
        memory_release_pct = memory_release_percent,
        memory_min_engaged_ms = memory_min_engaged.as_millis() as u64,
        memory_limit_mb = memory_limit_bytes / (1024 * 1024),
        memory_limit_source = ?resolved_process_cap.source,
        memory_watermark_mb = memory_watermark_bytes / (1024 * 1024),
        memory_watermark_pct = limits.memory_watermark_percent,
        max_query_memory_mb = limits.max_query_memory_mb,
        max_concurrent_searches,
        max_concurrent_bulks,
        disk_flood_pct = limits.disk_flood_stage_percent,
        segment_hydration_cache_mb = resolved_segment_hydration.bytes / (1024 * 1024),
        segment_hydration_cache_source = ?resolved_segment_hydration.source,
        "resource governor initialised (parent circuit breaker)"
    );

    ResourceGovernor {
        segment_hydration_budget,
        segment_hydration_budget_source: resolved_segment_hydration.source,
        memtable_budget_bytes,
        memtable_used_bytes: AtomicU64::new(0),
        memtable_tripped: AtomicBool::new(false),
        memory_limit_bytes,
        machine_memory_bytes,
        memory_watermark_bytes_default: memory_watermark_bytes,
        memory_watermark_bytes: AtomicU64::new(memory_watermark_bytes),
        rss_bytes: AtomicU64::new(if startup_starved {
            current_process_memory_bytes()
        } else {
            0
        }),
        analyze_reserved: Arc::new(AtomicU64::new(0)),
        memory_tripped: AtomicBool::new(startup_starved),
        memory_trip_watermark: AtomicU64::new(if startup_starved {
            memory_watermark_bytes
        } else {
            0
        }),
        memory_flat_since: Mutex::new(None),
        memory_release_percent,
        memory_min_engaged,
        memory_engaged_since: Mutex::new(None),
        memory_band_anchor: AtomicU64::new(0),
        merge_ledger: Arc::new(Mutex::new(MergeLedger::default())),
        max_query_memory_bytes,
        search_pool: Arc::new(Semaphore::new(max_concurrent_searches)),
        max_concurrent_searches,
        search_inflight: Arc::new(AtomicU64::new(0)),
        search_inflight_peak: Arc::new(AtomicU64::new(0)),
        search_pressure: Arc::new(Semaphore::new(1)),
        search_headroom_wait: SEARCH_HEADROOM_WAIT,
        merge_headroom_max_wait: crate::merge_budget::MERGE_HEADROOM_MAX_WAIT,
        bulk_pool: Arc::new(Semaphore::new(max_concurrent_bulks)),
        max_concurrent_bulks,
        disk_flood_pct_default: limits.disk_flood_stage_percent.min(100),
        disk_flood_pct: AtomicU8::new(limits.disk_flood_stage_percent.min(100)),
        disk_threshold_enabled: AtomicBool::new(true),
        disk_blocked: AtomicBool::new(false),
        disk_used_pct: AtomicU64::new(0),
        commit_limit_bytes: commit_budget.as_ref().map(|b| b.commit_limit).unwrap_or(0),
        commit_reserve_bytes: commit_budget.as_ref().map(|b| b.reserve_bytes).unwrap_or(0),
        available_commit_bytes: AtomicU64::new(
            commit_budget
                .as_ref()
                .map(|b| b.available_commit)
                .unwrap_or(0),
        ),
        commit_starved: AtomicBool::new(startup_starved),
        commit_capped,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// System probes (Linux; best-effort with safe fallbacks elsewhere)
// ─────────────────────────────────────────────────────────────────────────

/// Current resident set size of this process, in bytes, via
/// [`xerj_common::resource::current_rss_bytes`] (`/proc/self/statm` on Linux,
/// the physical footprint from `proc_pid_rusage` on macOS). Returns 0 where the platform
/// exposes no probe — the RSS watermark then never trips, which is safe but
/// means no admission control, so [`build`] says so out loud at startup.
///
/// Until #240 this returned a hardcoded 0 on *every* non-Linux platform, so
/// the watermark was dead on macOS however much memory the process took.
pub fn current_rss_bytes() -> u64 {
    xerj_common::resource::current_rss_bytes().unwrap_or(0)
}

/// Merge-pool ceiling for a `watermark` and process `limit` (#2427): the
/// limit less [`SEARCH_READ_RESERVE_PERCENT`], never above the watermark.
/// `0` when the watermark is off; the watermark alone when no limit is known.
fn merge_pool_ceiling(watermark: u64, limit: u64) -> u64 {
    if watermark == 0 || limit == 0 {
        return watermark;
    }
    let reserve = (limit as u128 * SEARCH_READ_RESERVE_PERCENT as u128 / 100) as u64;
    watermark.min(limit.saturating_sub(reserve)).max(1)
}

/// Bytes the memory breaker compares to the watermark.
///
/// Off Windows this is [`current_rss_bytes`]. On Windows it is the max of the
/// working set and the process commit charge: a trimmed working set can sit
/// under the watermark while commit charge is what the next allocation fails
/// on (#1122).
pub fn current_process_memory_bytes() -> u64 {
    let rss = current_rss_bytes();
    match xerj_common::resource::process_commit_charge_bytes() {
        Some(commit) => rss.max(commit),
        None => rss,
    }
}

/// Total system RAM in bytes. Falls back to [`ASSUMED_TOTAL_BYTES`] on a
/// platform with no probe.
///
/// Until #240 the fallback applied to every non-Linux platform, so a 64 GB Mac
/// derived its memtable budget, RSS watermark and hydration cache from a
/// fictional 8 GiB — eight times too small, with no way to tell from the logs.
fn system_total_bytes() -> u64 {
    xerj_common::resource::total_memory_bytes().unwrap_or(ASSUMED_TOTAL_BYTES)
}

/// What to assume when the machine's RAM cannot be read at all. Deliberately
/// unchanged from the pre-#240 constant: shrinking it would quietly re-tune
/// every derived budget on platforms nobody has measured. The assumption is
/// now announced (see [`xerj_common::resource::total_memory_bytes`]) instead of
/// being silently indistinguishable from a real 8 GiB machine.
const ASSUMED_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Effective process memory limit: the cgroup memory limit when one is set
/// (container / `systemd-run -p MemoryMax=`), otherwise total system RAM.
/// Takes the min of the two so a generous cgroup value never exceeds RAM.
pub fn effective_memory_limit_bytes() -> u64 {
    let sys = system_total_bytes().max(1);
    effective_memory_limit(sys, cgroup_memory_limit_bytes())
}

fn effective_memory_limit(sys: u64, cgroup: Option<u64>) -> u64 {
    match cgroup {
        Some(c) if c > 0 && c < sys => c,
        _ => sys,
    }
}

/// Read the cgroup memory limit for THIS process. Handles cgroup v2
/// (`memory.max`, walking up to a parent when a leaf reads `max`) and falls
/// back to cgroup v1 (`memory.limit_in_bytes`). Returns `None` when no
/// finite limit applies.
///
/// A limit file that is missing, unreadable, malformed, `0`, or the v1/v2
/// "no limit" sentinel (`max` / `9223372036854771712`) contributes nothing
/// rather than failing: the caller then falls back to host RAM. Nothing on
/// this path panics, so an unexpected cgroupfs layout degrades to the
/// pre-cgroup behaviour instead of taking the process down at startup.
#[cfg(target_os = "linux")]
fn fold_cgroup_memory_limit(current: Option<u64>, raw: &str) -> Option<u64> {
    const UNLIMITED_V1: u64 = 9_223_372_036_854_771_712;
    let candidate = raw
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0 && *value < UNLIMITED_V1);
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (None, Some(candidate)) => Some(candidate),
        (current, None) => current,
    }
}

/// Walk from `rel` up to the root, folding the tightest finite limit found
/// in `<root><rel>/<file>` at each level. cgroup memory limits are
/// hierarchical in BOTH v1 and v2 — a finite child value does not override a
/// smaller finite ancestor — so the effective limit is the minimum over the
/// whole chain. Levels whose file is missing or unreadable are skipped, so a
/// partially visible hierarchy still yields whatever it does expose.
///
/// `root` is a parameter rather than a constant so the hierarchy walk is
/// testable against a temp-dir fixture without a real cgroupfs mount.
#[cfg(target_os = "linux")]
fn tightest_limit_up_hierarchy(root: &str, rel: &str, file: &str) -> Option<u64> {
    let mut rel = rel.trim().to_string();
    let mut tightest = None;
    loop {
        let full = format!("{root}{rel}/{file}");
        if let Ok(s) = std::fs::read_to_string(&full) {
            tightest = fold_cgroup_memory_limit(tightest, &s);
        }
        if rel.is_empty() || rel == "/" {
            break;
        }
        match rel.rfind('/') {
            Some(0) => rel = String::new(), // step to root next iter
            Some(i) => rel.truncate(i),
            None => break,
        }
    }
    tightest
}

/// The cgroup-relative path of the v1 `memory` controller from one
/// `/proc/self/cgroup` line (`<hierarchy-id>:<controllers>:<path>`).
/// Returns `None` for v2 lines (`0::<path>`, empty controller list) and for
/// controller sets that do not include `memory`.
#[cfg(target_os = "linux")]
fn cgroup_v1_memory_path(line: &str) -> Option<&str> {
    let mut parts = line.splitn(3, ':');
    let _hierarchy_id = parts.next()?;
    let controllers = parts.next()?;
    let path = parts.next()?;
    controllers
        .split(',')
        .any(|c| c == "memory")
        .then_some(path.trim())
}

/// Resolve the memory limit from the contents of `/proc/self/cgroup`,
/// against the v2 unified mount at `v2_root` and the v1 memory-controller
/// mount at `v1_root`. Split out from [`cgroup_memory_limit_bytes`] so the
/// whole resolution — v2 line, v1 line, and the mount-root fallback — is
/// testable against a fixture instead of the host's real cgroupfs.
#[cfg(target_os = "linux")]
fn cgroup_memory_limit_from(self_cgroup: &str, v2_root: &str, v1_root: &str) -> Option<u64> {
    // cgroup v2: /proc/self/cgroup has a single "0::<path>" line, and the
    // limits live at <v2_root><path>/memory.max.
    for line in self_cgroup.lines() {
        if let Some(path) = line.strip_prefix("0::") {
            let tightest = tightest_limit_up_hierarchy(v2_root, path.trim(), "memory.max");
            if tightest.is_some() {
                return tightest;
            }
        }
    }

    // cgroup v1: the memory controller is mounted separately and
    // /proc/self/cgroup carries one "<id>:memory:<path>" line per hierarchy.
    // Walk that path from leaf to root exactly as for v2. Under Docker the
    // cgroupfs is bind-mounted at the container's own directory, so the leaf
    // path from /proc/self/cgroup does not resolve and only the mount root
    // reads — which is the container's real limit. Under a bare
    // `systemd-run -p MemoryMax=` on a v1/hybrid host it is the other way
    // round: the process sits in a leaf slice and the mount root reads
    // unlimited. Folding the whole chain is correct in both cases.
    for line in self_cgroup.lines() {
        if let Some(path) = cgroup_v1_memory_path(line) {
            let tightest = tightest_limit_up_hierarchy(v1_root, path, "memory.limit_in_bytes");
            if tightest.is_some() {
                return tightest;
            }
        }
    }

    // Last resort: /proc/self/cgroup was unreadable or named no memory
    // controller, so read the v1 mount root directly.
    tightest_limit_up_hierarchy(v1_root, "", "memory.limit_in_bytes")
}

#[cfg(target_os = "linux")]
fn cgroup_memory_limit_bytes() -> Option<u64> {
    let self_cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    cgroup_memory_limit_from(&self_cgroup, "/sys/fs/cgroup", "/sys/fs/cgroup/memory")
}

#[cfg(not(target_os = "linux"))]
fn cgroup_memory_limit_bytes() -> Option<u64> {
    None
}

/// `(total_bytes, avail_bytes)` for the filesystem backing `path`, via
/// `statvfs(2)` on Unix or `GetDiskFreeSpaceExW` on Windows.
/// Returns `None` on syscall failure.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub fn disk_stats(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: statvfs fully initialises the struct; we only read scalars.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bsize = if st.f_frsize > 0 {
        st.f_frsize as u64
    } else {
        st.f_bsize as u64
    };
    let total = (st.f_blocks as u64).saturating_mul(bsize);
    let avail = (st.f_bavail as u64).saturating_mul(bsize);
    if total == 0 {
        return None;
    }
    Some((total, avail))
}

#[cfg(target_os = "windows")]
pub fn disk_stats(path: &str) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt as _;

    let p = if path.is_empty() {
        std::path::Path::new(".")
    } else {
        std::path::Path::new(path)
    };
    let abs = std::path::absolute(p).ok()?;
    let candidate = if abs.exists() {
        abs
    } else {
        abs.ancestors().find(|a| a.exists())?.to_path_buf()
    };

    let mut os_str = candidate.into_os_string();
    let s = os_str.to_string_lossy();
    if s.ends_with(':') {
        os_str.push("\\");
    }

    let mut wide: Vec<u16> = os_str.encode_wide().collect();
    wide.push(0);

    let mut free_bytes_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free_bytes: u64 = 0;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_bytes_available,
            &mut total_bytes,
            &mut total_free_bytes,
        )
    };

    if ok != 0 && total_bytes > 0 {
        Some((total_bytes, free_bytes_available))
    } else {
        None
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
pub fn disk_stats(_path: &str) -> Option<(u64, u64)> {
    None
}

/// Used-percentage of the filesystem backing `path` (0..=100). Uses
/// `total - avail` over `total`, matching ES's disk-watermark accounting.
/// Returns 0 when `statvfs` is unavailable (the disk block never trips).
pub fn disk_used_pct(path: &str) -> u64 {
    disk_used_pct_and_avail(path).0
}

/// `(used_pct, avail_bytes)` for the filesystem backing `path` — the same
/// `statvfs` sample as [`disk_used_pct`], plus the raw free-byte figure the
/// flood-stage crossing WARN prints alongside it. Returns `(0, 0)` when
/// `statvfs` is unavailable (the disk block never trips in that case).
pub fn disk_used_pct_and_avail(path: &str) -> (u64, u64) {
    match disk_stats(path) {
        Some((total, avail)) if total > 0 => {
            let used = total.saturating_sub(avail);
            (((used as u128 * 100) / total as u128) as u64, avail)
        }
        _ => (0, 0),
    }
}

/// Parses a disk flood-stage percent override from a
/// `cluster.routing.allocation.disk.watermark.flood_stage` cluster-settings
/// value. Accepts the ES-style percentage string (`"95%"`), a bare numeric
/// string (`"95"`), or a JSON number; the value is clamped to `0..=100`.
/// Returns `None` for anything else — notably, ES's byte-size form
/// (`"500mb"`) isn't supported, only the percentage form.
pub fn parse_flood_stage_pct(v: &Value) -> Option<u8> {
    let pct = match v {
        Value::Number(n) => n.as_u64()?,
        Value::String(s) => s
            .trim()
            .strip_suffix('%')
            .unwrap_or(s.trim())
            .parse()
            .ok()?,
        _ => return None,
    };
    Some(pct.min(100) as u8)
}

/// Parses a boolean cluster setting: a JSON bool or the string `"true"` /
/// `"false"` (what `PUT _cluster/settings` stores for either form).
pub fn parse_setting_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parses the parent memory-breaker watermark override from an
/// `indices.breaker.total.limit` cluster-settings value, resolving it to an
/// absolute byte count. Accepts the two ES-compatible forms:
///
///   * a percentage (`"70%"`) — taken against `machine_bytes` (real machine /
///     cgroup memory), so an operator can climb past the conservative AUTO cap;
///   * a byte-size (`"46gb"`, `"49152mb"`, `"500000000"`, or a JSON number) —
///     an absolute watermark.
///
/// Clamping to `machine_bytes` is the caller's job
/// ([`ResourceGovernor::set_memory_watermark_override`]); this only parses.
/// Returns `None` for a non-positive, malformed, or unrecognised value, which
/// reverts the override to the boot default.
pub fn parse_breaker_total_limit(v: &Value, machine_bytes: u64) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().filter(|b| *b > 0),
        Value::String(s) => {
            let s = s.trim();
            if let Some(pct_str) = s.strip_suffix('%') {
                let pct: f64 = pct_str.trim().parse().ok()?;
                if !(pct.is_finite()) || pct <= 0.0 {
                    return None;
                }
                let pct = pct.min(100.0);
                Some(((machine_bytes as f64) * pct / 100.0) as u64).filter(|b| *b > 0)
            } else {
                parse_byte_size(s)
            }
        }
        _ => None,
    }
}

/// Parses an ES-style byte-size string (`"46gb"`, `"512mb"`, `"1024kb"`,
/// `"4096b"`, or a bare `"500000000"` meaning bytes). Case-insensitive on the
/// unit; the single-letter ES forms (`k`/`m`/`g`/`t`) are also accepted.
/// Returns `None` for a non-positive or malformed value.
fn parse_byte_size(s: &str) -> Option<u64> {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let (num_str, mult) =
        if let Some(n) = lower.strip_suffix("tb").or_else(|| lower.strip_suffix('t')) {
            (n, TB)
        } else if let Some(n) = lower.strip_suffix("gb").or_else(|| lower.strip_suffix('g')) {
            (n, GB)
        } else if let Some(n) = lower.strip_suffix("mb").or_else(|| lower.strip_suffix('m')) {
            (n, MB)
        } else if let Some(n) = lower.strip_suffix("kb").or_else(|| lower.strip_suffix('k')) {
            (n, KB)
        } else if let Some(n) = lower.strip_suffix('b') {
            (n, 1)
        } else {
            (lower.as_str(), 1)
        };
    let value: f64 = num_str.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let bytes = (value * mult as f64) as u64;
    (bytes > 0).then_some(bytes)
}

// ─────────────────────────────────────────────────────────────────────────
// Small formatting helpers
// ─────────────────────────────────────────────────────────────────────────

/// Process-wide merge accounting behind [`MergeReservation`] (#2427).
#[derive(Debug, Default)]
struct MergeLedger {
    reserved: u64,
    in_flight: u64,
    /// Adaptive batch input cap learned from batches that had to yield
    /// under memory pressure. `0` = none. See
    /// `merge_budget::pressure_cap_after_yield`.
    pressure_cap: u64,
    /// Background yields since the last completed batch. See
    /// `merge_budget::merge_must_yield_after`.
    yield_streak: u64,
    /// When a lone background merge was first held back for headroom
    /// since the last admitted batch. See
    /// `merge_budget::lone_merge_waits_for_headroom`.
    headroom_wait_since: Option<std::time::Instant>,
}

/// Holds one merge batch's estimated working set against the RSS watermark
/// (#2427). Returned by [`ResourceGovernor::try_reserve_merge`]; dropping it
/// returns the bytes and the in-flight slot.
#[must_use = "dropping the reservation releases the merge budget at once"]
pub struct MergeReservation {
    ledger: Arc<Mutex<MergeLedger>>,
    bytes: u64,
}

impl MergeReservation {
    /// Bytes this reservation holds.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl std::fmt::Debug for MergeReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeReservation")
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl Drop for MergeReservation {
    fn drop(&mut self) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.reserved = ledger.reserved.saturating_sub(self.bytes);
        ledger.in_flight = ledger.in_flight.saturating_sub(1);
    }
}

/// Releases an analyze reservation when the batch's tokens exist (#1122).
#[derive(Debug)]
pub(crate) struct AnalyzeReservation {
    slot: Option<Arc<AtomicU64>>,
    bytes: u64,
}

impl AnalyzeReservation {
    pub(crate) fn none() -> Self {
        Self {
            slot: None,
            bytes: 0,
        }
    }
}

impl Drop for AnalyzeReservation {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            if self.bytes > 0 {
                slot.fetch_sub(self.bytes, Ordering::AcqRel);
            }
        }
    }
}

fn pct_of(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        0
    } else {
        ((part as u128 * 100) / whole as u128) as u64
    }
}

struct CommitBreakerDetail {
    limit: u64,
    available: u64,
    reserve: u64,
}

/// Admission text for the parent memory breaker.
///
/// `commit == None` is the ordinary RSS sentence: it names
/// `limits.max_process_memory_mb` / `XERJ_MAX_PROCESS_MEMORY_MB` and the live
/// `indices.breaker.total.limit` hint (#2238). A Windows commit ceiling
/// replaces that hint: raising `indices.breaker.total.limit` cannot create
/// commit, and the sentence names the limit and the pagefile instead.
fn memory_breaker_reason(
    rss: u64,
    relation: &str,
    watermark: u64,
    limit: u64,
    commit: Option<CommitBreakerDetail>,
) -> String {
    const MIB: u64 = 1024 * 1024;
    let base = format!(
        "[parent] real memory circuit breaker tripped: rss={}MB {relation} watermark={}MB \
         ({}% of limit={}MB); writes rejected to prevent an out-of-memory kill",
        rss / MIB,
        watermark / MIB,
        pct_of(watermark, limit),
        limit / MIB,
    );
    match commit {
        Some(detail) => format!(
            "{base}. Windows commit limit is {}MB with {}MB still committable \
             (reserve {}MB). Enlarge the pagefile (System Properties → Advanced → \
             Performance → Settings → Advanced → Virtual memory) or close other programs. \
             XERJ_MAX_PROCESS_MEMORY_MB cannot exceed that commit limit",
            detail.limit / MIB,
            detail.available / MIB,
            detail.reserve / MIB,
        ),
        None => format!(
            "{base} (raise limits.max_process_memory_mb or XERJ_MAX_PROCESS_MEMORY_MB, or live \
             with PUT _cluster/settings {{\"persistent\":\
             {{\"indices.breaker.total.limit\":\"<N>gb\"}}}})"
        ),
    }
}

fn commit_ceiling_startup_line(
    physical: u64,
    limit: u64,
    budget: &xerj_common::resource::WindowsCommitBudget,
) -> String {
    const MIB: u64 = 1024 * 1024;
    format!(
        "memory: detected {} usable, using a {} cap (auto tier lowered to the Windows commit \
         ceiling); physical RAM is {}, Windows commit limit is {}MB, {}MB still committable \
         (this process holds {}MB, reserve {}MB). Enlarge the pagefile (System Properties → \
         Advanced → Performance → Settings → Advanced → Virtual memory) or close other \
         programs. limits.max_process_memory_mb and XERJ_MAX_PROCESS_MEMORY_MB cannot exceed \
         that commit limit",
        format_gib(limit),
        format_gib(limit),
        format_gib(physical),
        budget.commit_limit / MIB,
        budget.committable / MIB,
        budget.process_commit / MIB,
        budget.reserve_bytes / MIB,
    )
}

fn human_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if b >= GB {
        format!("{:.1}gb", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1}mb", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1}kb", b as f64 / KB as f64)
    } else {
        format!("{b}b")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2574: a 125 GiB box with ~40 GiB free took the 16 GiB auto tier and
    /// said only "detected 125.4 GiB usable". Warn when the cap is above
    /// what is free; stay quiet when there is room or nothing is known.
    #[test]
    fn startup_warns_when_free_memory_is_below_the_cap() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let line = available_memory_startup_line(Some(10 * GIB), 100 * GIB, 16 * GIB)
            .expect("10 GiB free under a 16 GiB cap must warn");
        assert!(
            line.contains("only 10 GiB of 100 GiB is available"),
            "{line}"
        );
        assert!(line.contains("16 GiB cap"), "{line}");
        assert!(line.contains("limits.max_process_memory_mb"), "{line}");
        assert_eq!(
            available_memory_startup_line(Some(40 * GIB), 125 * GIB, 16 * GIB),
            None
        );
        assert_eq!(
            available_memory_startup_line(None, 125 * GIB, 16 * GIB),
            None
        );
        assert_eq!(available_memory_startup_line(Some(GIB), 125 * GIB, 0), None);
    }

    /// #2427: the reservation follows token density instead of charging every
    /// byte as if it were the densest text. Dense text keeps the flat rate.
    #[test]
    fn analyze_reservation_follows_token_density_2427() {
        assert_eq!(estimated_analyzer_tokens(b"{\"body\":\"a a a\"}"), 4);
        assert_eq!(estimated_analyzer_tokens("\u{4e2d}\u{6587}".as_bytes()), 2);
        assert_eq!(estimated_analyzer_tokens(b"\"deadbeef0123abcd\""), 1);
        assert_eq!(estimated_analyzer_tokens(b"foo\\nbar_baz 3.14"), 5);

        let dense = format!("{{\"body\":\"{}\"}}", "a ".repeat(4096));
        assert_eq!(
            analyze_reserve_bytes_for_doc(dense.as_bytes()),
            analyze_reserve_bytes(dense.len() as u64)
        );

        let prose = format!(
            "{{\"body\":\"{}\"}}",
            "the quick brown fox jumps over lazy dogs ".repeat(100)
        );
        let reserve = analyze_reserve_bytes_for_doc(prose.as_bytes());
        assert!(reserve * 3 < analyze_reserve_bytes(prose.len() as u64) * 2);
        assert!(reserve >= prose.len() as u64 * ANALYZE_RESERVE_PER_INPUT_BYTE_BASE);
    }

    #[test]
    fn analyze_reservation_fits_under_the_watermark_and_rejects_at_it() {
        assert!(analyze_reservation_fits(0, 100, 100, 100));
        assert!(analyze_reservation_fits(1_000, 100, 100, 799));
        assert!(!analyze_reservation_fits(1_000, 100, 100, 800));
        assert!(!analyze_reservation_fits(1_000, 900, 50, 50));
        assert_eq!(analyze_reserve_bytes(8 << 20), (8 << 20) * 28);
    }

    /// The guard is a thin wrapper around [`analyze_reservation_fits`]. The
    /// watermark sits above a live probe so the test does not depend on this
    /// process's RSS, and a second reserve sees the first until it is dropped.
    /// #2427: a bulk that arrives while a breaker is latched is refused
    /// before its parse, and reserves nothing.
    #[test]
    fn bulk_parse_admission_refuses_while_a_breaker_is_latched() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        let governor = build(&cfg);
        let live = governor.observed_process_memory();
        governor
            .memory_watermark_bytes
            .store(live.saturating_add(512 * 1024 * 1024), Ordering::Relaxed);
        governor.rss_bytes.store(live, Ordering::Relaxed);

        governor.memory_tripped.store(true, Ordering::Relaxed);
        let refused = governor.admit_bulk_parse(1024).unwrap_err();
        assert_eq!(refused.http_status(), 429, "{refused}");
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);
        governor.memory_tripped.store(false, Ordering::Relaxed);

        governor.memtable_tripped.store(true, Ordering::Relaxed);
        let refused = governor.admit_bulk_parse(1024).unwrap_err();
        assert_eq!(refused.http_status(), 429, "{refused}");
        assert!(refused.to_string().contains("memtable"), "{refused}");
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);
    }

    /// #2427: an admitted bulk's body is counted against the watermark until
    /// its parse ends, so concurrent bulks cannot each be admitted on the
    /// same stale sample and together cross the cap.
    #[test]
    fn bulk_parse_admission_counts_concurrent_bodies_against_the_watermark() {
        const MIB: u64 = 1024 * 1024;
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        let governor = build(&cfg);
        let live = governor.observed_process_memory();
        governor
            .memory_watermark_bytes
            .store(live.saturating_add(1024 * MIB), Ordering::Relaxed);
        governor.rss_bytes.store(live, Ordering::Relaxed);

        let first = governor.admit_bulk_parse(400 * MIB).unwrap();
        let second = governor.admit_bulk_parse(400 * MIB).unwrap();
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 800 * MIB);
        // A third body would reach the watermark: backpressure, not admission.
        let refused = governor.admit_bulk_parse(400 * MIB).unwrap_err();
        assert_eq!(refused.http_status(), 429, "{refused}");
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 800 * MIB);

        drop(first);
        let third = governor.admit_bulk_parse(400 * MIB).unwrap();
        drop(second);
        drop(third);
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn analyze_reservation_releases_and_sees_a_prior_reserve() {
        let governor = build(&Config::default());
        let live = governor.observed_process_memory();
        let slack = 512 * 1024 * 1024;
        governor
            .memory_watermark_bytes
            .store(live.saturating_add(slack), Ordering::Relaxed);
        governor.rss_bytes.store(live, Ordering::Relaxed);
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);

        let first = governor.reserve_analyze(32 * 1024 * 1024).unwrap();
        assert_eq!(
            governor.analyze_reserved.load(Ordering::Relaxed),
            32 * 1024 * 1024
        );
        let refused = governor.reserve_analyze(480 * 1024 * 1024 + 1).unwrap_err();
        let text = refused.to_string();
        assert!(text.contains("circuit breaker"), "{text}");
        assert!(text.contains("writes rejected"), "{text}");
        assert!(text.contains("limits.max_process_memory_mb"), "{text}");
        assert_eq!(refused.http_status(), 429);

        drop(first);
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);
        let second = governor.reserve_analyze(400 * 1024 * 1024).unwrap();
        drop(second);
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);

        governor.memory_watermark_bytes.store(0, Ordering::Relaxed);
        let disabled = governor.reserve_analyze(u64::MAX).unwrap();
        assert_eq!(governor.analyze_reserved.load(Ordering::Relaxed), 0);
        drop(disabled);
    }

    #[test]
    fn segment_hydration_resolver_is_cgroup_proportional_and_explicit() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let auto = resolve_segment_hydration_budget(8 * GIB, 0, None);
        assert_eq!(auto.bytes, 8 * GIB / 5);
        assert_eq!(auto.source, SegmentHydrationBudgetSource::Auto);

        let tiny = resolve_segment_hydration_budget(256 * 1024 * 1024, 0, None);
        assert_eq!(tiny.bytes, 256 * 1024 * 1024 / 5);

        let config = resolve_segment_hydration_budget(8 * GIB, 1024, None);
        assert_eq!(config.bytes, GIB);
        assert_eq!(config.source, SegmentHydrationBudgetSource::Config);

        let off = resolve_segment_hydration_budget(8 * GIB, 1024, Some("off"));
        assert_eq!(off.bytes, 0);
        assert_eq!(off.source, SegmentHydrationBudgetSource::Env);

        // `unlimited` is an accepted synonym for `off`, so this resolver actually
        // mirrors the XERJ_MAX_PROCESS_MEMORY_MB grammar it claims to (#502).
        let unlimited = resolve_segment_hydration_budget(8 * GIB, 1024, Some("unlimited"));
        assert_eq!(unlimited.bytes, 0);
        assert_eq!(unlimited.source, SegmentHydrationBudgetSource::Env);

        let env = resolve_segment_hydration_budget(8 * GIB, 1024, Some("2048"));
        assert_eq!(env.bytes, 2 * GIB);
        assert_eq!(env.source, SegmentHydrationBudgetSource::Env);
    }

    #[test]
    fn segment_hydration_resolver_clamps_overflow_and_rejects_ambiguous_env_zero() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let clamped = resolve_segment_hydration_budget(8 * GIB, u64::MAX, None);
        assert_eq!(clamped.bytes, 4 * GIB);
        assert!(clamped.warning.is_some());

        let invalid = resolve_segment_hydration_budget(8 * GIB, 1024, Some("bogus"));
        assert_eq!(invalid.bytes, GIB);
        assert_eq!(invalid.source, SegmentHydrationBudgetSource::Config);
        assert!(invalid.warning.is_some());

        let zero = resolve_segment_hydration_budget(8 * GIB, 0, Some("0"));
        assert_eq!(zero.bytes, 8 * GIB / 5);
        assert_eq!(zero.source, SegmentHydrationBudgetSource::Auto);
        assert!(zero.warning.is_some());
    }

    /// The AUTO default (an omitted `max_process_memory_mb`) must pick a cap
    /// from the machine in binary-GiB steps, and the cap must only ever LOWER
    /// the base — never invent headroom the host does not have. This is the
    /// no-config first-run contract: a laptop is capped, a big server climbs.
    ///
    /// The historical bug this whole subsystem exists for: a big machine bought
    /// a hungrier XERJ (~20 GiB resident) because every budget derived from the
    /// uncapped whole-machine base. The tier keeps that base bounded while
    /// still letting a 64/128 GiB box use 16/32 GiB instead of a flat 8.
    #[test]
    fn the_auto_default_tiers_the_cap_and_only_lowers() {
        const GIB: u64 = 1024 * 1024 * 1024;
        const AUTO: u64 = xerj_common::config::AUTO_PROCESS_MEMORY_MB;

        // 200 GiB server, no cgroup → 32 GiB tier, source Auto.
        let big = resolve_process_memory_cap(200 * GIB, AUTO, None);
        assert_eq!(big.cap_mb, 32 * 1024);
        assert!(big.auto_tier);
        assert_eq!(big.source, ProcessMemoryCapSource::Auto);
        assert_eq!(cap_memory_limit(200 * GIB, big.cap_mb), 32 * GIB);
        assert_eq!(auto_memtable_budget(32 * GIB), 8 * GIB);

        // 64 GiB workstation → 16 GiB tier (not the old flat 8), memtables 4 GiB.
        let mid = resolve_process_memory_cap(64 * GIB, AUTO, None);
        assert_eq!(mid.cap_mb, 16 * 1024);
        assert_eq!(cap_memory_limit(64 * GIB, mid.cap_mb), 16 * GIB);
        assert_eq!(auto_memtable_budget(16 * GIB), 4 * GIB);

        // 40 GiB box → 8 GiB tier, min(40, 8) = 8, memtables 2 GiB.
        let laptop = resolve_process_memory_cap(40 * GIB, AUTO, None);
        assert_eq!(laptop.cap_mb, 8 * 1024);
        assert_eq!(cap_memory_limit(40 * GIB, laptop.cap_mb), 8 * GIB);
        assert_eq!(auto_memtable_budget(8 * GIB), 2 * GIB);

        // 8 GiB host → half of RAM, 4 GiB. The old tier was min(8, 8) = 8,
        // the whole machine; a merge storm on that budget is #1122.
        let small = resolve_process_memory_cap(8 * GIB, AUTO, None);
        assert_eq!(small.cap_mb, 4 * 1024);
        assert_eq!(cap_memory_limit(8 * GIB, small.cap_mb), 4 * GIB);
        // The memtable floor stays 2 GiB, the same budget the old 8 GiB
        // tier handed this host, so ingest is not given a smaller buffer.
        assert_eq!(auto_memtable_budget(4 * GIB), 2 * GIB);

        // 6 GiB box → half of RAM, 3 GiB, not min(6, 8) = 6.
        let tiny = resolve_process_memory_cap(6 * GIB, AUTO, None);
        assert_eq!(tiny.cap_mb, 3 * 1024);
        assert_eq!(cap_memory_limit(6 * GIB, tiny.cap_mb), 3 * GIB);
    }

    /// Under 16 GiB the cap is half of RAM. 16 GiB is the old 8 GiB step.
    #[test]
    fn a_host_under_sixteen_gib_is_capped_at_half_its_ram() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(tiered_cap_mb(4 * GIB), 2 * 1024);
        assert_eq!(tiered_cap_mb(8 * GIB), 4 * 1024);
        assert_eq!(tiered_cap_mb(12 * GIB), 6 * 1024);
        // One byte under 16 GiB is still the half-RAM step, in whole MiB.
        assert_eq!(tiered_cap_mb(16 * GIB - 1), 8191);
        assert_eq!(tiered_cap_mb(16 * GIB), 8 * 1024);
        // And the 64 GiB step is unchanged.
        assert_eq!(tiered_cap_mb(64 * GIB - 1), 8 * 1024);
    }

    /// The tier steps exactly on the 64 GiB and 128 GiB binary boundaries.
    #[test]
    fn tiered_cap_steps_on_binary_gib_boundaries() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let cases = [
            (32u64, 8 * 1024u64),
            (40, 8 * 1024),
            (63, 8 * 1024),
            (64, 16 * 1024), // boundary is inclusive at the top of the step
            (65, 16 * 1024),
            (127, 16 * 1024),
            (128, 32 * 1024),
            (256, 32 * 1024),
        ];
        for (gib, expected_mb) in cases {
            assert_eq!(
                tiered_cap_mb(gib * GIB),
                expected_mb,
                "AUTO tier for {gib} GiB effective must be {expected_mb} MiB"
            );
        }
    }

    /// The two explicit escape hatches keep their pre-tier meaning: `0` =
    /// uncapped (whole machine), `N` = a fixed MiB ceiling still min'd with
    /// the machine. Backward compatibility for anyone who set the flat cap.
    #[test]
    fn explicit_config_zero_is_uncapped_and_n_forces_a_fixed_cap() {
        const GIB: u64 = 1024 * 1024 * 1024;

        // 0 = uncapped: the whole machine, the preserved legacy meaning.
        let uncapped = resolve_process_memory_cap(64 * GIB, 0, None);
        assert_eq!(uncapped.cap_mb, 0);
        assert!(!uncapped.auto_tier);
        assert_eq!(uncapped.source, ProcessMemoryCapSource::Config);
        assert_eq!(cap_memory_limit(64 * GIB, uncapped.cap_mb), 64 * GIB);
        assert_eq!(
            auto_memtable_budget(64 * GIB),
            16 * GIB,
            "uncapped = old behaviour"
        );

        // N > 0 = a fixed ceiling of exactly N MiB, still min'd with the machine.
        let fixed = resolve_process_memory_cap(64 * GIB, 4096, None);
        assert_eq!(fixed.cap_mb, 4096);
        assert!(!fixed.auto_tier);
        assert_eq!(fixed.source, ProcessMemoryCapSource::Config);
        assert_eq!(cap_memory_limit(64 * GIB, fixed.cap_mb), 4 * GIB);

        // A fixed cap larger than the machine still only lowers.
        let over = resolve_process_memory_cap(2 * GIB, 8192, None);
        assert_eq!(cap_memory_limit(2 * GIB, over.cap_mb), 2 * GIB);
    }

    /// `XERJ_MAX_PROCESS_MEMORY_MB` mirrors the hydration grammar
    /// (`auto` | `off` | `<MiB>`), wins over config, and treats a bare `0` as
    /// ambiguous — falling back to config with a warning.
    #[test]
    fn env_override_wins_and_mirrors_the_hydration_grammar() {
        const GIB: u64 = 1024 * 1024 * 1024;
        const AUTO: u64 = xerj_common::config::AUTO_PROCESS_MEMORY_MB;

        // `auto` → tier, source Env, beating an explicit config number.
        let a = resolve_process_memory_cap(200 * GIB, 4096, Some("auto"));
        assert_eq!(a.cap_mb, 32 * 1024);
        assert!(a.auto_tier);
        assert_eq!(a.source, ProcessMemoryCapSource::Env);
        assert!(a.warning.is_none());

        // `off` / `unlimited` (trimmed) → uncapped.
        for spelling in ["off", "unlimited", "  off  "] {
            let off = resolve_process_memory_cap(64 * GIB, 4096, Some(spelling));
            assert_eq!(off.cap_mb, 0, "{spelling:?} must mean uncapped");
            assert_eq!(off.source, ProcessMemoryCapSource::Env);
        }

        // A positive number → a fixed cap, source Env.
        let n = resolve_process_memory_cap(64 * GIB, AUTO, Some("2048"));
        assert_eq!(n.cap_mb, 2048);
        assert!(!n.auto_tier);
        assert_eq!(n.source, ProcessMemoryCapSource::Env);

        // A bare `0` is ambiguous → warn and fall back to config (AUTO → tier).
        let zero = resolve_process_memory_cap(64 * GIB, AUTO, Some("0"));
        assert_eq!(zero.cap_mb, 16 * 1024);
        assert_eq!(zero.source, ProcessMemoryCapSource::Auto);
        assert!(zero.warning.is_some());
        // …and if config itself is an explicit 0, the fallback keeps uncapped.
        let zero_cfg = resolve_process_memory_cap(64 * GIB, 0, Some("0"));
        assert_eq!(zero_cfg.cap_mb, 0);
        assert_eq!(zero_cfg.source, ProcessMemoryCapSource::Config);
        assert!(zero_cfg.warning.is_some());

        // Garbage → warn and fall back to config.
        let bad = resolve_process_memory_cap(64 * GIB, AUTO, Some("banana"));
        assert_eq!(bad.cap_mb, 16 * 1024);
        assert_eq!(bad.source, ProcessMemoryCapSource::Auto);
        assert!(bad.warning.is_some());
    }

    /// The cap lowers a budget; it must never invent headroom the host does
    /// not have. A 4 GiB laptop stays a 4 GiB laptop.
    #[test]
    fn the_cap_never_raises_a_budget_above_the_machine() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(cap_memory_limit(4 * GIB, 8192), 4 * GIB);
        assert_eq!(cap_memory_limit(2 * GIB, 8192), 2 * GIB);
        // A cgroup smaller than the cap still wins.
        assert_eq!(cap_memory_limit(512 * 1024 * 1024, 8192), 512 * 1024 * 1024);
    }

    /// #1122, field report #2174. Injected commit numbers, not a live Windows
    /// box: 16 GiB physical, no pagefile, ~4 GiB still committable. The AUTO
    /// tier stays the 8 GiB step (physical RAM is unchanged). The cap the
    /// governor actually uses is the commit ceiling, and a 95% watermark
    /// leaves room for the 100 MiB flush that aborted that run.
    #[test]
    fn windows_commit_lowers_the_cap_without_moving_the_auto_tier() {
        const GIB: u64 = 1024 * 1024 * 1024;
        const MIB: u64 = 1024 * 1024;
        const AUTO: u64 = xerj_common::config::AUTO_PROCESS_MEMORY_MB;
        let physical = 16 * GIB;
        let resolved = resolve_process_memory_cap(physical, AUTO, None);
        assert_eq!(
            resolved.cap_mb,
            8 * 1024,
            "tier input is still physical RAM"
        );
        let tiered = cap_memory_limit(physical, resolved.cap_mb);
        assert_eq!(tiered, 8 * GIB);

        let budget = xerj_common::resource::windows_commit_budget(
            xerj_common::resource::WindowsCommitSample {
                total_phys: physical,
                commit_limit: physical,
                available_commit: 4 * GIB,
                process_commit: 200 * MIB,
            },
        );
        let limited = apply_windows_commit_ceiling(tiered, Some(&budget));
        assert!(limited < tiered);
        assert_eq!(limited, budget.effective_bytes);
        let watermark = limited * 95 / 100;
        assert!(budget.committable - watermark >= 100 * MIB);
        assert_eq!(
            windows_override_ceiling(physical, Some(&budget)),
            budget.effective_bytes,
            "a live breaker raise stops at the commit ceiling"
        );

        // No budget (Linux, macOS, failed probe): the tiered cap is returned.
        assert_eq!(apply_windows_commit_ceiling(tiered, None), tiered);
        assert_eq!(windows_override_ceiling(physical, None), physical);

        // 64 GiB with commit covering RAM: tier stays 16 GiB, ceiling does not bind.
        let big = 64 * GIB;
        let big_tier = cap_memory_limit(big, resolve_process_memory_cap(big, AUTO, None).cap_mb);
        assert_eq!(big_tier, 16 * GIB);
        let covered = xerj_common::resource::windows_commit_budget(
            xerj_common::resource::WindowsCommitSample {
                total_phys: big,
                commit_limit: 96 * GIB,
                available_commit: 80 * GIB,
                process_commit: 512 * MIB,
            },
        );
        assert!(!covered.commit_bound);
        assert_eq!(
            apply_windows_commit_ceiling(big_tier, Some(&covered)),
            big_tier
        );
        assert_eq!(windows_override_ceiling(big, Some(&covered)), big);
    }

    /// The 2 GiB memtable floor must not punch through a smaller ceiling —
    /// otherwise the floor alone would hand out the entire cap, or more than
    /// the machine has.
    #[test]
    fn the_memtable_floor_cannot_exceed_half_the_capped_base() {
        const GIB: u64 = 1024 * 1024 * 1024;
        for base_gib in [1u64, 2, 4, 8, 16, 64] {
            let base = base_gib * GIB;
            let budget = auto_memtable_budget(base);
            assert!(
                budget <= base / 2,
                "memtable budget {budget} exceeds half of a {base_gib} GiB base"
            );
        }
        // Concretely: at the 8 GiB AUTO tier the floor does not apply at all.
        assert_eq!(auto_memtable_budget(8 * GIB), 2 * GIB);
        // And under a 2 GiB cgroup the floor is clamped to 1 GiB, not 2.
        assert_eq!(auto_memtable_budget(2 * GIB), GIB);
    }

    #[test]
    fn auto_memtable_budget_is_cgroup_proportional_and_bounded() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let cases = [
            (GIB, 512 * 1024 * 1024),
            (4 * GIB, 2 * GIB),
            (8 * GIB, 2 * GIB),
            (64 * GIB, 16 * GIB),
        ];
        for (effective_limit, expected) in cases {
            let budget = auto_memtable_budget(effective_limit);
            assert_eq!(
                budget, expected,
                "auto_memtable_budget({effective_limit}) = {budget}, expected {expected}"
            );
            assert!(
                budget <= effective_limit / 2,
                "auto_memtable_budget({effective_limit}) = {budget} exceeds 50% of the limit"
            );
        }
    }

    #[test]
    fn effective_limit_is_positive() {
        assert!(effective_memory_limit_bytes() > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_v2_uses_tightest_finite_ancestor_and_host_limit() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let child_then_parent =
            fold_cgroup_memory_limit(fold_cgroup_memory_limit(None, "8589934592"), "4294967296");
        assert_eq!(child_then_parent, Some(4 * GIB));
        let parent_then_child =
            fold_cgroup_memory_limit(fold_cgroup_memory_limit(None, "4294967296"), "8589934592");
        assert_eq!(parent_then_child, Some(4 * GIB));

        let unlimited_child =
            fold_cgroup_memory_limit(fold_cgroup_memory_limit(None, "max"), "4294967296");
        assert_eq!(unlimited_child, Some(4 * GIB));

        let malformed =
            fold_cgroup_memory_limit(fold_cgroup_memory_limit(None, "not-a-limit"), "max");
        assert_eq!(malformed, None);
        assert_eq!(fold_cgroup_memory_limit(None, "0"), None);
        assert_eq!(fold_cgroup_memory_limit(None, "9223372036854771712"), None);

        assert_eq!(effective_memory_limit(2 * GIB, Some(4 * GIB)), 2 * GIB);
        assert_eq!(effective_memory_limit(8 * GIB, Some(4 * GIB)), 4 * GIB);
    }

    /// Regression test for the cgroup-v1 half of the limit lookup, and for
    /// the v2 leaf that reads the literal string `max`.
    ///
    /// Before this, only the v2 `0::<path>` line of `/proc/self/cgroup` was
    /// parsed and the v1 side blindly read the mount root
    /// `/sys/fs/cgroup/memory/memory.limit_in_bytes`. That root read is the
    /// container's own limit under Docker's v1 bind-mount, but on a v1 or
    /// hybrid host under `systemd-run -p MemoryMax=` the process sits in a
    /// leaf slice while the root reads unlimited — so no limit was found,
    /// the effective limit fell back to host RAM, and every cgroup-aware
    /// budget in `build()` (including the memtable circuit breaker) silently
    /// ignored the cap.
    ///
    /// The walk is driven against a temp-dir fixture rather than a real
    /// cgroupfs so it runs identically on any host, v1, v2 or neither.
    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_hierarchy_walk_handles_v1_v2_max_and_missing_files() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // The v1 "no limit" sentinel; v2 spells the same thing "max".
        const UNLIMITED_V1: &str = "9223372036854771712";

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap().to_string();
        let write = |rel: &str, file: &str, body: &str| {
            let dir = format!("{root}{rel}");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(format!("{dir}/{file}"), body).unwrap();
        };

        // ── cgroup v2: leaf reads the literal "max", a finite ancestor caps ──
        // This is the shape `systemd-run --user --scope -p MemoryMax=1G`
        // produces: the limit lands on the slice, not on the leaf scope.
        write("", "memory.max", "max");
        write("/user.slice", "memory.max", "1073741824");
        write("/user.slice/app.scope", "memory.max", "max");
        assert_eq!(
            tightest_limit_up_hierarchy(&root, "/user.slice/app.scope", "memory.max"),
            Some(GIB),
            "a v2 leaf reading the literal \"max\" must inherit its ancestor's finite limit"
        );

        // A tighter leaf still wins over a looser ancestor, and vice versa.
        write("/user.slice/tight.scope", "memory.max", "536870912");
        assert_eq!(
            tightest_limit_up_hierarchy(&root, "/user.slice/tight.scope", "memory.max"),
            Some(GIB / 2)
        );

        // ── cgroup v1: same walk, different file name and mount root ──
        write("/memory", "memory.limit_in_bytes", UNLIMITED_V1);
        write(
            "/memory/system.slice/xerj.service",
            "memory.limit_in_bytes",
            "2147483648",
        );
        let v1_root = format!("{root}/memory");
        assert_eq!(
            tightest_limit_up_hierarchy(
                &v1_root,
                "/system.slice/xerj.service",
                "memory.limit_in_bytes"
            ),
            Some(2 * GIB),
            "a v1 leaf slice limit must be found, not just the mount root"
        );

        // ── unlimited everywhere, and missing files, both mean "no limit" ──
        // Not 0, and not a panic: `effective_memory_limit` then keeps host RAM.
        write("/unlimited", "memory.max", "max");
        assert_eq!(
            tightest_limit_up_hierarchy(&root, "/unlimited", "memory.max"),
            None
        );
        assert_eq!(
            tightest_limit_up_hierarchy(&root, "/user.slice/no-such.scope", "memory.max"),
            Some(GIB),
            "a missing leaf is skipped, not fatal — ancestors still apply"
        );
        // Nothing readable anywhere on the chain: no limit, no panic, not 0.
        assert_eq!(
            tightest_limit_up_hierarchy(&root, "/does/not/exist", "memory.max"),
            None
        );
        assert_eq!(
            tightest_limit_up_hierarchy("/no/such/cgroupfs", "/a/b", "memory.max"),
            None
        );
        let sys = 4 * GIB;
        assert_eq!(effective_memory_limit(sys, None), sys);

        // ── /proc/self/cgroup line parsing: v1 memory lines vs everything else ──
        assert_eq!(
            cgroup_v1_memory_path("5:memory:/system.slice/xerj.service"),
            Some("/system.slice/xerj.service")
        );
        // Co-mounted controllers are comma-separated.
        assert_eq!(
            cgroup_v1_memory_path("4:cpu,cpuacct,memory:/foo"),
            Some("/foo")
        );
        assert_eq!(cgroup_v1_memory_path("3:cpuset:/foo"), None);
        // A v2 line has an empty controller list and must not match here.
        assert_eq!(cgroup_v1_memory_path("0::/user.slice/app.scope"), None);
        // Malformed lines are ignored rather than panicking.
        assert_eq!(cgroup_v1_memory_path(""), None);
        assert_eq!(cgroup_v1_memory_path("garbage"), None);

        // ── full resolution, as `cgroup_memory_limit_bytes` runs it ──
        // A pure-v2 host: the v2 line resolves and the v1 mount is absent.
        assert_eq!(
            cgroup_memory_limit_from("0::/user.slice/app.scope\n", &root, &v1_root),
            Some(GIB),
            "v2: leaf reads \"max\", the finite ancestor must win"
        );
        // A pure-v1 host under `systemd-run -p MemoryMax=`: there is no v2
        // line to resolve, and the limit is on the leaf slice while the v1
        // mount root reads unlimited. This is the case that was a no-op
        // before: the v1 "<id>:memory:<path>" line was never parsed at all.
        assert_eq!(
            cgroup_memory_limit_from(
                "7:memory:/system.slice/xerj.service\n5:cpuset:/\n",
                "/no/such/cgroupfs",
                &v1_root,
            ),
            Some(2 * GIB),
            "v1: the leaf slice limit must be found, not just the mount root"
        );
        // A hybrid host: a v2 line exists but carries no memory limit, so
        // resolution must fall through to the v1 memory controller.
        assert_eq!(
            cgroup_memory_limit_from(
                "0::/unlimited\n7:memory:/system.slice/xerj.service\n",
                &root,
                &v1_root,
            ),
            Some(2 * GIB),
            "hybrid: an unlimited v2 line must not shadow the v1 limit"
        );
        // Docker v1: the leaf path does not exist under the bind-mounted
        // cgroupfs, so the mount root is the container's real limit.
        write("/docker", "memory.limit_in_bytes", "3221225472");
        assert_eq!(
            cgroup_memory_limit_from(
                "7:memory:/docker/deadbeef\n",
                "/no/such/cgroupfs",
                &format!("{root}/docker"),
            ),
            Some(3 * GIB),
            "v1 bind-mount: an unresolvable leaf must fall back to the mount root"
        );
        // Unreadable /proc/self/cgroup, and no cgroupfs at all: no limit,
        // no panic, no 0 — the caller keeps host RAM.
        assert_eq!(
            cgroup_memory_limit_from("", "/no/such/cgroupfs", "/no/such/cgroupfs/memory"),
            None
        );

        // The real lookup must never panic or report 0 on this host,
        // whatever its cgroup layout is.
        assert!(cgroup_memory_limit_bytes().is_none_or(|limit| limit > 0));
    }

    #[test]
    fn disk_used_pct_bounded() {
        let p = disk_used_pct("/");
        assert!(p <= 100);
    }

    #[test]
    fn this_machine_disk_stats_are_sane() {
        if cfg!(any(unix, target_os = "windows")) {
            let (total, avail) =
                disk_stats(".").expect("disk_stats must succeed on the current directory");
            assert!(total > 0, "total disk space must be positive");
            assert!(avail <= total, "available disk space cannot exceed total");
            let pct = disk_used_pct(".");
            assert!(pct <= 100, "disk used percentage must be <= 100");
        }
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512b");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0mb");
    }

    #[test]
    fn build_from_default_config_trips_nothing() {
        let cfg = Config::default();
        let g = build(&cfg);
        // Fresh: nothing sampled yet, so no trips and ingest is admitted.
        assert!(g.check_ingest_admission().is_ok());
        assert!(g.check_disk_block("i").is_ok());
        assert_eq!(g.max_concurrent_searches(), 64);
    }

    #[test]
    fn memtable_budget_trips_on_refresh() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 100; // 100 MiB ceiling
        cfg.limits.memory_watermark_percent = 0; // isolate the memtable path
        let g = build(&cfg);
        assert!(g.check_ingest_admission().is_ok());
        g.refresh(200 * 1024 * 1024, 0, 0); // 200 MiB used > 100 MiB budget
        let err = g.check_ingest_admission().unwrap_err();
        // 429 + the CircuitBreaking variant (the ES mapper stamps the
        // `circuit_breaking_exception` type; the Display is the bare reason).
        assert_eq!(err.http_status(), 429);
        assert!(matches!(err, XerjError::CircuitBreaking { .. }));
        let rendered = format!("{err}");
        assert!(
            rendered.contains("memtable byte budget exceeded"),
            "{rendered}"
        );
        // #2238: the auto tier derives this budget from the process cap. The
        // remedy has to name that knob, and it has to survive the client's
        // 300-character per-item reason prefix.
        let marker = rendered.find("limits.max_process_memory_mb");
        assert!(
            marker.is_some_and(|at| at < 300),
            "the process-memory cap must be named inside the first 300 chars: {rendered}"
        );
        // Recovery once usage drops back under the budget.
        g.refresh(10 * 1024 * 1024, 0, 0);
        assert!(g.check_ingest_admission().is_ok());
    }

    /// #2238: a multi-index ingest on the 8 GiB auto tier trips the 2 GiB
    /// memtable budget while RSS is still under the watermark. The sampler
    /// used to drain only on the RSS latch, so every later write stayed 429
    /// and the indexed count stayed 0. Memtable pressure must request the
    /// same drain.
    #[test]
    fn memtable_pressure_requests_a_drain_while_rss_is_under_the_watermark() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 100;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let watermark = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(watermark > 0, "this test needs a live RSS watermark");
        g.refresh(200 * 1024 * 1024, watermark / 2, 0);
        assert!(
            g.memtable_breaker_engaged(),
            "summed memtables are over budget"
        );
        assert!(
            !g.memory_breaker_engaged(),
            "RSS at half the watermark must not engage the RSS latch"
        );
        assert!(
            g.ingest_pressure_needs_drain(),
            "memtable pressure alone must request a drain (#2238)"
        );
        assert!(g.check_ingest_admission().is_err());

        g.refresh(10 * 1024 * 1024, watermark / 2, 0);
        assert!(!g.memtable_breaker_engaged());
        assert!(!g.ingest_pressure_needs_drain());
        assert!(g.check_ingest_admission().is_ok());
    }

    /// #948: the sampler's drain wiring keys on this accessor, so it must
    /// track the RSS watermark latch exactly — engaging when the breaker
    /// engages, releasing when it releases. A stale engaged reading drains
    /// forever; a stale released one never drains at all.
    #[test]
    fn memory_breaker_engaged_tracks_the_rss_watermark() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0; // isolate the RSS path
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let watermark = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(watermark > 0, "this test needs a live RSS watermark");
        assert!(!g.memory_breaker_engaged(), "fresh governor is disengaged");
        g.refresh(0, watermark, 0);
        assert!(
            g.memory_breaker_engaged(),
            "RSS at the watermark engages the breaker"
        );
        // #2427: past the minimum engaged time.
        g.age_memory_engaged_for_test(g.memory_min_engaged() + Duration::from_millis(1));
        g.refresh(0, watermark / 2, 0);
        assert!(
            !g.memory_breaker_engaged(),
            "RSS back under the watermark releases it"
        );
    }

    /// #1122: a spawn attempted while the breaker is engaged aborts the node
    /// (std panics on the alternate-signal-stack mmap; release is
    /// panic=abort), so `thread_spawn_safe` must refuse exactly then and
    /// allow it again once pressure releases — the drain-free caller drops
    /// the Arcs inline instead, which is the fallback that path was written
    /// for.
    #[test]
    fn thread_spawn_safe_refuses_under_memory_pressure() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0; // isolate the RSS path
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let watermark = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(watermark > 0, "this test needs a live RSS watermark");
        // Disengaged + this process's real RSS under the watermark.
        assert!(
            g.thread_spawn_safe(),
            "healthy governor allows helper spawns"
        );
        g.refresh(0, watermark, 0);
        assert!(
            !g.thread_spawn_safe(),
            "breaker engaged: spawning would risk #1122's abort"
        );
        // #2427: past the minimum engaged time.
        g.age_memory_engaged_for_test(g.memory_min_engaged() + Duration::from_millis(1));
        g.refresh(0, watermark / 2, 0);
        assert!(
            g.thread_spawn_safe(),
            "pressure released: helper spawns allowed again"
        );
    }

    /// The sentence autoindex matches on (`real memory circuit breaker tripped`
    /// … `writes rejected to prevent an out-of-memory kill`) stays when commit
    /// is not the ceiling. That sentence names the process-memory cap and the
    /// live cluster-settings hint (#2238). Numbers are the historical 16 GiB
    /// fixture, and it does not mention the pagefile.
    #[test]
    fn the_rss_breaker_message_is_unchanged_without_a_commit_ceiling() {
        const MIB: u64 = 1024 * 1024;
        let msg = memory_breaker_reason(15_679 * MIB, ">=", 15_564 * MIB, 16_384 * MIB, None);
        assert_eq!(
            msg,
            "[parent] real memory circuit breaker tripped: rss=15679MB >= watermark=15564MB \
             (94% of limit=16384MB); writes rejected to prevent an out-of-memory kill \
             (raise limits.max_process_memory_mb or XERJ_MAX_PROCESS_MEMORY_MB, or live \
             with PUT _cluster/settings {\"persistent\":{\"indices.breaker.total.limit\":\"<N>gb\"}})"
        );
        assert!(!msg.contains("pagefile"));
    }

    #[test]
    fn a_commit_ceiling_names_the_limit_and_how_to_raise_it() {
        const MIB: u64 = 1024 * 1024;
        const GIB: u64 = 1024 * MIB;
        let msg = memory_breaker_reason(
            3_200 * MIB,
            "under",
            3_061 * MIB,
            3_222 * MIB,
            Some(CommitBreakerDetail {
                limit: 16 * GIB,
                available: 900 * MIB,
                reserve: 1_074 * MIB,
            }),
        );
        assert!(msg.contains("[parent] real memory circuit breaker tripped"));
        assert!(msg.contains("writes rejected to prevent an out-of-memory kill"));
        assert!(msg.contains("Windows commit limit is 16384MB"));
        assert!(msg.contains("900MB still committable"));
        assert!(msg.contains("reserve 1074MB"));
        assert!(msg.contains("pagefile"));
        assert!(msg.contains("Virtual memory"));
        assert!(msg.contains("XERJ_MAX_PROCESS_MEMORY_MB cannot exceed that commit limit"));
        assert!(
            !msg.contains("indices.breaker.total.limit"),
            "raising the breaker cannot create commit Windows will not give: {msg}"
        );

        let line = commit_ceiling_startup_line(
            16 * GIB,
            3_378_511_872,
            &xerj_common::resource::windows_commit_budget(
                xerj_common::resource::WindowsCommitSample {
                    total_phys: 16 * GIB,
                    commit_limit: 16 * GIB,
                    available_commit: 4 * GIB,
                    process_commit: 200 * MIB,
                },
            ),
        );
        assert!(line.contains("memory: detected "));
        assert!(line.contains(" usable, using a "));
        assert!(line.contains(" cap "));
        assert!(line.contains("physical RAM is 16 GiB"));
        assert!(line.contains("Windows commit limit is 16384MB"));
        assert!(line.contains("pagefile"));
        assert!(line.contains("XERJ_MAX_PROCESS_MEMORY_MB"));
    }

    /// RSS is under the watermark. Available commit is under the reserve.
    /// That is the pagefileless shape: the process has not "used its cap",
    /// the machine has run out of commit, and the next allocation would abort.
    #[test]
    fn commit_starvation_rejects_writes_and_releases_when_commit_returns() {
        const MIB: u64 = 1024 * 1024;
        const GIB: u64 = 1024 * MIB;
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let mut g = build(&cfg);
        // Overwrite whatever this host probed. The test injects the shortage.
        g.commit_limit_bytes = 16 * GIB;
        g.commit_reserve_bytes = 512 * MIB;
        g.commit_capped = true;
        g.available_commit_bytes.store(4 * GIB, Ordering::Relaxed);
        g.commit_starved.store(false, Ordering::Relaxed);
        g.memory_tripped.store(false, Ordering::Relaxed);

        let watermark = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(
            watermark > 10 * MIB,
            "rss sample must sit under the watermark"
        );
        g.refresh_memory_observing(10 * MIB, 0, 0, Some(128 * MIB));
        assert!(g.memory_breaker_engaged());
        assert!(!g.thread_spawn_safe());
        let msg = format!("{}", g.check_ingest_admission().unwrap_err());
        assert_eq!(g.check_ingest_admission().unwrap_err().http_status(), 429);
        assert!(msg.contains("rss=10MB under watermark="), "{msg}");
        assert!(msg.contains("Windows commit limit is 16384MB"), "{msg}");
        assert!(msg.contains("128MB still committable"), "{msg}");
        assert!(msg.contains("pagefile"), "{msg}");

        g.refresh_memory_observing(10 * MIB, 0, 0, Some(2 * GIB));
        assert!(
            g.check_ingest_admission().is_ok(),
            "commit back above the reserve releases the breaker"
        );
        assert!(g.thread_spawn_safe());
    }

    /// #1122 x #2427: commit starvation and RSS above the watermark are
    /// independent holds, and neither releases while RSS is at or above the
    /// watermark. Commit coming back does not release a footprint that is
    /// still over the line, however long it has sat flat; RSS falling under
    /// the release point (after the minimum engaged time) does.
    #[test]
    fn commit_recovery_does_not_release_rss_above_the_watermark() {
        const MIB: u64 = 1024 * 1024;
        const GIB: u64 = 1024 * MIB;
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let mut g = build(&cfg);
        g.commit_limit_bytes = 16 * GIB;
        g.commit_reserve_bytes = 512 * MIB;
        g.commit_capped = true;
        g.available_commit_bytes.store(4 * GIB, Ordering::Relaxed);
        g.commit_starved.store(false, Ordering::Relaxed);
        g.memory_tripped.store(false, Ordering::Relaxed);

        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");
        let stuck = wm + 100;
        g.refresh_memory_observing(stuck, 0, 0, Some(128 * MIB));
        assert!(g.memory_breaker_engaged(), "crossing the watermark engages");
        g.refresh_memory_observing(stuck, 0, 0, Some(128 * MIB));
        g.age_memory_engaged_for_test(Duration::from_secs(3600));
        g.refresh_memory_observing(stuck, 0, 0, Some(128 * MIB));
        assert!(g.memory_breaker_engaged());
        let msg = format!("{}", g.check_ingest_admission().unwrap_err());
        assert!(msg.contains("pagefile"), "{msg}");

        for _ in 0..50 {
            g.refresh_memory_observing(stuck, 0, 0, Some(2 * GIB));
            assert!(
                g.memory_breaker_engaged(),
                "commit recovery must not release RSS that is still above the watermark"
            );
        }
        g.refresh_memory_observing(wm / 2, 0, 0, Some(2 * GIB));
        assert!(
            !g.memory_breaker_engaged(),
            "RSS under the release point with commit back releases"
        );
    }

    /// Off Windows there is no commit charge, so the breaker probe is the RSS
    /// sample it just took. Both probes read the kernel counter, and that
    /// read allocates, as do other tests in this process, so one pair can
    /// differ by pages. A quiet pair is the contract.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn process_memory_matches_rss_off_windows() {
        assert!(xerj_common::resource::process_commit_charge_bytes().is_none());
        assert!(xerj_common::resource::available_commit_bytes().is_none());
        assert!(xerj_common::resource::windows_commit_sample().is_none());
        let mut last_process = 0u64;
        let mut last_rss = 0u64;
        for _ in 0..128 {
            last_process = current_process_memory_bytes();
            last_rss = current_rss_bytes();
            if last_process > 0 && last_process == last_rss {
                return;
            }
        }
        panic!(
            "off Windows the breaker probe is RSS; no quiet pair in 128 tries \
             (last process={last_process} rss={last_rss})"
        );
    }

    #[test]
    fn disk_flood_blocks_with_hysteresis() {
        let mut cfg = Config::default();
        cfg.limits.disk_flood_stage_percent = 95;
        let g = build(&cfg);
        let gate = g.disk_flood_admission();
        assert_eq!(gate.flood_pct, 95);
        assert!(gate.threshold_enabled);
        assert!(!gate.blocked);
        assert!(g.check_disk_block("i").is_ok());
        g.refresh(0, 0, 96); // over flood stage
        assert!(g.disk_flood_admission().blocked);
        assert!(g.check_disk_block("i").is_err());
        g.refresh(0, 0, 95); // still within release hysteresis (>= 94)
        assert!(g.check_disk_block("i").is_err());
        g.refresh(0, 0, 90); // clearly recovered
        assert!(g.check_disk_block("i").is_ok());
    }

    #[test]
    fn disk_flood_pct_override_retunes_without_restart() {
        let mut cfg = Config::default();
        cfg.limits.disk_flood_stage_percent = 95;
        let g = build(&cfg);
        g.refresh(0, 0, 96); // over the configured 95% default
        assert!(g.check_disk_block("i").is_err());

        // A runtime override raising the threshold clears the block on the
        // next sample, the way `PUT _cluster/settings` does in ES — no
        // restart needed. 95, not 96, to clear the raised threshold's own
        // release-hysteresis band (release_pct = 97 - 1 = 96).
        g.set_disk_flood_pct_override(Some(97));
        g.refresh(0, 0, 95);
        assert!(g.check_disk_block("i").is_ok());

        // Clearing the override (`None`) reverts to the config default.
        g.set_disk_flood_pct_override(None);
        g.refresh(0, 0, 96);
        assert!(g.check_disk_block("i").is_err());
    }

    #[test]
    fn disk_threshold_disabled_releases_and_suppresses_block() {
        let mut cfg = Config::default();
        cfg.limits.disk_flood_stage_percent = 95;
        let g = build(&cfg);
        g.refresh(0, 0, 97);
        assert!(g.check_disk_block("i").is_err());

        // `threshold_enabled: false` releases an engaged block on the next
        // sample and keeps it off however full the disk gets.
        g.set_disk_threshold_enabled(Some(false));
        g.refresh(0, 0, 97);
        assert!(!g.disk_flood_admission().threshold_enabled);
        assert!(!g.disk_flood_admission().blocked);
        assert!(g.check_disk_block("i").is_ok());
        g.refresh(0, 0, 100);
        assert!(g.check_disk_block("i").is_ok());

        // Unsetting it (`None`) turns the watermark back on.
        g.set_disk_threshold_enabled(None);
        g.refresh(0, 0, 97);
        assert!(g.check_disk_block("i").is_err());
    }

    #[test]
    fn disk_flood_pct_override_is_noop_when_disabled_at_startup() {
        let mut cfg = Config::default();
        cfg.limits.disk_flood_stage_percent = 0; // feature off
        let g = build(&cfg);
        g.set_disk_flood_pct_override(Some(1)); // must not turn it on
        g.refresh(0, 0, 99);
        assert!(g.check_disk_block("i").is_ok());
    }

    #[test]
    fn parse_flood_stage_pct_accepts_es_shapes() {
        assert_eq!(parse_flood_stage_pct(&Value::from("95%")), Some(95));
        assert_eq!(parse_flood_stage_pct(&Value::from("95")), Some(95));
        assert_eq!(parse_flood_stage_pct(&Value::from(95)), Some(95));
        // Clamped, not rejected, past 100.
        assert_eq!(parse_flood_stage_pct(&Value::from("150%")), Some(100));
        // ES's byte-size form isn't supported — no silent misinterpretation.
        assert_eq!(parse_flood_stage_pct(&Value::from("500mb")), None);
        assert_eq!(parse_flood_stage_pct(&Value::Null), None);
    }

    #[test]
    fn parse_setting_bool_accepts_es_shapes() {
        assert_eq!(parse_setting_bool(&Value::from(false)), Some(false));
        assert_eq!(parse_setting_bool(&Value::from("false")), Some(false));
        assert_eq!(parse_setting_bool(&Value::from("true")), Some(true));
        assert_eq!(parse_setting_bool(&Value::from("no")), None);
        assert_eq!(parse_setting_bool(&Value::Null), None);
    }

    /// The `indices.breaker.total.limit` override parser (#1250) must accept
    /// both ES forms — a percentage of real machine memory, and an absolute
    /// byte-size — and reject garbage so the override reverts to the boot
    /// default rather than silently mis-arming the breaker.
    #[test]
    fn parse_breaker_total_limit_accepts_pct_and_bytes() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let machine = 119 * GIB; // the reporter's box

        // Percentage is taken against machine memory, NOT the 16 GiB AUTO cap —
        // this is the whole point: 40% of 119 GiB climbs well past the cap.
        assert_eq!(
            parse_breaker_total_limit(&Value::from("40%"), machine),
            Some(machine * 40 / 100)
        );
        // Clamped to 100%, never above machine memory.
        assert_eq!(
            parse_breaker_total_limit(&Value::from("150%"), machine),
            Some(machine)
        );

        // Absolute byte-size forms (the operator's XERJ_MAX_PROCESS_MEMORY_MB
        // mental model, now live): 46 GiB reproduces today's env+restart fix.
        assert_eq!(
            parse_breaker_total_limit(&Value::from("46gb"), machine),
            Some(46 * GIB)
        );
        assert_eq!(
            parse_breaker_total_limit(&Value::from("49152mb"), machine),
            Some(49152 * 1024 * 1024)
        );
        assert_eq!(
            parse_breaker_total_limit(&Value::from("512kb"), machine),
            Some(512 * 1024)
        );
        // Bare number / JSON number = bytes.
        assert_eq!(
            parse_breaker_total_limit(&Value::from("500000000"), machine),
            Some(500_000_000)
        );
        assert_eq!(
            parse_breaker_total_limit(&Value::from(500_000_000u64), machine),
            Some(500_000_000)
        );

        // Rejections revert to the default (None).
        assert_eq!(parse_breaker_total_limit(&Value::from("0%"), machine), None);
        assert_eq!(
            parse_breaker_total_limit(&Value::from("0gb"), machine),
            None
        );
        assert_eq!(parse_breaker_total_limit(&Value::from(0u64), machine), None);
        assert_eq!(
            parse_breaker_total_limit(&Value::from("bogus"), machine),
            None
        );
        assert_eq!(parse_breaker_total_limit(&Value::Null, machine), None);
    }

    /// The runtime watermark override (#1250): it re-tunes an active watermark
    /// live (no restart), clamps above-RAM requests, reverts on `None`, and is
    /// a no-op when the watermark was disabled at startup — exactly the
    /// contract the disk flood-stage override already honours.
    #[test]
    fn memory_watermark_override_retunes_live_and_clamps() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0; // isolate the RSS path
        cfg.limits.memory_watermark_percent = 95;
        // Force a known, small AUTO-style cap so the override clearly climbs
        // past it — mirrors the 16 GiB cap on the reporter's 119 GiB box.
        cfg.limits.max_process_memory_mb = 16 * 1024;
        let g = build(&cfg);

        let default_wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(default_wm > 0, "this test needs a live RSS watermark");
        let machine = g.machine_memory_bytes;

        // RSS just over the default watermark trips the breaker.
        g.refresh(0, default_wm + 1, 0);
        assert!(g.check_ingest_admission().is_err());

        // Raise the watermark live to 2× the capped default (still within
        // machine memory on any CI box with ≥ a few GiB): the same RSS now
        // admits without a restart.
        let raised = (default_wm * 2).min(machine);
        g.set_memory_watermark_override(Some(raised));
        g.refresh(0, default_wm + 1, 0);
        if raised > default_wm {
            assert!(
                g.check_ingest_admission().is_ok(),
                "raising the watermark live must stop the 429s"
            );
        }
        assert_eq!(g.memory_watermark_bytes.load(Ordering::Relaxed), raised);

        // A request above physical RAM is clamped to machine memory, never
        // beyond — a watermark past RAM cannot protect anything.
        g.set_memory_watermark_override(Some(machine * 4));
        assert_eq!(g.memory_watermark_bytes.load(Ordering::Relaxed), machine);

        // None reverts to the boot default.
        g.set_memory_watermark_override(None);
        assert_eq!(g.memory_watermark_bytes.load(Ordering::Relaxed), default_wm);
    }

    /// When the watermark was disabled at startup (`memory_watermark_percent =
    /// 0`), a runtime override must stay a no-op — it only re-tunes an active
    /// watermark, it never arms the feature. Same rule as the disk override.
    #[test]
    fn memory_watermark_override_is_a_noop_when_disabled_at_startup() {
        let mut cfg = Config::default();
        cfg.limits.memory_watermark_percent = 0; // disabled at boot
        let g = build(&cfg);
        assert_eq!(g.memory_watermark_bytes.load(Ordering::Relaxed), 0);
        g.set_memory_watermark_override(Some(64 * 1024 * 1024 * 1024));
        assert_eq!(
            g.memory_watermark_bytes.load(Ordering::Relaxed),
            0,
            "a disabled watermark must not be armed by a runtime override"
        );
    }

    #[test]
    fn query_alloc_guard_fires() {
        let mut cfg = Config::default();
        cfg.limits.max_query_memory_mb = 1; // 1 MiB
        let g = build(&cfg);
        assert!(g.check_query_alloc(512 * 1024, "t").is_ok());
        let err = g.check_query_alloc(4 * 1024 * 1024, "t").unwrap_err();
        assert_eq!(err.http_status(), 429);
    }

    #[tokio::test]
    async fn global_search_pool_caps_concurrency() {
        // Item 2: a pool of 2 admits exactly 2 concurrent searches; the 3rd
        // acquire blocks until one is released.
        let mut cfg = Config::default();
        cfg.limits.max_concurrent_searches = 2;
        let g = Arc::new(build(&cfg));
        assert_eq!(g.max_concurrent_searches(), 2);

        let p1 = g.acquire_search().await.unwrap();
        let p2 = g.acquire_search().await.unwrap();
        assert_eq!(g.search_inflight(), 2);

        // A 3rd acquire must NOT complete while 2 are held.
        let g3 = Arc::clone(&g);
        let third = tokio::spawn(async move { g3.acquire_search().await.map(|_p| ()) });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), third)
                .await
                .is_err(),
            "3rd search must block while the pool of 2 is full"
        );

        // Release one → a subsequent acquire succeeds and peak stays at 2.
        drop(p1);
        let _p3 = tokio::time::timeout(std::time::Duration::from_millis(500), g.acquire_search())
            .await
            .expect("acquire must proceed once a permit frees")
            .unwrap();
        assert_eq!(g.search_inflight_peak(), 2, "concurrency never exceeded 2");
        drop(p2);
    }

    /// #2427 (b): while the RSS breaker is engaged, searches run one at a
    /// time; released, the full pool is available again.
    #[tokio::test]
    async fn engaged_memory_breaker_serializes_searches() {
        let mut cfg = Config::default();
        cfg.limits.max_concurrent_searches = 8;
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = Arc::new(build(&cfg));
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");

        // Engaged, but under the hard limit: admitted, one at a time.
        g.refresh(0, wm, 0);
        assert!(g.memory_breaker_engaged());
        assert!(
            wm < g.search_hard_limit_bytes(),
            "95% watermark sits under the limit"
        );
        let p1 = g.acquire_search().await.unwrap();
        let g2 = Arc::clone(&g);
        let second = tokio::spawn(async move { g2.acquire_search().await.map(|_p| ()) });
        assert!(
            tokio::time::timeout(Duration::from_millis(150), second)
                .await
                .is_err(),
            "a second search must wait while the breaker is engaged"
        );
        drop(p1);
        let p2 = tokio::time::timeout(Duration::from_millis(500), g.acquire_search())
            .await
            .expect("the gate frees with the first search")
            .unwrap();
        drop(p2);

        // Released: concurrency is back to the pool size.
        g.age_memory_engaged_for_test(Duration::from_secs(60));
        g.refresh(0, 0, 0);
        assert!(!g.memory_breaker_engaged());
        let a = g.acquire_search().await.unwrap();
        let b = tokio::time::timeout(Duration::from_millis(500), g.acquire_search())
            .await
            .expect("a released breaker does not serialize")
            .unwrap();
        assert_eq!(g.search_inflight(), 2);
        drop((a, b));
    }

    /// #2427 (b): at or above the hard process limit a new search is a 429,
    /// not another allocation on a node the kernel is about to kill. It is
    /// rejected only after the drain had [`SEARCH_HEADROOM_WAIT`] to work.
    #[tokio::test(start_paused = true)]
    async fn search_is_rejected_at_the_hard_memory_limit() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let limit = g.search_hard_limit_bytes();
        assert!(limit > 0, "this test needs a live memory limit");

        g.refresh(0, limit, 0);
        let started = tokio::time::Instant::now();
        let err = match g.acquire_search().await {
            Ok(_) => panic!("a search at the hard limit must be rejected"),
            Err(e) => e,
        };
        assert!(
            started.elapsed() >= SEARCH_HEADROOM_WAIT,
            "the search waits for the drain before it is rejected"
        );
        assert_eq!(err.http_status(), 429, "{err}");
        assert_eq!(g.search_inflight(), 0, "a rejected search holds no permit");

        g.refresh(0, limit - 1, 0);
        assert!(
            g.acquire_search().await.is_ok(),
            "just under the hard limit is admitted (serialized)"
        );
    }

    /// #2427 read-budget isolation: a search that arrives at a transient
    /// peak over the hard limit is served once the drain brings RSS back,
    /// instead of failing in milliseconds. The 1 GB gate's raw and MCP
    /// searches all got 429s within 300 ms of a heavy search while RSS fell
    /// back to about 1.5 GB a second later.
    #[tokio::test(start_paused = true)]
    async fn a_search_over_the_limit_waits_for_the_drain_and_is_admitted() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = Arc::new(build(&cfg));
        let limit = g.search_hard_limit_bytes();
        assert!(limit > 0, "this test needs a live memory limit");

        g.refresh(0, limit + limit / 4, 0);
        assert!(g.memory_breaker_engaged());
        let drainer = Arc::clone(&g);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            drainer.refresh(0, limit / 2, 0);
        });
        let started = tokio::time::Instant::now();
        let permit = g
            .acquire_search()
            .await
            .expect("the drain frees headroom inside the wait, so the search is admitted");
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(1_200), "{waited:?}");
        assert!(waited < SEARCH_HEADROOM_WAIT, "{waited:?}");
        assert_eq!(g.search_inflight(), 1);
        drop(permit);
        assert_eq!(g.search_inflight(), 0);
    }

    /// #2427 read-budget isolation: a large background merge in flight that
    /// holds the merge budget does not take the read budget. With process
    /// memory over the hard limit only because of the merge's reservation, a
    /// search is admitted at once (no drain wait), and it runs.
    #[tokio::test(start_paused = true)]
    async fn a_large_merge_in_flight_does_not_starve_searches() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = Arc::new(build(&cfg));
        let limit = g.search_hard_limit_bytes();
        let ceiling = g.merge_pool_ceiling_bytes();
        assert!(
            limit > 0 && ceiling > 0,
            "this test needs a live memory limit"
        );
        assert!(
            ceiling <= limit - limit / 100 * SEARCH_READ_RESERVE_PERCENT,
            "the merge pool stops below the read reserve"
        );

        // A big merge starts on a quiet node and reserves about half the
        // limit: the merge budget is held. The node is quiet enough that
        // the reservation fits under the merge pool, which a lone merge
        // now needs (or a full bounded wait).
        let base = limit / 4;
        g.refresh(0, base, 0);
        let input = (limit / 2) / crate::merge_budget::MERGE_WORKING_SET_NUMERATOR;
        let merge = g
            .try_reserve_merge(input, false)
            .expect("a lone merge starts");
        let held = g.merge_reserved_bytes();
        assert_eq!(held, merge.bytes());
        assert!(held >= limit / 2 - crate::merge_budget::MERGE_WORKING_SET_NUMERATOR);
        // No second merge fits in what is left of the merge pool.
        assert!(g.try_reserve_merge(input / 4, false).is_none());

        // The merge allocates its working set: RSS goes past the hard limit.
        // Its working set plus a little other growth takes RSS past the hard
        // limit, while the non-merge part stays well under it.
        let other = limit / 4 + limit / 16;
        g.refresh(0, base + held + other, 0);
        assert!(base + held + other >= limit);
        assert!(g.memory_breaker_engaged());
        // The merge itself is the one that must give way.
        assert!(
            g.merge_must_yield(false),
            "a merge past the merge pool yields"
        );

        let started = tokio::time::Instant::now();
        let permit = g
            .acquire_search()
            .await
            .expect("a search is admitted while the merge holds the merge budget");
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "admitted without waiting"
        );
        assert_eq!(g.search_inflight(), 1);
        // A second concurrent search queues behind the engaged breaker's
        // single pressure permit and is served when the first one finishes.
        let g2 = Arc::clone(&g);
        let second = tokio::spawn(async move { g2.acquire_search().await.map(|_p| ()) });
        tokio::task::yield_now().await;
        drop(permit);
        second
            .await
            .expect("task")
            .expect("the next search is served too");

        // Once the merge ends, admission is against plain RSS again.
        drop(merge);
        assert_eq!(g.merge_reserved_bytes(), 0);
        assert!(
            g.acquire_search().await.is_err(),
            "no merge bytes to discount"
        );
    }

    /// #2427 read-budget isolation: concurrent merges stop at the merge-pool
    /// ceiling, below the 95% watermark, and an in-flight batch yields once
    /// RSS enters the read reserve, before searches would be rejected.
    #[test]
    fn merges_stop_at_the_merge_pool_ceiling_below_the_read_reserve() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes();
        let limit = g.process_memory_limit_bytes();
        let ceiling = g.merge_pool_ceiling_bytes();
        assert!(wm > 0 && limit > wm, "this test needs a live RSS watermark");
        assert_eq!(ceiling, merge_pool_ceiling(wm, limit));
        assert!(
            ceiling < wm,
            "a 20% read reserve sits under a 95% watermark"
        );

        // RSS at half the ceiling; one merge running.
        g.refresh(0, ceiling / 2, 0);
        let first = g.try_reserve_merge(1, false).expect("first merge starts");
        // A second merge that would end between the ceiling and the
        // watermark used to be admitted; it is refused now.
        let gap = (ceiling + wm) / 2 - ceiling / 2 - first.bytes();
        let input = gap / crate::merge_budget::MERGE_WORKING_SET_NUMERATOR;
        assert!(
            ceiling / 2 + first.bytes() + input * crate::merge_budget::MERGE_WORKING_SET_NUMERATOR
                < wm
        );
        assert!(g.try_reserve_merge(input, false).is_none());

        // The output write of a batch may not take RSS into the reserve.
        assert!(!g.admit_merge_write(None, 0, ceiling - ceiling / 2, false, false));
        assert!(g.admit_merge_write(None, 0, ceiling / 4, false, false));

        // Engaged with RSS inside the read reserve (under the limit): a
        // background batch yields, `_forcemerge` does not.
        g.refresh(0, wm, 0);
        assert!(g.memory_breaker_engaged());
        assert!(wm < limit);
        assert!(g.merge_must_yield(false));
        assert!(!g.merge_must_yield(true));
        drop(first);

        assert_eq!(merge_pool_ceiling(0, limit), 0, "watermark off");
        assert_eq!(merge_pool_ceiling(wm, 0), wm, "no known limit");
        assert_eq!(merge_pool_ceiling(3_000, 3_072), 3_072 - 614);
    }

    #[test]
    fn memory_watermark_releases_with_hysteresis() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");

        g.refresh(0, wm, 0);
        assert!(g.check_ingest_admission().is_err(), "trips at watermark");

        // 98% of watermark is above the 85% release threshold and RSS is
        // still falling (the drain is working): stays tripped.
        g.refresh(0, wm * 98 / 100, 0);
        let err = g.check_ingest_admission().unwrap_err().to_string();
        assert!(
            err.contains("draining below watermark"),
            "an in-band rejection must not claim rss >= watermark: {err}"
        );
        if g.commit_capped {
            assert!(
                err.contains("Windows commit limit"),
                "a commit-capped rejection names the limit instead of the process cap: {err}"
            );
        } else {
            assert!(
                err.contains("limits.max_process_memory_mb"),
                "an RSS rejection must name the process-memory cap: {err}"
            );
        }
        g.refresh(0, wm * 97 / 100, 0);
        assert!(
            g.check_ingest_admission().is_err(),
            "must remain tripped while RSS drains through the band"
        );

        // #2427: 94% and 86% are inside the 85% release band now, and RSS
        // is still falling: stays tripped.
        g.refresh(0, wm * 94 / 100, 0);
        assert!(
            g.check_ingest_admission().is_err(),
            "94% of watermark is inside the 85% release band"
        );
        g.refresh(0, wm * 86 / 100, 0);
        assert!(
            g.check_ingest_admission().is_err(),
            "86% is still in the band"
        );

        // Below 85%, but the breaker engaged moments ago: the minimum
        // engaged time holds it so a drain can run.
        g.refresh(0, wm * 84 / 100, 0);
        assert!(
            g.memory_breaker_engaged(),
            "a release inside the minimum engaged time is the 100 ms flap"
        );
        g.age_memory_engaged_for_test(g.memory_min_engaged() + Duration::from_millis(1));
        g.refresh(0, wm * 84 / 100, 0);
        assert!(
            g.check_ingest_admission().is_ok(),
            "must release once RSS is below 85% of watermark and the minimum has passed"
        );
    }

    /// #1250 review, re-tuned for #2427: a node whose irreducible resident
    /// set settles INSIDE the band after a drain must not stay write-blocked
    /// (and re-draining every `MEMORY_DRAIN_MIN_INTERVAL`) forever. One flat
    /// sample is no longer enough (that released 100-500 ms after engaging
    /// under autoindex); RSS has to stay flat for
    /// [`MEMORY_BAND_STALL_RELEASE`].
    #[test]
    fn memory_hysteresis_releases_when_rss_stalls_inside_the_band() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");

        g.refresh(0, wm + 1, 0);
        assert!(g.memory_breaker_engaged());
        g.refresh(0, wm * 97 / 100, 0);
        assert!(g.memory_breaker_engaged(), "still draining");
        // The drain freed nothing more: RSS is flat inside the band.
        g.refresh(0, wm * 97 / 100, 0);
        assert!(
            g.memory_breaker_engaged(),
            "one flat in-band sample must not release (#2427 flap)"
        );
        // Page-sized wander is still flat.
        g.refresh(0, wm * 97 / 100 + 4096, 0);
        assert!(g.memory_breaker_engaged());
        g.age_memory_engaged_for_test(g.memory_min_engaged() + Duration::from_millis(1));
        g.age_memory_flat_for_test(MEMORY_BAND_STALL_RELEASE + Duration::from_millis(1));
        g.refresh(0, wm * 97 / 100, 0);
        assert!(
            !g.memory_breaker_engaged(),
            "a stalled in-band RSS must release, not latch forever"
        );
        assert!(g.check_ingest_admission().is_ok());
    }

    /// #2427: a drain that keeps lowering RSS through the band restarts the
    /// stall clock, so a slow but real drain is not cut short.
    #[test]
    fn falling_rss_inside_the_band_restarts_the_stall_clock() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(
            wm > 64 * MEMORY_RSS_NOISE_BYTES,
            "this test needs a live RSS watermark"
        );

        g.refresh(0, wm, 0);
        assert!(g.memory_breaker_engaged());
        g.age_memory_engaged_for_test(g.memory_min_engaged() + Duration::from_millis(1));
        let mut rss = wm * 97 / 100;
        g.refresh(0, rss, 0);
        // Stall for most of the window, then fall past the noise band.
        g.age_memory_flat_for_test(MEMORY_BAND_STALL_RELEASE - Duration::from_millis(100));
        rss -= 2 * MEMORY_RSS_NOISE_BYTES;
        g.refresh(0, rss, 0);
        assert!(
            g.memory_breaker_engaged(),
            "a falling drain holds the latch"
        );
        // The clock restarted: the old window has not elapsed for the new
        // anchor, so another flat sample still holds.
        g.age_memory_flat_for_test(Duration::from_millis(200));
        g.refresh(0, rss, 0);
        assert!(
            g.memory_breaker_engaged(),
            "the stall clock must restart when RSS falls past noise"
        );
    }

    /// #2427: the measured flap. Engage at the watermark, dip just under it,
    /// climb back: the latch must stay engaged through the whole cycle
    /// instead of releasing and re-engaging every 100 ms sample.
    #[test]
    fn breaker_does_not_flap_around_the_watermark() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");

        let mut transitions = 0;
        let mut last = g.memory_breaker_engaged();
        for i in 0..200u64 {
            // 2,918 MB / 2,850 MB style oscillation: above, then 1-3% under.
            let rss = if i % 2 == 0 {
                wm + wm / 50
            } else {
                wm - wm / 100 * (1 + i % 3)
            };
            g.refresh(0, rss, 0);
            let now = g.memory_breaker_engaged();
            if now != last {
                transitions += 1;
                last = now;
            }
        }
        assert_eq!(
            transitions, 1,
            "only the first engage; no release/re-engage flap"
        );
    }

    #[test]
    fn release_ratio_and_min_engaged_parse_with_safe_defaults() {
        assert_eq!(
            resolve_memory_release_percent(None),
            DEFAULT_MEMORY_RELEASE_PERCENT
        );
        assert_eq!(
            resolve_memory_release_percent(Some("")),
            DEFAULT_MEMORY_RELEASE_PERCENT
        );
        assert_eq!(resolve_memory_release_percent(Some("90")), 90);
        assert_eq!(resolve_memory_release_percent(Some(" 80% ")), 80);
        assert_eq!(
            resolve_memory_release_percent(Some("100")),
            DEFAULT_MEMORY_RELEASE_PERCENT
        );
        assert_eq!(
            resolve_memory_release_percent(Some("10")),
            DEFAULT_MEMORY_RELEASE_PERCENT
        );
        assert_eq!(
            resolve_memory_release_percent(Some("x")),
            DEFAULT_MEMORY_RELEASE_PERCENT
        );

        assert_eq!(resolve_memory_min_engaged(None), DEFAULT_MEMORY_MIN_ENGAGED);
        assert_eq!(resolve_memory_min_engaged(Some("0")), Duration::ZERO);
        assert_eq!(
            resolve_memory_min_engaged(Some("5000")),
            Duration::from_secs(5)
        );
        assert_eq!(
            resolve_memory_min_engaged(Some("999999999")),
            MAX_MEMORY_MIN_ENGAGED
        );
        assert_eq!(
            resolve_memory_min_engaged(Some("2s")),
            DEFAULT_MEMORY_MIN_ENGAGED
        );

        assert_eq!(memory_release_bytes(2_850, 85), 2_422);
        assert_eq!(memory_release_bytes(0, 85), 0);
        assert_eq!(
            memory_release_bytes(u64::MAX, 85),
            (u64::MAX as u128 * 85 / 100) as u64
        );
    }

    /// #2427: merges reserve their working set process-wide, and background
    /// merges wait while the breaker is engaged.
    #[test]
    fn merge_reservations_are_process_wide_and_respect_the_breaker() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");

        // RSS at half the watermark. The first merge always starts.
        g.refresh(0, wm / 2 + 1, 0);
        // Each estimate is a little over a quarter of the watermark.
        let per = wm / 4 / crate::merge_budget::MERGE_WORKING_SET_NUMERATOR + 1;
        let first = g.try_reserve_merge(per, false).expect("first merge starts");
        assert_eq!(g.merges_in_flight(), 1);
        assert_eq!(g.merge_reserved_bytes(), first.bytes());
        // 50% + 25% + 25% reaches the watermark: the second must wait.
        assert!(g.try_reserve_merge(per, false).is_none());
        // A smaller one still fits.
        let second = g.try_reserve_merge(per / 4, false).expect("fits");
        assert_eq!(g.merges_in_flight(), 2);
        drop(first);
        drop(second);
        assert_eq!(g.merges_in_flight(), 0);
        assert_eq!(g.merge_reserved_bytes(), 0);

        // Breaker engaged: no background merge, a forced one may run alone.
        g.refresh(0, wm, 0);
        assert!(g.memory_breaker_engaged());
        assert!(g.try_reserve_merge(1, false).is_none());
        let forced = g
            .try_reserve_merge(1, true)
            .expect("forcemerge progresses alone");
        assert!(g.try_reserve_merge(1, true).is_none());
        drop(forced);
    }

    /// #2427: a batch whose side-cars dwarf its segment file reserves on its
    /// on-disk footprint, so a second merge that the planned estimate would
    /// admit next to it waits.
    #[test]
    fn merge_reservation_uses_the_on_disk_footprint() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");
        g.refresh(0, wm / 2 + 1, 0);

        // Planned input small enough that two batches fit by the old rule.
        let planned = wm / 16 / crate::merge_budget::MERGE_WORKING_SET_NUMERATOR;
        // On disk, a footprint whose estimate is over a quarter of the watermark.
        let on_disk = wm / 4 / crate::merge_budget::MERGE_ON_DISK_WORKING_SET_NUMERATOR + 1;
        let first = g
            .try_reserve_merge_with_footprint(planned, on_disk, false)
            .expect("the first merge always starts");
        assert_eq!(
            first.bytes(),
            crate::merge_budget::merge_reservation_bytes(planned, on_disk)
        );
        assert!(first.bytes() > crate::merge_budget::merge_working_set_bytes(planned));
        // The same second batch: 50% + >25% + >25% is past the watermark.
        assert!(g
            .try_reserve_merge_with_footprint(planned, on_disk, false)
            .is_none());
        // By the planned estimate alone it would have been admitted.
        let second = g
            .try_reserve_merge(planned, false)
            .expect("planned-only fits");
        drop(second);
        drop(first);
        assert_eq!(g.merge_reserved_bytes(), 0);
    }

    /// #2427: on the 1 GB gate an admitted merge batch grew a 3,072 MB node
    /// to 8,122 MB while the breaker was engaged. A background batch yields
    /// once the breaker is engaged with RSS at the merge-pool ceiling (below
    /// the watermark since read-budget isolation), never when the breaker is
    /// released, never when forced; the yield narrows later batches and
    /// completed batches widen them back.
    #[test]
    fn a_background_merge_yields_at_the_limit_and_narrows_later_batches() {
        const MIB: u64 = 1024 * 1024;
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        let limit = g.process_memory_limit_bytes();
        assert!(wm > 0 && limit > wm, "this test needs a live RSS watermark");

        // Not engaged: keep merging, whatever the RSS under the watermark.
        g.refresh(0, wm - 1, 0);
        assert!(!g.memory_breaker_engaged());
        assert!(!g.merge_must_yield(false));
        // Engaged at the watermark: that is already past the merge-pool
        // ceiling, so a background batch yields; a forced one does not.
        g.refresh(0, wm, 0);
        assert!(g.memory_breaker_engaged());
        assert!(wm >= g.merge_pool_ceiling_bytes());
        assert!(g.merge_must_yield(false));
        assert!(!g.merge_must_yield(true));
        // Engaged at the limit: the same.
        g.refresh(0, limit, 0);
        assert!(g.memory_breaker_engaged());
        assert!(g.merge_must_yield(false));
        assert!(!g.merge_must_yield(true));

        assert_eq!(g.merge_pressure_cap_bytes(), 0);
        assert_eq!(g.note_merge_yield(66 * MIB), 33 * MIB);
        assert_eq!(g.merge_pressure_cap_bytes(), 33 * MIB);
        g.note_merge_completed(MIB);
        assert_eq!(g.merge_pressure_cap_bytes(), 33 * MIB);
        g.note_merge_completed(33 * MIB);
        assert_eq!(g.merge_pressure_cap_bytes(), 66 * MIB);

        // A streak of yields with nothing completing lets one small batch
        // finish, so a search node at the limit still compacts. A completed
        // batch resets the streak.
        g.refresh(0, limit, 0);
        assert!(g.memory_breaker_engaged());
        g.note_merge_completed(66 * MIB);
        for _ in 0..crate::merge_budget::MERGE_YIELD_STREAK_LIMIT {
            assert!(g.merge_must_yield_for(false, 49 * MIB));
            g.note_merge_yield(49 * MIB);
        }
        assert!(!g.merge_must_yield_for(false, 49 * MIB));
        assert!(g.merge_must_yield_for(false, 512 * MIB));
        g.note_merge_completed(49 * MIB);
        assert!(g.merge_must_yield_for(false, 49 * MIB));
    }

    /// #2427 ingest overshoot: on the 1 GB gate every over-cap sample on the
    /// ingest node was a pg-docs batch launched alone the moment the last one
    /// finished, with RSS already at 2.4-2.9 GB. A lone background batch now
    /// waits while RSS is at or above the merge-pool ceiling, starts once the
    /// drain brings it under, and never waits past the bound.
    #[test]
    fn a_lone_background_merge_waits_for_headroom_under_the_merge_pool() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let mut g = build(&cfg);
        let wm = g.memory_watermark_bytes();
        let ceiling = g.merge_pool_ceiling_bytes();
        assert!(
            wm > 0 && ceiling > 0 && ceiling < wm,
            "this test needs a live RSS watermark"
        );

        // Inside the read reserve, under the watermark: not engaged, but the
        // lone background batch waits. `_forcemerge` does not.
        let high = (ceiling + wm) / 2;
        g.refresh(0, high, 0);
        assert!(!g.memory_breaker_engaged());
        assert!(g.try_reserve_merge(1, false).is_none());
        let forced = g
            .try_reserve_merge(1, true)
            .expect("forcemerge never waits");
        drop(forced);
        assert!(g.try_reserve_merge(1, false).is_none());

        // The drain brings RSS under the ceiling: it starts, and the wait
        // clock resets.
        g.refresh(0, ceiling / 2, 0);
        let first = g
            .try_reserve_merge(1, false)
            .expect("starts after the drain");
        drop(first);
        assert!(g.merge_ledger.lock().unwrap().headroom_wait_since.is_none());

        // The wait is bounded: past it, a lone batch starts even inside the
        // reserve, so a node whose live set sits there still compacts.
        g.merge_headroom_max_wait = Duration::ZERO;
        g.refresh(0, high, 0);
        assert!(!g.memory_breaker_engaged());
        let late = g.try_reserve_merge(1, false).expect("bounded wait");
        drop(late);
    }

    /// #2427 1 GB gate on `0eaa8af9`, ingest node (3.5 GB peak, 47 breaker
    /// trips against a 3,072 MB cap): with nothing else merging, a batch
    /// started whenever RSS was under the merge-pool ceiling, even when its
    /// own reservation was larger than the room left. Now it waits while
    /// it does not fit, the next batches are planned to the headroom, and
    /// the wait clock is not restarted by RSS merely being under the
    /// ceiling, so the bounded wait still guarantees it runs.
    #[test]
    fn a_lone_merge_whose_reservation_exceeds_the_headroom_waits_and_shrinks() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let mut g = build(&cfg);
        let ceiling = g.merge_pool_ceiling_bytes();
        assert!(ceiling > 0, "this test needs a live RSS watermark");
        let headroom = 64 * 1024 * 1024;
        let rss = ceiling - headroom;
        g.refresh(0, rss, 0);
        assert!(!g.memory_breaker_engaged());

        // Its reservation alone is more than the headroom: held back.
        let big_input = headroom;
        let big = crate::merge_budget::merge_reservation_bytes(big_input, 0);
        assert!(big >= headroom);
        assert!(g.try_reserve_merge(big_input, false).is_none());
        let (cap, since) = {
            let l = g.merge_ledger.lock().unwrap();
            (l.pressure_cap, l.headroom_wait_since)
        };
        // The next batches are planned to fit what is left.
        assert!(cap > 0 && cap < big_input);
        assert!(crate::merge_budget::merge_reservation_bytes(cap, 0) < headroom);
        assert!(since.is_some(), "the wait clock started");
        // A retry under the ceiling keeps the same clock (it used to reset
        // whenever RSS was under the ceiling).
        assert!(g.try_reserve_merge(big_input, false).is_none());
        assert_eq!(g.merge_ledger.lock().unwrap().headroom_wait_since, since);

        // A batch planned under the narrowed cap fits and starts alone.
        let small = g.try_reserve_merge(cap, false).expect("fits the headroom");
        drop(small);
        assert!(g.merge_ledger.lock().unwrap().headroom_wait_since.is_none());

        // `_forcemerge` is never held back by this check.
        drop(g.try_reserve_merge(big_input, true).expect("forced"));

        // Progress: past the bound the oversized lone batch starts.
        g.merge_headroom_max_wait = Duration::ZERO;
        drop(g.try_reserve_merge(big_input, false).expect("bounded wait"));
    }

    /// #2427: the merge output-write step (doc-values build, stored encode,
    /// FTS build) runs after the last yield checkpoint. It is charged to the
    /// merge ledger before it starts. A step that would take RSS to the hard
    /// limit is refused and leaves the ledger as it was. An admitted step
    /// grows the batch's reservation, which other merges' admission sees,
    /// and dropping the reservation releases all of it.
    #[test]
    fn the_merge_write_step_is_charged_and_refused_at_the_hard_limit() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        let limit = g.process_memory_limit_bytes();
        assert!(wm > 0 && limit > wm, "this test needs a live RSS watermark");

        // RSS at half the watermark: one merge admitted.
        g.refresh(0, wm / 2, 0);
        let input = wm / 16 / crate::merge_budget::MERGE_WORKING_SET_NUMERATOR;
        let mut reservation = g
            .try_reserve_merge(input, false)
            .expect("first merge starts");
        let admitted = reservation.bytes();
        assert_eq!(g.merge_reserved_bytes(), admitted);

        // A write that fits under the limit is admitted and charged.
        let held = admitted;
        let extra = wm / 8;
        assert!(g.admit_merge_write(Some(&mut reservation), held, extra, false, false));
        assert_eq!(reservation.bytes(), held + extra);
        assert_eq!(g.merge_reserved_bytes(), held + extra);
        // Charging again for a smaller step never shrinks the reservation.
        assert!(g.admit_merge_write(Some(&mut reservation), 1, 1, false, false));
        assert_eq!(g.merge_reserved_bytes(), held + extra);

        // The charge is visible to admission: a second batch that fit
        // beside the admitted estimate no longer fits beside the write.
        let second = (wm / 2 - admitted) / crate::merge_budget::MERGE_WORKING_SET_NUMERATOR - 1;
        assert!(wm / 2 + admitted + second * crate::merge_budget::MERGE_WORKING_SET_NUMERATOR < wm);
        assert!(g.try_reserve_merge(second, false).is_none());

        // A write that would reach the hard limit is refused, and the
        // ledger does not change, background or not engaged.
        let before = g.merge_reserved_bytes();
        let too_big = limit - wm / 2;
        assert!(!g.admit_merge_write(Some(&mut reservation), held, too_big, false, false));
        assert_eq!(g.merge_reserved_bytes(), before);
        assert_eq!(reservation.bytes(), held + extra);
        // No reservation (no governor at launch): only the decision.
        assert!(!g.admit_merge_write(None, 0, too_big, false, false));
        assert!(g.admit_merge_write(None, 0, 1, false, false));
        // A #2468 streak-exempt batch and `_forcemerge` still write.
        assert!(g.admit_merge_write(None, 0, too_big, false, true));
        assert!(g.admit_merge_write(Some(&mut reservation), held, too_big, true, false));
        assert_eq!(reservation.bytes(), held + too_big);
        assert_eq!(g.merge_reserved_bytes(), held + too_big);

        drop(reservation);
        assert_eq!(g.merge_reserved_bytes(), 0);
        assert_eq!(g.merges_in_flight(), 0);
    }

    /// #2427: in the 1 GB gate the search node rested between the merge-pool
    /// ceiling and the watermark, so the breaker never engaged, the #2468
    /// exemption never applied, and every write step was refused: 70 yields
    /// of 1-10 MB batches and nothing merged. After the streak, a small
    /// batch's write is checked against the hard limit and runs.
    #[test]
    fn a_small_batch_write_runs_after_a_streak_below_the_watermark() {
        const MIB: u64 = 1024 * 1024;
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        let limit = g.process_memory_limit_bytes();
        let ceiling = g.merge_pool_ceiling_bytes();
        assert!(
            ceiling < wm && wm < limit,
            "this test needs ceiling < watermark < limit"
        );
        // Resting above the ceiling, below the watermark: not engaged.
        let rss = ceiling + (wm - ceiling) / 2;
        g.refresh(0, rss, 0);
        assert!(!g.memory_breaker_engaged());
        let small = 6 * MIB;
        let large = crate::merge_budget::MERGE_YIELD_STREAK_MAX_INPUT_BYTES + 1;
        // Fits under the hard limit, not under the ceiling.
        let write = (limit - rss) / 2;
        assert!(!g.merge_write_exempt(false, small));

        // Before the streak: refused, as before.
        assert!(!g.admit_merge_write_for(None, small, write, false, false, small));
        for _ in 0..crate::merge_budget::MERGE_YIELD_STREAK_LIMIT {
            g.note_merge_yield(small);
        }
        // The old exemption still does not apply below the watermark ...
        assert!(!g.merge_write_exempt(false, small));
        // ... but the small batch's write now runs, charged to the ledger.
        let mut reservation = g.try_reserve_merge(small, true).expect("forced admission");
        let held = reservation.bytes();
        assert!(g.admit_merge_write_for(Some(&mut reservation), held, write, false, false, small));
        assert_eq!(reservation.bytes(), held + write);
        drop(reservation);
        // A large batch, or a write that would reach the hard limit, is
        // still refused.
        assert!(!g.admit_merge_write_for(None, large, write, false, false, large));
        assert!(!g.admit_merge_write_for(None, small, limit - rss, false, false, small));
        // A completed batch ends the streak.
        g.note_merge_completed(small);
        assert!(!g.admit_merge_write_for(None, small, write, false, false, small));
    }

    /// #2427 x #2468: a small background batch that the yield-streak rule
    /// lets run to completion is not refused at its output write, even with
    /// RSS at the hard limit. A large batch, a batch before the streak, and
    /// a batch the plain rule never stopped get no exemption. A completed
    /// batch ends it.
    #[test]
    fn a_streak_exempt_batch_is_not_refused_at_its_write() {
        const MIB: u64 = 1024 * 1024;
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        let limit = g.process_memory_limit_bytes();
        assert!(wm > 0 && limit > wm, "this test needs a live RSS watermark");
        let small = 49 * MIB;
        let large = crate::merge_budget::MERGE_YIELD_STREAK_MAX_INPUT_BYTES + 1;
        let write = 256 * MIB;

        // Below the limit the plain rule never stops a batch: no exemption,
        // and the write decision is the hard-limit check alone.
        g.refresh(0, wm / 2, 0);
        assert!(!g.merge_write_exempt(false, small));

        // Engaged at the limit, before the streak: the small batch yields at
        // its checkpoints and its write would be refused.
        g.refresh(0, limit, 0);
        assert!(g.memory_breaker_engaged());
        assert!(!g.merge_write_exempt(false, small));
        let exempt = g.merge_write_exempt(false, small);
        assert!(!g.admit_merge_write(None, small, write, false, exempt));

        // After the streak, #2468 lets the small batch run, and so does its
        // write. The large batch still yields and its write is still refused.
        for _ in 0..crate::merge_budget::MERGE_YIELD_STREAK_LIMIT {
            g.note_merge_yield(small);
        }
        assert!(!g.merge_must_yield_for(false, small));
        assert!(g.merge_write_exempt(false, small));
        let exempt = g.merge_write_exempt(false, small);
        let mut reservation = g.try_reserve_merge(small, true).expect("forced admission");
        let held = reservation.bytes();
        assert!(g.admit_merge_write(Some(&mut reservation), held, write, false, exempt));
        // The exempt write is still charged to the ledger.
        assert_eq!(reservation.bytes(), held + write);
        assert_eq!(g.merge_reserved_bytes(), held + write);
        drop(reservation);
        assert!(!g.merge_write_exempt(false, large));
        let exempt = g.merge_write_exempt(false, large);
        assert!(!g.admit_merge_write(None, large, write, false, exempt));

        // `_forcemerge` is never stopped by either rule, so it needs no
        // exemption; its write runs on `forced` alone.
        assert!(!g.merge_write_exempt(true, small));
        assert!(g.admit_merge_write(None, small, write, true, false));

        // A completed batch resets the streak: no exemption again.
        g.note_merge_completed(small);
        assert!(!g.merge_write_exempt(false, small));
    }

    /// #2427: the 1 GB release gate released at rss=5,121MB against a
    /// 2,918MB watermark (the old #1981 flat-footprint release), re-engaged
    /// 100 ms later at 5,125MB, and was OOM-killed at 8.4 GB. A footprint at
    /// or above the watermark must hold the latch however long it sits flat,
    /// whatever page-sized noise it shows, and however long the breaker has
    /// been engaged.
    #[test]
    fn flat_rss_above_the_watermark_never_releases() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(
            wm > MEMORY_RSS_NOISE_BYTES,
            "this test needs a live RSS watermark"
        );
        let stuck = wm + wm / 4;

        g.refresh(0, stuck, 0);
        assert!(g.memory_breaker_engaged(), "crossing the watermark engages");
        for i in 0..200u64 {
            g.age_memory_flat_for_test(Duration::from_secs(3600));
            g.age_memory_engaged_for_test(Duration::from_secs(3600));
            let rss = match i % 4 {
                0 => stuck,
                1 => stuck + 4096,
                2 => stuck - 8192,
                _ => wm,
            };
            g.refresh(0, rss, 0);
            assert!(
                g.memory_breaker_engaged(),
                "rss={rss} >= watermark={wm} must never release (sample {i})"
            );
            assert!(g.check_ingest_admission().is_err());
        }
        g.refresh(0, wm - 1, 0);
        assert!(
            g.memory_breaker_engaged(),
            "one byte under the watermark is inside the release band"
        );
        g.refresh(0, wm / 2, 0);
        assert!(
            !g.memory_breaker_engaged(),
            "under the release point releases"
        );
    }

    /// #2427: the latch's release decision is a pure function of the
    /// sample, so walk a long random-ish RSS trace (the gate's 2,750-5,125MB
    /// shape scaled to the watermark) and check every release lands strictly
    /// under the watermark and every engage edge at or above it.
    #[test]
    fn no_release_edge_at_or_above_the_watermark_on_a_noisy_trace() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");
        let mut x: u64 = 0x2427;
        let mut was = false;
        let mut releases = 0;
        for _ in 0..5_000 {
            // xorshift: deterministic, no rand dependency.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // 80%..180% of the watermark.
            let rss = wm / 100 * (80 + x % 101);
            g.age_memory_flat_for_test(Duration::from_secs(60));
            g.age_memory_engaged_for_test(Duration::from_secs(60));
            g.refresh(0, rss, 0);
            let now = g.memory_breaker_engaged();
            if was && !now {
                releases += 1;
                assert!(rss < wm, "released at rss={rss} >= watermark={wm}");
            }
            if !was && now {
                assert!(rss >= wm, "engaged at rss={rss} < watermark={wm}");
            }
            was = now;
        }
        assert!(releases > 0, "the trace should exercise releases");
    }

    /// #1981: while the breaker stays engaged and RSS does not fall, drain
    /// requests must back off. An 18-hour stuck footprint sampled on the old
    /// 2 s cadence is 32_400 requests; the doubled schedule stays in the
    /// dozens. Falling RSS returns to the 2 s floor.
    #[test]
    fn drain_requests_back_off_when_rss_does_not_fall() {
        let min = Duration::from_secs(2);
        let mut schedule = MemoryDrainSchedule::new(min);
        let t0 = Instant::now();
        let horizon = Duration::from_secs(18 * 60 * 60);
        let stuck = 64 * MEMORY_RSS_NOISE_BYTES;
        let mut fires = 0u64;
        let mut t = Duration::ZERO;
        let mut step = 0u64;
        while t < horizon {
            // Alternate a page either side of the stuck footprint. The old
            // schedule treated every downward byte as a successful drain
            // and snapped back to 2 s.
            let rss = if step.is_multiple_of(2) {
                stuck
            } else {
                stuck + 4096
            };
            if schedule.observe(t0 + t, rss, true) {
                fires += 1;
            }
            t += min;
            step += 1;
        }
        let unbounded = horizon.as_secs() / min.as_secs();
        assert!(
            fires < unbounded / 50,
            "stuck RSS requested {fires} drains over 18h; the 2s loop would be {unbounded}"
        );
        assert!(fires > 10, "backoff must still retry, got {fires}");

        // A drop resets the spacing: the next request is one floor later,
        // not a doubled gap. Step a full cap ahead so this request is due
        // no matter where in the capped window the 18 h loop stopped.
        let now = t0 + horizon + MEMORY_DRAIN_MAX_INTERVAL;
        let dropped = stuck - MEMORY_RSS_NOISE_BYTES - 1;
        assert!(
            schedule.observe(now, dropped, true),
            "a lower RSS is due work"
        );
        assert!(
            !schedule.observe(now + min - Duration::from_millis(1), dropped, true),
            "the reset spacing is still the floor, not immediate"
        );
        assert!(schedule.observe(now + min, dropped - MEMORY_RSS_NOISE_BYTES - 1, true));

        // Disengage forgets the episode; the next engage fires at once.
        assert!(!schedule.observe(now + min + min, dropped, false));
        assert!(schedule.observe(now + min + min, dropped, true));
    }

    /// #2427: the latch holds above the watermark now, so a climbing
    /// footprint must not wait out a backed-off gap. Back the schedule off
    /// on a flat footprint, then climb: the next sample past the minimum
    /// spacing drains, and a further flat sample is back on the floor.
    #[test]
    fn a_climb_while_engaged_drains_inside_a_backed_off_gap() {
        let min = Duration::from_secs(2);
        let mut schedule = MemoryDrainSchedule::new(min);
        let t0 = Instant::now();
        let stuck = 64 * MEMORY_RSS_NOISE_BYTES;
        let mut t = Duration::ZERO;
        while t < Duration::from_secs(600) {
            schedule.observe(t0 + t, stuck, true);
            t += min;
        }
        assert!(schedule.interval > min, "a flat footprint backs off");
        let last = schedule.last_request.expect("requested at least once");
        let climbed = stuck + MEMORY_RSS_NOISE_BYTES + 1;
        assert!(
            !schedule.observe(last + min / 2, climbed, true),
            "the minimum spacing still applies to a climb"
        );
        assert!(
            schedule.observe(last + min, climbed, true),
            "a climb drains at the minimum spacing, not after the backed-off gap"
        );
        assert_eq!(schedule.interval, min);
        assert!(
            !schedule.observe(last + min + Duration::from_millis(100), climbed, true),
            "one climb is one request"
        );
    }

    /// #1250 x #1323: the watermark is live-tunable. Raising it while the
    /// breaker is engaged must release at once whenever RSS is under the NEW
    /// watermark — not wait for RSS to fall below the release point — or the live fix
    /// #1323 shipped would be undone by the band.
    #[test]
    fn live_watermark_raise_releases_without_the_hysteresis_band() {
        let mut cfg = Config::default();
        cfg.limits.max_total_memtable_mb = 0;
        cfg.limits.memory_watermark_percent = 95;
        cfg.limits.max_process_memory_mb = 1024;
        let g = build(&cfg);
        let wm = g.memory_watermark_bytes.load(Ordering::Relaxed);
        assert!(wm > 0, "this test needs a live RSS watermark");
        let rss = wm + 1;
        g.refresh(0, rss, 0);
        assert!(g.check_ingest_admission().is_err());

        // Raise just above the current RSS: RSS is now inside the new
        // watermark's release band, and still (barely) falling.
        let raised = (rss + rss / 50).min(g.machine_memory_bytes);
        assert!(raised > rss, "test box must have headroom over 1 GiB");
        g.set_memory_watermark_override(Some(raised));
        g.refresh(0, rss - 1, 0);
        assert!(
            g.check_ingest_admission().is_ok(),
            "a live raise above RSS must stop the 429s on the next sample"
        );

        // The band still applies to a trip against the raised watermark.
        g.refresh(0, raised, 0);
        assert!(g.check_ingest_admission().is_err());
        g.refresh(0, raised * 98 / 100, 0);
        assert!(g.check_ingest_admission().is_err());
    }
}
