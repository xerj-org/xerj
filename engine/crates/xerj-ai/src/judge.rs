//! The local judge runtime: which models exist, where they come from, how they
//! are loaded, and how much of the machine one scoring call may take.
//!
//! [`crate::seqcls`] knows how to run one model. This module is everything
//! around it that a *server* needs and a benchmark does not:
//!
//!   * a closed table of models ([`MODELS`]) — a search request picks a tier by
//!     name and can never make the server fetch an arbitrary repository;
//!   * lazy, shared, non-blocking loading — the first request starts the load
//!     (and the one-time download) on a background thread and every request
//!     that arrives meanwhile is told *that* instead of being parked on it;
//!   * a thread budget (one dedicated rayon pool — candle's CPU matmul runs on
//!     whichever pool calls it) and an admission limit, so concurrent searches
//!     queue for the judge rather than oversubscribing every core;
//!   * a memory check against the resource policy's safe zone before weights
//!     are mapped, because the large tier is 2.3 GB and "the server got
//!     OOM-killed by a search" is not an acceptable failure.
//!
//! Compiled only under the `neural` cargo feature.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::seqcls::{PairClassifier, PairModelSource, PairStats};

/// What a model is for. The two tasks never share a model: a cross-encoder has
/// one relevance logit, an NLI model has entailment classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JudgeTask {
    /// `(query, document)` → one relevance logit. The `local` rerank provider.
    Rerank,
    /// `(state, hypothesis)` → entailment logits. `POST /_judge`.
    Entail,
}

impl JudgeTask {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rerank => "rerank",
            Self::Entail => "entail",
        }
    }
}

/// `probability = sigmoid(scale × logit + bias)`.
///
/// A cross-encoder's raw sigmoid is a ranking score, not a probability: the
/// MS MARCO MiniLM model emits logits of ±10, so almost every document comes
/// out as 0.000 or 1.000 whatever its real chance of being relevant. Platt
/// scaling (one slope, one intercept, fitted by maximum likelihood on held-out
/// relevance judgements) repairs that without changing the order, because the
/// map is monotonic whenever `scale > 0`.
///
/// `fitted_on` says where the two numbers came from. They are only as general
/// as that data: see `benchmarks/local-judge/` for the fit and its error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub scale: f64,
    pub bias: f64,
    pub fitted_on: &'static str,
}

impl Calibration {
    /// The raw sigmoid. Used when no fit exists for a model.
    pub const NONE: Self = Self {
        scale: 1.0,
        bias: 0.0,
        fitted_on: "none (raw sigmoid)",
    };

    pub fn is_identity(&self) -> bool {
        self.scale == 1.0 && self.bias == 0.0
    }

    pub fn probability(&self, logit: f32) -> f64 {
        let z = self.scale * f64::from(logit) + self.bias;
        1.0 / (1.0 + (-z).exp())
    }
}

/// One model the judge can load.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JudgeModelSpec {
    pub task: JudgeTask,
    /// The name a request uses: `small`, `base`, `large`.
    pub tier: &'static str,
    pub repo: &'static str,
    /// Pinned commit: a moved `main` cannot change what is scored with.
    pub revision: &'static str,
    /// sha256 of `model.safetensors` at that commit, checked on load.
    pub weights_sha256: &'static str,
    /// Size of the one-time download (the weights dominate).
    pub download_bytes: u64,
    /// Bytes per parameter on disk: 4 for F32 checkpoints, 2 for F16. Weights
    /// are widened to F32 in memory, so resident size is `download × 4 / this`.
    pub disk_bytes_per_param: u64,
    /// Licence of the weights, from the model card.
    pub licence: &'static str,
    /// What the model card says about training data that a deployer should
    /// know before relying on the licence line alone.
    pub data_note: &'static str,
    pub calibration: Calibration,
}

impl JudgeModelSpec {
    /// Resident bytes once loaded: F32 weights.
    pub fn resident_bytes(&self) -> u64 {
        self.download_bytes * 4 / self.disk_bytes_per_param.max(1)
    }

