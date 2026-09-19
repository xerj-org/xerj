//! In-process sequence-pair classification — pure-Rust inference via `candle`.
//!
//! This is the model runtime behind two features that both reduce to "score a
//! pair of texts with a classification head":
//!
//!   * the `local` rerank provider — a cross-encoder reads `(query, document)`
//!     and emits one relevance logit;
//!   * `POST /_judge` — a natural-language-inference model reads
//!     `(state, hypothesis)` and emits entailment logits.
//!
//! It complements [`crate::neural`], which runs the *bi*-encoder: that one
//! embeds each text alone so vectors can be indexed; this one reads the two
//! texts together, which is why it cannot be precomputed and why it ranks
//! better (every attention layer sees both sides).
//!
//! Three encoder families cover every model the judge ships with, all of them
//! already in `candle-transformers` (MIT/Apache-2.0), so no model code is
//! written here beyond the BERT classification head candle does not provide:
//!
//! | `config.json` `model_type` | candle module | used by |
//! |---|---|---|
//! | `bert` | `models::bert` + the pooler/classifier head below | MiniLM cross-encoder |
//! | `xlm-roberta`, `roberta` | `models::xlm_roberta` (`:484`) | bge rerankers |
//! | `deberta-v2` | `models::debertav2` (`:1269`) | zero-shot NLI |
//!
//! Compiled only under the `neural` cargo feature, like [`crate::neural`].

use anyhow::{anyhow, bail, Context, Result};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use candle_transformers::models::{bert, debertav2, xlm_roberta};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokenizers::{EncodeInput, Encoding, Tokenizer, TruncationParams, TruncationStrategy};

use crate::microbatch::group_by_padded_cost;

/// Cap on tokens per pair, query and document together.
///
/// 512 is the positional limit of the BERT and XLM-R base checkpoints, and the
/// length every model here was trained and evaluated at. `bge-reranker-v2-m3`
/// accepts 8,192, but self-attention is quadratic in the sequence and this runs
/// on a CPU inside a search request: one 8k-token pair costs what 256 pairs of
/// 512 tokens cost. A pair that is cut is *counted* ([`PairStats::truncated`])
/// so the limit is visible in the response rather than silent.
pub const MAX_PAIR_TOKENS: usize = 512;

/// Rows in one forward pass, before the padded-token budget is applied.
const MAX_BATCH_ROWS: usize = 32;

/// Ceiling on `rows × padded_sequence_length` for one forward pass.
///
/// Bounds two things at once. Memory: the attention tensor is
/// `rows × heads × seq²` floats, so 8 rows of 512 tokens on a 16-head model is
/// ~0.5 GB transient and that is the worst case this budget admits. Latency
/// overshoot: a forward pass cannot be interrupted, so the deadline is only
/// checked between passes, and one pass is the most a request can overrun by.
const PADDED_TOKEN_BUDGET: usize = 4_096;

/// Candidates scored per scheduling chunk, in the caller's order.
///
/// Length-aware batching sorts by token length, which on its own would make a
/// deadline cut fall on the *longest* documents wherever they ranked. Chunking
/// first keeps the cut aligned with the engine's ranking: everything in chunk
/// `n` is scored before anything in chunk `n + 1`, so an expired deadline
/// leaves the tail unjudged rather than a random subset.
const CHUNK_ROWS: usize = 32;

/// Where the three model files come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PairModelSource {
    /// Hugging Face repository, e.g. `cross-encoder/ms-marco-MiniLM-L6-v2`.
    pub repo: String,
    /// Commit to resolve the repository at. Pinned for the built-in models so
    /// a moved `main` cannot change what a server scores with.
    pub revision: Option<String>,
    /// Override the Hugging Face cache directory (`None`: the default,
    /// `~/.cache/huggingface/hub`, or `HF_HOME`).
    pub cache_dir: Option<PathBuf>,
    /// Air-gapped: a directory holding `config.json`, `tokenizer.json` and
    /// `model.safetensors`. Takes precedence over the hub and the cache.
    pub local_dir: Option<PathBuf>,
    /// `false` never opens a network connection: the files must already be in
    /// `local_dir` or the cache.
    pub allow_download: bool,
    /// Expected sha256 of `model.safetensors`. Checked on load when set; a
    /// mismatch refuses the model rather than scoring with unknown weights.
    pub weights_sha256: Option<String>,
}

