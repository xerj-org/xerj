//! Opt-in ingest memory attribution.
//!
//! This module is deliberately independent of admission and scheduling. When
//! `XERJ_INGEST_MEMORY_TRACE` is absent or `off`, transition helpers return
//! before touching atomics, allocating, walking documents, probing `/proc`, or
//! constructing events.
//!
//! `summary` is the only enabled mode in v1: it emits bounded periodic
//! start/snapshot/stop NDJSON. Lifecycle `EventKind` variants and merge/cache
//! categories are reserved schema vocabulary; until their owners are wired,
//! their measurements are `unavailable` and their gauges must not be treated
//! as measured zero.
//!
//! `MemtableActive` is sampled from the engine while `FlushDrained` is owned at
//! the authoritative drain boundary. Those authorities cannot be sampled
//! atomically, so a transient snapshot may count the same estimate in both.
//! Logical totals are attribution diagnostics, never allocator identities.

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

pub const SCHEMA: &str = "xerj.ingest_memory.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceMode {
    Off,
    Summary,
}

#[derive(Clone, Copy, Debug)]
pub struct TraceConfig {
    pub mode: TraceMode,
    pub sample_ms: u64,
}

impl TraceConfig {
    fn from_env() -> Self {
        let mode = std::env::var("XERJ_INGEST_MEMORY_TRACE").unwrap_or_default();
        let sample_ms = std::env::var("XERJ_INGEST_MEMORY_SAMPLE_MS").unwrap_or_default();
        Self::parse(&mode, &sample_ms)
    }

    fn parse(mode: &str, sample_ms: &str) -> Self {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "summary" | "1" | "true" | "on" => TraceMode::Summary,
            _ => TraceMode::Off,
        };
        let sample_ms = sample_ms
            .parse::<u64>()
            .ok()
            .unwrap_or(100)
            .clamp(25, 60_000);
        Self { mode, sample_ms }
    }
}

static CONFIG: LazyLock<TraceConfig> = LazyLock::new(TraceConfig::from_env);

#[cfg(test)]
tokio::task_local! {
    static TEST_LEDGER: &'static Ledger;
}

#[cfg(test)]
fn test_ledger() -> Option<&'static Ledger> {
    TEST_LEDGER.try_with(|ledger| *ledger).ok()
}

#[cfg(not(test))]
fn test_ledger() -> Option<&'static Ledger> {
    None
}

#[inline]
pub fn config() -> TraceConfig {
    *CONFIG
}

#[inline]
pub fn enabled() -> bool {
    active_ledger().is_some()
}

#[inline]
pub(crate) fn active_ledger() -> Option<&'static Ledger> {
    test_ledger().or_else(|| (CONFIG.mode != TraceMode::Off).then_some(&LEDGER))
}