    /// Key used for cells and for `<model_dir>/<key>/`.
    pub fn key(&self) -> String {
        format!("{}-{}", self.task.as_str(), self.tier)
    }
}

/// Every model the judge will load. Closed on purpose — see the module doc.
///
/// Licences and data notes were read from each model card on 2026-09-19 at the
/// pinned revision; `docs/RERANK.md` ("Local provider") carries the longer
/// version, with the sources.
pub const MODELS: &[JudgeModelSpec] = &[
    JudgeModelSpec {
        task: JudgeTask::Rerank,
        tier: "small",
        repo: "cross-encoder/ms-marco-MiniLM-L6-v2",
        revision: "233902d25c440f23af6f7d6e94d2946bac0bee0a",
        weights_sha256: "821d1aa69520101d6e0737f78a042ae25b19e5cb9160701909d10434f4aeb0ae",
        download_bytes: 91_582_788,
        disk_bytes_per_param: 4,
        licence: "Apache-2.0",
        data_note: "trained on MS MARCO passage ranking (model card). Microsoft's MS MARCO \
                    terms say the datasets are \"intended for non-commercial research purposes \
                    only\"; the weights are Apache-2.0, and whether dataset terms reach a \
                    model trained on the data is not settled — take advice before commercial use",
        calibration: Calibration::NONE,
    },
    JudgeModelSpec {
        task: JudgeTask::Rerank,
        tier: "base",
        repo: "BAAI/bge-reranker-base",
        revision: "2cfc18c9415c912f9d8155881c133215df768a70",
        weights_sha256: "ced967c45fd1902eb92716c9ceeca7c95a936770ea9db611f5a841b926e33fbd",
        download_bytes: 1_129_305_046,
        disk_bytes_per_param: 4,
        licence: "MIT",
        data_note: "the model card says the released models \"can be used for commercial \
                    purposes free of charge\" and that the model was trained on \"multilingual \
                    pair data\", without listing the datasets; BAAI's published English \
                    fine-tuning data for its other models includes MS MARCO, so the same \
                    non-commercial-data question should be assumed",
        calibration: Calibration::NONE,
    },
    JudgeModelSpec {
        task: JudgeTask::Rerank,
        tier: "large",
        repo: "BAAI/bge-reranker-v2-m3",
        revision: "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e",
        weights_sha256: "d9e3e081faff1eefb84019509b2f5558fd74c1a05a2c7db22f74174fcedb5286",
        download_bytes: 2_288_170_920,
        disk_bytes_per_param: 4,
        licence: "Apache-2.0",
        data_note: "trained on bge-m3-data, Quora and FEVER per the model card; bge-m3-data \
                    lists MS MARCO among its sources, whose terms say \"non-commercial research \
                    purposes only\" — the same unsettled question as the small tier",
        calibration: Calibration::NONE,
    },
    JudgeModelSpec {
        task: JudgeTask::Entail,
        tier: "base",
        repo: "MoritzLaurer/deberta-v3-base-zeroshot-v2.0-c",
        revision: "bddf8c5411c34ac3565e16e04384fd68b2618dda",
        weights_sha256: "605e40d2020f4c66666db3e98a4bc277b6214565eac95f35c1a5027a93ee0ef0",
        download_bytes: 377_529_570,
        disk_bytes_per_param: 2,
        licence: "MIT",
        data_note: "the `-c` checkpoint: trained only on commercially-friendly data \
                    (Mixtral-generated synthetic tasks, MNLI, FEVER-NLI) per the model card",
        calibration: Calibration::NONE,
    },
    JudgeModelSpec {
        task: JudgeTask::Entail,
        tier: "large",
        repo: "MoritzLaurer/deberta-v3-large-zeroshot-v2.0-c",
        revision: "b2730f16019076bb0009481121efbe4705e0e378",
        weights_sha256: "688fdc38a29c0e301fa4d67805a45db82d84b8663349aa8d20d67de39aebd02d",
        download_bytes: 878_834_038,
        disk_bytes_per_param: 2,
        licence: "MIT",
        data_note: "the `-c` checkpoint: trained only on commercially-friendly data \
                    (Mixtral-generated synthetic tasks, MNLI, FEVER-NLI) per the model card",
        calibration: Calibration::NONE,
    },
];

