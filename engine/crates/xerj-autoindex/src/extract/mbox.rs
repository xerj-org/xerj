//! mbox — a mailbox: many RFC 5322 messages concatenated in one file.
//!
//! Nobody has a folder of `.eml` files. What people actually hold is ONE very
//! large file: a Google Takeout export (`All mail Including Spam and
//! Trash.mbox`, routinely many GB), a Thunderbird folder, an Apple Mail
//! `mbox`, a mutt/Dovecot spool. This module splits that file into messages
//! and hands each one to [`super::eml::emit_message`] — the same code a
//! standalone `.eml` goes through — so the message and attachment records are
//! identical in shape, and only the locator is namespaced (`m{offset}-msg-s0`).
//!
//! ## The format, and the parts of it that bite
//!
//! A message starts at a *separator line* — `From <sender> <asctime date>` —
//! and runs to the next one. Because a body line can itself begin with
//! `From `, writers quote such lines: **mboxo** turns `From ` into `>From `,
//! **mboxrd** additionally turns `>From ` into `>>From ` so the quoting is
//! reversible. Reading undoes exactly one level: a line matching `^>+From `
//! loses its first `>`.
//!
//! The reference read for that rule is mail-parser's own
//! `mailbox::mbox::MessageIterator` (`mail-parser-0.11.9/src/mailbox/mbox.rs`,
//! Apache-2.0 OR MIT — already a dependency of this crate). Its unquoting is
//! at `mbox.rs:75-80` and ours is the same rule. It is NOT used here, and
//! nothing is copied from it, for four reasons that are each visible in that
//! file: it buffers every line whole with `read_until` (`:49`), so one
//! newline-free binary part is held in memory at once; its `Message` carries
//! no byte offset (`:21-25`), which is what our locators are made of; message
//! contents grow without a cap (`:70`, `:80`, `:82`); and ANY line starting
//! `From ` is a separator (`:55`) — see the next paragraph for why that one
//! is a correctness bug on real mailboxes rather than a style difference.
//!
//! A separator here must carry an asctime-shaped date (`Mon Jan  1 00:00:00
//! 2024`, optionally with a zone token, which is what Gmail Takeout writes).
//! That is deliberate: **mboxcl/mboxcl2** writers do not quote at all, so an
//! unquoted body paragraph opening "From what I understand…" is legal in those
//! files, and treating it as a separator would cut a message in half and index
//! the second half as a headerless ghost. Requiring the date shape costs
//! nothing on real separators — Takeout, Thunderbird (`From - Tue Oct …`),
//! Apple Mail, mutt, Dovecot and Python's `mailbox` all write it.
//!
//! ## Memory
//!
//! Multi-GB files are the normal case, so nothing here is proportional to the
//! file. The reader holds one buffered chunk, at most [`HEAD_CAP`] bytes of
//! the current line while it decides what the line is, and the ONE message
//! being assembled — itself capped at [`super::eml::MAX_EML`]. A line of any
//! length (binary `Content-Transfer-Encoding` has no line structure at all) is
//! streamed through in chunks; it is never materialised.
//!
//! ## Parallelism
//!
//! Phase B runs one worker per FILE, and a Takeout mailbox is one file, so
//! before this every message of a multi-GB export was extracted on ONE
//! thread — including one PDF parser subprocess per attached PDF — while the
//! other cores idled (measured: ~4.7 MB/s on the 1 GB synthetic Takeout, the
//! figures are in `benchmarks/mbox-ingest/README.md`). Splitting is cheap
//! and inherently serial; parsing is not. So the splitter runs on its own
//! thread and hands each message to a pool that runs [`emit_message`], and
//! the calling thread forwards the results to the sink IN MESSAGE ORDER, so
//! the record stream is byte-for-byte what the sequential path produces (a
//! test pins that). Memory stays bounded by [`IN_FLIGHT_BUDGET`], which is
//! process-wide like the slots below: a splitter waits before reading a
//! message that would push the bytes held across every mailbox's pool and
//! reorder buffer over it, and a single message
//! larger than the whole budget is admitted alone. Phase-A sampling (a byte
//! limit) stays sequential so that what gets sampled is a function of the
//! bytes, not of scheduling. Slots are process-wide (`configure_parallelism`,
//! set from `--workers`), so ten mailboxes in one tree share the cores
//! instead of each taking all of them.
//!
//! ## The record cap (#381)
//!
//! `MAX_RECORDS_PER_FILE` (4096) bounds what ONE DOCUMENT may emit, so that a
//! misclassified magic-less payload cannot become thousands of noise records.
//! It is applied here per MESSAGE — each body and each attachment is capped by
//! `emit_message` exactly as in a standalone `.eml` — and deliberately NOT to
//! the mbox as a whole: a mailbox is a container of documents, like a JSONL
//! file (which has no cap either), and a real one legitimately holds far more
//! than 4096 records. Capping the file would index the first few thousand
//! messages of a 100k-message mailbox and drop the rest. The threat the cap
//! exists for stays bounded: a payload that merely *opens* like an mbox and
//! contains no further separators is one message, and one message is capped.
//!
//! ## Panic safety
//!
//! This crate builds with `panic = "abort"`, and mailboxes are the definition
//! of untrusted input. Splitting works on BYTES throughout — a `&str` is never
//! indexed by byte offset, there is no `unwrap` on parsed input, and every
//! slice bound is derived from a `len()` or a `position()` of the same buffer.

use super::eml::{emit_message, MessageEnvelope, MessageOutcome, MAX_EML};
use super::{emit_document_with_fields, open_reader, ExtractStats, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};

/// Bytes of a line held back while deciding whether it is a separator or a
/// quoted `>From `. A real separator is well under 200 bytes; a line that is
/// still going after this many bytes is body, whatever it starts with.
const HEAD_CAP: usize = 1024;

/// UTF-8 byte-order mark.
pub(crate) const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// One message cut out of a mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    /// Byte offset of this message's separator line in the (decompressed)
    /// mailbox stream. Deterministic for given bytes, unique within the file —
    /// the locator ingredient, and the seek target for "open the original".
    pub offset: u64,
    /// The separator line, lossily decoded, without its line ending.
    pub from_line: String,
    /// The message, with one level of `>From ` quoting undone and the
    /// container's trailing blank line removed. Holds at most the cap.
    pub bytes: Vec<u8>,
    /// Size of the message in the mailbox, which is what `bytes.len()` would
    /// have been without the cap.
    pub total_len: u64,
    /// The message was over the cap and `bytes` is only its head.
    capped: bool,
}

impl RawMessage {
    /// The message was larger than the splitter's cap; `bytes` is its head.
    pub fn oversized(&self) -> bool {
        self.capped
    }
}

/// Streaming mbox splitter. `next_message` yields one message at a time.
pub struct Splitter<R> {
    reader: R,
    /// Offset of the next unread byte.
    offset: u64,
    head: Vec<u8>,
    cur: Option<RawMessage>,
    cap: usize,
    /// Bytes that preceded the first separator (or every byte, in a file with
    /// no separator at all). Not part of any message; reported, never indexed.
    pub preamble_bytes: u64,
    /// The reader is exhausted; only the message in flight is left to yield.
    eof: bool,
}

enum Head {
    /// The whole line, including its `\n`, is in `head`.
    Complete,
    /// `HEAD_CAP` bytes are in `head` and the line is still going.
    Partial,
    /// Input ended. `head` holds a final unterminated line, or nothing.
    Eof,
}

impl<R: BufRead> Splitter<R> {
    pub fn new(reader: R) -> Self {
        Self::with_cap(reader, MAX_EML as usize)
    }

    /// `cap` bounds the bytes retained per message (tests shrink it).
    pub fn with_cap(reader: R, cap: usize) -> Self {
        Self {
            reader,
            offset: 0,
            head: Vec::with_capacity(HEAD_CAP),
            cur: None,
            cap,
            preamble_bytes: 0,
            eof: false,
        }
    }

