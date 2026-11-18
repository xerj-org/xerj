//! Corpus Hub v2 — the `xerj hub …` subcommands, `xerj ask`, and the `--def` code
//! path (plan §2 builder + client, §3 `def`, §5 `ask` / node, §8 `status`).
//!
//! Entry points, all returning a process exit code:
//!
//! - [`run_hub_cli`] dispatches `xerj hub build-pack` (CI builds a signed,
//!   pre-indexed pack), `pull` (a client downloads, verifies and restores one),
//!   `status`, `stop` (stop the automatic node) and `config hosted on|off`.
//!   `sync`, `update` and `rm` are plan §8 and are not here.
//! - [`run_ask`] is `xerj ask "<question>"` (and the bare `xerj "<question>"`
//!   form, see [`is_bare_question`]): the flags are parsed here, the answer flow
//!   itself is [`crate::ask`].
//! - [`run_code`] is the `xerj code` front door: `--def <SYMBOL>` answers from
//!   the corpus's `-defs` index via [`crate::def::lookup`] and the shared
//!   renderer; everything else delegates unchanged to the existing
//!   `xerj_autoindex::xc::run_code_cli`.
//!
//! The crate graph forbids `run_code_query` (in `xerj-common`) from calling
//! `def::lookup` (here, above `xerj-autoindex`), so the plan's "`run_code_query`
//! calls `lookup`" is realised one layer up, in `run_code`, which sees both the
//! code CLI and `def::lookup` without a dependency cycle. The user-facing
//! contract is identical: `xerj code <slug> --def <SYMBOL>` prints a def answer
//! through the §1 renderer.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use xerj_common::hub::{
    render, Answer, CatalogueEntry, LicenceClass, PackIndex, PackManifest, PackSource,
    AUTO_PULL_MAX_BYTES, DEFAULT_HUB_URL, DISK_BUDGET_BYTES, PACK_FORMAT_VERSION,
};
use xerj_common::xccode::manifest::read_hub_manifest;
use xerj_common::xccode::state;
use xerj_common::xccode::XcHttp;

use crate::ask::{self, AskArgs, HostedSwitch, ModeRequest};
use crate::catalogue::{build_catalogue, load_catalogue, CorpusBuildInput};
use crate::flock::{held_by, FileLock, LockError};
use crate::lockfile;
use crate::node::{self, resolve_url, HubNode, NodeLauncher, StopOutcome};
use crate::pack;

const HUB_USAGE: &str = "usage: xerj hub <command>

  build-pack <slug> --registry <dir> --out <dir> --url <node> --sign-key <seed-file>
        Build a signed, pre-indexed pack from a registry manifest (CI).
  build-pack <slug> --from-pack <dir> --out <dir> --url <node> --sign-key <seed-file>
        The same, from a record pack `xerj corpus build` made (rust-vulns,
        agent-session-trajectories): its pin is the first 40 hex of a sha256
        over its record files, so the same records give the same pin.
  build-catalogue --registry <dir> --packs <dir> --out <file> --sign-key <seed-file>
        Build and sign the catalogue from verified packs (CI; see -h).
  sync [--url <node>] | sync --install-timer
        Refresh the catalogue and bring every cached pack up to its pin;
        --install-timer schedules that every 6 hours (see `xerj hub sync -h`).
  pull <slug> [--quiet] [--url <node>]
        Download, verify and restore a pack, then `xerj code <slug>` works.
        Exits 2 for an unknown corpus, 3 when the pack fails verification, 5 when
        this build has no hub public key to verify with (nothing is downloaded),
        1 for anything else (an excluded licence and a pack over the size limit
        are refused here too).
  find \"<question>\" [--k N] [--json]
        Which corpora could answer, best first. Each line names the upstream
        repo (owner/name, when the catalogue has it) and the one-line description
        of what that pack is. A query that names a different project of the same
        slug (fabric.js) exits 4 instead of matching silently. A question no
        corpus covers exits 1 (not 4) and names `xerj autoindex`.
  feedback <slug> --usefulness <1-5> --found <yes|partial|no> [...]
        Rate a corpus after using it: one structured review (no query text) for
        its Agent score on hub.xerj.org (see `xerj hub feedback -h`).
  status
        Cached packs, the automatic node, the hosted setting, catalogue age.
  stop
        Stop the automatic node `xerj ask` started (only that process).
  config hosted on|off|auto
        Whether questions may be sent to the hosted hub (default auto: off
        inside a git repository, on elsewhere).

Ask a question with `xerj ask \"<question>\"` (see `xerj ask --help`).

Packs are pulled on demand and never cloned or indexed on the user's machine
(docs/CORPUS_HUB_V2_IMPLEMENTATION.md).";

/// `xerj hub <command> …`.
fn hub_verb(arg: &str) -> bool {
    matches!(
        arg,
        "build-pack"
            | "build-catalogue"
            | "sync"
            | "pull"
            | "find"
            | "feedback"
            | "status"
            | "stop"
            | "config"
            | "help"
            | "-h"
            | "--help"
    )
}

pub fn run_hub_cli(args: &[String]) -> i32 {
    let promoted = xerj_common::argv::reorder_flags_before_command(args, hub_verb);
    let args: &[String] = promoted.as_deref().unwrap_or(args);
    let Some(sub) = args.first() else {
        eprintln!("{HUB_USAGE}");
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "build-pack" => run_build_pack(rest),
        "build-catalogue" => crate::build_catalogue::run(rest),
        "sync" => crate::sync::run_sync(rest),
        "pull" => run_pull(rest),
        "find" => run_find_cmd(rest),
        "feedback" => crate::feedback::run_feedback(rest),
        "status" => run_status(rest),
        "stop" => run_stop(rest),
        "config" => run_config(rest),
        "-h" | "--help" | "help" => {
            println!("{HUB_USAGE}");
            0
        }
        other => {
            eprintln!("xerj hub: unknown command '{other}'\n\n{HUB_USAGE}");
            2
        }
    }
}

// ── xerj hub build-pack ──────────────────────────────────────────────────────

fn run_build_pack(args: &[String]) -> i32 {
    let mut slug: Option<String> = None;
    let mut registry: Option<String> = None;
    let mut from_pack: Option<String> = None;
    let mut out: Option<String> = None;
    let mut url: Option<String> = None;
    let mut sign_key: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        macro_rules! val {
            ($n:expr) => {
                match args.get(i + 1) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => {
                        eprintln!("xerj hub build-pack: {} needs a value\n\n{HUB_USAGE}", $n);
                        return 2;
                    }
                }
            };
        }
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{HUB_USAGE}");
                return 0;
            }
            "--registry" => registry = Some(val!("--registry")),
            "--from-pack" => from_pack = Some(val!("--from-pack")),
            "--out" => out = Some(val!("--out")),
            "--url" => url = Some(val!("--url")),
            "--sign-key" => sign_key = Some(val!("--sign-key")),
            a if a.starts_with('-') => {
                eprintln!("xerj hub build-pack: unknown flag '{a}'\n\n{HUB_USAGE}");
                return 2;
            }
            a => {
                if slug.is_none() {
                    slug = Some(a.to_string());
                } else {
                    eprintln!("xerj hub build-pack: one <slug>, not two\n\n{HUB_USAGE}");
                    return 2;
                }
            }
        }
        i += 1;
    }
    // `--url` is REQUIRED for the builder (plan §2 shows it without brackets).
    // It must never fall through to resolve_url's localhost:9200 default — the
    // builder clones + indexes + snapshots into the node it is handed, and a
    // silent default would mutate whatever shared node happens to answer there.
    let source = match (registry, from_pack) {
        (Some(r), None) => BuildSource::Registry(r),
        (None, Some(p)) => BuildSource::RecordPack(p),
        (Some(_), Some(_)) => {
            eprintln!(
                "xerj hub build-pack: --registry and --from-pack name two different sources; \
                 pass one\n\n{HUB_USAGE}"
            );
            return 2;
        }
        (None, None) => {
            eprintln!(
                "xerj hub build-pack: <slug>, --registry (or --from-pack), --out, --url and \
                 --sign-key are all required\n\n{HUB_USAGE}"
            );
            return 2;
        }
    };
    let (Some(slug), Some(out), Some(sign_key), Some(url)) = (slug, out, sign_key, url) else {
        eprintln!(
            "xerj hub build-pack: <slug>, --registry (or --from-pack), --out, --url and \
             --sign-key are all required\n\n{HUB_USAGE}"
        );
        return 2;
    };
    match build_pack(&slug, &source, &out, &url, &sign_key) {
        Ok(summary) => {
            println!("{summary}");
            0
        }
        Err(e) => {
            eprintln!("xerj hub build-pack: {e:#}");
            1
        }
    }
}

/// Build the pack and return the one-line summary `pack <slug> <pin8> <bytes>
/// <indices> <defs>` (plan §2 step 6).
/// What `build-pack` builds from.
enum BuildSource {
    /// `--registry <dir>`: the git manifest `<dir>/<slug>.json`.
    Registry(String),
    /// `--from-pack <dir>`: a record pack (`xerj corpus build` output).
    RecordPack(String),
}

fn build_pack(
    slug: &str,
    source: &BuildSource,
    out: &str,
    url: &str,
    sign_key_file: &str,
) -> Result<String> {
    let seed = fs::read_to_string(sign_key_file)
        .with_context(|| format!("read sign key {sign_key_file}"))?
        .trim()
        .to_string();

    // 1. Licence, pin and sources — from the EXISTING parsers (no second parser
    //    for the registry manifest, plan §11): `read_hub_manifest` for the
    //    per-repo sources, and the catalogue builder for the restrictive-first
    //    licence class + pin date it already computes. A record pack's come from
    //    its own verified manifest (`harvest::pack::read_manifest`).
    let (meta, add_from) = match source {
        BuildSource::Registry(registry) => {
            let manifest_path = Path::new(registry).join(format!("{slug}.json"));
            if !manifest_path.is_file() {
                bail!("no registry manifest at {}", manifest_path.display());
            }
            let meta = resolve_pack_metadata(&manifest_path, &seed)
                .with_context(|| format!("read licence/pin from {}", manifest_path.display()))?;
            (meta, manifest_path)
        }
        BuildSource::RecordPack(dir) => {
            let dir = PathBuf::from(dir);
            let meta = record_pack_metadata(&dir, slug)
                .with_context(|| format!("read record pack {}", dir.display()))?;
            (meta, dir)
        }
    };
    let pin8 = pin8_of(&meta.pin_sha)?;

    // 2. Clone (or materialize the records) + index into the builder node (the
    //    SAME lifecycle the CLI runs).
    let rc = xerj_autoindex::xc::run_corpus_cli(&[
        "add".into(),
        slug.into(),
        "--from".into(),
        add_from.to_string_lossy().into_owned(),
    ]);
    if rc != 0 {
        bail!("`corpus add` failed (exit {rc})");
    }
    let rc = xerj_autoindex::xc::run_corpus_cli(&[
        "index".into(),
        slug.into(),
        "--url".into(),
        url.into(),
    ]);
    if rc != 0 {
        bail!("`corpus index` failed (exit {rc})");
    }

    let root = xerj_autoindex::xc::code_root();
    let st = state::load_state(&root, slug).map_err(|e| anyhow!("{e}"))?;
    let index_prefix = state::query_prefix(&st);

    // 3. Defs + file inventory from the cloned tree (plan §3 extraction).
    let corpus_root = root.join("corpora").join(slug);
    let licence_map = xerj_common::xccode::manifest::licence_map(&root, slug);
    let overall_licence = meta.licence_spdx.clone();
    let licence_of = move |rel: &str| {
        // The file's repo is its top-level directory under the corpus root.
        rel.split('/')
            .next()
            .and_then(|top| licence_map.get(top))
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| overall_licence.clone())
    };
    let (defs, files) = match source {
        BuildSource::Registry(_) => pack::build_defs_and_files(&corpus_root, &licence_of),
        BuildSource::RecordPack(_) => pack::record_defs_and_files(&corpus_root, &licence_of),
    }
    .with_context(|| format!("extract defs from {}", corpus_root.display()))?;

    // 4. Build + index the `-defs` index (plan §3 mapping) on the builder node.
    let node = HubNode::connect_with_key(url, None)?;
    node.wait_until_reachable(Duration::from_secs(30))
        .context("builder node not reachable")?;
    let defs_index = format!("{index_prefix}-defs");
    create_defs_index(&node, &defs_index)?;
    index_defs(&node, &defs_index, &defs)?;
    node.request_ok("POST", &format!("/{defs_index}/_refresh"), None)
        .ok();

    // 5. Snapshot `<index_prefix>-*` (docs + defs) into <out>/<slug>/snapshot.
    fs::create_dir_all(out).with_context(|| format!("create {out}"))?;
    let out_canon = fs::canonicalize(out).with_context(|| format!("canonicalize {out}"))?;
    let snapshot_location = out_canon.join(slug).join("snapshot");
    let snapshot_name = format!("{slug}-{pin8}");
    register_repo(&node, "hub-build", &snapshot_location)?;
    node.request_ok(
        "PUT",
        &format!("/_snapshot/hub-build/{snapshot_name}?wait_for_completion=true"),
        Some(&json!({ "indices": format!("{index_prefix}-*") })),
    )
    .context("create snapshot")?;

    // 6. Assemble pack.json, write + sign the archive.
    let indices = pack_indices(&node, &index_prefix)?;
    let manifest = PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: slug.to_string(),
        pin_sha: meta.pin_sha.clone(),
        pin_date: meta.pin_date.clone(),
        built_at: now_rfc3339(),
        builder_version: format!("xerj {}", env!("CARGO_PKG_VERSION")),
        index_prefix: index_prefix.clone(),
        indices,
        snapshot_name: snapshot_name.clone(),
        licence_spdx: meta.licence_spdx.clone(),
        licence_class: meta.licence_class,
        sources: meta.sources.clone(),
        defs_count: defs.len() as u64,
        files_count: files.len() as u64,
    };
    let n_indices = manifest.indices.len();
    let out_archive = out_canon.join(format!("{slug}-{pin8}.hubpack.tar.zst"));
    let (_sha, bytes) = pack::write_pack(
        &out_archive,
        &manifest,
        &defs,
        &files,
        &snapshot_location,
        &seed,
    )
    .context("write pack archive")?;

    Ok(format!(
        "pack {slug} {pin8} {bytes} {n_indices} {}",
        defs.len()
    ))
}

