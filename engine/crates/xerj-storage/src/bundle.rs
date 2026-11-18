//! One immutable object per segment — the ZBM1 bundle format (issue #965).
//!
//! A flushed segment is not one file: it is a FAMILY of files (`.seg`,
//! `.sidx`, `.ids`, `.dv`, the per-field FTS side-cars, ...). A hand count of
//! one real 25-field segment came to 104 files. Object stores bill per
//! request, so uploading a family one file at a time would be ~104
//! `PutObject`s per flush — 899% of Cloudflare R2's free Class-A tier at a
//! 30-second flush interval, against ~18% for one object per segment plus
//! the catalog PUT (docs/OBJECT_STORAGE.md). This module packs the whole
//! family into exactly one object.
//!
//! ## Layout
//!
//! ```text
//! [body: family file bytes, name-sorted, incl. a synthesized .complete]
//! [footer "ZBM1": id_len u16 | id | doc_count u64 | min_seq_no u64 |
//!  max_seq_no u64 | file_count u32 | per file: name_len u16 | name |
//!  offset u64 | len u64 | crc32 u32 | envelope_crc32 over preceding bytes]
//! [footer_len u32]
//! [trailer, fixed 16 bytes: footer_start u64 | format_version u32 | b"XBB1"]
//! ```
//!
//! All integers little-endian, matching the ZCM1 manifest and the `.seg`
//! container. The trailer shape and size mirror quickwit's split-footer
//! trailer (`footer_start u64` + version `u32` + 4-byte magic,
//! `quickwit-storage/src/bundle_storage.rs:156-195`, Apache-2.0 — approach
//! adapted, no code taken): a reader locates the footer from the object's
//! last 16 bytes, so a future two-range-GET cold open (trailer, then footer)
//! needs no format change. v1 always fetches the whole object.
//!
//! The footer is the existing ZCM1 flush-completion manifest
//! (`IndexStore::write_flush_completion_manifest`) EXTENDED with absolute
//! offsets: same field order, same per-file CRC-32 scheme, same envelope
//! CRC — so family naming, roles and checksum validation stay on ONE code
//! path, reused rather than reinvented. The bundle always carries a
//! `.complete` entry: it is synthesized deterministically from the family
//! (the ZCM1 bytes are a pure function of the file set), which is why a
//! merged family — which gets no ZCM1 manifest on local disk today — packs
//! identically to a post-flush one, and why a hydrated directory is
//! byte-identical to a post-flush directory (every existing recovery path
//! works on it with zero special cases).

use std::path::Path;

use crate::segment::{SegmentId, SegmentMeta};
use crate::{Result, StorageError};

/// Bundle trailer magic.
pub const BUNDLE_MAGIC: &[u8; 4] = b"XBB1";

/// Bundle format version written in the trailer. Unknown versions are a
/// typed error, never a best-effort parse (the `IncompatibleDataDir`
/// discipline: refuse, keep the data, let an operator investigate).
pub const BUNDLE_FORMAT_VERSION: u32 = 1;

/// Fixed size of the trailer: `footer_start u64` + `format_version u32` +
/// magic.
pub const BUNDLE_TRAILER_LEN: usize = 16;

/// Object key for a segment's bundle, under the backend's prefix.
pub fn bundle_key(segment_id: &str) -> String {
    format!("segments/{segment_id}.bundle")
}

/// Object key for the bucket catalog (the serialized [`IndexSnapshot`]).
pub const CATALOG_KEY: &str = "snapshot.json";

/// Object key for an auxiliary index meta file (settings, schema).
pub fn aux_key(name: &str) -> String {
    format!("meta/{name}")
}

/// Upper bound on files in one bundle — same order as ZCM1's artifact cap.
const MAX_BUNDLE_FILES: u32 = 4096;

/// One file's entry in the bundle footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFileEntry {
    pub name: String,
    /// Absolute byte offset of the file's bytes within the bundle.
    pub offset: u64,
    pub len: u64,
    /// IEEE CRC-32 of the file's bytes (same scheme as ZCM1).
    pub crc32: u32,
}

