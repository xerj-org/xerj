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
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
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

// ── WalWriter ────────────────────────────────────────────────────────────────

/// Appends entries to the Write-Ahead Log.
///
/// This type is wrapped in `Arc<Mutex<...>>` inside [`crate::index_store::IndexStore`]
/// so multiple threads can serialize through it.  WAL writes are deliberately
/// **synchronous**: `append` takes a `&mut self` and does a blocking write so
/// the caller knows data is durable (in `Strict` mode) before returning.
pub struct WalWriter {
    dir: PathBuf,
    generation: u64,
    writer: BufWriter<File>,
    current_offset: u64,
    max_size_bytes: u64,
    sync_mode: SyncMode,
    /// Shared sequence number counter — also used by the index store.
    pub seq_counter: Arc<AtomicU64>,
}

impl WalWriter {
    /// Open (or create) the WAL in `dir`.
    ///
    /// If a WAL file for the latest generation already exists it is opened for
    /// append; otherwise a new generation-0 file is created.
    pub fn open(
        dir: impl AsRef<Path>,
        max_size_bytes: u64,
        sync_mode: SyncMode,
        seq_counter: Arc<AtomicU64>,
    ) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        // Find the highest existing generation
        let (generation, start_seq) = find_latest_generation(&dir)?;

        // Sync the seq counter with what was recovered from the WAL
        if start_seq > seq_counter.load(Ordering::Acquire) {
            seq_counter.store(start_seq, Ordering::Release);
        }

        let path = wal_path(&dir, generation);
        let file = if path.exists() {
            OpenOptions::new().append(true).open(&path)?
        } else {
            create_wal_file(&path, generation)?
        };
        let current_offset = file.metadata()?.len();

        Ok(Self {
            dir,
            generation,
            writer: BufWriter::with_capacity(8 * 1024 * 1024, file),
            current_offset,
            max_size_bytes,
            sync_mode,
            seq_counter,
        })
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

        // CRC covers seq_no(8) + op(1) + payload(n)
        let mut hasher = Crc32Hasher::new();
        let mut seq_buf = [0u8; 8];
        (&mut seq_buf[..])
            .write_u64::<LittleEndian>(seq_no)
            .unwrap();
        hasher.update(&seq_buf);
        hasher.update(&[op_code]);
        hasher.update(&payload);
        let crc = hasher.finalize();

        let entry_len = payload.len() as u32;
        self.writer.write_u32::<LittleEndian>(entry_len)?;
        self.writer.write_u64::<LittleEndian>(seq_no)?;
        self.writer.write_u8(op_code)?;
        self.writer.write_all(&payload)?;
        self.writer.write_u32::<LittleEndian>(crc)?;

        let written = 4 + 8 + 1 + payload.len() as u64 + 4;
        self.current_offset += written;

        if self.sync_mode == SyncMode::Strict {
            self.sync()?;
        } else {
            // Batched: drain the BufWriter to the kernel page cache so a
            // process drop / panic / SIGKILL leaves a recoverable WAL.
            // Skips fsync(2) — bytes survive process death (kernel keeps
            // the page cache) but not power loss until the next sync().
            // Mirrors what `wal_append_batch` does after its frame write.
            self.writer.flush()?;
        }

        debug!(seq_no, op = op_code, bytes = written, "WAL append");
        Ok(seq_no)
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

        let mut hasher = Crc32Hasher::new();
        let mut seq_buf = [0u8; 8];
        (&mut seq_buf[..])
            .write_u64::<LittleEndian>(seq_no)
            .unwrap();
        hasher.update(&seq_buf);
        hasher.update(&[op_code]);
        hasher.update(&payload);
        let crc = hasher.finalize();

        let entry_len = payload.len() as u32;
        self.writer.write_u32::<LittleEndian>(entry_len)?;
        self.writer.write_u64::<LittleEndian>(seq_no)?;
        self.writer.write_u8(op_code)?;
        self.writer.write_all(&payload)?;
        self.writer.write_u32::<LittleEndian>(crc)?;

