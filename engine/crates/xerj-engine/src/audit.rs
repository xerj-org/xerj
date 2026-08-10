//! Tamper-evident audit log — v0.9 9-P4, made durable by issue #201.
//!
//! Audited operations write a structured entry into an append-only log
//! that's queryable via `GET /_audit/_search`. Each entry includes a hash
//! chain over the previous entry so any tampering is detectable on verify.
//!
//! **Coverage, stated honestly:** the callers today are `_search` and the
//! three `_security/api_key` operations (create / get / invalidate). The
//! original module doc claimed "every search / index / delete / admin op",
//! which was never true — indexing and deletion are not audited. Do not read
//! an absent entry as evidence that a write did not happen.
//!
//! WORM semantics:
//! - Append-only (no API to mutate or remove past entries).
//! - Entry N's hash chains over entry N-1's hash → tampering breaks
//!   the chain at the modified position and every subsequent entry.
//! - Verifier walks the buffer top-to-bottom and stops at the first
//!   mismatch.  Operators can pin known-good chain heads externally.
//!
//! # Durability (issue #201)
//!
//! This shipped as an in-memory `VecDeque` and nothing else, so a restart
//! erased it. Tamper-evidence that dies with the process is not much use
//! when the process is what you are investigating: the cheapest way to
//! destroy the evidence was to restart the node, and an ordinary crash did
//! it by accident.
//!
//! Entries are now appended to `<data_dir>/audit.jsonl` as JSON lines and
//! reloaded on boot. Three things that shape the design:
//!
//! * **No `fsync` per entry.** `append` is on the search path — every query
//!   writes one — so a durability barrier per call would be a per-request
//!   tax measured against a sub-millisecond read. A plain `write(2)` puts
//!   the bytes in the page cache, which is what actually answers the threat
//!   in the issue: the log survives a process restart, including `kill -9`.
//!   A machine power-loss can still lose the unflushed tail. That is the
//!   honest boundary of this feature, and the reason
//!   [`AuditLog::sync_to_disk`] exists for callers that want the barrier.
//! * **Bounded on disk.** An unbounded append log fed by every search is a
//!   disk-exhaustion bug waiting to happen. When the file passes
//!   [`MAX_AUDIT_FILE_BYTES`] it is rewritten from the in-memory ring
//!   (atomic temp + rename), so the file converges to the ring's size.
//! * **The chain seed is explicit.** Both rotation and the ring dropping its
//!   head remove entries the surviving head chains over. The hash of the
//!   last dropped entry is kept as [`AuditLog::head_prev_hash`] and written
//!   as the file's first line, so a rotated or restored log still verifies
//!   from a real, recorded anchor instead of failing merely because it was
//!   truncated. (Before #201 `verify()` always reseeded from 64 zeros, so a
//!   ring that had rotated even once reported a broken chain — the previous
//!   `ring_rotates_at_capacity` test asserted exactly that, and it made
//!   `verify()` useless on any node that had been up for a while.)
//!
//! What this does **not** claim: the file is not tamper-*proof*. Anyone who
//! can write it can rewrite the whole chain, seed line included. The chain
//! makes edits detectable against an externally pinned head, which is what
//! WORM evidence collection needs and what the module has always said.

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::warn;

/// Default ring buffer capacity.  Each entry is < 256 bytes so the
/// total footprint at capacity is ~ 1 MB.
pub const DEFAULT_AUDIT_CAPACITY: usize = 4096;

/// Rewrite the on-disk log once it passes this size. Sized so the rewrite is
/// rare (a 4096-entry ring at ~256 B/entry is ~1 MB, so this is ~16 rings of
/// slack) while the file stays trivially greppable.
pub const MAX_AUDIT_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The all-zero anchor a chain with no dropped predecessor starts from.
fn genesis() -> String {
    "0".repeat(64)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Sequential entry number (starts at 1).
    pub seq: u64,
    /// Wall-clock millis since epoch.
    pub at_ms: u64,
    /// Operation tag (e.g. "search", "index", "delete", "admin.role.put").
    pub op: String,
    /// Subject (user / api key / OIDC sub).  "anonymous" if unauth.
    pub subject: String,
    /// Resource (index name, role name, etc.).
    pub resource: String,
    /// Outcome: "ok", "denied", "error".
    pub outcome: String,
    /// Optional short context (e.g. "took=12ms hits=3").
    pub note: String,
    /// SHA-256 hex digest over: prev_hash || serialised(this_entry_minus_hash).
    /// The first entry of a never-truncated chain uses prev_hash = 64 zeros;
    /// after a truncation it is the hash of the last dropped entry, recorded
    /// in the `chain_seed` line of the persisted log.
    pub hash: String,
}

