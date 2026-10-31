//! `xerj code` and `xerj corpus` — the reference-coding loop ported into the
//! binary (issue #977; previously `tools/xerj-code/scripts/xc*.py|sh`).
//!
//! Everything semantic lives in [`xerj_common::xccode`]; this file is the CLI
//! shell: argv parsing in the house style (no clap), the [`XcHttp`]
//! implementation over this crate's blocking [`Es`] client, the git/clone
//! lifecycle (`xerj corpus add`), and the #930 build-verify-swap
//! (`xerj corpus index`). Exit codes: 0 hits / 1 no-match-or-incomplete /
//! 2 usage-transport-stale / 3 corpus-in-state-but-not-loaded-here.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use xerj_common::localauth::{
    discover_local_admin_key_for_url, discover_local_admin_key_for_url_with_origin, url_is_loopback,
};
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
                        discover_local_admin_key_for_url_with_origin(url).map(|found| {
                            eprintln!(
                                "xerj code: no --api-key/XERJ_API_KEY given; using the admin \
                                 key at {} ({})",
                                found.path.display(),
                                found.provenance()
                            );
                            if let Some(warning) =
                                found.foreign_warning("--api-key or XERJ_API_KEY")
                            {
                                eprintln!("xerj code: warning: {warning}");
                            }
                            found.key
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

// ── empty ledger, but the node already has autoindex prefixes (#2181) ──────
//
// `xerj code` / `xerj corpus` read `~/.xerj-code`. `xerj autoindex --prefix`
// writes the node's catalog and does not touch that directory, so an empty
// ledger used to read as "nothing is indexed" on a node that already answers
// `xerj search --prefix`. The hint names those prefixes. It is one short
// request: [`Es::search`] retries a dead node for ~8s of backoff, and this
// path must stay quiet instead.

/// How long the hint waits for a connect. A refused port returns at once;
/// a blackhole must not hold `xerj corpus list`.
const CATALOG_HINT_CONNECT: Duration = Duration::from_millis(800);
/// Whole-request budget, connect included. One attempt, no retry.
const CATALOG_HINT_TIMEOUT: Duration = Duration::from_secs(2);
/// Catalog replies larger than this are not a prefix list. Ignore them.
const CATALOG_HINT_BODY_CAP: usize = 1024 * 1024;
/// How many prefix names the hint prints before it says "N more".
const CATALOG_HINT_PREFIX_CAP: usize = 8;

/// `{root}/state` has no `*.json` ledger. A missing directory is empty.
fn ledger_is_empty(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join("state")) else {
        return true;
    };
    !entries
        .flatten()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
}

/// A prefix safe to paste into the hint's command. Anything else (spaces,
/// quotes, a newline) is dropped so a catalog value cannot forge a line.
fn hintable_prefix(prefix: &str) -> bool {
    (1..=128).contains(&prefix.len())
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The lines to print. `None` when nothing safe remains.
fn format_autoindex_prefix_hint(prefixes: &[String]) -> Option<String> {
    let mut names: Vec<&str> = prefixes
        .iter()
        .map(String::as_str)
        .filter(|prefix| hintable_prefix(prefix))
        .collect();
    names.sort_unstable();
    names.dedup();
    let example = *names.first()?;
    let shown = if names.len() <= CATALOG_HINT_PREFIX_CAP {
        names.join(", ")
    } else {
        format!(
            "{}, … ({} more)",
            names[..CATALOG_HINT_PREFIX_CAP].join(", "),
            names.len() - CATALOG_HINT_PREFIX_CAP
        )
    };
    let each = if names.len() > 1 {
        " (same --prefix for each)"
    } else {
        ""
    };
    Some(format!(
        "this node's autoindex catalog has prefixes: {shown}\n\
         search them with `xerj search --prefix {example}` or `xerj def --prefix {example}`{each}; \
         register a folder for `xerj code` with `xerj corpus add <name> <path>`"
    ))
}

/// Explicit key, else `XERJ_API_KEY`, else the loopback admin key. Silent:
/// the hint must not announce a key and then say nothing when the node is down.
fn resolve_hint_key(url: &str, explicit: Option<&str>) -> Option<String> {
    if let Some(key) = explicit.filter(|key| !key.is_empty()) {
        return Some(key.to_string());
    }
    if let Some(key) = std::env::var("XERJ_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
    {
        return Some(key);
    }
    if url_is_loopback(url) {
        discover_local_admin_key_for_url(url).map(|(key, _path)| key)
    } else {
        None
    }
}

/// Distinct `prefix` values on catalog run documents.
///
/// Same query as `catalog_run_prefixes` in `search.rs`: `doc_kind: run`,
/// `_source: prefix`, size 200. `None` when `/hits/hits` is absent — that is
/// not an empty catalog.
fn prefixes_from_catalog_hits(v: &Value) -> Option<Vec<String>> {
    let hits = v.pointer("/hits/hits")?.as_array()?;
    let mut prefixes = BTreeSet::new();
    for hit in hits {
        if let Some(prefix) = hit.pointer("/_source/prefix").and_then(Value::as_str) {
            if !prefix.is_empty() {
                prefixes.insert(prefix.to_string());
            }
        }
    }
    Some(prefixes.into_iter().collect())
}

/// One catalog read. `None` on any failure, including a node that is not
/// there: callers print nothing in that case.
fn fetch_catalog_run_prefixes(url: &str, explicit_key: Option<&str>) -> Option<Vec<String>> {
    let http = crate::esclient::node_blocking_builder(url)
        .connect_timeout(CATALOG_HINT_CONNECT)
        .timeout(CATALOG_HINT_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .build()
        .ok()?;
    let body = json!({
        "size": 200,
        "query": { "term": { "doc_kind": "run" } },
        "_source": ["prefix"]
    });
    let endpoint = format!(
        "{}/{}/_search",
        url.trim_end_matches('/'),
        xerj_common::AUTOINDEX_CATALOG_INDEX
    );
    let mut req = http.post(endpoint).json(&body);
    if let Some(key) = resolve_hint_key(url, explicit_key) {
        req = req.header("Authorization", format!("ApiKey {key}"));
    }
    let resp = req.send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().ok()?;
    if bytes.len() > CATALOG_HINT_BODY_CAP {
        return None;
    }
    let parsed: Value = serde_json::from_slice(&bytes).ok()?;
    prefixes_from_catalog_hits(&parsed)
}

/// Hint text, or `None` when the node did not answer or has no safe prefix.
fn catalog_prefix_hint(url: &str, explicit_key: Option<&str>) -> Option<String> {
    format_autoindex_prefix_hint(&fetch_catalog_run_prefixes(url, explicit_key)?)
}

/// After a not-indexed refusal, when the ledger itself is empty. A stale
/// index, a bad state file, or a transport error already names its own next
/// step, and those ledgers are not empty.
fn append_autoindex_prefix_hint(
    out: &mut CodeOutcome,
    root: &Path,
    url: &str,
    api_key: Option<&str>,
) {
    if out.exit != 2 || !out.to_stderr || !ledger_is_empty(root) {
        return;
    }
    let Some(hint) = catalog_prefix_hint(url, api_key) else {
        return;
    };
    if !out.text.is_empty() && !out.text.ends_with('\n') {
        out.text.push('\n');
    }
    out.text.push_str(&hint);
}

fn push_prefix_hint(stderr: &mut String, url: &str) {
    if let Some(hint) = catalog_prefix_hint(url, None) {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&hint);
        stderr.push('\n');
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
                  Default ranking is lexical: it matches the words you typed,
                  and it is not a neural reading of the code. --mode semantic
                  and --mode hybrid embed with lexical feature hashing unless
                  the node was started with --embed-mode neural.
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
    match code_cli_outcome(args) {
        CodeCliDone::Printed(code) => code,
        CodeCliDone::Outcome(out) => {
            let code = out.exit;
            emit(&out);
            code
        }
    }
}

enum CodeCliDone {
    /// Usage and client-build failures already printed their own lines.
    Printed(i32),
    Outcome(CodeOutcome),
}

fn code_cli_outcome(args: &[String]) -> CodeCliDone {
    let mut corpus: Option<String> = None;
    let mut query: Option<String> = None;
    let mut url: Option<String> = None;
    let mut api_key: Option<String> = None;
    let mut p = CodeParams::new("", "");
    let mut i = 0;
    let bad = |msg: String| -> CodeCliDone {
        eprintln!("{msg}\n");
        eprintln!("{CODE_USAGE}");
        CodeCliDone::Printed(2)
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
                return CodeCliDone::Printed(0);
            }
            "-k" => {
                let v = val!("-k");
                match v.parse::<usize>() {
                    Ok(n) if (1..=50).contains(&n) => p.k = n,
                    _ => return bad(format!("xerj code: -k must be 1..=50, got '{v}'")),
                }
            }
            "--lang" => p.lang = Some(crate::extract::code::lang_filter_value(&val!("--lang"))),
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
    let es = match build_es(&url, api_key.clone()) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj code: cannot reach {url}: {e:#}");
            return CodeCliDone::Printed(2);
        }
    };
    p.corpus = corpus;
    p.query = query;
    let root = code_root();
    let mut out = xccode::run_code_query(&root, &EsXc(&es), &url, &p, "`--stale-ok`");
    append_autoindex_prefix_hint(&mut out, &root, &url, api_key.as_deref());
    CodeCliDone::Outcome(out)
}

// ── xerj corpus add ─────────────────────────────────────────────────────────

const CORPUS_USAGE: &str =
    "usage: xerj corpus add [--own] <name> <git-url>... | --from <manifest.json|pack-dir|pack.zip> [--as <name>] [--verify-sig <pubkey-file>]
       xerj corpus own <name> [--off]
       xerj corpus build <name> [--recipe <path>] [--fresh]
       xerj corpus sign <pack-dir> --key <seed-file>
       xerj corpus keygen --out <prefix>
       xerj corpus index <name> [--fresh] [--url URL] [--include-data]
         (can outlive an agent tool timeout; stopping is safe, re-run to resume)
       xerj corpus list [--url URL]

a flag may precede the verb (xerj corpus --url URL index <name>);
clone the repos, detect licences, write corpora/<name>/corpus.json;
a repo entry may set 'paths' to relative directories or files (for example
data/osv) and the checkout harvests only those; omit it to harvest the whole pin, and
the list is written back into corpus.json;
a local checkout's root .xerjignore is copied into the clone when it differs
from the committed file (git clone drops an untracked or uncommitted one);
a pinned --from rebuild stays exact and does not copy it, and says so;
if cloning a local partial clone fails, its promisor configuration is
reported as a possible source of missing-object errors. Hydrated partial
clones remain accepted. Index only already-present working-tree files with
'xerj autoindex <path>', or use a complete clone as the corpus source;
--from takes the corpus name from the manifest's 'corpus' field (or the
pack's 'pack' field) unless <name> or --as overrides it; a harvested pack
also checksum-verifies and materializes records per source; --verify-sig
checks the pack's SHA256SUMS.sig against a public key file first and
refuses the pack on failure (for a pack.zip, the sig cannot travel inside
— releases ship it loose beside the zip as <pack>-SHA256SUMS.sig, which
is where the lookup falls back to); build harvests recipe sources into a
deterministic pack under builds/<name>/ (see tools/xerj-code/); sign/keygen
are the publish step (ed25519 over SHA256SUMS);
index builds/verifies/switches the corpus (exit 3 skips junk — normal); it
skips CSV/HDF/simulation output by default (reference corpora are code and
docs, and a test-output folder can out-record the whole corpus) —
--include-data indexes those files too, reporting how many were left out;
at the end it prints a short per-file-class summary of what was indexed
and what data files were left out;
--own marks THIS corpus as your own first-party code and writes \"own\":true
in its corpus.json. An unidentified licence (UNKNOWN or NONE-FOUND) stays
recorded and is still printed on each hit, but xerj code and xerj_code_search
omit the do-not-copy banner for that unidentified licence. A licence the
detector actually named (GPL, AGPL, MPL, …) still warns. A local path is
not marked own unless you pass the flag, and a --from manifest cannot set
it — that file is untrusted. Re-adding the corpus keeps a marker already
recorded. `xerj corpus own <name>` sets it on an existing corpus; `--off`
clears it. Hub pack licence classes are unchanged;
a corpus does not follow its git remotes. To refresh one added from
git URLs, run `git -C <checkout> pull --ff-only` in each checkout under
corpora/<name>/, re-run the same `xerj corpus add <name> <git-url>...` so
corpus.json records the new commit, then `xerj corpus index <name>`, which
updates the build in place. A --from corpus stays at its manifest's pins;
change the manifest to move it;
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

/// A git that can outlive the `xerj` that started it — `clone`, `fetch`,
/// `checkout` — so it inherits the corpus lock (#2520). Short commands do not:
/// a `rev-parse` child that outlives its parent by a microsecond would hold
/// the lock past its parent and spuriously refuse the next run.
fn git_long(dir: Option<&Path>, args: &[&str]) -> Result<(i32, String)> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.current_dir(d).arg("-C").arg(d);
    }
    inherit_corpus_lock(&mut c);
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

/// `.git` alone does not establish a completed checkout (#2453). Check the
/// readable commit, the Git-owned index location (also for linked worktrees),
/// and a pending index lock before reusing an existing checkout.
fn existing_checkout_is_complete(target: &Path) -> bool {
    target.join(".git").exists() && unfinished_clone(target).is_none()
}

/// How much of git's own stdout/stderr a fetch failure keeps. A TLS
/// intercept or a rejected pin is a paragraph; the rest of the bail stays
/// one line an agent can read.
const GIT_FETCH_OUTPUT_CAP: usize = 500;

/// `git fetch <sha>` failed. Keep git's text (capped) and, when that text
/// names the failure, say what to do. A GnuTLS/TLS line is the intercepting
/// proxy from the 2026-10-17 workstation report; `not our ref` is a pin the
/// remote does not have (go-vulndb). Neither hint invents a replacement sha.
pub(crate) fn git_fetch_failure(sha: &str, output: &str) -> String {
    let trimmed = output.trim();
    let shown: String = trimmed.chars().take(GIT_FETCH_OUTPUT_CAP).collect();
    let mut msg = if shown.is_empty() {
        format!("git fetch {sha} failed")
    } else {
        format!("git fetch {sha} failed: {shown}")
    };
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("gnutls")
        || lower.contains("ssl")
        || lower.contains("certificate")
        || lower.contains("tls")
    {
        msg.push_str(
            ". HTTPS failed before any objects arrived: check HTTPS_PROXY and \
             SSL_CERT_FILE or GIT_SSL_CAINFO, or index a local checkout instead of \
             `xerj corpus add --from`",
        );
    } else if lower.contains("not our ref")
        || lower.contains("couldn't find remote ref")
        || lower.contains("could not find remote ref")
    {
        msg.push_str(
            ". The pinned commit is not on the remote. Do not invent a replacement \
             pin; check the manifest sha against the upstream repo",
        );
    }
    msg
}

/// Move a clone to `sha` (shallow). Never a shell string: the sha comes from
/// an untrusted manifest, and `Command` passes it as ONE argv element.
/// An empty `paths` list checks out the whole pin — every caller that is not
/// a subpath harvest.
pub(crate) fn checkout_at_sha(target: &Path, url: &str, sha: &str) -> Result<()> {
    checkout_at_sha_paths(target, url, sha, &[])
}

/// [`checkout_at_sha`] that harvests only `paths` when the list is non-empty
/// (#1842). `paths` is validated again here: this is the function that turns
/// the strings into a git argv.
pub(crate) fn checkout_at_sha_paths(
    target: &Path,
    url: &str,
    sha: &str,
    paths: &[String],
) -> Result<()> {
    ensure_pin_origin(target, url)?;
    fetch_pin(target, sha)?;
    // Refuse a path the pin does not contain BEFORE the sparse rules change
    // the worktree. A typo must not delete the previous checkout.
    let kinds = confirm_paths_exist(target, sha, paths)?;
    apply_sparse_limit(target, paths, &kinds)?;
    checkout_pin(target, sha)?;
    git(Some(target), &["clean", "-qfd"]).ok();
    confirm_paths_materialized(target, paths, &kinds)?;
    Ok(())
}

/// The worktree is already at `sha`. Apply or clear the subpath limit without
/// fetching again. A re-run of a manifest that gained `paths` narrows a
/// previous whole-repo checkout; a re-run that dropped `paths` restores it.
pub(crate) fn limit_checked_out_pin(target: &Path, sha: &str, paths: &[String]) -> Result<()> {
    let kinds = confirm_paths_exist(target, sha, paths)?;
    let was_sparse = sparse_checkout_enabled(target);
    apply_sparse_limit(target, paths, &kinds)?;
    if !paths.is_empty() || was_sparse {
        git(Some(target), &["clean", "-qfd"]).ok();
    }
    confirm_paths_materialized(target, paths, &kinds)?;
    Ok(())
}

fn ensure_pin_origin(target: &Path, url: &str) -> Result<()> {
    if target.join(".git").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(target)?;
    git(Some(target), &["init", "--quiet"])?;
    git(Some(target), &["remote", "add", "origin", url]).ok();
    git(Some(target), &["config", "remote.origin.promisor", "true"]).ok();
    git(
        Some(target),
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    )
    .ok();
    Ok(())
}

fn fetch_pin(target: &Path, sha: &str) -> Result<()> {
    match git_long(Some(target), &["fetch", "--depth", "1", "origin", sha]) {
        Ok((0, _)) => Ok(()),
        Ok((_, output)) => anyhow::bail!("{}", git_fetch_failure(sha, &output)),
        Err(e) => anyhow::bail!("{}", git_fetch_failure(sha, &e.to_string())),
    }
}

fn checkout_pin(target: &Path, sha: &str) -> Result<()> {
    let co = git_long(
        Some(target),
        &["checkout", "--quiet", "--force", "--detach", sha],
    );
    if !co.map(|(rc, _)| rc == 0).unwrap_or(false) {
        anyhow::bail!("git checkout {sha} failed");
    }
    Ok(())
}

/// `git()` glues stderr onto stdout. Object types and config values are one
/// word on stdout; a fetch warning on stderr must not change the word.
/// Stderr is returned separately so a failed command can still quote git.
fn git_stdout(dir: &Path, args: &[&str]) -> Result<(i32, String, String)> {
    let mut c = Command::new("git");
    c.current_dir(dir).arg("-C").arg(dir);
    let out = c
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.first().unwrap_or(&"")))?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

fn sparse_checkout_enabled(target: &Path) -> bool {
    match git_stdout(
        target,
        &[
            "config",
            "--get",
            "--default",
            "false",
            "core.sparseCheckout",
        ],
    ) {
        Ok((0, out, _)) => out.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Limit the worktree to `paths`, or clear a previous limit.
///
/// Cone mode is the wrong shape here. `git sparse-checkout set --cone
/// data/osv` also materializes every file in ancestor directories (the
/// licence, the module file, the root tests). `corpus index` walks whatever
/// is on disk, and those extra files are how a "subpath" harvest still
/// becomes another dataset. Non-cone pattern `/{path}/` is the directory and
/// everything under it, and not a sibling whose name merely starts with the
/// same prefix (`data/osv-extra`). A file entry is `/{path}`: that file and
/// not `resample.pyx` beside `resample.py` (#2486). Git 2.43 accepts
/// `--no-cone`. If a future git removes it, this fails closed instead of
/// cloning the whole pin.
fn apply_sparse_limit(target: &Path, paths: &[String], kinds: &[PathKind]) -> Result<()> {
    if paths.is_empty() {
        if !sparse_checkout_enabled(target) {
            return Ok(());
        }
        let (rc, out) = git(Some(target), &["sparse-checkout", "disable"])?;
        if rc != 0 {
            anyhow::bail!("git sparse-checkout disable failed: {out}");
        }
        return Ok(());
    }
    for path in paths {
        xccode::pathgate::valid_sparse_path(path).map_err(anyhow::Error::msg)?;
    }
    let patterns: Vec<String> = paths
        .iter()
        .zip(kinds)
        .map(|(path, kind)| match kind {
            PathKind::Dir => format!("/{path}/"),
            PathKind::File => format!("/{path}"),
        })
        .collect();
    let mut args: Vec<&str> = vec!["sparse-checkout", "set", "--no-cone", "--"];
    for pattern in &patterns {
        args.push(pattern);
    }
    let (rc, out) = git(Some(target), &args)?;
    if rc != 0 {
        anyhow::bail!(
            "git sparse-checkout set failed for {}: {out}",
            paths.join(", ")
        );
    }
    Ok(())
}

/// What a `paths` entry names at the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Dir,
    File,
}

/// Classify every entry at the pin, in order. A directory or a regular file
/// (#2486) is harvested. A symlink, a submodule, or a path the pin does not
/// contain is refused: a link would hand `corpus index` a target the
/// manifest author chose. `ls-tree` reads the tree entry's mode from the
/// tree objects the shallow fetch already has, so classifying a file does
/// not fetch its content (`cat-file -t` would, in a blob:none clone).
fn confirm_paths_exist(target: &Path, sha: &str, paths: &[String]) -> Result<Vec<PathKind>> {
    let mut kinds = Vec::with_capacity(paths.len());
    for path in paths {
        let out = match git_stdout(target, &["ls-tree", sha, "--", path]) {
            Ok((0, out, _)) => out,
            Ok((_, _, err)) => anyhow::bail!("could not inspect '{path}' at {sha}: {err}"),
            Err(e) => anyhow::bail!("could not inspect '{path}' at {sha}: {e}"),
        };
        let mut lines = out.lines();
        let mode = match (lines.next(), lines.next()) {
            (Some(line), None) => line.split_whitespace().next().unwrap_or(""),
            (None, _) => anyhow::bail!(
                "paths entry '{path}' is not a directory or file at {sha}. The pin \
                 has no such path, so nothing was checked out"
            ),
            (Some(_), Some(_)) => {
                anyhow::bail!("paths entry '{path}' names more than one entry at {sha}")
            }
        };
        match mode {
            "040000" => kinds.push(PathKind::Dir),
            "100644" | "100755" => kinds.push(PathKind::File),
            "120000" => anyhow::bail!(
                "paths entry '{path}' is a symlink at {sha}. Name the directory or \
                 file it points to instead"
            ),
            "160000" => anyhow::bail!(
                "paths entry '{path}' is a submodule at {sha}. Its content lives in \
                 another repository; pin that repository as its own entry"
            ),
            _ => anyhow::bail!(
                "paths entry '{path}' has git mode {mode} at {sha}, not a directory or \
                 file. Name a directory (for example \"data/osv\") or a file to harvest"
            ),
        }
    }
    Ok(kinds)
}

fn confirm_paths_materialized(target: &Path, paths: &[String], kinds: &[PathKind]) -> Result<()> {
    for (path, kind) in paths.iter().zip(kinds) {
        let present = match kind {
            PathKind::Dir => target.join(path).is_dir(),
            PathKind::File => std::fs::symlink_metadata(target.join(path))
                .map(|meta| meta.is_file())
                .unwrap_or(false),
        };
        if !present {
            anyhow::bail!(
                "sparse checkout did not materialize '{path}'. The pin lists that \
                 path, but the worktree does not contain it"
            );
        }
    }
    Ok(())
}

fn paths_note(paths: &[String]) -> String {
    if paths.is_empty() {
        String::new()
    } else {
        format!(" (only {})", paths.join(", "))
    }
}

/// Observable incomplete checkout state, never proof that files are disposable.
#[derive(Debug)]
enum Unfinished {
    NoCommit,
    Checkout,
    Locked,
    Unreadable(String),
}

fn unfinished_clone(target: &Path) -> Option<Unfinished> {
    let index = match checkout_index_path(target) {
        Ok(index) => index,
        Err(state) => return Some(state),
    };
    if index.with_file_name("index.lock").exists() {
        return Some(Unfinished::Locked);
    }
    unfinished_without_index_lock(target, &index)
}

/// `--git-dir` prevents accidentally resolving an enclosing repository;
/// `--git-path` handles linked worktrees and their separate index correctly.
fn checkout_index_path(target: &Path) -> std::result::Result<PathBuf, Unfinished> {
    match git_stdout(
        target,
        &["--git-dir=.git", "rev-parse", "--git-path", "index"],
    ) {
        Ok((0, out, _)) if !out.is_empty() => Ok(target.join(out)),
        Ok((_, _, err)) => Err(Unfinished::Unreadable(why_unreadable(&err))),
        Err(e) => Err(Unfinished::Unreadable(why_unreadable(&e.to_string()))),
    }
}

fn why_unreadable(out: &str) -> String {
    let why = out.lines().next().unwrap_or("").trim();
    if why.is_empty() {
        "HEAD names no readable commit".to_string()
    } else {
        why.chars().take(200).collect()
    }
}

/// Everything [`unfinished_clone`] checks except the pending index lock, so a
/// caller holding the corpus lock can ask whether the checkout itself ever
/// finished (#2520).
fn unfinished_without_index_lock(target: &Path, index: &Path) -> Option<Unfinished> {
    let unreadable = |out: &str| Some(Unfinished::Unreadable(why_unreadable(out)));
    let rev = |spec: &str| {
        git_stdout(
            target,
            &["--git-dir=.git", "rev-parse", "--verify", "--quiet", spec],
        )
    };
    match rev("HEAD") {
        Ok((0, _, _)) => {}
        Ok((1, _, _)) => return Some(Unfinished::NoCommit),
        Ok((_, _, err)) => return unreadable(&err),
        Err(e) => return unreadable(&e.to_string()),
    }
    match rev("HEAD^{commit}") {
        Ok((0, _, _)) => {}
        Ok((_, _, err)) => return unreadable(&err),
        Err(e) => return unreadable(&e.to_string()),
    }
    if !index.is_file() {
        // A committed empty tree legitimately needs no worktree index on
        // some Git versions. A nonempty tree without an index is ambiguous.
        let empty_tree = matches!(
            git_stdout(target, &["--git-dir=.git", "ls-tree", "--name-only", "HEAD"]),
            Ok((0, out, _)) if out.is_empty()
        );
        if !empty_tree {
            return Some(Unfinished::Checkout);
        }
    }
    None
}

/// The `.git/index.lock` of a checkout that never finished, if one is left.
///
/// A git that `corpus add` started keeps the corpus-add lock after `xerj` dies
/// (#2520), so while this run holds that lock no such git is alive, and a lock
/// in a checkout that never finished belongs to an attempt that is over. A lock
/// in a checkout that did finish is left alone: a git the operator is running
/// there may own it, and removing it would let two gits write one index.
#[cfg(unix)]
fn stale_index_lock(target: &Path) -> Option<PathBuf> {
    let index = checkout_index_path(target).ok()?;
    let lock = index.with_file_name("index.lock");
    if !lock.exists() {
        return None;
    }
    unfinished_without_index_lock(target, &index)
        .is_some()
        .then_some(lock)
}

fn unfinished_checkout_message(target: &Path, state: &Unfinished) -> String {
    let why = match state {
        Unfinished::NoCommit => "HEAD has no commit".to_string(),
        Unfinished::Checkout => "the Git index is missing".to_string(),
        Unfinished::Locked => {
            "Git index.lock exists; a Git operation may still be running".to_string()
        }
        Unfinished::Unreadable(why) => format!("Git cannot read the checkout: {why}"),
    };
    format!(
        "{} is not a reusable checkout ({why}); kept its files unchanged. \
             Wait for any active Git operation to finish. If this checkout is incomplete, \
             move the directory to an unused backup location, then rerun this corpus add command",
        target.display()
    )
}

/// Publish only a successfully completed new checkout. The caller holds the
/// corpus lock; the destination is under the operator-owned corpus directory.
/// TemporaryDir cleans this attempt's private stage on ordinary failure. A
/// process kill may leave a hidden stage, but never an apparent final clone.
fn stage_new_checkout(target: &Path, clone: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let absent = || -> Result<()> {
        match std::fs::symlink_metadata(target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("cannot inspect {}", target.display())),
            Ok(_) => anyhow::bail!("{} already exists; kept it unchanged. Move it to an unused backup location before retrying", target.display()),
        }
    };
    absent()?;
    let parent = target
        .parent()
        .context("checkout has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".xerj-clone-")
        .tempdir_in(parent)?;
    let checkout = stage.path().join("checkout");
    clone(&checkout)?;
    if !existing_checkout_is_complete(&checkout) {
        anyhow::bail!("Git did not produce a complete checkout; final directory was not published");
    }
    // An external writer may have created a destination while Git ran. Do
    // not replace it. Cooperating corpus writers are serialized by the lock.
    absent()?;
    std::fs::rename(&checkout, target)
        .with_context(|| format!("cannot publish checkout at {}", target.display()))?;
    Ok(())
}

/// One `corpus add` per corpus at a time (#2453): a second run would
/// otherwise take the first one's clone in progress for an unfinished one.
/// Released when the returned file is dropped.
fn lock_corpus_add(root: &Path, name: &str) -> Result<std::fs::File> {
    let dir = root.join("locks");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(format!("corpus-add-{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => anyhow::bail!(
            "corpus '{name}' is in use by another `xerj corpus add`, or by a git it started that \
             is still running (lock: {}). Run this again when it exits.",
            path.display()
        ),
        Err(e) => Err(anyhow::Error::new(e).context(format!("cannot lock {}", path.display()))),
    }
}

// The corpus-add lock's descriptor while a run holds it, `-1` otherwise
// (#2520). `GitLockHandoff` is the only writer, and it and the gits it covers
// run on the same thread: `corpus add` clones one repository at a time.
thread_local! {
    static GIT_LOCK_FD: Cell<i32> = const { Cell::new(-1) };
}

/// Hands the corpus-add lock to the git processes that can outlive this run, so
/// the lock lasts exactly as long as the git a killed `xerj` left behind.
///
/// Without it a caller that kills `xerj` alone leaves its git running, and the
/// next run takes the folder for one stopped before its first checkout and
/// clones into it while that git still writes there (#2520). With it, the next
/// run is refused until the git exits, and a `.git/index.lock` it left is known
/// to be stale ([`stale_index_lock`]).
///
/// The descriptor is inherited, not copied: `flock` belongs to the open file
/// description, so every child holding it keeps the lock. Git helpers start
/// children of their own (`git remote-https`, `index-pack`), which inherit it
/// too. `-c core.fsmonitor=false` keeps a fsmonitor daemon, which outlives the
/// git that started it, from holding the lock for its own timeout.
struct GitLockHandoff;

impl GitLockHandoff {
    #[cfg(unix)]
    fn new(lock: &std::fs::File) -> Self {
        use std::os::unix::io::AsRawFd;
        GIT_LOCK_FD.with(|fd| fd.set(lock.as_raw_fd()));
        Self
    }

    /// Windows has no `FD_CLOEXEC` to clear and no `flock` to inherit, so a
    /// killed `xerj` still leaves its git unaccounted for there.
    #[cfg(not(unix))]
    fn new(_lock: &std::fs::File) -> Self {
        Self
    }
}

impl Drop for GitLockHandoff {
    fn drop(&mut self) {
        GIT_LOCK_FD.with(|fd| fd.set(-1));
    }
}

#[cfg(unix)]
fn inherit_corpus_lock(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    let fd = GIT_LOCK_FD.with(|fd| fd.get());
    if fd < 0 {
        return;
    }
    c.arg("-c").arg("core.fsmonitor=false");
    // SAFETY: the closure runs in the child between `fork` and `exec`, where
    // only async-signal-safe calls are allowed; `fcntl` is one, and it does not
    // allocate or lock. `fd` is still open in the parent: the `File` that holds
    // the lock outlives every [`GitLockHandoff`] (the handoff is dropped first).
    unsafe {
        c.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn inherit_corpus_lock(_c: &mut Command) {}

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

/// What to do with a local source's root `.xerjignore` once the clone exists.
enum XerjignoreCarry {
    /// Unpinned `corpus add <name> <path>`: the working tree is what the
    /// caller pointed at. Copy when the clone's bytes differ.
    Copy,
    /// Pinned `--from`: the recorded sha is the contract. Do not copy.
    WarnPinned,
}

/// Ignore files are a few lines. Above this, refuse rather than pour a
/// symlink-to-a-dataset into the clone.
const XERJIGNORE_CARRY_MAX_BYTES: u64 = 1024 * 1024;

/// Repo directory name for a plain-URL or local-path source: its last path
/// segment without `.git`. #2472: a Windows path (`F:\Projects\Foo`) has
/// `\`-separated segments, so both separators split.
fn repo_name_from_source(url: &str) -> String {
    url.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".git")
        .to_string()
}

/// Filesystem root of a local checkout named by `url`, if `url` is one.
///
/// `git clone` of a filesystem path or a `file://` URL checks out committed
/// files only. A remote URL has no working tree to read. `git@host:path`
/// is not a directory, so it falls out through `is_dir`.
fn local_checkout_root(url: &str) -> Option<PathBuf> {
    let path = if let Some(rest) = url.strip_prefix("file://") {
        file_url_path(rest)?
    } else if url.contains("://") {
        return None;
    } else {
        PathBuf::from(url)
    };
    path.is_dir().then_some(path)
}

/// Partial-clone configuration is context for an observed clone failure,
/// not proof that objects are missing. A hydrated source remains cloneable.
/// Git supports multiple named promisor remotes, not only `origin`.
fn partial_clone_cause(src: &Path) -> Option<String> {
    let concise = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect::<String>()
    };
    if let Ok((0, out, _)) = git_stdout(src, &["config", "--get", "extensions.partialclone"]) {
        if !out.is_empty() {
            return Some(format!("extensions.partialClone={}", concise(&out)));
        }
    }
    if let Ok((0, out, _)) = git_stdout(
        src,
        &[
            "config",
            "--type=bool",
            "--get-regexp",
            r"^remote\..*\.promisor$",
        ],
    ) {
        for line in out.lines() {
            if let Some(key) = line.strip_suffix(" true") {
                return Some(format!("{}=true", concise(key)));
            }
        }
    }
    None
}

/// Advice is conditional and follows Git's actual failure. Quote the source
/// path for the named shell; the partial working tree may cover only a subset.
fn partial_clone_failure_hint(repo_name: &str, src: &Path, cause: &str) -> Vec<String> {
    let path = src.to_string_lossy();
    #[cfg(not(windows))]
    let (shell, quoted) = (
        "POSIX shell",
        format!("'{}'", path.replace('\'', "'\"'\"'")),
    );
    #[cfg(windows)]
    let (shell, quoted) = ("PowerShell", format!("'{}'", path.replace('\'', "''")));
    vec![
        format!("xerj corpus add: the source has partial-clone configuration ({cause}); missing promised objects may explain this Git failure"),
        format!("xerj corpus add: to index only files already present in its working tree ({shell}): xerj autoindex {quoted} --prefix {repo_name}"),
        "xerj corpus add: for a complete corpus, restore access to the promisor remote or use a new clone made without --filter, then retry".to_string(),
    ]
}

/// `file:///abs/path` → `/abs/path`. `file://localhost/abs/path` → `/abs/path`.
fn file_url_path(rest: &str) -> Option<PathBuf> {
    let path = if rest.starts_with('/') {
        rest
    } else {
        let slash = rest.find('/')?;
        &rest[slash..]
    };
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// #2199: `git clone` does not carry a root `.xerjignore` that is untracked
/// or edited and uncommitted, so the index then applies built-in rules only.
/// For an unpinned local checkout, copy the working-tree file when its bytes
/// differ. For a pin, leave the recorded tree alone and say the file was not
/// copied. Remote URLs and a missing file are silent. Nested `.xerjignore`
/// files are not carried.
fn settle_local_xerjignore(url: &str, target: &Path, repo_name: &str, mode: XerjignoreCarry) {
    let Some(source) = local_checkout_root(url) else {
        return;
    };
    let src = source.join(crate::ignore_rules::XERJIGNORE);
    let src_meta = match std::fs::metadata(&src) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!(
                "  [ignore] {repo_name}: could not read {} ({e}); it was NOT copied into the clone, so indexing will not see those rules",
                src.display()
            );
            return;
        }
    };
    if !src_meta.is_file() {
        eprintln!(
            "  [ignore] {repo_name}: {} is not a regular file and was NOT copied into the clone, so indexing will not see those rules",
            src.display()
        );
        return;
    }
    if src_meta.len() > XERJIGNORE_CARRY_MAX_BYTES {
        eprintln!(
            "  [ignore] {repo_name}: {} is {} bytes, over the {XERJIGNORE_CARRY_MAX_BYTES} byte carry limit, and was NOT copied into the clone, so indexing will not see those rules",
            src.display(),
            src_meta.len()
        );
        return;
    }
    let src_bytes = match std::fs::read(&src) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "  [ignore] {repo_name}: could not read {} ({e}); it was NOT copied into the clone, so indexing will not see those rules",
                src.display()
            );
            return;
        }
    };
    let dest = target.join(crate::ignore_rules::XERJIGNORE);
    let same = match std::fs::metadata(&dest) {
        Ok(m) if m.is_file() && m.len() == src_bytes.len() as u64 => {
            std::fs::read(&dest).ok().as_deref() == Some(src_bytes.as_slice())
        }
        _ => false,
    };
    if same {
        return;
    }
    match mode {
        XerjignoreCarry::Copy => {
            // A committed symlink must not be written through: that would
            // change the link target instead of the clone's ignore file.
            if std::fs::symlink_metadata(&dest)
                .ok()
                .is_some_and(|m| m.file_type().is_symlink())
            {
                if let Err(e) = std::fs::remove_file(&dest) {
                    eprintln!(
                        "  [ignore] {repo_name}: could not replace symlink {} ({e}); indexing will not see the source rules",
                        dest.display()
                    );
                    return;
                }
            }
            match std::fs::write(&dest, &src_bytes) {
                Ok(()) => eprintln!(
                    "  [ignore] {repo_name}: copied the source working tree's .xerjignore from {} into the clone (git clone only checks out committed files)",
                    source.display()
                ),
                Err(e) => eprintln!(
                    "  [ignore] {repo_name}: could not copy {} into the clone ({e}); indexing will not see those rules",
                    src.display()
                ),
            }
        }
        XerjignoreCarry::WarnPinned => eprintln!(
            "  [ignore] {repo_name}: source .xerjignore at {} differs from the pinned checkout and was NOT copied (a pinned rebuild stays at the recorded sha)",
            src.display()
        ),
    }
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