/// Pack licence/pin/sources, resolved from the registry manifest via existing
/// parsers only.
struct PackMetadata {
    pin_sha: String,
    pin_date: String,
    licence_spdx: String,
    licence_class: LicenceClass,
    sources: Vec<PackSource>,
}

/// Read the pack's licence class + pin date through the catalogue builder (it
/// already applies the restrictive-first rule over the manifest's review
/// blocks) and the per-repo sources through `read_hub_manifest` — both existing
/// parsers, so the registry manifest is never parsed by a second reader
/// (plan §11). `seed` is only used because `build_catalogue` signs its output;
/// the signature is discarded.
fn resolve_pack_metadata(manifest_path: &Path, seed: &str) -> Result<PackMetadata> {
    let input = CorpusBuildInput {
        manifest_path: manifest_path.to_path_buf(),
        domain: String::new(),
        refresh_cadence_days: 0,
        pack_bytes: 0,
        pack_url: String::new(),
        pack_sha256: String::new(),
        sig_url: String::new(),
        quality_grade: None,
        terms: Vec::new(),
        pin_sha: String::new(),
        ask_for: String::new(),
    };
    let (cat, _sig) = build_catalogue(&[input], "", seed)?;
    let entry = cat
        .corpora
        .first()
        .ok_or_else(|| anyhow!("catalogue builder produced no entry"))?;

    let hub = read_hub_manifest(manifest_path).map_err(|e| anyhow!("{e}"))?;
    let sources = hub
        .rows
        .iter()
        .map(|r| PackSource {
            repo: r.repo.clone(),
            url: r.url.clone(),
            sha: r.sha.clone(),
            licence: r.declared_licence.clone(),
        })
        .collect();

    Ok(PackMetadata {
        pin_sha: entry.pin_sha.clone(),
        pin_date: entry.pin_date.clone(),
        licence_spdx: entry.licence_spdx.clone(),
        licence_class: entry.licence_class,
        sources,
    })
}

/// Pack metadata for a record pack (`xerj corpus build` output), from its own
/// manifest after `harvest::pack::read_manifest` has re-hashed every file
/// SHA256SUMS names.
///
/// - pin: a record pack has no commit, so its pin is a CONTENT pin: the first
///   40 hex of sha256 over `"<file>  <sha256>\n"` for each record file, in name
///   order. The same records give the same pin whatever the build date, and a
///   changed record gives a new one (the hub's release-per-pin rule then
///   applies unchanged). 40 hex keeps every pin the same shape.
/// - pin_date: the pack's `created_at` date.
/// - licence: each source's recorded SPDX; the class from the repo's one
///   licence policy (`xccode::licence::is_restricted`, which also counts
///   UNKNOWN as restricted). A source with no licence is refused.
fn record_pack_metadata(dir: &Path, slug: &str) -> Result<PackMetadata> {
    let meta = xerj_autoindex::harvest::pack::read_manifest(dir)?;
    if meta.name != slug {
        bail!("the pack is '{}', not '{slug}'", meta.name);
    }
    let raw: Value = serde_json::from_str(
        &fs::read_to_string(dir.join("manifest.json")).context("read manifest.json")?,
    )
    .context("parse manifest.json")?;
    let mut listing = String::new();
    for f in &meta.files {
        // read_manifest checked each of these is 64 hex and matches the bytes.
        let sha = raw
            .pointer(&format!(
                "/files/{}/sha256",
                f.name.replace('~', "~0").replace('/', "~1")
            ))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("manifest.json lists no sha256 for {}", f.name))?;
        listing.push_str(&format!("{}  {sha}\n", f.name));
    }
    if meta.files.is_empty() {
        bail!("the pack has no record files");
    }
    let digest = Sha256::digest(listing.as_bytes());
    let pin_sha: String = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..40]
        .to_string();
    let pin_date = raw
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|c| c.get(0..10))
        .ok_or_else(|| anyhow!("manifest.json has no created_at date"))?
        .to_string();

    let mut spdx: Vec<String> = Vec::new();
    for s in &meta.sources {
        let lic = s.licence.trim();
        if lic.is_empty() {
            bail!("source '{}' records no licence", s.slug);
        }
        if !spdx.iter().any(|l| l == lic) {
            spdx.push(lic.to_string());
        }
    }
    if spdx.is_empty() {
        bail!("the pack declares no sources");
    }
    let licence_class = if spdx
        .iter()
        .any(|l| xerj_common::xccode::licence::is_restricted(l))
    {
        LicenceClass::ApproachOnly
    } else {
        LicenceClass::AdaptWithAttribution
    };
    Ok(PackMetadata {
        pin_sha,
        pin_date,
        licence_spdx: spdx.join("/"),
        licence_class,
        sources: meta
            .sources
            .iter()
            .map(|s| PackSource {
                repo: s.slug.clone(),
                url: s.url.clone().unwrap_or_default(),
                sha: s.watermark.clone().unwrap_or_default(),
                licence: s.licence.clone(),
            })
            .collect(),
    })
}

/// `PUT /<index>` with the plan's §3 defs mapping.
fn create_defs_index(node: &HubNode, index: &str) -> Result<()> {
    let body = json!({
        "mappings": { "properties": {
            "symbol":      { "type": "keyword" },
            "symbol_text": { "type": "text" },
            "kind":        { "type": "keyword" },
            "lang":        { "type": "keyword" },
            "file":        { "type": "keyword" },
            "line":        { "type": "integer" },
            "text":        { "type": "text" },
            "value":       { "type": "keyword" }
        }}
    });
    node.request_ok("PUT", &format!("/{index}"), Some(&body))
        .with_context(|| format!("create defs index {index}"))?;
    Ok(())
}

/// Most index actions a single defs `_bulk` request may carry. A large code
/// corpus has far more symbols than the engine's `max_actions_per_bulk`
/// (default 50 000 — biome alone extracts 107 317), and the engine rejects an
/// over-cap bulk *whole*, with a per-item 413 (`exceeds max_actions_per_bulk`),
/// so the builder must split. 10 000 actions per request mirrors the autoindex
/// client's `BULK_MAX_ACTIONS` (xerj-autoindex `esclient.rs`) and keeps each
/// request's intermediate heap bounded on the small CI builder node.
const DEFS_BULK_CHUNK: usize = 10_000;

/// Stable, non-empty `_id` for a def document, derived from WHERE it lives
/// (`file` + `line`) and WHAT it is (`kind` + `symbol`) through the same hash
/// the ingest path uses for its doc ids (`xerj_autoindex::ids::doc_id`). Two
/// reasons this is mandatory, not cosmetic:
///
///   * The builder node must never receive an EMPTY `_id`. The old action was
///     `{"index":{}}` (no `_id`); on the backfill node that minted an empty id
///     on the `-defs` bulk, so the FIRST def indexed under `""` and every later
///     def collided on it — the bulk came back `errors:true` and the whole pack
///     build failed (agno + ~6 corpora, run 39240121636). A hash is non-empty
///     by construction: even a def with an empty symbol still hashes its
///     file+line+kind, so no def is dropped and none yields `""`.
///   * A deterministic id makes a pack rebuild idempotent — a re-run overwrites
///     each def in place instead of appending a second copy under a fresh id.
///
/// `definitions()` already drops captures with an empty name, so an empty
/// `symbol` should not occur in practice; synthesising (rather than skipping)
/// keeps this robust even if that upstream guard ever changes.
fn def_doc_id(d: &xerj_common::hub::DefRecord) -> String {
    let locator = format!("{}\0{}\0{}", d.line, d.kind, d.symbol);
    xerj_autoindex::ids::doc_id("defs", &d.file, &locator)
}

/// The `_bulk` NDJSON body for one batch of the `-defs` index: an `index`
/// action carrying a stable, non-empty `_id` ([`def_doc_id`]) followed by the
/// source doc, per def (`symbol_text` mirrors `symbol`, analysed). Pure, so the
/// empty-`_id` regression is tested without a node
/// (`defs_bulk_never_emits_empty_id`).
fn defs_bulk_ndjson(defs: &[xerj_common::hub::DefRecord]) -> String {
    let mut ndjson = String::with_capacity(defs.len() * 200);
    for d in defs {
        let action = json!({ "index": { "_id": def_doc_id(d) } });
        ndjson.push_str(&action.to_string());
        ndjson.push('\n');
        let doc = json!({
            "symbol": d.symbol,
            "symbol_text": d.symbol,
            "kind": d.kind,
            "lang": d.lang,
            "file": d.file,
            "line": d.line,
            "text": d.text,
            "value": d.value,
        });
        ndjson.push_str(&doc.to_string());
        ndjson.push('\n');
    }
    ndjson
}

/// Serialize `defs` into NDJSON `_bulk` bodies of at most `chunk` index actions
/// each ([`defs_bulk_ndjson`] per batch: one action line carrying a stable
/// `_id` plus one document line). Returns one body per chunk; empty input
/// yields no bodies, hence no request.
fn defs_bulk_bodies(defs: &[xerj_common::hub::DefRecord], chunk: usize) -> Vec<String> {
    let chunk = chunk.max(1);
    defs.chunks(chunk).map(defs_bulk_ndjson).collect()
}

/// Bulk-index every def document, split into [`DEFS_BULK_CHUNK`]-action
/// requests so a corpus with more symbols than the engine's
/// `max_actions_per_bulk` is not rejected whole.
fn index_defs(node: &HubNode, index: &str, defs: &[xerj_common::hub::DefRecord]) -> Result<()> {
    for ndjson in defs_bulk_bodies(defs, DEFS_BULK_CHUNK) {
        let resp = node.bulk_ndjson(&format!("/{index}/_bulk"), ndjson)?;
        if resp.get("errors").and_then(Value::as_bool) == Some(true) {
            bail!("defs bulk index reported errors: {resp}");
        }
    }
    Ok(())
}

/// `PUT /_snapshot/<repo>` with the fs location.
fn register_repo(node: &HubNode, repo: &str, location: &Path) -> Result<()> {
    let body = json!({
        "type": "fs",
        "settings": { "location": location.to_string_lossy() }
    });
    node.request_ok("PUT", &format!("/_snapshot/{repo}"), Some(&body))
        .with_context(|| format!("register snapshot repo {repo}"))?;
    Ok(())
}

