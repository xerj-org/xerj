//! The machine-resource policy — the one place that answers *how many cores*
//! and *how much memory* XERJ may take on the machine it is running on.
//!
//! Before this module the answers were scattered magic numbers: `.min(8)` in
//! the autoindex CLI, `(cores * 8).clamp(64, 512)` in the server, `cores / 8`
//! in three engine pools, a hardcoded `8 GiB` "system memory" in the governor
//! on every non-Linux host, and three `EngineConfig` knobs that nothing read
//! (issue #240). Each was defensible on its own; together they had no story,
//! and on a laptop they added up to a bad citizen.
//!
//! # The policy
//!
//! 1. **Latency-critical work gets the whole machine.** Search fan-out and
//!    bulk parse are what a user is waiting on; holding cores back there only
//!    makes the wait longer.
//! 2. **Background work gets a named reserve, not the whole machine.** Merges
//!    and flush side-cars have nobody waiting on them, so they run on a
//!    fraction of the cores and at a lower thread priority.
//! 3. **Memory is budgeted from what the machine actually has**, with an
//!    explicit slice left for the user — never from an assumed constant.
//! 4. **Every rule is a function here with its reason written down**, so the
//!    next tuning argument is about a number in one place instead of six.
//!
//! # Peer precedent (retrieved, not copied — see the reference-coding mandate)
//!
//! * `quickwit-common/src/cpus.rs:23-63` (Apache-2.0): core count comes from
//!   `QW_NUM_CPUS`, then `KUBERNETES_LIMITS_CPU`, then the OS, then a warned
//!   default — an env override ahead of the OS, and a *loud* fallback.
//! * `quickwit-common/src/runtimes.rs:89-110` (Apache-2.0): headroom scaled by
//!   machine size — all cores at 0..=3, `n - 1` at 4..=6, `n - 2` at 7+.
//!   Reserve as a function of the machine, never a flat cap.
//! * `quickwit-config/src/node_config/mod.rs` `default_merge_concurrency()`
//!   (Apache-2.0): background merges get `num_cpus * 2 / 3`.
//! * `meilisearch/crates/meilisearch/src/option.rs:818-821,885` (MIT core):
//!   indexing pool = `num_cpus / 2`, with the stated reason "the indexer
//!   avoids using more than half of a machine's total processing units. This
//!   ensures Meilisearch is always ready to perform searches, even while you
//!   are updating an index"; `option.rs:1103-1106` budgets indexing memory at
//!   two thirds of (cgroup-aware) total RAM.
//! * `tantivy/src/indexer/index_writer.rs:32-36,285-291` (MIT): the writer is
//!   capped at 8 threads, and a memory budget below the per-thread minimum is
//!   **rejected with an error** rather than silently clamped — the same
//!   discipline this module applies to out-of-range knobs.
//!
//! What XERJ takes from that is the *shape* — full parallelism on the path a
//! user waits on, a declared reserve for the path nobody waits on, budgets
//! derived from the real machine — not the specific constants, which are tied
//! to those engines' own measurements.

use std::sync::OnceLock;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Environment override for the detected core count, honoured ahead of the OS
/// so a container or a CI runner can pin it (quickwit precedent, `cpus.rs:20`).
pub const NUM_CPUS_ENV: &str = "XERJ_NUM_CPUS";

// ─────────────────────────────────────────────────────────────────────────────
// Cores
// ─────────────────────────────────────────────────────────────────────────────

/// What a pool is for. This is the input to every thread-count decision, so a
/// new pool has to say which side of the latency/background line it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// Someone is waiting on it: search fan-out, bulk parse/analyze/insert,
    /// client-side content hashing and sniffing. Gets every core.
    Latency,
    /// Useful work with no client waiting, but which the user's next request
    /// depends on: flush side-cars, segment finalisation. Gets every core, at
    /// a lower thread priority, because holding it back delays visibility.
    Background,
    /// Pure maintenance nobody is waiting on: merges, compaction. Gets a
    /// declared fraction of the machine so it can never take the box over.
    Maintenance,
}