/// The first line of a persisted log: the hash the first retained entry
/// chains over. Written whenever the log is (re)created from a ring that has
/// already dropped entries, so verification has a real anchor.
#[derive(Serialize, Deserialize)]
struct ChainSeed {
    chain_seed: String,
}

/// Everything the persistence side owns. Separate from the ring's lock so a
/// reader (`snapshot`/`verify`) never waits on a write syscall.
struct Sink {
    path: PathBuf,
    file: Option<std::fs::File>,
    bytes: u64,
}

pub struct AuditLog {
    // VecDeque: the ring rotates on every append once at capacity;
    // `Vec::remove(0)` was an O(capacity) memmove (~750 KB at the 4096
    // default) on EVERY audited request — a measurable slice of the
    // fixed per-request tax on trivial reads. `pop_front` is O(1).
    buf: RwLock<std::collections::VecDeque<AuditEntry>>,
    /// Hash the oldest *retained* entry chains over. `genesis()` until the
    /// ring first drops an entry (or until a truncated log is loaded).
    head_prev_hash: RwLock<String>,
    capacity: usize,
    next_seq: AtomicU64,
    /// `None` for an in-memory-only log ([`AuditLog::new`]) — the shape the
    /// unit tests and any embedder without a data directory use.
    sink: Option<Mutex<Sink>>,
}

impl AuditLog {
    /// In-memory-only log. Entries are lost on restart; use
    /// [`AuditLog::open`] for a node with a data directory.
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            buf: RwLock::new(std::collections::VecDeque::with_capacity(capacity)),
            head_prev_hash: RwLock::new(genesis()),
            capacity,
            next_seq: AtomicU64::new(1),
            sink: None,
        })
    }

    /// Durable log backed by `path` (`<data_dir>/audit.jsonl`).
    ///
    /// Restores the last `capacity` entries, continues the sequence, and
    /// anchors verification on the recorded chain seed. A log that cannot be
    /// read or opened for append degrades to in-memory behaviour with a
    /// warning: an unwritable audit file must not stop the node from booting,
    /// but the operator has to be told the evidence is not being kept.
    pub fn open(capacity: usize, path: impl AsRef<Path>) -> Arc<Self> {
        let path = path.as_ref().to_path_buf();
        let (entries, seed) = read_log(&path, capacity);
        let next_seq = entries.back().map(|e| e.seq + 1).unwrap_or(1);

        let log = Self {
            buf: RwLock::new(entries),
            head_prev_hash: RwLock::new(seed),
            capacity,
            next_seq: AtomicU64::new(next_seq),
            sink: Some(Mutex::new(Sink {
                path: path.clone(),
                file: None,
                bytes: 0,
            })),
        };
        // Normalise the file to exactly what was restored: this drops any
        // entries beyond `capacity` that the ring did not keep, writes the
        // seed line, and leaves the handle positioned for appends.
        log.rewrite_from_ring();
        Arc::new(log)
    }

    /// Append an entry.  Computes the hash over (prev_hash || canonical
    /// JSON of the entry without its `hash` field).
    ///
    /// The persist happens **while the ring lock is still held**. That is
    /// deliberate and load-bearing: the on-disk order has to be the chain
    /// order, and two concurrent appends that hashed under the lock but wrote
    /// after releasing it could land in the file back-to-front, producing a
    /// log that fails to verify after the next restart for no reason but
    /// concurrency. The cost is one `write(2)` inside the lock; the
    /// alternative is a chain that is silently wrong under load, which is the
    /// worst possible failure mode for evidence.
    pub fn append(&self, op: &str, subject: &str, resource: &str, outcome: &str, note: &str) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut entry = AuditEntry {
            seq,
            at_ms,
            op: op.to_string(),
            subject: subject.to_string(),
            resource: resource.to_string(),
            outcome: outcome.to_string(),
            note: note.to_string(),
            hash: String::new(),
        };
        let mut buf = self.buf.write();
        let prev_hash = buf
            .back()
            .map(|e| e.hash.clone())
            .unwrap_or_else(|| self.head_prev_hash.read().clone());
        entry.hash = compute_hash(&prev_hash, &entry);
        if buf.len() >= self.capacity {
            if let Some(dropped) = buf.pop_front() {
                // The new oldest entry chains over the one just dropped —
                // record it so `verify` still has an anchor.
                *self.head_prev_hash.write() = dropped.hash;
            }
        }
        buf.push_back(entry);
        let Some(sink) = &self.sink else { return };
        let mut sink = sink.lock();
        let Some(entry) = buf.back() else { return };
        if append_line(&mut sink, entry) {
            // Bound the file. Rare (once per `MAX_AUDIT_FILE_BYTES`), and the
            // rewrite is over at most `capacity` entries.
            let seed = self.head_prev_hash.read().clone();
            rewrite_file(&mut sink, buf.iter(), &seed);
        }
    }

    pub fn snapshot(&self) -> Vec<AuditEntry> {
        self.buf.read().iter().cloned().collect()
    }

    /// Walk the chain top-to-bottom.  Returns Ok(()) if the chain is
    /// intact, or Err((seq, expected, actual)) at the first break.
    ///
    /// Seeds from [`Self::head_prev_hash`], so a log that has rotated (in the
    /// ring or on disk) verifies over the entries it still holds instead of
    /// reporting a break it created itself.
    pub fn verify(&self) -> Result<(), (u64, String, String)> {
        let buf = self.buf.read();
        let mut prev = self.head_prev_hash.read().clone();
        for e in buf.iter() {
            let expected = compute_hash(&prev, e);
            if expected != e.hash {
                return Err((e.seq, expected, e.hash.clone()));
            }
            prev = e.hash.clone();
        }
        Ok(())
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Relaxed)
    }

    /// Force the OS to commit the log to stable storage.
    ///
    /// Not called per entry on purpose (see the module docs); this is for a
    /// caller that has just recorded something it cannot afford to lose to a
    /// power cut. A no-op for an in-memory log.
    pub fn sync_to_disk(&self) {
        if let Some(sink) = &self.sink {
            let sink = sink.lock();
            if let Some(f) = sink.file.as_ref() {
                let _ = f.sync_data();
            }
        }
    }

    /// Rewrite the persisted log so it contains exactly the current ring,
    /// preceded by its chain seed. Used on open, to normalise a file that may
    /// hold more entries than the ring keeps.
    ///
    /// Takes the ring lock before the sink lock — the one ordering used
    /// everywhere in this module (`append` does the same), so the two can
    /// never deadlock against each other.
    fn rewrite_from_ring(&self) {
        let Some(sink) = &self.sink else {
            return;
        };
        let buf = self.buf.read();
        let seed = self.head_prev_hash.read().clone();
        let mut sink = sink.lock();
        rewrite_file(&mut sink, buf.iter(), &seed);
    }
}