/// The `-*` indices as `PackIndex` rows (name/docs/bytes), from `_cat/indices`.
fn pack_indices(node: &HubNode, prefix: &str) -> Result<Vec<PackIndex>> {
    let path = format!("/_cat/indices/{prefix}-*?format=json&h=index,docs.count,store.size.bytes");
    let v = node
        .request_ok("GET", &path, None)
        .with_context(|| format!("list indices {prefix}-*"))?;
    let mut out = Vec::new();
    for row in v.as_array().cloned().unwrap_or_default() {
        let name = row
            .get("index")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        out.push(PackIndex {
            name,
            docs: num_field(&row, "docs.count"),
            bytes: num_field(&row, "store.size.bytes"),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read a `_cat` numeric column that may arrive as a number or a string.
fn num_field(row: &Value, key: &str) -> u64 {
    match row.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

// ── xerj hub pull ────────────────────────────────────────────────────────────

fn run_pull(args: &[String]) -> i32 {
    let mut slug: Option<String> = None;
    let mut url: Option<String> = None;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{HUB_USAGE}");
                return 0;
            }
            "--quiet" => quiet = true,
            // Read by xerj_common::feedback::enabled() from the whole argv.
            "--disable-feedback" => {}
            "--url" => match args.get(i + 1) {
                Some(v) => {
                    url = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("xerj hub pull: --url needs a value\n\n{HUB_USAGE}");
                    return 2;
                }
            },
            a if a.starts_with('-') => {
                eprintln!("xerj hub pull: unknown flag '{a}'\n\n{HUB_USAGE}");
                return 2;
            }
            a => {
                if slug.is_none() {
                    slug = Some(a.to_string());
                } else {
                    eprintln!("xerj hub pull: one <slug>, not two\n\n{HUB_USAGE}");
                    return 2;
                }
            }
        }
        i += 1;
    }
    let Some(slug) = slug else {
        eprintln!("xerj hub pull: <slug> is required\n\n{HUB_USAGE}");
        return 2;
    };
    // Everything that can refuse this pull — a malformed or unknown slug, an
    // excluded licence, a pack larger than the whole disk budget, no key to verify
    // it with — is decided BEFORE a node is looked for or started. A doomed pull
    // used to boot a whole node first, and only then say "no". This is the explicit
    // command, so the ceiling is the disk budget, not the 50 MiB auto-pull cap: the
    // person typing it has named exactly this pack.
    let plan = match plan_pull(&slug, DISK_BUDGET_BYTES) {
        Ok(p) => p,
        Err(PullError { code, message }) => {
            eprintln!("xerj hub pull: {message}");
            return code;
        }
    };
    // Unlike build-pack, pull's --url is optional: `--url` > `XERJ_URL` >
    // `localhost:9200`, and when none of those answers, the automatic node
    // (plan §5) is started or reused.
    let launcher = match NodeLauncher::current() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("xerj hub pull: {e:#}");
            return 1;
        }
    };
    let resolved = match node::ensure_node(&hub_home(), url.as_deref(), &launcher) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xerj hub pull: {e:#}");
            return 1;
        }
    };
    let started = std::time::Instant::now();
    let outcome = pull_with(&plan, &resolved.url, resolved.managed, quiet);
    // One automatic pull note per corpus per week (worked? slow?) and the line
    // that asks the agent to rate the corpus once it has used it. Feedback off
    // (--disable-feedback / XERJ_DISABLE_FEEDBACK) sends and prints nothing.
    if !matches!(outcome, Ok(Pulled::Already)) {
        crate::feedback::auto_after_pull(&slug, &plan.pin8, outcome.is_err(), started.elapsed());
    }
    let ask_to_rate = || {
        if !quiet && xerj_common::feedback::enabled() {
            eprintln!("{}", xerj_common::corpus_feedback::AUTO_SEND_LINE);
        }
    };
    match outcome {
        Ok(Pulled::Already) => {
            if !quiet {
                println!("{slug}: already present");
            }
            ask_to_rate();
            0
        }
        Ok(Pulled::Done(line)) => {
            if !quiet {
                println!("{line}");
            }
            ask_to_rate();
            0
        }
        Err(PullError { code, message }) => {
            eprintln!("xerj hub pull: {message}");
            code
        }
    }
}

pub(crate) enum Pulled {
    Already,
    Done(String),
}

/// A pull failure carrying an exit code the caller can act on: **2** = the slug
/// is unknown or malformed; **3** = the pack failed verification (sha /
/// signature / format); **5** = this build has no hub public key, so nothing can
/// be verified (and nothing was downloaded); **1** = an infrastructure or policy
/// failure (catalogue load, the licence or size policy, node connect, a pull
/// already in progress, the download, the restore, or writing the ledger).
/// Keeping 2, 3 and 5 exact lets a caller tell "wrong name", "untrusted bytes"
/// and "this binary cannot verify anything" apart from "the machine or network
/// got in the way".
#[derive(Debug)]
pub(crate) struct PullError {
    pub(crate) code: i32,
    pub(crate) message: String,
}
impl PullError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        PullError {
            code,
            message: message.into(),
        }
    }
}

/// Infrastructure/policy failure — not a slug or a verification problem.
const E_INFRA: i32 = 1;
/// The slug is unknown or malformed.
const E_SLUG: i32 = 2;
/// The pack failed verification (sha / signature / format).
const E_VERIFY: i32 = 3;
/// No hub public key is configured: the pack was never even fetched. Its own code,
/// because it is not a verification FAILURE (the bytes were not judged and found
/// wanting) and not a transient one (retrying cannot help): the build is missing
/// the key.
const E_NOKEY: i32 = ask::EXIT_NO_KEY;

/// Largest signature file pull will download — an ed25519 sig is 128 hex chars,
/// so this caps a hostile hub's "signature" far below the pack cap.
const SIG_MAX_BYTES: u64 = 4096;
/// How long a pull waits for another pull of the same pin to finish before it
/// gives up. A pull is a download of at most the planned ceiling
/// ([`AUTO_PULL_MAX_BYTES`] for an automatic pull, [`DISK_BUDGET_BYTES`] for an
/// explicit one) plus a restore, so this is generous; it exists so a wedged holder
/// cannot hang a caller (an agent's tool call) for ever.
const PULL_LOCK_WAIT: Duration = Duration::from_secs(60);

/// Cross-process exclusion for one `(slug, pin)`: an OS lock on `<pin8>.lock`
/// (see [`crate::flock`]). A second concurrent pull of the same pin waits for the
/// first (bounded by [`PULL_LOCK_WAIT`]) instead of racing it over the shared
/// tmp/extract directories — and then finds the pack present.
///
/// The lock belongs to the process holding it: it is released the moment that
/// process exits, however it exits. This used to be an atomically created
/// directory that was only ever removed by the pull's own destructor, so a pull
/// killed with SIGKILL, an OOM or an agent's tool timeout left the corpus
/// un-pullable for thirty minutes, with no owner recorded and a message that
/// named no path.
struct PullLock {
    _lock: FileLock,
}
impl PullLock {
    fn acquire(
        pack_dir: &Path,
        slug: &str,
        pin8: &str,
        wait: Duration,
        quiet: bool,
    ) -> Result<PullLock, PullError> {
        fs::create_dir_all(pack_dir)
            .map_err(|e| PullError::new(E_INFRA, format!("create {}: {e}", pack_dir.display())))?;
        let path = pack_dir.join(format!("{pin8}.lock"));
        let contended = |holder| {
            PullError::new(
                E_INFRA,
                format!(
                    "a pull of '{slug}' ({pin8}) is already in progress (lock {}{}) and was still \
                     running after {}s; wait for it to finish — the lock is released the moment \
                     that process exits, however it exits",
                    path.display(),
                    held_by(holder),
                    wait.as_secs()
                ),
            )
        };
        let infra = |e: std::io::Error| {
            PullError::new(
                E_INFRA,
                format!("cannot take the pull lock {}: {e}", path.display()),
            )
        };
        // One immediate try, so the caller is only told it is waiting when it is.
        match FileLock::acquire(&path, Duration::ZERO) {
            Ok(lock) => return Ok(PullLock { _lock: lock }),
            Err(LockError::Io(e)) => return Err(infra(e)),
            Err(LockError::Timeout { .. }) => {}
        }
        if !quiet {
            eprintln!(
                "xerj hub: another pull of '{slug}' ({pin8}) is running; waiting up to {}s for it \
                 (lock {})",
                wait.as_secs(),
                path.display()
            );
        }
        match FileLock::acquire(&path, wait) {
            Ok(lock) => Ok(PullLock { _lock: lock }),
            Err(LockError::Timeout { holder }) => Err(contended(holder)),
            Err(LockError::Io(e)) => Err(infra(e)),
        }
    }
}

/// What a pull has decided before it touches a node or the disk: the catalogue
/// entry, the pin its files are named by, the key it will verify with, and the
/// byte ceiling this pull may download under (see [`plan_pull_entry`]).
pub(crate) struct PullPlan {
    entry: CatalogueEntry,
    pin8: String,
    pubkey: String,
    /// The largest pack this pull will accept and download. An *explicit*
    /// `xerj hub pull <slug>` passes [`DISK_BUDGET_BYTES`]; an *automatic* pull
    /// (the `ask` flow) passes [`AUTO_PULL_MAX_BYTES`]. The transfer in
    /// [`pull_with`] is bounded again by this value, so a hub that lies in
    /// `pack_bytes` still cannot write more than the ceiling the caller chose.
    max_bytes: u64,
}