/// Parsed bundle footer.
#[derive(Debug, Clone)]
pub struct BundleFooter {
    pub segment_id: SegmentId,
    pub doc_count: u64,
    pub min_seq_no: u64,
    pub max_seq_no: u64,
    pub files: Vec<BundleFileEntry>,
}

impl BundleFooter {
    /// The footer's file table as ZCM1 `(name, size, crc)` tuples, excluding
    /// the `.complete` entry (ZCM1 never lists itself).
    pub fn artifact_set(&self) -> Vec<(String, u64, u32)> {
        self.files
            .iter()
            .filter(|f| f.name != format!("{}.complete", self.segment_id))
            .map(|f| (f.name.clone(), f.len, f.crc32))
            .collect()
    }
}

/// ZCM1 flush-completion manifest bytes for `meta` over `artifacts`.
///
/// Shared by the on-disk writer (`write_flush_completion_manifest`) and the
/// bundle packer so the two can never diverge. `artifacts` is the family
/// EXCLUDING the `.complete` file itself, in any order.
pub fn complete_manifest_bytes(meta: &SegmentMeta, artifacts: &[(String, u64, u32)]) -> Vec<u8> {
    let mut sorted: Vec<&(String, u64, u32)> = artifacts.iter().collect();
    sorted.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut body = Vec::new();
    body.extend_from_slice(b"ZCM1");
    body.extend_from_slice(&(meta.id.len() as u16).to_le_bytes());
    body.extend_from_slice(meta.id.as_bytes());
    body.extend_from_slice(&meta.doc_count.to_le_bytes());
    body.extend_from_slice(&meta.min_seq_no.to_le_bytes());
    body.extend_from_slice(&meta.max_seq_no.to_le_bytes());
    body.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    for (name, size, crc) in sorted {
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&crc.to_le_bytes());
    }
    let envelope_crc = crc32fast::hash(&body);
    body.extend_from_slice(&envelope_crc.to_le_bytes());
    body
}

/// The exact set of file roles a complete segment family must cover.
///
/// Moved verbatim from `IndexStore` (issue #965: reused, not reinvented) so
/// the flush path, the manifest validator and the bundle packer/unpacker all
/// answer the same question. `artifacts` excludes any `.complete` manifest.
pub fn valid_flush_artifact_set(segment_id: &str, artifacts: &[(String, u64, u32)]) -> bool {
    let prefix = format!("{segment_id}.");
    let mut roles = std::collections::HashSet::new();
    let mut fts: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        std::collections::HashMap::new();
    for (name, _, _) in artifacts {
        if !name.starts_with(&prefix) || name.contains('/') || name.contains('\\') {
            return false;
        }
        let rest = &name[prefix.len()..];
        if matches!(
            rest,
            "seg" | "sidx" | "ids" | "dv" | "fts-layout-v2" | "fts-layout-v3" | "ftsan"
        ) {
            if !roles.insert(rest) {
                return false;
            }
            continue;
        }
        let Some((field, extension)) = rest.rsplit_once('.') else {
            return false;
        };
        if field.is_empty()
            || field.contains("..")
            || !matches!(extension, "fst" | "post" | "meta" | "norms")
            || !fts.entry(field).or_default().insert(extension)
        {
            return false;
        }
    }
    roles.contains("seg")
        && roles.contains("sidx")
        && roles.contains("ids")
        && fts.values().all(|extensions| {
            ["fst", "post", "meta", "norms"]
                .iter()
                .all(|extension| extensions.contains(extension))
        })
}

