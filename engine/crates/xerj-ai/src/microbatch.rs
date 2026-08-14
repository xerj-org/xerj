//! Length-aware inference microbatching, shared by every in-process backend.
//!
//! One transformer forward allocates `batch × padded_sequence × hidden`
//! activations, so a batch costs what its *longest* member costs, multiplied
//! by every row in it. Handing a whole document to one forward is therefore a
//! memory cliff, not a throughput win: a 258 KB `semantic_text` field chunks
//! into ~600 passages, and embedding all of them as one tensor cost +2.0 GB of
//! transient RSS and tens of seconds — the same text spread over 20 documents
//! cost +0.2 GB (#366).
//!
//! [`plan_microbatches`] bounds that. It sorts by token length so similar
//! lengths land together (padding stops being paid for), caps each group at
//! `max_batch` rows and `padded_token_budget` padded token slots, and returns
//! *input positions* so callers scatter the resulting vectors back into
//! request order.
//!
//! This started inside the experimental ONNX backend, where it was the only
//! defence against an unbounded `Run`. Nothing about it is ONNX-specific, and
//! the Candle neural backend needs exactly the same bound, so it is compiled
//! unconditionally and re-exported from [`crate::onnx`].

use anyhow::{anyhow, Result};

/// Cap on tokens per passage, shared by both in-process encoders. MiniLM's
/// positional table is 512; passages are already chunked upstream, so this is
/// a safety clamp rather than the usual path.
pub const MAX_TOKENS: usize = 512;

/// Bounds for one offline scheduling window.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct MicrobatchConfig {
    /// Maximum inputs accepted in one call. Larger calls receive a clear
    /// backpressure error instead of allocating an unbounded queue.
    pub max_pending: usize,
    /// Maximum documents sent to one inference call.
    pub max_batch: usize,
    /// Maximum `batch_size × longest_sequence` token slots.
    pub padded_token_budget: usize,
}

impl Default for MicrobatchConfig {
    fn default() -> Self {
        Self {
            max_pending: 4_096,
            max_batch: 64,
            padded_token_budget: 4_096,
        }
    }
}

impl MicrobatchConfig {
    pub(crate) fn validate(self) -> Result<Self> {
        if self.max_pending == 0 || self.max_batch == 0 || self.padded_token_budget == 0 {
            return Err(anyhow!(
                "microbatch limits must be non-zero (max_pending={}, max_batch={}, padded_token_budget={})",
                self.max_pending,
                self.max_batch,
                self.padded_token_budget
            ));
        }
        Ok(self)
    }
}

/// Plan length-aware microbatches. Indices are sorted for inference efficiency;
/// callers must use them to restore original order.
pub fn plan_microbatches(
    token_lengths: &[usize],
    config: MicrobatchConfig,
) -> Result<Vec<Vec<usize>>> {
    let config = config.validate()?;
    if token_lengths.len() > config.max_pending {
        return Err(anyhow!(
            "embedding queue is full: received {} texts, max_pending={}",
            token_lengths.len(),
            config.max_pending
        ));
    }
    let mut order = (0..token_lengths.len()).collect::<Vec<_>>();
    order.sort_by_key(|&i| token_lengths[i].min(MAX_TOKENS));
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut longest = 0;
    for i in order {
        let length = token_lengths[i].min(MAX_TOKENS);
        if length > config.padded_token_budget {
            return Err(anyhow!(
                "padded_token_budget={} cannot fit one {}-token input",
                config.padded_token_budget,
                length
            ));
        }
        let next_longest = longest.max(length);
        if !batch.is_empty()
            && (batch.len() == config.max_batch
                || next_longest * (batch.len() + 1) > config.padded_token_budget)
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
    Ok(batches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_bounds_every_batch_and_preserves_all_positions() {
        let lengths = [512, 14, 200, 16, 400, 15, 90, 300];
        let config = MicrobatchConfig {
            max_pending: 8,
            max_batch: 3,
            padded_token_budget: 600,
        };
        let batches = plan_microbatches(&lengths, config).unwrap();
        let mut positions = batches.iter().flatten().copied().collect::<Vec<_>>();
        positions.sort_unstable();
        assert_eq!(positions, (0..lengths.len()).collect::<Vec<_>>());
        for batch in batches {
            assert!(batch.len() <= config.max_batch);
            let longest = batch.iter().map(|&i| lengths[i]).max().unwrap();
            assert!(longest * batch.len() <= config.padded_token_budget);
        }
    }

    #[test]
    fn planner_applies_backpressure() {
        let error = plan_microbatches(
            &[10, 20, 30],
            MicrobatchConfig {
                max_pending: 2,
                ..MicrobatchConfig::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("queue is full"));
        assert!(error.contains("max_pending=2"));
    }

    #[test]
    fn planner_rejects_impossible_budget() {
        let error = plan_microbatches(
            &[512],
            MicrobatchConfig {
                padded_token_budget: 128,
                ..MicrobatchConfig::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("cannot fit"));
    }

    /// The defect this module exists to bound (#366): one 258 KB document
    /// chunks into ~600 passages, and the neural backend used to feed all of
    /// them to a single forward — +2.0 GB of transient activations, versus
    /// +0.2 GB for the same text split over 20 documents.
    ///
    /// The planner must (a) never let one forward exceed the padded-token
    /// budget, which is what caps the activation footprint, and (b) still
    /// account for every input exactly once, which is what keeps a vector
    /// attached to the passage it was computed from. A scatter bug here is
    /// silent — the vectors stay 384-wide and unit-norm, only relevance rots —
    /// so assert the positions, not just the shapes.
    #[test]
    fn one_large_document_is_split_into_bounded_forwards() {
        // 595 chunks with the length spread a real chunker produces: mostly
        // full 512-char windows, a short tail, a few tiny ones.
        let lengths = (0..595)
            .map(|i| match i % 7 {
                0 => 9,
                1 => 47,
                _ => 128,
            })
            .collect::<Vec<_>>();
        let config = MicrobatchConfig {
            max_pending: usize::MAX,
            ..MicrobatchConfig::default()
        };

        let batches = plan_microbatches(&lengths, config).unwrap();
        assert!(
            batches.len() > 1,
            "595 passages must not be planned as one forward"
        );

        let mut seen = vec![0usize; lengths.len()];
        for batch in &batches {
            assert!(batch.len() <= config.max_batch);
            let longest = batch.iter().map(|&i| lengths[i]).max().unwrap();
            assert!(
                longest * batch.len() <= config.padded_token_budget,
                "a forward of {} rows padded to {longest} exceeds the budget",
                batch.len()
            );
            for &position in batch {
                seen[position] += 1;
            }
        }
        assert!(
            seen.iter().all(|&count| count == 1),
            "every passage must be planned exactly once"
        );

        // Peak resident activation is set by the *largest single* forward,
        // which is what the +2.0 GB spike measured. The unbounded path built
        // one `passages × longest` tensor; every planned forward now fits the
        // padded-token budget, an 18× smaller peak for this document.
        let unbounded_slots = lengths.len() * lengths.iter().copied().max().unwrap();
        let peak_slots = batches
            .iter()
            .map(|batch| batch.len() * batch.iter().map(|&i| lengths[i]).max().unwrap())
            .max()
            .unwrap();
        assert!(peak_slots <= config.padded_token_budget);
        assert!(
            peak_slots * 8 < unbounded_slots,
            "the largest planned forward ({peak_slots} padded slots) must be a \
             small fraction of the unbounded {unbounded_slots}"
        );
    }
}
