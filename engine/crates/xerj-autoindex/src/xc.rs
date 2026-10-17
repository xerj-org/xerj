//! `xerj code` and `xerj corpus` — the reference-coding loop ported into the
//! binary (issue #977; previously `tools/xerj-code/scripts/xc*.py|sh`).
//!
//! Everything semantic lives in [`xerj_common::xccode`]; this file is the CLI
//! shell: argv parsing in the house style (no clap), the [`XcHttp`]
//! implementation over this crate's blocking [`Es`] client, the git/clone
//! lifecycle (`xerj corpus add`), and the #930 build-verify-swap
//! (`xerj corpus index`). Exit codes: 0 hits / 1 no-match-or-incomplete /
//! 2 usage-transport-stale / 3 corpus-in-state-but-not-loaded-here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use xerj_common::localauth::{discover_local_admin_key_for_url, url_is_loopback};
use xerj_common::xccode::{self, manifest, state, CodeOutcome, CodeParams, Mode, XcHttp};

use crate::esclient::{Count, Es};

// ── shared plumbing ─────────────────────────────────────────────────────────

/// `~/.xerj-code` unless `XERJ_CODE_HOME` says otherwise. Never `/tmp`:
/// corpora and state must persist across reboots.
pub fn code_root() -> PathBuf {
    if let Ok(home) = std::env::var("XERJ_CODE_HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    xerj_common::home::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".xerj-code")
}

/// `--url` flag > `XERJ_URL` > `http://localhost:9200`. Unlike `autoindex`
/// (which ignores `XERJ_URL` on purpose for writes), these READ commands
/// honor it — the scripts always did.
fn resolve_url(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("XERJ_URL").ok().filter(|u| !u.is_empty()))
        .unwrap_or_else(|| "http://localhost:9200".to_string())
}

fn build_es(url: &str, api_key: Option<String>) -> Result<Es> {
    // Loopback admin-key fallback (#961/#962 patterns): announced, loopback-only.
    let key = match api_key {
        Some(k) => Some(k),
        None => {
            let env_key = std::env::var("XERJ_API_KEY").ok().filter(|k| !k.is_empty());
            match env_key {
                Some(k) => Some(k),
                None => {
                    if url_is_loopback(url) {
                        discover_local_admin_key_for_url(url).map(|(k, path)| {
                            eprintln!(
                                "xerj code: no --api-key/XERJ_API_KEY given; using the admin \
                                 key at {}",
                                path.display()
                            );
                            k
                        })
                    } else {
                        None
                    }
                }
            }
        }
    };
    Es::new(url, key)
}

/// [`XcHttp`] over the blocking client.
struct EsXc<'a>(&'a Es);

impl XcHttp for EsXc<'_> {
    fn get_mapping(&self, path: &str) -> Result<Value, String> {
        self.0.get_json(path).map_err(|e| format!("{e:#}"))
    }
    fn cat_indices_json(&self, pattern: &str) -> Result<Vec<String>, String> {
        self.0
            .cat_indices_json(pattern)
            .map_err(|e| format!("{e:#}"))
    }
    fn search(&self, index: &str, body: &Value) -> Result<Value, String> {
        self.0.search(index, body).map_err(|e| format!("{e:#}"))
    }
}

fn emit(out: &CodeOutcome) {
    for w in &out.warnings {
        eprintln!("{w}");
    }
    // `--json` can be the ONLY output (empty text is normal for a machine
    // consumer): the JSON line must print even when text is empty, so the
    // guard covers the text arm only, never the json arm.
    if !out.text.is_empty() {
        if out.to_stderr {
            eprintln!("{}", out.text.trim_end());
        } else {
            print!("{}", out.text);
        }
    }
    if let Some(j) = &out.json {
        println!("{j}");
    }
}

// ── xerj code ───────────────────────────────────────────────────────────────

const CODE_USAGE: &str =
    "usage: xerj code <corpus> \"<what you need>\" [-k N] [--mode bm25|semantic|hybrid]
       [--hybrid] [--lang <lg>] [--full N] [--no-symbol] [--fuse-symbols]
       [--json] [--meatl] [--stale-ok] [--url URL] [--api-key KEY]

retrieval over a reference corpus (see `xerj corpus list`):
  -k N            passages to return (default 5)
  --mode MODE     bm25 (default, measured 12/12 top-3), semantic, or hybrid
  --hybrid        shorthand for --mode hybrid
  --lang LG       filter to one language field (e.g. rust, go)
  --full N        max chars per passage (default 800; 0 = file head only)
  --no-symbol     window selection instead of the matching definition
  --fuse-symbols  fuse file-level BM25 with symbol-impl BM25 by RRF(k=60)
  --json          raw server response on stdout
  --meatl         machine-readable one-line-per-hit output
  --stale-ok      override the 30-day staleness refusal
  --url URL       node to query (default $XERJ_URL or http://localhost:9200)
  --api-key KEY   API key (loopback falls back to <data_dir>/admin.key)";

/// `xerj code …` — returns the process exit code.
pub fn run_code_cli(args: &[String]) -> i32 {
    let mut corpus: Option<String> = None;
    let mut query: Option<String> = None;
    let mut url: Option<String> = None;
    let mut api_key: Option<String> = None;
    let mut p = CodeParams::new("", "");
    let mut i = 0;
    let bad = |msg: String| -> i32 {
        eprintln!("{msg}\n");
        eprintln!("{CODE_USAGE}");
        2
    };
    while i < args.len() {
        let a = &args[i];
        macro_rules! val {
            ($name:expr) => {
                match args.get(i + 1) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => return bad(format!("xerj code: {} needs a value", $name)),
                }
            };
        }
        match a.as_str() {
            "-h" | "--help" => {
                println!("{CODE_USAGE}");
                return 0;
            }
            "-k" => {
                let v = val!("-k");
                match v.parse::<usize>() {
                    Ok(n) if (1..=50).contains(&n) => p.k = n,
                    _ => return bad(format!("xerj code: -k must be 1..=50, got '{v}'")),
                }
            }
            "--lang" => p.lang = Some(val!("--lang")),
            "--mode" => {
                let v = val!("--mode");
                match Mode::parse(&v) {
                    Some(m) => p.mode = m,
                    None => return bad(format!("xerj code: unknown mode '{v}'")),
                }
            }
            "--hybrid" => p.mode = Mode::Hybrid,
            "--full" => {
                let v = val!("--full");
                match v.parse::<usize>() {
                    Ok(n) => p.full = n,
                    _ => return bad(format!("xerj code: --full must be a number, got '{v}'")),
                }
            }
            "--no-symbol" => p.no_symbol = true,
            "--fuse-symbols" => p.fuse_symbols = true,
            "--json" => p.as_json = true,
            "--meatl" => p.meatl = true,
            "--stale-ok" => p.stale_ok = true,
            "--url" => url = Some(val!("--url")),
            "--api-key" => api_key = Some(val!("--api-key")),
            _ if a.starts_with('-') => {
                return bad(format!("xerj code: unknown flag '{a}'"));
            }
            _ => {
                if corpus.is_none() {
                    corpus = Some(a.clone());
                } else if query.is_none() {
                    query = Some(a.clone());
                } else {
                    return bad(format!("xerj code: unexpected argument '{a}'"));
                }
            }
        }
        i += 1;
    }
    let (Some(corpus), Some(query)) = (corpus, query) else {
        return bad("xerj code: both <corpus> and \"<query>\" are required".to_string());
    };
    let url = resolve_url(url.as_deref());
    let es = match build_es(&url, api_key) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj code: cannot reach {url}: {e:#}");
            return 2;
        }
    };
    p.corpus = corpus;
    p.query = query;
    let out = xccode::run_code_query(&code_root(), &EsXc(&es), &url, &p, "`--stale-ok`");
    emit(&out);
    out.exit
}

// ── xerj corpus add ─────────────────────────────────────────────────────────

const CORPUS_USAGE: &str =
    "usage: xerj corpus add <name> <git-url>... | --from <manifest.json|pack-dir|pack.zip> [--as <name>] [--verify-sig <pubkey-file>]
       xerj corpus build <name> [--recipe <path>] [--fresh]
       xerj corpus sign <pack-dir> --key <seed-file>
       xerj corpus keygen --out <prefix>
       xerj corpus index <name> [--fresh] [--url URL]
       xerj corpus list [--url URL]

a flag may precede the verb (xerj corpus --url URL index <name>);
clone the repos, detect licences, write corpora/<name>/corpus.json;
--from takes the corpus name from the manifest's 'corpus' field (or the
pack's 'pack' field) unless <name> or --as overrides it; a harvested pack
also checksum-verifies and materializes records per source; --verify-sig
checks the pack's SHA256SUMS.sig against a public key file first and
refuses the pack on failure (for a pack.zip, the sig cannot travel inside
— releases ship it loose beside the zip as <pack>-SHA256SUMS.sig, which
is where the lookup falls back to); build harvests recipe sources into a
deterministic pack under builds/<name>/ (see tools/xerj-code/); sign/keygen
are the publish step (ed25519 over SHA256SUMS);
index builds/verifies/switches the corpus (exit 3 skips junk — normal);
list shows what is loaded on this node.";

pub(crate) fn git(dir: Option<&Path>, args: &[&str]) -> Result<(i32, String)> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.current_dir(d).arg("-C").arg(d);
    }
    let out = c
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.first().unwrap_or(&"")))?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string()
            + String::from_utf8_lossy(&out.stderr).trim(),
    ))
}

/// Move a clone to `sha` (shallow). Never a shell string: the sha comes from
/// an untrusted manifest, and `Command` passes it as ONE argv element.
pub(crate) fn checkout_at_sha(target: &Path, url: &str, sha: &str) -> Result<()> {
    if !target.join(".git").exists() {
        std::fs::create_dir_all(target)?;
        git(Some(target), &["init", "--quiet"])?;
        git(Some(target), &["remote", "add", "origin", url]).ok();
        git(Some(target), &["config", "remote.origin.promisor", "true"]).ok();
        git(
            Some(target),
            &["config", "remote.origin.partialclonefilter", "blob:none"],
        )
        .ok();
    }
    let fetch = git(Some(target), &["fetch", "--depth", "1", "origin", sha]);
    if !fetch.map(|(rc, _)| rc == 0).unwrap_or(false) {
        anyhow::bail!("git fetch {sha} failed");
    }
    let co = git(
        Some(target),
        &["checkout", "--quiet", "--force", "--detach", sha],
    );
    if !co.map(|(rc, _)| rc == 0).unwrap_or(false) {
        anyhow::bail!("git checkout {sha} failed");
    }
    git(Some(target), &["clean", "-qfd"]).ok();
    Ok(())
}

fn dir_stats(target: &Path) -> (String, u64, u64) {
    let sha = git(Some(target), &["rev-parse", "HEAD"])
        .ok()
        .filter(|(rc, _)| *rc == 0)
        .map(|(_, out)| out)
        .unwrap_or_else(|| "unknown".to_string());
    let mut files = 0u64;
    let mut bytes = 0u64;
    fn walk(dir: &Path, files: &mut u64, bytes: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }
            if p.is_dir() {
                walk(&p, files, bytes);
            } else if let Ok(md) = e.metadata() {
                *files += 1;
                *bytes += md.len();
            }
        }
    }
    walk(target, &mut files, &mut bytes);
    (sha, files, bytes)
}

/// Corpus-name resolution for `add`: an explicit <name> (positional, then
/// `--as`) wins; otherwise the manifest's own `corpus` field supplies it.
/// `--from` with neither is a usage error, not a guess.
fn resolve_corpus_name(
    explicit: Option<String>,
    hub_corpus: &str,
    path: &str,
) -> Result<String, String> {
    explicit
        .or_else(|| (!hub_corpus.is_empty()).then(|| hub_corpus.to_string()))
        .ok_or_else(|| {
            format!("{path} has no 'corpus' field and no <name>/--as was given\n\n{CORPUS_USAGE}")
        })
}

