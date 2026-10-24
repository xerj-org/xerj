//! Hand-rolled arg parser (house style of xerj-server — no clap).

use crate::gate::{Approval, DEFAULT_MAX_MINUTES};
use crate::ignore_rules::IgnoreOptions;
use crate::progress::ProgressMode;
use std::path::PathBuf;
use std::time::Duration;
// Loopback guard + local admin-key discovery, shared with `xerj mcp` (which
// hit the identical 401 failure mode, #961). The invariants — a false
// positive on `url_is_loopback` is a credential leak, not a cosmetic bug —
// are pinned by the `local admin-key discovery` tests at the bottom of this
// file: they stayed here when the helpers moved to `xerj-common`, so they
// are the shared helpers' tests wherever the helpers live.
use xerj_common::localauth::{discover_local_admin_key_for_url, url_is_loopback};

/// Largest `--max-minutes` accepted: one week. Past that the flag is a typo,
/// and `--max-minutes 0` already exists to mean "never ask".
pub const MAX_MAX_MINUTES: u64 = 7 * 24 * 60;

/// Largest `--debounce` accepted: one minute. A quiet period longer than that
/// is not debouncing, it is a timer — and `xerj autoindex` on a cron schedule
/// already is one, without holding watches open.
pub const MAX_DEBOUNCE_MS: u64 = 60_000;

/// Largest `--bulk-mb` accepted. Past this a single bulk body stops being a
/// unit of work and starts being a memory incident on the server.
pub const MAX_BULK_MB: usize = 24;

#[derive(Debug, Clone)]
pub struct IndexCfg {
    /// The positional argument: a folder, or `s3://bucket/prefix` /
    /// `r2://bucket/prefix`. An object URL is turned into the local mirror the
    /// run walks by [`crate::objsource::prepare`], which rewrites this field —
    /// everything after that point sees a path, exactly as it did before object
    /// storage existed.
    pub root: PathBuf,
    /// `--endpoint-url`: the S3-compatible endpoint to talk to (MinIO, Ceph,
    /// R2, localstack). Falls back to `AWS_ENDPOINT_URL_S3` then
    /// `AWS_ENDPOINT_URL`. Meaningless for a folder, and refused there rather
    /// than ignored.
    pub endpoint_url: Option<String>,
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
    /// because it is a safety warning, not routine progress chatter: the index
    /// path emits it through `pr.warn` (which, unlike `pr.note`, is not silenced
    /// by `--quiet`/`Surface::Silent`, yet stays a well-formed event on
    /// `--progress json`) and mirrors it into the `--json` result. `map`/`status`
    /// carry the same note but `eprintln` it, having no progress surface.
    pub xerj_url_note: Option<String>,
    pub pdf_timeout_secs: u64,
    pub bulk_mb: usize,
    pub bulk_timeout_secs: u64,
    pub snapshot_max_bytes: u64,
    pub prefix: String,
    /// #1845: set when `--prefix` was rewritten on the way to `prefix` (a
    /// trailing `-`/`_`, a `_`, a repeated separator, upper case). Said out loud
    /// through `pr.warn` because the rewrite is exactly what makes sibling
    /// projects collide under a `<prefix>-*` wildcard.
    pub prefix_note: Option<String>,
    /// `--replace-prefix`: this folder may take over a prefix whose catalog
    /// runs were indexed from a different root. Off by default; without it
    /// that run is refused before anything is written.
    pub replace_prefix: bool,
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
    /// `deep_count` is set from `--dry-run` (and from `--walk-only`, which
    /// implies it).
    pub ignore: IgnoreOptions,
    /// Whole-file size cap in BYTES for non-streaming families (JSON, HTML,
    /// prose, PDF, …). Streaming families are never capped. Set on the CLI by
    /// any one of `--max-file-gb` / `--max-file-mb` / `--max-file-bytes`
    /// (mutually exclusive), all normalised to bytes here so a sub-GB cap —
    /// e.g. a 10 MB source-watcher budget (#1233) — is expressible. Default
    /// 2 GB.
    pub max_file_bytes: u64,
    pub sample: usize,
    pub no_semantic: bool,
    /// `--code-analyzer` (#1198): `code` declares the built-in identifier-aware
    /// analyzer on newly created datasets that hold source code.
    pub code_analyzer: crate::infer::CodeAnalyzer,
    /// Second-brain name; None derives it from the root folder basename.
    pub brain: Option<String>,
    /// Disable edge detection entirely (no `.xerj-memory-*-edges` writes).
    pub no_graph: bool,
    pub dry_run: bool,
    /// `--walk-only`: print the per-folder walk summary and return before
    /// hashing. The parser also sets [`Self::dry_run`], so nothing is indexed.
    /// `--dry-run` without this flag still hashes, sniffs and prints the plan.
    pub walk_only: bool,
    /// Stop and ask before indexing when phase A's measured estimate exceeds
    /// this many minutes. `0` disables the gate outright.
    pub max_minutes: u64,
    /// An answer to a previous decision request. `None` means "nobody has
    /// answered yet", which is what arms the gate.
    pub approve: Option<Approval>,
    /// `--max-index-size`, in bytes. `None` means the only budget is the free
    /// space under the node's flood-stage watermark, when the node reports one.
    pub max_index_bytes: Option<u64>,
    /// `--plan`. `None` means the default plan, which is indexed when it fits
    /// and refused when it does not. A named plan runs even when the estimate
    /// says it does not fit (with a warning): that is the override.
    pub budget_plan: Option<String>,
    /// `--drop-indexed`. Confirms that a reduced plan may delete files it
    /// leaves out when those files are already indexed. Without it that
    /// re-run exits 4 and lists them. `--plan` and `--yes` do not set this.
    pub drop_indexed: bool,
    pub json: bool,
    pub quiet: bool,
    /// Progress surface. Orthogonal to `json`: `--json` shapes *stdout* (the
    /// result), `--progress` shapes *stderr* (liveness).
    pub progress: ProgressMode,
    /// Progress cadence. `None` means "the surface's default" — 1 s on a
    /// terminal, 5 s for a pipe.
    pub progress_interval: Option<Duration>,
    /// `--label <question-set.json>`: run every extracted record's payload
    /// through the node's `POST /_decide` at ingest and stamp `label` /
    /// `label_p` (plus per-question `label_<id>` / `label_<id>_p`) onto the
    /// document (#1062). `None` — the default — never constructs a Labeler.
    pub label: Option<PathBuf>,
    /// `--watch`: after the first pass, stay resident and reindex what changes.
    pub watch: bool,
    /// `--watch`'s quiet period. One editor save is several filesystem events,
    /// so a pass waits for the tree to go quiet for this long before running.
    pub debounce: Duration,
    /// Root-relative paths dropped after the walk. Empty for a normal
    /// `xerj autoindex <folder>`. `xerj corpus index` sets `corpus.json` so
    /// the hub manifest is not indexed as a document dataset.
    pub exclude_rels: Vec<String>,
    /// `xerj corpus index` only (#1985): skip data files (CSV/HDF/simulation
    /// output — see [`crate::CORPUS_DATA_EXTS`]) instead of indexing them as
    /// records. `--include-data` turns it off. Never set by
    /// `xerj autoindex <folder>`: a user's own project may want its data.
    pub skip_corpus_data: bool,
}

