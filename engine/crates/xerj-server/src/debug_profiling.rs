//! Bounded, local profiling for controlled benchmark runs.
//!
//! Compiled only with `--features debug-profiling`. There is deliberately no
//! HTTP endpoint. The output directory must exist and captures are requested
//! before process start:
//!
//! ```text
//! XERJ_DEBUG_PROFILE_DIR=/private/run \
//! XERJ_DEBUG_CPU_SECONDS=30 XERJ_DEBUG_HEAP_SECONDS=30 xerj
//! ```

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use pprof::protos::Message;

const MAX_CAPTURE_SECONDS: u64 = 300;
const MAX_CPU_HZ: i32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    output_dir: PathBuf,
    cpu_seconds: Option<u64>,
    heap_seconds: Option<u64>,
    cpu_hz: i32,
    delay_seconds: u64,
}

impl Config {
    fn from_env() -> Result<Option<Self>> {
        let Some(output) = std::env::var_os("XERJ_DEBUG_PROFILE_DIR") else {
            return Ok(None);
        };
        Self::parse(
            PathBuf::from(output),
            std::env::var("XERJ_DEBUG_CPU_SECONDS").ok().as_deref(),
            std::env::var("XERJ_DEBUG_HEAP_SECONDS").ok().as_deref(),
            std::env::var("XERJ_DEBUG_CPU_HZ").ok().as_deref(),
            std::env::var("XERJ_DEBUG_PROFILE_DELAY_SECONDS")
                .ok()
                .as_deref(),
        )
        .map(Some)
    }

    fn parse(
        output_dir: PathBuf,
        cpu_seconds: Option<&str>,
        heap_seconds: Option<&str>,
        cpu_hz: Option<&str>,
        delay_seconds: Option<&str>,
    ) -> Result<Self> {
        let output_dir = output_dir.canonicalize().with_context(|| {
            format!("profile directory does not exist: {}", output_dir.display())
        })?;
        if !output_dir.is_dir() {
            return Err(anyhow!(
                "profile output is not a directory: {}",
                output_dir.display()
            ));
        }
        let cpu_seconds = parse_duration("XERJ_DEBUG_CPU_SECONDS", cpu_seconds)?;
        let heap_seconds = parse_duration("XERJ_DEBUG_HEAP_SECONDS", heap_seconds)?;
        if cpu_seconds.is_none() && heap_seconds.is_none() {
            return Err(anyhow!(
                "set XERJ_DEBUG_CPU_SECONDS and/or XERJ_DEBUG_HEAP_SECONDS"
            ));
        }
        let cpu_hz = match cpu_hz {
            Some(value) => value
                .parse::<i32>()
                .with_context(|| format!("XERJ_DEBUG_CPU_HZ is not an integer: {value}"))?,
            None => 100,
        };
        if !(1..=MAX_CPU_HZ).contains(&cpu_hz) {
            return Err(anyhow!(
                "XERJ_DEBUG_CPU_HZ must be in 1..={MAX_CPU_HZ}, got {cpu_hz}"
            ));
        }
        let delay_seconds = match delay_seconds {
            Some(value) => value.parse::<u64>().with_context(|| {
                format!("XERJ_DEBUG_PROFILE_DELAY_SECONDS is not an integer: {value}")
            })?,
            None => 0,
        };
        if delay_seconds > MAX_CAPTURE_SECONDS {
            return Err(anyhow!(
                "XERJ_DEBUG_PROFILE_DELAY_SECONDS must be in 0..={MAX_CAPTURE_SECONDS}, got {delay_seconds}"
            ));
        }
        Ok(Self {
            output_dir,
            cpu_seconds,
            heap_seconds,
            cpu_hz,
            delay_seconds,
        })
    }
}

fn parse_duration(name: &str, value: Option<&str>) -> Result<Option<u64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let seconds = value
        .parse::<u64>()
        .with_context(|| format!("{name} is not an integer: {value}"))?;
    if !(1..=MAX_CAPTURE_SECONDS).contains(&seconds) {
        return Err(anyhow!(
            "{name} must be in 1..={MAX_CAPTURE_SECONDS}, got {seconds}"
        ));
    }
    Ok(Some(seconds))
}

