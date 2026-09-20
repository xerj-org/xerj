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

/// How many Class A operations a scan charges to the ledger in one write.
///
/// The ledger is written BEFORE the calls it pays for, in batches of this size,
/// so a process killed mid-scan still owes what it spent (#968 review, B1: five
/// `kill -9`s two seconds into a 2,001-call scan served 241 list calls and the
/// ledger file was never created, because the whole cycle's spend was charged
/// only after `scan()` returned). The unused tail of the last batch is refunded
/// when the scan ends, so a completed cycle records exactly what the store
/// served.
///
/// 16 is the trade: a crash can over-record at most 15 operations out of a
/// 200,000 default budget, and a full 10,000-object scan at the default page
/// size costs two ledger writes instead of one. It is never allowed to
/// under-record, because under-recording is the direction that spends money.
pub const CLASS_A_CHARGE_BATCH: u64 = 16;

/// Default poll interval: 5 minutes.
///
/// Chosen so the default is free-tier-safe on a realistic bucket rather than on
/// an empty one: at 300 s a watcher runs 8,640 cycles a month, so it stays
/// under [`DEFAULT_MAX_MONTHLY_CLASS_A`] for any bucket up to ~23 list pages
/// (~23,000 objects) and under the whole free tier up to ~115 pages
/// (~115,000 objects). A 5 s interval would cost ~518,400 calls a month on an
/// EMPTY bucket — half the free tier to watch nothing.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 300;

/// Default ceiling on Class B operations (GETs) per calendar month: 20% of the
/// free tier, for the same reason as the Class A default. A first scan of a
/// large bucket fetches every object once, and a churning bucket re-fetches
/// every changed one, so GETs need a breaker of their own.
pub const DEFAULT_MAX_MONTHLY_CLASS_B: u64 = 2_000_000;

/// List calls one full scan of `keys` objects costs at the API's maximum page
/// size. An empty bucket still costs one call: you have to ask to learn it is
/// empty.
pub fn list_calls_for_keys(keys: u64) -> u64 {
    list_calls_for_keys_at(keys, MAX_KEYS_PER_LIST)
}

/// List calls one full scan of `keys` objects costs at `page_size` keys per
/// call: `ceil(keys / page_size)`, at least 1.
///
/// The page size is the billing unit. Pricing every scan at 1,000 keys a page
/// while the watcher actually asked for 10 under-projected the Class A spend
/// 100x — `--page-size 1` on a 26-object prefix was projected at 1 call a cycle
/// and cost 26.
pub fn list_calls_for_keys_at(keys: u64, page_size: u64) -> u64 {
    let page = page_size.clamp(1, MAX_KEYS_PER_LIST);
    keys.div_ceil(page).max(1)
}

/// Poll cycles a month at this interval.
pub fn cycles_per_month(interval_secs: u64) -> u64 {
    cycles_per_month_millis(interval_secs.saturating_mul(1_000))
}

/// Poll cycles a month at a sub-second-capable interval.
///
/// Whole seconds are not enough resolution to be honest here. `Duration::as_secs`
/// truncates, so a 10 ms interval reads as 0 s, and a 0 s interval has to project
/// as unbounded — which made a sub-second poll look *unmeasurable* instead of
/// looking like the 259,200,000 operations a month it actually is. Millisecond
/// math gives the real number, and the guard can then refuse it for the right
/// reason.
pub fn cycles_per_month_millis(interval_millis: u64) -> u64 {
    if interval_millis == 0 {
        return u64::MAX;
    }
    (SECONDS_PER_MONTH.saturating_mul(1_000)) / interval_millis
}

/// Class A operations a month for a watcher that issues `list_calls_per_cycle`
/// list calls every `interval_secs`.
pub fn projected_monthly_class_a(list_calls_per_cycle: u64, interval_secs: u64) -> u64 {
    list_calls_per_cycle.saturating_mul(cycles_per_month(interval_secs))
}