/// One repo `corpus add` will materialize. `paths` empty means the whole pin.
struct CorpusSource {
    repo: String,
    url: String,
    sha: String,
    declared: String,
    paths: Vec<String>,
}

fn run_corpus_add(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut as_name: Option<String> = None;
    let mut from: Option<String> = None;
    let mut verify_sig: Option<String> = None;
    let mut own_flag = false;
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
            "--own" => own_flag = true,
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
                return run_corpus_add_pack(
                    &dir,
                    explicit_name,
                    &share,
                    verify_sig.as_deref(),
                    own_flag,
                );
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
                return run_corpus_add_pack(
                    &dir,
                    explicit_name,
                    &zip,
                    verify_sig.as_deref(),
                    own_flag,
                );
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
    // hints (#1254) ride beside the rows rather than inside them — a
    // three-slot tuple tripped clippy's type_complexity on the pinned
    // CI toolchain. `paths` (#1842) stays on the row: it is per repo.
    let mut pin_query: Option<manifest::QueryHints> = None;
    // repo -> (sha, review) from the hub pin: a human's licence review of the
    // repo at that sha, recorded when the checkout lands there (#1893).
    let mut pin_reviews: std::collections::HashMap<String, (String, Value)> =
        std::collections::HashMap::new();
    let (name, rows): (String, Vec<CorpusSource>) = if let Some(path) = &from {
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
        for r in &hub.rows {
            if let (Some(review), false) = (&r.review, r.sha.is_empty()) {
                pin_reviews.insert(r.repo.clone(), (r.sha.clone(), review.clone()));
            }
        }
        (
            resolved,
            hub.rows
                .into_iter()
                .map(|r| CorpusSource {
                    repo: r.repo,
                    url: r.url,
                    sha: r.sha,
                    declared: r.declared_licence,
                    paths: r.paths,
                })
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
                .map(|u| CorpusSource {
                    repo: repo_name_from_source(u),
                    url: u.clone(),
                    sha: String::new(),
                    declared: String::new(),
                    paths: Vec::new(),
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
    let _lock = match lock_corpus_add(&root, &name) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("xerj corpus add: {e:#}");
            return 1;
        }
    };
    // The git operations that outlive a killed `xerj` inherit the lock, so
    // one still writing into a folder the next run reuses is noticed (#2520).
    let _handoff = GitLockHandoff::new(&_lock);
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
    // #2339: the marker is the operator's, not the manifest they handed us.
    // `--own` sets it; a re-add without the flag keeps one already recorded
    // so a refresh cannot silently drop it. A `--from` file's own key is
    // never read.
    let own = own_flag || previous.as_ref().is_some_and(|m| m.own);
    // A hub pin's review of the same repo at the same sha comes next: it is
    // the review the registry shipped for exactly this checkout (#1893).
    let carry = |repo: &str, sha: &str| -> Option<Value> {
        let kept = previous.as_ref().and_then(|m| {
            m.repos.iter().find_map(|r| {
                (r.repo == repo && r.sha == sha)
                    .then(|| r.review.clone())
                    .flatten()
            })
        });
        kept.or_else(|| {
            pin_reviews
                .get(repo)
                .filter(|(pinned, _)| !sha.is_empty() && pinned == sha)
                .map(|(_, review)| review.clone())
        })
    };

    let mut entries: Vec<manifest::ManifestRepo> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for source in rows {
        let CorpusSource {
            repo: repo_name,
            url,
            sha,
            declared,
            paths,
        } = source;
        // The one destructive step (checkout --force + clean -fd) runs inside
        // this path: gate the name HERE, again, even though read_hub_manifest
        // already did — the plain-URL arm has no other gate.
        if xccode::pathgate::valid_repo_name(&repo_name).is_err() || repo_name.starts_with('-') {
            eprintln!("  [FAIL] refusing unsafe repo name '{repo_name}'");
            skipped.push(repo_name);
            continue;
        }
        let target = dest.join(&repo_name);
        let note = paths_note(&paths);
        // After the checkout exists and before the manifest counts files.
        // A pin warns instead of copying; an unpinned local path copies.
        let carry_ignore = |checkout: &Path| {
            settle_local_xerjignore(
                &url,
                checkout,
                &repo_name,
                if sha.is_empty() {
                    XerjignoreCarry::Copy
                } else {
                    XerjignoreCarry::WarnPinned
                },
            );
        };
        // Existing state may contain local edits, even without HEAD or an
        // index. Neither condition authorizes deletion or reset --hard.
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
            eprintln!("  [FAIL] {repo_name}: checkout path is a symlink; kept it unchanged");
            skipped.push(repo_name);
            continue;
        }
        if target.join(".git").exists() {
            #[cfg(unix)]
            if let Some(lock) = stale_index_lock(&target) {
                if std::fs::remove_file(&lock).is_ok() {
                    println!(
                        "  [lock] {repo_name}: removed a stale {} left by an interrupted checkout",
                        lock.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "index.lock".to_string())
                    );
                }
            }
            if let Some(state) = unfinished_clone(&target) {
                eprintln!(
                    "  [FAIL] {repo_name}: {}; continuing",
                    unfinished_checkout_message(&target, &state)
                );
                skipped.push(repo_name);
                continue;
            }
        }
        if target.join(".git").exists() && !sha.is_empty() {
            let at = git(Some(&target), &["rev-parse", "HEAD"])
                .ok()
                .filter(|(rc, _)| *rc == 0)
                .map(|(_, out)| out);
            if at.as_deref() == Some(sha.as_str()) {
                // HEAD matching the pin is not the harvest. A previous run
                // may have checked out the whole repo; `paths` still has to
                // narrow it, and a manifest that dropped `paths` has to
                // widen it back.
                if let Err(e) = limit_checked_out_pin(&target, &sha, &paths) {
                    eprintln!("  [FAIL] {repo_name}: {e:#}; continuing");
                    skipped.push(repo_name);
                    continue;
                }
                println!(
                    "  [ok] {repo_name} already at {}{note}",
                    &sha[..12.min(sha.len())]
                );
                carry_ignore(&target);
                record(
                    &mut entries,
                    &repo_name,
                    &url,
                    &target,
                    &declared,
                    &paths,
                    &carry,
                    own,
                );
                continue;
            }
            println!("  [pin] {repo_name} -> {}{note}", &sha[..12.min(sha.len())]);
            if let Err(e) = checkout_at_sha_paths(&target, &url, &sha, &paths) {
                eprintln!("  [FAIL] {repo_name}: {e:#}; continuing");
                skipped.push(repo_name);
                continue;
            }
            carry_ignore(&target);
            record(
                &mut entries,
                &repo_name,
                &url,
                &target,
                &declared,
                &paths,
                &carry,
                own,
            );
            continue;
        }
        if existing_checkout_is_complete(&target) {
            println!("  [skip] {repo_name} already cloned");
            carry_ignore(&target);
            record(
                &mut entries,
                &repo_name,
                &url,
                &target,
                &declared,
                &paths,
                &carry,
                own,
            );
            continue;
        }
        println!("  [clone] {repo_name}");
        let cloned = stage_new_checkout(&target, |checkout| {
            if sha.is_empty() {
                let target_s = checkout.to_string_lossy().to_string();
                let (rc, output) =
                    git_long(None, &["clone", "--depth", "1", "--", &url, &target_s])?;
                if rc != 0 {
                    let shown: String = output
                        .replace(&url, "[source]")
                        .chars()
                        .take(GIT_FETCH_OUTPUT_CAP)
                        .collect();
                    let hint = local_checkout_root(&url)
                        .and_then(|src| {
                            partial_clone_cause(&src).map(|cause| {
                                partial_clone_failure_hint(&repo_name, &src, &cause).join("\n")
                            })
                        })
                        .map(|lines| format!("\n{lines}"))
                        .unwrap_or_default();
                    anyhow::bail!("git clone failed (exit {rc}): {shown}{hint}");
                }
                Ok(())
            } else {
                println!("  [pin] {repo_name} -> {}{note}", &sha[..12.min(sha.len())]);
                checkout_at_sha_paths(checkout, &url, &sha, &paths)
            }
        });
        if let Err(e) = cloned {
            eprintln!("  [FAIL] {repo_name}: {e:#}; continuing");
            skipped.push(repo_name);
            continue;
        }
        carry_ignore(&target);
        record(
            &mut entries,
            &repo_name,
            &url,
            &target,
            &declared,
            &paths,
            &carry,
            own,
        );
    }

    if entries.is_empty() {
        eprintln!("xerj corpus add: nothing cloned");
        return 1;
    }

    // Regenerated FROM DISK, never copied through from the input — except
    // the pin's query hints (#1254) and per-repo `paths` (#1842), which are
    // author DECLARATION, not derived state: they travel from the hub pin
    // into the corpus.json the clone writes, so a re-clone cannot silently
    // drop the corpus's retrieval posture or widen a subpath harvest.
    let cloned_at = chrono_now_stamp();
    let manifest_path = dest.join("corpus.json");
    manifest::write_corpus_manifest_kind(
        &manifest_path,
        &name,
        None,
        &cloned_at,
        &entries,
        pin_query.as_ref(),
        own,
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
    if own {
        println!(
            "corpus '{name}' is marked own (first-party): an unidentified licence stays recorded, \
             and xerj code omits the do-not-copy banner for it. Clear with: xerj corpus own {name} --off"
        );
    }

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
    own_flag: bool,
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
    let _lock = match lock_corpus_add(&root, &name) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("xerj corpus add: {e:#}");
            return 1;
        }
    };
    // Same one-kind-per-name rule the git arm enforces, from the other side.
    let previous_own = manifest::read_corpus_manifest(&dest.join("corpus.json"))
        .ok()
        .is_some_and(|m| m.own);
    let own = own_flag || previous_own;
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
    if let Err(e) = add_pack_materialize(&dest, pack_dir, &meta, own) {
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
        if xccode::licence::copy_warning_applies(&s.licence, own) {
            if let Some(w) = xccode::licence::clone_warning_line(&s.licence) {
                eprintln!("{w}");
            }
        }
    }
    if own {
        println!(
            "corpus '{name}' is marked own (first-party): an unidentified licence stays recorded, \
             and xerj code omits the do-not-copy banner for it. Clear with: xerj corpus own {name} --off"
        );
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
    own: bool,
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
            paths: Vec::new(),
        });
    }
    manifest::write_corpus_manifest_kind(
        &dest.join("corpus.json"),
        &meta.name,
        Some("harvested"),
        &chrono_now_stamp(),
        &entries,
        None,
        own,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn record(
    entries: &mut Vec<manifest::ManifestRepo>,
    repo_name: &str,
    url: &str,
    target: &Path,
    declared: &str,
    paths: &[String],
    carry: &dyn Fn(&str, &str) -> Option<Value>,
    own: bool,
) {
    let lic = xccode::licence::detect_licence(target);
    let (sha, files, bytes) = dir_stats(target);
    println!(
        "          licence={lic} sha={} files={files}",
        &sha[..12.min(sha.len())]
    );
    let review = carry(repo_name, &sha);
    // The recorded `licence` stays the detector's reading; a completed human
    // review fills in a licence the detector could not identify (#1893), and
    // that is what `xerj code` labels passages with.
    let shown = xccode::licence::effective_licence(&lic, review.as_ref());
    if shown != lic {
        let by = review
            .as_ref()
            .and_then(|r| r.get("by"))
            .and_then(Value::as_str)
            .unwrap_or("");
        println!(
            "          licence {shown} per the manifest's review by {by} (the detector read {lic})"
        );
    }
    if xccode::licence::copy_warning_applies(&shown, own) {
        if let Some(w) = xccode::licence::clone_warning_line(&shown) {
            eprintln!("{w}");
        }
    }
    // The detector is text-matching heuristics and has been wrong before.
    // When a manifest disagrees with the checkout, say so rather than
    // overwrite quietly.
    // `shown` is the detector's reading, or a completed review's licence
    // when the detector read UNKNOWN (#1893). A coarse `BSD` reading agrees
    // with a manifest that names the BSD clause.
    if !declared.is_empty() && !xccode::licence::licence_readings_agree(declared, &shown) {
        eprintln!("          ! manifest says licence={declared}, checkout reads {lic} — verify before copying");
    }
    entries.push(manifest::ManifestRepo {
        repo: repo_name.to_string(),
        url: url.to_string(),
        licence: lic,
        sha,
        files: Some(files),
        bytes: Some(bytes),
        review,
        paths: paths.to_vec(),
    });
}

fn chrono_now_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── xerj corpus own ─────────────────────────────────────────────────────────

/// Set or clear the first-party marker on a corpus that already exists.
/// Does not clone, detect licences, or touch the index.
fn run_corpus_own(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut off = false;
    for a in args {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{CORPUS_USAGE}");
                return 0;
            }
            "--off" => off = true,
            a if a.starts_with('-') => {
                eprintln!("xerj corpus own: unknown flag '{a}'\n\n{CORPUS_USAGE}");
                return 2;
            }
            a if name.is_none() => name = Some(a.to_string()),
            a => {
                eprintln!("xerj corpus own: unexpected argument '{a}'\n\n{CORPUS_USAGE}");
                return 2;
            }
        }
    }
    let Some(name) = name else {
        eprintln!("xerj corpus own: <name> is required\n\n{CORPUS_USAGE}");
        return 2;
    };
    if let Err(e) = xccode::pathgate::valid_corpus_name(&name) {
        eprintln!("xerj corpus own: {e}");
        return 2;
    }
    let path = code_root().join("corpora").join(&name).join("corpus.json");
    if !path.is_file() {
        eprintln!(
            "xerj corpus own: no corpus '{name}' at {} — run `xerj corpus add {name} …` first",
            path.display()
        );
        return 2;
    }
    let own = !off;
    if let Err(e) = manifest::set_corpus_own(&path, &name, own) {
        eprintln!("xerj corpus own: {e}");
        return 1;
    }
    if own {
        println!(
            "corpus '{name}' is marked own (first-party): an unidentified licence stays recorded, \
             and xerj code omits the do-not-copy banner for it. Clear with: xerj corpus own {name} --off"
        );
    } else {
        println!(
            "corpus '{name}' is no longer marked own. Unidentified licences warn again: \
             adapt the approach, do not copy the code."
        );
    }
    0
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

