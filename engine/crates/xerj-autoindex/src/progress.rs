//! Progress, percent and ETA for `xerj autoindex` (#241).
//!
//! The contract this module implements:
//!
//! * **stdout is the RESULT, stderr is PROGRESS.** `--json` keeps stdout a
//!   single parseable document; liveness never contaminates it.
//! * **One ticker thread owns the progress surface.** Workers only bump
//!   atomics. A worker can block for many seconds inside one large file, so a
//!   worker-driven heartbeat cannot bound silence — a timer can.
//! * **Liveness is time-based, never item-based.** `--progress-interval`
//!   seconds is the guaranteed upper bound on silence between the first phase
//!   and the terminal line. It is an upper bound on silence *only* — never a
//!   floor on the run: stopping the ticker must be observed at once, not at the
//!   next tick, or a run that exits early pays the whole interval to shut down.
//!   `--progress none` (`--quiet`) opts out of the surface entirely and so out
//!   of both the liveness bound and the terminal line.
//! * **Nothing is displayed that cannot be computed honestly.** A percent is
//!   emitted only when a real denominator exists, and an ETA only after the
//!   quality gate below is satisfied; otherwise the field is literally
//!   `unknown` (`null` in JSON) rather than a comforting number.
//!
//! ETA is derived from **bytes**, not from file count. File count is a badly
//! skewed proxy for work — in the corpus that produced #241 the largest single
//! file held 40.4% of all bytes, so a files-done ETA collapses to ~0 while
//! minutes of work remain.
//!
//! Prior art, consulted and adapted rather than copied (both Apache-2.0/MIT and
//! cited per the repo's reference-coding rule):
//!
//! * `quickwit-common/src/progress.rs:78` `record_progress()`, `:90`
//!   `protect_zone()`, `:117` `registered_activity_since_last_call()` — a
//!   supervisor polls an atomic, and work that will block for a long time
//!   declares a *protected zone* so the supervisor does not judge it hung. We
//!   need the same property but cannot ask each call site to declare a zone, so
//!   the ticker here runs unconditionally and [`Progress::file`] names what is
//!   being waited on — the in-flight table is this design's answer to the
//!   question a protected zone answers ("it is not hung, it is doing *this*").
//! * `meilisearch/crates/milli/src/progress.rs:21` `Progress`, `:137` the
//!   nested overall percentage, `:331` `ProgressStepView { current_step,
//!   finished, total }` — the shape of an honest cross-phase percentage. We
//!   keep the per-phase denominator explicit instead of composing one global
//!   number, because autoindex's phases have no common unit.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Smoothing factor on the per-tick throughput sample.
const EWMA_ALPHA: f64 = 0.2;
/// No ETA until the phase has run at least this long.
const ETA_MIN_PHASE_SECS: f64 = 5.0;
/// No ETA until this fraction of the phase is complete.
const ETA_MIN_FRACTION: f64 = 0.02;
/// Below this fraction a computed ETA is labelled `rough`, not `good`.
const ETA_GOOD_FRACTION: f64 = 0.10;
/// A displayed ETA may not move by more than this fraction per tick.
const ETA_MAX_STEP: f64 = 0.20;
/// Name at most this many still-running files when the tail goes quiet.
const STRAGGLER_MAX: usize = 3;
/// Fallback terminal width when `COLUMNS` is unset.
const DEFAULT_COLUMNS: usize = 100;

/// Live-redraw cadence on a terminal.
pub const TTY_INTERVAL: Duration = Duration::from_secs(1);
/// Line cadence for pipes, agents and CI.
pub const STREAM_INTERVAL: Duration = Duration::from_secs(5);

/// `--progress` as the user wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// Terminal → live line; anything else → `plain`.
    Auto,
    /// One periodic key=value line, always, whatever stderr is.
    Plain,
    /// One periodic JSON object per line.
    Json,
    /// Emit nothing (what `--quiet` selects).
    None,
}

impl ProgressMode {
    /// Parse the flag value. An unrecognised mode is an error, never a silent
    /// fallback to `auto`: accepting a value we will not honour is exactly the
    /// class of defect tracked in #204.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "auto" => Ok(Self::Auto),
            "plain" => Ok(Self::Plain),
            "json" => Ok(Self::Json),
            "none" => Ok(Self::None),
            other => Err(format!(
                "--progress {other}: expected one of auto, plain, json, none"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Plain => "plain",
            Self::Json => "json",
            Self::None => "none",
        }
    }
}

/// What `auto` actually resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// In-place redraw with `\r`, for a human at a terminal.
    Tty,
    /// One `xerj-progress key=value …` line per tick.
    Plain,
    /// One JSON object per tick.
    Json,
    /// Nothing at all.
    Silent,
}

/// Resolve a mode against the environment. Pure so it can be tested without a
/// terminal: `auto` is a terminal surface only when stderr is a tty, `TERM` is
/// not `dumb`, and `CI` is unset.
pub fn resolve(mode: ProgressMode, stderr_tty: bool, term: Option<&str>, ci: bool) -> Surface {
    match mode {
        ProgressMode::None => Surface::Silent,
        ProgressMode::Plain => Surface::Plain,
        ProgressMode::Json => Surface::Json,
        ProgressMode::Auto => {
            if stderr_tty && term != Some("dumb") && !ci {
                Surface::Tty
            } else {
                Surface::Plain
            }
        }
    }
}

/// `resolve` against this process's real environment.
pub fn detect(mode: ProgressMode) -> Surface {
    let term = std::env::var("TERM").ok();
    let ci = std::env::var_os("CI").is_some_and(|value| !value.is_empty());
    resolve(mode, std::io::stderr().is_terminal(), term.as_deref(), ci)
}

