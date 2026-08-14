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
struct CancellationSafeInit<T> {
    result: std::sync::OnceLock<std::result::Result<std::sync::Arc<T>, std::sync::Arc<str>>>,
    started: std::sync::atomic::AtomicBool,
    slow_warning_emitted: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

#[cfg(feature = "onnx-experimental")]
struct InitCompletionGuard<T: Send + Sync + 'static> {
    shared: std::sync::Arc<CancellationSafeInit<T>>,
    armed: bool,
}

#[cfg(feature = "onnx-experimental")]
impl<T: Send + Sync + 'static> Drop for InitCompletionGuard<T> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let fallback = std::sync::Arc::<str>::from(
            "ONNX initialization worker panicked before publishing a terminal result",
        );
        let _ = self.shared.result.set(Err(fallback));
        self.shared.notify.notify_waiters();
    }
}

#[cfg(feature = "onnx-experimental")]
impl<T: Send + Sync + 'static> CancellationSafeInit<T> {
    fn new() -> Self {
        Self {
            result: std::sync::OnceLock::new(),
            started: std::sync::atomic::AtomicBool::new(false),
            slow_warning_emitted: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    async fn get_or_spawn<F>(
        self: &std::sync::Arc<Self>,
        thread_name: &str,
        load: F,
    ) -> Result<std::sync::Arc<T>>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        use std::sync::atomic::Ordering;

        if !self.started.swap(true, Ordering::AcqRel) {
            let shared = std::sync::Arc::clone(self);
            let thread_name = thread_name.to_string();
            let worker_name = thread_name.clone();
            let started = std::time::Instant::now();
            tracing::info!(%thread_name, "ONNX lazy initialization scheduled");
            let spawn = std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || {
                    let mut completion = InitCompletionGuard {
                        shared: std::sync::Arc::clone(&shared),
                        armed: true,
                    };
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match load() {
                            Ok(value) => Ok(std::sync::Arc::new(value)),
                            Err(error) => Err(std::sync::Arc::<str>::from(format!("{error:#}"))),
                        }))
                        .unwrap_or_else(|payload| {
                            let detail = if let Some(message) = payload.downcast_ref::<&str>() {
                                (*message).to_string()
                            } else if let Some(message) = payload.downcast_ref::<String>() {
                                message.clone()
                            } else {
                                "non-string panic payload".to_string()
                            };
                            Err(std::sync::Arc::<str>::from(format!(
                                "ONNX initialization loader panicked: {detail}"
                            )))
                        });
                    let elapsed_ms = started.elapsed().as_millis();
                    let _ = shared.result.set(result);
                    shared.notify.notify_waiters();
                    completion.armed = false;

                    let published = shared
                        .result
                        .get()
                        .expect("initialization result was just published");
                    match published {
                        Ok(_) => tracing::info!(
                            thread_name = %worker_name,
                            elapsed_ms,
                            "ONNX lazy initialization completed"
                        ),
                        Err(error) => tracing::error!(
                            thread_name = %worker_name,
                            elapsed_ms,
                            %error,
                            "ONNX lazy initialization failed"
                        ),
                    }
                });
            if let Err(error) = spawn {
                let error = std::sync::Arc::<str>::from(format!(
                    "spawn ONNX initialization thread {thread_name}: {error}"
                ));
                let _ = self.result.set(Err(error));
                self.notify.notify_waiters();
            }
        }

        loop {
            let notified = self.notify.notified();
            if let Some(result) = self.result.get() {
                return result
                    .clone()
                    .map_err(|error| anyhow!("ONNX model initialization failed: {error}"));
            }
            if self
                .slow_warning_emitted
                .load(std::sync::atomic::Ordering::Acquire)
            {
                notified.await;
            } else {
                tokio::select! {
                    () = notified => {}
                    () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                        if !self.slow_warning_emitted.swap(
                            true,
                            std::sync::atomic::Ordering::AcqRel,
                        ) {
                            tracing::warn!(
                                "ONNX lazy initialization is still running after 30 seconds"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(feature = "onnx-experimental")]
struct OnnxShared {
    init: std::sync::Arc<CancellationSafeInit<crate::onnx::OnnxPool>>,
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

    /// How many `embed_batch` calls the engine may keep in flight against this
    /// backend when it has several windows of one request ready to go.
    ///
    /// This is backend policy, not a global number, because the two active
    /// backends fail in opposite directions. In-process inference is CPU-bound
    /// and thread-safe behind `&self`, and running one window at a time left
    /// 2 of 20 cores busy on a semantic ingest (#366) — both reference engines
    /// make expensive per-item ingest work concurrent *by default*
    /// (Lucene's `HnswConcurrentMergeBuilder` spawns a worker per thread and
    /// they "pick the work in batches"; usearch's `executor_stl_t` defaults to
    /// `std::thread::hardware_concurrency()`). An external provider is the
    /// opposite case: k concurrent `/v1/embeddings` requests per bulk turns a
    /// slow ingest into a 429 storm, so the proxy stays at 1 until an operator
    /// says otherwise. The experimental ONNX backend has its own session pool
    /// and admission control and is scheduled by `onnx_session_pool_size`.
    ///
    /// Lucene bounds the same way — a limited pool for intra-operation work
    /// (`ConcurrentMergeScheduler.CachedExecutor`), never a thread per unit.
    pub fn default_inference_concurrency(&self) -> usize {
        match self {
            Embedder::Lexical | Embedder::Proxy(_) => 1,
            #[cfg(feature = "neural")]
            Embedder::Neural(_) => xerj_common::resource::cores().clamp(1, 8),
            #[cfg(feature = "onnx-experimental")]
            Embedder::Onnx(_) => 1,
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
#[derive(Clone)]
pub struct OnnxConfig {
    /// Immutable byte snapshots are shared by identity reporting and lazy
    /// loading. A later same-path replacement cannot change the loaded space.
    pub model_bytes: std::sync::Arc<[u8]>,
    pub tokenizer_bytes: std::sync::Arc<[u8]>,
    pub model_sha256: String,
    pub tokenizer_sha256: String,
    pub intra_threads: usize,
    /// Number of independently constructed ONNX Runtime sessions.
    pub session_pool_size: usize,
    pub microbatch: crate::onnx::MicrobatchConfig,
    pub max_inflight_calls: usize,
    pub max_input_bytes_per_call: usize,
    pub max_inflight_input_bytes: usize,
}

#[cfg(feature = "onnx-experimental")]
impl std::fmt::Debug for OnnxConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxConfig")
            .field("model_sha256", &self.model_sha256)
            .field("tokenizer_sha256", &self.tokenizer_sha256)
            .field("intra_threads", &self.intra_threads)
            .field("session_pool_size", &self.session_pool_size)
            .finish_non_exhaustive()
    }
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct OnnxCacheKey {
    model_sha256: String,
    tokenizer_sha256: String,
    intra_threads: usize,
    session_pool_size: usize,
    microbatch: crate::onnx::MicrobatchConfig,
    max_inflight_calls: usize,
    max_input_bytes_per_call: usize,
    max_inflight_input_bytes: usize,
}

#[cfg(feature = "onnx-experimental")]
impl From<&OnnxConfig> for OnnxCacheKey {
    fn from(cfg: &OnnxConfig) -> Self {
        use sha2::{Digest, Sha256};

        Self {
            // OnnxConfig is public, so its descriptive hash fields are not a
            // construction invariant. Session sharing must be keyed from the
            // bytes the runtime will actually load.
            model_sha256: format!("{:x}", Sha256::digest(&cfg.model_bytes)),
            tokenizer_sha256: format!("{:x}", Sha256::digest(&cfg.tokenizer_bytes)),
            intra_threads: cfg.intra_threads,
            session_pool_size: cfg.session_pool_size,
            microbatch: cfg.microbatch,
            max_inflight_calls: cfg.max_inflight_calls,
            max_input_bytes_per_call: cfg.max_input_bytes_per_call,
            max_inflight_input_bytes: cfg.max_inflight_input_bytes,
        }
    }
}

#[cfg(feature = "onnx-experimental")]
impl OnnxHandle {
    fn new(cfg: OnnxConfig) -> Self {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, Weak};
        static CELLS: OnceLock<Mutex<HashMap<OnnxCacheKey, Weak<OnnxShared>>>> = OnceLock::new();
        let key = OnnxCacheKey::from(&cfg);
        let mut cells = CELLS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(shared) = cells.get(&key).and_then(Weak::upgrade) {
            return Self { cfg, shared };
        }
        cells.retain(|_, shared| shared.strong_count() > 0);
        let shared = std::sync::Arc::new(OnnxShared {
            init: std::sync::Arc::new(CancellationSafeInit::new()),
            calls: std::sync::Arc::new(tokio::sync::Semaphore::new(cfg.max_inflight_calls.max(1))),
            bytes: std::sync::Arc::new(tokio::sync::Semaphore::new(
                cfg.max_inflight_input_bytes.max(1),
            )),
        });
        cells.insert(key, std::sync::Arc::downgrade(&shared));
        Self { cfg, shared }
    }

    async fn get(&self) -> Result<std::sync::Arc<crate::onnx::OnnxPool>> {
        let cfg = self.cfg.clone();
        self.shared
            .init
            .get_or_spawn("xerj-onnx-init", move || {
                    let embedder = crate::onnx::OnnxPool::load_bytes(
                        &cfg.model_bytes,
                        &cfg.tokenizer_bytes,
                        cfg.intra_threads,
                        cfg.session_pool_size,
                    )?;
                    tracing::info!(
                        model_sha256 = %cfg.model_sha256,
                        tokenizer_sha256 = %cfg.tokenizer_sha256,
                        dimensions = crate::onnx::DIMS,
                        session_pool_size = embedder.len(),
                        "experimental ONNX embedding backend active; first semantic inference loaded the verified model"
                    );
                    Ok(embedder)
            })
            .await
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
        let (call_permit, byte_permits) = self.try_admit(input_bytes)?;
        let model = self.get().await?;
        let limits = self.cfg.microbatch;
        tokio::task::spawn_blocking(move || {
            // Admission remains charged until native inference returns even if
            // the async waiter is cancelled.
            let _call_permit = call_permit;
            let _byte_permits = byte_permits;
            model.embed_scheduled_blocking(&texts, limits)
        })
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

#[cfg(all(test, feature = "onnx-experimental"))]
mod onnx_handle_tests {
    use super::{CancellationSafeInit, OnnxConfig, OnnxHandle};
    use crate::onnx::MicrobatchConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn cfg(model_sha256: &str) -> OnnxConfig {
        OnnxConfig {
            model_bytes: Arc::from(format!("model bytes for {model_sha256}").into_bytes()),
            tokenizer_bytes: Arc::from(b"tokenizer bytes".as_slice()),
            model_sha256: model_sha256.into(),
            tokenizer_sha256: "tokenizer-hash".into(),
            intra_threads: 4,
            session_pool_size: 2,
            microbatch: MicrobatchConfig::default(),
            max_inflight_calls: 2,
            max_input_bytes_per_call: 10,
            max_inflight_input_bytes: 12,
        }
    }

    #[test]
    fn different_actual_bytes_never_share_session_cell() {
        let first = cfg("model-a");
        let mut second = cfg("model-b");
        second.model_bytes = Arc::from(b"different model bytes".as_slice());
        let first = OnnxHandle::new(first);
        let second = OnnxHandle::new(second);
        assert!(!Arc::ptr_eq(&first.shared, &second.shared));
    }

    #[test]
    fn identical_bytes_share_even_when_descriptive_hash_fields_differ() {
        let first_cfg = cfg("descriptive-a");
        let mut second_cfg = cfg("descriptive-b");
        second_cfg.model_bytes = first_cfg.model_bytes.clone();
        second_cfg.tokenizer_bytes = first_cfg.tokenizer_bytes.clone();
        let first = OnnxHandle::new(first_cfg);
        let second = OnnxHandle::new(second_cfg);
        assert!(Arc::ptr_eq(&first.shared, &second.shared));
    }

    #[test]
    fn forged_equal_hash_fields_cannot_share_sessions_for_different_bytes() {
        let first = cfg("forged");
        let mut second = cfg("forged");
        second.model_bytes = Arc::from(b"different model bytes".as_slice());
        second.tokenizer_bytes = Arc::from(b"different tokenizer bytes".as_slice());
        let first = OnnxHandle::new(first);
        let second = OnnxHandle::new(second);
        assert!(!Arc::ptr_eq(&first.shared, &second.shared));
    }

    #[test]
    fn session_registry_key_does_not_retain_asset_bytes_after_handle_drop() {
        let config = cfg("releasable-assets");
        let weak_model = Arc::downgrade(&config.model_bytes);
        let weak_tokenizer = Arc::downgrade(&config.tokenizer_bytes);
        let handle = OnnxHandle::new(config);
        drop(handle);
        assert!(weak_model.upgrade().is_none());
        assert!(weak_tokenizer.upgrade().is_none());
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
    async fn cancelled_waiter_keeps_admission_charged_until_blocking_work_returns() {
        let handle = OnnxHandle::new(cfg("cancelled-native-admission"));
        let (call, bytes) = handle.try_admit(8).unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocking = tokio::task::spawn_blocking(move || {
            let _call = call;
            let _bytes = bytes;
            let _ = started_tx.send(());
            release_rx.recv().unwrap();
        });
        started_rx.await.unwrap();
        blocking.abort();
        let error = handle.try_admit(5).unwrap_err().to_string();
        assert!(
            error.contains("byte budget full") || error.contains("admission full"),
            "{error}"
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if handle.try_admit(5).is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("native completion must release admission");
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
            handle.shared.init.result.get().is_none(),
            "model must remain unloaded"
        );
    }

    #[tokio::test]
    async fn first_initialization_completes_without_a_second_caller() {
        let init = Arc::new(CancellationSafeInit::new());
        let value = init
            .get_or_spawn("onnx-init-first-test", || Ok::<_, anyhow::Error>(41usize))
            .await
            .unwrap();
        assert_eq!(*value, 41);
    }

    #[tokio::test]
    async fn cancelling_first_waiter_does_not_cancel_or_duplicate_initialization() {
        let init = Arc::new(CancellationSafeInit::new());
        let loads = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let first_init = Arc::clone(&init);
        let first_loads = Arc::clone(&loads);
        let first = tokio::spawn(async move {
            first_init
                .get_or_spawn("onnx-init-cancel-test", move || {
                    first_loads.fetch_add(1, Ordering::SeqCst);
                    let _ = started_tx.send(());
                    release_rx.recv().unwrap();
                    Ok::<_, anyhow::Error>(42usize)
                })
                .await
        });
        started_rx.await.unwrap();
        first.abort();
        let _ = first.await;
        release_tx.send(()).unwrap();

        let value = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            init.get_or_spawn("unused-duplicate-loader", || {
                panic!("cancelled waiter must not cause a second model load")
            }),
        )
        .await
        .expect("retry must not hang")
        .unwrap();
        assert_eq!(*value, 42);
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_cold_callers_share_one_initialization() {
        let init = Arc::new(CancellationSafeInit::new());
        let loads = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..16 {
            let init = Arc::clone(&init);
            let loads = Arc::clone(&loads);
            callers.push(tokio::spawn(async move {
                init.get_or_spawn("onnx-init-concurrent-test", move || {
                    loads.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    Ok::<_, anyhow::Error>(43usize)
                })
                .await
                .unwrap()
            }));
        }
        for caller in callers {
            assert_eq!(*caller.await.unwrap(), 43);
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn loader_panic_wakes_concurrent_and_future_callers_without_retrying() {
        let init = Arc::new(CancellationSafeInit::<usize>::new());
        let loads = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..8 {
            let init = Arc::clone(&init);
            let loads = Arc::clone(&loads);
            callers.push(tokio::spawn(async move {
                init.get_or_spawn("onnx-init-panic-test", move || {
                    loads.fetch_add(1, Ordering::SeqCst);
                    panic!("synthetic loader panic")
                })
                .await
                .unwrap_err()
                .to_string()
            }));
        }

        let mut errors = Vec::new();
        for caller in callers {
            errors.push(
                tokio::time::timeout(std::time::Duration::from_secs(2), caller)
                    .await
                    .expect("panic must wake every concurrent caller")
                    .unwrap(),
            );
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(errors.iter().all(|error| {
            error.contains("loader panicked") && error.contains("synthetic loader panic")
        }));

        let future_error = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            init.get_or_spawn("unused-after-panic", || {
                panic!("a retained panic error must not retry the loader")
            }),
        )
        .await
        .expect("future caller must receive the retained error promptly")
        .unwrap_err()
        .to_string();
        assert_eq!(future_error, errors[0]);
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn initialization_outlives_the_runtime_that_owned_the_first_waiter() {
        let init = Arc::new(CancellationSafeInit::new());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_init = Arc::clone(&init);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let first = runtime.spawn(async move {
            first_init
                .get_or_spawn("onnx-init-runtime-test", move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok::<_, anyhow::Error>(44usize)
                })
                .await
        });
        started_rx.recv().unwrap();
        first.abort();
        drop(runtime);
        release_tx.send(()).unwrap();

        let second_runtime = tokio::runtime::Runtime::new().unwrap();
        let value = second_runtime
            .block_on(init.get_or_spawn("unused-after-runtime-drop", || {
                panic!("runtime replacement must not duplicate initialization")
            }))
            .unwrap();
        assert_eq!(*value, 44);
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
        let permit = neural_inflight_forwards()
            .acquire()
            .await
            .map_err(|e| anyhow!("neural inference slots closed: {e}"))?;
        tokio::task::spawn_blocking(move || {
            // Hold the slot until the native forward returns, even if the
            // awaiting task is cancelled — the CPU and the activations are
            // committed either way.
            let _permit = permit;
            model.embed_blocking(&texts)
        })
        .await
        .map_err(|e| anyhow!("neural embed task panicked: {e}"))?
    }
}

/// Process-wide cap on concurrent in-process neural forwards.
///
/// The engine fans several embedding windows out per request and several
/// requests can be in flight at once; multiplied together that is an unbounded
/// number of concurrent BERT forwards, each holding its own activations. Past
/// one forward per core there is no throughput left to win, only resident
/// memory to lose — so bound it here, once, rather than hoping every caller
/// picks a safe width. Lucene bounds intra-operation parallelism the same way
/// (`ConcurrentMergeScheduler.CachedExecutor`: "a limited number of threads to
/// execute merge tasks") instead of forking per unit of work.
#[cfg(feature = "neural")]
fn neural_inflight_forwards() -> &'static tokio::sync::Semaphore {
    static INFLIGHT: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    INFLIGHT.get_or_init(|| tokio::sync::Semaphore::new(xerj_common::resource::cores().max(1)))
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