fn report(root: &Path, name: &str, rc: i32, docs: &str, include_data: bool) {
    // #2339 / #1985: the walk's file-class accounting, printed with the
    // verdict so it is in the tail of `xerj corpus index` rather than only
    // in the progress notes from the start of the run.
    for line in crate::corpus_file_class_summary(&root.join("corpora").join(name), include_data) {
        println!("xerj corpus index: {line}");
    }
    for line in report_footer(name, rc, docs) {
        println!("{line}");
    }
}

/// Printed before the long part of `corpus index`. An agent tool timeout
/// kills the process and looks like a silent failure; the run itself resumes.
pub(crate) fn corpus_index_duration_note() -> &'static str {
    "xerj corpus index: this often runs longer than an agent tool timeout \
     (minutes, longer with a neural embedder). Stopping is safe: re-run the \
     same command to resume. To keep it off the tool clock, start it in the \
     background and wait for the xerj-done line."
}

/// Parsed `xerj corpus index` arguments, separated from the run so the flag
/// set (including #1985's `--include-data`) is testable without a node.
#[derive(Debug, Default, PartialEq)]
struct CorpusIndexArgs {
    name: Option<String>,
    fresh: bool,
    include_data: bool,
    url: Option<String>,
    help: bool,
}

fn parse_corpus_index_args(args: &[String]) -> Result<CorpusIndexArgs, String> {
    let mut out = CorpusIndexArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                out.help = true;
                return Ok(out);
            }
            "--fresh" => out.fresh = true,
            // #1985: CSV/HDF/simulation output is skipped by default in a
            // reference corpus; this flag indexes it anyway.
            "--include-data" => out.include_data = true,
            "--url" => match args.get(i + 1) {
                Some(v) => {
                    out.url = Some(v.clone());
                    i += 1;
                }
                None => return Err("--url needs a value".into()),
            },
            a if a.starts_with('-') => return Err(format!("unknown flag '{a}'")),
            a if out.name.is_none() => out.name = Some(a.to_string()),
            a => return Err(format!("unexpected argument '{a}'")),
        }
        i += 1;
    }
    Ok(out)
}

fn run_corpus_index(args: &[String]) -> i32 {
    let parsed = match parse_corpus_index_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("xerj corpus index: {e}\n\n{CORPUS_USAGE}");
            return 2;
        }
    };
    if parsed.help {
        println!("{CORPUS_USAGE}");
        return 0;
    }
    let Some(name) = parsed.name else {
        eprintln!("xerj corpus index: <name> is required\n\n{CORPUS_USAGE}");
        return 2;
    };
    let fresh = parsed.fresh;
    let include_data = parsed.include_data;
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
    eprintln!("{}", corpus_index_duration_note());
    let url = resolve_url(parsed.url.as_deref());
    let es = match build_es(&url, None) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj corpus index: cannot reach {url}: {e:#}");
            return 2;
        }
    };
    corpus_index_flow(
        &es,
        &|prefix, state_dir| {
            crate::run_with_options(&dir, &url, prefix, state_dir, true, include_data)
        },
        &stamp_secs,
        &root,
        &url,
        &name,
        fresh,
        include_data,
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

/// One `autoindex-state/<corpus>/b<stamp>` directory left by a `corpus index`
/// that was killed before the ledger recorded the build.
#[derive(Debug, Clone)]
struct UnfinishedBuild {
    build: String,
    prefix: String,
    state_dir: PathBuf,
    /// A `sync_begin` the journal has not closed with `sync_commit`. That is
    /// the generation the index phase replays; autoindex does not hash the
    /// corpus again on that path.
    generation_pending: bool,
}

/// What a resumed build may be recorded as, once autoindex has returned and
/// the node has been counted and searched.
#[derive(Debug, PartialEq, Eq)]
enum ResumeVerdict {
    /// Clean autoindex exit, records present, search answered.
    Verified,
    /// Records present and searchable, but autoindex did not exit cleanly.
    /// The build is kept and marked salvaged so status explains the gap.
    Salvaged,
    /// Autoindex exited cleanly and records are present, but the store did
    /// not answer a search. Recorded as unverified, not as "no build".
    Unverified,
    /// Do not write a ledger. The journal stays the resume point. Writing a
    /// ledger with no build is what made a finished resume say "no verified
    /// build recorded".
    KeepJournal,
}

fn resume_verdict(rc: i32, docs: Option<u64>, queryable: bool) -> ResumeVerdict {
    let positive = matches!(docs, Some(n) if n > 0);
    let clean = rc == 0 || rc == 3;
    match (clean, positive, queryable) {
        (true, true, true) => ResumeVerdict::Verified,
        (false, true, true) => ResumeVerdict::Salvaged,
        (true, true, false) => ResumeVerdict::Unverified,
        _ => ResumeVerdict::KeepJournal,
    }
}

fn resume_build_note(name: &str, unfinished: &UnfinishedBuild) -> String {
    if unfinished.generation_pending {
        format!(
            "xerj corpus index: resuming build {} of '{name}' from its journal at {} — \
             indexing continues at the operations still uncommitted, and files already \
             applied are not hashed again",
            unfinished.build,
            unfinished.state_dir.display()
        )
    } else {
        format!(
            "xerj corpus index: resuming build {} of '{name}' from {} — the journal has no \
             generation in progress, so this run re-walks and re-hashes the corpus, then \
             reuses a sealed snapshot when one is already present",
            unfinished.build,
            unfinished.state_dir.display()
        )
    }
}

fn is_build_dir_name(name: &str) -> bool {
    let Some(stamp) = name.strip_prefix('b') else {
        return false;
    };
    !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit())
}