/// Work one [`PairClassifier::logits_blocking`] call pushed through the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PairStats {
    /// Forward passes run.
    pub forward_passes: usize,
    /// Tokens the scored pairs contained, after truncation.
    pub real_tokens: usize,
    /// `rows × padded_sequence_length`, summed over the forward passes.
    pub padded_token_slots: usize,
    /// Pairs longer than [`MAX_PAIR_TOKENS`] that were cut to fit.
    pub truncated: usize,
    /// Pairs left unscored because the caller said stop.
    pub unscored: usize,
}

enum Encoder {
    Bert {
        model: bert::BertModel,
        pooler: Linear,
        classifier: Linear,
    },
    XlmRoberta(xlm_roberta::XLMRobertaForSequenceClassification),
    DebertaV2(debertav2::DebertaV2SeqClassificationModel),
}

/// A loaded pair classifier. Share it behind an `Arc`; scoring takes `&self`.
pub struct PairClassifier {
    encoder: Encoder,
    tokenizer: Tokenizer,
    device: Device,
    pad_id: u32,
    labels: Vec<String>,
    model_type: String,
    weights_sha256: String,
    weights_bytes: u64,
}

impl std::fmt::Debug for PairClassifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairClassifier")
            .field("model_type", &self.model_type)
            .field("labels", &self.labels)
            .field("weights_sha256", &self.weights_sha256)
            .finish()
    }
}

/// The three files a model is built from, resolved to paths.
#[derive(Debug, Clone)]
pub struct PairModelFiles {
    pub config: PathBuf,
    pub tokenizer: PathBuf,
    pub weights: PathBuf,
}

impl PairClassifier {
    /// Load a model. **Blocking** (file I/O, possibly a download, and a sha256
    /// over the weights) — run it off the async executor.
    pub fn load(source: &PairModelSource) -> Result<Self> {
        let files = resolve(source)?;
        Self::load_files(&files, source.weights_sha256.as_deref())
    }

