//! Hand-rolled arg parser (house style of xerj-server — no clap).

use crate::ignore_rules::IgnoreOptions;
use crate::progress::ProgressMode;
use std::path::PathBuf;
use std::time::Duration;

/// Largest `--bulk-mb` accepted. Past this a single bulk body stops being a
/// unit of work and starts being a memory incident on the server.
pub const MAX_BULK_MB: usize = 24;

#[derive(Debug, Clone)]
pub struct IndexCfg {
    pub root: PathBuf,
    pub url: String,
    pub api_key: Option<String>,
    /// Phase-B index workers (concurrent bulk senders).
    pub workers: usize,
    /// Phase-A scan pool width (content hashing, sniffing, sampling). Both come
    /// from `--workers`; they differ only when the memory safe zone cannot pay
    /// for as many in-flight bulk buffers as the machine has cores.
    pub scan_workers: usize,
    pub pdf_workers: usize,
    /// What the machine forced on this run, printed once at start-up.
    pub resource_notes: Vec<String>,
    pub pdf_timeout_secs: u64,
    pub bulk_mb: usize,
    pub bulk_timeout_secs: u64,
    pub snapshot_max_bytes: u64,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
    pub fresh: bool,
    pub follow_symlinks: bool,
    /// `--stub <glob>` patterns: matching files are indexed as one
    /// existence-only name card, contents never opened.
    pub stub_globs: Vec<String>,
    /// `.gitignore` / `.xerjignore` / built-in build-output rules (#276).
    /// `deep_count` is set from `--dry-run`.
    pub ignore: IgnoreOptions,
    pub max_file_gb: u64,
    pub sample: usize,
    pub no_semantic: bool,
    /// Second-brain name; None derives it from the root folder basename.
    pub brain: Option<String>,
    /// Disable edge detection entirely (no `.xerj-memory-*-edges` writes).
    pub no_graph: bool,
    pub dry_run: bool,
    pub json: bool,
    pub quiet: bool,
    /// Progress surface. Orthogonal to `json`: `--json` shapes *stdout* (the
    /// result), `--progress` shapes *stderr* (liveness).
    pub progress: ProgressMode,
    /// Progress cadence. `None` means "the surface's default" — 1 s on a
    /// terminal, 5 s for a pipe.
    pub progress_interval: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct MapCfg {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub json: bool,
    pub dataset: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StatusCfg {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Cmd {
    Index(IndexCfg),
    Map(MapCfg),
    Status(StatusCfg),
    Help,
}

const FRESH_HELP: &str =
    "ignore an existing resume journal and restart, rebuild the plan in place\n\
                                  (ids stay idempotent); it never resets destination records,\n\
                                  and is refused on a durable corpus generation — see\n\
                                  RESUME POLICY";
const RESUME_POLICY_HELP: &str =
    "generated --no-graph journals reconcile add, change, delete, rename, and no-op runs; \
a --no-graph state directory written before the generation format must be rebuilt into a new \
--state-dir and --prefix; graph-enabled journals keep the existing crash-resume behaviour";

pub fn print_help() {
    println!(
        "xerj autoindex — point it at any folder and make the contents AI-searchable, zero config\n\
         \n\
         USAGE:\n\
             xerj autoindex <folder> [OPTIONS]     discover + index a folder\n\
             xerj autoindex map [OPTIONS]          print the discovered data map\n\
             xerj autoindex status [OPTIONS]       resume-journal + index progress view\n\
         \n\
         OPTIONS:\n\
             --url <U>            ES-compat endpoint (default http://localhost:9200)\n\
             --api-key <K>        Authorization header (or env XERJ_API_KEY)\n\
             --workers <N>        workers for BOTH phases — content hashing/sniffing and\n\
                                  indexing (default: every core, reduced if the memory\n\
                                  safe zone cannot pay for that many bulk buffers; a\n\
                                  value you pass is honoured and only warned about;\n\
                                  valid range 1..=1024)\n\
             --pdf-workers <N>    concurrent PDF parser processes (default min(cores,4); max 4,\n\
                                  the default reduced further on a machine with little\n\
                                  free memory)\n\
             --pdf-timeout-secs <N> per-PDF parser timeout (default 120; max 3600)\n\
             --bulk-mb <N>        bulk cut size in MB (default 8)\n\
             --bulk-timeout-secs <N>\n\
                                  bulk HTTP request timeout in seconds (default 300;\n\
                                  valid range 1..=3600)\n\
             --prefix <P>         index prefix (default ax)\n\
             --state-dir <PATH>   resume journal location (default ~/.xerj/autoindex/<hash>/)\n\
             --snapshot-max-gb <N> logical payload cap for sealed source+prepared records\n\
                                  bytes (default 64); excludes filesystem/manifest overhead\n\
             --fresh              {fresh_help}\n\
             --follow-symlinks    follow symlinks (loop-safe); off by default\n\
             --stub <GLOB>        index matching files as ONE existence-only name\n\
                                  card (title + provenance); contents are never\n\
                                  opened. Repeatable. A pattern without '/'\n\
                                  matches file names anywhere ('*.bvh'); with '/'\n\
                                  it matches the root-relative path\n\
                                  ('unity/**/*.csv'). '**' crosses directories\n\
             --no-ignore          index everything: no .gitignore, no .xerjignore, no\n\
                                  .git/info/exclude, no built-in defaults. Hidden files\n\
                                  (.env, .git/, .ssh) stay skipped either way — that is\n\
                                  not an ignore rule, it is what keeps secrets out.\n\
             --no-default-ignores keep the ignore files, drop only the built-in list\n\
                                  ({defaults})\n\
             --max-file-gb <N>    skip+record oversized non-streamable files (default 2)\n\
             --sample <N>         records sampled per file for inference (default 500)\n\
             --no-semantic        skip semantic_text on body fields (pure BM25+keyword)\n\
             --brain <NAME>       second-brain name; relationship edges land in\n\
                                  .xerj-memory-<NAME>-edges (default: folder name slug)\n\
             --no-graph           skip relationship detection (wikilinks, local links,\n\
                                  section order, directory chains) — no edges are written\n\
             --dry-run            walk+sniff+infer, print the plan, index nothing\n\
             --json               machine-readable RESULT on stdout (map: raw catalog docs).\n\
                                  Orthogonal to --progress, which owns stderr.\n\
             --progress <MODE>    liveness on stderr: auto|plain|json|none (default auto).\n\
                                  auto = live redrawn line when stderr is a terminal,\n\
                                  otherwise one parseable line per interval. plain and\n\
                                  json force that shape everywhere (CI, pipes, agents).\n\
             --progress-interval <SECS>\n\
                                  progress cadence, 1..=3600 (default 1 on a terminal,\n\
                                  5 otherwise). This is the guaranteed upper bound on\n\
                                  silence between phases.\n\
             --quiet              errors only (implies --progress none)\n\
             --dataset <SLUG>     (map) show a single dataset\n\
             --help, -h           this help\n\
         \n\
         IGNORE RULES:\n\
             The fastest file is the one that is never read, so junk is dropped during\n\
             the walk, not after it. Honoured, highest precedence first:\n\
               .xerjignore  same syntax as .gitignore, XERJ-only. Use it to exclude\n\
                            something git tracks, or to re-include (!pattern) something\n\
                            git ignores. Outranks .gitignore at any depth.\n\
               .gitignore   including nested ones and negation; the closest file wins.\n\
               .git/info/exclude  per-checkout excludes, including nested checkouts.\n\
               built-in     {defaults}\n\
             Your global gitignore (core.excludesFile) is NOT read: a machine-wide\n\
             preference should not silently decide what is in your index.\n\
             A directory the rules reject is never descended, so nothing inside it is\n\
             stat-ed, hashed or sent. Every run prints what was dropped and by which\n\
             rule; --dry-run additionally counts the files inside each pruned directory\n\
             (bounded — past the budget the count is reported as `at least N`).\n\
             The folder you name is never rejected: if it is itself ignored, it is\n\
             indexed anyway and the run says which rule it would have matched.\n\
         \n\
         PDF EXTRACTION:\n\
             Each PDF uses a fresh process. Limits: 512 MiB input, 32 MiB worker output,\n\
             100,000 pages, and 1.5 GiB address space on Unix. This is crash/resource\n\
             isolation, not a security sandbox; non-Unix platforms have no OS memory cap.\n\
             Image-only pages need OCR. Page parse failures reject the whole PDF instead\n\
             of silently creating a partial index. XERJ_PDF_WORKER_BIN is a trusted,\n\
             developer-only executable override.\n\
             On Linux, fresh runs attempt to retain each validated PDF extraction in\n\
             bounded anonymous storage under --state-dir. Live admission-time checks\n\
             attempt to preserve disk and descriptor headroom; refused artifacts are\n\
             parsed again in phase B and reported under pdf_extraction_reuse. Other\n\
             platforms currently disable this optimization. Frozen-plan resumes skip\n\
             phase A and parse unfinished PDFs once.\n\
             --pdf-workers bounds both phase-A parser processes and phase-B replay\n\
             materialization; --workers does not widen that PDF memory gate.\n\
         \n\
         INCREMENTAL RECONCILIATION:\n\
             Generated journals with --no-graph reconcile added, removed, moved, and changed\n\
             files. Legacy journals and graph-enabled generations remain fail-closed. Each\n\
             changed generation currently copies and prepares the full corpus (O(N)); the\n\
             latest full snapshot remains retained, and cleanup re-reads protected artifacts\n\
             to verify them. --snapshot-max-gb limits logical staged payload bytes before each\n\
             write; it is not a physical disk-space or peak-allocation guarantee.\n\
         \n\
         EMBEDDINGS:\n\
             autoindex sends semantic_text to the running server; it does not choose the\n\
             server's embedding backend. The default is lexical (not neural). For the\n\
             experimental ONNX backend, start xerj with `--embed-mode onnx-experimental\n\
             --onnx-model MODEL.onnx --onnx-tokenizer tokenizer.json`, then run autoindex.\n\
             ONNX runs only for fields inferred as semantic_text (normally long body text;\n\
             short/structured datasets may infer none). Use --dry-run or `autoindex map` to\n\
             confirm a semantic field before attributing an indexing result to embeddings.\n\
         \n\
         PROGRESS STREAM:\n\
             stdout is the RESULT, stderr is PROGRESS — pipe them separately.\n\
             Every run that reaches an exit — success OR error — ends with one\n\
             terminal line, in every progress mode EXCEPT `none` (which --quiet\n\
             selects), so an outcome never has to be guessed from silence:\n\
               xerj-done ok=true exit=3 reason=completed-with-junk wall=57.6s …\n\
             --quiet/--progress none prints no progress and NO terminal line\n\
             (only a fatal `error:` line, if any) — poll `autoindex status\n\
             --state-dir <dir>` or read the exit code instead of waiting for\n\
             output that never comes.\n\
             (A run killed by a signal cannot print one either; a missing\n\
             terminal line after the process is gone means it died, not that it\n\
             finished.)\n\
             --progress plain writes TWO lines per tick, in one write:\n\
             xerj-bar [######################--] 93.4% | index | 8082/8083 items | eta 7s\n\
             xerj-progress phase=index basis=bytes pct=93.4 items=8082/8083 eta_s=7.2 …\n\
             `xerj-bar` is the DISPLAY line — self-contained, meant to be shown\n\
             to a person verbatim by whatever is relaying the run. It is spaced\n\
             at most one per 15s, plus one per phase change and never two closer\n\
             than 2s, so it does not flood a transcript. Short phases therefore\n\
             draw fewer bars than transitions — read the machine line for those.\n\
             `xerj-progress` is the MACHINE line and keeps\n\
             the --progress-interval cadence; parse that one. --progress json\n\
             stays one object per line and carries the same rendered string in\n\
             a `bar` field.\n\
             `pct`/`eta_s` are the literal word `unknown` (JSON null) whenever they\n\
             cannot be computed honestly, never a filler number — and the drawn\n\
             bar obeys the same rule: `[????…]` when there is no denominator,\n\
             and a full bar only at a real 100%.\n\
         \n\
         RESUME POLICY:\n\
             {resume_policy_help}.\n\
             On a graph-enabled or pre-generation journal the durable plan supports no-op\n\
             resume and same-path content replacement. Files added after that plan was frozen\n\
             are reported as skipped and are not indexed; --fresh rebuilds the plan in place\n\
             and picks them up. Removing an indexed file is refused there before any remote\n\
             mutation: its documents are already live and nothing on that path deletes them.\n\
             Restore the file and rerun, or rebuild — in place by deleting the published\n\
             indices and the state directory, or isolated under a new --state-dir, --prefix\n\
             and --brain (or --no-graph), validated before you switch readers.\n\
             --fresh is not cleanup and is not destination reconciliation: it never removes\n\
             stale records from the destination, and it is refused outright once a durable\n\
             corpus generation exists (re-run without it — the generated path reconciles the\n\
             change incrementally). For an independent rebuild use a new --state-dir and a\n\
             new --prefix, plus a new --brain when graph is enabled (or --no-graph).\n\
             Validate before switching readers; explicitly clean the shared\n\
             autoindex-catalog and old target only after validation.\n\
         \n\
         EXIT CODES: 0 complete; 3 completed-with-junk (junk recorded, never fatal);\n\
                     2 usage; 1 endpoint/journal failure, a refused corpus removal, or a\n\
                     refused unsafe state transition\n",
        fresh_help = FRESH_HELP,
        resume_policy_help = RESUME_POLICY_HELP,
        defaults = crate::ignore_rules::DEFAULT_IGNORE_PATTERNS.join(" ")
    );
}

pub fn parse(args: Vec<String>) -> Result<Cmd, String> {
    let mut it = args.into_iter().peekable();
    let mut folder: Option<PathBuf> = None;
    let mut sub: Option<String> = None;

    let mut url = "http://localhost:9200".to_string();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    // Worker counts are decided by `crate::resources::plan` once every flag is
    // known, because the answer depends on --bulk-mb and on the machine. `None`
    // here means "the user did not ask for a number".
    let mut workers: Option<usize> = None;
    let mut pdf_workers: Option<usize> = None;
    let mut pdf_timeout_secs = 120u64;
    let mut bulk_mb = 8usize;
    let mut bulk_timeout_secs = 300u64;
    let mut snapshot_max_bytes = 64u64 << 30;
    let mut bulk_timeout_explicit = false;
    let mut prefix = "ax".to_string();
    let mut state_dir: Option<PathBuf> = None;
    let mut fresh = false;
    let mut follow_symlinks = false;
    let mut stub_globs: Vec<String> = Vec::new();
    let mut no_ignore = false;
    let mut no_default_ignores = false;
    let mut max_file_gb = 2u64;
    let mut sample = 500usize;
    let mut no_semantic = false;
    let mut brain: Option<String> = None;
    let mut no_graph = false;
    let mut dry_run = false;
    let mut json = false;
    let mut quiet = false;
    let mut progress: Option<ProgressMode> = None;
    let mut progress_interval: Option<Duration> = None;
    let mut dataset: Option<String> = None;

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--url" => url = it.next().ok_or("--url needs a value")?,
            "--api-key" => api_key = it.next(),
            "--workers" => {
                // Refused, not clamped: `--workers 0` used to become 1 and
                // `--workers 100000` was taken at face value, both without a
                // word to the user (#204's class).
                workers = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .filter(|n| (1..=crate::resources::MAX_WORKERS).contains(n))
                        .ok_or(format!(
                            "--workers needs a number from 1 to {}",
                            crate::resources::MAX_WORKERS
                        ))?,
                )
            }
            "--pdf-workers" => {
                pdf_workers = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .filter(|n| (1..=crate::resources::MAX_PDF_WORKERS).contains(n))
                        .ok_or(format!(
                            "--pdf-workers needs a number from 1 to {}",
                            crate::resources::MAX_PDF_WORKERS
                        ))?,
                )
            }
            "--pdf-timeout-secs" => {
                pdf_timeout_secs = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=3600).contains(n))
                    .ok_or("--pdf-timeout-secs needs a number from 1 to 3600")?
            }
            "--bulk-mb" => {
                // Was silently clamped into 1..=24 at the end of parsing, so
                // `--bulk-mb 512` ran at 24 and `--bulk-mb 0` at 1, in both
                // cases without telling anyone.
                bulk_mb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=MAX_BULK_MB).contains(n))
                    .ok_or(format!("--bulk-mb needs a number from 1 to {MAX_BULK_MB}"))?
            }
            "--bulk-timeout-secs" => {
                bulk_timeout_explicit = true;
                bulk_timeout_secs = it
                    .next()
                    .ok_or("--bulk-timeout-secs needs a value in seconds")?
                    .parse()
                    .map_err(|_| "--bulk-timeout-secs needs an integer in the range 1..=3600")?;
                if !(1..=3_600).contains(&bulk_timeout_secs) {
                    return Err("--bulk-timeout-secs must be in the range 1..=3600 seconds".into());
                }
            }
            "--snapshot-max-gb" => {
                let gib: u64 = it
                    .next()
                    .ok_or("--snapshot-max-gb needs a positive integer")?
                    .parse()
                    .map_err(|_| "--snapshot-max-gb needs a positive integer")?;
                snapshot_max_bytes = gib
                    .checked_mul(1u64 << 30)
                    .filter(|bytes| *bytes > 0)
                    .ok_or("--snapshot-max-gb is too large")?;
            }
            "--in-flight" => {
                let _ = it.next(); // reserved (bulks are worker-synchronous in v1)
            }
            "--prefix" => prefix = it.next().ok_or("--prefix needs a value")?,
            "--state-dir" => state_dir = it.next().map(PathBuf::from),
            "--fresh" => fresh = true,
            "--follow-symlinks" => follow_symlinks = true,
            "--stub" => stub_globs.push(it.next().ok_or("--stub needs a glob pattern")?),
            "--no-ignore" => no_ignore = true,
            "--no-default-ignores" => no_default_ignores = true,
            "--max-file-gb" => {
                max_file_gb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--max-file-gb needs a number")?
            }
            "--sample" => {
                sample = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--sample needs a number")?
            }
            "--no-semantic" => no_semantic = true,
            "--brain" => {
                let name = it.next().ok_or("--brain needs a value")?;
                // Fail at parse time — a bad brain name after a long Phase A
                // would be a rude place to learn about the '-edges' rule.
                crate::detect::validate_brain(&name)
                    .map_err(|reason| format!("--brain {name}: {reason}"))?;
                brain = Some(name);
            }
            "--no-graph" => no_graph = true,
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "--md" => json = false,
            "--progress" => {
                let raw = it
                    .next()
                    .ok_or("--progress needs a value: auto, plain, json or none")?;
                progress = Some(ProgressMode::parse(&raw)?);
            }
            "--progress-interval" => {
                let secs: u64 = it
                    .next()
                    .ok_or("--progress-interval needs a value in seconds")?
                    .parse()
                    .map_err(|_| {
                        "--progress-interval needs an integer in the range 1..=3600".to_string()
                    })?;
                if !(1..=3_600).contains(&secs) {
                    return Err("--progress-interval must be in the range 1..=3600 seconds".into());
                }
                progress_interval = Some(Duration::from_secs(secs));
            }
            "--quiet" => quiet = true,
            "--dataset" => dataset = it.next(),
            "--help" | "-h" => return Ok(Cmd::Help),
            "map" if sub.is_none() && folder.is_none() => sub = Some("map".into()),
            "status" if sub.is_none() && folder.is_none() => sub = Some("status".into()),
            other if !other.starts_with('-') && folder.is_none() && sub.is_none() => {
                folder = Some(PathBuf::from(other))
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let prefix = crate::dataset::sanitize_slug(&prefix);
    if prefix.is_empty() {
        return Err("--prefix must contain at least one [a-z0-9] character".into());
    }

    // Contradictory progress requests are refused, never silently resolved in
    // one side's favour: accepting a flag we will not honour is the defect
    // class tracked in #204, and "I asked for progress and got none" is the
    // exact complaint this option exists to answer.
    let progress_explicit = progress.is_some() || progress_interval.is_some();
    if quiet {
        match progress {
            Some(ProgressMode::None) | None => {}
            Some(mode) => {
                return Err(format!(
                    "--quiet and --progress {} contradict each other: --quiet means no progress \
                     output. Drop one of the two",
                    mode.as_str()
                ))
            }
        }
    }
    let progress = if quiet {
        ProgressMode::None
    } else {
        progress.unwrap_or(ProgressMode::Auto)
    };
    // Redundant-but-honoured is fine; accepted-and-ignored is the #204 class.
    // `--no-ignore` already removes the built-in defaults, so there is nothing
    // left for `--no-default-ignores` to do — say so instead of taking a flag
    // that changes nothing.
    if no_ignore && no_default_ignores {
        return Err(
            "--no-ignore already turns off the built-in default rules that --no-default-ignores \
             targets. Drop one of the two"
                .into(),
        );
    }
    if progress_interval.is_some() && progress == ProgressMode::None {
        return Err(
            "--progress-interval sets the cadence of a progress stream that --progress none / \
             --quiet turns off. Drop one of the two"
                .into(),
        );
    }

    match (sub.as_deref(), folder) {
        (Some("map"), _) | (Some("status"), _) if progress_explicit => Err(format!(
            "--progress/--progress-interval apply only to indexing, not `autoindex {}`",
            sub.as_deref().unwrap_or_default()
        )),
        (Some("map"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex map`".into())
        }
        (Some("map"), _) => Ok(Cmd::Map(MapCfg {
            url,
            api_key,
            prefix,
            json,
            dataset,
        })),
        (Some("status"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex status`".into())
        }
        (Some("status"), _) => Ok(Cmd::Status(StatusCfg {
            url,
            api_key,
            prefix,
            state_dir,
        })),
        (None, Some(root)) => {
            let plan = crate::resources::plan(workers, pdf_workers, bulk_mb);
            Ok(Cmd::Index(IndexCfg {
                root,
                url,
                api_key,
                workers: plan.index_workers,
                scan_workers: plan.scan_threads,
                pdf_workers: plan.pdf_workers,
                resource_notes: plan.notes,
                pdf_timeout_secs,
                bulk_mb,
                bulk_timeout_secs,
                snapshot_max_bytes,
                prefix,
                state_dir,
                fresh,
                follow_symlinks,
                stub_globs,
                ignore: IgnoreOptions {
                    enabled: !no_ignore,
                    defaults: !no_ignore && !no_default_ignores,
                    // Only --dry-run pays for counting what is inside a pruned
                    // directory; a real run's whole point is not touching it.
                    deep_count: dry_run,
                },
                max_file_gb,
                sample: sample.max(50),
                no_semantic,
                brain,
                no_graph,
                dry_run,
                json,
                quiet,
                progress,
                progress_interval,
            }))
        }
        _ => Ok(Cmd::Help),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Cmd, Duration, ProgressMode};

    fn index(args: &[&str]) -> super::IndexCfg {
        match parse(args.iter().map(|s| s.to_string()).collect()).unwrap() {
            Cmd::Index(cfg) => cfg,
            other => panic!("expected index config, got {other:?}"),
        }
    }

    fn err(args: &[&str]) -> String {
        parse(args.iter().map(|s| s.to_string()).collect()).expect_err("must be refused")
    }

    /// #240 §1/§2: `--workers` capped itself at 8 for no recorded reason and
    /// governed only phase B. It now governs both phases and defaults to the
    /// machine.
    #[test]
    fn workers_defaults_to_the_machine_and_governs_both_phases() {
        let cfg = index(&["data"]);
        let cores = xerj_common::resource::cores();
        assert_eq!(cfg.scan_workers, cores, "phase A gets the whole machine");
        assert!(cfg.workers <= cores && cfg.workers >= 1);
        let asked = index(&["data", "--workers", "3"]);
        assert_eq!(asked.scan_workers, 3, "--workers must bound phase A");
        assert_eq!(asked.workers, 3, "--workers must bound phase B");
    }

    /// An unusable count is a typo. Clamping it silently is the
    /// accepted-and-ignored class this repo keeps re-finding (#204).
    #[test]
    fn unusable_worker_and_bulk_values_are_refused_not_clamped() {
        assert!(err(&["data", "--workers", "0"]).contains("--workers needs a number from 1 to"));
        assert!(err(&["data", "--workers", "99999"]).contains("--workers"));
        assert!(err(&["data", "--workers", "eight"]).contains("--workers"));
        assert!(err(&["data", "--pdf-workers", "8"]).contains("--pdf-workers"));
        // `--bulk-mb 0` used to become 1 and `--bulk-mb 512` used to become 24.
        assert!(err(&["data", "--bulk-mb", "0"]).contains("--bulk-mb needs a number from 1 to 24"));
        assert!(err(&["data", "--bulk-mb", "512"]).contains("--bulk-mb"));
        assert_eq!(index(&["data", "--bulk-mb", "24"]).bulk_mb, 24);
    }

    #[test]
    fn bulk_timeout_defaults_to_300_seconds() {
        assert_eq!(index(&["data"]).bulk_timeout_secs, 300);
    }

    #[test]
    fn snapshot_budget_defaults_and_accepts_gibibytes() {
        assert_eq!(index(&["data"]).snapshot_max_bytes, 64u64 << 30);
        assert_eq!(
            index(&["data", "--snapshot-max-gb", "7"]).snapshot_max_bytes,
            7u64 << 30
        );
        for value in ["0", "nope", "18446744073709551615"] {
            assert!(super::parse(
                ["data", "--snapshot-max-gb", value]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            )
            .is_err());
        }
    }

    #[test]
    fn fresh_help_scopes_the_refusal_and_denies_being_cleanup() {
        let help = super::FRESH_HELP;
        assert!(help.contains("ignore an existing resume journal and restart"));
        assert!(help.contains("durable corpus generation"));
        assert!(help.contains("never resets destination records"));
    }

    #[test]
    fn resume_policy_help_distinguishes_generated_legacy_and_graph_state() {
        let help = super::RESUME_POLICY_HELP;
        for claim in [
            "generated --no-graph journals",
            "add, change, delete, rename, and no-op",
            "written before the generation format must be rebuilt",
            "graph-enabled journals keep the existing crash-resume behaviour",
        ] {
            assert!(help.contains(claim), "missing resume-policy claim: {claim}");
        }
    }

    #[test]
    fn fresh_help_points_at_the_resume_policy_that_bounds_it() {
        assert!(super::FRESH_HELP.contains("rebuild the plan in place"));
        assert!(super::FRESH_HELP.contains("ids stay idempotent"));
        assert!(super::FRESH_HELP.contains("RESUME POLICY"));
    }

    #[test]
    fn graph_is_on_by_default_with_derived_brain() {
        let cfg = index(&["data"]);
        assert!(!cfg.no_graph);
        assert_eq!(cfg.brain, None, "brain derives from the folder at run time");
        assert!(index(&["data", "--no-graph"]).no_graph);
        assert_eq!(
            index(&["data", "--brain", "notes"]).brain.as_deref(),
            Some("notes")
        );
    }

    #[test]
    fn brain_names_are_validated_at_parse_time() {
        for bad in ["kb-edges", "Notes", "-x", "a..b"] {
            let err = parse(
                ["data", "--brain", bad]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )
            .unwrap_err();
            assert!(err.contains("--brain"), "{err}");
        }
    }

    #[test]
    fn bulk_timeout_accepts_custom_bounded_value() {
        assert_eq!(
            index(&["data", "--bulk-timeout-secs", "3600"]).bulk_timeout_secs,
            3600
        );
    }

    #[test]
    fn bulk_timeout_rejects_missing_non_numeric_zero_and_too_large() {
        for args in [
            vec!["data", "--bulk-timeout-secs"],
            vec!["data", "--bulk-timeout-secs", "slow"],
            vec!["data", "--bulk-timeout-secs", "0"],
            vec!["data", "--bulk-timeout-secs", "3601"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("--bulk-timeout-secs"), "{err}");
        }
    }

    #[test]
    fn progress_defaults_to_auto_and_quiet_turns_it_off() {
        let cfg = index(&["data"]);
        assert_eq!(cfg.progress, ProgressMode::Auto);
        assert_eq!(cfg.progress_interval, None);
        assert_eq!(index(&["data", "--quiet"]).progress, ProgressMode::None);
        assert_eq!(
            index(&["data", "--progress", "json"]).progress,
            ProgressMode::Json
        );
        // --json shapes stdout, --progress shapes stderr; they never collide.
        let both = index(&["data", "--json", "--progress", "plain"]);
        assert!(both.json);
        assert_eq!(both.progress, ProgressMode::Plain);
        assert_eq!(
            index(&["data", "--progress-interval", "30"]).progress_interval,
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn contradictory_and_malformed_progress_requests_are_refused() {
        for args in [
            vec!["data", "--progress"],
            vec!["data", "--progress", "tty"],
            vec!["data", "--progress", "yes"],
            vec!["data", "--progress-interval"],
            vec!["data", "--progress-interval", "0"],
            vec!["data", "--progress-interval", "3601"],
            vec!["data", "--progress-interval", "soon"],
            // Accepting either of these and honouring only one half would be a
            // silent lie about what the run will print.
            vec!["data", "--quiet", "--progress", "plain"],
            vec!["data", "--progress", "none", "--progress-interval", "5"],
            vec!["data", "--quiet", "--progress-interval", "5"],
        ] {
            let rendered = args.join(" ");
            let err = match parse(args.into_iter().map(str::to_string).collect()) {
                Err(err) => err,
                Ok(other) => panic!("`{rendered}` must not be accepted, got {other:?}"),
            };
            assert!(
                err.contains("--progress"),
                "`{rendered}` must explain itself: {err}"
            );
        }
        // The one benign combination stays benign.
        assert_eq!(
            index(&["data", "--quiet", "--progress", "none"]).progress,
            ProgressMode::None
        );
    }

    #[test]
    fn progress_flags_are_rejected_for_non_index_subcommands() {
        for args in [
            vec!["map", "--progress", "json"],
            vec!["--progress", "json", "map"],
            vec!["status", "--progress-interval", "5"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("apply only to indexing"), "{err}");
        }
        // --quiet keeps its historical no-op acceptance on map/status.
        assert!(parse(["map", "--quiet"].into_iter().map(str::to_string).collect()).is_ok());
    }

    #[test]
    fn bulk_timeout_is_rejected_for_non_index_subcommands_in_any_position() {
        for args in [
            vec!["map", "--bulk-timeout-secs", "900"],
            vec!["--bulk-timeout-secs", "900", "map"],
            vec!["status", "--bulk-timeout-secs", "900"],
            vec!["--bulk-timeout-secs", "900", "status"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("applies only to indexing"), "{err}");
        }
    }

    /// #276. Ignore rules are on by default — that is the whole point of the
    /// issue — and each flag turns off exactly what it names.
    #[test]
    fn ignore_rules_default_on_and_each_flag_turns_off_what_it_names() {
        let cfg = index(&["data"]);
        assert!(cfg.ignore.enabled);
        assert!(cfg.ignore.defaults);
        assert!(!cfg.ignore.deep_count, "only --dry-run pays for the count");

        let none = index(&["data", "--no-ignore"]);
        assert!(!none.ignore.enabled);
        assert!(!none.ignore.defaults);

        let keep_files = index(&["data", "--no-default-ignores"]);
        assert!(keep_files.ignore.enabled, "ignore files still apply");
        assert!(!keep_files.ignore.defaults);

        assert!(index(&["data", "--dry-run"]).ignore.deep_count);
    }

    /// A flag that cannot change anything is refused rather than accepted and
    /// quietly dropped (#204's class).
    #[test]
    fn no_default_ignores_under_no_ignore_is_refused() {
        let err = err(&["data", "--no-ignore", "--no-default-ignores"]);
        assert!(err.contains("--no-ignore already turns off"), "{err}");
    }
}
