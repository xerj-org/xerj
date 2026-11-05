//! `xerj autoindex <archive>`: index a `.zip`, `.tar`, `.tar.gz`/`.tgz` or
//! `.tar.zst` file — a Google Takeout download is the case this exists for.
//!
//! The run never reads the archive in place. [`prepare`] extracts it once into
//! a mirror directory under the run's state dir (`archive-cache/<slug>/`) and
//! rewrites `cfg.root` to that mirror, exactly the shape `objsource::prepare`
//! uses for an `s3://` source: every later phase (walk, sniff, extract, plan,
//! journal, reconcile) is the code that has always run over a folder, and the
//! run's identity is the archive path, not the mirror. A re-run over the same
//! archive (same size and mtime) reuses the mirror instead of extracting again,
//! so incremental re-indexing keeps working.
//!
//! Untrusted input, so extraction is bounded and refuses what it cannot do
//! safely, counting every refusal in the [`ExtractReport`]:
//! - a member path must be relative and made only of normal components: an
//!   absolute path, a `..` component or a drive prefix is skipped (zip-slip);
//! - symlinks, hard links and device/FIFO entries are skipped, never created;
//! - every member's bytes are counted as they are written, so a lying header
//!   cannot exceed the caps: [`Limits::max_total_bytes`] for the whole
//!   archive, [`Limits::max_entry_bytes`] per member, [`Limits::max_entries`]
//!   members, and an expansion ratio cap (bytes out / archive bytes on disk)
//!   once more than [`Limits::ratio_floor_bytes`] have been written — the
//!   zip-bomb guard. Hitting a cap stops the extraction with an error that
//!   names the cap and the environment variable that raises it;
//! - nested archives inside the archive are not opened (one level); they are
//!   reported by the normal pipeline with the usual advice.

use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::cli::IndexCfg;

/// Archive formats `prepare` opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    TarZst,
}

impl ArchiveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "zip",
            ArchiveKind::Tar => "tar",
            ArchiveKind::TarGz => "tar.gz",
            ArchiveKind::TarZst => "tar.zst",
        }
    }
}

/// Extraction caps. Defaults are sized for a large Google Takeout part
/// (Google splits exports into 2/10/50 GB parts) on a laptop disk.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_total_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_entries: u64,
    pub max_ratio: u64,
    pub ratio_floor_bytes: u64,
}

const GIB: u64 = 1 << 30;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_total_bytes: 64 * GIB,
            max_entry_bytes: 16 * GIB,
            max_entries: 2_000_000,
            max_ratio: 200,
            ratio_floor_bytes: GIB,
        }
    }
}

impl Limits {
    /// Defaults, overridden by `XERJ_ARCHIVE_MAX_BYTES`, `XERJ_ARCHIVE_MAX_ENTRY_BYTES`,
    /// `XERJ_ARCHIVE_MAX_ENTRIES` and `XERJ_ARCHIVE_MAX_RATIO` when set to an integer.
    pub fn from_env() -> Self {
        let mut l = Limits::default();
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        if let Some(v) = get("XERJ_ARCHIVE_MAX_BYTES") {
            l.max_total_bytes = v;
        }
        if let Some(v) = get("XERJ_ARCHIVE_MAX_ENTRY_BYTES") {
            l.max_entry_bytes = v;
        }
        if let Some(v) = get("XERJ_ARCHIVE_MAX_ENTRIES") {
            l.max_entries = v;
        }
        if let Some(v) = get("XERJ_ARCHIVE_MAX_RATIO") {
            l.max_ratio = v.max(1);
        }
        l
    }
}

/// What one extraction did and refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub skipped_unsafe_path: u64,
    pub skipped_link: u64,
    pub skipped_special: u64,
}

/// The run-level handle: where the archive is and where it was mirrored.
#[derive(Clone, Debug)]
pub struct ArchiveRun {
    pub archive: PathBuf,
    pub kind: ArchiveKind,
    pub identity: String,
    pub mirror: PathBuf,
    pub reused: bool,
    pub report: ExtractReport,
}

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const STAMP_FILE: &str = ".xerj-archive-stamp";