/// Streaming IEEE CRC-32 (ZCM1's checksum scheme) over a reader.
pub(crate) fn stream_crc32_from_reader(mut reader: impl std::io::Read) -> std::io::Result<u32> {
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

fn file_crc32(path: &Path) -> std::io::Result<u32> {
    stream_crc32_from_reader(std::fs::File::open(path)?)
}

/// Pack the complete segment family living in `segments_dir` into one bundle.
///
/// The `.complete` manifest is synthesized (see the module docs) — a family
/// on disk with or without one packs to the same bytes. Known limit: the
/// bundle is built in memory (peak RSS ~ family size); a streaming build is
/// the recorded follow-up.
pub fn pack(segments_dir: &Path, meta: &SegmentMeta) -> Result<Vec<u8>> {
    let prefix = format!("{}.", meta.id);
    let mut artifacts: Vec<(String, u64, u32)> = Vec::new();
    for entry in std::fs::read_dir(segments_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || name.ends_with(".complete") {
            continue;
        }
        let metadata = entry.metadata()?;
        let crc = file_crc32(&entry.path())?;
        artifacts.push((name, metadata.len(), crc));
    }
    artifacts.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if !valid_flush_artifact_set(&meta.id, &artifacts) {
        return Err(StorageError::Backend(format!(
            "cannot pack segment {}: artifact set is incomplete or contains an unknown role",
            meta.id
        )));
    }
    // Synthesize the ZCM1 manifest and add it to the file table; a hydrated
    // directory then carries the same recovery evidence a post-flush one does.
    let complete_name = format!("{}.complete", meta.id);
    let complete_bytes = complete_manifest_bytes(meta, &artifacts);
    let complete_crc = crc32fast::hash(&complete_bytes);

    let total_body: u64 =
        artifacts.iter().map(|(_, len, _)| *len).sum::<u64>() + complete_bytes.len() as u64;
    let mut out = Vec::with_capacity(total_body as usize + 512);
    let mut entries: Vec<BundleFileEntry> = Vec::with_capacity(artifacts.len() + 1);
    // Emit files in name-sorted order — the exact sort ZCM1 uses — with the
    // synthesized `.complete` spliced in at its sorted position.
    let mut merged = artifacts
        .iter()
        .map(|(name, len, crc)| (name.clone(), *len, *crc))
        .collect::<Vec<_>>();
    merged.push((
        complete_name.clone(),
        complete_bytes.len() as u64,
        complete_crc,
    ));
    merged.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut offset: u64 = 0;
    for (name, len, crc) in &merged {
        let is_complete = *name == complete_name;
        let bytes: Vec<u8> = if is_complete {
            complete_bytes.clone()
        } else {
            let path = segments_dir.join(name);
            std::fs::read(&path)?
        };
        debug_assert_eq!(bytes.len() as u64, *len, "file changed under the packer");
        out.extend_from_slice(&bytes);
        entries.push(BundleFileEntry {
            name: name.clone(),
            offset,
            len: *len,
            crc32: *crc,
        });
        offset += *len;
    }

    let footer_start = out.len() as u64;
    let mut footer = Vec::with_capacity(64 + entries.len() * 32);
    footer.extend_from_slice(b"ZBM1");
    footer.extend_from_slice(&(meta.id.len() as u16).to_le_bytes());
    footer.extend_from_slice(meta.id.as_bytes());
    footer.extend_from_slice(&meta.doc_count.to_le_bytes());
    footer.extend_from_slice(&meta.min_seq_no.to_le_bytes());
    footer.extend_from_slice(&meta.max_seq_no.to_le_bytes());
    footer.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in &entries {
        footer.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        footer.extend_from_slice(entry.name.as_bytes());
        footer.extend_from_slice(&entry.offset.to_le_bytes());
        footer.extend_from_slice(&entry.len.to_le_bytes());
        footer.extend_from_slice(&entry.crc32.to_le_bytes());
    }
    let envelope_crc = crc32fast::hash(&footer);
    footer.extend_from_slice(&envelope_crc.to_le_bytes());
    let footer_len = footer.len() as u32;
    out.extend_from_slice(&footer);
    out.extend_from_slice(&footer_len.to_le_bytes());
    out.extend_from_slice(&footer_start.to_le_bytes());
    out.extend_from_slice(&BUNDLE_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(BUNDLE_MAGIC);
    Ok(out)
}

/// Parse and structurally validate a bundle's footer + trailer.
///
/// Returns the footer and the byte offset the footer starts at. Does NOT
/// recompute per-file CRCs — [`unpack`] does, on the write path, where a
/// mismatch must abort before any file lands.
pub fn parse_footer(bytes: &[u8]) -> Result<(BundleFooter, usize)> {
    let total = bytes.len();
    if total < BUNDLE_TRAILER_LEN + 4 {
        return Err(StorageError::Backend(format!(
            "bundle too short ({total} bytes) to carry a footer"
        )));
    }
    let trailer = &bytes[total - BUNDLE_TRAILER_LEN..];
    if &trailer[12..] != BUNDLE_MAGIC {
        return Err(StorageError::InvalidMagic {
            expected: BUNDLE_MAGIC,
            actual: trailer[12..].to_vec(),
        });
    }
    let version = u32::from_le_bytes(trailer[8..12].try_into().unwrap());
    if version != BUNDLE_FORMAT_VERSION {
        return Err(StorageError::UnsupportedVersion(version as u16));
    }
    let footer_start = u64::from_le_bytes(trailer[..8].try_into().unwrap()) as usize;
    let footer_len = u32::from_le_bytes(
        bytes[total - BUNDLE_TRAILER_LEN - 4..total - BUNDLE_TRAILER_LEN]
            .try_into()
            .unwrap(),
    ) as usize;
    let footer_end = total - BUNDLE_TRAILER_LEN - 4;
    if footer_start >= footer_end
        || footer_len != footer_end - footer_start
        || footer_end.checked_sub(footer_start).is_none()
    {
        return Err(StorageError::Backend(format!(
            "bundle trailer is inconsistent: footer_start={footer_start}, \
             footer_len={footer_len}, object_len={total}"
        )));
    }
    let footer = &mut &bytes[footer_start..footer_end];
    if footer.len() < 4 + 2 + 8 * 3 + 4 + 4 || footer[..4] != *b"ZBM1" {
        return Err(StorageError::InvalidMagic {
            expected: b"ZBM1",
            actual: footer[..4.min(footer.len())].to_vec(),
        });
    }
    // Envelope CRC covers the whole footer INCLUDING the magic — the same
    // discipline as ZCM1's validator (magic-inclusive payload).
    let envelope_crc = u32::from_le_bytes(footer[footer.len() - 4..].try_into().unwrap());
    let payload_len = footer.len() - 4;
    if crc32fast::hash(&footer[..payload_len]) != envelope_crc {
        return Err(StorageError::ChecksumMismatch {
            expected: envelope_crc,
            actual: crc32fast::hash(&footer[..payload_len]),
        });
    }
    *footer = &footer[4..payload_len];

    let take_u16 = |buf: &mut &[u8]| -> Option<u16> {
        let value = u16::from_le_bytes(buf.get(..2)?.try_into().ok()?);
        *buf = &buf[2..];
        Some(value)
    };
    let take_u32 = |buf: &mut &[u8]| -> Option<u32> {
        let value = u32::from_le_bytes(buf.get(..4)?.try_into().ok()?);
        *buf = &buf[4..];
        Some(value)
    };
    let take_u64 = |buf: &mut &[u8]| -> Option<u64> {
        let value = u64::from_le_bytes(buf.get(..8)?.try_into().ok()?);
        *buf = &buf[8..];
        Some(value)
    };

    let id_len = take_u16(footer).map(usize::from).ok_or_else(|| {
        StorageError::Backend("bundle footer truncated at segment id length".into())
    })?;
    if id_len == 0 || id_len > 128 {
        return Err(StorageError::Backend(format!(
            "bundle footer segment id length out of range: {id_len}"
        )));
    }
    let id_bytes = footer
        .get(..id_len)
        .ok_or_else(|| StorageError::Backend("bundle footer truncated at segment id".into()))?;
    *footer = &footer[id_len..];
    let segment_id = std::str::from_utf8(id_bytes)
        .map_err(|_| StorageError::Backend("bundle segment id is not UTF-8".into()))?
        .to_owned();
    let doc_count = take_u64(footer).ok_or_else(missing("doc_count"))?;
    let min_seq_no = take_u64(footer).ok_or_else(missing("min_seq_no"))?;
    let max_seq_no = take_u64(footer).ok_or_else(missing("max_seq_no"))?;
    let file_count = take_u32(footer).ok_or_else(missing("file_count"))?;
    if file_count == 0 || file_count > MAX_BUNDLE_FILES {
        return Err(StorageError::Backend(format!(
            "bundle file count out of range: {file_count}"
        )));
    }
    let mut files = Vec::with_capacity(file_count as usize);
    for _ in 0..file_count {
        let name_len = take_u16(footer)
            .map(usize::from)
            .ok_or_else(missing("name_len"))?;
        if name_len == 0 || name_len > 512 {
            return Err(StorageError::Backend(format!(
                "bundle file name length out of range: {name_len}"
            )));
        }
        let name = footer.get(..name_len).ok_or_else(missing("name"))?;
        *footer = &footer[name_len..];
        let name = std::str::from_utf8(name)
            .map_err(|_| StorageError::Backend("bundle file name is not UTF-8".into()))?
            .to_owned();
        let offset = take_u64(footer).ok_or_else(missing("offset"))?;
        let len = take_u64(footer).ok_or_else(missing("len"))?;
        let crc32 = take_u32(footer).ok_or_else(missing("crc32"))?;
        // Every file's bytes must live inside the body region, before the
        // footer — a table claiming to reach into the footer or past the
        // object end is corruption, not data.
        let end = offset.checked_add(len).ok_or_else(|| {
            StorageError::Backend(format!("bundle file {name} overflows the object size"))
        })?;
        if end > footer_start as u64 {
            return Err(StorageError::Backend(format!(
                "bundle file {name} claims bytes [{offset}, {end}) past the body region \
                 ({footer_start})"
            )));
        }
        files.push(BundleFileEntry {
            name,
            offset,
            len,
            crc32,
        });
    }
    if !footer.is_empty() {
        return Err(StorageError::Backend(format!(
            "bundle footer has {} trailing bytes after the file table",
            footer.len()
        )));
    }
    let complete_name = format!("{segment_id}.complete");
    if !files.iter().any(|f| f.name == complete_name) {
        return Err(StorageError::Backend(format!(
            "bundle for {segment_id} carries no {complete_name} manifest"
        )));
    }
    let artifact_set: Vec<(String, u64, u32)> = files
        .iter()
        .filter(|f| f.name != complete_name)
        .map(|f| (f.name.clone(), f.len, f.crc32))
        .collect();
    if !valid_flush_artifact_set(&segment_id, &artifact_set) {
        return Err(StorageError::Backend(format!(
            "bundle for {segment_id} has an incomplete artifact set or an unknown role"
        )));
    }
    Ok((
        BundleFooter {
            segment_id,
            doc_count,
            min_seq_no,
            max_seq_no,
            files,
        },
        footer_start,
    ))
}

fn missing(field: &'static str) -> impl Fn() -> StorageError {
    move || StorageError::Backend(format!("bundle footer truncated at {field}"))
}

/// Validate every per-file CRC and materialize the family into `into_dir`.
///
/// A flipped body byte surfaces as [`StorageError::ChecksumMismatch`] BEFORE
/// any file is written — never silently wrong data, never a partial family
/// visible to a later reader. Files land via `fsio::write_file_durable`
/// (unique tmp name + fsync + rename), the same discipline as every other
/// segment-side write.
pub fn unpack(bytes: &[u8], into_dir: &Path) -> Result<BundleFooter> {
    let (footer, _footer_start) = parse_footer(bytes)?;
    std::fs::create_dir_all(into_dir)?;
    // Verify EVERY file's checksum first; only then write.
    for entry in &footer.files {
        let start = entry.offset as usize;
        let end = start + entry.len as usize;
        let actual = crc32fast::hash(&bytes[start..end]);
        if actual != entry.crc32 {
            return Err(StorageError::ChecksumMismatch {
                expected: entry.crc32,
                actual,
            });
        }
    }
    for entry in &footer.files {
        let start = entry.offset as usize;
        let end = start + entry.len as usize;
        // Path safety: valid_flush_artifact_set already rejected '/', '\'
        // and anything outside the `{id}.` prefix in parse_footer.
        let path = into_dir.join(&entry.name);
        xerj_common::fsio::write_file_durable(&path, &bytes[start..end])?;
    }
    Ok(footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but role-complete family: seg + sidx + ids (+ synthesized
    /// .complete). Field side-cars would follow the same path.
    fn synthetic_family(dir: &Path, id: &str, doc_count: u64) -> SegmentMeta {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{id}.seg")), b"seg-bytes").unwrap();
        std::fs::write(dir.join(format!("{id}.sidx")), b"sidx-bytes").unwrap();
        std::fs::write(dir.join(format!("{id}.ids")), b"ids-bytes").unwrap();
        SegmentMeta {
            id: id.to_string(),
            doc_count,
            size_bytes: 24,
            min_seq_no: 1,
            max_seq_no: doc_count,
            created_at_ms: 0,
            has_tombstones: false,
            seg_path: format!("{id}.seg"),
            sidx_path: format!("{id}.sidx"),
        }
    }

    #[test]
    fn pack_unpack_round_trip_is_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let meta = synthetic_family(dir.path(), "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0001", 7);
        let bundle = pack(dir.path(), &meta).unwrap();
        let out = tempfile::tempdir().unwrap();
        let footer = unpack(&bundle, out.path()).unwrap();
        assert_eq!(footer.segment_id, meta.id);
        assert_eq!(footer.doc_count, 7);
        for name in ["seg", "sidx", "ids"] {
            let file = format!("{}.{}", meta.id, name);
            let orig = std::fs::read(dir.path().join(&file)).unwrap();
            let hydrated = std::fs::read(out.path().join(&file)).unwrap();
            assert_eq!(orig, hydrated, "{file} must round-trip byte-identical");
        }
        // The synthesized `.complete` lands too, and is a valid ZCM1
        // manifest: magic + envelope CRC over the magic-inclusive payload.
        let complete = std::fs::read(out.path().join(format!("{}.complete", meta.id))).unwrap();
        assert_eq!(&complete[..4], b"ZCM1");
        let payload_len = complete.len() - 4;
        let expected = u32::from_le_bytes(complete[payload_len..].try_into().unwrap());
        assert_eq!(crc32fast::hash(&complete[..payload_len]), expected);
        // Offsets are self-consistent: concatenating the ranges rebuilds the body.
        let mut rebuilt = Vec::new();
        let mut entries = footer.files.clone();
        entries.sort_by_key(|e| e.offset);
        for e in &entries {
            rebuilt.extend_from_slice(&bundle[e.offset as usize..(e.offset + e.len) as usize]);
        }
        let footer_start = bundle.len()
            - BUNDLE_TRAILER_LEN
            - 4
            - u32::from_le_bytes(
                bundle[bundle.len() - BUNDLE_TRAILER_LEN - 4..bundle.len() - BUNDLE_TRAILER_LEN]
                    .try_into()
                    .unwrap(),
            ) as usize;
        assert_eq!(rebuilt.len(), footer_start);
    }

    #[test]
    fn wrong_magic_is_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let meta = synthetic_family(dir.path(), "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0002", 1);
        let mut bundle = pack(dir.path(), &meta).unwrap();
        let last = bundle.len() - 1;
        bundle[last] ^= 0xFF;
        assert!(matches!(
            parse_footer(&bundle),
            Err(StorageError::InvalidMagic { .. })
        ));
    }

    #[test]
    fn unknown_version_is_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let meta = synthetic_family(dir.path(), "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0003", 1);
        let mut bundle = pack(dir.path(), &meta).unwrap();
        let at = bundle.len() - BUNDLE_TRAILER_LEN + 8;
        bundle[at..at + 4].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(
            parse_footer(&bundle),
            Err(StorageError::UnsupportedVersion(v)) if v == 99
        ));
    }

    #[test]
    fn flipped_body_byte_is_a_checksum_error_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let meta = synthetic_family(dir.path(), "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0004", 3);
        let mut bundle = pack(dir.path(), &meta).unwrap();
        // Flip one byte inside the first file's body region.
        bundle[3] ^= 0xFF;
        let out = tempfile::tempdir().unwrap();
        let err = unpack(&bundle, out.path()).unwrap_err();
        assert!(
            matches!(err, StorageError::ChecksumMismatch { .. }),
            "expected ChecksumMismatch, got {err:?}"
        );
        // No partial family may be visible.
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn incomplete_family_is_refused_at_pack() {
        let dir = tempfile::tempdir().unwrap();
        let meta = synthetic_family(dir.path(), "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0005", 1);
        std::fs::remove_file(dir.path().join(format!("{}.ids", meta.id))).unwrap();
        let err = pack(dir.path(), &meta).unwrap_err();
        assert!(err.to_string().contains("artifact set"), "{err}");
    }
}
