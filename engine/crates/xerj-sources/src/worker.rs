//! One running source: a thread, its stop flag and its shared status.

use crate::change::{FactsObserver, Sink};
use crate::pg::connector::{self, Ctx};
use crate::spec::SourceSpec;
use crate::status::{Shared, Status};
use crate::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Worker {
    pub spec: SourceSpec,
    pub status: Shared,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(
        spec: SourceSpec,
        store: Store,
        sink: Arc<dyn Sink>,
        facts: Option<Arc<dyn FactsObserver>>,
    ) -> Self {
        let status: Shared = Arc::new(Mutex::new(Status {
            started_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2, spec2) = (status.clone(), stop.clone(), spec.clone());
        let join = std::thread::Builder::new()
            .name(format!("xerj-source-{}", spec.name))
            .spawn(move || {
                let ctx = Ctx {
                    spec: &spec2,
                    store: &store,
                    status: &s2,
                    sink: sink.as_ref(),
                    facts: facts.as_deref(),
                    stop: &st2,
                };
                connector::run(&ctx);
            })
            .expect("spawn source worker thread");
        Self {
            spec,
            status,
            stop,
            join: Some(join),
        }
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Ask the worker to stop and wait up to `wait` for it. The worker
    /// acknowledges its last applied LSN on the way out.
    pub fn stop(mut self, wait: Duration) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + wait;
        if let Some(j) = self.join.take() {
            while !j.is_finished() {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = j.join();
        }
        true
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