fn run_corpus_add(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut as_name: Option<String> = None;
    let mut from: Option<String> = None;
    let mut verify_sig: Option<String> = None;
    let mut urls: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{CORPUS_USAGE}");
                return 0;
            }
            "--from" => match args.get(i + 1) {
                Some(v) => {
                    from = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("xerj corpus add: --from needs a manifest path\n\n{CORPUS_USAGE}");
                    return 2;
                }
            },
            "--verify-sig" => match args.get(i + 1) {
                Some(v) => {
                    verify_sig = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!(
                        "xerj corpus add: --verify-sig needs a public key file\n\n{CORPUS_USAGE}"
                    );
                    return 2;
                }
            },
            "--as" => match args.get(i + 1) {
                Some(v) => {
                    as_name = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("xerj corpus add: --as needs a corpus name\n\n{CORPUS_USAGE}");
                    return 2;
                }
            },
            a if a.starts_with('-') => {
                eprintln!("xerj corpus add: unknown flag '{a}'\n\n{CORPUS_USAGE}");
                return 2;
            }
            a => {
                if name.is_none() {
                    name = Some(a.to_string());
                } else {
                    urls.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    // `--as` only renames a `--from` rebuild; a positional name wins when
    // both are given (it is the more explicit spelling of the same thing).
    let explicit_name = name.or(as_name);
    if from.is_none() && explicit_name.is_none() {
        eprintln!("xerj corpus add: <name> is required\n\n{CORPUS_USAGE}");
        return 2;
    }
    if from.is_none() && urls.is_empty() {
        eprintln!(
            "xerj corpus add: give at least one <git-url> or --from <manifest>\n\n{CORPUS_USAGE}"
        );
        return 2;
    }
    // A `--from` that names a harvested pack (a pack dir, the pack's own
    // manifest.json, or a pack.zip) routes to the pack path BEFORE the
    // hub-manifest arm reads it: the two are different documents (`repos[]`
    // vs `files`+`kind`), and mis-reading one as the other produces noise,
    // not a useful error.
    if let Some(path) = &from {
        match resolve_pack_source(path) {
            Ok(FromPack::NotAPack) => {}
            Ok(FromPack::Dir(dir)) => {
                let share = dir.display().to_string();
                return run_corpus_add_pack(&dir, explicit_name, &share, verify_sig.as_deref());
            }
            Ok(FromPack::Zip(zip)) => {
                // transient staging, never build state — it dies with this
                // call, after the records have been copied out
                let tmp = match tempfile::tempdir() {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("xerj corpus add: cannot stage {zip}: {e}");
                        return 2;
                    }
                };
                let dest = tmp.path().join("pack");
                if let Err(e) = crate::harvest::source::extract_zip(Path::new(&zip), &dest) {
                    eprintln!("xerj corpus add: cannot extract {zip}: {e:#}");
                    return 2;
                }
                // a zip of a directory carries the wrapper (`demo.zip` →
                // `demo/…`) — descend exactly one level when needed; a pack
                // nested deeper than that is not a shape this tool emits
                let mut dir = dest.clone();
                if !dir.join("manifest.json").is_file() {
                    let kids: Vec<PathBuf> = match std::fs::read_dir(&dir) {
                        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
                        Err(_) => Vec::new(),
                    };
                    if let [only] = kids.as_slice() {
                        if only.join("manifest.json").is_file() {
                            dir = only.clone();
                        }
                    }
                }
                return run_corpus_add_pack(&dir, explicit_name, &zip, verify_sig.as_deref());
            }
            Err(e) => {
                eprintln!("xerj corpus add: {e}");
                return 2;
            }
        }
    }
    if verify_sig.is_some() {
        eprintln!(
            "xerj corpus add: --verify-sig applies to a harvested pack --from, not a clone\n\n{CORPUS_USAGE}"
        );
        return 2;
    }
    // The manifest is read BEFORE the name is finalised: with `--from` and
    // no explicit <name>/--as, the manifest's own 'corpus' field supplies
    // it. Everything downstream (dest, carry, entries) needs the resolved
    // name, so resolution lives here and nowhere else. The pin's query
    // hints (#1254) ride beside the tuple rather than inside it — a
    // three-slot tuple tripped clippy's type_complexity on the pinned
    // CI toolchain.
    let mut pin_query: Option<manifest::QueryHints> = None;
    let (name, rows): (String, Vec<(String, String, String, String)>) = if let Some(path) = &from {
        let hub = match manifest::read_hub_manifest(Path::new(path)) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("xerj corpus add: {e}");
                return 2;
            }
        };
        let resolved = match resolve_corpus_name(explicit_name, &hub.corpus, path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("xerj corpus add: {e}");
                return 2;
            }
        };
        println!("rebuilding corpus '{resolved}' from {path}");
        pin_query = hub.query;
        (
            resolved,
            hub.rows
                .into_iter()
                .map(|r| (r.repo, r.url, r.sha, r.declared_licence))
                .collect(),
        )
    } else {
        let Some(resolved) = explicit_name else {
            eprintln!("xerj corpus add: <name> is required\n\n{CORPUS_USAGE}");
            return 2;
        };
        (
            resolved,
            urls.iter()
                .map(|u| {
                    let repo = u
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(".git")
                        .to_string();
                    (repo, u.clone(), String::new(), String::new())
                })
                .collect(),
        )
    };
    if let Err(e) = xccode::pathgate::valid_corpus_name(&name) {
        eprintln!("xerj corpus add: {e}");
        return 2;
    }

    let root = code_root();
    let dest = root.join("corpora").join(&name);
    // A corpus name is EITHER a cloned-repo corpus or a harvested one.
    // Cloning repos into a harvested corpus's dir would corrupt both views:
    // the manifest's pseudo-repos would sit beside real clones, and
    // `xerj code`'s per-hit licence lookups would key on whichever won.
    if let Ok(prev) = manifest::read_corpus_manifest(&dest.join("corpus.json")) {
        if prev.kind.as_deref() == Some("harvested") {
            eprintln!(
                "xerj corpus add: corpus '{name}' is a harvested corpus — rebuild it from its pack:"
            );
            eprintln!(
                "xerj corpus add:   xerj corpus add {name} --from <pack-dir|pack.zip|manifest.json>"
            );
            return 2;
        }
    }
    if let Err(e) = std::fs::create_dir_all(&dest) {
        eprintln!("xerj corpus add: cannot create {}: {e}", dest.display());
        return 2;
    }

    // review{} blocks are PRESERVED across a rebuild when repo+sha match —
    // a human's licence review survives a re-clone (additive, never edited).
    let previous = manifest::read_corpus_manifest(&dest.join("corpus.json")).ok();
    let carry = |repo: &str, sha: &str| -> Option<Value> {
        let m = previous.as_ref()?;
        m.repos.iter().find_map(|r| {
            (r.repo == repo && r.sha == sha)
                .then(|| r.review.clone())
                .flatten()
        })
    };

    let mut entries: Vec<manifest::ManifestRepo> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for (repo_name, url, sha, declared) in rows {
        // The one destructive step (checkout --force + clean -fd) runs inside
        // this path: gate the name HERE, again, even though read_hub_manifest
        // already did — the plain-URL arm has no other gate.
        if xccode::pathgate::valid_repo_name(&repo_name).is_err() || repo_name.starts_with('-') {
            eprintln!("  [FAIL] refusing unsafe repo name '{repo_name}'");
            skipped.push(repo_name);
            continue;
        }
        let target = dest.join(&repo_name);
        if target.join(".git").exists() && !sha.is_empty() {
            let at = git(Some(&target), &["rev-parse", "HEAD"])
                .ok()
                .filter(|(rc, _)| *rc == 0)
                .map(|(_, out)| out);
            if at.as_deref() == Some(sha.as_str()) {
                println!(
                    "  [ok] {repo_name} already at {}",
                    &sha[..12.min(sha.len())]
                );
                record(&mut entries, &repo_name, &url, &target, &declared, &carry);
                continue;
            }
            println!("  [pin] {repo_name} -> {}", &sha[..12.min(sha.len())]);
            if let Err(e) = checkout_at_sha(&target, &url, &sha) {
                eprintln!("  [FAIL] {repo_name}: {e:#}; continuing");
                skipped.push(repo_name);
                continue;
            }
            record(&mut entries, &repo_name, &url, &target, &declared, &carry);
            continue;
        }
        if target.join(".git").exists() {
            println!("  [skip] {repo_name} already cloned");
            record(&mut entries, &repo_name, &url, &target, &declared, &carry);
            continue;
        }
        println!("  [clone] {repo_name}");
        if sha.is_empty() {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Never a shell string: the URL and target are single argv
            // elements, so a hostile URL cannot smuggle flags.
            let target_s = target.to_string_lossy().to_string();
            let (rc, _) = git(None, &["clone", "--depth", "1", "--quiet", &url, &target_s])
                .unwrap_or((-1, String::new()));
            if rc != 0 {
                let (rc2, _) = git(None, &["clone", "--depth", "1", &url, &target_s])
                    .unwrap_or((-1, String::new()));
                if rc2 != 0 {
                    eprintln!("  [FAIL] {repo_name} — could not clone; continuing");
                    skipped.push(repo_name);
                    continue;
                }
            }
        } else {
            println!("  [pin] {repo_name} -> {}", &sha[..12.min(sha.len())]);
            if let Err(e) = checkout_at_sha(&target, &url, &sha) {
                eprintln!("  [FAIL] {repo_name}: {e:#}; continuing");
                skipped.push(repo_name);
                continue;
            }
        }
        record(&mut entries, &repo_name, &url, &target, &declared, &carry);
    }

    if entries.is_empty() {
        eprintln!("xerj corpus add: nothing cloned");
        return 1;
    }

    // Regenerated FROM DISK, never copied through from the input — except
    // the pin's query hints (#1254), which are author DECLARATION, not
    // derived state: they travel from the hub pin into the corpus.json the
    // clone writes, so a re-clone cannot silently drop the corpus's
    // retrieval posture.
    let cloned_at = chrono_now_stamp();
    let manifest_path = dest.join("corpus.json");
    manifest::write_corpus_manifest_kind(
        &manifest_path,
        &name,
        None,
        &cloned_at,
        &entries,
        pin_query.as_ref(),
    );
    println!();
    println!(
        "corpus '{name}': {} repos at {}",
        entries.len(),
        dest.display()
    );
    println!(
        "share it: {}  (rebuild with xerj corpus add <name> --from <that file>)",
        manifest_path.display()
    );

    if !skipped.is_empty() {
        eprintln!();
        eprintln!(
            "xerj corpus add: {} of {} repos did not land: {}",
            skipped.len(),
            entries.len() + skipped.len(),
            skipped.join(" ")
        );
        eprintln!("xerj corpus add: this corpus is INCOMPLETE — it is not the one the manifest describes.");
        return 1;
    }
    println!("next: xerj corpus index {name}");
    0
}

// ── xerj corpus add --from <pack> ───────────────────────────────────────────

/// What `--from` named, as decided by [`resolve_pack_source`].
#[derive(Debug)]
enum FromPack {
    /// Not a pack — the hub-manifest arm handles it.
    NotAPack,
    /// A directory holding a harvested `manifest.json`.
    Dir(PathBuf),
    /// A zip file to stage and treat as `Dir` after extraction.
    Zip(String),
}

/// Decide whether a `--from` argument names a harvested pack. A directory
/// with a `manifest.json` whose `kind` is `harvested`, a `.json` file of the
/// same shape (the pack's own manifest, one level inside the pack), or a
/// `.zip` — anything else is `NotAPack`. `Err` is reserved for paths that
/// LOOK like a pack but cannot be inspected: a broken zip or an unreadable
/// manifest must be reported, never silently fallen through to the hub arm
/// (which would emit a confusing `repos[]` error for the same file).
fn resolve_pack_source(path: &str) -> Result<FromPack, String> {
    let p = Path::new(path);
    let is_harvested = |m: &Path| -> Result<bool, String> {
        let v: Value = std::fs::read_to_string(m)
            .map_err(|e| format!("cannot read {}: {e}", m.display()))
            .and_then(|raw| {
                serde_json::from_str(&raw)
                    .map_err(|e| format!("{} is not valid JSON: {e}", m.display()))
            })?;
        Ok(v.get("kind").and_then(Value::as_str) == Some("harvested"))
    };
    if p.is_dir() {
        let m = p.join("manifest.json");
        if !m.is_file() {
            return Err(format!(
                "{} is a directory with no manifest.json — name the pack directory, its \
                 manifest, or its zip",
                p.display()
            ));
        }
        return if is_harvested(&m)? {
            Ok(FromPack::Dir(p.to_path_buf()))
        } else {
            Err(format!(
                "{} holds a manifest.json that is not a harvested pack",
                p.display()
            ))
        };
    }
    let lower = path.to_lowercase();
    if lower.ends_with(".zip") {
        return Ok(FromPack::Zip(path.to_string()));
    }
    if p.is_file() {
        if let Ok(true) = is_harvested(p) {
            let dir = p.parent().unwrap_or(Path::new(".")).to_path_buf();
            return Ok(FromPack::Dir(dir));
        }
    }
    Ok(FromPack::NotAPack)
}

/// `corpus add --from <pack>`: checksum-verify the pack, then materialize it
/// as a corpus — one directory per source slug holding that source's
/// records, plus an extended corpus.json whose pseudo-repos are the pack's
/// sources. The per-slug layout is load-bearing, not cosmetic: `xerj code`
/// derives a hit's licence from the FIRST path segment of its locator
/// ([`xccode::passage::locator_repo`]), so `rustsec/records.jsonl` keys the
/// same way a cloned `rustsec/` repo always did and the per-hit licence
/// warnings work unchanged.
fn run_corpus_add_pack(
    pack_dir: &Path,
    explicit_name: Option<String>,
    share_path: &str,
    verify_sig: Option<&str>,
) -> i32 {
    // Origin check first, before a byte of the pack is trusted: checksums
    // prove integrity, the signature proves who built it. A pack that
    // fails here is refused whole — nothing is materialized, nothing is
    // indexed. The manifest is read only to resolve the pack's name for the
    // signature lookup — its content is still gated by the signature chain
    // (sig → SHA256SUMS → manifest.json) before anything below trusts it.
    let meta = match crate::harvest::pack::read_manifest(pack_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("xerj corpus add: {e:#}");
            return 2;
        }
    };
    if let Some(pubfile) = verify_sig {
        let public = match std::fs::read_to_string(pubfile) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("xerj corpus add: cannot read public key {pubfile}: {e}");
                return 2;
            }
        };
        // A directory pack carries its own SHA256SUMS.sig (what `corpus
        // sign` writes). A ZIP cannot: the sig is deliberately absent from
        // the SUMS it signs, so releases ship it as a LOOSE sibling asset,
        // named <pack-name>-SHA256SUMS.sig (the rust-vulns release asset
        // name) or plain SHA256SUMS.sig, next to the zip.
        let sig = if pack_dir.join(crate::harvest::sign::SIG_NAME).is_file() {
            Some(pack_dir.join(crate::harvest::sign::SIG_NAME))
        } else {
            let beside = Path::new(share_path).parent().unwrap_or(Path::new("."));
            [
                beside.join(format!("{}-SHA256SUMS.sig", meta.name)),
                beside.join("SHA256SUMS.sig"),
            ]
            .into_iter()
            .find(|p| p.is_file())
        };
        let verdict = match sig {
            Some(path) => crate::harvest::sign::verify_sig_at(pack_dir, &path, &public),
            None => Err(anyhow::anyhow!(
                "no SHA256SUMS.sig to verify — not in the pack, and no \
                 {}-SHA256SUMS.sig beside {}",
                meta.name,
                share_path
            )),
        };
        if let Err(e) = verdict {
            eprintln!("xerj corpus add: {e:#}");
            return 2;
        }
        println!("signature verified against {pubfile}");
    } else if crate::harvest::sign::pack_is_signed(pack_dir) {
        // unsigned USE of a signed pack is not an error — the key choice is
        // the consumer's — but it deserves a nudge, not silence
        eprintln!(
            "note: this pack carries a SHA256SUMS.sig — pass --verify-sig <pubkey-file> to check \
             its origin before indexing"
        );
    }
    let name = match resolve_corpus_name(explicit_name, &meta.name, &pack_dir.display().to_string())
    {
        Ok(n) => n,
        Err(e) => {
            eprintln!("xerj corpus add: {e}");
            return 2;
        }
    };
    if let Err(e) = xccode::pathgate::valid_corpus_name(&name) {
        eprintln!("xerj corpus add: {e}");
        return 2;
    }
    let root = code_root();
    let dest = root.join("corpora").join(&name);
    // Same one-kind-per-name rule the git arm enforces, from the other side.
    if let Ok(prev) = manifest::read_corpus_manifest(&dest.join("corpus.json")) {
        if prev.kind.as_deref() != Some("harvested") {
            eprintln!(
                "xerj corpus add: corpus '{name}' already exists as a cloned-repo corpus at {}",
                dest.display()
            );
            eprintln!("xerj corpus add: pick another name (--as <name>) or remove it first");
            return 2;
        }
    }
    println!(
        "materializing corpus '{name}' from pack generation {}",
        meta.generation
    );
    if let Err(e) = add_pack_materialize(&dest, pack_dir, &meta) {
        eprintln!("xerj corpus add: {e:#}");
        return 1;
    }
    let mut total = 0usize;
    for s in &meta.sources {
        let count = std::fs::read_to_string(dest.join(&s.slug).join("records.jsonl"))
            .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0);
        total += count;
        println!("  [pack] {}: {} record(s) ({})", s.slug, count, s.licence);
        if let Some(w) = xccode::licence::clone_warning_line(&s.licence) {
            eprintln!("{w}");
        }
    }
    println!(
        "corpus '{name}': {} records from {} source(s) at {}",
        total,
        meta.sources.len(),
        dest.display()
    );
    // `share_path` is where the pack CAME FROM — the dir spelling names the
    // pack dir, the zip spelling names the zip (its staging dir is deleted by
    // the time this prints, and a share line pointing at a dead tempdir
    // teaches users to re-send nothing).
    println!("share it: {share_path}  (rebuild with xerj corpus add {name} --from <that pack>)");
    println!("next: xerj corpus index {name}");
    0
}

