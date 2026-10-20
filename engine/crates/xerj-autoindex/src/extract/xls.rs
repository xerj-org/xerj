//! XLS — legacy Excel 97-2003 workbooks (BIFF8 inside an OLE2 compound file),
//! read as tables on the same record model as [`super::xlsx`].
//!
//! Each worksheet is its own dataset (`RawRecord.group` = sheet name) and each
//! row below the sheet's header row is one record keyed by the header cells,
//! with values carrying the type the cell stored: numbers stay numbers,
//! booleans stay booleans, and a date/time cell becomes an ISO-8601 string so
//! type inference elects `date` rather than seeing a serial number. The header
//! is the first row (among the first [`HEADER_SCAN_ROWS`] non-empty rows) with
//! at least two cells, all text or date and at least one text, that is not
//! immediately followed by a wider row of the same kind; rows above it are not
//! indexed. A sheet with no such header is indexed as a document, one line per
//! row with cells joined by ` | `.
//!
//! Reading is delegated to calamine, which ships its own OLE2/BIFF reader. The
//! heavy lifting of BIFF record parsing, the shared-string table and the
//! legacy 8-bit cell codepage all live there; this module only maps calamine's
//! `Data` cells onto the extractor's typed record model.
//!
//! Differences from the XLSX extractor, by design: calamine materialises a
//! whole worksheet as a `Range` (the BIFF format caps a sheet at 65,536 rows ×
//! 256 columns, and calamine reads it in one pass), so there is no streaming
//! row cap of our own — the per-sheet sample limit bounds how many rows we
//! EMIT, and [`MAX_ROWS_PER_SHEET`] bounds how many we even look at against a
//! crafted dimension record. Vertical merge fill-down is not applied (`.xls`
//! merge ranges are not surfaced on calamine's `Range`); a category label
//! merged down a column therefore appears only on its first row.

use super::{
    emit_document_with_fields, sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink,
    MAX_FIELDS_PER_RECORD,
};
use anyhow::Result;
use calamine::{Data, DataType, Reader, Xls};
use chrono::Timelike;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::Read;
use std::path::Path;

/// Non-empty rows searched for a header before the sheet is treated as a
/// document (mirrors the XLSX extractor).
const HEADER_SCAN_ROWS: usize = 20;

/// Defensive cap on rows inspected per sheet, in case a crafted BIFF dimension
/// record claims a range far larger than the format's own 65,536-row ceiling.
const MAX_ROWS_PER_SHEET: usize = 1 << 20;

/// Text kept from one sheet indexed as a document.
const MAX_DOC_SHEET_BYTES: usize = 16 << 20;

/// BIFF8 worksheet ceiling: 65,536 rows × 256 columns. A `DIMENSIONS` record
/// (`0x0200`) claiming more than this is malformed or crafted — and it matters
/// because calamine materialises a *dense* `Range` sized from that record
/// before we ever iterate a row (`MAX_ROWS_PER_SHEET` on `range.rows()` is
/// applied too late). An inflated dimension therefore drives an unbounded
/// allocation that aborts the whole scan (release builds are `panic = abort`,
/// and nothing wraps the extractor in `catch_unwind`). So we read the
/// dimensions ourselves and refuse the file before handing it to calamine.
const BIFF_MAX_ROWS: u32 = 65_536;
const BIFF_MAX_COLS: u32 = 256;

/// Upper bound on the Workbook stream bytes read for the dimension pre-flight.
/// A legitimate `.xls` Workbook stream is far below this; the `take()` cap also
/// bounds the pre-flight's own allocation regardless of what the container
/// claims the stream length to be.
const MAX_WORKBOOK_STREAM: u64 = 512 << 20;

/// `false` ⇒ refuse the file: a `DIMENSIONS` record exceeds the BIFF8 ceiling,
/// so calamine would over-allocate. `true` ⇒ no crafted dimension found;
/// calamine may read it. Never panics — any CFB/IO failure returns `true` (not
/// our gate; `Xls::new` rejects a genuinely broken file cleanly), and the read
/// is `take()`-bounded so it cannot itself be driven to over-allocate.
fn xls_allocations_bounded(path: &Path) -> bool {
    let Ok(mut comp) = cfb::open(path) else {
        return true;
    };
    let name = if comp.is_stream("Workbook") {
        "Workbook"
    } else if comp.is_stream("Book") {
        "Book"
    } else {
        return true;
    };
    let Ok(stream) = comp.open_stream(name) else {
        return true;
    };
    let mut buf = Vec::new();
    if stream
        .take(MAX_WORKBOOK_STREAM)
        .read_to_end(&mut buf)
        .is_err()
    {
        return true;
    }
    biff_allocations_bounded(&buf)
}