/// The build id of a stamped index (`xc-<corpus>-b<stamp>-<dataset>`), or
/// `None` for an unstamped legacy index.
fn index_build_id(name: &str, index: &str) -> Option<String> {
    let prefix = stamped_prefix(name, index)?;
    prefix
        .strip_prefix(&format!("xc-{name}-"))
        .map(str::to_string)
}

struct BuildJournalScan {
    runs: Vec<(String, String, String)>,
    generation_pending: bool,
}

impl BuildJournalScan {
    fn matches(&self, root: &str, url: &str, prefix: &str) -> bool {
        !self.runs.is_empty()
            && self
                .runs
                .iter()
                .all(|(recorded_root, recorded_url, recorded_prefix)| {
                    recorded_root == root && recorded_url == url && recorded_prefix == prefix
                })
    }
}

/// Read a build journal the way the resume journal does: a trailing line with
/// no newline is a torn record from a kill and is discarded. A corrupt line
/// that was newline-terminated is an error — resuming past it would guess.
fn read_build_journal(path: &Path) -> Result<BuildJournalScan, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut lines: Vec<&str> = text.lines().collect();
    if !text.ends_with('\n') {
        lines.pop();
    }
    let mut scan = BuildJournalScan {
        runs: Vec::new(),
        generation_pending: false,
    };
    for (i, line) in lines.into_iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|e| {
            format!(
                "journal line {} of {} is not valid JSON ({e})",
                i + 1,
                path.display()
            )
        })?;
        match value.get("kind").and_then(Value::as_str) {
            Some("run") => scan.runs.push((
                value
                    .get("root")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                value
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                value
                    .get("prefix")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )),
            Some("sync_begin") => scan.generation_pending = true,
            Some("sync_commit") => scan.generation_pending = false,
            _ => {}
        }
    }
    Ok(scan)
}

/// The root string autoindex writes into the journal: the canonical corpus
/// directory, or the path unchanged when canonicalization fails.
fn corpus_journal_root(corpus_dir: &Path) -> String {
    corpus_dir
        .canonicalize()
        .unwrap_or_else(|_| corpus_dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// An unfinished build to resume, or a refusal when more than one journal
/// could be the one a kill left behind.
fn find_unfinished_build(
    state_root: &Path,
    name: &str,
    corpus_dir: &Path,
    url: &str,
    live_indices: &[String],
) -> Result<Option<UnfinishedBuild>, String> {
    let parent = state_root.join(name);
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Ok(None);
    };
    let root_str = corpus_journal_root(corpus_dir);
    let mut matching: std::collections::BTreeMap<String, UnfinishedBuild> =
        std::collections::BTreeMap::new();
    // Build ids whose journal does not match this root/url/prefix, but which
    // the node is already serving. Falling through to legacy mode would
    // re-hash under another prefix and can retire those indices.
    let mut foreign_live: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().into_owned();
        if !is_build_dir_name(&fname) {
            continue;
        }
        let dir = entry.path();
        let journal = dir.join("journal.ndjson");
        if !journal.is_file() {
            continue;
        }
        let prefix = format!("xc-{name}-{fname}");
        let scan = read_build_journal(&journal).map_err(|reason| {
            format!(
                "xerj corpus index: build {fname} of '{name}' has a journal that cannot be \
                 resumed ({reason}). Nothing was changed. Repair the journal, or build a \
                 verified replacement with: xerj corpus index {name} --fresh"
            )
        })?;
        if scan.runs.is_empty() {
            continue;
        }
        if scan.matches(&root_str, url, &prefix) {
            matching.insert(
                fname.clone(),
                UnfinishedBuild {
                    build: fname,
                    prefix,
                    state_dir: dir,
                    generation_pending: scan.generation_pending,
                },
            );
        } else if live_indices
            .iter()
            .any(|index| index_build_id(name, index).as_deref() == Some(fname.as_str()))
        {
            let recorded_url = scan
                .runs
                .first()
                .map(|(_, recorded, _)| recorded.clone())
                .unwrap_or_else(|| "(none)".to_string());
            foreign_live.push((fname, recorded_url));
        }
    }
    let live: std::collections::BTreeSet<String> = live_indices
        .iter()
        .filter_map(|index| index_build_id(name, index))
        .collect();
    let pool: Vec<UnfinishedBuild> = if live.is_empty() {
        matching.into_values().collect()
    } else {
        matching
            .into_values()
            .filter(|build| live.contains(&build.build))
            .collect()
    };
    if pool.is_empty() {
        if foreign_live.is_empty() {
            return Ok(None);
        }
        foreign_live.sort();
        let listed = foreign_live
            .iter()
            .map(|(build, recorded_url)| format!("{build} (journal url {recorded_url})"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "xerj corpus index: {url} holds indices for unfinished build(s) of '{name}' whose \
             journal does not match this corpus directory or this url: {listed}. Refusing to \
             continue in legacy mode — that would re-hash the corpus under another prefix and \
             can retire this build. Nothing was changed. Re-run with the url the journal was \
             started against, or build a verified replacement with: xerj corpus index {name} \
             --fresh"
        ));
    }
    if pool.len() == 1 {
        return Ok(pool.into_iter().next());
    }
    let pending: Vec<UnfinishedBuild> = pool
        .iter()
        .filter(|build| build.generation_pending)
        .cloned()
        .collect();
    if pending.len() == 1 {
        return Ok(pending.into_iter().next());
    }
    let listed = pool
        .iter()
        .map(|build| build.build.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "xerj corpus index: '{name}' has unfinished builds {listed} and this run will not guess \
         which journal to resume. Nothing was changed. Re-run `xerj corpus index {name} --fresh` \
         to build a verified replacement beside them, or remove the state directory of the build \
         you do not want and re-run the same command."
    ))
}

/// Continue a build whose journal exists and whose ledger does not name it yet.
///
/// A kill during the index phase leaves `sync_begin` (and any committed
/// operations) in that journal and indices under `xc-<corpus>-b<stamp>`. The
/// ledger is written only after verification, so the next plain run used to
/// see "indices exist, no build recorded" and take the legacy arm: a new
/// prefix, no `--state-dir`, a full re-hash, and a ledger with no
/// `index_prefix` — the "no verified build recorded" notice.
fn resume_unfinished_build(
    node: &dyn CorpusNode,
    run_autoindex: &dyn Fn(&str, Option<&Path>) -> i32,
    root: &Path,
    url: &str,
    name: &str,
    unfinished: &UnfinishedBuild,
    include_data: bool,
) -> i32 {
    let build = &unfinished.build;
    let prefix = &unfinished.prefix;
    let state_dir = &unfinished.state_dir;
    println!("{}", resume_build_note(name, unfinished));
    let rc = run_autoindex(prefix, Some(state_dir));
    let docs = count_under(node, prefix);
    let queryable = queryable_store(node, name, prefix);
    let Some(state_dir_str) = state_dir.to_str() else {
        eprintln!(
            "xerj corpus index: build {build} resumed (autoindex exit {rc}) but its state \
             directory {} is not valid Unicode, so the ledger cannot record it. The journal is \
             unchanged.",
            state_dir.display()
        );
        return if rc == 0 || rc == 3 { 1 } else { rc };
    };
    let verdict = resume_verdict(rc, docs, queryable);
    let record = |salvaged: bool| {
        let _ = state::write_state(
            root,
            name,
            url,
            rc as i64,
            salvaged,
            Some(build),
            Some(prefix),
            Some(state_dir_str),
        );
    };
    match verdict {
        ResumeVerdict::Verified => {
            let n = docs.expect("verified verdict has a positive count");
            warn_short_of_pack_records(name, root, Some(n));
            record(false);
            report_leftovers(node, name, root, prefix);
            report(root, name, rc, &n.to_string(), include_data);
            0
        }
        ResumeVerdict::Salvaged => {
            let n = docs.expect("salvaged verdict has a positive count");
            eprintln!(
                "xerj corpus index: WARNING — autoindex exited {rc}, but resumed build {build} \
                 has {n} records and answers a search. Recording it as UNVERIFIED (salvaged)."
            );
            eprintln!(
                "xerj corpus index: coverage is not guaranteed. Re-run `xerj corpus index \
                 {name}` to resume the journal, or `xerj corpus index {name} --fresh` for a \
                 clean build."
            );
            record(true);
            report_leftovers(node, name, root, prefix);
            report(root, name, rc, &n.to_string(), include_data);
            0
        }
        ResumeVerdict::Unverified => {
            let n = docs.expect("unverified verdict has a positive count");
            eprintln!(
                "xerj corpus index: resumed build {build} finished (autoindex exit {rc}, {n} \
                 records) but the verification search failed. Recording it as UNVERIFIED."
            );
            eprintln!(
                "xerj corpus index: `xerj code` will say the index was not verified complete. \
                 Re-run `xerj corpus index {name}` to resume the journal at {}.",
                state_dir.display()
            );
            record(true);
            1
        }
        ResumeVerdict::KeepJournal => {
            let why = match docs {
                None => "the node did not answer a record count".to_string(),
                Some(0) => "the node reported 0 records".to_string(),
                Some(n) => format!(
                    "{n} records are present, but the store did not answer a search and autoindex \
                     exited {rc}"
                ),
            };
            eprintln!(
                "xerj corpus index: resumed build {build} is not verified ({why}; autoindex exit \
                 {rc})."
            );
            eprintln!(
                "xerj corpus index: nothing was recorded as a verified build, and no ledger \
                 without a build was written. The journal at {} is kept — re-run `xerj corpus \
                 index {name}` to resume it. A replacement beside it is `xerj corpus index \
                 {name} --fresh`.",
                state_dir.display()
            );
            if rc != 0 && rc != 3 {
                rc
            } else {
                1
            }
        }
    }
}