#[cfg(test)]
pub(crate) async fn with_test_ledger<F: std::future::Future>(
    ledger: &'static Ledger,
    future: F,
) -> F::Output {
    TEST_LEDGER.scope(ledger, future).await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Category {
    HttpBody = 0,
    HttpRewriteBuffer,
    RawSource,
    ParsedSource,
    ParsedVector,
    PreparedDocument,
    PreparedVector,
    MemtableActive,
    FlushDrained,
    MergeDecoded,
    MergeSurvivor,
    MergeJsonBuffer,
    MergeParsed,
    MergeEncoded,
    CacheStoredSlices,
    CacheDocValues,
    CacheStoredValues,
    CacheSortShadow,
    CacheIdPositions,
    CacheRowSequences,
    CacheDecodedStored,
}

impl Category {
    pub const ALL: [Self; 21] = [
        Self::HttpBody,
        Self::HttpRewriteBuffer,
        Self::RawSource,
        Self::ParsedSource,
        Self::ParsedVector,
        Self::PreparedDocument,
        Self::PreparedVector,
        Self::MemtableActive,
        Self::FlushDrained,
        Self::MergeDecoded,
        Self::MergeSurvivor,
        Self::MergeJsonBuffer,
        Self::MergeParsed,
        Self::MergeEncoded,
        Self::CacheStoredSlices,
        Self::CacheDocValues,
        Self::CacheStoredValues,
        Self::CacheSortShadow,
        Self::CacheIdPositions,
        Self::CacheRowSequences,
        Self::CacheDecodedStored,
    ];

    const COUNT: usize = Self::ALL.len();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    ExactCapacity,
    SerializedSize,
    Estimated,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    TraceStart,
    TraceStop,
    TraceSnapshot,
    HttpBulkAccept,
    HttpBulkRelease,
    BulkGroupRetain,
    BulkGroupRelease,
    SemanticParsedRetain,
    SemanticParsedRelease,
    SemanticPreparedRetain,
    SemanticPreparedRelease,
    MemtableInsert,
    MemtableDrain,
    FlushDrainedRetain,
    FlushFinalizeWaitEnter,
    FlushFinalizeStart,
    FlushFinalizeEnd,
    FlushDrainedRelease,
    MergeRetain,
    MergePhase,
    MergeRelease,
    CacheInsert,
    CacheReplace,
    CacheEvict,
    CacheClear,
    TraceError,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Scope {
    pub index_hash: Option<u32>,
    pub request: Option<u64>,
    pub flush: Option<u64>,
    pub merge: Option<u64>,
    pub shard: Option<u32>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Gauge {
    pub category: Category,
    pub current: u64,
    pub peak: u64,
    pub measurement: Measurement,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct AllocatorSnapshot {
    pub allocated: Option<u64>,
    pub active: Option<u64>,
    pub resident: Option<u64>,
    pub epoch_ok: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ProcessSnapshot {
    pub rss: Option<u64>,
    pub rss_ok: bool,
    pub cpu_time_ns: Option<u64>,
    pub cpu_time_ok: bool,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub schema: &'static str,
    pub seq: u64,
    pub ts_unix_ns: u128,
    pub event: EventKind,
    pub scope: Scope,
    pub logical: Vec<Gauge>,
    /// Sum of all logical category currents in this coherent snapshot.
    /// This is diagnostic attribution, not an allocator/RSS identity.
    pub logical_total_current: u64,
    /// Retain/release underflows observed since process start.
    pub accounting_errors: u64,
    /// Trace events rejected by the bounded output sink since trace start.
    pub dropped_events: u64,
    pub allocator: AllocatorSnapshot,
    pub process: ProcessSnapshot,
    pub flags: EventFlags,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct EventFlags {
    pub sampled: bool,
    pub logical_exact: bool,
    pub allocator_supported: bool,
}

struct Cell {
    current: AtomicU64,
    peak: AtomicU64,
}

impl Cell {
    const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }
}

pub struct Ledger {
    transition: Mutex<()>,
    cells: [Cell; Category::COUNT],
    seq: AtomicU64,
    accounting_errors: AtomicU64,
    physical_allocated: AtomicU64,
    physical_active: AtomicU64,
    physical_resident: AtomicU64,
    physical_rss: AtomicU64,
    physical_cpu_time_ns: AtomicU64,
    physical_flags: AtomicU64,
    dropped_events: AtomicU64,
}

impl Ledger {
    pub const fn new() -> Self {
        Self {
            transition: Mutex::new(()),
            cells: [const { Cell::new() }; Category::COUNT],
            seq: AtomicU64::new(0),
            accounting_errors: AtomicU64::new(0),
            physical_allocated: AtomicU64::new(0),
            physical_active: AtomicU64::new(0),
            physical_resident: AtomicU64::new(0),
            physical_rss: AtomicU64::new(0),
            physical_cpu_time_ns: AtomicU64::new(0),
            physical_flags: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
        }
    }

    fn retain(&self, category: Category, bytes: u64) {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        self.retain_locked(category, bytes);
    }

    fn retain_locked(&self, category: Category, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let cell = &self.cells[category as usize];
        let next = cell.current.fetch_add(bytes, Ordering::Relaxed) + bytes;
        let mut peak = cell.peak.load(Ordering::Relaxed);
        while next > peak {
            match cell
                .peak
                .compare_exchange_weak(peak, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(observed) => peak = observed,
            }
        }
    }

    fn release(&self, category: Category, bytes: u64) {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        self.release_locked(category, bytes);
    }

    fn release_locked(&self, category: Category, bytes: u64) -> bool {
        if bytes == 0 {
            return true;
        }
        let cell = &self.cells[category as usize];
        let mut current = cell.current.load(Ordering::Relaxed);
        loop {
            if bytes > current {
                self.accounting_errors.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match cell.current.compare_exchange_weak(
                current,
                current - bytes,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn transfer(&self, from: Category, to: Category, bytes: u64) {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        if self.release_locked(from, bytes) {
            self.retain_locked(to, bytes);
        }
    }

    pub fn gauge(&self, category: Category, measurement: Measurement) -> Gauge {
        let cell = &self.cells[category as usize];
        Gauge {
            category,
            current: cell.current.load(Ordering::Relaxed),
            peak: cell.peak.load(Ordering::Relaxed),
            measurement,
        }
    }

    pub fn accounting_errors(&self) -> u64 {
        self.accounting_errors.load(Ordering::Relaxed)
    }

    pub fn record_dropped_event(&self) {
        self.dropped_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe(&self, category: Category, bytes: usize) {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        let cell = &self.cells[category as usize];
        let bytes = bytes as u64;
        cell.current.store(bytes, Ordering::Relaxed);
        cell.peak.fetch_max(bytes, Ordering::Relaxed);
    }

    pub fn update_physical(&self, allocator: AllocatorSnapshot, process: ProcessSnapshot) {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        const ALLOCATOR_OK: u64 = 1;
        const RSS_OK: u64 = 2;
        const CPU_TIME_OK: u64 = 4;
        let mut flags = 0;
        if allocator.epoch_ok {
            flags |= ALLOCATOR_OK;
        }
        if process.rss_ok {
            flags |= RSS_OK;
        }
        if process.cpu_time_ok {
            flags |= CPU_TIME_OK;
        }
        self.physical_allocated
            .store(allocator.allocated.unwrap_or(0), Ordering::Relaxed);
        self.physical_active
            .store(allocator.active.unwrap_or(0), Ordering::Relaxed);
        self.physical_resident
            .store(allocator.resident.unwrap_or(0), Ordering::Relaxed);
        self.physical_rss
            .store(process.rss.unwrap_or(0), Ordering::Relaxed);
        self.physical_cpu_time_ns
            .store(process.cpu_time_ns.unwrap_or(0), Ordering::Relaxed);
        self.physical_flags.store(flags, Ordering::Release);
    }

    fn event(&self, kind: EventKind, scope: Scope, sampled: bool) -> Event {
        let _transition = self.transition.lock().unwrap_or_else(|e| e.into_inner());
        let flags = self.physical_flags.load(Ordering::Acquire);
        let allocator_ok = flags & 1 != 0;
        let rss_ok = flags & 2 != 0;
        let cpu_time_ok = flags & 4 != 0;
        let logical: Vec<_> = Category::ALL
            .into_iter()
            .map(|category| self.gauge(category, measurement(category)))
            .collect();
        let logical_total_current = logical.iter().map(|gauge| gauge.current).sum();
        Event {
            schema: SCHEMA,
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            ts_unix_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            event: kind,
            scope,
            logical,
            logical_total_current,
            accounting_errors: self.accounting_errors.load(Ordering::Relaxed),
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
            allocator: AllocatorSnapshot {
                allocated: allocator_ok.then(|| self.physical_allocated.load(Ordering::Relaxed)),
                active: allocator_ok.then(|| self.physical_active.load(Ordering::Relaxed)),
                resident: allocator_ok.then(|| self.physical_resident.load(Ordering::Relaxed)),
                epoch_ok: allocator_ok,
            },
            process: ProcessSnapshot {
                rss: rss_ok.then(|| self.physical_rss.load(Ordering::Relaxed)),
                rss_ok,
                cpu_time_ns: cpu_time_ok.then(|| self.physical_cpu_time_ns.load(Ordering::Relaxed)),
                cpu_time_ok,
            },
            flags: EventFlags {
                sampled,
                logical_exact: false,
                allocator_supported: allocator_ok,
            },
        }
    }
}

pub static LEDGER: Ledger = Ledger::new();

/// Build one sampler-owned event. This is the only public event constructor:
/// ingest hot paths cannot accidentally allocate events in `off` or `summary`.
pub fn snapshot_event(kind: EventKind, sampled: bool) -> Option<Event> {
    if !enabled()
        || !matches!(
            kind,
            EventKind::TraceStart | EventKind::TraceStop | EventKind::TraceSnapshot
        )
    {
        return None;
    }
    Some(LEDGER.event(kind, Scope::default(), sampled))
}

fn measurement(category: Category) -> Measurement {
    match category {
        Category::ParsedVector | Category::PreparedVector | Category::HttpRewriteBuffer => {
            Measurement::ExactCapacity
        }
        Category::HttpBody | Category::RawSource => Measurement::SerializedSize,
        Category::MemtableActive
        | Category::ParsedSource
        | Category::PreparedDocument
        | Category::FlushDrained => Measurement::Estimated,
        _ => Measurement::Unavailable,
    }
}

pub struct Retained<'a> {
    ledger: &'a Ledger,
    category: Category,
    bytes: u64,
    active: bool,
}

impl Retained<'static> {
    #[inline]
    pub fn new(category: Category, bytes: usize) -> Self {
        let ledger = active_ledger();
        let Some(ledger) = ledger else {
            return Self::disabled();
        };
        let bytes = bytes as u64;
        ledger.retain(category, bytes);
        Self {
            ledger,
            category,
            bytes,
            active: true,
        }
    }

    #[inline]
    pub const fn disabled() -> Self {
        Self {
            ledger: &LEDGER,
            category: Category::HttpBody,
            bytes: 0,
            active: false,
        }
    }
}

impl<'a> Retained<'a> {
    pub(crate) fn for_ledger(ledger: &'a Ledger, category: Category, bytes: usize) -> Self {
        let bytes = bytes as u64;
        ledger.retain(category, bytes);
        Self {
            ledger,
            category,
            bytes,
            active: true,
        }
    }

    pub fn transfer(mut self, category: Category) -> Self {
        if self.active {
            self.ledger.transfer(self.category, category, self.bytes);
            self.category = category;
        }
        self
    }

    pub fn replace(mut self, category: Category, bytes: usize) -> Self {
        if self.active {
            let _transition = self
                .ledger
                .transition
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.ledger.release_locked(self.category, self.bytes) {
                self.ledger.retain_locked(category, bytes as u64);
                self.category = category;
                self.bytes = bytes as u64;
            }
        }
        self
    }

    pub fn replace_split(
        mut self,
        first: (Category, usize),
        second: (Category, usize),
    ) -> (Self, Self) {
        let mut other = Self {
            ledger: self.ledger,
            category: second.0,
            bytes: second.1 as u64,
            active: false,
        };
        if self.active {
            let _transition = self
                .ledger
                .transition
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.ledger.release_locked(self.category, self.bytes) {
                self.ledger.retain_locked(first.0, first.1 as u64);
                self.ledger.retain_locked(second.0, second.1 as u64);
                self.category = first.0;
                self.bytes = first.1 as u64;
                other.active = true;
            }
        }
        (self, other)
    }

    pub fn replace_pair(
        mut first: Self,
        mut second: Self,
        first_next: (Category, usize),
        second_next: (Category, usize),
    ) -> (Self, Self) {
        if first.active && second.active && std::ptr::eq(first.ledger, second.ledger) {
            let ledger = first.ledger;
            let _transition = ledger.transition.lock().unwrap_or_else(|e| e.into_inner());
            let first_current = ledger.cells[first.category as usize]
                .current
                .load(Ordering::Relaxed);
            let second_current = ledger.cells[second.category as usize]
                .current
                .load(Ordering::Relaxed);
            let enough = if first.category == second.category {
                first_current >= first.bytes.saturating_add(second.bytes)
            } else {
                first_current >= first.bytes && second_current >= second.bytes
            };
            if enough {
                let _ = ledger.release_locked(first.category, first.bytes);
                let _ = ledger.release_locked(second.category, second.bytes);
                ledger.retain_locked(first_next.0, first_next.1 as u64);
                ledger.retain_locked(second_next.0, second_next.1 as u64);
                first.category = first_next.0;
                first.bytes = first_next.1 as u64;
                second.category = second_next.0;
                second.bytes = second_next.1 as u64;
            } else {
                ledger.accounting_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        (first, second)
    }
}

/// Non-allocating estimate of heap retained by a JSON tree.
pub fn estimated_json_heap(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 0,
        serde_json::Value::String(s) => s.capacity(),
        serde_json::Value::Array(values) => {
            values.capacity() * std::mem::size_of::<serde_json::Value>()
                + values.iter().map(estimated_json_heap).sum::<usize>()
        }
        serde_json::Value::Object(values) => {
            values.len()
                * (std::mem::size_of::<String>() + std::mem::size_of::<serde_json::Value>())
                + values
                    .iter()
                    .map(|(key, value)| key.capacity() + estimated_json_heap(value))
                    .sum::<usize>()
        }
    }
}

pub fn estimated_json_heap_excluding(
    value: &serde_json::Value,
    excluded: &std::collections::HashSet<String>,
) -> usize {
    fn visit(value: &serde_json::Value, excluded: &std::collections::HashSet<String>) -> usize {
        match value {
            serde_json::Value::Object(values) => {
                values.len()
                    * (std::mem::size_of::<String>() + std::mem::size_of::<serde_json::Value>())
                    + values
                        .iter()
                        .map(|(key, value)| {
                            key.capacity()
                                + if excluded.contains(key) {
                                    0
                                } else {
                                    visit(value, excluded)
                                }
                        })
                        .sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                values.capacity() * std::mem::size_of::<serde_json::Value>()
                    + values
                        .iter()
                        .map(|value| visit(value, excluded))
                        .sum::<usize>()
            }
            serde_json::Value::String(value) => value.capacity(),
            _ => 0,
        }
    }
    visit(value, excluded)
}

pub fn vector_value_capacity(
    value: &serde_json::Value,
    fields: &std::collections::HashSet<String>,
) -> usize {
    fn arrays(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Array(values) => {
                values.capacity() * std::mem::size_of::<serde_json::Value>()
                    + values.iter().map(arrays).sum::<usize>()
            }
            serde_json::Value::Object(values) => values.values().map(arrays).sum(),
            _ => 0,
        }
    }
    match value {
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                if fields.contains(key) {
                    arrays(value)
                } else {
                    vector_value_capacity(value, fields)
                }
            })
            .sum(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| vector_value_capacity(value, fields))
            .sum(),
        _ => 0,
    }
}

impl Drop for Retained<'_> {
    fn drop(&mut self) {
        if self.active {
            self.ledger.release(self.category, self.bytes);
        }
    }
}

pub struct TrackedArc<T> {
    value: T,
    _retained: Retained<'static>,
}

impl<T> TrackedArc<T> {
    pub fn new(value: T, category: Category, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            value,
            _retained: Retained::new(category, bytes),
        })
    }

    pub fn get(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_modes_are_distinct_and_unknown_events_mode_fails_closed() {
        assert_eq!(TraceConfig::parse("off", "100").mode, TraceMode::Off);
        assert_eq!(
            TraceConfig::parse("summary", "100").mode,
            TraceMode::Summary
        );
        assert_eq!(
            TraceConfig::parse("events", "100").mode,
            TraceMode::Off,
            "v1 must not claim unwired lifecycle events"
        );
        assert_eq!(TraceConfig::parse("summary", "1").sample_ms, 25);
        assert_eq!(TraceConfig::parse("summary", "999999").sample_ms, 60_000);
    }

    #[test]
    fn retain_release_peak_and_underflow_are_deterministic() {
        let ledger = Ledger::new();
        ledger.retain(Category::RawSource, 100);
        ledger.retain(Category::RawSource, 60);
        ledger.release(Category::RawSource, 100);
        let gauge = ledger.gauge(Category::RawSource, Measurement::SerializedSize);
        assert_eq!((gauge.current, gauge.peak), (60, 160));
        ledger.release(Category::RawSource, 61);
        assert_eq!(ledger.accounting_errors(), 1);
        assert_eq!(
            ledger
                .gauge(Category::RawSource, Measurement::SerializedSize)
                .current,
            60
        );
    }

    #[test]
    fn snapshot_self_reports_logical_total_accounting_and_sink_loss() {
        let ledger = Ledger::new();
        ledger.retain(Category::RawSource, 100);
        ledger.retain(Category::PreparedVector, 28);
        ledger.release(Category::RawSource, 101);
        ledger.record_dropped_event();
        let event = ledger.event(EventKind::TraceSnapshot, Scope::default(), true);
        assert_eq!(event.logical_total_current, 128);
        assert_eq!(event.accounting_errors, 1);
        assert_eq!(event.dropped_events, 1);
    }

    #[test]
    fn tracked_arc_charges_one_backing_owner() {
        let ledger = Ledger::new();
        struct LocalTracked<'a, T> {
            value: T,
            _retained: Retained<'a>,
        }
        let owner = Arc::new(LocalTracked {
            value: vec![0u8; 100],
            _retained: Retained::for_ledger(&ledger, Category::RawSource, 100),
        });
        let clones: Vec<_> = (0..100).map(|_| Arc::clone(&owner)).collect();
        assert_eq!(owner.value.len(), 100);
        assert_eq!(
            ledger
                .gauge(Category::RawSource, Measurement::SerializedSize)
                .current,
            100
        );
        drop(clones);
        drop(owner);
        assert_eq!(
            ledger
                .gauge(Category::RawSource, Measurement::SerializedSize)
                .current,
            0
        );
    }

    #[test]
    fn transfer_snapshot_has_no_zero_or_double_count_gap() {
        let ledger = Ledger::new();
        let retained = Retained::for_ledger(&ledger, Category::MemtableActive, 77);
        let retained = retained.transfer(Category::FlushDrained);
        let active = ledger.gauge(Category::MemtableActive, Measurement::Estimated);
        let drained = ledger.gauge(Category::FlushDrained, Measurement::Estimated);
        assert_eq!((active.current, drained.current), (0, 77));
        drop(retained);
        assert_eq!(
            ledger
                .gauge(Category::FlushDrained, Measurement::Estimated)
                .current,
            0
        );
    }

    #[test]
    fn failed_transfer_does_not_charge_destination() {
        let ledger = Ledger::new();
        ledger.retain(Category::MemtableActive, 10);
        ledger.transfer(Category::MemtableActive, Category::FlushDrained, 11);
        assert_eq!(
            ledger
                .gauge(Category::MemtableActive, Measurement::Estimated)
                .current,
            10
        );
        assert_eq!(
            ledger
                .gauge(Category::FlushDrained, Measurement::Estimated)
                .current,
            0
        );
        assert_eq!(ledger.accounting_errors(), 1);
    }

    #[test]
    fn semantic_phase_split_is_non_overlapping_and_balances_on_drop() {
        let ledger = Ledger::new();
        let parsed = Retained::for_ledger(&ledger, Category::ParsedSource, 120);
        let (document, vector) = parsed.replace_split(
            (Category::PreparedDocument, 80),
            (Category::PreparedVector, 40),
        );
        assert_eq!(
            ledger
                .gauge(Category::ParsedSource, Measurement::Estimated)
                .current,
            0
        );
        assert_eq!(
            ledger
                .gauge(Category::PreparedDocument, Measurement::Estimated)
                .current,
            80
        );
        assert_eq!(
            ledger
                .gauge(Category::PreparedVector, Measurement::ExactCapacity)
                .current,
            40
        );
        drop((document, vector));
        assert_eq!(
            ledger
                .gauge(Category::PreparedDocument, Measurement::Estimated)
                .current
                + ledger
                    .gauge(Category::PreparedVector, Measurement::ExactCapacity)
                    .current,
            0
        );
    }

    #[test]
    fn caller_vector_and_document_transition_as_one_snapshot() {
        let ledger = Ledger::new();
        let document = Retained::for_ledger(&ledger, Category::ParsedSource, 70);
        let vector = Retained::for_ledger(&ledger, Category::ParsedVector, 30);
        let (document, vector) = Retained::replace_pair(
            document,
            vector,
            (Category::PreparedDocument, 60),
            (Category::PreparedVector, 40),
        );
        assert_eq!(
            ledger
                .gauge(Category::ParsedSource, Measurement::Estimated)
                .current
                + ledger
                    .gauge(Category::ParsedVector, Measurement::ExactCapacity)
                    .current,
            0
        );
        assert_eq!(
            ledger
                .gauge(Category::PreparedDocument, Measurement::Estimated)
                .current
                + ledger
                    .gauge(Category::PreparedVector, Measurement::ExactCapacity)
                    .current,
            100
        );
        drop((document, vector));
        assert_eq!(
            ledger
                .gauge(Category::PreparedDocument, Measurement::Estimated)
                .current
                + ledger
                    .gauge(Category::PreparedVector, Measurement::ExactCapacity)
                    .current,
            0
        );
    }

    #[test]
    fn custom_vector_fields_are_excluded_and_capacity_counted() {
        let value = serde_json::json!({
            "body": "hello",
            "custom_embedding": [0.1, 0.2, 0.3],
        });
        let fields = std::collections::HashSet::from(["custom_embedding".to_string()]);
        let without = estimated_json_heap_excluding(&value, &fields);
        let with = estimated_json_heap(&value);
        assert!(without < with);
        assert_eq!(
            vector_value_capacity(&value, &fields),
            value["custom_embedding"].as_array().unwrap().capacity()
                * std::mem::size_of::<serde_json::Value>()
        );
    }

    #[test]
    fn guard_balances_error_and_panic_unwind() {
        let ledger = Ledger::new();
        let result = std::panic::catch_unwind(|| {
            ledger.retain(Category::FlushDrained, 42);
            struct Local<'a>(&'a Ledger);
            impl Drop for Local<'_> {
                fn drop(&mut self) {
                    self.0.release(Category::FlushDrained, 42);
                }
            }
            let _guard = Local(&ledger);
            panic!("intentional");
        });
        assert!(result.is_err());
        assert_eq!(
            ledger
                .gauge(Category::FlushDrained, Measurement::Estimated)
                .current,
            0
        );
    }

    #[test]
    fn physical_failure_is_null_not_zero() {
        let ledger = Ledger::new();
        ledger.update_physical(AllocatorSnapshot::default(), ProcessSnapshot::default());
        let event = ledger.event(EventKind::TraceSnapshot, Scope::default(), true);
        assert_eq!(event.allocator.allocated, None);
        assert_eq!(event.process.rss, None);
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["schema"], SCHEMA);
        assert!(json["allocator"]["allocated"].is_null());
    }
}