/// The archive kind of `path`, or `None` when it is not a regular file this
/// module opens. A DOCX/XLSX/PPTX/EPUB is a zip container but a document, and
/// `sniff` names it as such, so it is not an archive here.
pub fn detect(path: &Path) -> Option<ArchiveKind> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut head = [0u8; 4];
    if let Ok(mut f) = File::open(path) {
        if f.read_exact(&mut head).is_ok() && head == ZSTD_MAGIC {
            return zstd_is_tar(path).then_some(ArchiveKind::TarZst);
        }
    }
    let sn = crate::sniff::sniff(path).ok()?;
    if sn.family != crate::sniff::Family::Binary {
        return None;
    }
    match (sn.binary_kind.as_deref(), sn.gzip) {
        (Some("zip"), false) => Some(ArchiveKind::Zip),
        (Some("tar"), false) => Some(ArchiveKind::Tar),
        (Some("tar"), true) => Some(ArchiveKind::TarGz),
        _ => None,
    }
}

fn zstd_is_tar(path: &Path) -> bool {
    let Ok(f) = File::open(path) else {
        return false;
    };
    let Ok(mut dec) = zstd::stream::read::Decoder::new(f) else {
        return false;
    };
    let mut block = [0u8; 512];
    if dec.read_exact(&mut block).is_err() {
        return false;
    }
    &block[257..262] == b"ustar"
}

/// Turn an archive positional argument into a local mirror folder. `None`
/// (and `cfg` untouched) when `cfg.root` is not an archive file.
pub fn prepare(cfg: &mut IndexCfg) -> Result<Option<ArchiveRun>> {
    let Some(kind) = detect(&cfg.root) else {
        return Ok(None);
    };
    let archive = cfg.root.canonicalize().unwrap_or_else(|_| cfg.root.clone());
    let identity = format!("archive:{}", archive.to_string_lossy());
    let state_dir = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| crate::state::default_state_dir(&identity, &cfg.url, &cfg.prefix));
    let mirror = state_dir.join("archive-cache").join(slug(&archive));
    let stamp = stamp_of(&archive)?;
    let stamp_path = mirror.join(STAMP_FILE);
    let reused = fs::read_to_string(&stamp_path)
        .map(|s| s == stamp)
        .unwrap_or(false);
    let report = if reused {
        ExtractReport::default()
    } else {
        if mirror.exists() {
            fs::remove_dir_all(&mirror)
                .with_context(|| format!("clear stale archive mirror {}", mirror.display()))?;
        }
        fs::create_dir_all(&mirror)
            .with_context(|| format!("create archive mirror {}", mirror.display()))?;
        // A refused or failed extraction must not leave a half-written tree
        // behind: it would be indexed by nothing and fill the disk for nothing.
        let report = match extract(&archive, kind, &mirror, Limits::from_env()) {
            Ok(r) => r,
            Err(e) => {
                let _ = fs::remove_dir_all(&mirror);
                return Err(e);
            }
        };
        fs::write(&stamp_path, &stamp)
            .with_context(|| format!("write {}", stamp_path.display()))?;
        report
    };
    cfg.root = mirror.clone();
    Ok(Some(ArchiveRun {
        archive,
        kind,
        identity,
        mirror,
        reused,
        report,
    }))
}

fn slug(path: &Path) -> String {
    let name: String = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.to_string_lossy().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{name}-{h:016x}")
}

fn stamp_of(path: &Path) -> Result<String> {
    let m = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("v1 {} {}", m.len(), mtime))
}

