//! Write-Ahead Log (WAL)
//!
//! xerj uses a single durability system: every mutation is appended to the WAL
//! *before* being applied to the in-memory index.  On restart, the WAL is
//! replayed from the last checkpoint to recover any un-flushed data.
//!
//! ## File format
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │  File header (16 bytes)                                  │
//! │    magic        [u8;4]  = "ZWAL"                         │
//! │    generation   u64     (little-endian)                  │
//! │    reserved     u32                                       │
//! ├──────────────────────────────────────────────────────────┤
//! │  Entry* (repeating)                                      │
//! │    entry_len    u32     (payload bytes, excl. header)    │
//! │    seq_no       u64                                       │
//! │    op           u8      (WalOpCode)                      │
//! │    payload      [u8; entry_len]  (serde_json)            │
//! │    crc32c       u32     (over seq_no+op+payload)         │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Checkpoint file (`<gen>.wchk`)
//!
//! Written after a successful segment flush **only when every entry of the
//! generation is provably durable** (see the RC4 W1 #8 note below).
//!
//! ```text
//!   generation  u64
//!   offset      u64   (byte offset into .wal file up to which data is durable)
//!   max_seq_no  u64   (highest seq_no covered by a flushed segment)
//!   checksum    u32   (crc32c of the above 24 bytes)
//! ```
//!
//! ## Generation rotation
//!
//! When the current WAL file exceeds `wal_max_size_bytes` the writer opens a
//! new file with `generation + 1`.  Old generations are deleted by
//! [`WalWriter::prune_verified`] once **every entry they hold** is verified
//! durable-or-superseded.
//!
//! ## Durability contract (RC4 Wave-1 blocker #8)
//!
//! A WAL byte may be destroyed (pruned) or ignored (skipped at replay)
//! **only** if the entry it belongs to is provably durable in a flushed
//! segment or superseded by a newer durable/replayable version.  Two
//! mechanisms enforce this:
//!
//! 1. **Replay replays everything.**  Checkpoints are no longer used to
//!    skip entries at replay: both replay consumers (the storage memtable
//!    rebuild and the engine FTS memtable rebuild) are idempotent — they
//!    check the segment-rebuilt version map and skip entries whose doc is
//!    already segment-resident at an equal-or-newer seq.  The pre-fix skip
//!    rule (`positionally-covered && seq_no <= checkpoint.max_seq_no`)
//!    silently discarded acked-but-unflushed entries whenever a checkpoint
//!    over-covered — e.g. the flush path checkpointing
//!    `current_seq_no()-1` while other shards' memtables still held acked
//!    docs with smaller seqs.  Live-verified as 50/50 acked-doc loss on
//!    kill -9 racing `_flush` (RC4 review, stream C).
//!
//! 2. **Prune verifies per entry.**  [`WalWriter::prune_verified`] decodes
//!    every entry of a candidate generation and deletes the file only if
//!    the caller-supplied verifier proves every single entry durable.  A
//!    generation holding even one acked-but-unflushed entry is retained.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher as Crc32Hasher;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// LZ4 compression for WAL payloads.
use lz4_flex::{compress_prepend_size, decompress_size_prepended};

use crate::{Result, SeqNo, StorageError};

// ── Constants ─────────────────────────────────────────────────────────────────

const WAL_MAGIC: &[u8; 4] = b"ZWAL";
const WAL_HEADER_LEN: u64 = 16; // magic(4) + generation(8) + reserved(4)

/// Frame overhead: entry_len(4) + seq_no(8) + op(1) + crc32(4).
const WAL_FRAME_OVERHEAD: usize = 17;

/// Sanity cap on a single frame's payload length.  Anything larger is
/// treated as framing corruption (a garbage length field would otherwise
/// make the scanner skip gigabytes past real frames).
const WAL_MAX_ENTRY_LEN: u32 = 1 << 30; // 1 GiB

/// BufWriter capacity shared by open/rotate/recovery reseats.
///
/// Every append drains this userspace buffer before acknowledging the write,
/// so a multi-megabyte capacity cannot batch across requests.  Keeping it at
/// a conventional I/O-buffer size bounds idle retention when every index owns
/// multiple WAL shards (the old 8 MiB capacity retained 1 GiB for 128 shards).
///
/// Capacity is an allocation/coalescing bound only: frame payloads may be
/// larger than `WAL_BUF_CAP` (up to `WAL_MAX_ENTRY_LEN`).  Rust's `BufWriter`
/// flushes and issues direct writes for oversized `write_all` slices, so
/// correctness of large records is independent of this constant.
const WAL_BUF_CAP: usize = 64 * 1024;

// Op codes
const OP_INDEX: u8 = 0x01;
const OP_DELETE: u8 = 0x02;
const OP_UPDATE_MAPPING: u8 = 0x03;

/// High bit of the op_code byte is set when the payload is LZ4-compressed.
/// This is backward-compatible: old uncompressed entries have op_codes 0x01–0x03
/// (high bit clear) and are read correctly by new code.
const OP_COMPRESSED_FLAG: u8 = 0x80;

// ── WalOpCode ─────────────────────────────────────────────────────────────────

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalOpCode {
    Index = OP_INDEX,
    Delete = OP_DELETE,
    UpdateMapping = OP_UPDATE_MAPPING,
}

impl WalOpCode {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            OP_INDEX => Some(Self::Index),
            OP_DELETE => Some(Self::Delete),
            OP_UPDATE_MAPPING => Some(Self::UpdateMapping),
            _ => None,
        }
    }
}

// ── WalEntry ──────────────────────────────────────────────────────────────────

/// An entry replayed from the WAL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalEntry {
    /// Document was indexed (or re-indexed) with the given source JSON.
    Index {
        doc_id: String,
        /// Raw JSON bytes of the document source.
        source: serde_json::Value,
    },
    /// Document was deleted.
    Delete { doc_id: String },
    /// Index mapping (schema) was updated.
    UpdateMapping { schema: serde_json::Value },
}

/// A WAL entry with its associated sequence number, as returned by replay.
#[derive(Debug, Clone)]
pub struct ReplayEntry {
    pub seq_no: SeqNo,
    pub entry: WalEntry,
    /// Byte offset of this entry in the file (useful for checkpointing).
    pub file_offset: u64,
}

// ── Sync mode ────────────────────────────────────────────────────────────────

/// Controls when `fsync` is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Call `fsync` after every append.  Safest; highest latency.
    Strict,
    /// Call `fsync` only when [`WalWriter::sync`] is explicitly invoked.
    /// Use with a background sync thread for high-throughput indexing.
    Batched,
}

// ── Checkpoint ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct WalCheckpoint {
    pub generation: u64,
    pub offset: u64,
    pub max_seq_no: SeqNo,
}

impl WalCheckpoint {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_u64::<LittleEndian>(self.generation)?;
        w.write_u64::<LittleEndian>(self.offset)?;
        w.write_u64::<LittleEndian>(self.max_seq_no)?;
        let mut h = Crc32Hasher::new();
        let mut buf = [0u8; 24];
        let mut c = std::io::Cursor::new(&mut buf[..]);
        c.write_u64::<LittleEndian>(self.generation).unwrap();
        c.write_u64::<LittleEndian>(self.offset).unwrap();
        c.write_u64::<LittleEndian>(self.max_seq_no).unwrap();
        h.update(&buf);
        w.write_u32::<LittleEndian>(h.finalize())?;
        Ok(())
    }

    fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let generation = r.read_u64::<LittleEndian>()?;
        let offset = r.read_u64::<LittleEndian>()?;
        let max_seq_no = r.read_u64::<LittleEndian>()?;
        let stored_crc = r.read_u32::<LittleEndian>()?;

        let mut h = Crc32Hasher::new();
        let mut buf = [0u8; 24];
        let mut c = std::io::Cursor::new(&mut buf[..]);
        c.write_u64::<LittleEndian>(generation).unwrap();
        c.write_u64::<LittleEndian>(offset).unwrap();
        c.write_u64::<LittleEndian>(max_seq_no).unwrap();
        h.update(&buf);
        let computed = h.finalize();
        if stored_crc != computed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "checkpoint CRC mismatch: expected {stored_crc:#010x}, got {computed:#010x}"
                ),
            ));
        }
        Ok(Self {
            generation,
            offset,
            max_seq_no,
        })
    }
}

// ── WalFile ──────────────────────────────────────────────────────────────────

/// Thin `File` wrapper the `BufWriter` sits on.  Exists for two reasons:
///
/// 1. Gives the RC4 W2 #13 recovery path a single place to reseat the
///    writer on a fresh fd after a torn write.
/// 2. Test-only fault injection: `fail_after` is a remaining-byte budget —
///    once it hits zero every write returns `ENOSPC`, and a write that
///    crosses the boundary is PARTIAL first (exactly how a filling disk
///    tears a frame).  Zero-cost in release builds (the field doesn't
///    exist and `write` delegates directly).
pub(crate) struct WalFile {
    file: File,
    #[cfg(test)]
    fail_after: Option<u64>,
}

impl WalFile {
    fn new(file: File) -> Self {
        Self {
            file,
            #[cfg(test)]
            fail_after: None,
        }
    }

    fn file(&self) -> &File {
        &self.file
    }
}