/// The same, from a millisecond interval.
pub fn projected_monthly_class_a_millis(list_calls_per_cycle: u64, interval_millis: u64) -> u64 {
    list_calls_per_cycle.saturating_mul(cycles_per_month_millis(interval_millis))
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
    pub fn new(
        keys_listed: u64,
        list_calls_per_cycle: u64,
        interval_secs: u64,
        budget: u64,
    ) -> Projection {
        Projection::from_millis(
            keys_listed,
            list_calls_per_cycle,
            interval_secs.saturating_mul(1_000),
            budget,
        )
    }

    /// Project from a millisecond interval. `interval_secs` on the result is the
    /// interval rounded DOWN to whole seconds, which is what an operator-facing
    /// line should say; the arithmetic itself uses the milliseconds, so a
    /// sub-second interval is priced rather than reported as unbounded.
    pub fn from_millis(
        keys_listed: u64,
        list_calls_per_cycle: u64,
        interval_millis: u64,
        budget: u64,
    ) -> Projection {
        let monthly = projected_monthly_class_a_millis(list_calls_per_cycle, interval_millis);
        Projection {
            keys_listed,
            list_calls_per_cycle,
            interval_secs: interval_millis / 1_000,
            cycles_per_month: cycles_per_month_millis(interval_millis),
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
    /// Cycles that failed outright (the listing itself errored) and were
    /// retried at the next poll.
    pub failed_cycles: u64,
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
            "failed_cycles": self.failed_cycles,
        })
    }
}

/// Operations actually spent in one calendar month (UTC), persisted next to the
/// journal so the budget survives a restart.
///
/// The projection answers "will this interval fit?"; the ledger answers "has it
/// fit so far?". Without it a watcher under a supervisor that restarts it —
/// every crash, every deploy — would begin a fresh process-local count each time
/// and could re-scan a large bucket all month long without ever tripping. The
/// ledger is what makes the budget a monthly cap rather than a per-process one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MonthSpend {
    /// `YYYY-MM`, UTC. A different month on load means a new allowance.
    pub month: String,
    /// `ListObjectsV2` calls, including failed ones: whether a failed request
    /// is billed is the provider's call, so the ledger assumes it is.
    pub class_a: u64,
    /// `GetObject` calls, same rule.
    pub class_b: u64,
}

pub fn current_month() -> String {
    chrono::Utc::now().format("%Y-%m").to_string()
}

/// [`MonthSpend`] plus where it lives.
#[derive(Debug, Clone)]
pub struct SpendLedger {
    path: Option<std::path::PathBuf>,
    pub spend: MonthSpend,
}

impl SpendLedger {
    pub const FILE: &'static str = "objwatch-spend.json";