/// Look a model up by task and tier name.
pub fn model_spec(task: JudgeTask, tier: &str) -> Option<&'static JudgeModelSpec> {
    MODELS.iter().find(|m| m.task == task && m.tier == tier)
}

/// The tier names that exist for a task, in table order.
pub fn tiers(task: JudgeTask) -> Vec<&'static str> {
    MODELS
        .iter()
        .filter(|m| m.task == task)
        .map(|m| m.tier)
        .collect()
}

/// Operator settings for the runtime (`[judge]` in the server config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeRuntimeConfig {
    /// `false` refuses every local judge call.
    pub enabled: bool,
    /// `false` never opens a network connection; models must be on disk.
    pub allow_download: bool,
    /// Hugging Face cache override.
    pub cache_dir: Option<PathBuf>,
    /// Air-gapped root: `<model_dir>/<task>-<tier>/` holds the three files,
    /// e.g. `rerank-small/`, `entail-base/`.
    pub model_dir: Option<PathBuf>,
    /// Threads in the judge's pool. `0` picks [`default_threads`].
    pub threads: usize,
    /// Scoring calls admitted at once; the rest wait, inside their deadline.
    pub max_inflight: usize,
}

impl Default for JudgeRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_download: true,
            cache_dir: None,
            model_dir: None,
            threads: 0,
            max_inflight: DEFAULT_MAX_INFLIGHT,
        }
    }
}

/// Scoring calls admitted at once by default. Two, not one, so a slow
/// large-tier call does not make a small-tier call wait its whole duration;
/// both share the one thread pool, so the CPU ceiling is unchanged.
pub const DEFAULT_MAX_INFLIGHT: usize = 2;

/// Widest pool the judge picks for itself.
///
/// Measured with `examples/pair_score.rs` (see `benchmarks/local-judge/`):
/// candle's CPU matmul stops scaling well before a large host runs out of
/// cores, and past the knee extra threads only take cores from the search and
/// ingest pools. An operator can still set any width explicitly.
pub const AUTO_THREADS_CEILING: usize = 16;

/// Threads the judge uses when the operator sets none: every core the resource
/// policy grants latency-critical work, up to [`AUTO_THREADS_CEILING`].
pub fn default_threads() -> usize {
    xerj_common::resource::threads_for(xerj_common::resource::Workload::Latency)
        .clamp(1, AUTO_THREADS_CEILING)
}

/// Wait this long before retrying a load that failed. A failed download must
/// not be retried by every search that arrives, and must not stick forever.
const RETRY_FAILED_LOAD_AFTER: Duration = Duration::from_secs(60);

/// Activation headroom a scoring call needs on top of the weights.
const ACTIVATION_HEADROOM_BYTES: u64 = 768 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("the local judge is disabled on this server (`[judge] enabled = false`)")]
    Disabled,
    #[error("unknown {task} model `{tier}`; available: {available}")]
    UnknownModel {
        task: &'static str,
        tier: String,
        available: String,
    },
    /// The model is not ready yet. Degradable: the caller keeps what it has.
    #[error("{0}")]
    Loading(String),
    /// Every admission slot stayed busy for the whole deadline. Degradable.
    #[error("the local judge was busy for the whole {waited_ms}ms budget ({inflight} call(s) in flight)")]
    Busy { waited_ms: u128, inflight: usize },
    /// The model cannot be loaded on this server as configured.
    #[error("{0}")]
    LoadFailed(String),
    #[error("local judge inference failed: {0}")]
    Inference(String),
}

impl JudgeError {
    /// Whether the caller should keep its own result and carry on, as opposed
    /// to being shown the error. Time and load are degradable; configuration
    /// and contract faults are not — the same split the hosted provider makes.
    pub fn degradable(&self) -> bool {
        matches!(self, Self::Loading(_) | Self::Busy { .. })
    }
}