/// Replace the persisted log with `entries`, anchored on `seed`.
///
/// Atomic temp + rename, so a crash mid-rewrite leaves the previous log
/// intact rather than a half file. A failure is warned, not propagated: the
/// caller is either booting (an unwritable audit file must not stop the node)
/// or serving a request (which must not fail because the log could not be
/// bounded). Either way the operator is told the evidence is not being kept.
fn rewrite_file<'a>(sink: &mut Sink, entries: impl Iterator<Item = &'a AuditEntry>, seed: &str) {
    let mut out = Vec::with_capacity(4096);
    let seed_line = ChainSeed {
        chain_seed: seed.to_string(),
    };
    if serde_json::to_writer(&mut out, &seed_line).is_err() {
        return;
    }
    out.push(b'\n');
    for e in entries {
        if serde_json::to_writer(&mut out, e).is_err() {
            return;
        }
        out.push(b'\n');
    }

    // Drop the old handle before the rename so Windows can replace the file
    // (a rename over an open file fails there, unlike on unix).
    sink.file = None;
    let path = sink.path.clone();
    if let Err(e) = write_file_atomic_0600(&path, &out) {
        warn!(error = %e, path = %path.display(),
              "audit log is not being persisted (could not write it)");
        return;
    }
    sink.bytes = out.len() as u64;
    match open_append_0600(&path) {
        Ok(f) => sink.file = Some(f),
        Err(e) => warn!(error = %e, path = %path.display(),
                        "audit log is not being persisted (could not reopen it)"),
    }
}

/// Append one entry. Returns `true` when the file has grown past the cap and
/// the caller should rotate. Errors are warned once per occurrence rather
/// than propagated: losing an audit line must not fail the request that
/// produced it, but it must be visible.
fn append_line(sink: &mut Sink, entry: &AuditEntry) -> bool {
    if sink.file.is_none() {
        match open_append_0600(&sink.path) {
            Ok(f) => sink.file = Some(f),
            Err(e) => {
                warn!(error = %e, path = %sink.path.display(), "audit entry not persisted");
                return false;
            }
        }
    }
    let Some(file) = sink.file.as_mut() else {
        return false;
    };
    let mut line = match serde_json::to_vec(entry) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "could not serialise an audit entry");
            return false;
        }
    };
    line.push(b'\n');
    if let Err(e) = file.write_all(&line) {
        warn!(error = %e, "audit entry not persisted");
        // Force a reopen next time; a transient handle problem should not
        // silence the log forever.
        sink.file = None;
        return false;
    }
    sink.bytes += line.len() as u64;
    sink.bytes > MAX_AUDIT_FILE_BYTES
}

