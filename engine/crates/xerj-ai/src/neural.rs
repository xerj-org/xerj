//! Built-in neural sentence embedder — pure-Rust BERT inference via `candle`.
//!
//! This is XERJ's optional *real* semantic embedder. It loads a
//! sentence-transformers BERT model (default `all-MiniLM-L6-v2`, 384-dim)
//! and produces genuine neural embeddings, in-process, with no Python and
//! no external service. It complements the two existing backends:
//!
//!   * [`crate::local::local_embed`] — zero-dependency lexical feature-hash
//!     (the honest default; fast, deterministic, but *not* semantic).
//!   * [`crate::embed::EmbeddingProxy`] — any external OpenAI-compatible
//!     `/v1/embeddings` provider (bring-your-own model).
//!
//! Model files are fetched once on first use from the HuggingFace Hub and
//! cached on disk; air-gapped deployments point [`NeuralConfig::local_dir`]
//! at a directory holding `config.json`, `tokenizer.json`, and the
//! safetensors weights instead.
//!
//! Compiled only under the `neural` cargo feature.

use anyhow::{anyhow, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use std::path::{Path, PathBuf};
use tokenizers::{Encoding, Tokenizer, TruncationParams};

use crate::microbatch::group_by_padded_cost;

/// Default sentence encoder: 6-layer MiniLM, 384-dim, ~90 MB.
pub const DEFAULT_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Cap on tokens per passage. MiniLM's positional table is 512; passages are
/// already chunked upstream, so this is a safety clamp, not the usual path.
const MAX_TOKENS: usize = 512;

/// Rows in one forward pass. Measured on CPU with MiniLM
/// (`examples/neural_throughput.rs`): throughput climbs steeply to 64 rows and
/// then flattens and falls back. Two runs of that sweep on this shared box
/// disagree on absolute throughput — 32/64/128/256 rows read 139/155/151/132
/// passages/s in one and 179.1/199.9/198.7/166.2 in the other (the second is
/// the run published with this change) — but agree on where the knee is. 64 is
/// also what the ONNX backend uses.
const MAX_BATCH_ROWS: usize = 64;

/// Ceiling on `rows × padded_sequence_length` for one forward pass. Bounds the
/// activation memory a single call can allocate (BERT's attention tensor grows
/// with `rows × heads × seq²`) and stops a long passage from being batched with
/// many others at its own length. 4096 leaves a full 64-row batch of ~64-token
/// passages intact; on real ingest chunks (512 *characters*, so roughly
/// 110–130 tokens of prose and up to ~400 for dense code) it is usually the
/// budget rather than the row cap that binds — about 32 rows for prose, ~9 for
/// code.
const PADDED_TOKEN_BUDGET: usize = 4_096;

/// BERT's `[PAD]` id. Padded positions carry attention-mask 0, so the id only
/// has to be a valid embedding-table index; 0 is what the tokenizer's own
/// `PaddingParams::default()` used before batches were planned here.
const PAD_TOKEN_ID: u32 = 0;

/// What one [`NeuralEmbedder::embed_blocking_stats`] call actually pushed
/// through the model. `padded_token_slots` equals `real_tokens` exactly when
/// every batch is length-homogeneous; the ratio between them is the padding
/// waste, which is what length-aware batching exists to keep near 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchStats {
    /// Forward passes run for this call.
    pub inference_calls: usize,
    /// `rows × padded_sequence_length`, summed over those forward passes.
    pub padded_token_slots: usize,
    /// Tokens the input actually contained, after truncation.
    pub real_tokens: usize,
}

/// How to obtain the model weights.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NeuralConfig {
    /// HuggingFace model id (e.g. `sentence-transformers/all-MiniLM-L6-v2`).
    pub model_id: String,
    /// Override the HuggingFace cache directory. `None` uses the default
    /// (`~/.cache/huggingface`).
    pub cache_dir: Option<PathBuf>,
    /// Air-gapped: load `config.json` / `tokenizer.json` / weights from this
    /// directory instead of downloading. Takes precedence over the hub.
    pub local_dir: Option<PathBuf>,
}