    /// Load from already-resolved files, optionally checking the weights.
    pub fn load_files(files: &PairModelFiles, expect_sha256: Option<&str>) -> Result<Self> {
        let weights_sha256 = file_sha256(&files.weights)?;
        if let Some(expected) = expect_sha256 {
            if !expected.eq_ignore_ascii_case(&weights_sha256) {
                bail!(
                    "model weights {} have sha256 {weights_sha256}, expected {expected}: refusing \
                     to score with weights that are not the ones this build was measured with",
                    files.weights.display()
                );
            }
        }
        let weights_bytes = std::fs::metadata(&files.weights)
            .map(|m| m.len())
            .unwrap_or(0);

        let config_json = std::fs::read_to_string(&files.config)
            .with_context(|| format!("read model config {}", files.config.display()))?;
        let raw: serde_json::Value = serde_json::from_str(&config_json)
            .with_context(|| format!("parse model config {}", files.config.display()))?;
        let model_type = raw
            .get("model_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let labels = labels_from_config(&raw);

        let mut tokenizer = Tokenizer::from_file(&files.tokenizer)
            .map_err(|e| anyhow!("load tokenizer {}: {e}", files.tokenizer.display()))?;
        // Padding is applied per forward pass, to that pass's own longest row
        // (see `crate::microbatch`), never by the tokenizer.
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_PAIR_TOKENS,
                // What the reference implementations of these models use
                // (`truncation=True`): trim the longer side first. With a
                // short query that is always the document; with a long
                // `state` and a short hypothesis it is the state.
                strategy: TruncationStrategy::LongestFirst,
                ..Default::default()
            }))
            .map_err(|e| anyhow!("configure tokenizer truncation: {e}"))?;

        let device = Device::Cpu;
        // Weights published as F16 are widened on load: candle's CPU matmul
        // kernels are F32, and half precision on CPU is slower, not faster.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                std::slice::from_ref(&files.weights),
                DType::F32,
                &device,
            )
            .with_context(|| format!("map weights {}", files.weights.display()))?
        };

        let (encoder, pad_id) = match model_type.as_str() {
            "bert" => {
                let config: bert::Config = serde_json::from_str(&config_json)
                    .with_context(|| "parse BERT config".to_string())?;
                let hidden = config.hidden_size;
                // `BertModel::load` finds the encoder under the `bert.` prefix
                // by itself (it retries with `model_type` as the prefix).
                let model = bert::BertModel::load(vb.clone(), &config)
                    .map_err(|e| anyhow!("load BERT encoder: {e}"))?;
                let pooler = candle_nn::linear(hidden, hidden, vb.pp("bert.pooler.dense"))
                    .map_err(|e| anyhow!("load BERT pooler: {e}"))?;
                let classifier = candle_nn::linear(hidden, labels.len(), vb.pp("classifier"))
                    .map_err(|e| anyhow!("load BERT classifier: {e}"))?;
                let pad = raw
                    .get("pad_token_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                (
                    Encoder::Bert {
                        model,
                        pooler,
                        classifier,
                    },
                    pad,
                )
            }
            "xlm-roberta" | "roberta" => {
                let config: xlm_roberta::Config = serde_json::from_str(&config_json)
                    .with_context(|| "parse XLM-RoBERTa config".to_string())?;
                let pad = config.pad_token_id;
                let model = xlm_roberta::XLMRobertaForSequenceClassification::new(
                    labels.len(),
                    &config,
                    vb,
                )
                .map_err(|e| anyhow!("load XLM-RoBERTa classifier: {e}"))?;
                (Encoder::XlmRoberta(model), pad)
            }
            "deberta-v2" => {
                let config: debertav2::Config = serde_json::from_str(&config_json)
                    .with_context(|| "parse DeBERTa-v2 config".to_string())?;
                let pad = config.pad_token_id.unwrap_or(0) as u32;
                let id2label = config.id2label.clone();
                let model = debertav2::DebertaV2SeqClassificationModel::load(
                    vb.pp("deberta"),
                    &config,
                    id2label,
                )
                .map_err(|e| anyhow!("load DeBERTa-v2 classifier: {e}"))?;
                (Encoder::DebertaV2(model), pad)
            }
            other => bail!(
                "unsupported model_type `{other}` in {}: the local judge loads `bert`, \
                 `xlm-roberta`/`roberta` and `deberta-v2` sequence classifiers",
                files.config.display()
            ),
        };

        Ok(Self {
            encoder,
            tokenizer,
            device,
            pad_id,
            labels,
            model_type,
            weights_sha256,
            weights_bytes,
        })
    }

    /// Output labels in logit order (`id2label` from `config.json`). A
    /// cross-encoder has exactly one; an NLI model two or three.
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// `model_type` from `config.json`.
    pub fn model_type(&self) -> &str {
        &self.model_type
    }

    /// sha256 of the weights actually loaded.
    pub fn weights_sha256(&self) -> &str {
        &self.weights_sha256
    }

    /// Size of the weights file on disk.
    pub fn weights_bytes(&self) -> u64 {
        self.weights_bytes
    }

    /// Raw logits for each `(first, second)` pair, in input order.
    ///
    /// `keep_going` is polled between forward passes; once it returns `false`
    /// the remaining pairs come back as `None`. A pass cannot be interrupted,
    /// so the overrun is bounded by one pass ([`PADDED_TOKEN_BUDGET`]).
    ///
    /// **Blocking / CPU-bound.** Run it on a dedicated thread pool: candle's
    /// matmul parallelises through rayon and uses whichever pool it is called
    /// from, which is how the caller sets the thread budget.
    pub fn logits_blocking(
        &self,
        pairs: &[(String, String)],
        keep_going: &dyn Fn() -> bool,
    ) -> Result<(Vec<Option<Vec<f32>>>, PairStats)> {
        let mut out: Vec<Option<Vec<f32>>> = vec![None; pairs.len()];
        let mut stats = PairStats::default();

        for (chunk_no, chunk) in pairs.chunks(CHUNK_ROWS).enumerate() {
            if !keep_going() {
                break;
            }
            let base = chunk_no * CHUNK_ROWS;
            let inputs: Vec<EncodeInput> = chunk
                .iter()
                .map(|(a, b)| EncodeInput::Dual(a.as_str().into(), b.as_str().into()))
                .collect();
            let encodings = self
                .tokenizer
                .encode_batch(inputs, true)
                .map_err(|e| anyhow!("tokenize pairs: {e}"))?;
            let lengths: Vec<usize> = encodings
                .iter()
                .map(|e| e.get_ids().len().min(MAX_PAIR_TOKENS))
                .collect();
            stats.truncated += encodings
                .iter()
                .filter(|e| !e.get_overflowing().is_empty())
                .count();

            for rows in group_by_padded_cost(&lengths, MAX_BATCH_ROWS, PADDED_TOKEN_BUDGET) {
                if !keep_going() {
                    break;
                }
                let seq_len = rows.iter().map(|&i| lengths[i]).max().unwrap_or(0);
                if seq_len == 0 {
                    continue;
                }
                stats.forward_passes += 1;
                stats.padded_token_slots += rows.len() * seq_len;
                stats.real_tokens += rows.iter().map(|&i| lengths[i]).sum::<usize>();
                let logits = self.forward_padded(&encodings, &rows, &lengths, seq_len)?;
                for (i, l) in rows.into_iter().zip(logits) {
                    out[base + i] = Some(l);
                }
            }
        }
        stats.unscored = out.iter().filter(|o| o.is_none()).count();
        Ok((out, stats))
    }

    /// One rectangular forward pass over `rows`, right-padded to `seq_len`.
    fn forward_padded(
        &self,
        encodings: &[Encoding],
        rows: &[usize],
        lengths: &[usize],
        seq_len: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let batch = rows.len();
        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut types: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for &row in rows {
            let len = lengths[row];
            let pad = seq_len - len;
            ids.extend_from_slice(&encodings[row].get_ids()[..len]);
            ids.resize(ids.len() + pad, self.pad_id);
            // Segment ids come from the tokenizer's own post-processor: BERT
            // marks the second text 1, XLM-R and DeBERTa-v3 mark everything 0.
            types.extend_from_slice(&encodings[row].get_type_ids()[..len]);
            types.resize(types.len() + pad, 0);
            mask.extend_from_slice(&encodings[row].get_attention_mask()[..len]);
            mask.resize(mask.len() + pad, 0);
        }
        let shape = (batch, seq_len);
        let input_ids =
            Tensor::from_vec(ids, shape, &self.device).map_err(|e| anyhow!("input_ids: {e}"))?;
        let type_ids = Tensor::from_vec(types, shape, &self.device)
            .map_err(|e| anyhow!("token_type_ids: {e}"))?;
        let attention = Tensor::from_vec(mask, shape, &self.device)
            .map_err(|e| anyhow!("attention_mask: {e}"))?;

        let logits = match &self.encoder {
            Encoder::Bert {
                model,
                pooler,
                classifier,
            } => {
                let hidden = model
                    .forward(&input_ids, &type_ids, Some(&attention))
                    .map_err(|e| anyhow!("bert forward: {e}"))?;
                // `BertForSequenceClassification`: tanh(dense([CLS])) → linear.
                let cls = hidden
                    .i((.., 0))
                    .and_then(|t| t.contiguous())
                    .map_err(|e| anyhow!("take [CLS]: {e}"))?;
                pooler
                    .forward(&cls)
                    .and_then(|t| t.tanh())
                    .and_then(|t| classifier.forward(&t))
                    .map_err(|e| anyhow!("bert classification head: {e}"))?
            }
            Encoder::XlmRoberta(model) => model
                .forward(&input_ids, &attention, &type_ids)
                .map_err(|e| anyhow!("xlm-roberta forward: {e}"))?,
            Encoder::DebertaV2(model) => model
                .forward(&input_ids, Some(type_ids), Some(attention))
                .map_err(|e| anyhow!("deberta-v2 forward: {e}"))?,
        };
        logits
            .to_dtype(DType::F32)
            .and_then(|t| t.to_vec2::<f32>())
            .map_err(|e| anyhow!("read logits: {e}"))
    }
}

