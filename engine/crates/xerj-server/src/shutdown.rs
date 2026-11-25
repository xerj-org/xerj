//! Node stop path (#3418): SIGINT/SIGTERM handlers installed before the
//! engine opens, one shutdown budget (`--shutdown-timeout`,
//! `XERJ_SHUTDOWN_TIMEOUT_SECS`, default 30 s) shared by every phase, a
//! watchdog thread that ends the process when the budget is spent, and a
//! second signal that exits at once. Every forced exit drains and fsyncs the
//! WAL shards still waiting for their batched fsync before `_exit`.
//!
//! Prior art: quickwit `quickwit-cli/src/service.rs:52-74` (Apache-2.0,
//! second Ctrl-C calls `process::exit`), meilisearch
//! `crates/meilisearch/src/main.rs:136-139` (MIT core, exit 130). Approach
//! adapted, no code copied. Differences: one signal stream held for the whole
//! run (quickwit registers the second listener after the first signal), and
//! the WAL fsync before `_exit`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tracing::{error, info, warn};

pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
pub const SHUTDOWN_TIMEOUT_ENV: &str = "XERJ_SHUTDOWN_TIMEOUT_SECS";
const MAX_SHUTDOWN_TIMEOUT_SECS: u64 = 3600;
pub const WAL_SYNC_BUDGET: Duration = Duration::from_secs(2);

pub fn parse_timeout_secs(value: &str) -> Result<Duration, String> {
    let secs: u64 = value
        .trim()
        .parse()
        .map_err(|_| format!("shutdown timeout must be whole seconds, got {value:?}"))?;
    if !(1..=MAX_SHUTDOWN_TIMEOUT_SECS).contains(&secs) {
        return Err(format!(
            "shutdown timeout must be 1..={MAX_SHUTDOWN_TIMEOUT_SECS} seconds, got {secs}"
        ));
    }
    Ok(Duration::from_secs(secs))
}