    /// Bytes consumed from the reader so far.
    pub fn consumed(&self) -> u64 {
        self.offset
    }

    fn read_head(&mut self) -> std::io::Result<Head> {
        self.head.clear();
        loop {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(Head::Eof);
            }
            let room = HEAD_CAP - self.head.len();
            let window = &buf[..buf.len().min(room)];
            match memchr::memchr(b'\n', window) {
                Some(i) => {
                    self.head.extend_from_slice(&window[..=i]);
                    self.reader.consume(i + 1);
                    return Ok(Head::Complete);
                }
                None => {
                    let n = window.len();
                    self.head.extend_from_slice(window);
                    self.reader.consume(n);
                    if self.head.len() >= HEAD_CAP {
                        return Ok(Head::Partial);
                    }
                }
            }
        }
    }

    /// Append to the message in flight, never past the cap. `total_len` keeps
    /// counting so an oversized message is reported with its real size.
    fn push(cur: &mut RawMessage, cap: usize, bytes: &[u8]) {
        cur.total_len += bytes.len() as u64;
        let room = cap.saturating_sub(cur.bytes.len());
        if bytes.len() > room {
            cur.capped = true;
        }
        if room > 0 {
            cur.bytes.extend_from_slice(&bytes[..bytes.len().min(room)]);
        }
    }

    /// Stream the remainder of an over-long line (everything up to and
    /// including its `\n`) into the message in flight, chunk by chunk.
    fn drain_line(&mut self) -> std::io::Result<()> {
        loop {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(());
            }
            let (take, done) = match memchr::memchr(b'\n', buf) {
                Some(i) => (i + 1, true),
                None => (buf.len(), false),
            };
            match self.cur.as_mut() {
                Some(cur) => Self::push(cur, self.cap, &buf[..take]),
                None => self.preamble_bytes += take as u64,
            }
            self.offset += take as u64;
            self.reader.consume(take);
            if done {
                return Ok(());
            }
        }
    }

    fn finish(mut msg: RawMessage) -> RawMessage {
        // Writers append one blank line after every message; it belongs to the
        // container, not to the message. The message's own final newline stays.
        // Only when nothing was cut: a capped message does not end where it ends.
        if !msg.oversized() {
            let strip = if msg.bytes.ends_with(b"\r\n\r\n") {
                2
            } else if msg.bytes.ends_with(b"\n\n") {
                1
            } else {
                0
            };
            msg.bytes.truncate(msg.bytes.len() - strip);
            msg.total_len -= strip as u64;
        }
        msg
    }

    /// The next message, or `None` at end of input.
    pub fn next_message(&mut self) -> std::io::Result<Option<RawMessage>> {
        if self.eof {
            return Ok(self.cur.take().map(Self::finish));
        }
        loop {
            let line_start = self.offset;
            let kind = self.read_head()?;
            self.offset += self.head.len() as u64;
            let complete = !matches!(kind, Head::Partial);
            if matches!(kind, Head::Eof) && self.head.is_empty() {
                self.eof = true;
                return Ok(self.cur.take().map(Self::finish));
            }
            // A UTF-8 byte-order mark in front of the FIRST separator. Some
            // Windows tools write one when they save or convert a mailbox, and
            // with it the first line is not `From …`: the whole first message
            // was preamble, and `sniff` junked the file before it got here
            // (review finding on PR #949). Dropped at offset 0 only — anywhere
            // else `EF BB BF` is three bytes of somebody's text.
            if line_start == 0 && self.head.starts_with(UTF8_BOM) {
                self.head.drain(..UTF8_BOM.len());
            }
            if complete && is_from_line(&self.head) {
                let done = self.cur.take().map(Self::finish);
                self.cur = Some(RawMessage {
                    offset: line_start,
                    from_line: String::from_utf8_lossy(trim_eol(&self.head)).into_owned(),
                    bytes: Vec::new(),
                    total_len: 0,
                    capped: false,
                });
                if matches!(kind, Head::Eof) {
                    self.eof = true;
                }
                if done.is_some() {
                    return Ok(done);
                }
                if self.eof {
                    return Ok(self.cur.take().map(Self::finish));
                }
                continue;
            }
            match self.cur.as_mut() {
                Some(cur) => {
                    // mboxrd/mboxo: `^>+From ` loses exactly one `>`.
                    let skip = usize::from(is_quoted_from(&self.head));
                    Self::push(cur, self.cap, &self.head[skip..]);
                }
                None => self.preamble_bytes += self.head.len() as u64,
            }
            match kind {
                Head::Partial => self.drain_line()?,
                Head::Eof => {
                    self.eof = true;
                    return Ok(self.cur.take().map(Self::finish));
                }
                Head::Complete => {}
            }
        }
    }
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// `^>+From ` — a body line a writer quoted so it would not read as a separator.
fn is_quoted_from(line: &[u8]) -> bool {
    let gt = line.iter().take_while(|b| **b == b'>').count();
    gt > 0 && line[gt..].starts_with(b"From ")
}

const WEEKDAYS: [&[u8]; 7] = [b"mon", b"tue", b"wed", b"thu", b"fri", b"sat", b"sun"];
const MONTHS: [&[u8]; 12] = [
    b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov", b"dec",
];

fn token_in(tok: &[u8], set: &[&[u8]]) -> Option<usize> {
    // `Mon,` is tolerated; some writers keep the RFC 5322 comma.
    let tok = tok.strip_suffix(b",").unwrap_or(tok);
    set.iter().position(|w| tok.eq_ignore_ascii_case(w))
}

fn ascii_number(tok: &[u8]) -> Option<u32> {
    if tok.is_empty() || tok.len() > 9 || !tok.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // All-ASCII-digit and at most 9 bytes: valid UTF-8 and fits a u32.
    std::str::from_utf8(tok).ok()?.parse().ok()
}

/// `H:MM` or `HH:MM:SS`.
fn parse_time(tok: &[u8]) -> Option<(u32, u32, u32)> {
    let mut parts = tok.split(|b| *b == b':');
    let h = ascii_number(parts.next()?)?;
    let m = ascii_number(parts.next()?)?;
    let s = match parts.next() {
        Some(p) => ascii_number(p)?,
        None => 0,
    };
    (parts.next().is_none() && h < 24 && m < 60 && s < 62).then_some((h, m, s))
}

