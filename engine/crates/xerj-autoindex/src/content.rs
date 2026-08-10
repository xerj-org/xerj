//! Full-content identity, duplicate grouping, and mutation detection.

use crate::state::DuplicateFile;
use crate::walk::FileEntry;
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;

pub struct Inventory {
    pub files: Vec<FileEntry>,
    /// Durable document identity. New plans use the full-content key.
    pub keys: Vec<String>,
    /// Full digest used to detect mutations even when a legacy key is retained.
    pub digests: Vec<String>,
    pub duplicates: Vec<DuplicateFile>,
}

/// Hash each file once, group equal digests, then byte-verify only digest peers.
///
/// This is linear in corpus bytes for adversarial common-prefix inputs rather
/// than pairwise O(n² × file_size).
pub fn resolve(files: Vec<FileEntry>) -> Result<Inventory> {
    let digests: Vec<Result<String>> = files
        .par_iter()
        .map(|file| full_digest(&file.path, file.size))
        .collect();
    let digests: Vec<String> = digests.into_iter().collect::<Result<_>>()?;
    resolve_with_digests(files, digests)
}

fn resolve_with_digests(files: Vec<FileEntry>, digests: Vec<String>) -> Result<Inventory> {
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, digest) in digests.iter().enumerate() {
        groups.entry(digest).or_default().push(index);
    }

    let mut keep = vec![true; files.len()];
    let mut resolved_keys = digests.clone();
    let mut duplicates = Vec::new();
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        let mut indices = indices.clone();
        // Prefer a real in-tree path over a followed symlink as canonical.
        indices.sort_by_key(|&index| (files[index].is_symlink, files[index].rel.clone()));
        // A digest match is still only a candidate: verify exact bytes.
        let mut representatives: Vec<usize> = Vec::new();
        for index in indices {
            let canonical = representatives.iter().copied().find(|&candidate| {
                same_file_identity(&files[candidate].path, &files[index].path)
                    || files_equal(&files[candidate].path, &files[index].path).unwrap_or(false)
            });
            if let Some(canonical) = canonical {
                keep[index] = false;
                duplicates.push(DuplicateFile {
                    file_key: resolved_keys[canonical].clone(),
                    rel: files[index].rel.clone(),
                    path_id: files[index].rel_id.clone(),
                    duplicate_of: files[canonical].rel.clone(),
                    bytes: files[index].size,
                });
            } else {
                if !representatives.is_empty() {
                    // A full-digest collision was byte-proven, not assumed.
                    // Give the unequal representative a deterministic,
                    // path-specific discriminator so documents cannot collide.
                    resolved_keys[index] = format!(
                        "{}-collision-{:032x}",
                        digests[index],
                        xxhash_rust::xxh3::xxh3_128(files[index].rel_id.as_bytes())
                    );
                }
                representatives.push(index);
            }
        }
    }

    let mut unique_files = Vec::new();
    let mut unique_keys = Vec::new();
    let mut unique_digests = Vec::new();
    for (index, file) in files.into_iter().enumerate() {
        if keep[index] {
            unique_files.push(file);
            unique_keys.push(resolved_keys[index].clone());
            unique_digests.push(digests[index].clone());
        }
    }
    duplicates.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(Inventory {
        files: unique_files,
        keys: unique_keys,
        digests: unique_digests,
        duplicates,
    })
}

pub fn verify(path: &Path, expected_size: u64, expected_digest: &str) -> Result<()> {
    let actual_size = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    anyhow::ensure!(
        actual_size == expected_size,
        "{} changed size during autoindex (expected {}, now {}); retry the run",
        path.display(),
        expected_size,
        actual_size
    );
    let actual = full_digest(path, actual_size)?;
    anyhow::ensure!(
        actual == expected_digest,
        "{} changed content during autoindex; retry the run",
        path.display()
    );
    Ok(())
}

fn full_digest(path: &Path, size: u64) -> Result<String> {
    let mut file =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut hash = Xxh3::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("axf2-{:032x}-{size:x}", hash.digest128()))
}

