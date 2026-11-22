//! The local node: which port it is on, starting it when it is down, and
//! keeping it alive after the terminal that started it closes (#3267).
//!
//! In 52 rc.101 field reports an agent found nothing listening on
//! `localhost:9200`, did not start a node from inside a tool call, and fell
//! back to grep for the whole session. Three causes were seen: a node started
//! from an agent terminal got SIGTERM when that session ended, a Windows node
//! started with `cmd start /B` died with its shell, and a node on :9280 was
//! never found because every client assumed :9200.
//!
//! This module holds the pieces every client shares:
//!
//! * [`write_launch_record`]: the server records, in its data dir, the port
//!   it bound and the flags it was started with (`node.launch.json`).
//! * [`default_url`]: with no `XERJ_URL`, the port comes from what the local
//!   nodes recorded, not from a constant.
//! * [`start_for_url`]: a loopback client that finds the port closed starts
//!   the node that last served it, detached, and waits until it answers.
//! * [`spawn_detached`]: `setsid` on Unix; `DETACHED_PROCESS |
//!   CREATE_NEW_PROCESS_GROUP` (plus `CREATE_BREAKAWAY_FROM_JOB` when the
//!   job allows it) on Windows. stdin is closed and both output streams go to
//!   a log file, so no terminal, pipe or console is left to take it down.
//!
//! `XERJ_NO_AUTOSTART=1` turns auto-start off. A non-loopback URL is never
//! started.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::nodedir::{self, PidState};

/// Set to `1`/`true`/`yes`/`on` to stop clients from starting a node.
pub const NO_AUTOSTART_ENV: &str = "XERJ_NO_AUTOSTART";
/// Seconds a client waits for a node it started (default 120).
pub const AUTOSTART_TIMEOUT_ENV: &str = "XERJ_AUTOSTART_TIMEOUT_SECS";
/// Set by a launcher that manages its own node (the hub sets `hub`). Recorded
/// so port discovery does not hand a user's client the hub's private node.
pub const NODE_ROLE_ENV: &str = "XERJ_NODE_ROLE";
/// The launch record, beside `node.lock` in the data dir.
pub const LAUNCH_RECORD_FILE: &str = "node.launch.json";
/// Where a detached node's stdout and stderr go, in its data dir.
pub const NODE_LOG_FILE: &str = "node.log";
/// The product default port.
pub const DEFAULT_PORT: u16 = 9200;

const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(120);
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `true` when `XERJ_NO_AUTOSTART` asks clients not to start a node.
pub fn autostart_disabled() -> bool {
    env_flag_set(std::env::var(NO_AUTOSTART_ENV).ok().as_deref())
}

fn env_flag_set(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// How long to wait for a started node: `XERJ_AUTOSTART_TIMEOUT_SECS`, else
/// 120 s. Opening a large data dir is the slow part.
pub fn start_timeout() -> Duration {
    std::env::var(AUTOSTART_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_START_TIMEOUT)
}

// ─────────────────────────────── Launch record ──────────────────────────────

/// What a node recorded about its own start, so a client can start the same
/// node again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    /// The ES-compatible port the node bound (after `--auto-port`, if any).
    pub port: u16,
    /// The server flags after the program name, without `--detach`.
    pub args: Vec<String>,
    /// The working directory, for a relative `--config` path.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub pid: u32,
    /// [`NODE_ROLE_ENV`] at start, when set.
    #[serde(default)]
    pub role: Option<String>,
}

/// Record this process's start in `data_dir`. Written atomically (temp file
/// plus rename), so a reader never sees half a record.
pub fn write_launch_record(data_dir: &Path, port: u16) -> std::io::Result<()> {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--detach")
        .collect();
    let record = LaunchRecord {
        port,
        args,
        cwd: std::env::current_dir().ok(),
        pid: std::process::id(),
        role: std::env::var(NODE_ROLE_ENV).ok().filter(|r| !r.is_empty()),
    };
    write_launch_record_value(data_dir, &record)
}

