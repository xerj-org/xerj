//! What the watcher remembers between polls.
//!
//! One JSON file per watched location: `key -> (etag, size, last_modified,
//! digest)`. The next poll compares a fresh listing against it and that
//! comparison IS the change set. Three properties matter more than the format:
//!
//! * **Bounded-window durability.** The journal is saved every 256 accepted
//!   objects or every 2 seconds, whichever comes first, and at the end of every
//!   cycle — so a watcher killed mid-cycle re-emits at most that window, never
//!   the whole cycle, and never loses an update (an entry is recorded only
//!   after the sink accepted its event). It used to save after EVERY object,
//!   which rewrote and fsynced a growing file once per object: a quadratic
//!   first scan. Re-measured for the #968 remediation on one host against one
//!   MinIO, 10,000 objects, `--no-fetch`: 17.95 s -> 0.19 s with the journal on
//!   ext4, 6.08 s -> 0.19 s with it on tmpfs, and 10,000 saves -> 40. (The
//!   310.91 s the review first reported did not reproduce, and its "after" was
//!   taken on tmpfs while its "before" was not.)
//!   The feed is at-least-once either way; the ids downstream are idempotent.
//! * **Identity.** It records the endpoint, bucket and prefix it was built
//!   from and refuses to be reused for a different one. Silently adopting
//!   another bucket's state would report that bucket's keys as deleted.
//! * **An honest size.** One entry costs roughly 150-250 bytes of JSON and a
//!   similar amount of live memory, so a 1,000,000-object bucket means a
//!   ~200 MB journal loaded in full. That is a real limit of this design, not
//!   a rounding error; the docs state it and `--prefix`-scoped watches are the
//!   answer.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Journal format version. A file written by a newer XERJ is refused rather
/// than half-read.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchedObject {
    /// ETag exactly as the object store reported it for the bytes we processed,
    /// quotes stripped. For a multipart object this is `<hex>-<parts>`, which is
    /// not an MD5 of the content — the watcher never treats it as one.
    pub etag: String,
    pub size: u64,
    /// `LastModified` as the store formatted it, compared as an opaque string.
    pub last_modified: String,
    /// xxh3 of the bytes actually fetched. `None` in `--no-fetch` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// True when the GET response carried no ETag, so the recorded one came
    /// from the listing and could describe bytes we did not read. Forces a
    /// re-check on the next poll instead of trusting it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub etag_unverified: bool,
    /// When this entry was last accepted by the sink (RFC 3339, UTC).
    pub seen_at: String,
    /// True when the fetch was cut off at `--max-object-mb`: the digest covers
    /// a prefix of the object, so the entry is a partial record, and the docs
    /// say so rather than the index quietly holding half a file.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchJournal {
    pub version: u32,
    /// Endpoint host+scheme, never credentials.
    pub endpoint: String,
    pub bucket: String,
    pub prefix: String,
    /// True when this journal was built by an append-only (`start-after`) watch,
    /// which never sees the whole key space. Mixing the two modes on one
    /// journal is refused: a full scan would treat keys the append-only mode
    /// never recorded as new, and an append-only scan cannot detect deletes.
    #[serde(default)]
    pub append_only: bool,
    /// Highest key seen, used as `start-after` in append-only mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_key_seen: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub cycles: u64,
    pub objects: BTreeMap<String, WatchedObject>,
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    dirty: bool,
    /// Saves performed by this process. Observability for the write-amplification
    /// regression test; not persisted.
    #[serde(skip)]
    saves: u64,
}