#[derive(Debug, Clone)]
pub struct MapCfg {
    pub url: String,
    pub api_key: Option<String>,
    /// Explicit `--prefix` filter (`None` when the user did not pass `--prefix`,
    /// preserving the #971 multi-corpus view on a bare `autoindex map`).
    pub prefix: Option<String>,
    pub json: bool,
    pub dataset: Option<String>,
    /// `--offset` for the map's per-dataset field detail. The summary table
    /// always lists every dataset.
    pub detail_offset: usize,
    /// `--limit` for that detail page. `None` uses the default page when
    /// there are more datasets than [`crate::catalog::MAP_DETAIL_PAGE`].
    pub detail_limit: Option<usize>,
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

/// `xerj autoindex remove --prefix P [--root DIR]`.
///
/// Deletes one autoindexed folder after printing the plan. `--prefix` is
/// required: the default name `ax` is shared, and guessing it is how an agent
/// deletes another project's index. `--yes` confirms. It does not choose a
/// folder when the prefix holds more than one.
#[derive(Debug, Clone)]
pub struct RemoveCfg {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub root: Option<String>,
    pub yes: bool,
    /// Accepted. Stdout is the JSON plan either way.
    pub json: bool,
    pub quiet: bool,
    pub xerj_url_note: Option<String>,
    /// When set, `remove` also deletes `journal.ndjson` here. The default
    /// journal for the selected folder is deleted either way.
    pub state_dir: Option<PathBuf>,
}

/// `xerj autoindex s3://bucket/prefix --watch`: poll a bucket and emit a change
/// feed. Deliberately its own config rather than fields on [`IndexCfg`]: the
/// watch path shares no phase-A/phase-B machinery with folder indexing, and
/// bolting its flags onto the index config is what makes an accepted-and-ignored
/// flag possible.
#[derive(Debug, Clone)]
pub struct WatchCfg {
    /// `s3://bucket/prefix` as the user wrote it.
    pub url: String,
    /// Endpoint that serves the bucket (`--endpoint-url`, else
    /// `AWS_ENDPOINT_URL`, else AWS's regional endpoint).
    pub endpoint: Option<String>,
    /// `--region`, else `AWS_REGION`, else `auto` (which is what R2 wants).
    pub region: String,
    pub poll_interval: Duration,
    /// `None` = until stopped.
    pub max_cycles: Option<u64>,
    /// Fetch bytes of changed objects to digest them. `--no-fetch` turns every
    /// GET off: metadata-only change detection, zero Class B operations.
    pub fetch: bool,
    pub max_object_bytes: u64,
    pub append_only: bool,
    pub max_monthly_ops: u64,
    /// Class B (GET) budget per calendar month.
    pub max_monthly_gets: u64,
    pub allow_cost: bool,
    pub page_size: u64,
    pub state_dir: Option<PathBuf>,
    pub fresh: bool,
    /// JSONL change feed destination. `None` = stdout.
    pub events_out: Option<PathBuf>,
    pub status_file: Option<PathBuf>,
    /// One cycle, no fetches, nothing emitted: measure what the poll costs.
    pub dry_run: bool,
    pub json: bool,
    pub quiet: bool,
}

#[derive(Debug)]
pub enum Cmd {
    /// Boxed: `IndexCfg` is an order of magnitude larger than the other
    /// variants, so inlining it made every `Cmd` — including `Help` — pay for
    /// it (`clippy::large_enum_variant`).
    Index(Box<IndexCfg>),
    Map(MapCfg),
    Status(StatusCfg),
    Remove(RemoveCfg),
    /// Boxed for the same reason as `Index`.
    Watch(Box<WatchCfg>),
    Help,
}

/// Object-storage URL schemes the watcher understands. `r2://` is accepted as a
/// spelling of `s3://` because Cloudflare R2 IS the S3 API — it is the endpoint
/// that differs, not the protocol.
pub const OBJECT_URL_SCHEMES: [&str; 2] = ["s3://", "r2://"];

pub fn looks_like_object_url(arg: &str) -> bool {
    OBJECT_URL_SCHEMES.iter().any(|s| arg.starts_with(s))
}

/// Largest `--poll-interval` accepted: one day. Past that a watch is a cron job.
pub const MAX_POLL_INTERVAL_SECS: u64 = 86_400;

const FRESH_HELP: &str =
    "ignore an existing resume journal and restart, rebuild the plan in place\n\
     \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(ids stay idempotent for unchanged bytes). A file republished\n\
     \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20under a new content key drops the documents previously published for that path.\n\
     \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20It removes documents for a file that disappeared. It is refused on a\n\
     \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20durable corpus generation — see RESUME POLICY";
const RESUME_POLICY_HELP: &str =
    "generated --no-graph journals reconcile add, change, delete, rename, and no-op runs; \
a --no-graph state directory written before the generation format must be rebuilt into a new \
--state-dir and --prefix; graph-enabled journals reconcile added files that fit a frozen \
dataset, delete documents for files that disappeared, and keep crash-resume of an interrupted run";

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
         \x20\x20\x20\x20xerj autoindex <folder> [OPTIONS]     discover + index a folder\n\
         \x20\x20\x20\x20xerj autoindex s3://<bucket>/<prefix> [OPTIONS]\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20discover + index objects in a bucket\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(r2:// too; see OBJECT STORAGE)\n\
         \x20\x20\x20\x20xerj autoindex map [OPTIONS]          print the data map of what is already indexed\n\
         \x20\x20\x20\x20xerj autoindex map <folder> [OPTIONS] preview a folder that is not indexed yet.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Same discovery as `xerj autoindex <folder>\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--dry-run`: walk, sniff, infer, print the plan.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Nothing is written to an index. With no\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20folder, `map` reads the catalog on --url\n\
         \x20\x20\x20\x20xerj autoindex remove --prefix <P> [--root <DIR>] [--yes]\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20list one folder's indices and catalog rows,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20then delete them. Nothing is deleted without\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--yes or a terminal y. Another folder under\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the same prefix is left in place\n\
         \x20\x20\x20\x20xerj autoindex status [OPTIONS]       resume-journal + index progress view\n\
         \x20\x20\x20\x20xerj autoindex s3://bucket/prefix --watch [OPTIONS]\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20poll object storage and emit a change feed\n\
         \n\
         OPTIONS:\n\
         \x20\x20\x20\x20--url <U>            ES-compat endpoint (default http://localhost:9200)\n\
         \x20\x20\x20\x20--api-key <K>        Authorization header (or env XERJ_API_KEY)\n\
         \x20\x20\x20\x20--workers <N>        workers for BOTH phases — content hashing/sniffing and\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20indexing (default: every core, reduced if the memory\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20safe zone cannot pay for that many bulk buffers; a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20value you pass is honoured and only warned about;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20valid range 1..=1024)\n\
         \x20\x20\x20\x20--pdf-workers <N>    concurrent PDF parser processes (default min(cores,4); max 4,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the default reduced further on a machine with little\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20free memory)\n\
         \x20\x20\x20\x20--pdf-timeout-secs <N> per-PDF parser timeout (default 120; max 3600)\n\
         \x20\x20\x20\x20--bulk-mb <N>        bulk cut size in MB (default 8, max {max_bulk_mb})\n\
         \x20\x20\x20\x20--bulk-timeout-secs <N>\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20bulk HTTP request timeout in seconds (default 300;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20valid range 1..=3600)\n\
         \x20\x20\x20\x20--prefix <P>         index prefix (default ax). One prefix is one\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20source folder. The same prefix on a different\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20folder is refused: datasets are keyed\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20ds:<prefix>:<slug> and indices are <prefix>-*,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20so finalize overwrites the earlier datasets.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Several repositories under one prefix also\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20collapse: each repo indexed on its own has an\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20empty scope and becomes the single dataset\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20<prefix>-docs. Use one --prefix per folder,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20or index the parent folder once. A prefix\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20keeps only [a-z0-9] runs joined by single '-',\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20so a trailing '-' or '_' is dropped (the run\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20warns). <prefix>-* also matches a sibling\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20prefix that starts with <prefix>-, so pick\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20prefixes that do not start one another\n\
         \x20\x20\x20\x20--replace-prefix     retire datasets already stored under --prefix\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20that were indexed from a different folder.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Prints those folders and continues. Without\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20this flag that run is refused and nothing\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is written. --dry-run does not bypass it\n\
         \x20\x20\x20\x20--state-dir <PATH>   resume journal location (default ~/.xerj/autoindex/<hash>/).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20On `autoindex remove`, also deletes journal.ndjson in\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20this directory after the folder's indices are removed;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the default journal is deleted either way.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20for an s3:// source this is also where the local\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20object mirror lives, so point it at a disk with room\n\
         \x20\x20\x20\x20--endpoint-url <URL> S3-compatible endpoint for an s3:// source (MinIO,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Ceph, R2, localstack). Defaults to\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20$AWS_ENDPOINT_URL_S3, then $AWS_ENDPOINT_URL.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Refused for a folder rather than ignored\n\
         \x20\x20\x20\x20--snapshot-max-gb <N> logical payload cap for sealed source+prepared records\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20bytes (default 64); excludes filesystem/manifest overhead\n\
         \x20\x20\x20\x20--fresh              {fresh_help}\n\
         \x20\x20\x20\x20--follow-symlinks    follow symlinks (loop-safe); off by default.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20A link is judged by what it RESOLVES to: a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20target outside the folder is refused, and one\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20inside a hidden directory is skipped like any\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20dotfile, whatever the link itself is called\n\
         \x20\x20\x20\x20--follow-symlinks-outside-root\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20also follow links that resolve OUTSIDE the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20folder. Requires --follow-symlinks. Off by\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20default; pointing at a folder is not consent\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20to index whatever it links to. The hidden-file\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20rule still applies to the target, judged from\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20where it diverges from your folder — so a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20dotted directory the two paths SHARE does not\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20refuse anything, because your folder is\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20already inside it\n\
         \x20\x20\x20\x20--stub <GLOB>        index matching files as ONE existence-only name\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20card (title + provenance); contents are never\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20opened. Repeatable. A pattern without '/'\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20matches file names anywhere ('*.bvh'); with '/'\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20it matches the root-relative path\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20('unity/**/*.csv'). '**' crosses directories\n\
         \x20\x20\x20\x20--no-ignore          index everything: no .gitignore, no .xerjignore, no\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20.git/info/exclude, no built-in defaults. Hidden files\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(.env, .git/, .ssh) stay skipped either way — that is\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20not an ignore rule, it is what keeps secrets out.\n\
         \x20\x20\x20\x20--no-default-ignores keep the ignore files, drop only the built-in list\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20({defaults})\n\
         \x20\x20\x20\x20--max-file-gb <N>    skip+record oversized non-streamable files, cap in whole\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20GB (default 2). Use --max-file-mb/--max-file-bytes for a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20smaller cap; the three are mutually exclusive\n\
         \x20\x20\x20\x20--max-file-mb <N>    same cap expressed in whole MB (e.g. 10 for a source\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20watcher); mutually exclusive with --max-file-gb/-bytes\n\
         \x20\x20\x20\x20--max-file-bytes <N> same cap expressed in whole bytes; mutually exclusive\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20with --max-file-gb/--max-file-mb\n\
         \x20\x20\x20\x20--sample <N>         records sampled per file for inference (default 500)\n\
         \x20\x20\x20\x20--no-semantic        skip semantic_text on body fields (pure BM25+keyword)\n\
         \x20\x20\x20\x20--code-analyzer <A>  standard (default) or code: `code` declares the built-in\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20identifier-aware analyzer (splits snake_case/camelCase,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20keeps the original token) on NEWLY created datasets\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20holding source code; existing indexes keep theirs.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Measured +0.11 file-localization acc@5 (#1198); costs\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20~15% index size and ~2.4x text-analysis time\n\
         \x20\x20\x20\x20--brain <NAME>       second-brain name; relationship edges land in\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20.xerj-memory-<NAME>-edges (default: folder name slug)\n\
         \x20\x20\x20\x20--no-graph           skip relationship detection (wikilinks, local links,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20section order, directory chains) — no edges are written\n\
         \x20\x20\x20\x20--watch              index once, then stay resident and reindex what changes.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Needs --no-graph. A graph-path rerun sweeps a deleted\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20file (exit 3, files-removed) and indexes an added file\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20that fits a frozen dataset. A file that needs a new\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20dataset still stops (exit 3, stale-resume-plan) until\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--fresh. One OS watch per indexed directory, no polling;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20respects the same ignore rules as a re-run.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20docs/LIVE_REINDEXING.md has the measurements.\n\
         \x20\x20\x20\x20--debounce <MS>      --watch quiet period before a pass (default 400, max\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x2060000). One editor save is several filesystem events.\n\
         \x20\x20\x20\x20--max-minutes <N>    stop and ask before indexing if phase A's MEASURED estimate\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is longer than this (default 10; 0 disables the gate;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20max 10080). See ESTIMATE + DECISION GATE below.\n\
         \x20\x20\x20\x20--approve <ID>       answer a decision request: proceed | fast | cancel.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20`fast` also applies --no-semantic --no-graph. `narrower`\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is NOT accepted here — it means re-running against a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20subdirectory, which this flag cannot do.\n\
         \x20\x20\x20\x20--yes, -y            alias for --approve proceed. When the default index\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20plan does not fit the disk budget, --yes takes the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20recommended plan that does fit. It does not index a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20plan that is over budget. It does not delete indexed\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20files a smaller plan leaves out; that is --drop-indexed.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20On `autoindex remove` it is the confirmation: without\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20it a non-terminal run prints the plan and exits 4.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20It does not choose a folder.\n\
         \x20\x20\x20\x20--root <DIR>         (`autoindex remove`) the one folder to drop when\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--prefix holds more than one. Refused on any other\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20command\n\
         \x20\x20\x20\x20--max-index-size <N> cap the estimated index (bytes, or a KB/MB/GB suffix).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20The disk budget is the free space under the node's\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20flood-stage watermark; this flag lowers that cap.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x200 is rejected\n\
         \x20\x20\x20\x20--plan <NAME>        index with one plan: default, skip-vendored, skip-data,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20code-and-docs, no-vectors, cap-1mb,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20code-and-docs-no-vectors. A named plan runs even\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20when the estimate says it does not fit (warned).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20See DISK BUDGET.\n\
         \x20\x20\x20\x20--drop-indexed       delete already-indexed files a reduced plan leaves\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20out. Without this flag that re-run exits 4 and lists\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20them. --plan and --yes do not agree to the deletion.\n\
         \x20\x20\x20\x20--dry-run            walk+sniff+infer, print the plan, index nothing.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20`autoindex map <folder>` is this same preview\n\
         \x20\x20\x20\x20--walk-only          stop after the walk. Prints each top-level folder's\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20file count and size, what the ignore rules would drop\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20and which rule, and the totals. Stops before hashing:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20no digest, no sniff, nothing indexed. Ignore files are\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20still read, as on every walk. The node is still\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20contacted, as with --dry-run. --dry-run itself still\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20hashes, sniffs and prints the plan\n\
         \x20\x20\x20\x20--label <FILE>       label each document at ingest through POST /_decide\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(#1062): the question-set file's noul/choice questions\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20are rendered against each record's fields and the vote\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20stamped on as label + label_p (and label_<id> +\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20label_<id>_p per question). p is the RAW decide\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20confidence - calibration is #1063. One /_decide call\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20per noul (per option, for a choice) per document.\n\
         \x20\x20\x20\x20--json               machine-readable RESULT on stdout (map: raw catalog docs).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Orthogonal to --progress, which owns stderr.\n\
         \x20\x20\x20\x20--progress <MODE>    liveness on stderr: auto|plain|json|none (default auto).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20auto = live redrawn line when stderr is a terminal,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20otherwise one parseable line per interval. plain and\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20json force that shape everywhere (CI, pipes, agents).\n\
         \x20\x20\x20\x20--progress-interval <SECS>\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20progress cadence, 1..=3600 (default 1 on a terminal,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x205 otherwise). This is the guaranteed upper bound on\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20silence between phases.\n\
         \x20\x20\x20\x20--quiet              errors only (implies --progress none). The decision gate\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20NEVER prompts under this flag, even at a terminal — the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20question would be silenced with everything else. It emits\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the JSON decision request on stdout and exits 4 instead,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20exactly like an agent-driven run. See ESTIMATE + DECISION\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20GATE.\n\
         \x20\x20\x20\x20--dataset <SLUG>     (map, no folder) show a single catalog dataset\n\
         \x20\x20\x20\x20--offset <N>         (map, no folder) skip the first N dataset detail sections\n\
         \x20\x20\x20\x20--limit <N>          (map, no folder) how many dataset detail sections to print\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(default: all of them when there are few, otherwise\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the first page — the summary table always lists every\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20dataset)\n\
         \x20\x20\x20\x20--disable-feedback   do not print the feedback invitation above; honoured in\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20any position, including after --help (env\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20XERJ_DISABLE_FEEDBACK=true)\n\
         \x20\x20\x20\x20--help, -h           this help\n\
         \n\
         IGNORE RULES:\n\
         \x20\x20\x20\x20The fastest file is the one that is never read, so junk is dropped during\n\
         \x20\x20\x20\x20the walk, not after it. Honoured, highest precedence first:\n\
         \x20\x20\x20\x20\x20\x20.xerjignore  same syntax as .gitignore, XERJ-only. Use it to exclude\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20something git tracks, or to re-include (!pattern) something\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20git ignores. Outranks .gitignore at any depth.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20!pattern cannot re-include a hidden (dot-named)\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20entry: see HIDDEN below.\n\
         \x20\x20\x20\x20\x20\x20.gitignore   including nested ones and negation; the closest file wins.\n\
         \x20\x20\x20\x20\x20\x20.git/info/exclude  per-checkout excludes, including nested checkouts.\n\
         \x20\x20\x20\x20\x20\x20built-in     {defaults}\n\
         \x20\x20\x20\x20Both git-owned kinds stop at a repository boundary, exactly as git does:\n\
         \x20\x20\x20\x20a .gitignore above a nested checkout does not judge files inside it, so\n\
         \x20\x20\x20\x20vendored and submoduled trees keep their own rules. .xerjignore and the\n\
         \x20\x20\x20\x20built-in list are XERJ's, not git's, and apply throughout the folder.\n\
         \x20\x20\x20\x20Your global gitignore (core.excludesFile) is NOT read: a machine-wide\n\
         \x20\x20\x20\x20preference should not silently decide what is in your index.\n\
         \x20\x20\x20\x20A directory the rules reject is never descended, so nothing inside it is\n\
         \x20\x20\x20\x20stat-ed, hashed or sent. Every run prints what was dropped and by which\n\
         \x20\x20\x20\x20rule; --dry-run and --walk-only additionally count the non-hidden files inside each\n\
         \x20\x20\x20\x20pruned directory (bounded — past the budget the count is reported as\n\
         \x20\x20\x20\x20`at least N`, and xerj-done carries\n\
         \x20\x20\x20\x20ignored_files_in_pruned_dirs_exact=false to say so).\n\
         \x20\x20\x20\x20The folder you name is never rejected: if it is itself ignored, it is\n\
         \x20\x20\x20\x20indexed anyway and the run says which rule it would have matched.\n\
         \x20\x20\x20\x20HIDDEN: dot-named files and directories below the folder (.env, .git,\n\
         \x20\x20\x20\x20.ssh, .<tool>/) are skipped by a separate rule that runs before any\n\
         \x20\x20\x20\x20ignore file. No .xerjignore !pattern and no --no-ignore lifts it (the\n\
         \x20\x20\x20\x20run warns when a .xerjignore tries). To index one, name it as the\n\
         \x20\x20\x20\x20root, which is never rejected: xerj autoindex ./.openfox --prefix openfox\n\
         \n\
         OBJECT STORAGE:\n\
         \x20\x20\x20\x20`xerj autoindex s3://bucket/prefix` indexes objects instead of files.\n\
         \x20\x20\x20\x20`r2://bucket/prefix` is the same thing and needs --endpoint-url with the\n\
         \x20\x20\x20\x20account host. Credentials come from AWS_ACCESS_KEY_ID /\n\
         \x20\x20\x20\x20AWS_SECRET_ACCESS_KEY (+ AWS_SESSION_TOKEN) in the ENVIRONMENT ONLY — no\n\
         \x20\x20\x20\x20profile files, no instance metadata, no SSO — and never from the URL.\n\
         \x20\x20\x20\x20A non-empty prefix is treated as a FOLDER: s3://b/docs lists docs/ and not\n\
         \x20\x20\x20\x20docs-old/. The effective prefix is printed at the start of the run.\n\
         \x20\x20\x20\x20CHANGE DETECTION is the object's ETag plus its size, recorded in\n\
         \x20\x20\x20\x20<state-dir>/object-source.json. The ETag is treated as an opaque token, so\n\
         \x20\x20\x20\x20a multipart upload's `-N` form is fine and nothing is compared to an MD5.\n\
         \x20\x20\x20\x20A re-run downloads only what changed; an unchanged prefix downloads nothing.\n\
         \x20\x20\x20\x20THE BYTES ARE MIRRORED to <state-dir>/object-cache/<bucket-prefix>/ and the\n\
         \x20\x20\x20\x20ordinary walk runs over that mirror, so extraction sees exactly the bytes it\n\
         \x20\x20\x20\x20sees for a folder. Budget local disk for the prefix you index. The INDEX\n\
         \x20\x20\x20\x20still lives on the XERJ node at --url; nothing is written to the bucket.\n\
         \x20\x20\x20\x20Objects whose keys cannot be a safe local path are skipped and named:\n\
         \x20\x20\x20\x20`..`, an empty component, a backslash, a control character, a trailing dot\n\
         \x20\x20\x20\x20or space, a Windows reserved name, and any dot-prefixed component (so a\n\
         \x20\x20\x20\x20bucket's .env and .git/ stay out of the index, exactly as in a folder).\n\
         \x20\x20\x20\x20COST: one run costs ceil(objects/1000) class-A LIST requests plus one\n\
         \x20\x20\x20\x20class-B GET per changed object, and never writes. Counts are BILLED WIRE\n\
         \x20\x20\x20\x20ATTEMPTS: a request the store throttled and the client retried counts once\n\
         \x20\x20\x20\x20per attempt, because that is what the store bills. Every run prints what it\n\
         \x20\x20\x20\x20spent and what running it hourly or every 5 minutes would spend against a\n\
         \x20\x20\x20\x201,000,000/month class-A allowance — read that line before adding a cron.\n\
         \x20\x20\x20\x20A run that fails mid-transfer records the objects it already fetched, so a\n\
         \x20\x20\x20\x20re-run pays for the rest only — but it re-lists, so the LIST cost recurs.\n\
         \n\
         PDF EXTRACTION:\n\
         \x20\x20\x20\x20Each PDF uses a fresh process. Limits: 512 MiB input, 32 MiB worker output,\n\
         \x20\x20\x20\x20100,000 pages, and a 1.5 GiB address-space cap on Unix where the OS accepts\n\
         \x20\x20\x20\x20RLIMIT_AS. If the OS refuses that cap (EINVAL or EPERM, as on macOS 27), the\n\
         \x20\x20\x20\x20refusal is logged once and the worker still starts. The cap is crash isolation,\n\
         \x20\x20\x20\x20not a security sandbox; non-Unix platforms have no OS memory cap.\n\
         \x20\x20\x20\x20Image-only pages need OCR. Page parse failures reject the whole PDF instead\n\
         \x20\x20\x20\x20of silently creating a partial index. XERJ_PDF_WORKER_BIN is a trusted,\n\
         \x20\x20\x20\x20developer-only executable override.\n\
         \x20\x20\x20\x20On Linux, fresh runs attempt to retain each validated PDF extraction in\n\
         \x20\x20\x20\x20bounded anonymous storage under --state-dir. Live admission-time checks\n\
         \x20\x20\x20\x20attempt to preserve disk and descriptor headroom; refused artifacts are\n\
         \x20\x20\x20\x20parsed again in phase B and reported under pdf_extraction_reuse. Other\n\
         \x20\x20\x20\x20platforms currently disable this optimization. Frozen-plan resumes skip\n\
         \x20\x20\x20\x20phase A and parse unfinished PDFs once.\n\
         \x20\x20\x20\x20--pdf-workers bounds both phase-A parser processes and phase-B replay\n\
         \x20\x20\x20\x20materialization; --workers does not widen that PDF memory gate.\n\
         \n\
         INCREMENTAL RECONCILIATION:\n\
         \x20\x20\x20\x20Generated journals with --no-graph reconcile added, removed, moved, and changed\n\
         \x20\x20\x20\x20files. Graph-enabled journals absorb an added file that fits a frozen dataset,\n\
         \x20\x20\x20\x20delete documents for a file that disappeared, and leave a file that needs a new\n\
         \x20\x20\x20\x20dataset unindexed until --fresh rebuilds the plan. Each\n\
         \x20\x20\x20\x20changed generation currently copies and prepares the full corpus (O(N)); the\n\
         \x20\x20\x20\x20latest full snapshot remains retained, and cleanup re-reads protected artifacts\n\
         \x20\x20\x20\x20to verify them. --snapshot-max-gb limits logical staged payload bytes before each\n\
         \x20\x20\x20\x20write; it is not a physical disk-space or peak-allocation guarantee.\n\
         \n\
         EMBEDDINGS:\n\
         \x20\x20\x20\x20autoindex sends semantic_text to the running server; it does not choose the\n\
         \x20\x20\x20\x20server's embedding backend. The default is lexical (not neural). For the\n\
         \x20\x20\x20\x20experimental ONNX backend, start xerj with `--embed-mode onnx-experimental\n\
         \x20\x20\x20\x20--onnx-model MODEL.onnx --onnx-tokenizer tokenizer.json`, then run autoindex.\n\
         \x20\x20\x20\x20ONNX runs only for fields inferred as semantic_text (normally long body text;\n\
         \x20\x20\x20\x20short/structured datasets may infer none). Use --dry-run or `autoindex map` to\n\
         \x20\x20\x20\x20confirm a semantic field before attributing an indexing result to embeddings.\n\
         \n\
         PROGRESS STREAM:\n\
         \x20\x20\x20\x20stdout is the RESULT, stderr is PROGRESS — pipe them separately.\n\
         \x20\x20\x20\x20Every run that reaches an exit — success OR error — ends with one\n\
         \x20\x20\x20\x20terminal line, in every progress mode EXCEPT `none` (which --quiet\n\
         \x20\x20\x20\x20selects), so an outcome never has to be guessed from silence:\n\
         \x20\x20\x20\x20\x20\x20xerj-done ok=true exit=3 reason=completed-with-junk wall=57.6s …\n\
         \x20\x20\x20\x20The terminal line also carries code coverage — code_files=N\n\
         \x20\x20\x20\x20code_files_indexed=M code_files_junked=K — so a corpus whose\n\
         \x20\x20\x20\x20source files were ALL dropped cannot print the same line as a\n\
         \x20\x20\x20\x20healthy one. code_files>0 with code_files_indexed=0 is always a\n\
         \x20\x20\x20\x20defect, and the run says so in words as well.\n\
         \x20\x20\x20\x20--quiet/--progress none prints no progress and NO terminal line\n\
         \x20\x20\x20\x20(only a fatal `error:` line, if any) — poll `autoindex status\n\
         \x20\x20\x20\x20--state-dir <dir>` or read the exit code instead of waiting for\n\
         \x20\x20\x20\x20output that never comes.\n\
         \x20\x20\x20\x20(A run killed by a signal cannot print one either; a missing\n\
         \x20\x20\x20\x20terminal line after the process is gone means it died, not that it\n\
         \x20\x20\x20\x20finished.)\n\
         \x20\x20\x20\x20--progress plain writes TWO lines per tick, in one write:\n\
         \x20\x20\x20\x20xerj-bar [######################--] 93.4% | index | 8082/8083 items | eta 7s\n\
         \x20\x20\x20\x20xerj-progress phase=index basis=bytes pct=93.4 items=8082/8083 eta_s=7.2 …\n\
         \x20\x20\x20\x20`xerj-bar` is the DISPLAY line — self-contained, meant to be shown\n\
         \x20\x20\x20\x20to a person verbatim by whatever is relaying the run. It is spaced\n\
         \x20\x20\x20\x20at most one per 15s, plus one per phase change and never two closer\n\
         \x20\x20\x20\x20than 2s, so it does not flood a transcript. Short phases therefore\n\
         \x20\x20\x20\x20draw fewer bars than transitions — read the machine line for those.\n\
         \x20\x20\x20\x20`xerj-progress` is the MACHINE line and keeps\n\
         \x20\x20\x20\x20the --progress-interval cadence; parse that one. --progress json\n\
         \x20\x20\x20\x20stays one object per line and carries the same rendered string in\n\
         \x20\x20\x20\x20a `bar` field on the same schedule: a string on the ticks that owe\n\
         \x20\x20\x20\x20a bar, null in between.\n\
         \x20\x20\x20\x20Every record is identified by its leading token, and that is\n\
         \x20\x20\x20\x20enforced: paths and other outside text are stripped of control\n\
         \x20\x20\x20\x20characters and bounded before they reach any line, so a crafted\n\
         \x20\x20\x20\x20filename cannot forge a record or repaint your terminal.\n\
         \x20\x20\x20\x20`pct`/`eta_s` are the literal word `unknown` (JSON null) whenever they\n\
         \x20\x20\x20\x20cannot be computed honestly, never a filler number — and the drawn\n\
         \x20\x20\x20\x20bar obeys the same rule: `[????…]` when there is no denominator,\n\
         \x20\x20\x20\x20and a full bar only at a real 100%.\n\
         \x20\x20\x20\x20PHASES, in order. Every long step is a phase of its own, so a line\n\
         \x20\x20\x20\x20that still says `scan` at 100% means scan — not indexing:\n\
         \x20\x20\x20\x20\x20\x20default (graph) path: walk, hash, scan, prepare, graph, index,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20graph-corpus, finalize-refresh (one refresh per dataset,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20the in-flight index named), finalize-count,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20finalize-correlate, finalize-histogram, finalize-catalog\n\
         \x20\x20\x20\x20\x20\x20--no-graph path:      walk, hash, scan, prepare (install mappings),\n\
         \x20\x20\x20\x20\x20\x20\x20\x20snapshot (seal + extract, bytes), index (sealed bulk bytes, the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20in-flight file named), finalize-catalog, finalize-refresh (one\n\
         \x20\x20\x20\x20\x20\x20\x20\x20refresh per dataset), finalize-verify (read-backs batched\n\
         \x20\x20\x20\x20\x20\x20\x20\x20into per-index digest windows, one search per window). A\n\
         \x20\x20\x20\x20\x20\x20\x20\x20resumed run starts at `replay` and its `index` phase\n\
         \x20\x20\x20\x20\x20\x20\x20\x20counts only the operations still to apply.\n\
         \n\
         ESTIMATE + DECISION GATE:\n\
         \x20\x20\x20\x20Phase A already reads and parses every file to sniff and sample it, so it\n\
         \x20\x20\x20\x20measures throughput per format family ON THIS MACHINE. autoindex turns that\n\
         \x20\x20\x20\x20into a RANGE (never one confident number) for the indexing phase and prints\n\
         \x20\x20\x20\x20the basis with it. Families phase A never read end to end are named and left\n\
         \x20\x20\x20\x20OUT of the arithmetic instead of being priced at some other family's rate;\n\
         \x20\x20\x20\x20if nothing could be measured, it says so and does not gate.\n\
         \x20\x20\x20\x20The range covers CLIENT-SIDE EXTRACTION only — server indexing, embedding\n\
         \x20\x20\x20\x20and network time are not in it, which is why the gate compares the upper end.\n\
         \x20\x20\x20\x20If that upper end is longer than --max-minutes and no --approve/--yes was\n\
         \x20\x20\x20\x20given, nothing is indexed: a JSON decision request goes to stdout and the\n\
         \x20\x20\x20\x20process exits 4 (a code of its own — 1 is the catch-all for real failures).\n\
         \x20\x20\x20\x20Answer by re-running the same command with --approve proceed|fast|cancel.\n\
         \x20\x20\x20\x20A person at a terminal is prompted instead — but ONLY when the question can\n\
         \x20\x20\x20\x20actually be seen. All three must hold: stdin is a terminal, stderr is a\n\
         \x20\x20\x20\x20terminal, and the progress surface is on. --quiet / --progress none silences\n\
         \x20\x20\x20\x20the question, so those runs are never prompted and never wait on stdin; a\n\
         \x20\x20\x20\x20piped or agent-driven run is not prompted either. Every un-prompted run\n\
         \x20\x20\x20\x20behaves identically: the JSON decision request goes to stdout (which --quiet\n\
         \x20\x20\x20\x20does NOT silence) and the process exits 4. The payload's\n\
         \x20\x20\x20\x20`prompt_not_offered_because` says which of the three was missing.\n\
         \x20\x20\x20\x20autoindex never waits on stdin for a question it did not print.\n\
         \n\
         WORK ORDER:\n\
         \x20\x20\x20\x20Phase B drains source and documents first, then configuration, then\n\
         \x20\x20\x20\x20structured data, then logs and line files, and vendored/generated/minified\n\
         \x20\x20\x20\x20paths last — so stopping early, or searching while it runs, still gives you\n\
         \x20\x20\x20\x20the files you cared about. Inside a band the biggest file starts first (with\n\
         \x20\x20\x20\x20several workers it then runs alongside the rest instead of becoming the\n\
         \x20\x20\x20\x20tail); a single-worker run goes smallest-first instead. One exception: a file\n\
         \x20\x20\x20\x20so large that it outlasts everything ranked above it starts first whatever\n\
         \x20\x20\x20\x20band it is in. The full breakdown is printed with the plan.\n\
         \n\
         RESUME POLICY:\n\
         \x20\x20\x20\x20{resume_policy_help}.\n\
         \x20\x20\x20\x20On a graph-enabled or pre-generation journal the durable plan supports no-op\n\
         \x20\x20\x20\x20resume and same-path content replacement. Files added after that plan was frozen\n\
         \x20\x20\x20\x20are reported as skipped and are not indexed; --fresh rebuilds the plan in place\n\
         \x20\x20\x20\x20and picks them up. Removing an indexed file is refused there before any remote\n\
         \x20\x20\x20\x20mutation: its documents are already live and nothing on that path deletes them.\n\
         \x20\x20\x20\x20Restore the file and rerun, or rebuild — in place by deleting the published\n\
         \x20\x20\x20\x20indices and the state directory, or isolated under a new --state-dir, --prefix\n\
         \x20\x20\x20\x20and --brain (or --no-graph), validated before you switch readers.\n\
         \x20\x20\x20\x20--fresh is not a destination wipe. A file that is still present and whose\n\
         \x20\x20\x20\x20content key changed is republished, and the documents previously published\n\
         \x20\x20\x20\x20for that path are removed. A disappeared file is still refused before any\n\
         \x20\x20\x20\x20remote mutation, and its documents stay. --fresh is refused outright once a\n\
         \x20\x20\x20\x20durable corpus generation exists (re-run without it — the generated path\n\
         \x20\x20\x20\x20reconciles the change incrementally). For an independent rebuild use a new\n\
         \x20\x20\x20\x20--state-dir and a new --prefix, plus a new --brain when graph is enabled\n\
         \x20\x20\x20\x20(or --no-graph).\n\
         \x20\x20\x20\x20Validate before switching readers; explicitly clean the shared\n\
         \x20\x20\x20\x20autoindex-catalog and old target only after validation.\n\
         \n\
         WATCH (OBJECT STORAGE):\n\
         \x20\x20\x20\x20`xerj autoindex s3://<bucket>/<prefix> --watch` polls the bucket with\n\
         \x20\x20\x20\x20ListObjectsV2 and emits one JSON event per changed object (added / changed /\n\
         \x20\x20\x20\x20deleted) on stdout, or to --events-out. It does NOT index: pipe the feed into\n\
         \x20\x20\x20\x20the indexer. Deletes are detected only by a full scan (not --append-only).\n\
         \x20\x20\x20\x20A bucket has no inotify, so this is polling, and polling costs money:\n\
         \x20\x20\x20\x20ListObjectsV2 is a CLASS A operation, one call per page (--page-size keys,\n\
         \x20\x20\x20\x201,000 by default and at most), EVERY cycle,\n\
         \x20\x20\x20\x20whether anything changed or not. Cloudflare R2's free tier is 1,000,000\n\
         \x20\x20\x20\x20Class A operations a month, which is ~23 per minute for the whole account:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20objects   list calls/cycle   every 60s      every 300s (default)\n\
         \x20\x20\x20\x20\x20\x20\x20\x201,000     1                  43,200/mo      8,640/mo\n\
         \x20\x20\x20\x20\x20\x20\x20\x2010,000    10                 432,000/mo     86,400/mo\n\
         \x20\x20\x20\x20\x20\x20\x20\x20100,000   100                4,320,000/mo   864,000/mo\n\
         \x20\x20\x20\x20\x20\x20\x20\x201,000,000 1,000              43,200,000/mo  8,640,000/mo\n\
         \x20\x20\x20\x20The default interval is {default_poll}s and the default budget is\n\
         \x20\x20\x20\x20{default_budget} Class A and {default_gets} Class B operations a month (20% of each\n\
         \x20\x20\x20\x20free tier, because the rest of the account spends from it too). The budget is a\n\
         \x20\x20\x20\x20CIRCUIT BREAKER, checked on every cycle: a projection over budget (also when the\n\
         \x20\x20\x20\x20bucket grows mid-watch), or a month whose budget is spent (counted across\n\
         \x20\x20\x20\x20restarts in <state-dir>/objwatch-spend.json), STOPS the watch with exit 4 and a\n\
         \x20\x20\x20\x20decision request. --allow-cost accepts the spend, --append-only makes a growing\n\
         \x20\x20\x20\x20key space cost one call per cycle, and a narrower prefix is free.\n\
         \x20\x20\x20\x20--poll-interval <secs>   seconds between cycles (default {default_poll}, max {max_poll})\n\
         \x20\x20\x20\x20--once / --max-cycles N  stop after one / N cycles\n\
         \x20\x20\x20\x20--no-fetch               metadata-only: compare ETag+size+mtime, never GET\n\
         \x20\x20\x20\x20--append-only            list only keys above the highest one seen (start-after):\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20one list call per cycle, and NO delete or older-key\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20change detection — opt in only for an append-only\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20key space such as date-partitioned logs\n\
         \x20\x20\x20\x20--dry-run                one cycle, no GETs, nothing emitted, no journal: price\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the poll. The listing itself is real and IS billed\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(ceil(keys/page-size) Class A ops, on the ledger)\n\
         \x20\x20\x20\x20--endpoint-url <URL>     S3 endpoint (else AWS_ENDPOINT_URL). R2:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20https://<account>.r2.cloudflarestorage.com\n\
         \x20\x20\x20\x20--region <R>             signing region (else AWS_REGION, else auto)\n\
         \x20\x20\x20\x20--max-object-mb <N>      byte cap per fetched object (default 64)\n\
         \x20\x20\x20\x20--max-monthly-ops <N>    Class A budget per month (default {default_budget})\n\
         \x20\x20\x20\x20--max-monthly-gets <N>   Class B (GET) budget per month (default {default_gets})\n\
         \x20\x20\x20\x20--allow-cost             proceed although the projection is over budget\n\
         \x20\x20\x20\x20--page-size <N>          keys per list call (default 1000, the API maximum)\n\
         \x20\x20\x20\x20--events-out <PATH>      write the JSONL change feed here instead of stdout\n\
         \x20\x20\x20\x20--status-file <PATH>     running counts an operator can read (default\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20<state-dir>/objwatch-status.json)\n\
         \x20\x20\x20\x20Credentials come from the environment only: AWS_ACCESS_KEY_ID,\n\
         \x20\x20\x20\x20AWS_SECRET_ACCESS_KEY, optionally AWS_SESSION_TOKEN. Never from a flag.\n\
         \n\
         DISK BUDGET:\n\
         \x20\x20\x20\x20Before hashing, autoindex counts files by extension (code, prose, pdf,\n\
         \x20\x20\x20\x20data, binary, other) and by top-level folder, and estimates the index\n\
         \x20\x20\x20\x20from measured per-kind ratios. The budget is the free space on the\n\
         \x20\x20\x20\x20node's data volume under the flood-stage watermark (the same\n\
         \x20\x20\x20\x20GET /_cat/allocation read that refuses a volume already at the\n\
         \x20\x20\x20\x20watermark), optionally lowered with --max-index-size. A node that does\n\
         \x20\x20\x20\x20not report disk.flood_stage is not guessed at. When the default plan\n\
         \x20\x20\x20\x20fits, the run continues. When it does not, stdout carries a JSON plan\n\
         \x20\x20\x20\x20list and the run exits 4 unless --yes takes the recommended plan that\n\
         \x20\x20\x20\x20fits, or --plan names one (a named plan is the override: it runs\n\
         \x20\x20\x20\x20over the estimate with a warning; the estimate errs high).\n\
         \x20\x20\x20\x20A reduced plan on a folder that is already indexed does not delete\n\
         \x20\x20\x20\x20the files it leaves out. That re-run exits 4 and lists the files\n\
         \x20\x20\x20\x20and the datasets a reconcile would remove, until --drop-indexed\n\
         \x20\x20\x20\x20confirms it. --yes chooses a plan; it does not agree to delete\n\
         \x20\x20\x20\x20indexed data.\n\
         \x20\x20\x20\x20A terminal with the question visible is asked. Kinds without a\n\
         \x20\x20\x20\x20calibration are reported as unmeasured and do not refuse the run.\n\
         \x20\x20\x20\x20Skipped files are listed after the ok: line (up to 20, then a count).\n\
         \x20\x20\x20\x20A folder already indexed under another prefix is named before the walk.\n\
         \n\
         EXIT CODES: 0 complete (also: gate answered with --approve cancel);\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x203 completed-with-junk (junk recorded, never fatal) — SUCCESS:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the index is searchable, and the summary on stdout opens\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20with a line starting `ok:` that says so (xerj-done carries\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20ok=true). Treat 0 and 3 alike as a finished run. This\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20includes a dataset whose mapping the server REFUSED: its\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20files are recorded as junk with the server's reason, the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20xerj-done line carries datasets_refused / files_refused,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20and every other dataset is indexed; or\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20catalog-alias-sweep-failed — the corpus IS indexed and the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20journal committed; only the catalog's duplicate-alias cleanup\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20could not run. Read `reason` on the xerj-done line to tell the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20two apart, and rerun the same command once the reported server\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20condition clears;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20or stale-resume-plan — files on disk appeared after the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20resume plan was frozen. They are not counted as junk;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20`skipped_appeared` names them. Re-run with --fresh to\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20rebuild the plan in place and include them. Read `reason`\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20to tell this apart from completed-with-junk;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20or files-removed — a graph-path rerun deleted documents,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20catalog rows and edges for files that left the folder.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20The index matches the folder; the next unchanged rerun is\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20exit 0. Read `reason` to tell this apart from\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20completed-with-junk;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x204 NEEDS A DECISION — the estimate exceeded --max-minutes, the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20estimated index does not fit the disk budget, a reduced\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20plan would delete files that are already indexed, or a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--watch poll cost more than --max-monthly-ops; nothing was\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20indexed and a JSON decision request is on stdout. A plan\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20that does not fit is not started, including with --yes.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Already-indexed files are not deleted unless --drop-indexed\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is also set;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x202 usage; 1 endpoint/journal failure, a refused corpus removal, a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20refused unsafe state transition, or pdf-worker-unavailable — every PDF\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20failed because the isolated parser could not start. Other files may\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20already be indexed. Read `reason` on the xerj-done line; the summary\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20on stdout opens with `not ok:`\n",
        feedback_block = xerj_common::feedback::block(feedback),
        default_poll = crate::objwatch::cost::DEFAULT_POLL_INTERVAL_SECS,
        default_budget = crate::objwatch::cost::DEFAULT_MAX_MONTHLY_CLASS_A,
        default_gets = crate::objwatch::cost::DEFAULT_MAX_MONTHLY_CLASS_B,
        max_poll = MAX_POLL_INTERVAL_SECS,
        max_bulk_mb = MAX_BULK_MB,
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

/// Normalise one of the mutually-exclusive `--max-file-{gb,mb,bytes}` flags
/// into the shared byte cap (#1233). `unit` is bytes-per-unit (1<<30 for GB,
/// 1<<20 for MB, 1 for bytes). The flags are mutually exclusive: a second one
/// is a hard usage error, not last-wins, so a resumed generation can never
/// disagree with itself about the cap. Parsing as `u64` rejects negatives and
/// non-numbers with the actionable exit-2 error the reporter saw; the multiply
/// is overflow-checked.
fn set_max_file_bytes(
    slot: &mut Option<u64>,
    flag: &str,
    raw: Option<String>,
    unit: u64,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(
            "specify only one of --max-file-gb / --max-file-mb / --max-file-bytes".to_owned(),
        );
    }
    let n: u64 = raw
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("{flag} needs a non-negative integer"))?;
    let bytes = n
        .checked_mul(unit)
        .ok_or_else(|| format!("{flag} value is too large"))?;
    *slot = Some(bytes);
    Ok(())
}