/// Look `slug` up in the catalogue and decide whether it may be pulled at all
/// ([`plan_pull_entry`]). `max_bytes` is the largest pack this caller accepts —
/// [`DISK_BUDGET_BYTES`] for an explicit pull, [`AUTO_PULL_MAX_BYTES`] for an
/// automatic one.
pub(crate) fn plan_pull(slug: &str, max_bytes: u64) -> Result<PullPlan, PullError> {
    // 0. Validate the slug BEFORE it is ever joined into a filesystem path. The
    //    catalogue is accepted UNVERIFIED when no hub pubkey is configured, so a
    //    hostile `slug` ("../../etc") must be rejected here, ahead of every fs op.
    if !valid_slug(slug) {
        return Err(PullError::new(
            E_SLUG,
            format!(
                "invalid corpus slug '{slug}' (allowed: lower-case a-z 0-9 . _ -, no '..' or '/')"
            ),
        ));
    }

    let hub_home = hub_home();
    let hub_url = std::env::var("XERJ_HUB_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_HUB_URL.to_string());

    // 1. Catalogue → the entry for this slug.
    let cat = load_catalogue(&hub_url, &hub_home, false).map_err(|e| {
        if xerj_common::hub::hub_key_configured() {
            PullError::new(E_INFRA, format!("cannot load catalogue: {e:#}"))
        } else {
            // A build with no hub key could not verify the pack either; the
            // catalogue error (often a parse error on whatever the hub URL
            // serves) is not the useful answer.
            PullError::new(E_NOKEY, xerj_common::hub::NO_HUB_KEY_MESSAGE)
        }
    })?;
    let entry = cat.corpora.iter().find(|c| c.slug == slug).ok_or_else(|| {
        PullError::new(
            E_SLUG,
            format!(
                "unknown corpus '{slug}' (catalogue generated {}); `xerj hub status` lists what is \
                 available",
                cat.generated_at
            ),
        )
    })?;
    plan_pull_entry(entry, max_bytes)
}

/// Everything that can refuse to pull `entry`, checked in the order that costs
/// least and before any node, download or temp dir exists:
///
/// 1. its slug is safe to join into a path (the matched entry's own slug drives
///    the paths, so it is re-checked even though the lookup matched on a valid one);
/// 2. its pin is a commit;
/// 3. its licence class is not `excluded` — **plan §11: an excluded corpus is never
///    pulled.** This is enforced here, in the pull itself, and not only in the
///    `ask` flow that decides whether to *start* one: `xerj hub pull <slug>` is a
///    command anybody (or any agent that was handed a hint) can type, and a policy
///    that only the caller applies is a policy the next caller forgets. There is
///    deliberately no override flag: nothing in the plan lets a command line waive
///    a licence class, and a flag an agent can read in `--help` is not a human
///    decision. If an override is ever wanted it needs its own decision and its own
///    confirmation, not a switch here;
/// 4. its advertised size is within `max_bytes` — a malicious hub must not OOM the
///    client by advertising a huge pack (decision 2). The ceiling depends on who is
///    pulling: an *automatic* pull (the `ask` flow) passes [`AUTO_PULL_MAX_BYTES`],
///    so a question never silently drags down a large pack; an *explicit*
///    `xerj hub pull <slug>` passes the larger [`DISK_BUDGET_BYTES`], because the
///    person (or agent) naming the slug has asked for exactly this pack and the hub
///    cannot keep a pack bigger than the whole cache anyway. The 50 MiB cap is on
///    the implicit case, not on the command — the notice a build prints over the
///    cap ("run `xerj hub pull <slug>`") now resolves to a command that works. The
///    transfer is bounded again in [`fetch_to`] in case the field lies;
/// 5. there is a key to verify it with (exit [`E_NOKEY`]). Resolved before any temp
///    dir exists, so a pull with no key leaves nothing to re-fetch.
pub(crate) fn plan_pull_entry(
    entry: &CatalogueEntry,
    max_bytes: u64,
) -> Result<PullPlan, PullError> {
    let slug = entry.slug.as_str();
    if !valid_slug(slug) {
        return Err(PullError::new(
            E_SLUG,
            format!("catalogue entry has an invalid slug '{slug}'"),
        ));
    }
    let pin8 = pin8_of(&entry.pin_sha)
        .map_err(|e| PullError::new(E_INFRA, format!("catalogue entry for '{slug}': {e:#}")))?;
    if entry.licence_class == LicenceClass::Excluded {
        return Err(PullError::new(
            E_INFRA,
            format!(
                "pack '{slug}' has an excluded licence class ({}) and is never pulled",
                if entry.licence_spdx.is_empty() {
                    "unknown"
                } else {
                    &entry.licence_spdx
                }
            ),
        ));
    }
    if entry.pack_bytes > max_bytes {
        let hint = if max_bytes <= AUTO_PULL_MAX_BYTES {
            "over the auto-pull cap; run `xerj hub pull` to fetch it explicitly"
        } else {
            "larger than the disk budget"
        };
        return Err(PullError::new(
            E_INFRA,
            format!(
                "pack '{slug}' is {} bytes, over the {max_bytes}-byte limit for this pull \
                 ({hint}); not downloaded",
                entry.pack_bytes
            ),
        ));
    }
    let pubkey = hub_pubkey().map_err(|e| PullError::new(E_NOKEY, format!("{e:#}")))?;
    Ok(PullPlan {
        entry: entry.clone(),
        pin8,
        pubkey,
        max_bytes,
    })
}

/// Pull the planned corpus into the node at `url`. `managed` is true for the
/// automatic node (started `--insecure`, so no credential is sent); `quiet`
/// suppresses the eviction and wait notices.
pub(crate) fn pull_with(
    plan: &PullPlan,
    url: &str,
    managed: bool,
    quiet: bool,
) -> Result<Pulled, PullError> {
    let entry = &plan.entry;
    let slug = entry.slug.as_str();
    let pin8 = plan.pin8.as_str();
    let hub_home = hub_home();

    // Disk budget (plan §5 step 6): make room BEFORE any download, evicting
    // least-recently-used packs — never this slug's. Pack directories only; the
    // node's restored indices are not touched.
    for c in ask::evict_to_budget(&hub_home, slug, entry.pack_bytes, DISK_BUDGET_BYTES) {
        if !quiet {
            eprintln!(
                "xerj hub: evicted '{}' {} ({} MB, least recently used) to stay under the {} MB \
                 disk budget",
                c.pack.slug,
                c.pack.pin8,
                ask::megabytes(c.bytes),
                ask::megabytes(DISK_BUDGET_BYTES)
            );
        }
    }

    // The node (infra if unreachable).
    let root = xerj_autoindex::xc::code_root();
    let node = if managed {
        HubNode::connect_insecure(url)
    } else {
        HubNode::connect_with_key(url, None)
    }
    .map_err(|e| PullError::new(E_INFRA, format!("connect {url}: {e:#}")))?;

    // Take the per-pin lock so a concurrent pull does not race the tmp/extract
    // dirs; released on every return below (Drop), and by the OS if this process
    // dies before it gets there.
    let pack_dir = hub_home.join("packs").join(slug);
    let _lock = PullLock::acquire(&pack_dir, slug, pin8, PULL_LOCK_WAIT, quiet)?;

    // Download pack + sig into a temp dir (size-capped at the ceiling this pull was
    // planned under — see `PullPlan::max_bytes`). NOTHING is extracted.
    let tmp = pack_dir.join(format!("{pin8}.tmp"));
    let extract = pack_dir.join(pin8);
    let _ = fs::remove_dir_all(&tmp); // a stale temp from a crash must not poison this
    fs::create_dir_all(&tmp)
        .map_err(|e| PullError::new(E_INFRA, format!("create {}: {e}", tmp.display())))?;
    let archive = tmp.join(format!("{slug}-{pin8}.hubpack.tar.zst"));
    let sig = tmp.join(format!("{slug}-{pin8}.hubpack.tar.zst.sig"));
    let dl = (|| -> Result<u64> {
        let n = fetch_to(&entry.pack_url, &archive, plan.max_bytes)
            .with_context(|| format!("download pack {}", entry.pack_url))?;
        fetch_to(&entry.sig_url, &sig, SIG_MAX_BYTES)
            .with_context(|| format!("download sig {}", entry.sig_url))?;
        Ok(n)
    })();
    let bytes = match dl {
        Ok(n) => n,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(PullError::new(E_INFRA, format!("{e:#}")));
        }
    };

    // Verify sha256 THEN signature THEN format_version, extracting nothing until
    // all pass (plan §11). Any failure: delete the temp, exit 3.
    //
    // The extraction goes to a staging directory inside `tmp`, never to `extract`
    // itself, for two reasons. A pack killed mid-extraction must not leave a
    // directory at the final path that looks complete (`pack.json`, the marker
    // `ask` and `status` go by, used to be written before `snapshot/`). And a pull
    // that fails — or is killed — must not delete the pack an earlier, successful
    // pull put at `extract`: only what THIS run created is removed on failure.
    let staging = tmp.join("extract");
    let manifest = match pack::read_and_verify_pack(
        &archive,
        &sig,
        &entry.pack_sha256,
        &plan.pubkey,
        &staging,
    ) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(PullError::new(
                E_VERIFY,
                format!("pack verification failed: {e:#}"),
            ));
        }
    };
    install_extract(&staging, &extract, &tmp)?;
    let _ = fs::remove_dir_all(&tmp);

    // Restore — unless the WHOLE index set is already live on this node. A partial
    // match (a prior partial restore, or a local `corpus index` that produced a
    // different build) must NOT read as a complete pack: skipping then would leave
    // `--def` querying a missing index and stamp the ledger "answered from: local
    // pack" over data that is not the pack. So the skip requires EVERY manifest
    // index present; a list error counts as "not present" and restores (it never
    // silently skips). Idempotent on a real full match.
    let prefix = manifest.index_prefix.clone();
    let live = node
        .cat_indices_json(&format!("{prefix}*"))
        .unwrap_or_default();
    let fully_present = !manifest.indices.is_empty()
        && manifest
            .indices
            .iter()
            .all(|idx| live.iter().any(|n| n == &idx.name));
    if !fully_present {
        let location = extract.join("snapshot");
        register_repo(&node, &format!("hub-{slug}"), &location)
            .map_err(|e| PullError::new(E_INFRA, format!("register restore repo: {e:#}")))?;
        node.request_ok(
            "POST",
            &format!(
                "/_snapshot/hub-{slug}/{}/_restore?wait_for_completion=true",
                manifest.snapshot_name
            ),
            Some(&json!({ "indices": format!("{prefix}-*") })),
        )
        .map_err(|e| {
            PullError::new(
                E_INFRA,
                restore_error_message(&format!("{e:#}"), url, &hub_home.join("packs")),
            )
        })?;
    }

    // Only now — with the full set provably present — stamp the ledger.
    state::write_hubpack_state(
        &root,
        slug,
        url,
        &manifest.index_prefix,
        &manifest.pin_sha,
        &manifest.pin_date,
        &manifest.licence_spdx,
        licence_class_str(manifest.licence_class),
    )
    .map_err(|e| PullError::new(E_INFRA, format!("write state: {e}")))?;

    if fully_present {
        return Ok(Pulled::Already);
    }
    let n_indices = manifest.indices.len();
    Ok(Pulled::Done(format!(
        "pulled {slug} {pin8} {bytes} → {n_indices} indices, {} definitions",
        manifest.defs_count
    )))
}

/// The message for a failed restore. A server that refuses the pack's location
/// (`... not in limits.snapshot_repo_allowlist`) gets the fix spelled out: by
/// default a server restores from the pack cache of the user it runs as
/// (`limits.hub_pack_restore`), so this only happens when the server does not
/// share this cache — another user, a container, another machine — or has the
/// grant switched off. The exact config line names THIS client's cache.
fn restore_error_message(err: &str, url: &str, packs: &Path) -> String {
    if !err.contains("snapshot_repo_allowlist") {
        return format!("restore snapshot: {err}");
    }
    let dir = packs.canonicalize().unwrap_or_else(|_| packs.to_path_buf());
    let dir_toml = serde_json::to_string(&dir.to_string_lossy()).unwrap_or_default();
    format!(
        "restore snapshot: the XERJ server at {url} refused to read the pack from {dir}.\n\
         A server restores pulled packs from the hub pack cache of the user it runs as \
         (limits.hub_pack_restore, on by default); this server does not share yours — it runs \
         as another user, in a container or on another machine, or has hub_pack_restore = false.\n\
         Fix, either:\n  \
         - add this line under [limits] in the server's config TOML and restart it (a container \
         must also mount the directory at the same path):\n      \
         snapshot_repo_allowlist = [{dir_toml}]\n  \
         - or let xerj start its own node: unset XERJ_URL (or point it at a free port).\n\
         server said: {err}",
        dir = dir.display()
    )
}

/// Put the verified, fully extracted `staging` directory at `dest` by renaming it —
/// one step, so `dest` is either absent or complete. A pack already at `dest`
/// (from an earlier pull of the same pin) is moved into `scratch` first and put
/// back if the rename fails; it is deleted with `scratch` once the new one is in
/// place.
fn install_extract(staging: &Path, dest: &Path, scratch: &Path) -> Result<(), PullError> {
    let previous = scratch.join("previous");
    let had_previous = dest.exists();
    if had_previous {
        fs::rename(dest, &previous).map_err(|e| {
            PullError::new(
                E_INFRA,
                format!("cannot replace the pack at {}: {e}", dest.display()),
            )
        })?;
    }
    if let Err(e) = fs::rename(staging, dest) {
        if had_previous {
            let _ = fs::rename(&previous, dest);
        }
        return Err(PullError::new(
            E_INFRA,
            format!("cannot move the verified pack into {}: {e}", dest.display()),
        ));
    }
    Ok(())
}

/// `^[a-z0-9]([a-z0-9._-]*[a-z0-9])?$` — a slug safe to join into a path: no
/// `..`, no `/`, non-empty, no leading/trailing separator. This is what stops a
/// catalogue (which may be unverified) from steering a pull's writes out of
/// `~/.xerj/hub/packs/<slug>/`.
pub fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let body = |c: u8| edge(c) || matches!(c, b'.' | b'_' | b'-');
    edge(b[0]) && edge(b[b.len() - 1]) && b.iter().all(|&c| body(c))
}

/// Download `url` to `dest`, refusing anything over `max_bytes`. Supports
/// `file://` (the tests' local packs) and `http(s)://`. For HTTP a too-large
/// `Content-Length` is rejected AND the body is read through a `max_bytes + 1`
/// bound, so a missing or lying length cannot OOM the client. Returns the number
/// of bytes written.
fn fetch_to(url: &str, dest: &Path, max_bytes: u64) -> Result<u64> {
    if let Some(path) = url.strip_prefix("file://") {
        // A regular file only. `/dev/zero` (or a FIFO, `/dev/stdin`, …) stats as
        // 0 bytes, passed the size check, and was then `fs::copy`'d without end,
        // filling the disk under `~/.xerj/hub`. The read is bounded as well, in
        // case the file grows between the stat and the read.
        let meta = fs::metadata(path).with_context(|| format!("stat {path}"))?;
        if !meta.is_file() {
            bail!("{path} is not a regular file");
        }
        let sz = meta.len();
        if sz > max_bytes {
            bail!("{path} is {sz} bytes, over the {max_bytes}-byte cap");
        }
        let mut buf = Vec::new();
        fs::File::open(path)
            .with_context(|| format!("open {path}"))?
            .take(max_bytes + 1)
            .read_to_end(&mut buf)
            .with_context(|| format!("read {path}"))?;
        if buf.len() as u64 > max_bytes {
            bail!("{path} grew past the {max_bytes}-byte cap while it was read");
        }
        fs::write(dest, &buf).with_context(|| format!("write {}", dest.display()))?;
        return Ok(buf.len() as u64);
    }
    let resp = reqwest::blocking::get(url).with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("GET {url} -> HTTP {status}");
    }
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            bail!("{url} advertises {len} bytes, over the {max_bytes}-byte cap");
        }
    }
    let mut buf = Vec::new();
    resp.take(max_bytes + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("read body {url}"))?;
    if buf.len() as u64 > max_bytes {
        bail!("{url} body exceeds the {max_bytes}-byte cap");
    }
    fs::write(dest, &buf).with_context(|| format!("write {}", dest.display()))?;
    Ok(buf.len() as u64)
}

// ── xerj hub status / stop / config ──────────────────────────────────────────

/// `xerj hub status`: cached packs, the node, the hosted setting, last sync and
/// catalogue age — plan §8's report, one line per item. A node record whose
/// process is gone is cleared first so it is not reported as running.
fn run_status(args: &[String]) -> i32 {
    if let Some(code) = no_extra_args("status", args) {
        return code;
    }
    status_in(&hub_home())
}

fn status_in(home: &Path) -> i32 {
    node::reap_stale(home);
    println!(
        "{}",
        lockfile::render_status(&lockfile::gather_status(home))
    );
    0
}

/// `xerj hub stop`: stop the node `xerj ask` started — exactly the PID recorded
/// in `~/.xerj/hub/node.pid`, after confirming it is still that node.
fn run_stop(args: &[String]) -> i32 {
    if let Some(code) = no_extra_args("stop", args) {
        return code;
    }
    stop_in(&hub_home())
}

