//! Merge admission against the process memory budget (#1122).
//!
//! An 8 GiB host used to take the 8 GiB AUTO step, so the process budget
//! was the whole machine. A segment-merge storm then allocated a working
//! set measured at 5–9× the input bytes and the process died. These
//! functions size and defer batches from the resolved budget. They do
//! not allocate.
//!
//! A budget whose 95% watermark still covers a historical 512 MiB batch
//! (8× working set plus the 2 GiB memtable floor) keeps that batch and
//! the configured parallelism. That is the 8 GiB tier and above — a
//! 16 GiB host and larger — so those machines merge as they did before.

/// Historical input ceiling for one merge batch. The AUTO path used this
/// whenever `XERJ_MAX_PROCESS_MEMORY_MB` was unset.
pub const HISTORICAL_BATCH_CAP_BYTES: u64 = 512 * 1024 * 1024;

/// Smallest input cap. Below this a tier can stop making progress.
pub const MIN_BATCH_CAP_BYTES: u64 = 32 * 1024 * 1024;

/// Planning multiplier for one batch's heap. Mailbox-shaped merges were
/// measured at 5–9× the input bytes (`merge_parsed` held 191 MB on a
/// 20k-doc index). 8× is the figure a batch must fit under.
pub const MERGE_WORKING_SET_NUMERATOR: u64 = 8;

/// Upper bound on the input bytes of one merge batch.
///
/// `budget_bytes == 0` means the budget is unknown (no governor). That
/// keeps the historical 512 MiB ceiling. A positive budget that can hold
/// the historical batch also keeps 512 MiB. A smaller budget uses
/// `budget / 64`, clamped to [`MIN_BATCH_CAP_BYTES`]..=[`HISTORICAL_BATCH_CAP_BYTES`].
/// A 4 GiB budget — the AUTO cap on an 8 GiB host — is 64 MiB.
pub fn merge_batch_input_cap_bytes(budget_bytes: u64) -> u64 {
    if budget_bytes == 0 || historical_batch_fits(budget_bytes) {
        return HISTORICAL_BATCH_CAP_BYTES;
    }
    (budget_bytes / 64).clamp(MIN_BATCH_CAP_BYTES, HISTORICAL_BATCH_CAP_BYTES)
}

/// `true` when a 512 MiB batch plus the 2 GiB memtable floor still sits
/// under the 95% watermark of `budget_bytes`.
fn historical_batch_fits(budget_bytes: u64) -> bool {
    let historical = HISTORICAL_BATCH_CAP_BYTES
        .saturating_mul(MERGE_WORKING_SET_NUMERATOR)
        .saturating_add(2 * 1024 * 1024 * 1024);
    budget_bytes.saturating_mul(95) / 100 >= historical
}

/// How many merge batches may run at once.
///
/// A budget that still uses the historical 512 MiB batch returns
/// `configured` unchanged (at least 1). A smaller budget limits
/// concurrency so the batches' estimated working sets share a quarter
/// of the budget, and never drops below one.
pub fn merge_parallelism_under_budget(configured: usize, budget_bytes: u64) -> usize {
    let configured = configured.max(1);
    let cap = merge_batch_input_cap_bytes(budget_bytes);
    if cap >= HISTORICAL_BATCH_CAP_BYTES {
        return configured;
    }
    let per = cap.saturating_mul(MERGE_WORKING_SET_NUMERATOR).max(1);
    let slots = usize::try_from((budget_bytes / 4) / per).unwrap_or(usize::MAX);
    configured.min(slots.max(1))
}

/// Whether one batch may start, ignoring the one-batch drain exception.
///
/// `rss_bytes == 0` means RSS is unknown. `watermark_bytes == 0` means
/// the RSS check is off. Either one starts the batch: a guess must not
/// stall merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeBatchDecision {
    Start,
    Defer,
}

pub fn decide_merge_batch(
    input_bytes: u64,
    rss_bytes: u64,
    watermark_bytes: u64,
) -> MergeBatchDecision {
    if watermark_bytes == 0 || rss_bytes == 0 {
        return MergeBatchDecision::Start;
    }
    let estimate = input_bytes.saturating_mul(MERGE_WORKING_SET_NUMERATOR);
    if rss_bytes.saturating_add(estimate) >= watermark_bytes {
        MergeBatchDecision::Defer
    } else {
        MergeBatchDecision::Start
    }
}

/// Which of `input_bytes` (one entry per selected batch, policy order)
/// starts in this pass.
///
/// On a budget that still uses the 512 MiB batch, every entry starts.
/// On a smaller budget, a batch whose estimated working set would cross
/// the watermark waits for the next check. The first batch still starts:
/// its input is already capped, and a merge is what retires the
/// per-segment caches that hold RSS up. Zero batches would leave that
/// RSS in place.
pub fn throttle_merge_batches(
    input_bytes: &[u64],
    budget_bytes: u64,
    rss_bytes: u64,
    watermark_bytes: u64,
) -> Vec<bool> {
    let small = merge_batch_input_cap_bytes(budget_bytes) < HISTORICAL_BATCH_CAP_BYTES;
    let mut start = Vec::with_capacity(input_bytes.len());
    let mut started = 0usize;
    for bytes in input_bytes {
        if !small {
            start.push(true);
            continue;
        }
        let fits =
            decide_merge_batch(*bytes, rss_bytes, watermark_bytes) == MergeBatchDecision::Start;
        if fits || started == 0 {
            start.push(true);
            started += 1;
        } else {
            start.push(false);
        }
    }
    start
}

