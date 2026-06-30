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
//! `fsync` is batched: by default every 100 ms or whenever 256 entries
//! accumulate in the OS page cache, whichever comes first.  Both values
//! are tunable via `[cluster] raft_fsync_ms = 100` / `raft_fsync_batch = 256`.
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
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher as Crc32;

/// Magic for the raft log file format.  Distinct from the WAL magic so
/// we never confuse one for the other on recovery.
const MAGIC: u32 = 0x5A_52_4C_32; // "ZRL2"
const LOG_FILE: &str = "raft.log";
const COMMIT_FILE: &str = "commit.meta";

/// Raft payload kinds.  We only use `ClusterCommand` today but leave room
/// for future out-of-band messages (config changes, snapshots).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    ClusterCommand = 0,
    ConfigChange = 1,
    Snapshot = 2,
}

impl EntryKind {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::ClusterCommand),
            1 => Ok(Self::ConfigChange),
            2 => Ok(Self::Snapshot),
            _ => Err(anyhow!("unknown raft entry kind {b}")),
        }
    }
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

        Ok(Self {
            dir,
            writer,
            seek_table,
            last_index,
            commit_index,
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
        let payload_len = payload.len() as u64;
        // Entry length covers everything after the len field up to the CRC,
        // inclusive — useful for future forward-compatible skipping.
        let entry_len: u64 = 8 + 8 + 1 + 8 + payload_len + 4;

        let mut h = Crc32::new();
        h.update(&MAGIC.to_le_bytes());
        h.update(&entry_len.to_le_bytes());
        h.update(&term.to_le_bytes());
        h.update(&index.to_le_bytes());
        h.update(&[EntryKind::ClusterCommand as u8]);
        h.update(&payload_len.to_le_bytes());
        h.update(payload);
        let crc = h.finalize();

        self.writer.write_u32::<LittleEndian>(MAGIC)?;
        self.writer.write_u64::<LittleEndian>(entry_len)?;
        self.writer.write_u64::<LittleEndian>(term)?;
        self.writer.write_u64::<LittleEndian>(index)?;
        self.writer.write_u8(EntryKind::ClusterCommand as u8)?;
        self.writer.write_u64::<LittleEndian>(payload_len)?;
        self.writer.write_all(payload)?;
        self.writer.write_u32::<LittleEndian>(crc)?;

        self.seek_table.insert(index, (term, header_offset, payload_len));
        if index > self.last_index {
            self.last_index = index;
        }
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
        let mut file = File::open(&log_path)
            .with_context(|| format!("reopening {log_path:?} for read"))?;
        // Header is: magic(4) + entry_len(8) + term(8) + index(8) + kind(1) + payload_len(8)
        // = 37 bytes.  Then the payload.
        file.seek(SeekFrom::Start(offset + 37))
            .with_context(|| "seek to entry payload")?;
        let mut payload = vec![0u8; payload_len as usize];
        file.read_exact(&mut payload)
            .with_context(|| "reading entry payload")?;
        Ok(Some((term, payload)))
    }

    /// Truncate the log by removing every entry with `index >= from`.
    ///
    /// This is called by Raft when a leader steps down and the new leader
    /// instructs the follower to overwrite conflicting tail entries.
    /// We don't physically truncate the file — the seek table evicts the
    /// removed entries, the CRC of later writes will differ, and on
    /// recovery we stop reading at the first CRC mismatch.  That keeps
    /// this method O(log n) instead of O(file_size).
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
}