pub fn resolve_timeout(flag: Option<&str>, env: Option<&str>) -> Result<Duration, String> {
    if let Some(v) = flag {
        return parse_timeout_secs(v).map_err(|e| format!("--shutdown-timeout: {e}"));
    }
    match env {
        Some(v) if !v.trim().is_empty() => {
            parse_timeout_secs(v).map_err(|e| format!("{SHUTDOWN_TIMEOUT_ENV}: {e}"))
        }
        _ => Ok(DEFAULT_SHUTDOWN_TIMEOUT),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopSignal {
    Interrupt,
    Terminate,
}

impl StopSignal {
    pub fn name(self) -> &'static str {
        match self {
            StopSignal::Interrupt => "SIGINT",
            StopSignal::Terminate => "SIGTERM",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            StopSignal::Interrupt => 130,
            StopSignal::Terminate => 143,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ShutdownPlan {
    pub started: Instant,
    pub budget: Duration,
}

impl ShutdownPlan {
    pub fn deadline(&self) -> Instant {
        self.started + self.budget
    }

    pub fn in_flight_deadline(&self) -> Instant {
        self.started + (self.budget / 3).min(Duration::from_secs(10))
    }

    pub fn flush_deadline(&self) -> Instant {
        self.started + self.budget * 2 / 3
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline().saturating_duration_since(now)
    }

    pub fn listener_grace(&self, now: Instant) -> Duration {
        (self.remaining(now) / 2).min(Duration::from_secs(10))
    }

    pub fn runtime_teardown(&self, now: Instant) -> Duration {
        self.remaining(now).min(Duration::from_secs(5))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SignalAction {
    Begin(StopSignal),
    Force(StopSignal),
}

#[derive(Debug, Default)]
pub struct SignalCounter {
    seen: u32,
}

impl SignalCounter {
    pub fn on_signal(&mut self, sig: StopSignal) -> SignalAction {
        self.seen += 1;
        if self.seen == 1 {
            SignalAction::Begin(sig)
        } else {
            SignalAction::Force(sig)
        }
    }
}

static PLAN: OnceLock<ShutdownPlan> = OnceLock::new();

pub fn plan() -> Option<ShutdownPlan> {
    PLAN.get().copied()
}

pub fn listener_grace() -> Duration {
    plan()
        .map(|p| p.listener_grace(Instant::now()))
        .unwrap_or(Duration::from_secs(10))
}

pub fn runtime_teardown() -> Duration {
    plan()
        .map(|p| p.runtime_teardown(Instant::now()))
        .unwrap_or(Duration::from_secs(5))
}

#[derive(Clone)]
pub struct ShutdownRequest(tokio::sync::watch::Receiver<Option<StopSignal>>);

impl ShutdownRequest {
    pub async fn requested(&mut self) -> StopSignal {
        loop {
            if let Some(sig) = *self.0.borrow_and_update() {
                return sig;
            }
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    #[cfg(test)]
    fn is_requested(&self) -> bool {
        self.0.borrow().is_some()
    }
}

#[cfg(unix)]
pub fn restore_default_dispositions() {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut old) == 0
                && old.sa_sigaction == libc::SIG_IGN
            {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
    }
}

#[cfg(not(unix))]
pub fn restore_default_dispositions() {}

pub fn install(budget: Duration) -> ShutdownRequest {
    let (tx, rx) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        let mut signals = match Signals::register() {
            Ok(s) => s,
            Err(e) => {
                error!("could not install the SIGINT/SIGTERM handler: {e}; the node stops only on SIGKILL");
                return;
            }
        };
        let mut counter = SignalCounter::default();
        let tx = tx;
        loop {
            let sig = signals.next().await;
            match counter.on_signal(sig) {
                SignalAction::Begin(sig) => {
                    let plan = ShutdownPlan {
                        started: Instant::now(),
                        budget,
                    };
                    let _ = PLAN.set(plan);
                    info!(
                        "{} received — shutting down (at most {}s; send it again to exit now)",
                        sig.name(),
                        budget.as_secs()
                    );
                    spawn_watchdog(plan.deadline(), move || {
                        force_exit(
                            sig.exit_code(),
                            &format!(
                                "graceful shutdown did not finish within {}s (--shutdown-timeout)",
                                budget.as_secs()
                            ),
                        )
                    });
                    let _ = tx.send(Some(sig));
                }
                SignalAction::Force(sig) => {
                    force_exit(sig.exit_code(), &format!("second {}", sig.name()));
                }
            }
        }
    });
    ShutdownRequest(rx)
}

struct Signals {
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
}

impl Signals {
    fn register() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                int: signal(SignalKind::interrupt())?,
                term: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn next(&mut self) -> StopSignal {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.int.recv() => StopSignal::Interrupt,
                _ = self.term.recv() => StopSignal::Terminate,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            StopSignal::Interrupt
        }
    }
}

pub fn spawn_watchdog<F>(deadline: Instant, fire: F)
where
    F: FnOnce() + Send + 'static,
{
    let job = move || {
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::park_timeout(deadline - now);
        }
        fire();
    };
    if let Err(failure) = xerj_common::thread_spawn::try_spawn_unguarded("xerj-stop-watchdog", job)
    {
        warn!(
            "could not start the shutdown watchdog ({}); a second signal still exits at once",
            failure.error
        );
    }
}

static FORCING: AtomicBool = AtomicBool::new(false);

pub fn sync_wal_bounded(budget: Duration) -> Option<usize> {
    let (tx, rx) = std::sync::mpsc::channel();
    let deadline = Instant::now() + budget;
    let job = move || {
        let report = xerj_storage::wal_fsync::sync_pending_now(deadline);
        let _ = tx.send(report.synced);
    };
    match xerj_common::thread_spawn::try_spawn_unguarded("xerj-stop-walsync", job) {
        Ok(_) => rx.recv_timeout(budget).ok(),
        Err(failure) => {
            if let Some(job) = failure.job {
                job();
            }
            rx.try_recv().ok()
        }
    }
}

pub fn force_exit(code: i32, reason: &str) -> ! {
    if FORCING.swap(true, Ordering::SeqCst) {
        loop {
            std::thread::park();
        }
    }
    warn!("{reason}; fsyncing the WAL and exiting");
    match sync_wal_bounded(WAL_SYNC_BUDGET) {
        Some(n) => warn!("WAL fsynced ({n} shards); exit {code}"),
        None => error!(
            "WAL fsync did not finish within {}s; exit {code}",
            WAL_SYNC_BUDGET.as_secs()
        ),
    }
    #[cfg(unix)]
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    #[test]
    fn timeout_resolution_flag_then_env_then_default() {
        assert_eq!(
            resolve_timeout(None, None).unwrap(),
            DEFAULT_SHUTDOWN_TIMEOUT
        );
        assert_eq!(
            resolve_timeout(None, Some("")).unwrap(),
            DEFAULT_SHUTDOWN_TIMEOUT
        );
        assert_eq!(
            resolve_timeout(None, Some("7")).unwrap(),
            Duration::from_secs(7)
        );
        assert_eq!(
            resolve_timeout(Some("3"), Some("7")).unwrap(),
            Duration::from_secs(3)
        );
        for bad in ["0", "-1", "1.5", "abc", "3601"] {
            assert!(resolve_timeout(Some(bad), None).is_err(), "{bad}");
            assert!(resolve_timeout(None, Some(bad)).is_err(), "{bad}");
        }
        assert!(resolve_timeout(None, Some("x"))
            .unwrap_err()
            .contains(SHUTDOWN_TIMEOUT_ENV));
    }

    #[test]
    fn second_signal_forces() {
        let mut c = SignalCounter::default();
        assert_eq!(
            c.on_signal(StopSignal::Terminate),
            SignalAction::Begin(StopSignal::Terminate)
        );
        assert_eq!(
            c.on_signal(StopSignal::Interrupt),
            SignalAction::Force(StopSignal::Interrupt)
        );
        assert_eq!(
            c.on_signal(StopSignal::Interrupt),
            SignalAction::Force(StopSignal::Interrupt)
        );
        assert_eq!(StopSignal::Interrupt.exit_code(), 130);
        assert_eq!(StopSignal::Terminate.exit_code(), 143);
    }

    #[test]
    fn phases_fit_inside_the_budget() {
        let t0 = Instant::now();
        let p = ShutdownPlan {
            started: t0,
            budget: Duration::from_secs(30),
        };
        assert_eq!(p.in_flight_deadline() - t0, Duration::from_secs(10));
        assert_eq!(p.flush_deadline() - t0, Duration::from_secs(20));
        assert_eq!(p.deadline() - t0, Duration::from_secs(30));
        let at = p.flush_deadline();
        assert!(
            p.listener_grace(at) + p.runtime_teardown(at + p.listener_grace(at)) <= p.remaining(at)
        );
        let small = ShutdownPlan {
            started: t0,
            budget: Duration::from_secs(3),
        };
        assert_eq!(small.in_flight_deadline() - t0, Duration::from_secs(1));
        assert_eq!(small.flush_deadline() - t0, Duration::from_secs(2));
        assert_eq!(
            small.runtime_teardown(t0 + Duration::from_secs(5)),
            Duration::ZERO
        );
        let long = ShutdownPlan {
            started: t0,
            budget: Duration::from_secs(600),
        };
        assert_eq!(long.in_flight_deadline() - t0, Duration::from_secs(10));
    }

    #[test]
    fn watchdog_fires_at_its_deadline_not_before() {
        let fired = Arc::new(AtomicU32::new(0));
        let f = Arc::clone(&fired);
        let t0 = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_watchdog(t0 + Duration::from_millis(300), move || {
            f.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(Instant::now());
        });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fired.load(Ordering::SeqCst), 0, "fired early");
        let at = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("never fired");
        assert!(at - t0 >= Duration::from_millis(300));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_request_made_before_the_wait_is_seen() {
        let (tx, rx) = tokio::sync::watch::channel(None);
        let mut req = ShutdownRequest(rx);
        assert!(!req.is_requested());
        tx.send(Some(StopSignal::Interrupt)).unwrap();
        let sig = tokio::time::timeout(Duration::from_secs(5), req.requested())
            .await
            .expect("an earlier signal must resolve the wait at once");
        assert_eq!(sig, StopSignal::Interrupt);
        assert!(req.is_requested());
    }
}
