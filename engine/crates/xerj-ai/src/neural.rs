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

use crate::microbatch::{plan_microbatches, MicrobatchConfig, MAX_TOKENS};
use anyhow::{anyhow, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use std::path::{Path, PathBuf};
use tokenizers::{Encoding, Tokenizer, TruncationParams};

/// Default sentence encoder: 6-layer MiniLM, 384-dim, ~90 MB.
pub const DEFAULT_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Inference bounds for one `embed_blocking` call, matching the ONNX backend's
/// defaults (64 rows, 4096 padded token slots).
///
/// `max_pending` is deliberately unbounded here. It is the ONNX backend's
/// admission control, and the Candle backend has none of its own: a document
/// that chunks into more passages than the cap used to succeed (slowly), and
/// must keep succeeding — it is now split into bounded forwards rather than
/// rejected.
const MICROBATCH: MicrobatchConfig = MicrobatchConfig {
    max_pending: usize::MAX,
    max_batch: 64,
    padded_token_budget: 4_096,
};

/// `[PAD]` in every BERT/MiniLM vocabulary, and the id the tokenizer's own
/// `PaddingParams::default()` used before padding moved in here. Padded
/// positions are masked out of attention *and* of mean pooling, so the id
/// never reaches an output — it only has to be in range.
const PAD_TOKEN_ID: u32 = 0;

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
        // No tokenizer-level padding: it would pad every passage in a request
        // out to the request's longest, which is both the padding waste and
        // the memory cliff #366 measured. `embed_blocking` pads each planned
        // microbatch to *that* microbatch's longest instead, so the encodings
        // here carry their true lengths and the planner sees real token
        // counts. Over-long passages are still clamped to the positional limit.
        tokenizer.with_padding(None);
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
    /// The request is split into bounded, length-sorted microbatches
    /// ([`plan_microbatches`]) and each is run as its own forward. One
    /// 595-passage document therefore costs ~19 bounded forwards instead of
    /// one 595-row tensor, which is the difference between +0.2 GB and +2.0 GB
    /// of transient RSS (#366).
    pub fn embed_blocking(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_microbatched(texts, MICROBATCH)
    }

    /// [`Self::embed_blocking`] with explicit bounds, so a test can pin the
    /// plan (e.g. one row per forward) and compare against the default one.
    fn embed_microbatched(
        &self,
        texts: &[String],
        limits: MicrobatchConfig,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize once for the whole request — the plan only needs lengths,
        // and re-encoding per microbatch would pay the tokenizer twice.
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("tokenize batch: {e}"))?;
        let lengths = encodings
            .iter()
            .map(|enc| enc.get_ids().len())
            .collect::<Vec<_>>();

        let mut ordered = vec![Vec::new(); texts.len()];
        for rows in plan_microbatches(&lengths, limits)? {
            let vectors = self.forward_microbatch(&encodings, &rows)?;
            for (position, vector) in rows.into_iter().zip(vectors) {
                ordered[position] = vector;
            }
        }
        Ok(ordered)
    }

    /// One forward over the selected encodings, padded to the longest sequence
    /// in *this* microbatch. Returns vectors in `rows` order; the caller
    /// scatters them back to input positions.
    fn forward_microbatch(&self, encodings: &[Encoding], rows: &[usize]) -> Result<Vec<Vec<f32>>> {
        let batch = rows.len();
        let seq_len = rows
            .iter()
            .map(|&row| encodings[row].get_ids().len())
            .max()
            .unwrap_or(0);
        if seq_len == 0 {
            // All-empty input — return zero vectors of the right width.
            return Ok(vec![vec![0.0; self.dims]; batch]);
        }

        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for &row in rows {
            let enc = &encodings[row];
            let padding = seq_len - enc.get_ids().len();
            ids.extend_from_slice(enc.get_ids());
            ids.resize(ids.len() + padding, PAD_TOKEN_ID);
            mask.extend_from_slice(enc.get_attention_mask());
            mask.resize(mask.len() + padding, 0);
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

    /// Model source for the live tests. `XERJ_NEURAL_TEST_MODEL_DIR` points at
    /// a directory holding `config.json`, `tokenizer.json` and
    /// `model.safetensors` for an offline / air-gapped run; without it the hub
    /// default downloads once and is cached.
    fn live_config() -> NeuralConfig {
        match std::env::var_os("XERJ_NEURAL_TEST_MODEL_DIR") {
            Some(dir) => NeuralConfig {
                local_dir: Some(PathBuf::from(dir)),
                ..NeuralConfig::default()
            },
            None => NeuralConfig::default(),
        }
    }

    /// A document that chunks the way the reported 258 KB file did: hundreds
    /// of passages with genuinely mixed lengths, so the length-sorted plan
    /// actually reorders rows instead of leaving them in place.
    fn large_document_passages() -> Vec<String> {
        (0..600)
            .map(|i| match i % 5 {
                0 => format!("passage {i}"),
                1 => format!("A man is playing a guitar on stage, passage {i}."),
                2 => format!("{}tail {i}", "word ".repeat(64)),
                3 => format!("{}{i}", "sentence about quarterly revenue. ".repeat(12)),
                _ => format!("{i}"),
            })
            .collect()
    }

    /// Current resident set size in KiB from `/proc/self/status`; `None` off
    /// Linux. Deliberately `VmRSS` and not the `VmHWM` high-water mark: the
    /// mark is monotonic for the life of the process, so any earlier test that
    /// allocated more would silently turn the assertion below into a no-op.
    fn resident_kb() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|value| value.split_whitespace().next()?.parse().ok())
    }

    /// Run `work`, sampling resident memory throughout, and return its peak in
    /// KiB. The spike this guards against is transient — one tensor, freed as
    /// soon as the forward returns — so it has to be caught while it exists.
    fn peak_resident_kb_during<T>(work: impl FnOnce() -> T) -> (T, u64) {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;

        let peak = Arc::new(AtomicU64::new(resident_kb().unwrap_or(0)));
        let running = Arc::new(AtomicBool::new(true));
        let sampler = std::thread::spawn({
            let peak = Arc::clone(&peak);
            let running = Arc::clone(&running);
            move || {
                while running.load(Ordering::Relaxed) {
                    if let Some(rss) = resident_kb() {
                        peak.fetch_max(rss, Ordering::Relaxed);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
        });
        let value = work();
        running.store(false, Ordering::Relaxed);
        sampler.join().expect("RSS sampler thread");
        (value, peak.load(Ordering::Relaxed))
    }

    /// #366, correctness half. `embed_blocking` used to build ONE tensor for
    /// the whole request, padded to the request's longest passage; it now runs
    /// bounded, length-sorted microbatches and scatters the results back. The
    /// vectors must be identical to an unbatched run *at the same positions*.
    ///
    /// A scatter bug here is silent — the vectors stay 384-wide and unit-norm,
    /// only relevance rots — so this compares element-wise against a reference
    /// plan of one row per forward instead of checking shapes.
    ///
    /// Live test (model required, no network with
    /// `XERJ_NEURAL_TEST_MODEL_DIR`), so `#[ignore]`d:
    ///   cargo test -p xerj-ai --features neural -- --ignored --test-threads=1
    #[test]
    #[ignore]
    fn microbatched_embeddings_match_an_unbatched_reference() {
        let emb = NeuralEmbedder::load(&live_config()).expect("load MiniLM");
        let texts = large_document_passages();

        let unbatched = MicrobatchConfig {
            max_batch: 1,
            ..MICROBATCH
        };
        let reference = emb
            .embed_microbatched(&texts, unbatched)
            .expect("unbatched reference");
        let batched = emb.embed_blocking(&texts).expect("microbatched");

        assert_eq!(batched.len(), texts.len());
        for (i, (got, want)) in batched.iter().zip(&reference).enumerate() {
            assert_eq!(got.len(), emb.dims(), "passage {i} width");
            let drift = got
                .iter()
                .zip(want)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                drift < 1e-5,
                "passage {i} drifted {drift} from the unbatched reference — \
                 microbatch results were scattered to the wrong positions"
            );
        }
    }

    /// #366, memory half. One 258 KB document chunks into ~600 passages, and
    /// the old path stacked all of them into ONE tensor padded to the longest:
    /// +2.0 GB of transient activations, against +0.2 GB for the same text
    /// spread over 20 documents. That is an OOM vector, not merely slowness —
    /// and it gets multiplied by the fan-out width now that windows run
    /// concurrently, which is why this had to land first.
    ///
    /// Reproduces the reported shape rather than an easier one: 600 passages
    /// of ~120 tokens each, which is what the engine's 512-character chunker
    /// produces. Note that the cost is *quadratic* in sequence length — the
    /// attention scores alone are `rows × heads × seq²` — so one 600×128
    /// forward is ~470 MB of attention on top of its activations, and several
    /// of those are live at once. That is where the reported +2.0 GB comes
    /// from, and why capping rows per forward is the fix.
    ///
    ///   cargo test -p xerj-ai --features neural -- --ignored --test-threads=1
    #[test]
    #[ignore]
    fn one_large_document_does_not_spike_resident_memory() {
        // The unbounded forward measured +2.0 GB. `padded_token_budget` holds
        // each forward to 4096 slots — 32 rows at this length, ~25 MB of
        // attention — so the ceiling sits an order of magnitude below the
        // defect while leaving room for an allocator that retains arenas.
        const CEILING_KB: u64 = 384 * 1024;

        let emb = NeuralEmbedder::load(&live_config()).expect("load MiniLM");
        let Some(before) = resident_kb() else {
            eprintln!("no /proc/self/status on this platform; skipping the RSS ceiling");
            return;
        };

        let texts = vec!["word ".repeat(120); 600];
        let (vectors, peak) = peak_resident_kb_during(|| emb.embed_blocking(&texts));
        assert_eq!(vectors.expect("microbatched").len(), texts.len());

        let spike = peak.saturating_sub(before);
        eprintln!(
            "resident memory peaked +{spike} KiB embedding {} passages",
            texts.len()
        );
        assert!(
            spike < CEILING_KB,
            "embedding one {}-passage document spiked resident memory by \
             {spike} KiB against a {CEILING_KB} KiB ceiling — the request is \
             going to the model as one tensor again",
            texts.len()
        );
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
