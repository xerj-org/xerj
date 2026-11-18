//! Process-wide retained-payload budget for immutable segment hydration caches.
//!
//! This is deliberately not an RSS limit: allocator fragmentation, DashMap
//! capacity, mmaps, query-owned materialization, and decode scratch live
//! outside this accounting. A charge follows the cached value through every
//! in-flight reader and is refunded only when its last `Arc` is dropped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::ingest_memory::{Category, Retained};
use crate::read_budget::ReadPermit;

pub const CATEGORY_COUNT: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum SegmentCacheCategory {
    StoredSlices = 0,
    DocValues,
    StoredValues,
    SortShadow,
    IdPositions,
    RowSequences,
    DecodedStored,
    /// Decompressed `.post` + `.meta` blobs behind a cached `FtsIndexReader`.
    FtsReader,
    /// Flat `f32` vectors of one field over one segment, derived from its
    /// stored documents for the exact kNN scan (#939).
    VectorColumn,
}

impl SegmentCacheCategory {
    pub const ALL: [Self; CATEGORY_COUNT] = [
        Self::StoredSlices,
        Self::DocValues,
        Self::StoredValues,
        Self::SortShadow,
        Self::IdPositions,
        Self::RowSequences,
        Self::DecodedStored,
        Self::FtsReader,
        Self::VectorColumn,
    ];

    fn trace_category(self) -> Category {
        match self {
            Self::StoredSlices => Category::CacheStoredSlices,
            Self::DocValues => Category::CacheDocValues,
            Self::StoredValues => Category::CacheStoredValues,
            Self::SortShadow => Category::CacheSortShadow,
            Self::IdPositions => Category::CacheIdPositions,
            Self::RowSequences => Category::CacheRowSequences,
            Self::DecodedStored => Category::CacheDecodedStored,
            Self::FtsReader => Category::CacheFtsReader,
            Self::VectorColumn => Category::CacheVectorColumn,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize)]
pub struct SegmentHydrationBudgetSnapshot {
    pub limit: u64,
    pub current: u64,
    pub peak: u64,
    pub refused: u64,
    pub accounting_errors: u64,
    pub category_current: [u64; CATEGORY_COUNT],
    pub category_peak: [u64; CATEGORY_COUNT],
}

pub struct SegmentHydrationBudget {
    limit: u64,
    used: AtomicU64,
    peak: AtomicU64,
    refused: AtomicU64,
    accounting_errors: AtomicU64,
    category_current: [AtomicU64; CATEGORY_COUNT],
    category_peak: [AtomicU64; CATEGORY_COUNT],
}

impl SegmentHydrationBudget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            accounting_errors: AtomicU64::new(0),
            category_current: std::array::from_fn(|_| AtomicU64::new(0)),
            category_peak: std::array::from_fn(|_| AtomicU64::new(0)),
        })
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Whether a `bytes` charge would fit right now. Advisory (another thread
    /// may charge first): used to skip building a value the cache would only
    /// refuse, and to decide whether a build is transient.
    pub fn has_headroom(&self, bytes: u64) -> bool {
        self.used
            .load(Ordering::Acquire)
            .checked_add(bytes)
            .is_some_and(|next| next <= self.limit)
    }

    pub fn try_charge(
        self: &Arc<Self>,
        category: SegmentCacheCategory,
        bytes: u64,
    ) -> Option<BudgetCharge> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                self.refused.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            if next > self.limit {
                self.refused.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::Relaxed);
                    let category_current = self.category_current[category as usize]
                        .fetch_add(bytes, Ordering::AcqRel)
                        + bytes;
                    self.category_peak[category as usize]
                        .fetch_max(category_current, Ordering::Relaxed);
                    return Some(BudgetCharge {
                        budget: Arc::clone(self),
                        category,
                        bytes,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub fn snapshot(&self) -> SegmentHydrationBudgetSnapshot {
        SegmentHydrationBudgetSnapshot {
            limit: self.limit,
            current: self.used.load(Ordering::Acquire),
            peak: self.peak.load(Ordering::Acquire),
            refused: self.refused.load(Ordering::Acquire),
            accounting_errors: self.accounting_errors.load(Ordering::Acquire),
            category_current: std::array::from_fn(|i| {
                self.category_current[i].load(Ordering::Acquire)
            }),
            category_peak: std::array::from_fn(|i| self.category_peak[i].load(Ordering::Acquire)),
        }
    }

    fn release(&self, category: SegmentCacheCategory, bytes: u64) {
        let category_underflow =
            checked_sub(&self.category_current[category as usize], bytes).is_err();
        let total_underflow = checked_sub(&self.used, bytes).is_err();
        if category_underflow || total_underflow {
            self.accounting_errors.fetch_add(1, Ordering::Relaxed);
            debug_assert!(false, "segment hydration cache accounting underflow");
        }
    }
}

fn checked_sub(counter: &AtomicU64, bytes: u64) -> Result<u64, u64> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_sub(bytes))
        })
        .and_then(|previous| {
            if previous < bytes {
                Err(previous)
            } else {
                Ok(previous)
            }
        })
}

