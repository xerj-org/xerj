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
//!     allocation guard.
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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use xerj_common::config::Config;
use xerj_common::XerjError;

/// The process-wide governor singleton.
static GOVERNOR: OnceLock<Arc<ResourceGovernor>> = OnceLock::new();

/// Interval at which the background sampler refreshes the RSS / memtable /
/// disk atomics. Kept short so the RSS admission has a tight overshoot margin
/// under a runaway ingest — the sampler runs on a dedicated OS thread (see
/// `Engine::spawn_resource_sampler`), so this cadence is honoured even when the
/// tokio pool is saturated.
pub const SAMPLE_INTERVAL_MS: u64 = 100;

/// Process-wide resource governor. See the module docs.
pub struct ResourceGovernor {
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
    /// RSS admission threshold = `memory_limit_bytes * pct/100`. `0` = off.
    memory_watermark_bytes: u64,
    /// Last sampled resident set size of this process.
    rss_bytes: AtomicU64,
    /// Latched: last sample crossed the RSS watermark.
    memory_tripped: AtomicBool,

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

    // ── item 3: disk flood-stage write block ────────────────────────────
    /// Used-percentage watermark that engages the write block. `0` = off.
    disk_flood_pct: u8,
    /// Latched: the data-dir filesystem is at/over the flood-stage watermark.
    disk_blocked: AtomicBool,
    /// Last sampled used-percentage of the data-dir filesystem.
    disk_used_pct: AtomicU64,
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
                 limits.max_total_memtable_mb or slow ingest)",
                used / (1024 * 1024),
                self.memtable_budget_bytes / (1024 * 1024),
            )));
        }
        if self.memory_tripped.load(Ordering::Relaxed) {
            let rss = self.rss_bytes.load(Ordering::Relaxed);
            return Err(XerjError::circuit_breaking(format!(
                "[parent] real memory circuit breaker tripped: rss={}MB >= watermark={}MB \
                 ({}% of limit={}MB); writes rejected to prevent an out-of-memory kill",
                rss / (1024 * 1024),
                self.memory_watermark_bytes / (1024 * 1024),
                pct_of(self.memory_watermark_bytes, self.memory_limit_bytes),
                self.memory_limit_bytes / (1024 * 1024),
            )));
        }
        Ok(())
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
                    self.disk_flood_pct,
                ),
            ));
        }
        Ok(())
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
    pub async fn acquire_search(&self) -> Result<SearchPermit, XerjError> {
        let permit = Arc::clone(&self.search_pool)
            .acquire_owned()
            .await
            .map_err(|_| XerjError::internal("global search pool closed — shutting down"))?;
        let now = self.search_inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.search_inflight_peak.fetch_max(now, Ordering::Relaxed);
        Ok(SearchPermit {
            _permit: permit,
            inflight: Arc::clone(&self.search_inflight),
        })
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

    // ── Sampler surface (called by the background task) ─────────────────

    /// Refresh every sampled atomic in one call (memtable, RSS, disk).
    /// Convenience wrapper used by tests; the live sampler calls
    /// [`Self::refresh_memory_disk`] and [`Self::refresh_memtable`]
    /// SEPARATELY so a contended memtable read can never delay the memory
    /// admission update (see `Engine::spawn_resource_sampler`).
    pub fn refresh(&self, memtable_used: u64, rss: u64, disk_used_pct: u64) {
        self.refresh_memory_disk(rss, disk_used_pct);
        self.refresh_memtable(memtable_used);
    }

    /// Update the RSS + disk atomics and their latched trip flags. Depends on
    /// NOTHING but two syscalls — never blocks on an engine lock — so the
    /// parent memory breaker stays responsive even while a turbo batch holds
    /// every memtable shard's write lock.
    pub fn refresh_memory_disk(&self, rss: u64, disk_used_pct: u64) {
        // ── RSS watermark ──
        self.rss_bytes.store(rss, Ordering::Relaxed);
        let mem_next = self.memory_watermark_bytes != 0 && rss >= self.memory_watermark_bytes;
        if mem_next != self.memory_tripped.swap(mem_next, Ordering::Relaxed) {
            if mem_next {
                tracing::warn!(
                    rss_mb = rss / (1024 * 1024),
                    watermark_mb = self.memory_watermark_bytes / (1024 * 1024),
                    "RSS crossed the memory watermark — engaging the parent memory circuit breaker (writes → 429)"
                );
            } else {
                tracing::info!(
                    rss_mb = rss / (1024 * 1024),
                    "RSS back below the memory watermark — releasing the memory circuit breaker"
                );
            }
        }

        // ── disk flood stage (1% release hysteresis to avoid flapping) ──
        if self.disk_flood_pct != 0 {
            self.disk_used_pct.store(disk_used_pct, Ordering::Relaxed);
            let cur = self.disk_blocked.load(Ordering::Relaxed);
            let release_pct = (self.disk_flood_pct as u64).saturating_sub(1);
            let next = if cur {
                disk_used_pct >= release_pct
            } else {
                disk_used_pct >= self.disk_flood_pct as u64
            };
            if next != cur {
                if next {
                    tracing::warn!(
                        used_pct = disk_used_pct,
                        flood_pct = self.disk_flood_pct,
                        "disk flood-stage watermark crossed — engaging read_only_allow_delete write block"
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
            memtable_used_bytes: self.memtable_used_bytes.load(Ordering::Relaxed),
            memtable_budget_bytes: self.memtable_budget_bytes,
            memtable_tripped: self.memtable_tripped.load(Ordering::Relaxed),
            rss_bytes: self.rss_bytes.load(Ordering::Relaxed),
            memory_limit_bytes: self.memory_limit_bytes,
            memory_watermark_bytes: self.memory_watermark_bytes,
            memory_tripped: self.memory_tripped.load(Ordering::Relaxed),
            disk_used_pct: self.disk_used_pct.load(Ordering::Relaxed),
            disk_flood_pct: self.disk_flood_pct,
            disk_blocked: self.disk_blocked.load(Ordering::Relaxed),
            max_concurrent_searches: self.max_concurrent_searches,
            search_inflight: self.search_inflight.load(Ordering::Relaxed),
            search_inflight_peak: self.search_inflight_peak.load(Ordering::Relaxed),
        }
    }
}

/// RAII guard for a held global search permit. Decrements the in-flight
/// gauge on drop; the underlying semaphore permit is released with it.
pub struct SearchPermit {
    _permit: OwnedSemaphorePermit,
    inflight: Arc<AtomicU64>,
}

impl Drop for SearchPermit {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A cheap, `Copy`-able snapshot of governor state.
#[derive(Debug, Clone, Copy)]
pub struct GovernorSnapshot {
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
}

// ─────────────────────────────────────────────────────────────────────────
// Initialisation / access
// ─────────────────────────────────────────────────────────────────────────

/// Initialise the process-wide governor from config. Idempotent: the first
/// call wins (subsequent calls return the already-installed governor, so a
/// second `Engine` in-process — e.g. a test — does not re-key the budgets).
pub fn init(config: &Config) -> Arc<ResourceGovernor> {
    Arc::clone(GOVERNOR.get_or_init(|| Arc::new(build(config))))
}

/// The installed governor, if [`init`] has run. Engine-only unit tests that
/// construct an [`crate::index::Index`] directly (never calling
/// `Engine::new`) get `None` and skip all admission checks — behaviour is
/// unchanged for them.
pub fn global() -> Option<Arc<ResourceGovernor>> {
    GOVERNOR.get().map(Arc::clone)
}

fn build(config: &Config) -> ResourceGovernor {
    let limits = &config.limits;

    // ── memtable budget: 0 = auto-derive to 25% RAM, floored at 2 GiB ──
    let memtable_budget_bytes = if limits.max_total_memtable_mb != 0 {
        limits.max_total_memtable_mb.saturating_mul(1024 * 1024)
    } else {
        let sys = system_total_bytes();
        (sys / 4).max(2 * 1024 * 1024 * 1024)
    };

    // ── RSS watermark against the effective memory limit ──
    let memory_limit_bytes = effective_memory_limit_bytes();
    let memory_watermark_bytes = if limits.memory_watermark_percent == 0 {
        0
    } else {
        let pct = limits.memory_watermark_percent.min(100) as u64;
        ((memory_limit_bytes as u128 * pct as u128) / 100) as u64
    };

    let max_query_memory_bytes = limits.max_query_memory_mb.saturating_mul(1024 * 1024);

    let max_concurrent_searches = (limits.max_concurrent_searches.max(1)) as usize;

    tracing::info!(
        memtable_budget_mb = memtable_budget_bytes / (1024 * 1024),
        memory_limit_mb = memory_limit_bytes / (1024 * 1024),
        memory_watermark_mb = memory_watermark_bytes / (1024 * 1024),
        memory_watermark_pct = limits.memory_watermark_percent,
        max_query_memory_mb = limits.max_query_memory_mb,
        max_concurrent_searches,
        disk_flood_pct = limits.disk_flood_stage_percent,
        "resource governor initialised (parent circuit breaker)"
    );

    ResourceGovernor {
        memtable_budget_bytes,
        memtable_used_bytes: AtomicU64::new(0),
        memtable_tripped: AtomicBool::new(false),
        memory_limit_bytes,
        memory_watermark_bytes,
        rss_bytes: AtomicU64::new(0),
        memory_tripped: AtomicBool::new(false),
        max_query_memory_bytes,
        search_pool: Arc::new(Semaphore::new(max_concurrent_searches)),
        max_concurrent_searches,
        search_inflight: Arc::new(AtomicU64::new(0)),
        search_inflight_peak: Arc::new(AtomicU64::new(0)),
        disk_flood_pct: limits.disk_flood_stage_percent.min(100),
        disk_blocked: AtomicBool::new(false),
        disk_used_pct: AtomicU64::new(0),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// System probes (Linux; best-effort with safe fallbacks elsewhere)
// ─────────────────────────────────────────────────────────────────────────

/// Current resident set size of this process, in bytes. Reads
/// `/proc/self/statm` (field 2 = resident pages). Returns 0 if unreadable
/// (the RSS watermark then never trips — safe).
pub fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(res) = s.split_whitespace().nth(1) {
                if let Ok(pages) = res.parse::<u64>() {
                    return pages.saturating_mul(page_size_bytes());
                }
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cfg(target_os = "linux")]
fn page_size_bytes() -> u64 {
    // SAFETY: sysconf is a pure read of a system constant.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 {
        p as u64
    } else {
        4096
    }
}

/// Total system RAM in bytes (from `/proc/meminfo`). Falls back to 8 GiB if
/// unreadable so budgets stay sane on exotic platforms.
fn system_total_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Some(kb) = rest.split_whitespace().next() {
                        if let Ok(kb) = kb.parse::<u64>() {
                            return kb.saturating_mul(1024);
                        }
                    }
                }
            }
        }
    }
    8 * 1024 * 1024 * 1024
}