fn files_equal(a: &Path, b: &Path) -> Result<bool> {
    let mut a = BufReader::new(File::open(a).with_context(|| format!("open {}", a.display()))?);
    let mut b = BufReader::new(File::open(b).with_context(|| format!("open {}", b.display()))?);
    let mut ab = [0u8; 64 * 1024];
    let mut bb = [0u8; 64 * 1024];
    loop {
        let an = read_chunk(&mut a, &mut ab)?;
        let bn = read_chunk(&mut b, &mut bb)?;
        if an != bn || ab[..an] != bb[..bn] {
            return Ok(false);
        }
        if an == 0 {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
fn same_file_identity(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file_identity(_a: &Path, _b: &Path) -> bool {
    false
}

fn read_chunk(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while read < buf.len() {
        let n = reader.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn identical_paths_become_one_content_file_and_an_alias() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"same records\n").unwrap();
        fs::write(dir.path().join("b.txt"), b"same records\n").unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false, true, true).unwrap().0).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.files[0].rel, "a.txt");
        assert_eq!(inv.duplicates.len(), 1);
        assert_eq!(inv.duplicates[0].duplicate_of, "a.txt");
        assert_eq!(inv.keys[0], inv.duplicates[0].file_key);
    }

    #[test]
    fn same_prefix_and_size_but_different_tail_get_distinct_durable_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = vec![b'x'; 65_537];
        let mut second = first.clone();
        first[65_536] = b'a';
        second[65_536] = b'b';
        fs::write(dir.path().join("a.txt"), first).unwrap();
        fs::write(dir.path().join("b.txt"), second).unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false, true, true).unwrap().0).unwrap();
        assert_eq!(inv.files.len(), 2);
        assert!(inv.duplicates.is_empty());
        assert_ne!(inv.keys[0], inv.keys[1]);
    }

    #[test]
    fn verification_detects_same_size_tail_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"content-a").unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false, true, true).unwrap().0).unwrap();
        fs::write(&path, b"content-b").unwrap();
        assert!(verify(&path, 9, &inv.digests[0]).is_err());
    }

    #[test]
    fn byte_proven_full_digest_collision_gets_distinct_document_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"unequal-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"unequal-b").unwrap();
        let files = crate::walk::walk(dir.path(), false, true, true).unwrap().0;
        let forced = vec!["axf2-forced-9".to_string(); 2];
        let inv = resolve_with_digests(files, forced).unwrap();
        assert_eq!(inv.files.len(), 2);
        assert!(inv.duplicates.is_empty());
        assert_ne!(inv.keys[0], inv.keys[1]);
        assert!(inv.keys.iter().any(|key| key.contains("-collision-")));
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_are_aliases_without_pairwise_reading() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"hardlinked").unwrap();
        fs::hard_link(&a, &b).unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false, true, true).unwrap().0).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.duplicates.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn lossy_display_collisions_keep_distinct_alias_identities() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("canonical"), b"same").unwrap();
        fs::write(
            dir.path().join(OsString::from_vec(vec![b'a', 0x80])),
            b"same",
        )
        .unwrap();
        fs::write(
            dir.path().join(OsString::from_vec(vec![b'a', 0x81])),
            b"same",
        )
        .unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), false, true, true).unwrap().0).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.duplicates.len(), 2);
        assert_ne!(inv.duplicates[0].path_id, inv.duplicates[1].path_id);
    }

    #[cfg(unix)]
    #[test]
    fn followed_symlink_does_not_become_canonical_over_real_path() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("z-real");
        fs::write(&real, b"same").unwrap();
        symlink(&real, dir.path().join("a-link")).unwrap();
        let inv = resolve(crate::walk::walk(dir.path(), true, true, true).unwrap().0).unwrap();
        assert_eq!(inv.files[0].rel, "z-real");
        assert_eq!(inv.duplicates[0].rel, "a-link");
    }
}