/// Default cadence for a surface: a human wants a second, a log wants five.
pub fn default_interval(surface: Surface) -> Duration {
    match surface {
        Surface::Tty => TTY_INTERVAL,
        _ => STREAM_INTERVAL,
    }
}

/// Serialises the tests that redirect the process-wide progress surface.
#[cfg(test)]
pub static SINK_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
static TEST_SINK: Mutex<Option<Arc<Mutex<Vec<u8>>>>> = Mutex::new(None);

/// Redirect every `Progress::new` in this process into `buffer` until the
/// returned guard drops. Hold `SINK_TEST_LOCK` for the whole window.
#[cfg(test)]
pub fn install_test_sink(buffer: &Arc<Mutex<Vec<u8>>>) -> TestSinkGuard {
    *TEST_SINK.lock().unwrap() = Some(Arc::clone(buffer));
    TestSinkGuard
}

#[cfg(test)]
pub struct TestSinkGuard;

#[cfg(test)]
impl Drop for TestSinkGuard {
    fn drop(&mut self) {
        *TEST_SINK.lock().unwrap() = None;
    }
}

enum Sink {
    Stderr,
    /// Test-only: the identical render path, captured in memory.
    #[cfg(test)]
    Buffer(Arc<Mutex<Vec<u8>>>),
}

impl Sink {
    fn write(&self, bytes: &[u8]) {
        match self {
            Self::Stderr => {
                let stderr = std::io::stderr();
                let mut handle = stderr.lock();
                let _ = handle.write_all(bytes);
                let _ = handle.flush();
            }
            #[cfg(test)]
            Self::Buffer(buffer) => buffer.lock().unwrap().extend_from_slice(bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Eta {
    Unknown,
    /// Work is still running but nothing has completed for a long time, so
    /// there is no current throughput measurement to extrapolate from.
    Stalled,
    Rough(f64),
    Good(f64),
}

impl Eta {
    fn seconds(self) -> Option<f64> {
        match self {
            Self::Unknown | Self::Stalled => None,
            Self::Rough(s) | Self::Good(s) => Some(s),
        }
    }

    fn quality(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Stalled => "stalled",
            Self::Rough(_) => "rough",
            Self::Good(_) => "good",
        }
    }
}

/// Whether the percent/ETA denominator is bytes of work or a count of items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Basis {
    Bytes,
    Items,
    None,
}

impl Basis {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Items => "items",
            Self::None => "none",
        }
    }
}

struct State {
    phase: &'static str,
    items_total: u64,
    bytes_total: u64,
    phase_started: Instant,
    last_sample_at: Instant,
    last_sample_units: u64,
    /// When a unit last actually completed. Progress inside a single large
    /// file is not observable from here, so this is the only honest signal
    /// that the throughput estimate is still current.
    last_advance_at: Instant,
    rate: Option<f64>,
    shown_eta: Option<f64>,
    in_flight: Vec<(u64, String, u64)>,
    next_seq: u64,
    tty_width: usize,
}

struct Snapshot {
    phase: &'static str,
    basis: Basis,
    items_done: u64,
    items_total: u64,
    bytes_done: u64,
    bytes_total: u64,
    percent: Option<f64>,
    rate: Option<f64>,
    eta: Eta,
    elapsed: f64,
    phase_elapsed: f64,
    since_progress: f64,
    waiting_on: Vec<(String, u64)>,
}

/// Shared progress state. Cheap for workers: the hot path is two relaxed
/// atomic adds plus one short mutex for the in-flight table.
pub struct Progress {
    surface: Surface,
    interval: Duration,
    started: Instant,
    items_done: AtomicU64,
    bytes_done: AtomicU64,
    reported: AtomicBool,
    state: Mutex<State>,
    stopped: Mutex<bool>,
    wake: Condvar,
    sink: Sink,
}

