//! `xerj brain <folder>` — one command from a folder to a running, browsable
//! second brain: boot (or attach to) the local server, run the autoindex
//! pipeline with relationship detection on, then open the console's
//! second-brain view — with the server's own one-time passkey-setup link on
//! a first launch.
//!
//! Composition, not reimplementation:
//! - indexing + link detection = `xerj_autoindex::run_index_report` — the
//!   exact `xerj autoindex` pipeline, graph detectors on (its default;
//!   SECOND_BRAIN_SPEC §6). Re-runs converge because detector edges take
//!   `valid_at` from source mtimes, so unchanged files re-emit identical
//!   `edge_id`s and bulk `index` overwrites in place.
//! - console auth = the server's own first-launch bootstrap
//!   (`xerj-console-api::bootstrap`), which mints a single-use 30-minute
//!   setup link and prints it in the startup banner. This command only
//!   *relays* that link from the freshly-booted server's log — it never
//!   mints credentials or invents an auth path of its own.
//! - the server is the ordinary `xerj` binary (`current_exe()`), spawned
//!   detached (own process group, output to `<data-dir>/server.log`) so
//!   the brain outlives this command.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use xerj_autoindex::cli::IndexCfg;
use xerj_autoindex::esclient::Es;
use xerj_autoindex::{detect, walk};

/// How long a freshly-spawned server gets to answer `/health/ready`.
/// Fresh data dirs are ready in ~1s; a large existing dir replays WAL first.
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);
const BOOT_POLL: Duration = Duration::from_millis(150);

pub fn run_cli() -> i32 {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let cfg = match parse(args) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            print_help();
            return 0;
        }
        Err(e) => {
            eprintln!("error: {e}\n");
            print_help();
            return 2;
        }
    };
    match run(cfg) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

pub fn print_help() {
    println!("{}", help_text(xerj_common::feedback::enabled()));
}

/// The help text as a value, so tests can assert on it — presence *and*
/// position of the feedback invitation — instead of trusting a `println!`.
pub fn help_text(feedback: bool) -> String {
    format!(
        "xerj brain — turn a folder into a running, browsable second brain, in one command\n\
         \n\
         Point it at a folder. xerj starts its local server (or attaches to a running\n\
         one), indexes every readable file, detects the links between them (wikilinks,\n\
         relative links, section order, shared folders), and opens your knowledge in\n\
         the browser. Safe to re-run any time — re-runs converge, nothing duplicates.\n\
         \n\
         {}\
         USAGE:\n\
             xerj brain <folder> [OPTIONS]\n\
         \n\
         OPTIONS:\n\
             --brain <NAME>     brain name (default: the folder's name; part of the URL)\n\
             --url <U>          server to use (default http://localhost:9200); a server\n\
                                is booted only for a localhost URL nothing listens on\n\
             --data-dir <PATH>  where a newly-booted server keeps its data and log\n\
                                (default ~/.xerj/brain)\n\
             --api-key <K>      API key for an already-running secured server\n\
                                (or env XERJ_API_KEY; a server booted by this command\n\
                                needs neither — its key is read from <data-dir>/admin.key)\n\
             --fresh            ignore the resume journal and rebuild the plan in place,\n\
                                re-walking everything (ids stay idempotent). It never\n\
                                deletes documents for notes you removed\n\
             --no-open          print the links but do not open a browser\n\
             --disable-feedback do not print the feedback invitation above; honoured in\n\
                                any position, including after --help (env\n\
                                XERJ_DISABLE_FEEDBACK=true)\n\
             --help, -h         this help\n\
         \n\
         EXIT CODES: 0 ready; 3 ready-with-junk (unreadable files recorded, never\n\
                     fatal); 1 nothing indexable / server failure; 2 usage\n",
        xerj_common::feedback::block(feedback),
    )
}

pub struct BrainCfg {
    pub root: PathBuf,
    pub brain: Option<String>,
    pub url: String,
    pub data_dir: Option<PathBuf>,
    pub api_key: Option<String>,
    pub fresh: bool,
    pub no_open: bool,
}