impl Default for NeuralConfig {
    fn default() -> Self {
        Self {
            model_id: DEFAULT_MODEL_ID.to_string(),
            cache_dir: None,
            local_dir: None,
        }
    }
}

/// A loaded BERT sentence embedder. Cheap to share behind an `Arc`; `embed`
/// takes `&self` and is safe to call from many threads.
pub struct NeuralEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    dims: usize,
}

impl NeuralEmbedder {
    /// Output dimensionality (hidden size of the loaded model, e.g. 384).
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// Load the model, downloading from the HuggingFace Hub on first use
    /// (unless [`NeuralConfig::local_dir`] is set). **Blocking** — the caller
    /// runs this off the async executor (see [`crate::embedder`]).
    pub fn load(cfg: &NeuralConfig) -> Result<Self> {
        let (config_path, tokenizer_path, weights_path) = match &cfg.local_dir {
            Some(dir) => resolve_local(dir)?,
            None => resolve_from_hub(&cfg.model_id, cfg.cache_dir.as_deref())?,
        };

        let config_json = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read model config {}", config_path.display()))?;
        let config: Config = serde_json::from_str(&config_json)
            .with_context(|| format!("parse model config {}", config_path.display()))?;
        let dims = config.hidden_size;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow!("load tokenizer {}: {e}", tokenizer_path.display()))?;
        // Tokenize WITHOUT padding. Padding to the longest member of whatever
        // the caller handed us is exactly the cost this backend used to pay:
        // one long chunk in a window of short lines charged every short line
        // the long chunk's length. `embed_blocking` groups rows by length and
        // pads each group to its own longest row instead (see
        // [`crate::microbatch`]), so padding is applied here, per batch.
        tokenizer.with_padding(None);
        // Clamp over-long passages to the model's positional limit.
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow!("configure tokenizer truncation: {e}"))?;

        let device = Device::Cpu;
        // Safetensors is memory-mapped; the file must outlive the model, which
        // it does (candle copies tensors into the VarBuilder-backed model).
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(std::slice::from_ref(&weights_path), DTYPE, &device)
                .with_context(|| format!("map weights {}", weights_path.display()))?
        };
        let model = BertModel::load(vb, &config).map_err(|e| anyhow!("load BERT model: {e}"))?;

        Ok(Self {
            model,
            tokenizer,
            device,
            dims,
        })
    }

    /// Embed a batch of passages into L2-normalized sentence vectors using
    /// attention-masked mean pooling (the sentence-transformers convention).
    /// **Blocking / CPU-bound** — call via `spawn_blocking`.
    ///
    /// Output order matches input order regardless of how the passages were
    /// grouped internally.
    pub fn embed_blocking(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_blocking_stats(texts).map(|(vectors, _)| vectors)
    }

    /// Tokenized length of each passage, after truncation to [`MAX_TOKENS`].
    ///
    /// The cost of one forward pass is `rows × padded_sequence_length`, so
    /// these lengths are what any batching decision has to be made on; the
    /// throughput harness uses them to price a plan without running it.
    pub fn token_lengths(&self, texts: &[String]) -> Result<Vec<usize>> {
        Ok(self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("tokenize batch: {e}"))?
            .iter()
            .map(|enc| enc.get_ids().len().min(MAX_TOKENS))
            .collect())
    }

    /// [`Self::embed_blocking`], additionally reporting how much work the call
    /// actually pushed through the model. Used by the throughput harness and
    /// by the padding regression test; behaviour is otherwise identical.
    pub fn embed_blocking_stats(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, BatchStats)> {
        if texts.is_empty() {
            return Ok((vec![], BatchStats::default()));
        }

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("tokenize batch: {e}"))?;
        // The tokenizer truncates at MAX_TOKENS and no longer pads, so these
        // are the real per-passage lengths.
        let lengths = encodings
            .iter()
            .map(|enc| enc.get_ids().len().min(MAX_TOKENS))
            .collect::<Vec<_>>();

        let mut stats = BatchStats {
            real_tokens: lengths.iter().sum(),
            ..BatchStats::default()
        };
        let mut ordered = vec![Vec::new(); texts.len()];
        for rows in group_by_padded_cost(&lengths, MAX_BATCH_ROWS, PADDED_TOKEN_BUDGET) {
            let seq_len = rows.iter().map(|&i| lengths[i]).max().unwrap_or(0);
            if seq_len == 0 {
                // All-empty group — zero vectors of the right width.
                for i in rows {
                    ordered[i] = vec![0.0; self.dims];
                }
                continue;
            }
            stats.inference_calls += 1;
            stats.padded_token_slots += rows.len() * seq_len;
            let vectors = self.forward_padded(&encodings, &rows, &lengths, seq_len)?;
            for (i, vector) in rows.into_iter().zip(vectors) {
                ordered[i] = vector;
            }
        }
        Ok((ordered, stats))
    }

    /// Run one rectangular forward pass over `rows`, right-padding each row to
    /// `seq_len` with [`PAD_TOKEN_ID`] and attention-mask 0. Returns vectors in
    /// `rows` order.
    fn forward_padded(
        &self,
        encodings: &[Encoding],
        rows: &[usize],
        lengths: &[usize],
        seq_len: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let batch = rows.len();
        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for &row in rows {
            let len = lengths[row];
            ids.extend_from_slice(&encodings[row].get_ids()[..len]);
            ids.resize(ids.len() + (seq_len - len), PAD_TOKEN_ID);
            mask.extend_from_slice(&encodings[row].get_attention_mask()[..len]);
            mask.resize(mask.len() + (seq_len - len), 0);
        }

        let input_ids = Tensor::from_vec(ids, (batch, seq_len), &self.device)
            .map_err(|e| anyhow!("build input_ids tensor: {e}"))?;
        let attention_mask = Tensor::from_vec(mask, (batch, seq_len), &self.device)
            .map_err(|e| anyhow!("build attention_mask tensor: {e}"))?;
        let token_type_ids = input_ids
            .zeros_like()
            .map_err(|e| anyhow!("build token_type_ids: {e}"))?;

        // (batch, seq_len, hidden)
        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .map_err(|e| anyhow!("bert forward: {e}"))?;

        // Attention-masked mean pooling: sum(token * mask) / sum(mask).
        let mask_f = attention_mask
            .to_dtype(DType::F32)
            .and_then(|m| m.unsqueeze(2)) // (batch, seq_len, 1)
            .map_err(|e| anyhow!("mask to f32: {e}"))?;
        let summed = hidden
            .broadcast_mul(&mask_f)
            .and_then(|h| h.sum(1)) // (batch, hidden)
            .map_err(|e| anyhow!("masked sum: {e}"))?;
        let counts = mask_f
            .sum(1) // (batch, 1)
            .and_then(|c| c.clamp(1e-9, f32::INFINITY))
            .map_err(|e| anyhow!("mask counts: {e}"))?;
        let mean = summed
            .broadcast_div(&counts)
            .map_err(|e| anyhow!("mean pool: {e}"))?;

        // L2 normalize each row.
        let norm = mean
            .sqr()
            .and_then(|s| s.sum_keepdim(1))
            .and_then(|s| s.sqrt())
            .and_then(|n| n.clamp(1e-12, f32::INFINITY))
            .map_err(|e| anyhow!("l2 norm: {e}"))?;
        let normed = mean
            .broadcast_div(&norm)
            .map_err(|e| anyhow!("normalize: {e}"))?;

        normed
            .to_vec2::<f32>()
            .map_err(|e| anyhow!("read embeddings: {e}"))
    }
}