impl Progress {
    fn build(surface: Surface, interval: Duration, sink: Sink) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            surface,
            interval,
            started: now,
            items_done: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            reported: AtomicBool::new(false),
            state: Mutex::new(State {
                phase: "starting",
                items_total: 0,
                bytes_total: 0,
                phase_started: now,
                last_sample_at: now,
                last_sample_units: 0,
                last_advance_at: now,
                rate: None,
                shown_eta: None,
                in_flight: Vec::new(),
                next_seq: 0,
                tty_width: 0,
            }),
            stopped: Mutex::new(false),
            wake: Condvar::new(),
            sink,
        })
    }

    pub fn new(surface: Surface, interval: Duration) -> Arc<Self> {
        // Test hook, same shape as `REPLACEMENT_FAILPOINT` in lib.rs: an
        // end-to-end run must be observable through the REAL surface it uses
        // in production, not through a parallel one built for tests.
        #[cfg(test)]
        if let Some(buffer) = TEST_SINK.lock().unwrap().as_ref() {
            return Self::build(surface, interval, Sink::Buffer(Arc::clone(buffer)));
        }
        Self::build(surface, interval, Sink::Stderr)
    }

    /// A progress handle that emits nothing — `--quiet` / `--progress=none`,
    /// and the default for library callers that do not want a surface.
    pub fn silent() -> Arc<Self> {
        Self::new(Surface::Silent, STREAM_INTERVAL)
    }

    /// Test constructor: same code path, output captured in memory.
    #[cfg(test)]
    pub fn capture(surface: Surface, interval: Duration) -> (Arc<Self>, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let progress = Self::build(surface, interval, Sink::Buffer(Arc::clone(&buffer)));
        (progress, buffer)
    }

    pub fn surface(&self) -> Surface {
        self.surface
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn enabled(&self) -> bool {
        self.surface != Surface::Silent
    }

    /// Enter a phase. Counters reset, the ETA estimator restarts (throughput
    /// in one phase says nothing about the next), and a line is emitted at
    /// once so a phase transition is never hidden behind the tick cadence.
    ///
    /// `items_total` / `bytes_total` of 0 mean "not knowable here" and produce
    /// `pct=unknown` rather than an invented denominator.
    pub fn phase(&self, name: &'static str, items_total: u64, bytes_total: u64) {
        if !self.enabled() {
            return;
        }
        let now = Instant::now();
        {
            let mut state = self.state.lock().unwrap();
            state.phase = name;
            state.items_total = items_total;
            state.bytes_total = bytes_total;
            state.phase_started = now;
            state.last_sample_at = now;
            state.last_sample_units = 0;
            state.last_advance_at = now;
            state.rate = None;
            state.shown_eta = None;
            state.in_flight.clear();
        }
        self.items_done.store(0, Ordering::Relaxed);
        self.bytes_done.store(0, Ordering::Relaxed);
        self.emit();
    }

    /// One unit of the current phase finished.
    pub fn item_done(&self, bytes: u64) {
        self.items_done.fetch_add(1, Ordering::Relaxed);
        if bytes > 0 {
            self.bytes_done.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Register a file as in flight. The returned guard counts it done exactly
    /// once, on every exit path — including the `continue`s that skip a junk
    /// file — because progress measures work *drained from the queue*, not
    /// work that succeeded. Forgetting one would stall the bar below 100%.
    pub fn file(&self, rel: &str, bytes: u64) -> FileGuard<'_> {
        let seq = if self.enabled() {
            let mut state = self.state.lock().unwrap();
            let seq = state.next_seq;
            state.next_seq += 1;
            state.in_flight.push((seq, rel.to_string(), bytes));
            seq
        } else {
            0
        };
        FileGuard {
            progress: self,
            seq,
            bytes,
        }
    }

    fn file_finished(&self, seq: u64, bytes: u64) {
        if self.enabled() {
            let mut state = self.state.lock().unwrap();
            state
                .in_flight
                .retain(|(candidate, _, _)| *candidate != seq);
        }
        self.item_done(bytes);
    }

    /// A one-off human line. Routed through the same surface so it cannot
    /// scramble the terminal's in-place line, and so `--progress=json` keeps
    /// stderr machine-readable end to end.
    pub fn note(&self, message: &str) {
        match self.surface {
            Surface::Silent => {}
            Surface::Json => {
                let line = serde_json::json!({"event": "note", "message": message});
                self.sink.write(format!("{line}\n").as_bytes());
            }
            Surface::Plain => self.sink.write(format!("{message}\n").as_bytes()),
            Surface::Tty => {
                let mut state = self.state.lock().unwrap();
                let mut out = clear_tty(state.tty_width);
                state.tty_width = 0;
                drop(state);
                out.push_str(message);
                out.push('\n');
                self.sink.write(out.as_bytes());
            }
        }
    }

    /// Render one progress line now.
    pub fn tick(&self) {
        self.emit();
    }

    /// The terminal line, in every mode that has a surface at all. An exit code
    /// alone is ambiguous — autoindex exits 3 on success-with-junk — so the
    /// reason is spelled out.
    ///
    /// Every run that *reaches an exit* emits exactly one of these: on success
    /// from the call sites below, and on any error path from [`Ticker::drop`].
    ///
    /// Two documented exceptions, both of which a caller must plan for:
    /// - [`Surface::Silent`] (`--progress none`, which `--quiet` selects)
    ///   returns early below and emits nothing — no progress and no terminal
    ///   line. A quiet run is watched with `autoindex status` or its exit code,
    ///   never by waiting on this stream. (A fatal `error:` line is printed by
    ///   the caller and is unaffected.)
    /// - A run killed by a signal, or aborted by a panic (the release profile
    ///   is `panic = "abort"`, so unwinding never reaches `Drop`), cannot print
    ///   one — which is itself the signal that the process died abnormally.
    pub fn finish(&self, ok: bool, exit: i32, reason: &str, extra: &[(&str, u64)]) {
        if self.reported.swap(true, Ordering::SeqCst) || !self.enabled() {
            return;
        }
        let wall = self.started.elapsed().as_secs_f64();
        match self.surface {
            Surface::Json => {
                let mut doc = serde_json::Map::new();
                doc.insert("event".into(), "done".into());
                doc.insert("ok".into(), ok.into());
                doc.insert("exit".into(), exit.into());
                doc.insert("reason".into(), reason.into());
                doc.insert("wall_s".into(), serde_json::json!(round1(wall)));
                for (key, value) in extra {
                    doc.insert((*key).to_string(), (*value).into());
                }
                let line = serde_json::Value::Object(doc);
                self.sink.write(format!("{line}\n").as_bytes());
            }
            _ => {
                let mut line = String::new();
                if self.surface == Surface::Tty {
                    let mut state = self.state.lock().unwrap();
                    line.push_str(&clear_tty(state.tty_width));
                    state.tty_width = 0;
                }
                line.push_str(&format!(
                    "xerj-done ok={ok} exit={exit} reason={reason} wall={wall:.1}s"
                ));
                for (key, value) in extra {
                    line.push_str(&format!(" {key}={value}"));
                }
                line.push('\n');
                self.sink.write(line.as_bytes());
            }
        }
    }

    /// Terminal line for a run that returned an error before `finish` — so an
    /// agent never has to infer an outcome from silence on the failure path
    /// either.
    fn finish_aborted(&self) {
        self.finish(false, 1, "aborted", &[]);
    }

    fn snapshot(&self) -> Snapshot {
        let now = Instant::now();
        let items_done = self.items_done.load(Ordering::Relaxed);
        let bytes_done = self.bytes_done.load(Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();

        let basis = if state.bytes_total > 0 {
            Basis::Bytes
        } else if state.items_total > 0 {
            Basis::Items
        } else {
            Basis::None
        };
        let (done, total) = match basis {
            Basis::Bytes => (bytes_done.min(state.bytes_total), state.bytes_total),
            Basis::Items => (items_done.min(state.items_total), state.items_total),
            Basis::None => (0, 0),
        };

        // Throughput sample → EWMA. Guarded on dt so a burst of emits cannot
        // divide by a near-zero interval and produce an absurd rate.
        //
        // A sample where NOTHING completed is deliberately not fed in. Bytes
        // are credited when a file finishes, so a worker grinding through one
        // 20 MB file looks like zero throughput from out here — folding those
        // zeroes into the average drags the rate toward 0 and the ETA toward
        // infinity, which is precisely the moment a user is staring at the
        // screen. Nothing completed is an absence of measurement, not a
        // measurement of zero.
        let dt = now.duration_since(state.last_sample_at).as_secs_f64();
        let advanced = done > state.last_sample_units;
        if total > 0 && dt >= 0.05 && advanced {
            let instant = (done - state.last_sample_units) as f64 / dt;
            state.rate = Some(match state.rate {
                Some(previous) => EWMA_ALPHA * instant + (1.0 - EWMA_ALPHA) * previous,
                None => instant,
            });
            state.last_sample_at = now;
            state.last_sample_units = done;
            state.last_advance_at = now;
        }

        let phase_elapsed = now.duration_since(state.phase_started).as_secs_f64();
        let since_progress = now.duration_since(state.last_advance_at).as_secs_f64();
        let percent = (total > 0).then(|| (done as f64 / total as f64) * 100.0);
        let fraction = if total > 0 {
            done as f64 / total as f64
        } else {
            0.0
        };
        let eta = if total == 0 || phase_elapsed < ETA_MIN_PHASE_SECS || fraction < ETA_MIN_FRACTION
        {
            Eta::Unknown
        } else if since_progress > self.stall_threshold().as_secs_f64() {
            // The last measurement is too old to extrapolate from. The named
            // straggler below is the real answer to "what is it doing?" — an
            // extrapolated number here would be pure invention.
            Eta::Stalled
        } else {
            match state.rate {
                Some(rate) if rate > 0.0 => {
                    let raw = (total - done) as f64 / rate;
                    // Clamp the *displayed* movement. Instantaneous throughput
                    // swung 34.8x within one measured run; an unclamped ETA
                    // jumps around so much it reads as noise.
                    let shown = match state.shown_eta {
                        Some(previous) if previous > 0.001 => raw.clamp(
                            previous * (1.0 - ETA_MAX_STEP),
                            previous * (1.0 + ETA_MAX_STEP),
                        ),
                        _ => raw,
                    };
                    state.shown_eta = Some(shown);
                    if fraction < ETA_GOOD_FRACTION {
                        Eta::Rough(shown)
                    } else {
                        Eta::Good(shown)
                    }
                }
                _ => Eta::Unknown,
            }
        };

        // Name the tail. Once the queue is nearly drained the run looks hung
        // while one worker chews through the biggest file in the corpus.
        let mut waiting_on = Vec::new();
        if !state.in_flight.is_empty() && state.in_flight.len() <= STRAGGLER_MAX {
            waiting_on = state
                .in_flight
                .iter()
                .map(|(_, rel, bytes)| (rel.clone(), *bytes))
                .collect();
        }

        Snapshot {
            phase: state.phase,
            basis,
            items_done,
            items_total: state.items_total,
            bytes_done,
            bytes_total: state.bytes_total,
            percent,
            rate: state.rate,
            eta,
            elapsed: now.duration_since(self.started).as_secs_f64(),
            phase_elapsed,
            since_progress,
            waiting_on,
        }
    }

    /// How long nothing may complete before the ETA is withdrawn. Three ticks,
    /// and never less than 15 s so a slow-but-honest phase is not flagged by a
    /// short `--progress-interval`.
    fn stall_threshold(&self) -> Duration {
        (self.interval * 3).max(Duration::from_secs(15))
    }

    fn emit(&self) {
        if !self.enabled() {
            return;
        }
        let snapshot = self.snapshot();
        match self.surface {
            Surface::Silent => {}
            Surface::Json => self
                .sink
                .write(format!("{}\n", render_json(&snapshot)).as_bytes()),
            Surface::Plain => self
                .sink
                .write(format!("{}\n", render_plain(&snapshot)).as_bytes()),
            Surface::Tty => {
                let body = render_tty(&snapshot);
                let width = columns();
                let body = truncate(&body, width);
                let mut state = self.state.lock().unwrap();
                let pad = state.tty_width.saturating_sub(body.chars().count());
                state.tty_width = body.chars().count();
                drop(state);
                let line = format!("\r{body}{}\r", " ".repeat(pad));
                self.sink.write(line.as_bytes());
            }
        }
    }

    /// Start the ticker. Nothing is spawned for a silent surface.
    pub fn spawn_ticker(self: &Arc<Self>) -> Ticker {
        if !self.enabled() {
            return Ticker {
                progress: Arc::clone(self),
                handle: None,
            };
        }
        let progress = Arc::clone(self);
        let interval = self.interval;
        let handle = std::thread::Builder::new()
            .name("autoindex-progress".into())
            .spawn(move || {
                // Hold the guard across the loop and test the flag BEFORE
                // waiting. A condvar notification reaches only threads already
                // parked in `wait`, so a `stop()` that lands while this thread
                // is anywhere else — above all before its very first wait — is
                // delivered to nobody and is gone. Re-checking the predicate
                // under the lock is what makes it impossible to miss: the flag
                // is set under the same mutex, so either we see it here or we
                // are already parked and the notify wakes us.
                //
                // Getting this wrong is not a latency detail. `Ticker::drop`
                // joins this thread, so a lost stop turns every early exit —
                // `es.ping()` refused, `no-files`, a small `--dry-run` — into a
                // full `--progress-interval` of silence AFTER the terminal line
                // has printed, and `--progress-interval 3600` makes that an
                // hour.
                let mut stopped = progress.stopped.lock().unwrap();
                while !*stopped {
                    let (guard, _timeout) = progress
                        .wake
                        .wait_timeout(stopped, interval)
                        .expect("progress ticker mutex poisoned");
                    stopped = guard;
                    if *stopped {
                        break;
                    }
                    // Never emit while holding `stopped`: `stop()` would block
                    // behind a write to a slow pipe.
                    drop(stopped);
                    progress.emit();
                    stopped = progress.stopped.lock().unwrap();
                }
            })
            .ok();
        Ticker {
            progress: Arc::clone(self),
            handle,
        }
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

/// Counts its file done on drop, on every path.
pub struct FileGuard<'a> {
    progress: &'a Progress,
    seq: u64,
    bytes: u64,
}

impl Drop for FileGuard<'_> {
    fn drop(&mut self) {
        self.progress.file_finished(self.seq, self.bytes);
    }
}

/// Owns the ticker thread. Dropping it stops the thread and — if the run never
/// called `finish` — closes the stream with an honest `ok=false` line.
pub struct Ticker {
    progress: Arc<Progress>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.progress.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.progress.finish_aborted();
    }
}

fn clear_tty(width: usize) -> String {
    if width == 0 {
        String::new()
    } else {
        format!("\r{}\r", " ".repeat(width))
    }
}

fn columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 20)
        .unwrap_or(DEFAULT_COLUMNS)
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars().take(width.saturating_sub(1)).collect()
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn fmt_bytes(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    const KB: u64 = 1 << 10;
    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes}B")
    }
}