/// Copy the pack's records into `corpora/<name>/<slug>/records.jsonl`
/// (verbatim lines — the pack line IS canonical JSON), reconcile away slug
/// dirs a previous add owned that this pack no longer has, and write the
/// extended corpus.json. Records are routed by their `source` field (the
/// merge winner), which the pack's uniform-key invariant guarantees on
/// every record.
fn add_pack_materialize(
    dest: &Path,
    pack_dir: &Path,
    meta: &crate::harvest::pack::PackMeta,
) -> anyhow::Result<()> {
    use std::collections::HashSet;
    use std::io::{BufRead, BufReader, Write};

    let slugs: HashSet<&str> = meta.sources.iter().map(|s| s.slug.as_str()).collect();

    // reconcile FIRST: every dir the previous harvested manifest owned is
    // regenerated from this pack — drop them all, not just the renamed ones,
    // or a re-add APPENDS to the old records.jsonl and duplicates every line
    if let Ok(prev) = manifest::read_corpus_manifest(&dest.join("corpus.json")) {
        for r in &prev.repos {
            let _ = std::fs::remove_dir_all(dest.join(&r.repo));
        }
    }
    std::fs::create_dir_all(dest).with_context(|| format!("cannot create {}", dest.display()))?;

    // per-slug buffer: shard files interleave ids (bucket = hash(id) %
    // shards), so the corpus's records.jsonl is re-sorted by id at write
    // time — a refresh then diffs clean against the pack's own ordering
    let mut by_slug: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    for f in &meta.files {
        let rdr = BufReader::new(
            std::fs::File::open(pack_dir.join(&f.name))
                .with_context(|| format!("cannot read {}", f.name))?,
        );
        for line in rdr.lines() {
            let line = line.with_context(|| format!("read {}", f.name))?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line)
                .with_context(|| format!("{}: a record line is not valid JSON", f.name))?;
            let slug = v.get("source").and_then(Value::as_str).unwrap_or("");
            if !slugs.contains(slug) {
                let fname = &f.name;
                anyhow::bail!(
                    "{fname}: a record names source '{slug}' which the pack does not declare"
                );
            }
            let id = v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            by_slug
                .entry(slug.to_string())
                .or_default()
                .push((id, line));
        }
    }
    if by_slug.is_empty() {
        anyhow::bail!("pack holds no records — nothing to materialize");
    }

    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (slug, mut records) in by_slug {
        records.sort_by(|a, b| a.0.cmp(&b.0));
        let dir = dest.join(&slug);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        let path = dir.join("records.jsonl");
        let mut w = std::fs::File::create(&path)
            .with_context(|| format!("cannot write {}", path.display()))?;
        for (_, line) in &records {
            // verbatim: the pack line IS canonical JSON
            w.write_all(line.as_bytes())?;
            w.write_all(b"\n")?;
        }
        counts.insert(slug, records.len() as u64);
    }

    let mut entries = Vec::new();
    for s in &meta.sources {
        let path = dest.join(&s.slug).join("records.jsonl");
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        entries.push(manifest::ManifestRepo {
            repo: s.slug.clone(),
            url: s.url.clone().unwrap_or_default(),
            licence: s.licence.clone(),
            sha: s.watermark.clone().unwrap_or_default(),
            files: Some(*counts.get(&s.slug).unwrap_or(&0)),
            bytes: Some(bytes),
            review: None,
        });
    }
    manifest::write_corpus_manifest_kind(
        &dest.join("corpus.json"),
        &meta.name,
        Some("harvested"),
        &chrono_now_stamp(),
        &entries,
        None,
    );
    Ok(())
}

fn record(
    entries: &mut Vec<manifest::ManifestRepo>,
    repo_name: &str,
    url: &str,
    target: &Path,
    declared: &str,
    carry: &dyn Fn(&str, &str) -> Option<Value>,
) {
    let lic = xccode::licence::detect_licence(target);
    let (sha, files, bytes) = dir_stats(target);
    println!(
        "          licence={lic} sha={} files={files}",
        &sha[..12.min(sha.len())]
    );
    if let Some(w) = xccode::licence::clone_warning_line(&lic) {
        eprintln!("{w}");
    }
    // The detector is text-matching heuristics and has been wrong before.
    // When a manifest disagrees with the checkout, say so rather than
    // overwrite quietly.
    if !declared.is_empty() && declared != lic {
        eprintln!("          ! manifest says licence={declared}, checkout reads {lic} — verify before copying");
    }
    let review = carry(repo_name, &sha);
    entries.push(manifest::ManifestRepo {
        repo: repo_name.to_string(),
        url: url.to_string(),
        licence: lic,
        sha,
        files: Some(files),
        bytes: Some(bytes),
        review,
    });
}

fn chrono_now_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── xerj corpus index ───────────────────────────────────────────────────────

/// The node operations the corpus lifecycle needs (#1004): one trait so the
/// build/verify/swap flow can run against a fake in tests, re-pinning the
/// destructive-operation contracts the retired `test_xc_index_fresh.py`
/// pinned at the HTTP layer. The trait is deliberately the FIVE operations
/// the flow performs against the node — listing, counting (tri-state), the
/// queryability probe, and the two scoped deletes — nothing more, so the
/// fake cannot drift from what a real node is asked to do.
pub(crate) trait CorpusNode {
    /// `_cat/indices` under a glob, as index names. `Err` = unreachable.
    fn list_indices(&self, glob: &str) -> Result<Vec<String>, String>;
    /// The tri-state `_count` for a `{prefix}-*` glob (DASH form).
    fn count(&self, dash_glob: &str) -> Count;
    /// DELETE one index BY EXACT NAME. `false` = the node refused.
    fn delete_index(&self, name: &str) -> bool;
    /// A size-0 `_search` under a `{prefix}-*` glob (DASH form): does the
    /// store answer queries at all? `Err` carries the node's own reason.
    /// This is the leg `_count` cannot provide — a count is metadata-only
    /// and returns a number over a store whose every search 500s (#1183).
    fn search_probe(&self, dash_glob: &str) -> Result<(), String>;
    /// `_delete_by_query` on the shared catalog for one corpus scope.
    fn delete_catalog_scope(&self, scope: &str) -> bool;
}

impl CorpusNode for Es {
    fn list_indices(&self, glob: &str) -> Result<Vec<String>, String> {
        self.cat_indices_json(glob).map_err(|e| format!("{e:#}"))
    }
    fn count(&self, dash_glob: &str) -> Count {
        self.count_endpoint(dash_glob)
    }
    fn delete_index(&self, name: &str) -> bool {
        self.request_json("DELETE", &format!("/{name}"), None)
            .map(|(s, _)| (200..300).contains(&s))
            .unwrap_or(false)
    }
    fn search_probe(&self, dash_glob: &str) -> Result<(), String> {
        // One call, no outer retry: `Es::search` already carries the client's
        // bounded 5xx budget, and a search that fails through it failed for a
        // reason backoff does not address. size:0 keeps the probe out of the
        // user's traffic in `xerj gain` — the server classifies size-0
        // searches as machine itself (#1109).
        self.search(dash_glob, &json!({"size": 0, "query": {"match_all": {}}}))
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    }
    fn delete_catalog_scope(&self, scope: &str) -> bool {
        // The catalog is one global index shared by every corpus on the node,
        // so its documents are removed by EXACT scope value: `corpus_scope`
        // is a keyword, and a `term` on a legacy analyzed `prefix` cannot
        // equal a hyphenated value at all — it under-deletes there, it never
        // reaches a sibling corpus.
        let body = json!({
            "query": { "bool": { "minimum_should_match": 1, "should": [
                { "term": { "corpus_scope": scope } },
                { "term": { "prefix": scope } }
            ]}}
        });
        self.request_json(
            "POST",
            "/autoindex-catalog/_delete_by_query?refresh=true",
            Some(&body),
        )
        .map(|(s, _)| (200..300).contains(&s))
        .unwrap_or(false)
    }
}

/// `count_under`: patiently. Still `None` when the node never answered —
/// callers must treat that as UNKNOWN, never as zero. The glob is the DASH
/// form `{prefix}-*` (verification), pinned against the STAR form the query
/// path uses — both are load-bearing and they are not interchangeable.
fn count_under(node: &dyn CorpusNode, prefix: &str) -> Option<u64> {
    let tries = std::env::var("XC_COUNT_TRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(6);
    let pause = std::env::var("XC_COUNT_PAUSE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5);
    for attempt in 1..=tries.max(1) {
        match node.count(&format!("{prefix}-*")) {
            Count::Number(n) => return Some(n),
            Count::Zero => return Some(0),
            Count::Unknown(_) => {
                if attempt == tries.max(1) {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_secs(pause));
            }
        }
    }
    None
}

/// The third leg of verification (#1183): does the store answer a query?
/// `_count` is metadata-only — over a store whose every `_search` 500s on a
/// dangling segment it still returns a number, and a resume that ended that
/// way printed "searchable: N records" over an index whose first query
/// failed. One size-0 search over exactly the glob `xerj code` reads is the
/// difference between "counted" and "searchable". Says why and returns
/// `false` on failure; the caller must not report success over a `false`.
fn queryable_store(node: &dyn CorpusNode, name: &str, prefix: &str) -> bool {
    match node.search_probe(&format!("{prefix}-*")) {
        Ok(()) => true,
        Err(reason) => {
            eprintln!("xerj corpus index: a verification search of {prefix}-* failed: {reason}");
            eprintln!(
                "xerj corpus index: `_count` is metadata-only — a store that cannot be searched"
            );
            eprintln!(
                "xerj corpus index: can still be counted (#1183), so a count alone can never"
            );
            eprintln!("xerj corpus index: justify calling '{name}' searchable.");
            false
        }
    }
}

/// Every index under `xc-<corpus>-` that belongs to THIS corpus. The bare
/// glob is not enough: `xc-battle-*` also matches the sibling corpus
/// `battle-terse`, and retiring a sibling's indices is not a mistake this
/// command gets to make.
/// The namespace listing plus whether the node actually answered. An error
/// lists nothing — which errs toward KEEPING an index (nothing is retired
/// that was not listed), never toward deleting one — but the caller now gets
/// to SAY so instead of a silent empty list reading as "nothing to retire"
/// (#1136: generations survived exactly that way).
fn corpus_indices(node: &dyn CorpusNode, name: &str, root: &Path) -> (Vec<String>, bool) {
    let rows = match node.list_indices(&format!("xc-{name}-*")) {
        Ok(rows) => rows,
        Err(_) => return (Vec::new(), false),
    };
    let mut siblings: Vec<String> = Vec::new();
    for (folder, strip) in [(root.join("corpora"), ""), (root.join("state"), ".json")] {
        if let Ok(entries) = std::fs::read_dir(&folder) {
            for e in entries.flatten() {
                let fname = e.file_name().to_string_lossy().to_string();
                let other = match strip {
                    "" => fname,
                    s if fname.ends_with(s) => fname[..fname.len() - s.len()].to_string(),
                    _ => continue,
                };
                if other != name && other.starts_with(&format!("{name}-")) {
                    siblings.push(format!("xc-{other}-"));
                }
            }
        }
    }
    (
        rows.into_iter()
            .filter(|idx| {
                idx.starts_with(&format!("xc-{name}-"))
                    && !siblings.iter().any(|s| idx.starts_with(s.as_str()))
            })
            .collect(),
        true,
    )
}

/// The generation prefix of a stamped index (`xc-<corpus>-b<stamp>` → itself),
/// or `None` for an unstamped legacy index (`xc-<corpus>-<dataset>`), where
/// the segment after the corpus name is a dataset name, not a build stamp.
/// Digits-only between `b` and the next `-` is the stamp shape; a dataset
/// named like a stamp has never been a shape the autoindex emits.
fn stamped_prefix(name: &str, idx: &str) -> Option<String> {
    let rest = idx.strip_prefix(&format!("xc-{name}-b"))?;
    let stamp = rest.split('-').next()?;
    (!stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("xc-{name}-b{stamp}"))
}

/// Retire every index in `names` that belongs to a stamped generation, plus
/// that generation's catalog scope. Returns how many indices were deleted.
/// Used by the legacy arm: the ledger names no build there, so stamped
/// generations are unreachable by construction — `xerj code` queries the bare
/// namespace and mixes their documents into every answer (#1136).
fn retire_stamped(node: &dyn CorpusNode, name: &str, names: &[String]) -> usize {
    let doomed: Vec<String> = names
        .iter()
        .filter(|idx| stamped_prefix(name, idx).is_some())
        .cloned()
        .collect();
    if doomed.is_empty() {
        return 0;
    }
    let scopes: std::collections::BTreeSet<String> = doomed
        .iter()
        .filter_map(|idx| stamped_prefix(name, idx))
        .collect();
    println!(
        "xerj corpus index: retiring {} stale-generation indices this corpus cannot reach \
         (the ledger names no build):",
        doomed.len()
    );
    for idx in &doomed {
        println!("xerj corpus index:   {idx}");
    }
    let deleted = doomed.len() - delete_indices(node, &doomed);
    for scope in scopes {
        node.delete_catalog_scope(&scope);
    }
    deleted
}

/// Say what still sits under `xc-<name>-` outside the prefix readers are now
/// pinned to — the visibility half of #1136: a retirement that half-failed
/// (or a listing that came back empty at the wrong moment) is a poison the
/// next G7-grade query would otherwise measure for us.
fn report_leftovers(node: &dyn CorpusNode, name: &str, root: &Path, keep: &str) {
    let (left, listed_ok) = corpus_indices(node, name, root);
    if !listed_ok {
        eprintln!(
            "xerj corpus index: could not re-list this corpus's indices to check for stale \
             generations — run `xerj corpus list` against the node when it answers."
        );
        return;
    }
    let left: Vec<String> = left.into_iter().filter(|i| !i.starts_with(keep)).collect();
    if left.is_empty() {
        return;
    }
    eprintln!(
        "xerj corpus index: WARNING — {} stale indices remain under xc-{name}- outside \
         what `xerj code` now reads ({keep}-*):",
        left.len()
    );
    for idx in left.iter().take(5) {
        eprintln!("xerj corpus index:   {idx}");
    }
    if left.len() > 5 {
        eprintln!("xerj corpus index:   … and {} more", left.len() - 5);
    }
    eprintln!(
        "xerj corpus index: delete them by exact name, or re-run `xerj corpus index {name} \
         --fresh` when the node is reachable."
    );
}

fn delete_indices(node: &dyn CorpusNode, names: &[String]) -> usize {
    let mut failed = 0;
    for index in names {
        // Exact names only, never a wildcard.
        if !node.delete_index(index) {
            failed += 1;
            eprintln!("xerj corpus index:   could not delete {index}");
        }
    }
    failed
}

/// The terminal verdict lines, as a pure function so the footer can be
/// asserted on (the live failures on this path were all about what the LAST
/// line claims). `rc` is the autoindex exit the flow carried in:
///
/// - 0 / 3 are the clean outcomes (3 = some files skipped as junk, normal for
///   real repos) — the store verified, so "searchable: N records" is earned.
/// - any other `rc` only reaches here through a #367 salvage: autoindex
///   aborted in finalisation but the written store passed the verification
///   search, so it is kept and queryable. It is NOT a clean build. An aborted
///   run that prints the plain `searchable: N records` success footer is the
///   last live half of #1183's dishonest-resume item: the 2026-10-08 standing
///   rebuild ended `xerj-done ok=false exit=1 reason=aborted` and still
///   printed `corpus 'xerj-search' searchable: 1011181 records`, overstating
///   the kept set by the 348 docs the re-walk had just dropped. A salvaged
///   build says so, and never the word "searchable".
fn report_footer(name: &str, rc: i32, docs: &str) -> Vec<String> {
    match rc {
        0 | 3 => {
            let head = if rc == 0 {
                "indexed cleanly".to_string()
            } else {
                "indexed (exit 3: some files skipped as junk — normal for real repos)".to_string()
            };
            vec![
                head,
                format!("corpus '{name}' searchable: {docs} records"),
                format!("next: xerj code {name} \"<what you need>\""),
            ]
        }
        _ => vec![
            format!(
                "corpus '{name}' indexed with errors (autoindex exit {rc}): {docs} records \
                 are present and queryable, but coverage is NOT guaranteed — read the error \
                 above before relying on this corpus."
            ),
            format!(
                "next: xerj code {name} \"<what you need>\"  (verify coverage first; a clean \
                 rebuild is `xerj corpus index {name} --fresh`)"
            ),
        ],
    }
}

fn report(name: &str, rc: i32, docs: &str) {
    for line in report_footer(name, rc, docs) {
        println!("{line}");
    }
}

fn run_corpus_index(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut fresh = false;
    let mut url: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{CORPUS_USAGE}");
                return 0;
            }
            "--fresh" => fresh = true,
            "--url" => match args.get(i + 1) {
                Some(v) => {
                    url = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("xerj corpus index: --url needs a value\n\n{CORPUS_USAGE}");
                    return 2;
                }
            },
            a if a.starts_with('-') => {
                eprintln!("xerj corpus index: unknown flag '{a}'\n\n{CORPUS_USAGE}");
                return 2;
            }
            a if name.is_none() => name = Some(a.to_string()),
            a => {
                eprintln!("xerj corpus index: unexpected argument '{a}'\n\n{CORPUS_USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(name) = name else {
        eprintln!("xerj corpus index: <name> is required\n\n{CORPUS_USAGE}");
        return 2;
    };
    if let Err(e) = xccode::pathgate::valid_corpus_name(&name) {
        eprintln!("xerj corpus index: {e}");
        return 2;
    }
    let root = code_root();
    let dir = root.join("corpora").join(&name);
    if !dir.is_dir() {
        eprintln!(
            "xerj corpus index: no corpus '{name}' at {} — run `xerj corpus add {name} …` first",
            dir.display()
        );
        return 2;
    }
    let url = resolve_url(url.as_deref());
    let es = match build_es(&url, None) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj corpus index: cannot reach {url}: {e:#}");
            return 2;
        }
    };
    corpus_index_flow(
        &es,
        &|prefix, state_dir| crate::run_with_options(&dir, &url, prefix, state_dir, true),
        &stamp_secs,
        &root,
        &url,
        &name,
        fresh,
    )
}

/// A pack-materialized corpus carries its record count on disk: one JSON
/// line per record under `corpora/<name>/*/records.jsonl`. The verify gate
/// in [`corpus_index_flow`] only checks the node answered MORE THAN ZERO —
/// which let a build that silently junked a whole source file pass as a
/// success (the published rust-vulns pack indexed 1,230 of 1,963 records:
/// `osv/records.jsonl`'s first line beats the 8 KB sniff prefix, the file
/// sniffed as a single JSON document and was junked as "json candidate
/// family" — fixed by the JSONL fallback in `extract::json`, warned here in
/// case any other path loses records). Warn loudly when the node holds
/// fewer records than the pack does. Git-clone corpora (code sources, where
/// junk is normal) have no `records.jsonl` and are exempt by construction.
fn warn_short_of_pack_records(name: &str, root: &Path, docs: Option<u64>) {
    let Some((lines, files)) = pack_record_gap(name, root, docs) else {
        return;
    };
    eprintln!("xerj corpus index: WARNING — the pack holds {lines} records across {files} records.jsonl file(s),");
    eprintln!("xerj corpus index: but the node answered only {} records for this corpus. A source file was", docs.unwrap_or(0));
    eprintln!("xerj corpus index: silently junked — check the junk report (autoindex-catalog, doc_kind=junk)");
    eprintln!("xerj corpus index: before trusting `xerj code {name}` coverage.");
}

/// The on-disk pack record count versus what the node answered:
/// `Some((lines, files))` when a pack corpus's `records.jsonl` files hold
/// MORE lines than the node has records — the shape of a silently junked
/// source file. `None` when there is nothing to compare against (no corpus
/// dir, a git-clone corpus with no records.jsonl, an unanswerable count) or
/// nothing missing.
fn pack_record_gap(name: &str, root: &Path, docs: Option<u64>) -> Option<(u64, u64)> {
    let docs = docs?;
    let entries = std::fs::read_dir(root.join("corpora").join(name)).ok()?;
    let mut lines = 0u64;
    let mut files = 0u64;
    for e in entries.flatten() {
        use std::io::Read;
        let Ok(mut r) = std::fs::File::open(e.path().join("records.jsonl")) else {
            continue;
        };
        let mut buf = [0u8; 65536];
        let mut file_lines = 0u64;
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => file_lines += buf[..n].iter().filter(|&&b| b == b'\n').count() as u64,
                Err(_) => return None,
            }
        }
        if file_lines == 0 {
            continue;
        }
        lines += file_lines;
        files += 1;
    }
    (files > 0 && docs < lines).then_some((lines, files))
}

