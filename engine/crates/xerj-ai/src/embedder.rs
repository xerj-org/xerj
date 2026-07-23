//! Unified embedding backend.
//!
//! XERJ embeds `semantic_text` fields through one of four interchangeable
//! backends, all behind a single [`Embedder`] handle so the engine's ingest
//! and query paths never branch on the backend:
//!
//!   * [`Embedder::Lexical`] — the zero-dependency built-in feature-hash
//!     embedder ([`crate::local::local_embed`]). Deterministic, offline,
//!     fast — but lexical, *not* neural semantic understanding. This is the
//!     honest default when nothing else is configured.
//!   * [`Embedder::Proxy`] — an external OpenAI-compatible `/v1/embeddings`
//!     service ([`crate::embed::EmbeddingProxy`]). Bring any model/provider.
//!   * [`Embedder::Neural`] — the built-in BERT sentence encoder
//!     ([`crate::neural`]), running in-process via `candle`. Compiled only
//!     under the `neural` cargo feature; the model is loaded lazily on first
//!     use (download-on-first-run) so startup stays instant.
//!   * [`Embedder::Onnx`] — the experimental in-process ONNX Runtime backend.
//!     It is compiled only under `onnx-experimental`, requires explicit model
//!     and tokenizer paths, and loads the model lazily on first use.
//!
//! [`Embedder::is_active`] distinguishes a neural/proxy/ONNX backend from the
//! lexical fallback. The query path nevertheless auto-embeds `semantic_text`
//! fields with whichever backend indexed them, including lexical, so ingest
//! and query vectors always use the same embedding identity.

use anyhow::{anyhow, Result};

use crate::embed::EmbeddingProxy;
use crate::local::{local_embed, DEFAULT_DIMS};

#[cfg(feature = "onnx-experimental")]
type OnnxCell = tokio::sync::OnceCell<std::sync::Arc<crate::onnx::OnnxEmbedder>>;

#[cfg(feature = "onnx-experimental")]
struct OnnxShared {
    cell: OnnxCell,
    calls: std::sync::Arc<tokio::sync::Semaphore>,
    bytes: std::sync::Arc<tokio::sync::Semaphore>,
}

#[cfg(feature = "neural")]
type NeuralCell = tokio::sync::OnceCell<std::sync::Arc<crate::neural::NeuralEmbedder>>;

/// Process-scoped registry of lazily loaded neural models. Every index builds
/// its own [`Embedder`], but indices using the same complete neural
/// configuration must not each load another copy of the ~90 MB model.
///
/// Weak values are intentional: the registry coordinates sharing without
/// extending a model's lifetime after the last index using it is dropped.
#[cfg(feature = "neural")]
fn shared_neural_cell(cfg: &crate::neural::NeuralConfig) -> std::sync::Arc<NeuralCell> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, Weak};

    static CELLS: OnceLock<Mutex<HashMap<crate::neural::NeuralConfig, Weak<NeuralCell>>>> =
        OnceLock::new();

    let mut cells = CELLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cell) = cells.get(cfg).and_then(Weak::upgrade) {
        return cell;
    }

    // Opportunistically discard entries whose final handle has gone away.
    cells.retain(|_, cell| cell.strong_count() > 0);
    let cell = std::sync::Arc::new(NeuralCell::new());
    cells.insert(cfg.clone(), std::sync::Arc::downgrade(&cell));
    cell
}

/// A backend-agnostic text embedder shared across the engine.
pub enum Embedder {
    /// Built-in lexical feature-hash embedder (no model, no network).
    Lexical,
    /// External OpenAI-compatible embedding service.
    Proxy(EmbeddingProxy),
    /// Built-in neural BERT embedder (candle). Lazily loaded on first use.
    #[cfg(feature = "neural")]
    Neural(NeuralHandle),
    /// Experimental local ONNX Runtime sentence encoder.
    #[cfg(feature = "onnx-experimental")]
    Onnx(OnnxHandle),
}

