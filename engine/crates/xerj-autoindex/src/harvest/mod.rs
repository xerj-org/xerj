//! `xerj corpus build` — build a portable corpus pack from harvested records.
//!
//! The pipeline, end to end:
//!
//! ```text
//! recipe.toml ─→ harvest (dir walk / M2: http-zip, git)
//!             ─→ normalize (format adapter: osv | flat)
//!             ─→ store     (content-addressed, presence = dedup)
//!             ─→ identity  (union-find over declared edge values)
//!             ─→ merge     (precedence + array union + strictest licence)
//!             ─→ suggest   (mapping + join-key hints from a sample — never applied)
//!             ─→ pack      (deterministic sharded JSONL + manifest + SUMS)
//!
//! Publishing is a separate step, never part of a build:
//! `xerj corpus sign <pack-dir> --key <seed-file>` writes the detached
//! ed25519 signature over SHA256SUMS (see `sign.rs`); `xerj corpus add
//! --from <pack> --verify-sig <pubkey-file>` checks it on the way in.
//! ```
//!
//! Incrementality is not a mode, it is the storage layout: every normalized
//! record lands at `store/<slug>.<xxh3-of-canonical-json>.json`, so a re-run
//! recomputes the key, sees the file, and skips. Deleting the source tree's
//! unchanged files and re-running is a no-op beyond the walk; the pack is
//! then rebuilt from the store deterministically.
//!
//! The tool is domain-agnostic by directive: what a "record" is, which
//! fields are identity, and how fields merge all live in the recipe, never
//! here. Licence terms ride along as record data (`licence`, per-source
//! `sources[]`); the tool does not and must not enforce them — the pack
//! author owns what goes in the pack.

pub mod httpget;
pub mod identity;
pub mod normalize;
pub mod pack;
pub mod recipe;
pub mod sign;
pub mod source;
pub mod store;
pub mod suggest;

pub(crate) use store::MAX_FIELDS;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use crate::xc::code_root;
use pack::{RunStats, SourceStat};
use recipe::{SourceKind, LICENCE, SRC, SRC_PATH, SRC_URL};

const BUILD_USAGE: &str = "usage: xerj corpus build <name> [--recipe <path>] [--fresh]

build (or incrementally refresh) a harvested corpus pack:
  1. load the recipe (default $XERJ_CODE_HOME/recipes/<name>.toml)
  2. harvest every declared source into the content store
     (already-stored records are skipped — that is the point)
  3. resolve identity, merge duplicates, emit a deterministic pack
     under $XERJ_CODE_HOME/builds/<name>/pack/<name>/

--fresh discards the build state first (full re-harvest).";

/// Entry point from the corpus dispatcher. 0 = pack built, 1 = failure,
/// 2 = usage error.
pub fn run_build(args: &[String]) -> i32 {
    match run_build_inner(args, &code_root()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xerj corpus build: {e:#}");
            1
        }
    }
}