/// The note for a `--prefix` that was not used exactly as typed (#1845).
///
/// A prefix keeps only runs of `[a-z0-9]` joined by single `-`, so
/// `forbidden-lands-combat-` and `forbidden-lands-combat_` both become
/// `forbidden-lands-combat`, and its indices `forbidden-lands-combat-docs`. An
/// agent that added the trailing separator to keep this project apart from a
/// sibling (`forbidden-lands-combat-sauvegarde`) got no word that it had been
/// dropped, and the two still collide under `forbidden-lands-combat-*`.
/// `None` when the prefix was used as given.
pub(crate) fn prefix_rewrite_note(given: &str, used: &str) -> Option<String> {
    if given == used || used.is_empty() {
        return None;
    }
    let trailing = given.ends_with(['-', '_']);
    let why = if trailing {
        "a trailing '-' or '_' is not kept"
    } else {
        "a prefix keeps only lower-case [a-z0-9] runs joined by single '-'"
    };
    Some(format!(
        "--prefix {given:?} is used as {used:?} ({why}). Indices are named {used}-<dataset>, so \
         a {used}-* wildcard also matches any other prefix that starts with {used}- (for \
         example {used}-other-<dataset>). If a sibling project's prefix starts with {used}-, \
         give this one a prefix that is not the start of another (for example {used}-main)"
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
    let mut replace_prefix = false;
    let mut prefix_explicit = false;
    let mut prefix_given: Option<String> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut endpoint_url: Option<String> = None;
    let mut endpoint_explicit = false;
    let mut fresh = false;
    let mut follow_symlinks = false;
    let mut follow_symlinks_outside_root = false;
    let mut stub_globs: Vec<String> = Vec::new();
    let mut no_ignore = false;
    let mut no_default_ignores = false;
    // One of --max-file-gb / --max-file-mb / --max-file-bytes sets this, all
    // normalised to bytes; `None` until one is seen so a second size flag is a
    // hard error and the default (2 GB) applies only when none is given (#1233).
    let mut max_file_bytes: Option<u64> = None;
    let mut sample = 500usize;
    let mut no_semantic = false;
    let mut code_analyzer = crate::infer::CodeAnalyzer::default();
    let mut brain: Option<String> = None;
    let mut no_graph = false;
    let mut watch = false;
    let mut debounce_ms: Option<u64> = None;
    let mut dry_run = false;
    let mut walk_only = false;
    let mut max_minutes = DEFAULT_MAX_MINUTES;
    let mut max_minutes_explicit = false;
    let mut approve: Option<Approval> = None;
    let mut approve_explicit = false;
    // Set only by `--yes`/`-y`. `--approve proceed` is the indexing gate and
    // is not a confirmation for `autoindex remove`.
    let mut yes_flag = false;
    let mut remove_root: Option<String> = None;
    let mut markdown = false;
    let mut code_analyzer_explicit = false;
    let mut max_index_bytes: Option<u64> = None;
    let mut max_index_explicit = false;
    let mut budget_plan: Option<String> = None;
    let mut drop_indexed = false;
    let mut json = false;
    let mut quiet = false;
    let mut progress: Option<ProgressMode> = None;
    let mut progress_interval: Option<Duration> = None;
    let mut dataset: Option<String> = None;
    let mut detail_offset: Option<usize> = None;
    let mut detail_limit: Option<usize> = None;
    // Object-storage watch (`--watch` on an s3:// root). Kept in their own
    // variables so the watch route can refuse an index-only flag instead of
    // accepting it and doing nothing with it (#204's class).
    let mut root_raw: Option<String> = None;
    // `watch` itself is declared above, with the folder-indexing flags: there
    // is ONE --watch and it selects a route, it does not belong to one.
    let mut poll_interval: Option<Duration> = None;
    let mut max_cycles: Option<u64> = None;
    let mut watch_once = false;
    let mut no_fetch = false;
    let mut append_only = false;
    let mut region: Option<String> = None;
    let mut max_object_mb: u64 = 64;
    let mut max_monthly_ops: Option<u64> = None;
    let mut max_monthly_gets: Option<u64> = None;
    let mut allow_cost = false;
    let mut page_size: u64 = crate::objwatch::cost::MAX_KEYS_PER_LIST;
    let mut events_out: Option<PathBuf> = None;
    let mut status_file: Option<PathBuf> = None;
    let mut watch_flags_used: Vec<&'static str> = Vec::new();
    // `--label` (#1062): ingest-time decide labelling. Kept in its own
    // variable so the routes that do not label can refuse it by name.
    let mut label: Option<PathBuf> = None;

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
            "--prefix" => {
                prefix = it.next().ok_or("--prefix needs a value")?;
                prefix_explicit = true;
                prefix_given = Some(prefix.clone());
            }
            "--replace-prefix" => replace_prefix = true,
            "--state-dir" => state_dir = it.next().map(PathBuf::from),
            "--endpoint-url" => {
                let value = it.next().ok_or(
                    "--endpoint-url needs a URL, e.g. http://127.0.0.1:9000 (MinIO) or \
                     https://<account-id>.r2.cloudflarestorage.com (R2)",
                )?;
                // Validated here rather than at the first request: a typo in an
                // endpoint otherwise surfaces as a dispatch failure after the
                // run has already opened state and printed three phases.
                match reqwest::Url::parse(&value) {
                    Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
                    _ => {
                        return Err(format!(
                            "--endpoint-url must be an http:// or https:// URL, not {value:?}"
                        ))
                    }
                }
                endpoint_explicit = true;
                endpoint_url = Some(value);
            }
            "--fresh" => fresh = true,
            "--label" => {
                label = Some(PathBuf::from(
                    it.next().ok_or("--label needs a question-set file path")?,
                ))
            }
            "--follow-symlinks" => follow_symlinks = true,
            "--follow-symlinks-outside-root" => follow_symlinks_outside_root = true,
            "--stub" => stub_globs.push(it.next().ok_or("--stub needs a glob pattern")?),
            "--no-ignore" => no_ignore = true,
            "--no-default-ignores" => no_default_ignores = true,
            "--max-file-gb" => {
                set_max_file_bytes(&mut max_file_bytes, "--max-file-gb", it.next(), 1u64 << 30)?
            }
            "--max-file-mb" => {
                set_max_file_bytes(&mut max_file_bytes, "--max-file-mb", it.next(), 1u64 << 20)?
            }
            "--max-file-bytes" => {
                set_max_file_bytes(&mut max_file_bytes, "--max-file-bytes", it.next(), 1)?
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
            "--watch" => watch = true,
            "--debounce" => {
                let raw = it
                    .next()
                    .ok_or("--debounce needs a number of milliseconds (0 disables the wait)")?;
                let parsed: u64 = raw.parse().map_err(|_| {
                    format!("--debounce needs an integer from 0 to {MAX_DEBOUNCE_MS}")
                })?;
                if parsed > MAX_DEBOUNCE_MS {
                    return Err(format!(
                        "--debounce must be from 0 to {MAX_DEBOUNCE_MS} milliseconds; past that a \
                         change you just made would sit unindexed for minutes"
                    ));
                }
                debounce_ms = Some(parsed);
            }
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
            "--max-index-size" => {
                max_index_explicit = true;
                let raw = it
                    .next()
                    .ok_or("--max-index-size needs a size in bytes or with a KB/MB/GB suffix")?;
                max_index_bytes = Some(crate::budget::parse_byte_size(&raw)?);
            }
            "--plan" => {
                let raw = it.next().ok_or_else(|| {
                    format!(
                        "--plan needs one of: {}",
                        crate::budget::PLAN_NAMES.join(", ")
                    )
                })?;
                if !crate::budget::PLAN_NAMES.contains(&raw.as_str()) {
                    return Err(format!(
                        "--plan {raw} is not a plan. choices: {}",
                        crate::budget::PLAN_NAMES.join(", ")
                    ));
                }
                if budget_plan.is_some() {
                    return Err("--plan was given twice".into());
                }
                budget_plan = Some(raw);
            }
            "--drop-indexed" => {
                if drop_indexed {
                    return Err("--drop-indexed was given twice".into());
                }
                drop_indexed = true;
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
                yes_flag = true;
            }
            "--dry-run" => dry_run = true,
            "--walk-only" => walk_only = true,
            "--json" => json = true,
            "--md" => {
                json = false;
                markdown = true;
            }
            "--code-analyzer" => {
                let raw = it
                    .next()
                    .ok_or("--code-analyzer needs a value: standard or code")?;
                code_analyzer = crate::infer::CodeAnalyzer::parse(&raw)?;
                code_analyzer_explicit = true;
            }
            "--root" => {
                let raw = it
                    .next()
                    .ok_or("--root needs a directory: `autoindex remove --root <DIR>`")?;
                if remove_root.is_some() {
                    return Err("--root was given twice".into());
                }
                remove_root = Some(raw);
            }
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
            "--dataset" => {
                dataset =
                    Some(it.next().ok_or(
                        "--dataset needs a dataset slug: `autoindex map --dataset <SLUG>`",
                    )?);
            }
            "--offset" => {
                let n: usize = it
                    .next()
                    .ok_or("--offset needs a non-negative integer: `autoindex map --offset <N>`")?
                    .parse()
                    .map_err(|_| "--offset needs a non-negative integer".to_string())?;
                detail_offset = Some(n);
            }
            "--limit" => {
                let n: usize = it
                    .next()
                    .ok_or("--limit needs an integer from 1 to 100: `autoindex map --limit <N>`")?
                    .parse()
                    .map_err(|_| "--limit needs an integer from 1 to 100".to_string())?;
                if !(1..=100).contains(&n) {
                    return Err("--limit must be from 1 to 100".into());
                }
                detail_limit = Some(n);
            }
            "--poll-interval" => {
                watch_flags_used.push("--poll-interval");
                let secs: u64 = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=MAX_POLL_INTERVAL_SECS).contains(n))
                    .ok_or(format!(
                        "--poll-interval needs a number of seconds from 1 to \
                         {MAX_POLL_INTERVAL_SECS}"
                    ))?;
                poll_interval = Some(Duration::from_secs(secs));
            }
            "--max-cycles" => {
                watch_flags_used.push("--max-cycles");
                max_cycles = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .filter(|n: &u64| *n >= 1)
                        .ok_or("--max-cycles needs a number of poll cycles, 1 or more")?,
                );
            }
            "--once" => {
                watch_flags_used.push("--once");
                watch_once = true;
            }
            "--no-fetch" => {
                watch_flags_used.push("--no-fetch");
                no_fetch = true;
            }
            "--append-only" => {
                watch_flags_used.push("--append-only");
                append_only = true;
            }
            "--region" => {
                watch_flags_used.push("--region");
                region = it.next();
            }
            "--max-object-mb" => {
                watch_flags_used.push("--max-object-mb");
                max_object_mb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=1024).contains(n))
                    .ok_or("--max-object-mb needs a number from 1 to 1024")?;
            }
            "--max-monthly-ops" => {
                watch_flags_used.push("--max-monthly-ops");
                max_monthly_ops = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .filter(|n: &u64| *n >= 1)
                        .ok_or("--max-monthly-ops needs a number of operations, 1 or more")?,
                );
            }
            "--max-monthly-gets" => {
                watch_flags_used.push("--max-monthly-gets");
                max_monthly_gets = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .filter(|n: &u64| *n >= 1)
                        .ok_or("--max-monthly-gets needs a number of operations, 1 or more")?,
                );
            }
            "--allow-cost" => {
                watch_flags_used.push("--allow-cost");
                allow_cost = true;
            }
            "--page-size" => {
                watch_flags_used.push("--page-size");
                page_size = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|n| (1..=crate::objwatch::cost::MAX_KEYS_PER_LIST).contains(n))
                    .ok_or(format!(
                        "--page-size needs a number from 1 to {} (the ListObjectsV2 maximum)",
                        crate::objwatch::cost::MAX_KEYS_PER_LIST
                    ))?;
            }
            "--events-out" => {
                watch_flags_used.push("--events-out");
                events_out = it.next().map(PathBuf::from);
            }
            "--status-file" => {
                watch_flags_used.push("--status-file");
                status_file = it.next().map(PathBuf::from);
            }
            // Read out of band by `xerj_common::feedback`, which scans the
            // whole argument list; accepted here so it is not "unknown".
            xerj_common::feedback::DISABLE_FLAG => {}
            "--help" | "-h" => return Ok(Cmd::Help),
            "map" if sub.is_none() && folder.is_none() => sub = Some("map".into()),
            // #2368: `autoindex map <folder>` previews that folder. The folder
            // is the same positional the index command takes; it is accepted
            // only after `map` so `xerj autoindex ./map` still indexes a
            // directory of that name.
            other
                if !other.starts_with('-') && sub.as_deref() == Some("map") && folder.is_none() =>
            {
                root_raw = Some(other.to_string());
                folder = Some(PathBuf::from(other));
            }
            "status" if sub.is_none() && folder.is_none() => sub = Some("status".into()),
            "remove" if sub.is_none() && folder.is_none() => sub = Some("remove".into()),
            other if !other.starts_with('-') && folder.is_none() && sub.is_none() => {
                root_raw = Some(other.to_string());
                folder = Some(PathBuf::from(other))
            }
            // #1586: a second positional folder is not an "unknown argument".
            // Say what the command does (one folder per run) and show the
            // two-run form with a distinct --prefix, so an agent that read
            // "unknown argument: repoB" does not conclude repoA was refused.
            other if !other.starts_with('-') && sub.as_deref() == Some("remove") => {
                return Err(format!(
                    "`autoindex remove` takes the folder as --root, not as a positional \
                     argument. Try: xerj autoindex remove --prefix <P> --root {other}"
                ));
            }
            other if !other.starts_with('-') && folder.is_some() && sub.is_none() => {
                let first = folder
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                return Err(format!(
                    "takes one folder per run, got two: `{first}` and `{other}`; index each \
                     folder in its own run with a distinct --prefix, e.g. `xerj autoindex \
                     {first} --prefix <name-a>` then `xerj autoindex {other} --prefix <name-b>`"
                ));
            }
            other
                if !other.starts_with('-') && sub.as_deref() == Some("map") && folder.is_some() =>
            {
                let first = folder
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                return Err(format!(
                    "`autoindex map` takes at most one folder, got `{first}` and `{other}`. \
                     `xerj autoindex map` with no folder prints the catalog of what is already \
                     indexed. `xerj autoindex map <folder>` previews one folder and indexes nothing"
                ));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let prefix = crate::dataset::sanitize_prefix(&prefix);
    if prefix.is_empty() {
        return Err("--prefix must contain at least one [a-z0-9] character".into());
    }
    // #2057: reject a prefix that cannot name an index before the walk.
    if let Some(err) = crate::dataset::index_prefix_error(&prefix) {
        return Err(err);
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
    // Only meaningful for a run that walks a folder. A bare `map` (no folder),
    // `status`, and `remove` never walk, and the arm below says so — a message
    // about one flag subsuming the other would imply the pair is otherwise
    // honoured. `map <folder>` does walk: it is the `--dry-run` preview.
    let sub_walks_a_folder = folder.is_some();
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

    // Is the root an object-storage URL? Computed HERE, above every watch
    // check, rather than next to the routing block below, because
    // `feat/autoindex-watch` (#967) inserts its own `if watch { … }` block a few
    // lines down and both branches need to know which route a watch is on. See
    // the MERGE NOTE at the routing block.
    let root_is_object_url = root_raw
        .as_deref()
        .map(looks_like_object_url)
        .unwrap_or(false);

    // `--approve fast` is not a hint: it is the answer "index everything, but
    // without the two expensive features", and the run has to actually apply
    // them. Accepting the word and indexing semantically anyway is precisely
    // the accepted-and-silently-ignored class from #204.
    if approve == Some(Approval::Fast) {
        no_semantic = true;
        no_graph = true;
    }
    // `--walk-only` is a dry run that returns after the walk. Implying
    // `dry_run` here, before the contradiction checks and the object-watch
    // router, keeps every "index nothing" guard armed. `--dry-run` alone
    // leaves `walk_only` false, so that path still hashes and prints the plan.
    if walk_only && watch {
        return Err(
            "--walk-only and --watch contradict each other: a walk-only run stops after the \
             folder walk and indexes nothing, and a watcher exists to index changes as they \
             land. Drop one of the two"
                .into(),
        );
    }
    if walk_only && (max_minutes_explicit || approve_explicit) {
        return Err(
            "--max-minutes/--approve/--yes are the indexing decision gate, which --walk-only \
             never reaches: it stops after the walk and does not sniff or index. Drop them"
                .into(),
        );
    }
    if walk_only {
        dry_run = true;
    }
    // #2368: `autoindex map <folder>` previews an unindexed folder. It is the
    // same discovery `--dry-run` runs (walk, hash, sniff, infer, print the
    // plan) and it must not index. Forcing `dry_run` here arms every
    // index-nothing guard. A bare `map` still reads the catalog.
    let map_preview = sub.as_deref() == Some("map") && folder.is_some();
    if map_preview {
        if watch {
            return Err(
                "--watch reindexes a folder as it changes. `autoindex map <folder>` previews \
                 that folder and indexes nothing. Drop --watch"
                    .into(),
            );
        }
        if label.is_some() {
            return Err(
                "--label labels documents at ingest. `autoindex map <folder>` previews that \
                 folder and indexes nothing, so there is nowhere for the labels to go. Drop \
                 --label"
                    .into(),
            );
        }
        if approve == Some(Approval::Cancel) {
            return Err(
                "`autoindex map <folder>` already indexes nothing, so --approve cancel has \
                 nothing to cancel. Drop it"
                    .into(),
            );
        }
        if dataset.is_some() || detail_offset.is_some() || detail_limit.is_some() {
            return Err(
                "--dataset, --offset and --limit page the catalog that `autoindex map` reads \
                 when no folder is given. `autoindex map <folder>` previews a folder and does \
                 not read that catalog. Drop them, or run `xerj autoindex map` with no folder"
                    .into(),
            );
        }
        dry_run = true;
    }
    if approve == Some(Approval::Cancel) && dry_run {
        return Err(
            "--approve cancel and --dry-run contradict each other: a dry run already indexes \
             nothing. Drop one of the two"
                .into(),
        );
    }

    // `--watch` is refused rather than quietly downgraded in each of these
    // cases. Accepting a flag and not doing what it says is the #204 class, and
    // for a watcher the symptom is the worst one available: an index the
    // operator believes is live and is not.
    //
    // `&& !root_is_object_url`: these three are the LOCAL watcher's
    // constraints and none of them is true of an object-storage watch, which
    // indexes nothing, has no resume journal to discard and has no graph route
    // at all. `s3://… --watch --dry-run` is the documented way to price a poll,
    // so an ungated `if watch` here would refuse the flag the docs tell people
    // to use. Object-storage watches are routed further down; see the note
    // there for all three paths through this one positional argument.
    if watch && !root_is_object_url {
        if dry_run {
            return Err(
                "--watch and --dry-run contradict each other: a dry run indexes nothing, and a \
                 watcher exists to index changes as they land. Drop one of the two"
                    .into(),
            );
        }
        if label.is_some() {
            return Err(
                "--watch and --label are not combined today: a watcher would have to re-run the \
                 decide votes of every changed record on every pass, and that pass has not been \
                 built. Index once with --label, then start the watcher without it"
                    .into(),
            );
        }
        if fresh {
            return Err(
                "--watch and --fresh contradict each other: --fresh discards the resume journal \
                 and rebuilds the plan, which a watcher would then redo on every change. Run \
                 `xerj autoindex <folder> --fresh` once, then start the watcher without it"
                    .into(),
            );
        }
        if !no_graph {
            return Err(
                "--watch needs --no-graph today, and that is a real limitation rather than a \
                 formality: the watcher re-runs the generated route, which reconciles added, \
                 changed, renamed and deleted files against a committed generation, including \
                 a file that would be a new dataset. The default graph path sweeps a deleted \
                 file and indexes an added file that fits a frozen dataset, but a file that \
                 needs a new dataset still stops with reason=stale-resume-plan until --fresh, \
                 and this watcher is not built on that route. Re-run as `xerj autoindex \
                 <folder> --watch --no-graph` (relationship detection off), or keep rebuilding \
                 a graph corpus with `xerj autoindex <folder> --fresh`"
                    .into(),
            );
        }
    }
    if debounce_ms.is_some() && !watch {
        return Err(
            "--debounce sets the quiet period of a watcher that is not running. Add --watch, or \
             drop --debounce"
                .into(),
        );
    }

    // A bare `map` reads the catalog off the server and `status` reads the
    // local journal; neither walks a filesystem, so an ignore flag on either
    // cannot change one byte of the output. Measured before this check
    // existed: `xerj autoindex map --no-ignore` was byte-identical to
    // `xerj autoindex map`. Accepting it was the #204 accept-and-ignore class
    // (#279). `map <folder>` is the exception: it walks, and the flag is
    // honoured on that preview the same way `--dry-run` honours it.
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
        if let Some((key, path)) = discover_local_admin_key_for_url(&url) {
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

    // --- object-storage watch routing ------------------------------------
    //
    // Three refusals, all of the same kind: a flag that is accepted and then
    // does nothing is the defect class this repo refuses on purpose (#204).
    //
    // FOUR routes share this one positional argument, and git cannot see the
    // difference between them, because they are different LINES of the same
    // function rather than the same lines:
    //
    //   <folder>                -> index a folder, once           (always)
    //   <folder> --watch        -> #967, live reindexing          (lib.rs)
    //   s3://…                  -> #970, one-shot object indexing (objsource)
    //   s3://… --watch          -> this branch, a bounded poll    (objwatch)
    //
    // Each of the last three arrived on its own branch, and each branch had a
    // refusal saying the other two did not exist. Those refusals auto-merge
    // CLEANLY — git reports no conflict — into a build that compiles and
    // silently kills a shipped feature. Both have now been removed, in the
    // merge that brought their branch in, and the surviving guards are written
    // so the route, not the absence of the others, is what they test:
    //
    //   * #967's `if watch { … }` block above is gated on
    //     `!root_is_object_url`. Its three refusals (`--dry-run`, `--fresh`,
    //     `--no-graph`) are the LOCAL watcher's constraints, and `s3://…
    //     --watch --dry-run` is the documented way to price a poll.
    //   * the object-storage watch block below is gated on
    //     `root_is_object_url`, so a local `--watch` falls through to #967's
    //     route instead of being swallowed.
    //
    // `all_four_routes_through_the_positional_argument_stay_open` in this
    // file's tests is the guard. It is one test on purpose: three separate
    // ones would each keep passing while a merge broke the other two.
    let used_watch_flags = || {
        let mut u = watch_flags_used.clone();
        u.sort_unstable();
        u.dedup();
        u.join(" ")
    };
    if !watch && !watch_flags_used.is_empty() {
        let used = used_watch_flags();
        return Err(format!(
            "{used} only apply to a watch. Add --watch, or drop them"
        ));
    }
    // These flags configure the object-storage poll (`WatchCfg`); the folder
    // watcher (#967) has no field for any of them, so accepting them on a
    // folder root was the accept-and-ignore class (#279): `<folder> --watch
    // --no-graph --once` committed a generation and then kept watching. A
    // subcommand, or a missing root, keeps the refusal its own route gives.
    if watch
        && !watch_flags_used.is_empty()
        && sub.is_none()
        && root_raw.is_some()
        && !root_is_object_url
    {
        let used = used_watch_flags();
        return Err(format!(
            "{used} configure an object-storage watch (`s3://…` or `r2://…` with --watch); a \
             folder --watch reindexes on change until it is stopped, and its only knob is \
             --debounce. Drop them"
        ));
    }
    if watch && root_is_object_url && debounce_ms.is_some() {
        return Err(
            "--debounce is the folder watcher's quiet period; an object-storage watch polls on \
             a schedule instead. Use --poll-interval"
                .into(),
        );
    }
    // `--watch` on a LOCAL root is #967's live-reindexing route, which landed
    // on main while this branch was open. This is where the branch used to
    // refuse it ("--watch is implemented for object-storage roots on this
    // build"); that refusal auto-merged cleanly into a build that compiled and
    // would have killed #967's whole feature, which is why the note above
    // exists. A local root now falls through to the ordinary index path with
    // `watch` set.
    //
    // A `--watch` with no root at all is still an error, and it is the
    // folder-indexing path that says so.
    // An object root WITHOUT --watch is #970's one-shot indexing route
    // (`crate::objsource`), which landed on main while this branch was open.
    // This is where the branch used to refuse it ("a separate change; until it
    // lands, sync the objects to a folder"); it has landed, so the refusal is
    // gone and the root falls through to the ordinary index path.
    // `&& root_is_object_url` is redundant today (the refusal above guarantees
    // it) and load-bearing the moment #967's local route lands: see the MERGE
    // NOTE.
    if watch && root_is_object_url {
        if sub.is_some() {
            return Err(format!(
                "--watch does not apply to `autoindex {}`",
                sub.as_deref().unwrap_or_default()
            ));
        }
        if watch_once && max_cycles.is_some() {
            return Err(
                "--once and --max-cycles contradict each other. Drop one of the two".into(),
            );
        }
        if no_fetch && append_only {
            // Both are legal, and together they are the cheapest possible
            // watch — but they are also the least informative, so say what the
            // combination means rather than letting it be discovered later.
            eprintln!(
                "autoindex: --no-fetch --append-only is the cheapest watch there is (one list \
                 call per cycle, no GETs) and the least complete: no deletes, no edits to older \
                 keys, and no content digests."
            );
        }
        if url_explicit {
            return Err(
                "--url does not apply to --watch on this build: the watch produces a change feed \
                 (--events-out, or stdout) and never writes to a node. Pipe the feed into the \
                 indexer instead"
                    .into(),
            );
        }
        if max_minutes_explicit
            || approve_explicit
            || max_index_explicit
            || budget_plan.is_some()
            || drop_indexed
        {
            return Err(
                "--max-minutes/--approve/--yes/--max-index-size/--plan/--drop-indexed are the \
                 folder-indexing gate; a watch has its own cost gate (--max-monthly-ops, \
                 --allow-cost)"
                    .into(),
            );
        }
        if label.is_some() {
            return Err(
                "--label labels documents at ingest time; an object-storage watch indexes nothing \
                 and never reads document bytes. Drop --label"
                    .into(),
            );
        }
        if dataset.is_some() {
            // #1231: same accept-and-ignore class — --dataset only selects which
            // dataset `autoindex map` prints and has no meaning for a watch.
            return Err(
                "--dataset selects a single dataset to show and is only supported with `autoindex \
                 map`; an object-storage watch produces a change feed and reads no catalog. Drop \
                 --dataset"
                    .into(),
            );
        }
        if !ignore_flags_used.is_empty() {
            return Err(format!(
                "{} apply to a folder walk, not to an object-storage watch",
                ignore_flags_used.join(" and ")
            ));
        }
        if progress_explicit {
            return Err(
                "--progress/--progress-interval apply to indexing; a watch reports one line per \
                 poll cycle (or one JSON object per cycle with --json)"
                    .into(),
            );
        }
        let raw = root_raw.clone().unwrap_or_default();
        return Ok(Cmd::Watch(Box::new(WatchCfg {
            url: raw,
            // One flag, one parser: `--endpoint-url` is validated once, in the
            // arm #970 added, and both the one-shot object source and the watch
            // read the same value.
            endpoint: endpoint_url.clone(),
            region: region
                .or_else(|| std::env::var("AWS_REGION").ok().filter(|r| !r.is_empty()))
                .unwrap_or_else(|| "auto".to_string()),
            poll_interval: poll_interval.unwrap_or_else(|| {
                Duration::from_secs(crate::objwatch::cost::DEFAULT_POLL_INTERVAL_SECS)
            }),
            // --dry-run is one cycle by definition: it exists to price the poll.
            max_cycles: if dry_run || watch_once {
                Some(1)
            } else {
                max_cycles
            },
            fetch: !no_fetch && !dry_run,
            max_object_bytes: max_object_mb * 1024 * 1024,
            append_only,
            max_monthly_ops: max_monthly_ops
                .unwrap_or(crate::objwatch::cost::DEFAULT_MAX_MONTHLY_CLASS_A),
            max_monthly_gets: max_monthly_gets
                .unwrap_or(crate::objwatch::cost::DEFAULT_MAX_MONTHLY_CLASS_B),
            allow_cost,
            page_size,
            state_dir,
            fresh,
            events_out,
            status_file,
            dry_run,
            json,
            quiet,
        })));
    }

    if remove_root.is_some() && sub.as_deref() != Some("remove") {
        return Err(
            "--root names the folder `autoindex remove` drops. It is only supported with \
             `autoindex remove --prefix <P> --root <DIR>`."
                .into(),
        );
    }

    match (sub.as_deref(), folder) {
        (Some("remove"), _) => remove_cmd(RemoveParse {
            cfg: RemoveCfg {
                url,
                api_key,
                prefix,
                root: remove_root,
                yes: yes_flag,
                json,
                quiet,
                xerj_url_note,
                state_dir: state_dir.clone(),
            },
            prefix_explicit,
            approve_set: approve.is_some(),
            markdown,
            reject: RemoveReject {
                dry_run,
                replace_prefix,
                fresh,
                workers: workers.is_some(),
                pdf_workers: pdf_workers.is_some(),
                pdf_timeout: pdf_timeout_secs != 120,
                bulk_mb: bulk_mb != 8,
                bulk_timeout: bulk_timeout_explicit,
                snapshot: snapshot_max_bytes != (64u64 << 30),
                follow_symlinks,
                follow_symlinks_outside_root,
                stubs: !stub_globs.is_empty(),
                ignore: !ignore_flags_used.is_empty(),
                max_file: max_file_bytes.is_some(),
                sample: sample != 500,
                no_semantic,
                code_analyzer: code_analyzer_explicit,
                brain: brain.is_some(),
                no_graph,
                watch,
                debounce: debounce_ms.is_some(),
                watch_flags: !watch_flags_used.is_empty(),
                label: label.is_some(),
                dataset: dataset.is_some(),
                paging: detail_offset.is_some() || detail_limit.is_some(),
                progress: progress_explicit,
                endpoint: endpoint_explicit,
                max_minutes: max_minutes_explicit,
                max_index: max_index_explicit,
                plan: budget_plan.is_some(),
                walk_only,
            },
        }),
        (Some("map"), None) | (Some("status"), _) if walk_only => Err(format!(
            "--walk-only summarises a folder walk and does not apply to `autoindex {}`: that \
             subcommand never walks a tree. Drop it",
            sub.as_deref().unwrap_or_default()
        )),
        (Some("map"), None) | (Some("status"), _)
            if max_minutes_explicit
                || approve_explicit
                || max_index_explicit
                || budget_plan.is_some()
                || drop_indexed =>
        {
            Err(format!(
                "--max-minutes/--approve/--yes/--max-index-size/--plan/--drop-indexed apply only \
                 to indexing, not `autoindex {}`",
                sub.as_deref().unwrap_or_default()
            ))
        }
        (Some("map"), None) | (Some("status"), _) if progress_explicit => Err(format!(
            "--progress/--progress-interval apply only to indexing, not `autoindex {}`",
            sub.as_deref().unwrap_or_default()
        )),
        (Some("map"), None) | (Some("status"), _) if !ignore_flags_used.is_empty() => {
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
        (Some("map"), None) | (Some("status"), _) if endpoint_explicit => Err(format!(
            "--endpoint-url applies only to indexing, not `autoindex {}`: that subcommand reads the \
             XERJ node named by --url and never talks to an object store",
            sub.as_deref().unwrap_or_default()
        )),
        (Some("map"), _) if bulk_timeout_explicit => {
            Err("--bulk-timeout-secs applies only to indexing, not `autoindex map`".into())
        }
        (Some("map"), None) | (Some("status"), _) if label.is_some() => Err(format!(
            "--label labels indexed documents through /_decide; `autoindex {}` reads the node \
             and never indexes anything. Drop --label",
            sub.as_deref().unwrap_or_default()
        )),
        (Some("map"), None) => Ok(Cmd::Map(MapCfg {
            url,
            api_key,
            prefix: prefix_explicit.then_some(prefix),
            json,
            dataset,
            detail_offset: detail_offset.unwrap_or(0),
            detail_limit,
            xerj_url_note,
        })),
        (_, _) if detail_offset.is_some() || detail_limit.is_some() => Err(format!(
            "--offset and --limit page the data map and are only supported with \
             `autoindex map`, not `autoindex {}`",
            sub.as_deref().unwrap_or("<root>")
        )),
        // #1231: --dataset only selects which dataset `autoindex map` prints;
        // it is NOT an indexing destination filter. Accepting it on the
        // indexing/status routes and then dropping it is the silent no-op this
        // repo refuses (#204, #279): the operator reads it as an explicit
        // destination restriction and gets an unrestricted run instead. Only
        // `map` reaches here with dataset still unconsumed.
        (_, _) if dataset.is_some() => Err(format!(
            "--dataset selects a single dataset to show and is only supported with \
             `autoindex map`, not `autoindex {}`: it does not restrict where indexing \
             writes. To inspect one dataset run `autoindex map --dataset <SLUG>`; to \
             index, drop --dataset",
            sub.as_deref().unwrap_or("<root>")
        )),
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
        (None, Some(root)) | (Some("map"), Some(root)) => {
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
            if walk_only && label.is_some() {
                return Err(
                    "--walk-only and --label contradict each other: a walk-only run indexes \
                     nothing, so there is nowhere for the decide labels to go. Drop one of the two"
                        .into(),
                );
            }
            if dry_run && label.is_some() {
                return Err(
                    "--dry-run and --label contradict each other: a dry run indexes nothing, so \
                     there is nowhere for the decide labels to go. Drop one of the two"
                        .into(),
                );
            }
            let plan = crate::resources::plan(workers, pdf_workers, bulk_mb);
            Ok(Cmd::Index(Box::new(IndexCfg {
                root,
                endpoint_url,
                url,
                api_key,
                api_key_file,
                workers: plan.index_workers,
                scan_workers: plan.scan_threads,
                pdf_workers: plan.pdf_workers,
                resource_notes: plan.notes,
                // #768: delivered by run_index via pr.warn (not the
                // --quiet-suppressible resource_notes/pr.note surface) and
                // mirrored into --json, so a wrong-node warning is never silently
                // dropped, yet stays a well-formed event on --progress json.
                xerj_url_note,
                pdf_timeout_secs,
                bulk_mb,
                bulk_timeout_secs,
                snapshot_max_bytes,
                prefix_note: prefix_given
                    .as_deref()
                    .and_then(|given| prefix_rewrite_note(given, &prefix)),
                prefix,
                replace_prefix,
                state_dir,
                fresh,
                follow_symlinks,
                follow_symlinks_outside_root,
                stub_globs,
                ignore: IgnoreOptions {
                    enabled: !no_ignore,
                    defaults: !no_ignore && !no_default_ignores,
                    // Only --dry-run and --walk-only (which implies --dry-run)
                    // pay for counting what is inside a pruned directory; a
                    // real run's whole point is not touching it.
                    deep_count: dry_run,
                },
                max_file_bytes: max_file_bytes.unwrap_or(2u64 << 30),
                sample: sample.max(50),
                no_semantic,
                code_analyzer,
                brain,
                no_graph,
                dry_run,
                walk_only,
                max_minutes,
                approve,
                max_index_bytes,
                budget_plan,
                drop_indexed,
                json,
                quiet,
                progress,
                progress_interval,
                label,
                watch,
                debounce: Duration::from_millis(
                    debounce_ms.unwrap_or(crate::watch::DEFAULT_DEBOUNCE_MS),
                ),
                exclude_rels: Vec::new(),
                skip_corpus_data: false,
            })))
        }
        // No folder and no subcommand. This used to print the full help and
        // exit 0, so a bare `xerj autoindex` looked like a successful run.
        // `--help` / `-h` still return [`Cmd::Help`] above.
        _ => Err(
            "a folder is required (`xerj autoindex <folder>`), or use \
             `xerj autoindex map`, `xerj autoindex status`, or \
             `xerj autoindex remove --prefix <P>`. \
             `xerj autoindex --help` prints the full usage"
                .into(),
        ),
    }
}

struct RemoveReject {
    dry_run: bool,
    replace_prefix: bool,
    fresh: bool,
    workers: bool,
    pdf_workers: bool,
    pdf_timeout: bool,
    bulk_mb: bool,
    bulk_timeout: bool,
    snapshot: bool,
    follow_symlinks: bool,
    follow_symlinks_outside_root: bool,
    stubs: bool,
    ignore: bool,
    max_file: bool,
    sample: bool,
    no_semantic: bool,
    code_analyzer: bool,
    brain: bool,
    no_graph: bool,
    watch: bool,
    debounce: bool,
    watch_flags: bool,
    label: bool,
    dataset: bool,
    paging: bool,
    progress: bool,
    endpoint: bool,
    max_minutes: bool,
    max_index: bool,
    plan: bool,
    walk_only: bool,
}

struct RemoveParse {
    cfg: RemoveCfg,
    prefix_explicit: bool,
    approve_set: bool,
    markdown: bool,
    reject: RemoveReject,
}

fn remove_cmd(input: RemoveParse) -> Result<Cmd, String> {
    let RemoveParse {
        cfg,
        prefix_explicit,
        approve_set,
        markdown,
        reject,
    } = input;
    if !prefix_explicit {
        return Err(
            "autoindex remove requires --prefix. The default name ax is shared across folders, \
             so this command will not guess it. Pass the prefix you indexed with."
                .into(),
        );
    }
    if approve_set && !cfg.yes {
        return Err(
            "--approve is the indexing decision gate. `autoindex remove` confirms with --yes, \
             after it has printed what it will delete."
                .into(),
        );
    }
    let flags = [
        ("--dry-run", reject.dry_run),
        ("--replace-prefix", reject.replace_prefix),
        ("--fresh", reject.fresh),
        ("--workers", reject.workers),
        ("--pdf-workers", reject.pdf_workers),
        ("--pdf-timeout-secs", reject.pdf_timeout),
        ("--bulk-mb", reject.bulk_mb),
        ("--bulk-timeout-secs", reject.bulk_timeout),
        ("--snapshot-max-gb", reject.snapshot),
        ("--follow-symlinks", reject.follow_symlinks),
        (
            "--follow-symlinks-outside-root",
            reject.follow_symlinks_outside_root,
        ),
        ("--stub", reject.stubs),
        ("--no-ignore", reject.ignore),
        ("--max-file-bytes", reject.max_file),
        ("--sample", reject.sample),
        ("--no-semantic", reject.no_semantic),
        ("--code-analyzer", reject.code_analyzer),
        ("--brain", reject.brain),
        ("--no-graph", reject.no_graph),
        ("--watch", reject.watch),
        ("--debounce", reject.debounce),
        ("--poll-interval", reject.watch_flags),
        ("--label", reject.label),
        ("--dataset", reject.dataset),
        ("--offset", reject.paging),
        ("--progress", reject.progress),
        ("--endpoint-url", reject.endpoint),
        ("--max-minutes", reject.max_minutes),
        ("--max-index-size", reject.max_index),
        ("--plan", reject.plan),
        ("--walk-only", reject.walk_only),
        ("--md", markdown),
    ];
    let bad: Vec<&str> = flags
        .iter()
        .filter(|(_, used)| *used)
        .map(|(n, _)| *n)
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "{} apply only to indexing, not `autoindex remove`. This command lists one folder's \
             indices and catalog rows and deletes those. It does not walk or index. Drop them",
            bad.join(", ")
        ));
    }
    Ok(Cmd::Remove(cfg))
}