/// Walk the BIFF record stream and refuse the file if any record would drive
/// calamine 0.26.1 to an unbounded allocation from attacker-controlled bytes.
/// It is not enough to bound one record shape (an earlier version gated only
/// the 14-byte `DIMENSIONS` and was still abortable through three other
/// records): this covers every byte-driven allocation site audited in
/// calamine's `xls.rs`:
///
///   * `DIMENSIONS` (`0x0200`), BOTH the 14-byte (BIFF8) and 10-byte (BIFF2-5)
///     forms — `parse_dimensions` dispatches on the body length, then the
///     caller `reserve`s `(rl-rf)*(cl-cf)` cells for a dense `Range`. We refuse
///     a last-row past 65,536, a last-col past 256, or an inverted range (which
///     underflows calamine's `end - start` into a vast count).
///   * `SST` (`0x00FC`) — `Vec::with_capacity(cstUnique)` before a single
///     string is read, and `i32::try_into::<usize>().unwrap()` which *panics*
///     on a negative count. We refuse a negative count or one larger than the
///     stream can hold (every SST entry costs at least a byte).
///
/// Linear, bounds-checked and terminating: every step advances `pos` past a
/// `u16` length, and every slice is guarded by `end <= stream.len()`, so a
/// crafted length can neither loop nor read out of bounds. Unrecognised records
/// are skipped by their length.
fn biff_allocations_bounded(stream: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos + 4 <= stream.len() {
        let rectype = u16::from_le_bytes([stream[pos], stream[pos + 1]]);
        let reclen = u16::from_le_bytes([stream[pos + 2], stream[pos + 3]]) as usize;
        let body = pos + 4;
        let Some(end) = body.checked_add(reclen) else {
            break;
        };
        if end > stream.len() {
            break;
        }
        let d = &stream[body..end];
        match rectype {
            // DIMENSIONS — the dense-`Range` reserve. `rl`/`cl` are last-index
            // plus one; `rf`/`cf` are first index.
            0x0200 => {
                let (rf, rl, cf, cl) = if reclen >= 14 {
                    (
                        u32::from_le_bytes([d[0], d[1], d[2], d[3]]),
                        u32::from_le_bytes([d[4], d[5], d[6], d[7]]),
                        u16::from_le_bytes([d[8], d[9]]) as u32,
                        u16::from_le_bytes([d[10], d[11]]) as u32,
                    )
                } else if reclen >= 10 {
                    (
                        u16::from_le_bytes([d[0], d[1]]) as u32,
                        u16::from_le_bytes([d[2], d[3]]) as u32,
                        u16::from_le_bytes([d[4], d[5]]) as u32,
                        u16::from_le_bytes([d[6], d[7]]) as u32,
                    )
                } else {
                    // Too short for either form; calamine errors on it cleanly.
                    (0, 0, 0, 0)
                };
                if rl > BIFF_MAX_ROWS || cl > BIFF_MAX_COLS || rl < rf || cl < cf {
                    return false;
                }
            }
            // SST — shared-string table. `cstUnique` (a signed i32 at offset 4)
            // sizes the Vec before any string is parsed.
            0x00FC if reclen >= 8 => {
                let cst_unique = i32::from_le_bytes([d[4], d[5], d[6], d[7]]);
                if cst_unique < 0 || cst_unique as u64 > stream.len() as u64 {
                    return false;
                }
            }
            _ => {}
        }
        pos = end;
    }
    true
}

/// Hidden worker argv[1] the isolated BIFF reader is dispatched on. Reachable
/// through the ordinary `xerj` binary (see `xerj-server/src/main.rs`), spawned
/// via `current_exe()` — the same mechanism the PDF worker and the `xerj brain`
/// server boot already use.
pub const WORKER_ARG: &str = "__extract-xls";

/// Test/developer hook: substitute the worker executable. Production uses
/// `current_exe()`. A cargo test's `current_exe()` is the *test* binary, which
/// does not carry the `__extract-xls` dispatch, so the isolation integration
/// test points this at the built `xerj` binary — mirrors `XERJ_PDF_WORKER_BIN`.
const WORKER_BIN_ENV: &str = "XERJ_XLS_WORKER_BIN";

/// Address-space ceiling imposed on the worker (`RLIMIT_AS`). A crafted
/// `DIMENSIONS`/`SST` that slips the pre-filter, or any other attacker-driven
/// `Vec::with_capacity`, hits this and fails the allocation inside the worker —
/// where it only kills the worker — instead of aborting the indexer.
#[cfg(unix)]
const WORKER_ADDRESS_SPACE: u64 = 2 << 30; // 2 GiB

/// CPU-time ceiling imposed on the worker (`RLIMIT_CPU`, seconds). A record
/// stream crafted to spin inside calamine is bounded here; the kernel sends
/// `SIGXCPU` at the soft limit and `SIGKILL` at the hard limit, either of which
/// the parent reports as junk.
#[cfg(unix)]
const WORKER_CPU_SECONDS: u64 = 15;

/// Wall-clock backstop for the whole worker, independent of CPU time — covers a
/// worker wedged in a blocking syscall that burns no CPU. `RLIMIT_CPU` is the
/// primary guard; this only fires if the worker is stuck without the kernel
/// noticing.
#[cfg(unix)]
const WORKER_WALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Upper bound on the NDJSON the parent will buffer from one worker. A legacy
/// `.xls` sheet is capped by the format at 65,536 rows × 256 columns, so a
/// well-formed workbook is far below this; a worker emitting more than this is
/// treated as junk rather than letting the parent's own memory grow unbounded
/// on the worker's say-so.
#[cfg(unix)]
const MAX_WORKER_OUTPUT: usize = 128 << 20; // 128 MiB

/// One line of the parent↔worker protocol. The worker serializes every produced
/// record as a `Rec` line and closes with exactly one `End` line carrying the
/// truncation flag; the parent parses them back into the identical records the
/// in-process path would have emitted. Internally tagged on `k` so a record
/// line (which never carries a `k` field) and the terminator are distinguished
/// without a second pass.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "k")]
enum WorkerLine {
    /// One extracted record, byte-for-byte the `RawRecord` the sink receives.
    #[serde(rename = "rec")]
    Rec(RawRecord),
    /// Terminator: the worker finished the file cleanly. Its presence is what
    /// tells the parent the stream was not cut short by a crash mid-emit.
    #[serde(rename = "end")]
    End { truncated: bool },
}

/// `name` is the file as the corpus names it (under durable preparation this is
/// not `path`, a sealed snapshot blob); document sheets are titled from it.
/// `per_sheet_limit` caps the rows emitted from each sheet (phase-A sampling);
/// `None` reads every row.
///
/// The dump being indexed is untrusted (Jira/Confluence/GitLab attachments) and
/// calamine 0.26.1 is *not* panic-safe on hostile BIFF: beyond the allocation
/// vectors [`xls_allocations_bounded`] pre-filters, it indexes a short `Lbl`
/// (`0x0018`) or `ExternSheet` (`0x0017`) record straight out of bounds and
/// panics. The engine release profile is `panic = abort` with no `catch_unwind`
/// around the scan, so such a panic aborts the WHOLE indexer, and `catch_unwind`
/// cannot help under `panic = abort`. So on unix the actual BIFF read runs in a
/// short-lived child process under `RLIMIT_AS`/`RLIMIT_CPU`: a panic, abort,
/// OOM-kill or runaway there dies with the child and is counted as junk, and the
/// parent never calls calamine on the raw file itself. Non-unix keeps the
/// in-process bounded path as a documented best-effort (the dump is Linux).
pub fn extract(
    path: &Path,
    name: &Path,
    per_sheet_limit: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    // Cheap pre-filter, kept from #1324: refuse the whole crafted-allocation
    // class before paying to spawn a worker at all. It is a pre-filter now, not
    // the whole defence — the worker's rlimits catch what byte-walking a record
    // stream cannot (every other allocation site, and the out-of-bounds panics).
    if !xls_allocations_bounded(path) {
        let mut stats = ExtractStats::default();
        stats.junk += 1;
        return Ok(stats);
    }
    #[cfg(unix)]
    {
        extract_isolated(path, name, per_sheet_limit, sink)
    }
    #[cfg(not(unix))]
    {
        // No `setrlimit`/`pre_exec` here; best-effort in-process read bounded by
        // the pre-filter above. Documented: the production dump is Linux.
        extract_in_process(path, name, per_sheet_limit, sink)
    }
}