fn parse(args: Vec<String>) -> Result<Option<BrainCfg>, String> {
    let mut it = args.into_iter();
    let mut root: Option<PathBuf> = None;
    let mut brain: Option<String> = None;
    let mut url = "http://localhost:9200".to_string();
    let mut data_dir: Option<PathBuf> = None;
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut fresh = false;
    let mut no_open = false;

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--brain" => {
                let name = it.next().ok_or("--brain needs a value")?;
                detect::validate_brain(&name)
                    .map_err(|reason| format!("--brain {name}: {reason}"))?;
                brain = Some(name);
            }
            "--url" => url = it.next().ok_or("--url needs a value")?,
            "--data-dir" => {
                data_dir = Some(PathBuf::from(it.next().ok_or("--data-dir needs a value")?))
            }
            "--api-key" => api_key = Some(it.next().ok_or("--api-key needs a value")?),
            "--fresh" => fresh = true,
            "--no-open" => no_open = true,
            // Read out of band by `xerj_common::feedback`, which scans the
            // whole argument list; accepted here so it is not "unknown".
            xerj_common::feedback::DISABLE_FLAG => {}
            "--help" | "-h" => return Ok(None),
            other if !other.starts_with('-') && root.is_none() => root = Some(PathBuf::from(other)),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let Some(root) = root else {
        return Ok(None); // no folder → help, like `xerj autoindex`
    };
    Ok(Some(BrainCfg {
        root,
        brain,
        url: url.trim_end_matches('/').to_string(),
        data_dir,
        api_key,
        fresh,
        no_open,
    }))
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".xerj").join("brain")
}

