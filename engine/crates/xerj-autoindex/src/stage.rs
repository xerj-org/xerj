//! Per-file staging area for phase B (#2427).
//!
//! Phase B stages every record of a file locally, re-verifies the source, and
//! only then makes the file visible on the node (#1911). Finish the compressed
//! stream before deleting any prior records: buffered writes may fail at
//! finalization. Two properties motivated replacing the old staging file:
//!
//! * **Size.** A staged record is the bulk action line plus the document with
//!   its provenance fields (`ax_path`, `ax_paths`, `ax_file`, `ax_run`, ...).
//!   For row-shaped sources the provenance can exceed the source data, and
//!   concurrent workers each retain a stage while bulks are throttled.
//!   The staged stream repeats the same
//!   index name, path, key, run id and dataset on every record, so it is now
//!   zstd-compressed on the way to disk; see the test for the ratio on a
//!   trace-shaped file.
//! * **Lifetime.** The stage was a *named* temp file. `NamedTempFile::drop`
//!   removes it after the file is sent or abandoned, but a client stopped by a
//!   signal (`timeout`, Ctrl-C, OOM kill) never runs drop, so the files stayed
//!   in the state dir until the next run's journal open swept them. The stage
//!   is now an *unnamed* temp file (`O_TMPFILE` on Linux, unlinked at creation
//!   on other Unix systems): the kernel frees it when the descriptor closes, including on
//!   SIGKILL. Windows uses delete-on-close. The journal-open sweep of the
//!   reserved prefix stays for state dirs written by older binaries.
//!
//! A blocking global byte budget was considered and rejected: a worker parked
//! on it still counts as "could add bytes" for the shared bulk pipe's settle
//! barrier, so the oldest stage's settle could wait on it forever. Disk use
//! still depends on concurrent stages and their compressibility; this is not
//! a hard disk quota or a fix for the node's process memory limit.

use std::fs::File;
use std::io::{self, BufReader, Seek, Write};
use std::path::Path;

/// Use zstd's fast level; avoid the high-effort compression that previously
/// stalled background merges (#2441). Savings depend on the input.
const STAGE_ZSTD_LEVEL: i32 = 1;

/// Reader over a finished stage: the decompressed NDJSON, in write order.
pub(crate) type StageReader = BufReader<zstd::stream::read::Decoder<'static, BufReader<File>>>;

/// One file's staged NDJSON, compressed into an unnamed temp file.
pub(crate) struct Stage {
    encoder: zstd::stream::write::Encoder<'static, File>,
    raw_bytes: u64,
    #[cfg(test)]
    state_dir: std::path::PathBuf,
}

impl Stage {
    /// Create an unnamed stage in `dir` (the run's state dir, so its disk use
    /// lands on the same filesystem the operator budgeted for state).
    pub(crate) fn create_in(dir: &Path) -> io::Result<Self> {
        let file = tempfile::tempfile_in(dir)?;
        let encoder = zstd::stream::write::Encoder::new(file, STAGE_ZSTD_LEVEL)?;
        Ok(Self {
            encoder,
            raw_bytes: 0,
            #[cfg(test)]
            state_dir: dir.to_path_buf(),
        })
    }

    /// Uncompressed bytes written so far (what the send loop will read back).
    pub(crate) fn raw_bytes(&self) -> u64 {
        self.raw_bytes
    }

    /// Finish compression and return a reader positioned at the first staged
    /// line, plus the compressed size on disk.
    pub(crate) fn into_reader(self) -> io::Result<(StageReader, u64)> {
        // Model a buffered write failing at finish, scoped to one test run.
        #[cfg(test)]
        crate::replacement_failpoint(5, &self.state_dir).map_err(io::Error::other)?;
        let mut file = self.encoder.finish()?;
        let disk_bytes = file.stream_position()?;
        file.rewind()?;
        let decoder = zstd::stream::read::Decoder::new(file)?;
        Ok((BufReader::new(decoder), disk_bytes))
    }
}

