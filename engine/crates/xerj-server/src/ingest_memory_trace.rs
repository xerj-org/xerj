use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use xerj_engine::ingest_memory::{
    self, AllocatorSnapshot, EventKind, ProcessSnapshot, TraceMode, LEDGER,
};
use xerj_engine::Engine;

const CHANNEL_CAPACITY: usize = 256;

pub struct TraceRuntime {
    stop: Arc<AtomicBool>,
    wake: Arc<(Mutex<()>, Condvar)>,
    sampler: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
}

impl Drop for TraceRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake.1.notify_all();
        if let Some(handle) = self.sampler.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped != 0 {
            tracing::warn!(
                target: "xerj::ingest_memory",
                dropped,
                "ingest-memory trace events dropped by bounded sink"
            );
        }
    }
}

pub fn spawn(engine: &Arc<Engine>) -> Option<TraceRuntime> {
    let config = ingest_memory::config();
    if config.mode == TraceMode::Off {
        return None;
    }

    let (tx, rx) = mpsc::sync_channel::<String>(CHANNEL_CAPACITY);
    let stop = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));
    let writer_dropped = Arc::clone(&dropped);
    let writer = std::thread::Builder::new()
        .name("xerj-ingest-memory-writer".into())
        .spawn(move || writer_loop(rx, writer_dropped))
        .ok()?;

    let weak = Arc::downgrade(engine);
    let sampler_stop = Arc::clone(&stop);
    let wake = Arc::new((Mutex::new(()), Condvar::new()));
    let sampler_wake = Arc::clone(&wake);
    let sampler_dropped = Arc::clone(&dropped);
    let sampler = std::thread::Builder::new()
        .name("xerj-ingest-memory-sampler".into())
        .spawn(move || {
            send_snapshot(&tx, &sampler_dropped, EventKind::TraceStart, false);
            sample_loop(
                weak,
                tx,
                sampler_stop,
                sampler_wake,
                sampler_dropped,
                Duration::from_millis(config.sample_ms),
            );
        })
        .ok()?;

    Some(TraceRuntime {
        stop,
        wake,
        sampler: Some(sampler),
        writer: Some(writer),
        dropped,
    })
}

fn sample_loop(
    engine: Weak<Engine>,
    tx: mpsc::SyncSender<String>,
    stop: Arc<AtomicBool>,
    wake: Arc<(Mutex<()>, Condvar)>,
    dropped: Arc<AtomicU64>,
    period: Duration,
) {
    while !stop.load(Ordering::Acquire) {
        let Some(engine) = engine.upgrade() else {
            break;
        };
        refresh_snapshot_state(&engine);
        send_snapshot(&tx, &dropped, EventKind::TraceSnapshot, true);
        wait_for_stop_or_period(&stop, &wake, period);
    }

    // The shutdown path force-flushes every index before dropping
    // `TraceRuntime`. That can change the sampled memtable gauge after the
    // final periodic snapshot. Refresh synchronously here so `trace_stop`
    // describes the post-flush state even when shutdown happens immediately
    // after ingest, without waiting for another sampler period.
    if let Some(engine) = engine.upgrade() {
        refresh_snapshot_state(&engine);
    }
    send_snapshot(&tx, &dropped, EventKind::TraceStop, false);
}

fn refresh_snapshot_state(engine: &Engine) {
    refresh_snapshot_state_with(
        engine.total_memtable_bytes(),
        allocator_snapshot(),
        xerj_engine::governor::current_rss_bytes(),
        process_cpu_time_ns(),
    );
}

fn refresh_snapshot_state_with(
    memtable_bytes: u64,
    allocator: AllocatorSnapshot,
    rss: u64,
    cpu_time_ns: Option<u64>,
) {
    LEDGER.observe(
        ingest_memory::Category::MemtableActive,
        memtable_bytes as usize,
    );
    let process = ProcessSnapshot {
        rss: (rss != 0).then_some(rss),
        rss_ok: rss != 0,
        cpu_time_ns,
        cpu_time_ok: cpu_time_ns.is_some(),
    };
    LEDGER.update_physical(allocator, process);
}

fn wait_for_stop_or_period(stop: &AtomicBool, wake: &(Mutex<()>, Condvar), period: Duration) {
    let guard = wake.0.lock().unwrap_or_else(|e| e.into_inner());
    let _ = wake
        .1
        .wait_timeout_while(guard, period, |_| !stop.load(Ordering::Acquire));
}

fn send_snapshot(
    tx: &mpsc::SyncSender<String>,
    dropped: &AtomicU64,
    kind: EventKind,
    sampled: bool,
) {
    let Some(event) = ingest_memory::snapshot_event(kind, sampled) else {
        return;
    };
    match serde_json::to_string(&event) {
        Ok(line) => {
            if tx.try_send(line).is_err() {
                dropped.fetch_add(1, Ordering::Relaxed);
                LEDGER.record_dropped_event();
            }
        }
        Err(_) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            LEDGER.record_dropped_event();
        }
    }
}

#[cfg(unix)]
fn process_cpu_time_ns() -> Option<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid writable timespec and the process CPU clock
    // requires no caller-managed lifetime.
    if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) } != 0 {
        return None;
    }
    let seconds = u64::try_from(time.tv_sec).ok()?;
    let nanos = u64::try_from(time.tv_nsec).ok()?;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanos)
}

#[cfg(not(unix))]
fn process_cpu_time_ns() -> Option<u64> {
    None
}