pub struct ProfileRuntime {
    workers: Vec<JoinHandle<()>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    lock_path: Option<PathBuf>,
    _lock: File,
}

impl Drop for ProfileRuntime {
    fn drop(&mut self) {
        *self
            .stop
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.stop.1.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        if let Some(path) = self.lock_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn spawn() -> Result<Option<ProfileRuntime>> {
    let Some(config) = Config::from_env()? else {
        return Ok(None);
    };
    spawn_configured(config).map(Some)
}

fn spawn_configured(config: Config) -> Result<ProfileRuntime> {
    let lock_path = config.output_dir.join(".xerj-debug-profile.lock");
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "profile directory is already in use or contains stale lock: {}",
                lock_path.display()
            )
        })?;
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let mut runtime = ProfileRuntime {
        workers: Vec::new(),
        stop,
        lock_path: Some(lock_path),
        _lock: lock,
    };
    if let Some(seconds) = config.cpu_seconds {
        let directory = config.output_dir.clone();
        let stop = Arc::clone(&runtime.stop);
        let frequency = config.cpu_hz;
        let delay = config.delay_seconds;
        runtime.workers.push(
            std::thread::Builder::new()
                .name("xerj-debug-cpu-profile".into())
                .spawn(move || {
                    if wait_for_duration_or_stop(&stop, Duration::from_secs(delay)) {
                        return;
                    }
                    if let Err(error) = capture_cpu(&directory, seconds, frequency, &stop) {
                        write_profile_error(&directory, "cpu", &error);
                        tracing::error!(%error, "CPU profile capture failed");
                    }
                })
                .context("spawn CPU profile worker")?,
        );
    }
    if let Some(seconds) = config.heap_seconds {
        let directory = config.output_dir.clone();
        let stop = Arc::clone(&runtime.stop);
        let delay = config.delay_seconds;
        runtime.workers.push(
            std::thread::Builder::new()
                .name("xerj-debug-heap-profile".into())
                .spawn(move || {
                    if wait_for_duration_or_stop(&stop, Duration::from_secs(delay)) {
                        return;
                    }
                    if let Err(error) = capture_heap(&directory, seconds, &stop) {
                        write_profile_error(&directory, "heap", &error);
                        tracing::error!(%error, "heap profile capture failed");
                    }
                })
                .context("spawn heap profile worker")?,
        );
    }
    Ok(runtime)
}

fn capture_cpu(
    output_dir: &Path,
    seconds: u64,
    frequency: i32,
    stop: &(Mutex<bool>, Condvar),
) -> Result<()> {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(frequency)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .context("start SIGPROF CPU sampler")?;
    wait_for_duration_or_stop(stop, Duration::from_secs(seconds));
    let report = guard.report().build().context("build CPU profile")?;
    let profile = report.pprof().context("convert CPU profile to pprof")?;
    let mut bytes = Vec::new();
    profile.encode(&mut bytes).context("encode CPU profile")?;
    write_new_atomic(output_dir, "cpu.pb", &bytes)
}

fn capture_heap(output_dir: &Path, seconds: u64, stop: &(Mutex<bool>, Condvar)) -> Result<()> {
    let control = Arc::clone(
        jemalloc_pprof::PROF_CTL
            .as_ref()
            .ok_or_else(|| anyhow!("jemalloc was not compiled/configured with profiling"))?,
    );
    let mut ctl = control.blocking_lock();
    if ctl.activated() {
        return Err(anyhow!("jemalloc profiling is already active"));
    }
    ctl.activate().context("activate jemalloc profiling")?;
    drop(ctl);
    wait_for_duration_or_stop(stop, Duration::from_secs(seconds));
    let mut ctl = control.blocking_lock();
    let dump = ctl.dump_pprof().context("dump heap pprof");
    let deactivate = ctl.deactivate().context("deactivate jemalloc profiling");
    let bytes = dump?;
    deactivate?;
    write_new_atomic(output_dir, "heap.pb.gz", &bytes)
}

