//! Corpus Hub v2 — the pack archive FORMAT and its verification.
//!
//! A pack is a single `tar` + `zstd` file (`<slug>-<pin8>.hubpack.tar.zst`) with a
//! detached ed25519 signature in a sibling `<archive>.sig`. This module writes that
//! archive and reads it back under verification; it is NODE-FREE. The snapshot the
//! engine produced (`create_snapshot`'s output) is treated as OPAQUE files packed
//! verbatim under `snapshot/` and restored verbatim — interpreting them is a later
//! step's job (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md` §2, the node half).
//!
//! Archive layout (every member but `SHA256SUMS` is listed inside it):
//!
//! ```text
//! pack.json          PackManifest, JSON
//! SHA256SUMS         "<sha256>  <path>" per other member, sorted by path
//! defs.jsonl         one DefRecord per line
//! files.jsonl        one PackFileEntry per line
//! snapshot/...       opaque snapshot files, packed verbatim
//! ```
//!
//! Why the signature is detached: a signature cannot sit inside the file it signs,
//! and `harvest/sign.rs` already settled the key format (ed25519, raw key as hex).
//! The signature is over the FINAL archive bytes — sign the envelope, so one check
//! covers every member at once. `read_and_verify_pack` checks, in order, the whole
//! archive's sha256 against the catalogue's expected value, then the signature, then
//! the manifest's `format_version`, and extracts nothing until all three pass.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use xerj_autoindex::extract::code::{definitions, is_code_ext};
use xerj_autoindex::harvest::sign::{sign_bytes, verify_bytes};
use xerj_common::hub::{DefRecord, PackManifest, PACK_FORMAT_VERSION};

/// Archive member names (fixed by the format).
const PACK_JSON: &str = "pack.json";
const SHA256SUMS: &str = "SHA256SUMS";
const DEFS_JSONL: &str = "defs.jsonl";
const FILES_JSONL: &str = "files.jsonl";
/// Directory prefix for the opaque snapshot files inside the archive.
const SNAPSHOT_DIR: &str = "snapshot";
/// zstd level for the archive — 9 matches the builder recipe (§2 step 5).
const ZSTD_LEVEL: i32 = 9;

/// One line of `files.jsonl`: an indexed source file with its per-path licence.
/// `read_and_verify_pack` does not parse this (it is extracted verbatim); it is
/// defined here because it is written by [`write_pack`] and read by the later
/// routing step. `deny_unknown_fields` so a future field is refused, not half-read.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackFileEntry {
    pub path: String,
    pub lang: String,
    pub bytes: u64,
    pub licence: String,
}

/// Write the archive and its detached `.sig` (next to `out_archive`, named
/// `<out_archive>.sig`). Returns `(archive_sha256_hex, archive_bytes)` — the sha and
/// size the catalogue records for this pack.
///
/// `defs` and `files` are written in the order given (the builder supplies them
/// already sorted per the plan); the snapshot files and the `SHA256SUMS` lines are
/// sorted by path so the archive is canonical. `snapshot_dir` is packed verbatim
/// under `snapshot/`; its contents are never interpreted.
pub fn write_pack(
    out_archive: &Path,
    manifest: &PackManifest,
    defs: &[DefRecord],
    files: &[PackFileEntry],
    snapshot_dir: &Path,
    sign_seed_hex: &str,
) -> Result<(String, u64)> {
    use std::fmt::Write as _;

    // 1. Serialize the JSON members.
    let pack_json = serde_json::to_vec_pretty(manifest).context("serialize pack.json")?;
    let defs_jsonl = to_jsonl(defs).context("serialize defs.jsonl")?;
    let files_jsonl = to_jsonl(files).context("serialize files.jsonl")?;

    // 2. Gather the opaque snapshot payload, sorted by archive path.
    let snapshot = collect_snapshot_files(snapshot_dir)?;

    // 3. SHA256SUMS over every OTHER member, sorted by path.
    let mut sums: Vec<(String, String)> = Vec::with_capacity(3 + snapshot.len());
    sums.push((PACK_JSON.to_string(), sha256_hex(&pack_json)));
    sums.push((DEFS_JSONL.to_string(), sha256_hex(&defs_jsonl)));
    sums.push((FILES_JSONL.to_string(), sha256_hex(&files_jsonl)));
    for (rel, data) in &snapshot {
        sums.push((format!("{SNAPSHOT_DIR}/{rel}"), sha256_hex(data)));
    }
    sums.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sums_txt = String::new();
    for (path, hash) in &sums {
        let _ = writeln!(sums_txt, "{hash}  {path}");
    }

    // 4. tar the members (SHA256SUMS included) into an in-memory buffer.
    let mut builder = tar::Builder::new(Vec::new());
    append_regular(&mut builder, PACK_JSON, &pack_json)?;
    append_regular(&mut builder, SHA256SUMS, sums_txt.as_bytes())?;
    append_regular(&mut builder, DEFS_JSONL, &defs_jsonl)?;
    append_regular(&mut builder, FILES_JSONL, &files_jsonl)?;
    for (rel, data) in &snapshot {
        append_regular(&mut builder, &format!("{SNAPSHOT_DIR}/{rel}"), data)?;
    }
    let tar_bytes = builder.into_inner().context("finish tar")?;

    // 5. zstd-compress to the final archive bytes.
    let archive_bytes =
        zstd::encode_all(tar_bytes.as_slice(), ZSTD_LEVEL).context("zstd compress")?;

    // 6. Write the archive.
    if let Some(parent) = out_archive.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
    }
    fs::write(out_archive, &archive_bytes)
        .with_context(|| format!("write {}", out_archive.display()))?;

    // 7. Sign the FINAL archive bytes; write the detached signature.
    let archive_sha = sha256_hex(&archive_bytes);
    let sig_hex = sign_bytes(&archive_bytes, sign_seed_hex).context("sign archive")?;
    let sig_path = sig_path_for(out_archive);
    fs::write(&sig_path, sig_hex.as_bytes())
        .with_context(|| format!("write {}", sig_path.display()))?;

    Ok((archive_sha, archive_bytes.len() as u64))
}

