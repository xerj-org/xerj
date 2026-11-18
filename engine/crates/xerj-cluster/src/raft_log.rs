//! Persistent Raft log storage.
//!
//! This is the M5 v4 milestone unblock: the Raft state machine (see
//! [`crate::raft`]) keeps its log in a `Vec<LogEntry>` in RAM which is lost on
//! restart.  That makes the whole cluster design unsafe — acked writes
//! vanish if a leader crashes before its followers receive the commit.
//!
//! `FileRaftLog` stores entries in an append-only file inside
//! `{data_dir}/raft/` with the same framing we already use for the index
//! WAL (see `xerj_storage::wal`):
//!
//! ```text
//!     u32  magic = 0x5A_52_4C_32  ("ZRL2")
//!     u64  entry length
//!     u64  term
//!     u64  index
//!     u8   payload kind
//!     u64  payload length
//!     bytes payload
//!     u32  crc32 of the preceding header + payload
//! ```
//!
//! The file is append-only while the node is running.  On restart we scan
//! the file from start, replay every entry into an in-memory
//! `BTreeMap<index, (term, file_offset, payload_length)>`, and re-derive
//! `commit_index` + `last_applied` from a sidecar `commit.meta` file.
//!
//! The server opens this log at `{data_dir}/_raft` (`RAFT_DIR_NAME`).
//! `_raft` is not a valid index name, and the directory has no `wal`
//! child, so the engine's index scan does not open it.
//!
//! `hard.meta` stores the current term, the vote, and the logical end of
//! the log. Truncation does not rewrite `raft.log`; entries past that end
//! stay in the file and are ignored on the next open. A missing `hard.meta`
//! (a log written before this file existed) keeps every CRC-valid record.
//!
//! Exit criterion for M5.1 (verified by `raft_log_persist_test`):
//!
//! ```ignore
//! let path = tempdir()?;
//! let mut log = FileRaftLog::open(&path)?;
//! for i in 0..1000 { log.append(Entry::new(1, i, "cmd".into()))?; }
//! log.fsync()?;
//! drop(log);
//!
//! let log2 = FileRaftLog::open(&path)?;
//! assert_eq!(log2.len(), 1000);
//! assert_eq!(log2.entry(500)?.term, 1);
//! ```

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use crc32fast::Hasher as Crc32;

/// Magic for the raft log file format.  Distinct from the WAL magic so
/// we never confuse one for the other on recovery.
const MAGIC: u32 = 0x5A52_4C32; // "ZRL2" (ASCII Z R L 2)
const HARD_MAGIC: u32 = 0x5A52_4853; // "ZRHS"
const LOG_FILE: &str = "raft.log";
const COMMIT_FILE: &str = "commit.meta";
const HARD_FILE: &str = "hard.meta";
const SNAPSHOT_FILE: &str = "snapshot.meta";
const SNAPSHOT_MAGIC: u32 = 0x5A52_534E; // "ZRSN"

/// Directory name under the node's data dir. Not a valid index name, so a
/// user index cannot occupy the same path and take the log with it on delete.
pub const RAFT_DIR_NAME: &str = "_raft";

/// One on-disk log record. `append` and log compaction both use this so a
/// rewritten file scans the same way as a file that was only appended to.
fn encode_entry(term: u64, index: u64, payload: &[u8]) -> Vec<u8> {
    let payload_len = payload.len() as u64;
    // Entry length covers everything after the len field up to the CRC,
    // inclusive — useful for future forward-compatible skipping.
    let entry_len: u64 = 8 + 8 + 1 + 8 + payload_len + 4;
    let kind = EntryKind::ClusterCommand as u8;
    let mut h = Crc32::new();
    h.update(&MAGIC.to_le_bytes());
    h.update(&entry_len.to_le_bytes());
    h.update(&term.to_le_bytes());
    h.update(&index.to_le_bytes());
    h.update(&[kind]);
    h.update(&payload_len.to_le_bytes());
    h.update(payload);
    let crc = h.finalize();

    let mut buf = Vec::with_capacity(4 + entry_len as usize);
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&entry_len.to_le_bytes());
    buf.extend_from_slice(&term.to_le_bytes());
    buf.extend_from_slice(&index.to_le_bytes());
    buf.push(kind);
    buf.extend_from_slice(&payload_len.to_le_bytes());
    buf.extend_from_slice(payload);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

