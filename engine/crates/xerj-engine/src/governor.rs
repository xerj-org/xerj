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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use xerj_common::config::Config;
use xerj_common::XerjError;

use crate::segment_cache_budget::{SegmentHydrationBudget, SegmentHydrationBudgetSnapshot};

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
                    self.disk_flood_pct.load(Ordering::Relaxed),
                ),
            ));
        }
        Ok(())
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
        let flood_pct = self.disk_flood_pct.load(Ordering::Relaxed);
        if flood_pct != 0 {
            self.disk_used_pct.store(disk_used_pct, Ordering::Relaxed);
            let cur = self.disk_blocked.load(Ordering::Relaxed);
            let release_pct = (flood_pct as u64).saturating_sub(1);
            let next = if cur {
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
            memory_watermark_bytes: self.memory_watermark_bytes,
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

/// The AUTO process-memory cap, in MiB, chosen from the effective memory the
/// process can actually see, in binary-GiB steps:
///
///   effective < 64 GiB             → 8 GiB
///   64 GiB ≤ effective < 128 GiB   → 16 GiB
///   effective ≥ 128 GiB            → 32 GiB
///
/// Tier off the effective (cgroup-aware) limit — see
/// `effective_memory_limit_bytes` — not raw host RAM, so a container smaller
/// than its host is sized for the container. This only *chooses* a ceiling;
/// `cap_memory_limit` still mins it with the machine, so the cap only ever
/// LOWERS a budget: on a 40 GiB box the 8 GiB tier yields min(40, 8) = 8, and
/// on a 6 GiB box it yields min(6, 8) = 6. It never invents headroom.
fn tiered_cap_mb(effective_bytes: u64) -> u64 {
    const GIB: u64 = 1024 * 1024 * 1024;
    if effective_bytes < 64 * GIB {
        8 * 1024
    } else if effective_bytes < 128 * GIB {
        16 * 1024
    } else {
        32 * 1024
    }
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

    // ── RSS watermark against the effective memory limit ──
    //
    // Capped by `limits.max_process_memory_mb`, resolved through
    // `resolve_process_memory_cap`: the AUTO default tiers the cap off the
    // machine (8/16/32 GiB), `0` means uncapped (whole machine), an explicit
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
    let memory_limit_bytes = cap_memory_limit(effective_memory_bytes, resolved_process_cap.cap_mb);

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
    if resolved_process_cap.cap_mb == 0 {
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

    tracing::info!(
        memtable_budget_mb = memtable_budget_bytes / (1024 * 1024),
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
        memory_watermark_bytes,
        rss_bytes: AtomicU64::new(0),
        memory_tripped: AtomicBool::new(false),
        max_query_memory_bytes,
        search_pool: Arc::new(Semaphore::new(max_concurrent_searches)),
        max_concurrent_searches,
        search_inflight: Arc::new(AtomicU64::new(0)),
        search_inflight_peak: Arc::new(AtomicU64::new(0)),
        bulk_pool: Arc::new(Semaphore::new(max_concurrent_bulks)),
        max_concurrent_bulks,
        disk_flood_pct_default: limits.disk_flood_stage_percent.min(100),
        disk_flood_pct: AtomicU8::new(limits.disk_flood_stage_percent.min(100)),
        disk_blocked: AtomicBool::new(false),
        disk_used_pct: AtomicU64::new(0),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// System probes (Linux; best-effort with safe fallbacks elsewhere)
// ─────────────────────────────────────────────────────────────────────────

/// Current resident set size of this process, in bytes, via
/// [`xerj_common::resource::current_rss_bytes`] (`/proc/self/statm` on Linux,
/// `proc_pidinfo(PROC_PIDTASKINFO)` on macOS). Returns 0 where the platform
/// exposes no probe — the RSS watermark then never trips, which is safe but
/// means no admission control, so [`build`] says so out loud at startup.
///
/// Until #240 this returned a hardcoded 0 on *every* non-Linux platform, so
/// the watermark was dead on macOS however much memory the process took.
pub fn current_rss_bytes() -> u64 {
    xerj_common::resource::current_rss_bytes().unwrap_or(0)
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

        // 6 GiB box → 8 GiB tier, but the machine only has 6, so min = 6.
        let tiny = resolve_process_memory_cap(6 * GIB, AUTO, None);
        assert_eq!(tiny.cap_mb, 8 * 1024, "the tier is chosen before the min");
        assert_eq!(
            cap_memory_limit(6 * GIB, tiny.cap_mb),
            6 * GIB,
            "cap only lowers"
        );
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
