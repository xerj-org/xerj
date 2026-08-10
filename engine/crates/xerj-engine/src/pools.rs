//! Where the engine's thread pools get their widths.
//!
//! Every pool in this crate used to size itself inline from
//! `available_parallelism()` with its own magic fraction, while
//! `EngineConfig::{flush_workers, merge_workers, search_workers}` — declared,
//! defaulted, documented and validated in `xerj-common` — were read by nothing
//! at all. An operator who set `engine.search_workers = 2` to keep XERJ off
//! their laptop's cores got no effect and no warning: the accepted-and-ignored
//! class tracked in #204, and half of #240.
//!
//! This module is the single seam between configuration and pool construction:
//! [`resolve`] turns an [`EngineConfig`] plus the machine policy in
//! [`xerj_common::resource`] into concrete widths, [`init`] installs them once
//! per process, and the pools in `lib.rs` read them. Defaults reproduce the
//! widths the pools already used — the measured ingest/read tuning is
//! unchanged; what changes is that the knobs now do what they say.

use xerj_common::config::EngineConfig;
use xerj_common::resource::{self, Workload};

/// Concrete thread counts for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSizing {
    /// `ingest_pool`: bulk parse/analyze/insert. Latency-critical for writers.
    pub ingest: usize,
    /// `background_pool`: flush side-cars and segment finalisation.
    pub background: usize,
    /// `merge_pool`: merge re-encode. Pure maintenance.
    pub merge: usize,
    /// `flush_finalize_gate`: how many shard finalizes may run at once.
    pub flush_finalize: usize,
    /// Rayon's global pool, which is what search segment fan-out runs on.
    pub search: usize,
}

/// What [`resolve`] decided, plus anything the operator asked for that could
/// not be honoured verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub sizing: PoolSizing,
    pub warnings: Vec<String>,
}

/// Turn configuration into pool widths.
///
/// `env_flush_finalize` is `XERJ_FIN_CONC`, kept as the tuning override it has
/// always been; it wins over the config value, and a value that cannot be used
/// is reported rather than dropped on the floor.
pub fn resolve(cfg: &EngineConfig, env_flush_finalize: Option<&str>) -> Resolved {
    let mut warnings = Vec::new();

    let mut flush_finalize = cfg.flush_workers.max(1);
    if let Some(raw) = env_flush_finalize.map(str::trim).filter(|s| !s.is_empty()) {
        match raw.parse::<usize>() {
            Ok(n) if n >= 1 => flush_finalize = n,
            _ => warnings.push(format!(
                "XERJ_FIN_CONC={raw} is not a positive integer; using \
                 engine.flush_workers={flush_finalize} instead"
            )),
        }
    }

    Resolved {
        sizing: PoolSizing {
            ingest: resource::threads_for(Workload::Latency),
            background: resource::threads_for(Workload::Background),
            merge: cfg.merge_workers.max(1),
            flush_finalize,
            search: cfg.search_workers.max(1),
        },
        warnings,
    }
}

static SIZING: std::sync::OnceLock<PoolSizing> = std::sync::OnceLock::new();

/// Install the process-wide pool sizing from config. Idempotent, like
/// [`crate::governor::init`]: the first caller wins, because rayon pools are
/// process-global and already-running threads cannot be resized.
///
/// A second [`Engine`](crate::Engine) built in the same process with *different*
/// widths logs a warning naming the values that were ignored — silently serving
/// one engine's config to another is exactly the bug this module exists to end.
pub fn init(cfg: &EngineConfig) {
    let resolved = resolve(cfg, std::env::var("XERJ_FIN_CONC").ok().as_deref());
    for warning in &resolved.warnings {
        tracing::warn!("{warning}");
    }
    let installed = *SIZING.get_or_init(|| resolved.sizing);
    if installed != resolved.sizing {
        tracing::warn!(
            "engine pool sizing was already fixed for this process ({installed:?}); ignoring \
             {:?} from this engine's config",
            resolved.sizing
        );
    }
    install_search_pool(installed.search);
}

/// The installed sizing, or the policy defaults when no engine has been built
/// yet (engine-only unit tests that touch a pool directly).
pub fn sizing() -> PoolSizing {
    *SIZING.get_or_init(|| resolve(&EngineConfig::default(), None).sizing)
}

/// Search fan-out runs on rayon's *global* pool, so honouring
/// `engine.search_workers` means building that pool explicitly — rayon
/// otherwise defaults it to every core on first use.
///
/// `build_global` can only succeed once per process. If some par_iter already
/// ran, the request cannot be honoured; that is reported with the width the
/// process actually has, instead of leaving the operator to guess.
fn install_search_pool(threads: usize) {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        if let Err(err) = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("xerj-search-{i}"))
            .build_global()
        {
            let actual = rayon::current_num_threads();
            if actual != threads {
                tracing::warn!(
                    "engine.search_workers={threads} could not be applied ({err}); search \
                     fan-out runs on the already-initialised global pool of {actual} threads"
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(flush: usize, merge: usize, search: usize) -> EngineConfig {
        EngineConfig {
            flush_workers: flush,
            merge_workers: merge,
            search_workers: search,
            ..EngineConfig::default()
        }
    }

    #[test]
    fn configured_workers_reach_the_pools() {
        // The #240 §4 defect: these three were read by nothing.
        let r = resolve(&cfg(3, 5, 7), None);
        assert_eq!(r.sizing.flush_finalize, 3);
        assert_eq!(r.sizing.merge, 5);
        assert_eq!(r.sizing.search, 7);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn defaults_reproduce_the_measured_widths() {
        let cores = xerj_common::resource::cores();
        let r = resolve(&EngineConfig::default(), None);
        // Latency-critical pools keep the whole machine ...
        assert_eq!(r.sizing.ingest, cores);
        assert_eq!(r.sizing.background, cores);
        assert_eq!(r.sizing.search, cores);
        // ... maintenance keeps the measured max(2, cores/8).
        assert_eq!(r.sizing.merge, (cores / 8).max(2).min(cores.max(1)));
        assert_eq!(r.sizing.flush_finalize, r.sizing.merge);
    }

    #[test]
    fn the_env_override_wins_and_a_bad_one_is_reported() {
        assert_eq!(resolve(&cfg(3, 2, 4), Some("9")).sizing.flush_finalize, 9);
        assert_eq!(resolve(&cfg(3, 2, 4), Some("")).sizing.flush_finalize, 3);
        let r = resolve(&cfg(3, 2, 4), Some("lots"));
        assert_eq!(r.sizing.flush_finalize, 3);
        assert!(r.warnings[0].contains("XERJ_FIN_CONC=lots"));
        let r = resolve(&cfg(3, 2, 4), Some("0"));
        assert_eq!(r.sizing.flush_finalize, 3);
        assert_eq!(r.warnings.len(), 1);
    }
}