impl Write for WalFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(test)]
        {
            if let Some(budget) = self.fail_after {
                let allowed = (budget as usize).min(buf.len());
                if allowed == 0 && !buf.is_empty() {
                    // ENOSPC — "No space left on device"
                    return Err(io::Error::from_raw_os_error(28));
                }
                let n = self.file.write(&buf[..allowed])?;
                self.fail_after = Some(budget - n as u64);
                return Ok(n);
            }
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

// ── WalWriter ────────────────────────────────────────────────────────────────

/// Appends entries to the Write-Ahead Log.
///
/// This type is wrapped in `Arc<Mutex<...>>` inside [`crate::index_store::IndexStore`]
/// so multiple threads can serialize through it.  WAL writes are deliberately
/// **synchronous**: `append` takes a `&mut self` and does a blocking write so
/// the caller knows data is durable (in `Strict` mode) before returning.
///
/// ## Torn-frame recovery (RC4 W2 #13)
///
/// Every append path holds the invariant *"the on-disk file ends at a frame
/// boundary and the userspace buffer is empty between appends"*:
///
/// - each append drains the `BufWriter` before returning (Strict fsyncs,
///   Batched flushes to the kernel), so a frame is either fully in the
///   kernel or the append reports an error;
/// - on ANY write/flush/fsync error the writer runs
///   [`recover_after_write_error`](Self::recover_after_write_error): the
///   buffered torn bytes are DISCARDED (never flushed later), the file is
///   truncated back to the last good frame boundary, and the failed op is
///   NACKed.  If the truncate itself fails, the writer rotates to a fresh
///   generation (the old file keeps a torn TAIL, which replay tolerates);
///   if even that fails, the writer is poisoned — every subsequent append
///   fails fast (and re-attempts the fresh-generation heal) instead of
///   appending after a torn frame.
///
/// Pre-fix, a partial frame write (ENOSPC, EIO) left torn bytes MID-FILE
/// once later appends succeeded: replay stopped at the tear and silently
/// dropped every acked entry after it (live repro: `PUT a2` acked, gone
/// after restart; garbage frame headers even poisoned the seq counter via
/// the unvalidated open-time scan).
pub struct WalWriter {
    dir: PathBuf,
    generation: u64,
    /// The active generation's file handle and its `WAL_BUF_CAP` userspace
    /// buffer — **materialized on demand** (#873).
    ///
    /// `None` means the active generation is EMPTY: the file exists on disk
    /// (created by `open`/`rotate`, exactly as before) but holds nothing past
    /// its 16-byte header, so no append, replay, prune, checkpoint or tap path
    /// has anything to read from it. Holding an open descriptor and a 64 KiB
    /// buffer for that state is pure resident overhead, and it is the *normal*
    /// state of an idle index: every index opens with it, and every flush
    /// returns to it (`wal_maintain_all_verified` → `force_rotate`). With one
    /// shard per ingest shard — `(cpus/2).next_power_of_two()`, so 8 on a
    /// 16-core laptop and 16 here — that overhead is multiplied by the shard
    /// count and again by the number of open indices, which is what made a
    /// few hundred idle autoindex datasets cost hundreds of MB (#873).
    ///
    /// [`ensure_materialized`](Self::ensure_materialized) reopens the file for
    /// append at the first write; `rotate` drops it again. Nothing on disk
    /// changes: the file set, the generation numbering and the header bytes are
    /// byte-for-byte what they were before this became lazy.
    writer: Option<BufWriter<WalFile>>,
    /// Append position in the active generation. Always exactly
    /// `WAL_HEADER_LEN` while `writer` is `None` — that equality *is* the
    /// unmaterialized invariant, and it is what makes `checkpoint`,
    /// `force_rotate` and `rotate_if_large` no-op on an unmaterialized shard
    /// through the offset guards they already had.
    current_offset: u64,
    max_size_bytes: u64,
    sync_mode: SyncMode,
    /// True when bytes were appended since the last `fsync` — consumed by
    /// the `wal_batch_ms` fsync scheduler (RC4 W1 #9) so idle shards
    /// don't get pointless fsyncs.
    dirty: bool,
    /// Issue #334 — registration with the process-wide, event-driven fsync
    /// scheduler ([`crate::wal_fsync`]).  `Some` only when the owning store
    /// runs `wal_sync = "batched"` with `wal_batch_ms > 0`; `Strict` fsyncs
    /// inline and `Async` never fsyncs, so neither registers.
    ///
    /// This replaces the per-`IndexStore` fsync thread that made an idle
    /// node cost one OS thread and `1000 / wal_batch_ms` wakeups per second
    /// *per index*.
    sync_handle: Option<Arc<crate::wal_fsync::WalSyncHandle>>,
    /// Set when torn-frame recovery could neither truncate nor rotate
    /// (e.g. disk completely full).  Appends fail fast while set; each
    /// append first re-attempts the fresh-generation heal so the writer
    /// self-recovers once space frees up.
    poisoned: bool,
    /// Rotated generations a prune pass must keep even when every entry in
    /// them is proven durable — the retention floor a WAL consumer (the #320
    /// tap) needs to survive a target outage.
    ///
    /// Read by **both** prune paths: [`prune_verified`](Self::prune_verified)
    /// and — through [`min_retained_generations`](Self::min_retained_generations)
    /// — `IndexStore::wal_maintain_all_verified`, which is the one the engine
    /// actually runs. Enforcing it in only one of them is how this knob
    /// managed to be fully tested and still have no effect on a live node.
    ///
    /// `0` (the default) is the historical behaviour: retention never waits
    /// for anyone. Anything above it is a *bounded* floor, not a lease: the
    /// extra cost is at most `n * wal_max_size_mb` per shard whatever the
    /// consumer does, so a stalled consumer can never fill the disk. That
    /// bound is the whole difference from Elasticsearch's retention leases
    /// (`ReplicationTracker.addRetentionLease`), which hold translog
    /// generations for an unbounded time and are why a dead follower can
    /// wedge a leader's disk.
    min_retained_generations: u64,
    /// Shared sequence number counter — also used by the index store.
    pub seq_counter: Arc<AtomicU64>,
}

impl WalWriter {
    /// Userspace `BufWriter` capacity for this writer (test/instrumentation).
    ///
    /// Always equals [`WAL_BUF_CAP`] once materialized — after the first
    /// append, and after rotate, truncate-reseat and fresh-generation
    /// recovery.  Does not grow with record size.  `0` while the shard is
    /// unmaterialized (#873): an empty active generation holds no buffer at
    /// all, which is the property the size bound now rests on.
    #[cfg(test)]
    pub(crate) fn buffer_capacity(&self) -> usize {
        self.writer.as_ref().map_or(0, |w| w.capacity())
    }

    /// Whether this shard currently holds an open file descriptor and a
    /// userspace buffer for its active generation (#873).
    ///
    /// Exposed so the resident-cost property is assertable rather than
    /// argued: an index that has not been written to since its last flush
    /// must report `false` on every shard.
    pub fn is_materialized(&self) -> bool {
        self.writer.is_some()
    }

    /// Open the active generation's file for append and allocate its buffer,
    /// if that has not happened yet (#873). Idempotent.
    ///
    /// The file is expected to exist — `open` and `rotate` both create it —
    /// so the normal path is one `open(2)` in append mode. It is created here
    /// only as a defensive fallback (an operator deleting WAL files under a
    /// live node), which is strictly better than the alternative of failing an
    /// acknowledged write.
    fn ensure_materialized(&mut self) -> Result<()> {
        if self.writer.is_some() {
            return Ok(());
        }
        let path = wal_path(&self.dir, self.generation);
        let file = if path.exists() {
            OpenOptions::new().append(true).open(&path)?
        } else {
            warn!(
                generation = self.generation,
                "WAL active generation file was missing at first append — recreating it"
            );
            create_wal_file(&path, self.generation)?
        };
        self.writer = Some(BufWriter::with_capacity(WAL_BUF_CAP, WalFile::new(file)));
        Ok(())
    }

    /// The materialized writer. Every caller reaches this only after
    /// [`ensure_materialized`](Self::ensure_materialized) on the same
    /// `&mut self`, which is why it can be infallible.
    #[inline]
    fn writer(&mut self) -> &mut BufWriter<WalFile> {
        self.writer
            .as_mut()
            .expect("WAL writer materialized before use")
    }

    /// Open (or create) the WAL in `dir`.
    ///
    /// If a WAL file for the latest generation already exists it is opened for
    /// append; otherwise a new generation-0 file is created.
    ///
    /// RC4 W2 #13 — the active generation's tail is HEALED before any new
    /// append can land after it.  Pre-fix, `open` appended at the raw file
    /// length: a torn frame left by a crash/ENOSPC then sat MID-FILE under
    /// freshly-acked entries, and the next replay silently dropped every
    /// entry after the tear.  Now:
    ///
    /// - clean file → append at its end (unchanged);
    /// - torn TAIL (garbage after the last valid frame, no valid frame
    ///   beyond it) → truncate the garbage, append at the boundary;
    /// - mid-file corruption (valid frames RESUME after a bad region —
    ///   disk rot or a legacy-binary tear) → freeze the file untouched
    ///   (replay boundary-resync still recovers its parseable entries;
    ///   prune retains it) and start a fresh generation for appends.
    pub fn open(
        dir: impl AsRef<Path>,
        max_size_bytes: u64,
        sync_mode: SyncMode,
        seq_counter: Arc<AtomicU64>,
    ) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        // Find the highest existing generation
        let (generation, max_seq) = find_latest_generation(&dir)?;

        // Sync the seq counter with what was recovered from the WAL.  The
        // counter holds the NEXT seq to assign, so seed it one PAST the
        // highest valid frame (`max_seq` itself would make the next
        // append duplicate an existing seq — pre-fix this was masked by
        // the replay path's own `fetch_max(seq + 1)`).  `fetch_max`
        // rather than store: 16 shard writers share one counter.
        if max_seq > 0 {
            seq_counter.fetch_max(max_seq + 1, Ordering::AcqRel);
        }

        let path = wal_path(&dir, generation);
        let (generation, file, current_offset) = if path.exists() {
            match scan_wal_tail(&path) {
                Ok(WalTailScan::Clean { end }) => {
                    let f = OpenOptions::new().append(true).open(&path)?;
                    (generation, f, end)
                }
                Ok(WalTailScan::TornTail { last_good_end }) => {
                    // Truncate the torn bytes so appends land on a frame
                    // boundary.  Failure to truncate falls back to a
                    // fresh generation (the tear stays a TAIL tear).
                    match truncate_wal_file(&path, last_good_end) {
                        Ok(f) => {
                            warn!(
                                generation,
                                last_good_end,
                                "WAL open: healed torn tail (crash/ENOSPC leftover) by truncation"
                            );
                            (generation, f, last_good_end)
                        }
                        Err(e) => {
                            warn!(
                                generation,
                                error = %e,
                                "WAL open: torn-tail truncate failed — freezing generation, appending to a fresh one"
                            );
                            let next = generation + 1;
                            let f = create_wal_file(&wal_path(&dir, next), next)?;
                            (next, f, WAL_HEADER_LEN)
                        }
                    }
                }
                Ok(WalTailScan::MidFileCorruption) | Err(_) => {
                    // Valid frames resume after a bad region (or the file
                    // is unreadable): truncating would DESTROY acked
                    // entries.  Freeze the file for replay's boundary
                    // resync + prune retention; append to a fresh
                    // generation.
                    warn!(
                        generation,
                        "WAL open: mid-file corruption detected — freezing generation, appending to a fresh one"
                    );
                    let next = generation + 1;
                    let f = create_wal_file(&wal_path(&dir, next), next)?;
                    (next, f, WAL_HEADER_LEN)
                }
            }
        } else {
            let f = create_wal_file(&path, generation)?;
            (generation, f, WAL_HEADER_LEN)
        };

        // #873 — an active generation that holds nothing past its header is
        // the state every index opens in and every flush returns to. Drop the
        // descriptor and skip the 64 KiB buffer for it; the first append
        // reopens the same file for append at the same offset. The file itself
        // was created above exactly as before, so nothing on disk differs.
        let writer = if current_offset == WAL_HEADER_LEN {
            drop(file);
            None
        } else {
            Some(BufWriter::with_capacity(WAL_BUF_CAP, WalFile::new(file)))
        };

        Ok(Self {
            dir,
            generation,
            writer,
            current_offset,
            max_size_bytes,
            sync_mode,
            dirty: false,
            sync_handle: None,
            poisoned: false,
            min_retained_generations: 0,
            seq_counter,
        })
    }

    /// Set the retention floor: rotated generations a prune pass keeps even
    /// when it can prove every entry in them durable.
    ///
    /// Seeded by [`crate::index_store::IndexStore::open`] from
    /// `wal_tap.min_retained_generations`, and re-set **live** by
    /// [`IndexStore::set_wal_min_retained_generations`] when an operator
    /// changes it through `PUT /_xerj/wal_tap`.
    ///
    /// [`IndexStore::set_wal_min_retained_generations`]:
    /// crate::index_store::IndexStore::set_wal_min_retained_generations
    pub fn set_min_retained_generations(&mut self, n: u64) {
        self.min_retained_generations = n;
    }

    /// The retention floor currently in force on this shard.
    ///
    /// Exposed because the floor has to be enforced by the prune pass the
    /// engine actually runs (`IndexStore::wal_maintain_all_verified`), not
    /// only by [`prune_verified`](Self::prune_verified) — which no production
    /// path calls.
    pub fn min_retained_generations(&self) -> u64 {
        self.min_retained_generations
    }

    /// Install (or clear) this shard's registration with the batched-fsync
    /// scheduler.  Set once, right after the writer is placed in its `Arc`
    /// by [`crate::index_store::IndexStore::open`].
    pub fn set_sync_handle(&mut self, handle: Option<Arc<crate::wal_fsync::WalSyncHandle>>) {
        self.sync_handle = handle;
    }

    /// Mark the shard as holding un-fsynced bytes and, in batched mode, ask
    /// the shared scheduler for an fsync `wal_batch_ms` from now.
    ///
    /// LOAD-BEARING: this is the single chokepoint that every append path
    /// goes through, which is why the arming lives here rather than in the
    /// individual append methods — a future append path cannot forget to
    /// schedule its own durability.  Called under the shard mutex; `arm` is
    /// one atomic swap when the shard is already queued.
    #[inline]
    fn mark_dirty(&mut self) {
        self.dirty = true;
        if let Some(h) = &self.sync_handle {
            h.arm();
        }
    }

    /// Current sync mode (used by the batch path to temporarily suppress
    /// per-entry fsyncs and restore the original mode afterwards).
    pub fn sync_mode(&self) -> SyncMode {
        self.sync_mode
    }

    /// Override the sync mode. The caller is responsible for restoring the
    /// previous mode and for issuing the final fsync in Batched mode.
    pub fn set_sync_mode(&mut self, mode: SyncMode) {
        self.sync_mode = mode;
    }

    /// Append a single entry.  Returns the assigned sequence number.
    ///
    /// In `Strict` mode this flushes + fsyncs before returning.
    pub fn append(&mut self, entry: &WalEntry) -> Result<SeqNo> {
        self.ensure_writable()?;
        // Rotate if the file is too large
        if self.current_offset > self.max_size_bytes {
            self.rotate()?;
        }

        let seq_no = self.seq_counter.fetch_add(1, Ordering::AcqRel);

        let raw_payload = serde_json::to_vec(entry)?;
        let base_op_code = match entry {
            WalEntry::Index { .. } => OP_INDEX,
            WalEntry::Delete { .. } => OP_DELETE,
            WalEntry::UpdateMapping { .. } => OP_UPDATE_MAPPING,
        };

        // Compress payload with LZ4 (prepends the original size as u32 LE).
        // Only compress if it actually shrinks the data (usually it does for JSON).
        let compressed = compress_prepend_size(&raw_payload);
        let (payload, op_code) = if compressed.len() < raw_payload.len() {
            (compressed, base_op_code | OP_COMPRESSED_FLAG)
        } else {
            (raw_payload, base_op_code)
        };

        self.write_one_frame_recovered(seq_no, op_code, &payload)?;

        debug!(seq_no, op = op_code, "WAL append");
        Ok(seq_no)
    }

    /// Frame-write core shared by `append` / `append_index_raw`: CRC +
    /// framing + drain, wrapped in torn-frame recovery (RC4 W2 #13).  On
    /// error the WAL is restored to the frame boundary captured at entry
    /// and the error propagates (the op is NACKed but the log stays
    /// clean for every previously-acked entry).
    fn write_one_frame_recovered(
        &mut self,
        seq_no: SeqNo,
        op_code: u8,
        payload: &[u8],
    ) -> Result<()> {
        let frame_start = self.current_offset;
        let res = self.write_one_frame_inner(seq_no, op_code, payload);
        if let Err(e) = res {
            self.recover_after_write_error(frame_start, &e);
            return Err(e);
        }
        Ok(())
    }

    fn write_one_frame_inner(&mut self, seq_no: SeqNo, op_code: u8, payload: &[u8]) -> Result<()> {
        // CRC covers seq_no(8) + op(1) + payload(n)
        let mut hasher = Crc32Hasher::new();
        let mut seq_buf = [0u8; 8];
        (&mut seq_buf[..])
            .write_u64::<LittleEndian>(seq_no)
            .unwrap();
        hasher.update(&seq_buf);
        hasher.update(&[op_code]);
        hasher.update(payload);
        let crc = hasher.finalize();

        let entry_len = payload.len() as u32;
        let w = self.writer();
        w.write_u32::<LittleEndian>(entry_len)?;
        w.write_u64::<LittleEndian>(seq_no)?;
        w.write_u8(op_code)?;
        w.write_all(payload)?;
        w.write_u32::<LittleEndian>(crc)?;

        let written = WAL_FRAME_OVERHEAD as u64 + payload.len() as u64;
        self.current_offset += written;
        self.mark_dirty();

        if self.sync_mode == SyncMode::Strict {
            self.sync()?;
        } else {
            // Batched: drain the BufWriter to the kernel page cache so a
            // process drop / panic / SIGKILL leaves a recoverable WAL.
            // Skips fsync(2) — bytes survive process death (kernel keeps
            // the page cache) but not power loss until the next sync().
            // Mirrors what `wal_append_batch` does after its frame write.
            self.writer().flush()?;
        }
        Ok(())
    }

    /// V4 M4.8 — raw-bytes fast path for `WalEntry::Index`.
    ///
    /// Skips the `serde_json::to_vec(entry)` round-trip by assembling the
    /// on-disk JSON envelope directly from the caller's pre-formed source
    /// bytes.  Output is byte-identical to the legacy `append(Index { .. })`
    /// path, so replay code is unchanged.
    ///
    /// `source_bytes` MUST be a valid JSON object value (typically the raw
    /// bytes of one NDJSON line from an HTTP bulk body).  `doc_id` must be
    /// a safe UTF-8 string; we escape `"` and `\` for JSON correctness.
    ///
    /// Perf win: on the 60 k docs/s hot path, removing the `to_vec` and the
    /// upstream `serde_json::Value::clone()` it replaced accounts for ~30 %
    /// of per-doc CPU.
    pub fn append_index_raw(&mut self, doc_id: &str, source_bytes: &[u8]) -> Result<SeqNo> {
        self.ensure_writable()?;
        if self.current_offset > self.max_size_bytes {
            self.rotate()?;
        }

        let seq_no = self.seq_counter.fetch_add(1, Ordering::AcqRel);

        // Assemble `{"Index":{"doc_id":"<escaped>","source":<source_bytes>}}`
        // as a single byte buffer.  Order of keys matches serde_json's default
        // derive so replay via `serde_json::from_slice::<WalEntry>(..)` works.
        let mut raw_payload: Vec<u8> = Vec::with_capacity(source_bytes.len() + doc_id.len() + 64);
        raw_payload.extend_from_slice(br#"{"Index":{"doc_id":""#);
        for &b in doc_id.as_bytes() {
            match b {
                b'"' => raw_payload.extend_from_slice(br#"\""#),
                b'\\' => raw_payload.extend_from_slice(br#"\\"#),
                b'\n' => raw_payload.extend_from_slice(br"\n"),
                b'\r' => raw_payload.extend_from_slice(br"\r"),
                b'\t' => raw_payload.extend_from_slice(br"\t"),
                0x00..=0x1f => {
                    raw_payload.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
                }
                _ => raw_payload.push(b),
            }
        }
        raw_payload.extend_from_slice(br#"","source":"#);
        raw_payload.extend_from_slice(source_bytes);
        raw_payload.extend_from_slice(b"}}");

        let base_op_code = OP_INDEX;

        // Skip LZ4 for small entries.  Compressing 500-byte log lines
        // at 77 k docs/s costs ~1 full core of CPU time (~8 µs per
        // `compress_prepend_size` call) for a ~40 % byte-level win
        // that's irrelevant when the WAL is 38 MB/s on a 3 GB/s NVMe.
        // On large bulk payloads (> 1 KB) LZ4 still beats raw writes
        // handily because the compressor can match across fields, so
        // the threshold is conservative.
        const WAL_LZ4_MIN: usize = 1024;
        let (payload, op_code) = if raw_payload.len() >= WAL_LZ4_MIN {
            let compressed = compress_prepend_size(&raw_payload);
            if compressed.len() < raw_payload.len() {
                (compressed, base_op_code | OP_COMPRESSED_FLAG)
            } else {
                (raw_payload, base_op_code)
            }
        } else {
            (raw_payload, base_op_code)
        };

        // Durability consistency with `append` (RC4 W1 #8/#9): the frame
        // core drains the BufWriter to the kernel page cache before the
        // caller acks, so a SIGKILL cannot lose an acked single-doc
        // append that was still sitting in the userspace buffer.  Torn
        // writes are rolled back by the shared recovery (RC4 W2 #13).
        self.write_one_frame_recovered(seq_no, op_code, &payload)?;
        Ok(seq_no)
    }

    /// Write a pre-assembled batch of framed bytes.  Caller must hold
    /// the WAL mutex.  `total_written` is the byte count to add to
    /// `current_offset`.
    ///
    /// RC4 W2 #13 — Batched mode now DRAINS the BufWriter before
    /// returning (Strict already fsynced).  This makes the caller's
    /// follow-up `soft_flush()` a no-op and, crucially, keeps the
    /// "buffer empty between appends" invariant the torn-frame recovery
    /// relies on: an error anywhere in this batch rolls the WAL back to
    /// the batch's start boundary and NACKs the whole batch.
    pub fn append_frames_locked(&mut self, frames: &[u8], total_written: u64) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        self.ensure_writable()?;
        if self.current_offset > self.max_size_bytes {
            self.rotate()?;
        }
        let frame_start = self.current_offset;
        let res: Result<()> = (|| {
            self.writer().write_all(frames)?;
            self.current_offset += total_written;
            self.mark_dirty();
            if self.sync_mode == SyncMode::Strict {
                self.sync()?;
            } else {
                self.writer().flush()?;
            }
            Ok(())
        })();
        if let Err(e) = res {
            self.recover_after_write_error(frame_start, &e);
            return Err(e);
        }
        Ok(())
    }

    /// M5.5 — batched raw-bytes write.
    ///
    /// Caller passes pre-built `{"Index":{"doc_id":"...","source":<bytes>}}`
    /// payloads — the envelope build (allocation + escape loop + source
    /// extend) is done OUTSIDE the WAL mutex so only CRC + write happens
    /// while holding the lock.  All frames are assembled into a single
    /// buffer and emitted with one `write_all`, replacing the previous
    /// 5 small BufWriter calls per doc.
    ///
    /// Returns the first assigned seq_no (subsequent docs are +0, +1, ...).
    pub fn append_index_raw_batch(&mut self, pre_built: &[Vec<u8>]) -> Result<Vec<SeqNo>> {
        if pre_built.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_writable()?;
        if self.current_offset > self.max_size_bytes {
            self.rotate()?;
        }

        // Atomically reserve a contiguous range of seq_nos for this batch.
        let n = pre_built.len() as u64;
        let start_seq = self.seq_counter.fetch_add(n, Ordering::AcqRel);

        // Pre-allocate the output buffer (frame overhead: 4+8+1+4 = 17 bytes).
        let total_bytes: usize = pre_built.iter().map(|p| p.len() + 17).sum();
        let mut out: Vec<u8> = Vec::with_capacity(total_bytes);

        let mut seq_nos = Vec::with_capacity(pre_built.len());
        let mut written_total: u64 = 0;

        const WAL_LZ4_MIN: usize = 1024;
        let mut comp_scratch: Vec<u8> = Vec::new();

        for (i, raw_payload) in pre_built.iter().enumerate() {
            let seq_no = start_seq + i as u64;
            seq_nos.push(seq_no);

            // Compress if large — matches per-doc append_index_raw semantics.
            let (payload_slice, op_code): (&[u8], u8) = if raw_payload.len() >= WAL_LZ4_MIN {
                comp_scratch.clear();
                let compressed = compress_prepend_size(raw_payload);
                if compressed.len() < raw_payload.len() {
                    comp_scratch = compressed;
                    (&comp_scratch, OP_INDEX | OP_COMPRESSED_FLAG)
                } else {
                    (&raw_payload[..], OP_INDEX)
                }
            } else {
                (&raw_payload[..], OP_INDEX)
            };

            // CRC over seq(8) + op(1) + payload.
            let mut hasher = Crc32Hasher::new();
            let mut seq_buf = [0u8; 8];
            (&mut seq_buf[..])
                .write_u64::<LittleEndian>(seq_no)
                .unwrap();
            hasher.update(&seq_buf);
            hasher.update(&[op_code]);
            hasher.update(payload_slice);
            let crc = hasher.finalize();

            let entry_len = payload_slice.len() as u32;
            out.extend_from_slice(&entry_len.to_le_bytes());
            out.extend_from_slice(&seq_buf);
            out.push(op_code);
            out.extend_from_slice(payload_slice);
            out.extend_from_slice(&crc.to_le_bytes());

            written_total += 4 + 8 + 1 + payload_slice.len() as u64 + 4;
        }

        // Single write_all — one BufWriter call for the whole batch.
        // Drain in Batched mode too (see `append_frames_locked`); errors
        // roll the WAL back to the batch boundary and NACK the batch.
        let frame_start = self.current_offset;
        let res: Result<()> = (|| {
            self.writer().write_all(&out)?;
            self.current_offset += written_total;
            self.mark_dirty();
            if self.sync_mode == SyncMode::Strict {
                self.sync()?;
            } else {
                self.writer().flush()?;
            }
            Ok(())
        })();
        if let Err(e) = res {
            self.recover_after_write_error(frame_start, &e);
            return Err(e);
        }
        Ok(seq_nos)
    }

    /// Flush the write buffer and call `fsync`.
    ///
    /// A no-op on an unmaterialized shard (#873): its active generation holds
    /// nothing past the header it was created with and already fsynced, and
    /// there is no buffer to drain — so there is nothing an fsync could make
    /// durable that is not durable already.
    pub fn sync(&mut self) -> Result<()> {
        let Some(w) = self.writer.as_mut() else {
            self.dirty = false;
            return Ok(());
        };
        w.flush()?;
        w.get_ref().file().sync_all()?;
        self.dirty = false;
        Ok(())
    }

    // ── Torn-frame recovery (RC4 W2 #13) ─────────────────────────────────

    /// Fail-fast gate at the top of every append path.  A poisoned writer
    /// (recovery exhausted both truncate and rotate — e.g. disk full)
    /// re-attempts the fresh-generation heal on every call so the WAL
    /// self-recovers the moment space frees up; until then appends error
    /// instead of writing after a torn frame.
    fn ensure_writable(&mut self) -> Result<()> {
        if !self.poisoned {
            // #873 — the one chokepoint every append path already goes
            // through, which is why materialization is armed here rather than
            // in each append method: a future append path cannot forget to
            // open its own file. `rotate` deliberately re-seats the writer
            // itself, so the rotate-on-size check that follows every call to
            // this gate cannot leave the writer unmaterialized.
            return self.ensure_materialized();
        }
        if self.try_reseat_fresh_generation() {
            warn!(
                generation = self.generation,
                "WAL writer self-healed onto a fresh generation after earlier unrecoverable write error"
            );
            return Ok(());
        }
        Err(StorageError::WalCorrupt(
            self.current_offset,
            "WAL writer poisoned by an unrecoverable write error (disk full?); \
             append refused to avoid tearing the log"
                .to_string(),
        ))
    }

    /// Restore the invariant "file ends at a frame boundary, buffer is
    /// empty" after a failed append/flush/fsync:
    ///
    /// 1. Reseat the BufWriter on a fresh dup of the fd, DISCARDING any
    ///    buffered torn bytes (`into_parts` — never flushed), and truncate
    ///    the file back to `frame_start` (the last good boundary captured
    ///    before the failed frame started).
    /// 2. If the truncate fails, rotate to a brand-new generation — the
    ///    old file keeps at worst a torn TAIL, which replay tolerates and
    ///    open() heals.
    /// 3. If even that fails, poison the writer (appends fail fast and
    ///    keep re-trying step 2 via `ensure_writable`).
    fn recover_after_write_error(&mut self, frame_start: u64, cause: &StorageError) {
        let reseat: io::Result<WalFile> = (|| {
            // Only reachable from a failed append, which materialized the
            // writer before it wrote (#873) — but recovery is the last thing
            // that should panic, so an unmaterialized shard falls through to
            // the fresh-generation heal below instead.
            let Some(w) = self.writer.as_ref() else {
                return Err(io::Error::other("WAL writer not materialized"));
            };
            let dup = w.get_ref().file().try_clone()?;
            // Defensive min(): under the drain-per-append invariant the
            // file can only be AT or PAST the boundary; never extend it
            // (set_len past EOF would zero-fill — manufactured garbage).
            let len = dup.metadata()?.len();
            let target = frame_start.min(len);
            dup.set_len(target)?;
            dup.sync_all()?;
            Ok(WalFile::new(dup))
        })();
        match reseat {
            Ok(fresh) => {
                let old = self
                    .writer
                    .replace(BufWriter::with_capacity(WAL_BUF_CAP, fresh));
                // Discard buffered torn bytes WITHOUT flushing them.
                if let Some(old) = old {
                    let _ = old.into_parts();
                }
                self.current_offset = frame_start;
                self.poisoned = false;
                warn!(
                    generation = self.generation,
                    frame_start,
                    error = %cause,
                    "WAL append failed — torn frame discarded, log truncated to last good frame boundary"
                );
                return;
            }
            Err(e) => {
                warn!(
                    generation = self.generation,
                    error = %e,
                    "WAL torn-frame truncate failed — trying a fresh generation"
                );
            }
        }
        if self.try_reseat_fresh_generation() {
            warn!(
                generation = self.generation,
                error = %cause,
                "WAL append failed — recovered onto a fresh generation (old tail tear is replay-safe)"
            );
        } else {
            self.poisoned = true;
            tracing::error!(
                generation = self.generation,
                error = %cause,
                "WAL unrecoverable after write error — writer poisoned; appends fail fast until a heal succeeds"
            );
        }
    }

    /// Step 2/3 of recovery: seat the writer on a brand-new generation
    /// file, discarding whatever the old BufWriter still buffered.
    /// Returns false when even creating the new file fails (disk full).
    fn try_reseat_fresh_generation(&mut self) -> bool {
        let next = self.generation + 1;
        match create_wal_file(&wal_path(&self.dir, next), next) {
            Ok(f) => {
                self.generation = next;
                let old = self
                    .writer
                    .replace(BufWriter::with_capacity(WAL_BUF_CAP, WalFile::new(f)));
                if let Some(old) = old {
                    let _ = old.into_parts();
                }
                self.current_offset = WAL_HEADER_LEN;
                self.poisoned = false;
                true
            }
            Err(_) => false,
        }
    }

    /// Test-only ENOSPC injection: remaining-byte budget on the underlying
    /// file — writes crossing it are partial-then-error, exactly like a
    /// filling disk.  `None` clears the fault.
    #[cfg(test)]
    pub(crate) fn inject_write_fault(&mut self, budget: Option<u64>) {
        self.ensure_materialized()
            .expect("materialize WAL for fault injection");
        self.writer().get_mut().fail_after = budget;
    }

    /// True when bytes were appended since the last `fsync`.  Consumed by
    /// the `wal_batch_ms` background fsync loop so idle shards are not
    /// pointlessly fsynced (RC4 W1 #9).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Flush the BufWriter to the kernel without a `fsync(2)`.
    ///
    /// The buffered bytes are visible to any reader on the same
    /// machine immediately (they're in the kernel page cache) but
    /// are not guaranteed durable across a kernel crash or power
    /// loss until the next `sync` / file close / background
    /// writeback completes.  Used by the bulk ingest hot path to
    /// avoid the ~1 ms per-batch `fsync` round-trip on NVMe —
    /// each successful segment flush issues its own `fsync` on
    /// the committed `.seg` file, so acknowledged-and-flushed
    /// documents are still durable.
    pub fn soft_flush(&mut self) -> Result<()> {
        // Unmaterialized ⇒ no buffer, nothing to drain (#873).
        if let Some(w) = self.writer.as_mut() {
            w.flush()?;
        }
        Ok(())
    }

    /// Write a checkpoint file for the current generation.
    pub fn checkpoint(&mut self, max_seq_no: SeqNo) -> Result<()> {
        // STALE-CHECKPOINT FIX (2026-07, S3): never checkpoint an EMPTY
        // active generation.  There is nothing to cover (the previous
        // generation's checkpoint, if any, still exists), and because
        // `force_rotate` deliberately no-ops on empty generations, the
        // .wchk written here would sit on the very generation that
        // receives FUTURE appends.  Combined with a restart-regressed seq
        // counter that state made `WalReader::replay` discard every
        // post-restart acked op (seq_no <= checkpoint.max_seq_no) — byte-
        // verified as wchk=(offset=16, max_seq_no=X) next to tail entries
        // seq 1..K in the same shard file, 100% tail loss.
        if self.current_offset <= WAL_HEADER_LEN {
            return Ok(());
        }
        self.sync()?;
        let chkpt = WalCheckpoint {
            generation: self.generation,
            offset: self.current_offset,
            max_seq_no,
        };
        let path = checkpoint_path(&self.dir, self.generation);
        let mut f = File::create(&path)?;
        chkpt.write_to(&mut f)?;
        f.sync_all()?;
        // debug, not info: a checkpoint is written per WAL (16 shards × every
        // index) on each flush and again on shutdown drain — at info this
        // floods the console with hundreds of identical lines on Ctrl-C.
        debug!(
            generation = self.generation,
            max_seq_no, "WAL checkpoint written"
        );
        Ok(())
    }

    /// Current active generation number.
    pub fn active_generation(&self) -> u64 {
        self.generation
    }

    /// Read every decodable entry of generation `gen` from disk.
    ///
    /// Returns `(entries, clean)` where `clean == true` means the file was
    /// decoded to the end without a single framing/CRC/JSON error.  Callers
    /// verifying prunability MUST treat `clean == false` as "cannot prove
    /// anything about the undecodable tail" and retain the generation.
    ///
    /// For the ACTIVE generation the caller must hold the WAL mutex (this
    /// is a `&self` read of an append-only file; the writer soft-flushes
    /// on every append so the on-disk prefix is complete up to
    /// `current_offset`).
    pub fn read_generation_entries(&self, gen: u64) -> (Vec<ReplayEntry>, bool) {
        let path = wal_path(&self.dir, gen);
        if !path.exists() {
            return (Vec::new(), true);
        }
        let results = read_wal_file(path);
        let clean = results.iter().all(|r| r.is_ok());
        let entries = results.into_iter().filter_map(|r| r.ok()).collect();
        (entries, clean)
    }

    /// RC4 W1 #8 — remove WAL generations whose entries are **all verified
    /// durable-or-superseded** by the caller-supplied predicate.
    ///
    /// This replaces the pre-fix `prune()` whose rule was "gen < active &&
    /// gen has a checkpoint file".  That rule destroyed acked-but-unflushed
    /// entries: the flush path checkpointed every shard with a *global*
    /// max_seq (`current_seq_no()-1`, or a sibling shard's segment max)
    /// and a full-file offset, so a generation still holding entries whose
    /// docs lived only in the memtable got a checkpoint and was deleted —
    /// kill -9 then lost every one of those acked docs (live-verified
    /// 50/50 loss).
    ///
    /// New rule: decode every entry of every `gen < active_generation` and
    /// delete the file only when
    ///   1. the file decodes cleanly end-to-end (no torn/corrupt frames), and
    ///   2. `verify(entry, seq_no)` returns true for EVERY entry.
    ///
    /// The verifier is supplied by `IndexStore` and proves an entry durable
    /// via the version map (doc segment-resident at `>=` seq, superseded by
    /// a newer retained version, or tombstoned by a retained delete).
    ///
    /// ENOENT on `remove_file` is benign: sharded flushes run in parallel
    /// and every one of them prunes ALL WAL shards; two concurrent
    /// maintenance passes can race to delete the same file — the loser
    /// gets NotFound.
    ///
    /// Returns the number of generations pruned.
    pub fn prune_verified(&self, verify: &dyn Fn(&WalEntry, SeqNo) -> bool) -> Result<usize> {
        let mut pruned = 0usize;
        let rotated = self.rotated_generations()?;
        // Retention floor (#320): keep the newest `min_retained_generations`
        // rotated files whatever the verifier says, so a WAL consumer that is
        // briefly behind — a tap whose target is down for a minute — still
        // finds its entries. `rotated_generations` is sorted ascending, so
        // dropping the tail keeps the NEWEST ones.
        let keep = self.min_retained_generations.min(rotated.len() as u64) as usize;
        let prunable = rotated.len() - keep;
        if keep > 0 {
            debug!(
                keep,
                rotated = rotated.len(),
                "WAL retention floor: holding the newest rotated generations for consumers"
            );
        }
        for gen in rotated.into_iter().take(prunable) {
            let (gen_entries, clean) = self.read_generation_entries(gen);
            if !clean {
                // Undecodable bytes — cannot prove them durable; keep the
                // generation.  (Torn tails from a previous crash park here
                // until the Wave-2 torn-tail truncation lands.)
                debug!(gen, "WAL generation retained: undecodable entries");
                continue;
            }
            let all_durable = gen_entries.iter().all(|e| verify(&e.entry, e.seq_no));
            if !all_durable {
                debug!(
                    gen,
                    entries = gen_entries.len(),
                    "WAL generation retained: holds acked-but-unflushed entries"
                );
                continue;
            }
            if self.delete_generation(gen)? {
                pruned += 1;
                debug!(gen, entries = gen_entries.len(), "pruned WAL generation");
            }
        }
        Ok(pruned)
    }

    /// List every on-disk generation strictly older than the active one
    /// (i.e. rotated, frozen files — the prune candidates).
    pub fn rotated_generations(&self) -> Result<Vec<u64>> {
        let active_gen = self.generation;
        let mut gens: Vec<u64> = fs::read_dir(&self.dir)?
            .flatten()
            .filter_map(|e| parse_wal_generation(&e.file_name().to_string_lossy()))
            .filter(|&g| g < active_gen)
            .collect();
        gens.sort_unstable();
        Ok(gens)
    }

    /// Delete a generation's `.wal` file (and its stale `.wchk`).  Returns
    /// `Ok(true)` when this call removed the file, `Ok(false)` when a
    /// sibling maintenance pass got there first (ENOENT — benign: sharded
    /// flushes prune all WAL shards in parallel).
    pub fn delete_generation(&self, gen: u64) -> Result<bool> {
        let removed = match fs::remove_file(wal_path(&self.dir, gen)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(gen, "WAL file already pruned by sibling flush — ok");
                false
            }
            Err(e) => return Err(e.into()),
        };
        let _ = fs::remove_file(checkpoint_path(&self.dir, gen));
        Ok(removed)
    }

    /// Close the active generation and start the next one, seated and ready
    /// to be appended to.
    ///
    /// This one deliberately leaves the writer MATERIALIZED (#873): its
    /// size-driven callers are mid-append and write into the new generation
    /// immediately. Releasing an empty generation is
    /// [`force_rotate`](Self::force_rotate)'s job — that is the post-flush
    /// path, where no write follows.
    fn rotate(&mut self) -> Result<()> {
        self.sync()?;
        // Bump the generation only AFTER the new file exists: bumping
        // first meant a failed create left `generation` pointing past the
        // file still being appended — `rotated_generations()` then listed
        // the ACTIVE file as a prune candidate.
        let next = self.generation + 1;
        let path = wal_path(&self.dir, next);
        let file = create_wal_file(&path, next)?;
        self.generation = next;
        self.writer = Some(BufWriter::with_capacity(WAL_BUF_CAP, WalFile::new(file)));
        self.current_offset = WAL_HEADER_LEN;
        debug!(
            generation = self.generation,
            "WAL rotated to new generation"
        );
        Ok(())
    }

    /// V4 M4 — rotate iff the current generation is big enough to
    /// make rotation worthwhile.
    ///
    /// Called by `IndexStore::finalize_flush_with_publisher` after a
    /// checkpoint.  Rotating on EVERY flush was too eager — it added
    /// an extra fsync per flush and caused ingest regression from
    /// ~33 k → ~8 k docs/s.  Only rotate when the current generation
    /// is > 64 MB so the churn amortises across many flushes.
    pub fn rotate_if_large(&mut self, threshold_bytes: u64) -> Result<bool> {
        if self.current_offset >= threshold_bytes {
            self.rotate()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Force a rotation even if the current generation is below the
    /// `rotate_if_large` threshold.  Matches Elasticsearch's
    /// `_flush`-time translog rollover: every user-triggered flush
    /// closes the current generation and starts a fresh one, so the
    /// post-flush prune can reclaim the entries that are now durably
    /// in segments.  Without this, a workload that bulk-ingests a few
    /// MB and calls `_flush` keeps the WAL bytes around forever
    /// (XERJ measured 13 MB resident WAL after a 100 k-doc bulk +
    /// `_flush` on the 2026-04-25 head-to-head, vs ES's 55-byte
    /// post-flush translog header).  Used only on the explicit
    /// `force_wal_maintenance` path — the periodic background tick
    /// still uses `rotate_if_large(64 MB)` to amortise rotation
    /// churn during sustained ingest.
    pub fn force_rotate(&mut self) -> Result<()> {
        // Only rotate if the current gen has any actual data past the
        // header — if the user calls `_flush` twice in a row with no
        // writes between, the second one is a no-op.
        if self.current_offset > WAL_HEADER_LEN {
            self.rotate()?;
        }
        // #873 — this is the post-flush path (`wal_maintain_all_verified`
        // calls it on every shard of every flushed index), so the generation
        // it leaves active is empty by construction. Release its descriptor
        // and its 64 KiB buffer: an index that stops being written to must
        // stop paying for a write buffer, which — multiplied by the ingest
        // shard count and by hundreds of small indices — is a large part of
        // the resident floor a quiet node was carrying.
        self.release_if_empty();
        Ok(())
    }

    /// Drop the active generation's descriptor and userspace buffer when that
    /// generation holds nothing past its header (#873).
    ///
    /// Safe to call at any quiescent point: `current_offset == WAL_HEADER_LEN`
    /// means no frame has been appended to this generation, and every append
    /// drains the buffer before it returns, so there is nothing buffered to
    /// lose. The file stays on disk; [`ensure_materialized`](Self::ensure_materialized)
    /// reopens it for append at the same offset.
    fn release_if_empty(&mut self) {
        if self.current_offset == WAL_HEADER_LEN {
            self.writer = None;
            self.dirty = false;
        }
    }
}

/// M5.8 — lock-free WAL frame builder.
///
/// Reserves a contiguous range of seq_nos via fetch_add on the shared
/// seq counter, then assembles the fully-framed on-disk bytes (len,
/// seq, op, payload, crc) for every doc.  Callers then acquire the
/// WAL mutex and emit the resulting Vec in a single `write_all`
/// (see `WalWriter::append_frames_locked`).
///
/// This function runs 100% OUTSIDE the WAL mutex, so multiple concurrent
/// batches build their frames in parallel on their tokio worker thread
/// rather than queueing behind the writer.
pub fn wal_build_frames_lockfree(
    seq_counter: &Arc<AtomicU64>,
    pre_built: &[Vec<u8>],
) -> (Vec<SeqNo>, Vec<u8>, u64) {
    if pre_built.is_empty() {
        return (Vec::new(), Vec::new(), 0);
    }
    let n = pre_built.len() as u64;
    let start_seq = seq_counter.fetch_add(n, Ordering::AcqRel);

    let total_bytes: usize = pre_built.iter().map(|p| p.len() + 17).sum();
    let mut out: Vec<u8> = Vec::with_capacity(total_bytes);
    let mut seq_nos = Vec::with_capacity(pre_built.len());

    for (i, raw_payload) in pre_built.iter().enumerate() {
        let seq_no = start_seq + i as u64;
        seq_nos.push(seq_no);

        // Skip compression on the hot path — at 600 k docs/s the LZ4
        // cost eats a full core; NVMe has plenty of bandwidth.
        let payload_slice: &[u8] = &raw_payload[..];
        let op_code: u8 = OP_INDEX;

        let mut hasher = Crc32Hasher::new();
        let mut seq_buf = [0u8; 8];
        (&mut seq_buf[..])
            .write_u64::<LittleEndian>(seq_no)
            .unwrap();
        hasher.update(&seq_buf);
        hasher.update(&[op_code]);
        hasher.update(payload_slice);
        let crc = hasher.finalize();

        let entry_len = payload_slice.len() as u32;
        out.extend_from_slice(&entry_len.to_le_bytes());
        out.extend_from_slice(&seq_buf);
        out.push(op_code);
        out.extend_from_slice(payload_slice);
        out.extend_from_slice(&crc.to_le_bytes());
    }

    let total_written = out.len() as u64;
    (seq_nos, out, total_written)
}

// ── WalReader ────────────────────────────────────────────────────────────────

/// Replays WAL entries from a directory, starting from the last checkpoint.
pub struct WalReader {
    dir: PathBuf,
}

impl WalReader {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    /// Return an iterator over **every** entry in every surviving WAL
    /// generation, ordered by sequence number within each file.
    ///
    /// RC4 W1 #8 — replay no longer consults checkpoints to skip entries.
    /// Both replay consumers (`IndexStore::replay_wal` and the engine FTS
    /// memtable rebuild) are idempotent: they skip entries whose doc is
    /// already segment-resident at an equal-or-newer seq via the
    /// segment-rebuilt version map.  Skipping at the WAL layer, by
    /// contrast, silently DISCARDED acked-but-unflushed entries whenever a
    /// checkpoint over-covered (stale checkpoints, global max_seq written
    /// onto per-shard files, full-file offsets recorded while sibling
    /// memtables still held acked docs).  Surviving generations are exactly
    /// the not-yet-verified-durable set (see `prune_verified`), so the
    /// replay cost of reading them in full is proportional to the data
    /// that genuinely needs recovery.
    pub fn replay(&self) -> Result<impl Iterator<Item = Result<ReplayEntry>> + '_> {
        let mut gens: Vec<u64> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| parse_wal_generation(&e.file_name().to_string_lossy()))
            .collect();
        gens.sort_unstable();

        let dir = self.dir.clone();
        let iter = gens.into_iter().flat_map(move |gen| {
            let path = wal_path(&dir, gen);
            read_wal_file(path)
        });

        Ok(iter)
    }
}

/// Discover every WAL directory under `wal_root` and return all replayable
/// entries merge-sorted by `seq_no`.
///
/// Two layouts are supported, matching what `IndexStore` writes:
/// - legacy single-WAL: `*.wal` files directly in `wal_root`
/// - sharded WAL: `wal_root/s{N}/` subdirectories, one WAL stream per
///   ingest shard
///
/// Sorting globally by `seq_no` is required for correctness: a delete of a
/// document can live in a different shard stream than a later re-index of
/// the same id, so per-directory order alone must not be trusted. Corrupt
/// entries are skipped with a warning, matching single-dir replay.
pub fn replay_all_sorted(wal_root: &Path) -> Vec<ReplayEntry> {
    let mut wal_dirs: Vec<PathBuf> = Vec::new();
    // Legacy layout: .wal files directly in the root.
    if fs::read_dir(wal_root)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| e.path().extension().map(|x| x == "wal").unwrap_or(false))
        })
        .unwrap_or(false)
    {
        wal_dirs.push(wal_root.to_path_buf());
    }
    // Sharded layout: s0, s1, … subdirectories.
    if let Ok(rd) = fs::read_dir(wal_root) {
        for entry in rd.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('s')
                && name_str[1..].parse::<usize>().is_ok()
                && entry.path().is_dir()
            {
                wal_dirs.push(entry.path());
            }
        }
    }

    let mut all_entries: Vec<ReplayEntry> = Vec::new();
    for dir in &wal_dirs {
        let reader = WalReader::new(dir);
        let iter = match reader.replay() {
            Ok(it) => it,
            Err(e) => {
                warn!(error = %e, ?dir, "failed to open WAL for replay");
                continue;
            }
        };
        for result in iter {
            match result {
                Ok(e) => all_entries.push(e),
                Err(e) => {
                    warn!(error = %e, ?dir, "skipping corrupt WAL entry during replay");
                }
            }
        }
    }
    all_entries.sort_by_key(|e| e.seq_no);
    all_entries
}