fn fmt_secs(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 60.0 {
        format!("{seconds:.0}s")
    } else if seconds < 3600.0 {
        format!(
            "{}m{:02}s",
            (seconds / 60.0) as u64,
            (seconds % 60.0) as u64
        )
    } else {
        format!(
            "{}h{:02}m",
            (seconds / 3600.0) as u64,
            ((seconds % 3600.0) / 60.0) as u64
        )
    }
}

fn render_tty(snapshot: &Snapshot) -> String {
    let mut line = format!("{} ", snapshot.phase);
    match snapshot.percent {
        Some(percent) => line.push_str(&format!("{percent:5.1}% ")),
        None => line.push_str("  ..%  "),
    }
    if snapshot.items_total > 0 {
        line.push_str(&format!(
            "| {}/{} items ",
            snapshot.items_done, snapshot.items_total
        ));
    } else if snapshot.items_done > 0 {
        line.push_str(&format!("| {} items ", snapshot.items_done));
    }
    if snapshot.bytes_total > 0 {
        line.push_str(&format!(
            "| {}/{} ",
            fmt_bytes(snapshot.bytes_done),
            fmt_bytes(snapshot.bytes_total)
        ));
    }
    if let Some(rate) = snapshot.rate {
        match snapshot.basis {
            Basis::Bytes => line.push_str(&format!("| {}/s ", fmt_bytes(rate as u64))),
            Basis::Items => line.push_str(&format!("| {rate:.1} items/s ")),
            Basis::None => {}
        }
    }
    match snapshot.eta.seconds() {
        Some(seconds) => line.push_str(&format!("| eta {}", fmt_secs(seconds))),
        None => line.push_str("| eta unknown"),
    }
    if snapshot.waiting_on.len() == 1 {
        line.push_str(&format!(
            " | waiting on {}",
            straggler(&snapshot.waiting_on[0])
        ));
    } else if snapshot.waiting_on.len() > 1 {
        line.push_str(&format!(
            " | waiting on {} items",
            snapshot.waiting_on.len()
        ));
    }
    line
}