impl Write for Stage {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.encoder.write(buf)?;
        self.raw_bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.encoder.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    /// Trace-shaped records: what the gate's ADS-B files stage as.
    fn trace_record(i: u64) -> String {
        format!(
            "{{\"index\":{{\"_index\":\"ax-adsb-traces\",\"_id\":\"0f3c9a1b2d4e5f60718293a4b5c6d7e8-{i}\"}}}}\n\
             {{\"t\":{},\"lat\":{:.5},\"lon\":{:.5},\"alt\":{},\"gs\":{:.1},\"track\":{},\
             \"ax_path\":\"adsb/traces/00/trace_full_000000.json\",\
             \"ax_paths\":[\"adsb/traces/00/trace_full_000000.json\"],\
             \"ax_file\":\"5b1f0c8e4a7d2e9f3c6b0a1d8e7f4c2b9a6d3e0f1c8b5a2d9e6f3c0b7a4d1e8f\",\
             \"ax_locator\":\"trace[{i}]\",\"ax_dataset\":\"adsb-traces\",\
             \"ax_run\":\"run-20261026T230000Z-7f3a\",\"ax_format\":\"json\"}}\n",
            1_729_000_000 + i,
            (i as f64 * 0.731).sin() * 89.0,
            (i as f64 * 0.377).cos() * 179.0,
            (i * 7919) % 45_000,
            (i % 600) as f64 + 0.5,
            (i * 31) % 360,
        )
    }

    #[test]
    fn stage_round_trips_and_stays_far_below_raw_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut stage = Stage::create_in(dir.path()).unwrap();
        let mut expected = Vec::new();
        for i in 0..50_000 {
            let rec = trace_record(i);
            stage.write_all(rec.as_bytes()).unwrap();
            expected.push(rec);
        }
        let raw = stage.raw_bytes();
        assert_eq!(raw, expected.iter().map(|r| r.len() as u64).sum::<u64>());
        let (mut reader, disk) = stage.into_reader().unwrap();
        eprintln!("trace stage: raw_bytes={raw} compressed_bytes={disk}");
        // Several times smaller on disk than the stream the node will receive.
        assert!(
            disk * 5 < raw,
            "stage compressed {raw} raw bytes only to {disk} on disk"
        );
        // Lines come back exactly, in order, two per record.
        let mut line = String::new();
        for rec in &expected {
            let mut got = String::new();
            for _ in 0..2 {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                got.push_str(&line);
            }
            assert_eq!(&got, rec);
        }
        line.clear();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
    }

    #[test]
    fn stages_leave_nothing_in_the_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let listing = || std::fs::read_dir(dir.path()).unwrap().count();
        let mut stages: Vec<Stage> = (0..4)
            .map(|_| Stage::create_in(dir.path()).unwrap())
            .collect();
        for (n, stage) in stages.iter_mut().enumerate() {
            for i in 0..1_000 {
                stage
                    .write_all(trace_record(i + n as u64).as_bytes())
                    .unwrap();
            }
        }
        // On Unix live stages have no directory entry, so neither an abandoned retry
        // nor a client killed mid-file (no drop runs) can leave one behind.
        // Windows delete-on-close files may remain visible until closed.
        #[cfg(unix)]
        assert_eq!(listing(), 0);
        let (reader, _) = stages.pop().unwrap().into_reader().unwrap();
        #[cfg(unix)]
        assert_eq!(listing(), 0);
        drop(reader);
        drop(stages);
        assert_eq!(listing(), 0);
    }

    #[test]
    fn empty_stage_reads_back_empty() {
        let dir = tempfile::tempdir().unwrap();
        let stage = Stage::create_in(dir.path()).unwrap();
        assert_eq!(stage.raw_bytes(), 0);
        let (mut reader, _) = stage.into_reader().unwrap();
        let mut line = String::new();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn buffered_stage_write_can_fail_only_when_finishing() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut stage = Stage {
            encoder: zstd::stream::write::Encoder::new(file, STAGE_ZSTD_LEVEL).unwrap(),
            raw_bytes: 0,
            state_dir: dir.path().to_owned(),
        };
        let bytes = b"{\"index\":{}}\n{\"value\":\"buffered until finish\"}\n";
        stage
            .write_all(bytes)
            .expect("small writes are buffered by zstd");
        assert_eq!(stage.raw_bytes(), bytes.len() as u64);
        let error = stage
            .into_reader()
            .err()
            .expect("finishing must report ENOSPC");
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
    }
}