enum LoadState {
    Idle,
    Loading { since: Instant, downloading: bool },
    Ready(Arc<PairClassifier>),
    Failed { error: String, at: Instant },
}

struct ModelCell {
    state: Mutex<LoadState>,
    changed: tokio::sync::Notify,
}

/// What [`JudgeRuntime::status`] reports for one model, without loading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPresence {
    Loaded,
    Loading,
    OnDisk,
    NotDownloaded,
    Failed(String),
}

impl ModelPresence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Loading => "loading",
            Self::OnDisk => "on_disk",
            Self::NotDownloaded => "not_downloaded",
            Self::Failed(_) => "failed",
        }
    }
}

/// Result of one scoring call.
#[derive(Debug)]
pub struct Scored {
    /// Raw logits per input pair; `None` where the deadline cut scoring short.
    pub logits: Vec<Option<Vec<f32>>>,
    pub stats: PairStats,
    /// Time spent waiting for an admission slot.
    pub queued: Duration,
    /// Time spent in the model.
    pub inference: Duration,
}

/// The shared runtime. One per server, behind an `Arc`.
pub struct JudgeRuntime {
    cfg: JudgeRuntimeConfig,
    pool: OnceLock<Arc<rayon::ThreadPool>>,
    slots: Arc<tokio::sync::Semaphore>,
    cells: Mutex<HashMap<String, Arc<ModelCell>>>,
}

impl std::fmt::Debug for JudgeRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JudgeRuntime")
            .field("cfg", &self.cfg)
            .finish()
    }
}

