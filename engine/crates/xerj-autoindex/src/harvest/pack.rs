//! Deterministic pack emission: merged records → the portable artifact.
//!
//! A pack is a directory (a zip of it later) that ANY tool can consume:
//! sharded `records-*.jsonl`, a `manifest.json` with per-file sha256s, the
//! recipe that built it, the suggestion files (`mapping.suggested.json`,
//! `relations.jsonl`, `suggestions.md`), and a `SHA256SUMS` over
//! everything. Determinism is
//! the load-bearing property: shard assignment is `xxh3_64(id) % shards`,
//! records are id-ordered, and every record carries the SAME key set (a
//! key absent from one record is `[]` for array keys, `null` otherwise) —
//! so an unchanged input rebuilds byte-identical shard files, and the
//! `_id`s the engine derives stay stable across rebuilds (autoindex
//! clusters datasets by field-name set; an omitted optional field would
//! silently move a record between datasets).
//!
//! `manifest.json` and `SHA256SUMS` necessarily vary (timestamp,
//! generation) — determinism is claimed for the RECORDS, never the wrapper.
//!
//! Reading a pack back ([`read_manifest`]) is the consumption half of
//! `xerj corpus add --from <pack>`: checksum-verify EVERYTHING the manifest
//! and SHA256SUMS claim, refusing loudly on any mismatch, because a pack is
//! an artifact that traveled — disk, download, or release attachment —
//! before it got here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::xxh3_64;

use super::recipe::Recipe;

/// Per-source counts for the manifest and report: what this run actually
/// did. `unchanged` = watermark matched, so the source was not walked at
/// all (and must not be pruned either — everything it stored is live).
#[derive(serde::Serialize)]
pub struct SourceStat {
    pub slug: String,
    pub kind: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub licence: String,
    pub watermark: Option<String>,
    pub unchanged: bool,
    pub records: usize,
    pub new: usize,
    pub skipped: usize,
    pub pruned: usize,
}

pub struct RunStats {
    pub envelopes: usize,
    pub per_source: Vec<SourceStat>,
}

#[derive(Debug)]
pub struct PackResult {
    pub dir: PathBuf,
    pub generation: u64,
    pub records: usize,
    pub shards: usize,
}