fn write_launch_record_value(data_dir: &Path, record: &LaunchRecord) -> std::io::Result<()> {
    let body = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    let dest = data_dir.join(LAUNCH_RECORD_FILE);
    let tmp = data_dir.join(format!("{LAUNCH_RECORD_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The record in `data_dir`, if it is there and well formed. A record whose
/// first argument is not a flag is refused: it would start a subcommand, not
/// a node.
pub fn read_launch_record(data_dir: &Path) -> Option<LaunchRecord> {
    let text = std::fs::read_to_string(data_dir.join(LAUNCH_RECORD_FILE)).ok()?;
    let record: LaunchRecord = serde_json::from_str(&text).ok()?;
    if record
        .args
        .first()
        .is_some_and(|first| !first.starts_with('-'))
    {
        return None;
    }
    Some(record)
}

/// The flags that start `data_dir`'s node again on `port`.
///
/// With a record: its flags, with `--data-dir` and `--port` replaced by the
/// absolute data dir and the port the client is waiting on. Without one (a
/// node from before this record existed): `--data-dir` and `--port`, plus
/// `--insecure` when the dir has no `admin.key` (a node with auth on always
/// writes one there).
pub fn restart_args(record: Option<&LaunchRecord>, data_dir: &Path, port: u16) -> Vec<String> {
    let mut out = Vec::new();
    match record {
        Some(record) => {
            let mut it = record.args.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--data-dir" | "-d" | "--port" => {
                        it.next();
                    }
                    "--detach" => {}
                    _ => out.push(arg.clone()),
                }
            }
        }
        None => {
            if !data_dir.join("admin.key").is_file() {
                out.push("--insecure".to_string());
            }
        }
    }
    out.push("--data-dir".to_string());
    out.push(data_dir.display().to_string());
    out.push("--port".to_string());
    out.push(port.to_string());
    out
}

// ─────────────────────────────── Port discovery ─────────────────────────────

/// The URL a client uses when neither `--url` nor `XERJ_URL` names one.
///
/// `http://localhost:9200` when something answers there. Otherwise the port
/// of a live node the local machine recorded (newest first), and failing
/// that the port of the node that started last, so an auto-start brings it
/// back where its clients expect it. A node started by the hub for its own
/// packs is skipped, and a stopped node whose data dir is under the system
/// temp dir (a test or scratch node) is never the one brought back.
pub fn default_url() -> String {
    format!("http://localhost:{}", default_port())
}

/// The port part of [`default_url`].
pub fn default_port() -> u16 {
    let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    match crate::home::home_dir() {
        Some(home) => default_port_in(
            &home,
            &|port| port_accepts("localhost", port, PROBE_TIMEOUT),
            &nodedir::pid_state,
            &|dir: &Path| !dir.starts_with(&temp),
        ),
        None => DEFAULT_PORT,
    }
}

fn default_port_in(
    home: &Path,
    accepts: &dyn Fn(u16) -> bool,
    probe: &dyn Fn(u32) -> PidState,
    restartable: &dyn Fn(&Path) -> bool,
) -> u16 {
    if accepts(DEFAULT_PORT) {
        return DEFAULT_PORT;
    }
    let candidates: Vec<_> = nodedir::port_pointers_in(home)
        .into_iter()
        .filter(|p| p.data_dir.is_dir())
        .filter(|p| {
            read_launch_record(&p.data_dir).is_none_or(|r| r.role.as_deref() != Some("hub"))
        })
        .collect();
    let live = candidates.iter().find(|p| {
        p.port != DEFAULT_PORT
            && nodedir::lock_pid(&p.data_dir).is_some_and(|pid| probe(pid) == PidState::Alive)
            && accepts(p.port)
    });
    if let Some(p) = live {
        return p.port;
    }
    candidates
        .iter()
        .find(|p| p.data_dir.join(LAUNCH_RECORD_FILE).is_file() && restartable(&p.data_dir))
        .map(|p| p.port)
        .unwrap_or(DEFAULT_PORT)
}

/// `true` when a TCP connection to `host:port` is accepted within `timeout`.
pub fn port_accepts(host: &str, port: u16, timeout: Duration) -> bool {
    resolve(host, port)
        .iter()
        .any(|addr| TcpStream::connect_timeout(addr, timeout).is_ok())
}

fn resolve(host: &str, port: u16) -> Vec<SocketAddr> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (host, port)
        .to_socket_addrs()
        .map(|addrs| addrs.collect())
        .unwrap_or_default()
}

/// The host part of a node URL, for probing. `localhost` when it cannot be read.
pub fn url_host(url: &str) -> String {
    let parsed = match url::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => Some(u),
        _ => url::Url::parse(&format!("http://{url}")).ok(),
    };
    parsed
        .and_then(|u| u.host_str().map(str::to_string))
        .filter(|h| !h.is_empty() && h != "0.0.0.0")
        .unwrap_or_else(|| "localhost".to_string())
}

fn url_is_https(url: &str) -> bool {
    url.trim_start()
        .to_ascii_lowercase()
        .starts_with("https://")
}

/// `true` when a node is accepting connections at `url`.
pub fn url_accepts(url: &str) -> bool {
    port_accepts(
        &url_host(url),
        crate::localauth::xerj_url_port(url),
        PROBE_TIMEOUT,
    )
}

/// Whether the node at `url` is serving (see [`node_serving`]).
pub fn url_serving(url: &str) -> bool {
    node_serving(
        &url_host(url),
        crate::localauth::xerj_url_port(url),
        url_is_https(url),
    )
}