impl JudgeRuntime {
    pub fn new(cfg: JudgeRuntimeConfig) -> Arc<Self> {
        let slots = cfg.max_inflight.max(1);
        Arc::new(Self {
            cfg,
            pool: OnceLock::new(),
            slots: Arc::new(tokio::sync::Semaphore::new(slots)),
            cells: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &JudgeRuntimeConfig {
        &self.cfg
    }

    /// Threads the pool has, or will have once first used.
    pub fn threads(&self) -> usize {
        match self.cfg.threads {
            0 => default_threads(),
            n => n,
        }
    }

    /// Built on first use, so a server that never judges never spawns it.
    fn pool(&self) -> Arc<rayon::ThreadPool> {
        self.pool
            .get_or_init(|| {
                let threads = self.threads();
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .thread_name(|i| format!("xerj-judge-{i}"))
                    .build()
                    .expect("build the judge thread pool");
                tracing::info!(threads, "local judge: thread pool started");
                Arc::new(pool)
            })
            .clone()
    }

    fn source(&self, spec: &JudgeModelSpec) -> PairModelSource {
        PairModelSource {
            repo: spec.repo.to_string(),
            revision: Some(spec.revision.to_string()),
            cache_dir: self.cfg.cache_dir.clone(),
            local_dir: self.cfg.model_dir.as_ref().map(|d| d.join(spec.key())),
            allow_download: self.cfg.allow_download,
            weights_sha256: Some(spec.weights_sha256.to_string()),
        }
    }

    fn cell(&self, spec: &JudgeModelSpec) -> Arc<ModelCell> {
        self.cells
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(spec.key())
            .or_insert_with(|| {
                Arc::new(ModelCell {
                    state: Mutex::new(LoadState::Idle),
                    changed: tokio::sync::Notify::new(),
                })
            })
            .clone()
    }

    /// Where a model stands, without loading anything.
    pub fn presence(&self, spec: &JudgeModelSpec) -> ModelPresence {
        let cell = self.cell(spec);
        let state = cell.state.lock().unwrap_or_else(|p| p.into_inner());
        match &*state {
            LoadState::Ready(_) => ModelPresence::Loaded,
            LoadState::Loading { .. } => ModelPresence::Loading,
            LoadState::Failed { error, .. } => ModelPresence::Failed(error.clone()),
            LoadState::Idle => {
                if crate::seqcls::is_on_disk(&self.source(spec)) {
                    ModelPresence::OnDisk
                } else {
                    ModelPresence::NotDownloaded
                }
            }
        }
    }

    /// Refuse a load that would not fit the memory safe zone.
    fn check_memory(spec: &JudgeModelSpec) -> Result<(), String> {
        let needed = spec.resident_bytes() + ACTIVATION_HEADROOM_BYTES;
        match xerj_common::resource::memory_safe_zone_bytes() {
            Some(zone) if needed > zone => Err(format!(
                "not enough memory to load the {} `{}` model: it needs about {} MB resident and \
                 this machine's safe zone is {} MB. Pick a smaller tier, or free memory",
                spec.task.as_str(),
                spec.tier,
                needed / (1024 * 1024),
                zone / (1024 * 1024),
            )),
            // Unknown RAM means no memory-derived limit, never a small one.
            _ => Ok(()),
        }
    }

    /// Start loading if nothing has, and return the model if it is ready.
    fn poll(
        self: &Arc<Self>,
        spec: &'static JudgeModelSpec,
    ) -> Result<Arc<PairClassifier>, JudgeError> {
        let cell = self.cell(spec);
        let mut state = cell.state.lock().unwrap_or_else(|p| p.into_inner());
        match &*state {
            LoadState::Ready(model) => return Ok(model.clone()),
            LoadState::Loading { since, downloading } => {
                return Err(JudgeError::Loading(loading_message(
                    spec,
                    *downloading,
                    since.elapsed(),
                )));
            }
            LoadState::Failed { error, at } if at.elapsed() < RETRY_FAILED_LOAD_AFTER => {
                return Err(JudgeError::LoadFailed(error.clone()));
            }
            LoadState::Failed { .. } | LoadState::Idle => {}
        }

        if let Err(reason) = Self::check_memory(spec) {
            *state = LoadState::Failed {
                error: reason.clone(),
                at: Instant::now(),
            };
            return Err(JudgeError::LoadFailed(reason));
        }

        let source = self.source(spec);
        let downloading = !crate::seqcls::is_on_disk(&source);
        if downloading && source.allow_download && source.local_dir.is_none() {
            // The one line an operator needs when a search triggers a fetch:
            // what, how big, under which licence, and the data caveat.
            tracing::warn!(
                model = spec.repo,
                revision = spec.revision,
                size_mb = spec.download_bytes / 1_000_000,
                licence = spec.licence,
                "local judge: first use of the {} `{}` model — downloading it from \
                 huggingface.co (one time; cached afterwards). Training data: {}. Set \
                 `[judge] download = false` to forbid downloads",
                spec.task.as_str(),
                spec.tier,
                spec.data_note,
            );
        }
        *state = LoadState::Loading {
            since: Instant::now(),
            downloading,
        };
        drop(state);

        let cell_for_thread = cell.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("xerj-judge-load-{}", spec.key()))
            .spawn(move || {
                let started = Instant::now();
                let loaded = PairClassifier::load(&source);
                let mut state = cell_for_thread
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                *state = match loaded {
                    Ok(model) => {
                        tracing::info!(
                            model = spec.repo,
                            took_ms = started.elapsed().as_millis() as u64,
                            "local judge: model ready"
                        );
                        LoadState::Ready(Arc::new(model))
                    }
                    Err(e) => {
                        let error = format!(
                            "cannot load the {} `{}` model ({}): {e:#}",
                            spec.task.as_str(),
                            spec.tier,
                            spec.repo
                        );
                        tracing::error!("local judge: {error}");
                        LoadState::Failed {
                            error,
                            at: Instant::now(),
                        }
                    }
                };
                drop(state);
                cell_for_thread.changed.notify_waiters();
            });
        if let Err(e) = spawned {
            let error = format!("cannot start the model load thread: {e}");
            *cell.state.lock().unwrap_or_else(|p| p.into_inner()) = LoadState::Failed {
                error: error.clone(),
                at: Instant::now(),
            };
            return Err(JudgeError::LoadFailed(error));
        }
        Err(JudgeError::Loading(loading_message(
            spec,
            downloading,
            Duration::ZERO,
        )))
    }

    /// The model, waiting for a load in progress until `deadline` at most.
    ///
    /// Never parks a request on a download: once the deadline passes the caller
    /// gets [`JudgeError::Loading`] (degradable) and the load carries on in the
    /// background for the next request.
    pub async fn model(
        self: &Arc<Self>,
        spec: &'static JudgeModelSpec,
        deadline: Instant,
    ) -> Result<Arc<PairClassifier>, JudgeError> {
        if !self.cfg.enabled {
            return Err(JudgeError::Disabled);
        }
        let cell = self.cell(spec);
        loop {
            // Register for the wake-up BEFORE looking, so a load that finishes
            // between the look and the wait is not missed.
            let notified = cell.changed.notified();
            match self.poll(spec) {
                Err(JudgeError::Loading(message)) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(JudgeError::Loading(message));
                    }
                    if tokio::time::timeout(deadline - now, notified)
                        .await
                        .is_err()
                    {
                        // Report the state as it is now, not as it was.
                        return match self.poll(spec) {
                            Ok(model) => Ok(model),
                            Err(e) => Err(e),
                        };
                    }
                }
                other => return other,
            }
        }
    }