/// Estimated peak working set of a merge batch whose inputs total
/// `input_bytes`: the same `input * MERGE_WORKING_SET_NUMERATOR` that
/// [`decide_merge_batch`] uses.
pub fn merge_working_set_bytes(input_bytes: u64) -> u64 {
    input_bytes.saturating_mul(MERGE_WORKING_SET_NUMERATOR)
}

/// Reservation multiplier on a batch's whole on-disk footprint (#2427).
///
/// `SegmentMeta::size_bytes`, which the planner and
/// [`MERGE_WORKING_SET_NUMERATOR`] use, is the segment file alone. A
/// merge also reads every input's FTS, doc-values and ids side-cars
/// (about 60 files per segment on the autoindex corpora), so the planned
/// input under-counts what the batch decodes. On the reduced #2427 repro
/// (1,024 MB cap, `docs` corpus) a 9-input batch (13 MB on disk) ran
/// beside an 8-input batch (23 MB on disk) with only 24 MB reserved in
/// the ledger, and RSS went from about 740 MB to a 1,503 MB peak: about
/// 21x the two batches' combined on-disk bytes. The 3 GB gate analysis
/// puts a merge's working set at about 30x its on-disk input. 32x keeps
/// the reservation at or above both.
pub const MERGE_ON_DISK_WORKING_SET_NUMERATOR: u64 = 32;

/// Bytes one merge batch reserves against the watermark (#2427): the
/// larger of the planned-input estimate ([`merge_working_set_bytes`]) and
/// [`MERGE_ON_DISK_WORKING_SET_NUMERATOR`] × the inputs' on-disk bytes,
/// side-cars included. `on_disk_bytes == 0` (unknown) keeps the planned
/// estimate.
pub fn merge_reservation_bytes(planned_input_bytes: u64, on_disk_bytes: u64) -> u64 {
    merge_working_set_bytes(planned_input_bytes)
        .max(on_disk_bytes.saturating_mul(MERGE_ON_DISK_WORKING_SET_NUMERATOR))
}

/// Process-wide merge admission against the RSS watermark (#2427).
///
/// [`throttle_merge_batches`] plans one index's pass from an RSS snapshot
/// and always lets that index's first batch start. With hundreds of indices
/// (autoindex) every index started one, and none of them saw the others'
/// working sets until they had allocated. This is checked under the
/// governor's merge ledger at launch time instead:
///
/// * `watermark == 0` (breaker off) or `observed_rss == 0` (no probe):
///   start.
/// * No merge in flight anywhere in the process:
///   * a `forced` merge (`_forcemerge`) starts, engaged or not; it is a
///     deliberate operator request and it waits on its own;
///   * a background merge does not start while the breaker is engaged;
///   * otherwise a background merge starts only if its reservation fits:
///     `observed_rss + request` stays under the watermark. This is the
///     headroom check the lone merge used to skip (#2427, 1 GB gate on
///     `0eaa8af9`: the ingest node launched lone batches whose reservation
///     was larger than the room left and peaked at 3.5 GB against a
///     3,072 MB cap). Progress is still guaranteed:
///     `headroom_wait_expired` admits the batch once the governor has held
///     lone merges back for [`MERGE_HEADROOM_MAX_WAIT`], and in the
///     meantime the governor shrinks the next batches to the headroom
///     ([`pressure_cap_for_headroom`]) so a smaller batch usually fits
///     long before that.
/// * Breaker engaged: defer. Background merges are the one memory consumer
///   the node can postpone, so they wait for the drain.
/// * Otherwise start only if `observed_rss + reserved + request` stays under
///   the watermark.
#[allow(clippy::too_many_arguments)]
pub fn decide_merge_admission(
    watermark: u64,
    observed_rss: u64,
    reserved: u64,
    in_flight: u64,
    breaker_engaged: bool,
    request: u64,
    forced: bool,
    headroom_wait_expired: bool,
) -> bool {
    if watermark == 0 || observed_rss == 0 {
        return true;
    }
    if in_flight == 0 {
        if forced {
            return true;
        }
        if breaker_engaged {
            return false;
        }
        return headroom_wait_expired || observed_rss.saturating_add(request) < watermark;
    }
    if breaker_engaged {
        return false;
    }
    observed_rss
        .saturating_add(reserved)
        .saturating_add(request)
        < watermark
}