/// The build/verify/swap flow over an injected node, autoindex runner and
/// clock (#1004) — the destructive-operation contracts are pinned by the
/// tests below against a [`FakeNode`](tests::FakeNode), which is why none of
/// this touches [`Es`] directly. `run_autoindex(prefix, state_dir)` is the
/// real `crate::run_with_options` in production, with `--no-graph` set
/// (reference code needs ranked passages, not a relationship map) — and
/// `--fresh` is NEVER forwarded: this flow owns the swap.
#[allow(clippy::too_many_arguments)]
fn corpus_index_flow(
    node: &dyn CorpusNode,
    run_autoindex: &dyn Fn(&str, Option<&Path>) -> i32,
    now_secs: &dyn Fn() -> i64,
    root: &Path,
    url: &str,
    name: &str,
    fresh: bool,
    include_data: bool,
) -> i32 {
    let state_file = root.join("state").join(format!("{name}.json"));
    let state_root = root.join("autoindex-state");
    let old = state::load_state(root, name).ok();
    let old_build = old.as_ref().and_then(|s| s.build.clone());
    let old_index_prefix = old.as_ref().and_then(|s| s.index_prefix.clone());
    let old_state_dir = old.as_ref().and_then(|s| s.state_dir.clone());
    let old_url = old.as_ref().and_then(|s| s.url.clone());

    // Which run is this?
    // - build: --fresh, a first index, or a recorded build this node does not hold
    // - update: reconcile the recorded build in place
    // - resume: a build was killed before the ledger named it; continue its journal
    // - legacy: indexed before builds existed (no build journal on disk)
    let mut mode = "legacy";
    let (old_indices, _listed_ok) = corpus_indices(node, name, root);
    let corpus_dir = root.join("corpora").join(name);
    // Look only when the ledger does not already name a build. A recorded
    // build takes the update / #1214 refusal above; --fresh builds a new one.
    let unfinished = if fresh || old_build.is_some() {
        None
    } else {
        match find_unfinished_build(&state_root, name, &corpus_dir, url, &old_indices) {
            Ok(found) => found,
            Err(msg) => {
                eprintln!("{msg}");
                return 2;
            }
        }
    };
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
    } else if unfinished.is_some() {
        // Indices from the killed run must not select legacy mode. Legacy
        // opens a different prefix with no state dir, re-hashes the corpus,
        // and writes a ledger that names no build (#2239).
        mode = "resume";
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
            report(root, name, rc, &docs.to_string(), include_data);
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
            report(root, name, rc, &docs, include_data);
            0
        }
        "resume" => {
            let unfinished = unfinished
                .as_ref()
                .expect("resume mode is set only when an unfinished build was found");
            resume_unfinished_build(
                node,
                run_autoindex,
                root,
                url,
                name,
                unfinished,
                include_data,
            )
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
            report(root, name, rc, &docs_s, include_data);
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

struct Captured {
    stdout: String,
    stderr: String,
}

fn push_line(buf: &mut String, line: &str) {
    buf.push_str(line);
    buf.push('\n');
}

fn run_corpus_list(args: &[String]) -> i32 {
    let (code, text) = corpus_list_text(args);
    if !text.stdout.is_empty() {
        print!("{}", text.stdout);
    }
    if !text.stderr.is_empty() {
        eprint!("{}", text.stderr);
    }
    code
}

fn corpus_list_text(args: &[String]) -> (i32, Captured) {
    let mut url: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                let mut stdout = String::new();
                push_line(&mut stdout, CORPUS_USAGE);
                return (
                    0,
                    Captured {
                        stdout,
                        stderr: String::new(),
                    },
                );
            }
            "--url" => match args.get(i + 1) {
                Some(v) => {
                    url = Some(v.clone());
                    i += 1;
                }
                None => {
                    let mut stderr = String::new();
                    push_line(
                        &mut stderr,
                        &format!("xerj corpus list: --url needs a value\n\n{CORPUS_USAGE}"),
                    );
                    return (
                        2,
                        Captured {
                            stdout: String::new(),
                            stderr,
                        },
                    );
                }
            },
            a => {
                let mut stderr = String::new();
                push_line(
                    &mut stderr,
                    &format!("xerj corpus list: unexpected argument '{a}'\n\n{CORPUS_USAGE}"),
                );
                return (
                    2,
                    Captured {
                        stdout: String::new(),
                        stderr,
                    },
                );
            }
        }
        i += 1;
    }
    let root = code_root();
    let state_dir = root.join("state");
    if !state_dir.is_dir() {
        let mut stderr = String::new();
        push_line(
            &mut stderr,
            &format!(
                "no state directory at {} — nothing has been indexed on this machine",
                state_dir.display()
            ),
        );
        // The ledger is empty. A node that already has autoindex prefixes
        // should say so; a node that does not answer adds nothing.
        push_prefix_hint(&mut stderr, &resolve_url(url.as_deref()));
        return (
            2,
            Captured {
                stdout: String::new(),
                stderr,
            },
        );
    }
    let url = resolve_url(url.as_deref());
    let es = match build_es(&url, None) {
        Ok(es) => es,
        Err(e) => {
            let mut stderr = String::new();
            push_line(
                &mut stderr,
                &format!("xerj corpus list: cannot reach {url}: {e:#}"),
            );
            return (
                2,
                Captured {
                    stdout: String::new(),
                    stderr,
                },
            );
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
    let mut stdout = String::new();
    let mut loaded = 0usize;
    for name in &names {
        let st = match state::load_state(&root, name) {
            Ok(st) => st,
            Err(e) => {
                push_line(&mut stdout, &format!("{name}: unreadable state ({e})"));
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
        let own_note = if xccode::manifest::corpus_is_own(&root, name) {
            ", own"
        } else {
            ""
        };
        let uses = xccode::corpus_review_uses(&root, name);
        let use_note = uses
            .keys()
            .max()
            .and_then(|k| uses.get(k).map(|u| format!("\n  use: {u}")))
            .unwrap_or_default();
        match live {
            usize::MAX => push_line(&mut stdout, &format!("{name}  (indexed {indexed}{own_note}, prefix {prefix}) — server unreachable/ambiguous at {url}")),
            0 => {
                push_line(&mut stdout, &format!("{name}  (indexed {indexed}{own_note}, prefix {prefix}) — NOT loaded here (0 indices) — stale/other-server{use_note}"));
                // #1846: a name the index prefix rewrites (`curl_cffi`) was
                // indexed under another spelling, so "0 indices" is not the
                // whole story — say which name to move it to.
                if let Err(why) = xccode::pathgate::valid_corpus_name(name) {
                    push_line(&mut stdout, &format!("  {why}"));
                }
            }
            n => {
                loaded += 1;
                push_line(&mut stdout, &format!("{name}  (indexed {indexed}{own_note}, prefix {prefix}) — loaded — {n} index(es){use_note}"));
            }
        }
        if let Some(w) = state::incomplete_coverage(&st) {
            push_line(&mut stdout, &format!("  {w}"));
        }
    }
    stdout.push('\n');
    push_line(
        &mut stdout,
        &format!(
            "{} of {} corpora are actually loaded on {url}.",
            loaded,
            names.len()
        ),
    );
    let mut stderr = String::new();
    if names.is_empty() {
        push_prefix_hint(&mut stderr, &url);
    }
    (0, Captured { stdout, stderr })
}

// ── dispatch ────────────────────────────────────────────────────────────────

/// `xerj corpus <add|build|index|list> …` — returns the process exit code.
fn corpus_verb(arg: &str) -> bool {
    matches!(
        arg,
        "add" | "build" | "sign" | "keygen" | "index" | "list" | "own" | "help" | "-h" | "--help"
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
        "own" => run_corpus_own(&rest),
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

    /// #2453: a clone killed partway leaves `.git` behind with no HEAD, so
    /// `corpus add` skipped the empty directory as "already cloned" forever.
    #[test]
    fn an_interrupted_clone_is_not_a_reusable_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("repo");
        std::fs::create_dir_all(target.join(".git")).unwrap();
        assert!(!existing_checkout_is_complete(&target));
    }

    #[test]
    fn a_missing_checkout_is_not_a_reusable_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!existing_checkout_is_complete(&dir.path().join("repo")));
    }

    #[test]
    fn a_finished_clone_is_a_reusable_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("repo");
        std::fs::create_dir_all(&target).unwrap();
        let run = |args: &[&str]| {
            let (rc, out) = git(Some(&target), args).unwrap();
            assert_eq!(rc, 0, "git {args:?} failed: {out}");
        };
        run(&["init", "--quiet"]);
        std::fs::write(target.join("README.md"), "hello\n").unwrap();
        run(&["add", "README.md"]);
        run(&[
            "-c",
            "user.name=xerj-test",
            "-c",
            "user.email=xerj-test@example.com",
            "commit",
            "--quiet",
            "-m",
            "init",
        ]);
        assert!(existing_checkout_is_complete(&target));
    }

    #[test]
    fn git_fetch_failure_names_tls_and_a_stale_pin() {
        let tls = git_fetch_failure(
            "abc",
            "fatal: unable to access 'https://example.com/': GnuTLS recv error (-110): The TLS connection was non-properly terminated.",
        );
        assert!(tls.contains("git fetch abc failed:"), "{tls}");
        assert!(tls.contains("GnuTLS"), "{tls}");
        assert!(tls.contains("HTTPS_PROXY"), "{tls}");
        assert!(tls.contains("GIT_SSL_CAINFO"), "{tls}");

        let pin = git_fetch_failure(
            "6f4f07681f45",
            "fatal: remote error: upload-pack: not our ref 6f4f07681f45",
        );
        assert!(pin.contains("not our ref"), "{pin}");
        assert!(pin.contains("pinned commit is not on the remote"), "{pin}");
        assert!(!pin.contains("HTTPS_PROXY"), "{pin}");

        let bare = git_fetch_failure("abc", "   ");
        assert_eq!(bare, "git fetch abc failed");

        let long = git_fetch_failure("abc", &"x".repeat(800));
        assert!(long.chars().filter(|c| *c == 'x').count() <= 500, "{long}");
    }

    #[test]
    fn a_flag_before_the_corpus_verb_is_not_an_unknown_subcommand() {
        // Before the hoist, `--url` was the verb and this returned 2
        // ("unknown subcommand"). `--help` is the verb; it must run.
        let code = run_corpus_cli(&["--url".into(), "http://127.0.0.1:9".into(), "--help".into()]);
        assert_eq!(code, 0);
    }

    /// #1985: `--include-data` is the opt-out of the corpus data-file gate.
    /// Parsing is separated from the run so this needs no node.
    #[test]
    fn corpus_index_parses_include_data() {
        let p = parse_corpus_index_args(&["--include-data".into(), "repo".into()]).unwrap();
        assert!(p.include_data && !p.fresh && !p.help);
        assert_eq!(p.name.as_deref(), Some("repo"));

        let p = parse_corpus_index_args(&[
            "--fresh".into(),
            "repo".into(),
            "--url".into(),
            "http://127.0.0.1:9200".into(),
        ])
        .unwrap();
        assert!(p.fresh && !p.include_data);
        assert_eq!(p.url.as_deref(), Some("http://127.0.0.1:9200"));

        assert!(parse_corpus_index_args(&["--nope".into()]).is_err());
        assert!(parse_corpus_index_args(&["repo".into(), "extra".into()]).is_err());
        assert!(parse_corpus_index_args(&["--url".into()]).is_err());
        assert!(parse_corpus_index_args(&["--help".into()]).unwrap().help);
    }

    #[test]
    fn corpus_index_says_a_tool_timeout_is_not_a_failed_index() {
        let note = corpus_index_duration_note();
        assert!(note.contains("tool timeout"), "{note}");
        assert!(note.contains("re-run"), "{note}");
        assert!(note.contains("xerj-done"), "{note}");
        assert!(
            CORPUS_USAGE.contains("tool timeout"),
            "the usage line must point at the same warning"
        );
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
        assert!(
            CODE_USAGE.contains("Default ranking is lexical")
                && CODE_USAGE.contains("not a neural reading")
                && CODE_USAGE.contains("--embed-mode neural"),
            "{CODE_USAGE}"
        );
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
            corpus_index_flow(node, &auto.runner(), clock, root, url, name, fresh, false)
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

    /// The journal a `corpus index` kill leaves under `autoindex-state/<name>/<build>`.
    /// `after_run` is appended after the `run` record (include trailing newlines).
    /// `torn` appends a partial last line with no newline — the kill's in-flight record.
    fn write_killed_journal(
        root: &Path,
        name: &str,
        build: &str,
        url: &str,
        after_run: &str,
        torn: bool,
    ) -> PathBuf {
        let corpus = root.join("corpora").join(name);
        let root_str = corpus
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let prefix = format!("xc-{name}-{build}");
        let dir = root.join("autoindex-state").join(name).join(build);
        std::fs::create_dir_all(&dir).unwrap();
        let run = serde_json::json!({
            "v": 1,
            "kind": "run",
            "root": root_str,
            "url": url,
            "prefix": prefix,
            "bulk_timeout_secs": 300,
            "run_id": format!("run-{build}"),
            "started": "2026-10-22T00:00:00Z"
        });
        let mut text = format!("{run}\n{after_run}");
        if torn {
            text.push_str("{\"kind\":\"sync_operation_state\",\"partial\":");
        }
        std::fs::write(dir.join("journal.ndjson"), text).unwrap();
        dir
    }

    #[test]
    fn resume_verdict_records_a_clean_build_and_explains_the_rest() {
        use super::ResumeVerdict::*;
        assert_eq!(resume_verdict(0, Some(10), true), Verified);
        assert_eq!(resume_verdict(3, Some(10), true), Verified);
        assert_eq!(resume_verdict(1, Some(10), true), Salvaged);
        assert_eq!(resume_verdict(0, Some(10), false), Unverified);
        // A zero or unknown count must not become a ledger with no build.
        assert_eq!(resume_verdict(0, Some(0), true), KeepJournal);
        assert_eq!(resume_verdict(0, None, true), KeepJournal);
        assert_eq!(resume_verdict(1, Some(0), false), KeepJournal);
        assert_eq!(resume_verdict(1, None, false), KeepJournal);
    }

    #[test]
    fn resume_note_claims_no_rehash_only_when_a_generation_is_pending() {
        let pending = UnfinishedBuild {
            build: "b1".into(),
            prefix: "xc-kv-b1".into(),
            state_dir: PathBuf::from("/tmp/state"),
            generation_pending: true,
        };
        let note = resume_build_note("kv", &pending);
        assert!(note.contains("journal"), "{note}");
        assert!(note.contains("not hashed again"), "{note}");
        assert!(note.contains("uncommitted"), "{note}");

        let sealed = UnfinishedBuild {
            generation_pending: false,
            ..pending
        };
        let note = resume_build_note("kv", &sealed);
        assert!(
            note.contains("re-hashes"),
            "a journal with no in-progress generation must not claim the hash is skipped: {note}"
        );
        assert!(!note.contains("not hashed again"), "{note}");
    }

    /// #2239: killed mid-index. The ledger was never written (it is written
    /// only after verification). The journal holds `sync_begin` and the node
    /// already has this build's indices. The retry must hand that journal
    /// back to autoindex — which replays uncommitted operations and does not
    /// re-enter snapshot/hash — and a resume that then completes must record
    /// the verified build. The old path took legacy mode (`xc-kv`, no state
    /// dir) and wrote a ledger with no `index_prefix`.
    #[test]
    fn a_kill_mid_index_resumes_the_journal_and_records_a_verified_build() {
        let root = corpus_root("kv");
        let state_dir = write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            true,
        );
        let node = FakeNode::new();
        // What the killed run had already published.
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        node.seed_count("xc-kv-b1-*", Count::Number(500));

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        assert_eq!(auto.prefixes(), vec!["xc-kv-b1".to_string()]);
        let (_prefix, sd) = &auto.calls.borrow()[0];
        assert_eq!(
            sd.as_deref(),
            Some(state_dir.as_path()),
            "resume must pass the killed run's state dir, not start a new journal"
        );
        assert!(
            node.ops
                .borrow()
                .iter()
                .all(|op| !op.starts_with("delete:")),
            "a resume does not retire the indices the killed run already wrote"
        );
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.build.as_deref(), Some("b1"));
        assert_eq!(st.index_prefix.as_deref(), Some("xc-kv-b1"));
        assert_eq!(st.state_dir.as_deref(), state_dir.to_str());
        assert_eq!(st.salvaged, Some(false));
        assert_eq!(st.autoindex_exit, Some(serde_json::json!(0)));
        assert!(
            state::incomplete_coverage(&st).is_none(),
            "a clean resume is a verified build"
        );
        // The torn tail must not have hidden sync_begin: that flag is what
        // tells the resume it can continue mid-index instead of re-hashing.
        let scan = read_build_journal(&state_dir.join("journal.ndjson")).unwrap();
        assert!(
            scan.generation_pending,
            "a kill's torn last line is discarded; sync_begin still means the index phase"
        );
        assert!(scan.matches(
            &corpus_journal_root(&root.join("corpora/kv")),
            "http://x",
            "xc-kv-b1"
        ));
    }

    /// Same kill, but the store answers a count and not a search. The build
    /// is recorded so status can explain why it is unverified, instead of
    /// widening to "no verified build recorded".
    #[test]
    fn a_resumed_build_the_node_cannot_search_is_recorded_unverified() {
        let root = corpus_root("kv");
        let state_dir = write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let node = FakeNode::new();
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        node.seed_count("xc-kv-b1-*", Count::Number(500));
        node.seed_search(
            "xc-kv-b1-*",
            Err("HTTP 500: store_exception: Segment ea12 not found".into()),
        );

        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 1, "a failed verification search is not success");
        assert_eq!(auto.prefixes(), vec!["xc-kv-b1".to_string()]);
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some("xc-kv-b1"));
        assert_eq!(st.build.as_deref(), Some("b1"));
        assert_eq!(st.salvaged, Some(true));
        assert_eq!(st.state_dir.as_deref(), state_dir.to_str());
        let warning = state::incomplete_coverage(&st).expect("unverified status");
        assert!(
            warning.contains("NOT verified complete") && warning.contains("kept unverified"),
            "{warning}"
        );
        assert!(node.live("xc-kv-b1-000"));
        assert!(node
            .ops
            .borrow()
            .iter()
            .all(|op| !op.starts_with("delete:")));
    }

    /// Autoindex itself failed and the node has nothing new to stand on.
    /// Do not write the legacy ledger (no build) that the notice complains about.
    #[test]
    fn a_failed_resume_keeps_the_journal_and_writes_no_buildless_ledger() {
        let root = corpus_root("kv");
        let state_dir = write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let node = FakeNode::new();
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        node.seed_count("xc-kv-b1-*", Count::Zero);

        let auto = FakeAuto::with_rc(&node, 1);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 1);
        assert_eq!(auto.prefixes(), vec!["xc-kv-b1".to_string()]);
        assert_eq!(
            auto.calls.borrow()[0].1.as_deref(),
            Some(state_dir.as_path())
        );
        assert!(
            state::load_state(&root, "kv").is_err(),
            "a failed resume must not write a ledger that names no build"
        );
        assert!(
            state_dir.join("journal.ndjson").is_file(),
            "the journal remains the next resume point"
        );
    }

    /// Killed during snapshot, before any index existed: still the same state
    /// dir, not a freshly minted build that abandons the sealed snapshot.
    #[test]
    fn a_kill_before_any_index_resumes_the_same_build_directory() {
        let root = corpus_root("kv");
        let state_dir = write_killed_journal(&root, "kv", "b9", "http://x", "", false);
        let node = FakeNode::new();
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        assert_eq!(auto.prefixes(), vec!["xc-kv-b9".to_string()]);
        assert_eq!(
            auto.calls.borrow()[0].1.as_deref(),
            Some(state_dir.as_path())
        );
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some("xc-kv-b9"));
        assert_eq!(st.salvaged, Some(false));
        let note = resume_build_note(
            "kv",
            &UnfinishedBuild {
                build: "b9".into(),
                prefix: "xc-kv-b9".into(),
                state_dir: state_dir.clone(),
                generation_pending: false,
            },
        );
        assert!(note.contains("re-hashes"), "{note}");
    }

    #[test]
    fn a_first_index_with_no_journal_still_mints_a_build() {
        let root = corpus_root("kv");
        let node = FakeNode::new();
        let auto = FakeAuto::ok(&node);
        assert_eq!(run_flow(&node, &auto, &|| T0, &root, "kv", false), 0);
        assert_eq!(auto.prefixes(), vec![format!("xc-kv-b{T0}")]);
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(
            st.index_prefix.as_deref(),
            Some(format!("xc-kv-b{T0}").as_str())
        );
        assert_eq!(st.salvaged, Some(false));
    }

    #[test]
    fn fresh_ignores_an_unfinished_journal_and_builds_a_new_one() {
        let root = corpus_root("kv");
        write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let node = FakeNode::new();
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        node.seed_count("xc-kv-b1-*", Count::Number(500));
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", true);

        assert_eq!(rc, 0);
        assert_eq!(auto.prefixes(), vec![format!("xc-kv-b{T0}")]);
        let st = state::load_state(&root, "kv").unwrap();
        assert_eq!(
            st.index_prefix.as_deref(),
            Some(format!("xc-kv-b{T0}").as_str())
        );
        assert!(!node.live("xc-kv-b1-000"), "fresh retires the killed build");
    }

    #[test]
    fn two_pending_journals_are_refused_instead_of_guessing() {
        let root = corpus_root("kv");
        write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        write_killed_journal(
            &root,
            "kv",
            "b2",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let node = FakeNode::new();
        node.indices
            .borrow_mut()
            .extend(["xc-kv-b1-000".into(), "xc-kv-b2-000".into()]);
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 2);
        assert!(auto.calls.borrow().is_empty());
        assert!(node
            .ops
            .borrow()
            .iter()
            .all(|op| !op.starts_with("delete:")));
        assert!(state::load_state(&root, "kv").is_err());
    }

    #[test]
    fn a_single_pending_journal_wins_over_another_finished_attempt() {
        let root = corpus_root("kv");
        let pending = write_killed_journal(
            &root,
            "kv",
            "b2",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n{\"kind\":\"sync_commit\"}\n",
            false,
        );
        let node = FakeNode::new();
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        assert_eq!(auto.prefixes(), vec!["xc-kv-b2".to_string()]);
        assert_eq!(auto.calls.borrow()[0].1.as_deref(), Some(pending.as_path()));
    }

    #[test]
    fn live_indices_select_the_build_that_was_being_published() {
        let root = corpus_root("kv");
        write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let live = write_killed_journal(
            &root,
            "kv",
            "b2",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let node = FakeNode::new();
        node.indices.borrow_mut().push("xc-kv-b2-000".into());
        node.seed_count("xc-kv-b2-*", Count::Number(40));
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 0);
        assert_eq!(auto.prefixes(), vec!["xc-kv-b2".to_string()]);
        assert_eq!(auto.calls.borrow()[0].1.as_deref(), Some(live.as_path()));
        assert!(node.live("xc-kv-b2-000"));
    }

    #[test]
    fn a_journal_for_another_url_is_not_resumed_over_its_live_indices() {
        let root = corpus_root("kv");
        write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://elsewhere",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        let node = FakeNode::new();
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        node.seed_count("xc-kv-b1-*", Count::Number(500));
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);

        assert_eq!(rc, 2, "legacy mode would re-hash and can retire this build");
        assert!(auto.calls.borrow().is_empty());
        assert!(node.live("xc-kv-b1-000"));
        assert!(node
            .ops
            .borrow()
            .iter()
            .all(|op| !op.starts_with("delete:")));
    }

    #[test]
    fn a_corrupt_build_journal_is_refused() {
        let root = corpus_root("kv");
        let dir = write_killed_journal(
            &root,
            "kv",
            "b1",
            "http://x",
            "{\"kind\":\"sync_begin\"}\n",
            false,
        );
        std::fs::write(
            dir.join("journal.ndjson"),
            "{\"kind\":\"run\"}\n{not json}\n",
        )
        .unwrap();
        let node = FakeNode::new();
        node.indices.borrow_mut().push("xc-kv-b1-000".into());
        let auto = FakeAuto::ok(&node);
        let rc = run_flow(&node, &auto, &|| T0, &root, "kv", false);
        assert_eq!(rc, 2);
        assert!(auto.calls.borrow().is_empty());
        assert!(node
            .ops
            .borrow()
            .iter()
            .all(|op| !op.starts_with("delete:")));
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
                paths: Vec::new(),
            }],
            None,
            false,
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

    // ── corpus add of a local checkout's .xerjignore (#2199) ───────────────

    fn init_committed_repo(dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        let (rc, out) = git(Some(dir), &["init", "--quiet", "-b", "main"]).unwrap();
        assert_eq!(rc, 0, "git init: {out}");
        for (name, body) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, body).unwrap();
            let (rc, out) = git(Some(dir), &["add", "--", name]).unwrap();
            assert_eq!(rc, 0, "git add {name}: {out}");
        }
        let (rc, out) = git(
            Some(dir),
            &[
                "-c",
                "user.email=xerj-test@example.com",
                "-c",
                "user.name=xerj-test",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        )
        .unwrap();
        assert_eq!(rc, 0, "git commit: {out}");
    }

    fn head_sha(dir: &Path) -> String {
        let (rc, sha) = git(Some(dir), &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(rc, 0, "{sha}");
        assert_eq!(sha.len(), 40, "{sha}");
        sha
    }

    // ── corpus add over a clone killed partway (#2453) ─────────────────────

    /// Leaves `checkout` as a `git checkout` stopped partway does: no index,
    /// with `missing` not yet written. (SIGTERM lets git remove its
    /// `.git/index.lock`; SIGKILL leaves it, see the refusal test.)
    fn kill_mid_checkout(checkout: &Path, missing: &str) {
        std::fs::remove_file(checkout.join(missing)).unwrap();
        std::fs::remove_file(checkout.join(".git/index")).unwrap();
    }

    /// An unborn checkout is preserved, then a move-aside recovery succeeds.
    #[test]
    fn corpus_add_preserves_an_unborn_checkout_before_manual_recovery() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let clone = home.path().join("corpora/partial/widget");
        std::fs::create_dir_all(&clone).unwrap();
        let (rc, out) = git(Some(&clone), &["init", "--quiet"]).unwrap();
        assert_eq!(rc, 0, "git init: {out}");

        assert_eq!(
            run_corpus_add(&["partial".into(), origin.display().to_string()]),
            1
        );
        let backup = preserve_incomplete_for_retry(&clone);
        assert!(backup.join(".git/HEAD").is_file());
        assert_eq!(
            run_corpus_add(&["partial".into(), origin.display().to_string()]),
            0
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("keep.txt")).unwrap(),
            "tracked\n"
        );
        let recorded =
            manifest::read_corpus_manifest(&home.path().join("corpora/partial/corpus.json"))
                .unwrap();
        assert_eq!(recorded.repos[0].sha, head_sha(&origin));
        assert_eq!(recorded.repos[0].files, Some(1));
    }

    /// Missing object storage does not prove an existing directory is disposable.
    #[test]
    fn corpus_add_preserves_a_checkout_without_git_objects() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let clone = home.path().join("corpora/partial/widget");
        std::fs::create_dir_all(&clone).unwrap();
        let (rc, out) = git(Some(&clone), &["init", "--quiet"]).unwrap();
        assert_eq!(rc, 0, "git init: {out}");
        std::fs::remove_dir_all(clone.join(".git/objects")).unwrap();
        std::fs::write(clone.join(".git/config.lock"), "").unwrap();

        assert_eq!(
            run_corpus_add(&["partial".into(), origin.display().to_string()]),
            1
        );
        let backup = preserve_incomplete_for_retry(&clone);
        assert!(backup.join(".git/HEAD").is_file());
        assert_eq!(
            run_corpus_add(&["partial".into(), origin.display().to_string()]),
            0
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("keep.txt")).unwrap(),
            "tracked\n"
        );
    }

    /// A finished checkout is reused, but a missing index is preserved and
    /// refused even when the remote is offline. Recovery keeps local notes.
    #[test]
    fn corpus_add_preserves_an_ambiguous_checkout_when_remote_is_offline() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let add = || run_corpus_add(&["mid".into(), origin.display().to_string()]);
        let clone = home.path().join("corpora/mid/widget");
        assert_eq!(add(), 0);

        std::fs::write(clone.join("marker"), "kept\n").unwrap();
        assert_eq!(add(), 0);
        assert!(
            clone.join("marker").exists(),
            "a finished clone is not touched"
        );

        kill_mid_checkout(&clone, "b.txt");
        std::fs::rename(&origin, home.path().join("gone")).unwrap();
        assert_eq!(
            add(),
            1,
            "missing index is ambiguous even when the remote is unavailable"
        );
        assert!(!clone.join("b.txt").exists());
        assert_eq!(
            std::fs::read_to_string(clone.join("marker")).unwrap(),
            "kept\n"
        );
        let backup = preserve_incomplete_for_retry(&clone);
        std::fs::rename(home.path().join("gone"), &origin).unwrap();
        assert_eq!(add(), 0);
        assert_eq!(std::fs::read_to_string(clone.join("b.txt")).unwrap(), "b\n");
        assert!(
            backup.join("marker").exists(),
            "local files remain in the saved directory"
        );
        let recorded =
            manifest::read_corpus_manifest(&home.path().join("corpora/mid/corpus.json")).unwrap();
        // Only the committed a.txt and b.txt are in the new checkout.
        assert_eq!(recorded.repos[0].files, Some(2));
    }

    /// A leftover `.git/index.lock` may belong to a git that is still
    /// running, so the run fails and touches nothing. Removing a lock alone
    /// does not authorize overwriting files when the index is still absent.
    ///
    /// Unix is the exception (#2520): this run holds the corpus lock, so a git
    /// started by a previous `corpus add` would still hold it, and a lock in a
    /// checkout that never finished cannot have a live owner. It is removed,
    /// and the run then fails for the missing index like any other unfinished
    /// checkout.
    #[test]
    fn corpus_add_refuses_a_checkout_left_with_index_lock() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let add = || run_corpus_add(&["locked".into(), origin.display().to_string()]);
        let clone = home.path().join("corpora/locked/widget");
        assert_eq!(add(), 0);

        kill_mid_checkout(&clone, "b.txt");
        std::fs::write(clone.join(".git/index.lock"), "").unwrap();
        assert_eq!(add(), 1);
        #[cfg(unix)]
        assert!(
            !clone.join(".git/index.lock").exists(),
            "a lock in a checkout that never finished has no live owner"
        );
        #[cfg(not(unix))]
        assert!(
            clone.join(".git/index.lock").exists(),
            "the lock is not deleted"
        );
        assert!(!clone.join("b.txt").exists());

        if clone.join(".git/index.lock").exists() {
            std::fs::remove_file(clone.join(".git/index.lock")).unwrap();
        }
        assert_eq!(
            add(),
            1,
            "removing a lock does not authorize resetting local files"
        );
        let backup = preserve_incomplete_for_retry(&clone);
        assert!(backup.join("a.txt").exists());
        assert_eq!(add(), 0);
        assert_eq!(std::fs::read_to_string(clone.join("b.txt")).unwrap(), "b\n");
    }

    /// #2520: a complete checkout is not provably abandoned, so its
    /// `.git/index.lock` may belong to a git this run did not start. It is
    /// kept and the run is refused.
    #[test]
    fn corpus_add_keeps_an_index_lock_in_a_complete_checkout() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("a.txt", "a\n")]);
        let add = || run_corpus_add(&["busy".into(), origin.display().to_string()]);
        let clone = home.path().join("corpora/busy/widget");
        assert_eq!(add(), 0);

        std::fs::write(clone.join(".git/index.lock"), "").unwrap();
        assert_eq!(add(), 1);
        assert!(
            clone.join(".git/index.lock").exists(),
            "a lock in a finished checkout is left for its owner"
        );
    }

    /// #2520: the corpus lock is inherited by the git a run starts, so it
    /// outlives a killed `xerj` for as long as that git lives.
    #[cfg(unix)]
    #[test]
    fn a_git_started_by_corpus_add_keeps_the_corpus_lock() {
        let home = tempfile::tempdir().unwrap();
        let lock = lock_corpus_add(home.path(), "held").unwrap();
        let handoff = GitLockHandoff::new(&lock);
        // `hash-object --stdin` blocks until its stdin closes, which this test
        // never does, so a real git holds the lock until it is killed.
        let mut child = {
            let mut c = Command::new("git");
            c.args(["hash-object", "--stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            inherit_corpus_lock(&mut c);
            c.spawn().unwrap()
        };
        drop(handoff);
        drop(lock);
        // The child holds the descriptor from `fork` on, so the refusal is
        // immediate; the bounded retry only tolerates a loaded machine.
        let mut err = String::new();
        for _ in 0..100 {
            match lock_corpus_add(home.path(), "held") {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(e) => {
                    err = format!("{e:#}");
                    break;
                }
            }
        }
        assert!(
            err.contains("still running"),
            "a running git still holds the corpus lock: {err:?}"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        drop(lock_corpus_add(home.path(), "held").expect("the lock is free once the git exits"));
    }

    /// #2520: a fsmonitor daemon outlives the git that started it and would
    /// keep the inherited lock for its own timeout, so git runs with it off.
    #[cfg(unix)]
    #[test]
    fn a_git_started_by_corpus_add_disables_fsmonitor() {
        let home = tempfile::tempdir().unwrap();
        let lock = lock_corpus_add(home.path(), "fm").unwrap();
        let handoff = GitLockHandoff::new(&lock);
        let mut c = Command::new("git");
        inherit_corpus_lock(&mut c);
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        drop(handoff);
        drop(lock);
        assert_eq!(args, ["-c", "core.fsmonitor=false"]);
        let mut c = Command::new("git");
        inherit_corpus_lock(&mut c);
        assert_eq!(c.get_args().count(), 0, "no handoff, no extra arguments");
    }

    /// When the files cannot be restored the run fails and the checkout,
    /// with every object in it, is kept for the next try.
    #[cfg(unix)]
    #[test]
    fn corpus_add_keeps_a_checkout_it_cannot_restore() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let add = || run_corpus_add(&["stuck".into(), origin.display().to_string()]);
        let clone = home.path().join("corpora/stuck/widget");
        assert_eq!(add(), 0);
        kill_mid_checkout(&clone, "b.txt");
        assert_eq!(add(), 1);
        assert!(clone.join(".git").is_dir(), "the checkout is kept");
        assert_eq!(std::fs::read_to_string(clone.join("a.txt")).unwrap(), "a\n");
        assert!(!clone.join("b.txt").exists());
    }

    /// A pinned checkout without HEAD is kept, not silently republished.
    /// Moving it aside permits a fresh checkout at the declared pin.
    #[test]
    fn corpus_add_from_preserves_a_pin_without_head_before_recovery() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[("LICENSE", "bsd\n"), ("src/a.rs", "fn a() {}\n")],
        );
        let manifest = home.path().join("pin.json");
        write_pin(
            &manifest,
            &origin.display().to_string(),
            &head_sha(&origin),
            None,
        );
        let add = || run_corpus_add(&["--from".into(), manifest.display().to_string()]);
        let checkout = home.path().join("corpora/sparse-demo/vulndb");
        assert_eq!(add(), 0);

        kill_mid_checkout(&checkout, "src/a.rs");
        std::fs::write(checkout.join(".git/HEAD"), "ref: refs/heads/unborn\n").unwrap();
        assert_eq!(add(), 1);
        assert!(checkout.join("LICENSE").is_file());
        let backup = preserve_incomplete_for_retry(&checkout);
        assert!(backup.join("LICENSE").is_file());
        assert_eq!(add(), 0);
        assert_eq!(
            worktree_files(&checkout),
            vec!["LICENSE".to_string(), "src/a.rs".to_string()]
        );
        assert!(!checkout.join(".git/index.lock").exists());
    }

    /// HEAD at the pin does not make a missing index safe to reset.
    #[test]
    fn corpus_add_from_preserves_a_pin_without_index_before_recovery() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[("LICENSE", "bsd\n"), ("src/a.rs", "fn a() {}\n")],
        );
        let manifest = home.path().join("pin.json");
        write_pin(
            &manifest,
            &origin.display().to_string(),
            &head_sha(&origin),
            None,
        );
        let add = || run_corpus_add(&["--from".into(), manifest.display().to_string()]);
        let checkout = home.path().join("corpora/sparse-demo/vulndb");
        assert_eq!(add(), 0);

        kill_mid_checkout(&checkout, "src/a.rs");
        assert_eq!(add(), 1);
        assert!(checkout.join("LICENSE").is_file());
        let backup = preserve_incomplete_for_retry(&checkout);
        assert!(backup.join("LICENSE").is_file());
        assert_eq!(add(), 0);
        assert_eq!(
            worktree_files(&checkout),
            vec!["LICENSE".to_string(), "src/a.rs".to_string()]
        );
    }

    /// A checkout git cannot read at all (exit 128, not "no commit") is not
    /// proof of a killed clone: the run fails and leaves it exactly as it is,
    /// instead of recording it as already cloned. Moving it aside, as the
    /// diagnostic advises, permits a new clone while retaining local files.
    #[test]
    fn corpus_add_fails_on_a_checkout_git_cannot_read() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let add = || run_corpus_add(&["odd".into(), origin.display().to_string()]);
        let clone = home.path().join("corpora/odd/widget");
        assert_eq!(add(), 0);
        std::fs::write(clone.join(".git/HEAD"), "garbage\n").unwrap();
        std::fs::write(clone.join("marker"), "kept\n").unwrap();
        assert_eq!(add(), 1);
        assert!(
            clone.join("marker").exists(),
            "an unreadable checkout is not deleted"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join(".git/HEAD")).unwrap(),
            "garbage\n"
        );

        // The recovery keeps the old directory and all local files.
        let backup = preserve_incomplete_for_retry(&clone);
        assert!(backup.join("marker").exists());
        assert_eq!(add(), 0);
        assert_eq!(
            std::fs::read_to_string(clone.join("keep.txt")).unwrap(),
            "tracked\n"
        );
    }

    /// A finished clone that lost its HEAD commit object still has its
    /// files and index: git cannot read it, so the run fails and keeps it
    /// instead of deleting it as a clone that never got a commit.
    #[test]
    fn corpus_add_fails_on_a_clone_missing_its_head_commit() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let add = || run_corpus_add(&["lost".into(), origin.display().to_string()]);
        let clone = home.path().join("corpora/lost/widget");
        assert_eq!(add(), 0);
        std::fs::remove_dir_all(clone.join(".git/objects")).unwrap();
        std::fs::create_dir(clone.join(".git/objects")).unwrap();
        assert_eq!(add(), 1);
        assert_eq!(
            std::fs::read_to_string(clone.join("keep.txt")).unwrap(),
            "tracked\n"
        );
    }

    /// A `.git` file (a linked worktree) has no `.git/index` of its own and
    /// is never ours: it is left alone, not taken for an unfinished clone.
    #[test]
    fn corpus_add_leaves_a_linked_worktree_alone() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let clone = home.path().join("corpora/linked/widget");
        let clone_s = clone.display().to_string();
        let (rc, out) = git(Some(&origin), &["worktree", "add", "--quiet", &clone_s]).unwrap();
        assert_eq!(rc, 0, "git worktree add: {out}");
        std::fs::write(clone.join("keep.txt"), "edited\n").unwrap();

        assert_eq!(
            run_corpus_add(&["linked".into(), origin.display().to_string()]),
            0
        );
        assert_eq!(
            std::fs::read_to_string(clone.join("keep.txt")).unwrap(),
            "edited\n"
        );
        assert!(clone.join(".git").is_file());
    }

    /// A second `corpus add` of the same corpus refuses while the first holds
    /// the lock, and touches nothing.
    #[test]
    fn corpus_add_refuses_while_another_run_holds_the_corpus() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let held = lock_corpus_add(home.path(), "busy").unwrap();
        assert_eq!(
            run_corpus_add(&["busy".into(), origin.display().to_string()]),
            1
        );
        assert!(!home.path().join("corpora/busy/widget").exists());
        drop(held);
        assert_eq!(
            run_corpus_add(&["busy".into(), origin.display().to_string()]),
            0
        );
    }

    /// `corpus add --from <pack>` takes the same lock.
    #[test]
    fn corpus_add_from_pack_refuses_while_another_run_holds_the_corpus() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let pack = build_demo_pack(home.path());
        let add = || run_corpus_add(&["--from".to_string(), pack.display().to_string()]);
        let held = lock_corpus_add(home.path(), "demo").unwrap();
        assert_eq!(add(), 1);
        assert!(!home.path().join("corpora/demo").exists());
        drop(held);
        assert_eq!(add(), 0);
    }

    #[test]
    fn local_checkout_root_is_a_directory_or_a_file_url() {
        assert!(local_checkout_root("https://github.com/xerj-org/xerj").is_none());
        assert!(local_checkout_root("ssh://git@github.com/xerj-org/xerj.git").is_none());
        assert!(local_checkout_root("git@github.com:xerj-org/xerj.git").is_none());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().display().to_string();
        assert_eq!(local_checkout_root(&path).as_deref(), Some(dir.path()));
        assert_eq!(
            local_checkout_root(&format!("file://{path}")).as_deref(),
            Some(dir.path())
        );
        assert_eq!(
            local_checkout_root(&format!("file://localhost{path}")).as_deref(),
            Some(dir.path())
        );
        assert!(local_checkout_root("file://localhost/no/such/xerj-path-2199").is_none());
    }

    /// #2472: a Windows drive or UNC path names its repo by the last
    /// `\`-separated segment, as a URL or a Unix path does by its last `/` one.
    #[test]
    fn repo_name_from_source_splits_windows_paths() {
        for (source, repo) in [
            (r"F:\Projects\Roblox\LunarGame", "LunarGame"),
            (r"F:\Projects\Roblox\LunarGame\", "LunarGame"),
            (r"F:\src\widget.git", "widget"),
            (r"\\server\share\widget", "widget"),
            ("C:/src/widget/", "widget"),
            ("/home/me/widget", "widget"),
            ("https://github.com/xerj-org/xerj.git", "xerj"),
            ("git@github.com:xerj-org/xerj.git", "xerj"),
        ] {
            let name = repo_name_from_source(source);
            assert_eq!(name, repo, "{source}");
            assert!(
                xccode::pathgate::valid_repo_name(&name).is_ok(),
                "{source} -> {name}"
            );
        }
    }

    /// #2339: `--own` is explicit. A local path with no LICENSE is not marked
    /// own by itself. The marker persists across a re-add, a `--from` file
    /// cannot set it, and `xerj corpus own` sets and clears it.
    #[test]
    fn corpus_add_own_is_explicit_and_corpus_own_sets_an_existing_corpus() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("src/lib.rs", "fn mine() {}\n")]);

        assert_eq!(
            run_corpus_add(&["plain".into(), origin.display().to_string()]),
            0
        );
        let plain =
            manifest::read_corpus_manifest(&home.path().join("corpora/plain/corpus.json")).unwrap();
        assert!(!plain.own, "a local path is not own unless --own is passed");
        assert_eq!(plain.repos[0].licence, "NONE-FOUND");

        assert_eq!(
            run_corpus_add(&["--own".into(), "mine".into(), origin.display().to_string()]),
            0
        );
        let mine_path = home.path().join("corpora/mine/corpus.json");
        let mine = manifest::read_corpus_manifest(&mine_path).unwrap();
        assert!(mine.own);
        assert_eq!(mine.repos[0].licence, "NONE-FOUND");
        let text = std::fs::read_to_string(&mine_path).unwrap();
        assert!(text.contains("\"own\":true"), "{text}");

        assert_eq!(
            run_corpus_add(&["mine".into(), origin.display().to_string()]),
            0,
            "re-add without --own keeps the marker"
        );
        assert!(manifest::read_corpus_manifest(&mine_path).unwrap().own);

        let sha = head_sha(&origin);
        let pin = home.path().join("hostile.json");
        std::fs::write(
            &pin,
            format!(
                r#"{{"corpus":"from-file","own":true,"repos":[{{"repo":"widget","url":"{}","sha":"{sha}","licence":"UNKNOWN"}}]}}"#,
                origin.display()
            ),
        )
        .unwrap();
        assert_eq!(
            run_corpus_add(&["--from".into(), pin.display().to_string()]),
            0
        );
        let from_path = home.path().join("corpora/from-file/corpus.json");
        let from = manifest::read_corpus_manifest(&from_path).unwrap();
        assert!(
            !from.own,
            "a --from manifest must not be able to set own: {}",
            std::fs::read_to_string(&from_path).unwrap()
        );

        assert_eq!(run_corpus_own(&["from-file".into()]), 0);
        assert!(manifest::read_corpus_manifest(&from_path).unwrap().own);
        assert_eq!(
            manifest::read_corpus_manifest(&from_path).unwrap().repos[0].licence,
            "NONE-FOUND"
        );
        assert_eq!(run_corpus_own(&["from-file".into(), "--off".into()]), 0);
        let cleared = std::fs::read_to_string(&from_path).unwrap();
        assert!(!cleared.contains("own"), "{cleared}");
        assert!(!manifest::read_corpus_manifest(&from_path).unwrap().own);

        assert_eq!(run_corpus_own(&["missing".into()]), 2);
        assert_eq!(run_corpus_own(&[]), 2);
        assert!(CORPUS_USAGE.contains("xerj corpus own"));
        assert!(CORPUS_USAGE.contains("--own"));
        assert!(CORPUS_USAGE.contains("file classes") || CORPUS_USAGE.contains("per-file-class"));
    }

    /// #2576: both spellings the filter writes are recognised, and a complete
    /// checkout — even one with a remote — is not a partial clone.
    #[test]
    fn partial_clone_cause_reads_both_config_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("complete");
        init_committed_repo(&repo, &[("keep.txt", "tracked\n")]);
        let (rc, _) = git(
            Some(&repo),
            &["remote", "add", "origin", "https://example.com/x.git"],
        )
        .unwrap();
        assert_eq!(rc, 0);
        assert_eq!(
            partial_clone_cause(&repo),
            None,
            "a full clone has no promisor"
        );

        let (rc, _) = git(Some(&repo), &["config", "remote.origin.promisor", "true"]).unwrap();
        assert_eq!(rc, 0);
        assert_eq!(
            partial_clone_cause(&repo).as_deref(),
            Some("remote.origin.promisor=true"),
            "Git records the promisor property on a named remote"
        );

        let (rc, _) = git(
            Some(&repo),
            &["config", "extensions.partialClone", "origin"],
        )
        .unwrap();
        assert_eq!(rc, 0);
        assert_eq!(
            partial_clone_cause(&repo).as_deref(),
            Some("extensions.partialClone=origin"),
            "the optional extension records a promisor remote name"
        );
    }

    /// Real missing objects plus an unavailable promisor remote cause a
    /// clone failure. Restoring and hydrating the source makes a retry work.
    #[test]
    fn corpus_add_partial_clone_failure_preserves_retry_and_names_the_context() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(&origin, &[("src/keep.txt", "tracked\n")]);
        assert_eq!(
            git(Some(&origin), &["config", "uploadpack.allowFilter", "true"])
                .unwrap()
                .0,
            0
        );
        let partial = home.path().join("partial");
        let (rc, out) = git(
            None,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &format!("file://{}", origin.display()),
                &partial.display().to_string(),
            ],
        )
        .unwrap();
        assert_eq!(rc, 0, "{out}");
        let (rc, missing, err) = git_stdout(
            &partial,
            &["rev-list", "--objects", "--all", "--missing=print"],
        )
        .unwrap();
        assert_eq!(rc, 0, "{err}");
        assert!(
            missing.lines().any(|line| line.starts_with('?')),
            "fixture needs missing objects: {missing}"
        );
        std::fs::rename(&origin, home.path().join("offline-origin")).unwrap();
        let args = vec!["partial-corpus".into(), partial.display().to_string()];
        assert_eq!(run_corpus_add(&args), 1);
        assert_eq!(
            std::fs::read_dir(home.path().join("corpora/partial-corpus"))
                .unwrap()
                .count(),
            0,
            "failed clone cannot publish a partial target"
        );
        let cause = partial_clone_cause(&partial).unwrap();
        let lines = partial_clone_failure_hint("partial", &partial, &cause).join("\n");
        assert!(lines.contains("partial-clone configuration"), "{lines}");
        assert!(lines.contains("may explain"), "{lines}");
        assert!(lines.contains("only files already present"), "{lines}");
        assert!(lines.contains(&cause), "{lines}");
        std::fs::rename(home.path().join("offline-origin"), &origin).unwrap();
        let (rc, out) = git(Some(&partial), &["checkout", "HEAD", "--", "."]).unwrap();
        assert_eq!(rc, 0, "hydrate after restoring the remote: {out}");
        assert_eq!(run_corpus_add(&args), 0);
        assert!(home
            .path()
            .join("corpora/partial-corpus/partial/src/keep.txt")
            .is_file());
    }

    #[test]
    fn partial_clone_cause_accepts_non_origin_promisors_and_git_boolean_spellings() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        init_committed_repo(&repo, &[("keep.txt", "keep\n")]);
        for value in ["yes", "1", "true"] {
            assert_eq!(
                git(Some(&repo), &["config", "remote.cache.promisor", value])
                    .unwrap()
                    .0,
                0
            );
            assert_eq!(
                partial_clone_cause(&repo).as_deref(),
                Some("remote.cache.promisor=true")
            );
        }
        assert_eq!(
            git(Some(&repo), &["config", "remote.cache.promisor", "false"])
                .unwrap()
                .0,
            0
        );
        assert!(partial_clone_cause(&repo).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn partial_clone_hint_quotes_source_paths_for_the_named_shell() {
        let lines = partial_clone_failure_hint(
            "sample",
            Path::new("/tmp/a'b $(cmd)"),
            "remote.origin.promisor=true",
        )
        .join("\n");
        assert!(lines.contains("POSIX shell"));
        assert!(lines.contains("'/tmp/a'\"'\"'b $(cmd)'"), "{lines}");
    }

    /// An untracked root `.xerjignore` has to be copied or the index sees
    /// built-in rules alone. Re-adding refreshes it; `file://` is the same
    /// local checkout; a source with no ignore file does not gain one.
    #[test]
    fn corpus_add_copies_an_untracked_root_xerjignore_from_a_local_path() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "tracked\n")]);
        let ignore = "secret.csv\n";
        std::fs::write(origin.join(".xerjignore"), ignore).unwrap();
        let (rc, status) = git(
            Some(&origin),
            &["status", "--porcelain", "--untracked-files=all"],
        )
        .unwrap();
        assert_eq!(rc, 0, "{status}");
        assert!(
            status.contains("?? .xerjignore"),
            "the fixture must be untracked, not committed: {status}"
        );

        assert_eq!(
            run_corpus_add(&["local-ignore".into(), origin.display().to_string()]),
            0
        );
        let corpus = home.path().join("corpora/local-ignore");
        let clone = corpus.join("widget");
        assert_eq!(
            std::fs::read_to_string(clone.join("keep.txt")).unwrap(),
            "tracked\n"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join(".xerjignore")).expect("clone .xerjignore"),
            ignore,
            "an untracked root .xerjignore must be copied into the clone"
        );
        let recorded = manifest::read_corpus_manifest(&corpus.join("corpus.json")).unwrap();
        assert_eq!(
            recorded.repos[0].files,
            Some(2),
            "the carried ignore file is part of the tree the manifest counts"
        );

        let updated = "secret.csv\nalso.csv\n";
        std::fs::write(origin.join(".xerjignore"), updated).unwrap();
        assert_eq!(
            run_corpus_add(&["local-ignore".into(), origin.display().to_string()]),
            0,
            "re-add of an existing clone refreshes the carried file"
        );
        assert_eq!(
            std::fs::read_to_string(clone.join(".xerjignore")).unwrap(),
            updated
        );

        let file_url = format!("file://{}", origin.display());
        assert_eq!(run_corpus_add(&["via-file".into(), file_url]), 0);
        assert_eq!(
            std::fs::read_to_string(home.path().join("corpora/via-file/widget/.xerjignore"))
                .expect("file:// clone .xerjignore"),
            updated
        );

        let bare = home.path().join("plainrepo");
        init_committed_repo(&bare, &[("keep.txt", "tracked\n")]);
        assert_eq!(
            run_corpus_add(&["no-ignore".into(), bare.display().to_string()]),
            0
        );
        assert!(
            !home
                .path()
                .join("corpora/no-ignore/plainrepo/.xerjignore")
                .exists(),
            "a checkout with no .xerjignore must not gain one"
        );
    }

    /// An uncommitted edit of a tracked `.xerjignore` is the same drop:
    /// `git clone` checks out the committed bytes. A pinned `--from` rebuild
    /// must not copy the working tree over the recorded sha.
    #[test]
    fn corpus_add_copies_uncommitted_xerjignore_edits_but_a_pin_does_not() {
        let home = tempfile::tempdir().unwrap();
        let _h = code_home(home.path());

        let origin = home.path().join("edited");
        init_committed_repo(
            &origin,
            &[("keep.txt", "tracked\n"), (".xerjignore", "v1\n")],
        );
        std::fs::write(origin.join(".xerjignore"), "v2\n").unwrap();
        assert_eq!(
            run_corpus_add(&["edited-ignore".into(), origin.display().to_string()]),
            0
        );
        let clone = home.path().join("corpora/edited-ignore/edited");
        assert_eq!(
            std::fs::read_to_string(clone.join(".xerjignore")).unwrap(),
            "v2\n",
            "the working-tree edit must overlay the committed ignore file"
        );
        let (rc, committed) = git(Some(&clone), &["show", "HEAD:.xerjignore"]).unwrap();
        assert_eq!(rc, 0, "{committed}");
        assert_eq!(committed, "v1", "the overlay is not a new commit");

        let pinned_src = home.path().join("pinnedsrc");
        init_committed_repo(&pinned_src, &[("keep.txt", "tracked\n")]);
        std::fs::write(pinned_src.join(".xerjignore"), "secret.csv\n").unwrap();
        let sha = head_sha(&pinned_src);
        let manifest_path = home.path().join("pin.json");
        std::fs::write(
            &manifest_path,
            format!(
                r#"{{"corpus":"pinned-exact","repos":[{{"repo":"pinnedsrc","url":"{}","sha":"{sha}"}}]}}"#,
                pinned_src.display()
            ),
        )
        .unwrap();
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest_path.display().to_string()]),
            0
        );
        let pinned = home.path().join("corpora/pinned-exact/pinnedsrc");
        assert_eq!(
            std::fs::read_to_string(pinned.join("keep.txt")).unwrap(),
            "tracked\n",
            "the pin must still check out the recorded tree"
        );
        assert!(
            !pinned.join(".xerjignore").exists(),
            "a pinned rebuild stays at the recorded sha and does not copy the working tree's .xerjignore"
        );
    }

    // ── empty ledger vs autoindex prefixes (#2181) ──────────────────────────

    fn catalog_hits(prefixes: &[&str]) -> String {
        let hits: Vec<Value> = prefixes
            .iter()
            .map(|prefix| json!({"_source": {"doc_kind": "run", "prefix": prefix}}))
            .collect();
        json!({"hits": {"hits": hits}}).to_string()
    }

    /// One-shot JSON stand-in for the node. Records each request so a test
    /// can see that a path with corpora never asked the catalog.
    struct JsonServer {
        url: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl JsonServer {
        fn start(handler: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> Self {
            use std::io::{Read, Write};
            use std::net::TcpListener;
            use std::sync::atomic::Ordering;
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let seen_t = std::sync::Arc::clone(&seen);
            let stop_t = std::sync::Arc::clone(&stop);
            let handler = std::sync::Arc::new(handler);
            let join = std::thread::spawn(move || {
                while !stop_t.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            let _ =
                                stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                            let mut buf = [0u8; 8192];
                            let mut req = String::new();
                            loop {
                                match stream.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        req.push_str(&String::from_utf8_lossy(&buf[..n]));
                                        if let Some(end) = req.find("\r\n\r\n") {
                                            let header_end = end + 4;
                                            let need = req[..header_end].lines().find_map(|line| {
                                                let (name, value) = line.split_once(':')?;
                                                if name.eq_ignore_ascii_case("content-length") {
                                                    value.trim().parse::<usize>().ok()
                                                } else {
                                                    None
                                                }
                                            });
                                            match need {
                                                Some(need) if req.len() - header_end >= need => {
                                                    break
                                                }
                                                None => break,
                                                Some(_) => {}
                                            }
                                        }
                                        if req.len() > 64 * 1024 {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            seen_t.lock().unwrap().push(req.clone());
                            let (status, body) = handler(&req);
                            let reason = match status {
                                200 => "OK",
                                401 => "Unauthorized",
                                404 => "Not Found",
                                _ => "Error",
                            };
                            let bytes = format!(
                                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(bytes.as_bytes());
                            let _ = stream.flush();
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                url,
                seen,
                stop,
                join: Some(join),
            }
        }

        fn requests(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for JsonServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn closed_port_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        url
    }

    fn assert_fast(started: std::time::Instant, what: &str) {
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "{what} took {elapsed:?}; a dead node must not go through Es::search's six retries"
        );
    }

    #[test]
    fn prefix_hint_names_the_search_and_def_commands_and_corpus_add() {
        let one = format_autoindex_prefix_hint(&["proj".into()]).unwrap();
        assert!(one.contains("prefixes: proj\n"), "{one}");
        assert!(one.contains("`xerj search --prefix proj`"), "{one}");
        assert!(one.contains("`xerj def --prefix proj`"), "{one}");
        assert!(one.contains("`xerj corpus add <name> <path>`"), "{one}");
        assert!(!one.contains("same --prefix"), "{one}");
        assert_eq!(one.lines().count(), 2, "{one}");

        let many = format_autoindex_prefix_hint(&["refs".into(), "proj".into()]).unwrap();
        assert!(many.contains("prefixes: proj, refs"), "{many}");
        assert!(many.contains("`xerj search --prefix proj`"), "{many}");
        assert!(many.contains("same --prefix for each"), "{many}");

        let mut nine: Vec<String> = (b'a'..=b'i').map(|c| (c as char).to_string()).collect();
        nine.push("bad name".into());
        nine.push("has\nnewline".into());
        let capped = format_autoindex_prefix_hint(&nine).unwrap();
        assert!(capped.contains("… (1 more)"), "{capped}");
        assert!(capped.contains("`xerj search --prefix a`"), "{capped}");
        assert!(!capped.contains("bad name"), "{capped}");
        assert!(!capped.contains("has\nnewline"), "{capped}");
        assert_eq!(capped.lines().count(), 2, "{capped}");

        assert!(format_autoindex_prefix_hint(&[]).is_none());
        assert!(format_autoindex_prefix_hint(&["bad name".into()]).is_none());
    }

    #[test]
    fn catalog_hit_prefixes_are_deduped_and_a_non_search_body_is_not_empty() {
        let v = json!({"hits": {"hits": [
            {"_source": {"prefix": "b"}},
            {"_source": {"prefix": ""}},
            {"_source": {"prefix": "a"}},
            {"_source": {"prefix": "a"}},
            {"_source": {"other": 1}}
        ]}});
        assert_eq!(
            prefixes_from_catalog_hits(&v).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(prefixes_from_catalog_hits(&json!({})).is_none());
        assert_eq!(
            prefixes_from_catalog_hits(&json!({"hits": {"hits": []}})).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_unreachable_catalog_is_silence_not_an_error() {
        let url = closed_port_url();
        let started = std::time::Instant::now();
        assert!(fetch_catalog_run_prefixes(&url, None).is_none());
        assert_fast(started, "closed port");

        let server = JsonServer::start(|_| (500, "{\"error\":\"down\"}".into()));
        assert!(catalog_prefix_hint(&server.url, None).is_none());
        let server = JsonServer::start(|_| (200, "not-json".into()));
        assert!(fetch_catalog_run_prefixes(&server.url, None).is_none());
        let server = JsonServer::start(|_| (200, "{\"hits\":{\"hits\":[]}}".into()));
        assert!(catalog_prefix_hint(&server.url, None).is_none());
        let server = JsonServer::start(|_| (401, "{}".into()));
        assert!(catalog_prefix_hint(&server.url, Some("nope")).is_none());
    }

    fn request_has_api_key(req: &str, key: &str) -> bool {
        let want = format!("ApiKey {key}");
        req.lines().any(|line| {
            let Some((name, value)) = line.split_once(':') else {
                return false;
            };
            name.trim().eq_ignore_ascii_case("authorization") && value.trim() == want
        })
    }

    #[test]
    fn the_hint_sends_the_explicit_api_key() {
        let server = JsonServer::start(|req| {
            if request_has_api_key(req, "secret-key") {
                (200, catalog_hits(&["proj"]))
            } else {
                (401, "{}".into())
            }
        });
        let hint = catalog_prefix_hint(&server.url, Some("secret-key")).unwrap();
        assert!(hint.contains("prefixes: proj"), "{hint}");
        assert!(
            server
                .requests()
                .iter()
                .any(|req| request_has_api_key(req, "secret-key")),
            "{:?}",
            server.requests()
        );
    }

    #[test]
    fn corpus_list_with_no_ledger_names_catalog_prefixes() {
        let server = JsonServer::start(|_| (200, catalog_hits(&["refs", "proj", "proj"])));
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let (code, text) = corpus_list_text(&["--url".into(), server.url.clone()]);
        assert_eq!(code, 2);
        assert!(text.stdout.is_empty(), "{}", text.stdout);
        assert!(
            text.stderr
                .contains("nothing has been indexed on this machine"),
            "{}",
            text.stderr
        );
        assert!(
            text.stderr.contains("prefixes: proj, refs"),
            "{}",
            text.stderr
        );
        assert!(
            text.stderr.contains("`xerj search --prefix proj`"),
            "{}",
            text.stderr
        );
        assert!(
            text.stderr.contains("`xerj def --prefix proj`"),
            "{}",
            text.stderr
        );
        assert!(
            text.stderr.contains("`xerj corpus add <name> <path>`"),
            "{}",
            text.stderr
        );

        std::fs::create_dir_all(home.path().join("state")).unwrap();
        let (code, text) = corpus_list_text(&["--url".into(), server.url.clone()]);
        assert_eq!(code, 0, "{}", text.stderr);
        assert!(
            text.stdout.contains("0 of 0 corpora are actually loaded"),
            "{}",
            text.stdout
        );
        assert!(
            text.stderr.contains("prefixes: proj, refs"),
            "{}",
            text.stderr
        );
    }

    #[test]
    fn corpus_list_stays_quiet_when_the_node_is_down() {
        let url = closed_port_url();
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let started = std::time::Instant::now();
        let (code, text) = corpus_list_text(&["--url".into(), url.clone()]);
        assert_fast(started, "corpus list, no state dir");
        assert_eq!(code, 2);
        assert!(
            text.stderr
                .contains("nothing has been indexed on this machine"),
            "{}",
            text.stderr
        );
        assert!(
            !text.stderr.contains("autoindex catalog"),
            "{}",
            text.stderr
        );
        assert!(!text.stderr.contains("cannot reach"), "{}", text.stderr);

        std::fs::create_dir_all(home.path().join("state")).unwrap();
        let started = std::time::Instant::now();
        let (code, text) = corpus_list_text(&["--url".into(), url]);
        assert_fast(started, "corpus list, empty state dir");
        assert_eq!(code, 0, "{}", text.stderr);
        assert!(
            text.stdout.contains("0 of 0 corpora are actually loaded"),
            "{}",
            text.stdout
        );
        assert!(text.stderr.is_empty(), "{}", text.stderr);
    }

    #[test]
    fn corpus_list_with_a_corpus_does_not_mention_autoindex_prefixes() {
        let server = JsonServer::start(|req| {
            if req.contains("/_cat/indices/") {
                (200, r#"[{"index":"xc-demo-b1-docs"}]"#.into())
            } else if req.contains("autoindex-catalog") {
                (200, catalog_hits(&["proj"]))
            } else {
                (404, "{}".into())
            }
        });
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let state = home.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("demo.json"),
            r#"{"corpus":"demo","indexed_at":"2026-01-02T03:04:05Z","prefix":"xc-demo","autoindex_exit":0,"salvaged":false}"#,
        )
        .unwrap();
        let (code, text) = corpus_list_text(&["--url".into(), server.url.clone()]);
        assert_eq!(code, 0, "{}", text.stderr);
        assert!(text.stdout.contains("demo"), "{}", text.stdout);
        assert!(text.stdout.contains("loaded — 1 index"), "{}", text.stdout);
        assert!(
            text.stdout.contains("1 of 1 corpora are actually loaded"),
            "{}",
            text.stdout
        );
        assert!(
            !text.stderr.contains("autoindex catalog"),
            "{}",
            text.stderr
        );
        assert!(
            !text.stdout.contains("autoindex catalog"),
            "{}",
            text.stdout
        );
        let seen = server.requests();
        assert!(
            seen.iter().all(|req| !req.contains("autoindex-catalog")),
            "a ledger that has a corpus must not read the catalog: {seen:?}"
        );
    }

    #[test]
    fn xerj_code_on_an_empty_ledger_names_catalog_prefixes() {
        let server = JsonServer::start(|_| (200, catalog_hits(&["proj"])));
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let done = code_cli_outcome(&[
            "proj".into(),
            "how does it flush".into(),
            "--url".into(),
            server.url.clone(),
        ]);
        let CodeCliDone::Outcome(out) = done else {
            panic!("usage path");
        };
        assert_eq!(out.exit, 2);
        assert!(out.to_stderr);
        assert!(
            out.text.contains("corpus 'proj' is not indexed"),
            "{}",
            out.text
        );
        assert!(
            out.text.contains("`xerj search --prefix proj`"),
            "{}",
            out.text
        );
        assert!(
            out.text.contains("`xerj def --prefix proj`"),
            "{}",
            out.text
        );
        assert!(
            out.text.contains("`xerj corpus add <name> <path>`"),
            "{}",
            out.text
        );

        // A different registered corpus means the ledger is not empty: the
        // missing name keeps its own refusal and the catalog is not read.
        std::fs::create_dir_all(home.path().join("state")).unwrap();
        std::fs::write(
            home.path().join("state/other.json"),
            r#"{"corpus":"other","indexed_at":"2026-01-02T03:04:05Z","prefix":"xc-other"}"#,
        )
        .unwrap();
        let before = server.requests().len();
        let done = code_cli_outcome(&[
            "proj".into(),
            "how does it flush".into(),
            "--url".into(),
            server.url.clone(),
        ]);
        let CodeCliDone::Outcome(out) = done else {
            panic!("usage path");
        };
        assert_eq!(out.exit, 2);
        assert!(
            out.text.contains("corpus 'proj' is not indexed"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("autoindex catalog"), "{}", out.text);
        assert_eq!(
            server.requests().len(),
            before,
            "a non-empty ledger must not probe the catalog"
        );
        drop(_hold);

        let url = closed_port_url();
        let empty = tempfile::tempdir().unwrap();
        let _hold_empty = code_home(empty.path());
        let started = std::time::Instant::now();
        let done = code_cli_outcome(&[
            "proj".into(),
            "how does it flush".into(),
            "--url".into(),
            url,
        ]);
        assert_fast(started, "xerj code, dead node");
        let CodeCliDone::Outcome(out) = done else {
            panic!("usage path");
        };
        assert_eq!(out.exit, 2);
        assert!(
            out.text.contains("corpus 'proj' is not indexed"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("autoindex catalog"), "{}", out.text);
    }

    fn worktree_files(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        walk(root, root, &mut out);
        out.sort();
        out
    }

    fn write_pin(path: &Path, url: &str, sha: &str, paths: Option<&[&str]>) {
        let mut repo = json!({
            "repo": "vulndb",
            "url": url,
            "licence": "BSD-3-Clause",
            "sha": sha,
        });
        if let Some(paths) = paths {
            repo["paths"] = json!(paths);
        }
        std::fs::write(
            path,
            json!({ "corpus": "sparse-demo", "repos": [repo] }).to_string(),
        )
        .unwrap();
    }

    /// #1842: `paths` harvests only those directories. A whole-repo pin of
    /// golang/vulndb indexed ~1.2M docs; the documented corpus is the
    /// `data/osv` advisory tree. Cone mode would also leave ancestor files
    /// (LICENSE, go tests) for `corpus index` to ingest, so the checkout is
    /// the directory itself. Re-adding the same pin narrows a previous full
    /// checkout, and dropping `paths` restores it.
    #[test]
    fn corpus_add_from_paths_harvests_only_those_directories() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[
                ("LICENSE", "bsd\n"),
                ("data/osv/GO-1.json", "{}\n"),
                ("data/osv/sub/n.json", "{}\n"),
                ("data/osv-extra/nope.json", "{}\n"),
                ("data/reports/R.yaml", "id: 1\n"),
                ("cmd/main.go", "package main\n"),
            ],
        );
        let sha = head_sha(&origin);
        let url = origin.display().to_string();
        let manifest = home.path().join("pin.json");
        let checkout = home.path().join("corpora/sparse-demo/vulndb");

        write_pin(&manifest, &url, &sha, None);
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0,
            "a pin with no paths still harvests the whole repo"
        );
        assert_eq!(
            worktree_files(&checkout),
            vec![
                "LICENSE".to_string(),
                "cmd/main.go".to_string(),
                "data/osv-extra/nope.json".to_string(),
                "data/osv/GO-1.json".to_string(),
                "data/osv/sub/n.json".to_string(),
                "data/reports/R.yaml".to_string(),
            ]
        );

        write_pin(&manifest, &url, &sha, Some(&["data/osv"]));
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0,
            "re-adding the same sha with paths must narrow the existing checkout"
        );
        assert_eq!(
            worktree_files(&checkout),
            vec![
                "data/osv/GO-1.json".to_string(),
                "data/osv/sub/n.json".to_string(),
            ]
        );
        let recorded =
            manifest::read_corpus_manifest(&home.path().join("corpora/sparse-demo/corpus.json"))
                .unwrap();
        assert_eq!(recorded.repos[0].paths, vec!["data/osv".to_string()]);
        assert_eq!(
            recorded.repos[0].files,
            Some(2),
            "the manifest counts the harvested files, not the rest of the pin"
        );
        let shared =
            manifest::read_hub_manifest(&home.path().join("corpora/sparse-demo/corpus.json"))
                .unwrap();
        assert_eq!(shared.rows[0].paths, vec!["data/osv".to_string()]);

        // Idempotent: a second limited add does not bring the siblings back.
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0
        );
        assert_eq!(worktree_files(&checkout).len(), 2);

        write_pin(&manifest, &url, &sha, Some(&["data/osv", "cmd"]));
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0
        );
        assert_eq!(
            worktree_files(&checkout),
            vec![
                "cmd/main.go".to_string(),
                "data/osv/GO-1.json".to_string(),
                "data/osv/sub/n.json".to_string(),
            ]
        );

        write_pin(&manifest, &url, &sha, None);
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0,
            "dropping paths restores the whole pin"
        );
        assert!(checkout.join("data/reports/R.yaml").is_file());
        assert!(checkout.join("LICENSE").is_file());
        assert!(manifest::read_corpus_manifest(
            &home.path().join("corpora/sparse-demo/corpus.json")
        )
        .unwrap()
        .repos[0]
            .paths
            .is_empty());
    }

    /// A path the pin does not contain, and a path that escapes the repo,
    /// must not materialize the rest of the tree.
    #[test]
    fn corpus_add_from_paths_refuses_a_missing_or_escaping_directory() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[("LICENSE", "bsd\n"), ("data/reports/R.yaml", "id: 1\n")],
        );
        let sha = head_sha(&origin);
        let url = origin.display().to_string();
        let manifest = home.path().join("pin.json");
        write_pin(&manifest, &url, &sha, Some(&["data/nope"]));
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            1
        );
        let checkout = home.path().join("corpora/sparse-demo/vulndb");
        assert!(
            !home.path().join("corpora/sparse-demo/corpus.json").exists(),
            "a failed harvest must not record a corpus"
        );
        assert!(
            !checkout.join("LICENSE").exists(),
            "a missing paths entry must not fall back to a full checkout"
        );
        assert!(!checkout.join("data/reports/R.yaml").exists());

        std::fs::write(
            &manifest,
            json!({
                "corpus": "escape-demo",
                "repos": [{
                    "repo": "vulndb",
                    "url": url,
                    "sha": sha,
                    "paths": ["../secret"]
                }]
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            2
        );
        assert!(
            !home.path().join("corpora/escape-demo").exists(),
            "an escaping paths entry is refused before any directory is created"
        );
    }

    /// #2486: a `paths` entry may name a file. The pandas time-series pack
    /// needs `pandas/core/resample.py` without the rest of `pandas/core`, and
    /// naming the parent directory crowds the documentation out of the top
    /// hits. A file entry checks out that file and nothing beside it: not a
    /// sibling whose name starts the same way, and not a file of the same
    /// name deeper in the tree (the pattern is anchored at the root).
    #[test]
    fn corpus_add_from_paths_harvests_a_named_file() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[
                ("LICENSE", "bsd\n"),
                ("pkg/core/resample.py", "def resample(): pass\n"),
                ("pkg/core/resample.pyx", "cdef int x\n"),
                ("pkg/core/frame.py", "class DataFrame: pass\n"),
                ("other/pkg/core/resample.py", "def other(): pass\n"),
                ("doc/timeseries.rst", "Time series\n"),
            ],
        );
        let sha = head_sha(&origin);
        let url = origin.display().to_string();
        let manifest = home.path().join("pin.json");
        let checkout = home.path().join("corpora/sparse-demo/vulndb");

        write_pin(
            &manifest,
            &url,
            &sha,
            Some(&["pkg/core/resample.py", "doc"]),
        );
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0,
            "a paths entry naming a file must be harvested"
        );
        assert_eq!(
            worktree_files(&checkout),
            vec![
                "doc/timeseries.rst".to_string(),
                "pkg/core/resample.py".to_string(),
            ]
        );
        let recorded =
            manifest::read_corpus_manifest(&home.path().join("corpora/sparse-demo/corpus.json"))
                .unwrap();
        assert_eq!(
            recorded.repos[0].paths,
            vec!["pkg/core/resample.py".to_string(), "doc".to_string()]
        );
        assert_eq!(recorded.repos[0].files, Some(2));

        // Re-adding over the existing checkout narrows and widens it too.
        write_pin(&manifest, &url, &sha, Some(&["doc"]));
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0
        );
        assert_eq!(
            worktree_files(&checkout),
            vec!["doc/timeseries.rst".to_string()]
        );
        write_pin(&manifest, &url, &sha, Some(&["pkg/core/frame.py"]));
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            0
        );
        assert_eq!(
            worktree_files(&checkout),
            vec!["pkg/core/frame.py".to_string()]
        );
    }

    /// Each tree entry kind gets its own answer, read from the mode, so a
    /// submodule is not reported as a missing path.
    #[test]
    fn confirm_paths_exist_classifies_each_entry_kind() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        init_committed_repo(&repo, &[("doc/a.rst", "a\n"), ("pkg/m.py", "x = 1\n")]);
        let first = head_sha(&repo);
        let gitlink = format!("160000,{first},vendor/sub");
        let (rc, out) = git(
            Some(&repo),
            &["update-index", "--add", "--cacheinfo", &gitlink],
        )
        .unwrap();
        assert_eq!(rc, 0, "git update-index: {out}");
        let (rc, out) = git(
            Some(&repo),
            &[
                "-c",
                "user.email=xerj-test@example.com",
                "-c",
                "user.name=xerj-test",
                "commit",
                "--quiet",
                "-m",
                "gitlink",
            ],
        )
        .unwrap();
        assert_eq!(rc, 0, "git commit: {out}");
        let sha = head_sha(&repo);
        let paths = |list: &[&str]| list.iter().map(|p| p.to_string()).collect::<Vec<_>>();

        assert_eq!(
            confirm_paths_exist(&repo, &sha, &paths(&["pkg/m.py", "doc"])).unwrap(),
            vec![PathKind::File, PathKind::Dir]
        );
        let err = confirm_paths_exist(&repo, &sha, &paths(&["vendor/sub"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("is a submodule"), "{err}");
        let err = confirm_paths_exist(&repo, &sha, &paths(&["doc/nope.rst"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no such path"), "{err}");
    }

    /// A symlink entry is refused end to end. Naming one would hand
    /// `corpus index` a link the manifest author chose, possibly pointing out
    /// of the checkout, so it is refused and no corpus is recorded.
    #[cfg(unix)]
    #[test]
    fn corpus_add_from_paths_refuses_a_symlink_entry() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(&origin, &[("LICENSE", "bsd\n")]);
        std::os::unix::fs::symlink("/etc/hostname", origin.join("link")).unwrap();
        let (rc, out) = git(Some(&origin), &["add", "--", "link"]).unwrap();
        assert_eq!(rc, 0, "git add link: {out}");
        let (rc, out) = git(
            Some(&origin),
            &[
                "-c",
                "user.email=xerj-test@example.com",
                "-c",
                "user.name=xerj-test",
                "commit",
                "--quiet",
                "-m",
                "link",
            ],
        )
        .unwrap();
        assert_eq!(rc, 0, "git commit: {out}");
        let sha = head_sha(&origin);
        let manifest = home.path().join("pin.json");
        write_pin(
            &manifest,
            &origin.display().to_string(),
            &sha,
            Some(&["link"]),
        );
        assert_eq!(
            run_corpus_add(&["--from".into(), manifest.display().to_string()]),
            1
        );
        assert!(
            !home.path().join("corpora/sparse-demo/corpus.json").exists(),
            "a refused symlink entry must not record a corpus"
        );
        assert!(!home
            .path()
            .join("corpora/sparse-demo/vulndb/LICENSE")
            .exists());
    }
    #[test]
    fn corpus_bad_sparse_path_preserves_existing_checkout_and_manifest() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[
                ("doc/keep.txt", "retained content\n"),
                ("pkg/code.rs", "fn f() {}\n"),
            ],
        );
        let sha = head_sha(&origin);
        let pin = home.path().join("pin.json");
        let url = origin.display().to_string();
        write_pin(&pin, &url, &sha, Some(&["doc"]));
        let args = vec!["--from".to_string(), pin.display().to_string()];
        assert_eq!(run_corpus_add(&args), 0);
        let checkout = home.path().join("corpora/sparse-demo/vulndb");
        let manifest = home.path().join("corpora/sparse-demo/corpus.json");
        let before = std::fs::read(&manifest).unwrap();
        let index_before = std::fs::read(checkout.join(".git/index")).unwrap();
        let sparse_before = std::fs::read(checkout.join(".git/info/sparse-checkout")).unwrap();
        std::fs::write(checkout.join("local-note.txt"), "uncommitted local note").unwrap();
        write_pin(&pin, &url, &sha, Some(&["pkg/code.rs", "missing.txt"]));
        assert_eq!(run_corpus_add(&args), 1);
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        assert_eq!(
            std::fs::read(checkout.join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(
            std::fs::read(checkout.join(".git/info/sparse-checkout")).unwrap(),
            sparse_before
        );
        assert_eq!(
            std::fs::read_to_string(checkout.join("doc/keep.txt")).unwrap(),
            "retained content\n"
        );
        assert_eq!(
            std::fs::read_to_string(checkout.join("local-note.txt")).unwrap(),
            "uncommitted local note"
        );
        assert!(!checkout.join("pkg/code.rs").exists());
        assert_eq!(head_sha(&checkout), sha);
        // Correcting the path performs the requested narrower harvest.
        write_pin(&pin, &url, &sha, Some(&["pkg/code.rs"]));
        assert_eq!(run_corpus_add(&args), 0);
        assert!(checkout.join("pkg/code.rs").is_file());
        assert!(!checkout.join("doc/keep.txt").exists());
    }
    #[test]
    fn corpus_add_does_not_delete_an_unborn_repo_with_user_files() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "remote content\n")]);
        let target = home.path().join("corpora/preserve/widget");
        std::fs::create_dir_all(&target).unwrap();
        assert_eq!(git(Some(&target), &["init", "--quiet"]).unwrap().0, 0);
        std::fs::write(target.join("local-note.txt"), "irreplaceable local content").unwrap();
        let code = run_corpus_add(&["preserve".into(), origin.display().to_string()]);
        assert_eq!(
            std::fs::read_to_string(target.join("local-note.txt"))
                .ok()
                .as_deref(),
            Some("irreplaceable local content"),
            "an unborn repo is not proof of disposable state"
        );
        assert_eq!(code, 1, "an ambiguous existing checkout must be refused");
        assert!(!home.path().join("corpora/preserve/corpus.json").exists());
        // Keeping the old directory separately makes a normal retry safe.
        let backup = home.path().join("saved-incomplete-widget");
        std::fs::rename(&target, &backup).unwrap();
        assert_eq!(
            run_corpus_add(&["preserve".into(), origin.display().to_string()]),
            0
        );
        assert_eq!(
            std::fs::read_to_string(backup.join("local-note.txt")).unwrap(),
            "irreplaceable local content"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "remote content\n"
        );
    }

    #[test]
    fn corpus_add_does_not_reset_user_edits_when_index_is_missing() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("widget");
        init_committed_repo(&origin, &[("keep.txt", "remote content\n")]);
        let args = vec!["preserve".into(), origin.display().to_string()];
        assert_eq!(run_corpus_add(&args), 0);
        let target = home.path().join("corpora/preserve/widget");
        let manifest = home.path().join("corpora/preserve/corpus.json");
        let before = std::fs::read(&manifest).unwrap();
        std::fs::write(target.join("keep.txt"), "irreplaceable local edit").unwrap();
        std::fs::remove_file(target.join(".git/index")).unwrap();
        let code = run_corpus_add(&args);
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "irreplaceable local edit",
            "missing index does not authorize reset --hard"
        );
        assert_eq!(code, 1);
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
    }

    fn preserve_incomplete_for_retry(checkout: &Path) -> PathBuf {
        let backup = checkout.with_extension("saved");
        assert!(!backup.exists());
        std::fs::rename(checkout, &backup).unwrap();
        backup
    }

    #[test]
    fn staged_clone_failure_never_publishes_a_partial_checkout() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("repo");
        let error = stage_new_checkout(&target, |stage| {
            assert!(!target.exists());
            std::fs::create_dir_all(stage)?;
            std::fs::write(stage.join("partial.txt"), "partial")?;
            anyhow::bail!("injected clone failure")
        })
        .unwrap_err();
        assert!(error.to_string().contains("injected clone failure"));
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        stage_new_checkout(&target, |stage| {
            assert!(!target.exists());
            init_committed_repo(stage, &[("complete.txt", "complete\n")]);
            Ok(())
        })
        .unwrap();
        assert!(existing_checkout_is_complete(&target));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    }

    #[test]
    fn staged_clone_does_not_replace_a_destination_created_during_clone() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("repo");
        let error = stage_new_checkout(&target, |stage| {
            init_committed_repo(stage, &[("remote.txt", "remote\n")]);
            std::fs::create_dir(&target)?;
            std::fs::write(target.join("local.txt"), "local content")?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(target.join("local.txt")).unwrap(),
            "local content"
        );
        assert!(!target.join("remote.txt").exists());
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
        let error = stage_new_checkout(&target, |_| {
            panic!("existing destination must be refused before cloning")
        })
        .unwrap_err();
        assert!(error.to_string().contains("already exists"));
    }

    #[test]
    fn corpus_failed_new_clone_can_be_retried_without_deleting_anything() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("widget");
        let args = vec!["retry".into(), origin.display().to_string()];
        assert_eq!(run_corpus_add(&args), 1);
        let corpus = home.path().join("corpora/retry");
        assert_eq!(std::fs::read_dir(&corpus).unwrap().count(), 0);
        init_committed_repo(&origin, &[("complete.txt", "complete\n")]);
        assert_eq!(run_corpus_add(&args), 0);
        assert!(corpus.join("widget/complete.txt").is_file());
        assert!(corpus.join("corpus.json").is_file());
    }

    #[test]
    fn a_committed_empty_tree_is_a_reusable_checkout() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("empty");
        std::fs::create_dir(&repo).unwrap();
        assert_eq!(git(Some(&repo), &["init", "--quiet"]).unwrap().0, 0);
        assert_eq!(
            git(
                Some(&repo),
                &[
                    "-c",
                    "user.name=xerj-test",
                    "-c",
                    "user.email=xerj-test@example.com",
                    "commit",
                    "--allow-empty",
                    "--quiet",
                    "-m",
                    "empty"
                ]
            )
            .unwrap()
            .0,
            0
        );
        assert!(existing_checkout_is_complete(&repo));
    }
    #[test]
    fn corpus_add_accepts_a_hydrated_partial_clone_source() {
        let home = tempfile::tempdir().unwrap();
        let _hold = code_home(home.path());
        let origin = home.path().join("origin");
        init_committed_repo(
            &origin,
            &[
                ("src/a.txt", "source content\n"),
                ("docs/b.txt", "documentation\n"),
            ],
        );
        assert_eq!(
            git(Some(&origin), &["config", "uploadpack.allowFilter", "true"])
                .unwrap()
                .0,
            0
        );
        let partial = home.path().join("partial");
        let url = format!("file://{}", origin.display());
        let (rc, out) = git(
            None,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                &partial.display().to_string(),
            ],
        )
        .unwrap();
        assert_eq!(rc, 0, "{out}");
        let (rc, out) = git(Some(&partial), &["checkout", "HEAD", "--", "."]).unwrap();
        assert_eq!(rc, 0, "hydration: {out}");
        assert!(
            partial_clone_cause(&partial).is_some(),
            "config remains after hydration"
        );
        assert_eq!(
            run_corpus_add(&["hydrated".into(), partial.display().to_string()]),
            0,
            "promisor configuration is not proof that cloning will fail"
        );
        let target = home.path().join("corpora/hydrated/partial");
        assert_eq!(
            std::fs::read_to_string(target.join("src/a.txt")).unwrap(),
            "source content\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("docs/b.txt")).unwrap(),
            "documentation\n"
        );
    }
}