#[cfg(test)]
mod tests {
    use super::{parse, Approval, Cmd, Duration, ProgressMode, DEFAULT_MAX_MINUTES};
    use std::path::PathBuf;

    #[test]
    fn a_bare_autoindex_is_a_usage_error() {
        let err = parse(Vec::new()).expect_err("bare autoindex must not succeed");
        assert!(err.contains("a folder is required"), "{err}");
        assert!(matches!(
            parse(vec!["--help".into()]).expect("help still parses"),
            Cmd::Help
        ));
        assert!(matches!(
            parse(vec!["-h".into()]).expect("-h still parses"),
            Cmd::Help
        ));
        let err = parse(vec!["--url".into(), "http://127.0.0.1:9200".into()])
            .expect_err("flags without a folder are still usage");
        assert!(err.contains("a folder is required"), "{err}");
    }

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

    fn watch(args: &[&str]) -> super::WatchCfg {
        match parse(args.iter().map(|s| s.to_string()).collect()).unwrap() {
            Cmd::Watch(cfg) => *cfg,
            other => panic!("expected watch config, got {other:?}"),
        }
    }

    /// The default an operator gets when they pass nothing but a bucket. It is
    /// the free-tier-safe one, and asserting it here means a later edit to the
    /// constant has to be deliberate.
    #[test]
    fn a_bare_watch_defaults_to_the_free_tier_safe_interval_and_budget() {
        let cfg = watch(&["s3://logs/2026/09/", "--watch"]);
        assert_eq!(cfg.url, "s3://logs/2026/09/");
        assert_eq!(
            cfg.poll_interval,
            Duration::from_secs(crate::objwatch::cost::DEFAULT_POLL_INTERVAL_SECS)
        );
        assert_eq!(
            cfg.max_monthly_ops,
            crate::objwatch::cost::DEFAULT_MAX_MONTHLY_CLASS_A
        );
        assert_eq!(cfg.max_cycles, None, "a bare watch runs until stopped");
        assert!(cfg.fetch, "digests are on by default");
        assert!(!cfg.append_only);
        assert!(!cfg.allow_cost);
        assert_eq!(cfg.page_size, crate::objwatch::cost::MAX_KEYS_PER_LIST);
        assert_eq!(cfg.region, "auto", "which is what R2 wants");
    }