/// Verify an archive's whole-file sha256 against `expected_sha256` and its detached
/// `sig` against `pubkey_hex`, refuse a `format_version` this build does not read,
/// then extract into `dest`. Returns the pack manifest.
///
/// Nothing is written to `dest` until every check passes: a bad sha (checked first,
/// before the signature), a bad signature, an unknown `format_version`, or an
/// internally inconsistent `SHA256SUMS` all return `Err` with `dest` untouched.
pub fn read_and_verify_pack(
    archive: &Path,
    sig: &Path,
    expected_sha256: &str,
    pubkey_hex: &str,
    dest: &Path,
) -> Result<PackManifest> {
    // The whole archive is read into memory because the detached signature is over
    // the complete archive bytes (ed25519 verifies a contiguous message); the hub's
    // auto-pull packs are capped well below that cost (AUTO_PULL_MAX_BYTES).
    let archive_bytes = fs::read(archive).with_context(|| format!("read {}", archive.display()))?;

    // 1. Whole-archive sha256 — FIRST, before the signature.
    let got = sha256_hex(&archive_bytes);
    let want = expected_sha256.trim();
    if !got.eq_ignore_ascii_case(want) {
        bail!("pack sha256 mismatch: archive is {got}, expected {want}");
    }

    // 2. Signature over the archive bytes.
    let sig_hex = fs::read_to_string(sig).with_context(|| format!("read {}", sig.display()))?;
    verify_bytes(&archive_bytes, sig_hex.trim(), pubkey_hex).context("pack signature")?;

    // 3. Decompress and load the members (still nothing written to dest).
    let tar_bytes = zstd::decode_all(archive_bytes.as_slice()).context("zstd decompress")?;
    drop(archive_bytes);
    let members = read_members(&tar_bytes)?;

    // 4. Parse the manifest and refuse an unknown format version — before extracting.
    let pack_json = members
        .get(PACK_JSON)
        .with_context(|| format!("{PACK_JSON} missing from archive"))?;
    let manifest: PackManifest =
        serde_json::from_slice(pack_json).with_context(|| format!("parse {PACK_JSON}"))?;
    if manifest.format_version != PACK_FORMAT_VERSION {
        bail!(
            "unsupported pack format_version {} (this build reads {PACK_FORMAT_VERSION})",
            manifest.format_version
        );
    }

    // 5. Internal integrity: every SHA256SUMS line matches its member.
    verify_sha256sums(&members)?;

    // 6. Every check passed — extract into dest.
    extract_into(&members, dest)?;

    Ok(manifest)
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// `<archive>.sig` — the signature rides beside the archive, never inside it.
fn sig_path_for(archive: &Path) -> PathBuf {
    let mut os = archive.as_os_str().to_os_string();
    os.push(".sig");
    PathBuf::from(os)
}

/// Lower-case hex sha256 of a buffer. Mirrors `harvest/sign.rs`'s `hex`.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Serialize a slice as JSON Lines (one compact object per line, trailing `\n`).
fn to_jsonl<T: Serialize>(items: &[T]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for item in items {
        let line = serde_json::to_vec(item)?;
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    Ok(out)
}

/// Append one regular file to the tar with a deterministic header (mode 0644,
/// mtime 0) so the archive does not leak the builder's local file metadata.
/// `append_data` sets the path (GNU long-name when needed) and the checksum.
fn append_regular<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, path, data)
        .with_context(|| format!("tar append {path}"))
}