/// Effective process memory limit: the cgroup memory limit when one is set
/// (container / `systemd-run -p MemoryMax=`), otherwise total system RAM.
/// Takes the min of the two so a generous cgroup value never exceeds RAM.
pub fn effective_memory_limit_bytes() -> u64 {
    let sys = system_total_bytes().max(1);
    match cgroup_memory_limit_bytes() {
        Some(c) if c > 0 && c < sys => c,
        _ => sys,
    }
}

/// Read the cgroup memory limit for THIS process. Handles cgroup v2
/// (`memory.max`, walking up to a parent when a leaf reads `max`) and falls
/// back to cgroup v1 (`memory.limit_in_bytes`). Returns `None` when no
/// finite limit applies.
#[cfg(target_os = "linux")]
fn cgroup_memory_limit_bytes() -> Option<u64> {
    // Sentinels used by the kernel/systemd to mean "unlimited".
    const UNLIMITED_V1: u64 = 9_223_372_036_854_771_712; // PAGE_COUNTER_MAX * page
    let finite = |v: u64| -> Option<u64> {
        if v == 0 || v >= UNLIMITED_V1 {
            None
        } else {
            Some(v)
        }
    };

    // cgroup v2: /proc/self/cgroup has a single "0::<path>" line.
    if let Ok(cg) = std::fs::read_to_string("/proc/self/cgroup") {
        for line in cg.lines() {
            if let Some(path) = line.strip_prefix("0::") {
                // Walk from the leaf cgroup up to the root, honouring the
                // nearest finite memory.max (systemd sets MemoryMax on the
                // scope, which may be a parent of the leaf).
                let mut rel = path.trim().to_string();
                loop {
                    let full = format!("/sys/fs/cgroup{rel}/memory.max");
                    if let Ok(s) = std::fs::read_to_string(&full) {
                        let s = s.trim();
                        if s != "max" {
                            if let Ok(v) = s.parse::<u64>() {
                                if let Some(v) = finite(v) {
                                    return Some(v);
                                }
                            }
                        }
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
            }
        }
    }

    // cgroup v1 fallback.
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes") {
        if let Ok(v) = s.trim().parse::<u64>() {
            return finite(v);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn cgroup_memory_limit_bytes() -> Option<u64> {
    None
}

/// `(total_bytes, avail_bytes)` for the filesystem backing `path`, via
/// `statvfs(2)`. Returns `None` on syscall failure.
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

#[cfg(not(unix))]
pub fn disk_stats(_path: &str) -> Option<(u64, u64)> {
    None
}

/// Used-percentage of the filesystem backing `path` (0..=100). Uses
/// `total - avail` over `total`, matching ES's disk-watermark accounting.
/// Returns 0 when `statvfs` is unavailable (the disk block never trips).
pub fn disk_used_pct(path: &str) -> u64 {
    match disk_stats(path) {
        Some((total, avail)) if total > 0 => {
            let used = total.saturating_sub(avail);
            ((used as u128 * 100) / total as u128) as u64
        }
        _ => 0,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Small formatting helpers
// ─────────────────────────────────────────────────────────────────────────

fn pct_of(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        0
    } else {
        ((part as u128 * 100) / whole as u128) as u64
    }
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

    #[test]
    fn effective_limit_is_positive() {
        assert!(effective_memory_limit_bytes() > 0);
    }

    #[test]
    fn disk_used_pct_bounded() {
        let p = disk_used_pct("/");
        assert!(p <= 100);
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
        assert!(format!("{err}").contains("memtable byte budget exceeded"));
        // Recovery once usage drops back under the budget.
        g.refresh(10 * 1024 * 1024, 0, 0);
        assert!(g.check_ingest_admission().is_ok());
    }

    #[test]
    fn disk_flood_blocks_with_hysteresis() {
        let mut cfg = Config::default();
        cfg.limits.disk_flood_stage_percent = 95;
        let g = build(&cfg);
        assert!(g.check_disk_block("i").is_ok());
        g.refresh(0, 0, 96); // over flood stage
        assert!(g.check_disk_block("i").is_err());
        g.refresh(0, 0, 95); // still within release hysteresis (>= 94)
        assert!(g.check_disk_block("i").is_err());
        g.refresh(0, 0, 90); // clearly recovered
        assert!(g.check_disk_block("i").is_ok());
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
}