// ── Frame-level parsing (shared by replay, tail-heal, seq scan) ──────────────

/// Outcome of parsing one raw frame at `off` in a fully-read WAL file.
enum RawFrame<'a> {
    /// `off` is exactly the end of the buffer — clean end of file.
    End,
    /// A CRC-valid frame: `(seq_no, op_code, payload, end_offset)`.
    Frame {
        seq_no: SeqNo,
        op_code: u8,
        payload: &'a [u8],
        end: usize,
    },
    /// The bytes at `off` do not form a complete valid frame (truncated
    /// header/payload, implausible length, or CRC mismatch).
    Corrupt,
}

/// Parse the frame starting at `off`.  Integrity gate is the CRC over
/// seq_no + op + payload; a frame that passes it is genuine (2^-32 for
/// random bytes, further narrowed by the length/op sanity checks used
/// by the resync scan).
fn parse_raw_frame(buf: &[u8], off: usize) -> RawFrame<'_> {
    if off == buf.len() {
        return RawFrame::End;
    }
    if off + 13 > buf.len() {
        return RawFrame::Corrupt; // truncated header
    }
    let entry_len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
    if entry_len > WAL_MAX_ENTRY_LEN {
        return RawFrame::Corrupt;
    }
    let total = WAL_FRAME_OVERHEAD + entry_len as usize;
    if off + total > buf.len() {
        return RawFrame::Corrupt; // truncated payload/crc
    }
    let seq_no = u64::from_le_bytes(buf[off + 4..off + 12].try_into().unwrap());
    let op_code = buf[off + 12];
    let payload = &buf[off + 13..off + 13 + entry_len as usize];
    let stored_crc = u32::from_le_bytes(
        buf[off + 13 + entry_len as usize..off + total]
            .try_into()
            .unwrap(),
    );

    let mut hasher = Crc32Hasher::new();
    hasher.update(&buf[off + 4..off + 12]); // seq_no LE bytes
    hasher.update(&[op_code]);
    hasher.update(payload);
    if hasher.finalize() != stored_crc {
        return RawFrame::Corrupt;
    }
    RawFrame::Frame {
        seq_no,
        op_code,
        payload,
        end: off + total,
    }
}