/// (host, port) out of an `http://host[:port]` URL. Only plain-http local
/// URLs are boot candidates; anything else is attach-only.
fn parse_http_url(url: &str) -> Result<(String, u16)> {
    let rest = url
        .strip_prefix("http://")
        .with_context(|| format!("--url must start with http:// (got {url}); https servers can only be attached to, and need the full URL"))?;
    let hostport = rest.split('/').next().unwrap_or(rest);
    if let Some((host, port)) = hostport.rsplit_once(':') {
        let port: u16 = port
            .parse()
            .with_context(|| format!("invalid port in --url {url}"))?;
        Ok((host.to_string(), port))
    } else {
        Ok((hostport.to_string(), 9200))
    }
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

fn run(cfg: BrainCfg) -> Result<i32> {
    let t0 = Instant::now();

    // ── the folder first: no server side effects for an empty ask ────────
    if !cfg.root.exists() {
        bail!("{} does not exist", cfg.root.display());
    }
    let files = walk::walk(&cfg.root, false)?;
    if files.is_empty() {
        eprintln!(
            "xerj brain: no files under {} — nothing to build a brain from.\n\
             point it at a folder with notes or documents in it (markdown, text,\n\
             html, pdf, docx, csv, json …), e.g. `xerj brain ~/notes`.",
            cfg.root.display()
        );
        return Ok(1);
    }
    let total_mb = files.iter().map(|f| f.size).sum::<u64>() >> 20;
    let brain = match &cfg.brain {
        Some(b) => b.clone(),
        None => {
            let derived = xerj_autoindex::derive_brain_name(&cfg.root);
            detect::validate_brain(&derived).map_err(|reason| {
                anyhow::anyhow!(
                    "folder name '{derived}' is not usable as a brain name ({reason}) — \
                     pass one with --brain <name>"
                )
            })?;
            derived
        }
    };
    eprintln!(
        "brain '{brain}': {} files ({total_mb} MB) under {}",
        files.len(),
        cfg.root.display()
    );

    // ── server: attach if something answers, boot if nothing does ────────
    let (host, port) = parse_http_url(&cfg.url)?;
    let data_dir = cfg.data_dir.clone().unwrap_or_else(default_data_dir);
    let probe = Es::new(&cfg.url, None)?;
    let mut setup_link: Option<String> = None;
    match probe.get_status("/health/ready") {
        Ok(200) => {
            eprintln!("attached to the running xerj server at {}", cfg.url);
        }
        Ok(status) => bail!(
            "something is listening at {} but it does not look like xerj \
             (GET /health/ready → HTTP {status}). Stop it, or point --url at your xerj server",
            cfg.url
        ),
        Err(_) => {
            if !is_local_host(&host) {
                bail!(
                    "no server answers at {} and this command only boots servers on \
                     localhost — start xerj on that host, then re-run",
                    cfg.url
                );
            }
            let booted = boot_server(&data_dir, port)?;
            eprintln!(
                "booted xerj server (pid {}) — data: {}, log: {}",
                booted.pid,
                data_dir.display(),
                booted.log_path.display()
            );
            wait_ready(&probe, &booted)?;
            // First launch on an empty data dir: the server's bootstrap
            // banner (xerj-console-api::bootstrap) carries the single-use
            // passkey-setup link. Relay it. Absent on an already-enrolled
            // data dir — then the console's normal passkey login applies.
            setup_link = find_setup_link(&booted.log_path);
        }
    }

    // ── credentials: the server's own admin key, never a new auth path ───
    let api_key = cfg.api_key.clone().or_else(|| {
        let path = data_dir.join("admin.key");
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });
    let es = Es::new(&cfg.url, api_key.clone())?;
    match es.get_status("/") {
        Ok(200) => {}
        Ok(401) | Ok(403) => bail!(
            "the server at {} rejected our credentials. Pass --api-key <key> (or set \
             XERJ_API_KEY); for a server this command booted, the key lives at {}",
            cfg.url,
            data_dir.join("admin.key").display()
        ),
        Ok(status) => bail!("unexpected HTTP {status} from GET {}/", cfg.url),
        Err(e) => return Err(e.context(format!("server at {} stopped answering", cfg.url))),
    }

    // ── index: the autoindex pipeline, graph detection on ────────────────
    let (code, run_doc) =
        match xerj_autoindex::run_index_report(index_cfg(&cfg, &brain, api_key.clone())) {
            Ok(report) => report,
            // #195's zero-live verification fires INSIDE `run_index_report`,
            // before the probe below ever runs. A resume journal that outlived
            // a wiped data directory reaches it first — the run resumes, skips
            // every already-done file, and finds nothing live — so propagating
            // it unchanged hands the operator raw verification prose for
            // exactly the case `journal_server_disagreement()` was written to
            // explain, with no `xerj brain --fresh` recovery in it. Classify it
            // here instead. Every other failure keeps its own message, and so
            // does this one when the probe fails or contradicts it: a probe
            // that cannot answer is not evidence of a wiped destination.
            Err(error) => {
                if error.is::<xerj_autoindex::ZeroLiveVerificationError>() {
                    if let Ok(live_nodes) = live_node_docs(&es, &brain) {
                        if zero_live_is_journal_server_disagreement(&error, live_nodes, cfg.fresh) {
                            bail!("{}", journal_server_disagreement(&cfg, &brain));
                        }
                    }
                }
                return Err(error);
            }
        };

    let run_u64 = |doc: &Option<Value>, key: &str| {
        doc.as_ref()
            .and_then(|d| d.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    // `records_total` is run-scoped: a resumed re-run over an already-indexed
    // folder legitimately reports 0. Server truth decides what that means.
    if needs_live_node_probe(run_u64(&run_doc, "records_total")) {
        let live_nodes = live_node_docs(&es, &brain)?;
        if journal_server_disagrees(run_u64(&run_doc, "files_indexed"), live_nodes, cfg.fresh) {
            bail!("{}", journal_server_disagreement(&cfg, &brain));
        }
    }

    // Server truth for every surface below: run-scoped counters read 0 on a
    // converged re-run, but the brain is live on the server all the same.
    let records_live = match run_u64(&run_doc, "records_total") {
        0 => live_node_docs(&es, &brain)?.unwrap_or(0),
        n => n,
    };
    if records_live == 0 {
        eprintln!(
            "\nxerj brain: nothing indexable under {} — {} files seen, 0 records live.\n\
             autoindex reads text, markdown, html, pdf, docx, csv, json/ndjson, xml,\n\
             yaml, logs and sql dumps; unreadable files are junk-filed, never guessed.\n\
             point it at the folder where your notes actually live.",
            cfg.root.display(),
            files.len()
        );
        return Ok(1);
    }

    // Live edge count straight from the edges index (same probe as
    // `autoindex map`): honest across resumed re-runs, where per-run
    // counters would read 0 on an already-converged corpus.
    let edges_index = detect::edges_index_name(&brain);
    let live_edges = es
        .search(
            &edges_index,
            &json!({
                "size": 0,
                "track_total_hits": true,
                "query": {"bool": {
                    "filter": [{"exists": {"field": "src"}}],
                    "must_not": [{"exists": {"field": "invalid_at"}}]
                }}
            }),
        )
        .ok()
        .and_then(|v| v.pointer("/hits/total/value").and_then(Value::as_u64))
        .unwrap_or(0);

    let console_url = format!("{}/_xerj-console/#/second-brain?brain={brain}", cfg.url);
    // `next` rides the setup link's fragment; the setup page redirects into
    // this hash route after passkey enrollment. `brain` is a validated slug
    // ([a-z0-9-]), so only the literal `?`/`=` need escaping.
    let setup_link = setup_link.map(|l| format!("{l}&next=second-brain%3Fbrain%3D{brain}"));

    if live_edges == 0 {
        println!(
            "\nyour second brain indexed, but no links were found — {} files, {} records, 0 links",
            files.len(),
            records_live
        );
        println!(
            "  notes connect through [[wikilinks]], relative markdown/html links, and\n\
             \x20 shared folders (2+ files in one directory chain together); a single\n\
             \x20 isolated file has nothing to link to. The console still shows your\n\
             \x20 documents and will pick up links on the next run:"
        );
        println!("  → {console_url}");
        if let Some(link) = &setup_link {
            println!("  one-time passkey setup (open once, valid 30 min):");
            println!("  → {link}");
        }
        return Ok(code);
    }

    println!(
        "\n✓ your second brain is ready — {} files, {} links, {:.1}s",
        files.len(),
        live_edges,
        t0.elapsed().as_secs_f64()
    );
    println!("  → {console_url}");
    // The credential this run used, carried into the printed command. Without
    // it the `xerj-mcp` line 401s against the server `brain` just booted —
    // auth is on by default and `brain` read `<data-dir>/admin.key` itself to
    // get past it (ONBOARDING-401-REPRO.md §3). `$XERJ_API_KEY` when the
    // caller already exported it, so the secret is not echoed needlessly.
    println!(
        "  agents: {} xerj-mcp",
        mcp_env(&cfg.url, api_key.as_deref())
    );
    // Read once, at the end of a run that just finished — deliberately NOT on
    // every search response, where it would cost tokens on every single call
    // to say the same thing. Plain ASCII, one line, matching the surrounding
    // human-facing summary; nothing here asks anyone (or any agent) to write
    // praise into a commit message, comment, or PR.
    println!("  if this saved you time, tell a teammate — that is how it spreads.");
    if let Some(link) = &setup_link {
        println!("  one-time passkey setup (open once, valid 30 min):");
        println!("  → {link}");
    }
    if cfg.no_open {
        println!("  browser not opened (--no-open); use the links above.");
    } else {
        // Open the setup link when one was minted (first launch enrolls a
        // passkey, then lands on the brain); otherwise the console itself.
        let target = setup_link.as_deref().unwrap_or(&console_url);
        match open_browser(target) {
            Ok(()) => println!("  opening your browser…"),
            Err(e) => println!("  could not open a browser ({e}); use the links above."),
        }
    }
    Ok(code)
}

/// Count the node documents live on the server behind `brain`, via the brain
/// meta doc's `nodes_index` (SECOND_BRAIN_SPEC §2.5 — autoindex writes the
/// comma-list of its dataset indices there). Absence is distinct from an
/// authoritative zero, and probe failures are never converted into reset
/// authorization.
fn live_node_docs(es: &Es, brain: &str) -> Result<Option<u64>> {
    let edges_index = detect::edges_index_name(brain);
    let Some(meta) = es.get_doc(&edges_index, detect::BRAIN_META_ID)? else {
        return Ok(None);
    };
    // `Es::get_doc` returns the document `_source` itself.
    // A meta doc without `nodes_index` predates the field (or was written by
    // something other than autoindex). That is absence of evidence, not a
    // failure: report it as unknown instead of hard-failing every brain that
    // an older autoindex wrote.
    let Some(nodes_index) = meta
        .get("nodes_index")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    // A meta doc that outlived its nodes index is precisely the wiped-data-dir
    // case this probe exists to catch, so the index being gone must reach
    // `journal_server_disagreement()` as absence. Propagating the 404 would
    // short-circuit the caller and print a raw HTTP error instead of the
    // recovery text. Any other failure still propagates: a probe failure is
    // never converted into reset authorization.
    let Some(response) = es.search_present(
        nodes_index,
        &json!({"size": 0, "track_total_hits": true, "query": {"match_all": {}}}),
    )?
    else {
        return Ok(None);
    };
    response
        .pointer("/hits/total/value")
        .and_then(Value::as_u64)
        .map(Some)
        .context("node-count response has no numeric hits.total.value")
}

fn journal_server_disagreement(cfg: &BrainCfg, brain: &str) -> String {
    let root = std::fs::canonicalize(&cfg.root).unwrap_or_else(|_| cfg.root.clone());
    let state_dir =
        xerj_autoindex::state::default_state_dir(&root.to_string_lossy(), &cfg.url, "ax");
    format!(
        "the resume journal and server disagree: journal {} says {} is already indexed, \
         but {} has no confirmed live node documents for prefix ax and brain {brain}. No reset \
         was attempted: an absent or zero node probe does not prove that data, catalog, and \
         graph namespaces are empty. If this is the wrong server, restore or point at the data \
         directory this journal was written against. If the data directory really was wiped, \
         rerun with --fresh to rebuild the plan and republish every file in place (ids are \
         idempotent):\n\
         xerj brain {} --url {} --fresh\n\
         Otherwise, after validating or cleaning the old destination, run an isolated rebuild:\n\
         xerj autoindex {} --url {} --state-dir <new-state-dir> --prefix <new-prefix> \
         --brain <new-brain>\n\
         For a secured endpoint, set XERJ_API_KEY or add --api-key without placing its value in \
         logs. Validate the new target before switching readers",
        state_dir.display(),
        root.display(),
        cfg.url,
        root.display(),
        cfg.url,
        root.display(),
        cfg.url,
    )
}

fn journal_server_disagrees(files_indexed: u64, live_nodes: Option<u64>, fresh: bool) -> bool {
    files_indexed > 0 && matches!(live_nodes, None | Some(0)) && !fresh
}

/// A failed indexing run is the wiped-destination case only when the failure
/// is autoindex's own zero-live verification (#195) *and* the live-node probe
/// agrees that nothing is there. The journal's completed-file count is the
/// claim being contradicted, so it is what the disagreement test reads —
/// the run summary that would normally carry `files_indexed` is never
/// produced when the pipeline fails.
fn zero_live_is_journal_server_disagreement(
    error: &anyhow::Error,
    live_nodes: Option<u64>,
    fresh: bool,
) -> bool {
    error
        .downcast_ref::<xerj_autoindex::ZeroLiveVerificationError>()
        .is_some_and(|zero_live| {
            journal_server_disagrees(zero_live.files_done_journaled as u64, live_nodes, fresh)
        })
}

fn needs_live_node_probe(records_total: u64) -> bool {
    records_total == 0
}

/// The environment prefix for the `xerj-mcp` line in the success banner.
///
/// `xerj brain` resolves a credential (from `--api-key`, `XERJ_API_KEY`, or
/// `<data-dir>/admin.key`) and uses it for the whole run, but used to print an
/// `XERJ_URL=… xerj-mcp` hint with no credential in it at all — so the agent
/// command the banner suggests 401s against the very server the banner is
/// announcing. Carry the run's key into it.
///
/// `env_key` is the ambient `XERJ_API_KEY`: when it already holds this key the
/// hint says `$XERJ_API_KEY` rather than echoing the admin secret into output
/// that gets pasted into issues.
fn mcp_env(url: &str, api_key: Option<&str>) -> String {
    mcp_env_with(url, api_key, std::env::var("XERJ_API_KEY").ok().as_deref())
}

fn mcp_env_with(url: &str, api_key: Option<&str>, env_key: Option<&str>) -> String {
    match api_key {
        // Open server (`--insecure` / auth off): no credential to carry.
        None => format!("XERJ_URL={url}"),
        Some(key) if env_key == Some(key) => {
            format!("XERJ_URL={url} XERJ_API_KEY=\"$XERJ_API_KEY\"")
        }
        Some(key) => format!("XERJ_URL={url} XERJ_API_KEY=\"{key}\""),
    }
}

fn index_cfg(cfg: &BrainCfg, brain: &str, api_key: Option<String>) -> IndexCfg {
    // Same resource policy as `xerj autoindex` itself — `xerj brain` composes
    // autoindex, so it must not invent its own worker counts (#240).
    const BULK_MB: usize = 8;
    let plan = xerj_autoindex::resources::plan(None, None, BULK_MB);
    IndexCfg {
        root: cfg.root.clone(),
        url: cfg.url.clone(),
        api_key,
        // `brain` resolves its own credential (its data dir's admin.key) and
        // renders hints itself, so there is no discovered-file path to thread
        // through the autoindex hint path here.
        api_key_file: None,
        workers: plan.index_workers,
        scan_workers: plan.scan_threads,
        pdf_workers: plan.pdf_workers,
        resource_notes: plan.notes,
        // #768: the XERJ_URL-vs-`--url` mismatch is a CLI-parse concern; the
        // brain path takes its endpoint from BrainCfg directly and never reads
        // the env var, so there is no ignored-XERJ_URL warning to carry here.
        xerj_url_note: None,
        pdf_timeout_secs: 120,
        bulk_mb: BULK_MB,
        bulk_timeout_secs: 300,
        snapshot_max_bytes: 64u64 << 30,
        prefix: "ax".into(),
        state_dir: None,
        fresh: cfg.fresh,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        stub_globs: Vec::new(),
        // `xerj brain` composes autoindex, so it inherits its ignore rules
        // rather than inventing its own: build output does not belong in a
        // second brain either (#276).
        ignore: xerj_autoindex::ignore_rules::IgnoreOptions::default(),
        max_file_gb: 2,
        sample: 500,
        no_semantic: false,
        brain: Some(brain.to_string()),
        no_graph: false,
        dry_run: false,
        // The estimate/decision gate is an `xerj autoindex` CLI feature, and
        // `xerj brain` has no `--approve` of its own to answer it with. Arming
        // it here would stop the run with exit 4 and instructions the caller
        // literally cannot follow — so it is off, explicitly, until `brain`
        // grows its own answer flag. The estimate itself is still printed.
        max_minutes: 0,
        approve: None,
        json: false,
        quiet: false,
        // `xerj brain` is a foreground command a human watches; it gets the
        // same auto-resolved progress surface as `xerj autoindex` (#241).
        progress: xerj_autoindex::progress::ProgressMode::Auto,
        progress_interval: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Booting the server
// ─────────────────────────────────────────────────────────────────────────────

struct BootedServer {
    pid: u32,
    log_path: PathBuf,
}

/// Spawn `current_exe() --config <data-dir>/xerj-brain.toml` detached, with
/// stdout+stderr appended to `<data-dir>/server.log`. The rest/grpc ports are
/// pinned to `es_port+1`/`es_port+2` so a non-default `--url` port yields a
/// fully non-colliding triple deterministically.
fn boot_server(data_dir: &Path, es_port: u16) -> Result<BootedServer> {
    if es_port > u16::MAX - 2 {
        bail!("--url port {es_port} is too high — the booted server also needs ports +1 and +2");
    }
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;

    let config_path = data_dir.join("xerj-brain.toml");
    // serde_json string escaping is valid TOML basic-string escaping for the
    // characters paths can contain (`\`, `"`, control chars → \uXXXX).
    let dir_toml = serde_json::to_string(&data_dir.to_string_lossy())?;
    std::fs::write(
        &config_path,
        format!(
            "# written by `xerj brain` on every boot it performs — edit ports here\n\
             # only if you also change the --url this command is given.\n\
             [server]\n\
             data_dir = {dir_toml}\n\
             es_compat_port = {es_port}\n\
             rest_port = {}\n\
             grpc_port = {}\n",
            es_port + 1,
            es_port + 2,
        ),
    )
    .with_context(|| format!("write {}", config_path.display()))?;

    let log_path = data_dir.join("server.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let log_err = log.try_clone()?;

    let exe = std::env::current_exe().context("locate the xerj binary")?;
    let mut command = Command::new(exe);
    command
        .arg("--config")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // Detach: its own process group, so the server outlives this command and
    // never receives our Ctrl-C.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().context("spawn the xerj server")?;
    let pid = child.id();
    // Best effort — the pid file is a convenience for `kill $(cat …)`.
    let _ =
        std::fs::File::create(data_dir.join("server.pid")).and_then(|mut f| writeln!(f, "{pid}"));
    Ok(BootedServer { pid, log_path })
}

fn wait_ready(probe: &Es, booted: &BootedServer) -> Result<()> {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    loop {
        if matches!(probe.get_status("/health/ready"), Ok(200)) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!(
                "the booted server (pid {}) did not become ready within {}s — its log:\n{}",
                booted.pid,
                BOOT_TIMEOUT.as_secs(),
                log_tail(&booted.log_path, 30)
            );
        }
        // A dead child never becomes ready; surface its log immediately.
        // (`try_wait` is unavailable here — the child is detached — so probe
        // liveness via signal 0 on unix, and rely on the deadline elsewhere.)
        #[cfg(unix)]
        {
            let alive = unsafe { libc::kill(booted.pid as libc::pid_t, 0) } == 0;
            if !alive {
                bail!(
                    "the booted server (pid {}) exited before becoming ready — its log:\n{}",
                    booted.pid,
                    log_tail(&booted.log_path, 30)
                );
            }
        }
        std::thread::sleep(BOOT_POLL);
    }
}

fn log_tail(path: &Path, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(e) => format!("(log unreadable: {e})"),
    }
}

/// Fish the first-launch passkey-setup link out of the freshly-booted
/// server's log. The bordered banner wraps the URL in box-drawing chars and
/// padding, but the URL itself never contains whitespace, so scan from the
/// `http` before the marker to the first whitespace/border char after it.
fn find_setup_link(log_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log_path).ok()?;
    for line in text.lines().rev() {
        let Some(marker) = line.find("/_xerj-console/setup#token=") else {
            continue;
        };
        let Some(start) = line[..marker].rfind("http") else {
            continue;
        };
        let tail = &line[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '│')
            .unwrap_or(tail.len());
        return Some(tail[..end].to_string());
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Opening the browser
// ─────────────────────────────────────────────────────────────────────────────

/// Platform opener behind two env overrides. No new dependency: the command
/// is spawned fire-and-forget (some openers block for the browser's whole
/// lifetime, so waiting is wrong), and any spawn error is reported by the
/// caller next to the always-printed URL — a headless/SSH user is never
/// stranded.
///
/// `XERJ_BROWSER` (then `BROWSER`) names a command invoked with the URL as
/// its single argument.
fn open_browser(url: &str) -> Result<()> {
    let mut command = if let Some(over) = std::env::var("XERJ_BROWSER")
        .ok()
        .or_else(|| std::env::var("BROWSER").ok())
        .filter(|s| !s.trim().is_empty())
    {
        let mut c = Command::new(over);
        c.arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        // `start`'s first quoted arg is the window title — keep it empty so
        // the URL is not eaten as a title.
        c.args(["/C", "start", ""]).arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONBOARDING-401-REPRO.md §3: `brain` resolves a credential itself (from
    /// `<data-dir>/admin.key` when nothing else supplies one) and then printed
    /// an `XERJ_URL=… xerj-mcp` hint with no credential in it — a command that
    /// 401s against the very server the success banner is announcing.
    #[test]
    fn mcp_hint_carries_the_runs_credential() {
        // Key from admin.key / --api-key: nothing else in the shell holds it.
        let hint = mcp_env_with("http://localhost:9510", Some("s3cret"), None);
        assert_eq!(
            hint, "XERJ_URL=http://localhost:9510 XERJ_API_KEY=\"s3cret\"",
            "the mcp hint must carry the run's key"
        );

        // Already exported: reference the variable, don't echo the secret.
        let hint = mcp_env_with("http://localhost:9200", Some("s3cret"), Some("s3cret"));
        assert!(!hint.contains("s3cret"), "must not echo the secret: {hint}");
        assert!(
            hint.contains("XERJ_API_KEY=\"$XERJ_API_KEY\""),
            "must reference the exported variable: {hint}"
        );

        // A stale/different env value must not shadow the run's credential.
        let hint = mcp_env_with("http://localhost:9200", Some("s3cret"), Some("other"));
        assert!(hint.contains("XERJ_API_KEY=\"s3cret\""), "{hint}");

        // Auth-free server: no credential to carry, and none invented.
        assert_eq!(
            mcp_env_with("http://localhost:9200", None, Some("ignored")),
            "XERJ_URL=http://localhost:9200"
        );
    }

    #[test]
    fn url_parsing_defaults_port_and_rejects_https() {
        assert_eq!(
            parse_http_url("http://localhost:9200").unwrap(),
            ("localhost".into(), 9200)
        );
        assert_eq!(
            parse_http_url("http://localhost").unwrap(),
            ("localhost".into(), 9200)
        );
        assert_eq!(
            parse_http_url("http://10.0.0.7:9300").unwrap(),
            ("10.0.0.7".into(), 9300)
        );
        assert!(parse_http_url("https://example.com:9200").is_err());
    }

    #[test]
    fn parse_requires_folder_and_validates_brain() {
        let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse(strs(&[])).unwrap().is_none(), "no folder → help");
        assert!(parse(strs(&["--brain", "kb-edges", "notes"])).is_err());
        let cfg = parse(strs(&["notes", "--no-open", "--brain", "kb"]))
            .unwrap()
            .unwrap();
        assert!(cfg.no_open);
        assert_eq!(cfg.brain.as_deref(), Some("kb"));
        assert_eq!(cfg.url, "http://localhost:9200");
    }

    #[test]
    fn journal_server_disagreement_refuses_reset_with_executable_recovery() {
        let cfg = BrainCfg {
            root: PathBuf::from("/corpus/notes"),
            brain: Some("team-notes".into()),
            url: "http://localhost:9200".into(),
            data_dir: None,
            api_key: None,
            fresh: false,
            no_open: true,
        };
        let message = journal_server_disagreement(&cfg, "team-notes");
        assert!(message.contains("resume journal and server disagree"));
        assert!(message.contains("No reset was attempted"));
        assert!(message.contains("http://localhost:9200"));
        assert!(message.contains("prefix ax"));
        assert!(message.contains("brain team-notes"));
        assert!(message.contains("xerj brain /corpus/notes"));
        assert!(message.contains("--fresh"));
        assert!(message.contains("xerj autoindex /corpus/notes"));
        assert!(message.contains("--url http://localhost:9200"));
        assert!(message.contains("--state-dir <new-state-dir>"));
        assert!(message.contains("--prefix <new-prefix>"));
        assert!(message.contains("--brain <new-brain>"));
        assert!(message.contains("XERJ_API_KEY"));
    }

    #[test]
    fn disagreement_decision_refuses_absent_and_zero_but_not_positive_server_truth() {
        assert!(
            !needs_live_node_probe(1),
            "a run that indexed a new note must not enter resume probing"
        );
        assert!(needs_live_node_probe(0));
        assert!(journal_server_disagrees(1, None, false));
        assert!(journal_server_disagrees(1, Some(0), false));
        assert!(!journal_server_disagrees(1, Some(7), false));
        assert!(!journal_server_disagrees(0, Some(0), false));
        assert!(!journal_server_disagrees(1, Some(0), true));
    }

    /// The wiped-data-dir case never reaches the probe above on its own: the
    /// pipeline's own zero-live verification fails first. Unless that failure
    /// is classified, the operator reads verification prose instead of the
    /// recovery, which is what CI's use-case smoke phase 5 caught.
    #[test]
    fn a_zero_live_verification_failure_is_read_as_the_wiped_destination() {
        let zero_live: anyhow::Error = xerj_autoindex::ZeroLiveVerificationError {
            journal_records: 42,
            files_done_journaled: 7,
            dataset_indices: 2,
        }
        .into();
        assert!(zero_live_is_journal_server_disagreement(
            &zero_live, None, false
        ));
        assert!(zero_live_is_journal_server_disagreement(
            &zero_live,
            Some(0),
            false
        ));
        // Server truth contradicts the verification: keep the original error.
        assert!(!zero_live_is_journal_server_disagreement(
            &zero_live,
            Some(9),
            false
        ));
        // `--fresh` republishes in place, so a zero-live failure under it is
        // a real write problem, not a stale journal.
        assert!(!zero_live_is_journal_server_disagreement(
            &zero_live, None, true
        ));
        // Every other failure keeps its own message.
        assert!(!zero_live_is_journal_server_disagreement(
            &anyhow::anyhow!("server at http://localhost:9200 stopped answering"),
            None,
            false
        ));
    }

    /// The classified message must still be the recovery text, not the
    /// verification text — the two are distinguishable by their first clause.
    #[test]
    fn zero_live_verification_prose_is_not_the_recovery_prose() {
        let verification = xerj_autoindex::ZeroLiveVerificationError {
            journal_records: 3,
            files_done_journaled: 1,
            dataset_indices: 1,
        }
        .to_string();
        assert!(
            verification.contains("0 documents are live"),
            "{verification}"
        );
        assert!(
            !verification.contains("resume journal and server disagree"),
            "{verification}"
        );
    }

    #[test]
    fn setup_link_is_scraped_from_bordered_and_bare_banner_lines() {
        let dir = std::env::temp_dir().join(format!("xerj-brain-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("server.log");

        std::fs::write(
            &log,
            "│   http://localhost:9200/_xerj-console/setup#token=AbC-_9                    │\n",
        )
        .unwrap();
        assert_eq!(
            find_setup_link(&log).as_deref(),
            Some("http://localhost:9200/_xerj-console/setup#token=AbC-_9")
        );

        std::fs::write(
            &log,
            "├────┤\n  http://localhost:19200/_xerj-console/setup#token=zz\n├────┤\n",
        )
        .unwrap();
        assert_eq!(
            find_setup_link(&log).as_deref(),
            Some("http://localhost:19200/_xerj-console/setup#token=zz")
        );

        std::fs::write(&log, "no banner here\n").unwrap();
        assert_eq!(find_setup_link(&log), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