fn stop_in(home: &Path) -> i32 {
    match node::stop_node(home) {
        Ok(StopOutcome::NothingRecorded) => {
            println!("no hub node was started by xerj; nothing to stop");
            0
        }
        Ok(StopOutcome::StaleRemoved { pid }) => {
            println!("the recorded hub node (pid {pid}) is no longer running; removed its record");
            0
        }
        Ok(StopOutcome::Stopped { pid }) => {
            println!("stopped the hub node (pid {pid})");
            0
        }
        Err(e) => {
            eprintln!("xerj hub stop: {e:#}");
            1
        }
    }
}

/// `-h` prints the usage; any other argument to a no-argument command is a
/// usage error. `None` means "carry on".
fn no_extra_args(cmd: &str, args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        None => None,
        Some("-h" | "--help") => {
            println!("{HUB_USAGE}");
            Some(0)
        }
        Some(other) => {
            eprintln!("xerj hub {cmd}: unexpected argument '{other}'\n\n{HUB_USAGE}");
            Some(2)
        }
    }
}

/// `xerj hub find "<question>" [--k N] [--json]`. Same decision as
/// [`ask::run_find`] (and the MCP `xerj_hub_find` tool).
fn run_find_cmd(args: &[String]) -> i32 {
    let mut k = ask::FIND_DEFAULT_K;
    let mut as_json = false;
    let mut words: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{HUB_USAGE}");
                return 0;
            }
            "--json" => as_json = true,
            "--k" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("xerj hub find: --k needs a value\n\n{HUB_USAGE}");
                    return ask::EXIT_USAGE;
                };
                match v.parse::<usize>() {
                    Ok(n) if (1..=ask::FIND_MAX_K).contains(&n) => k = n,
                    _ => {
                        eprintln!(
                            "xerj hub find: --k must be an integer from 1 to {}, got '{v}'\n\n{HUB_USAGE}",
                            ask::FIND_MAX_K
                        );
                        return ask::EXIT_USAGE;
                    }
                }
            }
            f if f.starts_with('-') => {
                eprintln!("xerj hub find: unknown flag '{f}'\n\n{HUB_USAGE}");
                return ask::EXIT_USAGE;
            }
            word => words.push(word),
        }
        i += 1;
    }
    let question = words.join(" ");
    if question.trim().is_empty() {
        eprintln!("xerj hub find: a question is required\n\n{HUB_USAGE}");
        return ask::EXIT_USAGE;
    }
    match ask::run_find(question.trim(), k) {
        Ok(found) => {
            let text = if as_json {
                match serde_json::to_string(&found) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("xerj hub find: {e}");
                        return 1;
                    }
                }
            } else {
                ask::render_find(&found)
            };
            if found.corpora.is_empty() {
                eprintln!("{text}");
            } else {
                println!("{text}");
            }
            match found.routing {
                "different_project" | "ambiguous" => ask::EXIT_NEEDS_DECISION,
                "no_corpus" | "no_coverage" => ask::EXIT_NO_ANSWER,
                _ => 0,
            }
        }
        Err(e) => {
            eprintln!("xerj hub find: {e}");
            if xerj_common::hub::hub_key_configured() {
                1
            } else {
                ask::EXIT_NO_KEY
            }
        }
    }
}

/// `xerj hub config hosted on|off|auto`.
fn run_config(args: &[String]) -> i32 {
    config_in(&hub_home(), args)
}

fn config_in(home: &Path, args: &[String]) -> i32 {
    let usage = |msg: &str| {
        eprintln!("xerj hub config: {msg}\n\n{HUB_USAGE}");
        2
    };
    let value = match args {
        [key, value] if key == "hosted" => value.as_str(),
        [first, ..] if first == "-h" || first == "--help" => {
            println!("{HUB_USAGE}");
            return 0;
        }
        _ => return usage("expected `hosted on|off|auto`"),
    };
    let stored = match value {
        "on" | "off" => Some(value),
        "auto" => None,
        other => return usage(&format!("hosted must be on, off or auto, not '{other}'")),
    };
    match ask::set_config_hosted(home, stored) {
        Ok(()) => {
            println!("hosted = {value}");
            0
        }
        Err(e) => {
            eprintln!("xerj hub config: {e:#}");
            1
        }
    }
}

// ── xerj ask ─────────────────────────────────────────────────────────────────

const ASK_USAGE: &str = "usage: xerj ask \"<question>\" [options]
       xerj \"<question>\"            (the same, when the question contains a space)

Answers from a hub corpus - pulling its pre-indexed pack the first time - and
always names the corpus, the pinned revision and the licence.

options:
  --corpus <slug>        skip routing and ask this corpus
  --def <SYMBOL>         exact definition lookup (file:line, declaration, value)
  --how                  a how-does-it-work question (ranked passages)
  --files                list the relevant files with their licences
  --offline              never touch the network (cached catalogue and packs only)
  --no-fetch             do not download a pack for next time
  --hosted on|off|auto   may the question be sent to the hosted hub? (default
                         auto: off inside a git repository, on elsewhere)
  --max-tokens N         cap the answer at about N tokens
  --json                 print the answer as JSON
  --url <node>           the node that holds the packs (default: XERJ_URL, then
                         localhost:9200, else a local node is started)

exit codes: 0 answered; 1 no match, nothing to answer from, or no hub corpus
covers the topic (index it locally: xerj autoindex <path>, then xerj code or
xerj search; xerj hub find lists what the hub does have); 2 usage error or
unknown corpus; 3 a pack failed verification; 4 more than one corpus actually
matches and none clearly wins (a few candidates are printed, best first;
re-run with --corpus <slug>). A weak overlap, or a tie while the question
names a project no corpus contains, is exit 1, not 4; 5 this build has no hub
public key, so no pack can be verified (nothing was downloaded); 6 the hub
catalogue could not be loaded (hub unreachable, no signed catalogue served, or
--offline with none cached): a hub problem, not an answer about any corpus";

/// The first argument of every real `xerj` subcommand.
pub const KNOWN_SUBCOMMANDS: &[&str] = &[
    "index",
    "search",
    "batch-search",
    "def",
    "init",
    "gain",
    "autoindex",
    "brain",
    "share",
    "feedback",
    "memory",
    "code",
    "corpus",
    "hub",
    "ask",
    "mcp",
];

/// True for `xerj "<question>"`: `argv[1]` is not a known subcommand, does not
/// start with `-`, and contains a space (plan §5).
pub fn is_bare_question(argv1: Option<&str>) -> bool {
    match argv1 {
        Some(a) => !KNOWN_SUBCOMMANDS.contains(&a) && !a.starts_with('-') && a.contains(' '),
        None => false,
    }
}

/// `xerj ask "<question>" [options]` — also the target of the bare
/// `xerj "<question>"` form, whose arguments are the same minus the word `ask`.
pub fn run_ask(args: &[String]) -> i32 {
    let parsed = match parse_ask_args(args) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{ASK_USAGE}");
            return 0;
        }
        Err(msg) => {
            eprintln!("xerj ask: {msg}\n\n{ASK_USAGE}");
            return ask::EXIT_USAGE;
        }
    };
    let outcome = ask::run_ask(&parsed, None);
    for line in &outcome.stderr {
        eprintln!("{line}");
    }
    if !outcome.stdout.is_empty() {
        println!("{}", outcome.stdout);
    }
    outcome.exit
}

/// Parse `xerj ask`'s arguments. `Ok(None)` is `--help`. Words that are not
/// flags (or flag values) are the question, joined with spaces — so an
/// unquoted `xerj ask how does lz4 work` still asks the whole sentence.
pub fn parse_ask_args(args: &[String]) -> Result<Option<AskArgs>, String> {
    let mut a = AskArgs::new(String::new());
    let mut words: Vec<&str> = Vec::new();
    let mut mode_flags = 0usize;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "-h" | "--help" => return Ok(None),
            "--corpus" => a.corpus = Some(value("--corpus")?),
            "--def" => {
                a.mode = ModeRequest::Def(value("--def")?);
                mode_flags += 1;
            }
            "--how" => {
                a.mode = ModeRequest::How;
                mode_flags += 1;
            }
            "--files" => {
                a.mode = ModeRequest::Files;
                mode_flags += 1;
            }
            "--offline" => a.offline = true,
            "--no-fetch" => a.no_fetch = true,
            "--json" => a.json = true,
            "--hosted" => {
                a.hosted = match value("--hosted")?.as_str() {
                    "on" => HostedSwitch::On,
                    "off" => HostedSwitch::Off,
                    "auto" => HostedSwitch::Auto,
                    other => {
                        return Err(format!("--hosted must be on, off or auto, not '{other}'"))
                    }
                }
            }
            "--max-tokens" => {
                let v = value("--max-tokens")?;
                match v.parse::<usize>() {
                    Ok(n) if n >= 1 => a.max_tokens = Some(n),
                    _ => return Err(format!("--max-tokens must be a positive number, got '{v}'")),
                }
            }
            "--url" => a.url = Some(value("--url")?),
            f if f.starts_with("--") => return Err(format!("unknown flag '{f}'")),
            word => words.push(word),
        }
        i += 1;
    }
    if mode_flags > 1 {
        return Err("--def, --how and --files are mutually exclusive".to_string());
    }
    if let ModeRequest::Def(sym) = &a.mode {
        if sym.trim().is_empty() {
            return Err("--def needs a symbol".to_string());
        }
    }
    a.question = words.join(" ");
    if a.question.trim().is_empty() {
        // `--def SYM` alone is a complete request: the symbol is the question.
        if let ModeRequest::Def(sym) = &a.mode {
            a.question = sym.clone();
        } else {
            return Err("a question is required".to_string());
        }
    }
    Ok(Some(a))
}

// ── xerj code (with --def) ───────────────────────────────────────────────────

/// `xerj code …`: `--def <SYMBOL>` answers from the `-defs` index; everything
/// else is the existing retrieval path, delegated unchanged.
pub fn run_code(args: &[String]) -> i32 {
    let args = match with_pulled_node_url(args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("xerj code: {e:#}");
            return 2;
        }
    };
    if !args.iter().any(|a| a == "--def") {
        return xerj_autoindex::xc::run_code_cli(&args);
    }
    run_code_def(&args)
}

/// The `xerj code` flags (either mode) that take a value.
const CODE_VALUE_FLAGS: &[&str] = &[
    "-k",
    "--lang",
    "--mode",
    "--full",
    "--url",
    "--api-key",
    "--def",
];

/// The `<corpus>` argument of `xerj code …`: the first positional, skipping the
/// values of [`CODE_VALUE_FLAGS`] (both parsers take it in that position).
fn code_corpus_arg(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if CODE_VALUE_FLAGS.contains(&a) {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            return Some(a);
        }
        i += 1;
    }
    None
}

/// For a PULLED corpus queried with no `--url` and no `XERJ_URL`, add
/// `--url <the node the pack lives on>` ([`node::resolve_pulled_node`]).
///
/// Without this, `xerj code <slug>` — the command `xerj hub pull` tells the user
/// to run next — resolved `localhost:9200` while the pack had been restored into
/// the automatic node on 9300+: `--def` printed "no match … answered from: local
/// pack" and a question failed with a transport error. `xerj ask` already used
/// the automatic node. A locally indexed corpus, an explicit `--url` or
/// `XERJ_URL`, and anything that is not a pulled corpus pass through unchanged.
fn with_pulled_node_url(args: &[String]) -> Result<Vec<String>> {
    let explicit_env = std::env::var("XERJ_URL").is_ok_and(|u| !u.is_empty());
    let help = args.iter().any(|a| a == "-h" || a == "--help");
    if explicit_env || help || args.iter().any(|a| a == "--url") {
        return Ok(args.to_vec());
    }
    let Some(corpus) = code_corpus_arg(args) else {
        return Ok(args.to_vec());
    };
    let Ok(st) = state::load_state(&xerj_autoindex::xc::code_root(), corpus) else {
        return Ok(args.to_vec()); // the code path reports "not indexed" itself
    };
    if st.source.as_deref() != Some(state::SOURCE_HUBPACK) {
        return Ok(args.to_vec());
    }
    let launcher = NodeLauncher::current()?;
    let resolved = node::resolve_pulled_node(&hub_home(), st.url.as_deref(), &launcher)
        .with_context(|| format!("cannot reach the node holding the pulled corpus '{corpus}'"))?;
    let mut out = args.to_vec();
    out.push("--url".into());
    out.push(resolved.url);
    Ok(out)
}