/// Scan forward from `from` for the next offset that parses as a
/// CRC-valid frame with a KNOWN op code — the boundary-resync primitive.
/// The op-code check (8 valid values of 256) stacks with the CRC to make
/// a false resync on garbage astronomically unlikely.
fn scan_next_valid_frame(buf: &[u8], from: usize) -> Option<usize> {
    let mut c = from;
    while c + WAL_FRAME_OVERHEAD <= buf.len() {
        // Cheap pre-filter before paying a CRC: plausible op byte.
        let op = buf.get(c + 12).copied().unwrap_or(0);
        if WalOpCode::from_u8(op & !OP_COMPRESSED_FLAG).is_some() {
            if let RawFrame::Frame { .. } = parse_raw_frame(buf, c) {
                return Some(c);
            }
        }
        c += 1;
    }
    None
}

/// Writer-side verdict on the ACTIVE generation's on-disk state, used by
/// `WalWriter::open` to decide where appends may land (RC4 W2 #13).
enum WalTailScan {
    /// Every byte parses cleanly: append at `end` (== file length).
    Clean { end: u64 },
    /// Garbage after the last valid frame with NO valid frame beyond it —
    /// the classic crash/ENOSPC tail tear.  Truncate to `last_good_end`.
    TornTail { last_good_end: u64 },
    /// A bad region followed by more valid frames (or an unreadable
    /// header): truncation would destroy acked entries — freeze the file
    /// and append to a fresh generation instead.
    MidFileCorruption,
}

/// Classify the tail state of a WAL file (see [`WalTailScan`]).
fn scan_wal_tail(path: &Path) -> io::Result<WalTailScan> {
    let buf = fs::read(path)?;
    if buf.len() < WAL_HEADER_LEN as usize || &buf[..4] != WAL_MAGIC {
        // Unreadable header (e.g. crash during file creation) — never
        // append after it.
        return Ok(WalTailScan::MidFileCorruption);
    }
    let mut off = WAL_HEADER_LEN as usize;
    loop {
        match parse_raw_frame(&buf, off) {
            RawFrame::End => return Ok(WalTailScan::Clean { end: off as u64 }),
            RawFrame::Frame { end, .. } => off = end,
            RawFrame::Corrupt => {
                return if scan_next_valid_frame(&buf, off + 1).is_some() {
                    Ok(WalTailScan::MidFileCorruption)
                } else {
                    Ok(WalTailScan::TornTail {
                        last_good_end: off as u64,
                    })
                };
            }
        }
    }
}

/// Truncate a WAL file to `len` and reopen it for append, fsyncing the
/// truncation so the healed boundary is durable.
fn truncate_wal_file(path: &Path, len: u64) -> io::Result<File> {
    let f = OpenOptions::new().append(true).open(path)?;
    f.set_len(len)?;
    f.sync_all()?;
    Ok(f)
}

/// Read all entries from a single WAL file.
///
/// RC4 W1 #8: checkpoint-based skipping is GONE from replay (see
/// `WalReader::replay` docs).  The 2026-07 S3 "offset-bounded skip"
/// hardening was the previous, weaker containment of the same bug class
/// (stale/over-broad checkpoints discarding acked entries); removing the
/// skip entirely is its logical conclusion — replay-side loss is now
/// structurally impossible, and dedup of already-flushed entries is the
/// job of the idempotent replay consumers.
///
/// RC4 W2 #13 — BOUNDARY RESYNC: a corrupt region no longer aborts the
/// whole file.  Pre-fix the parser `break`-ed at the first bad frame, so
/// a mid-file tear (crash/ENOSPC leftover appended over by a pre-fix
/// binary, or disk rot) silently dropped every acked entry after it
/// (live repro: acked `PUT a2` gone after restart, WARN-only).  Now the
/// parser records one `Err` per bad region and RESUMES at the next
/// CRC-valid frame boundary; the `Err` keeps the file "unclean" so
/// `prune_verified` retains it.  Frames whose framing is intact but
/// whose payload fails to decode (lz4/json/op-mismatch) also record an
/// `Err` and CONTINUE at the next frame instead of dropping the rest of
/// the file.
fn read_wal_file(path: PathBuf) -> Vec<Result<ReplayEntry>> {
    let buf = match fs::read(&path) {
        Ok(b) => b,
        Err(e) => return vec![Err(e.into())],
    };
    if buf.len() < 4 {
        return vec![];
    }
    if &buf[..4] != WAL_MAGIC {
        return vec![Err(StorageError::InvalidMagic {
            expected: WAL_MAGIC,
            actual: buf[..4].to_vec(),
        })];
    }
    if buf.len() < WAL_HEADER_LEN as usize {
        return vec![];
    }
    let generation = u64::from_le_bytes(buf[4..12].try_into().unwrap());

    let mut results: Vec<Result<ReplayEntry>> = Vec::new();
    let mut off = WAL_HEADER_LEN as usize;

    loop {
        match parse_raw_frame(&buf, off) {
            RawFrame::End => break,
            RawFrame::Corrupt => {
                match scan_next_valid_frame(&buf, off + 1) {
                    Some(next) => {
                        warn!(
                            generation,
                            offset = off,
                            resync_at = next,
                            "WAL corrupt region mid-file — resynced to next valid frame \
                             boundary (entries in the gap are lost; file retained by prune)"
                        );
                        results.push(Err(StorageError::WalCorrupt(
                            off as u64,
                            format!("gen={generation} corrupt region at {off}, resynced at {next}"),
                        )));
                        off = next;
                    }
                    None => {
                        // Torn tail — everything before it was recovered.
                        results.push(Err(StorageError::WalCorrupt(
                            off as u64,
                            format!("gen={generation} truncated/torn tail"),
                        )));
                        break;
                    }
                }
            }
            RawFrame::Frame {
                seq_no,
                op_code,
                payload,
                end,
            } => {
                let this_offset = off as u64;
                off = end;

                match decode_frame_payload(op_code, payload, generation, this_offset) {
                    FrameDecode::Entry(entry) => results.push(Ok(ReplayEntry {
                        seq_no,
                        entry,
                        file_offset: this_offset,
                    })),
                    FrameDecode::UnknownOp => continue,
                    FrameDecode::Bad(e) => {
                        results.push(Err(e));
                        continue;
                    }
                }
            }
        }
    }

    // M5.8 — sort by seq_no so that the M5.8 lock-free WAL writer path
    // (which may land frames out-of-order in the file under high
    // concurrency) still replays in correct seq_no order.  Errors float
    // to the end so they don't disturb the sort key.
    results.sort_by_key(|r| match r {
        Ok(e) => (0u8, e.seq_no),
        Err(_) => (1u8, u64::MAX),
    });

    results
}

/// What one CRC-valid frame's payload turned out to hold.
enum FrameDecode {
    /// A decoded entry.
    Entry(WalEntry),
    /// The op byte is not one this build knows — warn and skip WITHOUT
    /// recording an error, so a file written by a future version stays
    /// prunable rather than being retained forever.
    UnknownOp,
    /// Framing was intact but the payload did not decode (lz4, JSON, or an
    /// op-code/variant mismatch).
    Bad(StorageError),
}