pub struct BudgetCharge {
    budget: Arc<SegmentHydrationBudget>,
    category: SegmentCacheCategory,
    bytes: u64,
}

impl Drop for BudgetCharge {
    fn drop(&mut self) {
        self.budget.release(self.category, self.bytes);
    }
}

pub struct CacheResident<T> {
    value: T,
    _charge: Option<BudgetCharge>,
    /// A value the cache refused carries its transient read reservation
    /// (#2427/#1122), returned when the last reader drops it.
    _read_permit: Option<ReadPermit>,
    _trace_retained: Retained<'static>,
}

impl<T> CacheResident<T> {
    pub fn admitted(
        value: T,
        charge: BudgetCharge,
        category: SegmentCacheCategory,
        bytes: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            value,
            _charge: Some(charge),
            _read_permit: None,
            _trace_retained: Retained::new(
                category.trace_category(),
                usize::try_from(bytes).unwrap_or(usize::MAX),
            ),
        })
    }

    pub fn uncached(value: T) -> Arc<Self> {
        Self::uncached_with_permit(value, None)
    }

    /// An uncached value that keeps `permit` (its transient read reservation)
    /// until the last reference is dropped.
    pub fn uncached_with_permit(value: T, permit: Option<ReadPermit>) -> Arc<Self> {
        Arc::new(Self {
            value,
            _charge: None,
            _read_permit: permit,
            _trace_retained: Retained::disabled(),
        })
    }

    pub fn value(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::Deref for CacheResident<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_is_refunded_only_after_last_arc() {
        let budget = SegmentHydrationBudget::new(64);
        let charge = budget
            .try_charge(SegmentCacheCategory::StoredValues, 40)
            .unwrap();
        let resident =
            CacheResident::admitted(vec![1_u8], charge, SegmentCacheCategory::StoredValues, 40);
        let reader = Arc::clone(&resident);
        drop(resident);
        assert_eq!(budget.snapshot().current, 40);
        drop(reader);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn zero_limit_overflow_and_categories_are_exact() {
        let off = SegmentHydrationBudget::new(0);
        assert!(off
            .try_charge(SegmentCacheCategory::StoredSlices, 1)
            .is_none());
        let budget = SegmentHydrationBudget::new(u64::MAX);
        let charge = budget
            .try_charge(SegmentCacheCategory::DocValues, u64::MAX)
            .unwrap();
        assert!(budget
            .try_charge(SegmentCacheCategory::RowSequences, 1)
            .is_none());
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.current, u64::MAX);
        assert_eq!(
            snapshot.category_current[SegmentCacheCategory::DocValues as usize],
            u64::MAX
        );
        assert_eq!(snapshot.refused, 1);
        drop(charge);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn underflow_is_detected_without_wrapping_the_counter() {
        let budget = SegmentHydrationBudget::new(16);
        let charge = budget
            .try_charge(SegmentCacheCategory::RowSequences, 8)
            .unwrap();
        drop(charge);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            budget.release(SegmentCacheCategory::RowSequences, 8);
        }));
        #[cfg(debug_assertions)]
        assert!(result.is_err(), "debug builds must surface double release");
        #[cfg(not(debug_assertions))]
        assert!(
            result.is_ok(),
            "release builds must record and repair underflow without panicking"
        );
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.current, 0);
        assert_eq!(
            snapshot.category_current[SegmentCacheCategory::RowSequences as usize],
            0
        );
        assert_eq!(snapshot.accounting_errors, 1);
    }

    #[test]
    fn concurrent_cas_never_exceeds_limit() {
        let budget = SegmentHydrationBudget::new(1_000);
        let barrier = Arc::new(std::sync::Barrier::new(65));
        let mut threads = Vec::new();
        for _ in 0..64 {
            let budget = Arc::clone(&budget);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                budget.try_charge(SegmentCacheCategory::DecodedStored, 31)
            }));
        }
        barrier.wait();
        let charges: Vec<_> = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap())
            .collect();
        let snapshot = budget.snapshot();
        assert!(snapshot.current <= 1_000);
        assert!(snapshot.peak <= 1_000);
        assert_eq!(snapshot.current, charges.len() as u64 * 31);
        drop(charges);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn refusal_can_return_an_uncharged_correct_local_value() {
        let budget = SegmentHydrationBudget::new(1);
        assert!(budget
            .try_charge(SegmentCacheCategory::StoredValues, 2)
            .is_none());
        let local = CacheResident::uncached(vec![String::from("answer")]);
        assert_eq!(local[0], "answer");
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.current, 0);
        assert_eq!(snapshot.refused, 1);
    }

    #[test]
    fn retirement_refunds_only_after_held_reader_drops() {
        let budget = SegmentHydrationBudget::new(128);
        let map = dashmap::DashMap::<String, Arc<CacheResident<Vec<u8>>>>::new();
        let charge = budget
            .try_charge(SegmentCacheCategory::DecodedStored, 80)
            .unwrap();
        let resident =
            CacheResident::admitted(vec![7], charge, SegmentCacheCategory::DecodedStored, 80);
        map.insert("retired".into(), Arc::clone(&resident));
        let reader = Arc::clone(&resident);
        drop(resident);
        map.remove("retired");
        assert_eq!(budget.snapshot().current, 80);
        drop(reader);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn two_index_maps_share_one_ceiling() {
        let budget = SegmentHydrationBudget::new(100);
        let first = dashmap::DashMap::<String, Arc<CacheResident<Vec<u8>>>>::new();
        let second = dashmap::DashMap::<String, Arc<CacheResident<Vec<u8>>>>::new();
        let charge = budget
            .try_charge(SegmentCacheCategory::StoredSlices, 60)
            .unwrap();
        first.insert(
            "a".into(),
            CacheResident::admitted(vec![1], charge, SegmentCacheCategory::StoredSlices, 60),
        );
        assert!(budget
            .try_charge(SegmentCacheCategory::DocValues, 60)
            .is_none());
        assert!(second.is_empty());
        assert_eq!(budget.snapshot().peak, 60);
        drop(first);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[tokio::test]
    async fn telemetry_categories_match_budget_and_release_to_zero() {
        let ledger = Box::leak(Box::new(crate::ingest_memory::Ledger::new()));
        crate::ingest_memory::with_test_ledger(ledger, async {
            let budget = SegmentHydrationBudget::new(1_000);
            for category in SegmentCacheCategory::ALL {
                let charge = budget.try_charge(category, 17).unwrap();
                let resident = CacheResident::admitted(vec![1_u8], charge, category, 17);
                assert_eq!(
                    ledger
                        .gauge(
                            category.trace_category(),
                            crate::ingest_memory::Measurement::Estimated,
                        )
                        .current,
                    17
                );
                assert_eq!(budget.snapshot().category_current[category as usize], 17);
                drop(resident);
                assert_eq!(
                    ledger
                        .gauge(
                            category.trace_category(),
                            crate::ingest_memory::Measurement::Estimated,
                        )
                        .current,
                    0
                );
            }
            assert_eq!(budget.snapshot().current, 0);
        })
        .await;
    }
}