/// Read a snapshot tree into `(relative-forward-slash path, bytes)`, sorted by path.
/// Regular files only; directories are recursed, other entry types skipped.
fn collect_snapshot_files(snapshot_dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    if !snapshot_dir.is_dir() {
        bail!("snapshot dir {} is not a directory", snapshot_dir.display());
    }
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    walk_files(snapshot_dir, snapshot_dir, &mut found)?;
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::with_capacity(found.len());
    for (rel, abs) in found {
        let data = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        out.push((rel, data));
    }
    Ok(out)
}

fn walk_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            walk_files(base, &path, out)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(base)
                .with_context(|| format!("{} is not under {}", path.display(), base.display()))?;
            out.push((rel_to_forward_slash(rel)?, path));
        }
        // symlinks and other node types are not snapshot data — skip them.
    }
    Ok(())
}

/// A relative path to a `/`-joined string, rejecting anything but plain components
/// (no `..`, no root, no non-UTF-8 — snapshot files are plain data files).
fn rel_to_forward_slash(rel: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => {
                parts.push(
                    s.to_str()
                        .context("non-UTF-8 path in snapshot")?
                        .to_string(),
                );
            }
            _ => bail!("unexpected path component in snapshot: {}", rel.display()),
        }
    }
    if parts.is_empty() {
        bail!("empty snapshot path");
    }
    Ok(parts.join("/"))
}

/// Read every regular tar entry into a map, keyed by a sanitised archive path.
fn read_members(tar_bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    let mut members: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("read tar entry")?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let raw = entry.path().context("tar entry path")?;
        let key = sanitize_archive_path(raw.as_ref())?;
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .with_context(|| format!("read archive member {key}"))?;
        if members.insert(key.clone(), buf).is_some() {
            bail!("duplicate archive member {key}");
        }
    }
    Ok(members)
}

/// Reject path traversal and absolute paths; return a `/`-joined relative key.
fn sanitize_archive_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(s) => {
                parts.push(s.to_str().context("non-UTF-8 path in archive")?.to_string());
            }
            Component::CurDir => {}
            _ => bail!("unsafe path in archive: {}", path.display()),
        }
    }
    if parts.is_empty() {
        bail!("empty path in archive");
    }
    Ok(parts.join("/"))
}

/// Verify every `SHA256SUMS` line against the loaded members. Redundant with the
/// outer signature (which covers these bytes too), but it is the pack's own
/// self-describing integrity record, so a reader honours it.
fn verify_sha256sums(members: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let sums = members
        .get(SHA256SUMS)
        .with_context(|| format!("{SHA256SUMS} missing from archive"))?;
    let text = std::str::from_utf8(sums).with_context(|| format!("{SHA256SUMS} is not UTF-8"))?;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (hash, path) = line
            .split_once("  ")
            .with_context(|| format!("malformed {SHA256SUMS} line: {line:?}"))?;
        let data = members
            .get(path)
            .with_context(|| format!("{SHA256SUMS} names {path}, absent from archive"))?;
        if !sha256_hex(data).eq_ignore_ascii_case(hash.trim()) {
            bail!("checksum mismatch for {path}");
        }
    }
    Ok(())
}

/// Write every member under `dest`, creating parent directories. Keys are `/`-joined
/// relative paths already sanitised by [`read_members`].
///
/// `pack.json` is written **last**. A pack directory counts as present (cached,
/// answerable, evictable) exactly when `pack.json` exists, and `BTreeMap` order puts
/// it *before* `snapshot/`, so a process killed partway through the extraction used
/// to leave a directory that looked complete with most of its snapshot missing.
/// Written last, `pack.json` is the marker that everything else is already there.
/// (`hub pull` extracts into a staging directory and renames it into place, so this
/// is the second line of defence for anyone extracting straight into a final path.)
fn extract_into(members: &BTreeMap<String, Vec<u8>>, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let write_member = |rel: &str, data: &[u8]| -> Result<()> {
        let mut target = dest.to_path_buf();
        for part in rel.split('/') {
            target.push(part);
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&target, data).with_context(|| format!("write {}", target.display()))
    };
    for (rel, data) in members {
        if rel != PACK_JSON {
            write_member(rel, data)?;
        }
    }
    if let Some(manifest) = members.get(PACK_JSON) {
        write_member(PACK_JSON, manifest)?;
    }
    Ok(())
}