/// Parent side of the isolation: spawn `current_exe() __extract-xls <path>
/// <name> <limit>`, read its NDJSON, and deliver the records to `sink` exactly
/// as the in-process path would. Any abnormal exit — non-zero, signal
/// (SIGABRT/SIGKILL/SIGXCPU), wall-clock timeout, over-cap output, a line that
/// will not parse, or a missing `End` terminator — becomes `junk += 1` and
/// `Ok`, so one hostile file is skipped and the scan continues. All-or-nothing:
/// records are buffered and only delivered once the worker has closed cleanly,
/// so a worker that crashes mid-file indexes nothing from it (no partial rows).
#[cfg(unix)]
fn extract_isolated(
    path: &Path,
    name: &Path,
    per_sheet_limit: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    match run_worker(path, name, per_sheet_limit) {
        Ok((records, truncated)) => {
            for rec in records {
                stats.records += 1;
                if !sink(rec) {
                    // Sampling stop: the caller has what it wanted. Success.
                    return Ok(stats);
                }
            }
            if stats.records == 0 {
                // Clean run that produced nothing (unreadable-but-not-crafted
                // workbook, or an empty one) — junk, mirroring the in-process
                // `records == 0` rule.
                stats.junk += 1;
            } else {
                stats.truncated = truncated;
            }
        }
        // Crash / timeout / protocol failure: skip the file, keep scanning.
        Err(_) => stats.junk += 1,
    }
    Ok(stats)
}