    /// `r2://` has to reach the runner as something the S3 parser accepts, or it
    /// is a scheme that parses and then fails — the accepted-and-ignored shape.
    #[test]
    fn the_r2_scheme_routes_to_a_watch_like_the_s3_one() {
        assert_eq!(watch(&["r2://logs/", "--watch"]).url, "r2://logs/");
        assert_eq!(
            crate::objwatch::run::normalize_object_url(&watch(&["r2://logs/", "--watch"]).url),
            "s3://logs/"
        );
    }

    #[test]
    fn once_and_dry_run_are_one_cycle_and_dry_run_never_reads_bytes() {
        assert_eq!(watch(&["s3://b/", "--watch", "--once"]).max_cycles, Some(1));
        assert_eq!(
            watch(&["s3://b/", "--watch", "--max-cycles", "7"]).max_cycles,
            Some(7)
        );
        let dry = watch(&["s3://b/", "--watch", "--dry-run"]);
        assert_eq!(dry.max_cycles, Some(1), "pricing a poll is one cycle");
        assert!(!dry.fetch, "and it must not spend Class B operations");
        assert!(dry.dry_run);
    }

    /// Every one of these is a flag that would otherwise parse and do nothing.
    /// The message has to name the flag, because an operator who passed it
    /// believes it took effect.
    #[test]
    fn index_only_flags_are_refused_on_a_watch_rather_than_silently_ignored() {
        for (args, needle) in [
            (
                vec!["s3://b/", "--watch", "--url", "http://x:9200"],
                "--url",
            ),
            (
                vec!["s3://b/", "--watch", "--max-minutes", "5"],
                "--max-minutes",
            ),
            (vec!["s3://b/", "--watch", "--yes"], "--max-minutes"),
            (
                vec!["s3://b/", "--watch", "--progress", "plain"],
                "--progress",
            ),
            (vec!["s3://b/", "--watch", "--no-ignore"], "folder walk"),
        ] {
            let text = err(&args);
            assert!(
                text.contains(needle),
                "refusing {args:?} must name what does not apply ({needle}): {text}"
            );
        }
    }