/// Whether the node at `host:port` is serving: an HTTP status line other than
/// 503 (the "starting" answer while it opens its indices). A 401 counts: the
/// node is up, it only wants a key. Over TLS the plain probe cannot speak, so
/// an accepted connection is the answer there.
fn node_serving(host: &str, port: u16, tls: bool) -> bool {
    for addr in resolve(host, port) {
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
            continue;
        };
        if tls {
            return true;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let request = format!(
            "GET /_cluster/health HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
        );
        if stream.write_all(request.as_bytes()).is_err() {
            continue;
        }
        let mut head = [0u8; 12];
        let mut got = 0;
        while got < head.len() {
            match stream.read(&mut head[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        let head = String::from_utf8_lossy(&head[..got]);
        return head.starts_with("HTTP/") && head.split_whitespace().nth(1) != Some("503");
    }
    false
}

// ─────────────────────────────── Starting a node ────────────────────────────

/// Which `xerj` binary starts a node: this one when it is `xerj`, else a
/// `xerj` beside it (the standalone `xerj-mcp` ships next to it), else `None`.
pub fn xerj_executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if exe.file_stem().and_then(|s| s.to_str()) == Some("xerj") {
        return Some(exe);
    }
    let sibling = exe.with_file_name(format!("xerj{}", std::env::consts::EXE_SUFFIX));
    sibling.is_file().then_some(sibling)
}

/// Spawn `exe args` detached from this process, its terminal and its job:
/// stdin closed, stdout and stderr appended to `log`.
///
/// Unix: `setsid()` in the child, so it has no controlling terminal and is in
/// no process group a shell or agent harness signals when its session ends.
/// Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, and
/// `CREATE_BREAKAWAY_FROM_JOB` first, so a console or job object closing does
/// not take the node with it; a job that forbids breakaway refuses that flag,
/// and the spawn is retried without it.
pub fn spawn_detached(
    exe: &Path,
    args: &[String],
    cwd: &Path,
    log: &Path,
) -> std::io::Result<Child> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let open_log = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
    };
    spawn_detached_with(|| {
        let out = open_log()?;
        let err = out.try_clone()?;
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        Ok(cmd)
    })
}

/// Spawn the command `make` builds, detached the same way as
/// [`spawn_detached`], for a caller that sets up its own arguments, stdio and
/// environment (the hub's node and background pulls, `xerj brain`'s server).
///
/// `make` may be called twice: on Windows a job that forbids breakaway
/// refuses the first spawn, and the retry needs fresh stdio handles. The
/// caller is responsible for not leaving stdin attached to a terminal
/// (`Stdio::null()`) and for sending output somewhere that outlives it.
pub fn spawn_detached_with(
    mut make: impl FnMut() -> std::io::Result<Command>,
) -> std::io::Result<Child> {
    let mut cmd = make()?;
    detach(&mut cmd, true);
    #[cfg(windows)]
    {
        match cmd.spawn() {
            Ok(child) => Ok(child),
            Err(_) => {
                let mut cmd = make()?;
                detach(&mut cmd, false);
                cmd.spawn()
            }
        }
    }
    #[cfg(not(windows))]
    {
        cmd.spawn()
    }
}

/// Windows creation flags for a detached node: no console, its own process
/// group (so a console Ctrl-C/Ctrl-Break does not reach it), and, when
/// `breakaway`, out of the parent's job object.
pub const fn windows_detach_flags(breakaway: bool) -> u32 {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    if breakaway {
        base | CREATE_BREAKAWAY_FROM_JOB
    } else {
        base
    }
}

/// Mark `cmd` to start detached from this process's terminal and session.
///
/// Unix: `setsid()` between fork and exec. The child leads a new session and
/// a new process group (sid = pgid = its pid) and has no controlling
/// terminal, so the SIGHUP a closing terminal sends, a shell's `kill -- -pgid`
/// of its job, and a Ctrl-C or Ctrl-Z in the parent's terminal all miss it.
/// `process_group(0)` alone is not enough: the child would still be in the
/// terminal's session and get SIGHUP+SIGCONT when its group is orphaned.
///
/// Windows: [`windows_detach_flags`].
#[allow(unused_variables)]
pub fn detach(cmd: &mut Command, breakaway: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_detach_flags(breakaway));
    }
}

/// A node a client started (or found starting) and waited for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    /// The PID that was spawned, or the one already starting.
    pub pid: u32,
    pub data_dir: PathBuf,
    pub log: PathBuf,
    pub port: u16,
    /// `false` when a node was already starting and this call only waited.
    pub spawned: bool,
    pub waited: Duration,
}

impl Started {
    /// One sentence for the agent or the terminal.
    pub fn sentence(&self) -> String {
        let what = if self.spawned {
            "started the local XERJ node"
        } else {
            "waited for the local XERJ node to finish starting"
        };
        format!(
            "No node was answering on port {port}, so xerj {what} (pid {pid}, data dir {dir}, \
             log {log}) in {secs:.1}s. It runs detached and keeps running after this session. \
             Set {NO_AUTOSTART_ENV}=1 to turn this off.",
            port = self.port,
            pid = self.pid,
            dir = self.data_dir.display(),
            log = self.log.display(),
            secs = self.waited.as_secs_f64(),
        )
    }
}