/// Resolve the three model files from a local directory (air-gapped).
fn resolve_local(dir: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let config = dir.join("config.json");
    let tokenizer = dir.join("tokenizer.json");
    let weights = find_local_weights(dir)?;
    for (label, p) in [("config.json", &config), ("tokenizer.json", &tokenizer)] {
        if !p.exists() {
            return Err(anyhow!(
                "local model dir {} is missing {label}",
                dir.display()
            ));
        }
    }
    Ok((config, tokenizer, weights))
}

/// Prefer `model.safetensors`; candle cannot read PyTorch `.bin` weights.
fn find_local_weights(dir: &Path) -> Result<PathBuf> {
    let st = dir.join("model.safetensors");
    if st.exists() {
        return Ok(st);
    }
    Err(anyhow!(
        "local model dir {} has no model.safetensors (candle requires safetensors, \
         not pytorch_model.bin)",
        dir.display()
    ))
}

/// Download (or read from cache) the model files from the HuggingFace Hub.
///
/// The first launch with `--embed-mode neural` on a fresh machine pulls the
/// weights (~90 MB for MiniLM); every launch after that is an instant cache
/// hit. We surface that clearly so a user staring at a terminal knows the
/// one-time download is happening rather than a hang. A progress bar is shown
/// (hf-hub writes it to stderr) for the same reason.
fn resolve_from_hub(
    model_id: &str,
    cache_dir: Option<&Path>,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    use hf_hub::api::sync::ApiBuilder;

    let mut builder = ApiBuilder::new().with_progress(true);
    if let Some(dir) = cache_dir {
        builder = builder.with_cache_dir(dir.to_path_buf());
    }
    let api = builder
        .build()
        .with_context(|| "init HuggingFace hub client")?;
    let repo = api.model(model_id.to_string());

    // Small metadata first (fast), then the big weights file. If the weights
    // are not already cached, this is the one-time download.
    let config = repo
        .get("config.json")
        .with_context(|| format!("fetch config.json for {model_id}"))?;
    let tokenizer = repo
        .get("tokenizer.json")
        .with_context(|| format!("fetch tokenizer.json for {model_id}"))?;
    let cached = config
        .parent()
        .map(|d| d.join("model.safetensors").exists())
        .unwrap_or(false);
    if !cached {
        tracing::info!(
            model = %model_id,
            "neural embedder: downloading model weights from HuggingFace \
             (one-time, ~90 MB for MiniLM; cached for every later start)…"
        );
    }
    let weights = repo.get("model.safetensors").with_context(|| {
        format!(
            "fetch model.safetensors for {model_id} (candle requires safetensors weights). \
             If this host has no internet, pre-download the model and point \
             `embedding.local_model_dir` at the folder holding config.json / \
             tokenizer.json / model.safetensors."
        )
    })?;
    tracing::info!(model = %model_id, "neural embedder: model ready");
    Ok((config, tokenizer, weights))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
    }

    /// Live test: downloads MiniLM on first run and checks that neural
    /// embeddings capture semantic similarity (paraphrase > unrelated),
    /// which the lexical feature-hash embedder cannot do. Network + ~90 MB
    /// download, so it is `#[ignore]`d — run explicitly:
    ///   cargo test -p xerj-ai --features neural -- --ignored --nocapture
    #[test]
    #[ignore]
    fn neural_captures_semantic_similarity() {
        let emb = NeuralEmbedder::load(&NeuralConfig::default()).expect("load MiniLM");
        assert_eq!(emb.dims(), 384, "MiniLM is 384-dim");

        let texts = vec![
            "A man is playing a guitar on stage.".to_string(),
            "A musician performs with his guitar at a concert.".to_string(),
            "The quarterly financial report shows rising interest rates.".to_string(),
        ];
        let vecs = emb.embed_blocking(&texts).expect("embed");
        assert_eq!(vecs.len(), 3);
        assert_eq!(vecs[0].len(), 384);

        // Each vector is L2-normalized, so cosine == dot product.
        let norm0 = cosine(&vecs[0], &vecs[0]).sqrt();
        assert!(
            (norm0 - 1.0).abs() < 1e-3,
            "vectors should be L2-normalized"
        );

        let sim_paraphrase = cosine(&vecs[0], &vecs[1]);
        let sim_unrelated = cosine(&vecs[0], &vecs[2]);
        eprintln!("paraphrase cos = {sim_paraphrase:.4}, unrelated cos = {sim_unrelated:.4}");
        assert!(
            sim_paraphrase > sim_unrelated + 0.15,
            "neural embedder must rank the paraphrase far above the unrelated \
             sentence (got paraphrase={sim_paraphrase:.3}, unrelated={sim_unrelated:.3})"
        );
    }

    /// The ~512-character chunk `TextChunker` emits for a long field.
    fn long_passage() -> String {
        let mut s = String::new();
        while s.len() < 512 {
            s.push_str(
                "the graph builder links each new node to its diverse neighbors and prunes the \
                 candidate list before the next level is entered. ",
            );
        }
        s.truncate(512);
        s
    }

    /// ~120 characters, the document shape measured in issue #366.
    fn short_passage(i: usize) -> String {
        format!(
            "method selectAndLinkDiverse{i} in HnswGraphBuilder.java. Select neighbors and \
                 return a mask of the ones kept."
        )
    }

    /// Regression for #366. A window that mixes one long chunk with many short
    /// lines must not charge the short lines the long chunk's length.
    ///
    /// Before length-aware batching this ran as ONE rectangular forward pass of
    /// 64 rows padded to the long chunk, i.e. `padded_token_slots` ≈ 3× the
    /// tokens the input actually held; measured wall time on the same box went
    /// 40 → 146 passages/s when this bound started holding.
    ///
    /// Live test (needs the MiniLM weights), so `#[ignore]`d:
    ///   cargo test -p xerj-ai --features neural -- --ignored --nocapture
    #[test]
    #[ignore]
    fn mixed_length_window_does_not_pad_short_passages_up_to_the_long_one() {
        let emb = NeuralEmbedder::load(&NeuralConfig::default()).expect("load MiniLM");

        let mut texts = vec![long_passage()];
        texts.extend((0..63).map(short_passage));
        let (vectors, stats) = emb.embed_blocking_stats(&texts).expect("embed");

        assert_eq!(vectors.len(), texts.len());
        eprintln!(
            "forwards={} padded_slots={} real_tokens={} ratio={:.2}",
            stats.inference_calls,
            stats.padded_token_slots,
            stats.real_tokens,
            stats.padded_token_slots as f64 / stats.real_tokens as f64
        );
        assert!(
            stats.padded_token_slots * 2 <= stats.real_tokens * 3,
            "padding waste must stay under 1.5×: {} padded slots for {} real tokens \
             ({} forward passes)",
            stats.padded_token_slots,
            stats.real_tokens,
            stats.inference_calls
        );
    }

    /// Grouping reorders rows internally; the vectors that come back must still
    /// line up with the inputs, and must match what each passage gets on its
    /// own. Live test, so `#[ignore]`d.
    #[test]
    #[ignore]
    fn grouping_preserves_input_order() {
        let emb = NeuralEmbedder::load(&NeuralConfig::default()).expect("load MiniLM");

        // Deliberately interleaved lengths so the length sort has to reorder.
        let texts = vec![
            long_passage(),
            short_passage(1),
            "a".to_string(),
            long_passage(),
            short_passage(2),
        ];
        let batched = emb.embed_blocking(&texts).expect("embed batch");
        assert_eq!(batched.len(), texts.len());
        for (i, text) in texts.iter().enumerate() {
            let alone = emb
                .embed_blocking(std::slice::from_ref(text))
                .expect("embed alone");
            let sim = cosine(&batched[i], &alone[0]);
            assert!(
                sim > 0.999,
                "row {i} came back out of order or altered: cosine {sim:.4} against the \
                 same passage embedded alone"
            );
        }
    }
}