impl Embedder {
    /// The zero-config lexical fallback.
    pub fn lexical() -> Self {
        Embedder::Lexical
    }

    /// Wrap an already-constructed external embedding proxy.
    pub fn proxy(proxy: EmbeddingProxy) -> Self {
        Embedder::Proxy(proxy)
    }

    /// A lazily-loaded built-in neural embedder.
    #[cfg(feature = "neural")]
    pub fn neural(cfg: crate::neural::NeuralConfig) -> Self {
        Embedder::Neural(NeuralHandle::new(cfg))
    }

    #[cfg(feature = "onnx-experimental")]
    pub fn onnx(cfg: OnnxConfig) -> Self {
        Embedder::Onnx(OnnxHandle::new(cfg))
    }

    /// `true` when a Candle neural, experimental ONNX, or external proxy
    /// embedder is configured; `false` for the lexical fallback. The query
    /// path uses this to decide whether to embed arbitrary query text (active)
    /// or restrict to `semantic_text` fields embedded the same lexical way at
    /// ingest.
    pub fn is_active(&self) -> bool {
        !matches!(self, Embedder::Lexical)
    }

    /// A short human-readable label for logs / honesty reporting.
    pub fn describe(&self) -> &'static str {
        match self {
            Embedder::Lexical => "lexical feature-hash (built-in, 384-dim, non-neural)",
            Embedder::Proxy(_) => "external proxy (OpenAI-compatible /v1/embeddings)",
            #[cfg(feature = "neural")]
            Embedder::Neural(_) => "neural BERT (built-in, candle)",
            #[cfg(feature = "onnx-experimental")]
            Embedder::Onnx(_) => "neural BERT (experimental ONNX Runtime)",
        }
    }

    /// Embed a batch of texts into vectors. Order matches the input.
    pub async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        match self {
            Embedder::Lexical => Ok(texts.iter().map(|t| local_embed(t, DEFAULT_DIMS)).collect()),
            Embedder::Proxy(proxy) => proxy
                .embed_batch(texts)
                .await
                .map_err(|e| anyhow!("embedding proxy failed: {e}")),
            #[cfg(feature = "neural")]
            Embedder::Neural(handle) => handle.embed_batch(texts).await,
            #[cfg(feature = "onnx-experimental")]
            Embedder::Onnx(handle) => handle.embed_batch(texts).await,
        }
    }
}

#[cfg(feature = "onnx-experimental")]
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct OnnxConfig {
    pub model_path: std::path::PathBuf,
    pub tokenizer_path: std::path::PathBuf,
    /// Content fingerprints are part of the shared-session cache key and are
    /// rechecked immediately before load, preventing same-path asset swaps.
    pub model_sha256: String,
    pub tokenizer_sha256: String,
    pub intra_threads: usize,
    pub microbatch: crate::onnx::MicrobatchConfig,
    pub max_inflight_calls: usize,
    pub max_input_bytes_per_call: usize,
    pub max_inflight_input_bytes: usize,
}

#[cfg(feature = "onnx-experimental")]
#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct OnnxAdmissionError {
    pub reason: String,
}

#[cfg(feature = "onnx-experimental")]
pub struct OnnxHandle {
    cfg: OnnxConfig,
    shared: std::sync::Arc<OnnxShared>,
}

