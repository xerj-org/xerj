//! Unified embedding backend.
//!
//! XERJ embeds `semantic_text` fields through one of three interchangeable
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
//!
//! [`Embedder::is_active`] distinguishes a *real* embedder (proxy or neural)
//! from the lexical fallback — the query path only auto-embeds against the
//! lexical backend for `semantic_text` fields, matching how they were
//! embedded at ingest.

use anyhow::{anyhow, Result};

use crate::embed::EmbeddingProxy;
use crate::local::{local_embed, DEFAULT_DIMS};

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

    /// `true` when a *real* embedder (neural or external proxy) is configured;
    /// `false` for the lexical fallback. The query path uses this to decide
    /// whether to embed arbitrary query text (active) or restrict to
    /// `semantic_text` fields embedded the same lexical way at ingest.
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
        }
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