/// Decode one CRC-valid frame's payload: strip the compression flag,
/// decompress, parse the JSON, and check the op byte against the decoded
/// variant.
///
/// Shared by [`read_wal_file`] (full replay) and [`tail_wal_file`] (the
/// WAL-tap reader) so the two can never drift on what a frame means.
fn decode_frame_payload(
    op_code: u8,
    payload: &[u8],
    generation: u64,
    this_offset: u64,
) -> FrameDecode {
    let is_compressed = (op_code & OP_COMPRESSED_FLAG) != 0;
    let raw_op_code = op_code & !OP_COMPRESSED_FLAG;

    let op = match WalOpCode::from_u8(raw_op_code) {
        Some(o) => o,
        None => {
            warn!(
                op_code = raw_op_code,
                "unknown WAL op code — skipping entry"
            );
            return FrameDecode::UnknownOp;
        }
    };

    let payload_owned: Vec<u8>;
    let payload_bytes: &[u8] = if is_compressed {
        match decompress_size_prepended(payload) {
            Ok(dec) => {
                payload_owned = dec;
                &payload_owned
            }
            Err(e) => {
                return FrameDecode::Bad(StorageError::WalCorrupt(
                    this_offset,
                    format!("gen={generation} lz4 decompression failed: {e}"),
                ))
            }
        }
    } else {
        payload
    };

    let entry: WalEntry = match serde_json::from_slice(payload_bytes) {
        Ok(e) => e,
        Err(e) => {
            return FrameDecode::Bad(StorageError::WalCorrupt(
                this_offset,
                format!("gen={generation} json error op={op:?}: {e}"),
            ))
        }
    };

    let expected_op = match &entry {
        WalEntry::Index { .. } => WalOpCode::Index,
        WalEntry::Delete { .. } => WalOpCode::Delete,
        WalEntry::UpdateMapping { .. } => WalOpCode::UpdateMapping,
    };
    if op != expected_op {
        return FrameDecode::Bad(StorageError::WalCorrupt(
            this_offset,
            format!("op code mismatch: header={op:?}, payload={expected_op:?}"),
        ));
    }

    FrameDecode::Entry(entry)
}

// ── WAL tail reader (issue #320) ─────────────────────────────────────────────

/// A consumer's durable position inside ONE WAL shard stream.
///
/// The position is a **byte offset**, not a sequence number, and that is not
/// an accident: the M5.8 lock-free writer reserves its seq_nos with a
/// `fetch_add` *outside* the shard mutex and only then appends under it, so
/// frames can land in a file out of seq order (see the sort in
/// [`read_wal_file`]). Append order is the only order a file position can be
/// resumed from without risking a skipped frame, so that is what a cursor is.
///
/// `generation` and `drained` exist to answer one question the offset alone
/// cannot: *was anything pruned out from under us?* Rotated generations are
/// deleted once every entry in them is durable in a segment
/// ([`WalWriter::prune_verified`]) — retention never waits for a consumer, by
/// design, because a slow remote target must not be able to fill the disk. A
/// consumer that falls far enough behind therefore loses source data, and the
/// contract here is that it always **finds out** ([`WalShardTail::gap`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalTailCursor {
    /// Generation the cursor sits in.
    pub generation: u64,
    /// Byte offset just past the last consumed frame.
    pub offset: u64,
    /// The last read reached a clean end-of-file for `generation`. Lets the
    /// gap check tell "we finished this generation and it was then pruned"
    /// (fine) from "this generation was pruned before we read it" (a gap).
    pub drained: bool,
}

impl WalTailCursor {
    /// A cursor positioned at the very start of `generation`.
    pub fn start_of(generation: u64) -> Self {
        Self {
            generation,
            offset: WAL_HEADER_LEN,
            drained: false,
        }
    }
}

/// The result of one tail poll over a single shard stream.
#[derive(Debug)]
pub struct WalShardTail {
    /// Entries read, in **append order** — the order the source engine
    /// applied them under the shard mutex.
    pub entries: Vec<ReplayEntry>,
    /// Where to resume. Persist this only after the entries have been
    /// consumed successfully; re-reading from an un-advanced cursor is how
    /// at-least-once delivery is achieved.
    pub cursor: WalTailCursor,
    /// Whole generations were pruned before this consumer read them: entries
    /// are missing and are not recoverable from the WAL. Never silent.
    pub gap: bool,
    /// `max_entries` cut the batch short — poll again immediately.
    pub truncated: bool,
    /// Frames skipped because their payload did not decode.
    pub corrupt_skipped: usize,
    /// Bytes this poll actually read off disk, summed over every generation
    /// file it touched.
    ///
    /// The windowed reader exists to keep this proportional to the bytes
    /// *appended since the last poll* rather than to the size of the stream —
    /// a caught-up poll costs one 16-byte header read per generation file it
    /// opens. Exposed so that property is testable through this entry point
    /// instead of by inspecting the reader's internals, which is what let the
    /// pre-window `fs::read(path)` shape go uncaught.
    pub bytes_read: u64,
}

/// Every WAL shard stream under `wal_root`, as `(shard_name, dir)`.
///
/// Mirrors what [`IndexStore`](crate::index_store::IndexStore) writes: the
/// legacy single-WAL layout puts `*.wal` straight in the root (shard name
/// `""`), the sharded layout uses `s0`, `s1`, … subdirectories.
pub fn wal_shard_dirs(wal_root: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let Ok(rd) = fs::read_dir(wal_root) else {
        return dirs;
    };
    let mut has_root_wal = false;
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.extension().map(|x| x == "wal").unwrap_or(false) {
            has_root_wal = true;
        } else if path.is_dir() && name.starts_with('s') && name[1..].parse::<usize>().is_ok() {
            dirs.push((name, path));
        }
    }
    if has_root_wal {
        dirs.push((String::new(), wal_root.to_path_buf()));
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    dirs
}

/// Sorted list of the generations present in one shard directory.
fn generations_in(dir: &Path) -> Vec<u64> {
    let mut gens: Vec<u64> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| parse_wal_generation(&e.file_name().to_string_lossy()))
        .collect();
    gens.sort_unstable();
    gens
}

/// Read the tail of one WAL shard stream.
///
/// `cursor == None` is a first look at this stream, and where it starts
/// depends on one question: has anything ever been pruned here?
///
/// - **Generation 0 is still on disk** — nothing has been reclaimed, so the
///   file set *is* the whole history. Start at the beginning and read it. This
///   is what makes an index created after the consumer starts get all of its
///   documents, including the ones written in the first poll interval.
/// - **Generation 0 is gone** — the stream has been pruned before, so its
///   surviving files are an arbitrary recent slice. Start at the end instead
///   and ship only what happens next. Shipping the slice would look like a
///   backfill while being nothing of the sort; snapshot/restore is the tool
///   for a real copy.
///
/// `max_entries == 0` reads nothing and leaves the cursor where it was.
pub fn tail_shard(dir: &Path, cursor: Option<WalTailCursor>, max_entries: usize) -> WalShardTail {
    let gens = generations_in(dir);
    let Some(&newest) = gens.last() else {
        // No generations at all (fresh index): stay where we are.
        return WalShardTail {
            entries: Vec::new(),
            cursor: cursor.unwrap_or(WalTailCursor::start_of(0)),
            gap: false,
            truncated: false,
            corrupt_skipped: 0,
            bytes_read: 0,
        };
    };
    let first = gens[0];

    let cursor = match cursor {
        Some(c) => c,
        // Never pruned: the files on disk are the entire history, so read it.
        None if first == 0 => WalTailCursor::start_of(0),
        // Pruned before: start at the end rather than ship an arbitrary slice.
        // Walk the newest generation to find its last frame boundary — paid
        // once, when a tap first sees the stream, and with `collect = false`
        // so it costs a boundary walk rather than decoding every frame in the
        // file into entries that are then thrown away.
        None => {
            let scan = tail_wal_file(
                &wal_path(dir, newest),
                WAL_HEADER_LEN,
                false,
                usize::MAX,
                false,
            );
            return WalShardTail {
                entries: Vec::new(),
                cursor: WalTailCursor {
                    generation: newest,
                    offset: scan.end_offset,
                    drained: scan.drained,
                },
                gap: false,
                truncated: false,
                corrupt_skipped: 0,
                bytes_read: scan.bytes_read,
            };
        }
    };

    // Did the pruner delete generations we had not finished reading?
    // `drained` on generation g means g is done, so g+1 appearing as the
    // oldest file on disk is normal progress, not a gap.
    let (start_gen, start_off, gap) = if cursor.generation > newest {
        // The cursor points past every generation that exists. A stream only
        // goes backwards when it is not the same stream: the index was deleted
        // and recreated under the same name, or the data directory was rolled
        // back. Restart from the beginning and flag it — holding position here
        // would stall this shard silently and forever, which is precisely the
        // failure a WAL consumer must never have.
        (first, WAL_HEADER_LEN, true)
    } else if cursor.generation < first {
        let expected_next = cursor.generation + if cursor.drained { 1 } else { 0 };
        (first, WAL_HEADER_LEN, first > expected_next)
    } else {
        (cursor.generation, cursor.offset, false)
    };

    // The generation number can agree and the stream still be a different one.
    // Delete-and-recreate under the same name restarts the WAL at generation
    // 0, so a cursor sitting at (0, 4096) survives the `cursor.generation >
    // newest` check above and resumes at byte 4096 of a file that has nothing
    // to do with it. A WAL generation file is append-only and is never
    // truncated below a boundary a consumer could have reached
    // (`WalTailScan::TornTail` only discards bytes AFTER the last valid
    // frame), so `offset > len` is a sound, false-positive-free witness that
    // this cursor belongs to a stream that no longer exists.
    //
    // Without this the reader clamped the offset to EOF and reported a clean
    // drain: every document written to the recreated index was skipped
    // silently, `gap` stayed false and `_stats` showed a healthy tap.
    let (start_gen, start_off, gap) = if !gap
        && fs::metadata(wal_path(dir, start_gen))
            .map(|m| m.len())
            .unwrap_or(u64::MAX)
            < start_off
    {
        warn!(
            ?dir,
            generation = start_gen,
            offset = start_off,
            "WAL tap: cursor points past the end of its generation — the stream was \
             replaced (index deleted and recreated, or data directory rolled back). \
             Restarting from the oldest surviving generation."
        );
        (first, WAL_HEADER_LEN, true)
    } else {
        (start_gen, start_off, gap)
    };

    let mut entries: Vec<ReplayEntry> = Vec::new();
    let mut corrupt_skipped = 0usize;
    let mut bytes_read = 0u64;
    let mut truncated = false;
    let mut out_cursor = WalTailCursor {
        generation: start_gen,
        offset: start_off,
        drained: false,
    };

    for &gen in gens.iter().filter(|&&g| g >= start_gen) {
        let from = if gen == start_gen {
            start_off
        } else {
            WAL_HEADER_LEN
        };
        // A rotated generation will never grow again, so a corrupt region in
        // it is permanent: resync past it exactly like replay does, and treat
        // a torn tail as the end of the file. The newest generation is still
        // being appended to, so a bad frame at its tail is almost certainly a
        // half-written one — re-read it on the next poll rather than resync
        // over a frame that is about to become valid.
        //
        // "Almost certainly" is doing real work there, which is why this flag
        // no longer decides the whole question: a half-written frame is by
        // construction the LAST thing in the file, so if a complete valid
        // frame exists after the bad bytes they are not a partial tail, and
        // `tail_wal_file` resyncs in the active generation too. Holding the
        // boundary unconditionally is what wedged a shard forever on a cursor
        // that had landed mid-frame in a stream replaced underneath it.
        let allow_resync = gen != newest;
        let budget = max_entries.saturating_sub(entries.len());
        let scan = tail_wal_file(&wal_path(dir, gen), from, allow_resync, budget, true);

        corrupt_skipped += scan.corrupt;
        bytes_read += scan.bytes_read;
        entries.extend(scan.entries);
        out_cursor = WalTailCursor {
            generation: gen,
            offset: scan.end_offset,
            drained: scan.drained,
        };
        if scan.truncated {
            truncated = true;
            break;
        }
    }

    WalShardTail {
        entries,
        cursor: out_cursor,
        gap,
        truncated,
        corrupt_skipped,
        bytes_read,
    }
}

/// One file's contribution to a tail poll.
struct FileTail {
    entries: Vec<ReplayEntry>,
    /// Offset to resume from: the start of the first frame NOT consumed.
    end_offset: u64,
    /// Reached a clean end-of-file.
    drained: bool,
    /// Stopped because the entry budget ran out.
    truncated: bool,
    corrupt: usize,
    /// Bytes actually read off this file by this poll.
    bytes_read: u64,
}

/// How much one buffer refill reads ahead.
///
/// Sized like [`WAL_BUF_CAP`] on the writer side: big enough that a reader
/// catching up on a burst of small frames pays one `read` for the lot.
///
/// It is deliberately **not** applied to the file-header read — see
/// [`WalWindow::fill_exact`]. That read is the only one a caught-up poll
/// performs, so charging it 64 KiB of read-ahead that is discarded unread put
/// the steady-state cost back to 64 KiB per shard per poll.
const WAL_TAIL_READ_CHUNK: usize = 64 * 1024;

/// Bytes of frame header needed before the payload length is known:
/// `len(4) + seq_no(8) + op(1)`.
const WAL_FRAME_HEADER_LEN: usize = 13;

/// A sliding window over one WAL file, read forward from a byte offset.
///
/// This exists so a tail poll costs O(bytes *appended since the last poll*)
/// instead of O(file). A tap polling 16 shards every 500 ms with a 30 s flush
/// window would otherwise re-read the node's entire live ingest volume ~60
/// times per shard per flush cycle — the same read-amplification class as the
/// RC4 W1 #8 `wal_prune_cache` follow-up recorded in
/// `index_store.rs:571-584`.
///
/// Shape taken from Lucene's `BufferedIndexInput`
/// (`lucene/core/src/java/org/apache/lucene/store/BufferedIndexInput.java:289-317`,
/// `refill`): a bounded window whose absolute position is `bufferStart +
/// buffer.position()`, with the read length clamped to the file length so it
/// never reads past EOF. The compaction step — carry the unconsumed remainder
/// to the front, then read into the tail — is
/// `SegmentingTokenizerBase.refill`
/// (`lucene/analysis/common/src/java/org/apache/lucene/analysis/util/SegmentingTokenizerBase.java:124-147`).
/// Apache-2.0, same licence as this repository; adapted, not copied.
struct WalWindow {
    file: File,
    /// Total file length, sampled once at open. A WAL file only ever grows,
    /// so a stale-but-smaller value can never make the reader run past real
    /// data — it only defers it to the next poll.
    len: u64,
    /// Absolute file offset of `buf[0]`.
    base: u64,
    buf: Vec<u8>,
    /// Read position inside `buf`.
    cur: usize,
    /// Bytes this window has actually pulled off the file.
    ///
    /// Not diagnostics: it is the property the windowed reader exists to
    /// bound, and the only way a test can fail if this path regresses to
    /// `fs::read`. Surfaced all the way out through
    /// [`WalShardTail::bytes_read`].
    bytes_read: u64,
}