/// Read a persisted log, keeping at most `capacity` entries.
///
/// Returns the retained entries plus the hash they chain over: the recorded
/// `chain_seed` when nothing was dropped, otherwise the hash of the last
/// entry that was. Malformed lines are skipped — a truncated tail (the
/// unflushed remainder of a crashed process) is the expected way for this
/// file to end.
fn read_log(path: &Path, capacity: usize) -> (std::collections::VecDeque<AuditEntry>, String) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (
            std::collections::VecDeque::with_capacity(capacity),
            genesis(),
        );
    };
    let mut seed = genesis();
    let mut entries: Vec<AuditEntry> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(s) = serde_json::from_str::<ChainSeed>(line) {
            // Only the header line carries a seed, and only if nothing has
            // been read yet — a stray one later is not authoritative.
            if entries.is_empty() {
                seed = s.chain_seed;
            }
            continue;
        }
        match serde_json::from_str::<AuditEntry>(line) {
            Ok(e) => entries.push(e),
            Err(e) => warn!(error = %e, "skipping unreadable audit log line"),
        }
    }
    if entries.len() > capacity {
        let drop = entries.len() - capacity;
        seed = entries[drop - 1].hash.clone();
        entries.drain(..drop);
    }
    (entries.into(), seed)
}

#[cfg(unix)]
fn open_append_0600(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_append_0600(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Temp-file + rename, owner-only. Same shape as the API-key store's writer
/// (`engine::write_secret_file_atomic`): the mode is set before any bytes are
/// written so the contents are never briefly world-readable.
fn write_file_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn compute_hash(prev_hash: &str, entry: &AuditEntry) -> String {
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(entry.seq.to_le_bytes());
    h.update(entry.at_ms.to_le_bytes());
    h.update(entry.op.as_bytes());
    h.update(b"\0");
    h.update(entry.subject.as_bytes());
    h.update(b"\0");
    h.update(entry.resource.as_bytes());
    h.update(b"\0");
    h.update(entry.outcome.as_bytes());
    h.update(b"\0");
    h.update(entry.note.as_bytes());
    let bytes = h.finalize();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_chain_verifies() {
        let log = AuditLog::new(8);
        log.append("search", "alice", "logs-prod", "ok", "took=12 hits=3");
        log.append("delete", "alice", "logs-prod", "ok", "id=42");
        log.append("admin.role.put", "root", "_security/role/auditor", "ok", "");
        assert!(log.verify().is_ok());
        assert_eq!(log.snapshot().len(), 3);
        assert_eq!(log.next_seq(), 4);
    }

    #[test]
    fn tampering_detected() {
        let log = AuditLog::new(8);
        log.append("search", "alice", "x", "ok", "");
        log.append("delete", "bob", "x", "ok", "");
        log.append("admin", "root", "y", "ok", "");
        // Tamper with entry 2's `subject`.
        {
            let mut buf = log.buf.write();
            buf[1].subject = "mallory".into();
        }
        // Verifier should fail at seq 2 (the tampered entry's hash no
        // longer matches the recomputed hash from prev + tampered fields).
        let r = log.verify();
        assert!(r.is_err());
        let (seq, _expected, _actual) = r.unwrap_err();
        assert_eq!(seq, 2);
    }

    /// Rotation keeps the chain verifiable.
    ///
    /// This used to assert the opposite — that a ring which had dropped its
    /// head no longer verified — because `verify()` always reseeded from 64
    /// zeros. That made the verifier useless in practice: a node past 4096
    /// audited operations reported "tampered" forever, so a real break was
    /// indistinguishable from ordinary uptime. The dropped entry's hash is now
    /// retained as the chain seed, which is what the retained head actually
    /// chains over.
    #[test]
    fn ring_rotates_at_capacity_and_still_verifies() {
        let log = AuditLog::new(2);
        for i in 0..5 {
            log.append("op", "u", &format!("r{i}"), "ok", "");
        }
        let snap = log.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].resource, "r3");
        assert!(log.verify().is_ok(), "a rotated ring must still verify");

        // …and tamper-evidence still holds over the retained window.
        {
            let mut buf = log.buf.write();
            buf[0].subject = "mallory".into();
        }
        assert!(log.verify().is_err());
    }

    #[test]
    fn entries_survive_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        {
            let log = AuditLog::open(8, &path);
            log.append("search", "alice", "logs-prod", "ok", "hits=3");
            log.append(
                "security.api_key.create",
                "root",
                "_security/api_key",
                "ok",
                "id=1",
            );
        }
        let log = AuditLog::open(8, &path);
        let snap = log.snapshot();
        assert_eq!(snap.len(), 2, "entries must survive the restart");
        assert_eq!(snap[1].op, "security.api_key.create");
        assert_eq!(log.next_seq(), 3, "the sequence must continue, not restart");
        assert!(log.verify().is_ok());

        // The chain keeps extending across the restart boundary.
        log.append("delete", "root", "logs-prod", "ok", "id=7");
        assert!(log.verify().is_ok());
        assert_eq!(log.snapshot().len(), 3);
    }

    /// A restored log that had to drop entries (more on disk than the ring
    /// holds) still verifies, from the recorded seed.
    #[test]
    fn a_truncated_restore_still_verifies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        {
            let log = AuditLog::open(64, &path);
            for i in 0..10 {
                log.append("op", "u", &format!("r{i}"), "ok", "");
            }
        }
        let log = AuditLog::open(3, &path);
        let snap = log.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].resource, "r7");
        assert!(log.verify().is_ok());

        // On-disk tampering is still caught: rewrite one retained entry's
        // subject in the file and reopen.
        let text = std::fs::read_to_string(&path).expect("read");
        std::fs::write(
            &path,
            text.replace("\"subject\":\"u\"", "\"subject\":\"mallory\""),
        )
        .expect("write");
        let log = AuditLog::open(3, &path);
        assert!(log.verify().is_err(), "on-disk edits must break the chain");
    }

    /// The file must not grow without bound — every search appends to it.
    ///
    /// 1000 entries at ~150 B is far under `MAX_AUDIT_FILE_BYTES`, so this
    /// drives the rewrite directly rather than waiting 16 MB for the rotation
    /// trigger. What it pins is the property rotation relies on: the rewrite
    /// converges the file to the ring, and the converged file still restores
    /// and verifies.
    #[test]
    fn the_file_converges_to_the_ring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(4, &path);
        for i in 0..1000 {
            log.append("op", "u", &format!("r{i}"), "ok", "");
        }
        let grown = std::fs::read_to_string(&path).expect("read");
        assert_eq!(grown.lines().count(), 1001, "seed line + every append");

        log.rewrite_from_ring();
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            text.lines().count(),
            5,
            "file must converge to the seed line + the 4-entry ring"
        );
        assert!(log.verify().is_ok());
        let reopened = AuditLog::open(4, &path);
        assert!(reopened.verify().is_ok());
        assert_eq!(reopened.snapshot().len(), 4);
        assert_eq!(reopened.next_seq(), 1001);
    }

    /// Concurrent appends must land in the file in chain order. Writing the
    /// line outside the ring lock passes every single-threaded test here and
    /// still produces a log that fails to verify after a restart under load.
    #[test]
    fn concurrent_appends_persist_in_chain_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        {
            let log = AuditLog::open(512, &path);
            std::thread::scope(|s| {
                for t in 0..8 {
                    let log = &log;
                    s.spawn(move || {
                        for i in 0..50 {
                            log.append("search", "u", &format!("t{t}-{i}"), "ok", "");
                        }
                    });
                }
            });
            assert!(log.verify().is_ok(), "in-memory chain");
        }
        let reopened = AuditLog::open(512, &path);
        assert_eq!(reopened.snapshot().len(), 400);
        assert!(
            reopened.verify().is_ok(),
            "the restored chain must verify — file order must be chain order"
        );
    }

    /// A half-written tail — what a `kill -9` leaves behind — must not stop
    /// the rest of the log from loading.
    #[test]
    fn a_torn_tail_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        {
            let log = AuditLog::open(8, &path);
            log.append("search", "alice", "x", "ok", "");
            log.append("search", "bob", "y", "ok", "");
        }
        let mut text = std::fs::read_to_string(&path).expect("read");
        text.push_str("{\"seq\":3,\"at_ms\":1,\"op\":\"sea");
        std::fs::write(&path, text).expect("write");

        let log = AuditLog::open(8, &path);
        assert_eq!(log.snapshot().len(), 2);
        assert!(log.verify().is_ok());
    }
}