/// A member path made safe to join under the mirror, or `None` when it is
/// absolute, has a `..`/root/drive component, or is empty.
pub fn safe_relative(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

struct Budget {
    limits: Limits,
    archive_bytes: u64,
    total: u64,
    entries: u64,
}

impl Budget {
    fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            bail!(
                "archive has more than {} members; refusing to extract further \
                 (raise XERJ_ARCHIVE_MAX_ENTRIES to allow more)",
                self.limits.max_entries
            );
        }
        Ok(())
    }

    fn check(&self, entry_bytes: u64, name: &Path) -> Result<()> {
        if entry_bytes > self.limits.max_entry_bytes {
            bail!(
                "archive member {} expands past {} bytes; refusing to extract it \
                 (raise XERJ_ARCHIVE_MAX_ENTRY_BYTES to allow larger members)",
                name.display(),
                self.limits.max_entry_bytes
            );
        }
        if self.total > self.limits.max_total_bytes {
            bail!(
                "archive expands past {} bytes in total; refusing to extract further \
                 (raise XERJ_ARCHIVE_MAX_BYTES to allow more)",
                self.limits.max_total_bytes
            );
        }
        if self.total > self.limits.ratio_floor_bytes
            && self.total / self.archive_bytes.max(1) > self.limits.max_ratio
        {
            bail!(
                "archive expands more than {}x its size on disk ({} bytes out of {}); \
                 this looks like a decompression bomb, so extraction stopped \
                 (raise XERJ_ARCHIVE_MAX_RATIO if the archive is trusted)",
                self.limits.max_ratio,
                self.total,
                self.archive_bytes
            );
        }
        Ok(())
    }
}

fn copy_bounded<R: Read>(mut r: R, dest: &Path, name: &Path, budget: &mut Budget) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .with_context(|| format!("create {}", dest.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut n_entry: u64 = 0;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("read archive member {}", name.display()))
            }
        };
        n_entry += n as u64;
        budget.total += n as u64;
        budget.check(n_entry, name)?;
        out.write_all(&buf[..n])
            .with_context(|| format!("write {}", dest.display()))?;
    }
    Ok(n_entry)
}

/// Extract `archive` into the empty directory `dest` under `limits`.
pub fn extract(
    archive: &Path,
    kind: ArchiveKind,
    dest: &Path,
    limits: Limits,
) -> Result<ExtractReport> {
    let archive_bytes = fs::metadata(archive)
        .with_context(|| format!("stat {}", archive.display()))?
        .len();
    let mut budget = Budget {
        limits,
        archive_bytes,
        total: 0,
        entries: 0,
    };
    let mut report = ExtractReport::default();
    let f = File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    match kind {
        ArchiveKind::Zip => extract_zip(f, dest, &mut budget, &mut report)?,
        ArchiveKind::Tar => extract_tar(f, dest, &mut budget, &mut report)?,
        ArchiveKind::TarGz => extract_tar(
            flate2::read::MultiGzDecoder::new(io::BufReader::new(f)),
            dest,
            &mut budget,
            &mut report,
        )?,
        ArchiveKind::TarZst => {
            let dec = zstd::stream::read::Decoder::new(f).context("open zstd stream")?;
            extract_tar(dec, dest, &mut budget, &mut report)?
        }
    }
    report.bytes = budget.total;
    Ok(report)
}

fn extract_zip(
    f: File,
    dest: &Path,
    budget: &mut Budget,
    report: &mut ExtractReport,
) -> Result<()> {
    let mut z = zip::ZipArchive::new(io::BufReader::new(f)).context("read zip directory")?;
    for i in 0..z.len() {
        budget.entry()?;
        let mut entry = z.by_index(i).with_context(|| format!("zip member #{i}"))?;
        let raw = PathBuf::from(entry.name().map(|n| n.into_owned()).unwrap_or_default());
        // Judge the raw name first: `enclosed_name` silently strips a leading
        // `/` and would write `/abs.txt` as `abs.txt`; an absolute member is a
        // hostile archive, so it is refused, not re-rooted.
        let raw_ok =
            safe_relative(&PathBuf::from(raw.to_string_lossy().replace('\\', "/"))).is_some();
        let Some(rel) = raw_ok
            .then(|| entry.enclosed_name())
            .flatten()
            .and_then(|p| safe_relative(&p))
        else {
            report.skipped_unsafe_path += 1;
            continue;
        };
        if entry.is_symlink() {
            report.skipped_link += 1;
            continue;
        }
        let target = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
            report.dirs += 1;
            continue;
        }
        if !entry.is_file() {
            report.skipped_special += 1;
            continue;
        }
        copy_bounded(&mut entry, &target, &raw, budget)?;
        report.files += 1;
    }
    Ok(())
}

