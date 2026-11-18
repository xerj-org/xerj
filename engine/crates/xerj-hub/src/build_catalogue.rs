//! `xerj hub build-catalogue`: the signed catalogue clients route and pull from
//! (docs/CORPUS_HUB_V2_IMPLEMENTATION.md §4).
//!
//! One [`CatalogueEntry`] per LIVE lane-A corpus in `backlog/backlog-100.json`
//! whose CURRENT pin (the manifest's first repo sha) has a pack in `--packs`.
//! The entry's fields come from the registry manifest (through
//! [`crate::catalogue::build_catalogue`], the one manifest parser), the backlog
//! row (domain, refresh cadence, the `ask_for` fallback) and the pack itself
//! (bytes, sha256, the definitions and paths for the term sketch).
//!
//! Every pack is checked before it is listed: whole-archive sha256, the detached
//! signature against the key clients embed, `format_version`, and that its
//! `pack.json` names this slug at this pin. A pack that fails is a hard error,
//! never a silent omission: a catalogue is a complete snapshot, so a dropped
//! entry would delist a corpus. The finished catalogue's own signature is
//! verified against the same key before the command succeeds, so a seed that
//! is not the private half of [`xerj_common::hub::HUB_PUBKEY_HEX`] fails here
//! instead of publishing a catalogue no client can load.
//!
//! Live lane-B corpora (record packs, `build-pack --from-pack`) have no
//! registry manifest and no commit pin: their entry comes from the backlog row
//! and the pack's own signed `pack.json`, and their current pin is the newest
//! verified pack for the slug in `--packs` (see [`record_entries`]).
//!
//! `quality_grade` comes only from the graded G7 suites in
//! `<registry>/backlog/*graded*.json`, read exactly as the hub site generator
//! reads them (`tools/corpus-hub-site/gen.py` `parse_graded_suites` and
//! `g7_badge`), so the catalogue and the corpus page show the same grade. A
//! lane-A corpus with no graded suite gets `None`, and a lane-B record pack
//! always does: the field's contract is "never invent one" (see
//! [`read_graded_suites`]).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use xerj_autoindex::harvest::sign::{sign_bytes, verify_bytes};
use xerj_common::hub::{
    Catalogue, CatalogueEntry, DefRecord, LicenceClass, PackManifest, PACK_FORMAT_VERSION,
};

use crate::catalogue::{build_catalogue, canonical_json, CorpusBuildInput};
use crate::pack::PackFileEntry;

/// Where published packs live unless `--release-base` says otherwise. The plan's
/// `pack_url` is `<this>/pack-<slug>-<pin8>/<slug>-<pin8>.hubpack.tar.zst`.
pub const DEFAULT_RELEASE_BASE: &str = "https://github.com/xerj-org/xerj/releases/download";

/// `refresh` class → `refresh_cadence_days` (plan §1: 7 for code, 1 for feeds).
/// The same table as `tools/hub-packs/hub_packs.py` `CADENCE_DAYS`.
fn cadence_days(refresh: &str) -> u32 {
    match refresh {
        "daily" => 1,
        _ => 7,
    }
}

const USAGE: &str = "usage: xerj hub build-catalogue --registry <dir> --packs <dir> --out <file> --sign-key <seed-file>
                                  [--backlog <file>] [--release-base <url>] [--tags <tsv>]