fn writer_loop(rx: mpsc::Receiver<String>, dropped: Arc<AtomicU64>) {
    let output =
        std::env::var("XERJ_INGEST_MEMORY_OUTPUT").unwrap_or_else(|_| "tracing".to_string());
    if output == "tracing" {
        while let Ok(line) = rx.recv() {
            tracing::info!(target: "xerj::ingest_memory", "{line}");
        }
        while let Ok(line) = rx.try_recv() {
            tracing::info!(target: "xerj::ingest_memory", "{line}");
        }
        return;
    }

    let mut file = match open_output(&output) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(
                target: "xerj::ingest_memory",
                %error,
                path = %output,
                "could not open ingest-memory NDJSON output"
            );
            return;
        }
    };
    while let Ok(line) = rx.recv() {
        if let Err(error) = writeln!(file, "{line}") {
            record_sink_error(&dropped, &output, &error);
            break;
        }
    }
    while let Ok(line) = rx.try_recv() {
        if let Err(error) = writeln!(file, "{line}") {
            record_sink_error(&dropped, &output, &error);
            break;
        }
    }
}

fn open_output(path: &str) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // `mode` applies only when this call creates the file. An existing
        // operator-managed file keeps its permissions and append semantics.
        options.mode(0o600);
    }
    options.open(path)
}

fn record_sink_error(dropped: &AtomicU64, path: &str, error: &std::io::Error) {
    dropped.fetch_add(1, Ordering::Relaxed);
    LEDGER.record_dropped_event();
    tracing::warn!(
        target: "xerj::ingest_memory",
        %error,
        path,
        "could not write ingest-memory NDJSON output; stopping file sink"
    );
}

#[cfg(not(target_env = "msvc"))]
fn allocator_snapshot() -> AllocatorSnapshot {
    use tikv_jemalloc_ctl::{epoch, stats};
    if epoch::advance().is_err() {
        return AllocatorSnapshot::default();
    }
    match (
        stats::allocated::read(),
        stats::active::read(),
        stats::resident::read(),
    ) {
        (Ok(allocated), Ok(active), Ok(resident)) => AllocatorSnapshot {
            allocated: Some(allocated as u64),
            active: Some(active as u64),
            resident: Some(resident as u64),
            epoch_ok: true,
        },
        _ => AllocatorSnapshot::default(),
    }
}

#[cfg(target_env = "msvc")]
fn allocator_snapshot() -> AllocatorSnapshot {
    AllocatorSnapshot::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_sink_reports_overflow_without_blocking() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let dropped = AtomicU64::new(0);
        let line = r#"{"schema":"xerj.ingest_memory.v1"}"#.to_string();
        assert!(tx.try_send(line.clone()).is_ok());
        if tx.try_send(line).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn allocator_probe_is_explicitly_available_or_null() {
        let snapshot = allocator_snapshot();
        if snapshot.epoch_ok {
            assert!(snapshot.allocated.is_some());
            assert!(snapshot.active.is_some());
            assert!(snapshot.resident.is_some());
        } else {
            assert_eq!(snapshot.allocated, None);
            assert_eq!(snapshot.active, None);
            assert_eq!(snapshot.resident, None);
        }
    }

    #[test]
    fn process_cpu_probe_is_explicitly_available_or_null() {
        let first = process_cpu_time_ns();
        let second = process_cpu_time_ns();
        match (first, second) {
            (Some(first), Some(second)) => assert!(second >= first),
            (None, None) => {}
            _ => panic!("CPU-time probe support changed within one process"),
        }
    }

    #[test]
    fn sampler_wait_is_interruptible_even_at_max_period() {
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((Mutex::new(()), Condvar::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_wake = Arc::clone(&wake);
        let started = std::time::Instant::now();
        let handle = std::thread::spawn(move || {
            wait_for_stop_or_period(&thread_stop, &thread_wake, Duration::from_secs(60));
        });
        stop.store(true, Ordering::Release);
        wake.1.notify_all();
        handle.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn stop_refresh_observes_changed_gauge_without_waiting_for_period() {
        LEDGER.observe(ingest_memory::Category::MemtableActive, 8192);

        let started = std::time::Instant::now();
        refresh_snapshot_state_with(
            0,
            AllocatorSnapshot {
                allocated: Some(11),
                active: Some(12),
                resident: Some(13),
                epoch_ok: true,
            },
            14,
            Some(15),
        );

        assert!(started.elapsed() < Duration::from_secs(1));
        let memtable = LEDGER.gauge(
            ingest_memory::Category::MemtableActive,
            ingest_memory::Measurement::Estimated,
        );
        assert_eq!(memtable.current, 0);
    }

    #[cfg(unix)]
    #[test]
    fn output_file_is_private_when_created_and_preserves_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let created = dir.path().join("created.ndjson");
        drop(open_output(created.to_str().unwrap()).unwrap());
        assert_eq!(
            std::fs::metadata(&created).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let existing = dir.path().join("existing.ndjson");
        std::fs::write(&existing, b"existing\n").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut file = open_output(existing.to_str().unwrap()).unwrap();
        writeln!(file, "appended").unwrap();
        drop(file);
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            std::fs::read_to_string(existing).unwrap(),
            "existing\nappended\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_write_failure_is_accounted() {
        let mut file = open_output("/dev/full").unwrap();
        let dropped = AtomicU64::new(0);
        let error = writeln!(file, "cannot fit").unwrap_err();
        record_sink_error(&dropped, "/dev/full", &error);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }
}