/// `+0000` / `-0700` → seconds east of UTC.
fn parse_zone(tok: &[u8]) -> Option<i32> {
    let (sign, digits) = match tok.split_first()? {
        (b'+', rest) => (1, rest),
        (b'-', rest) => (-1, rest),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let hh = ascii_number(&digits[..2])? as i32;
    let mm = ascii_number(&digits[2..])? as i32;
    (hh < 24 && mm < 60).then_some(sign * (hh * 3600 + mm * 60))
}

/// The date carried by a separator line, as (naive local time, zone offset in
/// seconds when the line names a numeric one).
fn from_line_date(line: &[u8]) -> Option<(chrono::NaiveDateTime, Option<i32>)> {
    let rest = line.strip_prefix(b"From ")?;
    let toks: Vec<&[u8]> = trim_eol(rest)
        .split(|b| b.is_ascii_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    // weekday month day time [zone] year — anywhere after `From `, because the
    // sender token is free-form (`-`, `MAILER-DAEMON`, `1699…@xxx`, or absent).
    for i in 0..toks.len() {
        if token_in(toks[i], &WEEKDAYS).is_none() || i + 4 >= toks.len() {
            continue;
        }
        let Some(month) = token_in(toks[i + 1], &MONTHS) else {
            continue;
        };
        let Some(day) = ascii_number(toks[i + 2]) else {
            continue;
        };
        let Some((h, m, s)) = parse_time(toks[i + 3]) else {
            continue;
        };
        let mut zone = None;
        for tok in &toks[i + 4..] {
            if let Some(z) = parse_zone(tok) {
                zone = Some(z);
                continue;
            }
            let Some(year) = ascii_number(tok).filter(|y| (1970..=9999).contains(y)) else {
                continue;
            };
            let date = chrono::NaiveDate::from_ymd_opt(year as i32, month as u32 + 1, day)?;
            // A leap second (`:60`) is legal in a date and not in chrono.
            let time = date.and_hms_opt(h, m, s.min(59))?;
            return Some((time, zone));
        }
    }
    None
}

/// Is this line an mbox separator? `From ` plus an asctime-shaped date — see
/// the module docs for why the date is required.
pub fn is_from_line(line: &[u8]) -> bool {
    from_line_date(line).is_some()
}

/// RFC 3339 form of a separator line's date, in UTC. A zone-less asctime date
/// is taken as UTC — the line does not say otherwise, and it is only ever a
/// fallback for a message with no `Date:` header of its own.
fn from_line_rfc3339(line: &[u8]) -> Option<String> {
    let (naive, zone) = from_line_date(line)?;
    let utc = naive.checked_sub_signed(chrono::Duration::seconds(i64::from(zone.unwrap_or(0))))?;
    Some(
        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(utc, chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// The byte offset a record's locator carries: `m{offset}-msg-s0` → `offset`.
/// `None` for any locator this module did not write (`msg-s0` of a standalone
/// `.eml` starts with `m` too, and is not an offset).
///
/// Lives next to the code that WRITES the prefix (`format!("m{}-", msg.offset)`
/// in [`extract_from`]) so the two cannot drift. It is how Phase B reports
/// progress INSIDE a mailbox: each record says how far into the file its
/// message began, which costs nothing and needs no channel from the splitter.
///
/// Byte-safe: the prefix is ASCII (`m`, digits, `-`), and `strip_prefix` /
/// `split_once` only ever cut at the ASCII bytes they matched.
pub fn locator_offset(locator: &str) -> Option<u64> {
    let (digits, _rest) = locator.strip_prefix('m')?.split_once('-')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Extract every message of the mailbox at `path`.
///
/// `limit_bytes` bounds a SAMPLING read (phase A): splitting stops once that
/// many bytes have been consumed. `None` streams the whole file.
pub fn extract(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    // No `take()` on the reader: a byte limit that cut the stream mid-message
    // would hand the MIME parser a torso. The limit is checked BETWEEN
    // messages instead, so every sampled message is whole.
    let reader = open_reader(path, gzip, None)?;
    extract_from(reader, limit_bytes, sink)
}

/// Bytes of messages the PROCESS may hold in flight across every open
/// mailbox's worker pool and reorder buffer (see the module docs,
/// "Parallelism", and [`shared_budget`]). One message over this is admitted on
/// its own, so a 64 MB message (the `MAX_EML` cap) never deadlocks the
/// pipeline; it just runs alone.
pub const IN_FLIGHT_BUDGET: usize = 256 << 20;

/// Message-extraction slots shared by every mailbox open in this process.
/// Set once per run from `--workers` (the Phase-B width). The default only
/// covers code paths that never call it: unit tests and one-shot probes.
pub fn configure_parallelism(workers: usize) {
    gate().set_limit(workers.max(1));
}

/// Slots currently configured — what one mailbox may use at most.
pub fn parallelism() -> usize {
    gate().limit()
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate::new(xerj_common::resource::cores().clamp(1, 8)))
}

/// A counting gate: `limit` permits, taken one per message being parsed.
struct Gate {
    /// (in use, limit)
    state: Mutex<(usize, usize)>,
    ready: Condvar,
}

impl Gate {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new((0, limit.max(1))),
            ready: Condvar::new(),
        }
    }

    fn set_limit(&self, limit: usize) {
        self.state.lock().expect("mbox gate poisoned").1 = limit.max(1);
        self.ready.notify_all();
    }

    fn limit(&self) -> usize {
        self.state.lock().expect("mbox gate poisoned").1
    }

    fn acquire(&self) -> GatePermit<'_> {
        let mut state = self.state.lock().expect("mbox gate poisoned");
        while state.0 >= state.1 {
            state = self.ready.wait(state).expect("mbox gate poisoned");
        }
        state.0 += 1;
        GatePermit(self)
    }
}

struct GatePermit<'a>(&'a Gate);

impl Drop for GatePermit<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("mbox gate poisoned");
        state.0 = state.0.saturating_sub(1);
        self.0.ready.notify_one();
    }
}

/// Bytes admitted into the pipeline. `acquire` blocks while the request would
/// overflow the cap AND something is already in flight; with nothing in
/// flight any size goes through, which is what keeps an oversized message
/// from waiting forever.
struct Budget {
    used: Mutex<usize>,
    freed: Condvar,
    cap: usize,
}

impl Budget {
    fn new(cap: usize) -> Self {
        Self {
            used: Mutex::new(0),
            freed: Condvar::new(),
            cap,
        }
    }

    fn acquire(&self, bytes: usize) {
        let mut used = self.used.lock().expect("mbox budget poisoned");
        while *used > 0 && used.saturating_add(bytes) > self.cap {
            used = self.freed.wait(used).expect("mbox budget poisoned");
        }
        *used = used.saturating_add(bytes);
    }

    fn release(&self, bytes: usize) {
        let mut used = self.used.lock().expect("mbox budget poisoned");
        *used = used.saturating_sub(bytes);
        drop(used);
        self.freed.notify_all();
    }
}

/// THE byte budget: one for the process, like the parse gate. It used to be one
/// per `extract_parallel` call, which bounded a MAILBOX, not the run — Phase B
/// reads up to `--workers` files at once, so a tree of twelve Thunderbird
/// folders full of large attachments held twelve budgets (measured by the
/// review of PR #949: 1.71 GB of client RSS against the ~0.3 GB documented for
/// the one-mailbox Takeout case).
fn shared_budget() -> &'static Budget {
    static BUDGET: OnceLock<Budget> = OnceLock::new();
    BUDGET.get_or_init(|| Budget::new(IN_FLIGHT_BUDGET))
}

/// One mailbox's claim on a shared [`Budget`]. Whatever it still holds when
/// the mailbox is done with — a splitter error, a sink that said stop, a job
/// that could not be handed over — goes back on drop, so no exit path of one
/// mailbox can shrink the budget for every mailbox after it.
struct Lease<'a> {
    budget: &'a Budget,
    held: Mutex<usize>,
}

impl<'a> Lease<'a> {
    fn new(budget: &'a Budget) -> Self {
        Self {
            budget,
            held: Mutex::new(0),
        }
    }

    fn acquire(&self, bytes: usize) {
        self.budget.acquire(bytes);
        let mut held = self.held.lock().expect("mbox lease poisoned");
        *held = held.saturating_add(bytes);
    }

    fn release(&self, bytes: usize) {
        let bytes = {
            let mut held = self.held.lock().expect("mbox lease poisoned");
            let bytes = bytes.min(*held);
            *held -= bytes;
            bytes
        };
        self.budget.release(bytes);
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let rest = std::mem::take(&mut *self.held.lock().unwrap_or_else(|p| p.into_inner()));
        if rest > 0 {
            self.budget.release(rest);
        }
    }
}

struct Job {
    seq: u64,
    msg: RawMessage,
}

enum Done {
    Message {
        seq: u64,
        bytes: usize,
        records: Vec<RawRecord>,
        stats: ExtractStats,
    },
    /// The splitter failed after handing out `seq` messages.
    Error { seq: u64, error: std::io::Error },
    /// The splitter is done; `total` messages were handed out.
    End { total: u64 },
}

