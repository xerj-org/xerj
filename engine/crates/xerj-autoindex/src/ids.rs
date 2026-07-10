//! Idempotent identity derivation.
//!
//! `file_key` identifies a file by CONTENT (first 64KB hash) + size — stable
//! across mtime changes and re-walks, cheap for huge files.
//! `doc_id` is derived from (dataset slug, file_key, locator) so a re-run of
//! the same corpus emits byte-identical _ids and bulk `index` actions
//! overwrite instead of duplicating: kill -9 + re-run converges.

use std::io::Read;
use std::path::Path;
use xxhash_rust::xxh3::{xxh3_128, xxh3_64};

/// Content-based file identity: xxh3_64(first 64KB) ‖ file size.
pub fn file_key(path: &Path, size: u64) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 65536];
    let mut read = 0usize;
    while read < buf.len() {
        let n = f.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(format!("{:016x}-{:x}", xxh3_64(&buf[..read]), size))
}

/// Deterministic document _id.
pub fn doc_id(dataset_slug: &str, file_key: &str, locator: &str) -> String {
    let mut input = Vec::with_capacity(8 + dataset_slug.len() + file_key.len() + locator.len());
    input.extend_from_slice(b"ax1\x00");
    input.extend_from_slice(dataset_slug.as_bytes());
    input.push(0);
    input.extend_from_slice(file_key.as_bytes());
    input.push(0);
    input.extend_from_slice(locator.as_bytes());
    format!("{:032x}", xxh3_128(&input))
}

/// Deterministic state-dir key for (root, url, prefix).
pub fn state_key(root: &str, url: &str, prefix: &str) -> String {
    format!("{:016x}", xxh3_64(format!("{root}\x00{url}\x00{prefix}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_id_stable() {
        let a = doc_id("events", "abc-1", "b1024");
        let b = doc_id("events", "abc-1", "b1024");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert_ne!(a, doc_id("events", "abc-1", "b1025"));
        assert_ne!(a, doc_id("logs", "abc-1", "b1024"));
    }
}