/// Start the node that last served `url`'s port, detached, and wait until it
/// serves. `exe` is the `xerj` binary ([`xerj_executable`]).
///
/// Refuses a non-loopback URL, a port with no recorded data dir, and a data
/// dir that is gone; the error says what to run instead. When the recorded
/// node is still alive (opening its indices), nothing is spawned and the
/// call waits for it.
pub fn start_for_url(url: &str, exe: &Path) -> Result<Started, String> {
    let home = crate::home::home_dir()
        .ok_or_else(|| "no home directory, so no recorded data dir".to_string())?;
    start_for_url_in(&home, url, exe, start_timeout())
}

fn start_for_url_in(
    home: &Path,
    url: &str,
    exe: &Path,
    timeout: Duration,
) -> Result<Started, String> {
    if !crate::localauth::url_is_loopback(url) {
        return Err(format!(
            "{url} is not on this machine; xerj only starts a local node"
        ));
    }
    let port = crate::localauth::xerj_url_port(url);
    let data_dir = nodedir::published_data_dir_in(home, port).ok_or_else(|| {
        format!(
            "no node has run on port {port} on this machine yet, so there is no data dir to \
             start. Start one with: xerj --detach --insecure --data-dir ~/xerj-data{}",
            if port == DEFAULT_PORT {
                String::new()
            } else {
                format!(" --port {port}")
            }
        )
    })?;
    if !data_dir.is_dir() {
        return Err(format!(
            "the data dir that last served port {port} ({}) no longer exists",
            data_dir.display()
        ));
    }
    let host = url_host(url);
    let tls = url_is_https(url);
    let log = data_dir.join(NODE_LOG_FILE);
    let started_at = Instant::now();

    // A live holder of node.lock is a node still opening its indices (or one
    // listening elsewhere). A second node on the same dir would only fail on
    // the lock, so wait for this one instead. A holder that is shutting down
    // (it closed its port after SIGTERM and is flushing) exits without ever
    // answering; when it is gone, start the node here instead of waiting out
    // the whole timeout (#3267: this blocked a session for 120 s).
    if let Some(pid) =
        nodedir::lock_pid(&data_dir).filter(|pid| nodedir::pid_state(*pid) == PidState::Alive)
    {
        let waited = wait_serving(
            &host,
            port,
            tls,
            None,
            Some(pid),
            &data_dir,
            &log,
            started_at + timeout,
        )?;
        if waited == Waited::Serving {
            return Ok(Started {
                pid,
                data_dir,
                log,
                port,
                spawned: false,
                waited: started_at.elapsed(),
            });
        }
    }

    let record = read_launch_record(&data_dir);
    let args = restart_args(record.as_ref(), &data_dir, port);
    let cwd = record
        .as_ref()
        .and_then(|r| r.cwd.clone())
        .filter(|cwd| cwd.is_dir())
        .unwrap_or_else(|| data_dir.clone());
    let mut child = spawn_detached(exe, &args, &cwd, &log)
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    let pid = child.id();
    let waited = wait_serving(
        &host,
        port,
        tls,
        Some(&mut child),
        None,
        &data_dir,
        &log,
        started_at + timeout,
    );
    reap_in_background(child);
    if waited? == Waited::HolderGone {
        return Err(format!(
            "the node that started at the same time exited before it answered on port \
             {port}. Last lines of {}:\n{}",
            log.display(),
            log_tail(&log, 20)
        ));
    }
    Ok(Started {
        pid,
        data_dir,
        log,
        port,
        spawned: true,
        waited: started_at.elapsed(),
    })
}

/// Collect the child's exit status when it ends, so a node that dies while
/// this (long-lived) client runs is not left a zombie that `kill(pid, 0)`
/// still reports alive.
fn reap_in_background(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("xerj-node-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// How [`wait_serving`] ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waited {
    /// The node answers.
    Serving,
    /// The process being waited for (not a child of ours) exited without
    /// answering, and nothing else is starting.
    HolderGone,
}

/// `true` when `pid` is stopped (SIGSTOP, or Ctrl-Z in its terminal): it
/// holds the lock and may hold the port, but answers nothing until resumed.
#[cfg(target_os = "linux")]
fn pid_stopped(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| {
            let rest = text.get(text.rfind(')')? + 2..)?;
            rest.chars().next()
        })
        .is_some_and(|state| state == 'T')
}

#[cfg(not(target_os = "linux"))]
fn pid_stopped(_pid: u32) -> bool {
    false
}