fn run_build_inner(args: &[String], home: &Path) -> Result<i32> {
    let mut name: Option<String> = None;
    let mut recipe_path: Option<PathBuf> = None;
    let mut fresh = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--recipe" => {
                recipe_path = Some(PathBuf::from(it.next().context("--recipe needs a path")?));
            }
            "--fresh" => fresh = true,
            "-h" | "--help" | "help" => {
                println!("{BUILD_USAGE}");
                return Ok(0);
            }
            s if s.starts_with('-') => bail!("unknown flag '{s}'\n\n{BUILD_USAGE}"),
            s => {
                if name.replace(s.to_string()).is_some() {
                    bail!("unexpected second name '{s}'\n\n{BUILD_USAGE}");
                }
            }
        }
    }
    let Some(name) = name else {
        eprintln!("{BUILD_USAGE}");
        return Ok(2);
    };

    let recipe_file = match recipe_path {
        Some(p) => p,
        None => home.join("recipes").join(format!("{name}.toml")),
    };
    if !recipe_file.is_file() {
        bail!(
            "no recipe for '{name}' at {} (pass --recipe <path>, or create it there)",
            recipe_file.display()
        );
    }
    let recipe = recipe::load(&recipe_file)?;
    if recipe.name != name {
        bail!(
            "recipe at {} builds corpus '{}', not '{}' — the name must match",
            recipe_file.display(),
            recipe.name,
            name
        );
    }
    let recipe_dir = recipe_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let recipe_toml = std::fs::read_to_string(&recipe_file)?;

    let build_dir = home.join("builds").join(&recipe.name);
    if fresh && build_dir.exists() {
        std::fs::remove_dir_all(&build_dir)
            .with_context(|| format!("--fresh: cannot clear {}", build_dir.display()))?;
    }
    let store_dir = build_dir.join("store");
    let cache_dir = build_dir.join("cache");

    // ── harvest into the content store ───────────────────────────────────
    let mut per_source = Vec::new();
    for src in &recipe.sources {
        let fetched = source::fetch(src, &recipe_dir, &cache_dir)?;
        let mut stat = SourceStat {
            slug: src.slug.clone(),
            kind: kind_name(src.kind).to_string(),
            path: src.path.clone(),
            url: src.url.clone(),
            licence: src.licence.clone(),
            watermark: fetched.watermark.clone(),
            unchanged: fetched.unchanged,
            records: 0,
            new: 0,
            skipped: 0,
            pruned: 0,
        };
        if fetched.unchanged {
            // The whole upstream tree is byte-identical to the previous run:
            // skip the walk AND the prune — every envelope this source ever
            // stored is still live, and pruning needs the full key set.
            per_source.push(stat);
            continue;
        }
        let files = walk(&fetched.root, &src.glob)?;
        let mut keys = HashSet::new();
        for (rel, path) in files {
            for value in parse_file(&path, src.records_field.as_deref())? {
                let mut env = normalize::normalize(src.format, &value);
                env.insert(SRC.into(), Value::String(src.slug.clone()));
                env.insert(
                    SRC_URL.into(),
                    Value::String(src.url.clone().unwrap_or_default()),
                );
                env.insert(SRC_PATH.into(), Value::String(rel.clone()));
                env.insert(LICENCE.into(), Value::String(src.licence.clone()));
                let (key, was_new) = store::put(&store_dir, &src.slug, &env)?;
                keys.insert(key);
                stat.records += 1;
                if was_new {
                    stat.new += 1;
                } else {
                    stat.skipped += 1;
                }
            }
        }
        stat.pruned = store::prune(&store_dir, &src.slug, &keys);
        // persist the watermark only after the source's harvest fully
        // succeeded — a failed run must re-walk
        if let Some(wm) = &fetched.watermark {
            source::save_watermark(&cache_dir, &src.slug, wm)?;
        }
        per_source.push(stat);
    }

    // ── identity + merge over the FULL store (not just this run's files) ──
    let slugs: HashSet<String> = recipe.sources.iter().map(|s| s.slug.clone()).collect();
    let envelopes = store::read_all(&store_dir, &slugs)?;
    let clusters = identity::cluster(&envelopes, &recipe.identity);
    let merge_order = if recipe.merge.precedence.is_empty() {
        recipe.identity.canonical_source_order.clone()
    } else {
        recipe.merge.precedence.clone()
    };
    let mut records = Vec::with_capacity(clusters.len());
    for c in &clusters {
        let members: Vec<&Map<String, Value>> = c.members.iter().map(|&i| &envelopes[i]).collect();
        records.push(identity::merge_cluster(
            &members,
            &merge_order,
            &recipe.envelope,
            &recipe.merge,
            &recipe.derived,
        ));
    }

    // ── emit ──────────────────────────────────────────────────────────────
    let stats = RunStats {
        envelopes: envelopes.len(),
        per_source,
    };
    // suggestions from a deterministic sample — the recipe author's feedback
    // loop, never applied to the records or the index
    let suggestions = suggest::analyze(&records, recipe.suggest.sample);
    let result = pack::emit(
        &build_dir,
        &recipe,
        &recipe_toml,
        records,
        &stats,
        &suggestions,
    )?;

    // run report: the machine-readable twin of the stdout summary, with the
    // watermarks a scheduled rebuild (M6) will diff against
    let report = serde_json::json!({
        "finished_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "pack": recipe.name,
        "generation": result.generation,
        "records": result.records,
        "envelopes": stats.envelopes,
        "sources": stats.per_source.iter()
            .map(|s| serde_json::to_value(s).expect("SourceStat serializes"))
            .collect::<Vec<_>>(),
    });
    std::fs::write(
        build_dir.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )
    .with_context(|| format!("write {}", build_dir.join("report.json").display()))?;

    println!(
        "built '{}' generation {}: {} envelopes → {} records, {} shard file(s)",
        recipe.name, result.generation, stats.envelopes, result.records, result.shards
    );
    for s in &stats.per_source {
        if s.unchanged {
            println!(
                "  {}: unchanged (watermark {})",
                s.slug,
                s.watermark.clone().unwrap_or_default()
            );
            continue;
        }
        println!(
            "  {}: {} record(s) — {} new, {} skipped, {} pruned ({})",
            s.slug, s.records, s.new, s.skipped, s.pruned, s.licence
        );
    }
    println!("pack: {}", result.dir.display());
    println!(
        "  suggestions: {} field(s), {} relation(s) — see {}",
        suggestions.fields.len(),
        suggestions.relations.len(),
        result.dir.join("suggestions.md").display()
    );
    Ok(0)
}

fn kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Dir => "dir",
        SourceKind::HttpZip => "http-zip",
        SourceKind::HttpJson => "http-json",
        SourceKind::Git => "git",
    }
}

// ── dir source ──────────────────────────────────────────────────────────────