    /// The mirror image: a watch-only flag without `--watch`.
    #[test]
    fn watch_only_flags_without_watch_are_refused() {
        let text = err(&["data", "--poll-interval", "60"]);
        assert!(
            text.contains("--poll-interval") && text.contains("--watch"),
            "{text}"
        );
        let text = err(&["data", "--append-only", "--no-fetch"]);
        assert!(
            text.contains("--append-only") && text.contains("--no-fetch"),
            "{text}"
        );
    }

    /// `--label` reaches the folder route as a path (#1062) — the flag an
    /// operator passes must be the flag the indexer reads.
    #[test]
    fn label_parses_on_the_index_route() {
        let cfg = index(&["data", "--label", "questions.json"]);
        assert_eq!(cfg.label, Some(std::path::PathBuf::from("questions.json")));
        assert!(index(&["data"]).label.is_none(), "absent by default");
    }

    /// Every route that cannot label refuses the flag BY NAME rather than
    /// accepting and ignoring it — the #204 shape. `--watch` (both kinds)
    /// never runs the decide votes a watcher would need; `map`/`status` never
    /// index; `--dry-run` indexes nothing, so there is nowhere for the labels
    /// to go. And the flag without a path is a parse error, not a silently
    /// ignored label.
    #[test]
    fn label_is_refused_on_every_route_that_cannot_apply_it() {
        let cases = [
            (
                vec!["data", "--watch", "--no-graph", "--label", "q.json"],
                "--watch and --label",
            ),
            (
                vec!["s3://b/", "--watch", "--label", "q.json"],
                "an object-storage watch indexes nothing",
            ),
            (vec!["map", "--label", "q.json"], "never indexes anything"),
            (
                vec!["map", "knowledge", "--label", "q.json"],
                "indexes nothing",
            ),
            (
                vec!["status", "--label", "q.json"],
                "never indexes anything",
            ),
            (vec!["data", "--label", "q.json", "--dry-run"], "contradict"),
            (vec!["data", "--label"], "needs a question-set file path"),
        ];
        for (args, needle) in cases {
            let text = err(&args);
            assert!(
                text.contains(needle),
                "refusing {args:?} must say why ({needle}): {text}"
            );
            assert!(
                text.contains("--label"),
                "refusing {args:?} must name the flag: {text}"
            );
        }
    }

