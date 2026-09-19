//! What a poll costs, in the units the bill is denominated in.
//!
//! Object storage has no inotify, so keeping an index current means asking the
//! bucket what changed. `ListObjectsV2` is a **Class A** operation on
//! Cloudflare R2 (and a LIST request on S3): the expensive class, charged per
//! call, one call per 1,000 keys returned. That makes the poll interval a
//! spending decision, not a latency preference, and this module is the
//! arithmetic that decision needs.
//!
//! The numbers below are R2's free tier as Cloudflare publishes it
//! (10 GB-month stored, 1,000,000 Class A operations/month, 10,000,000 Class B,
//! free egress). Deletes are free; `HeadObject`/`GetObject` are Class B.
//! Verify them against the current pricing page before quoting them: this file
//! is the arithmetic, not the price list.

/// Cloudflare R2's free-tier Class A allowance per month (`ListObjectsV2`,
/// `PutObject`, `CopyObject`, the multipart verbs).
pub const CLASS_A_FREE_MONTHLY: u64 = 1_000_000;

/// Free-tier Class B allowance per month (`GetObject`, `HeadObject`).
pub const CLASS_B_FREE_MONTHLY: u64 = 10_000_000;

/// A 30-day month, in seconds. Cloudflare bills per calendar month; 30 days is
/// the conservative divisor (a 31-day month yields ~3% more cycles).
pub const SECONDS_PER_MONTH: u64 = 30 * 24 * 60 * 60;

/// Keys one `ListObjectsV2` call can return. The API's own maximum, and the
/// unit Class A billing counts in.
pub const MAX_KEYS_PER_LIST: u64 = 1_000;

/// Default ceiling on projected Class A operations per month: 20% of the free
/// tier. Not 100%, because the same account's other buckets — release
/// downloads, backups, anything a Worker touches — spend from the same
/// allowance, and a watcher that budgets for all of it leaves nothing for the
/// product it is watching for.
pub const DEFAULT_MAX_MONTHLY_CLASS_A: u64 = 200_000;

/// Default poll interval: 5 minutes.
///
/// Chosen so the default is free-tier-safe on a realistic bucket rather than on
/// an empty one: at 300 s a watcher runs 8,640 cycles a month, so it stays
/// under [`DEFAULT_MAX_MONTHLY_CLASS_A`] for any bucket up to ~23 list pages
/// (~23,000 objects) and under the whole free tier up to ~115 pages
/// (~115,000 objects). A 5 s interval would cost ~518,400 calls a month on an
/// EMPTY bucket — half the free tier to watch nothing.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 300;

/// List calls one full scan of `keys` objects costs. An empty bucket still
/// costs one call: you have to ask to learn it is empty.
pub fn list_calls_for_keys(keys: u64) -> u64 {
    keys.div_ceil(MAX_KEYS_PER_LIST).max(1)
}

/// Poll cycles a month at this interval.
pub fn cycles_per_month(interval_secs: u64) -> u64 {
    if interval_secs == 0 {
        return u64::MAX;
    }
    SECONDS_PER_MONTH / interval_secs
}

/// Class A operations a month for a watcher that issues `list_calls_per_cycle`
/// list calls every `interval_secs`.
pub fn projected_monthly_class_a(list_calls_per_cycle: u64, interval_secs: u64) -> u64 {
    list_calls_per_cycle.saturating_mul(cycles_per_month(interval_secs))
}

/// The smallest interval whose projection fits `budget` Class A operations a
/// month. Returns [`SECONDS_PER_MONTH`] when even one cycle a month would not
/// fit (budget 0).
pub fn min_safe_interval_secs(list_calls_per_cycle: u64, budget: u64) -> u64 {
    if budget == 0 {
        return SECONDS_PER_MONTH;
    }
    let calls = list_calls_per_cycle.max(1);
    let needed = calls
        .saturating_mul(SECONDS_PER_MONTH)
        .div_ceil(budget)
        .max(1);
    needed.min(SECONDS_PER_MONTH)
}

/// A projection an operator can read, and the reason a run was refused.
///
/// `PartialEq` but not `Eq`: `free_tier_percent` is a float, derived from the
/// integer fields, so two projections that compare equal on the integers are the
/// same projection. Tests compare those integers, never the percentage.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub keys_listed: u64,
    pub list_calls_per_cycle: u64,
    pub interval_secs: u64,
    pub cycles_per_month: u64,
    pub monthly_class_a: u64,
    pub budget: u64,
    /// Free-tier share, in percent, of [`CLASS_A_FREE_MONTHLY`].
    pub free_tier_percent: f64,
    pub min_safe_interval_secs: u64,
}

impl Projection {
    pub fn new(keys_listed: u64, list_calls_per_cycle: u64, interval_secs: u64, budget: u64) -> Projection {
        let monthly = projected_monthly_class_a(list_calls_per_cycle, interval_secs);
        Projection {
            keys_listed,
            list_calls_per_cycle,
            interval_secs,
            cycles_per_month: cycles_per_month(interval_secs),
            monthly_class_a: monthly,
            budget,
            free_tier_percent: (monthly as f64) * 100.0 / (CLASS_A_FREE_MONTHLY as f64),
            min_safe_interval_secs: min_safe_interval_secs(list_calls_per_cycle, budget),
        }
    }