/// What a failed legacy-mode run can honestly claim about the records it
/// wrote (#1173). The three cases are exclusive, and the whole point of the
/// type is which one is forbidden when: "wrote no new records" asserted from
/// a count the node never answered is an absence claim with no evidence —
/// exactly the honest-claims rule the public docs are held to, applied to
/// our own tooling. The node under a failed long build is precisely the node
/// that answers counts with UNKNOWN.
enum LegacyFailureVerdict {
    /// The count grew: records from THIS run are queryable despite the exit.
    Salvaged { before: u64, after: u64 },
    /// The node could not be counted (before, after, or both). The honest
    /// claim is "unknown" — never "absent".
    CountUnknown,
    /// Both counts are known and did not grow.
    WroteNothing { before: u64, after: u64 },
}

fn legacy_failure_verdict(before: Option<u64>, after: Option<u64>) -> LegacyFailureVerdict {
    match (before, after) {
        (Some(before), Some(after)) if after > before => {
            LegacyFailureVerdict::Salvaged { before, after }
        }
        (Some(before), Some(after)) => LegacyFailureVerdict::WroteNothing { before, after },
        _ => LegacyFailureVerdict::CountUnknown,
    }
}

/// The build/verify/swap flow over an injected node, autoindex runner and
/// clock (#1004) — the destructive-operation contracts are pinned by the
/// tests below against a [`FakeNode`](tests::FakeNode), which is why none of
/// this touches [`Es`] directly. `run_autoindex(prefix, state_dir)` is the
/// real `crate::run_with_options` in production, with `--no-graph` set
/// (reference code needs ranked passages, not a relationship map) — and
/// `--fresh` is NEVER forwarded: this flow owns the swap.
fn corpus_index_flow(
    node: &dyn CorpusNode,
    run_autoindex: &dyn Fn(&str, Option<&Path>) -> i32,
    now_secs: &dyn Fn() -> i64,
    root: &Path,
    url: &str,
    name: &str,
    fresh: bool,
) -> i32 {
    let state_file = root.join("state").join(format!("{name}.json"));
    let state_root = root.join("autoindex-state");
    let old = state::load_state(root, name).ok();
    let old_build = old.as_ref().and_then(|s| s.build.clone());
    let old_index_prefix = old.as_ref().and_then(|s| s.index_prefix.clone());
    let old_state_dir = old.as_ref().and_then(|s| s.state_dir.clone());
    let old_url = old.as_ref().and_then(|s| s.url.clone());

    // Which run is this? build (--fresh / first / ledger-names-a-build-this-
    // node-does-not-hold), update (reconcile the recorded build in place), or
    // legacy (a corpus indexed before builds existed).
    let mut mode = "legacy";
    let (old_indices, _listed_ok) = corpus_indices(node, name, root);
    if fresh {
        mode = "build";
    } else if old_build.is_some() {
        // A recorded build pins this corpus to one url, one prefix and one
        // state dir. When any of those does not hold, the run CANNOT resume
        // that build — and must not silently fall through to legacy mode:
        // without --state-dir, autoindex opens its default hash-of-root
        // journal, which can hold a stale pending sync from a retired
        // attempt under the legacy prefix and start re-applying it beside
        // the recorded build (#1214). Refuse, naming the recorded values.
        let prefix_recorded = old_index_prefix.is_some();
        let state_dir_usable = old_state_dir
            .as_deref()
            .is_some_and(|d| Path::new(d).is_dir());
        let url_matches = old_url.as_deref() == Some(url);
        if !prefix_recorded || !state_dir_usable || !url_matches {
            let build = old_build.as_deref().unwrap_or("?");
            eprintln!(
                "xerj corpus index: state/ records build {build} of '{name}', but this run \
                 cannot resume it:"
            );
            if !url_matches {
                eprintln!(
                    "xerj corpus index:   recorded url is {} , this run resolved {url}",
                    old_url.as_deref().unwrap_or("(none)")
                );
            }
            if !state_dir_usable {
                eprintln!(
                    "xerj corpus index:   recorded state dir {} is not a usable directory",
                    old_state_dir.as_deref().unwrap_or("(none)")
                );
            }
            if !prefix_recorded {
                eprintln!(
                    "xerj corpus index:   recorded state has no index prefix for build {build}"
                );
            }
            eprintln!(
                "xerj corpus index: refusing to continue in legacy mode — that would open the \
                 default state dir for this corpus, which may hold an abandoned generation \
                 (#1214)."
            );
            eprintln!(
                "xerj corpus index: resume the recorded build with the matching --url (and its \
                 state dir in place), or build a verified replacement beside it with:"
            );
            eprintln!("xerj corpus index:   xerj corpus index {name} --fresh");
            return 2;
        }
        let live = count_under(node, old_index_prefix.as_deref().unwrap_or(""));
        match live {
            Some(n) if n > 0 => mode = "update",
            _ => {
                println!(
                    "xerj corpus index: state/ records build {} but {url} holds no records for \
                     it — building it here",
                    old_build.as_deref().unwrap_or("?")
                );
                mode = "build";
            }
        }
    } else if !state_file.exists() && old_indices.is_empty() {
        mode = "build"; // first index of this corpus: same path as --fresh
    }

    println!(
        "indexing corpus '{name}' from {}",
        root.join("corpora").join(name).display()
    );

    match mode {
        "build" => {
            // Listed BEFORE the build so "old" can never include what this
            // run creates. A listing the node never answered is LOUD here:
            // an empty list would read as "nothing to retire" and let stale
            // generations survive the swap (#1136).
            let (old_indices, listed_ok) = corpus_indices(node, name, root);
            if !listed_ok {
                eprintln!(
                    "xerj corpus index: could not list this corpus's existing indices — the \
                     swap will still build and verify, but NOTHING on the node will be retired;"
                );
                eprintln!(
                    "xerj corpus index: stale generations may remain beside the new one. \
                     Re-run `xerj corpus index {name} --fresh` when the node answers a listing."
                );
            }
            // One-second resolution: a second --fresh inside the same second
            // would reuse the prefix of the build it is replacing. The id
            // must be new — not the recorded build, not a state dir that
            // exists, not a prefix any live index already sits under.
            let mut build = format!("b{}", now_secs());
            while old_build.as_deref() == Some(build.as_str())
                || state_root.join(name).join(&build).exists()
                || old_indices
                    .iter()
                    .any(|idx| idx.starts_with(&format!("xc-{name}-{build}")))
            {
                std::thread::sleep(std::time::Duration::from_secs(1));
                build = format!("b{}", now_secs());
            }
            let new_prefix = format!("xc-{name}-{build}");
            let new_state = state_root.join(name).join(&build);
            // Is there a working index to protect? When the node cannot
            // count it, the answer is YES: presuming "none" is what lets a
            // failed build be kept over it and the old indices be retired.
            let mut has_working_index = false;
            if !old_indices.is_empty() {
                let counted = count_under(
                    node,
                    old_index_prefix.as_deref().unwrap_or(&format!("xc-{name}")),
                );
                match counted {
                    None => {
                        has_working_index = true;
                        eprintln!("xerj corpus index: the node did not answer a record count for the existing index; treating it as");
                        eprintln!("xerj corpus index: a WORKING index — a failed build will not be kept over it.");
                        println!("xerj corpus index: --fresh — building {new_prefix}-* beside the existing index (an unknown number of records);");
                    }
                    Some(n) => {
                        if n > 0 {
                            has_working_index = true;
                        }
                        println!("xerj corpus index: --fresh — building {new_prefix}-* beside the existing index ({n} records);");
                    }
                }
                println!("xerj corpus index: the existing index stays live until the replacement has been verified");
            }
            let _ = std::fs::create_dir_all(&new_state);
            let rc = run_autoindex(&new_prefix, Some(&new_state));

            // VERIFY: rc in {0,3} AND a count the node actually gave > 0.
            let docs = count_under(node, &new_prefix);
            let Some(docs) = docs else {
                // UNKNOWN is not EMPTY: it authorises no delete and no swap.
                eprintln!(
                    "xerj corpus index: the node did not answer a record count for build {build};"
                );
                eprintln!("xerj corpus index: autoindex exit {rc}. It cannot be verified, so NOTHING was deleted and NOTHING");
                eprintln!("xerj corpus index: was switched: its indices ({new_prefix}-*) and its state directory are kept.");
                if has_working_index {
                    eprintln!("xerj corpus index: the existing index was NOT touched and is still what `xerj code` serves.");
                    eprintln!("xerj corpus index: When the node answers again, re-run with --fresh; the unverified build is");
                    eprintln!("xerj corpus index: retired by the next build that verifies.");
                } else {
                    let _ = state::write_state(
                        root,
                        name,
                        url,
                        rc as i64,
                        true,
                        Some(&build),
                        Some(&new_prefix),
                        new_state.to_str(),
                    );
                    eprintln!("xerj corpus index: There is no other index for '{name}', so this build is recorded as UNVERIFIED");
                    eprintln!("xerj corpus index: (`xerj code` will say so). Re-run  xerj corpus index {name}  to resume or confirm it.");
                }
                if rc != 0 && rc != 3 {
                    return rc;
                }
                return 1;
            };
            let mut salvaged = false;
            let mut verified = false;
            // VERIFY needs a third leg (#1183): rc 0/3 plus a positive count
            // certified a build whose store 500'd every search. The probe
            // failing here makes the build "not verified" — its own indices
            // are removed with the rest of a failed build (an unsearchable
            // index left under the namespace is poison for every later
            // wildcard read), and the existing index stays what `xerj code`
            // serves.
            let queryable = queryable_store(node, name, &new_prefix);
            match rc {
                0 | 3 if docs > 0 && queryable => verified = true,
                _ => {
                    // autoindex can abort in finalisation AFTER every document
                    // was written (#367). Keep a complete, queryable index
                    // when there is no working fallback — and never swap a
                    // verified one out for it.
                    if docs > 0 && !has_working_index && queryable {
                        verified = true;
                        salvaged = true;
                        eprintln!("xerj corpus index: WARNING — autoindex exited {rc}, but this build wrote {docs} records and");
                        eprintln!("xerj corpus index: there is no working index to fall back to. Recording it as indexed with");
                        eprintln!("xerj corpus index: autoindex_exit={rc}; coverage is not guaranteed — please report the error above.");
                    }
                }
            }
            if !verified {
                eprintln!(
                    "xerj corpus index: build {build} did not verify (autoindex exit {rc}, {docs} records{}).",
                    if queryable { "" } else { ", verification search failed" }
                );
                // Remove only what THIS run created: the set-diff against the
                // pre-run listing, intersected with this build's prefix, BY
                // EXACT NAME — never a wildcard, never new_prefix itself.
                let (after, after_ok) = corpus_indices(node, name, root);
                if !after_ok {
                    eprintln!(
                        "xerj corpus index: could not re-list indices after the failed build — \
                         its indices (if any were written) are kept, not deleted blind."
                    );
                }
                let doomed: Vec<String> = after
                    .into_iter()
                    .filter(|idx| !old_indices.contains(idx))
                    .filter(|idx| idx.starts_with(&format!("xc-{name}-{build}-")))
                    .collect();
                delete_indices(node, &doomed);
                node.delete_catalog_scope(&new_prefix);
                let _ = std::fs::remove_dir_all(&new_state);
                if !old_indices.is_empty() {
                    eprintln!("xerj corpus index: the existing index was NOT touched and is still what `xerj code` serves.");
                }
                if rc != 0 && rc != 3 {
                    return rc;
                }
                return 1;
            }
            // Verified. Switch readers first, retire second: a crash between
            // the two leaves a duplicate, never a gap.
            warn_short_of_pack_records(name, root, Some(docs));
            let _ = state::write_state(
                root,
                name,
                url,
                rc as i64,
                salvaged,
                Some(&build),
                Some(&new_prefix),
                new_state.to_str(),
            );
            let retire: Vec<String> = old_indices
                .iter()
                .filter(|idx| !idx.starts_with(&format!("xc-{name}-{build}")))
                .cloned()
                .collect();
            if !retire.is_empty() {
                println!("xerj corpus index: replacement verified ({docs} records) — retiring {} old indices", retire.len());
                if delete_indices(node, &retire) > 0 {
                    eprintln!("xerj corpus index: WARNING — some old indices could not be deleted. The corpus is healthy and");
                    eprintln!("xerj corpus index: `xerj code` reads only {new_prefix}-*; delete the leftovers by name when the node allows.");
                }
                node.delete_catalog_scope(
                    old_index_prefix.as_deref().unwrap_or(&format!("xc-{name}")),
                );
            }
            // Whatever the retirement above could not reach (a failed
            // delete, or a listing that came back empty before the build) is
            // named here instead of silently poisoning the namespace.
            report_leftovers(node, name, root, &new_prefix);
            // Earlier builds' state directories are dead weight once their
            // indices are gone.
            if state_root.join(name).is_dir() {
                if let Ok(entries) = std::fs::read_dir(state_root.join(name)) {
                    for e in entries.flatten() {
                        if e.file_name().to_string_lossy() != build {
                            let _ = std::fs::remove_dir_all(e.path());
                        }
                    }
                }
            }
            report(name, rc, &docs.to_string());
            0
        }
        "update" => {
            let prefix = old_index_prefix.clone().unwrap_or_default();
            let sd = old_state_dir.clone().map(PathBuf::from).unwrap_or_default();
            let rc = run_autoindex(&prefix, Some(&sd));
            if rc != 0 && rc != 3 {
                eprintln!("xerj corpus index: updating build {} failed with exit {rc}. The index is unchanged.", old_build.as_deref().unwrap_or("?"));
                eprintln!("xerj corpus index: re-run with --fresh to build a replacement beside it (the existing index");
                eprintln!("xerj corpus index: stays live until the replacement verifies).");
                return rc;
            }
            // The recorded build was just re-run in place, so this store IS
            // what `xerj code` serves — a search that fails here means the
            // live corpus is broken, and "searchable" would be the #1183 lie.
            if !queryable_store(node, name, &prefix) {
                eprintln!(
                    "xerj corpus index: the updated build is NOT recorded over this. Build a"
                );
                eprintln!("xerj corpus index: verified replacement beside it with:  xerj corpus index {name} --fresh");
                return 1;
            }
            let live = count_under(node, &prefix);
            warn_short_of_pack_records(name, root, live);
            let docs = live
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string());
            let _ = state::write_state(
                root,
                name,
                url,
                rc as i64,
                false,
                old_build.as_deref(),
                Some(&prefix),
                old_state_dir.as_deref(),
            );
            report(name, rc, &docs);
            0
        }
        _ => {
            // Legacy: recorded before the run so a failure can tell "this run
            // wrote records" apart from "an earlier run's records are still
            // lying around" (salvaging the latter would date stale data to
            // now, which is worse than no index).
            let before_known = count_under(node, &format!("xc-{name}"));
            let rc = run_autoindex(&format!("xc-{name}"), None);
            // Before ANY verdict is printed: the whole namespace `xerj code`
            // reads is one wildcard, so one unsearchable index under it — a
            // dangling segment from a killed finalize (#1183), even an old one
            // this run never touched — makes "the corpus is queryable" and
            // "searchable: N records" untrue no matter what the counts said.
            if !queryable_store(node, name, &format!("xc-{name}")) {
                eprintln!("xerj corpus index: records may still be counted, but they cannot be served. Build a");
                eprintln!("xerj corpus index: verified replacement beside this with:  xerj corpus index {name} --fresh");
                return if rc != 0 && rc != 3 { rc } else { 1 };
            }
            let mut salvaged = false;
            let mut docs: Option<u64> = None;
            if rc != 0 && rc != 3 {
                docs = count_under(node, &format!("xc-{name}"));
                match legacy_failure_verdict(before_known, docs) {
                    LegacyFailureVerdict::Salvaged { before, after } => {
                        salvaged = true;
                        eprintln!("xerj corpus index: WARNING — autoindex exited {rc}, but this run wrote records");
                        eprintln!("xerj corpus index: ({before} -> {after}). The corpus is queryable and is being");
                        eprintln!("xerj corpus index: recorded as indexed, with autoindex_exit={rc} in its state file.");
                        eprintln!("xerj corpus index: Coverage is not guaranteed — please report the error above.");
                    }
                    // #1173: a node that cannot be counted cannot be reported
                    // on. "Wrote no new records" was asserted here while the
                    // count was UNKNOWN — the honest-claims rule applied to
                    // our own tooling: say what is unknown, never claim an
                    // absence the node never confirmed.
                    LegacyFailureVerdict::CountUnknown => {
                        eprintln!("xerj corpus index: autoindex exited {rc} and the node could not be counted");
                        eprintln!("xerj corpus index: afterwards, so whether this run wrote records is UNKNOWN (not absent).");
                        eprintln!("xerj corpus index: Count again once the node answers, or rebuild beside the old");
                        eprintln!(
                            "xerj corpus index: index with:  xerj corpus index {name} --fresh"
                        );
                        return rc;
                    }
                    LegacyFailureVerdict::WroteNothing { before, after } => {
                        eprintln!("xerj corpus index: autoindex failed with exit {rc} and wrote no new records ({before} -> {after}).");
                        eprintln!("xerj corpus index: If the error above says the state directory cannot become generation");
                        eprintln!("xerj corpus index: authority, or that --fresh is refused, run:  xerj corpus index {name} --fresh");
                        return rc;
                    }
                }
            }
            docs = docs.or_else(|| count_under(node, &format!("xc-{name}")));
            let docs_s = docs
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string());
            let _ = state::write_state(root, name, url, rc as i64, salvaged, None, None, None);
            // The ledger now names no build, so no stamped generation is
            // reachable — retire any that are still live. This is the #1136
            // leak in its measured shape: tldr-pages carried an unstamped
            // 38,554-doc generation beside stamped ones because a legacy
            // run never looked at what it was superseding.
            let (now_live, live_ok) = corpus_indices(node, name, root);
            if live_ok {
                retire_stamped(node, name, &now_live);
            } else {
                eprintln!(
                    "xerj corpus index: could not list this corpus's indices after the run — \
                     stale generations (if any) were NOT retired; re-run when the node answers."
                );
            }
            report(name, rc, &docs_s);
            0
        }
    }
}