/// `path(22.4MB)`, or just `path` for an item with no meaningful byte size
/// (a catalog delete, a correlation query) — `(0B)` would read as a defect.
fn straggler((name, bytes): &(String, u64)) -> String {
    if *bytes > 0 {
        format!("{name}({})", fmt_bytes(*bytes))
    } else {
        name.clone()
    }
}

fn render_plain(snapshot: &Snapshot) -> String {
    let percent = match snapshot.percent {
        Some(percent) => format!("{percent:.1}"),
        None => "unknown".to_string(),
    };
    let rate = match snapshot.rate {
        Some(rate) => format!("{rate:.1}"),
        None => "unknown".to_string(),
    };
    let eta = match snapshot.eta.seconds() {
        Some(seconds) => format!("{seconds:.1}"),
        None => "unknown".to_string(),
    };
    let mut line = format!(
        "xerj-progress phase={} basis={} pct={} items={}/{} bytes={}/{} rate={} eta_s={} \
         eta_quality={} since_progress_s={:.1} phase_elapsed_s={:.1} elapsed_s={:.1}",
        snapshot.phase,
        snapshot.basis.as_str(),
        percent,
        snapshot.items_done,
        snapshot.items_total,
        snapshot.bytes_done,
        snapshot.bytes_total,
        rate,
        eta,
        snapshot.eta.quality(),
        snapshot.since_progress,
        snapshot.phase_elapsed,
        snapshot.elapsed,
    );
    if !snapshot.waiting_on.is_empty() {
        let names: Vec<String> = snapshot.waiting_on.iter().map(straggler).collect();
        line.push_str(&format!(" waiting_on={}", names.join(",")));
    }
    line
}