fn extract_tar<R: Read>(
    r: R,
    dest: &Path,
    budget: &mut Budget,
    report: &mut ExtractReport,
) -> Result<()> {
    let mut ar = tar::Archive::new(r);
    for entry in ar.entries().context("read tar stream")? {
        budget.entry()?;
        let mut entry = entry.context("read tar member")?;
        let raw = entry.path().map(|p| p.into_owned()).unwrap_or_default();
        let Some(rel) = safe_relative(&raw) else {
            report.skipped_unsafe_path += 1;
            continue;
        };
        let et = entry.header().entry_type();
        if et.is_symlink() || et.is_hard_link() {
            report.skipped_link += 1;
            continue;
        }
        let target = dest.join(&rel);
        if et.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
            report.dirs += 1;
            continue;
        }
        if !(et.is_file() || et == tar::EntryType::Continuous || et == tar::EntryType::GNUSparse) {
            report.skipped_special += 1;
            continue;
        }
        copy_bounded(&mut entry, &target, &raw, budget)?;
        report.files += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in members {
            w.start_file(*name, opt).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn tar_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in members {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            // set_path refuses `..`; write the raw name like a hostile archive would
            let bytes = name.as_bytes();
            h.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_takeout_style_zip_extracts_its_tree() {
        let t = tempfile::tempdir().unwrap();
        let z = write(
            t.path(),
            "takeout.zip",
            &zip_bytes(&[
                (
                    "Takeout/Mail/All mail.mbox",
                    b"From a@b Mon Jan 1 00:00:00 2024\nSubject: hi\n\nbody\n",
                ),
                ("Takeout/Drive/people.csv", b"id,name\n1,Ann\n"),
            ]),
        );
        assert_eq!(detect(&z), Some(ArchiveKind::Zip));
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = extract(&z, ArchiveKind::Zip, &out, Limits::default()).unwrap();
        assert_eq!(r.files, 2);
        assert!(out.join("Takeout/Mail/All mail.mbox").is_file());
        assert_eq!(
            fs::read_to_string(out.join("Takeout/Drive/people.csv")).unwrap(),
            "id,name\n1,Ann\n"
        );
    }

    #[test]
    fn zip_slip_and_absolute_members_are_skipped_not_written() {
        let t = tempfile::tempdir().unwrap();
        let z = write(
            t.path(),
            "evil.zip",
            &zip_bytes(&[
                ("../escape.txt", b"x"),
                ("/abs.txt", b"y"),
                ("ok/fine.txt", b"z"),
            ]),
        );
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = extract(&z, ArchiveKind::Zip, &out, Limits::default()).unwrap();
        assert_eq!(r.files, 1);
        assert_eq!(r.skipped_unsafe_path, 2);
        assert!(!t.path().join("escape.txt").exists());
        assert!(out.join("ok/fine.txt").is_file());
    }

    #[test]
    fn tar_parent_paths_are_skipped() {
        let t = tempfile::tempdir().unwrap();
        let a = write(
            t.path(),
            "evil.tar",
            &tar_bytes(&[("../up.txt", b"x"), ("in/a.txt", b"a")]),
        );
        assert_eq!(detect(&a), Some(ArchiveKind::Tar));
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = extract(&a, ArchiveKind::Tar, &out, Limits::default()).unwrap();
        assert_eq!((r.files, r.skipped_unsafe_path), (1, 1));
        assert!(!t.path().join("up.txt").exists());
    }

    #[test]
    fn tar_symlinks_are_never_created() {
        let t = tempfile::tempdir().unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "link", "/etc/passwd").unwrap();
        let a = write(t.path(), "l.tar", &b.into_inner().unwrap());
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = extract(&a, ArchiveKind::Tar, &out, Limits::default()).unwrap();
        assert_eq!(r.skipped_link, 1);
        assert!(fs::symlink_metadata(out.join("link")).is_err());
    }

    #[test]
    fn gzip_and_zstd_tars_are_detected_and_extracted() {
        let t = tempfile::tempdir().unwrap();
        let raw = tar_bytes(&[("d/x.txt", b"hello")]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        let tgz = write(t.path(), "a.tgz", &gz.finish().unwrap());
        let tzst = write(
            t.path(),
            "a.tar.zst",
            &zstd::encode_all(&raw[..], 3).unwrap(),
        );
        for (p, k) in [(&tgz, ArchiveKind::TarGz), (&tzst, ArchiveKind::TarZst)] {
            assert_eq!(detect(p), Some(k), "{}", p.display());
            let out = t.path().join(format!("out-{}", k.as_str()));
            fs::create_dir(&out).unwrap();
            let r = extract(p, k, &out, Limits::default()).unwrap();
            assert_eq!(r.files, 1);
            assert_eq!(fs::read_to_string(out.join("d/x.txt")).unwrap(), "hello");
        }
    }

    #[test]
    fn a_decompression_bomb_stops_at_the_ratio_cap() {
        let t = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 8 << 20];
        let z = write(t.path(), "bomb.zip", &zip_bytes(&[("zeros.bin", &zeros)]));
        let out = t.path().join("out");
        fs::create_dir(&out).unwrap();
        let limits = Limits {
            ratio_floor_bytes: 1 << 20,
            max_ratio: 50,
            ..Limits::default()
        };
        let err = extract(&z, ArchiveKind::Zip, &out, limits)
            .unwrap_err()
            .to_string();
        assert!(err.contains("decompression bomb"), "{err}");
    }

    #[test]
    fn total_and_member_caps_are_enforced_on_bytes_actually_written() {
        let t = tempfile::tempdir().unwrap();
        let z = write(
            t.path(),
            "big.zip",
            &zip_bytes(&[("a", &[1u8; 4096]), ("b", &[2u8; 4096])]),
        );
        let out = t.path().join("o1");
        fs::create_dir(&out).unwrap();
        let err = extract(
            &z,
            ArchiveKind::Zip,
            &out,
            Limits {
                max_total_bytes: 6000,
                ..Limits::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("XERJ_ARCHIVE_MAX_BYTES"), "{err}");
        let out2 = t.path().join("o2");
        fs::create_dir(&out2).unwrap();
        let err = extract(
            &z,
            ArchiveKind::Zip,
            &out2,
            Limits {
                max_entry_bytes: 1000,
                ..Limits::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("XERJ_ARCHIVE_MAX_ENTRY_BYTES"), "{err}");
    }

    #[test]
    fn office_documents_are_not_archives() {
        let t = tempfile::tempdir().unwrap();
        let docx = write(
            t.path(),
            "d.docx",
            &zip_bytes(&[
                ("[Content_Types].xml", b"<Types/>"),
                ("word/document.xml", b"<w:document/>"),
            ]),
        );
        assert_eq!(detect(&docx), None);
        assert_eq!(detect(t.path()), None);
    }

    #[test]
    fn safe_relative_rejects_escapes() {
        assert!(safe_relative(Path::new("../a")).is_none());
        assert!(safe_relative(Path::new("/a")).is_none());
        assert!(safe_relative(Path::new("a/../../b")).is_none());
        assert_eq!(
            safe_relative(Path::new("./a/b")),
            Some(PathBuf::from("a/b"))
        );
    }
}
