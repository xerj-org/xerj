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
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// Default sentence encoder: 6-layer MiniLM, 384-dim, ~90 MB.
pub const DEFAULT_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Cap on tokens per passage. MiniLM's positional table is 512; passages are
/// already chunked upstream, so this is a safety clamp, not the usual path.
const MAX_TOKENS: usize = 512;

/// How to obtain the model weights.
#[derive(Debug, Clone)]
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
        // Pad each batch to its longest member so we can stack into one tensor,
        // and clamp over-long passages to the model's positional limit.
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

        let device = Device::Cpu;
        // Safetensors is memory-mapped; the file must outlive the model, which
        // it does (candle copies tensors into the VarBuilder-backed model).
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path.clone()], DTYPE, &device)
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
            // All-empty input — return zero vectors of the right width.
            return Ok(vec![vec![0.0; self.dims]; batch]);
        }

        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for enc in &encodings {
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
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
}