impl WalWindow {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            file,
            len,
            base: 0,
            buf: Vec::new(),
            cur: 0,
            bytes_read: 0,
        })
    }

    /// Absolute offset of the read position.
    fn pos(&self) -> u64 {
        self.base + self.cur as u64
    }

    /// Discard the window and restart it at absolute offset `off`.
    fn seek_to(&mut self, off: u64) {
        self.base = off;
        self.buf.clear();
        self.cur = 0;
    }

    fn available(&self) -> usize {
        self.buf.len() - self.cur
    }

    /// Make at least `need` bytes readable at the cursor, pulling more of the
    /// file in if necessary. `false` means the file ends first.
    ///
    /// Reads ahead by [`WAL_TAIL_READ_CHUNK`] so a burst of small frames costs
    /// one syscall. Use [`WalWindow::fill_exact`] where the read-ahead would
    /// be pure waste.
    fn fill(&mut self, need: usize) -> bool {
        self.fill_chunked(need, WAL_TAIL_READ_CHUNK)
    }

    /// [`WalWindow::fill`] with no read-ahead at all: pull `need` bytes and
    /// not one more.
    ///
    /// Same escape hatch Lucene gives its own buffered reader:
    /// `BufferedIndexInput.readBytes(byte[], int, int, boolean useBuffer)`
    /// (`lucene/core/src/java/org/apache/lucene/store/BufferedIndexInput.java:91-134`,
    /// Apache-2.0) routes a read around the read-ahead buffer whenever
    /// buffering would not pay — "there's no performance reason not to read it
    /// all at once … no need to reread what we had in the buffer". Adapted,
    /// not copied: the case here is the mirror image, a read too *small* to
    /// justify a chunk that is then discarded.
    ///
    /// The header read is the caller that needs this. It is followed
    /// immediately by `seek_to(start)`, which throws the buffer away, so any
    /// read-ahead there is discarded unread — and on a *caught-up* poll it is
    /// the only read that happens, which made "a caught-up poll costs one
    /// 16-byte header read" false by 4096x: `fill(16)` took the
    /// `.max(WAL_TAIL_READ_CHUNK)` branch and pulled 64 KiB, per shard, per
    /// poll. Measured by
    /// [`a_caught_up_poll_does_not_read_the_whole_generation`] over a
    /// 4,176,200-byte generation: 65,536 bytes before this split, 16 after.
    fn fill_exact(&mut self, need: usize) -> bool {
        self.fill_chunked(need, need)
    }

    fn fill_chunked(&mut self, need: usize, chunk: usize) -> bool {
        if self.available() >= need {
            return true;
        }
        // Compact: the consumed prefix can never be needed again.
        if self.cur > 0 {
            self.buf.drain(..self.cur);
            self.base += self.cur as u64;
            self.cur = 0;
        }
        while self.buf.len() < need {
            let read_from = self.base + self.buf.len() as u64;
            if read_from >= self.len {
                return false;
            }
            let want = (need - self.buf.len()).max(chunk);
            let want = want.min((self.len - read_from) as usize);
            let start = self.buf.len();
            self.buf.resize(start + want, 0);
            if self.file.seek(SeekFrom::Start(read_from)).is_err() {
                self.buf.truncate(start);
                return false;
            }
            match self.file.read(&mut self.buf[start..]) {
                Ok(0) => {
                    self.buf.truncate(start);
                    return false;
                }
                Ok(n) => {
                    self.bytes_read += n as u64;
                    self.buf.truncate(start + n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => self.buf.truncate(start),
                Err(_) => {
                    self.buf.truncate(start);
                    return false;
                }
            }
        }
        true
    }

    /// Pull the rest of the file into the window.
    ///
    /// Only the resync path calls this: [`scan_next_valid_frame`] needs a
    /// contiguous buffer to walk, and corruption is exceptional. The
    /// steady-state path never gets here, which is the whole point of the
    /// window.
    fn fill_to_end(&mut self) {
        let remaining = self.len.saturating_sub(self.pos());
        self.fill(remaining as usize);
    }
}

/// Read frames from `path` starting at `from_offset`, at most `budget` of them.
///
/// Frames are returned in append order. `end_offset` always lands on a frame
/// boundary, so the caller can persist it and resume from exactly there.
///
/// `collect = false` walks the same boundaries without decoding any payload —
/// what positioning a brand-new cursor at the end of an established stream
/// needs, and it skips the lz4 + serde work (and the `Vec<ReplayEntry>`) for
/// every frame in the file.
fn tail_wal_file(
    path: &Path,
    from_offset: u64,
    allow_resync: bool,
    budget: usize,
    collect: bool,
) -> FileTail {
    let mut win = match WalWindow::open(path) {
        Ok(w) => w,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Pruned between listing and reading. Treat as finished so the
            // caller moves on to the next generation.
            return FileTail {
                entries: Vec::new(),
                end_offset: from_offset,
                drained: true,
                truncated: false,
                corrupt: 0,
                bytes_read: 0,
            };
        }
        Err(e) => {
            warn!(?path, error = %e, "WAL tap: cannot read generation");
            return FileTail {
                entries: Vec::new(),
                end_offset: from_offset,
                drained: false,
                truncated: false,
                corrupt: 0,
                bytes_read: 0,
            };
        }
    };

    win.seek_to(0);
    // `fill_exact`, not `fill`: the very next statement is `seek_to(start)`,
    // which discards the buffer, so read-ahead here is read-and-throw-away —
    // and on a caught-up poll this is the ONLY read, so the read-ahead was the
    // whole per-poll cost.
    if !win.fill_exact(WAL_HEADER_LEN as usize) || &win.buf[..4] != WAL_MAGIC {
        return FileTail {
            entries: Vec::new(),
            end_offset: from_offset.max(WAL_HEADER_LEN),
            drained: false,
            truncated: false,
            corrupt: 0,
            bytes_read: win.bytes_read,
        };
    }
    let generation = u64::from_le_bytes(win.buf[4..12].try_into().unwrap());

    // A resume offset past the end is the caller's problem to classify (see
    // `tail_shard`); clamping here is what silently skipped a recreated
    // index's whole WAL, so refuse to move rather than clamp.
    let start = from_offset.max(WAL_HEADER_LEN);
    if start > win.len {
        return FileTail {
            entries: Vec::new(),
            end_offset: start,
            drained: false,
            truncated: false,
            corrupt: 0,
            bytes_read: win.bytes_read,
        };
    }
    win.seek_to(start);

    let mut entries: Vec<ReplayEntry> = Vec::new();
    let mut read = 0usize;
    let mut corrupt = 0usize;

    loop {
        if collect && read >= budget {
            return FileTail {
                entries,
                end_offset: win.pos(),
                drained: false,
                truncated: true,
                corrupt,
                bytes_read: win.bytes_read,
            };
        }
        if win.pos() >= win.len {
            return FileTail {
                entries,
                end_offset: win.pos(),
                drained: true,
                truncated: false,
                corrupt,
                bytes_read: win.bytes_read,
            };
        }

        // Enough bytes to know how long this frame claims to be?
        let framed = win.fill(WAL_FRAME_HEADER_LEN) && {
            let entry_len =
                u32::from_le_bytes(win.buf[win.cur..win.cur + 4].try_into().unwrap_or([0; 4]));
            entry_len <= WAL_MAX_ENTRY_LEN
                && win.fill(WAL_FRAME_OVERHEAD + entry_len as usize)
                && matches!(parse_raw_frame(&win.buf, win.cur), RawFrame::Frame { .. })
        };

        if !framed {
            // Not a frame. In a rotated generation that is permanent damage
            // and replay's boundary resync applies. In the ACTIVE generation
            // the usual cause is a half-written frame at the tail — but a
            // half-written frame is by construction the LAST thing in the
            // file (`write_one_frame_inner` drains whole frames), so if a
            // complete valid frame exists *after* this point these bytes are
            // not a partial tail. Holding the boundary there is what wedged a
            // cursor forever on a stream that had been replaced underneath
            // it; resync instead and count it.
            win.fill_to_end();
            match scan_next_valid_frame(&win.buf, win.cur + 1) {
                Some(next) => {
                    let at = win.pos();
                    let resync_at = win.base + next as u64;
                    warn!(
                        generation,
                        offset = at,
                        resync_at,
                        active = !allow_resync,
                        "WAL tap: corrupt region — entries in the gap cannot be shipped"
                    );
                    corrupt += 1;
                    win.cur = next;
                }
                None if allow_resync => {
                    // Torn tail on a file that will never grow again.
                    corrupt += 1;
                    return FileTail {
                        entries,
                        end_offset: win.pos(),
                        drained: true,
                        truncated: false,
                        corrupt,
                        bytes_read: win.bytes_read,
                    };
                }
                None => {
                    // Active generation, nothing valid beyond: a genuine
                    // half-written frame. It completes on the writer's next
                    // flush — hold the boundary.
                    return FileTail {
                        entries,
                        end_offset: win.pos(),
                        drained: false,
                        truncated: false,
                        corrupt,
                        bytes_read: win.bytes_read,
                    };
                }
            }
            continue;
        }

        let RawFrame::Frame {
            seq_no,
            op_code,
            payload,
            end,
        } = parse_raw_frame(&win.buf, win.cur)
        else {
            unreachable!("`framed` already proved this parses as a frame");
        };
        let this_offset = win.pos();
        let consumed = end - win.cur;

        if collect {
            match decode_frame_payload(op_code, payload, generation, this_offset) {
                FrameDecode::Entry(entry) => entries.push(ReplayEntry {
                    seq_no,
                    entry,
                    file_offset: this_offset,
                }),
                FrameDecode::UnknownOp => {}
                FrameDecode::Bad(e) => {
                    warn!(generation, offset = this_offset, error = %e,
                          "WAL tap: undecodable frame — skipped");
                    corrupt += 1;
                }
            }
        }
        read += 1;
        win.cur += consumed;
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn wal_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("{generation:016x}.wal"))
}

fn checkpoint_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("{generation:016x}.wchk"))
}

/// Find the highest WAL generation present in `dir`, plus the highest seq_no
/// seen across all entries (for recovering the sequence counter).
fn find_latest_generation(dir: &Path) -> Result<(u64, SeqNo)> {
    let mut max_gen = 0u64;
    let mut max_seq = 0u64;

    if !dir.exists() {
        return Ok((0, 0));
    }

    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        if let Some(gen) = parse_wal_generation(&name.to_string_lossy()) {
            if gen >= max_gen {
                max_gen = gen;
                // Scan this file for the highest seq_no
                if let Ok(Some(s)) = scan_seq_nos(entry.path()) {
                    max_seq = max_seq.max(s);
                }
            }
        }
    }

    Ok((max_gen, max_seq))
}

/// RC4 W2 #13 — the seq scan only trusts CRC-VALID frames.  The pre-fix
/// version read raw `(len, seq)` headers with zero validation and seeked
/// past `len`: a 40-byte garbage tail parsed as seq_no
/// 0xABAB_ABAB_ABAB_ABAB and permanently jumped the global seq counter to
/// ~1.2e19 (live-reproduced: the next acked PUT returned that _seq_no).
/// Corrupt regions are skipped via the same boundary resync replay uses.
fn scan_seq_nos(path: PathBuf) -> io::Result<Option<SeqNo>> {
    let buf = fs::read(&path)?;
    if buf.len() < WAL_HEADER_LEN as usize || &buf[..4] != WAL_MAGIC {
        return Ok(None);
    }
    let mut max_seq = None;
    let mut off = WAL_HEADER_LEN as usize;
    loop {
        match parse_raw_frame(&buf, off) {
            RawFrame::End => break,
            RawFrame::Frame { seq_no, end, .. } => {
                max_seq = Some(max_seq.unwrap_or(0).max(seq_no));
                off = end;
            }
            RawFrame::Corrupt => match scan_next_valid_frame(&buf, off + 1) {
                Some(next) => off = next,
                None => break,
            },
        }
    }
    Ok(max_seq)
}

fn create_wal_file(path: &Path, generation: u64) -> io::Result<File> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(WAL_MAGIC)?;
    file.write_u64::<LittleEndian>(generation)?;
    file.write_u32::<LittleEndian>(0)?; // reserved
    file.flush()?;
    file.sync_all()?;
    // Re-open for append
    OpenOptions::new().append(true).open(path)
}

/// Read a generation's checkpoint file.  Replay no longer consumes
/// checkpoints (RC4 W1 #8); they are still WRITTEN — with verified-safe
/// values only — for data-dir compatibility with older binaries whose
/// replay/prune still read them.  Kept for tests and offline tooling.
#[allow(dead_code)]
fn read_checkpoint(dir: &Path, generation: u64) -> io::Result<WalCheckpoint> {
    let path = checkpoint_path(dir, generation);
    let mut f = File::open(&path)?;
    WalCheckpoint::read_from(&mut f)
}