/// The adaptive batch input cap after a lone background batch was held
/// back because its reservation did not fit the headroom (#2427).
///
/// `input_bytes` is the batch's planned input and `request` the bytes it
/// asked to reserve ([`merge_reservation_bytes`]); `headroom` is what is
/// left under the merge-pool ceiling. The next batches are planned at the
/// same input-to-reservation ratio scaled down to the headroom, so the
/// planner splits the work into batches that fit instead of retrying the
/// one that did not. `current == 0` means no adaptive cap yet. Never below
/// 1; the planner still builds a minimum-count batch under any cap, and
/// [`decide_merge_admission`]'s bounded wait lets that one through.
pub fn pressure_cap_for_headroom(
    current: u64,
    input_bytes: u64,
    request: u64,
    headroom: u64,
) -> u64 {
    let scaled = (u128::from(input_bytes) * u128::from(headroom) / u128::from(request.max(1)))
        .min(u128::from(u64::MAX)) as u64;
    // Leave a little room: a batch planned exactly at the headroom would
    // still be refused by the strict `<` check.
    let target = (scaled - scaled / 8).max(1);
    if current == 0 {
        target
    } else {
        current.min(target)
    }
}

/// Whether an in-flight background merge batch must stop (#2427).
///
/// Admission checks a batch against `input * MERGE_WORKING_SET_NUMERATOR`
/// before it starts. On the 1 GB gate corpus that estimate was several
/// times too small: JSON-heavy segments compress 10–20×, and a merge holds
/// the decoded survivors plus a parsed `Value` per document. One admitted
/// batch took a 3,072 MB node to 8,122 MB in 18 s while the breaker was
/// engaged and writes were already getting 429s. Nothing else could free
/// that memory, because the merge was the thing allocating it.
///
/// A batch stops when the breaker is engaged and RSS has reached the full
/// process limit (past the 95% watermark). A stopped batch commits nothing
/// and keeps its inputs, like any aborted merge. `forced` (`_forcemerge`)
/// batches are not stopped. `limit == 0` or `rss == 0` (unknown) never
/// stops a batch.
pub fn merge_must_yield(rss: u64, limit: u64, breaker_engaged: bool, forced: bool) -> bool {
    !forced && breaker_engaged && limit > 0 && rss > 0 && rss >= limit
}

/// Docs between two governor yield checks inside a merge batch's survivor
/// scan and drain (#2427).
///
/// The checks used to run once per input segment and every 8,192 drained
/// docs. On the 1 GB gate a pg-docs batch (7 inputs, about 90k docs) took
/// the ingest node from the watermark (2,918 MB) to 3,100-3,440 MB between
/// two checks. A check is one atomic load and one `/proc/self/statm` read.
pub const MERGE_YIELD_CHECKPOINT_DOCS: u64 = 1024;

/// Longest a lone background merge waits for RSS to come back under the
/// merge-pool ceiling before it starts anyway (#2427). See
/// [`lone_merge_waits_for_headroom`].
pub const MERGE_HEADROOM_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether a background merge that would run alone must wait for headroom
/// before it starts (#2427 ingest overshoot).
///
/// [`decide_merge_admission`] always starts a lone merge unless the breaker
/// is engaged, so merging makes progress. On the 1 GB gate the ingest node
/// launched the next pg-docs batch the moment the previous one finished,
/// with RSS at 2,400-2,900 MB, just under the 2,918 MB watermark. Each batch
/// grew RSS by 400-900 MB, and every one of the run's over-cap samples
/// (3,100-3,502 MB against 3,072 MB) is one of those batches.
///
/// A lone background batch now waits while `observed_rss + request` is at
/// or above `ceiling` (the merge-pool ceiling), so it starts after the
/// drain, with the batch's growth room under the cap. `request` is the
/// batch's reservation; `0` checks RSS alone. Including it closes the gap
/// where RSS sat just under the ceiling and a batch whose reservation was
/// larger than the room left still started alone. The wait is bounded by `max_wait`
/// (`waited` is how long lone merges have been held back), so a node whose
/// live set sits above the ceiling still compacts. `_forcemerge`, a merge
/// beside others (already checked against the ledger), and an unknown RSS
/// or ceiling never wait.
pub fn lone_merge_waits_for_headroom(
    observed_rss: u64,
    ceiling: u64,
    request: u64,
    in_flight: u64,
    forced: bool,
    waited: std::time::Duration,
    max_wait: std::time::Duration,
) -> bool {
    !forced
        && in_flight == 0
        && ceiling > 0
        && observed_rss > 0
        && observed_rss.saturating_add(request) >= ceiling
        && waited < max_wait
}

/// Consecutive background yields after which a small batch stops yielding.
/// See [`merge_must_yield_after`].
pub const MERGE_YIELD_STREAK_LIMIT: u64 = 3;