/// Logical cores XERJ may use, in the order the answer is trusted:
/// `XERJ_NUM_CPUS`, the cgroup CPU quota (Linux containers), the OS, then a
/// warned fallback of 4.
///
/// Cached: the answer cannot change usefully within a process, and the cgroup
/// read is a file parse.
pub fn cores() -> usize {
    static CORES: OnceLock<usize> = OnceLock::new();
    *CORES.get_or_init(|| {
        let (n, note) = resolve_cores(
            std::env::var(NUM_CPUS_ENV).ok().as_deref(),
            cgroup_cpu_quota(),
            std::thread::available_parallelism().map(|n| n.get()).ok(),
        );
        if let Some(note) = note {
            warn_once(&note);
        }
        n
    })
}

/// Report a policy problem on both channels.
///
/// The server installs a `tracing` subscriber; `xerj autoindex` does not, so a
/// warning emitted only through `tracing` is invisible to exactly the user most
/// likely to have set `XERJ_NUM_CPUS` wrong. These fire at most once per process
/// and only when something is misconfigured or unreadable, so the duplicate line
/// in a server log is a fair price for never losing the message.
fn warn_once(message: &str) {
    tracing::warn!("{message}");
    eprintln!("xerj: {message}");
}

/// Pure core-count resolution — the tested half of [`cores`].
///
/// Returns the count and an optional warning: an unusable `XERJ_NUM_CPUS` or a
/// machine whose core count cannot be detected must say so, never resolve
/// silently to a number the operator did not ask for (issue #204's class).
fn resolve_cores(
    env: Option<&str>,
    cgroup_quota: Option<usize>,
    os: Option<usize>,
) -> (usize, Option<String>) {
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        return match raw.parse::<usize>() {
            Ok(n) if n >= 1 => (n, None),
            _ => (
                os.unwrap_or(4),
                Some(format!(
                    "{NUM_CPUS_ENV}={raw} is not a positive integer; ignoring it and using the \
                     detected core count instead"
                )),
            ),
        };
    }
    match (os, cgroup_quota) {
        // A cgroup CPU quota is a hard ceiling: the kernel throttles past it,
        // so sizing pools off the host's core count only builds queues.
        (Some(os), Some(q)) => (q.min(os).max(1), None),
        (Some(os), None) => (os, None),
        (None, Some(q)) => (q.max(1), None),
        (None, None) => (
            4,
            Some(
                "could not detect the number of CPUs on this machine; assuming 4 — set \
                 XERJ_NUM_CPUS to the real count"
                    .into(),
            ),
        ),
    }
}

/// Whole cores permitted by this process's cgroup CPU quota, if any (cgroup v2
/// `cpu.max`, then cgroup v1 `cpu.cfs_quota_us`/`cpu.cfs_period_us`). A
/// fractional quota rounds up to 1 — half a core still runs one thread.
#[cfg(target_os = "linux")]
fn cgroup_cpu_quota() -> Option<usize> {
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        let mut parts = s.split_whitespace();
        let quota = parts.next()?;
        let period: u64 = parts.next()?.parse().ok()?;
        if quota != "max" && period > 0 {
            let quota: u64 = quota.parse().ok()?;
            return Some(quota.div_ceil(period).max(1) as usize);
        }
        return None;
    }
    let quota: i64 = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let period: i64 = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (quota > 0 && period > 0).then(|| (quota as u64).div_ceil(period as u64).max(1) as usize)
}

#[cfg(not(target_os = "linux"))]
fn cgroup_cpu_quota() -> Option<usize> {
    None
}

/// Threads for a pool serving `workload` on this machine.
pub fn threads_for(workload: Workload) -> usize {
    threads_for_cores(workload, cores())
}