    /// The help names the flag and the honesty rule that travels with it: raw,
    /// uncalibrated p (#1063).
    #[test]
    fn help_advertises_the_label_flag_and_its_raw_p() {
        let help = super::help_text();
        assert!(help.contains("--label <FILE>"), "{help}");
        assert!(help.contains("/_decide"), "{help}");
        assert!(help.contains("calibration is #1063"), "{help}");
    }

    /// **The merge guard.** Four routes share one positional argument, and each
    /// arrived on its own branch carrying a refusal that said the others did
    /// not exist:
    ///
    /// | argument | flag | route |
    /// |---|---|---|
    /// | `<folder>` | — | index a folder, once |
    /// | `<folder>` | `--watch` | #967, live reindexing |
    /// | `s3://…` | — | #970, one-shot object indexing |
    /// | `s3://…` | `--watch` | this branch, a bounded poll |
    ///
    /// Those refusals AUTO-MERGE CLEANLY — git reports no conflict on
    /// `cli.rs` — into a build that compiles and silently kills a shipped
    /// feature. Both were caught this way and removed.
    ///
    /// It is deliberately ONE test over all four. Four separate tests would
    /// each keep passing while a merge broke the other three, which is exactly
    /// the failure mode: nothing here is about a flag being parsed, and
    /// everything is about the four staying open at the same time.
    #[test]
    fn all_four_routes_through_the_positional_argument_stay_open() {
        // 1. A folder, indexed once. Still the default and still not a watch.
        let folder = index(&["data"]);
        assert!(!folder.watch, "a bare folder is not a watch");

        // 2. A folder with --watch: #967's live reindexing. The refusal this
        //    branch used to carry ("--watch is implemented for object-storage
        //    roots on this build") would fail here.
        let local_watch = index(&["data", "--watch", "--no-graph"]);
        assert!(
            local_watch.watch,
            "`xerj autoindex <folder> --watch --no-graph` is #967's route and must reach the \
             indexer"
        );
        assert_eq!(local_watch.root, std::path::PathBuf::from("data"));
        // #967's own constraints still bite on ITS route.
        assert!(err(&["data", "--watch"]).contains("--no-graph"));
        assert!(err(&["data", "--watch", "--no-graph", "--dry-run"]).contains("contradict"));

        // 3. `s3://…` without --watch: #970's one-shot object indexing. The
        //    refusal this branch used to carry ("an object-storage root is only
        //    supported with --watch on this build") would fail here.
        let one_shot = index(&["s3://logs/2026/", "--endpoint-url", "http://127.0.0.1:9000"]);
        assert_eq!(one_shot.root, std::path::PathBuf::from("s3://logs/2026/"));
        assert!(!one_shot.watch);
        assert_eq!(
            one_shot.endpoint_url.as_deref(),
            Some("http://127.0.0.1:9000")
        );

        // 4. `s3://…` with --watch: the bounded poll. Its own flags are NOT
        //    #967's: --dry-run is how the docs tell you to price a poll, and
        //    there is no graph route to turn off, so #967's block must not
        //    reach this route.
        let poll = watch(&[
            "s3://logs/2026/",
            "--watch",
            "--dry-run",
            "--endpoint-url",
            "http://127.0.0.1:9000",
        ]);
        assert!(poll.dry_run, "`s3://… --watch --dry-run` prices a poll");
        assert_eq!(poll.max_cycles, Some(1));
        assert_eq!(poll.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
        assert!(
            watch(&["s3://logs/", "--watch"]).endpoint.is_none(),
            "no --endpoint-url is the AWS_ENDPOINT_URL fallback, not an error"
        );

        // One flag, one parser: a bad endpoint is refused identically on the
        // one-shot route and on the watch.
        assert!(err(&["s3://logs/", "--endpoint-url", "ftp://x"]).contains("http://"));
        assert!(err(&["s3://logs/", "--watch", "--endpoint-url", "ftp://x"]).contains("http://"));
    }

    /// Each watch route takes only the flags it reads. The object poll's knobs
    /// have no field on the folder watcher, and `--debounce` has none on the
    /// poll, so either one on the wrong route is refused rather than accepted
    /// and ignored (#279). Before this, `data --watch --no-graph --once`
    /// indexed once and then kept watching until it was killed.
    #[test]
    fn watch_flags_are_refused_on_the_route_that_cannot_read_them() {
        for flags in [
            &["--once"][..],
            &["--max-cycles", "3"],
            &["--poll-interval", "60"],
            &["--events-out", "events.jsonl"],
            &["--status-file", "status.json"],
            &["--no-fetch"],
        ] {
            let mut args = vec!["data", "--watch", "--no-graph"];
            args.extend_from_slice(flags);
            let e = err(&args);
            assert!(e.contains(flags[0]), "{flags:?}: {e}");
            assert!(e.contains("object-storage watch"), "{flags:?}: {e}");
        }
        let e = err(&["s3://logs/", "--watch", "--debounce", "500"]);
        assert!(e.contains("--poll-interval"), "{e}");

        // The flags still reach the route that reads them.
        assert_eq!(
            watch(&["s3://logs/", "--watch", "--once"]).max_cycles,
            Some(1)
        );
        let local = index(&["data", "--watch", "--no-graph", "--debounce", "500"]);
        assert_eq!(local.debounce, std::time::Duration::from_millis(500));
        // And without --watch, the existing refusal still names --watch.
        assert!(err(&["data", "--once"]).contains("Add --watch"));
    }

    #[test]
    fn contradictory_and_out_of_range_watch_flags_are_refused() {
        assert!(err(&["s3://b/", "--watch", "--once", "--max-cycles", "3"]).contains("contradict"));
        // 0 seconds and more than a day are both outside the accepted range.
        assert!(err(&["s3://b/", "--watch", "--poll-interval", "0"]).contains("--poll-interval"));
        assert!(
            err(&["s3://b/", "--watch", "--poll-interval", "86401"]).contains("--poll-interval")
        );
        // A page size above the API maximum would misreport the billing unit.
        assert!(err(&["s3://b/", "--watch", "--page-size", "1001"]).contains("1000"));
        assert!(
            err(&["s3://b/", "--watch", "--max-monthly-ops", "0"]).contains("--max-monthly-ops")
        );
    }

    #[test]
    fn the_help_text_carries_the_cost_table_and_the_defaults_it_enforces() {
        let help = super::help_text_with(false);
        assert!(
            help.contains("CLASS A"),
            "the expensive class must be named"
        );
        assert!(
            help.contains("1,000,000"),
            "the free-tier allowance must be stated"
        );
        assert!(
            help.contains(&crate::objwatch::cost::DEFAULT_POLL_INTERVAL_SECS.to_string()),
            "the default interval in the help must come from the constant"
        );
        assert!(
            help.contains(&crate::objwatch::cost::DEFAULT_MAX_MONTHLY_CLASS_A.to_string()),
            "and so must the default budget"
        );
        assert!(help.contains("--append-only"));
        assert!(help.contains("exit 4") || help.contains("NEEDS A DECISION"));
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

    /// The `--watch` refusal must name the limitation it actually has.
    /// The watcher is the generated route. On the default graph path a deleted
    /// file is swept and an added file that fits a frozen dataset is indexed;
    /// a file that needs a new dataset still waits for `--fresh`. The message
    /// used to claim a content change was lost, which came from a measurement
    /// whose shell append had created a new file rather than modifying one.
    #[test]
    fn the_watch_refusal_names_additions_and_deletions_not_content_changes() {
        let text = err(&["data", "--watch"]);
        assert!(
            text.contains("--no-graph"),
            "the message must name the flag that makes it work: {text}"
        );
        assert!(
            text.contains("sweeps a deleted") && text.contains("fits a frozen dataset"),
            "the message must say what the graph path does with a deletion and an addition: {text}"
        );
        assert!(
            text.contains("stale-resume-plan"),
            "a new dataset still waits for --fresh: {text}"
        );
        assert!(
            !text.contains("ABORTS the run"),
            "a deletion is swept, not aborted: {text}"
        );
        assert!(
            !text.contains("a file whose content changed is reported"),
            "the corrected claim must not come back: {text}"
        );
        // With --no-graph it parses, and the watcher is on.
        assert!(index(&["data", "--watch", "--no-graph"]).watch);
    }

    /// #1845: `--prefix forbidden-lands-combat-` silently became
    /// `forbidden-lands-combat`, so a trailing separator meant to keep sibling
    /// projects apart did nothing and nobody was told. The rewrite is now
    /// carried as a note naming the prefix actually used and the wildcard
    /// collision it implies; a prefix used as typed carries none.
    #[test]
    fn a_rewritten_prefix_is_named_and_an_exact_one_is_not() {
        for given in ["forbidden-lands-combat-", "forbidden-lands-combat_"] {
            let cfg = index(&["data", "--prefix", given]);
            assert_eq!(cfg.prefix, "forbidden-lands-combat");
            let note = cfg
                .prefix_note
                .expect("a dropped trailing separator must be said");
            assert!(note.contains(&format!("{given:?}")), "{note}");
            assert!(note.contains("\"forbidden-lands-combat\""), "{note}");
            assert!(note.contains("trailing '-' or '_' is not kept"), "{note}");
            assert!(note.contains("forbidden-lands-combat-*"), "{note}");
        }
        let cfg = index(&["data", "--prefix", "My_Proj"]);
        assert_eq!(cfg.prefix, "my-proj");
        assert!(cfg.prefix_note.is_some_and(|n| n.contains("\"my-proj\"")));
        // Used exactly as typed, or not typed at all: nothing to say.
        assert!(index(&["data", "--prefix", "forbidden-lands-combat"])
            .prefix_note
            .is_none());
        assert!(index(&["data"]).prefix_note.is_none());
        assert!(super::help_text().contains("trailing '-' or '_' is dropped"));
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
    /// (delivered by `pr.warn`, which `--quiet` cannot silence, rather than
    /// folding it into the --quiet-suppressible resource_notes). An explicit
    /// --url means the env var was not ignored, so there is nothing to warn
    /// about — deterministic regardless of the environment the test runs in.
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
    fn bulk_help_documents_the_enforced_maximum() {
        for feedback in [false, true] {
            let help = super::help_text_with(feedback);
            let line = help
                .lines()
                .find(|line| line.starts_with("    --bulk-mb <N>"))
                .expect("--bulk-mb must be documented");
            assert!(
                line.contains(&format!("(default 8, max {})", super::MAX_BULK_MB)),
                "{line}"
            );
        }
        assert!(
            err(&["data", "--bulk-mb", &(super::MAX_BULK_MB + 1).to_string()])
                .contains("--bulk-mb")
        );
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

    /// #1233: the whole-file cap must be expressible below 1 GB. `--max-file-gb`
    /// stays the whole-GB flag (default 2 GB), and `--max-file-mb` /
    /// `--max-file-bytes` reach the sub-GB range a source watcher needs. All
    /// three normalise to the same `max_file_bytes`.
    #[test]
    fn max_file_cap_defaults_to_2gb_and_accepts_mb_and_bytes() {
        assert_eq!(index(&["data"]).max_file_bytes, 2u64 << 30);
        assert_eq!(
            index(&["data", "--max-file-gb", "5"]).max_file_bytes,
            5u64 << 30
        );
        // The exact case the reporter could not express: a 10 MB source budget.
        assert_eq!(
            index(&["data", "--max-file-mb", "10"]).max_file_bytes,
            10u64 << 20
        );
        assert_eq!(
            index(&["data", "--max-file-bytes", "4096"]).max_file_bytes,
            4096
        );
    }

    /// Malformed, negative and overflowing values are refused with an
    /// actionable usage error (#1233); the three size flags are mutually
    /// exclusive so a resumed generation can never disagree with itself.
    #[test]
    fn max_file_cap_rejects_bad_values_and_double_specification() {
        // Non-numeric and negative both fail the u64 parse with the actionable
        // message (negatives are the exit-2 the reporter saw, kept actionable).
        assert!(err(&["data", "--max-file-mb", "ten"])
            .contains("--max-file-mb needs a non-negative integer"));
        assert!(err(&["data", "--max-file-gb", "-1"])
            .contains("--max-file-gb needs a non-negative integer"));
        assert!(
            err(&["data", "--max-file-gb"]).contains("--max-file-gb needs a non-negative integer")
        );
        // Overflow of the unit multiply is caught, not wrapped.
        assert!(err(&["data", "--max-file-gb", "18446744073709551615"])
            .contains("--max-file-gb value is too large"));
        // Mutually exclusive: a second size flag is a hard error, not last-wins.
        assert!(err(&["data", "--max-file-gb", "1", "--max-file-mb", "10"])
            .contains("specify only one of --max-file-gb / --max-file-mb / --max-file-bytes"));
    }

    #[test]
    fn fresh_help_scopes_the_refusal_and_denies_being_cleanup() {
        let help = super::FRESH_HELP;
        assert!(help.contains("ignore an existing resume journal and restart"));
        assert!(help.contains("durable corpus generation"));
        assert!(help.contains("drops the documents previously published for that path"));
        assert!(help.contains("removes documents for a file that disappeared"));
    }

    #[test]
    fn resume_policy_help_distinguishes_generated_legacy_and_graph_state() {
        let help = super::RESUME_POLICY_HELP;
        for claim in [
            "generated --no-graph journals",
            "add, change, delete, rename, and no-op",
            "written before the generation format must be rebuilt",
            "graph-enabled journals reconcile added files that fit a frozen",
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

    /// #1198: `--code-analyzer` defaults to `standard`, takes `code`, and
    /// refuses anything else by name instead of silently falling back.
    #[test]
    fn code_analyzer_flag_parses_and_refuses_unknown_values() {
        assert_eq!(
            index(&["data"]).code_analyzer,
            crate::infer::CodeAnalyzer::Standard
        );
        assert_eq!(
            index(&["data", "--code-analyzer", "code"]).code_analyzer,
            crate::infer::CodeAnalyzer::Code
        );
        assert_eq!(
            index(&["data", "--code-analyzer", "standard"]).code_analyzer,
            crate::infer::CodeAnalyzer::Standard
        );
        for args in [
            vec!["data", "--code-analyzer"],
            vec!["data", "--code-analyzer", "stemmer"],
        ] {
            let rendered = args.join(" ");
            let err = parse(args.into_iter().map(str::to_string).collect())
                .expect_err(&format!("`{rendered}` must be refused"));
            assert!(err.contains("--code-analyzer"), "{err}");
        }
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
    fn remove_requires_a_prefix_and_confirms_with_yes() {
        let missing = err(&["remove"]);
        assert!(missing.contains("requires --prefix"), "{missing}");
        let cfg = match parse(
            ["remove", "--prefix", "proj", "--root", "/tmp/proj", "--yes"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        )
        .unwrap()
        {
            Cmd::Remove(cfg) => cfg,
            other => panic!("expected remove, got {other:?}"),
        };
        assert_eq!(cfg.prefix, "proj");
        assert_eq!(cfg.root.as_deref(), Some("/tmp/proj"));
        assert!(cfg.yes);
        let refused = err(&["remove", "--prefix", "proj", "--dry-run"]);
        assert!(refused.contains("--dry-run"), "{refused}");
        assert!(refused.contains("not `autoindex remove`"), "{refused}");
        let walk = err(&["remove", "--prefix", "proj", "--walk-only"]);
        assert!(walk.contains("--walk-only"), "{walk}");
        assert!(walk.contains("not `autoindex remove`"), "{walk}");
        let cfg = match parse(
            [
                "remove",
                "--prefix",
                "proj",
                "--state-dir",
                "/tmp/j",
                "--yes",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        )
        .unwrap()
        {
            Cmd::Remove(cfg) => cfg,
            other => panic!("expected remove, got {other:?}"),
        };
        assert_eq!(
            cfg.state_dir.as_deref(),
            Some(std::path::Path::new("/tmp/j"))
        );
        assert!(err(&["data", "--root", "/tmp/a"]).contains("only supported with"));
        assert!(err(&["remove", "--prefix", "proj", "--approve", "proceed"]).contains("--yes"));
        assert!(super::help_text().contains("autoindex remove"));
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

    /// `--walk-only` is the folder map that stops before hashing. `--dry-run`
    /// stays the plan preview: it still hashes. The two together are the
    /// walk-only run, because that is the stricter "index nothing".
    #[test]
    fn walk_only_is_accepted_and_dry_run_still_hashes() {
        let help = super::help_text();
        assert!(
            help.contains("--walk-only"),
            "the flag must be in `xerj autoindex --help`"
        );
        assert!(
            help.contains("Stops before hashing") && help.contains("no digest, no sniff"),
            "help must say the walk does not hash or sniff file contents: {help}"
        );
        assert!(
            help.contains("Ignore files are"),
            "help must not claim ignore files are unread; the walk has always read them"
        );
        assert!(
            help.contains("--dry-run itself still"),
            "help must keep --dry-run's hash-and-plan behaviour distinct"
        );

        let plain = index(&["data"]);
        assert!(!plain.dry_run && !plain.walk_only);
        assert!(!plain.ignore.deep_count);

        let dry = index(&["data", "--dry-run"]);
        assert!(dry.dry_run);
        assert!(!dry.walk_only, "--dry-run must not become walk-only");
        assert!(dry.ignore.deep_count);

        let walk = index(&["data", "--walk-only"]);
        assert!(walk.walk_only);
        assert!(
            walk.dry_run,
            "walk-only indexes nothing, so the dry-run guards have to be armed"
        );
        assert!(walk.ignore.deep_count);

        let both = index(&["data", "--dry-run", "--walk-only"]);
        assert!(both.dry_run && both.walk_only);

        assert!(err(&["data", "--watch", "--no-graph", "--walk-only"]).contains("--watch"));
        assert!(err(&["s3://b/", "--watch", "--walk-only"]).contains("--watch"));
        assert!(err(&["map", "--walk-only"]).contains("autoindex map"));
        assert!(err(&["status", "--walk-only"]).contains("autoindex status"));
        assert!(err(&["data", "--walk-only", "--max-minutes", "1"]).contains("--max-minutes"));
        assert!(err(&["data", "--walk-only", "--yes"]).contains("--approve"));
        assert!(err(&["data", "--walk-only", "--label", "q.json"]).contains("--label"));
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
            vec!["map", "--drop-indexed"],
        ] {
            let err = parse(args.into_iter().map(str::to_string).collect()).unwrap_err();
            assert!(err.contains("apply only to indexing"), "{err}");
        }
    }

    /// The graph route sweeps a deletion and indexes an addition that fits a
    /// frozen dataset. The help used to say a deletion aborts, and pointed at
    /// a help section that does not exist.
    #[test]
    fn the_watch_help_does_not_repeat_the_retracted_graph_claim() {
        let help = super::help_text();
        assert!(
            !help.contains("only that route reindexes"),
            "the --watch help repeats the retracted claim about the graph route"
        );
        assert!(
            !help.contains("ABORTS on a deletion"),
            "the --watch help still says a deletion aborts"
        );
        for expected in ["sweeps a deleted", "files-removed", "stale-resume-plan"] {
            assert!(
                help.contains(expected),
                "--watch help is missing {expected:?}"
            );
        }
        // It pointed at a help section that does not exist.
        assert!(
            !help.contains("See LIVE REINDEXING below"),
            "the help points at a LIVE REINDEXING section it does not have"
        );
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
            "--max-index-size",
            "--plan <NAME>",
            "--drop-indexed",
            "DISK BUDGET:",
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

    /// #2057: a digit-leading prefix is a usage error, before any walk. The
    /// budget flags parse here too, and an unknown plan is refused.
    #[test]
    fn a_digit_leading_prefix_and_an_unknown_plan_are_usage_errors() {
        let prefix_err = err(&["data", "--prefix", "2042"]);
        assert!(prefix_err.contains("p2042"), "{prefix_err}");
        assert!(prefix_err.contains("lowercase letter"), "{prefix_err}");
        let cfg = index(&["data", "--plan", "code-and-docs", "--max-index-size", "2MB"]);
        assert_eq!(cfg.budget_plan.as_deref(), Some("code-and-docs"));
        assert_eq!(cfg.max_index_bytes, Some(2 * 1024 * 1024));
        assert!(
            !cfg.drop_indexed,
            "--plan does not agree to delete an existing index"
        );
        let confirmed = index(&["data", "--plan", "code-and-docs", "--drop-indexed"]);
        assert!(confirmed.drop_indexed);
        assert!(!index(&["data", "--yes"]).drop_indexed);
        let plan_err = err(&["data", "--plan", "everything"]);
        assert!(plan_err.contains("not a plan"), "{plan_err}");
    }

    /// A `\` at the end of a help line used to eat the next line's indent, so
    /// `--limit`'s wrap started at column 0. The wrap stays under the
    /// description.
    #[test]
    fn help_keeps_the_indent_of_a_wrapped_flag_line() {
        let help = super::help_text_with(false);
        let lines: Vec<&str> = help.lines().collect();
        let limit = lines
            .iter()
            .position(|l| l.contains("--limit <N>"))
            .expect("--limit must be documented");
        let wrap = lines[limit + 1];
        assert!(
            wrap.starts_with(' '),
            "the --limit wrap lost its indent: {wrap:?}"
        );
        assert!(
            wrap.contains("default: all of them"),
            "expected the --limit description wrap, got {wrap:?}"
        );
        let flag_col = lines[limit].find("--limit").unwrap();
        let wrap_col = wrap.find("default:").unwrap();
        assert!(
            wrap_col > flag_col,
            "wrap column {wrap_col} should sit past the flag at {flag_col}"
        );
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

    #[test]
    fn replace_prefix_is_off_unless_asked_and_help_names_the_refusal() {
        assert!(!index(&["data", "--prefix", "ref"]).replace_prefix);
        assert!(index(&["data", "--prefix", "ref", "--replace-prefix"]).replace_prefix);
        let help = super::help_text();
        assert!(help.contains("--replace-prefix"), "{help}");
        assert!(
            help.contains("ds:<prefix>:<slug>"),
            "help must say how catalog ids collide: {help}"
        );
        assert!(
            help.contains("<prefix>-docs"),
            "help must say a shared prefix collapses per-repo provenance: {help}"
        );
    }

    /// #1586: a second positional folder used to fall through to
    /// `unknown argument: repoB`, which reads as if the first folder were
    /// refused and says nothing about how to index both. The refusal must
    /// name both folders and show the one-run-per-folder form with `--prefix`.
    #[test]
    fn a_second_folder_says_one_folder_per_run_and_shows_the_two_run_form() {
        let text = err(&["repoA", "repoB"]);
        assert!(
            text.contains("takes one folder per run"),
            "the refusal must say the constraint: {text}"
        );
        assert!(
            text.contains("`repoA`") && text.contains("`repoB`"),
            "the refusal must name both folders: {text}"
        );
        assert!(
            text.contains("--prefix"),
            "the refusal must show the one-run-per-folder form: {text}"
        );
        assert!(
            !text.contains("unknown argument"),
            "a folder is not an unknown argument: {text}"
        );
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

    use xerj_common::localauth::{discover_local_admin_key, discover_local_admin_key_for_url};

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
        assert_eq!(discover_local_admin_key(), None, "nothing to find");

        // Weakest candidate first, then override it one rung at a time.
        sandbox.write("home/.xerj/admin.key", "home-key\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("home-key")
        );

        sandbox.write("home/xerj-data/admin.key", "llms-home-key\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("llms-home-key"),
            "~/xerj-data is the data dir llms.txt tells an agent to start"
        );

        sandbox.write("home/.xerj/brain/admin.key", "  brain-key \t\r\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("brain-key"),
            "the brain data dir outranks the bare ~/.xerj key, and is trimmed"
        );

        sandbox.write("xerj-data/admin.key", "xerj-data-key\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("xerj-data-key"),
            "a data dir in the working directory outranks anything in $HOME"
        );

        sandbox.write(".xerj-data/admin.key", "dot-data-key\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("dot-data-key"),
            "./.xerj-data is the directory the search/def start hint names"
        );

        sandbox.write("data/admin.key", "\ndata-key\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
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
            discover_local_admin_key(),
            None,
            "whitespace-only and zero-byte files are not credentials"
        );

        sandbox.write("home/.xerj/admin.key", "real-key\n");
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("real-key"),
            "an empty candidate must be skipped over, not stop the search"
        );
    }

    /// The server writes `~/.xerj/by-port/<port>` with the path of that
    /// listener's admin.key. That pointer outranks a leftover `./data` key,
    /// and a different port does not read it.
    #[test]
    fn a_port_pointer_names_the_key_for_that_listener() {
        let sandbox = Sandbox::new();
        sandbox.write("data/admin.key", "cwd-key\n");
        sandbox.write("elsewhere/admin.key", "port-key\n");
        let abs = sandbox.dir.path().join("elsewhere/admin.key");
        xerj_common::localauth::publish_admin_key_for_port(9200, &abs).unwrap();
        let (key, _) = discover_local_admin_key_for_url("http://localhost:9200").unwrap();
        assert_eq!(key, "port-key");
        let (key, _) = discover_local_admin_key_for_url("http://127.0.0.1:9201").unwrap();
        assert_eq!(
            key, "cwd-key",
            "another port must not reuse the 9200 pointer"
        );
        assert_eq!(
            discover_local_admin_key().map(|(k, _)| k).as_deref(),
            Some("cwd-key"),
            "discovery without a URL does not guess a port"
        );
        assert_eq!(
            xerj_common::localauth::xerj_url_port("localhost:9207"),
            9207
        );
        assert_eq!(
            xerj_common::localauth::xerj_url_port("http://localhost"),
            9200
        );
    }

    #[test]
    fn an_auth_rejection_is_not_reported_as_an_unreachable_node() {
        let auth = anyhow::anyhow!(
            "the server at http://localhost:9200 requires authentication and no API key was supplied (HTTP 401)."
        );
        assert!(crate::esclient::Es::ping_failure_is_auth(&auth));
        let down = anyhow::anyhow!("endpoint unreachable: http://localhost:9200");
        assert!(!crate::esclient::Es::ping_failure_is_auth(&down));
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

    /// #1231: `--dataset` only picks which dataset `autoindex map` prints. On
    /// the indexing and status routes it used to parse and then be dropped, so
    /// `autoindex <root> ... --dataset repository-docs` read like an explicit
    /// destination restriction and silently ran an unrestricted pass instead.
    /// It must be refused there, naming the flag and the only command it works
    /// with — and existence of the slug cannot matter, or a nonexistent one
    /// would still start indexing.
    #[test]
    fn dataset_is_refused_on_indexing_and_status_but_kept_for_map() {
        // The report's two invocations: the second only differs by --dataset,
        // and an existing slug and a made-up one must both be refused the same
        // way, since neither may start an indexing pass.
        for slug in ["repository-docs", "no-such-dataset-xyz"] {
            let text = err(&[
                "/path/to/root",
                "--no-graph",
                "--no-semantic",
                "--yes",
                "--prefix",
                "example",
                "--dataset",
                slug,
            ]);
            assert!(
                text.contains("--dataset") && text.contains("autoindex map"),
                "the refusal must name the flag and the command it belongs to: {text}"
            );
            assert!(
                text.contains("does not restrict where indexing writes"),
                "the message must correct the destination-restriction misread: {text}"
            );
        }

        // Status reads a node and never indexes, so --dataset is equally inapt.
        let text = err(&["status", "--dataset", "repository-docs"]);
        assert!(
            text.contains("--dataset") && text.contains("autoindex status"),
            "status must refuse --dataset and name itself: {text}"
        );

        // The one route it is valid on still carries the selection through.
        match parse(
            ["map", "--dataset", "repository-docs"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .expect("map --dataset must parse")
        {
            Cmd::Map(cfg) => assert_eq!(cfg.dataset.as_deref(), Some("repository-docs")),
            other => panic!("expected a map config, got {other:?}"),
        }

        // And a bare --dataset with no slug is a usage error, not a silent all.
        let text = err(&["map", "--dataset"]);
        assert!(
            text.contains("--dataset needs a dataset slug"),
            "a valueless --dataset must be refused: {text}"
        );
    }

    #[test]
    fn map_records_explicit_prefix_and_leaves_unscoped_map_none() {
        match parse(["map"].iter().map(|s| s.to_string()).collect()).expect("bare map must parse") {
            Cmd::Map(cfg) => assert_eq!(cfg.prefix, None),
            other => panic!("expected a map config, got {other:?}"),
        }

        match parse(
            ["map", "--prefix", "xc-tantivy"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .expect("map --prefix must parse")
        {
            Cmd::Map(cfg) => assert_eq!(cfg.prefix.as_deref(), Some("xc-tantivy")),
            other => panic!("expected a map config, got {other:?}"),
        }
    }

    /// #2368: `map` described itself as printing the discovered data map and
    /// then rejected `map <folder>` as an unknown argument. The folder form
    /// is the `--dry-run` preview. A bare `map` still reads the catalog.
    #[test]
    fn map_with_a_folder_is_the_dry_run_preview_and_bare_map_stays_the_catalog() {
        let help = super::help_text();
        assert!(
            help.contains("xerj autoindex map <folder>"),
            "help must show the folder form: {help}"
        );
        assert!(
            help.contains("what is already indexed"),
            "help must say a bare map reads indexed data: {help}"
        );
        assert!(
            help.contains("`autoindex map <folder>` is this same preview"),
            "help must point the preview at --dry-run: {help}"
        );
        assert!(
            !help.contains("print the discovered data map"),
            "the old usage line is what sent agents to pass a folder and get `unknown argument`"
        );

        match parse(["map"].iter().map(|s| s.to_string()).collect()).expect("bare map") {
            Cmd::Map(cfg) => {
                assert_eq!(cfg.prefix, None);
                assert_eq!(cfg.dataset, None);
            }
            other => panic!("a bare map must stay a catalog read, got {other:?}"),
        }

        let preview = index(&["map", "knowledge"]);
        let dry = index(&["knowledge", "--dry-run"]);
        assert_eq!(
            format!("{preview:?}"),
            format!("{dry:?}"),
            "map <folder> must be the same command as <folder> --dry-run"
        );
        assert_eq!(preview.root, PathBuf::from("knowledge"));
        assert!(preview.dry_run);
        assert!(!preview.walk_only);
        assert!(preview.ignore.deep_count);

        let flagged = index(&[
            "map",
            "--prefix",
            "notes",
            "--no-ignore",
            "--no-semantic",
            "--progress",
            "plain",
            "knowledge",
        ]);
        assert_eq!(flagged.root, PathBuf::from("knowledge"));
        assert_eq!(flagged.prefix, "notes");
        assert!(!flagged.ignore.enabled);
        assert!(flagged.no_semantic);
        assert_eq!(flagged.progress, ProgressMode::Plain);
        assert!(flagged.dry_run);

        let explicit = index(&["map", "knowledge", "--dry-run", "--prefix", "notes"]);
        assert!(explicit.dry_run);
        assert_eq!(explicit.prefix, "notes");
        assert_eq!(explicit.root, PathBuf::from("knowledge"));

        // A folder before `map` is still two positionals: `map` is only the
        // subcommand when it comes first, so `xerj autoindex ./map` indexes
        // a directory of that name.
        let reversed = err(&["knowledge", "map"]);
        assert!(reversed.contains("takes one folder per run"), "{reversed}");

        let two = err(&["map", "knowledge", "other"]);
        assert!(
            two.contains("at most one folder")
                && two.contains("knowledge")
                && two.contains("other"),
            "{two}"
        );
        assert!(!two.contains("unknown argument"), "{two}");

        let dataset = err(&["map", "knowledge", "--dataset", "docs"]);
        assert!(
            dataset.contains("--dataset") && dataset.contains("no folder"),
            "{dataset}"
        );
        let paging = err(&["map", "knowledge", "--limit", "2"]);
        assert!(
            paging.contains("--limit") && paging.contains("no folder"),
            "{paging}"
        );

        // Catalog selectors still belong to the no-folder form.
        match parse(
            ["map", "--dataset", "docs"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .expect("catalog map")
        {
            Cmd::Map(cfg) => assert_eq!(cfg.dataset.as_deref(), Some("docs")),
            other => panic!("expected a catalog map, got {other:?}"),
        }

        // Flags that change the walk are honoured on the preview and still
        // refused when there is no folder to walk.
        assert!(err(&["map", "--no-ignore"]).contains("never walks a folder"));
        assert!(err(&["map", "--max-minutes", "5"]).contains("apply only to indexing"));
        assert!(err(&["map", "knowledge", "--bulk-timeout-secs", "900"]).contains("autoindex map"));
        assert!(err(&["map", "knowledge", "--watch"]).contains("Drop --watch"));
        assert!(err(&["map", "knowledge", "--approve", "cancel"]).contains("nothing to cancel"));

        let both = err(&["map", "knowledge", "--no-ignore", "--no-default-ignores"]);
        assert!(
            both.contains("already turns off"),
            "the preview walks, so the redundant ignore pair is the indexing error: {both}"
        );
    }
}