fn render_json(snapshot: &Snapshot) -> String {
    let waiting: Vec<serde_json::Value> = snapshot
        .waiting_on
        .iter()
        .map(|(rel, bytes)| serde_json::json!({"path": rel, "bytes": bytes}))
        .collect();
    serde_json::json!({
        "event": "progress",
        "phase": snapshot.phase,
        "basis": snapshot.basis.as_str(),
        "pct": snapshot.percent.map(round1),
        "items_done": snapshot.items_done,
        "items_total": snapshot.items_total,
        "bytes_done": snapshot.bytes_done,
        "bytes_total": snapshot.bytes_total,
        "rate": snapshot.rate.map(round1),
        "eta_s": snapshot.eta.seconds().map(round1),
        "eta_quality": snapshot.eta.quality(),
        "since_progress_s": round1(snapshot.since_progress),
        "phase_elapsed_s": round1(snapshot.phase_elapsed),
        "elapsed_s": round1(snapshot.elapsed),
        "waiting_on": waiting,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn auto_is_a_terminal_surface_only_on_a_real_terminal() {
        assert_eq!(
            resolve(ProgressMode::Auto, true, Some("xterm"), false),
            Surface::Tty
        );
        // The three ways `auto` must degrade to parseable lines.
        assert_eq!(
            resolve(ProgressMode::Auto, false, Some("xterm"), false),
            Surface::Plain
        );
        assert_eq!(
            resolve(ProgressMode::Auto, true, Some("dumb"), false),
            Surface::Plain
        );
        assert_eq!(
            resolve(ProgressMode::Auto, true, Some("xterm"), true),
            Surface::Plain
        );
        // Explicit modes ignore the environment entirely.
        assert_eq!(
            resolve(ProgressMode::Json, true, Some("xterm"), false),
            Surface::Json
        );
        assert_eq!(
            resolve(ProgressMode::None, true, Some("xterm"), false),
            Surface::Silent
        );
    }

    #[test]
    fn unknown_progress_mode_is_rejected_not_defaulted() {
        assert!(ProgressMode::parse("tty").is_err());
        assert!(ProgressMode::parse("").is_err());
        assert_eq!(ProgressMode::parse("json").unwrap(), ProgressMode::Json);
    }

    /// The defect in #241: a blocked phase emitted nothing. Liveness must come
    /// from the clock, not from item completions — a worker can sit inside one
    /// 22 MB file for many seconds while every counter stands still.
    ///
    /// Deliberately written as "wait until it happens, fail on a deadline"
    /// rather than "sleep N and count": a rate assertion measures the machine's
    /// scheduler, not this code. An earlier `sleep(400ms)` + `ticks >= 4` form
    /// of this test failed once at load average 357 with 2 lines — where the
    /// ticker had in fact fired, just twice instead of ten times. The property
    /// under test is *repeated emission with nothing completing*, so three
    /// lines (one from `phase`, two from independent ticker wakeups) prove it
    /// and no upper bound on the machine's slowness can make that false.
    #[test]
    fn ticker_keeps_emitting_while_no_item_completes() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_millis(40));
        let ticker = progress.spawn_ticker();
        progress.phase("finalize", 135, 0);

        let deadline = Instant::now() + Duration::from_secs(20);
        let text = loop {
            let text = captured(&buffer);
            let ticks = text
                .lines()
                .filter(|line| line.starts_with("xerj-progress phase=finalize"))
                .count();
            if ticks >= 3 {
                break text;
            }
            assert!(
                Instant::now() < deadline,
                "a phase that completes no items must still prove liveness; \
                 only {ticks} line(s) in 20s:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(ticker);
        assert!(text.contains("items=0/135"), "{text}");
    }

    #[test]
    fn percent_is_bytes_based_when_bytes_are_known() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        progress.phase("index", 4, 1000);
        // Three of four files done, but only 10% of the bytes: a file-count
        // percent would claim 75% here. The measured corpus had 40.4% of all
        // bytes in ONE file, which is exactly this shape.
        for _ in 0..3 {
            progress.item_done(33);
        }
        progress.tick();
        let text = captured(&buffer);
        let last = text.lines().last().unwrap();
        assert!(last.contains("basis=bytes"), "{last}");
        assert!(last.contains("pct=9.9"), "{last}");
        assert!(last.contains("items=3/4"), "{last}");
    }

    #[test]
    fn percent_is_unknown_when_no_denominator_exists() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        progress.phase("walk", 0, 0);
        progress.tick();
        let text = captured(&buffer);
        assert!(text.contains("pct=unknown"), "{text}");
        assert!(text.contains("basis=none"), "{text}");
        assert!(text.contains("eta_s=unknown"), "{text}");
    }

    #[test]
    fn eta_stays_unknown_until_the_quality_gate_is_met() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        progress.phase("index", 100, 1000);
        // Well past 2% of the bytes, but nowhere near 5s into the phase.
        progress.item_done(500);
        progress.tick();
        let text = captured(&buffer);
        assert!(
            text.lines().last().unwrap().contains("eta_s=unknown"),
            "an ETA from a sub-5s sample is not one we can stand behind: {text}"
        );
        assert!(text.contains("eta_quality=unknown"), "{text}");
    }

    #[test]
    fn stragglers_are_named_when_the_tail_goes_quiet() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        progress.phase("index", 3, 30_000_000);
        let guard = progress.file("tantivy/benches/hdfs.json", 23_488_102);
        progress.tick();
        let text = captured(&buffer);
        assert!(
            text.contains("waiting_on=tantivy/benches/hdfs.json(22.4MB)"),
            "{text}"
        );
        drop(guard);
        assert_eq!(progress.items_done.load(Ordering::Relaxed), 1);
        assert_eq!(progress.bytes_done.load(Ordering::Relaxed), 23_488_102);
    }

    #[test]
    fn json_surface_emits_one_object_per_line_with_nulls_for_unknowns() {
        let (progress, buffer) = Progress::capture(Surface::Json, Duration::from_secs(3600));
        progress.phase("walk", 0, 0);
        progress.note("resuming from journal");
        progress.finish(true, 3, "completed-with-junk", &[("files", 1922)]);
        let text = captured(&buffer);
        for line in text.lines() {
            let value: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert!(value.get("event").is_some(), "{line}");
        }
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert!(first["pct"].is_null(), "{text}");
        assert!(first["eta_s"].is_null(), "{text}");
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["event"], "done");
        assert_eq!(last["exit"], 3);
        assert_eq!(last["reason"], "completed-with-junk");
        assert_eq!(last["files"], 1922);
    }

    /// Exit 3 is success. The stream must say so in words, because an agent
    /// reading a bare `3` reads failure.
    #[test]
    fn terminal_line_states_the_outcome_in_words() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        progress.finish(
            true,
            3,
            "completed-with-junk",
            &[("files", 12), ("records", 7)],
        );
        let text = captured(&buffer);
        assert!(
            text.starts_with("xerj-done ok=true exit=3 reason=completed-with-junk"),
            "{text}"
        );
        assert!(text.contains("files=12 records=7"), "{text}");
    }

    #[test]
    fn a_run_that_dies_still_closes_the_stream() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_millis(20));
        {
            let _ticker = progress.spawn_ticker();
            progress.phase("index", 10, 100);
        }
        let text = captured(&buffer);
        assert!(
            text.contains("xerj-done ok=false exit=1 reason=aborted"),
            "{text}"
        );
    }

    #[test]
    fn silent_surface_writes_nothing_at_all() {
        let (progress, buffer) = Progress::capture(Surface::Silent, Duration::from_millis(10));
        let ticker = progress.spawn_ticker();
        progress.phase("index", 10, 100);
        progress.note("hello");
        progress.item_done(10);
        progress.tick();
        progress.finish(true, 0, "completed", &[]);
        drop(ticker);
        assert!(captured(&buffer).is_empty());
    }

    #[test]
    fn tty_surface_redraws_in_place_and_never_leaves_a_stale_tail() {
        let (progress, buffer) = Progress::capture(Surface::Tty, Duration::from_secs(3600));
        progress.phase("index", 1000, 1_000_000);
        progress.item_done(500_000);
        progress.tick();
        progress.note("graph: 12 structural edges");
        let text = captured(&buffer);
        assert!(
            text.contains('\r'),
            "a tty surface redraws in place: {text:?}"
        );
        assert!(
            text.ends_with("graph: 12 structural edges\n"),
            "a note must land on its own line: {text:?}"
        );
    }

    /// Caught on a real 185 s run, not in review: with 269 of 270 files done
    /// and one 20 MB file still streaming, bytes stop being credited, so
    /// folding those empty samples into the average sent the ETA from 10 s to
    /// 471 s and climbing while roughly 50 s of work remained. An ETA that
    /// runs away exactly when the user is watching is worse than no ETA, so
    /// the estimate is withdrawn and the straggler named instead.
    #[test]
    fn a_stalled_phase_withdraws_its_eta_instead_of_inventing_one() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(1));
        progress.phase("index", 270, 31_130_222);
        let big = progress.file("bench/hdfs.jsonl", 20_881_790);
        progress.bytes_done.store(10_248_432, Ordering::Relaxed);
        progress.items_done.store(269, Ordering::Relaxed);
        {
            let mut state = progress.state.lock().unwrap();
            state.phase_started = Instant::now() - Duration::from_secs(120);
            state.last_sample_at = Instant::now() - Duration::from_secs(100);
            state.last_advance_at = Instant::now() - Duration::from_secs(100);
            state.last_sample_units = 10_248_432;
            state.rate = Some(2_545_880.0);
            state.shown_eta = Some(10.3);
        }
        progress.tick();
        let line = captured(&buffer).lines().last().unwrap().to_string();
        assert!(
            line.contains("eta_s=unknown") && line.contains("eta_quality=stalled"),
            "no completion in 100s is an absence of measurement, not a slow rate: {line}"
        );
        assert!(line.contains("since_progress_s=100."), "{line}");
        assert!(
            line.contains("waiting_on=bench/hdfs.jsonl(19.9MB)"),
            "name what it is actually waiting on: {line}"
        );
        // ...and the percent stays honest: 99.6% of the FILES are done, but
        // only a third of the bytes are.
        assert!(line.contains("pct=32.9"), "{line}");
        drop(big);
    }

    /// An empty sample must not drag the measured rate down either — the rate
    /// is a statement about the work that actually completed.
    #[test]
    fn empty_samples_do_not_decay_the_measured_rate() {
        let (progress, _buffer) = Progress::capture(Surface::Plain, Duration::from_secs(1));
        progress.phase("index", 10, 1000);
        progress.item_done(500);
        {
            let mut state = progress.state.lock().unwrap();
            state.phase_started = Instant::now() - Duration::from_secs(10);
            state.last_sample_at = Instant::now() - Duration::from_secs(1);
        }
        let first = progress.snapshot().rate.expect("a sample was taken");
        for _ in 0..20 {
            assert_eq!(progress.snapshot().rate, Some(first));
        }
    }

    #[test]
    fn displayed_eta_cannot_jump_more_than_the_clamp_per_tick() {
        let (progress, _buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        {
            let mut state = progress.state.lock().unwrap();
            state.phase = "index";
            state.items_total = 100;
            state.bytes_total = 1000;
            state.phase_started = Instant::now() - Duration::from_secs(30);
            state.last_sample_at = Instant::now() - Duration::from_secs(1);
            state.rate = Some(10.0);
            state.shown_eta = Some(100.0);
        }
        progress.bytes_done.store(500, Ordering::Relaxed);
        let snapshot = progress.snapshot();
        let eta = snapshot.eta.seconds().expect("gate is satisfied");
        assert!(
            (80.0..=120.0).contains(&eta),
            "a displayed ETA may move at most 20% per tick, got {eta}"
        );
    }

    /// Run `f` on its own thread and return how long it took, or `None` if it
    /// had not finished within `limit`. The thread is deliberately **not**
    /// joined on the timeout path: the failure under test is a thread that is
    /// parked for an entire `--progress-interval`, so joining it would make a
    /// failing test hang for that interval instead of reporting in seconds.
    fn time_boxed(limit: Duration, f: impl FnOnce() + Send + 'static) -> Option<Duration> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let start = Instant::now();
            f();
            let _ = tx.send(start.elapsed());
        });
        rx.recv_timeout(limit).ok()
    }

    /// Window 1 — `stop()` before the ticker's first wait.
    ///
    /// A condvar notification reaches only threads already parked in `wait`.
    /// The original loop locked `stopped` and went straight into
    /// `wait_timeout` without testing the flag, so a `stop()` that landed
    /// before the thread parked was delivered to nobody, and `Ticker::drop`'s
    /// `join()` blocked for a full `--progress-interval`. Every early exit hits
    /// this: `es.ping()` refused (lib.rs), the `no-files` return, a small
    /// `--dry-run`.
    ///
    /// The interval here is an hour against a five-second box, so the assertion
    /// cannot pass by being lucky with the scheduler and no machine is slow
    /// enough to make a correct implementation fail it. Fifty attempts because
    /// the defect is a race: pre-fix, the standalone reproduction of this loop
    /// hung 11/12 at a 5 s interval and 4/5 at 30 s, not 12/12.
    #[test]
    fn a_stop_before_the_first_wait_is_not_lost() {
        for attempt in 0..50 {
            let blocked = time_boxed(Duration::from_secs(5), || {
                let (progress, _buffer) =
                    Progress::capture(Surface::Plain, Duration::from_secs(3600));
                let ticker = progress.spawn_ticker();
                drop(ticker);
            });
            assert!(
                blocked.is_some(),
                "attempt {attempt}: drop(ticker) had not returned after 5s at a \
                 3600s interval — the stop() racing the ticker's first wait was \
                 lost, and a real run would sit silent for the whole interval"
            );
        }
    }

    /// Window 2 — `stop()` while the ticker is inside `emit()`.
    ///
    /// Held deterministically open rather than raced for: the capture sink
    /// takes the buffer mutex on every write, so holding that mutex parks the
    /// ticker thread inside `emit()`, provably outside `wait_timeout`. The
    /// wakeup is delivered by `notify_all` directly (the interval is an hour,
    /// so the timeout can never rescue a broken loop), and `stop()` is made to
    /// land while the thread is parked in the sink. A loop that re-enters
    /// `wait_timeout` without re-testing the flag hangs for the hour.
    #[test]
    fn a_stop_that_lands_while_the_ticker_is_emitting_is_not_lost() {
        let (progress, buffer) = Progress::capture(Surface::Plain, Duration::from_secs(3600));
        let ticker = progress.spawn_ticker();

        // Own the sink the ticker writes to, then wake it. Notified repeatedly
        // for 300 ms rather than once, because a notify sent before the thread
        // parks is lost by definition — that is this module's whole subject,
        // and a single early notify would leave the thread parked, where
        // `stop()` wakes it normally and the test proves nothing. After this
        // loop the thread is provably blocked in `emit()`: it took at least one
        // notify while parked, and it cannot return to `wait_timeout` until the
        // sink is released. The notifies stop here, so nothing rescues a loop
        // that re-parks after emitting.
        let held = buffer.lock().unwrap();
        for _ in 0..60 {
            progress.wake.notify_all();
            std::thread::sleep(Duration::from_millis(5));
        }

        // Drop on another thread: stop() takes `stopped` (which the ticker is
        // not holding while it emits) and lands now; join() then blocks.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let start = Instant::now();
            drop(ticker);
            let _ = tx.send(start.elapsed());
        });
        std::thread::sleep(Duration::from_millis(100));

        // Let emit() complete. A loop that re-tests the flag exits here; one
        // that goes straight back into wait_timeout sleeps for the hour.
        drop(held);

        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "drop(ticker) had not returned after 5s at a 3600s interval — a \
             stop() that landed while the ticker was emitting was lost"
        );
    }
}
