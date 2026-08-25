//! Hand-rolled arg parser (house style of xerj-server — no clap).

use crate::gate::{Approval, DEFAULT_MAX_MINUTES};
use crate::ignore_rules::IgnoreOptions;
use crate::progress::ProgressMode;
use std::path::PathBuf;
use std::time::Duration;

/// Largest `--max-minutes` accepted: one week. Past that the flag is a typo,
/// and `--max-minutes 0` already exists to mean "never ask".
pub const MAX_MAX_MINUTES: u64 = 7 * 24 * 60;

/// Largest `--bulk-mb` accepted. Past this a single bulk body stops being a
/// unit of work and starts being a memory incident on the server.
pub const MAX_BULK_MB: usize = 24;

#[derive(Debug, Clone)]
pub struct IndexCfg {
    pub root: PathBuf,
    pub url: String,
    pub api_key: Option<String>,
    /// Set only when `api_key` was discovered on disk rather than supplied by
    /// the user, so completion hints can reference the file instead of
    /// printing the secret into output people paste into bug reports.
    pub api_key_file: Option<PathBuf>,
    /// Phase-B index workers (concurrent bulk senders).
    pub workers: usize,
    /// Phase-A scan pool width (content hashing, sniffing, sampling). Both come
    /// from `--workers`; they differ only when the memory safe zone cannot pay
    /// for as many in-flight bulk buffers as the machine has cores.
    pub scan_workers: usize,
    pub pdf_workers: usize,
    /// What the machine forced on this run, printed once at start-up.
    pub resource_notes: Vec<String>,
    /// #768: the `XERJ_URL`-ignored safety note (set when `XERJ_URL` is present
    /// but `--url` was not passed). Carried separately from `resource_notes`
    /// because it is a safety warning, not routine progress chatter: it is
    /// emitted with an unconditional `eprintln` (so `--quiet` does not silence a
    /// "you may be writing to the wrong node" message) and mirrored into the
    /// `--json` result, matching how `map`/`status` deliver the same note.
    pub xerj_url_note: Option<String>,
    pub pdf_timeout_secs: u64,
    pub bulk_mb: usize,
    pub bulk_timeout_secs: u64,
    pub snapshot_max_bytes: u64,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
    pub fresh: bool,
    pub follow_symlinks: bool,
    /// `--follow-symlinks-outside-root`: waive the root boundary for followed
    /// links. The hidden-name rule still applies to whatever they resolve to.
    pub follow_symlinks_outside_root: bool,
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
    /// Stop and ask before indexing when phase A's measured estimate exceeds
    /// this many minutes. `0` disables the gate outright.
    pub max_minutes: u64,
    /// An answer to a previous decision request. `None` means "nobody has
    /// answered yet", which is what arms the gate.
    pub approve: Option<Approval>,
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
    /// A startup note when `XERJ_URL` was set but ignored (endpoint comes from
    /// `--url` only). `map` reads the catalog, so a stale var shows the wrong
    /// node's map without a word — same trap as the index path, read side.
    pub xerj_url_note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StatusCfg {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub state_dir: Option<PathBuf>,
    /// See [`MapCfg::xerj_url_note`]: `status` queries the same endpoint, so a
    /// set-but-ignored `XERJ_URL` would report the wrong node's progress.
    pub xerj_url_note: Option<String>,
}

#[derive(Debug)]
pub enum Cmd {
    /// Boxed: `IndexCfg` is an order of magnitude larger than the other
    /// variants, so inlining it made every `Cmd` — including `Help` — pay for
    /// it (`clippy::large_enum_variant`).
    Index(Box<IndexCfg>),
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
    println!("{}", help_text());
}

/// The help text as a value, so tests can assert that a documented flag is
/// still documented instead of trusting a `println!`.
pub fn help_text() -> String {
    help_text_with(xerj_common::feedback::enabled())
}

/// [`help_text`] with the feedback invitation forced on or off, so a test does
/// not have to mutate the process environment to see both shapes.
pub fn help_text_with(feedback: bool) -> String {
    format!(
        "xerj autoindex — point it at any folder and make the contents AI-searchable, zero config\n\
         \n\
         {feedback_block}\
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
             --follow-symlinks    follow symlinks (loop-safe); off by default.\n\
                                  A link is judged by what it RESOLVES to: a\n\
                                  target outside the folder is refused, and one\n\
                                  inside a hidden directory is skipped like any\n\
                                  dotfile, whatever the link itself is called\n\
             --follow-symlinks-outside-root\n\
                                  also follow links that resolve OUTSIDE the\n\
                                  folder. Requires --follow-symlinks. Off by\n\
                                  default; pointing at a folder is not consent\n\
                                  to index whatever it links to. The hidden-file\n\
                                  rule still applies to the target, judged from\n\
                                  where it diverges from your folder — so a\n\
                                  dotted directory the two paths SHARE does not\n\
                                  refuse anything, because your folder is\n\
                                  already inside it\n\
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
             --max-minutes <N>    stop and ask before indexing if phase A's MEASURED estimate\n\
                                  is longer than this (default 10; 0 disables the gate;\n\
                                  max 10080). See ESTIMATE + DECISION GATE below.\n\
             --approve <ID>       answer a decision request: proceed | fast | cancel.\n\
                                  `fast` also applies --no-semantic --no-graph. `narrower`\n\
                                  is NOT accepted here — it means re-running against a\n\
                                  subdirectory, which this flag cannot do.\n\
             --yes, -y            alias for --approve proceed\n\
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
             --quiet              errors only (implies --progress none). The decision gate\n\
                                  NEVER prompts under this flag, even at a terminal — the\n\
                                  question would be silenced with everything else. It emits\n\
                                  the JSON decision request on stdout and exits 4 instead,\n\
                                  exactly like an agent-driven run. See ESTIMATE + DECISION\n\
                                  GATE.\n\
             --dataset <SLUG>     (map) show a single dataset\n\
             --disable-feedback   do not print the feedback invitation above; honoured in\n\
                                  any position, including after --help (env\n\
                                  XERJ_DISABLE_FEEDBACK=true)\n\
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
             Both git-owned kinds stop at a repository boundary, exactly as git does:\n\
             a .gitignore above a nested checkout does not judge files inside it, so\n\
             vendored and submoduled trees keep their own rules. .xerjignore and the\n\
             built-in list are XERJ's, not git's, and apply throughout the folder.\n\
             Your global gitignore (core.excludesFile) is NOT read: a machine-wide\n\
             preference should not silently decide what is in your index.\n\
             A directory the rules reject is never descended, so nothing inside it is\n\
             stat-ed, hashed or sent. Every run prints what was dropped and by which\n\
             rule; --dry-run additionally counts the non-hidden files inside each\n\
             pruned directory (bounded — past the budget the count is reported as\n\
             `at least N`, and xerj-done carries\n\
             ignored_files_in_pruned_dirs_exact=false to say so).\n\
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
             The terminal line also carries code coverage — code_files=N\n\
             code_files_indexed=M code_files_junked=K — so a corpus whose\n\
             source files were ALL dropped cannot print the same line as a\n\
             healthy one. code_files>0 with code_files_indexed=0 is always a\n\
             defect, and the run says so in words as well.\n\
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
             a `bar` field on the same schedule: a string on the ticks that owe\n\
             a bar, null in between.\n\
             Every record is identified by its leading token, and that is\n\
             enforced: paths and other outside text are stripped of control\n\
             characters and bounded before they reach any line, so a crafted\n\
             filename cannot forge a record or repaint your terminal.\n\
             `pct`/`eta_s` are the literal word `unknown` (JSON null) whenever they\n\
             cannot be computed honestly, never a filler number — and the drawn\n\
             bar obeys the same rule: `[????…]` when there is no denominator,\n\
             and a full bar only at a real 100%.\n\
         \n\
         ESTIMATE + DECISION GATE:\n\
             Phase A already reads and parses every file to sniff and sample it, so it\n\
             measures throughput per format family ON THIS MACHINE. autoindex turns that\n\
             into a RANGE (never one confident number) for the indexing phase and prints\n\
             the basis with it. Families phase A never read end to end are named and left\n\
             OUT of the arithmetic instead of being priced at some other family's rate;\n\
             if nothing could be measured, it says so and does not gate.\n\
             The range covers CLIENT-SIDE EXTRACTION only — server indexing, embedding\n\
             and network time are not in it, which is why the gate compares the upper end.\n\
             If that upper end is longer than --max-minutes and no --approve/--yes was\n\
             given, nothing is indexed: a JSON decision request goes to stdout and the\n\
             process exits 4 (a code of its own — 1 is the catch-all for real failures).\n\
             Answer by re-running the same command with --approve proceed|fast|cancel.\n\
             A person at a terminal is prompted instead — but ONLY when the question can\n\
             actually be seen. All three must hold: stdin is a terminal, stderr is a\n\
             terminal, and the progress surface is on. --quiet / --progress none silences\n\
             the question, so those runs are never prompted and never wait on stdin; a\n\
             piped or agent-driven run is not prompted either. Every un-prompted run\n\
             behaves identically: the JSON decision request goes to stdout (which --quiet\n\
             does NOT silence) and the process exits 4. The payload's\n\
             `prompt_not_offered_because` says which of the three was missing.\n\
             autoindex never waits on stdin for a question it did not print.\n\
         \n\
         WORK ORDER:\n\
             Phase B drains source and documents first, then configuration, then\n\
             structured data, then logs and line files, and vendored/generated/minified\n\
             paths last — so stopping early, or searching while it runs, still gives you\n\
             the files you cared about. Inside a band the biggest file starts first (with\n\
             several workers it then runs alongside the rest instead of becoming the\n\
             tail); a single-worker run goes smallest-first instead. One exception: a file\n\
             so large that it outlasts everything ranked above it starts first whatever\n\
             band it is in. The full breakdown is printed with the plan.\n\
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
         EXIT CODES: 0 complete (also: gate answered with --approve cancel);\n\
                     3 completed-with-junk (junk recorded, never fatal), or\n\
                       catalog-alias-sweep-failed — the corpus IS indexed and the\n\
                       journal committed; only the catalog's duplicate-alias cleanup\n\
                       could not run. Read `reason` on the xerj-done line to tell the\n\
                       two apart, and rerun the same command once the reported server\n\
                       condition clears;\n\
                     4 NEEDS A DECISION — the estimate exceeded --max-minutes and\n\
                       nothing was indexed; a JSON decision request is on stdout;\n\
                     2 usage; 1 endpoint/journal failure, a refused corpus removal, or a\n\
                     refused unsafe state transition\n",
        feedback_block = xerj_common::feedback::block(feedback),
        fresh_help = FRESH_HELP,
        resume_policy_help = RESUME_POLICY_HELP,
        defaults = crate::ignore_rules::DEFAULT_IGNORE_PATTERNS.join(" ")
    )
}

/// A startup note for the case where `XERJ_URL` is set but the run did not pass
/// `--url`. `autoindex` resolves its endpoint from `--url` only (default
/// `http://localhost:9200`); `XERJ_URL` is honored by `xerj search` but not
/// here, on purpose, so a stale variable can't silently redirect a write. Silent
/// is the trap though, so name the mismatch and how to act on it. `None` when
/// `--url` was passed, or `XERJ_URL` is unset or empty.
pub(crate) fn xerj_url_ignored_note(xerj_url: Option<&str>, url_explicit: bool) -> Option<String> {
    if url_explicit {
        return None;
    }
    let value = xerj_url?.trim();
    if value.is_empty() {
        return None;
    }
    Some(format!(
        "XERJ_URL={value} is set but ignored here: autoindex takes its endpoint from --url only \
         (default http://localhost:9200). Pass --url {value} to target it."
    ))
}

pub fn parse(args: Vec<String>) -> Result<Cmd, String> {
    let mut it = args.into_iter().peekable();
    let mut folder: Option<PathBuf> = None;
    let mut sub: Option<String> = None;

    let mut url = "http://localhost:9200".to_string();
    // `xerj autoindex` takes its endpoint from `--url` ONLY, unlike `xerj
    // search` which honors `XERJ_URL`. Ignoring the env var here is deliberate:
    // a write must not be redirected by a stale variable. But silent is a trap
    // (it cost a multi-session mis-target debug), so we track whether --url was
    // actually passed and warn when XERJ_URL is set and it was not.
    let mut url_explicit = false;
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    // Set only when the key was discovered on disk, so output can reference the
    // file instead of echoing the secret.
    let mut api_key_file: Option<PathBuf> = None;
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
    let mut follow_symlinks_outside_root = false;
    let mut stub_globs: Vec<String> = Vec::new();
    let mut no_ignore = false;
    let mut no_default_ignores = false;
    let mut max_file_gb = 2u64;
    let mut sample = 500usize;
    let mut no_semantic = false;
    let mut brain: Option<String> = None;
    let mut no_graph = false;
    let mut dry_run = false;
    let mut max_minutes = DEFAULT_MAX_MINUTES;
    let mut max_minutes_explicit = false;
    let mut approve: Option<Approval> = None;
    let mut approve_explicit = false;
    let mut json = false;
    let mut quiet = false;
    let mut progress: Option<ProgressMode> = None;
    let mut progress_interval: Option<Duration> = None;
    let mut dataset: Option<String> = None;

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--url" => {
                url = it.next().ok_or("--url needs a value")?;
                url_explicit = true;
            }
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
            "--follow-symlinks-outside-root" => follow_symlinks_outside_root = true,
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
            "--max-minutes" => {
                max_minutes_explicit = true;
                max_minutes = it
                    .next()
                    .ok_or("--max-minutes needs a number of minutes (0 disables the gate)")?
                    .parse()
                    .map_err(|_| {
                        format!("--max-minutes needs an integer from 0 to {MAX_MAX_MINUTES}")
                    })?;
                if max_minutes > MAX_MAX_MINUTES {
                    return Err(format!(
                        "--max-minutes must be from 0 to {MAX_MAX_MINUTES} (0 disables the gate)"
                    ));
                }
            }
            "--approve" => {
                let raw = it.next().ok_or(
                    "--approve needs one of: proceed, fast, cancel (narrower means re-running \
                     against a subdirectory)",
                )?;
                let parsed = Approval::parse(&raw)?;
                // Two different answers in one invocation is not something to
                // resolve in either side's favour.
                if let Some(previous) = approve {
                    if previous != parsed {
                        return Err(format!(
                            "--approve {} and --approve {} contradict each other; pass one",
                            previous.as_str(),
                            parsed.as_str()
                        ));
                    }
                }
                approve = Some(parsed);
                approve_explicit = true;
            }
            "--yes" | "-y" => {
                if let Some(previous) = approve {
                    if previous != Approval::Proceed {
                        return Err(format!(
                            "--yes means --approve proceed and contradicts --approve {}; pass one",
                            previous.as_str()
                        ));
                    }
                }
                approve = Some(Approval::Proceed);
                approve_explicit = true;
            }
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
            // Read out of band by `xerj_common::feedback`, which scans the
            // whole argument list; accepted here so it is not "unknown".
            xerj_common::feedback::DISABLE_FLAG => {}
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
    // Only meaningful for a run that walks a folder. On `map`/`status` neither
    // flag does anything at all, and the arm below says so — a message about
    // one flag subsuming the other would imply the pair is otherwise honoured.
    let sub_walks_a_folder = !matches!(sub.as_deref(), Some("map") | Some("status"));
    if no_ignore && no_default_ignores && sub_walks_a_folder {
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

    // `--approve fast` is not a hint: it is the answer "index everything, but
    // without the two expensive features", and the run has to actually apply
    // them. Accepting the word and indexing semantically anyway is precisely
    // the accepted-and-silently-ignored class from #204.
    if approve == Some(Approval::Fast) {
        no_semantic = true;
        no_graph = true;
    }
    if approve == Some(Approval::Cancel) && dry_run {
        return Err(
            "--approve cancel and --dry-run contradict each other: a dry run already indexes \
             nothing. Drop one of the two"
                .into(),
        );
    }

    // `map` reads the catalog off the server and `status` reads the local
    // journal; neither walks a filesystem, so an ignore flag on either cannot
    // change one byte of the output. Measured before this check existed:
    // `xerj autoindex map --no-ignore` was byte-identical to `xerj autoindex
    // map`. Accepting it was the #204 accept-and-ignore class (#279).
    let ignore_flags_used: Vec<&str> = [
        ("--no-ignore", no_ignore),
        ("--no-default-ignores", no_default_ignores),
    ]
    .into_iter()
    .filter_map(|(name, used)| used.then_some(name))
    .collect();

    // Last resort for credentials: the key the server wrote for itself.
    //
    // The shipped default config has `[auth] enabled = true`, and the server
    // mints `<data_dir>/admin.key` on first start. `xerj brain` already reads
    // that file, which is why it works with no flags; `autoindex` did not, so
    // the documented "copy the default config, then autoindex" path ended in a
    // 401 and users turned auth off to escape it.
    //
    // Only for a loopback `--url`: reading a local key file is meaningful when
    // we are talking to a server on this machine, and sending it anywhere else
    // would leak a credential to a host it does not belong to.
    if api_key.is_none() && url_is_loopback(&url) {
        if let Some((key, path)) = discover_local_admin_key() {
            // Announced, never silent: a blind onboarding run found this
            // fallback made success depend on the working directory, because
            // `./data/admin.key` is resolved relative to cwd. Saying which
            // file was used turns "it worked here and not there" into
            // something the reader can see and reason about.
            eprintln!(
                "autoindex: no --api-key/XERJ_API_KEY given; using the admin key at {}",
                path.display()
            );
            api_key = Some(key);
            api_key_file = Some(path);
        }
    }

    // `--url` is the only endpoint source for every subcommand; XERJ_URL is
    // ignored on purpose (a stale var must not redirect a write). Computed once
    // so index, map, and status all surface the same mismatch note.
    let xerj_url_note =
        xerj_url_ignored_note(std::env::var("XERJ_URL").ok().as_deref(), url_explicit);

    match (sub.as_deref(), folder) {
        (Some("map"), _) | (Some("status"), _) if max_minutes_explicit || approve_explicit => {
            Err(format!(
                "--max-minutes/--approve/--yes apply only to indexing, not `autoindex {}`",
                sub.as_deref().unwrap_or_default()
            ))
        }
        (Some("map"), _) | (Some("status"), _) if progress_explicit => Err(format!(
            "--progress/--progress-interval apply only to indexing, not `autoindex {}`",
            sub.as_deref().unwrap_or_default()
        )),
        (Some("map"), _) | (Some("status"), _) if !ignore_flags_used.is_empty() => {
            let sub = sub.as_deref().unwrap_or_default();
            let (verb, them) = if ignore_flags_used.len() == 1 {
                ("applies", "it")
            } else {
                ("apply", "them")
            };
            Err(format!(
                "{} {verb} only to indexing, not `autoindex {sub}`: that subcommand never walks \
                 a folder, so there is nothing for an ignore rule to skip. Drop {them}",
                ignore_flags_used.join(" and "),
            ))
        }
        (Some("map"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex map`".into())
        }
        (Some("map"), _) => Ok(Cmd::Map(MapCfg {
            url,
            api_key,
            prefix,
            json,
            dataset,
            xerj_url_note,
        })),
        (Some("status"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex status`".into())
        }
        (Some("status"), _) => Ok(Cmd::Status(StatusCfg {
            url,
            api_key,
            prefix,
            state_dir,
            xerj_url_note,
        })),
        (None, Some(root)) => {
            // A flag that is accepted and does nothing is the shape this repo
            // refuses on purpose (#204, #279): the operator believes they asked
            // for out-of-root targets and gets a run that silently did not.
            if follow_symlinks_outside_root && !follow_symlinks {
                return Err(
                    "--follow-symlinks-outside-root has no effect without --follow-symlinks: \
                     links are not followed at all unless that flag is given"
                        .into(),
                );
            }
            let plan = crate::resources::plan(workers, pdf_workers, bulk_mb);
            Ok(Cmd::Index(Box::new(IndexCfg {
                root,
                url,
                api_key,
                api_key_file,
                workers: plan.index_workers,
                scan_workers: plan.scan_threads,
                pdf_workers: plan.pdf_workers,
                resource_notes: plan.notes,
                // #768: delivered by run_index via unconditional eprintln (not the
                // --quiet-suppressible resource_notes surface) and mirrored into
                // --json, so a wrong-node warning is never silently dropped.
                xerj_url_note,
                pdf_timeout_secs,
                bulk_mb,
                bulk_timeout_secs,
                snapshot_max_bytes,
                prefix,
                state_dir,
                fresh,
                follow_symlinks,
                follow_symlinks_outside_root,
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
                max_minutes,
                approve,
                json,
                quiet,
                progress,
                progress_interval,
            })))
        }
        _ => Ok(Cmd::Help),
    }
}

/// True when `url`'s host is this machine, so a locally readable admin key is
/// the right credential to send. Anything else — a LAN address, a hostname, a
/// remote deployment — must be given a key explicitly.
///
/// Parsed with a real URL parser rather than string surgery, because the
/// hand-rolled version of this was wrong in a way that leaked credentials:
/// splitting on the last `:` treats the userinfo in
/// `http://localhost:9200@evil.com/` as a host:port pair, judges it loopback,
/// and sends the admin key to `evil.com`. `Url::host_str` resolves that to
/// `evil.com`, which is the whole point of using it.
fn url_is_loopback(url: &str) -> bool {
    // A schemeless `localhost:9200` parses as scheme `localhost`, path `9200`,
    // with no host at all, so retry those through `http://`. The retry still
    // goes through the parser: `localhost:9200@evil.com` becomes
    // `http://localhost:9200@evil.com`, whose host is `evil.com`, not loopback.
    let parsed = match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u,
        _ => match reqwest::Url::parse(&format!("http://{url}")) {
            Ok(u) => u,
            // Unparseable is not loopback. Failing closed here costs a user
            // with an exotic URL one explicit --api-key; failing open costs
            // them the key itself.
            Err(_) => return false,
        },
    };
    match parsed.host_str() {
        // `host_str` strips the brackets from `[::1]` and does not lowercase
        // an IPv6 literal, so compare case-insensitively and cover both forms.
        Some(h) => {
            let h = h.trim_start_matches('[').trim_end_matches(']');
            h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1" || h == "0.0.0.0"
        }
        None => false,
    }
}

/// The admin key a local server wrote for itself, if we can find it.
///
/// Checked in the order a user is most likely to have created them: the
/// working directory's data dir (what the quickstart tells you to use), then
/// the documented package install location. Absent or unreadable is not an
/// error — the caller falls back to the actionable 401 message.
fn discover_local_admin_key() -> Option<(String, PathBuf)> {
    const CANDIDATES: &[&str] = &[
        "./data/admin.key",
        "./xerj-data/admin.key",
        "/var/lib/xerj/admin.key",
    ];
    let mut paths: Vec<PathBuf> = CANDIDATES.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(&home).join(".xerj/brain/admin.key"));
        paths.push(PathBuf::from(&home).join(".xerj/admin.key"));
    }
    paths.into_iter().find_map(|p| {
        std::fs::read_to_string(&p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|k| (k, p))
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, Approval, Cmd, Duration, ProgressMode, DEFAULT_MAX_MINUTES};
    use std::path::PathBuf;

    /// A flag that is accepted and silently does nothing is refused, because
    /// the operator who passed it believes the run did something it did not.
    #[test]
    fn outside_root_without_follow_symlinks_is_refused() {
        let err = parse(
            ["data", "--follow-symlinks-outside-root"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .expect_err("must not be accepted as a silent no-op");
        let text = err.to_string();
        assert!(
            text.contains("--follow-symlinks-outside-root") && text.contains("no effect"),
            "the message must name the flag and say why: {text}"
        );

        // Both together is the supported combination and must still parse.
        let cfg = index(&[
            "data",
            "--follow-symlinks",
            "--follow-symlinks-outside-root",
        ]);
        assert!(cfg.follow_symlinks && cfg.follow_symlinks_outside_root);
    }

    fn index(args: &[&str]) -> super::IndexCfg {
        match parse(args.iter().map(|s| s.to_string()).collect()).unwrap() {
            Cmd::Index(cfg) => *cfg,
            other => panic!("expected index config, got {other:?}"),
        }
    }

    fn err(args: &[&str]) -> String {
        parse(args.iter().map(|s| s.to_string()).collect()).expect_err("must be refused")
    }

    /// `xerj autoindex` reads its endpoint from `--url` only; setting `XERJ_URL`
    /// (which `xerj search` honors) and forgetting `--url` used to silently hit
    /// the loopback default with no word to the operator. The note fires exactly
    /// on that mismatch and nowhere else.
    #[test]
    fn xerj_url_ignored_note_fires_only_on_the_mismatch() {
        use super::xerj_url_ignored_note as note;

        // XERJ_URL set, --url absent: warn, and say what and how.
        let n = note(Some("http://es.internal:9200"), false)
            .expect("a set-but-ignored XERJ_URL must be surfaced");
        assert!(
            n.contains("http://es.internal:9200") && n.contains("--url") && n.contains("ignored"),
            "the note must echo the value, name --url, and say it is ignored: {n}"
        );

        // --url was passed: it wins, no note (env is genuinely irrelevant).
        assert!(
            note(Some("http://es.internal:9200"), true).is_none(),
            "an explicit --url means XERJ_URL was not ignored; no note"
        );
        // Nothing set, or set to blank: nothing to warn about.
        assert!(note(None, false).is_none(), "unset XERJ_URL: no note");
        assert!(
            note(Some("   "), false).is_none(),
            "blank XERJ_URL: no note"
        );
    }

    /// #768: the index cfg carries the XERJ_URL-ignored note on its own field
    /// (delivered by an unconditional eprintln, so --quiet cannot silence a
    /// wrong-node warning) rather than folding it into the --quiet-suppressible
    /// resource_notes. An explicit --url means the env var was not ignored, so
    /// there is nothing to warn about — deterministic regardless of the
    /// environment the test runs in.
    #[test]
    fn index_suppresses_the_xerj_url_note_when_url_is_explicit() {
        let cfg = index(&["data", "--url", "http://es.internal:9200"]);
        assert!(
            cfg.xerj_url_note.is_none(),
            "an explicit --url means XERJ_URL was not ignored; no note"
        );
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

    /// The owner's threshold, verbatim: "if estimated more than 10min work
    /// needs to ask AI back what to do".
    #[test]
    fn the_gate_defaults_to_ten_minutes_and_to_unanswered() {
        let cfg = index(&["data"]);
        assert_eq!(DEFAULT_MAX_MINUTES, 10);
        assert_eq!(cfg.max_minutes, 10);
        assert_eq!(cfg.approve, None, "an unanswered run is what arms the gate");
        assert_eq!(index(&["data", "--max-minutes", "45"]).max_minutes, 45);
        // 0 is the documented "never ask" value, not a rejected one.
        assert_eq!(index(&["data", "--max-minutes", "0"]).max_minutes, 0);
    }

    #[test]
    fn an_unusable_max_minutes_is_refused_not_clamped() {
        for args in [
            vec!["data", "--max-minutes"],
            vec!["data", "--max-minutes", "soon"],
            vec!["data", "--max-minutes", "-1"],
            vec!["data", "--max-minutes", "10081"],
        ] {
            let rendered = args.join(" ");
            let err = parse(args.into_iter().map(str::to_string).collect())
                .expect_err(&format!("`{rendered}` must be refused"));
            assert!(err.contains("--max-minutes"), "{err}");
        }
        assert_eq!(
            index(&["data", "--max-minutes", "10080"]).max_minutes,
            10080
        );
    }

    /// `--approve fast` is an instruction, not a label: the run must really
    /// drop semantic fields and edges. Accepting it and indexing everything
    /// anyway is the #204 defect class.
    #[test]
    fn approve_fast_actually_applies_the_flags_it_names() {
        let cfg = index(&["data", "--approve", "fast"]);
        assert_eq!(cfg.approve, Some(Approval::Fast));
        assert!(cfg.no_semantic, "--approve fast must set --no-semantic");
        assert!(cfg.no_graph, "--approve fast must set --no-graph");
        // The other two answers change nothing about what gets indexed.
        let proceed = index(&["data", "--approve", "proceed"]);
        assert_eq!(proceed.approve, Some(Approval::Proceed));
        assert!(!proceed.no_semantic && !proceed.no_graph);
        assert_eq!(
            index(&["data", "--approve", "cancel"]).approve,
            Some(Approval::Cancel)
        );
    }

    #[test]
    fn yes_is_an_alias_for_approve_proceed() {
        for flag in ["--yes", "-y"] {
            assert_eq!(index(&["data", flag]).approve, Some(Approval::Proceed));
        }
        // Agreeing with yourself twice is fine; disagreeing is not.
        assert_eq!(
            index(&["data", "--yes", "--approve", "proceed"]).approve,
            Some(Approval::Proceed)
        );
        assert!(err(&["data", "--approve", "cancel", "--yes"]).contains("--yes"));
        for contradiction in [
            vec!["data", "--yes", "--approve", "cancel"],
            vec!["data", "--approve", "fast", "--approve", "cancel"],
        ] {
            assert!(err(&contradiction).contains("contradict each other"));
        }
    }

    /// `narrower` is a real option in the decision request and an impossible
    /// one for this flag: it means running against a different folder.
    #[test]
    fn approve_refuses_the_answer_it_cannot_carry_out() {
        let refused = err(&["data", "--approve", "narrower"]);
        assert!(refused.contains("subdirectory"), "{refused}");
        assert!(err(&["data", "--approve", "maybe"]).contains("proceed, fast, cancel"));
        assert!(err(&["data", "--approve"]).contains("--approve"));
    }

    #[test]
    fn cancel_and_dry_run_are_refused_rather_than_silently_merged() {
        let err = err(&["data", "--approve", "cancel", "--dry-run"]);
        assert!(err.contains("already indexes nothing"), "{err}");
        // A dry run may still be told what threshold to report against.
        assert_eq!(
            index(&["data", "--dry-run", "--max-minutes", "3"]).max_minutes,
            3
        );
    }

    #[test]
    fn gate_flags_are_rejected_for_non_index_subcommands() {
        for args in [
            vec!["map", "--max-minutes", "5"],
            vec!["status", "--approve", "proceed"],
            vec!["--yes", "map"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("apply only to indexing"), "{err}");
        }
    }

    /// A flag the engine honours but never mentions is only half-shipped, and
    /// the exit code is the one thing an agent cannot discover by trying.
    #[test]
    fn the_help_documents_the_gate_its_exit_code_and_the_work_order() {
        let help = super::help_text();
        for expected in [
            "--max-minutes",
            "--approve <ID>",
            "--yes, -y",
            "0 disables the gate",
            "ESTIMATE + DECISION GATE:",
            "exits 4",
            "4 NEEDS A DECISION",
            "WORK ORDER:",
            "vendored/generated/minified",
            "CLIENT-SIDE EXTRACTION only",
            // A run that silently stops asking has to say so where the user
            // looks: --quiet is the flag, the gate section is the rule.
            "NEVER prompts under this flag",
            "never waits on stdin for a question it did not print",
            "prompt_not_offered_because",
        ] {
            assert!(help.contains(expected), "help is missing {expected:?}");
        }
    }

    /// The invitation is on by default and near the top — at the bottom of a
    /// 200-line help body it would never be read. Position is the requirement,
    /// so position is what is asserted.
    #[test]
    fn the_help_invites_a_bug_report_near_the_top_by_default() {
        let help = super::help_text_with(true);
        let line = help
            .lines()
            .position(|l| l.contains("Hit a bug, or a flow that confused you?"))
            .expect("no invitation in the help");
        assert!(line < 10, "invitation sits on line {}", line + 1);
        for expected in [
            "https://github.com/xerj-org/xerj/issues",
            "GitHub tool",
            "Discussion",
            "secrets, API keys and private data",
            // The off-switch belongs on the same screen as the thing it turns
            // off.
            "--disable-feedback",
        ] {
            assert!(help.contains(expected), "help is missing {expected:?}");
        }
    }

    #[test]
    fn the_invitation_is_gone_when_it_is_turned_off() {
        let help = super::help_text_with(false);
        assert!(!help.contains("Hit a bug"), "invitation was not silenced");
        assert!(
            !help.contains("\n\n\n"),
            "silencing it left a blank gap:\n{help}"
        );
        // Silencing the invitation must not silence the rest of the help.
        assert!(help.contains("USAGE:") && help.contains("--disable-feedback"));
    }

    /// `--disable-feedback` is consumed wherever it appears, on every
    /// subcommand — it must never be an "unknown argument".
    #[test]
    fn disable_feedback_is_accepted_in_any_position() {
        for args in [
            vec!["data", "--disable-feedback"],
            vec!["--disable-feedback", "data"],
            vec!["map", "--disable-feedback"],
            vec!["--disable-feedback", "map"],
            vec!["status", "--disable-feedback"],
            vec!["--disable-feedback", "--help"],
            vec!["--help", "--disable-feedback"],
        ] {
            parse(args.iter().map(|s| s.to_string()).collect())
                .unwrap_or_else(|e| panic!("{args:?} was rejected: {e}"));
        }
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

    /// Every `--flag` the autoindex use-case README names must be one this
    /// parser actually accepts.
    ///
    /// `parse` hard-errors `unknown argument: {other}` on anything it does
    /// not know, so a documented-but-nonexistent flag does not degrade — the
    /// command exits. The README shipped `--no-default-excludes` and
    /// `--no-gitignore`, which have never existed under those names (they are
    /// `--no-default-ignores` and `--no-ignore`), so every reader who copied
    /// the documented invocation got an error instead of an index.
    ///
    /// Flags belonging to the `xerj` SERVER binary are excluded by name —
    /// the README also shows how to start a server, and those are not this
    /// parser's vocabulary.
    #[test]
    fn every_flag_named_in_the_usecase_readme_is_a_real_autoindex_flag() {
        const SERVER_ONLY: &[&str] = &["--insecure", "--data-dir"];

        let readme = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../demo/usecases/autoindex/README.md");
        let text = std::fs::read_to_string(&readme)
            .unwrap_or_else(|e| panic!("read {}: {e}", readme.display()));

        let mut named: Vec<String> = Vec::new();
        for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            // `---` is a YAML document marker in the sample output, not a flag.
            if tok.starts_with("--") && tok.len() > 2 && !tok.starts_with("---") {
                let tok = tok.trim_end_matches('-');
                if !SERVER_ONLY.contains(&tok) && !named.contains(&tok.to_string()) {
                    named.push(tok.to_string());
                }
            }
        }
        assert!(
            named.len() >= 5,
            "extraction found almost nothing ({named:?}) — the test would pass vacuously"
        );

        for flag in &named {
            // A flag that exists but wants a value fails on the VALUE; only an
            // unrecognised name produces "unknown argument".
            if let Err(e) = parse(["data", flag].iter().map(|s| s.to_string()).collect()) {
                assert!(
                    !e.contains("unknown argument"),
                    "README documents {flag}, which the CLI rejects: {e}"
                );
            }
        }
    }

    /// A flag that cannot change anything is refused rather than accepted and
    /// quietly dropped (#204's class).
    #[test]
    fn no_default_ignores_under_no_ignore_is_refused() {
        let err = err(&["data", "--no-ignore", "--no-default-ignores"]);
        assert!(err.contains("--no-ignore already turns off"), "{err}");
    }

    /// #279, same class. `map` reads the catalog off the server and `status`
    /// reads the local journal; neither walks a folder, so an ignore flag on
    /// either changes nothing. Both were accepted, and `xerj autoindex map
    /// --no-ignore` was measured byte-identical to `xerj autoindex map`.
    #[test]
    fn ignore_flags_are_rejected_for_non_index_subcommands_in_any_position() {
        for args in [
            vec!["map", "--no-ignore"],
            vec!["--no-ignore", "map"],
            vec!["map", "--no-default-ignores"],
            vec!["--no-default-ignores", "map"],
            vec!["status", "--no-ignore"],
            vec!["--no-ignore", "status"],
            vec!["status", "--no-default-ignores"],
            vec!["--no-default-ignores", "status"],
        ] {
            let err = parse(args.iter().map(|a| a.to_string()).collect()).unwrap_err();
            assert!(
                err.contains("applies only to indexing") && err.contains("never walks a folder"),
                "{args:?} -> {err}"
            );
        }
    }

    /// Both flags on `map` must be refused for the reason that is actually
    /// true there — neither applies at all — not for the indexing-only reason
    /// that one subsumes the other, which would imply the pair is honoured.
    #[test]
    fn both_ignore_flags_on_map_are_refused_as_inapplicable_not_as_redundant() {
        let err = err(&["map", "--no-ignore", "--no-default-ignores"]);
        assert!(err.contains("never walks a folder"), "{err}");
        assert!(!err.contains("already turns off"), "{err}");
        assert!(
            err.contains("--no-ignore") && err.contains("--no-default-ignores"),
            "both flags must be named: {err}"
        );
    }

    /// …and the index path is untouched: the flags still work where they mean
    /// something.
    #[test]
    fn ignore_flags_still_apply_to_an_index_run() {
        assert!(!index(&["data", "--no-ignore"]).ignore.enabled);
        assert!(!index(&["data", "--no-default-ignores"]).ignore.defaults);
    }

    // ─── local admin-key discovery ──────────────────────────────────────

    /// `url_is_loopback` decides whether a credential read off this machine's
    /// disk is put on the wire, so a false positive is a credential leak, not
    /// a cosmetic bug. The negatives below are the shapes an attacker would
    /// reach for: a subdomain that starts with the magic word, a hostname that
    /// merely contains it, a private address, and the loopback name appearing
    /// somewhere in the URL that is not the host.
    #[test]
    fn url_is_loopback_is_true_only_for_this_machine() {
        for url in [
            "http://localhost:9200",
            "https://127.0.0.1",
            "http://[::1]:9200/path",
            "http://localhost",
            "http://127.0.0.1:9200/",
            "http://127.0.0.1/_cluster/health",
            "http://0.0.0.0:9200",
            "https://localhost:9200/?pretty",
            // No scheme at all: `--url localhost:9200` still names this box.
            "localhost:9200",
        ] {
            assert!(super::url_is_loopback(url), "{url} is this machine");
        }
        for url in [
            "http://localhost.evil.com",
            "http://localhost.evil.com:9200",
            "http://notlocalhost",
            "http://notlocalhost:9200",
            "http://xerj-localhost.example.com:9200",
            "http://192.168.1.5:9200",
            "http://10.0.0.7",
            "http://169.254.169.254/latest/meta-data",
            "http://example.com",
            "https://search.example.com:9200/path",
            "http://127.0.0.1.evil.com:9200",
            "http://1270.0.0.1:9200",
            "http://[fe80::1]:9200",
            // The word appears, but never as the host.
            "http://evil.com/localhost",
            "http://evil.com:9200/localhost",
            "http://evil.com/?next=http://localhost:9200",
            "http://evil.com#localhost",
        ] {
            assert!(!super::url_is_loopback(url), "{url} is NOT this machine");
        }
    }

    /// The bug this function was rewritten to fix: `rsplit_once(':')` read the
    /// userinfo in `http://localhost:9200@evil.com/` as a host:port pair, called
    /// it loopback, and would have sent the local admin key to `evil.com` as
    /// `Authorization: ApiKey <key>`. Found by an adversarial review of the
    /// original hand-rolled parser, not by the happy-path cases above.
    #[test]
    fn userinfo_cannot_disguise_a_remote_host_as_loopback() {
        for url in [
            "http://localhost:9200@evil.com/",
            "http://127.0.0.1:80@evil.com/",
            "http://localhost@evil.com/",
            "https://[::1]:443@evil.com/",
            "localhost:9200@evil.com",
            "http://user:pass@evil.com/",
        ] {
            assert!(
                !super::url_is_loopback(url),
                "{url} is NOT this machine - treating it as loopback leaks the admin key"
            );
        }
    }

    /// Forms that are genuinely this machine and must keep working, including
    /// the ones the hand-rolled version got wrong in the safe direction.
    #[test]
    fn loopback_spellings_all_resolve_to_this_machine() {
        for url in [
            "http://localhost:9200",
            "http://LOCALHOST:9200",
            "https://127.0.0.1",
            "http://[::1]",
            "http://[::1]:9200/path",
            "localhost:9200",
            "http://0.0.0.0:9200",
        ] {
            assert!(super::url_is_loopback(url), "{url} is this machine");
        }
    }

    /// Serialises the tests that move process-global state. Both inputs to
    /// `discover_local_admin_key` — the working directory and `HOME` — are
    /// per-process, so these tests cannot be isolated any other way.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A temporary working directory and `HOME`, restored on drop (including
    /// on panic) so no other test in this binary observes the mutation for
    /// longer than the lock is held.
    struct Sandbox {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
        previous_dir: PathBuf,
        previous_home: Option<std::ffi::OsString>,
        previous_env_key: Option<std::ffi::OsString>,
    }

    impl Sandbox {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let previous_dir = std::env::current_dir().unwrap();
            let previous_home = std::env::var_os("HOME");
            let previous_env_key = std::env::var_os("XERJ_API_KEY");
            std::env::set_current_dir(dir.path()).unwrap();
            std::env::set_var("HOME", dir.path().join("home"));
            std::env::remove_var("XERJ_API_KEY");
            Self {
                _lock: lock,
                dir,
                previous_dir,
                previous_home,
                previous_env_key,
            }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.dir.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous_dir).unwrap();
            match &self.previous_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            match &self.previous_env_key {
                Some(key) => std::env::set_var("XERJ_API_KEY", key),
                None => std::env::remove_var("XERJ_API_KEY"),
            }
        }
    }

    /// The order matters: a user who is running a server out of this very
    /// directory means that key, not one left in `$HOME` by an older run
    /// against a different data dir.
    #[test]
    fn discovery_walks_the_candidates_in_order_and_trims_the_key() {
        let sandbox = Sandbox::new();
        assert_eq!(super::discover_local_admin_key(), None, "nothing to find");

        // Weakest candidate first, then override it one rung at a time.
        sandbox.write("home/.xerj/admin.key", "home-key\n");
        assert_eq!(
            super::discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("home-key")
        );

        sandbox.write("home/.xerj/brain/admin.key", "  brain-key \t\r\n");
        assert_eq!(
            super::discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("brain-key"),
            "the brain data dir outranks the bare ~/.xerj key, and is trimmed"
        );

        sandbox.write("xerj-data/admin.key", "xerj-data-key\n");
        assert_eq!(
            super::discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("xerj-data-key"),
            "a data dir in the working directory outranks anything in $HOME"
        );

        sandbox.write("data/admin.key", "\ndata-key\n");
        assert_eq!(
            super::discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("data-key"),
            "./data is the quickstart's own path and wins outright"
        );
    }

    /// A server that has not finished writing its key, or a file truncated by
    /// hand, must not turn into `Authorization: ApiKey ` — that produces the
    /// same 401 with none of the diagnosis.
    #[test]
    fn an_empty_key_file_is_skipped_not_returned_as_an_empty_key() {
        let sandbox = Sandbox::new();
        sandbox.write("data/admin.key", "   \n\t\n");
        sandbox.write("xerj-data/admin.key", "");
        assert_eq!(
            super::discover_local_admin_key(),
            None,
            "whitespace-only and zero-byte files are not credentials"
        );

        sandbox.write("home/.xerj/admin.key", "real-key\n");
        assert_eq!(
            super::discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("real-key"),
            "an empty candidate must be skipped over, not stop the search"
        );
    }

    /// The wiring: discovery only ever fires when the run has no credential of
    /// its own AND the target is this machine. Sending a locally readable
    /// admin key to a host that did not write it is the failure mode this
    /// whole feature has to avoid.
    #[test]
    fn a_discovered_key_is_used_for_a_local_url_and_never_for_a_remote_one() {
        let sandbox = Sandbox::new();
        assert_eq!(
            index(&["notes"]).api_key,
            None,
            "no key file, no invented credential"
        );

        sandbox.write("data/admin.key", "local-admin-key\n");
        assert_eq!(
            index(&["notes"]).api_key.as_deref(),
            Some("local-admin-key"),
            "the default --url is loopback, so the run picks the key up"
        );
        assert_eq!(
            index(&["notes", "--url", "http://localhost:9201"])
                .api_key
                .as_deref(),
            Some("local-admin-key")
        );

        for remote in [
            "http://search.example.com:9200",
            "http://192.168.1.5:9200",
            "https://localhost.evil.com:9200",
        ] {
            assert_eq!(
                index(&["notes", "--url", remote]).api_key,
                None,
                "{remote} must never be sent a key found on this disk"
            );
        }

        assert_eq!(
            index(&["notes", "--api-key", "explicit"])
                .api_key
                .as_deref(),
            Some("explicit"),
            "an explicit flag is never overwritten by discovery"
        );

        std::env::set_var("XERJ_API_KEY", "from-env");
        let from_env = index(&["notes"]).api_key;
        std::env::remove_var("XERJ_API_KEY");
        assert_eq!(
            from_env.as_deref(),
            Some("from-env"),
            "the environment is also never overwritten by discovery"
        );
    }
}