fn sync_dir(dir: &Path) -> Result<()> {
    let file = File::open(dir).with_context(|| format!("opening {dir:?} to sync"))?;
    file.sync_all()
        .with_context(|| format!("syncing raft dir {dir:?}"))?;
    Ok(())
}

/// `{data_dir}/_raft`, where the server opens [`FileRaftLog`].
pub fn raft_storage_dir(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join(RAFT_DIR_NAME)
}

/// Raft payload kinds.  We only use `ClusterCommand` today but leave room
/// for future out-of-band messages (config changes, snapshots).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    ClusterCommand = 0,
    ConfigChange = 1,
    Snapshot = 2,
}

/// A single persisted log entry.
///
/// The payload is the `serde_json::to_vec` form of a
/// [`crate::raft::LogEntry::command`] — kept as raw bytes here so the
/// storage layer doesn't depend on the raft module's own command enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedEntry {
    pub term: u64,
    pub index: u64,
    pub kind: EntryKind,
    pub payload: Vec<u8>,
}

/// File-backed Raft log.
///
/// Owns the write handle to `{dir}/raft.log` and a seek table mapping
/// Raft log `index` → (term, file offset, payload length).  The seek
/// table is rebuilt on open from the log file.
pub struct FileRaftLog {
    dir: PathBuf,
    writer: BufWriter<File>,
    /// `index → (term, absolute file offset of the entry header,
    /// payload length)`.  Sorted by index.
    seek_table: BTreeMap<u64, (u64, u64, u64)>,
    /// Highest index appended so far (0 if empty).
    last_index: u64,
    /// Commit index persisted to `commit.meta`.
    commit_index: u64,
    /// Current term persisted in `hard.meta`. 0 when that file is absent.
    hard_term: u64,
    /// Vote in `hard_term`, when one was persisted.
    hard_vote: Option<String>,
    /// Logical end of the log. `None` means `hard.meta` did not record one
    /// and the scan is authoritative. `Some(0)` is an empty log.
    log_end: Option<u64>,
    /// Raw snapshot payload from `snapshot.meta`, when that file exists.
    /// The Raft node decodes it. A missing file is a log that has not been
    /// compacted. A truncated or checksum-mismatched file fails the open.
    snapshot: Option<Vec<u8>>,
}

impl FileRaftLog {
    /// Open (or create) a Raft log directory.  Rebuilds the seek table
    /// from the on-disk file.  Skips any corrupt trailing record.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating raft dir {dir:?}"))?;
        let log_path = dir.join(LOG_FILE);

        // Scan the file (if any) to rebuild the seek table.
        let mut seek_table: BTreeMap<u64, (u64, u64, u64)> = BTreeMap::new();
        let mut last_index: u64 = 0;

        if log_path.exists() {
            let file = File::open(&log_path).with_context(|| format!("opening {log_path:?}"))?;
            let mut reader = BufReader::new(file);

            loop {
                let header_offset = reader
                    .stream_position()
                    .with_context(|| "seek in raft log")?;

                // Try to read a record header.  On EOF or partial read, stop
                // cleanly — the writer's append-only contract means the only
                // time we see a partial record is at the file tail after a
                // crash mid-fsync, in which case we discard it.
                let magic = match reader.read_u32::<LittleEndian>() {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(anyhow!("reading magic: {e}")),
                };
                if magic != MAGIC {
                    // Corrupt tail — truncate and stop.
                    break;
                }
                let entry_len = match reader.read_u64::<LittleEndian>() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let term = match reader.read_u64::<LittleEndian>() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let index = match reader.read_u64::<LittleEndian>() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let _kind = match reader.read_u8() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let payload_len = match reader.read_u64::<LittleEndian>() {
                    Ok(v) => v,
                    Err(_) => break,
                };

                // Read the payload + crc.
                let mut payload = vec![0u8; payload_len as usize];
                if reader.read_exact(&mut payload).is_err() {
                    break;
                }
                let expected_crc = match reader.read_u32::<LittleEndian>() {
                    Ok(v) => v,
                    Err(_) => break,
                };

                // Verify CRC — if it mismatches, we hit a torn write.
                let mut h = Crc32::new();
                h.update(&MAGIC.to_le_bytes());
                h.update(&entry_len.to_le_bytes());
                h.update(&term.to_le_bytes());
                h.update(&index.to_le_bytes());
                h.update(&[_kind]);
                h.update(&payload_len.to_le_bytes());
                h.update(&payload);
                if h.finalize() != expected_crc {
                    break;
                }

                // New entries overwrite older ones at the same index
                // (leader truncation on term change); BTreeMap::insert
                // handles it for us.
                seek_table.insert(index, (term, header_offset, payload_len));
                if index > last_index {
                    last_index = index;
                }
            }
        }

