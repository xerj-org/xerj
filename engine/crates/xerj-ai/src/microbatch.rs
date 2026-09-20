//! Length-aware batching for transformer inference.
//!
//! Both in-process encoder backends (Candle [`crate::neural`] and the
//! experimental [`crate::onnx`] one) run a *rectangular* tensor: every row in
//! one call is padded to the longest row in that call, and the model pays for
//! the padding exactly as if it were real text. A window that mixes one long
//! chunk with sixty short lines therefore costs `61 × long`, not
//! `long + 60 × short`.
//!
//! [`group_by_padded_cost`] is the shared cure: sort the rows by token length,
//! then cut a batch as soon as adding the next row would break either the row
//! cap or the `rows × padded_sequence_length` budget. Rows of similar length
//! end up together, so padding waste is bounded rather than set by the single
//! longest member of the window, and the token budget bounds the activation
//! memory one call can allocate.
//!
//! The returned groups hold *positions into the input*, so callers must map
//! results back into input order — neither backend may reorder its output.

/// Group row positions into inference batches.
///
/// `token_lengths[i]` is the tokenized length of row `i`. A batch is cut when
/// it already holds `max_rows` rows, or when admitting the next row would push
/// `rows × longest_row` past `padded_token_budget`.
///
/// A single row longer than the whole budget is still emitted, alone: the
/// caller's model has its own truncation limit and refusing the row here would
/// turn an expensive document into a failed one. Callers that want an error
/// instead check the lengths before calling (see [`crate::onnx`]).
///
/// The sort is stable, so equal-length rows keep input order and the plan is
/// deterministic for a given input.
pub fn group_by_padded_cost(
    token_lengths: &[usize],
    max_rows: usize,
    padded_token_budget: usize,
) -> Vec<Vec<usize>> {
    debug_assert!(max_rows > 0 && padded_token_budget > 0);
    let max_rows = max_rows.max(1);
    let padded_token_budget = padded_token_budget.max(1);

    let mut order = (0..token_lengths.len()).collect::<Vec<_>>();
    order.sort_by_key(|&i| token_lengths[i]);

    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut batch: Vec<usize> = Vec::new();
    let mut longest = 0usize;
    for i in order {
        let length = token_lengths[i];
        let next_longest = longest.max(length);
        if !batch.is_empty()
            && (batch.len() >= max_rows
                || next_longest.saturating_mul(batch.len() + 1) > padded_token_budget)
        {
            batches.push(std::mem::take(&mut batch));
            longest = 0;
        }
        longest = longest.max(length);
        batch.push(i);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// Total `rows × longest_row` slots a plan pushes through the model.
///
/// This is the quantity the padding waste lives in: it equals the sum of the
/// real token lengths only when every batch is length-homogeneous.
pub fn padded_token_slots(token_lengths: &[usize], batches: &[Vec<usize>]) -> usize {
    batches
        .iter()
        .map(|batch| {
            let longest = batch
                .iter()
                .map(|&i| token_lengths[i])
                .max()
                .unwrap_or_default();
            longest * batch.len()
        })
        .sum()
}

/// Cut a batching plan into *waves*: runs of consecutive batches that may be
/// in flight at the same time (#964). A wave holds at most `width` passes and
/// at most `padded_token_budget` padded token slots in flight, so the
/// activation-memory bound is the same at every width — `width` passes of
/// `budget / width` slots each cost what one full-budget pass cost.
///
/// Batches stay in plan order (shortest first), so a caller that stops between
/// waves still leaves the *longest* documents unjudged, exactly as the serial
/// loop did; `width = 1` reproduces that loop one batch per wave.
///
/// Returns ranges into `batches`, in order, covering every batch exactly once.
pub fn plan_waves(
    batches: &[Vec<usize>],
    token_lengths: &[usize],
    width: usize,
    padded_token_budget: usize,
) -> Vec<std::ops::Range<usize>> {
    debug_assert!(width > 0 && padded_token_budget > 0);
    let width = width.max(1);
    let mut waves = Vec::new();
    let mut start = 0usize;
    let mut passes = 0usize;
    let mut slots = 0usize;
    for (i, batch) in batches.iter().enumerate() {
        let batch_slots = batch
            .iter()
            .map(|&r| token_lengths[r])
            .max()
            .unwrap_or_default()
            .saturating_mul(batch.len());
        if passes > 0 && (passes == width || slots + batch_slots > padded_token_budget) {
            waves.push(start..i);
            start = i;
            passes = 0;
            slots = 0;
        }
        passes += 1;
        slots += batch_slots;
    }
    if start < batches.len() {
        waves.push(start..batches.len());
    }
    waves
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_position_appears_exactly_once() {
        let lengths = [512, 14, 200, 16, 400, 15, 90, 300];
        let batches = group_by_padded_cost(&lengths, 3, 600);
        let mut positions = batches.iter().flatten().copied().collect::<Vec<_>>();
        positions.sort_unstable();
        assert_eq!(positions, (0..lengths.len()).collect::<Vec<_>>());
    }

    #[test]
    fn batches_respect_the_row_cap_and_the_token_budget() {
        let lengths = [512, 14, 200, 16, 400, 15, 90, 300];
        let batches = group_by_padded_cost(&lengths, 3, 600);
        for batch in &batches {
            assert!(batch.len() <= 3, "row cap");
            let longest = batch.iter().map(|&i| lengths[i]).max().unwrap();
            assert!(
                longest * batch.len() <= 600 || batch.len() == 1,
                "token budget: {longest} × {}",
                batch.len()
            );
        }
    }

    /// The defect this module exists for: one long row in a window of short
    /// ones must not drag the short ones up to its length.
    #[test]
    fn one_long_row_does_not_pad_the_short_rows_up_to_it() {
        let mut lengths = vec![128];
        lengths.resize(64, 30);
        let real: usize = lengths.iter().sum();

        // What a single rectangular call costs: 64 rows × the longest row.
        let unbatched = padded_token_slots(&lengths, &[(0..lengths.len()).collect()]);
        assert_eq!(unbatched, 64 * 128);

        let planned = padded_token_slots(&lengths, &group_by_padded_cost(&lengths, 64, 4096));
        assert_eq!(planned, real, "length-homogeneous groups pad nothing");
        assert!(
            planned * 3 < unbatched,
            "planned={planned} must be far below the rectangular cost {unbatched}"
        );
    }

    #[test]
    fn a_row_longer_than_the_budget_is_emitted_alone() {
        let batches = group_by_padded_cost(&[8, 9_000, 8], 64, 4_096);
        let oversized = batches
            .iter()
            .find(|batch| batch.contains(&1))
            .expect("the oversized row is still planned");
        assert_eq!(oversized.as_slice(), &[1]);
    }

    #[test]
    fn empty_input_plans_nothing() {
        assert!(group_by_padded_cost(&[], 64, 4_096).is_empty());
    }

    #[test]
    fn equal_lengths_keep_input_order() {
        let batches = group_by_padded_cost(&[10, 10, 10, 10], 2, 4_096);
        assert_eq!(batches, vec![vec![0, 1], vec![2, 3]]);
    }

    /// #964: every batch is in exactly one wave, waves cover the plan in
    /// order, and each wave respects both the pass-count and the in-flight
    /// slot bound — the same activation-memory ceiling the serial loop had.
    #[test]
    fn waves_cover_the_plan_in_order_within_both_bounds() {
        let lengths: Vec<usize> = (0..60).map(|i| 120 + (i % 7) * 40).collect();
        let batches = group_by_padded_cost(&lengths, 32, 512);
        assert!(
            batches.len() > 4,
            "need a multi-batch plan, got {batches:?}"
        );
        for width in [1usize, 2, 4, 8, 64] {
            let waves = plan_waves(&batches, &lengths, width, 4_096);
            let mut seen = 0usize;
            for wave in &waves {
                assert!(wave.len() <= width, "{width}-wide wave held {}", wave.len());
                let slots: usize = batches[wave.clone()]
                    .iter()
                    .map(|batch| batch.iter().map(|&r| lengths[r]).max().unwrap() * batch.len())
                    .sum();
                assert!(
                    slots <= 4_096 || wave.len() == 1,
                    "wave of {slots} slots (budget 4096, width {width})"
                );
                assert_eq!(wave.start, seen, "waves must be consecutive");
                seen = wave.end;
            }
            assert_eq!(seen, batches.len(), "every batch in exactly one wave");
        }
    }

    /// #964: `width = 1` is the historical serial loop — one batch per wave,
    /// in plan order, so the pre-fix pass sequence is reproduced exactly.
    #[test]
    fn width_one_is_one_batch_per_wave() {
        let lengths = [512, 14, 200, 16, 400, 15, 90, 300];
        let batches = group_by_padded_cost(&lengths, 3, 600);
        let waves = plan_waves(&batches, &lengths, 1, 600);
        assert_eq!(waves.len(), batches.len());
        for (n, wave) in waves.iter().enumerate() {
            assert_eq!(*wave, n..n + 1);
        }
    }

    /// #964: a wider split fills its waves — the reason the plan's per-pass
    /// budget shrinks with width — while an oversized single batch is never
    /// split by the *wave* cut (splitting is `group_by_padded_cost`'s job).
    #[test]
    fn a_wider_split_runs_more_passes_per_wave() {
        let lengths: Vec<usize> = (0..30).map(|i| 200 + i).collect();
        let narrow = plan_waves(
            &group_by_padded_cost(&lengths, 32, 4_096),
            &lengths,
            1,
            4_096,
        );
        let wide = plan_waves(
            &group_by_padded_cost(&lengths, 32, 4_096 / 8),
            &lengths,
            8,
            4_096,
        );
        assert_eq!(narrow.iter().map(|w| w.len()).max().unwrap(), 1);
        let widest = wide.iter().map(|w| w.len()).max().unwrap();
        assert!(
            widest > 1,
            "an 8-wide split must run passes together: {wide:?}"
        );
        assert!(widest <= 8);
    }

    #[test]
    fn an_empty_plan_plans_no_waves() {
        assert!(plan_waves(&[], &[], 8, 4_096).is_empty());
    }
}