/// Emit one split-out message through `sink`. Returns the sink's last answer
/// (`false` = stop). Shared by the sequential and the parallel path so the
/// two cannot drift.
fn emit_raw_message(msg: &RawMessage, sink: Sink, stats: &mut ExtractStats) -> bool {
    let prefix = format!("m{}-", msg.offset);
    if msg.oversized() {
        // Over the per-message cap: the head is parsed (headers, body and
        // whatever attachments fit), the tail is not. Reported, never
        // silent — the run names this file as truncated.
        stats.truncated = true;
    }
    let env = MessageEnvelope {
        loc_prefix: &prefix,
        fallback_date: from_line_rfc3339(msg.from_line.as_bytes()),
    };
    match emit_message(&msg.bytes, &env, sink, stats) {
        MessageOutcome::Emitted { alive } => alive,
        MessageOutcome::Unparseable => emit_unparseable(msg, &prefix, &env, sink, stats),
    }
}

fn extract_from<R: BufRead + Send>(
    reader: R,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    // Sampling stays sequential: it stops at a byte limit, and which
    // messages it saw must be a function of the bytes, not of scheduling.
    let workers = if limit_bytes.is_some() {
        1
    } else {
        parallelism()
    };
    if workers <= 1 {
        extract_sequential(reader, limit_bytes, sink)
    } else {
        extract_parallel(reader, sink, workers)
    }
}

fn extract_sequential<R: BufRead>(
    reader: R,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let mut split = Splitter::new(reader);
    while let Some(msg) = split.next_message()? {
        if !emit_raw_message(&msg, sink, &mut stats) {
            break;
        }
        if limit_bytes.is_some_and(|limit| split.consumed() >= limit) {
            break;
        }
    }
    Ok(stats)
}

/// One message, parsed on a pool thread into a local buffer.
fn extract_one(msg: &RawMessage) -> (Vec<RawRecord>, ExtractStats) {
    let mut stats = ExtractStats::default();
    let mut records = Vec::new();
    let mut sink = |r: RawRecord| {
        records.push(r);
        true
    };
    emit_raw_message(msg, &mut sink, &mut stats);
    (records, stats)
}