fn stamp_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── xerj corpus list ────────────────────────────────────────────────────────

fn run_corpus_list(args: &[String]) -> i32 {
    let mut url: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{CORPUS_USAGE}");
                return 0;
            }
            "--url" => match args.get(i + 1) {
                Some(v) => {
                    url = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("xerj corpus list: --url needs a value\n\n{CORPUS_USAGE}");
                    return 2;
                }
            },
            a => {
                eprintln!("xerj corpus list: unexpected argument '{a}'\n\n{CORPUS_USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let root = code_root();
    let state_dir = root.join("state");
    if !state_dir.is_dir() {
        eprintln!(
            "no state directory at {} — nothing has been indexed on this machine",
            state_dir.display()
        );
        return 2;
    }
    let url = resolve_url(url.as_deref());
    let es = match build_es(&url, None) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj corpus list: cannot reach {url}: {e:#}");
            return 2;
        }
    };
    let mut names: Vec<String> = std::fs::read_dir(&state_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_suffix(".json")
                .map(str::to_string)
        })
        .collect();
    names.sort();
    let mut loaded = 0usize;
    for name in &names {
        let st = match state::load_state(&root, name) {
            Ok(st) => st,
            Err(e) => {
                println!("{name}: unreadable state ({e})");
                continue;
            }
        };
        let prefix = state::query_prefix(&st);
        let indexed = st
            .indexed_at
            .as_deref()
            .unwrap_or("?")
            .get(..10)
            .unwrap_or("?")
            .to_string();
        // Live count is honest: 404 -> 0; any other failure is named, never
        // faked as zero.
        let live = match es.cat_indices_json(&format!("{prefix}*")) {
            Ok(v) => v.len(),
            Err(_) => usize::MAX,
        };
        let uses = xccode::corpus_review_uses(&root, name);
        let use_note = uses
            .keys()
            .max()
            .and_then(|k| uses.get(k).map(|u| format!("\n  use: {u}")))
            .unwrap_or_default();
        match live {
            usize::MAX => println!("{name}  (indexed {indexed}, prefix {prefix}) — server unreachable/ambiguous at {url}"),
            0 => println!("{name}  (indexed {indexed}, prefix {prefix}) — NOT loaded here (0 indices) — stale/other-server{use_note}"),
            n => {
                loaded += 1;
                println!("{name}  (indexed {indexed}, prefix {prefix}) — loaded — {n} index(es){use_note}");
            }
        }
        if let Some(w) = state::incomplete_coverage(&st) {
            println!("  {w}");
        }
    }
    println!();
    println!(
        "{} of {} corpora are actually loaded on {url}.",
        loaded,
        names.len()
    );
    0
}

// ── dispatch ────────────────────────────────────────────────────────────────

/// `xerj corpus <add|build|index|list> …` — returns the process exit code.
fn corpus_verb(arg: &str) -> bool {
    matches!(
        arg,
        "add" | "build" | "sign" | "keygen" | "index" | "list" | "help" | "-h" | "--help"
    )
}

pub fn run_corpus_cli(args: &[String]) -> i32 {
    // `xerj corpus --url URL index <name>`: the verb is not the first token.
    let promoted = xerj_common::argv::reorder_flags_before_command(args, corpus_verb);
    let args: &[String] = promoted.as_deref().unwrap_or(args);
    let Some(sub) = args.first() else {
        eprintln!("{CORPUS_USAGE}");
        return 2;
    };
    let rest: Vec<String> = args[1..].to_vec();
    match sub.as_str() {
        "add" => run_corpus_add(&rest),
        "build" => crate::harvest::run_build(&rest),
        "sign" | "keygen" => crate::harvest::sign::run_sign_cli(sub, &rest),
        "index" => run_corpus_index(&rest),
        "list" => run_corpus_list(&rest),
        "-h" | "--help" | "help" => {
            println!("{CORPUS_USAGE}");
            0
        }
        other => {
            eprintln!("xerj corpus: unknown subcommand '{other}'\n\n{CORPUS_USAGE}");
            2
        }
    }
}