/// Recursively list files under `root` matching `glob`, as
/// (posix-relative-path, absolute-path), sorted — directory iteration order
/// must never reach the output.
fn walk(root: &Path, glob: &str) -> Result<Vec<(String, PathBuf)>> {
    // A source root that cannot be read is a HARD error, never an empty
    // walk. A `dir` source whose path is typo'd, points at the wrong cwd, or
    // names a tree that was renamed upstream would otherwise walk to zero
    // files; `prune` then deletes every envelope the slug ever stored and
    // the build exits 0 with a smaller, all-checksums-valid pack — silent
    // data loss (#1110 negative-path audit, gap 1). An existing but empty
    // directory is still allowed: a legitimately empty upstream prunes to
    // zero on purpose, a missing one does not.
    let meta = std::fs::metadata(root).with_context(|| {
        format!(
            "source root {} cannot be read — a typo'd path or a moved upstream \
             tree must fail the build, not silently empty the pack",
            root.display()
        )
    })?;
    if !meta.is_dir() {
        bail!("source root {} is not a directory", root.display());
    }
    let re = glob_regex(glob);
    let mut out = Vec::new();
    // A listing error must not become "no files". The autoindex walker
    // skips an unreadable entry (`walk.rs:327`, the `Err` arm of `it.next()`)
    // because a skipped file is only absent from that pass. Here `prune`
    // treats "not seen" as "deleted upstream", so swallowing `read_dir` —
    // on the root or on any nested directory — deletes every envelope under
    // it and the build still exits 0 (#1110 gap 1). `metadata` above already
    // refuses a missing root; this is the root that exists but cannot be
    // listed, and the same failure one directory down.
    fn recurse(
        dir: &Path,
        root: &Path,
        re: &regex::Regex,
        out: &mut Vec<(String, PathBuf)>,
    ) -> Result<()> {
        let entries = std::fs::read_dir(dir).with_context(|| {
            format!(
                "source directory {} cannot be listed — an unreadable tree must \
                 fail the build, not silently empty the pack",
                dir.display()
            )
        })?;
        for e in entries {
            let e = e.with_context(|| format!("cannot read entry in {}", dir.display()))?;
            let p = e.path();
            // `Path::is_dir` follows symlinks, which is the walk this
            // replaced. A stat failure reports "not a directory" (same as
            // before); the failure this function refuses is `read_dir`.
            if p.is_dir() {
                recurse(&p, root, re, out)?;
            } else {
                let rel = p.strip_prefix(root).unwrap_or(&p);
                let rel = rel.to_string_lossy().replace('\\', "/");
                if re.is_match(&rel) {
                    out.push((rel, p));
                }
            }
        }
        Ok(())
    }
    recurse(root, root, &re, &mut out)?;
    out.sort();
    Ok(out)
}