fn run_code_def(args: &[String]) -> i32 {
    let mut slug: Option<String> = None;
    let mut symbol: Option<String> = None;
    let mut k: usize = crate::def::DEFAULT_K;
    let mut url: Option<String> = None;
    let mut api_key: Option<String> = None;
    let mut as_json = false;
    let mut i = 0;
    let bad = |m: String| -> i32 {
        eprintln!("xerj code: {m}");
        2
    };
    while i < args.len() {
        macro_rules! val {
            ($n:expr) => {
                match args.get(i + 1) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => return bad(format!("{} needs a value", $n)),
                }
            };
        }
        match args[i].as_str() {
            "--def" => symbol = Some(val!("--def")),
            "-k" => {
                let v = val!("-k");
                match v.parse::<usize>() {
                    Ok(n) if (1..=50).contains(&n) => k = n,
                    _ => return bad(format!("-k must be 1..=50, got '{v}'")),
                }
            }
            "--url" => url = Some(val!("--url")),
            "--api-key" => api_key = Some(val!("--api-key")),
            "--json" => as_json = true,
            "-h" | "--help" => {
                println!("usage: xerj code <corpus> --def <SYMBOL> [-k N] [--json] [--url URL]");
                return 0;
            }
            a if a.starts_with('-') => return bad(format!("unknown flag '{a}' in --def mode")),
            a => {
                if slug.is_none() {
                    slug = Some(a.to_string());
                } else {
                    return bad(format!("unexpected argument '{a}'"));
                }
            }
        }
        i += 1;
    }
    let (Some(slug), Some(symbol)) = (slug, symbol) else {
        return bad("both <corpus> and --def <SYMBOL> are required".into());
    };

    let url = resolve_url(url.as_deref());
    let node = match HubNode::connect_with_key(&url, api_key) {
        Ok(n) => n,
        Err(e) => return bad(format!("cannot reach {url}: {e:#}")),
    };
    let root = xerj_autoindex::xc::code_root();

    // Mirror the retrieval path's refusals so a def miss is distinguishable
    // from "not indexed" (exit 2) and "not loaded on this node" (exit 3).
    let st = match state::load_state(&root, &slug) {
        Ok(st) => st,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    if let Err(refusal) = state::check_fresh(&st, false, "`--stale-ok`") {
        eprintln!("{refusal}");
        return 2;
    }
    let prefix = state::query_prefix(&st);
    match node.cat_indices_json(&format!("{prefix}*")) {
        Ok(live) if live.is_empty() => {
            eprintln!("{}", state::not_loaded_message(&st, &prefix, &url));
            return 3;
        }
        Ok(_) => {}
        // An unreachable node is a failure to answer (exit 2), not "no match"
        // (exit 1) — and never "answered from: local pack".
        Err(e) => {
            eprintln!("xerj code: cannot reach {url}: {e}");
            return 2;
        }
    }

    let answer = crate::def::lookup(&node, &slug, &symbol, k);
    emit_answer(&answer, as_json);
    if answer.hits.is_empty() {
        1
    } else {
        0
    }
}

/// Print an [`Answer`]: warnings to stderr, the §1 rendering to stdout, and the
/// JSON form to stdout when `--json`.
fn emit_answer(answer: &Answer, as_json: bool) {
    for w in &answer.warnings {
        eprintln!("{w}");
    }
    if as_json {
        match serde_json::to_string(answer) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("xerj code: cannot serialize answer: {e}"),
        }
    } else {
        println!("{}", render(answer));
    }
}

// ── small shared helpers ─────────────────────────────────────────────────────

/// `~/.xerj/hub`, overridable by `XERJ_HUB_HOME` (tests). Never `/tmp`: the
/// pack cache and catalogue must survive a reboot (same rule as `code_root`).
pub(crate) fn hub_home() -> PathBuf {
    // One rule with `lockfile::default_hub_root` (incl. the Windows
    // `USERPROFILE` fallback), so `status` and `pull` never disagree.
    lockfile::default_hub_root()
}

/// The hub's ed25519 public key ([`xerj_common::hub::hub_pubkey`]: a test
/// `XERJ_HUB_PUBKEY` override, else the compiled-in
/// [`xerj_common::hub::HUB_PUBKEY_HEX`]). No key
/// is a hard error — an unverifiable pack is never extracted.
fn hub_pubkey() -> Result<String> {
    xerj_common::hub::hub_pubkey().ok_or_else(|| {
        anyhow!(
            "no hub public key is configured in this build (or XERJ_HUB_PUBKEY={}), so a \
             pack's signature cannot be verified; nothing was downloaded",
            xerj_common::hub::HUB_PUBKEY_NONE
        )
    })
}

/// First 8 hex chars of a 40-hex commit — the `pin8` in pack names.
fn pin8_of(pin_sha: &str) -> Result<String> {
    let s = pin_sha.trim();
    if s.len() < 8 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("pin sha '{s}' is not a hex commit of at least 8 chars");
    }
    Ok(s[..8].to_ascii_lowercase())
}