/// Poll until the node serves, the child dies, the holder dies, or the
/// deadline passes.
///
/// `child` is a node this call spawned; `holder` is a live process already
/// holding the data dir's lock. A child that exits is not the end when
/// another live process holds the lock: two clients raced, and the other
/// one's node is the one that will answer, so it becomes the holder. A
/// holder that exits without answering ends the wait with
/// [`Waited::HolderGone`]; a stopped holder ends it with an error that says
/// how to resume it.
#[allow(clippy::too_many_arguments)]
fn wait_serving(
    host: &str,
    port: u16,
    tls: bool,
    mut child: Option<&mut Child>,
    mut holder: Option<u32>,
    data_dir: &Path,
    log: &Path,
    deadline: Instant,
) -> Result<Waited, String> {
    loop {
        if node_serving(host, port, tls) {
            return Ok(Waited::Serving);
        }
        if let Some(c) = child.as_deref_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                let other = nodedir::lock_pid(data_dir)
                    .filter(|pid| *pid != c.id() && nodedir::pid_state(*pid) == PidState::Alive);
                let Some(other) = other else {
                    return Err(format!(
                        "the node exited ({status}) before it answered on port {port}. \
                         Last lines of {}:\n{}",
                        log.display(),
                        log_tail(log, 20)
                    ));
                };
                child = None;
                holder = Some(other);
            }
        }
        if let Some(pid) = holder {
            if nodedir::pid_state(pid) == PidState::Dead {
                return Ok(Waited::HolderGone);
            }
            if pid_stopped(pid) {
                return Err(format!(
                    "the node on this data dir (pid {pid}) is stopped (SIGSTOP or Ctrl-Z in its \
                     terminal), so it answers nothing. Resume it with: kill -CONT {pid}"
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the node did not answer on port {port} within the wait; it may still be \
                 opening a large data dir. Follow {} and retry.",
                log.display()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The last `lines` lines of `path`, for an error message.
pub fn log_tail(path: &Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A loopback client's whole decision: when `url` is loopback, auto-start is
/// on, and nothing accepts at `url`, start the node and return what was
/// started. `Ok(None)` means nothing needed doing (or auto-start does not
/// apply); `Err` carries why a start was attempted and failed.
pub fn ensure_for_url(url: &str) -> Result<Option<Started>, String> {
    if autostart_disabled() || !crate::localauth::url_is_loopback(url) || url_accepts(url) {
        return Ok(None);
    }
    let exe = xerj_executable().ok_or_else(|| {
        "no xerj binary found next to this one, so the node cannot be started from here".to_string()
    })?;
    start_for_url(url, &exe).map(Some)
}

/// [`ensure_for_url`] for a CLI read command: the outcome goes to stderr as
/// `<tool>: …`, and the command carries on (its own "no node" message still
/// applies when the start failed).
pub fn ensure_for_cli(tool: &str, url: &str) {
    match ensure_for_url(url) {
        Ok(Some(started)) => {
            let _ = writeln!(std::io::stderr(), "{tool}: {}", started.sentence());
        }
        Ok(None) => {}
        Err(reason) => {
            let _ = writeln!(
                std::io::stderr(),
                "{tool}: nothing answers at {url}, and the local node could not be started: {reason}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn pointer(home: &Path, port: u16, data_dir: &Path, age_secs: u64) {
        let dir = home.join(".xerj/by-port");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{port}.data-dir"));
        std::fs::write(&path, format!("{}\n", data_dir.display())).unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn record(data_dir: &Path, port: u16, role: Option<&str>) {
        write_launch_record_value(
            data_dir,
            &LaunchRecord {
                port,
                args: vec!["--insecure".into(), "--data-dir".into(), "rel".into()],
                cwd: None,
                pid: 1,
                role: role.map(str::to_string),
            },
        )
        .unwrap();
    }

    #[test]
    fn the_env_flag_reads_the_usual_spellings() {
        for on in ["1", "true", "YES", " on "] {
            assert!(env_flag_set(Some(on)), "{on}");
        }
        for off in ["", "0", "false", "no", "maybe"] {
            assert!(!env_flag_set(Some(off)), "{off}");
        }
        assert!(!env_flag_set(None));
    }

    #[test]
    fn a_launch_record_round_trips_and_a_subcommand_record_is_refused() {
        let data = tempfile::tempdir().unwrap();
        record(data.path(), 9280, None);
        let got = read_launch_record(data.path()).expect("record");
        assert_eq!(got.port, 9280);
        assert_eq!(got.args[0], "--insecure");
        let mut bad = got.clone();
        bad.args.insert(0, "autoindex".into());
        write_launch_record_value(data.path(), &bad).unwrap();
        assert!(read_launch_record(data.path()).is_none());
    }

    #[test]
    fn restart_args_keep_the_flags_and_pin_the_dir_and_port() {
        let data = tempfile::tempdir().unwrap();
        let rec = LaunchRecord {
            port: 9280,
            args: vec![
                "--insecure".into(),
                "-d".into(),
                "./rel".into(),
                "--port".into(),
                "9280".into(),
                "--detach".into(),
                "--embed-mode".into(),
                "lexical".into(),
            ],
            cwd: None,
            pid: 0,
            role: None,
        };
        let args = restart_args(Some(&rec), data.path(), 9280);
        let dir = data.path().display().to_string();
        assert_eq!(
            args,
            vec![
                "--insecure",
                "--embed-mode",
                "lexical",
                "--data-dir",
                dir.as_str(),
                "--port",
                "9280"
            ]
        );
    }

    #[test]
    fn restart_args_without_a_record_follow_the_admin_key() {
        let data = tempfile::tempdir().unwrap();
        let args = restart_args(None, data.path(), 9200);
        assert_eq!(args[0], "--insecure");
        std::fs::write(data.path().join("admin.key"), "k").unwrap();
        let args = restart_args(None, data.path(), 9200);
        assert_eq!(args[0], "--data-dir");
    }

    #[test]
    fn default_port_prefers_a_live_9200() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        record(data.path(), 9280, None);
        pointer(home.path(), 9280, data.path(), 0);
        let port = default_port_in(home.path(), &|p| p == 9200, &|_| PidState::Alive, &ANY);
        assert_eq!(port, 9200);
    }

    const ANY: fn(&Path) -> bool = |_| true;

    /// #3267: a node on :9280 recorded on this machine is found without XERJ_URL.
    #[test]
    fn a_live_node_on_another_port_is_found() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("node.lock"), "4242").unwrap();
        record(data.path(), 9280, None);
        pointer(home.path(), 9280, data.path(), 0);
        let port = default_port_in(
            home.path(),
            &|p| p == 9280,
            &|pid| {
                assert_eq!(pid, 4242);
                PidState::Alive
            },
            &|_| false,
        );
        assert_eq!(port, 9280, "a live node is used even from a scratch dir");
    }

    #[test]
    fn with_nothing_live_the_newest_recorded_node_wins() {
        let home = tempfile::tempdir().unwrap();
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        record(old.path(), 9300, None);
        record(new.path(), 9280, None);
        pointer(home.path(), 9300, old.path(), 600);
        pointer(home.path(), 9280, new.path(), 5);
        assert_eq!(
            default_port_in(home.path(), &|_| false, &|_| PidState::Dead, &ANY),
            9280
        );
        // A stopped scratch node (under the temp dir) is never brought back.
        let new_dir = new.path().to_path_buf();
        assert_eq!(
            default_port_in(home.path(), &|_| false, &|_| PidState::Dead, &|d: &Path| {
                d != new_dir
            }),
            9300
        );
    }

    #[test]
    fn the_hub_node_and_missing_dirs_are_skipped() {
        let home = tempfile::tempdir().unwrap();
        let hub = tempfile::tempdir().unwrap();
        std::fs::write(hub.path().join("node.lock"), "7").unwrap();
        record(hub.path(), 9300, Some("hub"));
        pointer(home.path(), 9300, hub.path(), 0);
        pointer(home.path(), 9400, &home.path().join("gone"), 0);
        assert_eq!(
            default_port_in(home.path(), &|_| true, &|_| PidState::Alive, &ANY),
            9200
        );
        assert_eq!(
            default_port_in(home.path(), &|p| p != 9200, &|_| PidState::Alive, &ANY),
            9200
        );
    }

    #[test]
    fn start_refuses_a_remote_url_and_an_unknown_port() {
        let home = tempfile::tempdir().unwrap();
        let exe = Path::new("/nonexistent/xerj");
        let err = start_for_url_in(
            home.path(),
            "http://search.example.com:9200",
            exe,
            POLL_INTERVAL,
        )
        .unwrap_err();
        assert!(err.contains("not on this machine"), "{err}");
        let err =
            start_for_url_in(home.path(), "http://localhost:9281", exe, POLL_INTERVAL).unwrap_err();
        assert!(err.contains("no node has run on port 9281"), "{err}");
        assert!(err.contains("xerj --detach"), "{err}");
        assert!(err.contains("--port 9281"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_dies_reports_its_log() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        pointer(home.path(), 9283, data.path(), 0);
        let script = data.path().join("fake-xerj.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"boot failed: $*\"\nexit 3\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = start_for_url_in(
            home.path(),
            "http://127.0.0.1:9283",
            &script,
            Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(err.contains("exited"), "{err}");
        assert!(err.contains("boot failed: --insecure --data-dir"), "{err}");
        assert!(err.contains("--port 9283"), "{err}");
    }

    /// A fake `xerj` that fails to boot, so a spawn shows up as an "exited"
    /// error carrying its arguments.
    #[cfg(unix)]
    fn failing_xerj(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-xerj.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"boot failed: $*\"\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// #3267 (seen end to end): the node was sent SIGTERM, closed its port,
    /// and spent ~11 s flushing before it exited. An MCP call in that window
    /// found the lock held by a live pid, waited for it, and kept waiting
    /// after it exited: 120 s, then an error. Once the holder is gone the
    /// node is started here.
    #[cfg(unix)]
    #[test]
    fn a_lock_holder_that_exits_without_answering_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        pointer(home.path(), 28483, data.path(), 0);
        let mut holder = Command::new("/bin/sh")
            .args(["-c", "sleep 1"])
            .spawn()
            .unwrap();
        std::fs::write(data.path().join("node.lock"), holder.id().to_string()).unwrap();
        let reaper = std::thread::spawn(move || holder.wait());
        let script = failing_xerj(data.path());
        let started = Instant::now();
        let err = start_for_url_in(
            home.path(),
            "http://127.0.0.1:28483",
            &script,
            Duration::from_secs(60),
        )
        .unwrap_err();
        let took = started.elapsed();
        reaper.join().unwrap().unwrap();
        assert!(err.contains("boot failed: --insecure --data-dir"), "{err}");
        assert!(
            took < Duration::from_secs(30),
            "waited {took:?} for a holder that had exited"
        );
    }

    /// A lock holder stopped with SIGSTOP answers nothing; the wait ends at
    /// once with how to resume it instead of running out the timeout.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_stopped_lock_holder_is_reported_not_waited_out() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        pointer(home.path(), 28484, data.path(), 0);
        let mut holder = Command::new("/bin/sh")
            .args(["-c", "exec sleep 60"])
            .spawn()
            .unwrap();
        let pid = holder.id();
        std::fs::write(data.path().join("node.lock"), pid.to_string()).unwrap();
        // SAFETY: signalling the child this test spawned.
        unsafe {
            libc::kill(pid as i32, libc::SIGSTOP);
        }
        assert!(wait_for(|| pid_stopped(pid)));
        let script = failing_xerj(data.path());
        let started = Instant::now();
        let err = start_for_url_in(
            home.path(),
            "http://127.0.0.1:28484",
            &script,
            Duration::from_secs(60),
        )
        .unwrap_err();
        let took = started.elapsed();
        let _ = holder.kill();
        let _ = holder.wait();
        assert!(err.contains(&format!("kill -CONT {pid}")), "{err}");
        assert!(took < Duration::from_secs(30), "waited {took:?}");
    }

    /// The detached child is in its own session: no controlling terminal and
    /// not in this process's group, so the group signal a shell or harness
    /// sends when it ends does not reach it.
    #[cfg(unix)]
    #[test]
    fn a_detached_child_leads_its_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out.log");
        let mut child = spawn_detached(
            Path::new("/bin/sh"),
            &["-c".into(), "ps -o sid= -p $$; ps -o pgid= -p $$".into()],
            dir.path(),
            &log,
        )
        .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let ids: Vec<u32> = text
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        assert_eq!(
            ids,
            vec![pid, pid],
            "sid and pgid should be the child's pid: {text}"
        );
    }

    #[test]
    fn windows_flags_detach_from_console_group_and_job() {
        assert_eq!(windows_detach_flags(false), 0x0000_0208);
        assert_eq!(windows_detach_flags(true), 0x0100_0208);
    }

    /// `(state, session id, tty_nr)` from `/proc/<pid>/stat`, or `None` once
    /// the process is gone.
    #[cfg(target_os = "linux")]
    fn proc_stat(pid: u32) -> Option<(char, i64, i64)> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &text[text.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        Some((
            f.first()?.chars().next()?,
            f.get(3)?.parse().ok()?,
            f.get(4)?.parse().ok()?,
        ))
    }

    /// A zombie (exited, waiting for a reaper) counts as gone.
    #[cfg(target_os = "linux")]
    fn running(pid: u32) -> bool {
        proc_stat(pid).is_some_and(|(state, _, _)| state != 'Z' && state != 'X')
    }

    #[cfg(target_os = "linux")]
    fn signal_group(pgid: u32, sig: libc::c_int) {
        // SAFETY: a negative pid signals the process group the test created.
        unsafe {
            libc::kill(-(pgid as i32), sig);
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for(what: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        what()
    }

    const HELPER_ENV: &str = "XERJ_TEST_DETACH_HELPER_OUT";

    /// Not a test on its own: when [`HELPER_ENV`] is set it plays the agent
    /// terminal of [`a_detached_child_outlives_its_terminal_session`]. It
    /// starts one child the normal way and one with [`spawn_detached`],
    /// writes both PIDs, and blocks until it is killed.
    #[cfg(target_os = "linux")]
    #[test]
    fn detach_helper_terminal() {
        let Ok(out) = std::env::var(HELPER_ENV) else {
            return;
        };
        let out = PathBuf::from(out);
        let sleep = || vec!["-c".to_string(), "exec sleep 120".to_string()];
        let mut plain = Command::new("/bin/sh")
            .args(sleep())
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let mut detached = spawn_detached(
            Path::new("/bin/sh"),
            &sleep(),
            out.parent().unwrap(),
            &out.with_extension("log"),
        )
        .unwrap();
        let tmp = out.with_extension("tmp");
        std::fs::write(&tmp, format!("{} {}\n", plain.id(), detached.id())).unwrap();
        std::fs::rename(&tmp, &out).unwrap();
        // The test kills this process long before the sleep ends; the waits
        // only matter if it is run by hand.
        std::thread::sleep(Duration::from_secs(120));
        let _ = plain.wait();
        let _ = detached.kill();
        let _ = detached.wait();
    }

    /// #3267: the agent terminal a node was started from is stopped
    /// (SIGSTOP; SIGTSTP is discarded for this orphaned stand-in group), then
    /// closed (SIGHUP), then its process group is killed (SIGTERM, SIGKILL),
    /// the way a shell or an agent harness ends a session. A child
    /// started the normal way is stopped and then killed with it; the
    /// detached child is never stopped and keeps running.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_detached_child_outlives_its_terminal_session() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("pids");
        let mut terminal = Command::new(std::env::current_exe().unwrap());
        terminal
            .args([
                "--exact",
                "localnode::tests::detach_helper_terminal",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_ENV, &out)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // The stand-in terminal gets its own session, so the group signals
        // below reach it and its children, never this test runner.
        // SAFETY: `setsid` is async-signal-safe.
        unsafe {
            terminal.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut terminal = terminal.spawn().unwrap();
        let session = terminal.id();
        assert!(
            wait_for(|| out.is_file()),
            "the helper never wrote its pids"
        );
        let pids: Vec<u32> = std::fs::read_to_string(&out)
            .unwrap()
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        let (plain, detached) = (pids[0], pids[1]);
        // SAFETY: both PIDs are children the helper started for this test.
        let cleanup = |pid: u32| unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        };

        let (_, plain_sid, _) = proc_stat(plain).unwrap();
        let (_, detached_sid, detached_tty) = proc_stat(detached).unwrap();
        assert_eq!(plain_sid, session as i64, "control child is in the session");
        assert_eq!(
            detached_sid, detached as i64,
            "detached child leads its own"
        );
        assert_eq!(
            detached_tty, 0,
            "detached child has no controlling terminal"
        );

        signal_group(session, libc::SIGSTOP);
        assert!(
            wait_for(|| proc_stat(plain).is_some_and(|(s, _, _)| s == 'T')),
            "control child should stop with its terminal"
        );
        assert!(
            proc_stat(detached).is_some_and(|(s, _, _)| s != 'T'),
            "the detached child must not stop with its terminal"
        );

        for sig in [libc::SIGHUP, libc::SIGCONT, libc::SIGTERM, libc::SIGKILL] {
            signal_group(session, sig);
        }
        let _ = terminal.wait();
        let plain_gone = wait_for(|| !running(plain));
        let detached_alive = running(detached);
        cleanup(plain);
        cleanup(detached);
        assert!(plain_gone, "control child should die with its session");
        assert!(
            detached_alive,
            "the detached child must survive its terminal session ending"
        );
    }

    /// stdin is `/dev/null` and both output streams are the log: no pipe or
    /// terminal of the parent is left open in the child (a closed pipe would
    /// SIGPIPE it, a hung-up terminal would SIGHUP or EIO it).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_detached_child_holds_no_stdio_of_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("node.log");
        let mut child = spawn_detached(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "for fd in 0 1 2; do readlink /proc/$$/fd/$fd; done".into(),
            ],
            dir.path(),
            &log,
        )
        .unwrap();
        child.wait().unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let log = std::fs::canonicalize(&log).unwrap();
        let log = log.to_str().unwrap();
        assert_eq!(lines, vec!["/dev/null", log, log], "{text}");
    }

    /// A caller that builds its own command gets the same session, and the
    /// builder is called once on Unix.
    #[cfg(unix)]
    #[test]
    fn spawn_detached_with_starts_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out.log");
        let mut calls = 0;
        let mut child = spawn_detached_with(|| {
            calls += 1;
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "ps -o sid= -p $$"])
                .stdin(Stdio::null())
                .stdout(Stdio::from(std::fs::File::create(&log)?));
            Ok(cmd)
        })
        .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(calls, 1);
        let sid: u32 = std::fs::read_to_string(&log)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(sid, pid);
    }

    #[test]
    fn a_closed_port_is_not_serving_and_a_503_is_still_starting() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let answer = |status: &'static str| {
            let listener = listener.try_clone().unwrap();
            std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = s.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
                );
            })
        };
        let t = answer("503 Service Unavailable");
        assert!(!node_serving("127.0.0.1", port, false));
        t.join().unwrap();
        let t = answer("401 Unauthorized");
        assert!(node_serving("127.0.0.1", port, false));
        t.join().unwrap();
        drop(listener);
        assert!(!node_serving("127.0.0.1", port, false));
    }
}