#[cfg(feature = "onnx-experimental")]
impl OnnxHandle {
    fn new(cfg: OnnxConfig) -> Self {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, Weak};
        static CELLS: OnceLock<Mutex<HashMap<OnnxConfig, Weak<OnnxShared>>>> = OnceLock::new();
        let mut cells = CELLS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(shared) = cells.get(&cfg).and_then(Weak::upgrade) {
            return Self { cfg, shared };
        }
        cells.retain(|_, shared| shared.strong_count() > 0);
        let shared = std::sync::Arc::new(OnnxShared {
            cell: OnnxCell::new(),
            calls: std::sync::Arc::new(tokio::sync::Semaphore::new(cfg.max_inflight_calls.max(1))),
            bytes: std::sync::Arc::new(tokio::sync::Semaphore::new(
                cfg.max_inflight_input_bytes.max(1),
            )),
        });
        cells.insert(cfg.clone(), std::sync::Arc::downgrade(&shared));
        Self { cfg, shared }
    }

    async fn get(&self) -> Result<std::sync::Arc<crate::onnx::OnnxEmbedder>> {
        self.shared
            .cell
            .get_or_try_init(|| async {
                let cfg = self.cfg.clone();
                let model = tokio::task::spawn_blocking(move || {
                    let model_bytes = std::fs::read(&cfg.model_path).map_err(|e| {
                        anyhow!("read ONNX model {}: {e}", cfg.model_path.display())
                    })?;
                    let tokenizer_bytes = std::fs::read(&cfg.tokenizer_path).map_err(|e| {
                        anyhow!("read ONNX tokenizer {}: {e}", cfg.tokenizer_path.display())
                    })?;
                    let actual_model = sha256_bytes(&model_bytes);
                    let actual_tokenizer = sha256_bytes(&tokenizer_bytes);
                    if actual_model != cfg.model_sha256 || actual_tokenizer != cfg.tokenizer_sha256
                    {
                        return Err(anyhow!(
                            "ONNX assets changed after configuration; refusing to load a \
                             different vector space from the same path (model expected {}, \
                             actual {}; tokenizer expected {}, actual {}). Restart with the \
                             intended assets or reindex under a new prefix",
                            cfg.model_sha256,
                            actual_model,
                            cfg.tokenizer_sha256,
                            actual_tokenizer
                        ));
                    }
                    let embedder = crate::onnx::OnnxEmbedder::load_bytes(
                        &model_bytes,
                        &tokenizer_bytes,
                        cfg.intra_threads,
                    )?;
                    tracing::info!(
                        model_sha256 = %cfg.model_sha256,
                        tokenizer_sha256 = %cfg.tokenizer_sha256,
                        dimensions = crate::onnx::DIMS,
                        "experimental ONNX embedding backend active; first semantic inference loaded the verified model"
                    );
                    Ok::<_, anyhow::Error>(embedder)
                })
                .await
                .map_err(|e| anyhow!("ONNX model load task panicked: {e}"))??;
                Ok::<_, anyhow::Error>(std::sync::Arc::new(model))
            })
            .await
            .cloned()
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() > self.cfg.microbatch.max_pending {
            return Err(anyhow::Error::new(OnnxAdmissionError {
                reason: format!(
                    "ONNX request rejected before tokenization: {} texts exceed max_pending={}; \
                     split the request and retry",
                    texts.len(),
                    self.cfg.microbatch.max_pending
                ),
            }));
        }
        let input_bytes = texts
            .iter()
            .try_fold(0usize, |total, text| total.checked_add(text.len()))
            .ok_or_else(|| {
                anyhow::Error::new(OnnxAdmissionError {
                    reason: "ONNX input byte count overflowed; split the request".into(),
                })
            })?;
        let (_call_permit, _byte_permits) = self.try_admit(input_bytes)?;
        let model = self.get().await?;
        let limits = self.cfg.microbatch;
        tokio::task::spawn_blocking(move || model.embed_scheduled_blocking(&texts, limits))
            .await
            .map_err(|e| anyhow!("ONNX embed task panicked: {e}"))?
    }

    fn try_admit(
        &self,
        input_bytes: usize,
    ) -> Result<(
        tokio::sync::OwnedSemaphorePermit,
        tokio::sync::OwnedSemaphorePermit,
    )> {
        if input_bytes > self.cfg.max_input_bytes_per_call {
            return Err(anyhow::Error::new(OnnxAdmissionError {
                reason: format!(
                    "ONNX request rejected before tokenization: input is {input_bytes} bytes, \
                     per-call limit is {} bytes; split the request and retry",
                    self.cfg.max_input_bytes_per_call
                ),
            }));
        }
        let byte_permits = u32::try_from(input_bytes.max(1)).map_err(|_| {
            anyhow::Error::new(OnnxAdmissionError {
                reason: format!(
                    "ONNX request rejected before tokenization: {input_bytes} input bytes \
                     exceed the semaphore permit range; split the request"
                ),
            })
        })?;
        let call = self
            .shared
            .calls
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                anyhow::Error::new(OnnxAdmissionError {
                    reason: format!(
                        "ONNX embedding admission full: {} calls are already admitted \
                         (loading, running, or awaiting the serialized session); retry with backoff",
                        self.cfg.max_inflight_calls
                    ),
                })
            })?;
        let bytes = self
            .shared
            .bytes
            .clone()
            .try_acquire_many_owned(byte_permits)
            .map_err(|_| {
                anyhow::Error::new(OnnxAdmissionError {
                    reason: format!(
                        "ONNX embedding byte budget full: request needs {input_bytes} bytes, \
                         global in-flight limit is {} bytes; retry with backoff",
                        self.cfg.max_inflight_input_bytes
                    ),
                })
            })?;
        Ok((call, bytes))
    }
}