fn wait_for_duration_or_stop(stop: &(Mutex<bool>, Condvar), duration: Duration) -> bool {
    let guard = stop.0.lock().unwrap_or_else(|error| error.into_inner());
    let (guard, _) = stop
        .1
        .wait_timeout_while(guard, duration, |stopped| !*stopped)
        .unwrap_or_else(|error| error.into_inner());
    *guard
}

fn write_new_atomic(output_dir: &Path, name: &str, contents: &[u8]) -> Result<()> {
    let final_path = output_dir.join(name);
    let temporary = output_dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("create profile temporary file: {}", temporary.display()))?;
    let result = (|| -> io::Result<()> {
        file.write_all(contents)?;
        file.sync_all()?;
        // hard_link fails if the final name exists; rename would overwrite it.
        std::fs::hard_link(&temporary, &final_path)?;
        std::fs::remove_file(&temporary)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("publish profile: {}", final_path.display()))
}

fn write_profile_error(output_dir: &Path, kind: &str, error: &anyhow::Error) {
    let payload = serde_json::json!({
        "schema": "xerj.debug_profile.error.v1",
        "profiler": kind,
        "error": format!("{error:#}"),
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&payload) {
        let _ = write_new_atomic(output_dir, &format!("{kind}-error.json"), &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_requires_a_bounded_capture() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Config::parse(dir.path().into(), None, None, None, None).is_err());
        for invalid in ["0", "301", "garbage"] {
            assert!(Config::parse(dir.path().into(), Some(invalid), None, None, None).is_err());
        }
    }

    #[test]
    fn configuration_defaults_to_low_rate_cpu_sampling() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::parse(dir.path().into(), Some("2"), None, None, Some("3")).unwrap();
        assert_eq!(config.cpu_seconds, Some(2));
        assert_eq!(config.cpu_hz, 100);
        assert_eq!(config.delay_seconds, 3);
    }

    #[test]
    fn atomic_publication_never_overwrites_evidence() {
        let dir = tempfile::tempdir().unwrap();
        write_new_atomic(dir.path(), "profile.pb", b"first").unwrap();
        assert!(write_new_atomic(dir.path(), "profile.pb", b"second").is_err());
        assert_eq!(
            std::fs::read(dir.path().join("profile.pb")).unwrap(),
            b"first"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("profile.pb"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn capture_wait_is_interruptible() {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_stop = Arc::clone(&stop);
        let started = std::time::Instant::now();
        let worker = std::thread::spawn(move || {
            wait_for_duration_or_stop(&thread_stop, Duration::from_secs(300))
        });
        *stop.0.lock().unwrap() = true;
        stop.1.notify_all();
        worker.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn heap_capture_is_a_pprof_gzip() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let stop = (Mutex::new(false), Condvar::new());
        let allocation = std::thread::spawn(|| vec![0x5a_u8; 8 * 1024 * 1024]);
        capture_heap(dir.path(), 1, &stop).unwrap();
        let bytes = std::fs::read(dir.path().join("heap.pb.gz")).unwrap();
        assert_eq!(bytes.get(..2), Some(&[0x1f, 0x8b][..]));
        let mut protobuf = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut protobuf)
            .unwrap();
        pprof::protos::Profile::decode(protobuf.as_slice()).unwrap();
        drop(allocation.join().unwrap());
    }

    #[test]
    fn cpu_capture_is_a_nonempty_pprof_protobuf() {
        let dir = tempfile::tempdir().unwrap();
        let stop = (Mutex::new(false), Condvar::new());
        let work = std::thread::spawn(|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            let mut value = 1_u64;
            while std::time::Instant::now() < deadline {
                value = std::hint::black_box(value.wrapping_mul(6364136223846793005));
            }
        });
        capture_cpu(dir.path(), 1, 100, &stop).unwrap();
        let bytes = std::fs::read(dir.path().join("cpu.pb")).unwrap();
        pprof::protos::Profile::decode(bytes.as_slice()).unwrap();
        work.join().unwrap();
    }
}