impl WatchJournal {
    /// `<state_dir>/objwatch.json`.
    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join("objwatch.json")
    }

    pub fn new(
        path: PathBuf,
        endpoint: &str,
        bucket: &str,
        prefix: &str,
        append_only: bool,
    ) -> WatchJournal {
        let now = now_rfc3339();
        WatchJournal {
            version: FORMAT_VERSION,
            endpoint: endpoint.to_string(),
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            append_only,
            max_key_seen: None,
            created_at: now.clone(),
            updated_at: now,
            cycles: 0,
            objects: BTreeMap::new(),
            path,
            dirty: false,
            saves: 0,
        }
    }

    /// Open an existing journal, or start a fresh one. An existing journal for a
    /// DIFFERENT location is an error, never a silent adoption.
    pub fn open(
        state_dir: &Path,
        endpoint: &str,
        bucket: &str,
        prefix: &str,
        append_only: bool,
        fresh: bool,
    ) -> Result<WatchJournal> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create watch state dir {}", state_dir.display()))?;
        let path = Self::path_in(state_dir);
        if fresh || !path.exists() {
            return Ok(WatchJournal::new(
                path,
                endpoint,
                bucket,
                prefix,
                append_only,
            ));
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read watch journal {}", path.display()))?;
        let mut j: WatchJournal = serde_json::from_str(&raw)
            .with_context(|| format!("parse watch journal {}", path.display()))?;
        if j.version > FORMAT_VERSION {
            bail!(
                "watch journal {} was written by a newer XERJ (format {} > {}). Use that version, \
                 or start a new --state-dir",
                path.display(),
                j.version,
                FORMAT_VERSION
            );
        }
        if j.bucket != bucket || j.prefix != prefix || j.endpoint != endpoint {
            bail!(
                "watch journal {} belongs to s3://{}/{} @ {} — refusing to reuse it for \
                 s3://{}/{} @ {}. Reusing it would report the other location's keys as deleted. \
                 Use a different --state-dir, or --fresh to start over here",
                path.display(),
                j.bucket,
                j.prefix,
                j.endpoint,
                bucket,
                prefix,
                endpoint
            );
        }
        if j.append_only != append_only {
            bail!(
                "watch journal {} was built with --append-only={}, this run asked for {}. \
                 An append-only journal has never seen the whole key space, so a full scan would \
                 report unrecorded keys as new and an append-only scan cannot detect deletes. \
                 Use a different --state-dir, or --fresh",
                path.display(),
                j.append_only,
                append_only
            );
        }
        j.path = path;
        j.dirty = false;
        Ok(j)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory the journal lives in, which is where the rest of the
    /// watch's state (the spend ledger, the lock, the status file) lives too.
    pub fn state_dir(&self) -> &Path {
        match self.path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        }
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&WatchedObject> {
        self.objects.get(key)
    }

    pub fn record(&mut self, key: &str, obj: WatchedObject) {
        if self.append_only {
            match &self.max_key_seen {
                Some(max) if max.as_str() >= key => {}
                _ => self.max_key_seen = Some(key.to_string()),
            }
        }
        self.objects.insert(key.to_string(), obj);
        self.dirty = true;
    }

    pub fn forget(&mut self, key: &str) {
        if self.objects.remove(key).is_some() {
            self.dirty = true;
        }
    }

    pub fn note_cycle(&mut self) {
        self.cycles += 1;
        self.dirty = true;
    }

    /// Atomic save: write a sibling temp file, fsync it, rename over the
    /// journal. A crash therefore leaves either the previous journal or the new
    /// one, never a half-written file that would be read as "the bucket is
    /// empty" — which would re-index everything.
    pub fn save(&mut self) -> Result<()> {
        self.updated_at = now_rfc3339();
        // Compact, not pretty: the journal is rewritten on every save and a
        // pretty-printed one is ~40% larger for nobody's benefit (`jq .` reads it).
        let body = serde_json::to_vec(self).context("serialise watch journal")?;
        let dir = self
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let tmp = dir.join(format!("objwatch.json.tmp.{}", std::process::id()));
        {
            let mut f =
                std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            std::io::Write::write_all(&mut f, &body)?;
            f.sync_all().context("fsync watch journal")?;
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), self.path.display()))?;
        if let Ok(d) = std::fs::File::open(&dir) {
            let _ = d.sync_all();
        }
        self.dirty = false;
        self.saves += 1;
        Ok(())
    }

    /// How many times this process has written the journal.
    pub fn saves(&self) -> u64 {
        self.saves
    }

    pub fn save_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            self.save()?;
        }
        Ok(())
    }
}