        // Reopen the file for append.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(false)
            .open(&log_path)
            .with_context(|| format!("opening {log_path:?} for append"))?;
        let writer = BufWriter::with_capacity(64 * 1024, file);

        // Load committed index (best-effort).
        let commit_index = Self::load_commit_index(&dir).unwrap_or(0);
        let (hard_term, hard_vote, log_end) = Self::load_hard_state(&dir)?;
        if let Some(end) = log_end {
            seek_table.retain(|&idx, _| idx <= end);
            last_index = seek_table.keys().next_back().copied().unwrap_or(0);
        }
        let snapshot = Self::load_snapshot(&dir)?;

        Ok(Self {
            dir,
            writer,
            seek_table,
            last_index,
            commit_index,
            hard_term,
            hard_vote,
            log_end,
            snapshot,
        })
    }

    fn load_commit_index(dir: &Path) -> Option<u64> {
        let path = dir.join(COMMIT_FILE);
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() < 8 {
            return None;
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes[..8]);
        Some(u64::from_le_bytes(arr))
    }

    /// Append one entry to the log.  Does NOT fsync — call
    /// [`FileRaftLog::fsync`] after a batch to make the entries durable.
    pub fn append(&mut self, term: u64, index: u64, payload: &[u8]) -> Result<()> {
        let header_offset = self.current_offset()?;
        let record = encode_entry(term, index, payload);
        self.writer.write_all(&record)?;

        self.seek_table
            .insert(index, (term, header_offset, payload.len() as u64));
        if index > self.last_index {
            self.last_index = index;
        }
        Ok(())
    }

    /// Bytes of `snapshot.meta`, if a snapshot has been saved.
    pub fn snapshot_bytes(&self) -> Option<&[u8]> {
        self.snapshot.as_deref()
    }

    /// Replace `snapshot.meta`. The in-memory copy updates only after the
    /// rename and directory sync, so a failed write leaves the previous
    /// snapshot in place.
    pub fn save_snapshot(&mut self, payload: &[u8]) -> Result<()> {
        let path = self.dir.join(SNAPSHOT_FILE);
        let tmp = self.dir.join("snapshot.meta.tmp");
        let mut buf = Vec::with_capacity(16 + payload.len() + 4);
        buf.extend_from_slice(&SNAPSHOT_MAGIC.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        let len = u64::try_from(payload.len()).context("raft snapshot longer than u64")?;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(payload);
        let mut h = Crc32::new();
        h.update(&buf);
        buf.extend_from_slice(&h.finalize().to_le_bytes());
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .with_context(|| format!("writing {tmp:?}"))?;
            file.write_all(&buf)
                .with_context(|| format!("writing {tmp:?}"))?;
            file.sync_all()
                .with_context(|| format!("syncing {tmp:?}"))?;
        }
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {path:?}"))?;
        sync_dir(&self.dir)?;
        self.snapshot = Some(payload.to_vec());
        Ok(())
    }

    /// Rewrite `raft.log` so it contains only entries with `index > through`.
    ///
    /// The new file is synced and renamed over the old one before the seek
    /// table drops the prefix. A failed write leaves the previous file, and
    /// its entries, in place.
    pub fn rewrite_without_prefix(&mut self, through: u64) -> Result<()> {
        if self.seek_table.keys().all(|idx| *idx > through) {
            return Ok(());
        }
        self.writer
            .flush()
            .context("flush raft log before compaction")?;
        let kept_idx: Vec<u64> = self
            .seek_table
            .keys()
            .copied()
            .filter(|idx| *idx > through)
            .collect();
        let mut kept = Vec::with_capacity(kept_idx.len());
        for idx in kept_idx {
            let (term, payload) = self.read(idx)?.ok_or_else(|| {
                anyhow!("raft entry {idx} disappeared while compacting the log prefix")
            })?;
            kept.push((term, idx, payload));
        }

        let new_path = self.dir.join("raft.log.new");
        let mut new_table: BTreeMap<u64, (u64, u64, u64)> = BTreeMap::new();
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&new_path)
                .with_context(|| format!("writing {new_path:?}"))?;
            let mut offset = 0u64;
            for (term, index, payload) in &kept {
                let record = encode_entry(*term, *index, payload);
                file.write_all(&record)
                    .with_context(|| format!("writing {new_path:?}"))?;
                new_table.insert(*index, (*term, offset, payload.len() as u64));
                offset += record.len() as u64;
            }
            file.sync_all()
                .with_context(|| format!("syncing {new_path:?}"))?;
        }
        let final_path = self.dir.join(LOG_FILE);
        std::fs::rename(&new_path, &final_path)
            .with_context(|| format!("replacing {final_path:?}"))?;
        sync_dir(&self.dir)?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(false)
            .open(&final_path)
            .with_context(|| format!("reopening {final_path:?} after compaction"))?;
        self.writer = BufWriter::with_capacity(64 * 1024, file);
        self.seek_table = new_table;
        self.last_index = self.seek_table.keys().next_back().copied().unwrap_or(0);
        self.set_log_end(self.last_index)?;
        Ok(())
    }

    /// Force durability — flushes buffered writes and `fsync`s the
    /// underlying file.  Callers should batch appends and then fsync
    /// at the end of the batch.
    pub fn fsync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        Ok(())
    }

    fn current_offset(&mut self) -> Result<u64> {
        // BufWriter doesn't expose the underlying cursor directly; we
        // flush then ask the file for the position.  Flush is cheap
        // because append-only keeps the buffer small.
        self.writer.flush()?;
        let pos = self.writer.get_mut().stream_position()?;
        Ok(pos)
    }

    /// Read an entry back from disk by log index.
    ///
    /// Returns `None` if the index has not been written or has been
    /// truncated away.
    pub fn read(&mut self, index: u64) -> Result<Option<(u64, Vec<u8>)>> {
        let Some(&(term, offset, payload_len)) = self.seek_table.get(&index) else {
            return Ok(None);
        };
        // Open a fresh read handle so we don't fight with the writer's
        // buffered position.
        let log_path = self.dir.join(LOG_FILE);
        let mut file =
            File::open(&log_path).with_context(|| format!("reopening {log_path:?} for read"))?;
        // Header is: magic(4) + entry_len(8) + term(8) + index(8) + kind(1) + payload_len(8)
        // = 37 bytes.  Then the payload.
        file.seek(SeekFrom::Start(offset + 37))
            .with_context(|| "seek to entry payload")?;
        let mut payload = vec![0u8; payload_len as usize];
        file.read_exact(&mut payload)
            .with_context(|| "reading entry payload")?;
        Ok(Some((term, payload)))
    }

    /// Drop every seek-table entry with `index >= from`.
    ///
    /// The file is not rewritten. The drop becomes durable when
    /// [`FileRaftLog::set_log_end`] records the new end; until that file
    /// is replaced, a crash reopens the previous tail. Callers that have
    /// already decided the tail is gone must set the end before they
    /// acknowledge the append that conflicted.
    pub fn truncate_from(&mut self, from: u64) {
        self.seek_table.retain(|&idx, _| idx < from);
        self.last_index = self
            .seek_table
            .iter()
            .next_back()
            .map(|(k, _)| *k)
            .unwrap_or(0);
    }

    /// Persist the `commit_index` to a sidecar file.  Called by the
    /// Raft state machine after every commit-index advance.
    pub fn set_commit_index(&mut self, idx: u64) -> Result<()> {
        self.commit_index = idx;
        let path = self.dir.join(COMMIT_FILE);
        let tmp = self.dir.join("commit.meta.tmp");
        std::fs::write(&tmp, idx.to_le_bytes())?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Term and vote from `hard.meta`. Term 0 and a missing vote mean the
    /// file was absent.
    pub fn hard_term(&self) -> u64 {
        self.hard_term
    }

    pub fn hard_vote(&self) -> Option<&str> {
        self.hard_vote.as_deref()
    }

    /// Logical end recorded in `hard.meta`, if that file had one.
    pub fn persisted_log_end(&self) -> Option<u64> {
        self.log_end
    }

    /// Record the current term and vote. The logical log end is left as it is.
    ///
    /// Memory is updated only after `hard.meta` is replaced. A failed write
    /// leaves the previous term and vote in place, so a later log-end save
    /// cannot publish an election this process aborted.
    pub fn set_hard_state(&mut self, term: u64, voted_for: Option<&str>) -> Result<()> {
        self.persist_hard(term, voted_for, self.log_end)?;
        self.hard_term = term;
        self.hard_vote = voted_for.map(str::to_string);
        Ok(())
    }

    /// Record the highest index that is still part of the log.
    ///
    /// The file is replaced before the seek table drops indexes above `end`.
    /// A failed write leaves those entries readable. The next open ignores
    /// them once this call has succeeded.
    pub fn set_log_end(&mut self, end: u64) -> Result<()> {
        self.persist_hard(self.hard_term, self.hard_vote.as_deref(), Some(end))?;
        if end < self.last_index {
            self.truncate_from(end + 1);
        }
        self.log_end = Some(end);
        Ok(())
    }

    /// Replace `hard.meta`. The caller updates its fields only after this
    /// returns `Ok`.
    fn persist_hard(&self, term: u64, vote: Option<&str>, log_end: Option<u64>) -> Result<()> {
        let path = self.dir.join(HARD_FILE);
        let tmp = self.dir.join("hard.meta.tmp");
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&HARD_MAGIC.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&term.to_le_bytes());
        match log_end {
            Some(end) => {
                buf.push(1);
                buf.extend_from_slice(&end.to_le_bytes());
            }
            None => {
                buf.push(0);
                buf.extend_from_slice(&0u64.to_le_bytes());
            }
        }
        match vote {
            Some(vote) => {
                let bytes = vote.as_bytes();
                let len = u32::try_from(bytes.len()).context("raft vote longer than u32")?;
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            None => buf.extend_from_slice(&u32::MAX.to_le_bytes()),
        }
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .with_context(|| format!("writing {tmp:?}"))?;
            file.write_all(&buf)
                .with_context(|| format!("writing {tmp:?}"))?;
            file.sync_all()
                .with_context(|| format!("syncing {tmp:?}"))?;
        }
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {path:?}"))?;
        // The rename is the publication. Sync the directory so a crash cannot
        // lose the directory entry that names the new file.
        sync_dir(&self.dir)?;
        Ok(())
    }

    fn load_hard_state(dir: &Path) -> Result<(u64, Option<String>, Option<u64>)> {
        let path = dir.join(HARD_FILE);
        if !path.exists() {
            return Ok((0, None, None));
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {path:?}"))?;
        if bytes.len() < 4 + 4 + 8 + 1 + 8 + 4 {
            return Err(anyhow!("raft hard state {path:?} is truncated"));
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"));
        if magic != HARD_MAGIC {
            return Err(anyhow!("raft hard state {path:?} has a bad magic"));
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
        if version != 1 {
            return Err(anyhow!(
                "raft hard state {path:?} has version {version}, want 1"
            ));
        }
        let term = u64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
        let has_end = bytes[16];
        let end = u64::from_le_bytes(bytes[17..25].try_into().expect("8 bytes"));
        let log_end = match has_end {
            0 => None,
            1 => Some(end),
            other => return Err(anyhow!("raft hard state {path:?} has log-end flag {other}")),
        };
        let vote_len = u32::from_le_bytes(bytes[25..29].try_into().expect("4 bytes"));
        let vote = if vote_len == u32::MAX {
            None
        } else {
            let start = 29;
            let end_at = start + vote_len as usize;
            if bytes.len() < end_at {
                return Err(anyhow!("raft hard state {path:?} vote is truncated"));
            }
            let text = std::str::from_utf8(&bytes[start..end_at])
                .with_context(|| format!("raft vote in {path:?} is not utf-8"))?;
            Some(text.to_string())
        };
        Ok((term, vote, log_end))
    }

    fn load_snapshot(dir: &Path) -> Result<Option<Vec<u8>>> {
        let path = dir.join(SNAPSHOT_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {path:?}"))?;
        if bytes.len() < 4 + 4 + 8 + 4 {
            return Err(anyhow!("raft snapshot {path:?} is truncated"));
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"));
        if magic != SNAPSHOT_MAGIC {
            return Err(anyhow!("raft snapshot {path:?} has a bad magic"));
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
        if version != 1 {
            return Err(anyhow!(
                "raft snapshot {path:?} has version {version}, want 1"
            ));
        }
        let len = usize::try_from(u64::from_le_bytes(
            bytes[8..16].try_into().expect("8 bytes"),
        ))
        .context("raft snapshot length does not fit usize")?;
        let header = 16;
        let crc_at = header + len;
        if bytes.len() != crc_at + 4 {
            return Err(anyhow!("raft snapshot {path:?} is truncated"));
        }
        let expected = u32::from_le_bytes(bytes[crc_at..crc_at + 4].try_into().expect("4 bytes"));
        let mut h = Crc32::new();
        h.update(&bytes[..crc_at]);
        if h.finalize() != expected {
            return Err(anyhow!("raft snapshot {path:?} has a bad checksum"));
        }
        Ok(Some(bytes[header..crc_at].to_vec()))
    }

    /// Highest log index appended so far (0 if the log is empty).
    pub fn last_index(&self) -> u64 {
        self.last_index
    }

    /// Persisted commit index.  Survives restarts.
    pub fn commit_index(&self) -> u64 {
        self.commit_index
    }

    /// Number of entries currently in the seek table.
    pub fn len(&self) -> usize {
        self.seek_table.len()
    }

    /// `true` if no entries have been appended (ignoring truncated tails).
    pub fn is_empty(&self) -> bool {
        self.seek_table.is_empty()
    }

    /// Term of the entry at `index`, or `None` if missing.
    pub fn term_of(&self, index: u64) -> Option<u64> {
        self.seek_table.get(&index).map(|(t, _, _)| *t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_and_reread() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        for i in 1..=1000u64 {
            log.append(1, i, format!("entry-{}", i).as_bytes()).unwrap();
        }
        log.fsync().unwrap();
        drop(log);

        let mut log2 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log2.len(), 1000);
        assert_eq!(log2.last_index(), 1000);
        let (term, payload) = log2.read(500).unwrap().unwrap();
        assert_eq!(term, 1);
        assert_eq!(payload, b"entry-500");
    }

    #[test]
    fn commit_index_persists() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        log.append(1, 1, b"x").unwrap();
        log.append(1, 2, b"y").unwrap();
        log.fsync().unwrap();
        log.set_commit_index(2).unwrap();
        drop(log);

        let log2 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log2.commit_index(), 2);
    }

    #[test]
    fn truncate_drops_tail() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        for i in 1..=10 {
            log.append(1, i, format!("{i}").as_bytes()).unwrap();
        }
        log.fsync().unwrap();
        log.truncate_from(6);
        assert_eq!(log.last_index(), 5);
        assert!(log.read(7).unwrap().is_none());
        assert_eq!(log.read(3).unwrap().unwrap().1, b"3");
    }

    #[test]
    fn corrupt_tail_is_ignored_on_reopen() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        for i in 1..=5 {
            log.append(1, i, format!("{i}").as_bytes()).unwrap();
        }
        log.fsync().unwrap();
        drop(log);

        // Corrupt the last few bytes of the file.
        let log_path = dir.path().join(LOG_FILE);
        let mut bytes = std::fs::read(&log_path).unwrap();
        let last_byte = bytes.last_mut().unwrap();
        *last_byte ^= 0xff;
        std::fs::write(&log_path, &bytes).unwrap();

        // Reopen — the last entry should be dropped by the CRC check;
        // at minimum, we should have entries 1..=4 recoverable.
        let log2 = FileRaftLog::open(dir.path()).unwrap();
        assert!(log2.len() >= 4);
        assert!(log2.len() <= 5);
    }

    #[test]
    fn log_end_hides_a_truncated_tail_on_reopen() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        for i in 1..=5 {
            log.append(1, i, format!("{i}").as_bytes()).unwrap();
        }
        log.fsync().unwrap();
        log.truncate_from(4);
        log.set_log_end(3).unwrap();
        drop(log);

        let mut log2 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log2.last_index(), 3);
        assert!(log2.read(4).unwrap().is_none());
        assert!(log2.read(5).unwrap().is_none());
        assert_eq!(log2.read(3).unwrap().unwrap().1, b"3");

        log2.append(2, 4, b"new").unwrap();
        log2.fsync().unwrap();
        log2.set_log_end(4).unwrap();
        drop(log2);

        let mut log3 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log3.last_index(), 4);
        assert_eq!(log3.read(4).unwrap().unwrap().1, b"new");
        assert!(log3.read(5).unwrap().is_none());
    }

    #[test]
    fn hard_state_roundtrip_and_a_truncated_file_refuses_to_open() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        log.set_hard_state(4, Some("n2")).unwrap();
        log.set_log_end(0).unwrap();
        drop(log);

        let log2 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log2.hard_term(), 4);
        assert_eq!(log2.hard_vote(), Some("n2"));
        assert_eq!(log2.persisted_log_end(), Some(0));

        std::fs::write(dir.path().join(HARD_FILE), [1u8, 2, 3]).unwrap();
        match FileRaftLog::open(dir.path()) {
            Err(err) => assert!(
                err.to_string().contains("truncated"),
                "corrupt hard state must not look like no vote: {err}"
            ),
            Ok(_) => panic!("corrupt hard state opened"),
        }
    }

    #[test]
    fn snapshot_roundtrip_and_a_bad_checksum_refuses_to_open() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        assert!(log.snapshot_bytes().is_none());
        log.save_snapshot(b"commands").unwrap();
        assert_eq!(log.snapshot_bytes(), Some(b"commands".as_slice()));
        drop(log);

        let log2 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log2.snapshot_bytes(), Some(b"commands".as_slice()));

        let path = dir.path().join(SNAPSHOT_FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        match FileRaftLog::open(dir.path()) {
            Err(err) => assert!(
                err.to_string().contains("checksum"),
                "a torn snapshot must not open as an empty one: {err}"
            ),
            Ok(_) => panic!("corrupt snapshot opened"),
        }
    }

    #[test]
    fn rewrite_drops_the_prefix_and_keeps_the_suffix() {
        let dir = tempdir().unwrap();
        let mut log = FileRaftLog::open(dir.path()).unwrap();
        for i in 1..=5 {
            log.append(1, i, format!("{i}").as_bytes()).unwrap();
        }
        log.fsync().unwrap();
        log.save_snapshot(b"snap").unwrap();
        log.rewrite_without_prefix(3).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.read(1).unwrap().is_none());
        assert!(log.read(3).unwrap().is_none());
        assert_eq!(log.read(4).unwrap().unwrap().1, b"4");
        assert_eq!(log.read(5).unwrap().unwrap().1, b"5");
        drop(log);

        let mut log2 = FileRaftLog::open(dir.path()).unwrap();
        assert_eq!(log2.snapshot_bytes(), Some(b"snap".as_slice()));
        assert_eq!(log2.len(), 2);
        assert_eq!(log2.last_index(), 5);
        assert!(log2.read(2).unwrap().is_none());
        assert_eq!(log2.read(5).unwrap().unwrap().1, b"5");
    }
}