/// Largest batch input exempt from yielding once the streak is reached.
/// The batch that took a node 5 GB past its cap was hundreds of MB. The
/// livelocked one in the gate was 49 MB.
pub const MERGE_YIELD_STREAK_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// [`merge_must_yield`] with a bound on how long compaction can stall.
///
/// On a search node, the search working set alone can hold RSS at the
/// process limit while the breaker cycles between engaged and released.
/// Every admitted batch then hits the limit and yields. Halving the
/// pressure cap does not help once the batch is already at its minimum
/// size, so the planner rebuilds the same inputs. In the Run 5 gate, one
/// 49 MB batch yielded 68 times in 14 minutes and nothing merged. Segments
/// piled up, and each one costs a per-segment open on every search, which
/// holds RSS at the limit.
///
/// After [`MERGE_YIELD_STREAK_LIMIT`] background yields in a row with no
/// batch completing in between, a batch of at most
/// [`MERGE_YIELD_STREAK_MAX_INPUT_BYTES`] runs to completion. Admission
/// already lets only one merge start while the process is at the
/// watermark, and larger batches still yield as before.
pub fn merge_must_yield_after(
    rss: u64,
    limit: u64,
    breaker_engaged: bool,
    forced: bool,
    input_bytes: u64,
    yield_streak: u64,
) -> bool {
    if !merge_must_yield(rss, limit, breaker_engaged, forced) {
        return false;
    }
    !(yield_streak >= MERGE_YIELD_STREAK_LIMIT
        && input_bytes > 0
        && input_bytes <= MERGE_YIELD_STREAK_MAX_INPUT_BYTES)
}

/// The adaptive batch input cap after a batch of `input_bytes` stopped
/// under memory pressure (#2427). `current == 0` means no adaptive cap yet.
/// The next batch may use at most half of what did not fit. Never below 1.
pub fn pressure_cap_after_yield(current: u64, input_bytes: u64) -> u64 {
    let halved = (input_bytes / 2).max(1);
    if current == 0 {
        halved
    } else {
        current.min(halved)
    }
}

/// The adaptive batch input cap after a background batch of `input_bytes`
/// completed (#2427). A batch that used at least half of the cap doubles
/// it. Once the cap reaches [`HISTORICAL_BATCH_CAP_BYTES`] it is removed
/// (`0`), and the static cap applies again.
pub fn pressure_cap_after_success(current: u64, input_bytes: u64) -> u64 {
    if current == 0 {
        return 0;
    }
    if input_bytes.saturating_mul(2) < current {
        return current;
    }
    let doubled = current.saturating_mul(2);
    if doubled >= HISTORICAL_BATCH_CAP_BYTES {
        0
    } else {
        doubled
    }
}

/// The batch input cap to plan with: the static cap, narrowed by the
/// adaptive pressure cap when one is set. A static cap of `0` (the
/// `XERJ_MERGE_BATCH_MAX_INPUT_MB=0` opt-out) still takes a pressure cap.
pub fn effective_batch_cap_bytes(static_cap: u64, pressure_cap: u64) -> u64 {
    match (static_cap, pressure_cap) {
        (s, 0) => s,
        (0, p) => p,
        (s, p) => s.min(p),
    }
}

/// Multiplier on the merged stored JSON for the merge output-write step
/// (#2427).
///
/// After the last yield checkpoint, a batch builds its doc-values columns
/// from the merged JSON and then runs the stored encode
/// (`encode_stored_v2_*_at_level`). The encode parses the whole JSON array
/// into a `Vec<serde_json::Value>` (about 4–5× the JSON bytes in maps,
/// strings and per-value overhead), then builds per-column buffers (about
/// 1×) and the compressed output. All of that sits on top of the merged
/// buffer, which is still alive. On the 3,072 MB search node, the large
/// pg-docs merges (29k–166k docs) peaked at 4.4–4.9 GB about 5 s before
/// they completed, which is this step. Neither admission nor the yield
/// checks charged it.
pub const MERGE_WRITE_JSON_NUMERATOR: u64 = 6;

/// Multiplier on the FTS input for the output's FTS side-car build
/// (#2427). The rebuild path re-analyses `fts_input` into in-memory
/// posting lists. The replay path remaps the inputs' decoded postings,
/// whose retained bytes bound it. In both cases about 2× covers the
/// writer's buffers plus its encoded output.
pub const MERGE_WRITE_FTS_NUMERATOR: u64 = 2;

/// Extra bytes the merge output-write step allocates beyond what the
/// batch already holds at its last yield checkpoint (#2427).
///
/// `json_bytes` is the merged stored JSON. `fts_bytes` is what the FTS
/// side-car build works from: `fts_input`'s bytes on the rebuild path, or
/// the replay readers' retained bytes. The stored encode and the FTS build
/// run one after the other, and the encode's parsed copy is dropped before
/// the FTS build starts, so the step's peak is the larger of the two, not
/// their sum. Saturates instead of wrapping.
pub fn merge_write_phase_bytes(json_bytes: u64, fts_bytes: u64) -> u64 {
    json_bytes
        .saturating_mul(MERGE_WRITE_JSON_NUMERATOR)
        .max(fts_bytes.saturating_mul(MERGE_WRITE_FTS_NUMERATOR))
}