/// Minimal glob → regex: `**` spans directories (`**/` also matches zero
/// directories), `*` stays inside one segment, `?` is one char. Recipes
/// need path matching, not the full glob grammar; if a recipe ever needs
/// character classes we take a real glob crate rather than growing this.
fn glob_regex(glob: &str) -> regex::Regex {
    let mut re = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        re.push_str("(.*/)?");
                    } else {
                        re.push_str(".*");
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            other => re.push_str(&regex::escape(&other.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).expect("glob translation always yields a valid regex")
}

/// Parse one harvested file: `.json`/`.jsonl`/`.ndjson`/`.md`. A `.json`
/// array yields its elements; `.jsonl` yields one per non-empty line; a
/// `.md` yields its raw text as ONE string value — the format adapter (e.g.
/// `rustsec-md`) owns interpreting it, so an `.md` globbed by a json-format
/// source is visible junk, not a silent parse error. Other extensions are
/// skipped by the glob, not here — reaching this with an unlisted type is a
/// hard error, not a silent drop.
fn parse_file(path: &Path, records_field: Option<&str>) -> Result<Vec<Value>> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let markdown = name.ends_with(".md") || name.ends_with(".markdown");
    let jsonl = name.ends_with(".jsonl") || name.ends_with(".ndjson");
    // `records_field` only means something for a wrapped `.json` catalog; on
    // any other file type it would be silently ignored, so refuse instead.
    if let (Some(field), true) = (records_field, markdown || jsonl) {
        bail!(
            "{}: records_field '{field}' applies only to .json files, not .md/.jsonl",
            path.display()
        );
    }
    if markdown {
        return Ok(vec![Value::String(raw)]);
    }
    if jsonl {
        let mut out = Vec::new();
        for (n, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(line)
                .with_context(|| format!("{}:{}: not valid JSON", path.display(), n + 1))?;
            out.push(v);
        }
        Ok(out)
    } else {
        let v: Value = serde_json::from_str(&raw)
            .with_context(|| format!("{}: not valid JSON", path.display()))?;
        if let Some(field) = records_field {
            let Value::Object(mut m) = v else {
                bail!(
                    "{}: records_field '{field}' requires a top-level JSON object",
                    path.display()
                );
            };
            let Some(Value::Array(a)) = m.remove(field) else {
                bail!(
                    "{}: records_field '{field}' missing or not a JSON array",
                    path.display()
                );
            };
            return Ok(a);
        }
        Ok(match v {
            Value::Array(a) => a,
            other => vec![other],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn glob_matches_paths_like_a_recipe_expects() {
        let re = glob_regex("**/*.json");
        assert!(re.is_match("a.json"));
        assert!(re.is_match("deep/nested/a.json"));
        assert!(!re.is_match("a.jsonl"));
        assert!(!re.is_match("dir/sub")); // '.json' must be a suffix

        let re = glob_regex("osv/*.json");
        assert!(re.is_match("osv/x.json"));
        assert!(!re.is_match("osv/nested/x.json"), "* does not cross '/'");
        assert!(!re.is_match("other/x.json"));

        let re = glob_regex("**/GHSA-????.json");
        assert!(re.is_match("GHSA-1234.json"));
        assert!(!re.is_match("GHSA-12345.json"));
    }

    #[test]
    fn walk_is_sorted_and_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("src");
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("b/second.json"), "{}").unwrap();
        std::fs::write(root.join("a/first.json"), "{}").unwrap();
        std::fs::write(root.join("a/note.txt"), "x").unwrap();
        let files = walk(&root, "**/*.json").unwrap();
        let rels: Vec<&str> = files.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(rels, vec!["a/first.json", "b/second.json"]);
    }

    #[test]
    fn walk_refuses_a_missing_root_instead_of_pruning_everything() {
        // #1110 gap 1: a missing source root must fail the build, not walk to
        // zero files and let `prune` wipe the slug with a green exit.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let err = walk(&missing, "**/*.json").unwrap_err().to_string();
        assert!(err.contains("cannot be read"), "{err}");

        // an existing but empty directory is still fine — a legitimately
        // empty upstream prunes to zero on purpose.
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(walk(&empty, "**/*.json").unwrap().is_empty());

        // a file where a directory is expected is also refused.
        let afile = tmp.path().join("a.json");
        std::fs::write(&afile, "{}").unwrap();
        let err = walk(&afile, "**/*.json").unwrap_err().to_string();
        assert!(err.contains("is not a directory"), "{err}");
    }

    /// `chmod` a path for the duration of the test, then put the previous
    /// mode back — including on panic — so the tempdir can be removed.
    #[cfg(unix)]
    struct RestoreMode {
        path: PathBuf,
        mode: u32,
    }

    #[cfg(unix)]
    impl RestoreMode {
        fn lock(path: &Path, mode: u32) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let prev = std::fs::metadata(path).unwrap().permissions().mode();
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(path, perms).unwrap();
            RestoreMode {
                path: path.to_path_buf(),
                mode: prev,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&self.path) {
                let mut perms = meta.permissions();
                perms.set_mode(self.mode);
                std::fs::set_permissions(&self.path, perms).ok();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn walk_refuses_an_unreadable_root() {
        // #1110 gap 1, the read_dir half: metadata succeeding is not enough.
        // A directory that exists but cannot be listed (mode 000) used to
        // fall through `recurse`'s swallowed `read_dir` and come back as an
        // empty file list — the same silent prune as a typo'd path.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("src");
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::write(root.join("keep/a.json"), "{}").unwrap();
        let _restore = RestoreMode::lock(&root, 0o000);
        let err = walk(&root, "**/*.json").unwrap_err().to_string();
        assert!(
            err.contains("cannot be listed"),
            "unreadable root must fail the walk, not return no files: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn walk_refuses_an_unreadable_subdirectory() {
        // A nested directory that cannot be listed must fail the walk too.
        // Skipping it would omit its files from the live key set and `prune`
        // would delete their envelopes as if upstream had removed them.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("src");
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("keep/a.json"), "{}").unwrap();
        std::fs::write(root.join("secret/b.json"), "{}").unwrap();
        let _restore = RestoreMode::lock(&root.join("secret"), 0o000);
        let err = walk(&root, "**/*.json").unwrap_err().to_string();
        assert!(
            err.contains("cannot be listed"),
            "unreadable subdirectory must fail the walk: {err}"
        );
        assert!(
            err.contains("secret"),
            "the error must name the directory that could not be listed: {err}"
        );
    }

    #[test]
    fn parse_file_understands_json_and_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.json");
        std::fs::write(&a, r#"[{"id":"x"},{"id":"y"}]"#).unwrap();
        assert_eq!(parse_file(&a, None).unwrap().len(), 2);
        let b = tmp.path().join("b.jsonl");
        std::fs::write(&b, "{\"id\":\"z\"}\n\n{\"id\":\"w\"}\n").unwrap();
        assert_eq!(
            parse_file(&b, None).unwrap().len(),
            2,
            "blank lines skipped"
        );
        let c = tmp.path().join("c.json");
        std::fs::write(&c, r#"{"id":"one"}"#).unwrap();
        assert_eq!(parse_file(&c, None).unwrap().len(), 1);
        let d = tmp.path().join("d.md");
        std::fs::write(&d, "```toml\n[advisory]\nid = \"R\"\n```\n\n# T\n").unwrap();
        let parsed = parse_file(&d, None).unwrap();
        assert_eq!(parsed.len(), 1, "one raw string value, not json-parsed");
        assert!(parsed[0].as_str().unwrap().starts_with("```toml"));
        let wrapped = tmp.path().join("wrapped.json");
        std::fs::write(
            &wrapped,
            r#"{"title":"KEV","vulnerabilities":[{"cveID":"CVE-1"},{"cveID":"CVE-2"}]}"#,
        )
        .unwrap();
        assert_eq!(
            parse_file(&wrapped, Some("vulnerabilities")).unwrap().len(),
            2
        );
        let err = parse_file(&wrapped, Some("missing"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing or not a JSON array"), "{err}");
        let err = parse_file(&b, Some("vulnerabilities"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("applies only to .json files"), "{err}");
        let err = parse_file(&d, Some("vulnerabilities"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("applies only to .json files"), "{err}");
    }

    // ── end to end: recipe → harvest → identity → pack, twice ─────────────

    const E2E_RECIPE: &str = r#"
[recipe]
format = 1
name = "e2e"
description = "two sources, one alias-linked record"

[[sources]]
slug = "rustsec"
kind = "dir"
path = "data/rustsec"
format = "osv"
licence = "CC0-1.0"

[[sources]]
slug = "ghsa"
kind = "dir"
path = "data/ghsa"
glob = "**/*.jsonl"
format = "osv"
licence = "CC-BY-4.0"

[envelope]
id_from = ["id"]
title_from = ["summary", "id"]
body_join = ["summary", "details"]
defs_from = ["affected_functions"]

[identity]
edges = [{ field = "id" }, { field = "aliases", each = true }]
canonical_source_order = ["rustsec", "ghsa"]

[merge]
precedence = ["rustsec", "ghsa"]

[[derived]]
name = "cve_ids"
op = "regex_extract"
from = ["id", "aliases"]
pattern = '(?i)\bCVE-\d{4}-\d{4,}\b'
unique = true

[emit]
shards = 4
"#;

    fn e2e_env(home: &Path) {
        let root = home.join("recipes");
        std::fs::create_dir_all(root.join("data/rustsec")).unwrap();
        std::fs::create_dir_all(root.join("data/ghsa")).unwrap();
        std::fs::write(root.join("e2e.toml"), E2E_RECIPE).unwrap();
        std::fs::write(
            root.join("data/rustsec/RUSTSEC-2021-0003.json"),
            r#"{
                "id": "RUSTSEC-2021-0003",
                "aliases": ["CVE-2021-25900"],
                "summary": "Out of bounds write in SmallVec::insert_many",
                "details": "An issue was discovered that allows attackers...",
                "modified": "2023-06-20T00:00:00Z",
                "affected": [{
                    "package": {"ecosystem": "crates.io", "name": "smallvec"},
                    "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}, {"fixed": "0.6.14"}]}]
                }],
                "ecosystem_specific": {"affects": {"functions": {"smallvec::SmallVec::insert_many": ["<0.6.14"]}}}
            }"#,
        )
        .unwrap();
        // second rustsec record with NO alias link: its own cluster
        std::fs::write(
            root.join("data/rustsec/RUSTSEC-2020-0001.json"),
            r#"{"id": "RUSTSEC-2020-0001", "summary": "other issue"}"#,
        )
        .unwrap();
        // same logical record, GHSA shape: split ranges, no functions
        std::fs::write(
            root.join("data/ghsa/reviewed.jsonl"),
            concat!(
                r#"{"id": "GHSA-43w2-9j62-hq99", "aliases": ["CVE-2021-25900"], "summary": "Out of bounds write in insert_many", "modified": "2025-01-02T00:00:00Z", "affected": [{"package": {"ecosystem": "crates.io", "name": "smallvec"}, "ranges": [{"type": "SEMVER", "events": [{"introduced": "1.0.0"}, {"fixed": "1.6.1"}]}]}]}"#,
                "\n",
            ),
        )
        .unwrap();
    }

    fn read_records(pack: &Path) -> Vec<Map<String, Value>> {
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        let mut out = Vec::new();
        for name in manifest["files"].as_object().unwrap().keys() {
            for line in std::fs::read_to_string(pack.join(name)).unwrap().lines() {
                out.push(serde_json::from_str(line).unwrap());
            }
        }
        out.sort_by_key(|r: &Map<String, Value>| {
            r.get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        });
        out
    }

    #[test]
    fn build_end_to_end_and_incrementally() {
        let home = tempfile::tempdir().unwrap();
        e2e_env(home.path());

        let args: Vec<String> = vec!["e2e".to_string()];
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);

        let pack = home.path().join("builds/e2e/pack/e2e");
        let records = read_records(&pack);
        assert_eq!(
            records.len(),
            2,
            "GHSA+RUSTSEC merge via CVE alias; RUSTSEC-2020-0001 alone"
        );

        let merged = records
            .iter()
            .find(|r| r["id"] == json!("RUSTSEC-2021-0003"))
            .expect("precedence-first source names the id");
        assert_eq!(merged["sources"], json!(["rustsec", "ghsa"]));
        assert_eq!(merged["source"], json!("rustsec"));
        assert_eq!(
            merged["title"],
            json!("Out of bounds write in SmallVec::insert_many")
        );
        assert_eq!(
            merged["defs"],
            json!("smallvec::SmallVec::insert_many"),
            "rustsec-only field still feeds xerj code"
        );
        assert_eq!(
            merged["fixed_versions"],
            json!(["0.6.14", "1.6.1"]),
            "split GHSA range unions with the packed RustSec one"
        );
        assert_eq!(
            merged["modified"],
            json!("2025-01-02T00:00:00Z"),
            "max across members"
        );
        assert_eq!(
            merged["licence"],
            json!("CC0-1.0"),
            "neither is restricted; precedence first"
        );
        assert_eq!(
            merged["cve_ids"],
            json!(["CVE-2021-25900"]),
            "derived over the merged record"
        );

        // uniform keys: the GHSA-less record still carries every key
        let lone = records
            .iter()
            .find(|r| r["id"] == json!("RUSTSEC-2020-0001"))
            .unwrap();
        assert_eq!(lone["sources"], json!(["rustsec"]));
        assert_eq!(lone["fixed_versions"], json!([]), "array key fills empty");
        assert_eq!(lone["cve_ids"], json!([]));
        assert_eq!(lone["affected_functions"], json!([]));
        assert_eq!(lone["modified"], json!(""));

        // manifest structure + checksums
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["format_version"], json!(1));
        assert_eq!(manifest["kind"], json!("harvested"));
        assert_eq!(manifest["generation"], json!(1));
        assert_eq!(manifest["counts"]["records"], json!(2));
        assert_eq!(
            manifest["description"],
            json!("two sources, one alias-linked record")
        );
        let rs = &manifest["sources"][0];
        assert_eq!(rs["slug"], json!("rustsec"));
        assert_eq!(rs["new"], json!(2));
        assert_eq!(rs["skipped"], json!(0));
        assert!(manifest["recipe_sha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64));
        assert!(pack.join("recipe.toml").is_file(), "recipe ships verbatim");

        // suggestion artifacts: shipped, checksum-covered, and honest about
        // the alias pair this recipe encodes
        let sums = std::fs::read_to_string(pack.join("SHA256SUMS")).unwrap();
        for f in [
            "mapping.suggested.json",
            "relations.jsonl",
            "suggestions.md",
        ] {
            assert!(pack.join(f).is_file(), "{f} ships");
            assert!(sums.contains(&format!("  {f}\n")), "{f} checksum-covered");
        }
        let mapping: Value = serde_json::from_str(
            &std::fs::read_to_string(pack.join("mapping.suggested.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mapping["total_records"], json!(2));
        assert_eq!(mapping["sampled_records"], json!(2));
        assert!(mapping["es_mapping"]["properties"]["id"]["type"].is_string());
        let rels: Vec<Value> = std::fs::read_to_string(pack.join("relations.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let alias = rels
            .iter()
            .find(|r| {
                (r["left"] == json!("aliases") && r["right"] == json!("cve_ids"))
                    || (r["left"] == json!("cve_ids") && r["right"] == json!("aliases"))
            })
            .expect("aliases ↔ cve_ids discovered from the data itself");
        assert_eq!(alias["grammar"], json!("cve"));
        assert_eq!(alias["cardinality"], json!("many-to-many"));
        assert!(alias["overlap"]
            .as_array()
            .is_some_and(|o| o.contains(&json!("CVE-2021-25900"))));
        let md = std::fs::read_to_string(pack.join("suggestions.md")).unwrap();
        assert!(md.contains("Suggestions, not decisions"), "{md}");

        // ── second run: nothing new, shards byte-identical, generation up ──
        let before: Vec<(String, Vec<u8>)> = std::fs::read_dir(&pack)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().to_string(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .filter(|(n, _)| {
                n.starts_with("records-")
                    || n == "mapping.suggested.json"
                    || n == "relations.jsonl"
                    || n == "suggestions.md"
            })
            .collect();
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest["generation"],
            json!(2),
            "rebuild advances generation"
        );
        for s in manifest["sources"].as_array().unwrap() {
            assert_eq!(
                s["new"],
                json!(0),
                "content-addressed store skipped everything"
            );
            assert_eq!(s["skipped"], json!(s["records"]));
            assert_eq!(s["pruned"], json!(0));
        }
        for (name, bytes) in before {
            assert_eq!(
                std::fs::read(pack.join(&name)).unwrap(),
                bytes,
                "{name} byte-identical"
            );
        }

        // ── third run after deleting an upstream file: prune removes it ────
        std::fs::remove_file(
            home.path()
                .join("recipes/data/rustsec/RUSTSEC-2020-0001.json"),
        )
        .unwrap();
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest["sources"][0]["pruned"],
            json!(1),
            "dead envelope pruned"
        );
        assert_eq!(read_records(&pack).len(), 1);

        // ── fourth run with --fresh: wipes build dir, resets generation ────
        let fresh_args: Vec<String> = vec!["e2e".to_string(), "--fresh".to_string()];
        assert_eq!(run_build_inner(&fresh_args, home.path()).unwrap(), 0);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest["generation"],
            json!(1),
            "--fresh wipes previous build dir and resets generation to 1"
        );
        for s in manifest["sources"].as_array().unwrap() {
            assert_eq!(s["skipped"], json!(0), "--fresh re-harvests from scratch");
            assert_eq!(s["new"], s["records"]);
        }
    }

    fn store_files(home: &Path, build: &str, prefix: &str) -> Vec<String> {
        let store = home.join("builds").join(build).join("store");
        let mut names: Vec<String> = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(prefix))
            .collect();
        names.sort();
        names
    }

    fn pack_generation(home: &Path, build: &str) -> u64 {
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(
                home.join("builds")
                    .join(build)
                    .join("pack")
                    .join(build)
                    .join("manifest.json"),
            )
            .unwrap(),
        )
        .unwrap();
        manifest["generation"].as_u64().unwrap()
    }

    #[test]
    fn missing_source_root_fails_the_build_and_does_not_prune() {
        // #1110 gap 1 at the pipeline: a typo'd `dir` path must exit as an
        // error AND leave the previously stored envelopes on disk. `walk`
        // failing is not enough if `prune` already ran.
        let home = tempfile::tempdir().unwrap();
        e2e_env(home.path());
        let args: Vec<String> = vec!["e2e".to_string()];
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let before = store_files(home.path(), "e2e", "rustsec.");
        assert_eq!(before.len(), 2, "two rustsec envelopes stored");
        let gen = pack_generation(home.path(), "e2e");

        std::fs::rename(
            home.path().join("recipes/data/rustsec"),
            home.path().join("recipes/data/rustsec-moved"),
        )
        .unwrap();
        let err = run_build_inner(&args, home.path()).unwrap_err().to_string();
        assert!(
            err.contains("cannot be read"),
            "missing source root must fail the build: {err}"
        );
        assert_eq!(
            store_files(home.path(), "e2e", "rustsec."),
            before,
            "a failed walk must not prune the slug's envelopes"
        );
        assert_eq!(
            pack_generation(home.path(), "e2e"),
            gen,
            "a failed walk must not publish a new generation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_source_root_fails_the_build_and_does_not_prune() {
        // Same contract as the missing-root test, for a directory that
        // exists but whose `read_dir` fails. That is the path `recurse`
        // used to swallow.
        let home = tempfile::tempdir().unwrap();
        e2e_env(home.path());
        let args: Vec<String> = vec!["e2e".to_string()];
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let before = store_files(home.path(), "e2e", "rustsec.");
        assert_eq!(before.len(), 2);
        let gen = pack_generation(home.path(), "e2e");

        let root = home.path().join("recipes/data/rustsec");
        let _restore = RestoreMode::lock(&root, 0o000);
        let err = run_build_inner(&args, home.path()).unwrap_err().to_string();
        assert!(
            err.contains("cannot be listed"),
            "unreadable source root must fail the build: {err}"
        );
        assert_eq!(
            store_files(home.path(), "e2e", "rustsec."),
            before,
            "an unreadable root must not prune the slug's envelopes"
        );
        assert_eq!(pack_generation(home.path(), "e2e"), gen);
    }

    fn idless_recipe(name: &str, glob: &str) -> String {
        format!(
            r#"[recipe]
format = 1
name = "{name}"

[[sources]]
slug = "notes"
kind = "dir"
path = "notes"
glob = "{glob}"
format = "flat"
licence = "CC0-1.0"

[identity]
edges = [{{ field = "id" }}]
"#
        )
    }

    #[test]
    fn markdown_blobs_without_ids_are_refused() {
        // #1110 gap 2, the normalize path: `.md` files flattened by `flat`
        // become `{"value": …}` with no id. N of them must not ship in one
        // shard under id "".
        let home = tempfile::tempdir().unwrap();
        let notes = home.path().join("recipes/notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(
            home.path().join("recipes/idless.toml"),
            idless_recipe("idless", "**/*.md"),
        )
        .unwrap();
        for (i, body) in ["alpha", "bravo", "charlie", "delta"].iter().enumerate() {
            std::fs::write(notes.join(format!("n{i}.md")), format!("# {body}\n")).unwrap();
        }
        let err = run_build_inner(&["idless".into()], home.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty id"), "{err}");
        assert_eq!(
            store_files(home.path(), "idless", "notes.").len(),
            4,
            "all four blobs were harvested; the refusal is at emit, not a silent drop"
        );
        let pack = home.path().join("builds/idless/pack/idless");
        assert!(pack.is_dir(), "emit creates the pack dir before refusing");
        let shards: Vec<_> = std::fs::read_dir(&pack)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("records-"))
            .collect();
        assert!(
            shards.is_empty(),
            "refused records must not be written into the pack: {shards:?}"
        );
    }

    #[test]
    fn non_string_and_empty_ids_are_refused() {
        // #1110 gap 2: a non-string `id` (number, null, missing) is not a
        // stable identity. N such records must be refused, not collapsed.
        let home = tempfile::tempdir().unwrap();
        let notes = home.path().join("recipes/notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(
            home.path().join("recipes/idless.toml"),
            idless_recipe("idless", "**/*.json"),
        )
        .unwrap();
        let bodies = [
            r#"{"id":""}"#,
            r#"{"id":7}"#,
            r#"{"id":null,"name":"n"}"#,
            r#"{"note":"no id field"}"#,
        ];
        for (i, body) in bodies.iter().enumerate() {
            std::fs::write(notes.join(format!("n{i}.json")), body).unwrap();
        }
        let err = run_build_inner(&["idless".into()], home.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty id"), "{err}");
        assert_eq!(store_files(home.path(), "idless", "notes.").len(), 4);
        let pack = home.path().join("builds/idless/pack/idless");
        if pack.is_dir() {
            for entry in std::fs::read_dir(&pack).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                assert!(
                    !name.starts_with("records-"),
                    "refused records must not be written: {name}"
                );
            }
        }
    }

    #[test]
    fn corrupt_store_envelope_fails_the_rebuild_without_shrinking_the_pack() {
        // #1110 gap 4 at the pipeline: a truncated `<slug>.<key>.json` must
        // fail the rebuild. The previous pack stays, and the corrupt file is
        // not pruned (its key is still live) and then quietly omitted.
        let home = tempfile::tempdir().unwrap();
        e2e_env(home.path());
        let args: Vec<String> = vec!["e2e".to_string()];
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let before = store_files(home.path(), "e2e", "");
        let gen = pack_generation(home.path(), "e2e");
        let records_before = read_records(&home.path().join("builds/e2e/pack/e2e")).len();

        let victim = home
            .path()
            .join("builds/e2e/store")
            .join(before.iter().find(|n| n.starts_with("rustsec.")).unwrap());
        std::fs::write(&victim, "{truncated").unwrap();

        let err = run_build_inner(&args, home.path()).unwrap_err().to_string();
        assert!(err.contains("corrupt store envelope"), "{err}");
        assert!(
            victim.is_file(),
            "the corrupt envelope must still be on disk, not pruned"
        );
        assert_eq!(pack_generation(home.path(), "e2e"), gen);
        assert_eq!(
            read_records(&home.path().join("builds/e2e/pack/e2e")).len(),
            records_before,
            "the published pack must not shrink"
        );
    }

    #[test]
    fn fresh_rebuilds_after_a_corrupt_manifest() {
        // #1110 gap 5: a corrupt manifest.json fails a normal rebuild (it
        // must not reset generation to 0), and `--fresh` is the recovery —
        // it discards the build dir and harvests again.
        let home = tempfile::tempdir().unwrap();
        e2e_env(home.path());
        let args: Vec<String> = vec!["e2e".to_string()];
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let manifest_path = home.path().join("builds/e2e/pack/e2e/manifest.json");
        std::fs::write(&manifest_path, "{truncated").unwrap();

        let err = run_build_inner(&args, home.path()).unwrap_err().to_string();
        assert!(err.contains("corrupt previous manifest"), "{err}");
        // the failed rebuild must not have replaced the torn manifest with
        // a generation-1 success
        assert_eq!(std::fs::read(&manifest_path).unwrap(), b"{truncated");

        let fresh: Vec<String> = vec!["e2e".into(), "--fresh".into()];
        assert_eq!(run_build_inner(&fresh, home.path()).unwrap(), 0);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["generation"], json!(1));
        assert_eq!(manifest["counts"]["records"], json!(2));
        assert_eq!(read_records(manifest_path.parent().unwrap()).len(), 2);
    }

    // ── end to end: a git source, watermark-skipped on the second run ─────

    #[test]
    fn build_from_git_source_and_skip_when_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let origin = home.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        crate::xc::git(Some(&origin), &["init", "--quiet", "-b", "main"]).ok();
        std::fs::write(origin.join("a.json"), r#"{"id": "R1", "aliases": ["C1"]}"#).unwrap();
        crate::xc::git(Some(&origin), &["add", "."]).ok();
        crate::xc::git(
            Some(&origin),
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "one",
            ],
        )
        .ok();

        let recipe = format!(
            "[recipe]\nformat = 1\nname = \"gitdemo\"\n\n\
             [[sources]]\nslug = \"upstream\"\nkind = \"git\"\nurl = \"{}\"\nformat = \"flat\"\n\
             licence = \"CC0-1.0\"\n\n\
             [identity]\nedges = [{{ field = \"id\" }}]\n",
            origin.to_string_lossy()
        );
        std::fs::create_dir_all(home.path().join("recipes")).unwrap();
        std::fs::write(home.path().join("recipes/gitdemo.toml"), recipe).unwrap();

        let args: Vec<String> = vec!["gitdemo".to_string()];
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let pack = home.path().join("builds/gitdemo/pack/gitdemo");
        assert_eq!(read_records(&pack).len(), 1);

        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        let s = &manifest["sources"][0];
        assert_eq!(s["kind"], json!("git"));
        assert_eq!(s["new"], json!(1));
        assert!(s["watermark"].as_str().is_some_and(|w| w.len() == 40));

        // second run: same HEAD → the source is not walked, the store is
        // untouched, the pack is rebuilt identically from the store
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        let s = &manifest["sources"][0];
        assert_eq!(s["unchanged"], json!(true));
        assert_eq!(s["records"], json!(0), "no walk happened");
        assert_eq!(
            read_records(&pack).len(),
            1,
            "records survive via the store"
        );
        assert!(home.path().join("builds/gitdemo/report.json").is_file());

        // a new commit upstream re-activates the source
        std::fs::write(origin.join("b.json"), r#"{"id": "R2"}"#).unwrap();
        crate::xc::git(Some(&origin), &["add", "."]).ok();
        crate::xc::git(
            Some(&origin),
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "two",
            ],
        )
        .ok();
        assert_eq!(run_build_inner(&args, home.path()).unwrap(), 0);
        assert_eq!(read_records(&pack).len(), 2);
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(pack.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["sources"][0]["unchanged"], json!(false));
        assert_eq!(manifest["sources"][0]["new"], json!(1));
    }
}
