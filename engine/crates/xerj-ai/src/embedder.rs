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
            Embedder::Lexical => Ok(texts
                .iter()
                .map(|t| local_embed(t, DEFAULT_DIMS))
                .collect()),
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
    cell: tokio::sync::OnceCell<std::sync::Arc<crate::neural::NeuralEmbedder>>,
}

#[cfg(feature = "neural")]
impl NeuralHandle {
    pub fn new(cfg: crate::neural::NeuralConfig) -> Self {
        Self {
            cfg,
            cell: tokio::sync::OnceCell::new(),
        }
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