/// Run one worker to completion and return its buffered records plus the
/// truncation flag, or an error describing why the file could not be trusted.
#[cfg(unix)]
fn run_worker(
    path: &Path,
    name: &Path,
    per_sheet_limit: Option<u64>,
) -> Result<(Vec<RawRecord>, bool)> {
    use anyhow::{anyhow, Context};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let exe = std::env::var_os(WORKER_BIN_ENV)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("locate the xerj binary for isolated .xls extraction")?;

    let mut command = Command::new(exe);
    command
        .arg(WORKER_ARG)
        .arg(path.as_os_str())
        .arg(name.as_os_str())
        .arg(match per_sheet_limit {
            Some(n) => n.to_string(),
            None => "none".to_string(),
        })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child before exec and calls only
    // async-signal-safe libc (`setpgid`, `setrlimit`). Its own process group so
    // the parent can reap the whole tree; the rlimits cap the child's blast
    // radius to itself.
    unsafe {
        command.pre_exec(worker_pre_exec);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn the isolated .xls reader for {}", path.display()))?;
    let pid = child.id();

    let stdout = child.stdout.take().expect("piped stdout");
    // Read on a thread so the wall-clock watchdog below can act even if the
    // worker stalls with the pipe open.
    //
    // #1122: `std::thread::spawn` panics when a thread cannot be created and
    // release builds are panic=abort; a failed spawn stops the worker and
    // fails this one file instead of aborting the run.
    let reader = match std::thread::Builder::new()
        .name("xerj-xls-stdout".into())
        .spawn(move || read_capped(stdout, MAX_WORKER_OUTPUT))
    {
        Ok(reader) => reader,
        Err(error) => {
            kill_group(pid);
            let _ = child.wait();
            return Err(error).with_context(|| {
                format!(
                    "could not start the .xls reader's output thread for {}",
                    path.display()
                )
            });
        }
    };

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group(pid);
                let _ = child.wait();
                let _ = reader.join();
                return Err(e)
                    .with_context(|| format!("monitor the .xls reader for {}", path.display()));
            }
        }
        if started.elapsed() >= WORKER_WALL_TIMEOUT {
            kill_group(pid);
            let _ = child.wait();
            let _ = reader.join();
            return Err(anyhow!(
                "isolated .xls reader for {} exceeded the {}s wall-clock limit",
                path.display(),
                WORKER_WALL_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    // Reap any stragglers left in the group, then collect the output.
    kill_group(pid);
    let captured = reader
        .join()
        .map_err(|_| anyhow!("the .xls reader output thread panicked"))??;
    if captured.overflow {
        return Err(anyhow!(
            "isolated .xls reader for {} produced more than {} MiB of output",
            path.display(),
            MAX_WORKER_OUTPUT >> 20
        ));
    }
    if !status.success() {
        return Err(anyhow!(
            "isolated .xls reader for {} exited abnormally ({status})",
            path.display()
        ));
    }
    parse_worker_output(&captured.bytes).with_context(|| {
        format!(
            "parse the isolated .xls reader output for {}",
            path.display()
        )
    })
}

/// Parse the worker's NDJSON into the records and truncation flag. A clean run
/// ends with exactly one `End` line; its absence means the stream was cut short
/// (a crash the exit status somehow did not surface) and is an error.
#[cfg(unix)]
fn parse_worker_output(bytes: &[u8]) -> Result<(Vec<RawRecord>, bool)> {
    use anyhow::anyhow;
    let mut records = Vec::new();
    let mut ended: Option<bool> = None;
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if ended.is_some() {
            return Err(anyhow!("trailing output after the .xls reader terminator"));
        }
        match serde_json::from_slice::<WorkerLine>(line)? {
            WorkerLine::Rec(rec) => records.push(rec),
            WorkerLine::End { truncated } => ended = Some(truncated),
        }
    }
    match ended {
        Some(truncated) => Ok((records, truncated)),
        None => Err(anyhow!("the .xls reader did not emit a terminator")),
    }
}

/// Pre-exec child setup: own process group + address-space and CPU-time caps.
/// Runs in the forked child; only async-signal-safe libc calls.
#[cfg(unix)]
fn worker_pre_exec() -> std::io::Result<()> {
    // SAFETY: `setpgid`/`setrlimit` are async-signal-safe and documented here.
    unsafe {
        if libc::setpgid(0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    set_worker_rlimits()
}

/// Apply `RLIMIT_AS` and `RLIMIT_CPU` to the current process. Used by
/// [`worker_pre_exec`] (parent-forked child) and again by the worker itself at
/// entry (belt-and-suspenders, in case it was spawned some other way).
///
/// Only ever *lowers* a limit: an unprivileged process cannot raise its hard
/// limit, so the target is clamped to whatever was inherited. This matters when
/// the indexer already runs under a tighter cgroup or `ulimit -v` — trying to
/// set a higher ceiling there would `EPERM` and fail the whole spawn, turning
/// every `.xls` (benign included) into junk.
#[cfg(unix)]
fn set_worker_rlimits() -> std::io::Result<()> {
    // Set `$res`'s soft/hard limits to the requested values, each capped at the
    // inherited hard limit so the call never tries to RAISE a ceiling (which an
    // unprivileged process may not do, and which would `EPERM` the whole spawn).
    // A macro, not a function, so the resource constant is passed straight to
    // libc without naming its type — that type is `__rlimit_resource_t` on
    // glibc but `c_int` on macOS/BSD.
    macro_rules! clamp {
        ($res:expr, $soft:expr, $hard:expr) => {{
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: plain libc FFI over an out-param `rlimit`.
            if unsafe { libc::getrlimit($res, &mut current) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let ceiling = current.rlim_max; // inherited hard limit (maybe RLIM_INFINITY)
            let want_hard = ($hard as libc::rlim_t).min(ceiling);
            let want_soft = ($soft as libc::rlim_t).min(want_hard);
            let limit = libc::rlimit {
                rlim_cur: want_soft,
                rlim_max: want_hard,
            };
            // SAFETY: as above; cur <= max <= inherited max, so this only lowers.
            if unsafe { libc::setrlimit($res, &limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }};
    }
    clamp!(libc::RLIMIT_AS, WORKER_ADDRESS_SPACE, WORKER_ADDRESS_SPACE);
    // Soft limit sends SIGXCPU; hard limit (a touch higher) escalates to SIGKILL.
    clamp!(libc::RLIMIT_CPU, WORKER_CPU_SECONDS, WORKER_CPU_SECONDS + 2);
    Ok(())
}

/// SIGKILL the worker's whole process group (negative pid). Best effort.
#[cfg(unix)]
fn kill_group(pid: u32) {
    // SAFETY: plain libc FFI; an already-dead group is a harmless ESRCH.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(unix)]
struct Captured {
    bytes: Vec<u8>,
    overflow: bool,
}

/// Read `input` to EOF, keeping at most `cap` bytes and flagging overflow.
/// Keeps draining past the cap so the child never blocks on a full pipe.
#[cfg(unix)]
fn read_capped(mut input: impl Read, cap: usize) -> Result<Captured> {
    use anyhow::Context;
    let mut bytes = Vec::new();
    let mut overflow = false;
    let mut chunk = [0u8; 64 << 10];
    loop {
        let n = input
            .read(&mut chunk)
            .context("read isolated .xls reader output")?;
        if n == 0 {
            break;
        }
        if !overflow {
            let room = cap.saturating_sub(bytes.len());
            let take = room.min(n);
            bytes.extend_from_slice(&chunk[..take]);
            overflow |= take < n;
        }
    }
    Ok(Captured { bytes, overflow })
}

/// Hidden same-binary worker entry point (`xerj __extract-xls <path> <name>
/// <limit|none>`). One invocation reads one workbook and streams its records as
/// NDJSON on stdout. If calamine panics/aborts here, this process dies and the
/// parent handles it; nothing in the parent ever touches the raw file.
pub fn run_worker_cli() -> i32 {
    // Re-apply the resource caps the parent set via `pre_exec`, so the worker is
    // bounded even if it was launched directly (tests, a future caller).
    #[cfg(unix)]
    if let Err(e) = set_worker_rlimits() {
        eprintln!(".xls worker could not set its resource limits: {e}");
        // Not fatal: the parent's `pre_exec` is the authoritative cap.
    }

    let mut args = std::env::args_os().skip(2);
    let (Some(path), Some(name), Some(limit)) = (args.next(), args.next(), args.next()) else {
        eprintln!(".xls worker protocol error: expected <path> <name> <limit>");
        return 2;
    };
    if args.next().is_some() {
        eprintln!(".xls worker protocol error: unexpected trailing arguments");
        return 2;
    }
    let per_sheet_limit = match limit.to_string_lossy().as_ref() {
        "none" => None,
        other => match other.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => {
                eprintln!(".xls worker protocol error: bad limit {other:?}");
                return 2;
            }
        },
    };

    let path = std::path::PathBuf::from(path);
    let name = std::path::PathBuf::from(name);

    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut write_err = false;
    let mut emit = |rec: RawRecord| -> bool {
        if serde_json::to_writer(&mut out, &WorkerLine::Rec(rec)).is_err()
            || out.write_all(b"\n").is_err()
        {
            write_err = true;
            return false;
        }
        true
    };

    match extract_in_process(&path, &name, per_sheet_limit, &mut emit) {
        Ok(stats) => {
            if write_err {
                return 1;
            }
            let end = WorkerLine::End {
                truncated: stats.truncated,
            };
            if serde_json::to_writer(&mut out, &end).is_err()
                || out.write_all(b"\n").is_err()
                || out.flush().is_err()
            {
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!(".xls worker failed: {e:#}");
            1
        }
    }
}

/// The in-process BIFF read. On unix this runs *inside the worker*; on non-unix
/// it is the whole (best-effort) path. It still runs the allocation pre-filter
/// itself, so it is self-contained wherever it is called from.
fn extract_in_process(
    path: &Path,
    name: &Path,
    per_sheet_limit: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    // DoS gate: calamine allocates a dense `Range` from the sheet's declared
    // `DIMENSIONS` before any row is read, so a crafted record makes it attempt
    // a multi-terabyte allocation and abort the process (release = panic=abort,
    // no catch_unwind around the scan). Refuse such a file up front; the read
    // here is bounded by `cfb`'s FAT chain, so the gate cannot itself be driven
    // to over-allocate.
    if !xls_allocations_bounded(path) {
        stats.junk += 1;
        return Ok(stats);
    }
    let f = std::fs::File::open(path)?;
    // A malformed/truncated workbook that still clears the dimension gate is
    // treated as junk rather than propagating — a single unreadable legacy file
    // must not abort a folder scan. (calamine *returns* an error for most such
    // files, but is not panic-free on hostile input, which is why the gate
    // above runs first.)
    let mut wb = match Xls::new(f) {
        Ok(wb) => wb,
        Err(_) => {
            stats.junk += 1;
            return Ok(stats);
        }
    };

    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into());

    let sheet_names = wb.sheet_names().to_vec();
    for sheet_name in sheet_names {
        let Ok(range) = wb.worksheet_range(&sheet_name) else {
            continue;
        };
        let start_row = range.start().map(|(r, _)| r).unwrap_or(0);
        let start_col = range.start().map(|(_, c)| c).unwrap_or(0);

        // Build the non-empty rows, each carrying 1-based column numbers so the
        // header/column machinery matches the XLSX extractor's.
        let mut rows: Vec<Row> = Vec::new();
        for (i, cells) in range.rows().take(MAX_ROWS_PER_SHEET).enumerate() {
            let mut out = Vec::new();
            for (j, data) in cells.iter().enumerate() {
                if let Some(cell) = convert_cell(start_col + j as u32 + 1, data) {
                    out.push(cell);
                }
            }
            if !out.is_empty() {
                rows.push(Row {
                    num: start_row + i as u32 + 1,
                    cells: out,
                });
            }
        }
        if rows.is_empty() {
            continue;
        }

        if let Some(h) = find_header(&rows) {
            if !emit_table(&sheet_name, &rows, h, per_sheet_limit, sink, &mut stats) {
                break;
            }
        } else if !emit_document_sheet(&sheet_name, &stem, &rows, sink, &mut stats) {
            break;
        }
    }

    if stats.records == 0 {
        stats.junk += 1;
    }
    Ok(stats)
}

/// A cell's typed kind, mirroring the XLSX extractor so header detection and
/// type inference agree across the two formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Number,
    Bool,
    Date,
}

#[derive(Debug, Clone)]
struct Cell {
    /// 1-based column (A = 1).
    col: u32,
    value: Value,
    kind: Kind,
}

impl Cell {
    fn display(&self) -> String {
        match &self.value {
            Value::String(s) => s.clone(),
            Value::Bool(true) => "TRUE".into(),
            Value::Bool(false) => "FALSE".into(),
            v => v.to_string(),
        }
    }
}

#[derive(Debug)]
struct Row {
    /// 1-based row number, as Excel shows it.
    num: u32,
    cells: Vec<Cell>,
}

/// Map one calamine cell onto the typed model; `None` for an empty cell or an
/// error value (`#N/A`, `#DIV/0!`), exactly as the XLSX extractor drops them.
fn convert_cell(col: u32, data: &Data) -> Option<Cell> {
    match data {
        Data::Empty | Data::Error(_) => None,
        Data::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| Cell {
                col,
                value: Value::String(s.to_string()),
                kind: Kind::Text,
            })
        }
        Data::Int(i) => Some(Cell {
            col,
            value: Value::Number((*i).into()),
            kind: Kind::Number,
        }),
        Data::Float(f) => Some(Cell {
            col,
            value: number_value(*f)?,
            kind: Kind::Number,
        }),
        Data::Bool(b) => Some(Cell {
            col,
            value: Value::Bool(*b),
            kind: Kind::Bool,
        }),
        Data::DateTimeIso(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| Cell {
                col,
                value: Value::String(s.to_string()),
                kind: Kind::Date,
            })
        }
        Data::DurationIso(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| Cell {
                col,
                value: Value::String(s.to_string()),
                kind: Kind::Text,
            })
        }
        Data::DateTime(_) => date_cell(col, data),
    }
}

/// A calamine `Data::DateTime` rendered to the same ISO-8601 strings the XLSX
/// extractor emits: a bare date at midnight, a `T`-joined datetime otherwise,
/// and `HH:MM:SS` for a duration/time-of-day. Falls back to the raw serial
/// number if calamine cannot resolve it to a chrono value.
fn date_cell(col: u32, data: &Data) -> Option<Cell> {
    // A time of day (fraction of a day with no date part) resolves to a
    // `NaiveTime` but not a sensible `NaiveDateTime`; check it first.
    if let Some(dt) = data.as_datetime() {
        let iso = if dt.num_seconds_from_midnight() == 0 && dt.nanosecond() == 0 {
            dt.format("%Y-%m-%d").to_string()
        } else if dt.nanosecond() == 0 {
            dt.format("%Y-%m-%dT%H:%M:%S").to_string()
        } else {
            dt.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
        };
        return Some(Cell {
            col,
            value: Value::String(iso),
            kind: Kind::Date,
        });
    }
    if let Some(t) = data.as_time() {
        return Some(Cell {
            col,
            value: Value::String(t.format("%H:%M:%S").to_string()),
            kind: Kind::Date,
        });
    }
    // Could not resolve a calendar value: keep the serial number as a number so
    // nothing is dropped.
    let f = data.as_f64()?;
    Some(Cell {
        col,
        value: number_value(f)?,
        kind: Kind::Number,
    })
}

/// Integral floats inside the i64-exact range become integers (so `1250.0`
/// indexes as `1250`), matching the XLSX extractor's `number_value`.
fn number_value(f: f64) -> Option<Value> {
    if !f.is_finite() {
        return None;
    }
    if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
        Some(Value::Number((f as i64).into()))
    } else {
        serde_json::Number::from_f64(f).map(Value::Number)
    }
}