/// An exclusive lock on one watch state directory.
///
/// Two watchers sharing a journal would each see the other's writes as bucket
/// changes and re-index them in a loop, so the second one is refused rather
/// than allowed to interleave.
#[derive(Debug)]
pub struct WatchLock {
    _file: std::fs::File,
}

impl WatchLock {
    pub fn acquire(state_dir: &Path) -> Result<WatchLock> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create watch state dir {}", state_dir.display()))?;
        let path = state_dir.join("objwatch.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open watch lock {}", path.display()))?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(WatchLock { _file: file }),
            Err(_) => bail!(
                "another xerj watch already holds {} — two watchers on one journal would \
                 re-index each other's writes. Stop it, or use a different --state-dir",
                path.display()
            ),
        }
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(etag: &str) -> WatchedObject {
        WatchedObject {
            etag: etag.into(),
            size: 3,
            last_modified: "2026-09-19T10:00:00.000Z".into(),
            digest: Some("d".into()),
            etag_unverified: false,
            seen_at: now_rfc3339(),
            truncated: false,
        }
    }

    #[test]
    fn a_saved_journal_reopens_with_the_same_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = WatchJournal::open(dir.path(), "http://h:1", "b", "p/", false, false).unwrap();
        j.record("p/a", obj("e1"));
        j.save().unwrap();
        let re = WatchJournal::open(dir.path(), "http://h:1", "b", "p/", false, false).unwrap();
        assert_eq!(re.len(), 1);
        assert_eq!(re.get("p/a").unwrap().etag, "e1");
    }

    #[test]
    fn a_journal_from_another_location_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = WatchJournal::open(dir.path(), "http://h:1", "b", "p/", false, false).unwrap();
        j.record("p/a", obj("e1"));
        j.save().unwrap();
        for (ep, bucket, prefix) in [
            ("http://other:1", "b", "p/"),
            ("http://h:1", "other", "p/"),
            ("http://h:1", "b", "other/"),
        ] {
            let err = WatchJournal::open(dir.path(), ep, bucket, prefix, false, false)
                .unwrap_err()
                .to_string();
            assert!(err.contains("refusing to reuse"), "{err}");
        }
        // --fresh is the documented way through.
        assert!(WatchJournal::open(dir.path(), "http://other:1", "b", "p/", false, true).is_ok());
    }

    #[test]
    fn mixing_append_only_with_a_full_scan_journal_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = WatchJournal::open(dir.path(), "http://h:1", "b", "", false, false).unwrap();
        j.record("a", obj("e1"));
        j.save().unwrap();
        let err = WatchJournal::open(dir.path(), "http://h:1", "b", "", true, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("append-only"), "{err}");
    }

    #[test]
    fn append_only_tracks_the_highest_key_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = WatchJournal::open(dir.path(), "http://h:1", "b", "", true, false).unwrap();
        j.record("2026/01", obj("e1"));
        j.record("2026/03", obj("e2"));
        j.record("2026/02", obj("e3"));
        assert_eq!(j.max_key_seen.as_deref(), Some("2026/03"));
    }

    #[test]
    fn a_newer_format_version_is_refused_not_half_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = WatchJournal::path_in(dir.path());
        std::fs::write(
            &path,
            serde_json::json!({
                "version": FORMAT_VERSION + 1,
                "endpoint": "http://h:1", "bucket": "b", "prefix": "",
                "created_at": "x", "updated_at": "x", "objects": {}
            })
            .to_string(),
        )
        .unwrap();
        let err = WatchJournal::open(dir.path(), "http://h:1", "b", "", false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("newer XERJ"), "{err}");
    }

    #[test]
    fn a_second_watcher_on_the_same_state_dir_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = WatchLock::acquire(dir.path()).unwrap();
        let err = WatchLock::acquire(dir.path()).unwrap_err().to_string();
        assert!(err.contains("already holds"), "{err}");
        drop(first);
        assert!(WatchLock::acquire(dir.path()).is_ok());
    }
}