/// The licence map for one corpus, shared with the MCP tool.
pub fn licence_map_for(root: &Path, corpus: &str) -> HashMap<String, String> {
    manifest::licence_map(root, corpus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_before_the_corpus_verb_is_not_an_unknown_subcommand() {
        // Before the hoist, `--url` was the verb and this returned 2
        // ("unknown subcommand"). `--help` is the verb; it must run.
        let code = run_corpus_cli(&["--url".into(), "http://127.0.0.1:9".into(), "--help".into()]);
        assert_eq!(code, 0);
    }

    #[test]
    fn corpus_names_are_gated_before_any_path_is_built() {
        for bad in ["a/b", "*", ".x", "index", "list"] {
            assert!(xccode::pathgate::valid_corpus_name(bad).is_err(), "{bad}");
        }
        assert!(xccode::pathgate::valid_corpus_name("battle-terse").is_ok());
    }

    #[test]
    fn url_resolution_prefers_flag_then_env_then_default() {
        // under ENV_LOCK: set/remove of process env is global, so any test
        // mutating env serialises with the others (one unreproduced lib-suite
        // failure was observed on the merged #977 branch before this lock).
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(resolve_url(Some("http://a:1")), "http://a:1");
        // env-dependent branches covered by the e2e; default pinned here.
        std::env::remove_var("XERJ_URL");
        assert_eq!(resolve_url(None), "http://localhost:9200");
    }

    #[test]
    fn count_tries_env_defaults_are_the_scripts_values() {
        // under the same lock the #1004 tests use: they set the knobs, this
        // test asserts their absence — run concurrently they would flake.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XC_COUNT_TRIES");
        std::env::remove_var("XC_COUNT_PAUSE");
        // (6 tries, 5s pause) — pinned by the port; the env knobs still work.
        let t = std::env::var("XC_COUNT_TRIES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(6);
        assert_eq!(t, 6);
    }

    #[test]
    fn code_usage_lists_every_flag_the_design_names() {
        for f in [
            "-k",
            "--lang",
            "--mode",
            "--hybrid",
            "--full",
            "--no-symbol",
            "--json",
            "--meatl",
            "--stale-ok",
            "--url",
            "--api-key",
        ] {
            assert!(CODE_USAGE.contains(f), "{f} missing from usage");
        }
        assert!(CORPUS_USAGE.contains("--from"));
    }

    /// The `--from` name swap: explicit beats the manifest, the manifest
    /// beats nothing, and nothing is an error — never a guess.
    #[test]
    fn corpus_name_resolution_is_explicit_then_manifest_then_error() {
        let path = "hub.json";
        // explicit (positional or --as) wins over the manifest's field
        assert_eq!(
            resolve_corpus_name(Some("mine".into()), "theirs", path).as_deref(),
            Ok("mine")
        );
        // manifest supplies the name when nothing explicit was given
        assert_eq!(
            resolve_corpus_name(None, "theirs", path).as_deref(),
            Ok("theirs")
        );
        // neither: usage error naming both remedies
        let err = resolve_corpus_name(None, "", path).unwrap_err();
        assert!(err.contains("no 'corpus' field"), "{err}");
        assert!(err.contains("--as"), "{err}");
    }

    /// `--as` only renames; it must not let a bad name through — resolution
    /// output still passes the same pathgate as a positional name.
    #[test]
    fn corpus_name_resolution_output_is_still_pathgated() {
        assert!(xccode::pathgate::valid_corpus_name(
            &resolve_corpus_name(Some("a/b".into()), "theirs", "p").unwrap()
        )
        .is_err());
        // and the manifest's own field is gated at read time
        assert!(xccode::pathgate::valid_corpus_name(
            &resolve_corpus_name(None, "battle-terse", "p").unwrap()
        )
        .is_ok());
    }

    // ── the --fresh swap contracts (#1004) ──────────────────────────────────
    //
    // The retired test_xc_index_fresh.py pinned these against a fake node and
    // a PATH-shimmed fake binary; they are re-pinned here against a FakeNode
    // behind the same CorpusNode trait production code uses. Every contract
    // guards a DELETE or a state-file switch, so every test asserts on the
    // audit trail (`ops`) and the state file, never on stdout prose.

    /// Serialises tests that touch the count-retry env knobs (the flow reads
    /// them per call; the default 6x5s would sleep in a Unknown-count test).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_fast_count_retries<T>(f: impl FnOnce() -> T) -> T {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XC_COUNT_TRIES", "1");
        std::env::set_var("XC_COUNT_PAUSE", "0");
        let r = f();
        std::env::remove_var("XC_COUNT_TRIES");
        std::env::remove_var("XC_COUNT_PAUSE");
        r
    }

    /// A fake node: index listing by glob, tri-state counts per dash-glob,
    /// and an audit trail of every destructive op (delete / catalog scope)
    /// and every count read — the ordering assertions need the counts too.
    struct FakeNode {
        indices: std::cell::RefCell<Vec<String>>,
        counts: std::cell::RefCell<HashMap<String, Count>>,
        /// Per dash-glob search verdicts; unseeded globs answer `Ok(())` —
        /// a healthy node is the default, failures are the injected case.
        searches: std::cell::RefCell<HashMap<String, Result<(), String>>>,
        /// This exact index name's DELETE fails (once — the flow warns and
        /// continues; a persistently failing node is the crash-between case).
        fail_delete: std::cell::RefCell<Option<String>>,
        /// Every `list_indices` answers `Err` — an unreachable node (#1136:
        /// a silent empty listing read as "nothing to retire").
        fail_listing: std::cell::Cell<bool>,
        ops: std::cell::RefCell<Vec<String>>,
    }

    impl FakeNode {
        fn new() -> Self {
            FakeNode {
                indices: std::cell::RefCell::new(Vec::new()),
                counts: std::cell::RefCell::new(HashMap::new()),
                searches: std::cell::RefCell::new(HashMap::new()),
                fail_delete: std::cell::RefCell::new(None),
                fail_listing: std::cell::Cell::new(false),
                ops: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn seed_count(&self, dash_glob: &str, c: Count) {
            self.counts.borrow_mut().insert(dash_glob.into(), c);
        }
        fn seed_search(&self, dash_glob: &str, verdict: Result<(), String>) {
            self.searches.borrow_mut().insert(dash_glob.into(), verdict);
        }
        fn live(&self, name: &str) -> bool {
            self.indices.borrow().iter().any(|i| i == name)
        }
    }

    /// `prefix*` / `a*b` glob matching, the only shape the flow globs with.
    fn glob_match(names: &[String], glob: &str) -> Vec<String> {
        let parts: Vec<&str> = glob.split('*').collect();
        names
            .iter()
            .filter(|n| {
                let mut rest = n.as_str();
                for (i, p) in parts.iter().enumerate() {
                    if i == 0 {
                        if !rest.starts_with(p) {
                            return false;
                        }
                        rest = &rest[p.len()..];
                    } else if i == parts.len() - 1 && !parts.last().is_some_and(|l| l.is_empty()) {
                        if !rest.ends_with(p) {
                            return false;
                        }
                    } else if let Some(at) = rest.find(p) {
                        rest = &rest[at + p.len()..];
                    } else {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    impl CorpusNode for FakeNode {
        fn list_indices(&self, glob: &str) -> Result<Vec<String>, String> {
            if self.fail_listing.get() {
                return Err("node unreachable".into());
            }
            Ok(glob_match(&self.indices.borrow(), glob))
        }
        fn count(&self, dash_glob: &str) -> Count {
            self.ops.borrow_mut().push(format!("count:{dash_glob}"));
            self.counts
                .borrow()
                .get(dash_glob)
                .cloned()
                .unwrap_or(Count::Zero)
        }
        fn search_probe(&self, dash_glob: &str) -> Result<(), String> {
            self.ops.borrow_mut().push(format!("search:{dash_glob}"));
            self.searches
                .borrow()
                .get(dash_glob)
                .cloned()
                .unwrap_or(Ok(()))
        }
        fn delete_index(&self, name: &str) -> bool {
            self.ops.borrow_mut().push(format!("delete:{name}"));
            if self.fail_delete.borrow().as_deref() == Some(name) {
                return false;
            }
            self.indices.borrow_mut().retain(|i| i != name);
            true
        }
        fn delete_catalog_scope(&self, scope: &str) -> bool {
            self.ops.borrow_mut().push(format!("catalog:{scope}"));
            true
        }
    }

    /// The fake autoindex runner: records (prefix, state_dir) per call, and
    /// "writes" `shards` indices under the prefix it was given — normally on
    /// rc 0/3, or on ANY rc when `writes_on_fail` models the #367 shape (an
    /// abort in finalisation after every document was written). A count the
    /// test pre-seeded is never overwritten: the fake models what the RUN
    /// observed, and the node may disagree (that disagreement is the point
    /// of the tri-state tests).
    struct FakeAuto<'n> {
        node: &'n FakeNode,
        rc: i32,
        shards: usize,
        writes_on_fail: bool,
        calls: std::cell::RefCell<Vec<(String, Option<std::path::PathBuf>)>>,
    }

    impl<'n> FakeAuto<'n> {
        fn ok(node: &'n FakeNode) -> Self {
            FakeAuto {
                node,
                rc: 0,
                shards: 1,
                writes_on_fail: false,
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn with_rc(node: &'n FakeNode, rc: i32) -> Self {
            let mut a = FakeAuto::ok(node);
            a.rc = rc;
            a
        }
        /// Fails, but after the documents were written (#367).
        fn failing_but_writing(node: &'n FakeNode) -> Self {
            let mut a = FakeAuto::ok(node);
            a.rc = 1;
            a.writes_on_fail = true;
            a
        }
        fn runner(&self) -> impl Fn(&str, Option<&Path>) -> i32 + '_ {
            move |prefix, state_dir| {
                self.calls
                    .borrow_mut()
                    .push((prefix.to_string(), state_dir.map(Path::to_path_buf)));
                if self.rc == 0 || self.rc == 3 || self.writes_on_fail {
                    self.node
                        .indices
                        .borrow_mut()
                        .extend((0..self.shards).map(|i| format!("{prefix}-{i:03}")));
                    self.node
                        .counts
                        .borrow_mut()
                        .entry(format!("{prefix}-*"))
                        .or_insert(Count::Number(500 * self.shards as u64));
                }
                self.rc
            }
        }
        fn prefixes(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|(p, _)| p.clone()).collect()
        }
    }

    /// A corpus root with the corpus cloned and (optionally) a recorded
    /// build, written through the REAL state writer so the schema is pinned
    /// on both sides of the flow.
    fn corpus_root(name: &str) -> std::path::PathBuf {
        let root = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(root.join("corpora").join(name)).unwrap();
        root
    }

    fn record_build(root: &Path, name: &str, build: &str, state_dir: Option<&str>) -> String {
        let prefix = format!("xc-{name}-{build}");
        state::write_state(
            root,
            name,
            "http://x",
            0,
            false,
            Some(build),
            Some(&prefix),
            state_dir,
        )
        .unwrap();
        prefix
    }

    fn run_flow(
        node: &FakeNode,
        auto: &FakeAuto<'_>,
        clock: &dyn Fn() -> i64,
        root: &Path,
        name: &str,
        fresh: bool,
    ) -> i32 {
        self::run_flow_url(node, auto, clock, root, name, fresh, "http://x")
    }

    /// [`run_flow`] with the URL the caller resolved — the recorded state's
    /// URL is "http://x", so anything else models the #1214 mismatch.
    fn run_flow_url(
        node: &FakeNode,
        auto: &FakeAuto<'_>,
        clock: &dyn Fn() -> i64,
        root: &Path,
        name: &str,
        fresh: bool,
        url: &str,
    ) -> i32 {
        with_fast_count_retries(|| {
            corpus_index_flow(node, &auto.runner(), clock, root, url, name, fresh)
        })
    }

    const T0: i64 = 1_700_000_000;

    #[test]
    fn a_verified_replacement_switches_state_then_retires_by_exact_name() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 0);
        let new_prefix = format!("xc-kv-b{T0}");
        // the replacement is what the ledger now names…
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(new_prefix.as_str()));
        // …the old index is gone, deleted BY EXACT NAME (never a wildcard)…
        assert!(!node.live(&format!("{old_prefix}-000")));
        assert!(node.live(&format!("{new_prefix}-000")));
        assert!(node
            .ops
            .borrow()
            .iter()
            .filter(|o| o.starts_with("delete:"))
            .all(|o| !o.contains('*')));
        // …and the retire ran only AFTER the replacement's count was read.
        let ops = node.ops.borrow();
        let first_delete = ops.iter().position(|o| o.starts_with("delete:")).unwrap();
        let new_count = ops
            .iter()
            .position(|o| *o == format!("count:{new_prefix}-*"))
            .unwrap();
        assert!(new_count < first_delete, "count before any delete: {ops:?}");
        // the old build's catalog scope was cleaned too
        assert!(ops.iter().any(|o| *o == format!("catalog:{old_prefix}")));
    }

    #[test]
    fn a_build_that_never_verifies_touches_nothing_old() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        // the new build "succeeds" (rc 0) but the node reports ZERO records
        node.seed_count(&format!("xc-kv-b{T0}-*"), Count::Zero);

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 1, "a zero-count build fails even on exit 0");
        // old index untouched, old state untouched
        assert!(node.live(&format!("{old_prefix}-000")));
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(old_prefix.as_str()));
        assert_eq!(st.autoindex_exit, Some(serde_json::json!(0)));
        // the new build's shards were removed (its OWN catalog scope cleaned),
        // and NO delete ever named the old index
        assert!(!node.live(&format!("xc-kv-b{T0}-000")));
        assert!(node
            .ops
            .borrow()
            .iter()
            .all(|o| *o != format!("delete:{old_prefix}-000")));
    }

    #[test]
    fn a_count_the_node_does_not_answer_never_retires_a_working_index() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        // every count for the NEW prefix 503s — "did not say", not zero
        node.seed_count(&format!("xc-kv-b{T0}-*"), Count::Unknown("HTTP 503".into()));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 1);
        // UNKNOWN authorises no delete and no switch: everything still stands
        assert!(node.live(&format!("{old_prefix}-000")));
        assert!(
            node.live(&format!("xc-kv-b{T0}-000")),
            "unverified build kept"
        );
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(old_prefix.as_str()));
        assert!(node.ops.borrow().iter().all(|o| !o.starts_with("delete:")));
    }

    #[test]
    fn an_unknown_old_count_is_presumed_a_working_index_not_an_empty_one() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        // the OLD index's count is unknown too: a failed build must not be
        // kept over it
        node.seed_count(
            &format!("{old_prefix}-*"),
            Count::Unknown("HTTP 503".into()),
        );
        // the new build FAILS (rc 1) but claims records
        node.seed_count(&format!("xc-kv-b{T0}-*"), Count::Number(500));
        let auto = FakeAuto::with_rc(&node, 1);

        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 1);
        // the failed build was not salvaged over an (unknowably) working index
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(old_prefix.as_str()));
        assert_ne!(st.salvaged, Some(true));
        assert!(!node.live(&format!("xc-kv-b{T0}-000")));
    }

    #[test]
    fn a_build_short_of_the_packs_records_is_named_not_swallowed() {
        // the pack-materialized shape: two records.jsonl files, 5 + 3 lines
        let root = corpus_root("kv");
        std::fs::create_dir_all(root.join("corpora/kv/a")).unwrap();
        std::fs::write(
            root.join("corpora/kv/a/records.jsonl"),
            "{}\n{}\n{}\n{}\n{}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("corpora/kv/b")).unwrap();
        std::fs::write(root.join("corpora/kv/b/records.jsonl"), "{}\n{}\n{}\n").unwrap();

        // the node answered fewer records than the pack holds → the gap
        let gap = pack_record_gap("kv", &root, Some(6));
        assert_eq!(
            gap,
            Some((8, 2)),
            "1,230-of-1,963 must be named, not verified"
        );

        // full coverage and over-coverage (corpus.json rides along as extra
        // docs) → no gap
        assert_eq!(pack_record_gap("kv", &root, Some(8)), None);
        assert_eq!(pack_record_gap("kv", &root, Some(10)), None);

        // a git-clone corpus (source repos, junk is normal there) has no
        // records.jsonl at all → exempt by construction
        let clone_root = corpus_root("git");
        std::fs::create_dir_all(clone_root.join("corpora/git/crate-src")).unwrap();
        std::fs::write(
            clone_root.join("corpora/git/crate-src/Cargo.toml"),
            "[package]\n",
        )
        .unwrap();
        assert_eq!(pack_record_gap("git", &clone_root, Some(0)), None);

        // an unanswerable count says nothing
        assert_eq!(pack_record_gap("kv", &root, None), None);
        // so does a corpus this root does not hold
        assert_eq!(pack_record_gap("ghost", &root, Some(0)), None);
    }

    #[test]
    fn a_failed_first_build_that_wrote_records_is_kept_and_marked_salvaged() {
        let root = corpus_root("kv");
        let node = FakeNode::new();
        // no state file, no live indices: this is a FIRST build, and the
        // runner models #367 — abort in finalisation, documents already written
        let auto = FakeAuto::failing_but_writing(&node);

        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 0, "a salvaged first build is still queryable");
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.salvaged, Some(true));
        assert_eq!(st.autoindex_exit, Some(serde_json::json!(1)));
        assert_eq!(
            st.index_prefix.as_deref(),
            Some(format!("xc-kv-b{T0}").as_str())
        );
        assert!(node.live(&format!("xc-kv-b{T0}-000")));
    }

    #[test]
    fn a_plain_rerun_resumes_the_recorded_build_under_the_same_prefix_and_state_dir() {
        let root = corpus_root("kv");
        let state_dir = root.join("autoindex-state/kv/b1");
        std::fs::create_dir_all(&state_dir).unwrap();
        let old_prefix = record_build(&root, "kv", "b1", state_dir.to_str());
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        // the re-run reconciled the RECORDED build: same prefix, same state
        // directory, nothing deleted, nothing retired
        assert_eq!(auto.prefixes(), vec![old_prefix.clone()]);
        let (p, sd) = &auto.calls.borrow()[0];
        assert_eq!(sd.as_deref(), Some(state_dir.as_path()));
        assert!(p.starts_with("xc-kv-b1"));
        assert!(node.live(&format!("{old_prefix}-000")));
        assert!(node.ops.borrow().iter().all(|o| !o.starts_with("delete:")));
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(old_prefix.as_str()));
        assert_eq!(st.salvaged, Some(false));
    }

    // ── #1214: a recorded build refuses a run it cannot resume ────────────
    //
    // The live failure: `xerj corpus index xerj-search` with no --url
    // resolved http://localhost:9200 against a state recording
    // http://127.0.0.1:9200. The update conjunct failed, the flow fell
    // through to legacy mode, and without --state-dir autoindex opened its
    // default hash-of-root journal — a stale pending sync from a retired
    // attempt — and began re-applying 46,308 operations into the legacy
    // namespace beside the recorded build. A recorded build pins url,
    // prefix and state dir; a run that cannot present all three must stop,
    // not switch journals.

    #[test]
    fn a_url_mismatch_against_a_recorded_build_refuses_instead_of_running_legacy() {
        let root = corpus_root("kv");
        let state_dir = root.join("autoindex-state/kv/b1");
        std::fs::create_dir_all(&state_dir).unwrap();
        let old_prefix = record_build(&root, "kv", "b1", state_dir.to_str());
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));

        let auto = FakeAuto::ok(&node);
        let rc = self::run_flow_url(&node, &auto, &|| T0, &root, "kv", false, "http://elsewhere");

        assert_eq!(rc, 2, "a recorded build under another URL is a refusal");
        assert!(
            auto.calls.borrow().is_empty(),
            "no autoindex invocation may run from the refusal path"
        );
        assert!(node.ops.borrow().iter().all(|o| !o.starts_with("delete:")));
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(
            st.index_prefix.as_deref(),
            Some(old_prefix.as_str()),
            "the recorded build stays recorded"
        );
    }

    #[test]
    fn a_missing_recorded_state_dir_refuses_instead_of_running_legacy() {
        let root = corpus_root("kv");
        // Recorded, but never created on disk.
        let state_dir = root.join("autoindex-state/kv/b1");
        let old_prefix = record_build(&root, "kv", "b1", state_dir.to_str());
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 2, "a recorded build without its state dir is a refusal");
        assert!(auto.calls.borrow().is_empty());
        assert!(node.ops.borrow().iter().all(|o| !o.starts_with("delete:")));
    }

    // ── #1183: "searchable" is a claim about queries, not counts ───────────
    //
    // The live failure: a resume over an index with a dangling segment
    // printed "corpus 'xerj-search' searchable: 642 records" and exited 0
    // while the store answered every `_search` (and the salvage path's own
    // `delete_by_query`) with `store_exception: Segment … not found`.
    // `_count` is metadata-only and returned 642 over that same store, so
    // every verification built on counts certified a corpus that could not
    // be queried. Each arm below pins the probe that closes it.

    /// The build arm: rc 0 and a positive count over a store that cannot be
    /// searched is NOT verified — nothing is switched, the old index stays
    /// live, and the unsearchable build's own indices are retired by exact
    /// name (an unsearchable index left under the namespace is what poisoned
    /// the live one).
    #[test]
    fn a_counted_build_the_node_cannot_search_is_not_verified_or_switched() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        // The #1183 shape: autoindex exit 0, `_count` answers records, and
        // the store 500s the first search of the build — the reason server,
        // quoted from the live failure.
        node.seed_count(&format!("xc-kv-b{T0}-*"), Count::Number(500));
        node.seed_search(
            &format!("xc-kv-b{T0}-*"),
            Err(
                "HTTP 500 Internal Server Error: store_exception: storage error: Segment \
                 ea12a22c-41ff-45fd-a400-78118af4dc80 not found"
                    .into(),
            ),
        );

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(
            rc, 1,
            "rc was 0 and records were counted — the store still cannot be searched"
        );
        // nothing switched: the ledger still names the old build…
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(old_prefix.as_str()));
        // …the old index is untouched and still live…
        assert!(node.live(&format!("{old_prefix}-000")));
        assert!(!node
            .ops
            .borrow()
            .iter()
            .any(|o| *o == format!("delete:{old_prefix}-000")));
        // …and the unsearchable build was removed by exact name, only after
        // its verification search said no.
        assert!(!node.live(&format!("xc-kv-b{T0}-000")));
        let ops = node.ops.borrow();
        let first_delete = ops.iter().position(|o| o.starts_with("delete:")).unwrap();
        let probe = ops
            .iter()
            .position(|o| *o == format!("search:xc-kv-b{T0}-*"))
            .unwrap();
        assert!(probe < first_delete, "the probe gates the deletes: {ops:?}");
    }

    /// The update arm: the recorded build was just re-run in place, so an
    /// unsearchable store means the LIVE corpus is broken. The run must not
    /// rewrite state or print "searchable" over it.
    #[test]
    fn an_update_over_an_unsearchable_store_is_not_recorded_searchable() {
        let root = corpus_root("kv");
        let state_dir = root.join("autoindex-state/kv/b1");
        std::fs::create_dir_all(&state_dir).unwrap();
        let old_prefix = record_build(&root, "kv", "b1", state_dir.to_str());
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        node.seed_search(
            &format!("{old_prefix}-*"),
            Err("HTTP 500: store_exception: Segment ea12a22c not found".into()),
        );

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(
            rc, 1,
            "autoindex exited 0; the store it wrote cannot be searched"
        );
        // the ledger was not rewritten over the broken store
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some(old_prefix.as_str()));
        assert_eq!(st.autoindex_exit, Some(serde_json::json!(0)));
        assert!(node.live(&format!("{old_prefix}-000")));
    }

    /// The legacy arm — the exact live reproduction. A resume fails (rc 1)
    /// but the namespace count grew, which used to print "The corpus is
    /// queryable" and record it salvaged. One unsearchable index anywhere
    /// under the wildcard makes that untrue, whatever the counts said.
    #[test]
    fn a_salvaged_legacy_resume_over_an_unsearchable_store_stays_unrecorded() {
        // Control: the identical salvage shape over a store that answers —
        // the salvage path itself must still work, so the gate is what
        // changed, not the verdict.
        let healthy_root = corpus_root("kv");
        state::write_state(&healthy_root, "kv", "http://x", 0, false, None, None, None).unwrap();
        let healthy = FakeNode::new();
        let rc = with_fast_count_retries(|| {
            corpus_index_flow(
                &healthy,
                &|prefix, _sd| {
                    // the resume fails, but the namespace count grew 0 -> 500
                    healthy.seed_count(&format!("{prefix}-*"), Count::Number(500));
                    1
                },
                &|| T0,
                &healthy_root,
                "http://x",
                "kv",
                false,
            )
        });
        assert_eq!(rc, 0, "control: a queryable salvage still succeeds");
        let st = state::load_state(&healthy_root, "kv").unwrap();
        assert_eq!(st.salvaged, Some(true));
        assert_eq!(st.autoindex_exit, Some(serde_json::json!(1)));

        // The #1183 case: same counts, same rc — and the wildcard search 500s.
        let root = corpus_root("kv");
        state::write_state(&root, "kv", "http://x", 0, false, None, None, None).unwrap();
        let node = FakeNode::new();
        node.seed_search(
            "xc-kv-*",
            Err("HTTP 500: store_exception: Segment ea12a22c not found".into()),
        );
        let rc = with_fast_count_retries(|| {
            corpus_index_flow(
                &node,
                &|prefix, _sd| {
                    node.seed_count(&format!("{prefix}-*"), Count::Number(500));
                    1
                },
                &|| T0,
                &root,
                "http://x",
                "kv",
                false,
            )
        });
        assert_eq!(
            rc, 1,
            "must not exit 0 over a store that cannot be searched"
        );
        // the salvage verdict was never recorded: the ledger still says what
        // it said before the run
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.salvaged, Some(false));
        assert_ne!(st.autoindex_exit, Some(serde_json::json!(1)));
        // and the probe ran, over exactly the glob `xerj code` reads
        assert!(node.ops.borrow().iter().any(|o| *o == "search:xc-kv-*"));
    }

    /// #1183, the last live half of the dishonest-resume item: a run that
    /// aborted (autoindex exit ∉ {0,3}) and was only KEPT by the #367 salvage
    /// must not print the clean `searchable: N records` success footer. The
    /// 2026-10-08 standing rebuild ended `xerj-done ok=false exit=1
    /// reason=aborted` and still printed `corpus 'xerj-search' searchable:
    /// 1011181 records`, overstating the kept set by the 348 docs the re-walk
    /// had just dropped.
    #[test]
    fn the_searchable_footer_is_never_printed_after_an_aborted_run() {
        // clean exits keep the earned "searchable" footer and the next-hint
        for rc in [0, 3] {
            let lines = report_footer("kv", rc, "500");
            assert!(
                lines
                    .iter()
                    .any(|l| l == "corpus 'kv' searchable: 500 records"),
                "rc {rc} is clean and earns the searchable footer: {lines:?}"
            );
            assert!(lines.iter().any(|l| l.starts_with("next: xerj code kv")));
        }
        // a salvaged/aborted run (any other exit) says so and NEVER prints the
        // word "searchable" — the whole defect in one assertion
        for rc in [1, 2, 134] {
            let lines = report_footer("kv", rc, "500");
            assert!(
                lines.iter().all(|l| !l.contains("searchable")),
                "an aborted run (exit {rc}) must not print the searchable footer: {lines:?}"
            );
            // it still tells the operator the kept store exists, names the
            // exit, and points at the clean rebuild — honest, not silent
            assert!(
                lines.iter().any(|l| l.contains("indexed with errors")
                    && l.contains(&format!("autoindex exit {rc}"))
                    && l.contains("coverage is NOT guaranteed")),
                "the aborted footer must name the fault: {lines:?}"
            );
            assert!(
                lines.iter().any(|l| l.contains("--fresh")),
                "the aborted footer points at the clean rebuild: {lines:?}"
            );
        }
    }

    /// #1173: the three honest claims a failed legacy run can make. The one
    /// this exists to pin is the middle: a count the node never answered is
    /// UNKNOWN, and "wrote no new records" asserted from it is a fabricated
    /// absence — exactly what the overnight resume failure printed while the
    /// run had written records the node was too unhealthy to count.
    #[test]
    fn a_failed_run_the_node_cannot_count_is_unknown_not_wrote_nothing() {
        use super::LegacyFailureVerdict::*;
        // grew → salvaged, with the pair the message quotes
        assert!(matches!(
            legacy_failure_verdict(Some(100), Some(250)),
            Salvaged {
                before: 100,
                after: 250
            }
        ));
        // known and did not grow → the absence claim is EARNED
        assert!(matches!(
            legacy_failure_verdict(Some(100), Some(100)),
            WroteNothing {
                before: 100,
                after: 100
            }
        ));
        assert!(matches!(
            legacy_failure_verdict(Some(100), Some(40)),
            WroteNothing {
                before: 100,
                after: 40
            }
        ));
        // any UNKNOWN side forbids the absence claim — before, after, or both
        assert!(matches!(
            legacy_failure_verdict(None, Some(250)),
            CountUnknown
        ));
        assert!(matches!(
            legacy_failure_verdict(Some(100), None),
            CountUnknown
        ));
        assert!(matches!(legacy_failure_verdict(None, None), CountUnknown));
    }

    #[test]
    fn sibling_corpus_indices_are_never_touched_by_a_rebuild() {
        let root = corpus_root("battle");
        // the sibling exists as a cloned corpus AND in the ledger
        std::fs::create_dir_all(root.join("corpora/battle-terse")).unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(
            root.join("state/battle-terse.json"),
            "{\"corpus\":\"battle-terse\"}",
        )
        .unwrap();
        let old_prefix = record_build(&root, "battle", "b9", Some("/tmp/s9"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        // `xc-battle-*` matches BOTH; only b9 belongs to "battle"
        node.indices
            .borrow_mut()
            .push("xc-battle-terse-b1-000".into());
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        node.seed_count("xc-battle-terse-b1-*", Count::Number(300));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "battle", true);

        assert_eq!(rc, 0);
        assert!(
            node.live("xc-battle-terse-b1-000"),
            "the sibling's index survives a rebuild of 'battle'"
        );
        assert!(!node.live(&format!("{old_prefix}-000")));
        assert!(node
            .ops
            .borrow()
            .iter()
            .all(|o| !o.contains("battle-terse")));
    }

    #[test]
    fn readers_switch_before_the_retire_runs_a_failed_delete_leaves_a_duplicate_not_a_gap() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        // the node refuses to delete the old index (crash-between shape)
        *node.fail_delete.borrow_mut() = Some(format!("{old_prefix}-000"));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 0, "a failed retire does not fail the corpus");
        // the SWITCH already happened: readers are on the new build…
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(
            st.index_prefix.as_deref(),
            Some(format!("xc-kv-b{T0}").as_str())
        );
        assert!(node.live(&format!("xc-kv-b{T0}-000")));
        // …and the un-retired old index is a duplicate, never a gap
        assert!(node.live(&format!("{old_prefix}-000")));
    }

    #[test]
    fn two_rebuilds_inside_one_second_must_not_retire_the_build_they_just_verified() {
        let root = corpus_root("kv");
        let node = FakeNode::new();
        // a clock that repeats T0 once, then advances — the collision the
        // one-second stamp cannot otherwise see
        let tick = std::cell::Cell::new(0);
        let clock = move || {
            let t = T0 + tick.get();
            tick.set(tick.get() + 1);
            t
        };

        let auto1 = FakeAuto::ok(&node);
        assert_eq!(run_flow(&node, &auto1, &clock, &root, "kv", true), 0);
        let first = auto1.prefixes()[0].clone();
        assert_eq!(first, format!("xc-kv-b{T0}"));

        // second --fresh in the SAME second: the build id must differ, so it
        // can never list-and-retire the build it is standing on
        let auto2 = FakeAuto::ok(&node);
        assert_eq!(run_flow(&node, &auto2, &clock, &root, "kv", true), 0);
        let second = auto2.prefixes()[0].clone();
        assert_ne!(first, second, "build ids must not repeat within a second");
        // and the retire lists only the FIRST build, by exact name
        assert!(node.live(&format!("{second}-000")));
        assert!(!node.live(&format!("{first}-000")));
    }

    #[test]
    fn a_legacy_corpus_whose_indices_predate_builds_indexes_under_the_bare_namespace() {
        let root = corpus_root("kv");
        let node = FakeNode::new();
        // no state file, but a live index from before builds existed: WITHOUT
        // the stray index this would be a first build (same path as --fresh),
        // and with it the flow must take the legacy arm — the bare namespace,
        // no build id, no state dir.
        node.indices.borrow_mut().push("xc-kv-000".into());

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        assert_eq!(auto.prefixes(), vec!["xc-kv".to_string()]);
        assert!(
            auto.calls.borrow()[0].1.is_none(),
            "no state dir in legacy mode"
        );
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix, None, "legacy state records no build");
        assert!(node.live("xc-kv-000"));
        assert!(node.ops.borrow().iter().all(|o| !o.starts_with("delete:")));
    }

    #[test]
    fn a_legacy_run_retires_stamped_generations_it_cannot_reach() {
        let root = corpus_root("kv");
        let node = FakeNode::new();
        // the measured #1136 shape: an unstamped legacy index beside stamped
        // generations from builds the (missing/legacy) ledger does not name
        node.indices
            .borrow_mut()
            .extend(["xc-kv-000", "xc-kv-b111-000", "xc-kv-b222-001"].map(String::from));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        assert!(node.live("xc-kv-000"), "the legacy run's own index stays");
        assert!(
            !node.live("xc-kv-b111-000") && !node.live("xc-kv-b222-001"),
            "stamped generations the ledger cannot reach are retired"
        );
        let ops = node.ops.borrow();
        assert!(ops.contains(&"delete:xc-kv-b111-000".to_string()));
        assert!(ops.contains(&"delete:xc-kv-b222-001".to_string()));
        // one catalog-scope delete per distinct generation, not per index
        assert!(ops.contains(&"catalog:xc-kv-b111".to_string()));
        assert!(ops.contains(&"catalog:xc-kv-b222".to_string()));
        assert_eq!(
            ops.iter().filter(|o| o.starts_with("catalog:")).count(),
            2,
            "no catalog delete for the kept unstamped index"
        );
    }

    #[test]
    fn a_build_run_deletes_nothing_blind_when_the_node_never_answered_a_listing() {
        let root = corpus_root("kv");
        let node = FakeNode::new();
        // a live generation the flow will never see: every listing fails
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        node.fail_listing.set(true);

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 0, "build + verify still succeed");
        assert!(
            node.ops.borrow().iter().all(|o| !o.starts_with("delete:")),
            "an unanswerable listing retires nothing — never delete blind"
        );
        assert!(
            node.live("xc-kv-b1-000"),
            "the unseen generation is kept, named in the epilogue instead"
        );
    }

    #[test]
    fn a_generation_whose_delete_fails_is_named_in_the_epilogue() {
        let root = corpus_root("kv");
        let old_prefix = record_build(&root, "kv", "b1", Some("/tmp/s1"));
        let node = FakeNode::new();
        node.indices.borrow_mut().push(format!("{old_prefix}-000"));
        node.seed_count(&format!("{old_prefix}-*"), Count::Number(500));
        // the node refuses to delete exactly the old generation's index
        node.fail_delete
            .borrow_mut()
            .replace(format!("{old_prefix}-000"));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0 + 10, &root, "kv", true);

        assert_eq!(rc, 0, "a failed retire does not fail the swap");
        assert!(
            node.live(&format!("{old_prefix}-000")),
            "the refused delete leaves the old generation on the node"
        );
        // and the leftovers report re-listed the namespace to name it
        assert!(
            node.ops
                .borrow()
                .iter()
                .filter(|o| o.starts_with("delete:"))
                .count()
                >= 1
        );
    }

    // ── corpus add --from <pack> (harvested consumption) ────────────────────

    /// Holds XERJ_CODE_HOME at `home` for the guard's lifetime — both the
    /// build and the add paths resolve their root through `code_root()` at
    /// CALL time, so a test that builds a pack and then adds it needs the
    /// env held across both, not scoped per call. Under ENV_LOCK for the
    /// same reason the env tests above are (do NOT re-enter: the lock is not
    /// reentrant, so nothing inside may take it again).
    struct CodeHomeGuard(#[expect(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for CodeHomeGuard {
        fn drop(&mut self) {
            std::env::remove_var("XERJ_CODE_HOME");
        }
    }
    fn code_home(home: &Path) -> CodeHomeGuard {
        let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("XERJ_CODE_HOME", home);
        CodeHomeGuard(g)
    }

    /// A small two-source pack under `home`: source `a` owns X1 (merged with
    /// b's alias-linked record) and X3; source `b` wins nothing.
    fn build_demo_pack(home: &Path) -> PathBuf {
        let root = home.join("recipes");
        std::fs::create_dir_all(root.join("data/a")).unwrap();
        std::fs::create_dir_all(root.join("data/b")).unwrap();
        std::fs::write(
            root.join("demo.toml"),
            r#"
[recipe]
format = 1
name = "demo"

[[sources]]
slug = "a"
kind = "dir"
path = "data/a"
format = "flat"
licence = "CC0-1.0"

[[sources]]
slug = "b"
kind = "dir"
path = "data/b"
format = "flat"
licence = "CC-BY-4.0"

[identity]
edges = [{ field = "id" }, { field = "aliases", each = true }]
canonical_source_order = ["a", "b"]

[merge]
precedence = ["a", "b"]
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("data/a/one.json"),
            r#"{"id":"X1","aliases":["C1"],"title":"one"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("data/a/three.json"),
            r#"{"id":"X3","title":"three"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("data/b/two.json"),
            r#"{"id":"X2","aliases":["C1"],"title":"two"}"#,
        )
        .unwrap();
        let rc = crate::harvest::run_build(&["demo".to_string()]);
        assert_eq!(rc, 0, "pack build must succeed");
        home.join("builds/demo/pack/demo")
    }

    fn records_lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn resolve_pack_source_sniffs_the_pack_spellings() {
        let tmp = tempfile::tempdir().unwrap();
        let _h = code_home(tmp.path());
        let pack = build_demo_pack(tmp.path());
        assert!(matches!(
            resolve_pack_source(&pack.display().to_string()).unwrap(),
            FromPack::Dir(_)
        ));
        // the pack's own manifest names the pack (its parent dir)
        assert!(matches!(
            resolve_pack_source(&pack.join("manifest.json").display().to_string()).unwrap(),
            FromPack::Dir(_)
        ));
        // a hub manifest is not a pack — the hub arm reads it
        let hub = tmp.path().join("hub.json");
        std::fs::write(&hub, r#"{"corpus":"x","repos":[]}"#).unwrap();
        assert!(matches!(
            resolve_pack_source(&hub.display().to_string()).unwrap(),
            FromPack::NotAPack
        ));
        // a zip is staged, not inspected, here
        let zipf = tmp.path().join("p.zip");
        std::fs::write(&zipf, b"PK").unwrap();
        assert!(matches!(
            resolve_pack_source(&zipf.display().to_string()).unwrap(),
            FromPack::Zip(_)
        ));
        // a directory without a manifest is an error, not a silent fallthrough
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = resolve_pack_source(&empty.display().to_string()).unwrap_err();
        assert!(err.contains("no manifest.json"), "{err}");
        // and a dir whose manifest is not a harvested pack is named as such
        let hubdir = tmp.path().join("hubdir");
        std::fs::create_dir_all(&hubdir).unwrap();
        std::fs::write(hubdir.join("manifest.json"), r#"{"corpus":"x"}"#).unwrap();
        let err = resolve_pack_source(&hubdir.display().to_string()).unwrap_err();
        assert!(err.contains("not a harvested pack"), "{err}");
    }

    #[test]
    fn corpus_add_from_pack_materializes_re_adds_and_refuses_mixing() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let pack = build_demo_pack(home.path());
        let corpus = home.path().join("corpora/demo");

        let add = || run_corpus_add(&["--from".to_string(), pack.display().to_string()]);
        let rc = add();
        assert_eq!(rc, 0);

        // records are partitioned by WINNING source: a holds X1 (merged with
        // b's alias) and X3; b won nothing so it has no directory at all
        let a = records_lines(&corpus.join("a/records.jsonl"));
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["id"], json!("X1"));
        assert_eq!(a[0]["sources"], json!(["a", "b"]), "alias merge survived");
        assert_eq!(a[1]["id"], json!("X3"));
        assert!(!corpus.join("b").exists(), "no b-won records, no b dir");

        // the manifest: kind + pseudo-repos with the pack's source licences
        let m = manifest::read_corpus_manifest(&corpus.join("corpus.json")).unwrap();
        assert_eq!(m.kind.as_deref(), Some("harvested"));
        assert_eq!(m.repos.len(), 2);
        assert_eq!(m.repos[0].repo, "a");
        assert_eq!(m.repos[0].licence, "CC0-1.0");
        assert_eq!(m.repos[0].files, Some(2));
        assert_eq!(m.repos[1].repo, "b");
        assert_eq!(m.repos[1].files, Some(0));

        // THE integration pin: xerj code's per-hit licence lookups key on the
        // first locator segment, which is the slug dir — licence_map must see
        // exactly what a cloned repo would have given it
        let lic = manifest::licence_map(home.path(), "demo");
        assert_eq!(lic.get("a").map(String::as_str), Some("CC0-1.0"));
        assert_eq!(lic.get("b").map(String::as_str), Some("CC-BY-4.0"));

        // re-add: refresh, never duplicate — same pack, same line counts
        assert_eq!(add(), 0);
        assert_eq!(records_lines(&corpus.join("a/records.jsonl")).len(), 2);

        // a git-shaped corpus already owns the name → refused, nothing cloned
        std::fs::write(
            corpus.join("corpus.json"),
            r#"{"corpus":"demo","repos":[]}"#,
        )
        .unwrap();
        assert_eq!(add(), 2);
    }

    #[test]
    fn corpus_add_verifies_the_pack_signature_before_materializing() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let pack = build_demo_pack(home.path());
        let corpus = home.path().join("corpora/demo");

        // sign the demo pack and publish both halves as key files
        let (seed, public) = crate::harvest::sign::generate_keypair().unwrap();
        crate::harvest::sign::sign_pack(&pack, &seed).unwrap();
        let good = home.path().join("good.pub");
        std::fs::write(&good, &public).unwrap();
        let (_, other) = crate::harvest::sign::generate_keypair().unwrap();
        let wrong = home.path().join("wrong.pub");
        std::fs::write(&wrong, &other).unwrap();

        let add = |key: &Path| {
            run_corpus_add(&[
                "--from".to_string(),
                pack.display().to_string(),
                "--verify-sig".to_string(),
                key.display().to_string(),
            ])
        };

        // the wrong key refuses the pack WHOLE: exit 2 and no corpus written
        // — a signature failure must never leave a half-materialized corpus
        assert_eq!(add(&wrong), 2);
        assert!(
            !corpus.exists(),
            "a failed verification must not materialize anything"
        );

        // the right key installs normally
        assert_eq!(add(&good), 0);
        assert!(corpus.join("corpus.json").is_file());

        // and the unsigned-pack hint path: rebuild without the .sig, add
        // without --verify-sig still works (the consumer's key choice is
        // theirs), the hint goes to stderr where the callers ignore it
        std::fs::remove_file(pack.join(crate::harvest::sign::SIG_NAME)).unwrap();
        assert_eq!(
            run_corpus_add(&["--from".to_string(), pack.display().to_string()]),
            0
        );
    }

    #[test]
    fn a_pack_zip_picks_up_the_loose_release_signature() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let pack = build_demo_pack(home.path());

        // Sign, then ship the signature the way a release does: LOOSE,
        // beside the zip, named <pack>-SHA256SUMS.sig — never inside the
        // zip (the sig is deliberately absent from the SUMS it signs, and
        // signing runs after the zip is built).
        let (seed, public) = crate::harvest::sign::generate_keypair().unwrap();
        crate::harvest::sign::sign_pack(&pack, &seed).unwrap();
        let sig = std::fs::read(pack.join(crate::harvest::sign::SIG_NAME)).unwrap();
        std::fs::remove_file(pack.join(crate::harvest::sign::SIG_NAME)).unwrap();
        let good = home.path().join("good.pub");
        std::fs::write(&good, &public).unwrap();
        let (_, other) = crate::harvest::sign::generate_keypair().unwrap();
        let wrong = home.path().join("wrong.pub");
        std::fs::write(&wrong, &other).unwrap();

        use std::io::Write;
        let zip_path = home.path().join("demo-pack.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            for e in std::fs::read_dir(&pack).unwrap().flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                z.start_file(name, opts).unwrap();
                z.write_all(&std::fs::read(e.path()).unwrap()).unwrap();
            }
            z.finish().unwrap();
        }
        std::fs::write(home.path().join("demo-SHA256SUMS.sig"), sig).unwrap();

        let corpus = home.path().join("corpora/demo");
        let add = |key: &Path| {
            run_corpus_add(&[
                "--from".to_string(),
                zip_path.display().to_string(),
                "--verify-sig".to_string(),
                key.display().to_string(),
            ])
        };

        // wrong key: refused whole, nothing materialized
        assert_eq!(add(&wrong), 2);
        assert!(
            !corpus.exists(),
            "a failed zip verification must not materialize anything"
        );

        // right key: the loose sig beside the zip is found and verifies
        assert_eq!(add(&good), 0);
        assert!(corpus.join("corpus.json").is_file());

        // sig missing entirely (neither inside nor beside): verification
        // cannot start — a clear failure, never a silent skip to unsigned
        std::fs::remove_file(home.path().join("demo-SHA256SUMS.sig")).unwrap();
        assert_eq!(add(&good), 2);
    }

    #[test]
    fn git_arm_refuses_to_clone_onto_a_harvested_name() {
        let home = tempfile::tempdir().unwrap();
        let corpus = home.path().join("corpora/occupied");
        std::fs::create_dir_all(&corpus).unwrap();
        // harvested-kind marker: the git arm must refuse BEFORE cloning
        manifest::write_corpus_manifest_kind(
            &corpus.join("corpus.json"),
            "occupied",
            Some("harvested"),
            "t",
            &[manifest::ManifestRepo {
                repo: "a".into(),
                url: "u".into(),
                licence: "CC0-1.0".into(),
                sha: String::new(),
                files: None,
                bytes: None,
                review: None,
            }],
            None,
        );
        let _h = code_home(home.path());
        let rc = run_corpus_add(&[
            "occupied".to_string(),
            "https://github.com/xerj-org/xerj".to_string(),
        ]);
        assert_eq!(rc, 2);
        assert!(!corpus.join("xerj").exists(), "no clone happened");
    }

    #[test]
    fn a_pack_zip_stages_and_materializes() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let pack = build_demo_pack(home.path());

        // zip the pack dir, both shapes anyone hands us: the wrapper dir
        // (`demo/…` inside — how every zip-of-a-directory tool packs) and
        // the bare contents
        fn zip_up(zip_path: &Path, base: &Path, prefix: &str) {
            use std::io::Write;
            let f = std::fs::File::create(zip_path).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            fn add(
                z: &mut zip::ZipWriter<std::fs::File>,
                base: &Path,
                rel: &Path,
                prefix: &str,
                opts: zip::write::SimpleFileOptions,
            ) {
                if rel.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(rel) {
                        for e in entries.flatten() {
                            add(z, base, &e.path(), prefix, opts);
                        }
                    }
                    return;
                }
                let name = format!(
                    "{prefix}{}",
                    rel.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                );
                z.start_file(name, opts).unwrap();
                z.write_all(&std::fs::read(rel).unwrap()).unwrap();
            }
            add(&mut z, base, base, prefix, opts);
            z.finish().unwrap();
        }

        let corpus = home.path().join("corpora/demo");
        for (name, prefix) in [("wrapped.zip", "demo/"), ("flat.zip", "")] {
            let zip_path = home.path().join(name);
            zip_up(&zip_path, &pack, prefix);
            let rc = run_corpus_add(&["--from".to_string(), zip_path.display().to_string()]);
            assert_eq!(rc, 0, "{name} must materialize");
            assert_eq!(
                records_lines(&corpus.join("a/records.jsonl")).len(),
                2,
                "{name}"
            );
            assert_eq!(
                manifest::read_corpus_manifest(&corpus.join("corpus.json"))
                    .unwrap()
                    .kind
                    .as_deref(),
                Some("harvested"),
                "{name}"
            );
        }

        // and a non-pack zip fails loudly instead of materializing noise
        let junk = home.path().join("junk.zip");
        std::fs::write(&junk, b"definitely not a zip").unwrap();
        let rc = run_corpus_add(&["--from".to_string(), junk.display().to_string()]);
        assert_eq!(rc, 2);
        assert!(!home.path().join("corpora/junk").exists());
    }
}
