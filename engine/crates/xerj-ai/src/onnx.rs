//! Experimental FP32 ONNX Runtime MiniLM backend.
//!
//! This module is an embedding-layer prototype. It is not wired to XERJ's
//! server or CLI. The safe `ort` API requires mutable session access, so one
//! session is serialized behind a mutex and aggregate throughput comes from
//! bounded, length-aware microbatches rather than unsafe concurrent `Run`.

use anyhow::{anyhow, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

pub const MAX_TOKENS: usize = 512;
pub const DIMS: usize = 384;

/// Bounds for one offline scheduling window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    fn validate(self) -> Result<Self> {
        if self.max_pending == 0 || self.max_batch == 0 || self.padded_token_budget == 0 {
            return Err(anyhow!(
                "ONNX microbatch limits must be non-zero (max_pending={}, max_batch={}, padded_token_budget={})",
                self.max_pending,
                self.max_batch,
                self.padded_token_budget
            ));
        }
        Ok(self)
    }
}

pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
}

impl OnnxEmbedder {
    pub fn load(model_path: &Path, tokenizer_path: &Path, intra_threads: usize) -> Result<Self> {
        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow!("load tokenizer {}: {e}", tokenizer_path.display()))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow!("configure tokenizer truncation: {e}"))?;

        let session = Session::builder()
            .map_err(|e| anyhow!("create ONNX Runtime session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("configure ONNX graph optimizations: {e}"))?
            .with_config_entry("session.intra_op.allow_spinning", "0")
            .map_err(|e| anyhow!("disable ONNX intra-op spinning: {e}"))?
            .with_intra_threads(intra_threads.max(1))
            .map_err(|e| anyhow!("configure ONNX intra-op threads: {e}"))?
            .commit_from_file(model_path)
            .with_context(|| format!("load ONNX model {}", model_path.display()))?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
        })
    }

    /// Embed a bounded scheduling window, grouping similar lengths and
    /// restoring vectors to exact input order.
    pub fn embed_scheduled_blocking(
        &self,
        texts: &[String],
        config: MicrobatchConfig,
    ) -> Result<Vec<Vec<f32>>> {
        let config = config.validate()?;
        if texts.len() > config.max_pending {
            return Err(anyhow!(
                "ONNX embedding queue is full: received {} texts, max_pending={}; split the request or retry after draining",
                texts.len(),
                config.max_pending
            ));
        }
        let lengths = texts
            .iter()
            .map(|text| self.token_len(text))
            .collect::<Result<Vec<_>>>()?;
        let batches = plan_microbatches(&lengths, config)?;
        let mut ordered = vec![Vec::new(); texts.len()];
        for batch in batches {
            let input = batch.iter().map(|&i| texts[i].clone()).collect::<Vec<_>>();
            let vectors = self.embed_blocking(&input)?;
            for (position, vector) in batch.into_iter().zip(vectors) {
                ordered[position] = vector;
            }
        }
        Ok(ordered)
    }

    pub fn token_len(&self, text: &str) -> Result<usize> {
        self.tokenizer
            .encode(text, true)
            .map(|encoding| encoding.len())
            .map_err(|e| anyhow!("tokenize for ONNX scheduling: {e}"))
    }

    pub fn embed_blocking(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("tokenize batch: {e}"))?;
        let batch = encodings.len();
        let seq_len = encodings.first().map(|e| e.get_ids().len()).unwrap_or(0);
        if seq_len == 0 {
            return Ok(vec![vec![0.0; DIMS]; batch]);
        }

        let mut ids = Vec::with_capacity(batch * seq_len);
        let mut mask = Vec::with_capacity(batch * seq_len);
        for encoding in &encodings {
            ids.extend(encoding.get_ids().iter().map(|&v| i64::from(v)));
            mask.extend(encoding.get_attention_mask().iter().map(|&v| i64::from(v)));
        }
        let token_types = vec![0_i64; ids.len()];
        let shape = [batch, seq_len];
        let input_ids = Tensor::from_array((shape, ids)).context("build input_ids")?;
        let attention_mask =
            Tensor::from_array((shape, mask.clone())).context("build attention_mask")?;
        let token_type_ids =
            Tensor::from_array((shape, token_types)).context("build token_type_ids")?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("ONNX session mutex poisoned"))?;
        let outputs = session
            .run(ort::inputs! {
                "input_ids" => input_ids,
                "attention_mask" => attention_mask,
                "token_type_ids" => token_type_ids,
            })
            .context("ONNX MiniLM inference")?;
        let output = outputs
            .get("last_hidden_state")
            .or_else(|| outputs.get("token_embeddings"))
            .ok_or_else(|| anyhow!("ONNX model did not return token embeddings"))?;
        let (output_shape, hidden) = output
            .try_extract_tensor::<f32>()
            .context("extract ONNX token embeddings")?;
        let shape_dims = output_shape.iter().copied().collect::<Vec<_>>();
        if shape_dims != [batch as i64, seq_len as i64, DIMS as i64] {
            return Err(anyhow!(
                "unexpected ONNX output shape {shape_dims:?}, expected [{batch}, {seq_len}, {DIMS}]"
            ));
        }

        let mut vectors = Vec::with_capacity(batch);
        for row in 0..batch {
            let mut pooled = vec![0.0_f32; DIMS];
            let mut count = 0.0_f32;
            for token in 0..seq_len {
                let weight = mask[row * seq_len + token] as f32;
                count += weight;
                let base = (row * seq_len + token) * DIMS;
                for dim in 0..DIMS {
                    pooled[dim] += hidden[base + dim] * weight;
                }
            }
            for value in &mut pooled {
                *value /= count.max(1e-9);
            }
            let norm = pooled
                .iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt()
                .max(1e-12);
            for value in &mut pooled {
                *value /= norm;
            }
            vectors.push(pooled);
        }
        Ok(vectors)
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
            "ONNX embedding queue is full: received {} texts, max_pending={}",
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
}