    /// Load `<state_dir>/objwatch-spend.json`, or start at zero. A ledger for a
    /// past month is a new month's allowance; an unreadable one is an error,
    /// because treating it as zero would hand a broken watcher a fresh budget.
    ///
    /// A ledger stamped with a **future** month is not a new allowance either
    /// (#968 review, m3). `{"month":"2099-01",…}` used to roll over to zero on
    /// every load, so a backwards clock step across a UTC month boundary — a VM
    /// restored from a snapshot, an NTP correction, a container with no RTC —
    /// minted a fresh budget. The spend is carried into the current month
    /// instead, and the anomaly is reported on stderr.
    ///
    /// What this still cannot defend against: **deleting** the file. The state
    /// directory is the trust boundary, and a ledger that is not there is
    /// indistinguishable from a first run. Anything that can unlink it can
    /// reset the budget; that is why the file lives beside the journal and the
    /// lock rather than in a temp directory.
    pub fn open(state_dir: &std::path::Path) -> anyhow::Result<SpendLedger> {
        use anyhow::Context;
        let path = state_dir.join(Self::FILE);
        let month = current_month();
        let spend = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read spend ledger {}", path.display()))?;
            let s: MonthSpend = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "parse spend ledger {} (delete it only if you know this month's spend)",
                    path.display()
                )
            })?;
            if s.month == month {
                s
            } else if s.month.as_str() > month.as_str() {
                // "YYYY-MM" sorts chronologically, so this is a ledger from the
                // future: the clock moved backwards. Keep the spend.
                eprintln!(
                    "xerj-watch: spend ledger {} is stamped {}, which is after the current UTC \
                     month {month} — the clock moved backwards. Carrying the recorded spend \
                     (Class A {}, Class B {}) into {month} rather than granting a fresh \
                     allowance.",
                    path.display(),
                    s.month,
                    s.class_a,
                    s.class_b
                );
                MonthSpend {
                    month,
                    class_a: s.class_a,
                    class_b: s.class_b,
                }
            } else {
                MonthSpend {
                    month,
                    ..Default::default()
                }
            }
        } else {
            MonthSpend {
                month,
                ..Default::default()
            }
        };
        Ok(SpendLedger {
            path: Some(path),
            spend,
        })
    }

    /// A ledger that is never written. For callers without a state directory.
    pub fn in_memory() -> SpendLedger {
        SpendLedger {
            path: None,
            spend: MonthSpend {
                month: current_month(),
                ..Default::default()
            },
        }
    }

    fn roll(&mut self) {
        let m = current_month();
        if self.spend.month != m {
            self.spend = MonthSpend {
                month: m,
                ..Default::default()
            };
        }
    }

    pub fn add(&mut self, class_a: u64, class_b: u64) {
        self.roll();
        self.spend.class_a = self.spend.class_a.saturating_add(class_a);
        self.spend.class_b = self.spend.class_b.saturating_add(class_b);
    }

    /// Give back Class A operations that were charged in advance and then not
    /// made. Only [`SpendLedger::reserve_class_a`]'s unused tail is ever
    /// refunded, so a completed cycle records exactly the calls the store
    /// served, while a crashed one keeps the conservative over-charge.
    pub fn refund_class_a(&mut self, class_a: u64) {
        self.spend.class_a = self.spend.class_a.saturating_sub(class_a);
    }

    /// Charge `class_a` operations and put the ledger on disk BEFORE they are
    /// made. The pair exists as one call so a caller cannot charge without
    /// persisting, which is the bug this replaced.
    pub fn reserve_class_a(&mut self, class_a: u64) -> anyhow::Result<()> {
        self.add(class_a, 0);
        self.save()
    }

    /// Atomic write (temp file + rename), so a crash never leaves a half file
    /// that would fail to parse.
    pub fn save(&self) -> anyhow::Result<()> {
        use anyhow::Context;
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(&self.spend)?)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F2 of the #968 review: the projection must use the page size the watcher
    /// actually lists with. 26 objects at `--page-size 1` is 26 calls a cycle.
    #[test]
    fn list_calls_are_counted_at_the_page_size_actually_used() {
        assert_eq!(list_calls_for_keys_at(26, 1), 26);
        assert_eq!(list_calls_for_keys_at(25, 10), 3);
        assert_eq!(list_calls_for_keys_at(0, 10), 1);
        assert_eq!(list_calls_for_keys_at(10_000, 100), 100);
        assert_eq!(list_calls_for_keys_at(10_000, 1_000), 10);
        // A page size above the API maximum is priced at the maximum, which is
        // what the store actually serves.
        assert_eq!(list_calls_for_keys_at(10_000, 5_000), 10);
    }

    #[test]
    fn the_spend_ledger_persists_and_rolls_over_at_a_new_month() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = SpendLedger::open(dir.path()).unwrap();
        assert_eq!(l.spend.class_a, 0);
        l.add(7, 3);
        l.save().unwrap();
        let again = SpendLedger::open(dir.path()).unwrap();
        assert_eq!((again.spend.class_a, again.spend.class_b), (7, 3));
        // A ledger from a past month is a fresh allowance.
        std::fs::write(
            dir.path().join(SpendLedger::FILE),
            r#"{"month":"1999-01","class_a":999999,"class_b":5}"#,
        )
        .unwrap();
        let rolled = SpendLedger::open(dir.path()).unwrap();
        assert_eq!(rolled.spend.class_a, 0);
        assert_eq!(rolled.spend.month, current_month());
        // A corrupt one is refused, not read as zero.
        std::fs::write(dir.path().join(SpendLedger::FILE), "{not json").unwrap();
        assert!(SpendLedger::open(dir.path()).is_err());
    }

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
        assert_eq!(
            projected_monthly_class_a(10, DEFAULT_POLL_INTERVAL_SECS),
            86_400
        );
    }

    /// The WHOLE table published in `docs/WATCHING_OBJECT_STORAGE.md` and in the
    /// answers article, cell by cell. The two shorter tests above pin the
    /// headline figures; this one makes it impossible to edit a cell in either
    /// document without the suite noticing.
    #[test]
    fn every_cell_of_the_published_cost_table_is_reproduced_here() {
        // (objects, [5s, 60s, 300s, 3600s])
        let table: [(u64, [u64; 4]); 5] = [
            (0, [518_400, 43_200, 8_640, 720]),
            (1_000, [518_400, 43_200, 8_640, 720]),
            (10_000, [5_184_000, 432_000, 86_400, 7_200]),
            (100_000, [51_840_000, 4_320_000, 864_000, 72_000]),
            (1_000_000, [518_400_000, 43_200_000, 8_640_000, 720_000]),
        ];
        for (objects, expected) in table {
            let calls = list_calls_for_keys(objects);
            for (interval, want) in [5u64, 60, 300, 3600].into_iter().zip(expected) {
                assert_eq!(
                    projected_monthly_class_a(calls, interval),
                    want,
                    "{objects} objects ({calls} list call(s)/cycle) every {interval}s"
                );
            }
        }
        // The two sentences the documents put in bold, as assertions.
        // "A 5 s poll on an EMPTY bucket spends half the free tier to watch
        // nothing" — 518,400 of 1,000,000, so more than half.
        let empty_at_5s = projected_monthly_class_a(list_calls_for_keys(0), 5);
        assert_eq!(empty_at_5s, 518_400);
        assert!(empty_at_5s * 2 > CLASS_A_FREE_MONTHLY);
        // "A 60 s poll on a 100,000-object bucket is over four times the tier."
        assert!(
            projected_monthly_class_a(list_calls_for_keys(100_000), 60) > 4 * CLASS_A_FREE_MONTHLY
        );
        // And the 1 s poll on a 100,000-object bucket the refusal text names.
        assert_eq!(
            projected_monthly_class_a(list_calls_for_keys(100_000), 1),
            259_200_000
        );

        // Every cell above is the ARITHMETIC FLOOR, and both documents now say
        // so (#968 review, m1). A real store may serve more: MinIO served 11
        // calls for 10,000 keys at page size 1,000 — ten full pages, then an
        // empty one — which is the figure the docs publish beside the 86,400.
        assert_eq!(
            projected_monthly_class_a(11, DEFAULT_POLL_INTERVAL_SECS),
            95_040
        );
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
        for (calls, budget) in [
            (1u64, 200_000u64),
            (10, 200_000),
            (100, 1_000_000),
            (1_000, 1_000_000),
        ] {
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
        assert_eq!(cycles_per_month_millis(0), u64::MAX);
    }

    /// A sub-second interval must be PRICED, not reported as unmeasurable. With
    /// whole-second math `Duration::as_secs()` truncated 10 ms to 0 s, which
    /// projected as u64::MAX — so the guard refused it for the wrong reason and
    /// no test could use a fast interval without turning the guard off.
    #[test]
    fn a_sub_second_interval_is_priced_rather_than_treated_as_zero() {
        // 10 ms: 2,592,000,000 ms in a month / 10 = 259,200,000 cycles.
        assert_eq!(cycles_per_month_millis(10), 259_200_000);
        assert_eq!(projected_monthly_class_a_millis(1, 10), 259_200_000);
        assert_ne!(projected_monthly_class_a_millis(1, 10), u64::MAX);
        // And it agrees with the whole-second path at whole seconds.
        for secs in [1u64, 5, 60, 300, 3600] {
            assert_eq!(
                cycles_per_month_millis(secs * 1_000),
                cycles_per_month(secs),
                "{secs}s"
            );
        }
        // The operator-facing seconds field rounds down, and the arithmetic does
        // not: a 1,500 ms interval reads as "1s" but is priced at 1.5 s.
        let p = Projection::from_millis(1, 1, 1_500, CLASS_A_FREE_MONTHLY);
        assert_eq!(p.interval_secs, 1);
        assert_eq!(p.cycles_per_month, 1_728_000);
        assert_eq!(p.monthly_class_a, 1_728_000);
    }
}