    /// Load a model on the calling thread. For tools with no async runtime.
    pub fn model_blocking(
        &self,
        spec: &'static JudgeModelSpec,
    ) -> Result<Arc<PairClassifier>, JudgeError> {
        if !self.cfg.enabled {
            return Err(JudgeError::Disabled);
        }
        Self::check_memory(spec).map_err(JudgeError::LoadFailed)?;
        PairClassifier::load(&self.source(spec))
            .map(Arc::new)
            .map_err(|e| JudgeError::LoadFailed(format!("{e:#}")))
    }

    /// Score `pairs`, admitted through the slot limit and run on the judge's
    /// own pool. Stops at `deadline`; pairs not reached come back `None`.
    pub async fn score(
        self: &Arc<Self>,
        model: Arc<PairClassifier>,
        pairs: Vec<(String, String)>,
        deadline: Instant,
    ) -> Result<Scored, JudgeError> {
        let asked = Instant::now();
        let budget = deadline.saturating_duration_since(asked);
        let permit = match tokio::time::timeout(budget, self.slots.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(JudgeError::Inference("judge runtime is shut down".into())),
            Err(_) => {
                return Err(JudgeError::Busy {
                    waited_ms: asked.elapsed().as_millis(),
                    inflight: self.cfg.max_inflight.max(1),
                })
            }
        };
        let queued = asked.elapsed();

        // A caller that goes away (client disconnect, outer timeout) must not
        // leave a full window of forward passes running for nobody.
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = CancelOnDrop(cancelled.clone());
        let pool = self.pool();
        let started = Instant::now();
        let joined = tokio::task::spawn_blocking(move || {
            // The permit lives exactly as long as the work it admitted, even
            // if the awaiting future is dropped first.
            let _permit = permit;
            pool.install(|| {
                model.logits_blocking(&pairs, &|| {
                    Instant::now() < deadline && !cancelled.load(Ordering::Relaxed)
                })
            })
        })
        .await;
        guard.disarm();
        let (logits, stats) = joined
            .map_err(|e| JudgeError::Inference(format!("scoring task panicked: {e}")))?
            .map_err(|e| JudgeError::Inference(format!("{e:#}")))?;
        Ok(Scored {
            logits,
            stats,
            queued,
            inference: started.elapsed(),
        })
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

impl CancelOnDrop {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn loading_message(spec: &JudgeModelSpec, downloading: bool, elapsed: Duration) -> String {
    if downloading {
        format!(
            "the local {} `{}` model is being downloaded ({} MB from huggingface.co/{}, one \
             time; {}s so far) — retry shortly",
            spec.task.as_str(),
            spec.tier,
            spec.download_bytes / 1_000_000,
            spec.repo,
            elapsed.as_secs()
        )
    } else {
        format!(
            "the local {} `{}` model is loading ({}ms so far) — retry shortly",
            spec.task.as_str(),
            spec.tier,
            elapsed.as_millis()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_table_is_a_closed_set_with_unique_keys() {
        let mut keys: Vec<String> = MODELS.iter().map(|m| m.key()).collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate task/tier");
        for m in MODELS {
            assert_eq!(m.revision.len(), 40, "{}: pin a full commit", m.repo);
            assert_eq!(m.weights_sha256.len(), 64, "{}", m.repo);
            assert!(m.calibration.scale > 0.0, "{}: must stay monotonic", m.repo);
            assert!(!m.licence.is_empty() && !m.data_note.is_empty());
        }
        assert_eq!(tiers(JudgeTask::Rerank), ["small", "base", "large"]);
        assert!(model_spec(JudgeTask::Rerank, "small").is_some());
        assert!(model_spec(JudgeTask::Rerank, "huge").is_none());
        // Tiers are per task: there is no small NLI model.
        assert!(model_spec(JudgeTask::Entail, "small").is_none());
    }

    #[test]
    fn f16_checkpoints_are_budgeted_at_their_widened_size() {
        let nli = model_spec(JudgeTask::Entail, "base").unwrap();
        assert_eq!(nli.resident_bytes(), nli.download_bytes * 2);
        let ce = model_spec(JudgeTask::Rerank, "small").unwrap();
        assert_eq!(ce.resident_bytes(), ce.download_bytes);
    }

    #[test]
    fn calibration_is_monotonic_and_identity_is_the_raw_sigmoid() {
        let raw = Calibration::NONE;
        assert!(raw.is_identity());
        assert!((raw.probability(0.0) - 0.5).abs() < 1e-12);
        let fitted = Calibration {
            scale: 0.4,
            bias: -2.0,
            fitted_on: "test",
        };
        let mut last = -1.0;
        for logit in [-12.0f32, -3.0, 0.0, 2.5, 11.0] {
            let p = fitted.probability(logit);
            assert!(p > last && (0.0..=1.0).contains(&p));
            last = p;
        }
    }

    #[test]
    fn auto_threads_never_exceed_the_measured_ceiling() {
        let n = default_threads();
        assert!((1..=AUTO_THREADS_CEILING).contains(&n));
    }

    #[tokio::test]
    async fn a_disabled_runtime_refuses_before_touching_a_model() {
        let rt = JudgeRuntime::new(JudgeRuntimeConfig {
            enabled: false,
            ..Default::default()
        });
        let spec = model_spec(JudgeTask::Rerank, "small").unwrap();
        let err = rt
            .model(spec, Instant::now() + Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, JudgeError::Disabled));
        assert!(!err.degradable());
    }

    #[tokio::test]
    async fn an_offline_runtime_without_the_files_fails_loud_and_does_not_retry_at_once() {
        let dir = std::env::temp_dir().join(format!("xerj-judge-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rt = JudgeRuntime::new(JudgeRuntimeConfig {
            allow_download: false,
            cache_dir: Some(dir.clone()),
            ..Default::default()
        });
        let spec = model_spec(JudgeTask::Rerank, "small").unwrap();
        assert_eq!(rt.presence(spec), ModelPresence::NotDownloaded);
        let err = rt
            .model(spec, Instant::now() + Duration::from_secs(10))
            .await
            .unwrap_err();
        match &err {
            JudgeError::LoadFailed(m) => {
                assert!(m.contains("downloads are disabled"), "{m}")
            }
            other => panic!("expected LoadFailed, got {other:?}"),
        }
        assert!(
            !err.degradable(),
            "a missing model is a configuration fault"
        );
        // The failure is remembered: the next request is answered from the
        // cell instead of starting another load.
        assert!(matches!(rt.presence(spec), ModelPresence::Failed(_)));
        let again = rt
            .model(spec, Instant::now() + Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(matches!(again, JudgeError::LoadFailed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