fn parse_wal_generation(name: &str) -> Option<u64> {
    if !name.ends_with(".wal") {
        return None;
    }
    let hex = &name[..name.len() - 4];
    u64::from_str_radix(hex, 16).ok()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    fn make_writer(dir: &Path) -> WalWriter {
        WalWriter::open(
            dir,
            64 * 1024 * 1024,
            SyncMode::Strict,
            Arc::new(AtomicU64::new(1)),
        )
        .unwrap()
    }

    /// #873 — an EMPTY active generation holds no file descriptor and no
    /// 64 KiB buffer, and nothing about the on-disk log changes.
    ///
    /// This is the state every index opens in and every flush returns to
    /// (`wal_maintain_all_verified` → `force_rotate`), multiplied by the
    /// ingest-shard count and again by the number of open indices — so the
    /// property under test is "an index that is not being written to stops
    /// paying for a write buffer", not a micro-optimisation of one shard.
    #[test]
    fn an_empty_wal_generation_holds_no_descriptor_and_no_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());

        // Fresh shard: unmaterialized, but the generation-0 file was created
        // on disk exactly as before — the laziness is in memory only.
        assert!(
            !w.is_materialized(),
            "a fresh WAL shard must not hold a descriptor"
        );
        assert_eq!(w.buffer_capacity(), 0, "and must not allocate a buffer");
        assert!(
            wal_path(dir.path(), 0).exists(),
            "the active generation file must still be created on disk"
        );
        assert_eq!(
            fs::metadata(wal_path(dir.path(), 0)).unwrap().len(),
            WAL_HEADER_LEN,
            "an empty generation is exactly its header"
        );

        // The first append materializes it.
        w.append(&doc("d1")).unwrap();
        assert!(w.is_materialized(), "an append must materialize the shard");
        assert_eq!(w.buffer_capacity(), WAL_BUF_CAP);

        // The post-flush path hands the buffer back: `force_rotate` freezes
        // the generation it wrote and leaves an empty one active.
        w.force_rotate().unwrap();
        assert!(
            !w.is_materialized(),
            "force_rotate must release the descriptor of the empty generation \
             it leaves active"
        );
        assert_eq!(w.buffer_capacity(), 0);
        assert_eq!(w.active_generation(), 1);

        // A write after the release re-materializes and lands correctly.
        w.append(&doc("d2")).unwrap();
        assert!(w.is_materialized());
        drop(w);

        // Reopening a shard whose active generation is empty stays lazy, and
        // every entry ever written is still replayable.
        let mut reopened = make_writer(dir.path());
        assert!(
            reopened.is_materialized(),
            "gen 1 has data, so it is opened"
        );
        reopened.force_rotate().unwrap();
        drop(reopened);
        let reopened = make_writer(dir.path());
        assert!(
            !reopened.is_materialized(),
            "a cleanly rotated shard reopens without a descriptor or a buffer"
        );
        assert_eq!(reopened.buffer_capacity(), 0);
        drop(reopened);

        let ids: Vec<String> = WalReader::new(dir.path())
            .replay()
            .unwrap()
            .map(|e| match e.unwrap().entry {
                WalEntry::Index { doc_id, .. } => doc_id,
                other => panic!("unexpected entry {other:?}"),
            })
            .collect();
        assert_eq!(
            ids,
            vec!["d1".to_string(), "d2".to_string()],
            "laziness must not cost a single acknowledged entry"
        );
    }

    #[test]
    fn round_trip_index_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());

        let entry = WalEntry::Index {
            doc_id: "doc-1".to_string(),
            source: serde_json::json!({"title": "hello world"}),
        };
        let seq = w.append(&entry).unwrap();
        assert_eq!(seq, 1);
        drop(w);

        let reader = WalReader::new(dir.path());
        let entries: Vec<_> = reader.replay().unwrap().collect();
        assert_eq!(entries.len(), 1);
        let r = entries[0].as_ref().unwrap();
        assert_eq!(r.seq_no, 1);
        match &r.entry {
            WalEntry::Index { doc_id, .. } => assert_eq!(doc_id, "doc-1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn replay_ignores_checkpoints_and_returns_everything() {
        // RC4 W1 #8: checkpoints must never cause replay to discard
        // entries.  Pre-fix, a checkpoint with max_seq_no=3 made replay
        // skip seqs 1..3 — if any of those were acked-but-unflushed
        // (over-broad checkpoint), they were silently lost.  Replay now
        // returns every surviving entry; dedup of genuinely-flushed docs
        // is the idempotent replay consumers' job (version-map guarded).
        let dir = tempfile::tempdir().unwrap();
        let seq_ctr = Arc::new(AtomicU64::new(1));
        let mut w = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_ctr),
        )
        .unwrap();

        for i in 0..5 {
            w.append(&WalEntry::Delete {
                doc_id: format!("doc-{i}"),
            })
            .unwrap();
        }
        // A (deliberately over-broad) checkpoint after seq 3.
        w.checkpoint(3).unwrap();
        drop(w);

        let reader = WalReader::new(dir.path());
        let entries: Vec<_> = reader.replay().unwrap().collect();
        assert_eq!(entries.len(), 5, "replay must return ALL entries");
    }

    #[test]
    fn prune_verified_retains_generations_with_unflushed_entries() {
        // RC4 W1 #8 core regression at the WAL layer: a rotated
        // generation holding even one entry the verifier cannot prove
        // durable must survive prune; fully-verified generations are
        // deleted.
        let dir = tempfile::tempdir().unwrap();
        let seq_ctr = Arc::new(AtomicU64::new(1));
        let mut w = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_ctr),
        )
        .unwrap();

        // Gen 0: seqs 1..=3.  Rotate.  Gen 1: seqs 4..=5.  Rotate.
        for i in 0..3 {
            w.append(&WalEntry::Delete {
                doc_id: format!("g0-{i}"),
            })
            .unwrap();
        }
        w.force_rotate().unwrap();
        for i in 0..2 {
            w.append(&WalEntry::Delete {
                doc_id: format!("g1-{i}"),
            })
            .unwrap();
        }
        w.force_rotate().unwrap();

        // Verifier: everything with seq <= 3 is durable; seq 4+ is not
        // (i.e. gen 1 holds acked-but-unflushed entries).
        let pruned = w.prune_verified(&|_e, seq| seq <= 3).unwrap();
        assert_eq!(pruned, 1, "exactly gen 0 must be pruned");

        // Gen 1's entries must still replay in full.
        let reader = WalReader::new(dir.path());
        let survivors: Vec<_> = reader
            .replay()
            .unwrap()
            .map(|r| r.unwrap().seq_no)
            .collect();
        assert_eq!(survivors, vec![4, 5], "unverified generation retained");

        // Once the verifier can prove everything, the rest is reclaimed.
        let pruned2 = w.prune_verified(&|_e, _seq| true).unwrap();
        assert_eq!(pruned2, 1);
        let reader = WalReader::new(dir.path());
        assert_eq!(reader.replay().unwrap().count(), 0);
    }

    #[test]
    fn rotation_creates_new_generation() {
        let dir = tempfile::tempdir().unwrap();
        // Force rotation after 1 byte
        let mut w = WalWriter::open(
            dir.path(),
            1,
            SyncMode::Batched,
            Arc::new(AtomicU64::new(1)),
        )
        .unwrap();
        w.append(&WalEntry::Delete { doc_id: "a".into() }).unwrap();
        w.append(&WalEntry::Delete { doc_id: "b".into() }).unwrap();
        assert!(w.generation > 0, "should have rotated");
    }

    // ── RC4 W2 #13 — torn-frame / ENOSPC hardening ─────────────────────

    /// ENOSPC injection: a mid-frame write error must NOT poison the
    /// generation.  Pre-fix the torn bytes stayed in the stream, later
    /// acked appends landed after them, and replay dropped everything
    /// from the tear onward.  Post-fix the failed frame is rolled back
    /// (truncate + buffer discard), the failed op is NACKed, and the log
    /// stays byte-clean for both earlier and later acked entries.
    #[test]
    fn enospc_mid_frame_rolls_back_and_wal_stays_clean() {
        let dir = tempfile::tempdir().unwrap();
        let seq_ctr = Arc::new(AtomicU64::new(1));
        let mut w = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_ctr),
        )
        .unwrap();

        // A — acked.
        let seq_a = w
            .append(&WalEntry::Index {
                doc_id: "a".into(),
                source: serde_json::json!({"v": 1}),
            })
            .unwrap();

        // B — disk "fills up" 10 bytes into the frame: partial write,
        // then ENOSPC.  The append must error (NACK).
        w.inject_write_fault(Some(10));
        let err = w.append(&WalEntry::Index {
            doc_id: "b".into(),
            source: serde_json::json!({"v": 2, "pad": "x".repeat(64)}),
        });
        assert!(err.is_err(), "append during ENOSPC must NACK");
        assert_eq!(
            w.buffer_capacity(),
            WAL_BUF_CAP,
            "truncate-and-reseat recovery must preserve the bounded capacity"
        );

        // Space frees up (recovery reseated the writer on a clean fd, so
        // the fault is naturally gone) — C is acked.
        let seq_c = w
            .append(&WalEntry::Index {
                doc_id: "c".into(),
                source: serde_json::json!({"v": 3}),
            })
            .unwrap();

        // The active generation must decode CLEAN end-to-end: exactly
        // A and C, no torn bytes anywhere.
        let (entries, clean) = w.read_generation_entries(w.active_generation());
        assert!(
            clean,
            "WAL generation must stay clean after ENOSPC rollback"
        );
        let ids: Vec<String> = entries
            .iter()
            .map(|e| match &e.entry {
                WalEntry::Index { doc_id, .. } => doc_id.clone(),
                other => panic!("unexpected entry {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "c".to_string()]);
        drop(w);

        // Replay agrees: every acked entry, zero errors.
        let reader = WalReader::new(dir.path());
        let replayed: Vec<_> = reader.replay().unwrap().collect();
        assert_eq!(replayed.len(), 2);
        let seqs: Vec<u64> = replayed
            .iter()
            .map(|r| r.as_ref().unwrap().seq_no)
            .collect();
        assert_eq!(seqs, vec![seq_a, seq_c]);
    }

    /// Batch-path variant of the ENOSPC rollback: the whole batch is
    /// NACKed and rolled back to the batch's start boundary.
    #[test]
    fn enospc_mid_batch_rolls_back_whole_batch() {
        let dir = tempfile::tempdir().unwrap();
        let seq_ctr = Arc::new(AtomicU64::new(1));
        let mut w = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_ctr),
        )
        .unwrap();

        w.append(&WalEntry::Delete { doc_id: "a".into() }).unwrap();

        let (_seqs, frames, total) = wal_build_frames_lockfree(
            &seq_ctr,
            &[
                br#"{"Index":{"doc_id":"b1","source":{}}}"#.to_vec(),
                br#"{"Index":{"doc_id":"b2","source":{}}}"#.to_vec(),
            ],
        );
        w.inject_write_fault(Some(7));
        assert!(
            w.append_frames_locked(&frames, total).is_err(),
            "batch during ENOSPC must NACK"
        );

        let seq_c = w.append(&WalEntry::Delete { doc_id: "c".into() }).unwrap();
        let (entries, clean) = w.read_generation_entries(w.active_generation());
        assert!(clean);
        assert_eq!(entries.len(), 2, "only a and c survive: {entries:?}");
        assert_eq!(entries[1].seq_no, seq_c);
        assert_eq!(
            w.buffer_capacity(),
            WAL_BUF_CAP,
            "batch error recovery must preserve the bounded capacity"
        );
        drop(w);

        // Batched mode flushes each acknowledged append into the kernel even
        // though it does not fsync inline. Reopen must retain every acked
        // frame and use the same bounded writer capacity.
        let reopened =
            WalWriter::open(dir.path(), 64 * 1024 * 1024, SyncMode::Batched, seq_ctr).unwrap();
        assert_eq!(reopened.buffer_capacity(), WAL_BUF_CAP);
        drop(reopened);
        let replayed: Vec<_> = WalReader::new(dir.path())
            .replay()
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[1].seq_no, seq_c);
    }

    /// Crash-torn tail heal at open: a partial frame left at the file
    /// tail (kill -9 / ENOSPC without in-process recovery) must be
    /// truncated on reopen so new acked appends land on a frame boundary.
    /// Pre-fix, open() appended at the raw file length — the tear became
    /// MID-FILE corruption and replay dropped every later acked entry
    /// (live repro: acked a2 404 after restart), while the unvalidated
    /// seq scan even seeded the counter from the garbage
    /// (0xABABABABABABABAB = 12370169555311111083).
    #[test]
    fn open_heals_torn_tail_and_preserves_later_acked_appends() {
        let dir = tempfile::tempdir().unwrap();
        let seq_ctr = Arc::new(AtomicU64::new(1));
        let mut w = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_ctr),
        )
        .unwrap();
        let gen0 = w.active_generation();
        let seq_a = w
            .append(&WalEntry::Index {
                doc_id: "a1".into(),
                source: serde_json::json!({"msg": "first acked doc"}),
            })
            .unwrap();
        drop(w);

        // Simulate the crash-torn partial frame.
        let path = dir.path().join(format!("{gen0:016x}.wal"));
        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[0xAB; 40]).unwrap();
        }

        // Reopen — the torn tail must be healed and the seq counter must
        // NOT be poisoned by the garbage header.
        let seq_ctr2 = Arc::new(AtomicU64::new(1));
        let mut w2 = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_ctr2),
        )
        .unwrap();
        let seq_b = w2
            .append(&WalEntry::Index {
                doc_id: "a2".into(),
                source: serde_json::json!({"msg": "second acked doc"}),
            })
            .unwrap();
        assert_eq!(
            seq_b,
            seq_a + 1,
            "seq counter must continue from the last VALID frame, not garbage"
        );
        drop(w2);

        // Both acked docs replay, with zero errors.
        let reader = WalReader::new(dir.path());
        let replayed: Vec<_> = reader.replay().unwrap().collect();
        let mut ids = Vec::new();
        for r in &replayed {
            let e = r.as_ref().expect("no corrupt entries after heal");
            if let WalEntry::Index { doc_id, .. } = &e.entry {
                ids.push(doc_id.clone());
            }
        }
        assert_eq!(ids, vec!["a1".to_string(), "a2".to_string()]);
    }

    /// Mid-file corruption with parseable frames after it (disk rot, or a
    /// tear appended over by a pre-fix binary): replay must RESYNC at the
    /// next valid frame boundary and recover the later acked entries
    /// (pre-fix: silently dropped), while the file stays "unclean" so the
    /// verified prune retains it.
    #[test]
    fn replay_resyncs_past_midfile_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        let gen0 = w.active_generation();
        for id in ["a", "b", "c"] {
            w.append(&WalEntry::Index {
                doc_id: id.into(),
                source: serde_json::json!({"pad": "p".repeat(32), "id": id}),
            })
            .unwrap();
        }
        drop(w);

        // Corrupt ONE byte inside b's frame (payload region), leaving a
        // parseable frame after it.
        let path = dir.path().join(format!("{gen0:016x}.wal"));
        let mut bytes = fs::read(&path).unwrap();
        // Find b's frame: parse frame 1's end (a), flip a byte in the
        // middle of the second frame's payload.
        let first_end = match parse_raw_frame(&bytes, WAL_HEADER_LEN as usize) {
            RawFrame::Frame { end, .. } => end,
            _ => panic!("first frame must parse"),
        };
        bytes[first_end + 20] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();

        let results = read_wal_file(path.clone());
        let ok_ids: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|e| match &e.entry {
                WalEntry::Index { doc_id, .. } => doc_id.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            ok_ids,
            vec!["a".to_string(), "c".to_string()],
            "entries after the corrupt region must be recovered via resync"
        );
        assert!(
            results.iter().any(|r| r.is_err()),
            "the corrupt region must surface as an error (keeps prune conservative)"
        );

        // Writer-side: open() must FREEZE this generation (mid-file
        // corruption is never truncated — that would destroy c) and seat
        // appends on a fresh one.
        let w2 = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::new(AtomicU64::new(1)),
        )
        .unwrap();
        assert_eq!(
            w2.active_generation(),
            gen0 + 1,
            "mid-file-corrupt generation must be frozen, not appended to"
        );
    }

    #[test]
    fn all_entry_types() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());

        w.append(&WalEntry::Index {
            doc_id: "x".into(),
            source: serde_json::json!({}),
        })
        .unwrap();
        w.append(&WalEntry::Delete { doc_id: "y".into() }).unwrap();
        w.append(&WalEntry::UpdateMapping {
            schema: serde_json::json!({"fields": {}}),
        })
        .unwrap();
        drop(w);

        let reader = WalReader::new(dir.path());
        let entries: Vec<_> = reader.replay().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn bounded_buffer_survives_append_rotate_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let seq_counter = Arc::new(AtomicU64::new(1));
        let mut writer = WalWriter::open(
            dir.path(),
            64 * 1024 * 1024,
            SyncMode::Strict,
            Arc::clone(&seq_counter),
        )
        .unwrap();
        // #873 — a shard with an empty active generation holds no buffer at
        // all; the bound below is what the first append allocates.
        assert_eq!(writer.buffer_capacity(), 0);

        let first = WalEntry::Index {
            doc_id: "before-rotate".into(),
            source: serde_json::json!({"payload": "durable"}),
        };
        assert_eq!(writer.append(&first).unwrap(), 1);
        writer.rotate().unwrap();
        assert_eq!(writer.buffer_capacity(), WAL_BUF_CAP);

        let second = WalEntry::Index {
            doc_id: "after-rotate".into(),
            source: serde_json::json!({"payload": "also durable"}),
        };
        assert_eq!(writer.append(&second).unwrap(), 2);
        drop(writer);

        let reopened =
            WalWriter::open(dir.path(), 64 * 1024 * 1024, SyncMode::Strict, seq_counter).unwrap();
        assert_eq!(reopened.buffer_capacity(), WAL_BUF_CAP);
        drop(reopened);

        let entries: Vec<_> = WalReader::new(dir.path())
            .replay()
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq_no, 1);
        assert_eq!(entries[1].seq_no, 2);
    }

    /// Records smaller than, equal to, and much larger than `WAL_BUF_CAP`
    /// must all survive append, forced rotation (truncation of generations),
    /// reopen, and exact replay.  Capacity stays fixed regardless of payload.
    #[test]
    fn records_across_buffer_capacity_survive_append_rotate_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let seq_counter = Arc::new(AtomicU64::new(1));
        let mut writer = WalWriter::open(
            dir.path(),
            // Force rotation after a few large frames.
            256 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_counter),
        )
        .unwrap();
        assert_eq!(writer.buffer_capacity(), 0, "empty generation, no buffer");

        // High-entropy printable so LZ4 cannot collapse oversized payloads
        // back under the buffer capacity.
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mk_payload = |n: usize, salt: u32| {
            let mut state = salt;
            let body: String = (0..n)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    alphabet[state as usize % alphabet.len()] as char
                })
                .collect();
            serde_json::json!({"body": body, "n": n})
        };

        let shapes: &[(usize, &str)] = &[
            (64, "small"),
            (WAL_BUF_CAP, "equal-cap"),
            (WAL_BUF_CAP * 2 + 17, "2x-cap"),
            (131_072, "131kib"),
        ];
        let mut expected_ids = Vec::new();
        for (size, label) in shapes {
            for copy in 0..3 {
                let doc_id = format!("{label}-{copy}");
                let seq = writer
                    .append(&WalEntry::Index {
                        doc_id: doc_id.clone(),
                        source: mk_payload(*size, (size + copy) as u32 + 0x9e37_79b9),
                    })
                    .unwrap();
                assert!(seq > 0);
                expected_ids.push(doc_id);
                assert_eq!(
                    writer.buffer_capacity(),
                    WAL_BUF_CAP,
                    "capacity must not grow with record size {size}"
                );
            }
        }
        writer.force_rotate().unwrap();
        // #873 — force_rotate leaves an EMPTY generation active and hands the
        // buffer back, which is what keeps a flushed index from holding one.
        assert_eq!(writer.buffer_capacity(), 0);

        // One more append after rotate to prove the new generation is live.
        let final_id = "post-rotate".to_string();
        writer
            .append(&WalEntry::Index {
                doc_id: final_id.clone(),
                source: mk_payload(128, 7),
            })
            .unwrap();
        expected_ids.push(final_id);
        drop(writer);

        let reopened = WalWriter::open(
            dir.path(),
            256 * 1024,
            SyncMode::Batched,
            Arc::clone(&seq_counter),
        )
        .unwrap();
        // The post-rotate append above left data in the active generation, so
        // the reopen materializes it — at the same bounded capacity (#873).
        assert_eq!(reopened.buffer_capacity(), WAL_BUF_CAP);
        drop(reopened);

        let replayed: Vec<_> = WalReader::new(dir.path())
            .replay()
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        let ids: Vec<String> = replayed
            .iter()
            .map(|e| match &e.entry {
                WalEntry::Index { doc_id, .. } => doc_id.clone(),
                other => panic!("unexpected entry {other:?}"),
            })
            .collect();
        assert_eq!(ids, expected_ids);
    }

    /// Concurrent shard writers sharing one sequence counter must keep the
    /// bounded capacity and produce an exact combined replay set.
    #[test]
    fn concurrent_shard_writers_preserve_capacity_and_exact_replay() {
        use std::sync::Barrier;
        use std::thread;

        let root = tempfile::tempdir().unwrap();
        let seq = Arc::new(AtomicU64::new(1));
        let barrier = Arc::new(Barrier::new(5));
        let mut handles = Vec::new();
        for shard in 0..4 {
            let dir = root.path().join(format!("s{shard}"));
            fs::create_dir_all(&dir).unwrap();
            let seq = Arc::clone(&seq);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let mut w =
                    WalWriter::open(&dir, 64 * 1024 * 1024, SyncMode::Batched, seq).unwrap();
                assert_eq!(w.buffer_capacity(), 0, "fresh shard holds no buffer");
                barrier.wait();
                for i in 0..50 {
                    let pad = if i % 10 == 0 {
                        "X".repeat(WAL_BUF_CAP + 256)
                    } else {
                        "y".repeat(32)
                    };
                    w.append(&WalEntry::Index {
                        doc_id: format!("s{shard}-{i}"),
                        source: serde_json::json!({"pad": pad}),
                    })
                    .unwrap();
                }
                w.sync().unwrap();
                assert_eq!(w.buffer_capacity(), WAL_BUF_CAP);
            }));
        }
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }

        let mut total = 0usize;
        for shard in 0..4 {
            let dir = root.path().join(format!("s{shard}"));
            let replayed: Vec<_> = WalReader::new(&dir)
                .replay()
                .unwrap()
                .map(|e| e.unwrap())
                .collect();
            assert_eq!(replayed.len(), 50);
            total += replayed.len();
            let reopened =
                WalWriter::open(&dir, 64 * 1024 * 1024, SyncMode::Batched, Arc::clone(&seq))
                    .unwrap();
            assert_eq!(reopened.buffer_capacity(), WAL_BUF_CAP);
        }
        assert_eq!(total, 200);
    }

    // ── WAL tail reader (issue #320) ─────────────────────────────────────────

    fn doc(id: &str) -> WalEntry {
        WalEntry::Index {
            doc_id: id.to_string(),
            source: serde_json::json!({"v": id}),
        }
    }

    fn doc_ids(tail: &WalShardTail) -> Vec<String> {
        tail.entries
            .iter()
            .map(|e| match &e.entry {
                WalEntry::Index { doc_id, .. } => format!("+{doc_id}"),
                WalEntry::Delete { doc_id } => format!("-{doc_id}"),
                WalEntry::UpdateMapping { .. } => "~".to_string(),
            })
            .collect()
    }

    /// A never-pruned stream is read from the beginning, so an index created
    /// while a tap is running is shipped whole — including the documents
    /// written before its first poll.
    #[test]
    fn a_fresh_stream_is_tailed_from_the_beginning() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();
        w.append(&doc("b")).unwrap();
        drop(w);

        let tail = tail_shard(dir.path(), None, 100);
        assert_eq!(doc_ids(&tail), vec!["+a", "+b"]);
        assert!(!tail.gap);
        assert!(tail.cursor.drained);
    }

    /// The resume contract: a cursor read back returns only what arrived
    /// after it, exactly once.
    #[test]
    fn a_cursor_resumes_without_re_reading_or_skipping() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();

        let first = tail_shard(dir.path(), None, 100);
        assert_eq!(doc_ids(&first), vec!["+a"]);

        // Nothing new yet.
        let idle = tail_shard(dir.path(), Some(first.cursor), 100);
        assert!(idle.entries.is_empty());
        assert!(!idle.gap);

        w.append(&doc("b")).unwrap();
        w.append(&WalEntry::Delete {
            doc_id: "a".to_string(),
        })
        .unwrap();
        let next = tail_shard(dir.path(), Some(idle.cursor), 100);
        assert_eq!(doc_ids(&next), vec!["+b", "-a"]);
    }

    /// `max_entries` bounds one read; the cursor lands on a frame boundary so
    /// the next read continues from exactly there.
    #[test]
    fn a_truncated_read_resumes_at_the_frame_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        for id in ["a", "b", "c", "d"] {
            w.append(&doc(id)).unwrap();
        }
        drop(w);

        let first = tail_shard(dir.path(), None, 2);
        assert_eq!(doc_ids(&first), vec!["+a", "+b"]);
        assert!(first.truncated);
        assert!(!first.cursor.drained);

        let second = tail_shard(dir.path(), Some(first.cursor), 100);
        assert_eq!(doc_ids(&second), vec!["+c", "+d"]);
        assert!(!second.truncated);
    }

    /// Rotation is not a gap: a consumer that keeps up walks from one
    /// generation into the next without noticing.
    #[test]
    fn rotation_is_not_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();
        let caught_up = tail_shard(dir.path(), None, 100);
        assert_eq!(doc_ids(&caught_up), vec!["+a"]);

        w.force_rotate().unwrap();
        w.append(&doc("b")).unwrap();
        drop(w);

        let after = tail_shard(dir.path(), Some(caught_up.cursor), 100);
        assert_eq!(doc_ids(&after), vec!["+b"]);
        assert!(
            !after.gap,
            "rotating past a drained generation is not a gap"
        );
    }

    /// Draining a generation that is THEN pruned is still not a gap — the
    /// consumer had already read every entry in it.
    #[test]
    fn pruning_a_generation_we_already_drained_is_not_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();
        let caught_up = tail_shard(dir.path(), None, 100);
        assert_eq!(doc_ids(&caught_up), vec!["+a"]);
        assert_eq!(caught_up.cursor.generation, 0);
        assert!(caught_up.cursor.drained);

        w.force_rotate().unwrap();
        w.append(&doc("b")).unwrap();
        drop(w);
        // The flush path reclaims generation 0 once its entries are durable.
        std::fs::remove_file(wal_path(dir.path(), 0)).unwrap();

        let after = tail_shard(dir.path(), Some(caught_up.cursor), 100);
        assert_eq!(doc_ids(&after), vec!["+b"]);
        assert!(!after.gap);
    }

    /// The honesty contract. Retention never waits for a consumer, so a
    /// consumer that falls far enough behind loses entries — and MUST be told.
    /// This is the case the whole cursor design exists to detect.
    #[test]
    fn a_generation_pruned_before_it_was_read_is_reported_as_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();
        let behind = tail_shard(dir.path(), None, 100);
        assert_eq!(behind.cursor.generation, 0);

        // Two more generations come and go while the consumer is stalled.
        w.force_rotate().unwrap();
        w.append(&doc("b")).unwrap();
        w.force_rotate().unwrap();
        w.append(&doc("c")).unwrap();
        drop(w);
        std::fs::remove_file(wal_path(dir.path(), 0)).unwrap();
        std::fs::remove_file(wal_path(dir.path(), 1)).unwrap();

        let after = tail_shard(dir.path(), Some(behind.cursor), 100);
        assert!(
            after.gap,
            "generation 1 was pruned unread — that is data the target will \
             never receive and it must not be silent"
        );
        assert_eq!(doc_ids(&after), vec!["+c"]);
    }

    /// A stream that has been pruned before starts at the END: its surviving
    /// files are an arbitrary recent slice, and shipping them would look like
    /// a backfill without being one.
    #[test]
    fn a_previously_pruned_stream_starts_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("old")).unwrap();
        w.force_rotate().unwrap();
        w.append(&doc("recent")).unwrap();
        drop(w);
        std::fs::remove_file(wal_path(dir.path(), 0)).unwrap();

        let first = tail_shard(dir.path(), None, 100);
        assert!(
            first.entries.is_empty(),
            "an established index must not half-backfill; got {:?}",
            doc_ids(&first)
        );
        assert!(!first.gap);

        // …and it picks up everything from there on.
        let mut w = make_writer(dir.path());
        w.append(&doc("new")).unwrap();
        drop(w);
        let next = tail_shard(dir.path(), Some(first.cursor), 100);
        assert_eq!(doc_ids(&next), vec!["+new"]);
    }

    /// A half-written frame at the tail of the ACTIVE generation is not
    /// corruption — the writer is mid-append. Hold the boundary and pick the
    /// frame up once it is complete, rather than resyncing over it.
    #[test]
    fn a_half_written_tail_frame_is_waited_for_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();
        drop(w);

        let path = wal_path(dir.path(), 0);
        let good = std::fs::read(&path).unwrap();
        // Append a plausible but incomplete frame header.
        let mut torn = good.clone();
        torn.extend_from_slice(&[0u8; 8]);
        std::fs::write(&path, &torn).unwrap();

        let tail = tail_shard(dir.path(), None, 100);
        assert_eq!(doc_ids(&tail), vec!["+a"]);
        assert!(!tail.cursor.drained);
        assert_eq!(tail.corrupt_skipped, 0, "a torn tail is not corruption");
        assert_eq!(tail.cursor.offset as usize, good.len());
    }

    /// A cursor from a stream that no longer exists — an index deleted and
    /// recreated under the same name — must not stall the consumer forever.
    /// Restart from the beginning and flag it.
    #[test]
    fn a_cursor_past_every_generation_restarts_instead_of_stalling() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("fresh")).unwrap();
        drop(w);

        let stale = WalTailCursor {
            generation: 42,
            offset: 9_000,
            drained: false,
        };
        let tail = tail_shard(dir.path(), Some(stale), 100);
        assert_eq!(doc_ids(&tail), vec!["+fresh"]);
        assert!(
            tail.gap,
            "a rolled-back stream is a reportable discontinuity"
        );
        assert_eq!(tail.cursor.generation, 0);
    }

    /// The delete-and-recreate case the generation check above CANNOT catch:
    /// the new stream restarts at generation 0, so the cursor's generation is
    /// not "past every generation" — it agrees exactly, and only the byte
    /// offset is nonsense.
    ///
    /// Pre-fix the reader clamped that offset to EOF and reported a clean
    /// drain: every document in the recreated index was skipped, `gap` stayed
    /// false and the consumer's stats showed a healthy stream. WAL files are
    /// append-only, so `offset > len` is an exact witness.
    #[test]
    fn a_cursor_past_the_end_of_generation_zero_is_a_gap_not_a_silent_skip() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("recreated")).unwrap();
        drop(w);

        let len = std::fs::metadata(wal_path(dir.path(), 0)).unwrap().len();
        let stale = WalTailCursor {
            generation: 0,
            offset: len + 4_096,
            drained: false,
        };
        let tail = tail_shard(dir.path(), Some(stale), 100);
        assert_eq!(
            doc_ids(&tail),
            vec!["+recreated"],
            "the recreated stream must be read from the start, not skipped"
        );
        assert!(tail.gap, "and the discontinuity must be reported");
    }

    /// The other half of the same failure: the stale offset lands INSIDE the
    /// new file but not on a frame boundary.
    ///
    /// The active generation refuses to resync, so pre-fix this wedged the
    /// shard forever — no entries, no gap, no error, and the file kept
    /// growing underneath it. A half-written frame is by construction the last
    /// thing in the file, so a valid frame existing *after* the bad bytes
    /// proves these are not a partial tail.
    #[test]
    fn a_cursor_landing_mid_frame_in_the_active_generation_resyncs_instead_of_wedging() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        for i in 0..6 {
            w.append(&doc(&format!("d{i}"))).unwrap();
        }
        drop(w);

        // A few bytes past the header is mid-first-frame: not a boundary.
        let stale = WalTailCursor {
            generation: 0,
            offset: WAL_HEADER_LEN + 7,
            drained: false,
        };
        let tail = tail_shard(dir.path(), Some(stale), 100);
        assert!(
            !tail.entries.is_empty(),
            "the reader must resync forward rather than hold a boundary that is not one"
        );
        assert!(
            tail.corrupt_skipped > 0,
            "and the resync must be counted, not silent"
        );
        assert!(
            tail.cursor.offset > stale.offset,
            "the cursor has to move, or the next poll repeats this forever"
        );
    }

    /// Reading the tail must cost the bytes appended since the last poll, not
    /// the size of the file.
    ///
    /// The pre-fix reader did `fs::read(path)` on every poll and seeked inside
    /// the resulting `Vec`: fully caught up, it still allocated and copied the
    /// whole active generation just to find EOF. At 16 shards and a 500 ms
    /// interval that is ~60x read amplification of the node's live ingest
    /// volume per flush window, per shard.
    ///
    /// Asserted **through `tail_shard`**, on the bytes it reports reading.
    /// An earlier version of this test hand-built a `WalWindow`, seeked it
    /// past the header read and asserted on `win.buf.len()`; it never called
    /// the entry point, so it could not fail if this path regressed to
    /// `fs::read(path)` — and it did not fail on the 64 KiB header read-ahead
    /// that made a caught-up poll cost 65,536 bytes rather than 16.
    #[test]
    fn a_caught_up_poll_does_not_read_the_whole_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        // WAL payloads are LZ4-compressed, so a run of one character would
        // produce a tiny file and prove nothing. Generate incompressible hex.
        let mut lcg: u64 = 0x2545_F491_4F6C_DD1D;
        let mut noise = || {
            let mut s = String::with_capacity(8192);
            for _ in 0..1024 {
                lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                s.push_str(&format!("{:016x}", lcg));
            }
            s
        };
        for i in 0..256 {
            w.append(&WalEntry::Index {
                doc_id: format!("d{i}"),
                source: serde_json::json!({ "v": noise() }),
            })
            .unwrap();
        }
        drop(w);

        let caught_up = tail_shard(dir.path(), None, 10_000);
        assert_eq!(caught_up.entries.len(), 256);
        let len = std::fs::metadata(wal_path(dir.path(), 0)).unwrap().len();
        assert!(len > 512 * 1024, "the file must be big enough to matter");

        // The poll that matters: the cursor is at EOF, nothing was appended.
        // It must read the 16-byte file header to identify the generation and
        // then stop — not the file (`fs::read`, the original shape), and not a
        // 64 KiB read-ahead chunk it immediately throws away.
        let idle = tail_shard(dir.path(), Some(caught_up.cursor), 10_000);
        assert!(idle.entries.is_empty(), "nothing was appended");
        assert_eq!(
            idle.bytes_read, WAL_HEADER_LEN,
            "a caught-up poll read {} bytes of a {len}-byte generation; it must read \
             the {WAL_HEADER_LEN}-byte header and stop",
            idle.bytes_read
        );

        // And it is still correct: a poll after new appends reads only those,
        // plus the header — never the history in front of them.
        let mut w = make_writer(dir.path());
        w.append(&doc("late")).unwrap();
        drop(w);
        let appended = std::fs::metadata(wal_path(dir.path(), 0)).unwrap().len() - len;
        let after = tail_shard(dir.path(), Some(idle.cursor), 10_000);
        assert_eq!(doc_ids(&after), vec!["+late"]);
        assert!(
            after.bytes_read <= WAL_HEADER_LEN + appended,
            "a poll that is one frame behind read {} bytes for {appended} bytes of new \
             data in a {len}-byte generation",
            after.bytes_read
        );
    }

    /// The same property one level up: `tail_shard` must not read the history
    /// in front of a cursor that is far into an established stream.
    ///
    /// `fs::read(path)` — the shape this replaced — passes
    /// [`a_caught_up_poll_does_not_read_the_whole_generation`]'s *correctness*
    /// assertions perfectly. Only the byte count separates them.
    #[test]
    fn a_poll_from_the_middle_of_a_generation_reads_forward_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        let mut lcg: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut noise = || {
            let mut s = String::with_capacity(8192);
            for _ in 0..1024 {
                lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                s.push_str(&format!("{:016x}", lcg));
            }
            s
        };
        for i in 0..256 {
            w.append(&WalEntry::Index {
                doc_id: format!("d{i}"),
                source: serde_json::json!({ "v": noise() }),
            })
            .unwrap();
        }
        // Read the whole thing once so the cursor sits at the end…
        let full = tail_shard(dir.path(), None, 10_000);
        assert_eq!(full.entries.len(), 256);
        let history = std::fs::metadata(wal_path(dir.path(), 0)).unwrap().len();
        assert!(history > 512 * 1024);

        // …then append one small frame and poll for exactly it.
        w.append(&doc("tail")).unwrap();
        drop(w);
        let grown = std::fs::metadata(wal_path(dir.path(), 0)).unwrap().len();
        let appended = grown - history;
        assert!(appended < 4096, "the new frame must be small: {appended}");

        let incremental = tail_shard(dir.path(), Some(full.cursor), 10_000);
        assert_eq!(doc_ids(&incremental), vec!["+tail"]);
        assert!(
            incremental.bytes_read < history / 4,
            "reading {appended} new bytes cost {} of a {grown}-byte file — the poll is \
             re-reading history",
            incremental.bytes_read
        );
    }

    /// A rotated generation still resyncs past a corrupt region, exactly as
    /// full replay does — the windowed reader must not have quietly changed
    /// that.
    #[test]
    fn a_rotated_generation_still_resyncs_past_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = make_writer(dir.path());
        w.append(&doc("a")).unwrap();
        w.append(&doc("b")).unwrap();
        let good = std::fs::read(wal_path(dir.path(), 0)).unwrap();
        w.rotate().unwrap();
        w.append(&doc("c")).unwrap();
        drop(w);

        // Corrupt the middle of the now-frozen generation 0.
        let mut damaged = good.clone();
        let mid = WAL_HEADER_LEN as usize + 4;
        damaged[mid] ^= 0xFF;
        std::fs::write(wal_path(dir.path(), 0), &damaged).unwrap();

        let tail = tail_shard(dir.path(), None, 100);
        assert!(
            tail.corrupt_skipped > 0,
            "damage in a frozen generation must be counted"
        );
        assert!(
            doc_ids(&tail).contains(&"+c".to_string()),
            "and the reader must get past it to the live generation: {:?}",
            doc_ids(&tail)
        );
    }

    /// The retention floor (#320): `prune_verified` keeps the newest N rotated
    /// generations even when it can prove every entry in them durable, so a
    /// WAL consumer that is briefly behind still finds them.
    ///
    /// Bounded by construction — it is a floor, not a lease, so it holds N
    /// files whether the consumer is one second or one week behind.
    #[test]
    fn a_retention_floor_holds_the_newest_rotated_generations() {
        fn build(dir: &Path) -> WalWriter {
            let mut w = make_writer(dir);
            for gen in 0..4 {
                w.append(&doc(&format!("g{gen}"))).unwrap();
                w.rotate().unwrap();
            }
            w.append(&doc("active")).unwrap();
            w
        }

        // Floor 0 (the default): everything provable is pruned.
        let bare = tempfile::tempdir().unwrap();
        let w = build(bare.path());
        assert_eq!(w.rotated_generations().unwrap().len(), 4);
        assert_eq!(w.prune_verified(&|_, _| true).unwrap(), 4);
        assert!(w.rotated_generations().unwrap().is_empty());

        // Floor 2: the two newest rotated generations survive.
        let floored = tempfile::tempdir().unwrap();
        let mut w = build(floored.path());
        w.set_min_retained_generations(2);
        assert_eq!(w.prune_verified(&|_, _| true).unwrap(), 2);
        assert_eq!(
            w.rotated_generations().unwrap(),
            vec![2, 3],
            "the floor must keep the NEWEST generations — the oldest are the ones a \
             consumer has already passed"
        );
        // And it is a floor, not a lease: pruning again holds the same two,
        // however far behind any consumer is.
        assert_eq!(w.prune_verified(&|_, _| true).unwrap(), 0);
        assert_eq!(w.rotated_generations().unwrap(), vec![2, 3]);
    }

    /// Both WAL layouts are discovered: legacy `*.wal` in the root, and the
    /// sharded `s0`/`s1`/… subdirectories.
    #[test]
    fn shard_dirs_cover_both_layouts() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        make_writer(&legacy).append(&doc("a")).unwrap();
        assert_eq!(
            wal_shard_dirs(&legacy)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>(),
            vec![""]
        );

        let sharded = root.path().join("sharded");
        for shard in ["s0", "s1"] {
            make_writer(&sharded.join(shard)).append(&doc("a")).unwrap();
        }
        assert_eq!(
            wal_shard_dirs(&sharded)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>(),
            vec!["s0", "s1"]
        );
    }
}