#[cfg(feature = "onnx-experimental")]
fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(all(test, feature = "onnx-experimental"))]
mod onnx_handle_tests {
    use super::{OnnxConfig, OnnxHandle};
    use crate::onnx::MicrobatchConfig;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn cfg(model_sha256: &str) -> OnnxConfig {
        OnnxConfig {
            model_path: PathBuf::from("/tmp/model.onnx"),
            tokenizer_path: PathBuf::from("/tmp/tokenizer.json"),
            model_sha256: model_sha256.into(),
            tokenizer_sha256: "tokenizer-hash".into(),
            intra_threads: 4,
            microbatch: MicrobatchConfig::default(),
            max_inflight_calls: 2,
            max_input_bytes_per_call: 10,
            max_inflight_input_bytes: 12,
        }
    }

    #[test]
    fn same_paths_with_different_content_hashes_never_share_session_cell() {
        let first = OnnxHandle::new(cfg("model-a"));
        let second = OnnxHandle::new(cfg("model-b"));
        assert!(!Arc::ptr_eq(&first.shared, &second.shared));
    }

    #[test]
    fn global_admission_caps_calls_and_releases_permits() {
        let handle = OnnxHandle::new(cfg("admission-calls"));
        let first = handle.try_admit(1).unwrap();
        let second = handle.try_admit(1).unwrap();
        let error = handle.try_admit(1).unwrap_err().to_string();
        assert!(error.contains("admission full"), "{error}");
        drop(first);
        assert!(handle.try_admit(1).is_ok());
        drop(second);
    }

    #[test]
    fn concurrent_handles_share_global_call_cap() {
        let config = cfg("admission-concurrent");
        let first = OnnxHandle::new(config.clone());
        let second = OnnxHandle::new(config.clone());
        let rejected = OnnxHandle::new(config);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let release = Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            for handle in [first, second] {
                let ready_tx = ready_tx.clone();
                let release = Arc::clone(&release);
                scope.spawn(move || {
                    let _permit = handle.try_admit(1).unwrap();
                    ready_tx.send(()).unwrap();
                    release.wait();
                });
            }
            ready_rx.recv().unwrap();
            ready_rx.recv().unwrap();
            let error = rejected.try_admit(1).unwrap_err().to_string();
            assert!(error.contains("admission full"), "{error}");
            release.wait();
        });
        assert!(rejected.try_admit(1).is_ok(), "permits must release");
    }

    #[test]
    fn global_byte_budget_and_per_call_limit_are_enforced_before_work() {
        let handle = OnnxHandle::new(cfg("admission-bytes"));
        let held = handle.try_admit(8).unwrap();
        let error = handle.try_admit(5).unwrap_err().to_string();
        assert!(error.contains("byte budget full"), "{error}");
        drop(held);
        assert!(handle.try_admit(5).is_ok());

        let error = handle.try_admit(11).unwrap_err().to_string();
        assert!(error.contains("per-call limit"), "{error}");
    }

    #[tokio::test]
    async fn document_cap_rejects_before_model_load_or_tokenization() {
        let mut config = cfg("admission-docs");
        config.microbatch.max_pending = 1;
        let handle = OnnxHandle::new(config);
        let error = handle
            .embed_batch(vec!["one".into(), "two".into()])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("before tokenization"), "{error}");
        assert!(error.contains("max_pending=1"), "{error}");
        assert!(
            handle.shared.cell.get().is_none(),
            "model must remain unloaded"
        );
    }
}