        let written = 4 + 8 + 1 + payload.len() as u64 + 4;
        self.current_offset += written;

        if self.sync_mode == SyncMode::Strict {
            self.sync()?;
        }
        Ok(seq_no)
    }

    /// Write a pre-assembled batch of framed bytes.  Caller must hold
    /// the WAL mutex.  `total_written` is the byte count to add to
    /// `current_offset`.
    pub fn append_frames_locked(&mut self, frames: &[u8], total_written: u64) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        if self.current_offset > self.max_size_bytes {
            self.rotate()?;
        }
        self.writer.write_all(frames)?;
        self.current_offset += total_written;
        if self.sync_mode == SyncMode::Strict {
            self.sync()?;
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
        self.writer.write_all(&out)?;
        self.current_offset += written_total;

        if self.sync_mode == SyncMode::Strict {
            self.sync()?;
        }
        Ok(seq_nos)
    }

    /// Flush the write buffer and call `fsync`.
    pub fn sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(())
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
        self.writer.flush()?;
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
    pub fn prune_verified(
        &self,
        verify: &dyn Fn(&WalEntry, SeqNo) -> bool,
    ) -> Result<usize> {
        let active_gen = self.generation;
        let entries = fs::read_dir(&self.dir)?;
        let mut wal_files: Vec<(u64, std::path::PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".wal") {
                if let Some(gen) = parse_wal_generation(&name_str) {
                    if gen < active_gen {
                        wal_files.push((gen, entry.path()));
                    }
                }
            }
        }

        let mut pruned = 0usize;
        for (gen, path) in &wal_files {
            let (gen_entries, clean) = self.read_generation_entries(*gen);
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
            match fs::remove_file(path) {
                Ok(()) => {
                    pruned += 1;
                    debug!(gen, entries = gen_entries.len(), "pruned WAL generation");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    debug!(gen, "WAL file already pruned by sibling flush — ok");
                }
                Err(e) => return Err(e.into()),
            }
            let ck = checkpoint_path(&self.dir, *gen);
            let _ = fs::remove_file(&ck);
        }
        Ok(pruned)
    }

    fn rotate(&mut self) -> Result<()> {
        self.sync()?;
        self.generation += 1;
        let path = wal_path(&self.dir, self.generation);
        let file = create_wal_file(&path, self.generation)?;
        self.writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
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
        Ok(())
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

/// Read all entries from a single WAL file.
///
/// RC4 W1 #8: checkpoint-based skipping is GONE from replay (see
/// `WalReader::replay` docs).  The 2026-07 S3 "offset-bounded skip"
/// hardening was the previous, weaker containment of the same bug class
/// (stale/over-broad checkpoints discarding acked entries); removing the
/// skip entirely is its logical conclusion — replay-side loss is now
/// structurally impossible, and dedup of already-flushed entries is the
/// job of the idempotent replay consumers.
fn read_wal_file(path: PathBuf) -> Vec<Result<ReplayEntry>> {
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => return vec![Err(e.into())],
    };
    let mut reader = BufReader::new(file);

    // Read + validate header
    let mut magic = [0u8; 4];
    if reader.read_exact(&mut magic).is_err() {
        return vec![];
    }
    if &magic != WAL_MAGIC {
        return vec![Err(StorageError::InvalidMagic {
            expected: WAL_MAGIC,
            actual: magic.to_vec(),
        })];
    }
    let generation = match reader.read_u64::<LittleEndian>() {
        Ok(g) => g,
        Err(e) => return vec![Err(e.into())],
    };
    let _reserved = reader.read_u32::<LittleEndian>(); // ignored

    let mut file_offset = WAL_HEADER_LEN;
    let mut results = Vec::new();

    loop {
        let entry_len = match reader.read_u32::<LittleEndian>() {
            Ok(l) => l,
            Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                results.push(Err(e.into()));
                break;
            }
        };
        let seq_no = match reader.read_u64::<LittleEndian>() {
            Ok(s) => s,
            Err(e) => {
                results.push(Err(e.into()));
                break;
            }
        };
        let op_code = match reader.read_u8() {
            Ok(o) => o,
            Err(e) => {
                results.push(Err(e.into()));
                break;
            }
        };
        let mut payload = vec![0u8; entry_len as usize];
        if let Err(e) = reader.read_exact(&mut payload) {
            results.push(Err(StorageError::WalCorrupt(
                file_offset,
                format!("gen={generation} truncated payload: {e}"),
            )));
            break;
        }
        let stored_crc = match reader.read_u32::<LittleEndian>() {
            Ok(c) => c,
            Err(e) => {
                results.push(Err(e.into()));
                break;
            }
        };

        // Verify CRC
        let mut hasher = Crc32Hasher::new();
        let mut seq_buf = [0u8; 8];
        (&mut seq_buf[..])
            .write_u64::<LittleEndian>(seq_no)
            .unwrap();
        hasher.update(&seq_buf);
        hasher.update(&[op_code]);
        hasher.update(&payload);
        let computed_crc = hasher.finalize();
        if stored_crc != computed_crc {
            results.push(Err(StorageError::ChecksumMismatch {
                expected: stored_crc,
                actual: computed_crc,
            }));
            break;
        }

        let entry_total = 4u64 + 8 + 1 + entry_len as u64 + 4;
        let this_offset = file_offset;
        file_offset += entry_total;

        // Check and strip the compression flag from op_code.
        let is_compressed = (op_code & OP_COMPRESSED_FLAG) != 0;
        let raw_op_code = op_code & !OP_COMPRESSED_FLAG;

        // Decode the entry
        let op = match WalOpCode::from_u8(raw_op_code) {
            Some(o) => o,
            None => {
                warn!(
                    op_code = raw_op_code,
                    "unknown WAL op code — skipping entry"
                );
                continue;
            }
        };

        // Decompress payload if compressed.
        let payload = if is_compressed {
            match decompress_size_prepended(&payload) {
                Ok(dec) => dec,
                Err(e) => {
                    results.push(Err(StorageError::WalCorrupt(
                        this_offset,
                        format!("gen={generation} lz4 decompression failed: {e}"),
                    )));
                    break;
                }
            }
        } else {
            payload
        };

        let entry: WalEntry = match serde_json::from_slice(&payload) {
            Ok(e) => e,
            Err(e) => {
                results.push(Err(StorageError::WalCorrupt(
                    this_offset,
                    format!("gen={generation} json error op={op:?}: {e}"),
                )));
                break;
            }
        };

        // Sanity-check op code matches enum variant
        let expected_op = match &entry {
            WalEntry::Index { .. } => WalOpCode::Index,
            WalEntry::Delete { .. } => WalOpCode::Delete,
            WalEntry::UpdateMapping { .. } => WalOpCode::UpdateMapping,
        };
        if op != expected_op {
            results.push(Err(StorageError::WalCorrupt(
                this_offset,
                format!("op code mismatch: header={op:?}, payload={expected_op:?}"),
            )));
            break;
        }

        results.push(Ok(ReplayEntry {
            seq_no,
            entry,
            file_offset: this_offset,
        }));
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

fn scan_seq_nos(path: PathBuf) -> io::Result<Option<SeqNo>> {
    let file = File::open(&path)?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 4];
    if reader.read_exact(&mut magic).is_err() {
        return Ok(None);
    }
    if &magic != WAL_MAGIC {
        return Ok(None);
    }
    let _ = reader.read_u64::<LittleEndian>()?; // generation
    let _ = reader.read_u32::<LittleEndian>()?; // reserved

    let mut max_seq = None;
    loop {
        let entry_len = match reader.read_u32::<LittleEndian>() {
            Ok(l) => l,
            Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        let seq_no = reader.read_u64::<LittleEndian>()?;
        max_seq = Some(max_seq.unwrap_or(0).max(seq_no));
        let _op = reader.read_u8()?;
        reader.seek(SeekFrom::Current(entry_len as i64 + 4))?; // skip payload + crc
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
}