/// Build (or rebuild) the pack under `<build_dir>/pack/<name>`.
pub fn emit(
    build_dir: &Path,
    recipe: &Recipe,
    recipe_toml: &str,
    mut records: Vec<Map<String, Value>>,
    stats: &RunStats,
    sugg: &crate::harvest::suggest::Suggestions,
) -> Result<PackResult> {
    let pack_dir = build_dir.join("pack").join(&recipe.name);
    let generation = previous_generation(&pack_dir)? + 1;
    std::fs::create_dir_all(&pack_dir)
        .with_context(|| format!("cannot create {}", pack_dir.display()))?;

    // Order first, then refuse duplicate ids: a pack with two records of
    // the same id breaks the engine-side _id determinism promise, and a
    // duplicate here means identity resolution failed silently.
    records.sort_by(|a, b| {
        a.get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("id").and_then(Value::as_str).unwrap_or(""))
    });
    // An empty id is refused BEFORE the duplicate scan, not folded into it:
    // the old guard only compared NON-empty ids (`!seen_id.is_empty()`), so
    // any number of empty-id records slipped past and collapsed to one _id
    // at the consumer — a silent-wrong-result, not a loud refusal (#1110
    // negative-path audit, gap 2). A record whose `id_from` fields never
    // resolved (identity.rs defaults the id to "") has no stable identity to
    // key `_id` on, so it cannot ship in a pack at all.
    let mut seen_id: Option<&str> = None;
    for r in &records {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() {
            bail!(
                "a record reached the pack with an empty id: its `id_from` fields never \
                 resolved, so it has no stable _id — fix the recipe's id mapping or the \
                 source rather than ship a record that collapses with every other id-less one"
            );
        }
        if seen_id == Some(id) {
            bail!("duplicate emitted id '{id}': identity edges failed to merge two records that claim the same id");
        }
        seen_id = Some(id);
    }

    let shards = recipe.emit.shards.min(records.len()).max(1);
    let uniform = uniform_keys(&records);

    // shard → line buffer; records are id-sorted, so appending preserves
    // in-shard order without a second sort.
    let mut shard_lines: Vec<String> = vec![String::new(); shards];
    for r in &records {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");
        let bucket = (xxh3_64(id.as_bytes()) % shards as u64) as usize;
        let mut full = r.clone();
        for (k, is_array) in &uniform {
            full.entry(k.clone()).or_insert_with(|| {
                if *is_array {
                    Value::Array(Vec::new())
                } else {
                    Value::Null
                }
            });
        }
        shard_lines[bucket].push_str(&super::store::canonical_json(&full));
        shard_lines[bucket].push('\n');
    }

    // Remove any `records-*.jsonl` left in `pack_dir` by an earlier generation
    // that populated different (or more) shard buckets: otherwise orphan shard
    // files survive on disk and ship in a zip while invisible to `manifest.json`
    // and `SHA256SUMS` (#1110 gap 5a).
    for entry in std::fs::read_dir(&pack_dir)
        .with_context(|| format!("cannot read pack dir {}", pack_dir.display()))?
    {
        let e = entry.with_context(|| format!("cannot read entry in {}", pack_dir.display()))?;
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("records-") && name.ends_with(".jsonl") {
            std::fs::remove_file(e.path())
                .with_context(|| format!("cannot remove stale shard {}", e.path().display()))?;
        }
    }

    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    for (i, lines) in shard_lines.iter().enumerate() {
        let name = format!("records-{:05}.jsonl", i);
        if lines.is_empty() {
            continue; // small corpora need fewer files than shards
        }
        let path = pack_dir.join(&name);
        std::fs::write(&path, lines.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?;
        let n = lines.lines().count();
        files.insert(
            name.clone(),
            serde_json::json!({ "sha256": hex(&Sha256::digest(lines.as_bytes())), "records": n }),
        );
    }

    // the recipe ships verbatim: it is the pack's provenance, licence
    // reasoning included
    std::fs::write(pack_dir.join("recipe.toml"), recipe_toml)?;
    let recipe_sha = hex(&Sha256::digest(recipe_toml.as_bytes()));

    // suggestions — computed by `suggest` from a deterministic sample, never
    // applied to anything. Deliberately NOT in the manifest's `files` map
    // (that map is records files with per-file counts semantics); they ride
    // in SHA256SUMS like every other artifact, so a consumer can verify
    // them, and the M3 reader re-hashes them without needing to interpret
    // them — packs from before this table existed still verify clean.
    std::fs::write(
        pack_dir.join("mapping.suggested.json"),
        serde_json::to_vec_pretty(&sugg.mapping_doc())?,
    )?;
    std::fs::write(pack_dir.join("relations.jsonl"), sugg.relations_jsonl())?;
    std::fs::write(pack_dir.join("suggestions.md"), sugg.to_markdown())?;

    let manifest = serde_json::json!({
        "format_version": 1,
        "kind": "harvested",
        "pack": recipe.name,
        "description": recipe.description,
        "created_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "generation": generation,
        "recipe_sha256": recipe_sha,
        "counts": {
            "envelopes": stats.envelopes,
            "records": records.len(),
            "shards": shards,
        },
        "sources": stats
            .per_source
            .iter()
            .map(|s| serde_json::to_value(s).expect("SourceStat serializes"))
            .collect::<Vec<_>>(),
        "files": Value::Object(files.clone().into_iter().collect()),
        "tool": { "name": "xerj", "version": env!("CARGO_PKG_VERSION") },
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write(pack_dir.join("manifest.json"), &manifest_bytes)?;

    let mut sums = String::new();
    let mut names: Vec<String> = files.keys().cloned().collect();
    names.push("recipe.toml".to_string());
    names.push("manifest.json".to_string());
    names.push("mapping.suggested.json".to_string());
    names.push("relations.jsonl".to_string());
    names.push("suggestions.md".to_string());
    names.sort();
    for n in &names {
        let bytes = std::fs::read(pack_dir.join(n)).with_context(|| format!("checksumming {n}"))?;
        sums.push_str(&hex(&Sha256::digest(&bytes)));
        sums.push_str("  ");
        sums.push_str(n);
        sums.push('\n');
    }
    std::fs::write(pack_dir.join("SHA256SUMS"), sums)?;

    Ok(PackResult {
        dir: pack_dir,
        generation,
        records: records.len(),
        shards,
    })
}

/// The uniform key set: every key any record carries, and whether missing
/// values should fill as `[]` (array wherever it appears) or `null`.
fn uniform_keys(records: &[Map<String, Value>]) -> BTreeMap<String, bool> {
    let mut out: BTreeMap<String, (bool /*any array*/, bool /*all non-null array*/)> =
        BTreeMap::new();
    for r in records {
        for (k, v) in r {
            let e = out.entry(k.clone()).or_insert((false, true));
            if v.is_array() {
                e.0 = true;
            } else if !v.is_null() {
                e.1 = false;
            }
        }
    }
    out.into_iter()
        .map(|(k, (any, all))| (k, any && all))
        .collect()
}

fn previous_generation(pack_dir: &Path) -> Result<u64> {
    let manifest_path = pack_dir.join("manifest.json");
    let bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {}", manifest_path.display()))
        }
    };
    let v: Value = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "corrupt previous manifest {} (not valid JSON; pass --fresh to rebuild from scratch)",
            manifest_path.display()
        )
    })?;
    v.get("generation").and_then(Value::as_u64).ok_or_else(|| {
        anyhow::anyhow!(
            "corrupt previous manifest {}: missing or non-integer 'generation' \
             (pass --fresh to rebuild from scratch)",
            manifest_path.display()
        )
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── reading a pack back (consumption) ───────────────────────────────────────

/// One source row of a read pack — the pseudo-repo `corpus add` writes for
/// it (repo=slug, licence, url, watermark-as-sha).
#[derive(Debug, Clone)]
pub struct PackSource {
    pub slug: String,
    pub licence: String,
    pub url: Option<String>,
    pub watermark: Option<String>,
}

/// One records file of a read pack, already checksum-verified.
#[derive(Debug, Clone)]
pub struct PackFile {
    pub name: String,
    pub records: usize,
}

/// A VERIFIED pack: everything the manifest and SHA256SUMS claim was checked
/// against the bytes on disk before this was returned.
#[derive(Debug)]
pub struct PackMeta {
    pub name: String,
    pub description: String,
    pub generation: u64,
    pub records: usize,
    pub sources: Vec<PackSource>,
    /// Records files in name order — the deterministic read order.
    pub files: Vec<PackFile>,
}

/// Read and VERIFY a pack directory. Strict where [`emit`] was careful: an
/// unknown `format_version` is refused (a reader that silently indexed a
/// future format would mis-map fields), every SHA256SUMS line is re-hashed,
/// and every records file's sha256 and line count must match the manifest.
/// A pack that fails here is corrupt or tampered with, and the error says
/// which file and why.
pub fn read_manifest(pack_dir: &Path) -> Result<PackMeta> {
    let at = |what: &str| format!("{}: {what}", pack_dir.display());

    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(pack_dir.join("manifest.json"))
            .with_context(|| at("cannot read manifest.json"))?,
    )
    .with_context(|| at("manifest.json is not valid JSON"))?;

    let version = v.get("format_version").and_then(Value::as_u64).unwrap_or(0);
    if version != 1 {
        bail!(at(&format!(
            "pack format version {version} is not supported by this build (supported: 1)"
        )));
    }
    let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "harvested" {
        bail!(at(&format!(
            "not a harvested pack (kind='{kind}') — this reader consumes packs from `xerj \
             corpus build`"
        )));
    }
    let name = v.get("pack").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        bail!(at("manifest has no 'pack' name"));
    }

    // files: name → {sha256, records}. A file name is about to become a path
    // under pack_dir — plain file names only, same charset thinking as the
    // zip-entry gate in source.rs.
    let mut files: Vec<PackFile> = Vec::new();
    let mut declared: BTreeMap<String, String> = BTreeMap::new();
    for (fname, meta) in v
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!(at("manifest has no 'files' map")))?
    {
        if !plain_file_name(fname) {
            bail!(at(&format!(
                "manifest names file '{fname}', which is not a plain file name"
            )));
        }
        let sha = meta
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| anyhow::anyhow!(at(&format!("file '{fname}' has no valid sha256"))))?;
        let records =
            meta.get("records").and_then(Value::as_u64).ok_or_else(|| {
                anyhow::anyhow!(at(&format!("file '{fname}' has no record count")))
            })? as usize;
        declared.insert(fname.clone(), sha.to_string());
        files.push(PackFile {
            name: fname.clone(),
            records,
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));

    // SHA256SUMS is the outer truth: re-hash EVERYTHING it names.
    let sums_raw = std::fs::read_to_string(pack_dir.join("SHA256SUMS"))
        .with_context(|| at("cannot read SHA256SUMS"))?;
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    for line in sums_raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Some((sha, fname)) = line.split_once("  ") else {
            bail!(at(&format!(
                "SHA256SUMS line is not '<sha>  <file>': '{line}'"
            )));
        };
        if !plain_file_name(fname) {
            bail!(at(&format!(
                "SHA256SUMS names '{fname}', which is not a plain file name"
            )));
        }
        sums.insert(fname.to_string(), sha.to_string());
    }
    for required in ["manifest.json", "recipe.toml"] {
        if !sums.contains_key(required) {
            bail!(at(&format!("SHA256SUMS does not cover {required}")));
        }
    }
    for (fname, sha) in &declared {
        if !sums.contains_key(fname) {
            bail!(at(&format!("SHA256SUMS does not cover {fname}")));
        }
        if &sums[fname] != sha {
            bail!(at(&format!(
                "{fname}: manifest sha256 and SHA256SUMS disagree — the pack is inconsistent"
            )));
        }
    }
    for (fname, sha) in &sums {
        let bytes = std::fs::read(pack_dir.join(fname))
            .with_context(|| at(&format!("SHA256SUMS names {fname}, which is missing")))?;
        let got = hex(&Sha256::digest(&bytes));
        if &got != sha {
            bail!(at(&format!(
                "{fname}: checksum mismatch (want {sha}, got {got}) — corrupt or tampered"
            )));
        }
    }

    // line counts: the manifest's per-file record counts must be the truth
    for f in &files {
        let n = std::fs::read_to_string(pack_dir.join(&f.name))
            .with_context(|| at(&format!("cannot read {}", f.name)))?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        if n != f.records {
            bail!(at(&format!(
                "{}: manifest says {} records, file holds {n}",
                f.name, f.records
            )));
        }
    }

    let sources = v
        .get("sources")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!(at("manifest has no 'sources' array")))?
        .iter()
        .map(|s| PackSource {
            slug: s
                .get("slug")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            licence: s
                .get("licence")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            url: s
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
                .map(str::to_string),
            watermark: s
                .get("watermark")
                .and_then(Value::as_str)
                .filter(|w| !w.is_empty())
                .map(str::to_string),
        })
        .collect::<Vec<PackSource>>();
    if sources.is_empty() {
        bail!(at("manifest declares no sources"));
    }
    if sources.iter().any(|s| s.slug.is_empty()) {
        bail!(at("a manifest source has no slug"));
    }

    Ok(PackMeta {
        name: name.to_string(),
        description: v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        generation: v.get("generation").and_then(Value::as_u64).unwrap_or(0),
        records: v
            .get("counts")
            .and_then(|c| c.get("records"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        sources,
        files,
    })
}

/// A name that may be joined onto a pack dir without escaping it: no path
/// separators, no `..`, no drive letters, not hidden, not empty.
fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
        && name != "."
        && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::recipe::{Emit, Envelope, Identity, Merge, Suggest};
    use serde_json::json;

    fn recipe(name: &str, shards: usize) -> Recipe {
        Recipe {
            name: name.to_string(),
            description: String::new(),
            envelope: Envelope {
                id_from: vec!["id".into()],
                title_from: vec!["id".into()],
                body_join: vec![],
                defs_from: vec![],
                passthrough: false,
            },
            sources: vec![],
            identity: Identity {
                edges: vec![],
                canonical_source_order: vec![],
            },
            merge: Merge::default(),
            derived: vec![],
            emit: Emit { shards },
            suggest: Suggest { sample: 64 },
        }
    }

    fn rec(id: &str, extra: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(id));
        for (k, v) in extra {
            m.insert(k.to_string(), v.clone());
        }
        m
    }

    fn stats() -> RunStats {
        RunStats {
            envelopes: 0,
            per_source: vec![],
        }
    }

    /// A minimal real suggestion set — the files must exist and be
    /// checksum-covered even for a tiny pack.
    fn sugg(records: &[Map<String, Value>]) -> crate::harvest::suggest::Suggestions {
        crate::harvest::suggest::analyze(records, 64)
    }

    #[test]
    fn uniform_keys_fill_and_shard_determinism() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![
            rec("a", &[("aliases", json!(["X1"]))]),
            rec("b", &[("severity", json!("HIGH"))]),
        ];
        let r1 = emit(
            tmp.path(),
            &recipe("t", 4),
            "# recipe\n",
            records.clone(),
            &stats(),
            &sugg(&records),
        )
        .unwrap();
        assert_eq!(r1.records, 2);
        assert_eq!(r1.generation, 1);

        // every shard file's records carry BOTH keys
        for entry in std::fs::read_dir(&r1.dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("records-") {
                continue;
            }
            for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                let v: Value = serde_json::from_str(line).unwrap();
                assert!(v.get("aliases").is_some(), "array key filled");
                assert!(v.get("severity").is_some(), "scalar key filled");
                let id = v["id"].as_str().unwrap();
                if id == "a" {
                    assert_eq!(v["aliases"], json!(["X1"]));
                    assert_eq!(v["severity"], json!(null));
                } else {
                    assert_eq!(v["aliases"], json!([]), "array key fills empty");
                    assert_eq!(v["severity"], json!("HIGH"));
                }
            }
        }

        // rebuild from a DIFFERENT input order → byte-identical records
        let mut shuffled = records.clone();
        shuffled.reverse();
        let before: Vec<(String, Vec<u8>)> = std::fs::read_dir(&r1.dir)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().to_string(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .filter(|(n, _)| n.starts_with("records-"))
            .collect();
        let r2 = emit(
            tmp.path(),
            &recipe("t", 4),
            "# recipe\n",
            shuffled,
            &stats(),
            &sugg(&records),
        )
        .unwrap();
        assert_eq!(r2.generation, 2, "generation advances");
        for (name, bytes) in before {
            assert_eq!(
                std::fs::read(r2.dir.join(&name)).unwrap(),
                bytes,
                "{name} identical"
            );
        }
    }

    fn src_stat(slug: &str) -> SourceStat {
        SourceStat {
            slug: slug.to_string(),
            kind: "dir".to_string(),
            path: Some("data".to_string()),
            url: None,
            licence: "CC0-1.0".to_string(),
            watermark: None,
            unchanged: false,
            records: 1,
            new: 1,
            skipped: 0,
            pruned: 0,
        }
    }

    fn stats_with_source() -> RunStats {
        RunStats {
            envelopes: 3,
            per_source: vec![src_stat("a")],
        }
    }

    #[test]
    fn read_manifest_round_trips_a_verified_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("b", &[]), rec("c", &[])];
        let r = emit(
            tmp.path(),
            &recipe("t", 2),
            "# r\n",
            records.clone(),
            &stats_with_source(),
            &sugg(&records),
        )
        .unwrap();
        let meta = read_manifest(&r.dir).unwrap();
        assert_eq!(meta.name, "t");
        assert_eq!(meta.generation, 1);
        assert_eq!(meta.records, 3);
        let total: usize = meta.files.iter().map(|f| f.records).sum();
        assert_eq!(total, 3);
        assert_eq!(meta.sources.len(), 1);
        assert_eq!(meta.sources[0].slug, "a");
        assert_eq!(meta.sources[0].licence, "CC0-1.0");
    }

    #[test]
    fn read_manifest_names_the_tampered_file() {
        let tmp = tempfile::tempdir().unwrap();
        let r = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            vec![rec("a", &[])],
            &stats_with_source(),
            &sugg(&[rec("a", &[])]),
        )
        .unwrap();
        let fname = meta_file_name(&r.dir);
        let path = r.dir.join(&fname);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(b' ');
        std::fs::write(&path, bytes).unwrap();
        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(err.contains(&fname), "{err}");
    }

    #[test]
    fn read_manifest_refuses_an_unknown_format_version() {
        let tmp = tempfile::tempdir().unwrap();
        let r = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            vec![rec("a", &[])],
            &stats_with_source(),
            &sugg(&[rec("a", &[])]),
        )
        .unwrap();
        // checked BEFORE any checksum: a future format must be refused on the
        // version alone, not on a hash of a file we cannot interpret
        let m: Value =
            serde_json::from_str(&std::fs::read_to_string(r.dir.join("manifest.json")).unwrap())
                .unwrap();
        let mut m = m;
        m["format_version"] = json!(2);
        std::fs::write(
            r.dir.join("manifest.json"),
            serde_json::to_vec_pretty(&m).unwrap(),
        )
        .unwrap();
        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(err.contains("format version 2 is not supported"), "{err}");
    }

    #[test]
    fn read_manifest_refuses_a_truncated_sums() {
        let tmp = tempfile::tempdir().unwrap();
        let r = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            vec![rec("a", &[])],
            &stats_with_source(),
            &sugg(&[rec("a", &[])]),
        )
        .unwrap();
        let kept: String = std::fs::read_to_string(r.dir.join("SHA256SUMS"))
            .unwrap()
            .lines()
            .filter(|l| !l.ends_with("manifest.json"))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(r.dir.join("SHA256SUMS"), kept).unwrap();
        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(err.contains("does not cover manifest.json"), "{err}");
    }

    #[test]
    fn read_manifest_refuses_manifest_and_sums_disagreement() {
        // A manifest whose files[] sha disagrees with SHA256SUMS: caught
        // before any bytes are re-hashed, because the pack is internally
        // inconsistent whichever of the two is the tampered one. (The case
        // sums + manifest agree but the FILE differs is the tamper test
        // above; the case everything-agree-but-lying is what signatures —
        // a later milestone — exist for.)
        let tmp = tempfile::tempdir().unwrap();
        let r = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            vec![rec("a", &[])],
            &stats_with_source(),
            &sugg(&[rec("a", &[])]),
        )
        .unwrap();
        let fname = meta_file_name(&r.dir);
        let mut m: Value =
            serde_json::from_str(&std::fs::read_to_string(r.dir.join("manifest.json")).unwrap())
                .unwrap();
        m["files"][&fname]["sha256"] = json!("0".repeat(64));
        std::fs::write(
            r.dir.join("manifest.json"),
            serde_json::to_vec_pretty(&m).unwrap(),
        )
        .unwrap();
        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(err.contains("disagree"), "{err}");
        assert!(err.contains(&fname), "{err}");
    }

    /// The first records file the manifest names (shards are hash-assigned,
    /// so read the name instead of guessing it).
    fn meta_file_name(pack_dir: &Path) -> String {
        let m: Value =
            serde_json::from_str(&std::fs::read_to_string(pack_dir.join("manifest.json")).unwrap())
                .unwrap();
        m["files"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone()
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("a", &[])];
        let err = emit(
            tmp.path(),
            &recipe("t", 2),
            "",
            records.clone(),
            &stats(),
            &sugg(&records),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate emitted id"), "{err}");
    }

    #[test]
    fn empty_ids_are_refused_not_collapsed() {
        // #1110 gap 2: records whose id never resolved default to "" in
        // identity.rs; the old duplicate guard compared only non-empty ids,
        // so N of them shipped in one shard and collapsed to one _id at the
        // consumer. emit must now refuse them loudly.
        let tmp = tempfile::tempdir().unwrap();
        let idless = || {
            let mut m = Map::new();
            m.insert("id".into(), json!(""));
            m.insert("body".into(), json!("text"));
            m
        };
        let records = vec![idless(), idless()];
        let err = emit(
            tmp.path(),
            &recipe("t", 2),
            "",
            records.clone(),
            &stats(),
            &sugg(&records),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("empty id"), "{err}");

        // a single id-less record is refused for the same reason — it has no
        // stable _id even on its own.
        let one = vec![idless()];
        let err = emit(
            tmp.path(),
            &recipe("t", 2),
            "",
            one.clone(),
            &stats(),
            &sugg(&one),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("empty id"), "{err}");
    }

    #[test]
    fn read_manifest_catches_a_record_count_lie_with_valid_checksums() {
        // #1110 gap 3: every prior tamper test also broke a sha, so the
        // line-count check (the only defence against a SUMS recomputed over
        // the wrong bytes, or a torn final line) had never been reached. Here
        // every checksum is consistent and ONLY the manifest's per-file
        // `records` integer lies — the count check must still refuse.
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("b", &[]), rec("c", &[])];
        let r = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            records.clone(),
            &stats_with_source(),
            &sugg(&records),
        )
        .unwrap();
        let fname = meta_file_name(&r.dir);

        // flip the manifest's claimed record count for the shard, then make
        // EVERY checksum honest again (manifest sha in SHA256SUMS included),
        // so nothing but the count can be what the reader objects to.
        let mut m: Value =
            serde_json::from_str(&std::fs::read_to_string(r.dir.join("manifest.json")).unwrap())
                .unwrap();
        let real = m["files"][&fname]["records"].as_u64().unwrap();
        m["files"][&fname]["records"] = json!(real + 1);
        std::fs::write(
            r.dir.join("manifest.json"),
            serde_json::to_vec_pretty(&m).unwrap(),
        )
        .unwrap();
        // recompute SHA256SUMS from the bytes now on disk
        let sums_raw = std::fs::read_to_string(r.dir.join("SHA256SUMS")).unwrap();
        let mut fresh = String::new();
        for line in sums_raw.lines() {
            let (_, name) = line.split_once("  ").unwrap();
            let bytes = std::fs::read(r.dir.join(name)).unwrap();
            fresh.push_str(&format!("{}  {}\n", hex(&Sha256::digest(&bytes)), name));
        }
        std::fs::write(r.dir.join("SHA256SUMS"), fresh).unwrap();

        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(err.contains("manifest says"), "{err}");
        assert!(err.contains(&fname), "{err}");
    }

    /// Point `manifest.files[fname].sha256` at the bytes now on disk and
    /// rewrite SHA256SUMS over every name it already lists. The per-file
    /// `records` integer is left untouched, so a following `read_manifest`
    /// can fail only on the line-count check.
    fn recompute_sums_leaving_record_counts(pack_dir: &Path, fname: &str) {
        let bytes = std::fs::read(pack_dir.join(fname)).unwrap();
        let sha = hex(&Sha256::digest(&bytes));
        let mut m: Value =
            serde_json::from_str(&std::fs::read_to_string(pack_dir.join("manifest.json")).unwrap())
                .unwrap();
        m["files"][fname]["sha256"] = json!(sha);
        std::fs::write(
            pack_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&m).unwrap(),
        )
        .unwrap();
        let sums_raw = std::fs::read_to_string(pack_dir.join("SHA256SUMS")).unwrap();
        let mut fresh = String::new();
        for line in sums_raw.lines() {
            let (_, name) = line.split_once("  ").unwrap();
            let file_bytes = std::fs::read(pack_dir.join(name)).unwrap();
            fresh.push_str(&format!(
                "{}  {}\n",
                hex(&Sha256::digest(&file_bytes)),
                name
            ));
        }
        std::fs::write(pack_dir.join("SHA256SUMS"), fresh).unwrap();
    }

    #[test]
    fn read_manifest_count_check_refuses_a_torn_line_when_sums_match() {
        // #1110 gap 3, the byte-tamper shape: SHA256SUMS is recomputed over
        // the tampered records (and the manifest sha is updated to match), so
        // the checksum branches all pass. A torn final line that changes the
        // line count is then refused by the record-count check alone.
        // `read_manifest_catches_a_record_count_lie_with_valid_checksums`
        // reaches the same branch by lying about the integer; this one
        // tampers the file.
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("b", &[]), rec("c", &[])];
        let r = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            records.clone(),
            &stats_with_source(),
            &sugg(&records),
        )
        .unwrap();
        read_manifest(&r.dir).unwrap();
        let fname = meta_file_name(&r.dir);
        let path = r.dir.join(&fname);
        let original = std::fs::read(&path).unwrap();

        // appended torn final line: a partial JSON object with no closing
        // brace and no trailing newline. `.lines()` counts it, so the file
        // holds one more record than the manifest still claims.
        let mut torn = original.clone();
        torn.extend_from_slice(b"{\"id\":\"TORN\"");
        std::fs::write(&path, &torn).unwrap();
        recompute_sums_leaving_record_counts(&r.dir, &fname);
        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(
            err.contains("manifest says"),
            "count check must be the refusal, not a checksum: {err}"
        );
        assert!(
            !err.contains("checksum mismatch") && !err.contains("disagree"),
            "checksums were recomputed; they must not be why this fails: {err}"
        );
        assert!(err.contains("file holds"), "{err}");
        assert!(err.contains(&fname), "{err}");

        // truncated final record: the last complete line is cut away, as a
        // writer that died before flushing it. Count drops; checksums are
        // honest again; the count check still refuses.
        std::fs::write(&path, &original).unwrap();
        let text = String::from_utf8(original.clone()).unwrap();
        let trimmed = text.trim_end_matches('\n');
        let (head, last) = trimmed.rsplit_once('\n').unwrap();
        assert!(last.contains("\"id\""), "fixture has a final record line");
        std::fs::write(&path, format!("{head}\n")).unwrap();
        recompute_sums_leaving_record_counts(&r.dir, &fname);
        let err = read_manifest(&r.dir).unwrap_err().to_string();
        assert!(err.contains("manifest says"), "{err}");
        assert!(
            !err.contains("checksum mismatch") && !err.contains("disagree"),
            "{err}"
        );
        assert!(err.contains(&fname), "{err}");
    }

    #[test]
    fn sums_cover_every_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let records = vec![rec("a", &[]), rec("b", &[]), rec("c", &[])];
        let r = emit(
            tmp.path(),
            &recipe("t", 2),
            "# r\n",
            records.clone(),
            &stats(),
            &sugg(&records),
        )
        .unwrap();
        let sums = std::fs::read_to_string(r.dir.join("SHA256SUMS")).unwrap();
        assert!(sums.contains("  manifest.json\n"));
        assert!(sums.contains("  recipe.toml\n"));
        for line in sums.lines() {
            let (sum, name) = line.split_once("  ").unwrap();
            let bytes = std::fs::read(r.dir.join(name)).unwrap();
            assert_eq!(sum, hex(&Sha256::digest(&bytes)), "{name} hash matches");
        }
        let m: Value =
            serde_json::from_str(&std::fs::read_to_string(r.dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(m["format_version"], json!(1));
        assert_eq!(m["kind"], json!("harvested"));
        assert_eq!(m["counts"]["records"], json!(3));
        // shard assignment is a hash — do not assert WHICH files exist, only
        // that the ones that do cover every record and none is empty
        let files = m["files"].as_object().unwrap();
        assert!(!files.is_empty());
        assert!(files.len() <= 2);
        let total: usize = files
            .values()
            .map(|f| f["records"].as_u64().unwrap() as usize)
            .sum();
        assert_eq!(total, 3);
        for f in files.values() {
            assert!(
                f["records"].as_u64().unwrap() > 0,
                "empty shard files are never written"
            );
        }
    }

    #[test]
    fn rebuild_with_fewer_shards_removes_orphan_records_files() {
        // #1110 gap 5a: when a rebuild populates fewer (or different) shard
        // buckets than the previous generation, stale `records-*.jsonl` files
        // must be swept before writing new shards so they never survive on
        // disk outside `manifest.json` and `SHA256SUMS`.
        let tmp = tempfile::tempdir().unwrap();
        let many: Vec<Map<String, Value>> = (0..32).map(|i| rec(&format!("id-{i}"), &[])).collect();
        let r1 = emit(
            tmp.path(),
            &recipe("t", 16),
            "# r\n",
            many.clone(),
            &stats_with_source(),
            &sugg(&many),
        )
        .unwrap();
        let gen1_shards: Vec<String> = std::fs::read_dir(&r1.dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("records-") && n.ends_with(".jsonl"))
            .collect();
        assert!(gen1_shards.len() > 1, "multiple shard files populated");

        let one = vec![rec("id-0", &[])];
        let r2 = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            one.clone(),
            &stats_with_source(),
            &sugg(&one),
        )
        .unwrap();
        assert_eq!(r2.generation, 2);
        let gen2_shards: Vec<String> = std::fs::read_dir(&r2.dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("records-") && n.ends_with(".jsonl"))
            .collect();
        assert_eq!(gen2_shards, vec!["records-00000.jsonl".to_string()]);
    }

    #[test]
    fn emit_refuses_corrupt_previous_manifest_instead_of_resetting_generation() {
        // #1110 gap 5b: a truncated or generation-less existing `manifest.json`
        // must fail the rebuild loudly, not silently reset `generation` to 1.
        let tmp = tempfile::tempdir().unwrap();
        let one = vec![rec("a", &[])];
        let r1 = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            one.clone(),
            &stats_with_source(),
            &sugg(&one),
        )
        .unwrap();
        assert_eq!(r1.generation, 1);

        std::fs::write(r1.dir.join("manifest.json"), "{truncated").unwrap();
        let err = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            one.clone(),
            &stats_with_source(),
            &sugg(&one),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("corrupt previous manifest"), "{err}");

        std::fs::write(r1.dir.join("manifest.json"), r#"{"format_version":1}"#).unwrap();
        let err = emit(
            tmp.path(),
            &recipe("t", 1),
            "# r\n",
            one.clone(),
            &stats_with_source(),
            &sugg(&one),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing or non-integer 'generation'"), "{err}");
    }
}