/// The pipeline described in the module docs: splitter thread → `workers`
/// parsers → this thread, which forwards records in message order.
///
/// Why it cannot deadlock: the forwarding thread blocks only on the result
/// channel; a parser blocks only on the job channel (closed by the splitter
/// when it ends) and on the process gate (released by other parsers); the
/// splitter blocks on the budget and on the job channel, both released by
/// the forwarding thread and the parsers respectively. When the sink says
/// stop, the splitter sees the flag on its next message, and this thread
/// keeps draining results — without forwarding — until the splitter's `End`
/// arrives, so nothing is left blocked on a channel this thread has dropped.
fn extract_parallel<R: BufRead + Send>(
    reader: R,
    sink: Sink,
    workers: usize,
) -> Result<ExtractStats> {
    let (job_tx, job_rx) = mpsc::sync_channel::<Job>(workers);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    let stop = AtomicBool::new(false);
    let budget = Lease::new(shared_budget());
    let mut stats = ExtractStats::default();
    let mut failure: Option<std::io::Error> = None;

    std::thread::scope(|scope| {
        let splitter_tx = done_tx.clone();
        let (stop_ref, budget_ref) = (&stop, &budget);
        scope.spawn(move || {
            let mut split = Splitter::new(reader);
            let mut seq = 0u64;
            loop {
                if stop_ref.load(Ordering::Relaxed) {
                    break;
                }
                match split.next_message() {
                    Ok(Some(msg)) => {
                        budget_ref.acquire(msg.bytes.len());
                        if let Err(mpsc::SendError(job)) = job_tx.send(Job { seq, msg }) {
                            // Never handed over, so nobody will release it.
                            budget_ref.release(job.msg.bytes.len());
                            break;
                        }
                        seq += 1;
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = splitter_tx.send(Done::Error { seq, error });
                        break;
                    }
                }
            }
            let _ = splitter_tx.send(Done::End { total: seq });
            // `job_tx` drops here: the parsers drain what is queued and exit.
        });
        for _ in 0..workers {
            let job_rx = Arc::clone(&job_rx);
            let done_tx = done_tx.clone();
            scope.spawn(move || loop {
                // Hold the lock only while waiting for a job, never while
                // parsing one: the other parsers must be able to take theirs.
                let job = match job_rx.lock() {
                    Ok(rx) => rx.recv(),
                    Err(_) => return,
                };
                let Ok(Job { seq, msg }) = job else { return };
                let _slot = gate().acquire();
                let bytes = msg.bytes.len();
                let (records, stats) = extract_one(&msg);
                drop(msg);
                if done_tx
                    .send(Done::Message {
                        seq,
                        bytes,
                        records,
                        stats,
                    })
                    .is_err()
                {
                    return;
                }
            });
        }
        drop(done_tx);

        let mut next = 0u64;
        let mut total: Option<u64> = None;
        let mut pending: BTreeMap<u64, (usize, Vec<RawRecord>, ExtractStats)> = BTreeMap::new();
        let mut alive = true;
        loop {
            if total.is_some_and(|t| next >= t) {
                break;
            }
            let Ok(done) = done_rx.recv() else { break };
            match done {
                Done::End { total: t } => total = Some(t),
                Done::Error { seq, error } => {
                    failure = Some(error);
                    total = Some(seq);
                }
                Done::Message {
                    seq,
                    bytes,
                    records,
                    stats,
                } => {
                    pending.insert(seq, (bytes, records, stats));
                }
            }
            while let Some((bytes, records, s)) = pending.remove(&next) {
                next += 1;
                budget.release(bytes);
                if !alive {
                    continue;
                }
                stats.records += s.records;
                stats.junk += s.junk;
                stats.truncated |= s.truncated;
                for record in records {
                    if !sink(record) {
                        alive = false;
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        }
    });
    match failure {
        Some(error) => Err(error.into()),
        None => Ok(stats),
    }
}

/// A mailbox entry the MIME parser refused outright. Its text is still in the
/// user's mailbox and still worth finding, so it is indexed as a plain
/// document rather than dropped; an entry with no text at all is junk.
fn emit_unparseable(
    msg: &RawMessage,
    prefix: &str,
    env: &MessageEnvelope<'_>,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    let (text, _) = crate::sniff::decode_text(&msg.bytes);
    let text = text.trim();
    if text.is_empty() {
        stats.junk += 1;
        return true;
    }
    let mut fields = Map::new();
    if let Some(date) = &env.fallback_date {
        fields.insert("email_date".into(), Value::String(date.clone()));
    }
    emit_document_with_fields(
        &fields,
        "(unparseable message)",
        text,
        &format!("{prefix}raw"),
        sink,
        stats,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::RawRecord;
    use std::io::{BufReader, Cursor};

    fn split_all(data: &[u8]) -> Vec<RawMessage> {
        split_with(data, 1 << 20, 256 << 10)
    }

    /// `buf` is the BufReader capacity: tests shrink it so that lines, heads
    /// and separators straddle chunk boundaries.
    fn split_with(data: &[u8], cap: usize, buf: usize) -> Vec<RawMessage> {
        let mut s = Splitter::with_cap(BufReader::with_capacity(buf, Cursor::new(data)), cap);
        let mut out = Vec::new();
        while let Some(m) = s.next_message().unwrap() {
            out.push(m);
        }
        out
    }

    fn records(data: &[u8]) -> (Vec<RawRecord>, ExtractStats) {
        let mut out = Vec::new();
        let mut sink = |r: RawRecord| {
            out.push(r);
            true
        };
        let stats = extract_from(
            BufReader::with_capacity(4096, Cursor::new(data)),
            None,
            &mut sink,
        )
        .unwrap();
        (out, stats)
    }

    fn field<'a>(r: &'a RawRecord, k: &str) -> Option<&'a str> {
        r.fields.get(k).and_then(|v| v.as_str())
    }

    const SEP_A: &str = "From alice@example.org Mon Jan  1 10:00:00 2024";
    const SEP_GMAIL: &str = "From 1787654321098765432@xxx Tue Nov 14 22:13:20 +0000 2023";
    const SEP_TBIRD: &str = "From - Tue Oct 10 12:34:56 2023";

    fn msg(id: &str, subject: &str, body: &str) -> String {
        format!(
            "From: Alice <alice@example.org>\nTo: bob@example.org\nSubject: {subject}\n\
             Date: Mon, 1 Jan 2024 10:00:00 +0000\nMessage-ID: <{id}>\n\n{body}\n"
        )
    }

    #[test]
    fn separator_lines_from_real_writers_are_recognised() {
        for sep in [
            SEP_A,
            SEP_GMAIL,
            SEP_TBIRD,
            "From MAILER-DAEMON Fri Jul  8 12:08:34 2011",
            "From bob@example.org  Sat Jan  3 01:05:34 EST 1996",
            "From ???@??? Mon Jan 01 00:00:00 2024",
            "From carol@example.org Mon, Jan  1 9:05 2024 remote from relay",
        ] {
            assert!(is_from_line(sep.as_bytes()), "{sep}");
            assert!(is_from_line(format!("{sep}\r\n").as_bytes()), "{sep} CRLF");
        }
    }

    /// The reason the date shape is required: mboxcl2 does not quote, so these
    /// are all legal BODY lines, and none of them may cut a message in two.
    #[test]
    fn prose_that_starts_with_from_is_not_a_separator() {
        for line in [
            "From what I understand, the deal closes Monday.",
            "From here on it gets worse",
            "From ",
            "From",
            "From: Alice <alice@example.org>",
            "From alice@example.org",
            "From alice@example.org Mon Jan 32 10:00:00 2024",
            "From alice@example.org Mon Jan  1 25:00:00 2024",
            "From alice@example.org Mon Foo  1 10:00:00 2024",
            ">From alice@example.org Mon Jan  1 10:00:00 2024",
            "from alice@example.org Mon Jan  1 10:00:00 2024",
        ] {
            assert!(!is_from_line(line.as_bytes()), "{line}");
        }
    }

    #[test]
    fn separator_dates_become_utc_rfc3339() {
        assert_eq!(
            from_line_rfc3339(SEP_GMAIL.as_bytes()).as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(
            from_line_rfc3339(b"From a@b Mon Jan  1 10:00:00 -0700 2024").as_deref(),
            Some("2024-01-01T17:00:00Z")
        );
        // Zone-less asctime: taken as UTC.
        assert_eq!(
            from_line_rfc3339(SEP_A.as_bytes()).as_deref(),
            Some("2024-01-01T10:00:00Z")
        );
        // A leap second is legal in a date and must not panic or be refused.
        assert_eq!(
            from_line_rfc3339(b"From a@b Sat Dec 31 23:59:60 2016").as_deref(),
            Some("2016-12-31T23:59:59Z")
        );
        // Feb 30 has the right SHAPE and no such day: not a date, no panic.
        assert_eq!(
            from_line_rfc3339(b"From a@b Mon Feb 30 10:00:00 2024"),
            None
        );
    }

    #[test]
    fn splits_messages_and_records_their_byte_offsets() {
        let a = msg("a@x", "first", "hello");
        let b = msg("b@x", "second", "world");
        let mbox = format!("{SEP_A}\n{a}\n{SEP_GMAIL}\n{b}\n");
        let out = split_all(mbox.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].offset, 0);
        assert_eq!(out[0].from_line, SEP_A);
        assert_eq!(out[0].bytes, a.as_bytes(), "container blank line removed");
        let second_at = (SEP_A.len() + 1 + a.len() + 1) as u64;
        assert_eq!(out[1].offset, second_at);
        assert_eq!(&mbox.as_bytes()[second_at as usize..][..5], b"From ");
        assert_eq!(out[1].bytes, b.as_bytes());
        assert!(!out[0].oversized() && !out[1].oversized());
    }

    /// mboxrd: exactly one `>` comes off `^>+From `; `>` quoting of anything
    /// else (an ordinary quoted reply) is left alone.
    #[test]
    fn from_quoting_is_undone_by_exactly_one_level() {
        let body = ">From the desk of Bob\n>>From the archive\n> From spaced\n>Fromage\n> quoted reply\nFrom what I hear";
        let mbox = format!("{SEP_A}\n{}\n", msg("q@x", "quoting", body));
        let out = split_all(mbox.as_bytes());
        assert_eq!(
            out.len(),
            1,
            "an unquoted prose `From ` line is not a split"
        );
        let text = String::from_utf8(out[0].bytes.clone()).unwrap();
        assert!(text.contains("\nFrom the desk of Bob\n"), "{text}");
        assert!(text.contains("\n>From the archive\n"), "{text}");
        assert!(text.contains("\n> From spaced\n"), "{text}");
        assert!(text.contains("\n>Fromage\n"), "{text}");
        assert!(text.contains("\n> quoted reply\n"), "{text}");
        assert!(text.contains("\nFrom what I hear\n"), "{text}");
    }

    #[test]
    fn crlf_mailboxes_split_the_same_way() {
        let a = msg("a@x", "one", "alpha").replace('\n', "\r\n");
        let b = msg("b@x", "two", ">From quoted").replace('\n', "\r\n");
        let mbox = format!("{SEP_A}\r\n{a}\r\n{SEP_TBIRD}\r\n{b}\r\n");
        let out = split_all(mbox.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].from_line, SEP_A, "no stray CR on the separator");
        assert_eq!(out[0].bytes, a.as_bytes());
        assert!(out[1].bytes.ends_with(b"\r\nFrom quoted\r\n"));
    }

    #[test]
    fn a_missing_trailing_newline_keeps_the_last_line() {
        let mbox = format!(
            "{SEP_A}\n{}",
            msg("a@x", "tail", "last line without newline")
        );
        let mbox = mbox.trim_end_matches('\n');
        let out = split_all(mbox.as_bytes());
        assert_eq!(out.len(), 1);
        assert!(out[0].bytes.ends_with(b"last line without newline"));
        // …and a file that ends ON a separator yields that (empty) message
        // rather than losing track of it.
        let out = split_all(format!("{SEP_A}\n{}\n{SEP_TBIRD}", msg("a@x", "s", "b")).as_bytes());
        assert_eq!(out.len(), 2);
        assert!(out[1].bytes.is_empty());
    }

    #[test]
    fn bytes_before_the_first_separator_are_counted_not_indexed() {
        let mbox = format!("garbage line\nmore\n{SEP_A}\n{}\n", msg("a@x", "s", "b"));
        let mut s = Splitter::new(BufReader::new(Cursor::new(mbox.as_bytes())));
        let first = s.next_message().unwrap().unwrap();
        assert_eq!(first.offset, "garbage line\nmore\n".len() as u64);
        assert_eq!(s.preamble_bytes, first.offset);
        assert!(s.next_message().unwrap().is_none());
        assert!(s.next_message().unwrap().is_none(), "stays finished");
    }

    /// The property the whole module exists for: the result must not depend on
    /// where the reader's chunk boundaries fall. Every buffer size from 1 byte
    /// up puts a boundary inside separators, inside `>From `, inside CRLF and
    /// inside multi-byte characters.
    #[test]
    fn chunk_boundaries_never_change_the_split() {
        let a = msg(
            "a@x",
            "Überweisung — 設計書",
            "Grüße\n>From Zoë\nمرحبا بالعالم",
        );
        let b = msg("b@x", "second", "x").replace('\n', "\r\n");
        let long = "L".repeat(3 * HEAD_CAP + 7);
        let c = msg(
            "c@x",
            "long line",
            &format!("{long}\n>From after the long line"),
        );
        let mbox = format!("{SEP_A}\n{a}\n{SEP_GMAIL}\r\n{b}\r\n{SEP_TBIRD}\n{c}\n");
        let want = split_with(mbox.as_bytes(), 1 << 20, 1 << 16);
        assert_eq!(want.len(), 3);
        assert!(want[2]
            .bytes
            .ends_with(format!("{long}\nFrom after the long line\n").as_bytes()));
        for buf in (1..40).chain([63, 64, 65, 1023, 1024, 1025]) {
            assert_eq!(split_with(mbox.as_bytes(), 1 << 20, buf), want, "buf={buf}");
        }
    }

    /// A line longer than the head window is streamed, never buffered whole,
    /// and a `From `-looking head on such a line is body: a separator is short.
    #[test]
    fn an_over_long_line_is_body_even_when_it_opens_like_a_separator() {
        let long = format!("{SEP_A} {}", "x".repeat(2 * HEAD_CAP));
        let mbox = format!("{SEP_A}\n{}\n", msg("a@x", "s", &long));
        let out = split_all(mbox.as_bytes());
        assert_eq!(out.len(), 1);
        assert!(out[0].bytes.ends_with(format!("{long}\n").as_bytes()));
    }

    /// A message over the cap keeps its head, reports its real size, and does
    /// not stop the messages after it from being found. The cut lands in the
    /// middle of multi-byte text on purpose.
    #[test]
    fn a_byte_order_mark_before_the_first_separator_is_not_preamble() {
        let mut mbox = UTF8_BOM.to_vec();
        mbox.extend_from_slice(
            b"From a@x.org Mon Jan  1 10:00:00 2024\nSubject: first\n\nxqbom one\n\n\
              From b@x.org Mon Jan  1 11:00:00 2024\nSubject: second\n\n\xef\xbb\xbfnot a bom here\n",
        );
        let mut split = Splitter::new(Cursor::new(mbox));
        let first = split.next_message().unwrap().expect("the first message");
        assert_eq!(
            first.offset, 0,
            "the separator is still the start of the file"
        );
        assert_eq!(first.from_line, "From a@x.org Mon Jan  1 10:00:00 2024");
        assert!(first.bytes.starts_with(b"Subject: first"));
        let second = split.next_message().unwrap().expect("the second message");
        // Mid-file the same three bytes are text and stay where they are.
        assert!(second.bytes.ends_with(b"\xef\xbb\xbfnot a bom here\n"));
        assert!(split.next_message().unwrap().is_none());
        assert_eq!(split.preamble_bytes, 0);
    }

    #[test]
    fn an_oversized_message_is_capped_and_the_next_one_still_splits() {
        let big_body = "設計書مرحبا".repeat(400);
        let a = msg("big@x", "big", &big_body);
        let b = msg("b@x", "after", "still here");
        let mbox = format!("{SEP_A}\n{a}\n{SEP_TBIRD}\n{b}\n");
        for cap in [200usize, 201, 202, 203, 204, 205] {
            let out = split_with(mbox.as_bytes(), cap, 64);
            assert_eq!(out.len(), 2, "cap={cap}");
            assert_eq!(out[0].bytes.len(), cap);
            assert!(out[0].oversized());
            assert_eq!(out[0].total_len, a.len() as u64 + 1, "real size kept");
            assert_eq!(out[1].bytes, b.as_bytes());
            // The capped head is cut mid-character somewhere in this range;
            // turning it into records must not panic (panic = abort).
            let mut sink = |_r: RawRecord| true;
            let mut stats = ExtractStats::default();
            let env = MessageEnvelope::default();
            let _ = emit_message(&out[0].bytes, &env, &mut sink, &mut stats);
        }
    }

    #[test]
    fn records_have_the_eml_shape_namespaced_by_offset() {
        let a = msg("a@x", "Acquisition — next steps", "term sheet at $420M");
        let b = String::from(
            "From: Bob <bob@example.org>\nTo: alice@example.org\nSubject: Re: Acquisition\n\
             Date: Tue, 2 Jan 2024 09:00:00 +0000\nMessage-ID: <b@x>\nIn-Reply-To: <a@x>\n\
             References: <root@x> <a@x>\nX-Gmail-Labels: Inbox,Important,=?UTF-8?B?w4RyZ2Vy?=\n\
             X-GM-THRID: 1787654321098765432\nMIME-Version: 1.0\n\
             Content-Type: multipart/mixed; boundary=\"b1\"\n\n--b1\nContent-Type: text/plain\n\n\
             agreed\n--b1\nContent-Type: text/plain; name=\"notes.txt\"\n\
             Content-Disposition: attachment; filename=\"notes.txt\"\n\nsecret plan\n--b1--\n",
        );
        let mbox = format!("{SEP_A}\n{a}\n{SEP_GMAIL}\n{b}\n");
        let (recs, stats) = records(mbox.as_bytes());
        assert_eq!(stats.records, 3);
        assert!(!stats.truncated);
        assert_eq!(recs[0].locator, "m0-msg-s0");
        assert_eq!(field(&recs[0], "email_message_id"), Some("a@x"));
        let off = SEP_A.len() + 1 + a.len() + 1;
        assert_eq!(recs[1].locator, format!("m{off}-msg-s0"));
        assert_eq!(field(&recs[1], "email_in_reply_to"), Some("a@x"));
        assert_eq!(
            recs[1].fields["email_references"],
            serde_json::json!(["root@x", "a@x"])
        );
        assert_eq!(
            recs[1].fields["email_labels"],
            serde_json::json!(["Inbox", "Important", "Ärger"]),
            "labels split, encoded-word decoded"
        );
        assert_eq!(
            field(&recs[1], "email_thread_id"),
            Some("18cf06223648b478"),
            "X-GM-THRID 1787654321098765432 in the hex form Gmail's own URLs use"
        );
        assert_eq!(recs[2].locator, format!("m{off}-att0-s0"));
        assert_eq!(field(&recs[2], "attachment_name"), Some("notes.txt"));
        assert_eq!(field(&recs[2], "email_message_id"), Some("b@x"));
        assert_eq!(field(&recs[2], "body"), Some("secret plan"));
        for r in &recs {
            assert_eq!(r.group, None);
            assert_eq!(r.origin, crate::extract::FieldOrigin::Extractor);
        }
    }

    #[test]
    fn locator_offsets_round_trip_and_reject_everything_else() {
        assert_eq!(locator_offset("m0-msg-s0"), Some(0));
        assert_eq!(
            locator_offset("m1073777879-att2-p3-s1"),
            Some(1_073_777_879)
        );
        assert_eq!(
            locator_offset("m18446744073709551615-raw-s0"),
            Some(u64::MAX)
        );
        // Not ours: a standalone .eml, other families, overflow, junk, non-ASCII.
        for other in [
            "msg-s0",
            "m-msg-s0",
            "m12",
            "m12x-msg-s0",
            "att0-card",
            "p1-s0",
            "",
            "m",
            "m18446744073709551616-msg-s0",
            "m１２-msg-s0",
            "mé-1",
            "設計-m5-",
        ] {
            assert_eq!(locator_offset(other), None, "{other:?}");
        }
        // What the extractor writes is what this reads back.
        let mbox = format!(
            "{SEP_A}\n{}\n{SEP_TBIRD}\n{}\n",
            msg("a@x", "one", "x"),
            msg("b@x", "two", "y")
        );
        let offsets: Vec<u64> = records(mbox.as_bytes())
            .0
            .iter()
            .map(|r| locator_offset(&r.locator).expect("every mbox record carries its offset"))
            .collect();
        assert_eq!(
            offsets,
            vec![
                0,
                (SEP_A.len() + 1 + msg("a@x", "one", "x").len() + 1) as u64
            ]
        );
    }

    /// Same bytes, same ids — and an id does not move when a LATER message
    /// changes, because the locator is the message's own offset.
    #[test]
    fn locators_are_deterministic() {
        let a = msg("a@x", "one", "alpha");
        let mbox1 = format!("{SEP_A}\n{a}\n{SEP_TBIRD}\n{}\n", msg("b@x", "two", "beta"));
        let mbox2 = format!(
            "{SEP_A}\n{a}\n{SEP_TBIRD}\n{}\n",
            msg("c@x", "2", "gamma gamma")
        );
        let loc = |data: &str| -> Vec<String> {
            records(data.as_bytes())
                .0
                .into_iter()
                .map(|r| r.locator)
                .collect()
        };
        assert_eq!(loc(&mbox1), loc(&mbox1));
        assert_eq!(loc(&mbox1)[0], loc(&mbox2)[0]);
        let unique: std::collections::HashSet<_> = loc(&mbox1).into_iter().collect();
        assert_eq!(unique.len(), 2);
    }

    /// A message with no `Date:` of its own falls back to the separator's
    /// delivery time; one WITH a date keeps the sender's.
    #[test]
    fn the_separator_date_is_only_a_fallback() {
        let dated = msg("a@x", "dated", "x");
        let undated = "From: a@x.org\nTo: b@x.org\nSubject: undated\nMessage-ID: <u@x>\n\nbody\n";
        let mbox = format!("{SEP_GMAIL}\n{dated}\n{SEP_GMAIL}\n{undated}\n");
        let (recs, _) = records(mbox.as_bytes());
        assert_eq!(field(&recs[0], "email_date"), Some("2024-01-01T10:00:00Z"));
        assert_eq!(field(&recs[1], "email_date"), Some("2023-11-14T22:13:20Z"));
    }

    /// Hostile and broken entries: none may panic, and none may take the rest
    /// of the mailbox down with it.
    #[test]
    fn malformed_messages_never_stop_the_mailbox() {
        let good = msg("good@x", "survivor", "still indexed");
        let mut mbox: Vec<u8> = Vec::new();
        fn add_to(mbox: &mut Vec<u8>, sep: &str, body: &[u8]) {
            mbox.extend_from_slice(sep.as_bytes());
            mbox.push(b'\n');
            mbox.extend_from_slice(body);
            mbox.extend_from_slice(b"\n\n");
        }
        // no headers at all, only non-UTF-8 bytes
        add_to(&mut mbox, SEP_A, &[0xff, 0xfe, 0xfd, b'\n', 0x80, 0x81]);
        // an unterminated multipart with a truncated base64 attachment
        add_to(
            &mut mbox,
            SEP_A,
            b"From: a@x.org\nSubject: broken\nMIME-Version: 1.0\n\
              Content-Type: multipart/mixed; boundary=\"zz\"\n\n--zz\n\
              Content-Type: application/pdf; name=\"x.pdf\"\n\
              Content-Transfer-Encoding: base64\n\nJVBERi0xLjQKJ",
        );
        // latin-1 body declared as such, 8-bit, no MIME-Version
        add_to(
            &mut mbox,
            SEP_A,
            b"From: a@x.org\nSubject: caf\xe9\nContent-Type: text/plain; charset=iso-8859-1\n\n\
              Gr\xfc\xdfe aus K\xf6ln\n",
        );
        // an entry that is nothing but a separator
        mbox.extend_from_slice(format!("{SEP_TBIRD}\n").as_bytes());
        add_to(&mut mbox, SEP_GMAIL, good.as_bytes());
        let (recs, stats) = records(&mbox);
        assert!(
            recs.iter()
                .any(|r| field(r, "email_message_id") == Some("good@x")),
            "the message after the wreckage must still be indexed"
        );
        // The separator-only entry is junk, not an empty "(no subject)" document.
        assert!(
            recs.iter()
                .all(|r| field(r, "title") != Some("(no subject)")),
            "an entry with no bytes must not become an empty document"
        );
        assert!(stats.junk >= 1, "{stats:?}");
        let latin = recs
            .iter()
            .find(|r| field(r, "body").is_some_and(|b| b.contains("Köln")))
            .expect("declared latin-1 body is decoded, not dropped");
        // The subject opens the searchable body — and it is a RAW Latin-1
        // `caf\xe9` in the header, which the Windows-1252 header rule rescues
        // (it used to be indexed as `caf\u{fffd}`).
        assert_eq!(field(latin, "body"), Some("café\n\nGrüße aus Köln"));
        assert!(stats.records >= 3, "{stats:?}");
    }

    /// Phase A stops the extractor through the sink; the mailbox must stop
    /// splitting, not read the remaining gigabytes.
    #[test]
    fn a_sink_that_says_stop_stops_the_split() {
        let mut mbox = String::new();
        for i in 0..50 {
            mbox.push_str(&format!("{SEP_A}\n{}\n", msg(&format!("{i}@x"), "s", "b")));
        }
        let mut seen = 0usize;
        let mut sink = |_r: RawRecord| {
            seen += 1;
            seen < 3
        };
        let stats = extract_from(
            BufReader::new(Cursor::new(mbox.as_bytes())),
            None,
            &mut sink,
        )
        .unwrap();
        assert_eq!(seen, 3);
        assert_eq!(stats.records, 3);
        // …and the byte limit stops it BETWEEN messages, never inside one.
        let mut whole = Vec::new();
        let mut sink = |r: RawRecord| {
            whole.push(r);
            true
        };
        extract_from(
            BufReader::with_capacity(64, Cursor::new(mbox.as_bytes())),
            Some(1),
            &mut sink,
        )
        .unwrap();
        assert_eq!(whole.len(), 1);
        // Subject `s`, blank line, body `b` (`eml::with_subject`).
        assert_eq!(field(&whole[0], "body"), Some("s\n\nb"));
    }

    /// One message may still not exceed the per-DOCUMENT record cap (#381),
    /// and hitting it is reported. The mailbox as a whole has no such cap —
    /// see the module docs — which the 5000-message case pins.
    #[test]
    fn the_record_cap_is_per_message_not_per_mailbox() {
        let mut mbox = String::new();
        for i in 0..5000 {
            mbox.push_str(&format!("{SEP_A}\n{}\n", msg(&format!("{i}@x"), "s", "b")));
        }
        let (recs, stats) = records(mbox.as_bytes());
        assert_eq!(recs.len(), 5000, "more records than MAX_RECORDS_PER_FILE");
        assert!(recs.len() > crate::extract::MAX_RECORDS_PER_FILE);
        assert!(!stats.truncated);

        // ~13 MB of paragraphs in ONE body: over 4096 sections.
        let para = "word ".repeat(600);
        let body = vec![para; 4400].join("\n\n");
        let mbox = format!("{SEP_A}\n{}\n", msg("huge@x", "huge", &body));
        let (recs, stats) = records(mbox.as_bytes());
        assert_eq!(recs.len(), crate::extract::MAX_RECORDS_PER_FILE);
        assert!(
            stats.truncated,
            "a capped message is reported, never silent"
        );
    }

    // ── the parallel pipeline ──────────────────────────────────────────

    fn shape(recs: &[RawRecord]) -> Vec<(String, Option<String>, Map<String, Value>)> {
        recs.iter()
            .map(|r| (r.locator.clone(), r.group.clone(), r.fields.clone()))
            .collect()
    }

    fn mime_msg(i: usize) -> String {
        format!(
            "From: Zoë <zoe@example.org>\nTo: bob@example.org\nSubject: Anhang {i} — 添付\n\
             Date: Mon, 1 Jan 2024 10:00:00 +0000\nMessage-ID: <mime-{i}@example.org>\n\
             MIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"b{i}\"\n\n\
             --b{i}\nContent-Type: text/plain; charset=utf-8\n\nbody {i} with attachment ✓\n\
             --b{i}\nContent-Type: text/plain; name=\"notes-{i}.txt\"\n\
             Content-Disposition: attachment; filename=\"notes-{i}.txt\"\n\n\
             attached text {i} — café\n--b{i}--\n"
        )
    }

    /// Every shape the splitter and the parser have a branch for, mixed:
    /// three writers' separators, multi-byte text, quoted and unquoted
    /// `From ` lines, CRLF, a long body that sections, a MIME attachment,
    /// an entry with no headers, an empty entry.
    fn varied_mailbox(n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            let sep = match i % 3 {
                0 => SEP_A,
                1 => SEP_GMAIL,
                _ => SEP_TBIRD,
            };
            out.extend_from_slice(sep.as_bytes());
            out.push(b'\n');
            if i % 7 == 6 {
                out.extend_from_slice(format!("no headers here {i} — 設計\n\n").as_bytes());
                continue;
            }
            if i % 11 == 10 {
                out.push(b'\n');
                continue;
            }
            if i % 5 == 4 {
                out.extend_from_slice(mime_msg(i).as_bytes());
                out.push(b'\n');
                continue;
            }
            let body = match i % 5 {
                0 => format!(
                    "Résumé № {i}: 設計 données — café\n>From quoted {i}\n\
                     From what I understand, this is prose.\n"
                ),
                1 => format!("plain {i}\n\n{}", "Paragraph text here. ".repeat(300)),
                2 => String::new(),
                _ => format!("line one {i}\r\nline two ✓\r\n"),
            };
            out.extend_from_slice(
                msg(
                    &format!("id-{i}@example.org"),
                    &format!("Subject {i} — 件名"),
                    &body,
                )
                .as_bytes(),
            );
            out.push(b'\n');
        }
        out
    }

    fn run_sequential(data: &[u8]) -> (Vec<RawRecord>, ExtractStats) {
        let mut out = Vec::new();
        let mut sink = |r: RawRecord| {
            out.push(r);
            true
        };
        let stats = extract_sequential(
            BufReader::with_capacity(4096, Cursor::new(data)),
            None,
            &mut sink,
        )
        .unwrap();
        (out, stats)
    }

    fn run_parallel(data: &[u8], workers: usize) -> (Vec<RawRecord>, ExtractStats) {
        let mut out = Vec::new();
        let mut sink = |r: RawRecord| {
            out.push(r);
            true
        };
        let stats = extract_parallel(
            BufReader::with_capacity(4096, Cursor::new(data)),
            &mut sink,
            workers,
        )
        .unwrap();
        (out, stats)
    }

    #[test]
    fn parallel_and_sequential_paths_emit_identical_record_streams() {
        let data = varied_mailbox(120);
        let (seq, seq_stats) = run_sequential(&data);
        assert!(
            seq.len() > 120,
            "sections and attachments outnumber messages: {}",
            seq.len()
        );
        assert!(seq_stats.junk > 0, "the empty entries are junk");
        for workers in [2, 3, 7, 16] {
            let (par, par_stats) = run_parallel(&data, workers);
            assert_eq!(shape(&par), shape(&seq), "workers={workers}");
            assert_eq!(
                (par_stats.records, par_stats.junk, par_stats.truncated),
                (seq_stats.records, seq_stats.junk, seq_stats.truncated),
                "workers={workers}"
            );
        }
        // And the dispatcher itself, at whatever width this process has.
        let (via_dispatch, _) = records(&data);
        assert_eq!(shape(&via_dispatch), shape(&seq));
    }

    #[test]
    fn sink_stop_ends_the_parallel_run_without_a_hang() {
        let data = varied_mailbox(200);
        let mut out = Vec::new();
        let mut sink = |r: RawRecord| {
            out.push(r);
            out.len() < 5
        };
        let stats = extract_parallel(
            BufReader::with_capacity(4096, Cursor::new(&data[..])),
            &mut sink,
            4,
        )
        .unwrap();
        assert_eq!(out.len(), 5);
        assert!(stats.records >= 5);
        let (seq, _) = run_sequential(&data);
        assert_eq!(shape(&out), shape(&seq[..5]), "the first five, in order");
    }

    /// Serves `data` until `fail_at`, then fails every read: a disk that goes
    /// away, or a pipe that closes, mid-mailbox.
    struct Flaky<'a> {
        data: &'a [u8],
        pos: usize,
        fail_at: usize,
    }

    impl std::io::Read for Flaky<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.fail_at {
                return Err(std::io::Error::other("disk gone"));
            }
            let end = self.data.len().min(self.fail_at).min(self.pos + buf.len());
            let n = end - self.pos;
            buf[..n].copy_from_slice(&self.data[self.pos..end]);
            self.pos = end;
            Ok(n)
        }
    }

    #[test]
    fn splitter_error_surfaces_after_the_messages_before_it_were_emitted() {
        let data = varied_mailbox(60);
        let fail_at = data.len() / 2;
        let run = |workers: usize| {
            let mut out = Vec::new();
            let mut sink = |r: RawRecord| {
                out.push(r);
                true
            };
            let reader = BufReader::with_capacity(
                1024,
                Flaky {
                    data: &data,
                    pos: 0,
                    fail_at,
                },
            );
            let res = if workers == 1 {
                extract_sequential(reader, None, &mut sink)
            } else {
                extract_parallel(reader, &mut sink, workers)
            };
            (res.map(|_| ()).map_err(|e| e.to_string()), out)
        };
        let (seq_res, seq) = run(1);
        assert!(seq_res.unwrap_err().contains("disk gone"));
        assert!(
            !seq.is_empty(),
            "messages before the failure are still indexed"
        );
        for workers in [2, 5] {
            let (par_res, par) = run(workers);
            assert!(
                par_res.unwrap_err().contains("disk gone"),
                "workers={workers}"
            );
            assert_eq!(shape(&par), shape(&seq), "workers={workers}");
        }
    }

    #[test]
    fn a_lease_gives_back_whatever_it_still_holds() {
        let budget = Budget::new(100);
        {
            let lease = Lease::new(&budget);
            lease.acquire(60);
            lease.acquire(30);
            lease.release(30);
            // Releasing more than is held must not underflow the shared pool.
            lease.release(1_000);
            lease.acquire(70);
            assert_eq!(*budget.used.lock().unwrap(), 70);
        } // a stopped sink / splitter error: the mailbox ends still holding 70
        assert_eq!(
            *budget.used.lock().unwrap(),
            0,
            "one mailbox's exit path must not shrink the process budget"
        );
    }

    #[test]
    fn budget_admits_an_oversized_message_alone_and_blocks_the_next() {
        use std::sync::atomic::AtomicBool;
        let budget = Arc::new(Budget::new(10));
        budget.acquire(50); // over the cap, nothing in flight: admitted
        let entered = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (budget, entered) = (Arc::clone(&budget), Arc::clone(&entered));
            std::thread::spawn(move || {
                budget.acquire(1);
                entered.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !entered.load(Ordering::SeqCst),
            "must wait while 50 > cap is in flight"
        );
        budget.release(50);
        waiter.join().unwrap();
        assert!(entered.load(Ordering::SeqCst));
    }

    #[test]
    fn gate_bounds_parsers_across_mailboxes_and_a_dropped_permit_frees_a_slot() {
        let gate = Arc::new(Gate::new(2));
        let a = gate.acquire();
        let _b = gate.acquire();
        let third = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let _c = gate.acquire();
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !third.is_finished(),
            "two slots, two holders: the third waits"
        );
        drop(a);
        third.join().unwrap();
        gate.set_limit(1);
        assert_eq!(gate.limit(), 1);
    }
}