// ── builder: defs + file inventory from a cloned corpus tree (plan §2/§3) ────

/// Largest file read for AST extraction — past a couple MB a "file" is a
/// generated bundle, not human code (mirrors the ingest extractor's `CODE_CAP`).
const DEFS_READ_CAP: u64 = 2 << 20;

/// Walk a cloned corpus tree and return (a) every definition
/// ([`xerj_autoindex::extract::code::definitions`]) as [`DefRecord`]s, sorted by
/// `(file, line)`, and (b) a file inventory as [`PackFileEntry`]s, sorted by
/// path — the `defs.jsonl` and `files.jsonl` members of the pack (plan §2).
///
/// `corpus_root` is `<code_root>/corpora/<slug>`; paths are recorded relative to
/// it with forward slashes (so they read the same on every OS and line up with
/// the docs index's `ax_path`). `licence_of(rel)` supplies the per-file licence
/// (the licence of the repo sub-tree the file sits in). `.git` and the corpus's
/// own `corpus.json` are skipped. This is NODE-FREE: it only reads files.
pub fn build_defs_and_files(
    corpus_root: &Path,
    licence_of: &dyn Fn(&str) -> String,
) -> Result<(Vec<DefRecord>, Vec<PackFileEntry>)> {
    let mut defs: Vec<DefRecord> = Vec::new();
    let mut files: Vec<PackFileEntry> = Vec::new();
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    walk_corpus(corpus_root, corpus_root, &mut found)?;

    for (rel, abs) in &found {
        // The corpus's own metadata file is not a source file.
        if rel == "corpus.json" {
            continue;
        }
        let bytes = fs::metadata(abs).map(|m| m.len()).unwrap_or(0);
        let ext = ext_of(rel);
        let licence = licence_of(rel);

        let lang = if is_code_ext(&ext) && bytes <= DEFS_READ_CAP {
            // Read once; the decoded text is used for both language resolution
            // (the `.m` objc/matlab probe needs content) and extraction.
            let raw = fs::read(abs).unwrap_or_default();
            let text = String::from_utf8_lossy(&raw);
            if let Some(lang) = lang_for_ext(&ext, &text) {
                defs.extend(definitions(rel, lang, &text));
                Some(lang)
            } else {
                None
            }
        } else {
            None
        };

        files.push(PackFileEntry {
            path: rel.clone(),
            lang: lang.unwrap_or("").to_string(),
            bytes,
            licence,
        });
    }

    defs.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((defs, files))
}

/// Recursively collect regular files as `(relative-forward-slash path, abs)`,
/// skipping `.git`. Mirrors the snapshot walker but keyed on a corpus root.
fn walk_corpus(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            walk_corpus(base, &path, out)?;
        } else if ft.is_file() {
            if let Ok(rel) = path.strip_prefix(base) {
                if let Ok(s) = rel_to_forward_slash(rel) {
                    out.push((s, path));
                }
            }
        }
    }
    Ok(())
}

/// Lower-case extension of a `/`-joined relative path (empty when none).
fn ext_of(rel: &str) -> String {
    rel.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.').map(|(_, e)| e))
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Map a lower-case extension to the registry language name
/// `extract/code.rs`'s `definitions` expects. Kept in lock-step with that
/// registry (there is no public enumerator to borrow it from); the
/// `lang_table_matches_is_code_ext` test fails if the two drift on any
/// extension this table knows. The `.m`/`.mm` collision is resolved the way the
/// extractor does — Objective-C markers win, MATLAB is the fallback for `.m`.
fn lang_for_ext(ext: &str, text: &str) -> Option<&'static str> {
    let name = match ext {
        "py" | "pyi" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "rs" => "rust",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "sh" | "bash" => "bash",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "scala" | "sc" => "scala",
        "dart" => "dart",
        "lua" => "lua",
        "pl" | "pm" => "perl",
        "r" => "r",
        "jl" => "julia",
        "hs" => "haskell",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "ml" => "ocaml",
        "mli" => "ocaml_interface",
        "zig" => "zig",
        "mm" => "objc",
        "m" => {
            if looks_like_objc(text) {
                "objc"
            } else {
                "matlab"
            }
        }
        "groovy" | "gradle" => "groovy",
        "ps1" | "psm1" | "psd1" => "powershell",
        "fs" | "fsx" => "fsharp",
        "nix" => "nix",
        "f90" | "f95" | "f03" => "fortran",
        "sol" => "solidity",
        "clj" | "cljs" | "cljc" => "clojure",
        "scm" | "ss" | "sld" | "sps" => "scheme",
        _ => return None,
    };
    Some(name)
}