/// Pure thread-count rule — the tested half of [`threads_for`].
///
/// `Latency` and `Background` get every core: XERJ's measured ingest/read
/// tuning separates them by *thread priority*, not by width (see the pool
/// docs in `xerj-engine/src/lib.rs`), and narrowing them would trade
/// throughput for nothing. `Maintenance` gets `max(2, cores / 8)` — the width
/// measured on the 1M×c8 ingest benchmark, where an all-core merge pool stalled
/// ingest for 17.5 s per merge pass.
pub const fn threads_for_cores(workload: Workload, cores: usize) -> usize {
    let cores = if cores == 0 { 1 } else { cores };
    match workload {
        Workload::Latency | Workload::Background => cores,
        Workload::Maintenance => {
            let eighth = cores / 8;
            if eighth < 2 {
                if cores < 2 {
                    1
                } else {
                    2
                }
            } else {
                eighth
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-index concurrent-map sharding
// ─────────────────────────────────────────────────────────────────────────────

/// Ceiling on the shard count of a **per-index** concurrent map (#873).
///
/// `dashmap`'s own default is `(available_parallelism * 4).next_power_of_two()`,
/// which is the right answer for a map there is ONE of per process and the
/// wrong answer for a map there is one of per index: the shard array is
/// `Box<[CachePadded<RwLock<HashMap>>]>`, 128 bytes per shard on x86_64,
/// written at construction and therefore resident forever. Every open index
/// carries ~23 of these maps, so on a 32-core host the default cost 128 × 128 B
/// × 23 ≈ 368 KiB per index before a single document existed — measured as the
/// largest single term in the idle-RSS floor of #873.
///
/// 16 is a ceiling, not a target: a machine whose dashmap default is already
/// smaller keeps that smaller value. Striping wider than this buys nothing here
/// because the contention these maps see is bounded by the concurrency on ONE
/// index, and it is read-dominated — hydration inserts are rare, and readers
/// share the shard lock.
pub const MAX_PER_INDEX_MAP_SHARDS: usize = 16;

/// Shard count for a concurrent map that exists once per index.
///
/// Never larger than the `dashmap` default this replaced, so no machine ends up
/// with *more* striping than before; capped at [`MAX_PER_INDEX_MAP_SHARDS`] so
/// a 64-core host does not pay 256-way striping on every cache of every open
/// index. Must stay a power of two — `DashMap::with_shard_amount` panics
/// otherwise.
pub fn per_index_map_shards() -> usize {
    static SHARDS: OnceLock<usize> = OnceLock::new();
    *SHARDS.get_or_init(|| per_index_map_shards_for(cores()))
}

/// Pure rule behind [`per_index_map_shards`] — the tested half.
pub const fn per_index_map_shards_for(cores: usize) -> usize {
    let cores = if cores == 0 { 1 } else { cores };
    // `dashmap::default_shard_amount()`, then capped.
    let dashmap_default = (cores * 4).next_power_of_two();
    if dashmap_default < MAX_PER_INDEX_MAP_SHARDS {
        dashmap_default
    } else {
        MAX_PER_INDEX_MAP_SHARDS
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Thread priority
// ─────────────────────────────────────────────────────────────────────────────

/// Where a pool sits on the maintenance ladder, i.e. how far below foreground
/// work its threads should be scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deprioritize {
    /// Ingest parse/analyze/insert — one step below reads.
    Ingest,
    /// Flush side-cars and finalisation — below ingest.
    Background,
    /// Merges — the bottom of the ladder.
    Maintenance,
}

/// The platform-specific action that implements a rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityAction {
    /// Leave the thread at normal priority.
    None,
    /// `nice(n)` on the calling **thread** — Linux semantics, where the nice
    /// value is a per-task attribute.
    NiceThread(i32),
    /// `setpriority(PRIO_DARWIN_THREAD, 0, PRIO_DARWIN_BG)` — Darwin's
    /// thread-scoped background tier.
    DarwinThreadBackground,
}

/// The platforms whose scheduling semantics differ enough to matter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Other,
}

/// This build's platform.
pub const HOST_PLATFORM: Platform = if cfg!(target_os = "linux") {
    Platform::Linux
} else if cfg!(target_os = "macos") {
    Platform::MacOs
} else {
    Platform::Other
};

/// The ladder, as a decision table — the tested half of
/// [`deprioritize_current_thread`].
///
/// **Why this is not just `nice()` everywhere.** POSIX `nice()` adjusts the
/// *process*; Linux makes it a per-task (per-thread) attribute, which is what
/// the measured ingest/background/merge ladder relies on. Darwin keeps the
/// POSIX meaning, so calling `nice(5)`, `nice(10)`, `nice(15)` from pool
/// `start_handler`s there does not separate the three pools at all — it
/// ratchets the *whole server process*, search threads included, toward the
/// nice ceiling as pool threads start. That is the opposite of the intent, on
/// the platform whose users reported the slowness (#240).
///
/// Darwin's replacement is thread-scoped: `PRIO_DARWIN_THREAD` with
/// `PRIO_DARWIN_BG` moves *this thread* into the background tier. It is an
/// on/off tier, not a ladder rung, and it throttles I/O as well as CPU — right
/// for merges, wrong for ingest and flush side-cars (a user's next search waits
/// on those). So on macOS only the bottom rung is applied, and the two upper
/// rungs stay at normal priority rather than being faked with a call that would
/// deprioritize the whole process.
///
/// This decision table is reasoned from documented POSIX/Darwin semantics and
/// the libc surface; it has **not** been measured on Apple hardware.
pub const fn priority_action(platform: Platform, class: Deprioritize) -> PriorityAction {
    match platform {
        Platform::Linux => match class {
            Deprioritize::Ingest => PriorityAction::NiceThread(5),
            Deprioritize::Background => PriorityAction::NiceThread(10),
            Deprioritize::Maintenance => PriorityAction::NiceThread(15),
        },
        Platform::MacOs => match class {
            Deprioritize::Ingest | Deprioritize::Background => PriorityAction::None,
            Deprioritize::Maintenance => PriorityAction::DarwinThreadBackground,
        },
        // Windows and the BSDs: no per-thread nice with the semantics above.
        Platform::Other => PriorityAction::None,
    }
}

/// Apply the ladder to the calling thread. Call it from a pool's
/// `start_handler`, once per pool thread.
pub fn deprioritize_current_thread(class: Deprioritize) {
    match priority_action(HOST_PLATFORM, class) {
        PriorityAction::None => {}
        PriorityAction::NiceThread(_n) => {
            #[cfg(all(unix, not(target_os = "macos")))]
            // SAFETY: `nice` takes an int and touches only the caller's
            // scheduling attributes; on Linux that is this thread.
            unsafe {
                let _ = libc::nice(_n);
            }
        }
        PriorityAction::DarwinThreadBackground => {
            #[cfg(target_os = "macos")]
            // SAFETY: `setpriority` with PRIO_DARWIN_THREAD and `who = 0`
            // addresses the calling thread only.
            unsafe {
                let _ = libc::setpriority(libc::PRIO_DARWIN_THREAD, 0, libc::PRIO_DARWIN_BG);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Memory
// ─────────────────────────────────────────────────────────────────────────────

/// Total physical RAM, or `None` when this platform has no probe here.
///
/// `None` means *unknown*, and callers must treat it as unknown — the bug this
/// replaces was a non-Linux fallback that returned a fictional 8 GiB, so every
/// derived budget on a 64 GB Mac was sized for a machine eight times smaller
/// (#240 §7).
pub fn total_memory_bytes() -> Option<u64> {
    static TOTAL: OnceLock<Option<u64>> = OnceLock::new();
    *TOTAL.get_or_init(|| {
        let total = probe_total_memory();
        if total.is_none() {
            warn_once(&format!(
                "cannot read this machine's total RAM on {}; memory budgets fall back to \
                 conservative defaults — set them explicitly (limits.max_total_memtable_mb, \
                 autoindex --workers/--bulk-mb) if that is wrong",
                std::env::consts::OS
            ));
        }
        total
    })
}

/// RAM available for a new allocation without swapping — Linux `MemAvailable`.
/// `None` where unknown (including macOS: Darwin exposes free/inactive/
/// compressed page counts through `host_statistics64`, which is not in the
/// pinned `libc` surface, and guessing from `hw.memsize` alone would be a
/// fabricated number).
pub fn available_memory_bytes() -> Option<u64> {
    probe_available_memory()
}

/// Resident set size of this process, or `None` where this platform has no
/// probe. `None` must disable an RSS-driven decision, never read as zero.
pub fn current_rss_bytes() -> Option<u64> {
    probe_rss()
}

#[cfg(target_os = "linux")]
fn probe_total_memory() -> Option<u64> {
    meminfo_field(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemTotal:")
}

#[cfg(target_os = "macos")]
fn probe_total_memory() -> Option<u64> {
    // `hw.memsize` is the machine's physical RAM in bytes.
    sysctl_u64(c"hw.memsize")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_total_memory() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn probe_available_memory() -> Option<u64> {
    meminfo_field(
        &std::fs::read_to_string("/proc/meminfo").ok()?,
        "MemAvailable:",
    )
}

#[cfg(not(target_os = "linux"))]
fn probe_available_memory() -> Option<u64> {
    None
}

/// Parse one `kB` field out of `/proc/meminfo`, in bytes.
#[cfg(any(target_os = "linux", test))]
fn meminfo_field(meminfo: &str, key: &str) -> Option<u64> {
    for line in meminfo.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(key) {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn probe_rss() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: `sysconf` reads a system constant.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = if page > 0 { page as u64 } else { 4096 };
    Some(pages.saturating_mul(page))
}

#[cfg(target_os = "macos")]
fn probe_rss() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `proc_pidinfo` writes at most `size` bytes into `info` and
    // reports how many it wrote; anything short is treated as a failure.
    let written = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: proc_pidinfo filled the whole struct (checked above).
    Some(unsafe { info.assume_init() }.pti_resident_size)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_rss() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `name` is NUL-terminated, and the out-buffer length is the size
    // of the buffer being written.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<u64>() && value > 0).then_some(value)
}

/// How many bytes a **client-side** XERJ tool (autoindex) may allocate on a
/// machine whose owner is also using it.
///
/// The server's own admission budgets are cgroup-aware and live in
/// `xerj-engine::governor`; this is the laptop-citizen rule for the client:
///
/// ```text
/// reserve = max(1 GiB, total / 8)                 // left for the user
/// safe    = min(available - reserve, total / 2)   // never more than half
/// ```
///
/// Half of RAM as the ceiling follows the same reasoning meilisearch gives for
/// its two-thirds indexing budget (`option.rs:1103-1106`) — a machine that is
/// also serving its owner should not have its page cache evicted by a
/// background indexer.
///
/// `None` means **this machine's RAM is not knowable here** (Windows, FreeBSD,
/// …: [`total_memory_bytes`] has no probe), and callers must read it as "no
/// memory-derived limit", never as "a small limit". An earlier revision of this
/// function returned a 1 GiB fallback instead, and the result was that a
/// Windows box with 64 GB of RAM was budgeted as a 1 GiB machine — a phantom
/// budget that no code on that platform enforces. meilisearch keeps the same
/// shape for the same reason: `total_memory_bytes() -> Option<u64>` is `None`
/// on an unsupported system (`crates/meilisearch/src/option.rs:1135`) and the
/// derived per-thread budget stays an `Option` all the way down, where `None`
/// means unbounded rather than minimal
/// (`crates/milli/src/update/index_documents/helpers/grenad_helpers.rs:122`).
pub fn memory_safe_zone_bytes() -> Option<u64> {
    total_memory_bytes().map(|total| safe_zone(total, available_memory_bytes()))
}

/// Pure safe-zone rule — the tested half of [`memory_safe_zone_bytes`].
fn safe_zone(total: u64, available: Option<u64>) -> u64 {
    let reserve = (total / 8).max(GIB);
    let headroom = available.unwrap_or(total).saturating_sub(reserve);
    headroom.min(total / 2).max(256 * MIB)
}

/// One line describing what this policy decided, for a startup log or a
/// `--json` run summary. Sizes are in MiB.
pub fn describe() -> String {
    let mib = |b: Option<u64>| match b {
        Some(b) => format!("{}", b / MIB),
        None => "unknown".to_string(),
    };
    format!(
        "cores={} ram_total_mib={} ram_available_mib={} safe_zone_mib={}",
        cores(),
        mib(total_memory_bytes()),
        mib(available_memory_bytes()),
        mib(memory_safe_zone_bytes()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_beats_the_os_and_a_bad_value_is_reported() {
        assert_eq!(resolve_cores(Some("6"), None, Some(64)), (6, None));
        assert_eq!(resolve_cores(Some(" 6 "), None, Some(64)), (6, None));
        // Accepted-and-ignored is the class this repo keeps re-finding (#204):
        // an unusable override must be reported, not silently discarded.
        let (n, note) = resolve_cores(Some("many"), None, Some(64));
        assert_eq!(n, 64);
        assert!(note.unwrap().contains("XERJ_NUM_CPUS=many"));
        let (n, note) = resolve_cores(Some("0"), None, Some(64));
        assert_eq!(n, 64);
        assert!(note.is_some());
        // Empty means "unset", not "invalid".
        assert_eq!(resolve_cores(Some(""), None, Some(8)), (8, None));
    }

    #[test]
    fn a_cgroup_quota_caps_the_host_core_count() {
        assert_eq!(resolve_cores(None, Some(2), Some(64)), (2, None));
        // A quota larger than the machine is not a licence to oversubscribe.
        assert_eq!(resolve_cores(None, Some(128), Some(8)), (8, None));
        assert_eq!(resolve_cores(None, None, Some(8)), (8, None));
        let (n, note) = resolve_cores(None, None, None);
        assert_eq!(n, 4);
        assert!(note.is_some(), "an undetectable machine must say so");
    }

    /// #873 - a per-index concurrent map must not stripe with the machine.
    ///
    /// The rule is asserted at explicit core counts rather than at the host's,
    /// because a 2-core CI runner would otherwise agree with the unfixed
    /// behaviour and report a green that means nothing.
    #[test]
    fn a_per_index_map_is_capped_and_never_wider_than_the_dashmap_default() {
        // The value this replaced, restated so the comparison is explicit.
        let dashmap_default = |cores: usize| (cores * 4).next_power_of_two();
        for cores in [1usize, 2, 3, 4, 8, 12, 16, 32, 64, 128] {
            let n = per_index_map_shards_for(cores);
            assert!(n.is_power_of_two(), "cores={cores} n={n} must be pow2");
            assert!(n >= 1, "cores={cores}");
            assert!(
                n <= MAX_PER_INDEX_MAP_SHARDS,
                "cores={cores} n={n} exceeds the cap"
            );
            assert!(
                n <= dashmap_default(cores),
                "cores={cores}: must never stripe WIDER than dashmap would have"
            );
        }
        // Below the cap the machine still decides; above it, it does not.
        assert_eq!(per_index_map_shards_for(1), 4);
        assert_eq!(per_index_map_shards_for(2), 8);
        assert_eq!(per_index_map_shards_for(4), 16);
        assert_eq!(per_index_map_shards_for(8), 16);
        assert_eq!(per_index_map_shards_for(64), 16);
        // A machine that reports zero cores must not produce a zero shard
        // count: `DashMap::with_shard_amount(0)` panics.
        assert_eq!(per_index_map_shards_for(0), 4);
        // The cached host answer obeys the same rule.
        assert_eq!(per_index_map_shards(), per_index_map_shards_for(cores()));
    }

    #[test]
    fn latency_work_gets_every_core_and_maintenance_gets_a_reserve() {
        for cores in [1usize, 2, 4, 8, 12, 16, 32, 64] {
            assert_eq!(threads_for_cores(Workload::Latency, cores), cores);
            assert_eq!(threads_for_cores(Workload::Background, cores), cores);
            let maint = threads_for_cores(Workload::Maintenance, cores);
            assert!(maint >= 1 && maint <= cores, "cores={cores} maint={maint}");
            assert!(
                maint <= (cores / 2).max(2),
                "maintenance must never take the machine over: cores={cores} maint={maint}"
            );
        }
        // The measured widths this replaces, preserved exactly.
        assert_eq!(threads_for_cores(Workload::Maintenance, 32), 4);
        assert_eq!(threads_for_cores(Workload::Maintenance, 8), 2);
        assert_eq!(threads_for_cores(Workload::Maintenance, 1), 1);
        assert_eq!(threads_for_cores(Workload::Latency, 0), 1);
    }

    #[test]
    fn macos_never_uses_process_wide_nice_for_the_ladder() {
        // The #240 §5 defect: `nice()` is per-process on Darwin, so using it
        // from pool start handlers deprioritizes search along with merges.
        for class in [
            Deprioritize::Ingest,
            Deprioritize::Background,
            Deprioritize::Maintenance,
        ] {
            assert!(
                !matches!(
                    priority_action(Platform::MacOs, class),
                    PriorityAction::NiceThread(_)
                ),
                "{class:?} must not use nice() on macOS"
            );
        }
        assert_eq!(
            priority_action(Platform::MacOs, Deprioritize::Maintenance),
            PriorityAction::DarwinThreadBackground
        );
        // Ingest and flush side-cars stay at normal priority there: Darwin's
        // background tier throttles I/O, and a user's next search waits on them.
        assert_eq!(
            priority_action(Platform::MacOs, Deprioritize::Ingest),
            PriorityAction::None
        );
        assert_eq!(
            priority_action(Platform::MacOs, Deprioritize::Background),
            PriorityAction::None
        );
        // Linux keeps the measured ladder.
        assert_eq!(
            priority_action(Platform::Linux, Deprioritize::Ingest),
            PriorityAction::NiceThread(5)
        );
        assert_eq!(
            priority_action(Platform::Linux, Deprioritize::Background),
            PriorityAction::NiceThread(10)
        );
        assert_eq!(
            priority_action(Platform::Linux, Deprioritize::Maintenance),
            PriorityAction::NiceThread(15)
        );
    }

    #[test]
    fn meminfo_parsing_reads_total_and_available() {
        let sample =
            "MemTotal:       16316908 kB\nMemFree:         1234 kB\nMemAvailable:    8000000 kB\n";
        assert_eq!(meminfo_field(sample, "MemTotal:"), Some(16_316_908 * 1024));
        assert_eq!(
            meminfo_field(sample, "MemAvailable:"),
            Some(8_000_000 * 1024)
        );
        assert_eq!(meminfo_field(sample, "Bogus:"), None);
        assert_eq!(meminfo_field("MemTotal:  garbage\n", "MemTotal:"), None);
    }

    #[test]
    fn the_safe_zone_leaves_the_user_a_reserve_and_never_exceeds_half() {
        // 16 GiB laptop with 10 GiB available: reserve 2 GiB, ceiling 8 GiB.
        assert_eq!(safe_zone(16 * GIB, Some(10 * GIB)), 8 * GIB);
        // Same laptop under pressure: only what is actually free, minus reserve.
        assert_eq!(safe_zone(16 * GIB, Some(4 * GIB)), 2 * GIB);
        // Availability unknown (macOS): fall back to the half-of-RAM ceiling.
        assert_eq!(safe_zone(16 * GIB, None), 8 * GIB);
        // Small container: the 1 GiB floor of the reserve still applies.
        assert_eq!(safe_zone(2 * GIB, Some(2 * GIB)), GIB);
        // Never zero, however tight the machine.
        assert_eq!(safe_zone(512 * MIB, Some(0)), 256 * MIB);
        for total in [GIB, 4 * GIB, 16 * GIB, 128 * GIB] {
            assert!(safe_zone(total, None) <= total / 2 || total < 2 * GIB);
        }
    }

    #[test]
    fn this_machine_probes_are_sane() {
        // Not a fixed expectation — a probe that returns something absurd is
        // the bug (the 8 GiB constant this replaces would fail on this box).
        let cores = cores();
        assert!((1..=4096).contains(&cores));
        if let Some(total) = total_memory_bytes() {
            assert!(total >= 256 * MIB, "implausible total RAM: {total}");
            assert!(memory_safe_zone_bytes().expect("probeable RAM has a safe zone") <= total);
        } else {
            // Unknown must stay unknown: a fabricated small budget here is the
            // Windows PDF-throttling bug this API shape exists to prevent.
            assert_eq!(memory_safe_zone_bytes(), None);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let rss = current_rss_bytes().expect("RSS must be probeable here");
            assert!(rss > 0, "a running process has non-zero RSS");
            assert_eq!(
                available_memory_bytes().is_some(),
                cfg!(target_os = "linux")
            );
        }
    }
}