/// Lazily-loaded neural backend. The heavy model is loaded once, on the first
/// `embed_batch`, off the async executor via `spawn_blocking`; every later
/// call reuses the shared `Arc`.
#[cfg(feature = "neural")]
pub struct NeuralHandle {
    cfg: crate::neural::NeuralConfig,
    cell: std::sync::Arc<NeuralCell>,
}

#[cfg(feature = "neural")]
impl NeuralHandle {
    pub fn new(cfg: crate::neural::NeuralConfig) -> Self {
        let cell = shared_neural_cell(&cfg);
        Self { cfg, cell }
    }

    /// Get-or-load the model. First caller pays the (blocking) load / download;
    /// concurrent callers await the same init.
    async fn get(&self) -> Result<std::sync::Arc<crate::neural::NeuralEmbedder>> {
        self.cell
            .get_or_try_init(|| async {
                let cfg = self.cfg.clone();
                let model =
                    tokio::task::spawn_blocking(move || crate::neural::NeuralEmbedder::load(&cfg))
                        .await
                        .map_err(|e| anyhow!("neural model load task panicked: {e}"))??;
                Ok::<_, anyhow::Error>(std::sync::Arc::new(model))
            })
            .await
            .cloned()
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let model = self.get().await?;
        tokio::task::spawn_blocking(move || model.embed_blocking(&texts))
            .await
            .map_err(|e| anyhow!("neural embed task panicked: {e}"))?
    }
}

#[cfg(all(test, feature = "neural"))]
mod neural_handle_tests {
    use super::NeuralHandle;
    use crate::neural::NeuralConfig;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn identical_configs_share_lazy_model_cell() {
        let cfg = NeuralConfig {
            model_id: "test/model-shared".into(),
            cache_dir: Some(PathBuf::from("/tmp/xerj-neural-shared-cache")),
            local_dir: None,
        };
        let first = NeuralHandle::new(cfg.clone());
        let second = NeuralHandle::new(cfg);

        assert!(Arc::ptr_eq(&first.cell, &second.cell));
        assert!(first.cell.get().is_none(), "construction must remain lazy");
    }

    #[test]
    fn distinct_configs_do_not_share_lazy_model_cell() {
        let first = NeuralHandle::new(NeuralConfig {
            model_id: "test/model-a".into(),
            cache_dir: None,
            local_dir: None,
        });
        let second = NeuralHandle::new(NeuralConfig {
            model_id: "test/model-b".into(),
            cache_dir: None,
            local_dir: None,
        });

        assert!(!Arc::ptr_eq(&first.cell, &second.cell));
    }

    #[test]
    fn registry_does_not_keep_unused_cells_alive() {
        let cfg = NeuralConfig {
            model_id: "test/model-reclaimable".into(),
            cache_dir: None,
            local_dir: None,
        };
        let weak = {
            let handle = NeuralHandle::new(cfg.clone());
            Arc::downgrade(&handle.cell)
        };
        assert!(weak.upgrade().is_none());

        let replacement = NeuralHandle::new(cfg);
        assert!(replacement.cell.get().is_none());
    }
}