/// Column letters of a column number (28 → `AB`).
fn column_letters(mut col: u32) -> String {
    let mut out = Vec::new();
    while col > 0 {
        let rem = (col - 1) % 26;
        out.push(b'A' + rem as u8);
        col = (col - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Whether a row can be a header: two or more cells, some text, and no numbers
/// or booleans (date cells are allowed, for month-column headers).
fn is_header_candidate(row: &Row) -> bool {
    row.cells.len() >= 2
        && row
            .cells
            .iter()
            .all(|c| matches!(c.kind, Kind::Text | Kind::Date))
        && row.cells.iter().any(|c| c.kind == Kind::Text)
}

/// Index of the header row. A candidate followed by a WIDER candidate is a
/// title line above the real header and is passed over. Only the first
/// [`HEADER_SCAN_ROWS`] non-empty rows are considered.
fn find_header(rows: &[Row]) -> Option<usize> {
    let scan = rows.len().min(HEADER_SCAN_ROWS);
    rows[..scan].windows(2).position(|w| {
        is_header_candidate(&w[0])
            && !(is_header_candidate(&w[1]) && w[1].cells.len() > w[0].cells.len())
    })
}

/// Emit every data row under `header_idx` as a typed record keyed by the header
/// cells. Returns false if the sink asked to stop the whole file.
fn emit_table(
    sheet: &str,
    rows: &[Row],
    header_idx: usize,
    limit: Option<u64>,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    let header = &rows[header_idx];
    let mut seen = std::collections::HashSet::new();
    let mut names: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for c in &header.cells {
        let mut name = sanitize_field_name(&c.display());
        while !seen.insert(name.clone()) {
            name.push_str("_2");
        }
        names.insert(c.col, name);
    }
    let header_cells: Vec<(u32, String)> =
        header.cells.iter().map(|c| (c.col, c.display())).collect();

    let mut emitted = 0u64;
    for row in &rows[header_idx + 1..] {
        // A repeated header row (common when a sheet stacks sections) is not a
        // data record.
        let row_text: Vec<(u32, String)> = row.cells.iter().map(|c| (c.col, c.display())).collect();
        if row_text == header_cells {
            continue;
        }
        let mut fields = Map::new();
        for c in &row.cells {
            if fields.len() >= MAX_FIELDS_PER_RECORD {
                break;
            }
            let name = names
                .get(&c.col)
                .cloned()
                .unwrap_or_else(|| format!("col_{}", column_letters(c.col)));
            fields.insert(name, c.value.clone());
        }
        stats.records += 1;
        if !sink(RawRecord {
            fields,
            locator: format!("{sheet}!r{}", row.num),
            group: Some(sheet.to_string()),
            origin: FieldOrigin::Data,
        }) {
            return false;
        }
        emitted += 1;
        if limit.is_some_and(|lim| emitted >= lim) {
            break;
        }
    }
    true
}

/// Emit a sheet with no detectable header as a document: one line per row,
/// cells joined by ` | `. Returns false if the sink asked to stop.
fn emit_document_sheet(
    sheet: &str,
    stem: &str,
    rows: &[Row],
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    let mut body = String::new();
    let mut last_row = 0u32;
    for row in rows {
        let line = row
            .cells
            .iter()
            .map(Cell::display)
            .collect::<Vec<_>>()
            .join(" | ");
        if body.len() + line.len() + 2 > MAX_DOC_SHEET_BYTES {
            stats.truncated = true;
            break;
        }
        if !body.is_empty() {
            // A gap in row numbers is a paragraph break, where section
            // splitting prefers to cut.
            body.push_str(if row.num > last_row + 1 { "\n\n" } else { "\n" });
        }
        body.push_str(&line);
        last_row = row.num;
    }
    if body.trim().is_empty() {
        return true;
    }
    let mut base = Map::new();
    base.insert("sheet".into(), Value::String(sheet.to_string()));
    emit_document_with_fields(
        &base,
        &format!("{stem} — {sheet}"),
        &body,
        sheet,
        sink,
        stats,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(r: &RawRecord) -> Value {
        Value::Object(r.fields.clone())
    }

    /// A BIFF8 `DIMENSIONS` record (type 0x0200, len 14): rwMic, rwMac, colMic,
    /// colMac (each last-index-plus-one), reserved.
    fn dimensions_record(rw_mic: u32, rw_mac: u32, col_mic: u16, col_mac: u16) -> Vec<u8> {
        let mut r = vec![0x00, 0x02, 14, 0x00]; // type 0x0200, length 14
        r.extend_from_slice(&rw_mic.to_le_bytes());
        r.extend_from_slice(&rw_mac.to_le_bytes());
        r.extend_from_slice(&col_mic.to_le_bytes());
        r.extend_from_slice(&col_mac.to_le_bytes());
        r.extend_from_slice(&[0x00, 0x00]); // reserved
        r
    }

    #[test]
    fn a_real_dimension_is_accepted() {
        // A 100-row × 10-column sheet is well within the BIFF8 ceiling.
        assert!(biff_allocations_bounded(&dimensions_record(0, 100, 0, 10)));
        // And the format's own maximum is still accepted.
        assert!(biff_allocations_bounded(&dimensions_record(
            0, 65_536, 0, 256
        )));
    }

    #[test]
    fn a_crafted_dimension_past_the_biff_ceiling_is_refused() {
        // The shape of the 36 KB proof-of-concept: a DIMENSIONS claiming a vast
        // row range, which drove calamine to attempt a ~2.1 TB Range alloc.
        assert!(!biff_allocations_bounded(&dimensions_record(
            0,
            0x7FFF_FFFF,
            0,
            256
        )));
        // An inflated column count is refused the same way.
        assert!(!biff_allocations_bounded(&dimensions_record(
            0, 100, 0, 60_000
        )));
        // Inverted ranges (mac < mic) are malformed and refused.
        assert!(!biff_allocations_bounded(&dimensions_record(
            500, 100, 0, 10
        )));
    }

    #[test]
    fn a_truncated_or_dimensionless_stream_is_not_mistaken_for_crafted() {
        // No DIMENSIONS record at all: nothing to object to, let calamine try.
        assert!(biff_allocations_bounded(
            b"\x09\x08\x04\x00\x00\x00\x00\x00"
        )); // a BOF-ish record, no 0x0200
            // A record header whose declared length runs past the buffer end stops
            // the walk without a panic and without a false refusal.
        assert!(biff_allocations_bounded(&[0x00, 0x02, 0xFF, 0xFF, 0x01]));
        // Empty stream.
        assert!(biff_allocations_bounded(&[]));
    }

    /// The 10-byte (BIFF2-5) `DIMENSIONS` form: type 0x0200, length 10, four
    /// u16 fields (rf, rl, cf, cl).
    fn dimensions_record_biff5(rf: u16, rl: u16, cf: u16, cl: u16) -> Vec<u8> {
        let mut r = vec![0x00, 0x02, 10, 0x00]; // type 0x0200, body length 10
        for v in [rf, rl, cf, cl] {
            r.extend_from_slice(&v.to_le_bytes()); // 8 bytes of fields
        }
        r.extend_from_slice(&[0x00, 0x00]); // + 2 bytes so the body is 10, as calamine expects
        r
    }

    /// An `SST` record (type 0x00FC): cstTotal (i32) then cstUnique (i32). The
    /// body past the header is irrelevant to the allocation gate.
    fn sst_record(cst_unique: i32) -> Vec<u8> {
        let mut r = vec![0xFC, 0x00, 8, 0x00];
        r.extend_from_slice(&0i32.to_le_bytes()); // cstTotal
        r.extend_from_slice(&cst_unique.to_le_bytes()); // cstUnique
        r
    }

    #[test]
    fn the_10_byte_dimensions_form_is_also_bounded() {
        // A real small BIFF5 sheet is accepted.
        assert!(biff_allocations_bounded(&dimensions_record_biff5(
            0, 100, 0, 10
        )));
        // The reviewer's bypass: rl = cl = 0xFFFF drove calamine to a ~171 GB
        // reserve because the earlier gate only checked the 14-byte form. The
        // 65,535-column claim is past the 256 ceiling and is now refused.
        assert!(!biff_allocations_bounded(&dimensions_record_biff5(
            0, 0xFFFF, 0, 0xFFFF
        )));
    }

    #[test]
    fn a_crafted_sst_count_is_refused() {
        // A sane shared-string count is accepted (well under the stream length).
        let mut benign = sst_record(3);
        benign.extend_from_slice(&[0u8; 64]); // give the stream some length
        assert!(biff_allocations_bounded(&benign));
        // A huge cstUnique drove `Vec::with_capacity(~2.1e9)` → ~51 GB abort.
        assert!(!biff_allocations_bounded(&sst_record(0x7FFF_FFFF)));
        // A negative cstUnique made calamine `i32::try_into::<usize>().unwrap()`
        // panic (also an abort under panic=abort).
        assert!(!biff_allocations_bounded(&sst_record(-1)));
    }

    /// Build a one-sheet `.xls` in a tempdir with `rustexcel`-free, synthetic
    /// content by driving calamine's own writer? calamine has no writer, so the
    /// unit tests here exercise the typed-model mapping and the table/document
    /// routing directly on synthetic rows. The end-to-end round-trip through a
    /// real BIFF file is covered by `extract_real_xls_fixture` (gated on a
    /// generated fixture), so these pin the behaviour deterministically.
    fn row(num: u32, cells: &[Cell]) -> Row {
        Row {
            num,
            cells: cells.to_vec(),
        }
    }
    fn txt(col: u32, s: &str) -> Cell {
        Cell {
            col,
            value: Value::String(s.into()),
            kind: Kind::Text,
        }
    }
    fn num(col: u32, v: i64) -> Cell {
        Cell {
            col,
            value: Value::Number(v.into()),
            kind: Kind::Number,
        }
    }
    fn boolean(col: u32, v: bool) -> Cell {
        Cell {
            col,
            value: Value::Bool(v),
            kind: Kind::Bool,
        }
    }

    fn collect_table(sheet: &str, rows: &[Row]) -> (ExtractStats, Vec<RawRecord>) {
        let h = find_header(rows).expect("a header should be found");
        let mut recs = Vec::new();
        let mut stats = ExtractStats::default();
        emit_table(
            sheet,
            rows,
            h,
            None,
            &mut |r| {
                recs.push(r);
                true
            },
            &mut stats,
        );
        (stats, recs)
    }

    #[test]
    fn convert_cell_types_mirror_the_xlsx_model() {
        assert!(convert_cell(1, &Data::Empty).is_none());
        assert!(convert_cell(1, &Data::Error(calamine::CellErrorType::Div0)).is_none());
        // An integral float indexes as an integer, not 1250.0.
        assert_eq!(
            convert_cell(3, &Data::Float(1250.0)).unwrap().value,
            json!(1250)
        );
        assert_eq!(
            convert_cell(3, &Data::Float(980.5)).unwrap().value,
            json!(980.5)
        );
        assert_eq!(convert_cell(4, &Data::Bool(true)).unwrap().kind, Kind::Bool);
        // A whitespace-only string is dropped like an empty cell.
        assert!(convert_cell(1, &Data::String("   ".into())).is_none());
    }

    #[test]
    fn a_title_row_is_skipped_and_each_row_under_the_header_is_a_typed_record() {
        // Row 1 is a single-cell title; row 3 the header; 4-5 the data.
        let rows = vec![
            row(1, &[txt(1, "Q3 Sales Report")]),
            row(
                3,
                &[
                    txt(1, "Region"),
                    txt(2, "Month"),
                    txt(3, "Revenue"),
                    txt(4, "Closed"),
                ],
            ),
            row(
                4,
                &[
                    txt(1, "EMEA"),
                    txt(2, "July"),
                    num(3, 1250),
                    boolean(4, true),
                ],
            ),
            row(
                5,
                &[
                    txt(1, "APAC"),
                    txt(2, "August"),
                    num(3, 980),
                    boolean(4, false),
                ],
            ),
        ];
        let (stats, recs) = collect_table("Sales", &rows);
        assert_eq!((stats.records, stats.junk), (2, 0));
        assert_eq!(recs[0].group.as_deref(), Some("Sales"));
        assert_eq!(recs[0].locator, "Sales!r4");
        assert_eq!(recs[0].origin, FieldOrigin::Data);
        assert_eq!(
            fields(&recs[0]),
            json!({"Region": "EMEA", "Month": "July", "Revenue": 1250, "Closed": true})
        );
        assert_eq!(
            fields(&recs[1]),
            json!({"Region": "APAC", "Month": "August", "Revenue": 980, "Closed": false})
        );
    }

    #[test]
    fn a_repeated_header_row_is_not_a_data_record() {
        let rows = vec![
            row(1, &[txt(1, "Name"), txt(2, "Score")]),
            row(2, &[txt(1, "Ann"), num(2, 10)]),
            row(3, &[txt(1, "Name"), txt(2, "Score")]), // stacked header again
            row(4, &[txt(1, "Bob"), num(2, 20)]),
        ];
        let (stats, recs) = collect_table("S", &rows);
        assert_eq!(stats.records, 2);
        assert_eq!(fields(&recs[0]), json!({"Name": "Ann", "Score": 10}));
        assert_eq!(fields(&recs[1]), json!({"Name": "Bob", "Score": 20}));
    }

    #[test]
    fn duplicate_header_names_are_disambiguated() {
        let rows = vec![
            row(1, &[txt(1, "Amount"), txt(2, "Amount")]),
            row(2, &[num(1, 1), num(2, 2)]),
        ];
        let (_stats, recs) = collect_table("S", &rows);
        let obj = recs[0].fields.clone();
        assert!(obj.contains_key("Amount") && obj.contains_key("Amount_2"));
    }

    #[test]
    fn a_sheet_with_no_header_is_emitted_as_a_document() {
        // Every row leads with a number, so no row is a header candidate.
        let rows = vec![
            row(1, &[num(1, 1), txt(2, "alpha")]),
            row(2, &[num(1, 2), txt(2, "beta")]),
        ];
        assert!(find_header(&rows).is_none());
        let mut recs = Vec::new();
        let mut stats = ExtractStats::default();
        emit_document_sheet(
            "Notes",
            "book",
            &rows,
            &mut |r| {
                recs.push(r);
                true
            },
            &mut stats,
        );
        assert_eq!(stats.records, 1);
        let body = recs[0].fields["body"].as_str().unwrap();
        assert!(body.contains("1 | alpha") && body.contains("2 | beta"));
        assert_eq!(recs[0].fields["title"], "book — Notes");
        assert_eq!(recs[0].fields["sheet"], "Notes");
    }

    #[test]
    fn date_cell_formats_match_the_xlsx_extractor() {
        // calamine resolves an ExcelDateTime; check the bare-date vs datetime
        // split against a serial we can predict (1904-01-01 is serial 1462 in
        // the 1900 system; use a known midnight serial instead).
        use calamine::ExcelDateTime;
        // 2025-07-01 00:00:00 — serial 45839 in the 1900 date system.
        let d = Data::DateTime(ExcelDateTime::new(
            45839.0,
            calamine::ExcelDateTimeType::DateTime,
            false,
        ));
        let cell = date_cell(1, &d).unwrap();
        assert_eq!(cell.kind, Kind::Date);
        assert_eq!(cell.value, json!("2025-07-01"));
    }

    fn rec(locator: &str) -> RawRecord {
        let mut fields = Map::new();
        fields.insert("Region".into(), Value::String("EMEA".into()));
        fields.insert("Revenue".into(), Value::Number(1250.into()));
        RawRecord {
            fields,
            locator: locator.to_string(),
            group: Some("Sales".into()),
            origin: FieldOrigin::Data,
        }
    }

    /// The parent reconstructs the worker's records byte-for-byte: a `Rec` line
    /// round-trips every field, the locator, the group and the origin, and the
    /// `End` line is not mistaken for a record.
    #[test]
    fn worker_line_rec_and_end_round_trip() {
        let line = serde_json::to_string(&WorkerLine::Rec(rec("Sales!r4"))).unwrap();
        // A record line carries no discriminator a RawRecord could own.
        assert!(line.contains("\"k\":\"rec\""));
        match serde_json::from_str::<WorkerLine>(&line).unwrap() {
            WorkerLine::Rec(r) => {
                assert_eq!(r.locator, "Sales!r4");
                assert_eq!(r.group.as_deref(), Some("Sales"));
                assert_eq!(r.origin, FieldOrigin::Data);
                assert_eq!(r.fields["Region"], json!("EMEA"));
                assert_eq!(r.fields["Revenue"], json!(1250));
            }
            WorkerLine::End { .. } => panic!("record line decoded as terminator"),
        }
        let end = serde_json::to_string(&WorkerLine::End { truncated: true }).unwrap();
        assert!(matches!(
            serde_json::from_str::<WorkerLine>(&end).unwrap(),
            WorkerLine::End { truncated: true }
        ));
    }

    /// The NDJSON parser rebuilds the record list and the truncation flag, and
    /// tolerates the blank splits a trailing newline produces.
    #[cfg(unix)]
    #[test]
    fn parse_worker_output_reconstructs_records_and_truncation() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            serde_json::to_string(&WorkerLine::Rec(rec("Sales!r4")))
                .unwrap()
                .as_bytes(),
        );
        buf.push(b'\n');
        buf.extend_from_slice(
            serde_json::to_string(&WorkerLine::Rec(rec("Sales!r5")))
                .unwrap()
                .as_bytes(),
        );
        buf.push(b'\n');
        buf.extend_from_slice(
            serde_json::to_string(&WorkerLine::End { truncated: true })
                .unwrap()
                .as_bytes(),
        );
        buf.push(b'\n');
        let (records, truncated) = parse_worker_output(&buf).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].locator, "Sales!r4");
        assert_eq!(records[1].locator, "Sales!r5");
        assert!(truncated);
    }

    /// A stream cut short before the terminator — exactly the shape of a worker
    /// that crashed mid-emit after flushing a record — is an error, so the
    /// parent discards the partial file rather than indexing half of it.
    #[cfg(unix)]
    #[test]
    fn parse_worker_output_rejects_a_missing_terminator() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            serde_json::to_string(&WorkerLine::Rec(rec("Sales!r4")))
                .unwrap()
                .as_bytes(),
        );
        buf.push(b'\n');
        assert!(parse_worker_output(&buf).is_err());
        // Garbage that is not our protocol is also rejected.
        assert!(parse_worker_output(b"{not json}\n").is_err());
        // A clean empty run (no rows, just a terminator) is valid and empty.
        let end = serde_json::to_string(&WorkerLine::End { truncated: false }).unwrap();
        let (records, truncated) = parse_worker_output(end.as_bytes()).unwrap();
        assert!(records.is_empty() && !truncated);
    }
}