/// `.m` disambiguator — Objective-C near-universally carries one of these
/// markers; a `.m` with none is MATLAB (the extractor's `looks_like_objc`).
fn looks_like_objc(text: &str) -> bool {
    [
        "#import",
        "@interface",
        "@implementation",
        "@protocol",
        "@end",
    ]
    .iter()
    .any(|m| text.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_autoindex::harvest::sign::generate_keypair;
    use xerj_common::hub::{LicenceClass, PackIndex, PackSource};

    fn sample_manifest() -> PackManifest {
        PackManifest {
            format_version: PACK_FORMAT_VERSION,
            slug: "lz4".into(),
            pin_sha: "0774d05537f9762f838f7ab541b7765f1a729cb5".into(),
            pin_date: "2026-10-02".into(),
            built_at: "2026-10-14T00:00:00Z".into(),
            builder_version: "xerj 1.0.0".into(),
            index_prefix: "xc-lz4-b1".into(),
            indices: vec![PackIndex {
                name: "xc-lz4-b1-docs".into(),
                docs: 10,
                bytes: 2048,
            }],
            snapshot_name: "lz4-0774d055".into(),
            licence_spdx: "BSD-2-Clause".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            sources: vec![PackSource {
                repo: "lz4".into(),
                url: "https://github.com/lz4/lz4".into(),
                sha: "0774d05537f9762f838f7ab541b7765f1a729cb5".into(),
                licence: "BSD-2-Clause".into(),
            }],
            defs_count: 2,
            files_count: 2,
        }
    }

    fn sample_defs() -> Vec<DefRecord> {
        vec![
            DefRecord {
                symbol: "LZ4_compress_default".into(),
                kind: "function".into(),
                lang: "c".into(),
                file: "lib/lz4.c".into(),
                line: 700,
                text: "int LZ4_compress_default(const char* src, char* dst, int n, int cap)".into(),
                value: None,
            },
            DefRecord {
                symbol: "LZ4_VERSION_RELEASE".into(),
                kind: "define".into(),
                lang: "c".into(),
                file: "lib/lz4.h".into(),
                line: 133,
                text: "#define LZ4_VERSION_RELEASE 0".into(),
                value: Some("0".into()),
            },
        ]
    }

    fn sample_files() -> Vec<PackFileEntry> {
        vec![
            PackFileEntry {
                path: "lib/lz4.c".into(),
                lang: "c".into(),
                bytes: 100_000,
                licence: "BSD-2-Clause".into(),
            },
            PackFileEntry {
                path: "lib/lz4.h".into(),
                lang: "c".into(),
                bytes: 20_000,
                licence: "BSD-2-Clause".into(),
            },
        ]
    }

    /// Build a `snapshot/` dir with two small files (one binary, one in a subdir)
    /// under `root`. Returns the snapshot dir path.
    fn make_snapshot(root: &Path) -> PathBuf {
        let snap = root.join("snapshot");
        fs::create_dir_all(snap.join("idx")).unwrap();
        fs::write(
            snap.join("manifest.json"),
            br#"{"snapshot":"lz4-0774d055"}"#,
        )
        .unwrap();
        fs::write(snap.join("idx/segment_0.bin"), b"\x00\x01\x02binary\xff").unwrap();
        snap
    }

    fn dir_is_empty_or_absent(p: &Path) -> bool {
        match fs::read_dir(p) {
            Ok(mut it) => it.next().is_none(),
            Err(_) => true,
        }
    }

    #[test]
    fn round_trip_write_then_read_and_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot(tmp.path());
        let out = tmp.path().join("lz4-0774d055.hubpack.tar.zst");
        let (seed, public) = generate_keypair().unwrap();

        let (sha, bytes) = write_pack(
            &out,
            &sample_manifest(),
            &sample_defs(),
            &sample_files(),
            &snap,
            &seed,
        )
        .unwrap();
        assert!(bytes > 0);
        assert_eq!(bytes, fs::metadata(&out).unwrap().len());
        assert_eq!(sha.len(), 64);
        let sig = sig_path_for(&out);
        assert!(
            sig.exists(),
            "detached signature written next to the archive"
        );

        let dest = tmp.path().join("dest");
        let manifest = read_and_verify_pack(&out, &sig, &sha, &public, &dest).unwrap();

        // Manifest round-trips.
        assert_eq!(manifest.slug, "lz4");
        assert_eq!(manifest.format_version, PACK_FORMAT_VERSION);
        assert_eq!(manifest.indices.len(), 1);
        assert_eq!(manifest.indices[0].name, "xc-lz4-b1-docs");
        assert_eq!(manifest.sources[0].repo, "lz4");
        assert_eq!(manifest.licence_class, LicenceClass::AdaptWithAttribution);

        // Every member is present in dest.
        for member in ["pack.json", "SHA256SUMS", "defs.jsonl", "files.jsonl"] {
            assert!(dest.join(member).exists(), "{member} missing from dest");
        }
        assert!(dest.join("snapshot/manifest.json").exists());
        assert!(dest.join("snapshot/idx/segment_0.bin").exists());

        // Opaque snapshot bytes survive verbatim.
        assert_eq!(
            fs::read(dest.join("snapshot/idx/segment_0.bin")).unwrap(),
            b"\x00\x01\x02binary\xff"
        );

        // defs.jsonl / files.jsonl round-trip line by line.
        let defs_txt = fs::read_to_string(dest.join("defs.jsonl")).unwrap();
        assert_eq!(defs_txt.lines().count(), 2);
        let first: DefRecord = serde_json::from_str(defs_txt.lines().next().unwrap()).unwrap();
        assert_eq!(first, sample_defs()[0]);
        let files_txt = fs::read_to_string(dest.join("files.jsonl")).unwrap();
        assert_eq!(files_txt.lines().count(), 2);
        let f0: PackFileEntry = serde_json::from_str(files_txt.lines().next().unwrap()).unwrap();
        assert_eq!(f0, sample_files()[0]);
    }

    #[test]
    fn flipped_archive_byte_is_refused_and_dest_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot(tmp.path());
        let out = tmp.path().join("pack.hubpack.tar.zst");
        let (seed, public) = generate_keypair().unwrap();
        let (sha, _) = write_pack(
            &out,
            &sample_manifest(),
            &sample_defs(),
            &sample_files(),
            &snap,
            &seed,
        )
        .unwrap();
        let sig = sig_path_for(&out);

        // Flip a byte in the middle of the archive.
        let mut data = fs::read(&out).unwrap();
        let mid = data.len() / 2;
        data[mid] ^= 0xff;
        fs::write(&out, &data).unwrap();

        let dest = tmp.path().join("dest");
        // `sha` is the original, correct digest; the flipped archive no longer matches.
        let err = read_and_verify_pack(&out, &sig, &sha, &public, &dest).unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err:#}");
        assert!(dir_is_empty_or_absent(&dest), "dest must stay unpopulated");
    }

    #[test]
    fn wrong_expected_sha_is_refused_before_signature() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot(tmp.path());
        let out = tmp.path().join("pack.hubpack.tar.zst");
        let (seed, public) = generate_keypair().unwrap();
        let (_sha, _) = write_pack(
            &out,
            &sample_manifest(),
            &sample_defs(),
            &sample_files(),
            &snap,
            &seed,
        )
        .unwrap();
        let sig = sig_path_for(&out);

        // The archive and its signature are valid; only the expected sha is wrong.
        // A sha error here proves the sha is checked before the (valid) signature.
        let wrong = "0".repeat(64);
        let dest = tmp.path().join("dest");
        let err = read_and_verify_pack(&out, &sig, &wrong, &public, &dest).unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err:#}");
        assert!(dir_is_empty_or_absent(&dest));
    }

    #[test]
    fn unknown_format_version_is_refused_and_dest_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot(tmp.path());
        let out = tmp.path().join("pack.hubpack.tar.zst");
        let (seed, public) = generate_keypair().unwrap();

        let mut manifest = sample_manifest();
        manifest.format_version = 2; // a layout this build does not read
        let (sha, _) = write_pack(
            &out,
            &manifest,
            &sample_defs(),
            &sample_files(),
            &snap,
            &seed,
        )
        .unwrap();
        let sig = sig_path_for(&out);

        // sha + signature are valid; the manifest's format_version is the problem.
        let dest = tmp.path().join("dest");
        let err = read_and_verify_pack(&out, &sig, &sha, &public, &dest).unwrap_err();
        assert!(err.to_string().contains("format_version"), "{err:#}");
        assert!(dir_is_empty_or_absent(&dest), "dest must stay unpopulated");
    }

    #[test]
    fn foreign_public_key_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot(tmp.path());
        let out = tmp.path().join("pack.hubpack.tar.zst");
        let (seed, _public) = generate_keypair().unwrap();
        let (sha, _) = write_pack(
            &out,
            &sample_manifest(),
            &sample_defs(),
            &sample_files(),
            &snap,
            &seed,
        )
        .unwrap();
        let sig = sig_path_for(&out);

        // A pack signed by one key must not verify under an unrelated key.
        let (_other_seed, other_public) = generate_keypair().unwrap();
        let dest = tmp.path().join("dest");
        let err = read_and_verify_pack(&out, &sig, &sha, &other_public, &dest).unwrap_err();
        assert!(err.to_string().contains("signature"), "{err:#}");
        assert!(dir_is_empty_or_absent(&dest));
    }

    #[test]
    fn sha256sums_covers_every_other_member_sorted() {
        // The written SHA256SUMS names pack.json/defs.jsonl/files.jsonl and each
        // snapshot file, sorted by path, and never itself.
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot(tmp.path());
        let out = tmp.path().join("pack.hubpack.tar.zst");
        let (seed, public) = generate_keypair().unwrap();
        let (sha, _) = write_pack(
            &out,
            &sample_manifest(),
            &sample_defs(),
            &sample_files(),
            &snap,
            &seed,
        )
        .unwrap();
        let sig = sig_path_for(&out);
        let dest = tmp.path().join("dest");
        read_and_verify_pack(&out, &sig, &sha, &public, &dest).unwrap();

        let sums = fs::read_to_string(dest.join("SHA256SUMS")).unwrap();
        let paths: Vec<&str> = sums
            .lines()
            .filter_map(|l| l.split_once("  ").map(|(_, p)| p))
            .collect();
        assert_eq!(
            paths,
            vec![
                "defs.jsonl",
                "files.jsonl",
                "pack.json",
                "snapshot/idx/segment_0.bin",
                "snapshot/manifest.json",
            ]
        );
        assert!(
            !sums.contains(SHA256SUMS),
            "SHA256SUMS must not list itself"
        );
    }

    #[test]
    fn pack_json_is_written_last_so_an_interrupted_extraction_never_looks_complete() {
        // A pack dir is "present" when pack.json exists, and BTreeMap order puts
        // pack.json BEFORE snapshot/: extracted in map order, a process killed
        // after pack.json looked like a complete pack with most of its snapshot
        // missing. Fail the extraction partway (a file where the snapshot
        // directory has to go) and check pack.json was not written.
        let mut members: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        members.insert(PACK_JSON.into(), b"{}".to_vec());
        members.insert(SHA256SUMS.into(), b"".to_vec());
        members.insert("defs.jsonl".into(), b"".to_vec());
        members.insert("snapshot/idx/segment_0.bin".into(), vec![1, 2, 3]);

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("snapshot"), b"in the way").unwrap();
        assert!(extract_into(&members, &dest).is_err());
        assert!(
            !dest.join(PACK_JSON).exists(),
            "an extraction that failed on the snapshot must not have left pack.json"
        );
        assert!(
            dest.join("defs.jsonl").exists(),
            "the members before the failure were written"
        );

        // Cleared, the same members extract completely, pack.json included.
        let ok = tmp.path().join("ok");
        extract_into(&members, &ok).unwrap();
        assert_eq!(fs::read(ok.join(PACK_JSON)).unwrap(), b"{}");
        assert!(ok.join("snapshot/idx/segment_0.bin").is_file());
    }

    #[test]
    fn build_defs_and_files_extracts_c_defines_and_inventories_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("corpora").join("mini-c");
        fs::create_dir_all(root.join("lz4")).unwrap();
        fs::write(
            root.join("lz4/limits.h"),
            "#define MINI_LIMIT 128\nint helper(int x);\n",
        )
        .unwrap();
        fs::write(root.join("LICENSE"), "MIT License\n").unwrap();
        // corpus.json is metadata, never a source file.
        fs::write(root.join("corpus.json"), "{}").unwrap();
        // .git content is skipped.
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let licence_of = |_rel: &str| "MIT".to_string();
        let (defs, files) = build_defs_and_files(&root, &licence_of).unwrap();

        // The #define carries its value; the prototype has none.
        let limit = defs
            .iter()
            .find(|d| d.symbol == "MINI_LIMIT")
            .expect("MINI_LIMIT extracted");
        assert_eq!(limit.value.as_deref(), Some("128"));
        assert_eq!(limit.file, "lz4/limits.h");
        assert_eq!(limit.lang, "c");
        assert_eq!(limit.line, 1);

        // Inventory: the .h and the LICENSE, never corpus.json or .git/*.
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["LICENSE", "lz4/limits.h"]);
        assert!(!paths
            .iter()
            .any(|p| p.contains(".git") || *p == "corpus.json"));
        let h = files.iter().find(|f| f.path == "lz4/limits.h").unwrap();
        assert_eq!(h.lang, "c");
        assert_eq!(h.licence, "MIT");
        // A non-code file is inventoried with an empty lang.
        assert_eq!(files.iter().find(|f| f.path == "LICENSE").unwrap().lang, "");
    }

    #[test]
    fn defs_are_sorted_by_file_then_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("c");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("b.c"), "#define B1 1\n#define B2 2\n").unwrap();
        fs::write(root.join("a.c"), "#define A1 9\n").unwrap();
        let (defs, _) = build_defs_and_files(&root, &|_| "MIT".into()).unwrap();
        let order: Vec<(&str, u32)> = defs.iter().map(|d| (d.file.as_str(), d.line)).collect();
        assert_eq!(order, vec![("a.c", 1), ("b.c", 1), ("b.c", 2)]);
    }

    /// The hub's extension→language table must agree with the extractor's own
    /// `is_code_ext` for every extension it claims — the guard against the two
    /// drifting apart (there is no public registry enumerator to share).
    #[test]
    fn lang_table_matches_is_code_ext() {
        let claimed = [
            "py", "pyi", "js", "jsx", "mjs", "cjs", "ts", "mts", "cts", "tsx", "rs", "go", "java",
            "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "rb", "php", "cs", "sh", "bash",
            "kt", "kts", "swift", "scala", "sc", "dart", "lua", "pl", "pm", "r", "jl", "hs", "ex",
            "exs", "erl", "hrl", "ml", "mli", "zig", "m", "mm", "groovy", "gradle", "ps1", "psm1",
            "psd1", "fs", "fsx", "nix", "f90", "f95", "f03", "sol", "clj", "cljs", "cljc", "scm",
            "ss", "sld", "sps",
        ];
        for ext in claimed {
            assert!(
                is_code_ext(ext),
                "{ext} is in the hub lang table but the extractor no longer treats it as code"
            );
            assert!(
                lang_for_ext(ext, "").is_some(),
                "{ext} resolves to no language"
            );
        }
        // A non-code extension resolves to no language.
        assert!(lang_for_ext("md", "").is_none());
        assert!(lang_for_ext("", "").is_none());
    }

    #[test]
    fn dot_m_probes_objc_vs_matlab() {
        assert_eq!(lang_for_ext("m", "@interface Foo\n@end\n"), Some("objc"));
        assert_eq!(lang_for_ext("m", "function y = f(x)\n"), Some("matlab"));
        assert_eq!(lang_for_ext("mm", ""), Some("objc"));
    }

    /// The language NAME `lang_for_ext` returns must actually exist in the
    /// extractor's `registry()` — otherwise a row rename (e.g. `csharp` ->
    /// `c_sharp`) would make every `.cs` file yield ZERO defs while `files.jsonl`
    /// still labels them `csharp`. There is no public registry enumerator, so we
    /// prove membership the only observable way: a known one-definition snippet
    /// routed through the resolved name must extract at least one def. (Names
    /// that do not resolve in the registry return an empty Vec from
    /// `definitions`.)
    #[test]
    fn lang_names_resolve_in_the_extractor_registry() {
        // (extension, file, a one-definition snippet in that language).
        let cases: &[(&str, &str, &str)] = &[
            ("c", "a.c", "int cfn(void){return 0;}\n"),
            ("cpp", "a.cpp", "class CppThing { public: void run(); };\n"),
            ("rs", "a.rs", "pub fn rfn() {}\n"),
            ("go", "a.go", "package p\nfunc GoFn() {}\n"),
            ("py", "a.py", "def pyfn():\n    return 1\n"),
            ("js", "a.js", "function jsfn(){}\n"),
            ("ts", "a.ts", "function tsfn(){}\n"),
            ("java", "A.java", "class JavaThing {}\n"),
            ("cs", "A.cs", "class CsThing {}\n"),
            ("rb", "a.rb", "def rbfn\nend\n"),
            ("php", "a.php", "<?php function phpfn(){}\n"),
            ("sh", "a.sh", "shfn() { :; }\n"),
        ];
        for (ext, file, snippet) in cases {
            let lang = lang_for_ext(ext, snippet)
                .unwrap_or_else(|| panic!(".{ext} resolves to no language"));
            let defs = definitions(file, lang, snippet);
            assert!(
                !defs.is_empty(),
                "lang_for_ext(.{ext}) = {lang:?} extracted no defs — the name is no longer a \
                 registry() row (a rename would silently zero out every .{ext} file's defs)"
            );
        }
    }
}