/// `id2label` in index order; one anonymous label when the config has none.
fn labels_from_config(raw: &serde_json::Value) -> Vec<String> {
    if let Some(map) = raw.get("id2label").and_then(|v| v.as_object()) {
        let mut pairs: Vec<(usize, String)> = map
            .iter()
            .filter_map(|(k, v)| Some((k.parse::<usize>().ok()?, v.as_str()?.to_string())))
            .collect();
        pairs.sort_by_key(|(i, _)| *i);
        if !pairs.is_empty() && pairs.iter().enumerate().all(|(n, (i, _))| n == *i) {
            return pairs.into_iter().map(|(_, l)| l).collect();
        }
    }
    let n = raw
        .get("num_labels")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    (0..n).map(|i| format!("LABEL_{i}")).collect()
}

/// Streamed sha256 of a file.
pub fn file_sha256(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

fn files_in(dir: &Path) -> Option<PairModelFiles> {
    let files = PairModelFiles {
        config: dir.join(FILES[0]),
        tokenizer: dir.join(FILES[1]),
        weights: dir.join(FILES[2]),
    };
    (files.config.exists() && files.tokenizer.exists() && files.weights.exists()).then_some(files)
}

/// The Hugging Face cache root this process would use.
fn hub_cache_root(cache_dir: Option<&Path>) -> PathBuf {
    match cache_dir {
        Some(dir) => dir.to_path_buf(),
        None => hf_hub::Cache::from_env().path().clone(),
    }
}

/// Look for the three files in the hub cache without touching the network.
///
/// Walks `snapshots/` directly instead of going through `refs/`: a snapshot
/// fetched by commit with the Python CLI has no ref file, and an operator who
/// pre-seeded the cache that way must not be told the model is missing.
fn resolve_cached(source: &PairModelSource) -> Option<PairModelFiles> {
    let repo_dir = hub_cache_root(source.cache_dir.as_deref())
        .join(format!("models--{}", source.repo.replace('/', "--")));
    let snapshots = repo_dir.join("snapshots");
    if let Some(rev) = &source.revision {
        if let Some(found) = files_in(&snapshots.join(rev)) {
            return Some(found);
        }
        // A branch name: follow the ref.
        let commit = std::fs::read_to_string(repo_dir.join("refs").join(rev)).ok()?;
        return files_in(&snapshots.join(commit.trim()));
    }
    let commit = std::fs::read_to_string(repo_dir.join("refs").join("main")).ok()?;
    files_in(&snapshots.join(commit.trim()))
}

/// Whether [`resolve`] would have to download anything.
pub fn is_on_disk(source: &PairModelSource) -> bool {
    match &source.local_dir {
        Some(dir) => files_in(dir).is_some(),
        None => resolve_cached(source).is_some(),
    }
}

/// Resolve the three model files: local directory, then cache, then hub.
pub fn resolve(source: &PairModelSource) -> Result<PairModelFiles> {
    if let Some(dir) = &source.local_dir {
        return files_in(dir).ok_or_else(|| {
            anyhow!(
                "model directory {} must hold config.json, tokenizer.json and model.safetensors \
                 (candle reads safetensors, not pytorch_model.bin)",
                dir.display()
            )
        });
    }
    if let Some(found) = resolve_cached(source) {
        return Ok(found);
    }
    if !source.allow_download {
        bail!(
            "model `{}` is not on disk and downloads are disabled: fetch config.json, \
             tokenizer.json and model.safetensors from https://huggingface.co/{} and point the \
             model directory at the folder holding them",
            source.repo,
            source.repo
        );
    }

    use hf_hub::api::sync::ApiBuilder;
    use hf_hub::{Repo, RepoType};
    let mut builder = ApiBuilder::new().with_progress(false);
    if let Some(dir) = &source.cache_dir {
        builder = builder.with_cache_dir(dir.clone());
    }
    let api = builder
        .build()
        .with_context(|| "init Hugging Face hub client")?;
    let repo = match &source.revision {
        Some(rev) => Repo::with_revision(source.repo.clone(), RepoType::Model, rev.clone()),
        None => Repo::new(source.repo.clone(), RepoType::Model),
    };
    let repo = api.repo(repo);
    let mut paths = Vec::with_capacity(FILES.len());
    for name in FILES {
        paths.push(
            repo.get(name)
                .with_context(|| format!("fetch {name} for {}", source.repo))?,
        );
    }
    Ok(PairModelFiles {
        config: paths[0].clone(),
        tokenizer: paths[1].clone(),
        weights: paths[2].clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_id2label_order_and_default_to_one() {
        let nli = serde_json::json!({"id2label": {"1": "not_entailment", "0": "entailment"}});
        assert_eq!(labels_from_config(&nli), ["entailment", "not_entailment"]);
        let ce = serde_json::json!({"id2label": {"0": "LABEL_0"}});
        assert_eq!(labels_from_config(&ce), ["LABEL_0"]);
        assert_eq!(labels_from_config(&serde_json::json!({})), ["LABEL_0"]);
        assert_eq!(
            labels_from_config(&serde_json::json!({"num_labels": 3})).len(),
            3
        );
        // A gap in the ids is not a usable label table.
        let gap = serde_json::json!({"id2label": {"0": "a", "2": "c"}, "num_labels": 2});
        assert_eq!(labels_from_config(&gap), ["LABEL_0", "LABEL_1"]);
    }

    #[test]
    fn an_offline_source_with_nothing_on_disk_says_how_to_fix_it() {
        let dir = std::env::temp_dir().join(format!("xerj-seqcls-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = PairModelSource {
            repo: "org/model".into(),
            revision: Some("abc".into()),
            cache_dir: Some(dir.clone()),
            local_dir: None,
            allow_download: false,
            weights_sha256: None,
        };
        assert!(!is_on_disk(&source));
        let err = resolve(&source).unwrap_err().to_string();
        assert!(err.contains("downloads are disabled"), "{err}");
        assert!(err.contains("huggingface.co/org/model"), "{err}");

        let local = PairModelSource {
            local_dir: Some(dir.clone()),
            ..source
        };
        let err = resolve(&local).unwrap_err().to_string();
        assert!(err.contains("model.safetensors"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_is_found_by_commit_without_a_ref_file() {
        let root = std::env::temp_dir().join(format!("xerj-seqcls-cache-{}", std::process::id()));
        let snap = root
            .join("models--org--model")
            .join("snapshots")
            .join("c0ffee");
        std::fs::create_dir_all(&snap).unwrap();
        for f in FILES {
            std::fs::write(snap.join(f), b"x").unwrap();
        }
        let source = PairModelSource {
            repo: "org/model".into(),
            revision: Some("c0ffee".into()),
            cache_dir: Some(root.clone()),
            local_dir: None,
            allow_download: false,
            weights_sha256: None,
        };
        assert!(is_on_disk(&source));
        assert_eq!(resolve(&source).unwrap().weights, snap.join(FILES[2]));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Live: downloads the MiniLM cross-encoder (~91 MB) and checks the two
    /// logits its model card publishes for this exact input
    /// (`[ 8.607138 -4.320078]`). `#[ignore]`d — run explicitly:
    ///   cargo test -p xerj-ai --features neural seqcls -- --ignored --nocapture
    #[test]
    #[ignore]
    fn minilm_cross_encoder_reproduces_its_model_card() {
        let model = PairClassifier::load(&PairModelSource {
            repo: "cross-encoder/ms-marco-MiniLM-L6-v2".into(),
            revision: None,
            cache_dir: None,
            local_dir: None,
            allow_download: true,
            weights_sha256: None,
        })
        .expect("load cross-encoder");
        assert_eq!(model.labels().len(), 1);
        let q = "How many people live in Berlin?".to_string();
        let pairs = vec![
            (
                q.clone(),
                "Berlin had a population of 3,520,031 registered inhabitants in an area of \
                 891.82 square kilometers."
                    .to_string(),
            ),
            (q, "Berlin is well known for its museums.".to_string()),
        ];
        let (logits, stats) = model.logits_blocking(&pairs, &|| true).expect("score");
        let got: Vec<f32> = logits.into_iter().map(|l| l.unwrap()[0]).collect();
        eprintln!("logits = {got:?}  stats = {stats:?}");
        assert!((got[0] - 8.607138).abs() < 0.01, "{got:?}");
        assert!((got[1] + 4.320078).abs() < 0.01, "{got:?}");
        assert_eq!(stats.unscored, 0);
    }

    /// Live: the XLM-RoBERTa path against the numbers `BAAI/bge-reranker-v2-m3`'s
    /// model card prints (`[-8.1875, 5.26171875]` for the panda pair, `-5.65234375`
    /// for `['query', 'passage']`). The card ran in fp16; this runs in f32, so
    /// agreement is to fp16 precision, not bit-exact. Downloads ~2.3 GB:
    ///   cargo test -p xerj-ai --features neural seqcls -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bge_reranker_v2_m3_reproduces_its_model_card() {
        let model = PairClassifier::load(&PairModelSource {
            repo: "BAAI/bge-reranker-v2-m3".into(),
            revision: Some("953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e".into()),
            cache_dir: None,
            local_dir: None,
            allow_download: true,
            weights_sha256: None,
        })
        .expect("load bge-reranker-v2-m3");
        assert_eq!(model.model_type(), "xlm-roberta");
        let pairs: Vec<(String, String)> = [
            ("query", "passage"),
            ("what is panda?", "hi"),
            (
                "what is panda?",
                "The giant panda (Ailuropoda melanoleuca), sometimes called a panda bear or \
                 simply panda, is a bear species endemic to China.",
            ),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let (logits, _) = model.logits_blocking(&pairs, &|| true).expect("score");
        let got: Vec<f32> = logits.into_iter().map(|l| l.unwrap()[0]).collect();
        eprintln!("logits = {got:?}");
        for (g, want) in got.iter().zip([-5.652_343_8f32, -8.1875, 5.261_718_8]) {
            assert!((g - want).abs() < 0.02, "{got:?}");
        }
    }

    /// Live: a pair's logit must not depend on what it is batched with. Right
    /// padding, the attention mask and (for XLM-R) padding-aware position ids
    /// all have to be right for this to hold; a mistake in any of them shows up
    /// as a score that changes with the length of the longest neighbour.
    #[test]
    #[ignore]
    fn a_pair_scores_the_same_alone_and_padded_next_to_a_long_one() {
        let model = PairClassifier::load(&PairModelSource {
            repo: "cross-encoder/ms-marco-MiniLM-L6-v2".into(),
            revision: None,
            cache_dir: None,
            local_dir: None,
            allow_download: true,
            weights_sha256: None,
        })
        .expect("load cross-encoder");
        let short = (
            "How many people live in Berlin?".to_string(),
            "Berlin is well known for its museums.".to_string(),
        );
        let long = (
            short.0.clone(),
            "Berlin is a city with many museums and parks and long streets ".repeat(60),
        );
        let (alone, _) = model
            .logits_blocking(std::slice::from_ref(&short), &|| true)
            .expect("alone");
        let (batched, stats) = model
            .logits_blocking(&[short.clone(), long], &|| true)
            .expect("batched");
        let (a, b) = (
            alone[0].as_ref().unwrap()[0],
            batched[0].as_ref().unwrap()[0],
        );
        eprintln!("alone = {a}, batched = {b}, stats = {stats:?}");
        assert!((a - b).abs() < 1e-3, "alone {a} vs batched {b}");
        assert_eq!(
            stats.truncated, 1,
            "the 60-sentence document is cut at 512 tokens"
        );
    }
}