/// Whether a merge batch may run its output-write step (#2427).
///
/// The step comes after the last yield checkpoint, and once it starts it
/// cannot stop without throwing the batch away, so the check runs before
/// it starts. `extra` is [`merge_write_phase_bytes`], and `observed_rss`
/// already includes what the batch holds. The step is refused when
/// `observed_rss + extra` would reach the process `limit` (the hard limit,
/// not the watermark). A refused batch yields like any other: no output,
/// inputs kept, and later batches narrowed.
///
/// These always run:
/// * `forced` batches (`_forcemerge`), which are never stopped;
/// * `exempt` batches. This is for the #2468 rule: once a background
///   batch has been allowed to run to completion after a streak of yields
///   with nothing merged, refusing its write would bring back the stall
///   that rule exists to break;
/// * batches where the limit or RSS is unknown (`0`).
pub fn decide_merge_write(
    observed_rss: u64,
    limit: u64,
    extra: u64,
    forced: bool,
    exempt: bool,
) -> bool {
    if forced || exempt || limit == 0 || observed_rss == 0 {
        return true;
    }
    observed_rss.saturating_add(extra) < limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_background_merge_yields_only_when_engaged_at_the_limit() {
        const MIB: u64 = 1024 * 1024;
        let limit = 1024 * MIB;
        assert!(merge_must_yield(1024 * MIB, limit, true, false));
        assert!(merge_must_yield(1600 * MIB, limit, true, false));
        // Watermark band (95%..100%) and below: keep going.
        assert!(!merge_must_yield(1000 * MIB, limit, true, false));
        // Not engaged: keep going (the sampler will engage first).
        assert!(!merge_must_yield(1600 * MIB, limit, false, false));
        // `_forcemerge` is never stopped.
        assert!(!merge_must_yield(1600 * MIB, limit, true, true));
        // Unknown limit or RSS.
        assert!(!merge_must_yield(1600 * MIB, 0, true, false));
        assert!(!merge_must_yield(0, limit, true, false));
    }

    #[test]
    fn a_small_batch_stops_yielding_after_a_streak() {
        const MIB: u64 = 1024 * 1024;
        let limit = 1024 * MIB;
        let rss = 1100 * MIB;
        // Below the streak: identical to `merge_must_yield`.
        for streak in 0..MERGE_YIELD_STREAK_LIMIT {
            assert!(merge_must_yield_after(
                rss,
                limit,
                true,
                false,
                49 * MIB,
                streak
            ));
        }
        // At the streak: a small batch runs to completion.
        assert!(!merge_must_yield_after(
            rss,
            limit,
            true,
            false,
            49 * MIB,
            MERGE_YIELD_STREAK_LIMIT
        ));
        assert!(!merge_must_yield_after(
            rss,
            limit,
            true,
            false,
            MERGE_YIELD_STREAK_MAX_INPUT_BYTES,
            MERGE_YIELD_STREAK_LIMIT + 10
        ));
        // A large batch keeps yielding however long the streak.
        assert!(merge_must_yield_after(
            rss,
            limit,
            true,
            false,
            MERGE_YIELD_STREAK_MAX_INPUT_BYTES + 1,
            MERGE_YIELD_STREAK_LIMIT + 10
        ));
        // Unknown size keeps the old rule.
        assert!(merge_must_yield_after(rss, limit, true, false, 0, 99));
        // Nothing yields that `merge_must_yield` would not yield.
        assert!(!merge_must_yield_after(
            1000 * MIB,
            limit,
            true,
            false,
            49 * MIB,
            0
        ));
        assert!(!merge_must_yield_after(
            rss,
            limit,
            false,
            false,
            49 * MIB,
            0
        ));
        assert!(!merge_must_yield_after(rss, limit, true, true, 99 * MIB, 0));
    }

    #[test]
    fn the_pressure_cap_halves_on_yield_and_recovers_on_success() {
        const MIB: u64 = 1024 * 1024;
        let cap = pressure_cap_after_yield(0, 66 * MIB);
        assert_eq!(cap, 33 * MIB);
        // A second, larger yield never widens it.
        assert_eq!(pressure_cap_after_yield(cap, 200 * MIB), 33 * MIB);
        let cap = pressure_cap_after_yield(cap, 30 * MIB);
        assert_eq!(cap, 15 * MIB);
        // A tiny batch's success does not widen it.
        assert_eq!(pressure_cap_after_success(cap, MIB), cap);
        // Batches that use the cap double it until it is removed.
        let mut cap = cap;
        let mut rounds = 0;
        while cap != 0 {
            cap = pressure_cap_after_success(cap, cap);
            rounds += 1;
            assert!(rounds < 16);
        }
        assert_eq!(pressure_cap_after_success(0, 512 * MIB), 0);
        assert_eq!(pressure_cap_after_yield(0, 0), 1);
    }

    #[test]
    fn the_pressure_cap_narrows_the_static_cap() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(effective_batch_cap_bytes(32 * MIB, 0), 32 * MIB);
        assert_eq!(effective_batch_cap_bytes(32 * MIB, 8 * MIB), 8 * MIB);
        assert_eq!(effective_batch_cap_bytes(32 * MIB, 64 * MIB), 32 * MIB);
        assert_eq!(effective_batch_cap_bytes(0, 8 * MIB), 8 * MIB);
        assert_eq!(effective_batch_cap_bytes(0, 0), 0);
    }

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn merges_are_throttled_under_a_small_budget() {
        // AUTO cap on an 8 GiB host: half of RAM, the setting that kept
        // the Windows 10 field report up (`XERJ_MAX_PROCESS_MEMORY_MB=4096`).
        let budget = 4 * GIB;
        let cap = merge_batch_input_cap_bytes(budget);
        assert_eq!(
            cap,
            64 * MIB,
            "4 GiB / 64 = 64 MiB, not the 512 MiB default"
        );
        assert_eq!(
            merge_parallelism_under_budget(4, budget),
            2,
            "four workers would hold 4 × 512 MiB; the budget allows two"
        );

        let watermark = budget * 95 / 100;
        // RSS already within 32 MiB of the watermark. One capped batch
        // still runs (it retires segments). The other two wait.
        let rss = watermark - 32 * MIB;
        let plan = throttle_merge_batches(&[cap, cap, cap], budget, rss, watermark);
        assert_eq!(plan, vec![true, false, false]);

        // The same batches with headroom all start. Parallelism is a
        // separate bound, applied by the caller.
        let open = throttle_merge_batches(&[cap, cap, cap], budget, 256 * MIB, watermark);
        assert_eq!(open, vec![true, true, true]);

        // Unknown RSS does not stall the drain.
        assert_eq!(
            decide_merge_batch(cap, 0, watermark),
            MergeBatchDecision::Start
        );
        assert_eq!(decide_merge_batch(cap, rss, 0), MergeBatchDecision::Start);
    }

    #[test]
    fn a_normal_host_keeps_the_historical_batch_and_parallelism() {
        // The 8 GiB tier (16 GiB of RAM and up, under 64 GiB) and the
        // 16 / 32 GiB tiers all cover a 512 MiB batch under the watermark.
        for budget in [8 * GIB, 16 * GIB, 32 * GIB] {
            assert_eq!(
                merge_batch_input_cap_bytes(budget),
                HISTORICAL_BATCH_CAP_BYTES
            );
            assert_eq!(merge_parallelism_under_budget(4, budget), 4);
            let watermark = budget * 95 / 100;
            let plan = throttle_merge_batches(
                &[HISTORICAL_BATCH_CAP_BYTES; 3],
                budget,
                watermark,
                watermark,
            );
            assert_eq!(
                plan,
                vec![true, true, true],
                "budget {budget} must not defer a historical batch"
            );
        }
        // No governor: the pre-change fallback.
        assert_eq!(merge_batch_input_cap_bytes(0), HISTORICAL_BATCH_CAP_BYTES);
        assert_eq!(merge_parallelism_under_budget(1, 0), 1);
    }

    #[test]
    fn the_batch_cap_floors_at_thirty_two_mebibytes() {
        assert_eq!(merge_batch_input_cap_bytes(GIB), MIN_BATCH_CAP_BYTES);
        assert_eq!(merge_batch_input_cap_bytes(2 * GIB), MIN_BATCH_CAP_BYTES);
    }

    #[test]
    fn merge_admission_is_open_when_the_breaker_is_off_or_rss_is_unknown() {
        assert!(decide_merge_admission(
            0,
            10_000 * MIB,
            0,
            5,
            true,
            4_000 * MIB,
            false,
            false
        ));
        assert!(decide_merge_admission(
            2_850 * MIB,
            0,
            0,
            5,
            true,
            4_000 * MIB,
            false,
            false
        ));
    }

    #[test]
    fn a_lone_background_merge_waits_for_headroom_under_the_ceiling() {
        use std::time::Duration;
        let ceiling = 2_458 * MIB;
        let max = MERGE_HEADROOM_MAX_WAIT;
        // The gate's case: the next batch right after the last one, at
        // 2,700 MB, with nothing else merging. It waits.
        assert!(lone_merge_waits_for_headroom(
            2_700 * MIB,
            ceiling,
            0,
            0,
            false,
            Duration::ZERO,
            max
        ));
        assert!(lone_merge_waits_for_headroom(
            ceiling,
            ceiling,
            0,
            0,
            false,
            max - Duration::from_millis(1),
            max
        ));
        // After the drain it starts.
        assert!(!lone_merge_waits_for_headroom(
            ceiling - 1,
            ceiling,
            0,
            0,
            false,
            Duration::ZERO,
            max
        ));
        // The wait is bounded, so a node whose live set sits above the
        // ceiling still compacts.
        assert!(!lone_merge_waits_for_headroom(
            2_700 * MIB,
            ceiling,
            0,
            0,
            false,
            max,
            max
        ));
        // `_forcemerge`, a merge beside others (the ledger check covers it),
        // and unknown RSS or ceiling never wait here.
        assert!(!lone_merge_waits_for_headroom(
            2_700 * MIB,
            ceiling,
            0,
            0,
            true,
            Duration::ZERO,
            max
        ));
        assert!(!lone_merge_waits_for_headroom(
            2_700 * MIB,
            ceiling,
            0,
            1,
            false,
            Duration::ZERO,
            max
        ));
        assert!(!lone_merge_waits_for_headroom(
            0,
            ceiling,
            0,
            0,
            false,
            Duration::ZERO,
            max
        ));
        assert!(!lone_merge_waits_for_headroom(
            2_700 * MIB,
            0,
            0,
            0,
            false,
            Duration::ZERO,
            max
        ));
    }

    #[test]
    fn a_lone_merge_needs_headroom_but_always_makes_progress() {
        let wm = 2_850 * MIB;
        // #2427 1 GB gate: a lone 400 MiB reservation at 2,700 MiB RSS
        // would cross the watermark. It used to start because nothing
        // else was merging; now it is held back while the wait runs.
        assert!(!decide_merge_admission(
            wm,
            2_700 * MIB,
            0,
            0,
            false,
            400 * MIB,
            false,
            false
        ));
        // A batch that fits the headroom starts alone as before.
        assert!(decide_merge_admission(
            wm,
            2_700 * MIB,
            0,
            0,
            false,
            100 * MIB,
            false,
            false
        ));
        // Progress: once the bounded wait has expired the lone batch
        // starts even though it does not fit.
        assert!(decide_merge_admission(
            wm,
            2_700 * MIB,
            0,
            0,
            false,
            400 * MIB,
            false,
            true
        ));
        // `_forcemerge` is never held back by the headroom check.
        assert!(decide_merge_admission(
            wm,
            2_700 * MIB,
            0,
            0,
            false,
            400 * MIB,
            true,
            false
        ));
        // The expired wait does not override an engaged breaker.
        assert!(!decide_merge_admission(
            wm,
            2_900 * MIB,
            0,
            0,
            true,
            8 * MIB,
            false,
            true
        ));
    }

    #[test]
    fn a_held_back_lone_merge_shrinks_the_next_batches_to_the_headroom() {
        // 50 MiB of planned input asked for 400 MiB; 150 MiB is left.
        let cap = pressure_cap_for_headroom(0, 50 * MIB, 400 * MIB, 150 * MIB);
        // Scaled to the headroom (18.75 MiB) less an eighth for the strict
        // check: the next batch's reservation fits.
        assert!(cap < 50 * MIB * 150 / 400);
        assert!(cap * 400 / 50 < 150 * MIB);
        assert!(cap > 0);
        // An existing tighter cap is kept; a looser one is narrowed.
        assert_eq!(
            pressure_cap_for_headroom(MIB, 50 * MIB, 400 * MIB, 150 * MIB),
            MIB
        );
        assert_eq!(
            pressure_cap_for_headroom(512 * MIB, 50 * MIB, 400 * MIB, 150 * MIB),
            cap
        );
        // No headroom or an unknown request never yields a zero cap.
        assert_eq!(pressure_cap_for_headroom(0, 50 * MIB, 400 * MIB, 0), 1);
        assert!(pressure_cap_for_headroom(0, 50 * MIB, 0, 150 * MIB) > 0);
    }

    #[test]
    fn a_lone_merge_waits_when_its_reservation_does_not_fit_under_the_ceiling() {
        use std::time::Duration;
        let ceiling = 2_458 * MIB;
        let max = MERGE_HEADROOM_MAX_WAIT;
        // RSS just under the ceiling, but the batch would take it over.
        assert!(lone_merge_waits_for_headroom(
            2_400 * MIB,
            ceiling,
            400 * MIB,
            0,
            false,
            Duration::ZERO,
            max
        ));
        // The same batch with room for it starts.
        assert!(!lone_merge_waits_for_headroom(
            1_900 * MIB,
            ceiling,
            400 * MIB,
            0,
            false,
            Duration::ZERO,
            max
        ));
        // Bounded.
        assert!(!lone_merge_waits_for_headroom(
            2_400 * MIB,
            ceiling,
            400 * MIB,
            0,
            false,
            max,
            max
        ));
    }

    #[test]
    fn merge_admission_counts_other_in_flight_reservations() {
        let wm = 2_850 * MIB;
        // One 47 MiB batch (376 MiB working set) already reserved.
        let reserved = merge_working_set_bytes(47 * MIB);
        assert!(decide_merge_admission(
            wm,
            2_000 * MIB,
            reserved,
            1,
            false,
            reserved,
            false,
            false
        ));
        // At 2,200 MiB RSS a second one would cross the watermark.
        assert!(!decide_merge_admission(
            wm,
            2_200 * MIB,
            reserved,
            1,
            false,
            reserved,
            false,
            false
        ));
        // And a third does not fit at all once two are reserved.
        assert!(!decide_merge_admission(
            wm,
            2_000 * MIB,
            2 * reserved,
            2,
            false,
            reserved,
            false,
            false
        ));
    }

    #[test]
    fn merge_admission_defers_background_merges_while_engaged() {
        let wm = 2_850 * MIB;
        // Engaged: no background merge starts, not even the first.
        assert!(!decide_merge_admission(
            wm,
            2_900 * MIB,
            0,
            0,
            true,
            8 * MIB,
            false,
            false
        ));
        assert!(!decide_merge_admission(
            wm,
            1_000 * MIB,
            0,
            1,
            true,
            8 * MIB,
            false,
            false
        ));
        // A forced merge may run alone while engaged, but not beside another.
        assert!(decide_merge_admission(
            wm,
            2_900 * MIB,
            0,
            0,
            true,
            8 * MIB,
            true,
            false
        ));
        assert!(!decide_merge_admission(
            wm,
            2_900 * MIB,
            64 * MIB,
            1,
            true,
            8 * MIB,
            true,
            false
        ));
    }

    #[test]
    fn merge_working_set_matches_the_batch_decision() {
        assert_eq!(
            merge_working_set_bytes(10),
            10 * MERGE_WORKING_SET_NUMERATOR
        );
        assert_eq!(merge_working_set_bytes(u64::MAX), u64::MAX);
    }

    /// #2427: the reservation follows the on-disk footprint, side-cars
    /// included, when that is the larger estimate.
    #[test]
    fn merge_reservation_counts_the_on_disk_footprint() {
        const MIB: u64 = 1024 * 1024;
        // Unknown footprint: the planned estimate, unchanged.
        assert_eq!(
            merge_reservation_bytes(3 * MIB, 0),
            merge_working_set_bytes(3 * MIB)
        );
        // A repro-sized batch: 3 MB planned, 23 MB on disk. 8x planned is
        // 24 MB; the footprint estimate covers the ~700 MB RSS rise measured.
        let reserved = merge_reservation_bytes(3 * MIB, 23 * MIB);
        assert_eq!(reserved, 23 * MIB * MERGE_ON_DISK_WORKING_SET_NUMERATOR);
        assert!(reserved >= 700 * MIB);
        // A footprint smaller than the planned estimate never lowers it.
        assert_eq!(
            merge_reservation_bytes(64 * MIB, MIB),
            merge_working_set_bytes(64 * MIB)
        );
        // Saturates instead of wrapping.
        assert_eq!(merge_reservation_bytes(0, u64::MAX), u64::MAX);
    }

    #[test]
    fn the_write_phase_charges_the_larger_of_encode_and_fts() {
        const MIB: u64 = 1024 * 1024;
        // Encode dominates on a JSON-heavy batch.
        assert_eq!(
            merge_write_phase_bytes(300 * MIB, 100 * MIB),
            300 * MIB * MERGE_WRITE_JSON_NUMERATOR
        );
        // The FTS build dominates when its input is the larger one.
        assert_eq!(
            merge_write_phase_bytes(10 * MIB, 100 * MIB),
            100 * MIB * MERGE_WRITE_FTS_NUMERATOR
        );
        assert_eq!(merge_write_phase_bytes(0, 0), 0);
        assert_eq!(merge_write_phase_bytes(u64::MAX, 1), u64::MAX);
        assert_eq!(merge_write_phase_bytes(1, u64::MAX), u64::MAX);
    }

    /// The search-node shape from #2427: a 3,072 MB limit, RSS at about
    /// 2.9 GB, and a large pg-docs batch whose merged JSON is a few hundred
    /// MB. Its write step would take RSS to 4.4–4.9 GB, so it must not start.
    #[test]
    fn a_write_that_would_cross_the_hard_limit_is_refused() {
        const MIB: u64 = 1024 * 1024;
        let limit = 3072 * MIB;
        let rss = 2900 * MIB;
        let extra = merge_write_phase_bytes(300 * MIB, 50 * MIB);
        assert!(rss + extra > 4400 * MIB);
        assert!(!decide_merge_write(rss, limit, extra, false, false));
        // Exactly reaching the limit is refused, just under is allowed.
        assert!(!decide_merge_write(rss, limit, limit - rss, false, false));
        assert!(decide_merge_write(
            rss,
            limit,
            limit - rss - 1,
            false,
            false
        ));
        // A small write with headroom runs, whatever the breaker state.
        assert!(decide_merge_write(
            1000 * MIB,
            limit,
            600 * MIB,
            false,
            false
        ));
        // `_forcemerge`, a #2468 streak-exempt batch, or unknown limit/RSS: runs.
        assert!(decide_merge_write(rss, limit, extra, true, false));
        assert!(decide_merge_write(rss, limit, extra, false, true));
        assert!(decide_merge_write(rss, 0, extra, false, false));
        assert!(decide_merge_write(0, limit, extra, false, false));
        // Saturates instead of wrapping past the limit.
        assert!(!decide_merge_write(rss, limit, u64::MAX, false, false));
    }
}