    pub fn over_budget(&self) -> bool {
        self.monthly_class_a > self.budget
    }

    /// One line, in the units the bill uses.
    pub fn line(&self) -> String {
        format!(
            "poll cost: {} objects = {} list call(s)/cycle x {} cycles/month = {} Class A ops/month \
             ({:.1}% of the 1,000,000 free tier; budget {})",
            self.keys_listed,
            self.list_calls_per_cycle,
            self.cycles_per_month,
            self.monthly_class_a,
            self.free_tier_percent,
            self.budget
        )
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "keys_listed": self.keys_listed,
            "list_calls_per_cycle": self.list_calls_per_cycle,
            "interval_secs": self.interval_secs,
            "cycles_per_month": self.cycles_per_month,
            "monthly_class_a": self.monthly_class_a,
            "budget_monthly_class_a": self.budget,
            "free_tier_monthly_class_a": CLASS_A_FREE_MONTHLY,
            "free_tier_percent": (self.free_tier_percent * 10.0).round() / 10.0,
            "min_safe_interval_secs": self.min_safe_interval_secs,
            "over_budget": self.over_budget(),
        })
    }
}

/// Running totals for one watch process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostTotals {
    pub cycles: u64,
    /// Class A.
    pub list_calls: u64,
    /// Class B.
    pub gets: u64,
    pub bytes_fetched: u64,
    pub keys_listed: u64,
    pub events: u64,
    /// Deadlines that passed while a cycle was still running.
    pub skipped_deadlines: u64,
    pub errors: u64,
}

impl CostTotals {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "cycles": self.cycles,
            "list_calls_class_a": self.list_calls,
            "gets_class_b": self.gets,
            "bytes_fetched": self.bytes_fetched,
            "keys_listed": self.keys_listed,
            "events": self.events,
            "skipped_deadlines": self.skipped_deadlines,
            "errors": self.errors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_call_covers_a_thousand_keys_and_an_empty_bucket_still_costs_one() {
        assert_eq!(list_calls_for_keys(0), 1);
        assert_eq!(list_calls_for_keys(1), 1);
        assert_eq!(list_calls_for_keys(1_000), 1);
        assert_eq!(list_calls_for_keys(1_001), 2);
        assert_eq!(list_calls_for_keys(100_000), 100);
        assert_eq!(list_calls_for_keys(1_000_000), 1_000);
    }

    /// The arithmetic the docs publish, asserted here so the docs cannot drift
    /// from the code that enforces them.
    #[test]
    fn the_published_cost_table_is_what_the_code_computes() {
        // Empty bucket, 5 s poll: half the free tier to watch nothing.
        assert_eq!(projected_monthly_class_a(1, 5), 518_400);
        // Empty bucket, 60 s poll: 4% of the tier.
        assert_eq!(projected_monthly_class_a(1, 60), 43_200);
        // 100,000 objects (100 pages), 60 s poll: over four times the tier.
        assert_eq!(projected_monthly_class_a(100, 60), 4_320_000);
        assert!(projected_monthly_class_a(100, 60) > 4 * CLASS_A_FREE_MONTHLY);
        // 1,000,000 objects (1,000 pages), 30 s poll: 86,400,000.
        assert_eq!(projected_monthly_class_a(1_000, 30), 86_400_000);
        // The default interval on a 10,000-object bucket.
        assert_eq!(projected_monthly_class_a(10, DEFAULT_POLL_INTERVAL_SECS), 86_400);
    }

    #[test]
    fn the_default_interval_is_inside_the_default_budget_for_a_realistic_bucket() {
        // 20,000 objects at the default interval stays inside the default 20%
        // budget; that is what makes the default safe rather than lucky.
        let p = Projection::new(
            20_000,
            list_calls_for_keys(20_000),
            DEFAULT_POLL_INTERVAL_SECS,
            DEFAULT_MAX_MONTHLY_CLASS_A,
        );
        assert!(!p.over_budget(), "{}", p.line());
        // 100,000 objects does not, and the guard must say so.
        let p = Projection::new(
            100_000,
            list_calls_for_keys(100_000),
            DEFAULT_POLL_INTERVAL_SECS,
            DEFAULT_MAX_MONTHLY_CLASS_A,
        );
        assert!(p.over_budget(), "{}", p.line());
        assert!(p.min_safe_interval_secs > DEFAULT_POLL_INTERVAL_SECS);
    }

    #[test]
    fn the_minimum_safe_interval_is_the_inverse_of_the_projection() {
        for (calls, budget) in [(1u64, 200_000u64), (10, 200_000), (100, 1_000_000), (1_000, 1_000_000)] {
            let secs = min_safe_interval_secs(calls, budget);
            assert!(
                projected_monthly_class_a(calls, secs) <= budget,
                "calls={calls} budget={budget} secs={secs} => {}",
                projected_monthly_class_a(calls, secs)
            );
        }
        // A 1 s poll on a 100,000-object bucket is the example in the docs.
        assert_eq!(projected_monthly_class_a(100, 1), 259_200_000);
    }

    #[test]
    fn a_zero_interval_projects_as_unbounded_rather_than_dividing_by_zero() {
        assert_eq!(cycles_per_month(0), u64::MAX);
        assert_eq!(projected_monthly_class_a(2, 0), u64::MAX);
    }
}