/// The kebab-case string for a licence class — the state ledger's spelling.
fn licence_class_str(c: LicenceClass) -> &'static str {
    match c {
        LicenceClass::AdaptWithAttribution => "adapt-with-attribution",
        LicenceClass::ApproachOnly => "approach-only",
        LicenceClass::Excluded => "excluded",
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_common::hub::DefRecord;

    fn mk_def(symbol: &str, file: &str, line: u32, kind: &str) -> DefRecord {
        DefRecord {
            symbol: symbol.into(),
            kind: kind.into(),
            lang: "python".into(),
            file: file.into(),
            line,
            text: format!("def {symbol}(): ..."),
            value: None,
        }
    }

    /// Regression for the backfill crash (run 39240121636: agno + ~6 corpora
    /// failed `build` with "defs bulk index reported errors"). The `-defs` bulk
    /// used `{"index":{}}`, so the builder node minted an empty `_id`; the first
    /// def indexed under `""` and the rest collided on it, failing the bulk.
    /// Every action line must now carry a present, NON-EMPTY, UNIQUE `_id`.
    #[test]
    fn defs_bulk_never_emits_empty_id() {
        let defs = vec![
            mk_def("Agent", "agno/agent.py", 10, "class"),
            mk_def("run", "agno/agent.py", 42, "function"),
            // No usable identifier — the pathological record. It must still get
            // a synthesized non-empty id, not be dropped and not yield "".
            mk_def("", "agno/weird.py", 1, "function"),
            // Same symbol+line+kind as above but a different file: distinct id.
            mk_def("run", "agno/other.py", 42, "function"),
        ];
        let body = defs_bulk_ndjson(&defs);

        // Belt-and-braces: the literal that the server rejected never appears.
        assert!(
            !body.contains("\"_id\":\"\""),
            "defs bulk must not contain an empty _id:\n{body}"
        );

        // action/doc lines interleave; inspect every action line.
        let actions: Vec<&str> = body.lines().step_by(2).collect();
        assert_eq!(actions.len(), defs.len());
        let mut ids = std::collections::HashSet::new();
        for line in &actions {
            let v: Value = serde_json::from_str(line).expect("action line is JSON");
            let id = v["index"]["_id"]
                .as_str()
                .expect("index action carries an _id");
            assert!(!id.is_empty(), "def _id must never be empty: {line}");
            assert!(ids.insert(id.to_string()), "def _id must be unique: {id}");
        }
        assert_eq!(ids.len(), defs.len(), "no def collapsed onto another's id");

        // Deterministic: a pack rebuild overwrites in place, never duplicates.
        assert_eq!(body, defs_bulk_ndjson(&defs));
    }

    /// The id-generation path itself: a def with an empty symbol (and otherwise
    /// empty fields) still hashes to a stable, non-empty 32-hex id.
    #[test]
    fn def_doc_id_is_nonempty_even_without_a_symbol() {
        let empty = DefRecord {
            symbol: String::new(),
            kind: String::new(),
            lang: String::new(),
            file: String::new(),
            line: 0,
            text: String::new(),
            value: None,
        };
        let id = def_doc_id(&empty);
        assert!(!id.is_empty(), "synthesized id must not be empty");
        assert_eq!(id.len(), 32, "xxh3_128 hex is 32 chars");
        assert_eq!(id, def_doc_id(&empty), "id is stable for the same record");
        // Distinct records get distinct ids.
        assert_ne!(def_doc_id(&mk_def("f", "a.py", 1, "function")), id);
    }

    #[test]
    fn ask_hub_unavailable_has_its_own_documented_exit_code() {
        // "the hub catalogue cannot be loaded" used to share exit 1 with "no
        // match", which an agent reads as an answer about the corpus.
        let codes = [
            ask::EXIT_OK,
            ask::EXIT_NO_ANSWER,
            ask::EXIT_USAGE,
            ask::EXIT_UNVERIFIED,
            ask::EXIT_NEEDS_DECISION,
            ask::EXIT_NO_KEY,
            ask::EXIT_HUB_UNAVAILABLE,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            codes.len(),
            "exit codes must be distinct: {codes:?}"
        );
        assert_eq!(ask::EXIT_HUB_UNAVAILABLE, 6);
        assert!(
            ASK_USAGE.contains("6 the hub\ncatalogue could not be loaded"),
            "{ASK_USAGE}"
        );
    }

    /// A minimal record pack in the layout `xerj corpus build` writes and
    /// `harvest::pack::read_manifest` verifies.
    fn record_pack(dir: &Path, slug: &str, created_at: &str, records: &str, licence: &str) {
        fs::create_dir_all(dir).unwrap();
        let hex = |b: &[u8]| -> String {
            Sha256::digest(b)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect()
        };
        fs::write(dir.join("records-000.jsonl"), records).unwrap();
        fs::write(dir.join("recipe.toml"), "[recipe]\n").unwrap();
        let n = records.lines().filter(|l| !l.trim().is_empty()).count();
        let manifest = json!({
            "format_version": 1, "kind": "harvested", "pack": slug,
            "created_at": created_at, "generation": 3,
            "counts": { "records": n },
            "files": { "records-000.jsonl": { "sha256": hex(records.as_bytes()), "records": n } },
            "sources": [ { "slug": "src", "licence": licence, "url": "https://x.test/src" } ],
        });
        let manifest = serde_json::to_string_pretty(&manifest).unwrap();
        fs::write(dir.join("manifest.json"), &manifest).unwrap();
        let sums = format!(
            "{}  manifest.json\n{}  recipe.toml\n{}  records-000.jsonl\n",
            hex(manifest.as_bytes()),
            hex(b"[recipe]\n"),
            hex(records.as_bytes())
        );
        fs::write(dir.join("SHA256SUMS"), sums).unwrap();
    }

    const RECORDS: &str = "{\"id\":\"A-1\",\"source\":\"src\"}\n";

    #[test]
    fn a_record_packs_pin_is_its_content_not_its_build_date() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("c"),
        );
        record_pack(&a, "rv", "2026-10-16T04:23:00Z", RECORDS, "CC0-1.0");
        record_pack(&b, "rv", "2026-10-17T04:23:00Z", RECORDS, "CC0-1.0");
        record_pack(
            &c,
            "rv",
            "2026-10-17T04:23:00Z",
            "{\"id\":\"A-2\",\"source\":\"src\"}\n",
            "CC0-1.0",
        );
        let ma = record_pack_metadata(&a, "rv").unwrap();
        let mb = record_pack_metadata(&b, "rv").unwrap();
        let mc = record_pack_metadata(&c, "rv").unwrap();
        assert_eq!(ma.pin_sha.len(), 40);
        assert!(ma.pin_sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(ma.pin_sha, mb.pin_sha, "same records, same pin");
        assert_ne!(mb.pin_sha, mc.pin_sha, "a changed record moves the pin");
        assert_eq!(ma.pin_date, "2026-10-16");
        assert_eq!(mb.pin_date, "2026-10-17");
        assert_eq!(ma.licence_spdx, "CC0-1.0");
        assert_eq!(ma.licence_class, LicenceClass::AdaptWithAttribution);
        assert_eq!(ma.sources.len(), 1);
        assert_eq!(ma.sources[0].repo, "src");
        assert_eq!(ma.sources[0].url, "https://x.test/src");
    }

    #[test]
    fn a_record_packs_licence_follows_the_one_licence_policy_and_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let gpl = tmp.path().join("gpl");
        record_pack(&gpl, "rv", "2026-10-16T00:00:00Z", RECORDS, "GPL-3.0");
        let m = record_pack_metadata(&gpl, "rv").unwrap();
        assert_eq!(m.licence_class, LicenceClass::ApproachOnly);
        let none = tmp.path().join("none");
        record_pack(&none, "rv", "2026-10-16T00:00:00Z", RECORDS, "");
        let err = format!("{:#}", record_pack_metadata(&none, "rv").err().unwrap());
        assert!(err.contains("records no licence"), "{err}");
    }

    #[test]
    fn a_record_pack_for_another_slug_or_a_tampered_one_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p");
        record_pack(&dir, "rv", "2026-10-16T00:00:00Z", RECORDS, "CC0-1.0");
        let err = format!("{:#}", record_pack_metadata(&dir, "other").err().unwrap());
        assert!(err.contains("not 'other'"), "{err}");
        fs::write(
            dir.join("records-000.jsonl"),
            "{\"id\":\"B\",\"source\":\"src\"}\n",
        )
        .unwrap();
        assert!(record_pack_metadata(&dir, "rv").is_err());
    }

    #[test]
    fn build_pack_takes_one_source_not_two() {
        let args: Vec<String> = [
            "rv",
            "--registry",
            "r",
            "--from-pack",
            "p",
            "--out",
            "o",
            "--url",
            "http://127.0.0.1:1",
            "--sign-key",
            "k",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(run_build_pack(&args), 2);
    }

    #[test]
    fn a_refused_restore_names_the_exact_config_line_that_fixes_it() {
        let hub = tempfile::tempdir().unwrap();
        let packs = hub.path().join("packs");
        std::fs::create_dir_all(&packs).unwrap();
        let canon = packs.canonicalize().unwrap();
        let server = "invalid_mapping: snapshot repository location [/x] is outside data_dir \
                      and not in limits.snapshot_repo_allowlist; refusing";
        let msg = restore_error_message(server, "http://127.0.0.1:9200", &packs);
        let line = format!(
            "snapshot_repo_allowlist = [{}]",
            serde_json::to_string(&canon.to_string_lossy()).unwrap()
        );
        assert!(msg.contains(&line), "{msg}");
        assert!(msg.contains("[limits]"), "{msg}");
        assert!(msg.contains("http://127.0.0.1:9200"), "{msg}");
        assert!(msg.contains("XERJ_URL"), "{msg}");
        assert!(
            msg.contains(server),
            "the server's own words are kept: {msg}"
        );
        // The line it prints is one the real config loader accepts, and it
        // allowlists exactly the cache.
        let cfg_path = hub.path().join("fix.toml");
        std::fs::write(&cfg_path, format!("[limits]\n{line}\n")).unwrap();
        let cfg = xerj_common::config::Config::load(&cfg_path).expect("the printed fix loads");
        assert_eq!(
            cfg.limits.snapshot_repo_allowlist,
            [canon.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn any_other_restore_failure_is_reported_as_it_was() {
        let msg = restore_error_message("boom", "http://h:1", Path::new("/p"));
        assert_eq!(msg, "restore snapshot: boom");
    }

    #[test]
    fn pin8_takes_eight_lowercase_hex() {
        assert_eq!(
            pin8_of("0774D05537f9762f838f7ab541b7765f1a729cb5").unwrap(),
            "0774d055"
        );
        assert!(pin8_of("short").is_err());
        assert!(pin8_of("nothexxx").is_err());
    }

    #[test]
    fn licence_class_strings_round_trip_through_the_state_spelling() {
        for (c, s) in [
            (LicenceClass::AdaptWithAttribution, "adapt-with-attribution"),
            (LicenceClass::ApproachOnly, "approach-only"),
            (LicenceClass::Excluded, "excluded"),
        ] {
            assert_eq!(licence_class_str(c), s);
        }
    }

    #[test]
    fn code_corpus_arg_skips_flag_values() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(code_corpus_arg(&v(&["lz4", "q"])), Some("lz4"));
        assert_eq!(code_corpus_arg(&v(&["--def", "X", "lz4"])), Some("lz4"));
        assert_eq!(
            code_corpus_arg(&v(&["-k", "3", "--json", "lz4", "q"])),
            Some("lz4")
        );
        assert_eq!(
            code_corpus_arg(&v(&["--mode", "bm25", "--lang", "c"])),
            None
        );
        assert_eq!(code_corpus_arg(&v(&["--def"])), None);
    }

    #[test]
    fn run_code_without_def_does_not_enter_the_def_path() {
        // No node is contacted and no panic: a bare `code` with a bad flag
        // routes to the existing CLI, which returns its own usage exit code.
        let rc = run_code(&["--definitely-not-a-flag".into()]);
        assert_eq!(rc, 2, "delegates to run_code_cli, which rejects the flag");
    }

    #[test]
    fn hub_usage_names_both_subcommands() {
        assert!(HUB_USAGE.contains("build-pack"));
        assert!(HUB_USAGE.contains("pull"));
        assert_eq!(run_hub_cli(&["help".into()]), 0);
        assert_eq!(run_hub_cli(&["bogus".into()]), 2);
    }

    #[test]
    fn fetch_to_copies_a_file_url_within_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        fs::write(&src, b"hub pack bytes").unwrap();
        let dst = tmp.path().join("dst.bin");
        let url = format!("file://{}", src.display());
        let n = fetch_to(&url, &dst, 1024).unwrap();
        assert_eq!(n, 14);
        assert_eq!(fs::read(&dst).unwrap(), b"hub pack bytes");
    }

    #[test]
    fn fetch_to_refuses_a_file_over_the_cap_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big.bin");
        fs::write(&src, vec![0u8; 2048]).unwrap();
        let dst = tmp.path().join("dst.bin");
        let url = format!("file://{}", src.display());
        let err = fetch_to(&url, &dst, 1024).unwrap_err();
        assert!(err.to_string().contains("over the"), "{err}");
        assert!(!dst.exists(), "nothing is written when over the cap");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_to_refuses_a_device_that_stats_as_empty() {
        // `/dev/zero` stats as 0 bytes. It used to pass the cap and then be copied
        // without end (a signed catalogue `pack_url: "file:///dev/zero"` fills the
        // disk). A non-regular file is refused before a byte is read.
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dst.bin");
        let err = fetch_to("file:///dev/zero", &dst, 1024).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
        assert!(!dst.exists(), "nothing is written");
    }

    #[test]
    fn valid_slug_accepts_real_slugs_and_rejects_traversal() {
        for ok in ["mini-c", "lz4", "a", "xerj-blogposts", "a.b_c-1", "x9"] {
            assert!(valid_slug(ok), "{ok} should be valid");
        }
        for bad in [
            "",
            "..",
            "../../etc",
            "a/b",
            "/abs",
            ".hidden",
            "trailing-",
            "-leading",
            "UPPER",
            "a b",
            "x\0y",
        ] {
            assert!(!valid_slug(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn build_pack_requires_url_and_makes_no_network_call() {
        // A build-pack missing --url must error with the usage (exit 2) BEFORE it
        // could reach a node — never fall through to a localhost:9200 default.
        // The registry/out/sign-key point at non-existent paths; if --url were
        // defaulted the code would try to clone/connect, but the required-args
        // gate returns first, so no filesystem/network work happens.
        let rc = run_hub_cli(&[
            "build-pack".into(),
            "mini-c".into(),
            "--registry".into(),
            "/nonexistent/registry".into(),
            "--out".into(),
            "/nonexistent/out".into(),
            "--sign-key".into(),
            "/nonexistent/key".into(),
        ]);
        assert_eq!(rc, 2, "missing --url is a usage error");
    }

    #[test]
    fn defs_bulk_bodies_split_under_the_engine_action_cap() {
        let defs: Vec<xerj_common::hub::DefRecord> = (0..5)
            .map(|i| xerj_common::hub::DefRecord {
                symbol: format!("s{i}"),
                kind: "function".into(),
                lang: "rust".into(),
                file: "a.rs".into(),
                line: i + 1,
                text: format!("fn s{i}()"),
                value: None,
            })
            .collect();

        // 5 defs at 2 actions per body -> three bodies (2, 2, 1).
        let bodies = defs_bulk_bodies(&defs, 2);
        assert_eq!(bodies.len(), 3);
        let per_body: Vec<usize> = bodies
            .iter()
            .map(|b| b.matches("{\"index\":{\"_id\":").count())
            .collect();
        assert_eq!(per_body, vec![2, 2, 1]);
        // Every chunk carries the stable, non-empty ids (#1860), and chunking
        // does not change them: the bodies concatenate to the unchunked body.
        assert!(!bodies.iter().any(|b| b.contains("{\"index\":{}}")));
        assert_eq!(bodies.concat(), defs_bulk_ndjson(&defs));
        // No body exceeds the chunk, and every def is indexed exactly once.
        assert!(per_body.iter().all(|&n| n <= 2));
        assert_eq!(per_body.iter().sum::<usize>(), defs.len());

        // Empty input issues no request at all.
        assert!(defs_bulk_bodies(&[], DEFS_BULK_CHUNK).is_empty());
        // The production chunk can never trip the engine's default cap, which is
        // exactly the 413 that failed biome/dagger/duckdb before this split.
        assert!(
            DEFS_BULK_CHUNK < xerj_common::config::LimitsConfig::default().max_actions_per_bulk
        );
    }

    // ── xerj ask: argument parsing ───────────────────────────────────────────

    fn strs(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn parsed(a: &[&str]) -> AskArgs {
        parse_ask_args(&strs(a)).unwrap().unwrap()
    }

    #[test]
    fn ask_args_default_to_auto_everything() {
        let a = parsed(&["how does lz4 framing work"]);
        assert_eq!(a.question, "how does lz4 framing work");
        assert_eq!(a.mode, ModeRequest::Auto);
        assert_eq!(a.hosted, HostedSwitch::Auto);
        assert!(a.corpus.is_none() && a.url.is_none() && a.max_tokens.is_none());
        assert!(!a.offline && !a.no_fetch && !a.json);
    }

    #[test]
    fn ask_args_take_every_documented_flag() {
        let a = parsed(&[
            "a question",
            "--corpus",
            "lz4",
            "--offline",
            "--no-fetch",
            "--hosted",
            "off",
            "--max-tokens",
            "80",
            "--json",
            "--url",
            "http://127.0.0.1:9300",
            "--files",
        ]);
        assert_eq!(a.corpus.as_deref(), Some("lz4"));
        assert!(a.offline && a.no_fetch && a.json);
        assert_eq!(a.hosted, HostedSwitch::Off);
        assert_eq!(a.max_tokens, Some(80));
        assert_eq!(a.url.as_deref(), Some("http://127.0.0.1:9300"));
        assert_eq!(a.mode, ModeRequest::Files);
        assert_eq!(parsed(&["q", "--how"]).mode, ModeRequest::How);
        assert_eq!(
            parsed(&["q", "--def", "LZ4_VERSION"]).mode,
            ModeRequest::Def("LZ4_VERSION".into())
        );
    }

    #[test]
    fn unquoted_words_are_one_question_and_flags_may_come_first() {
        let a = parsed(&["--json", "how", "does", "lz4", "work", "--corpus", "lz4"]);
        assert_eq!(a.question, "how does lz4 work");
        assert!(a.json);
        // `--def SYM` alone is a complete request.
        let a = parsed(&["--def", "LZ4_VERSION_RELEASE"]);
        assert_eq!(a.question, "LZ4_VERSION_RELEASE");
    }

    #[test]
    fn ask_args_reject_misuse() {
        for (bad, why) in [
            (vec![], "no question"),
            (vec!["--json"], "flags only"),
            (vec!["q", "--bogus"], "unknown flag"),
            (vec!["q", "--corpus"], "missing value"),
            (vec!["q", "--hosted", "maybe"], "bad hosted value"),
            (vec!["q", "--max-tokens", "0"], "zero tokens"),
            (vec!["q", "--max-tokens", "lots"], "non-numeric tokens"),
            (vec!["q", "--def", "A", "--how"], "two modes"),
            (vec!["q", "--how", "--files"], "two modes"),
            (vec!["q", "--def", " "], "blank symbol"),
        ] {
            assert!(parse_ask_args(&strs(&bad)).is_err(), "{why}: {bad:?}");
        }
        assert!(parse_ask_args(&strs(&["--help"])).unwrap().is_none());
        assert!(parse_ask_args(&strs(&["q", "-h"])).unwrap().is_none());
    }

    #[test]
    fn the_bare_form_needs_a_space_a_non_flag_and_a_non_subcommand() {
        assert!(is_bare_question(Some("how does lz4 frame work")));
        assert!(is_bare_question(Some("lz4 block")));
        // No space: a typo'd or unknown word is not a question.
        assert!(!is_bare_question(Some("lz4")));
        assert!(!is_bare_question(Some("LZ4_VERSION_RELEASE")));
        // A flag, with or without spaces, is not a question.
        assert!(!is_bare_question(Some("--port")));
        assert!(!is_bare_question(Some("--data-dir /x y")));
        // Subcommands are never questions, even if one ever grew a space.
        for sub in KNOWN_SUBCOMMANDS {
            assert!(!is_bare_question(Some(sub)), "{sub}");
        }
        assert!(!is_bare_question(None));
        assert!(!is_bare_question(Some("")));
    }

    #[test]
    fn known_subcommands_cover_what_main_dispatches() {
        for sub in [
            "index",
            "search",
            "def",
            "init",
            "gain",
            "autoindex",
            "brain",
            "share",
            "feedback",
            "memory",
            "code",
            "corpus",
            "hub",
            "ask",
            "mcp",
        ] {
            assert!(KNOWN_SUBCOMMANDS.contains(&sub), "{sub}");
        }
    }

    // ── xerj hub status / stop / config ──────────────────────────────────────

    #[test]
    fn hub_config_writes_and_clears_the_hosted_setting() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(config_in(home.path(), &strs(&["hosted", "on"])), 0);
        let text = fs::read_to_string(home.path().join("config.toml")).unwrap();
        assert!(text.contains("hosted = \"on\""), "{text}");
        assert_eq!(config_in(home.path(), &strs(&["hosted", "off"])), 0);
        assert!(fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .contains("hosted = \"off\""));
        assert_eq!(config_in(home.path(), &strs(&["hosted", "auto"])), 0);
        assert!(!fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .contains("hosted"));
        // Misuse is a usage error and writes nothing new.
        assert_eq!(config_in(home.path(), &strs(&["hosted", "sometimes"])), 2);
        assert_eq!(config_in(home.path(), &strs(&["hosted"])), 2);
        assert_eq!(config_in(home.path(), &strs(&["colour", "on"])), 2);
        assert_eq!(config_in(home.path(), &strs(&[])), 2);
    }

    #[test]
    fn hub_stop_and_status_with_nothing_running_succeed() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(stop_in(home.path()), 0);
        assert_eq!(status_in(home.path()), 0);
        assert!(!home.path().join("node.pid").exists());
    }

    #[test]
    fn hub_usage_lists_the_new_commands() {
        for word in [
            "status",
            "stop",
            "config hosted",
            "feedback <slug>",
            "find \"<question>\"",
        ] {
            assert!(HUB_USAGE.contains(word), "{word}");
        }
        assert_eq!(run_hub_cli(&strs(&["status", "extra"])), 2);
        assert_eq!(run_hub_cli(&strs(&["stop", "--bogus"])), 2);
        assert_eq!(run_hub_cli(&strs(&["config"])), 2);
    }

    /// Restored from d26ecf43, which deleted it because it wrote
    /// `XERJ_HUB_PUBKEY` without the `catalogue` tests' private lock. It now
    /// holds the crate's one lock ([`crate::test_support::HubPubkeyEnv`]).
    /// The no-key path end to end (exit 5, no `.tmp`, no node) stays covered
    /// by the `nokey` scenario in `tests/pack_roundtrip.rs`.
    #[test]
    fn hub_pubkey_prefers_the_env_override_and_refuses_with_none() {
        let env = crate::test_support::HubPubkeyEnv::lock();

        env.set("abcd");
        assert_eq!(hub_pubkey().unwrap(), "abcd");

        env.set(xerj_common::hub::HUB_PUBKEY_NONE);
        let msg = format!("{:#}", hub_pubkey().unwrap_err());
        assert!(msg.contains("no hub public key"), "{msg}");
        assert!(msg.contains("XERJ_HUB_PUBKEY=none"), "{msg}");

        env.remove();
        if xerj_common::hub::HUB_PUBKEY_HEX.is_empty() {
            assert!(hub_pubkey().is_err(), "a build with no key refuses");
        } else {
            assert_eq!(hub_pubkey().unwrap(), xerj_common::hub::HUB_PUBKEY_HEX);
        }
    }

    // ── the pull policy lives in the pull ────────────────────────────────────

    fn entry(slug: &str, class: LicenceClass, pack_bytes: u64) -> CatalogueEntry {
        CatalogueEntry {
            slug: slug.to_string(),
            domain: "TEST".to_string(),
            topics: vec![],
            ask_for: "x".to_string(),
            upstream: String::new(),
            pin_sha: "0774d05537f9762f838f7ab541b7765f1a729cb5".to_string(),
            pin_date: "2026-10-02".to_string(),
            refresh_cadence_days: 7,
            licence_spdx: "GPL-3.0".to_string(),
            licence_class: class,
            pack_bytes,
            pack_url: "file:///nonexistent".to_string(),
            pack_sha256: "0".repeat(64),
            sig_url: "file:///nonexistent.sig".to_string(),
            dependency_names: vec![],
            quality_grade: None,
            term_sketch: xerj_common::hub::Sketch {
                m_bits: 64,
                k_hashes: 7,
                bits_base64: "AAAAAAAAAAA=".to_string(),
            },
        }
    }

    #[test]
    fn an_excluded_corpus_is_refused_by_pull_itself_not_only_by_the_ask_flow() {
        // `xerj hub pull <slug>` is a command anyone can type — including from a
        // hint an earlier run printed. Plan §11: an excluded licence class is never
        // pulled. The refusal is the pull's, before any node, download or temp dir.
        let err = plan_pull_entry(
            &entry("gpl-thing", LicenceClass::Excluded, 1024),
            DISK_BUDGET_BYTES,
        )
        .err()
        .expect("refused");
        assert_eq!(err.code, 1, "a policy refusal, exit 1");
        assert!(
            err.message.contains("excluded licence class"),
            "{}",
            err.message
        );
        assert!(err.message.contains("never pulled"), "{}", err.message);
        assert!(
            err.message.contains("GPL-3.0"),
            "names the licence: {}",
            err.message
        );
        // Excluded wins over the size cap: the licence is the first thing said,
        // even for a pack far over any ceiling.
        let err = plan_pull_entry(
            &entry("gpl-thing", LicenceClass::Excluded, u64::MAX / 2),
            DISK_BUDGET_BYTES,
        )
        .err()
        .expect("refused");
        assert!(
            err.message.contains("excluded licence class"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_size_ceiling_is_the_callers_not_a_fixed_cap() {
        // A pack just over the 50 MiB auto-pull cap. Under the AUTOMATIC ceiling it
        // is refused — a question must never silently drag it down — and the refusal
        // points at the command that CAN fetch it.
        let big = entry(
            "big",
            LicenceClass::AdaptWithAttribution,
            AUTO_PULL_MAX_BYTES + 1,
        );
        let err = plan_pull_entry(&big, AUTO_PULL_MAX_BYTES)
            .err()
            .expect("auto-pull refuses an over-cap pack");
        assert_eq!(err.code, 1);
        assert!(err.message.contains("over the"), "{}", err.message);
        assert!(
            err.message.contains("xerj hub pull"),
            "the refusal names the explicit escape hatch: {}",
            err.message
        );

        // The SAME pack under the EXPLICIT ceiling (the disk budget) is NOT refused
        // for its size: the person typing `xerj hub pull big` asked for exactly it.
        // The only thing that may still stop it here is the key lookup (no hub key
        // in a unit test) — never a size refusal. This is the fix: over-50-MiB
        // corpora were previously unpullable by any route.
        match plan_pull_entry(&big, DISK_BUDGET_BYTES) {
            Ok(plan) => assert_eq!(plan.max_bytes, DISK_BUDGET_BYTES),
            Err(e) => {
                assert_eq!(
                    e.code, E_NOKEY,
                    "only a missing key may stop it: {}",
                    e.message
                );
                assert!(
                    !e.message.contains("not downloaded"),
                    "an explicit pull must not refuse on size: {}",
                    e.message
                );
            }
        }

        // A pack larger than the whole disk budget is refused even explicitly —
        // it could not be stored — and says so.
        let huge = entry(
            "huge",
            LicenceClass::AdaptWithAttribution,
            DISK_BUDGET_BYTES + 1,
        );
        let err = plan_pull_entry(&huge, DISK_BUDGET_BYTES)
            .err()
            .expect("over the disk budget is refused");
        assert_eq!(err.code, 1);
        assert!(err.message.contains("disk budget"), "{}", err.message);

        let err = plan_pull_entry(
            &entry("../../etc", LicenceClass::AdaptWithAttribution, 1),
            DISK_BUDGET_BYTES,
        )
        .err()
        .expect("refused");
        assert_eq!(err.code, 2, "a malformed slug is exit 2");

        let mut bad_pin = entry("ok", LicenceClass::AdaptWithAttribution, 1);
        bad_pin.pin_sha = "not-a-commit".to_string();
        assert_eq!(
            plan_pull_entry(&bad_pin, DISK_BUDGET_BYTES)
                .err()
                .expect("refused")
                .code,
            1
        );
    }

    #[test]
    fn approach_only_is_still_pullable_the_policy_only_stops_excluded() {
        // The key lookup is the first thing that can still fail for these; either
        // way the answer is not a licence refusal.
        for class in [
            LicenceClass::AdaptWithAttribution,
            LicenceClass::ApproachOnly,
        ] {
            match plan_pull_entry(&entry("fine", class, 1024), DISK_BUDGET_BYTES) {
                Ok(plan) => assert_eq!(plan.pin8, "0774d055"),
                Err(e) => assert_eq!(
                    e.code, E_NOKEY,
                    "only a missing key may stop it: {}",
                    e.message
                ),
            }
        }
    }

    // ── the pull lock belongs to the process that holds it ───────────────────

    #[test]
    fn a_pull_waits_a_bounded_time_for_the_lock_and_names_it_when_it_gives_up() {
        let packs = tempfile::tempdir().unwrap();
        let dir = packs.path().join("lz4");
        let held = PullLock::acquire(&dir, "lz4", "0774d055", Duration::ZERO, true).unwrap();

        let t0 = std::time::Instant::now();
        let err = PullLock::acquire(&dir, "lz4", "0774d055", Duration::from_millis(300), true)
            .err()
            .expect("still held");
        let waited = t0.elapsed();
        assert_eq!(err.code, 1);
        let lock = dir.join("0774d055.lock");
        assert!(
            err.message.contains(&lock.display().to_string()),
            "the message names the lock file: {}",
            err.message
        );
        assert!(
            err.message
                .contains(&format!("held by pid {}", std::process::id())),
            "and its owner: {}",
            err.message
        );
        assert!(
            waited >= Duration::from_millis(300) && waited < Duration::from_secs(5),
            "bounded: {waited:?}"
        );

        // A different pin of the same corpus is a different lock.
        let other = PullLock::acquire(&dir, "lz4", "deadbeef", Duration::ZERO, true).unwrap();
        drop(other);

        drop(held);
        PullLock::acquire(&dir, "lz4", "0774d055", Duration::ZERO, true)
            .expect("free the moment the holder lets go");
    }

    #[test]
    fn a_waiting_pull_proceeds_as_soon_as_the_other_one_finishes() {
        let packs = tempfile::tempdir().unwrap();
        let dir = packs.path().join("lz4");
        let held = PullLock::acquire(&dir, "lz4", "0774d055", Duration::ZERO, true).unwrap();
        let waiter_dir = dir.clone();
        let waiter = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let got = PullLock::acquire(
                &waiter_dir,
                "lz4",
                "0774d055",
                Duration::from_secs(20),
                true,
            );
            (got.is_ok(), t0.elapsed())
        });
        std::thread::sleep(Duration::from_millis(400));
        drop(held);
        let (got, waited) = waiter.join().unwrap();
        assert!(got, "the second pull got the lock once the first finished");
        assert!(
            waited >= Duration::from_millis(300) && waited < Duration::from_secs(10),
            "{waited:?}"
        );
    }

    #[test]
    fn a_lock_directory_left_by_an_older_build_does_not_block_a_pull() {
        // The old scheme: `<pin8>.lock` was a directory, removed only by the pull's
        // own destructor.
        let packs = tempfile::tempdir().unwrap();
        let dir = packs.path().join("lz4");
        fs::create_dir_all(dir.join("0774d055.lock")).unwrap();
        PullLock::acquire(&dir, "lz4", "0774d055", Duration::ZERO, true)
            .expect("the leftover directory is replaced by a lock file");
    }

    // ── installing a verified extract ────────────────────────────────────────

    fn fake_extract(dir: &Path, marker: &str) {
        fs::create_dir_all(dir.join("snapshot")).unwrap();
        fs::write(dir.join("pack.json"), marker).unwrap();
        fs::write(dir.join("snapshot").join("data"), marker).unwrap();
    }

    #[test]
    fn a_verified_extract_is_renamed_into_place_and_replaces_an_older_one() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("0774d055.tmp");
        let dest = root.path().join("0774d055");
        let staging = scratch.join("extract");

        // Nothing there yet.
        fake_extract(&staging, "first");
        install_extract(&staging, &dest, &scratch).unwrap();
        assert_eq!(fs::read_to_string(dest.join("pack.json")).unwrap(), "first");
        assert!(!staging.exists(), "moved, not copied");

        // An older extract is replaced whole: nothing of it is mixed into the new one.
        fs::write(dest.join("stale-file"), "x").unwrap();
        fake_extract(&staging, "second");
        install_extract(&staging, &dest, &scratch).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("pack.json")).unwrap(),
            "second"
        );
        assert!(!dest.join("stale-file").exists());
    }

    #[test]
    fn a_failed_install_puts_the_previous_extract_back() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("0774d055.tmp");
        fs::create_dir_all(&scratch).unwrap();
        let dest = root.path().join("0774d055");
        fake_extract(&dest, "verified-earlier");
        // The staging directory is missing, so the second rename fails.
        let err =
            install_extract(&scratch.join("no-such-staging"), &dest, &scratch).expect_err("fails");
        assert_eq!(err.code, 1);
        assert_eq!(
            fs::read_to_string(dest.join("pack.json")).unwrap(),
            "verified-earlier",
            "the pack an earlier pull verified is still there"
        );
    }
}