Builds and signs the hub catalogue (writes <out> and <out>.sig) from the packs in
--packs (`<slug>-<pin8>.hubpack.tar.zst` + `.sig`, as `hub build-pack --out` writes
them). One entry per live lane-A corpus in the backlog (default
<registry>/backlog/backlog-100.json) whose current pin has a pack there.

  --release-base <url>  pack_url prefix (default
                        https://github.com/xerj-org/xerj/releases/download)
  --tags <tsv>          `slug<TAB>pin8<TAB>tag` lines naming each pack's release
                        tag (a -rN retag); default tag pack-<slug>-<pin8>

Every pack is verified (sha256, signature against the hub key this build embeds,
format, slug and pin) before it is listed; any failure aborts.

quality_grade comes from the graded G7 suites in <registry>/backlog/*graded*.json
(the files the hub site reads); a corpus with no graded suite has none.";

/// Inputs to [`build`]. Paths as given on the command line.
pub struct Options {
    pub registry: PathBuf,
    pub backlog: PathBuf,
    pub packs: PathBuf,
    pub release_base: String,
    /// `(slug, pin8) -> release tag`, from `--tags`.
    pub tags: BTreeMap<(String, String), String>,
    /// RFC 3339; the CLI passes now.
    pub generated_at: String,
}

/// What [`build`] produced.
pub struct Built {
    pub catalogue: Catalogue,
    /// The exact bytes to write to `catalogue.json`.
    pub bytes: Vec<u8>,
    /// Hex ed25519 signature over `bytes`.
    pub sig_hex: String,
    /// Live corpora with no pack for their current pin in `--packs`.
    pub without_pack: Vec<String>,
    /// Archives in `--packs` that matched no live corpus's current pin.
    pub unmatched: Vec<String>,
}

/// `xerj hub build-catalogue …`.
pub fn run(args: &[String]) -> i32 {
    let mut registry: Option<String> = None;
    let mut backlog: Option<String> = None;
    let mut packs: Option<String> = None;
    let mut out: Option<String> = None;
    let mut sign_key: Option<String> = None;
    let mut release_base: Option<String> = None;
    let mut tags: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if matches!(flag, "-h" | "--help") {
            println!("{USAGE}");
            return 0;
        }
        let slot = match flag {
            "--registry" => &mut registry,
            "--backlog" => &mut backlog,
            "--packs" => &mut packs,
            "--out" => &mut out,
            "--sign-key" => &mut sign_key,
            "--release-base" => &mut release_base,
            "--tags" => &mut tags,
            other => {
                eprintln!("xerj hub build-catalogue: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        };
        match args.get(i + 1) {
            Some(v) => *slot = Some(v.clone()),
            None => {
                eprintln!("xerj hub build-catalogue: {flag} needs a value\n\n{USAGE}");
                return 2;
            }
        }
        i += 2;
    }
    let (Some(registry), Some(packs), Some(out), Some(sign_key)) = (registry, packs, out, sign_key)
    else {
        eprintln!(
            "xerj hub build-catalogue: --registry, --packs, --out and --sign-key are all \
             required\n\n{USAGE}"
        );
        return 2;
    };
    match run_inner(
        &registry,
        backlog.as_deref(),
        &packs,
        &out,
        &sign_key,
        release_base.as_deref(),
        tags.as_deref(),
    ) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(e) => {
            eprintln!("xerj hub build-catalogue: {e:#}");
            1
        }
    }
}

fn run_inner(
    registry: &str,
    backlog: Option<&str>,
    packs: &str,
    out: &str,
    sign_key_file: &str,
    release_base: Option<&str>,
    tags: Option<&str>,
) -> Result<String> {
    let seed = fs::read_to_string(sign_key_file)
        .with_context(|| format!("read sign key {sign_key_file}"))?
        .trim()
        .to_string();
    let pubkey = xerj_common::hub::hub_pubkey().ok_or_else(|| {
        anyhow!(
            "this build has no hub public key, so neither the packs nor the catalogue can be \
             verified; refusing to build"
        )
    })?;
    let registry = PathBuf::from(registry);
    let opts = Options {
        backlog: backlog
            .map(PathBuf::from)
            .unwrap_or_else(|| registry.join("backlog").join("backlog-100.json")),
        registry,
        packs: PathBuf::from(packs),
        release_base: release_base.unwrap_or(DEFAULT_RELEASE_BASE).to_string(),
        tags: match tags {
            Some(p) => read_tags(Path::new(p))?,
            None => BTreeMap::new(),
        },
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let built = build(&opts, &seed, &pubkey)?;
    for slug in &built.unmatched {
        eprintln!("xerj hub build-catalogue: note: {slug} matches no live corpus's current pin; not listed");
    }
    let out = PathBuf::from(out);
    fs::write(&out, &built.bytes).with_context(|| format!("write {}", out.display()))?;
    let mut sig = out.clone().into_os_string();
    sig.push(".sig");
    fs::write(&sig, built.sig_hex.as_bytes())
        .with_context(|| format!("write {}", PathBuf::from(&sig).display()))?;
    Ok(format!(
        "catalogue {} corpora, {} bytes ({} live corpora have no pack yet)",
        built.catalogue.corpora.len(),
        built.bytes.len(),
        built.without_pack.len()
    ))
}

/// `slug<TAB>pin8<TAB>tag` lines (the `fetch-list` output of
/// `tools/hub-packs/hub_packs.py`). Blank lines are skipped; anything else
/// malformed is an error.
pub fn read_tags(path: &Path) -> Result<BTreeMap<(String, String), String>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut map = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').map(str::trim).collect();
        let [slug, pin8, tag] = parts[..] else {
            bail!(
                "{}:{}: expected slug<TAB>pin8<TAB>tag",
                path.display(),
                n + 1
            );
        };
        map.insert((slug.to_string(), pin8.to_string()), tag.to_string());
    }
    Ok(map)
}

/// Build and sign the catalogue. `pubkey_hex` is the key clients embed: every
/// pack and the finished catalogue must verify under it.
pub fn build(opts: &Options, seed_hex: &str, pubkey_hex: &str) -> Result<Built> {
    let backlog: Value = serde_json::from_str(
        &fs::read_to_string(&opts.backlog)
            .with_context(|| format!("read backlog {}", opts.backlog.display()))?,
    )
    .with_context(|| format!("parse backlog {}", opts.backlog.display()))?;
    let rows = backlog
        .get("corpora")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("backlog {} has no corpora[]", opts.backlog.display()))?;

    let grades = read_graded_suites(&opts.registry.join("backlog"))?;
    let mut inputs: Vec<CorpusBuildInput> = Vec::new();
    let mut without_pack: Vec<String> = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    for row in rows {
        let field = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("");
        if field("status") != "live" || field("lane") != "A" {
            continue;
        }
        let slug = field("slug");
        let manifest_path = opts.registry.join(format!("{slug}.json"));
        if !manifest_path.is_file() {
            // `hub_packs.py plan` warns about these; there is no pack to list.
            without_pack.push(slug.to_string());
            continue;
        }
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(&manifest_path)
                .with_context(|| format!("read {}", manifest_path.display()))?,
        )
        .with_context(|| format!("parse {}", manifest_path.display()))?;
        let pin_sha = manifest
            .pointer("/repos/0/sha")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if pin_sha.len() != 40 || !pin_sha.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!(
                "{}: repos[0].sha is not a 40-hex commit",
                manifest_path.display()
            );
        }
        let pin8 = &pin_sha[..8];
        let file = format!("{slug}-{pin8}.hubpack.tar.zst");
        let archive = opts.packs.join(&file);
        if !archive.is_file() {
            without_pack.push(slug.to_string());
            continue;
        }
        used.insert(file.clone());

        let pack = read_pack(&archive, pubkey_hex)
            .with_context(|| format!("pack {}", archive.display()))?;
        if pack.manifest.slug != slug {
            bail!(
                "{file}: pack.json names slug '{}', not '{slug}'",
                pack.manifest.slug
            );
        }
        if !pack.manifest.pin_sha.eq_ignore_ascii_case(&pin_sha) {
            bail!(
                "{file}: pack.json pin {} is not the manifest's pin {pin_sha}",
                pack.manifest.pin_sha
            );
        }

        let backlog_domain = field("domain");
        let terms = sketch_terms(&pack.defs, &pack.files, &manifest, backlog_domain);

        let tag = opts
            .tags
            .get(&(slug.to_string(), pin8.to_string()))
            .cloned()
            .unwrap_or_else(|| format!("pack-{slug}-{pin8}"));
        let pack_url = format!("{}/{tag}/{file}", opts.release_base.trim_end_matches('/'));
        inputs.push(CorpusBuildInput {
            manifest_path,
            domain: field("cat").to_string(),
            refresh_cadence_days: cadence_days(field("refresh")),
            pack_bytes: pack.bytes,
            sig_url: format!("{pack_url}.sig"),
            pack_url,
            pack_sha256: pack.sha256,
            quality_grade: grades.get(slug).cloned(),
            terms,
            pin_sha,
            ask_for: backlog_domain.to_string(),
        });
    }
    let records = record_entries(opts, rows, pubkey_hex, &mut used, &mut without_pack)?;
    if inputs.is_empty() && records.is_empty() {
        bail!(
            "no live corpus has a pack for its current pin in {}",
            opts.packs.display()
        );
    }

    let mut unmatched: Vec<String> = Vec::new();
    for entry in
        fs::read_dir(&opts.packs).with_context(|| format!("list {}", opts.packs.display()))?
    {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".hubpack.tar.zst") && !used.contains(&name) {
            unmatched.push(name);
        }
    }
    unmatched.sort();

    let (mut catalogue, mut sig_hex) = build_catalogue(&inputs, &opts.generated_at, seed_hex)?;
    if !records.is_empty() {
        catalogue.corpora.extend(records);
        catalogue.corpora.sort_by(|a, b| a.slug.cmp(&b.slug));
        sig_hex = sign_bytes(&canonical_json(&catalogue)?, seed_hex).context("sign catalogue")?;
    }
    let bytes = canonical_json(&catalogue)?;
    verify_bytes(&bytes, &sig_hex, pubkey_hex).context(
        "the catalogue's signature does not verify under the hub key this build embeds: the \
         signing seed is not its private half, and no client could load this catalogue",
    )?;
    Ok(Built {
        catalogue,
        bytes,
        sig_hex,
        without_pack,
        unmatched,
    })
}

/// The terms a corpus's [`Sketch`](xerj_common::hub::Sketch) is built over
/// (plan §4): every definition symbol, every path component without extension,
/// every lower-case 3+ letter word of `ask_for` and `topics`. Symbols and path
/// components go in raw and lower-cased, because the router probes both forms of
/// each token. `ask_for` is the manifest's `ask_for`, else its `domain`, else
/// the backlog row's `domain`.
///
/// Public so the scale measurement (`examples/hub_scale.rs`) builds sketches
/// from a cloned tree with exactly the terms `build-catalogue` would use.
pub fn sketch_terms(
    defs: &[DefRecord],
    files: &[PackFileEntry],
    manifest: &Value,
    backlog_domain: &str,
) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut push = |t: &str| {
        if !t.is_empty() {
            terms.push(t.to_string());
            terms.push(t.to_lowercase());
        }
    };
    for d in defs {
        push(&d.symbol);
    }
    for f in files {
        for comp in f.path.split('/') {
            push(strip_extension(comp));
        }
    }
    let ask_for = [
        manifest.get("ask_for").and_then(Value::as_str),
        manifest.get("domain").and_then(Value::as_str),
        Some(backlog_domain),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|s| !s.is_empty())
    .unwrap_or("");
    let topics: Vec<&str> = manifest
        .get("topics")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for text in std::iter::once(ask_for).chain(topics) {
        terms.extend(words(text));
    }
    terms
}

/// `slug -> "G7 <median>/5"` from every `*graded*.json` in `backlog_dir`, the
/// files and rules of the site generator (`tools/corpus-hub-site/gen.py`):
///
/// - files in sorted name order; a later file's suite for a slug replaces an
///   earlier one (`out[slug] = suite`);
/// - the suites map is the first of `graded`, `suites`, `corpora` present; a
///   file whose map is not an object (e.g. `"graded": "<date>"`) or that is
///   not valid JSON is skipped;
/// - only suites that carry `queries` count;
/// - the suite's own `median` when it has one, else `suite_median`: per
///   query `verdict` (falling back to `grade`), pass/relevant = 1,
///   partial = 0.5, anything else 0, `*excluded*` left out of both sides,
///   scaled to 5;
/// - rendered `G7 {:.1}/5`, the site badge's text.
///
/// One deliberate difference: where the site would print `0.0/5` for a suite
/// with no gradable query (or a `median` that is not a number), this returns
/// no grade, because nothing was graded there.
pub fn read_graded_suites(backlog_dir: &Path) -> Result<BTreeMap<String, String>> {
    let mut files: Vec<PathBuf> = Vec::new();
    match fs::read_dir(backlog_dir) {
        Ok(rd) => {
            for entry in rd {
                let path = entry
                    .with_context(|| format!("list {}", backlog_dir.display()))?
                    .path();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.contains("graded") && name.ends_with(".json") && path.is_file() {
                    files.push(path);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).with_context(|| format!("list {}", backlog_dir.display())),
    }
    files.sort();
    let mut out = BTreeMap::new();
    for path in files {
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let Ok(data) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(container) = ["graded", "suites", "corpora"]
            .into_iter()
            .find_map(|k| data.get(k))
        else {
            continue;
        };
        let Some(suites) = container.as_object() else {
            continue;
        };
        for (slug, suite) in suites {
            let Some(queries) = suite.get("queries") else {
                continue;
            };
            let median = match suite.get("median") {
                None | Some(Value::Null) => suite_median(queries),
                Some(v) => v.as_f64(),
            };
            match median {
                Some(m) => out.insert(slug.clone(), format!("G7 {m:.1}/5")),
                None => out.remove(slug),
            };
        }
    }
    Ok(out)
}

/// One entry per LIVE lane-B corpus (a record pack built by `hub build-pack
/// --from-pack`) with a pack in `--packs`. A record pack's pin is a content
/// pin, not a registry fact, so its current pack is the newest one for the
/// slug in `--packs` (by `pack.json` `built_at`); the catalogue job downloads
/// the record packs' published releases for it to choose from. Every candidate
/// is verified like a lane-A pack; a failure aborts. Fields: slug, pin, licence
/// and sources from the signed `pack.json`; domain, refresh, `ask_for` and
/// `topics` from the backlog row.
fn record_entries(
    opts: &Options,
    rows: &[Value],
    pubkey_hex: &str,
    used: &mut HashSet<String>,
    without_pack: &mut Vec<String>,
) -> Result<Vec<CatalogueEntry>> {
    let mut names: Vec<String> = Vec::new();
    for entry in
        fs::read_dir(&opts.packs).with_context(|| format!("list {}", opts.packs.display()))?
    {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    let mut out = Vec::new();
    for row in rows {
        let field = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("");
        if field("status") != "live" || field("lane") != "B" {
            continue;
        }
        let slug = field("slug");
        let mut best: Option<(String, PackFacts)> = None;
        for name in &names {
            let Some(pin8) = name
                .strip_prefix(&format!("{slug}-"))
                .and_then(|r| r.strip_suffix(".hubpack.tar.zst"))
            else {
                continue;
            };
            if pin8.len() != 8 || !pin8.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                continue;
            }
            let pack = read_pack(&opts.packs.join(name), pubkey_hex)
                .with_context(|| format!("pack {name}"))?;
            if pack.manifest.slug != slug {
                bail!(
                    "{name}: pack.json names slug '{}', not '{slug}'",
                    pack.manifest.slug
                );
            }
            if !pack.manifest.pin_sha.starts_with(pin8) {
                bail!(
                    "{name}: pack.json pin {} does not start with the file's {pin8}",
                    pack.manifest.pin_sha
                );
            }
            used.insert(name.clone());
            let newer = match &best {
                Some((_, b)) => pack.manifest.built_at > b.manifest.built_at,
                None => true,
            };
            if newer {
                best = Some((name.clone(), pack));
            }
        }
        let Some((file, pack)) = best else {
            without_pack.push(slug.to_string());
            continue;
        };
        if pack.manifest.licence_class == LicenceClass::Excluded {
            // Plan §12.1: an excluded licence gets no listing.
            continue;
        }
        let pin8 = &pack.manifest.pin_sha[..8];
        let ask_for = field("domain").trim().to_string();
        let topics: Vec<String> = row
            .pointer("/tags/topics")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut terms: Vec<String> = Vec::new();
        let mut push = |t: &str| {
            if !t.is_empty() {
                terms.push(t.to_string());
                terms.push(t.to_lowercase());
            }
        };
        for d in &pack.defs {
            push(&d.symbol);
        }
        for f in &pack.files {
            for comp in f.path.split('/') {
                push(strip_extension(comp));
            }
        }
        for text in std::iter::once(ask_for.as_str()).chain(topics.iter().map(String::as_str)) {
            terms.extend(words(text));
        }
        let tag = opts
            .tags
            .get(&(slug.to_string(), pin8.to_string()))
            .cloned()
            .unwrap_or_else(|| format!("pack-{slug}-{pin8}"));
        let pack_url = format!("{}/{tag}/{file}", opts.release_base.trim_end_matches('/'));
        out.push(CatalogueEntry {
            slug: slug.to_string(),
            domain: field("cat").to_string(),
            topics,
            ask_for,
            pin_sha: pack.manifest.pin_sha.clone(),
            pin_date: pack.manifest.pin_date.clone(),
            refresh_cadence_days: cadence_days(field("refresh")),
            licence_spdx: pack.manifest.licence_spdx.clone(),
            licence_class: pack.manifest.licence_class,
            pack_bytes: pack.bytes,
            sig_url: format!("{pack_url}.sig"),
            pack_url,
            pack_sha256: pack.sha256,
            dependency_names: Vec::new(),
            quality_grade: None,
            upstream: crate::catalogue::upstream_list(
                pack.manifest
                    .sources
                    .iter()
                    .map(|s| (s.url.as_str(), s.repo.as_str())),
            ),
            term_sketch: crate::sketch::build(terms),
        });
    }
    Ok(out)
}

/// gen.py `suite_median`: relevant-of-5 from the per-query verdicts, or
/// `None` when no query carries a gradable verdict.
fn suite_median(queries: &Value) -> Option<f64> {
    let mut got: Vec<f64> = Vec::new();
    for q in queries.as_array()? {
        let verdict = ["verdict", "grade"]
            .into_iter()
            .filter_map(|k| q.get(k))
            .find(|v| truthy(v));
        let label = match verdict {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        };
        if label.contains("excluded") {
            continue;
        }
        got.push(match label.as_str() {
            "pass" | "relevant" => 1.0,
            "partial" => 0.5,
            _ => 0.0,
        });
    }
    if got.is_empty() {
        return None;
    }
    Some(got.iter().sum::<f64>() / got.len() as f64 * 5.0)
}

/// Python truthiness for the `a or b` in gen.py.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The members of a verified pack the catalogue needs.
struct PackFacts {
    bytes: u64,
    sha256: String,
    manifest: PackManifest,
    defs: Vec<DefRecord>,
    files: Vec<PackFileEntry>,
}

/// Verify `archive` (sha256 is computed, the `.sig` beside it must verify under
/// `pubkey_hex`) and read `pack.json`, `defs.jsonl` and `files.jsonl` without
/// extracting anything.
fn read_pack(archive: &Path, pubkey_hex: &str) -> Result<PackFacts> {
    let raw = fs::read(archive).with_context(|| format!("read {}", archive.display()))?;
    let mut sig = archive.as_os_str().to_os_string();
    sig.push(".sig");
    let sig = PathBuf::from(sig);
    let sig_hex = fs::read_to_string(&sig).with_context(|| format!("read {}", sig.display()))?;
    verify_bytes(&raw, sig_hex.trim(), pubkey_hex).context("pack signature")?;
    let sha256 = hex(&Sha256::digest(&raw));

    let mut pack_json: Option<Vec<u8>> = None;
    let mut defs_jsonl: Option<Vec<u8>> = None;
    let mut files_jsonl: Option<Vec<u8>> = None;
    let mut tar = tar::Archive::new(zstd::Decoder::new(raw.as_slice()).context("zstd")?);
    for entry in tar.entries().context("read tar")? {
        let mut entry = entry.context("read tar entry")?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let slot = match path.as_str() {
            "pack.json" => &mut pack_json,
            "defs.jsonl" => &mut defs_jsonl,
            "files.jsonl" => &mut files_jsonl,
            _ => continue,
        };
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        *slot = Some(buf);
    }
    let pack_json = pack_json.ok_or_else(|| anyhow!("pack.json missing"))?;
    let manifest: PackManifest = serde_json::from_slice(&pack_json).context("parse pack.json")?;
    if manifest.format_version != PACK_FORMAT_VERSION {
        bail!(
            "pack format_version {} (this build writes {PACK_FORMAT_VERSION})",
            manifest.format_version
        );
    }
    Ok(PackFacts {
        bytes: raw.len() as u64,
        sha256,
        manifest,
        defs: parse_jsonl(
            &defs_jsonl.ok_or_else(|| anyhow!("defs.jsonl missing"))?,
            "defs.jsonl",
        )?,
        files: parse_jsonl(
            &files_jsonl.ok_or_else(|| anyhow!("files.jsonl missing"))?,
            "files.jsonl",
        )?,
    })
}

fn parse_jsonl<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<Vec<T>> {
    let text = std::str::from_utf8(bytes).with_context(|| format!("{what} is not UTF-8"))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(n, l)| serde_json::from_str(l).with_context(|| format!("{what} line {}", n + 1)))
        .collect()
}

/// `lz4frame.c` → `lz4frame`; `lib` → `lib`; `.github` → `.github` (a leading
/// dot is a name, not an extension).
fn strip_extension(comp: &str) -> &str {
    match comp.rfind('.') {
        Some(i) if i > 0 => &comp[..i],
        _ => comp,
    }
}

/// Lower-case words of 3+ ASCII letters.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 3)
        .map(str::to_ascii_lowercase)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::write_pack;
    use crate::sketch;
    use xerj_autoindex::harvest::sign::generate_keypair;
    use xerj_common::hub::{LicenceClass, PackIndex, PackSource};

    const PIN: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";

    fn manifest(slug: &str, pin: &str) -> PackManifest {
        PackManifest {
            format_version: PACK_FORMAT_VERSION,
            slug: slug.into(),
            pin_sha: pin.into(),
            pin_date: "2026-10-02".into(),
            built_at: "2026-10-16T00:00:00Z".into(),
            builder_version: "xerj test".into(),
            index_prefix: format!("xc-{slug}-b1"),
            indices: vec![PackIndex {
                name: format!("xc-{slug}-b1-docs"),
                docs: 1,
                bytes: 1,
            }],
            snapshot_name: format!("{slug}-{}", &pin[..8]),
            licence_spdx: "BSD-2-Clause".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            sources: vec![PackSource {
                repo: slug.into(),
                url: format!("https://github.com/x/{slug}"),
                sha: pin.into(),
                licence: "BSD-2-Clause".into(),
            }],
            defs_count: 1,
            files_count: 1,
        }
    }

    /// A registry with one live lane-A corpus (`lz4`), one live corpus with no
    /// pack (`zstd`) and one retired corpus, plus a signed pack for lz4.
    fn fixture(dir: &Path, seed: &str) -> Options {
        let registry = dir.join("hub");
        fs::create_dir_all(registry.join("backlog")).unwrap();
        fs::write(
            registry.join("backlog/backlog-100.json"),
            r#"{"corpora":[
              {"slug":"lz4","status":"live","lane":"A","cat":"NET","refresh":"pinned",
               "domain":"an agent needing fast block compression queries this for the decoder loop"},
              {"slug":"zstd","status":"live","lane":"A","cat":"NET","refresh":"weekly","domain":"x"},
              {"slug":"old","status":"retired","lane":"A","cat":"NET","refresh":"pinned","domain":"x"}
            ]}"#,
        )
        .unwrap();
        for (slug, pin) in [
            ("lz4", PIN),
            ("zstd", "1111111111111111111111111111111111111111"),
        ] {
            fs::write(
                registry.join(format!("{slug}.json")),
                format!(
                    r#"{{"corpus":"{slug}","cloned_at":"2026-10-02T00:49:45Z","repos":[{{"repo":"{slug}",
                    "url":"https://github.com/x/{slug}","sha":"{pin}",
                    "review":{{"spdx":"BSD-2-Clause","use":"adapt-with-attribution","at":"2026-10-02"}}}}]}}"#
                ),
            )
            .unwrap();
        }
        let packs = dir.join("packs");
        fs::create_dir_all(&packs).unwrap();
        let snap = dir.join("snap");
        fs::create_dir_all(&snap).unwrap();
        fs::write(snap.join("index-0"), b"opaque").unwrap();
        let defs = vec![DefRecord {
            symbol: "LZ4_compress_default".into(),
            kind: "function".into(),
            lang: "c".into(),
            file: "lz4/lib/lz4.c".into(),
            line: 42,
            text: "int LZ4_compress_default(...)".into(),
            value: None,
        }];
        let files = vec![PackFileEntry {
            path: "lz4/lib/lz4frame.c".into(),
            lang: "c".into(),
            bytes: 10,
            licence: "BSD-2-Clause".into(),
        }];
        write_pack(
            &packs.join(format!("lz4-{}.hubpack.tar.zst", &PIN[..8])),
            &manifest("lz4", PIN),
            &defs,
            &files,
            &snap,
            seed,
        )
        .unwrap();
        Options {
            backlog: registry.join("backlog/backlog-100.json"),
            registry,
            packs,
            release_base: DEFAULT_RELEASE_BASE.into(),
            tags: BTreeMap::new(),
            generated_at: "2026-10-16T00:00:00Z".into(),
        }
    }

    #[test]
    fn lists_the_live_corpus_with_a_verified_pack_and_signs_the_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        let built = build(&opts, &seed, &public).unwrap();

        assert_eq!(built.catalogue.corpora.len(), 1);
        let e = &built.catalogue.corpora[0];
        assert_eq!(e.slug, "lz4");
        assert_eq!(e.domain, "NET");
        assert_eq!(e.refresh_cadence_days, 7);
        assert_eq!(e.pin_sha, PIN);
        assert_eq!(
            e.pack_url,
            "https://github.com/xerj-org/xerj/releases/download/pack-lz4-0774d055/lz4-0774d055.hubpack.tar.zst"
        );
        assert_eq!(e.sig_url, format!("{}.sig", e.pack_url));
        let archive = opts.packs.join("lz4-0774d055.hubpack.tar.zst");
        let raw = fs::read(&archive).unwrap();
        assert_eq!(e.pack_bytes, raw.len() as u64);
        assert_eq!(e.pack_sha256, hex(&Sha256::digest(&raw)));
        assert_eq!(e.quality_grade, None);
        assert!(e.ask_for.contains("block compression"));
        // The sketch holds terms lower-cased (so raw and lower no longer both
        // occupy it), plus the sub-words of symbols and paths, plus the curated
        // description words.
        assert_eq!(e.upstream, "x/lz4");
        for term in [
            "lz4_compress_default", // a symbol, lower-cased
            "compress",             // its sub-word
            "lz4frame",             // a path component
            "decoder",              // an ask_for word
            "lz4",                  // the slug
        ] {
            assert!(sketch::contains(&e.term_sketch, term), "{term} in sketch");
        }
        assert_eq!(built.without_pack, vec!["zstd".to_string()]);
        assert!(built.unmatched.is_empty());
        verify_bytes(&built.bytes, &built.sig_hex, &public).unwrap();
        assert_eq!(built.bytes, canonical_json(&built.catalogue).unwrap());
    }

    /// The shapes the real registry's graded files have (corpus-hub branch,
    /// `tools/xerj-code/hub/backlog/*graded*.json`).
    fn write_graded(backlog: &Path) {
        // A suite with its own median, and one for a corpus that is not live.
        fs::write(
            backlog.join("g7-a-2026-10-graded.json"),
            r#"{"graded_at":"2026-10-04","corpora":{
              "lz4":{"median":3.0,"verdict":"PASS","queries":[{"verdict":"pass"}]},
              "otel":{"pass":0,"verdict":"FAIL","queries":[
                {"verdict":"miss"},{"verdict":"miss"},{"verdict":"miss"},{"verdict":"miss"},{"verdict":"miss"}]},
              "nogrades":{"verdict":"PENDING","queries":[]}
            }}"#,
        )
        .unwrap();
        // Later in sort order: replaces lz4 (verdict falls back to grade,
        // partial = 0.5, a suite defect is excluded from both sides).
        fs::write(
            backlog.join("g7-b-2026-10-graded.json"),
            r#"{"suite":"x","graded":{
              "lz4":{"graded_at":"2026-10-02","queries":[
                {"grade":"relevant"},{"verdict":"","grade":"relevant"},{"grade":"miss"},
                {"grade":"partial"},{"grade":"suite-defect (excluded)"}]}
            }}"#,
        )
        .unwrap();
        // A file whose `graded` is a date string, and one that is not JSON:
        // both skipped, as the site skips them.
        fs::write(
            backlog.join("g7-c-2026-10-graded.json"),
            r#"{"graded":"2026-10-08 (re-graded)","corpora":{"lz4":{"median":5,"queries":[]}}}"#,
        )
        .unwrap();
        fs::write(backlog.join("g7-d-graded.json"), "{not json").unwrap();
        // Not a graded file: never read.
        fs::write(
            backlog.join("g7-e-2026-10.json"),
            r#"{"corpora":{"zstd":{"median":5,"queries":[]}}}"#,
        )
        .unwrap();
    }

    #[test]
    fn graded_suites_are_read_with_the_site_generators_rules() {
        let tmp = tempfile::tempdir().unwrap();
        write_graded(tmp.path());
        let grades = read_graded_suites(tmp.path()).unwrap();
        // (1 + 1 + 0 + 0.5) / 4 * 5 = 3.125 -> "3.1"
        assert_eq!(grades.get("lz4").map(String::as_str), Some("G7 3.1/5"));
        assert_eq!(grades.get("otel").map(String::as_str), Some("G7 0.0/5"));
        // Nothing gradable: no grade, never an invented 0.0.
        assert_eq!(grades.get("nogrades"), None);
        assert_eq!(grades.get("zstd"), None);
        assert_eq!(grades.len(), 2);
    }

    #[test]
    fn no_backlog_dir_means_no_grades() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_graded_suites(&tmp.path().join("absent"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_catalogue_entry_carries_the_graded_suites_grade() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        write_graded(&opts.registry.join("backlog"));
        let built = build(&opts, &seed, &public).unwrap();
        assert_eq!(
            built.catalogue.corpora[0].quality_grade.as_deref(),
            Some("G7 3.1/5")
        );
        let json = String::from_utf8(built.bytes).unwrap();
        assert!(json.contains(r#""quality_grade":"G7 3.1/5""#), "{json}");
    }

    /// Adds a live lane-B record corpus `rv` to the fixture's backlog and writes
    /// a signed record hubpack for it per `(pin, built_at)`.
    fn add_record_corpus(opts: &Options, seed: &str, packs: &[(&str, &str)]) {
        let mut backlog: Value =
            serde_json::from_str(&fs::read_to_string(&opts.backlog).unwrap()).unwrap();
        backlog["corpora"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "slug": "rv", "status": "live", "lane": "B", "cat": "SEC", "refresh": "daily",
                "domain": "an agent reviewing Rust code queries this for vulnerability precedent",
                "tags": { "topics": ["vulnerabilities"] }
            }));
        fs::write(&opts.backlog, backlog.to_string()).unwrap();
        let snap = opts.packs.parent().unwrap().join("snap");
        for (pin, built_at) in packs {
            let mut m = manifest("rv", pin);
            m.built_at = built_at.to_string();
            m.licence_spdx = "CC0-1.0".into();
            let defs = vec![DefRecord {
                symbol: format!("RUSTSEC-{}", &pin[..4]),
                kind: "record".into(),
                lang: String::new(),
                file: "rustsec/records.jsonl".into(),
                line: 1,
                text: "x".into(),
                value: None,
            }];
            let files = vec![PackFileEntry {
                path: "rustsec/records.jsonl".into(),
                lang: String::new(),
                bytes: 10,
                licence: "CC0-1.0".into(),
            }];
            write_pack(
                &opts.packs.join(format!("rv-{}.hubpack.tar.zst", &pin[..8])),
                &m,
                &defs,
                &files,
                &snap,
                seed,
            )
            .unwrap();
        }
    }

    #[test]
    fn a_live_record_corpus_is_listed_at_its_newest_verified_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        let old = "1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa";
        let new = "2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb";
        add_record_corpus(
            &opts,
            &seed,
            &[(old, "2026-10-16T04:30:00Z"), (new, "2026-10-17T04:30:00Z")],
        );
        let built = build(&opts, &seed, &public).unwrap();
        let slugs: Vec<&str> = built
            .catalogue
            .corpora
            .iter()
            .map(|e| e.slug.as_str())
            .collect();
        assert_eq!(slugs, ["lz4", "rv"], "sorted, both lanes");
        let e = &built.catalogue.corpora[1];
        assert_eq!(e.pin_sha, new);
        assert_eq!(
            e.pack_url,
            "https://github.com/xerj-org/xerj/releases/download/pack-rv-2222bbbb/rv-2222bbbb.hubpack.tar.zst"
        );
        assert_eq!(e.domain, "SEC");
        assert_eq!(e.refresh_cadence_days, 1);
        assert_eq!(e.topics, ["vulnerabilities"]);
        assert!(e.ask_for.contains("vulnerability precedent"));
        assert_eq!(e.licence_spdx, "CC0-1.0");
        assert_eq!(e.quality_grade, None);
        assert!(sketch::contains(&e.term_sketch, "RUSTSEC-2222"));
        assert!(sketch::contains(&e.term_sketch, "rustsec"));
        assert!(built.unmatched.is_empty(), "{:?}", built.unmatched);
        // The signature covers the merged list.
        verify_bytes(&built.bytes, &built.sig_hex, &public).unwrap();
        assert_eq!(built.bytes, canonical_json(&built.catalogue).unwrap());
    }

    #[test]
    fn a_record_pack_whose_pin_disagrees_with_its_file_name_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        add_record_corpus(
            &opts,
            &seed,
            &[("3333cccc3333cccc3333cccc3333cccc3333cccc", "x")],
        );
        fs::rename(
            opts.packs.join("rv-3333cccc.hubpack.tar.zst"),
            opts.packs.join("rv-4444dddd.hubpack.tar.zst"),
        )
        .unwrap();
        fs::rename(
            opts.packs.join("rv-3333cccc.hubpack.tar.zst.sig"),
            opts.packs.join("rv-4444dddd.hubpack.tar.zst.sig"),
        )
        .unwrap();
        let err = format!("{:#}", build(&opts, &seed, &public).err().unwrap());
        assert!(
            err.contains("does not start with the file's 4444dddd"),
            "{err}"
        );
    }

    #[test]
    fn a_live_record_corpus_without_a_pack_is_reported_not_invented() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        add_record_corpus(&opts, &seed, &[]);
        let built = build(&opts, &seed, &public).unwrap();
        assert_eq!(built.catalogue.corpora.len(), 1);
        assert_eq!(built.without_pack, ["zstd", "rv"]);
    }

    #[test]
    fn a_retag_from_the_tags_file_and_a_custom_base_shape_the_url() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let mut opts = fixture(tmp.path(), &seed);
        let tsv = tmp.path().join("fetch.tsv");
        fs::write(&tsv, "lz4\t0774d055\tpack-lz4-0774d055-r2\n\n").unwrap();
        opts.tags = read_tags(&tsv).unwrap();
        opts.release_base = "file:///srv/hub/".into();
        let built = build(&opts, &seed, &public).unwrap();
        assert_eq!(
            built.catalogue.corpora[0].pack_url,
            "file:///srv/hub/pack-lz4-0774d055-r2/lz4-0774d055.hubpack.tar.zst"
        );
    }

    #[test]
    fn a_tampered_pack_aborts_instead_of_being_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        let archive = opts.packs.join("lz4-0774d055.hubpack.tar.zst");
        let mut raw = fs::read(&archive).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        fs::write(&archive, raw).unwrap();
        let err = format!("{:#}", build(&opts, &seed, &public).err().unwrap());
        assert!(err.contains("signature"), "{err}");
    }

    #[test]
    fn a_seed_that_is_not_the_embedded_keys_private_half_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, _) = generate_keypair().unwrap();
        let (_, other_public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        // The pack was signed by `seed`, so it already fails under the other key.
        assert!(build(&opts, &seed, &other_public).is_err());
    }

    #[test]
    fn a_pack_whose_pack_json_disagrees_with_the_manifest_pin_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        // Rewrite lz4's pack with a pack.json for another pin but the same file name.
        let snap = tmp.path().join("snap");
        let other = "0774d055ffffffffffffffffffffffffffffffff";
        write_pack(
            &opts.packs.join("lz4-0774d055.hubpack.tar.zst"),
            &manifest("lz4", other),
            &[],
            &[],
            &snap,
            &seed,
        )
        .unwrap();
        let err = format!("{:#}", build(&opts, &seed, &public).err().unwrap());
        assert!(err.contains("is not the manifest's pin"), "{err}");
    }

    #[test]
    fn no_pack_for_any_live_corpus_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (seed, public) = generate_keypair().unwrap();
        let opts = fixture(tmp.path(), &seed);
        fs::rename(
            opts.packs.join("lz4-0774d055.hubpack.tar.zst"),
            opts.packs.join("lz4-deadbeef.hubpack.tar.zst"),
        )
        .unwrap();
        assert!(build(&opts, &seed, &public).is_err());
    }

    #[test]
    fn path_components_lose_only_a_real_extension() {
        assert_eq!(strip_extension("lz4frame.c"), "lz4frame");
        assert_eq!(strip_extension("a.tar.gz"), "a.tar");
        assert_eq!(strip_extension(".github"), ".github");
        assert_eq!(strip_extension("lib"), "lib");
        assert_eq!(
            words("an Agent, x-y block_ok").collect::<Vec<_>>(),
            vec!["agent", "block"]
        );
    }

    #[test]
    fn the_cli_requires_its_four_flags() {
        assert_eq!(run(&["--registry".into(), "r".into()]), 2);
        assert_eq!(run(&["--bogus".into()]), 2);
        assert_eq!(run(&["--help".into()]), 0);
    }
}
