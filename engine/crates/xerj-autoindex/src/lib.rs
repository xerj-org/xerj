//! `xerj autoindex` — point it at ANY folder and it makes the contents
//! AI-searchable with ZERO configuration. Pure ES-compat HTTP client feature:
//! it does NOT link xerj-engine, works against any endpoint, and cannot
//! destabilize the server.

pub mod catalog;
pub mod cli;
pub mod coerce;
mod content;
pub mod correlate;
pub mod dataset;
pub mod detect;
pub mod esclient;
pub mod estimate;
pub mod extract;
pub mod feedback;
pub mod gate;
mod generation_catalog;
#[cfg(test)]
mod generation_catalog_http_tests;
pub mod ids;
pub mod ignore_rules;
pub mod infer;
pub mod order;
pub mod pool;
pub mod progress;
mod reconcile_plan;
pub mod resources;
pub mod sniff;
pub mod state;
mod sync;
mod sync_executor;
pub mod walk;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use cli::{Cmd, IndexCfg, MapCfg, StatusCfg};
// Trait must be in scope for `href_raw.counters()` (a concrete `Href`, not a
// `Box<dyn EdgeDetector>` like the registry entries).
use detect::EdgeDetector as _;
use esclient::Es;
use progress::Progress;
use sniff::{Family, Sniffed};
use state::{DuplicateFile, FileAssignment, FileDone, JunkFile, Plan, PlanDataset};

const PREPARED_RECORDS_IDENTITY: &str = "prepared-records-v1";
const DOCUMENT_IDS_IDENTITY: &str = "document-ids-v1";
const DETECTOR_DISABLED_IDENTITY: &str = "disabled";

/// Render the `next:` guidance printed after a successful index run.
///
/// The run's own credential and URL are carried into the printed commands.
/// They used to be printed bare, so against an auth-enabled server — which is
/// the default, and what `xerj brain` boots — a *successful* run signed off by
/// handing the user two commands that both answer 401
/// (`ONBOARDING-401-REPRO.md` §3). Guidance that cannot be pasted is worse
/// than no guidance: it reads as a broken server.
///
/// `api_key` is the credential this run used (`None` means the server needs
/// none — `--insecure` or auth off — and the bare commands really do work).
/// `env_key` is the ambient `XERJ_API_KEY`; when it already holds the run's
/// key the hints reference `$XERJ_API_KEY` rather than the literal secret, so
/// the command still pastes into the same shell while the admin key stays out
/// of a banner users routinely paste into bug reports.
///
/// When the key came from neither the environment nor the user's own command
/// line — i.e. it was discovered on disk — the hint must still not echo it.
/// A blind onboarding run caught exactly that: `autoindex` found
/// `./data/admin.key` by itself and then printed the admin key verbatim in a
/// banner. The reader never typed that secret, so seeing it in copyable output
/// is a disclosure, not a convenience.
pub fn next_hint(
    url: &str,
    prefix: &str,
    api_key: Option<&str>,
    env_key: Option<&str>,
    key_file: Option<&std::path::Path>,
) -> String {
    let Some(key) = api_key else {
        return format!(
            "\nnext: `xerj autoindex map --url {url}` for the data map; \
             search via `curl '{url}/{prefix}-*/_search'`"
        );
    };
    // A shell *expression* in every arm, so all renderings quote identically.
    let key_expr = if env_key == Some(key) {
        "$XERJ_API_KEY".to_string()
    } else if let Some(path) = key_file {
        // Reads the same secret at paste time without printing it here.
        format!("$(cat {})", path.display())
    } else {
        key.to_string()
    };
    format!(
        "\nnext: `xerj autoindex map --url {url} --api-key \"{key_expr}\"` for the data map;\n\
         \x20     search via `curl -H \"Authorization: ApiKey {key_expr}\" \
         '{url}/{prefix}-*/_search'`"
    )
}

fn prepared_records_identity(cfg: &IndexCfg) -> Result<String> {
    let value = json!({
        "contract": PREPARED_RECORDS_IDENTITY,
        "sample": cfg.sample,
        "max_file_gb": cfg.max_file_gb,
        "no_semantic": cfg.no_semantic,
        // Worker counts are operational only. The timeout can change whether
        // a PDF yields records, so it is part of the semantic contract.
        "pdf_timeout_secs": cfg.pdf_timeout_secs,
    });
    Ok(format!(
        "{}-{:032x}",
        PREPARED_RECORDS_IDENTITY,
        xxhash_rust::xxh3::xxh3_128(&serde_json::to_vec(&value)?)
    ))
}

fn preparation_contract_digest(cfg: &IndexCfg, plan: &Plan) -> Result<String> {
    let (schema_identity, index_identity) = generation_contract_identities(plan)?;
    let encoded = serde_json::to_vec(&json!({
        "prepared_records": prepared_records_identity(cfg)?,
        "document_ids": DOCUMENT_IDS_IDENTITY,
        "schema_identity": schema_identity,
        "index_identity": index_identity,
        "plan": plan,
    }))?;
    Ok(format!(
        "axpc1-{:032x}",
        xxhash_rust::xxh3::xxh3_128(&encoded)
    ))
}

pub(crate) fn generation_contract_identities(plan: &Plan) -> Result<(String, String)> {
    let mut datasets = plan.datasets.iter().collect::<Vec<_>>();
    datasets.sort_by(|left, right| {
        left.slug
            .cmp(&right.slug)
            .then_with(|| left.index.cmp(&right.index))
    });
    let schema = datasets
        .iter()
        .map(|dataset| {
            json!({
                "slug": dataset.slug,
                "family": dataset.family,
                "group": dataset.group,
                "specs": dataset.specs,
                "time_field": dataset.time_field,
                "semantic_field": dataset.semantic_field,
            })
        })
        .collect::<Vec<_>>();
    let indices = datasets
        .iter()
        .map(|dataset| {
            json!({
                "index": dataset.index,
                "mapping": build_mapping(&dataset.specs),
            })
        })
        .collect::<Vec<_>>();
    let digest = |label: &str, value: Value| -> Result<String> {
        let bytes = serde_json::to_vec(&json!({"contract": label, "value": value}))?;
        Ok(format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes)))
    };
    Ok((
        digest(PREPARED_RECORDS_IDENTITY, Value::Array(schema))?,
        digest(
            DOCUMENT_IDS_IDENTITY,
            json!({
                "datasets": indices,
                "catalog_index": catalog::CATALOG_INDEX,
                "catalog_mapping": catalog::catalog_mapping(),
                "document_ids": DOCUMENT_IDS_IDENTITY,
            }),
        )?,
    ))
}

pub(crate) fn ensure_generation_mappings(es: &Es, plan: &Plan) -> Result<()> {
    for dataset in &plan.datasets {
        let mut create_body = build_mapping(&dataset.specs);
        create_body["mappings"]["properties"]["ax_paths"] = json!({"type": "keyword"});
        let update_body = json!({
            "properties": create_body["mappings"]["properties"].clone()
        });
        es.ensure_index(&dataset.index, &create_body)
            .with_context(|| format!("create generation index {}", dataset.index))?;
        es.update_mapping(&dataset.index, &update_body)
            .with_context(|| format!("install generation mapping for {}", dataset.index))?;
    }
    let mut catalog_create_body = catalog::catalog_mapping();
    catalog_create_body["mappings"]["properties"]["duplicate_of"] = json!({"type": "keyword"});
    let catalog_update_body = json!({
        "properties": catalog_create_body["mappings"]["properties"].clone()
    });
    es.ensure_index(catalog::CATALOG_INDEX, &catalog_create_body)?;
    es.update_mapping(catalog::CATALOG_INDEX, &catalog_update_body)
        .context("install generation catalog mapping")
}

/// Project the current inventory onto a committed plan.
///
/// Pure by design and deliberately shared: `--dry-run` prints exactly the plan
/// the real reconcile would act on, because it is produced by this same call
/// rather than by a parallel implementation that could drift away from it.
///
/// The scan itself obeys the same two policies the legacy phase A does. It runs
/// inside the run's own pool, because `--workers` has to bound the CPU-bound
/// phase for the knob to mean anything (#240 §2), and it opens a per-file
/// progress guard, because a file this route touches must not drop out of the
/// progress denominator (#241). Run-local PDF artifact reuse is deliberately
/// disabled here: the generated route publishes from its own sealed snapshot
/// and never reaches the legacy phase B, so a retained artifact could never be
/// replayed (#248).
fn project_reconcile_plan(
    inventory: &content::Inventory,
    base_plan: &Plan,
    cfg: &IndexCfg,
    state_dir: &Path,
    pr: &Progress,
    meter: &estimate::Meter,
) -> Result<Plan> {
    let budget = extract::pdf::ExtractionSpoolBudget::new(0, 0);
    let ctx = PhaseAContext {
        state_dir,
        budget: &budget,
        capacity_warning: None,
        progress: pr,
        meter,
    };
    pr.phase(
        "scan",
        inventory.files.len() as u64,
        inventory.files.iter().map(|file| file.size).sum(),
    );
    let stub_matcher = StubMatcher::compile(&cfg.stub_globs).expect("validated at startup");
    let scans: Vec<FileScan> = crate::pool::install(|| {
        use rayon::prelude::*;
        inventory
            .files
            .par_iter()
            .zip(inventory.digests.par_iter())
            .map(|(file, digest)| {
                let _in_flight = pr.file(&file.rel, file.size);
                scan_file(
                    &file.path,
                    file.size,
                    digest,
                    &ctx,
                    cfg.sample,
                    cfg.max_file_gb,
                    stub_matcher.matches(&file.rel),
                )
            })
            .collect()
    });
    reconcile_plan::reconcile_plan(inventory, base_plan, scans, cfg.sample)
}

/// Code/AST coverage for one corpus: how much of what a person would call
/// "the source code" actually reached the index.
///
/// This exists because success looked identical to total loss. #294 junked
/// EVERY source file on the durable `--no-graph` path, and the run still
/// printed
///
/// ```text
/// xerj-done ok=true exit=3 reason=completed-with-junk wall=0.6s files=4 records=1 generation=1
/// ```
///
/// over a corpus of three source files and one `.md` — the same line a healthy
/// one-prose-file corpus prints. `records` counts records, not families, so
/// nothing on that line could distinguish "small corpus" from "the entire code
/// half is gone". These three counters make that state unrepresentable: a run
/// whose `code_files` is non-zero while `code_files_indexed` is zero can never
/// again print the same terminal line as a healthy one, on either path.
///
/// Counted per FILE, not per record, and derived from the same durable
/// artifacts the catalog is projected from (the plan's family strings plus the
/// per-file record counts), so a resumed run reports the corpus it holds rather
/// than the slice this invocation happened to touch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodeCoverage {
    /// Source files classified into `Family::Code`, junked ones included.
    pub files: u64,
    /// …of those, the ones that produced at least one indexed record.
    pub indexed: u64,
    /// …of those, the ones that produced none.
    pub junked: u64,
}

impl CodeCoverage {
    /// Both call sites hold a catalog FORMAT string, which is the plan's
    /// family (`Family::as_str`) plus the `(gzip)` suffix `format_str` and
    /// `assignment_format` append — match the family, not the compression.
    fn is_code_format(format: &str) -> bool {
        format.strip_suffix("(gzip)").unwrap_or(format) == Family::Code.as_str()
    }

    /// Count one file the run classified as `format` and which produced
    /// `records` indexed records. Non-code files are ignored, so both paths can
    /// feed this their whole file list without pre-filtering.
    pub fn observe(&mut self, format: &str, records: u64) {
        if !Self::is_code_format(format) {
            return;
        }
        self.files += 1;
        if records > 0 {
            self.indexed += 1;
        } else {
            self.junked += 1;
        }
    }

    /// The terminal-line and run-document fields, in a fixed order. One
    /// definition, so the legacy and generated paths cannot drift into
    /// spelling the same measurement differently.
    pub fn fields(&self) -> [(&'static str, u64); 3] {
        [
            ("code_files", self.files),
            ("code_files_indexed", self.indexed),
            ("code_files_junked", self.junked),
        ]
    }

    /// The one state that is always a defect: source code was found, and none
    /// of it reached the index. Cheap to compute and worth a sentence, because
    /// the counters alone still need a reader who knows to look at them.
    pub fn warning(&self) -> Option<String> {
        (self.files > 0 && self.indexed == 0).then(|| {
            format!(
                "warning: {} source-code file(s) were detected and NONE produced an indexed \
                 record — code search over this corpus will return nothing. Check the catalog's \
                 file documents (doc_kind=file, status=indexed, records=0) before trusting \
                 this index.",
                self.files
            )
        })
    }
}

/// Exit code for a run that committed (or confirmed) a corpus generation.
///
/// `3` is "completed with junk — recorded, never fatal", the same contract the
/// legacy path publishes (`cli.rs` EXIT CODES, and CLAUDE.md's own indexing
/// workflow treats 3 as success). The generated path returned a flat `0`, which
/// silently downgraded that signal for `--no-graph`. Both inputs come from the
/// committed generation's own run document, so a no-op re-run over a corpus
/// that still contains junk reports 3 again instead of flapping to 0.
fn generated_exit_code(summary: &Value) -> i32 {
    let count = |field: &str| summary.get(field).and_then(Value::as_u64).unwrap_or(0);
    if count("junk_records_total") > 0 || count("files_junk") > 0 {
        3
    } else {
        0
    }
}

/// Terminal progress line for a run that ends on the generated `--no-graph`
/// path.
///
/// Every exit closes the stream itself (#241). `Ticker::drop` reports
/// `ok=false` for a run that never called `finish`, so a generated run that
/// merely returned would print an aborted line straight after a successful
/// commit — the one place the two landed changes could contradict each other.
/// Exit 3 is success ("completed, some input was unusable"), so it is spelled
/// out in words rather than left as a bare number.
fn finish_generated_progress(pr: &Progress, code: i32, summary: &Value) {
    let count = |field: &str| summary.get(field).and_then(Value::as_u64).unwrap_or(0);
    // Read back from the committed generation's own run document, exactly like
    // every other number on this line — a resumed or no-op run therefore
    // reports the corpus's coverage, not an empty one.
    let coverage = CodeCoverage {
        files: count("code_files"),
        indexed: count("code_files_indexed"),
        junked: count("code_files_junked"),
    };
    // Before `finish`: the terminal line is the last thing this stream emits.
    if let Some(warning) = coverage.warning() {
        pr.note(&warning);
    }
    let mut extra = vec![
        ("files", count("files_indexed")),
        ("records", count("records_total")),
        ("generation", count("generation")),
    ];
    extra.extend(coverage.fields());
    pr.finish(
        true,
        code,
        if code == 3 {
            "completed-with-junk"
        } else {
            "completed"
        },
        &extra,
    );
}

fn begin_non_graph_generation(
    es: &Es,
    journal: &mut state::Journal,
    state_dir: &Path,
    cfg: &IndexCfg,
    root_identity: &str,
    inventory: &content::Inventory,
    plan: Plan,
) -> Result<()> {
    anyhow::ensure!(
        cfg.no_graph,
        "non-graph generation cutover requires --no-graph"
    );
    anyhow::ensure!(
        journal.pending_sync.is_none(),
        "cannot prepare over an existing pending generation"
    );
    if journal.committed_manifest.is_none() {
        journal.sync_bootstrap_genesis()?;
    }
    let base = journal
        .committed_manifest
        .as_ref()
        .context("generation cutover has no committed base")?
        .clone();
    let tx_id = format!("{}-g{}", journal.run_id, base.generation + 1);
    let preparation_contract = preparation_contract_digest(cfg, &plan)?;
    let snapshot = sync_executor::create_prepared_snapshot(
        state_dir,
        &tx_id,
        inventory,
        &plan,
        &preparation_contract,
        cfg.snapshot_max_bytes,
    )?;
    let chunker_identity = prepared_records_identity(cfg)?;
    let semantic = plan
        .datasets
        .iter()
        .any(|dataset| dataset.semantic_field.is_some());
    let identity = if semantic {
        let identity = es
            .embedding_execution_identity()
            .context("generation cutover could not pin the server embedding execution identity")?;
        anyhow::ensure!(
            identity.resumable,
            "generation cutover requires a resumable embedding execution identity: {}",
            identity
                .non_resumable_reason
                .as_deref()
                .unwrap_or("the server did not provide an immutable identity")
        );
        journal.pin_embedding_identity(
            &identity.identity_sha256,
            identity.resumable,
            identity.non_resumable_reason.as_deref(),
        )?;
        identity
    } else {
        crate::esclient::EmbeddingExecutionIdentity {
            version: 1,
            backend: "disabled".into(),
            identity_sha256: "0".repeat(64),
            // A disabled embedding execution has no vector width at all. `None`
            // says exactly that; any number here would be a fiction the
            // generation contract would then hold future runs to.
            dimensions: None,
            semantic_contract: "disabled-no-semantic-fields-v1".into(),
            resumable: true,
            non_resumable_reason: None,
        }
    };

    let aliases_by_content = plan.duplicate_files.iter().fold(
        HashMap::<&str, Vec<sync::ManifestPath>>::new(),
        |mut aliases, alias| {
            aliases
                .entry(alias.file_key.as_str())
                .or_default()
                .push(sync::ManifestPath {
                    path_id: alias.path_id.clone(),
                    rel: alias.rel.clone(),
                    is_symlink: alias.is_symlink.unwrap_or(false),
                });
            aliases
        },
    );
    let file_by_content = inventory
        .keys
        .iter()
        .zip(&inventory.files)
        .zip(&inventory.digests)
        .map(|((key, file), digest)| (key.as_str(), (file, digest)))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::with_capacity(plan.files.len());
    for (content_id, assignment) in &plan.files {
        let (file, digest) = file_by_content
            .get(content_id.as_str())
            .with_context(|| format!("planned content {content_id} is absent from inventory"))?;
        let mut paths = vec![sync::ManifestPath {
            path_id: assignment.path_id.clone(),
            rel: assignment.rel.clone(),
            is_symlink: assignment.is_symlink.unwrap_or(file.is_symlink),
        }];
        paths.extend(
            aliases_by_content
                .get(content_id.as_str())
                .cloned()
                .unwrap_or_default(),
        );
        candidates.push(sync::DesiredContentGroup {
            content_id: content_id.clone(),
            content_digest: (*digest).clone(),
            content_size: file.size,
            dataset_slugs: assignment
                .assignments
                .iter()
                .map(|(_, slug)| slug.clone())
                .collect(),
            paths,
            expected_records: 0,
            expected_passages: 0,
            expected_vectors: 0,
            expected_junk_records: 0,
            expected_records_by_dataset: BTreeMap::new(),
        });
    }
    let mut groups = sync::reconcile_groups(&base.groups, candidates)?;
    sync_executor::bind_prepared_counts(&mut groups, &snapshot, &inventory.keys)?;
    let (schema_identity, index_identity) = generation_contract_identities(&plan)?;
    let desired = sync::GenerationManifest {
        generation: base.generation + 1,
        execution: Some(sync::ExecutionIdentity {
            version: sync::EXECUTION_IDENTITY_VERSION,
            root_identity: root_identity.to_owned(),
            url: cfg.url.clone(),
            prefix: cfg.prefix.clone(),
            follow_symlinks: cfg.follow_symlinks,
            chunker_identity,
            embedding_identity_sha256: identity.identity_sha256,
            embedding_backend: identity.backend,
            embedding_dimension: identity.dimensions,
            embedding_semantic_contract: identity.semantic_contract,
            embedding_resumable: identity.resumable,
            graph_enabled: false,
            brain: "disabled".into(),
            detector_identity: DETECTOR_DISABLED_IDENTITY.into(),
            schema_identity,
            index_identity,
            source_policy: sync::SourceExecutionPolicy::DurableSnapshot {
                reference: format!("sync-snapshots/{tx_id}"),
                snapshot_digest: snapshot.snapshot_digest,
            },
        }),
        plan,
        groups,
    };
    // A failure here is the generation machinery contradicting itself about
    // durable state, and the invariant text alone gives the user nothing to
    // act on (#283). Name the recovery route; the invariant stays attached as
    // the cause so the report keeps its diagnostic value.
    let pending = sync::PendingSync::new(tx_id, &base, desired).with_context(|| {
        format!(
            "autoindex could not derive generation {} from committed generation {}; the durable \
             generation state in {} is not internally consistent, and re-running will not repair \
             it. No remote data was changed. Rebuild with a new --state-dir and a new --prefix",
            base.generation + 1,
            base.generation,
            state_dir.display()
        )
    })?;
    journal.sync_begin(&pending)
}

#[derive(Debug, PartialEq, Eq)]
struct CliErrorRoute {
    exit_code: i32,
    stdout: Option<String>,
    stderr: Option<String>,
}

fn route_cli_error(error: &anyhow::Error, json_output: bool) -> CliErrorRoute {
    if json_output {
        // Two distinct refusals reach the JSON route, and both must keep it.
        // `UnsafeFreshGenerationError` guards durable *generation* state on the
        // `--no-graph` path; `UnsupportedInventoryDeltaError` (#254) guards the
        // legacy graph-enabled path against a rerun that would strand live
        // documents. Neither supersedes the other — they protect different
        // destinations — so routing only one would silently drop the other's
        // machine-readable rendering back to a prose stderr line.
        if let Some(unsafe_fresh) = error.downcast_ref::<UnsafeFreshGenerationError>() {
            return CliErrorRoute {
                exit_code: 1,
                stdout: Some(unsafe_fresh.to_json().to_string()),
                stderr: None,
            };
        }
        if let Some(delta) = error.downcast_ref::<UnsupportedInventoryDeltaError>() {
            return CliErrorRoute {
                exit_code: 1,
                stdout: Some(delta.to_json().to_string()),
                stderr: None,
            };
        }
    }
    CliErrorRoute {
        exit_code: 1,
        stdout: None,
        stderr: Some(format!("error: {error:#}")),
    }
}

/// Entry point for the `xerj autoindex` subcommand (blocking; the server
/// binary calls this via spawn_blocking). Returns the process exit code.
pub fn run_cli() -> i32 {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let cmd = match cli::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n");
            cli::print_help();
            return 2;
        }
    };
    let json_output = matches!(&cmd, Cmd::Index(cfg) if cfg.json);
    let res = match cmd {
        Cmd::Help => {
            cli::print_help();
            return 0;
        }
        Cmd::Index(cfg) => run_index(*cfg),
        Cmd::Map(cfg) => run_map(cfg),
        Cmd::Status(cfg) => run_status(cfg),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            let route = route_cli_error(&e, json_output);
            if let Some(stdout) = route.stdout {
                println!("{stdout}");
            }
            if let Some(stderr) = route.stderr {
                eprintln!("{stderr}");
            }
            route.exit_code
        }
    }
}

const GB: u64 = 1 << 30;

/// Junk entries that name a file which also reached `file_done`.
///
/// The catalog holds ONE document per file: `catalog::file_doc` derives its
/// `_id` from the file key alone (`catalog::file_id`). The completion pass and
/// the junk pass both write into the same bulk body, so an entry that appears
/// in both is not two rows, it is one row written twice — and the junk write,
/// which lands second, is the one that survives. The observable damage is that
/// a file which indexed N records is reported as status "junk" with records 0,
/// plus a double count in `junk_file_count` and a double
/// `CodeCoverage::observe`.
///
/// Producers are required to keep the two sets disjoint: a worker that gives up
/// on a file sets `send_err`, which suppresses the completion, and a worker
/// that merely wants to report a partial problem notes it on the progress meter
/// instead. This returns the violations rather than assuming there are none,
/// because the caller is the only place that holds both sets and because the
/// failure is silent everywhere else.
fn shadowed_junk_entries<'a>(
    all_junk: &[&'a state::JunkFile],
    completed_keys: &HashSet<String>,
) -> Vec<&'a state::JunkFile> {
    all_junk
        .iter()
        .filter(|jf| completed_keys.contains(&jf.file_key))
        .copied()
        .collect()
}

/// How many entries a human-facing listing prints before it summarises the
/// rest. These lists are bounded by the corpus, not by the fault: unmounting a
/// bind mount under an indexed root makes every content group vanish at once,
/// so an uncapped listing is one rendered entry per journalled file — megabytes
/// of stderr, in the code paths whose entire job is to be read by a person.
const REFUSAL_LIST_CAP: usize = 10;
const SAMPLE_LIMIT_BYTES: u64 = 4 << 20;
const SQLDUMP_SAMPLE_LIMIT: u64 = 64 << 20;
/// Sampling byte cap for Unity assets.
///
/// Unity YAML is a GROUPED family: each `unity_class` is its own cluster, and
/// a class's first document can sit anywhere in the file — `extract/unity.rs`
/// notes real scenes exceed 200 MB. A class whose first document starts past
/// the cap is never sampled, so it gets no entry in `fa.assignments`, and
/// phase B then has nowhere to route its records. Under the old 4 MiB cap
/// that silently became `file_junk`. This cap is set past the size of the
/// scenes the format actually produces so that outcome needs a genuinely
/// pathological file, and when it does happen phase B now names the
/// unsampled group and its record count in `extra_junk` instead of adding
/// them to an anonymous counter.
const UNITY_SAMPLE_LIMIT: u64 = 512 << 20;

/// How many bytes phase A reads from a file of this family before it stops
/// sampling. `None` means the family's extractor caps itself.
///
/// Split out of `scan_file` so tests can shrink it. The consequence of a
/// GROUPED family hitting this cap is not "a slightly thinner sample", it is a
/// whole `unity_class`/SQL table with no dataset to route to in phase B — and
/// the only fixture that reaches it naturally is a half-gigabyte file, which
/// is why that path shipped untested. `SampleLimitOverride` gives the suite a
/// fixture it can afford.
fn sample_limit_bytes(family: Family, path: &Path) -> Option<u64> {
    // Only the test override reads the path; the shipped caps are per-family.
    #[cfg(not(test))]
    let _ = path;
    #[cfg(test)]
    {
        // Scoped to ONE corpus root, not process-global. `cargo test` runs this
        // binary multi-threaded, phase A itself runs on `crate::pool`, and a
        // bare global would silently re-cap sampling for every unrelated test
        // that happened to overlap — a flake that would look like anything but
        // its cause.
        if let Some((root, bytes)) = SAMPLE_LIMIT_OVERRIDE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            if path.starts_with(root) {
                return Some(*bytes);
            }
        }
    }
    match family {
        Family::SqlDump => Some(SQLDUMP_SAMPLE_LIMIT),
        Family::UnityYaml => Some(UNITY_SAMPLE_LIMIT),
        Family::Jsonl | Family::Logs | Family::Csv | Family::TxtLines => Some(SAMPLE_LIMIT_BYTES),
        Family::Sqlite => Some(1), // signals per-table row cap inside the extractor
        _ => None,                 // whole-file extractors cap themselves
    }
}

/// Test-only phase-A byte cap: `(corpus root, bytes)`, applied only to paths
/// under that root. `None` = off.
#[cfg(test)]
static SAMPLE_LIMIT_OVERRIDE: Mutex<Option<(std::path::PathBuf, u64)>> = Mutex::new(None);

/// Caps phase-A sampling under `root` for the lifetime of the guard.
#[cfg(test)]
pub(crate) struct SampleLimitOverride;

#[cfg(test)]
impl SampleLimitOverride {
    pub(crate) fn set(root: &Path, bytes: u64) -> Self {
        *SAMPLE_LIMIT_OVERRIDE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some((root.to_owned(), bytes));
        Self
    }
}

#[cfg(test)]
impl Drop for SampleLimitOverride {
    fn drop(&mut self) {
        *SAMPLE_LIMIT_OVERRIDE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[cfg(test)]
static REPLACEMENT_FAILPOINT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
fn replacement_failpoint(boundary: u8) -> Result<()> {
    if REPLACEMENT_FAILPOINT
        .compare_exchange(boundary, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        anyhow::bail!("injected replacement crash boundary {boundary}");
    }
    Ok(())
}

#[cfg(not(test))]
#[inline]
fn replacement_failpoint(_boundary: u8) -> Result<()> {
    Ok(())
}

/// Send one bulk body and fold its per-item rejections into
/// `rejected_records`.
///
/// This counter is deliberately NOT the parser-junk counter. A record the
/// *backend* refused and a record the *parser* could not read are different
/// failures with different lifetimes: parser junk is durable (journaled per
/// file and replayed on every resume), a backend rejection is not (no
/// `FileDone` records a document that was never accepted). Adding both to one
/// number is what let `junk_records_total` mean two things at once.
///
/// Note where this number can and cannot surface. Any non-zero value here also
/// puts an entry in `bulk_errors`, which aborts the run before the run
/// document exists — so it is reported in the abort message and nowhere else.
fn record_bulk_outcome(
    es: &Es,
    body: Vec<u8>,
    rejected_records: &AtomicU64,
    bulk_errors: &Mutex<Vec<String>>,
    send_err: &mut Option<String>,
) -> bool {
    match es.bulk(body) {
        Ok(outcome) => {
            if outcome.server_errors > 0 {
                *send_err = Some(format!(
                    "bulk backend failed for {} item(s): {}. Source file was not journaled \
                     complete; fix the server/embedding configuration and rerun autoindex",
                    outcome.server_errors,
                    outcome
                        .first_server_error
                        .as_deref()
                        .unwrap_or("unknown server error")
                ));
                return true;
            }
            if outcome.item_errors > 0 {
                rejected_records.fetch_add(outcome.item_errors, Ordering::Relaxed);
                if let Some(error) = outcome.first_error {
                    let mut errors = bulk_errors.lock().unwrap();
                    if errors.len() < 5 {
                        errors.push(error);
                    }
                }
            }
            false
        }
        Err(error) => {
            *send_err = Some(format!("{error:#}"));
            true
        }
    }
}

// ─── second-brain graph runtime ──────────────────────────────────────────

/// Per-run graph state, shared read-only with the Phase B workers
/// (SECOND_BRAIN_SPEC §6.6). Built after the plan is final because the
/// detectors resolve links against the FULL corpus — a per-file view could
/// not tell "dangling" from "not walked yet".
struct GraphRt {
    corpus: detect::CorpusIndex,
    detectors: Vec<Box<dyn detect::EdgeDetector>>,
    /// Raw-source href pass handle. Lives outside the registry because the
    /// HTML extractor strips markup before sectioning, so anchors only exist
    /// in the raw bytes — a source the `EdgeDetector` trait deliberately
    /// never sees (see `detect::href` module docs).
    href_raw: detect::href::Href,
    edges_index: String,
    brain: String,
    /// ONE wall-clock stamp per run: `created_at` is the single
    /// non-deterministic edge field (§6.4); per-worker clocks would make two
    /// halves of one run disagree about when it happened.
    created_at_ms: i64,
    /// detector tag → edges written this run (run-summary honesty §6.6.4).
    written: Mutex<std::collections::BTreeMap<&'static str, u64>>,
    self_dropped: AtomicU64,
    /// Prior-generation edges soft-invalidated before this run's writes.
    invalidated: u64,
}

/// Text-section locator → human label ("section 3", "page 2 section 0").
/// `emit_document` section locators are `s{i}`; PDF sections are
/// `p{page}-s{sec}` (extract/pdf.rs — page-major, so stream order IS the
/// lexicographic (page, sec) reading order). Everything else (row/line/byte/
/// table locators) is not a text section, returns None, and must not reach
/// `detect_text`. The label is used verbatim in sequence evidence rationales.
fn section_label(locator: &str) -> Option<String> {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    if let Some(rest) = locator.strip_prefix('s') {
        return digits(rest).then(|| format!("section {rest}"));
    }
    let rest = locator.strip_prefix('p')?;
    let (page, sec) = rest.split_once("-s")?;
    (digits(page) && digits(sec)).then(|| format!("page {page} section {sec}"))
}

// ─── --stub glob matcher ──────────────────────────────────────────────────

/// Longest accepted `--stub` glob. Far beyond any pattern a person types,
/// and far below the length at which the compiled regex hits its size limit.
const MAX_STUB_GLOB_LEN: usize = 4096;

/// Compiled `--stub <glob>` patterns. A matching file is indexed as ONE
/// name-card record (`Family::Stub`) and its contents are never opened —
/// the owner's way of saying "this data blob should be referenceable but
/// not parsed" without the engine hardcoding per-corpus rules.
///
/// Glob semantics (gitignore-flavored): `**` crosses `/`, `*` and `?` do
/// not; a pattern without `/` matches against the file NAME anywhere in the
/// tree, a pattern with `/` matches the full root-relative path.
pub struct StubMatcher {
    by_name: Vec<regex::Regex>,
    by_path: Vec<regex::Regex>,
}

impl StubMatcher {
    pub fn compile(globs: &[String]) -> Result<Self> {
        let mut by_name = Vec::new();
        let mut by_path = Vec::new();
        for g in globs {
            // `glob_to_regex` escapes every metacharacter, so no glob can
            // produce a SYNTACTICALLY invalid regex — the only reachable
            // failure is the compiled-size limit, which `?` (→ `[^/]`) and
            // `**` (→ `.*`) reach at ~10^5 characters. Caught here by length
            // so the message names the flag and the cause instead of
            // surfacing a regex-internal "compiled regex exceeds size limit".
            if g.len() > MAX_STUB_GLOB_LEN {
                anyhow::bail!(
                    "--stub {}…: pattern is {} characters (limit {MAX_STUB_GLOB_LEN})",
                    g.chars().take(40).collect::<String>(),
                    g.len()
                );
            }
            let re = regex::Regex::new(&glob_to_regex(g))
                .with_context(|| format!("--stub {g}: invalid pattern"))?;
            if g.contains('/') {
                by_path.push(re);
            } else {
                by_name.push(re);
            }
        }
        Ok(Self { by_name, by_path })
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty() && self.by_path.is_empty()
    }

    /// `rel` is the root-relative path with forward slashes.
    pub fn matches(&self, rel: &str) -> bool {
        if self.by_path.iter().any(|re| re.is_match(rel)) {
            return true;
        }
        if self.by_name.is_empty() {
            return false;
        }
        let name = rel.rsplit('/').next().unwrap_or(rel);
        self.by_name.iter().any(|re| re.is_match(name))
    }
}

fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    // `**/` also swallows its slash so `**/x` matches a
                    // top-level `x`.
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        out.push_str("(?:.*/)?");
                    } else {
                        out.push_str(".*");
                    }
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

/// The synthetic sniff result for a `--stub`-designated file.
///
/// `logical_path` is the file as the CORPUS names it, which under durable
/// preparation is not the path the bytes live at (`blobs/00000000`). A stub's
/// only output is a name card, so taking the name from the content path would
/// title every stub after a blob ordinal — the #294 failure class that
/// `Sniffed::logical_name` exists to prevent.
fn stub_sniffed(logical_path: &Path) -> Sniffed {
    Sniffed {
        family: Family::Stub,
        gzip: false,
        binary_kind: None,
        csv: None,
        encoding: "utf-8",
        logical_name: logical_path.file_name().map(std::path::PathBuf::from),
    }
}

#[cfg(test)]
mod stub_matcher_tests {
    use super::StubMatcher;

    fn m(globs: &[&str]) -> StubMatcher {
        StubMatcher::compile(&globs.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn a_bare_pattern_matches_file_names_anywhere() {
        let s = m(&["*.csv"]);
        assert!(s.matches("unity/Assets/Face/f_roommate_004.csv"));
        assert!(s.matches("top.csv"));
        assert!(!s.matches("unity/Assets/notes.csv.md"));
    }

    #[test]
    fn a_path_pattern_matches_the_root_relative_path() {
        let s = m(&["unity/**/*.csv"]);
        assert!(s.matches("unity/Assets/Face/f_roommate_004.csv"));
        assert!(s.matches("unity/top.csv"), "**/ also matches zero dirs");
        assert!(!s.matches("backend/data/users.csv"), "scoped to unity/");
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let s = m(&["unity/*.csv"]);
        assert!(s.matches("unity/top.csv"));
        assert!(!s.matches("unity/Assets/deep.csv"));
    }

    #[test]
    fn regex_metacharacters_in_patterns_are_literal() {
        let s = m(&["data(v1).csv"]);
        assert!(s.matches("x/data(v1).csv"));
        assert!(!s.matches("x/dataXv1Y.csv"));
    }

    #[test]
    fn an_invalid_pattern_fails_loudly_at_startup() {
        // The old body asserted only that a VALID pattern compiles, so it
        // never tested its own name and would have passed against a `compile`
        // that could not fail at all.
        let msg = match StubMatcher::compile(&["?".repeat(super::MAX_STUB_GLOB_LEN + 1)]) {
            Ok(_) => panic!("an over-long pattern must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("--stub"), "message must name the flag: {msg}");
        assert!(
            msg.contains(&super::MAX_STUB_GLOB_LEN.to_string()),
            "message must state the limit: {msg}"
        );
    }

    /// Why the error above is the ONLY reachable one: `glob_to_regex`
    /// escapes every metacharacter, so no amount of regex syntax in a glob
    /// can produce an invalid pattern. Anything asserting otherwise is
    /// asserting something the code makes impossible.
    #[test]
    fn regex_syntax_in_a_glob_is_never_a_compile_error() {
        for g in ["[", "(", "\\", "*?[a-", "a{2,", "(?P<x>", "+", "|", "^$"] {
            assert!(
                StubMatcher::compile(&[g.to_string()]).is_ok(),
                "{g} must compile: metacharacters are escaped, not interpreted"
            );
        }
    }
}

// ─── Phase A: per-file scan (sniff + bounded sampling) ───────────────────

struct FileScan {
    sniffed: Option<Sniffed>,
    sketches: Vec<GroupSketch>,
    junk: Option<(String, String)>, // (status, reason)
    /// Run-local PDF extraction produced during sampling. This is consumed by
    /// Phase B only when it is bound to the same full-content generation.
    pdf_spool: Option<extract::pdf::ExtractionSpool>,
    pdf_spool_fallbacks: Vec<extract::pdf::SpoolFallback>,
}

/// One sampled group within a file: every field it produced, plus the names
/// that came from the file rather than from the extractor (`FieldOrigin`).
/// Only the latter may decide which dataset the file joins — see `dataset`.
struct GroupSketch {
    group: Option<String>,
    fields: HashMap<String, infer::FieldAcc>,
    key_fields: std::collections::HashSet<String>,
    records: u64,
}

fn take_pdf_spool_if_indexable<T>(
    spool: &mut Option<T>,
    is_junk: bool,
    budget: &extract::pdf::ExtractionSpoolBudget,
) -> Option<T> {
    if is_junk {
        if spool.is_some() {
            budget.record_discarded_before_replay();
        }
        spool.take();
        None
    } else {
        spool.take()
    }
}

/// Everything phase A needs beyond the file list and the run config: where a
/// run-local PDF artifact may live, the shared admission budget, the one-line
/// capacity explanation reported when files fall back — and the progress
/// surface those lines go out through, because phase A now reports every file
/// it touches (#241) as well as every artifact it could not keep.
///
/// Grouped so `scan_file` and `build_phase_a` each take one parameter instead
/// of four.
struct PhaseAContext<'a> {
    state_dir: &'a Path,
    budget: &'a std::sync::Arc<extract::pdf::ExtractionSpoolBudget>,
    capacity_warning: Option<&'a str>,
    progress: &'a Progress,
    /// Throughput evidence for the pre-index estimate. Phase A already parses
    /// every file; timing that parse is the only way to price the run on the
    /// machine it will actually run on rather than on ours.
    meter: &'a estimate::Meter,
}

fn scan_file(
    path: &Path,
    size: u64,
    digest: &str,
    ctx: &PhaseAContext<'_>,
    sample: usize,
    max_file_gb: u64,
    stub: bool,
) -> FileScan {
    let state_dir = ctx.state_dir;
    let pdf_spool_budget = ctx.budget;
    let mut out = FileScan {
        sniffed: None,
        sketches: Vec::new(),
        junk: None,
        pdf_spool: None,
        pdf_spool_fallbacks: Vec::new(),
    };
    let sn = if stub {
        stub_sniffed(path)
    } else {
        match sniff::sniff(path) {
            Ok(s) => s,
            Err(e) => {
                out.junk = Some(("junk".into(), format!("unreadable: {e}")));
                return out;
            }
        }
    };
    if sn.family == Family::Binary {
        out.junk = Some((
            "junk".into(),
            format!(
                "binary content ({})",
                sn.binary_kind.clone().unwrap_or_else(|| "unknown".into())
            ),
        ));
        out.sniffed = Some(sn);
        return out;
    }
    // whole-file families get a size cap; streaming families don't need one
    let whole_file = matches!(
        sn.family,
        Family::Json | Family::Html | Family::Yaml | Family::TxtProse | Family::Pdf | Family::Docx
    );
    if whole_file && size > max_file_gb * GB {
        out.junk = Some((
            "skipped".into(),
            format!(
                "oversized for non-streaming family {} (> {max_file_gb} GB)",
                sn.family.as_str()
            ),
        ));
        out.sniffed = Some(sn);
        return out;
    }
    let limit = sample_limit_bytes(sn.family, path);
    type GroupAcc = (
        HashMap<String, infer::FieldAcc>,
        u64,
        std::collections::HashSet<String>,
    );
    let mut groups: HashMap<Option<String>, GroupAcc> = HashMap::new();
    // Grouped families keep reading past the per-group sample size: their
    // groups (SQL tables, Unity classes) appear all through the file, and
    // stopping at the first N records would leave later groups unsampled and
    // untyped.
    let grouped_family = matches!(
        sn.family,
        Family::SqlDump | Family::Sqlite | Family::UnityYaml
    );
    let mut sink = |rec: extract::RawRecord| -> bool {
        let entry = groups.entry(rec.group.clone()).or_default();
        if (entry.1 as usize) < sample {
            // Every field feeds type inference; only the ones the FILE named
            // feed the clustering key, so an extractor that starts emitting a
            // new field cannot re-home the file (#178).
            let from_file = rec.origin == extract::FieldOrigin::Data;
            for (k, v) in &rec.fields {
                entry.0.entry(k.clone()).or_default().add(v);
                if from_file {
                    entry.2.insert(k.clone());
                }
            }
        }
        entry.1 += 1;
        if grouped_family {
            true // read on — later tables still need sampling
        } else {
            (entry.1 as usize) < sample
        }
    };
    // Timed span: the extraction itself, nothing around it. The PDF spool's
    // post-parse `content::verify` re-read is deliberately outside it — phase
    // B never repeats that read, so charging it here would price a cost the
    // estimate is trying to predict away.
    let extraction_started = Instant::now();
    let extraction_elapsed;
    let extraction = if sn.family == Family::Pdf {
        let parsed = extract::pdf::extract_and_spool(
            path,
            state_dir,
            size,
            digest,
            pdf_spool_budget,
            &mut sink,
        );
        extraction_elapsed = extraction_started.elapsed();
        match parsed {
            Ok((stats, spool, fallback)) => {
                // The inventory digest was computed before Phase A. Only hand
                // bytes to Phase B when the source still matches that exact
                // generation after the parser has finished reading it.
                // If no reusable artifact exists, avoid a second full-file
                // read: Phase B performs the authoritative generation check
                // immediately before its ordinary reparse.
                if spool.is_none() {
                    out.pdf_spool_fallbacks.extend(fallback);
                } else {
                    match content::verify(path, size, digest) {
                        Ok(()) => {
                            out.pdf_spool = spool;
                            out.pdf_spool_fallbacks.extend(fallback);
                        }
                        Err(error) => {
                            if spool.is_some() {
                                pdf_spool_budget.record_discarded_before_replay();
                            }
                            pdf_spool_budget.record_source_generation_changed();
                            out.pdf_spool_fallbacks.extend(fallback);
                            out.pdf_spool_fallbacks.push(extract::pdf::SpoolFallback {
                                category: "source_generation_changed",
                                message: format!(
                                    "source generation changed after extraction: {error:#}"
                                ),
                            });
                        }
                    }
                }
                Ok(stats)
            }
            Err(error) => Err(error),
        }
    } else {
        let parsed = extract::extract(path, &sn, limit, &mut sink);
        extraction_elapsed = extraction_started.elapsed();
        parsed
    };
    match extraction {
        Ok(stats) => {
            if groups.is_empty() {
                out.junk = Some((
                    "junk".into(),
                    format!(
                        "no records extracted ({} candidate family, {} junk lines)",
                        sn.family.as_str(),
                        stats.junk
                    ),
                ));
            }
        }
        Err(e) => {
            if groups.is_empty() {
                out.junk = Some(("junk".into(), format!("extract failed: {e}")));
            }
        }
    }
    // Throughput evidence, but only where the read was provably complete: the
    // sampler stops at whichever of the byte cap or the record cap comes
    // first, and timing a partial read against the full file size would invent
    // a rate this machine never demonstrated. `exact_scan_bytes` owns that
    // judgement; a junked file is never timed at all, because phase B will
    // not process it.
    if out.junk.is_none() {
        let sampled = groups.values().map(|(_, records, _)| *records).max();
        if let Some(bytes) = sampled.and_then(|records| {
            estimate::exact_scan_bytes(sn.family, sn.gzip, size, records, sample)
        }) {
            ctx.meter.record(sn.family, bytes, extraction_elapsed);
        }
    }

    out.sketches = groups
        .into_iter()
        .map(|(group, (fields, records, key_fields))| GroupSketch {
            group,
            fields,
            key_fields,
            records,
        })
        .collect();
    out.sketches.sort_by(|a, b| a.group.cmp(&b.group));
    out.sniffed = Some(sn);
    out
}

#[cfg(test)]
mod clustering_key_tests {
    use super::*;

    fn scan(dir: &Path, name: &str, body: &str) -> FileScan {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        // Clustering keys are decided by extraction, not by PDF artifact
        // reuse: a zero budget keeps these cases on the plain parse path.
        let budget = extract::pdf::ExtractionSpoolBudget::new(0, 0);
        let progress = Progress::silent();
        let meter = estimate::Meter::new();
        let ctx = PhaseAContext {
            state_dir: dir,
            budget: &budget,
            capacity_warning: None,
            progress: &progress,
            meter: &meter,
        };
        scan_file(&path, size, "d0", &ctx, 500, 2, false)
    }

    /// The #178 mechanism, from the extractor to the clustering key: a source
    /// file that yields symbols and one that yields none produce the SAME
    /// (empty) key, so no extractor improvement can move a file between
    /// datasets. `defs` still reaches the mapping — it is indexed, just not
    /// used to decide identity.
    #[test]
    fn symbols_never_enter_the_clustering_key() {
        let dir = std::env::temp_dir().join("xerj-ax-178-key");
        std::fs::create_dir_all(&dir).unwrap();

        // #170 now captures the `const`, so this file parses to one symbol and
        // GAINS a `defs` field — the case #170 improves. The point of this test
        // survives that change: the newly-captured extractor name must still be
        // kept out of the clustering key (#180), so a const-only file and a
        // fn/struct file still land in ONE dataset rather than re-homing apart.
        let table = scan(
            &dir,
            "table.rs",
            "const BYTE_FREQUENCIES: [u8; 2] = [1, 2];\n",
        );
        let code = scan(&dir, "code.rs", "fn main() {}\nstruct S;\n");
        let _ = std::fs::remove_dir_all(&dir);

        for s in [&table, &code] {
            assert_eq!(s.sketches.len(), 1, "one code record per file");
            assert!(
                s.sketches[0].key_fields.is_empty(),
                "extractor-invented names leaked into the clustering key: {:?}",
                s.sketches[0].key_fields
            );
        }
        // #170: the const is now captured, so the table file has `defs` too.
        assert!(table.sketches[0].fields.contains_key("defs"));
        assert!(code.sketches[0].fields.contains_key("defs"));

        // …so the two land in one dataset instead of one dataset each.
        let rels = vec!["table.rs".to_string(), "code.rs".to_string()];
        let sketches: Vec<dataset::Sketch> = [&table, &code]
            .iter()
            .enumerate()
            .map(|(i, s)| dataset::Sketch {
                file_idx: i,
                group: s.sketches[0].group.clone(),
                family: s.sniffed.as_ref().unwrap().family,
                fields: s.sketches[0].fields.clone(),
                key_fields: s.sketches[0].key_fields.clone(),
                records: s.sketches[0].records,
            })
            .collect();
        let scopes = vec![String::new(); rels.len()];
        let clusters = dataset::cluster(sketches, &rels, &scopes);
        assert_eq!(clusters.len(), 1, "{clusters:#?}");
    }

    /// The other half: a data file's own column names ARE the key, so real
    /// schemas still drive clustering.
    #[test]
    fn data_field_names_are_the_clustering_key() {
        let dir = std::env::temp_dir().join("xerj-ax-178-data");
        std::fs::create_dir_all(&dir).unwrap();
        let csv = scan(&dir, "t.csv", "id,email\n1,a@b.c\n2,d@e.f\n");
        let _ = std::fs::remove_dir_all(&dir);
        let mut names: Vec<&str> = csv.sketches[0]
            .key_fields
            .iter()
            .map(String::as_str)
            .collect();
        names.sort();
        assert_eq!(names, ["email", "id"]);
    }
}

#[cfg(test)]
mod phase_a_grouping_tests {
    use super::*;

    pub(super) fn cfg_for(root: &Path) -> IndexCfg {
        IndexCfg {
            root: root.to_path_buf(),
            stub_globs: Vec::new(),
            url: "http://unused.invalid".into(),
            api_key: None,
            api_key_file: None,
            workers: 1,
            scan_workers: 1,
            pdf_workers: 1,
            resource_notes: Vec::new(),
            pdf_timeout_secs: 10,
            bulk_mb: 1,
            bulk_timeout_secs: 10,
            snapshot_max_bytes: 64 << 30,
            prefix: "t".into(),
            state_dir: None,
            fresh: true,
            follow_symlinks: false,
            follow_symlinks_outside_root: false,
            ignore: crate::ignore_rules::IgnoreOptions::default(),
            max_file_gb: 2,
            sample: 500,
            no_semantic: false,
            brain: None,
            no_graph: true,
            // The gate is switched off in these fixtures on purpose: they assert
            // indexing, resume and edge behaviour, and a timing-derived stop would
            // make them depend on how loaded the runner was. The gate's own
            // behaviour is covered in `gate_tests` and `cli::tests`.
            max_minutes: 0,
            approve: None,
            dry_run: true,
            json: false,
            quiet: true,
            progress: crate::progress::ProgressMode::None,
            progress_interval: None,
        }
    }

    fn plan_for(root: &Path) -> Plan {
        let files = walk::walk(root, false).unwrap();
        let keys: Vec<String> = files
            .iter()
            .map(|f| ids::file_key(&f.path, f.size).unwrap())
            .collect();
        let digests: Vec<String> = (0..files.len()).map(|i| format!("d{i}")).collect();
        // Planning is what these cases assert on; a zero budget keeps every
        // file on the plain parse path so no artifact is ever retained.
        let budget = extract::pdf::ExtractionSpoolBudget::new(0, 0);
        let progress = Progress::silent();
        let meter = estimate::Meter::new();
        let ctx = PhaseAContext {
            state_dir: root,
            budget: &budget,
            capacity_warning: None,
            progress: &progress,
            meter: &meter,
        };
        build_phase_a(
            root,
            &files,
            &keys,
            &digests,
            Vec::new(),
            &ctx,
            &cfg_for(root),
        )
        .plan
    }

    const CODE: &str = "// The event loop dispatches every ready connection to a worker.\n\
        // Each worker drains its queue before polling for more sockets.\n\
        static void dispatch_ready_connections(struct event_loop *loop) {\n\
            for (int index = 0; index < loop->ready_count; index++) {\n\
                worker_submit(loop->workers, loop->ready[index]);\n\
            }\n\
        }\n";

    const PROSE: &str = "# Overview\n\nThis server accepts client connections and stores \
        keys in memory. Every command travels through the same parser before the \
        dispatcher routes it to the matching handler function.\n";

    /// #173 end to end through the real planner: a tree holding two
    /// repositories of source, prose and one-off config JSON yields one
    /// document dataset per repository — with `body` elected `semantic_text`
    /// — plus the genuine data dataset, instead of one dataset per incidental
    /// config schema with no vector arm.
    #[test]
    fn a_two_repo_tree_plans_one_document_dataset_per_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for repo in ["valkey", "memcached"] {
            std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
            std::fs::write(root.join(repo).join(".git").join("HEAD"), "ref: x").unwrap();
        }
        std::fs::create_dir_all(root.join("valkey/src")).unwrap();
        std::fs::create_dir_all(root.join("valkey/commands")).unwrap();
        std::fs::create_dir_all(root.join("memcached/data")).unwrap();
        std::fs::write(root.join("valkey/src/server.c"), CODE).unwrap();
        std::fs::write(root.join("valkey/README.md"), PROSE).unwrap();
        // one-off configs: single-record JSON, each with its own key set
        std::fs::write(
            root.join("valkey/commands/get.json"),
            r#"{"GET": {"summary": "Return the string value stored at the given key.", "arity": 2}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("valkey/commands/set.json"),
            r#"{"SET": {"summary": "Store the given string value under the given key.", "arity": 3}}"#,
        )
        .unwrap();
        // distinct bytes — byte-identical files alias into one canonical copy
        std::fs::write(root.join("memcached/proto.c"), format!("{CODE}\n// v2\n")).unwrap();
        // a genuine data file: recurring rows, real schema
        std::fs::write(
            root.join("memcached/data/events.csv"),
            "id,email,level\n1,a@b.example,info\n2,c@d.example,warn\n3,e@f.example,info\n",
        )
        .unwrap();

        let plan = plan_for(root);
        let mut slugs: Vec<&str> = plan.datasets.iter().map(|d| d.slug.as_str()).collect();
        slugs.sort();
        assert_eq!(
            slugs,
            ["memcached-data", "memcached-docs", "valkey-docs"],
            "{:#?}",
            plan.datasets
        );

        // every docs dataset carries the vector arm on body
        for d in plan.datasets.iter().filter(|d| d.family == "docs") {
            let body = d.specs.iter().find(|s| s.name == "body").unwrap();
            assert_eq!(body.es_type, "semantic_text", "{}: {:#?}", d.slug, d.specs);
            assert_eq!(d.semantic_field.as_deref(), Some("body"), "{}", d.slug);
        }

        // the one-off configs were demoted: document-rendered, and their
        // config keys never reached the docs mapping
        let by_rel: HashMap<&str, &FileAssignment> =
            plan.files.values().map(|f| (f.rel.as_str(), f)).collect();
        assert!(by_rel["valkey/commands/get.json"].as_document);
        assert!(by_rel["valkey/commands/set.json"].as_document);
        assert!(!by_rel["valkey/src/server.c"].as_document);
        assert!(!by_rel["memcached/data/events.csv"].as_document);
        let vdocs = plan
            .datasets
            .iter()
            .find(|d| d.slug == "valkey-docs")
            .unwrap();
        assert!(
            !vdocs.specs.iter().any(|s| s.name.contains("summary")),
            "config keys leaked into the docs mapping: {:#?}",
            vdocs.specs
        );
        assert_eq!(vdocs.file_count, 4, "code + prose + 2 demoted configs");

        // the CSV kept its real schema
        let data = plan
            .datasets
            .iter()
            .find(|d| d.slug == "memcached-data")
            .unwrap();
        assert!(data.specs.iter().any(|s| s.name == "email"), "{data:#?}");
    }

    /// #196: the same tree WITHOUT nested `.git` markers (or with one at the
    /// root — a workspace) is ONE scope: a single document corpus.
    #[test]
    fn a_single_workspace_plans_one_document_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("wasm/examples")).unwrap();
        std::fs::write(root.join("src/a.c"), CODE).unwrap();
        std::fs::write(root.join("src/b.c"), CODE).unwrap();
        std::fs::write(root.join("README.md"), PROSE).unwrap();
        std::fs::write(root.join("wasm/examples/demo.md"), PROSE).unwrap();

        let plan = plan_for(root);
        assert_eq!(
            plan.datasets.len(),
            1,
            "one workspace, one corpus: {:#?}",
            plan.datasets
        );
        assert_eq!(plan.datasets[0].slug, "docs");
        assert_eq!(plan.datasets[0].file_count, 4);
    }
}

// ─── Phase A plan building (pure: no server contact) ─────────────────────

/// Repository scope per file: the deepest ancestor directory (root-relative,
/// "/"-separated) containing a `.git` entry, or "" when the file is under
/// none. The walk never descends into `.git` itself, but the marker is still
/// on disk. A `.git` at the autoindex root yields "" for every file — the
/// whole tree is one scope — and so does a tree with no `.git` at all, which
/// is exactly the #173/#196 property: one autoindex of one folder yields a
/// corpus searchable as one corpus, split only at nested repository roots.
fn compute_scopes(root: &Path, rels: &[String]) -> Vec<String> {
    let mut cache: HashMap<String, bool> = HashMap::new();
    rels.iter()
        .map(|rel| {
            let mut scope = String::new();
            let mut prefix = String::new();
            let mut segs = rel.split('/').peekable();
            while let Some(seg) = segs.next() {
                if segs.peek().is_none() {
                    break; // final segment is the file name
                }
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(seg);
                let repo = *cache
                    .entry(prefix.clone())
                    .or_insert_with(|| root.join(&prefix).join(".git").exists());
                if repo {
                    scope = prefix.clone();
                }
            }
            scope
        })
        .collect()
}

/// What phase A produces: the frozen plan, the clusters phase B needs, and
/// the run-local PDF artifacts (index-aligned with `files`) that phase B may
/// replay instead of parsing again. A spool is always optional — every entry
/// may legitimately be `None`.
struct PhaseA {
    plan: Plan,
    clusters: Vec<dataset::Cluster>,
    pdf_spools: Vec<Option<extract::pdf::ExtractionSpool>>,
}

/// Record and report why individual PDFs could not retain a run-local
/// artifact. Reuse is an accelerator, so this is purely informational: every
/// listed file is parsed again by the normal phase B path. Recording is kept
/// even when the budget is globally disabled — the stored examples are the
/// only place the `--json` report explains *why* nothing was reused, and they
/// are already capped at three.
///
/// Reporting goes out through the progress surface, never a bare `eprintln!`:
/// stderr belongs to that surface, so `--progress none` stays silent and
/// `--progress json` stays a single parseable stream (#241).
fn report_pdf_spool_fallbacks(
    files: &[walk::FileEntry],
    scans: &[FileScan],
    ctx: &PhaseAContext<'_>,
) {
    let pdf_spool_budget = ctx.budget;
    let pr = ctx.progress;
    let reasons: Vec<(&str, &extract::pdf::SpoolFallback)> = scans
        .iter()
        .enumerate()
        .flat_map(|(index, scan)| {
            scan.pdf_spool_fallbacks
                .iter()
                .map(move |fallback| (files[index].rel.as_str(), fallback))
        })
        .collect();
    for (path, fallback) in &reasons {
        pdf_spool_budget.record_fallback_example(path, fallback.category, &fallback.message);
    }
    if reasons.is_empty() {
        return;
    }
    if pdf_spool_budget.platform_reuse_is_unavailable() {
        pr.note(
            "phase A: run-local PDF extraction reuse is unavailable on this platform; \
             phase B will use the normal parser",
        );
        return;
    }
    pr.note(&format!(
        "phase A: {} PDF extraction(s) could not retain a bounded run-local artifact; \
         phase B will parse them again safely",
        reasons.len()
    ));
    if let Some(warning) = ctx.capacity_warning {
        pr.note(&format!("  PDF reuse capacity: {warning}"));
    }
    for (path, fallback) in reasons.iter().take(3) {
        pr.note(&format!(
            "  PDF reuse fallback for {path}: {}",
            fallback.message
        ));
    }
    if reasons.len() > 3 {
        pr.note(&format!(
            "  … and {} more PDF reuse fallback(s)",
            reasons.len() - 3
        ));
    }
}

/// Sniff + sample every file, cluster into datasets, and assemble the plan.
/// Pure planning: reads the tree, never the server — which is what makes the
/// #173/#196 grouping behaviour testable end-to-end without a cluster.
fn build_phase_a(
    root: &Path,
    files: &[walk::FileEntry],
    keys: &[String],
    digests: &[String],
    duplicate_files: Vec<DuplicateFile>,
    ctx: &PhaseAContext<'_>,
    cfg: &IndexCfg,
) -> PhaseA {
    use rayon::prelude::*;
    let pdf_spool_budget = ctx.budget;
    let pr = ctx.progress;
    // Same pool as the digest phase: sniffing and sampling are the other half
    // of the CPU-bound phase `--workers` has to bound (#240 §2). Progress is
    // reported from inside that pool, so the straggler the ticker names is the
    // file a scan-pool thread is genuinely sitting on (#241). Retaining a PDF
    // artifact happens inside that same guard, so a file whose extraction is
    // spooled is counted exactly like a plainly parsed one.
    // Patterns were validated (loudly) at run start; a failure here would be
    // a programming error, not user input.
    let stub_matcher = StubMatcher::compile(&cfg.stub_globs).expect("validated at startup");
    let scans: Vec<FileScan> = crate::pool::install(|| {
        files
            .par_iter()
            .zip(digests.par_iter())
            .map(|(f, digest)| {
                let _in_flight = pr.file(&f.rel, f.size);
                scan_file(
                    &f.path,
                    f.size,
                    digest,
                    ctx,
                    cfg.sample,
                    cfg.max_file_gb,
                    stub_matcher.matches(&f.rel),
                )
            })
            .collect()
    });

    report_pdf_spool_fallbacks(files, &scans, ctx);

    let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
    let scopes = compute_scopes(root, &rels);
    // (family, gzip) per file, from the scan's sniff — reused for assignments
    // and demoted-file re-sampling instead of re-sniffing every file.
    let file_meta: Vec<Option<(Family, bool)>> = scans
        .iter()
        .map(|sc| sc.sniffed.as_ref().map(|s| (s.family, s.gzip)))
        .collect();
    let mut sketches = Vec::new();
    let mut junk_files = Vec::new();
    let mut pdf_spools: Vec<Option<extract::pdf::ExtractionSpool>> =
        (0..files.len()).map(|_| None).collect();
    for (i, mut sc) in scans.into_iter().enumerate() {
        let family = sc
            .sniffed
            .as_ref()
            .map(|s| s.family)
            .unwrap_or(Family::Binary);
        // A file that phase A junks is never indexed, so its artifact is
        // refunded here rather than held to the phase A→B boundary.
        pdf_spools[i] =
            take_pdf_spool_if_indexable(&mut sc.pdf_spool, sc.junk.is_some(), pdf_spool_budget);
        if let Some((status, reason)) = sc.junk {
            junk_files.push(JunkFile {
                file_key: keys[i].clone(),
                rel: files[i].rel.clone(),
                format: format_str(sc.sniffed.as_ref()),
                status,
                reason,
                bytes: files[i].size,
            });
            continue;
        }
        for gs in sc.sketches {
            sketches.push(dataset::Sketch {
                file_idx: i,
                group: gs.group,
                family,
                fields: gs.fields,
                key_fields: gs.key_fields,
                records: gs.records,
            });
        }
    }
    let mut clusters = dataset::cluster(sketches, &rels, &scopes);

    // Demoted one-off config files (#173) were sampled as flattened records;
    // phase B will index them as documents. Re-sample them through the same
    // document renderer so the docs dataset's mapping and stats describe what
    // actually gets indexed.
    for c in clusters.iter_mut().filter(|c| !c.demoted.is_empty()) {
        let demoted = c.demoted.clone();
        let fields = &mut c.fields;
        let mut sampled_total = 0u64;
        for m in demoted {
            let gzip = file_meta[m].map(|(_, g)| g).unwrap_or(false);
            let mut sampled = 0u64;
            let mut sink = |rec: extract::RawRecord| -> bool {
                if (sampled as usize) < cfg.sample {
                    for (k, v) in &rec.fields {
                        fields.entry(k.clone()).or_default().add(v);
                    }
                }
                sampled += 1;
                (sampled as usize) < cfg.sample
            };
            // An unreadable file junks at phase B like any other; the plan
            // keeps its membership either way.
            let _ = extract::extract_as_document(&files[m].path, gzip, &mut sink);
            sampled_total += sampled;
        }
        c.records += sampled_total;
    }

    // per-file assignments
    let mut file_assignments: HashMap<String, FileAssignment> = HashMap::new();
    for (ci, c) in clusters.iter().enumerate() {
        let demoted: std::collections::HashSet<usize> = c.demoted.iter().copied().collect();
        for &m in &c.members {
            let key = &keys[m];
            let (family, gzip) = file_meta[m].unwrap_or((c.family, false));
            let fa = file_assignments
                .entry(key.clone())
                .or_insert_with(|| FileAssignment {
                    rel: files[m].rel.clone(),
                    path_id: files[m].rel_id.clone(),
                    is_symlink: Some(files[m].is_symlink),
                    family: family.as_str().to_string(),
                    gzip,
                    content_digest: Some(digests[m].clone()),
                    assignments: Vec::new(),
                    as_document: false,
                });
            fa.as_document |= demoted.contains(&m);
            fa.assignments
                .push((c.group.clone(), clusters[ci].slug.clone()));
        }
    }

    let mut datasets = Vec::new();
    for c in &clusters {
        let mut specs =
            infer::infer_fields_with_policy(&c.fields, c.records, cfg.no_semantic, c.is_docs);
        // Unity script-link enrichment fields are stamped by the phase-B
        // pipeline (not the extractor), so inference never sees them —
        // register them here or they would be dynamic-mapped coarsely.
        //
        // Registered for EVERY UnityYaml cluster, not only those whose sample
        // happened to contain a `script_guid`. Phase B stamps these whenever a
        // record carries a resolvable guid, and phase A reads a bounded window
        // of the file — so gating the mapping on the sample meant a cluster
        // whose sampled window held no `m_Script` got these two fields
        // dynamic-mapped at index time instead, feeding the field-budget
        // overshoot in #312. Two keyword specs per Unity cluster is a fixed,
        // predictable cost; a dynamic mapping is not.
        if c.family == Family::UnityYaml {
            specs.push(pipeline_keyword_spec("script_path"));
            specs.push(pipeline_keyword_spec("script_class"));
        }
        if c.family == Family::UnityMeta {
            specs.push(pipeline_keyword_spec("asset_path"));
        }
        let time_field = infer::elect_time_field(&specs);
        let semantic_field = specs
            .iter()
            .find(|s| s.es_type == "semantic_text")
            .map(|s| s.name.clone());
        datasets.push(PlanDataset {
            slug: c.slug.clone(),
            index: format!("{}-{}", cfg.prefix, c.slug),
            family: if c.is_docs {
                "docs".to_string()
            } else {
                c.family.as_str().to_string()
            },
            group: c.group.clone(),
            specs,
            time_field,
            semantic_field,
            sampled_records: c.records,
            file_count: c.members.len(),
        });
    }
    // Aliases of junk/skipped content have no `plan.files` entry and therefore
    // no manifest group to attach to, so keeping them made the generation
    // cutover fail its own alias-projection invariant on any folder holding
    // two byte-identical junk files — two empty files are enough (#283). The
    // incremental projection already drops them (`reconcile_plan`'s
    // `live_content` filter); the fresh plan must project the same way or a
    // no-op re-run would see a changed plan and commit a spurious generation.
    let duplicate_files = duplicate_files
        .into_iter()
        .filter(|alias| file_assignments.contains_key(&alias.file_key))
        .collect();
    let plan = Plan {
        datasets,
        files: file_assignments,
        junk_files,
        duplicate_files,
        alias_paths_indexed: true,
    };
    PhaseA {
        plan,
        clusters,
        pdf_spools,
    }
}

// ─── mapping builder ─────────────────────────────────────────────────────

pub const PROVENANCE_FIELDS: &[&str] = &[
    "ax_path",
    "ax_paths",
    "ax_file",
    "ax_locator",
    "ax_dataset",
    "ax_run",
    "ax_format",
];

/// Spec for a field the PIPELINE derives at index time (Unity script-link
/// enrichment): typed keyword in the explicit mapping, zeroed sampling stats
/// because phase-A inference never observes it.
fn pipeline_keyword_spec(name: &str) -> infer::FieldSpec {
    infer::FieldSpec {
        name: name.into(),
        es_type: "keyword".into(),
        date_enc: None,
        semantic: None,
        cardinality_est: 0,
        cardinality_overflow: false,
        null_ratio: 0.0,
        avg_len: 0.0,
        coverage: 0.0,
        examples: Vec::new(),
        notes: vec!["pipeline-derived: resolved from the .meta guid map at index time".into()],
        date_min: None,
        date_max: None,
        date_evidence: Vec::new(),
    }
}

/// Outcome of building the Unity script-link map, including what did NOT
/// resolve. The failures are the whole point: `script_path`/`script_class`
/// are absent both when a guid is unreadable and when nothing references it,
/// and those two look identical in the index.
#[derive(Debug, Default)]
struct UnityGuidMap {
    /// `.meta` guid → root-relative asset path.
    map: std::collections::HashMap<String, String>,
    /// `.meta` files that could not be read or parsed at all.
    unreadable: Vec<(String, String)>,
    /// `.meta` files that parsed but carried no `guid:` key.
    no_guid: Vec<String>,
}

/// Unity script-link map: `.meta` guid → root-relative asset path.
///
/// Runs on `crate::pool` and under the progress meter. A `.meta` is tiny, but
/// a real Unity project has 10k-500k of them, and this is on the critical
/// path of EVERY run — including a resumed no-op incremental, which otherwise
/// has no work to do at all. Doing that serially and unmetered is the
/// unattributed-stretch pattern of #241: the process sits silent for minutes
/// with the bar parked.
fn build_unity_guid_map(files: &[walk::FileEntry], plan: &Plan, pr: &Progress) -> UnityGuidMap {
    use rayon::prelude::*;

    let by_rel: HashMap<&str, &Path> = files
        .iter()
        .map(|f| (f.rel.as_str(), f.path.as_path()))
        .collect();
    let metas: Vec<&FileAssignment> = plan
        .files
        .values()
        .filter(|fa| fa.family == "unity-meta")
        .collect();
    if metas.is_empty() {
        return UnityGuidMap::default();
    }
    pr.phase("unity-guids", metas.len() as u64, 0);

    #[allow(clippy::type_complexity)]
    let parts: Vec<(
        Option<(String, String)>,
        Option<(String, String)>,
        Option<String>,
    )> = crate::pool::install(|| {
        metas
            .par_iter()
            .map(|fa| {
                let _in_flight = pr.file(&fa.rel, 0);
                let Some(asset_rel) = fa.rel.strip_suffix(".meta") else {
                    return (None, None, None);
                };
                let Some(path) = by_rel.get(fa.rel.as_str()) else {
                    return (None, None, None);
                };
                let mut guid: Option<String> = None;
                // The Result is NOT discarded: an unreadable `.meta` is
                // how the headline "which scenes use this script?" query
                // silently returns nothing.
                match extract::unity::extract_meta(path, fa.gzip, &mut |rec| {
                    guid = rec
                        .fields
                        .get("guid")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    false
                }) {
                    Ok(_) => match guid {
                        Some(g) => (Some((g, asset_rel.to_string())), None, None),
                        None => (None, None, Some(fa.rel.clone())),
                    },
                    Err(e) => (None, Some((fa.rel.clone(), e.to_string())), None),
                }
            })
            .collect()
    });

    let mut out = UnityGuidMap::default();
    for (hit, bad, missing) in parts {
        if let Some((g, rel)) = hit {
            out.map.insert(g, rel);
        }
        if let Some(b) = bad {
            out.unreadable.push(b);
        }
        if let Some(m) = missing {
            out.no_guid.push(m);
        }
    }
    out.unreadable.sort();
    out.no_guid.sort();
    out
}

/// Say out loud what the guid map could not resolve. Without this a broken
/// `.meta` produces no counter, no warning and no report line, and the
/// feature's headline query returns empty in a way that is indistinguishable
/// from a script nothing references.
fn report_unity_guid_map(g: &UnityGuidMap, pr: &Progress) {
    if !g.unreadable.is_empty() {
        pr.note(&format!(
            "unity: {} .meta sidecar(s) could not be read — scripts they name \
             will have no script_path/script_class: {}",
            g.unreadable.len(),
            g.unreadable
                .iter()
                .take(REFUSAL_LIST_CAP)
                .map(|(rel, e)| format!("{rel} ({e})"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !g.no_guid.is_empty() {
        pr.note(&format!(
            "unity: {} .meta sidecar(s) carry no guid: {}",
            g.no_guid.len(),
            g.no_guid
                .iter()
                .take(REFUSAL_LIST_CAP)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// Stamp pipeline-derived Unity fields onto a record. MonoBehaviour records
/// gain `script_path`/`script_class` when their `script_guid` resolves; meta
/// records gain the root-relative `asset_path` their guid names. Denormalized
/// for one-query answers — `script_guid` remains the authoritative join.
/// Returns the `script_guid` that was present but did NOT resolve, if any.
/// A caller that throws this away recreates the silent-failure bug: the
/// record then ships without `script_path`/`script_class` and the index
/// cannot distinguish "guid is broken" from "nothing references this script".
#[must_use]
fn enrich_unity_fields(
    family: Family,
    fields: &mut Map<String, Value>,
    guid_map: &std::collections::HashMap<String, String>,
    rel: &str,
) -> Option<String> {
    match family {
        Family::UnityYaml => {
            let g = fields.get("script_guid").and_then(Value::as_str)?;
            let Some(p) = guid_map.get(g) else {
                return Some(g.to_string());
            };
            let p = p.clone();
            fields.insert("script_path".into(), Value::String(p.clone()));
            if let Some(stem) = Path::new(&p).file_stem().and_then(|s| s.to_str()) {
                fields.insert("script_class".into(), Value::String(stem.to_string()));
            }
            None
        }
        Family::UnityMeta => {
            if let Some(asset_rel) = rel.strip_suffix(".meta") {
                fields.insert("asset_path".into(), Value::String(asset_rel.to_string()));
            }
            None
        }
        _ => None,
    }
}

fn build_mapping(specs: &[infer::FieldSpec]) -> Value {
    let mut props = Map::new();
    for s in specs {
        let m = match s.es_type.as_str() {
            "date" => json!({"type": "date", "format": "strict_date_optional_time||epoch_millis"}),
            t => json!({"type": t}),
        };
        props.insert(s.name.clone(), m);
    }
    for p in PROVENANCE_FIELDS {
        props.insert((*p).into(), json!({"type": "keyword"}));
    }
    json!({"mappings": {"properties": props}})
}

// ─── the main run ────────────────────────────────────────────────────────

/// Files phase B still has work for: planned, with a real content key, and
/// either never finished or finished against different bytes.
///
/// This predicate is evaluated twice per run — once to price the work for the
/// pre-index estimate, once to build the real queue — against two different
/// snapshots of `journal.done`. The two agree by construction: between the two
/// calls the only thing that removes a key from `done` is `file_replace_start`,
/// which is driven from `content_changed`, and for a key in `content_changed`
/// this predicate ignores `done` altogether. Keeping it in one function is what
/// makes "the estimate priced what the run then did" true rather than hoped.
fn pending_for_phase_b(
    keys: &[String],
    plan: &Plan,
    done: &std::collections::HashSet<String>,
    content_changed: &std::collections::HashSet<String>,
) -> Vec<usize> {
    (0..keys.len())
        .filter(|&i| {
            !keys[i].is_empty()
                && plan.files.contains_key(&keys[i])
                && (!done.contains(&keys[i]) || content_changed.contains(&keys[i]))
        })
        .collect()
}

/// #487: an unchanged re-index on a `neural`/`proxy` backend must not fail with
/// a false "embedding execution identity changed" message. The identity CHANGED
/// only if a comparable field differs from what the journal pinned; `resumable`
/// is a standing property of the backend (false by construction for backends
/// whose identity hashes the model NAME, not its bytes), so it is checked
/// SEPARATELY with an accurate message — restoring the identity cannot fix a
/// backend that simply cannot resume.
fn ensure_embedding_execution_unchanged_and_resumable(
    current: &crate::esclient::EmbeddingExecutionIdentity,
    expected_sha: &str,
    expected_backend: &str,
    expected_dimension: Option<usize>,
    expected_semantic_contract: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        current.identity_sha256 == expected_sha
            && current.backend == expected_backend
            && current.dimensions == expected_dimension
            && current.semantic_contract == expected_semantic_contract,
        "embedding execution identity changed since this autoindex journal was created; \
         refusing to mix vector spaces. No remote mutation was attempted. Restore the \
         original identity, or rebuild with a new --state-dir and a new --prefix"
    );
    anyhow::ensure!(
        current.resumable,
        "the `{}` embedding backend cannot resume semantic indexing{}. The pinned identity is \
         UNCHANGED — this is a backend limitation, not a change, so restoring the identity will \
         not help. Rebuild with a new --state-dir and a new --prefix, or content-address the \
         model identity so this backend can resume (#367).",
        current.backend,
        current
            .non_resumable_reason
            .as_deref()
            .map(|r| format!(": {r}"))
            .unwrap_or_default()
    );
    Ok(())
}

fn select_resume_plan_keys(
    files: &[walk::FileEntry],
    content_keys: &[String],
    plan: &Plan,
    journal_path: &Path,
) -> Result<Vec<Option<String>>> {
    let mut planned_by_rel: HashMap<&str, &str> = HashMap::new();
    let mut planned_by_path_id: HashMap<&str, &str> = HashMap::new();
    for (key, assignment) in &plan.files {
        if let Some(previous) = planned_by_rel.insert(&assignment.rel, key) {
            anyhow::bail!(
                "resume plan assigns path {} to both {} and {}; refusing to discard history under \
                 the same destination. For an isolated rebuild use a new --state-dir, new \
                 --prefix, and new --brain when graph detection is enabled (or --no-graph); \
                 explicitly validate and clean the shared catalog and old target",
                assignment.rel,
                previous,
                key
            );
        }
        if !assignment.path_id.is_empty() {
            if let Some(previous) = planned_by_path_id.insert(&assignment.path_id, key) {
                anyhow::bail!(
                    "resume plan assigns one native path identity to both {} and {}; refusing to \
                     discard history under the same destination. For an isolated rebuild use a \
                     new --state-dir, new --prefix, and new --brain when graph detection is \
                     enabled (or --no-graph); explicitly validate and clean the shared catalog \
                     and old target",
                    previous,
                    key
                );
            }
        }
    }
    let current_rels: std::collections::HashSet<&str> =
        files.iter().map(|file| file.rel.as_str()).collect();
    let current_path_ids: std::collections::HashSet<&str> =
        files.iter().map(|file| file.rel_id.as_str()).collect();
    let mut claimed = std::collections::HashSet::new();
    let mut selected = Vec::with_capacity(files.len());
    for (file, content_key) in files.iter().zip(content_keys) {
        let exact_path = planned_by_path_id
            .get(file.rel_id.as_str())
            .or_else(|| planned_by_rel.get(file.rel.as_str()))
            .filter(|key| !claimed.contains(**key))
            .map(|key| (*key).to_string());
        let exact_content = plan
            .files
            .contains_key(content_key)
            .then(|| content_key.clone())
            .filter(|key| !claimed.contains(key.as_str()));
        let key = if let Some(key) = exact_path.or(exact_content) {
            Some(key)
        } else {
            // Computing the legacy prefix key is intentionally the final
            // fallback. Normal resumes are O(files), with no 64 KiB read.
            let legacy_key = ids::file_key(&file.path, file.size)?;
            if let Some(assignment) = plan.files.get(&legacy_key) {
                let has_exact_current_owner = current_rels.contains(assignment.rel.as_str())
                    || (!assignment.path_id.is_empty()
                        && current_path_ids.contains(assignment.path_id.as_str()));
                if claimed.contains(legacy_key.as_str()) || has_exact_current_owner {
                    anyhow::bail!(
                        "{} collides with legacy resume key {} already owned by {}. No documents \
                         were changed; remove or move one of these two files out of the corpus \
                         and rerun — every other file keeps its resume state. Refusing to discard \
                         history under the same destination. For an isolated rebuild use a new \
                         --state-dir, new --prefix, and new --brain when graph detection is \
                         enabled (or --no-graph); explicitly validate and clean the shared \
                         catalog and old target. Journal: {}",
                        file.rel,
                        legacy_key,
                        assignment.rel,
                        journal_path.display()
                    );
                }
                Some(legacy_key)
            } else if claimed.contains(content_key.as_str()) {
                // Another current file already owns this planned key. Ownership
                // must stay exclusive — two owners would each run the
                // replacement transaction on one ax_file key and delete each
                // other's freshly published documents. Divert this file to a
                // deterministic path-derived key, the same discriminator scheme
                // content::resolve_reporting uses for byte-proven digest
                // collisions.
                Some(format!(
                    "{content_key}-claimed-{:032x}",
                    xxhash_rust::xxh3::xxh3_128(file.rel_id.as_bytes())
                ))
            } else {
                None
            }
        };
        if let Some(key) = &key {
            claimed.insert(key.clone());
        }
        selected.push(key);
    }
    Ok(selected)
}

/// `--fresh` was asked to discard a durable corpus *generation*.
///
/// The generated `--no-graph` path keeps its authority in a committed
/// manifest plus sealed snapshots under the state directory. `--fresh` deletes
/// `journal.ndjson`, and the `gc_snapshots` call that follows every open then
/// sees an empty protected set and removes every sealed snapshot directory —
/// so by the time any later gate could object, the manifest, the pending
/// replay evidence, and the alias/path/stale-record knowledge needed to
/// reconcile the destination are already gone. Refuse before anything is
/// touched.
///
/// This carries no inventory delta on purpose: nothing has been compared yet,
/// and printing empty `added`/`vanished` arrays (as an earlier revision did)
/// is worse than saying plainly which durable state is in the way.
#[derive(Debug)]
struct UnsafeFreshGenerationError {
    /// Committed generation number, or `None` when the blocker is an
    /// uncommitted pending generation.
    committed_generation: Option<u64>,
}

impl UnsafeFreshGenerationError {
    fn to_json(&self) -> Value {
        json!({
            "schema": "xerj.autoindex.unsafe_fresh_generation.v1",
            "status": "error",
            "error": "unsafe_fresh_existing_generation",
            "message": "this attempt made no remote mutations; --fresh cannot discard a durable corpus generation under the same destination because the committed manifest, sealed replay evidence, and alias, path and stale-record cleanup knowledge would be lost",
            "blocking_state": match self.committed_generation {
                Some(generation) => json!({"kind": "committed_generation", "generation": generation}),
                None => json!({"kind": "pending_generation"}),
            },
            "recovery": {
                "resume": "run the same command WITHOUT --fresh: a generated --no-graph journal reconciles additions, changes, deletions and renames incrementally, and replays a pending generation",
                "exact_rebuild": "index with a new --state-dir and a new --prefix. Validate the isolated target before switching readers",
                "warning": "--fresh is not recovery or destination reconciliation. The global autoindex-catalog and old target are not cleaned automatically; validate and clean them explicitly"
            }
        })
    }
}

impl std::fmt::Display for UnsafeFreshGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let blocker = match self.committed_generation {
            Some(generation) => format!("committed corpus generation {generation}"),
            None => "an uncommitted pending corpus generation".to_owned(),
        };
        write!(
            formatter,
            "this attempt made no remote mutations. `--fresh` cannot discard {blocker} under the \
             same destination: the committed manifest, sealed replay evidence, and the alias, \
             path and stale-record cleanup knowledge would be lost, and the existing destination \
             may already be partial or stale. Re-run the same command without `--fresh` — a \
             generated `--no-graph` journal reconciles additions, changes, deletions and renames \
             incrementally. For an exact rebuild, index the current folder with a new \
             --state-dir and a new --prefix, and validate the isolated target before switching \
             readers. `--fresh` is not recovery or destination reconciliation; the global \
             autoindex-catalog and old target require explicit validated cleanup"
        )
    }
}

impl std::error::Error for UnsafeFreshGenerationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InventoryDeltaEntry {
    file_key: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct UnsupportedInventoryDelta {
    added_content_groups: Vec<InventoryDeltaEntry>,
    /// Vanished from the walk AND absent from disk: a genuine user deletion.
    /// Refused, because the published documents stay live with no source file
    /// behind them and nothing here removes them.
    deleted_content_groups: Vec<InventoryDeltaEntry>,
    /// Vanished from the walk but STILL ON DISK: the file was excluded by a
    /// widened ignore/hidden rule, not removed by the user (#439). Its documents
    /// likewise stay live, so the rerun is still refused — but the recovery is
    /// different, and leading an operator (or an agent parsing the JSON) to
    /// "restore the removed file" is wrong when the file is one `ls` away. A
    /// follow-up sweeps these from the destination instead of refusing.
    excluded_content_groups: Vec<InventoryDeltaEntry>,
}

/// Everything the refusal needs to name the destination it is protecting.
/// Collected at the gate so the message can list the exact indices and state
/// directory an operator has to act on, instead of describing them abstractly.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefusalTargets {
    state_dir: String,
    data_indices: Vec<String>,
    edges_index: Option<String>,
}

impl RefusalTargets {
    fn describe(cfg: &IndexCfg, state_dir: &Path, plan: &Plan) -> Self {
        let mut data_indices: Vec<String> = plan.datasets.iter().map(|d| d.index.clone()).collect();
        data_indices.sort();
        data_indices.dedup();
        let edges_index = (!cfg.no_graph).then(|| {
            let brain = cfg
                .brain
                .clone()
                .unwrap_or_else(|| derive_brain_name(&cfg.root));
            detect::edges_index_name(&brain)
        });
        Self {
            state_dir: state_dir.display().to_string(),
            data_indices,
            edges_index,
        }
    }

    fn indices_phrase(&self) -> String {
        if self.data_indices.is_empty() {
            "the indices this plan publishes".to_string()
        } else {
            self.data_indices.join(", ")
        }
    }

    fn edges_note(&self) -> String {
        match &self.edges_index {
            Some(edges) => format!(
                " Graph edges taught by the removed file(s) stay live in {edges} until that \
                 index is deleted too."
            ),
            None => String::new(),
        }
    }
}

/// The #195 zero-live verification failure, as a type rather than a string.
///
/// The journal claims records were made visible; the destination answers with
/// none. That is always a failure, but not always the *same* failure: for a
/// composing caller like `xerj brain` a resume journal that outlived a wiped
/// data directory has a specific, executable recovery (`--fresh` in place),
/// while a write-blocked or unreachable server does not. Only a typed error
/// lets the caller tell them apart — `anyhow::bail!` forces it to either
/// string-match the prose or reprint it verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZeroLiveVerificationError {
    /// Records the resume journal says were published.
    pub journal_records: u64,
    /// Completed files those records came from.
    pub files_done_journaled: usize,
    /// Dataset indices that answered with zero live documents.
    pub dataset_indices: usize,
}

impl std::fmt::Display for ZeroLiveVerificationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "autoindex verification failed: the resume journal records {} \
             record(s) from {} completed file(s), but 0 documents are \
             live across the {} dataset index(es). The indices exist and look healthy but \
             hold nothing — a server-side write rejection (e.g. a disk flood-stage or \
             index write block), deleted indices, or an unreadable server can all cause \
             this. Fix the server-side condition, then rerun with --fresh to re-index \
             from scratch",
            self.journal_records, self.files_done_journaled, self.dataset_indices
        )
    }
}

impl std::error::Error for ZeroLiveVerificationError {}

#[derive(Debug)]
struct UnsupportedInventoryDeltaError {
    delta: UnsupportedInventoryDelta,
    targets: RefusalTargets,
}

impl UnsupportedInventoryDeltaError {
    fn to_json(&self) -> Value {
        let deleted = &self.delta.deleted_content_groups;
        let excluded = &self.delta.excluded_content_groups;
        // Back-compat: `vanished_content_groups` (the v1 field) stays as the
        // union of both causes; `deleted_`/`excluded_content_groups` add the
        // #439 distinction without removing anything a v1 consumer reads.
        // Globally sorted, matching the pre-split v1 field: the two buckets are
        // each sorted, but a mixed run must not expose bucket order to a v1
        // consumer that saw one (path, file_key)-sorted list.
        let mut vanished: Vec<&InventoryDeltaEntry> =
            deleted.iter().chain(excluded.iter()).collect();
        vanished.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.file_key.cmp(&right.file_key))
        });
        let message = if excluded.is_empty() {
            "this attempt made no remote mutations. Files that were indexed under this resume plan no longer exist in the folder, and their documents are still live in the destination; removing files from an indexed folder is not reconciled yet".to_string()
        } else if deleted.is_empty() {
            "this attempt made no remote mutations. Files that were indexed under this resume plan are STILL ON DISK but the walk no longer yields them — an ignore or hidden-file rule widened to exclude them. They were not removed, and their documents are still live in the destination; sweeping an excluded file's documents in place is not reconciled yet".to_string()
        } else {
            "this attempt made no remote mutations. Some files indexed under this resume plan no longer exist in the folder (deleted); others are still on disk but the walk no longer yields them because an ignore or hidden-file rule widened (excluded, not removed). Both sets' documents are still live in the destination and neither deletion nor exclusion is reconciled in place yet".to_string()
        };
        let mut recovery = serde_json::Map::new();
        if !deleted.is_empty() {
            recovery.insert(
                "restore_removed_files".into(),
                json!("for the DELETED file(s) only: put them back and rerun; every other file keeps its resume state. Do NOT apply this to the excluded file(s) — they were never removed"),
            );
        }
        if !excluded.is_empty() {
            recovery.insert(
                "excluded_not_removed".into(),
                json!("the excluded file(s) are still on disk — an ignore/hidden rule now matches them, so the walk stopped yielding them. Restoring them is wrong (they were never removed). To keep them indexed, narrow the rule so the walk re-admits them; to drop them from the index, rebuild isolated (below) — their documents cannot yet be swept in place"),
            );
        }
        recovery.insert(
            "rebuild_in_place".into(),
            json!(format!(
                "delete the indices this plan publishes ({}) and the state directory {}, then rerun. This re-extracts and re-embeds the whole corpus.{}",
                self.targets.indices_phrase(),
                self.targets.state_dir,
                self.targets.edges_note().trim_start_matches(' ')
            )),
        );
        recovery.insert(
            "rebuild_isolated".into(),
            json!("index with a new --state-dir, new --prefix, and (when graph detection is enabled) new --brain; alternatively add --no-graph. Validate the isolated target before switching readers, then clean the old one"),
        );
        recovery.insert(
            "fresh_warning".into(),
            json!("--fresh re-extracts the current folder in place and does pick up added and changed files, but it never deletes documents already published for removed or excluded files, so it is refused here"),
        );
        json!({
            "schema": "xerj.autoindex.unsupported_sync_delta.v1",
            "status": "error",
            "error": "unsupported_content_group_removal",
            "message": message,
            "vanished_content_groups": vanished,
            "deleted_content_groups": deleted,
            "excluded_content_groups": excluded,
            // Context, not the reason for the refusal: a rerun over a frozen
            // plan does not index files added after the plan was frozen.
            "added_content_groups": self.delta.added_content_groups,
            "recovery": Value::Object(recovery)
        })
    }
}

impl std::fmt::Display for UnsupportedInventoryDeltaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Capped like the duplicate and unplanned-file listings above; see
        // REFUSAL_LIST_CAP. The machine-readable `--json` rendering keeps the
        // full lists, so nothing is lost — only the prose is bounded.
        let render = |entries: &[InventoryDeltaEntry]| {
            let mut rendered = entries
                .iter()
                .take(REFUSAL_LIST_CAP)
                .map(|entry| format!("{} ({})", entry.path, entry.file_key))
                .collect::<Vec<_>>()
                .join(", ");
            let remaining = entries.len().saturating_sub(REFUSAL_LIST_CAP);
            if remaining > 0 {
                rendered.push_str(&format!(", … and {remaining} more"));
            }
            rendered
        };
        let deleted = &self.delta.deleted_content_groups;
        let excluded = &self.delta.excluded_content_groups;
        write!(
            formatter,
            "this attempt made no remote mutations — no documents, aliases, graph edges or \
             catalog entries were written."
        )?;
        if !deleted.is_empty() {
            write!(
                formatter,
                " {} file(s) indexed under this resume plan no longer exist in the folder, and \
                 their documents are still live in the destination; removing files from an \
                 indexed folder is not reconciled yet. Deleted content groups [{}].",
                deleted.len(),
                render(deleted)
            )?;
        }
        if !excluded.is_empty() {
            // #439: still on disk, so "restore the removed file" is wrong.
            write!(
                formatter,
                " {} file(s) indexed under this resume plan are still on disk but the walk no \
                 longer yields them — an ignore or hidden-file rule widened to exclude them, so \
                 they were not removed. Their documents are still live in the destination; \
                 sweeping an excluded file's documents in place is not reconciled yet. Excluded \
                 content groups [{}].",
                excluded.len(),
                render(excluded)
            )?;
        }
        if !self.delta.added_content_groups.is_empty() {
            write!(
                formatter,
                " Also present but not in the frozen resume plan, so not indexed by this run \
                 either [{}].",
                render(&self.delta.added_content_groups)
            )?;
        }
        write!(formatter, " Recovery, cheapest first:")?;
        if !deleted.is_empty() {
            write!(
                formatter,
                " restore the DELETED file(s) and rerun — every other file keeps its resume \
                 state (this does not apply to the excluded file(s), which were never removed);"
            )?;
        }
        if !excluded.is_empty() {
            write!(
                formatter,
                " for the EXCLUDED file(s), narrow the ignore/hidden rule so the walk re-admits \
                 them (keeps them indexed), or rebuild isolated to drop them — their documents \
                 cannot yet be swept in place;"
            )?;
        }
        write!(
            formatter,
            " rebuild in place by deleting the indices this plan publishes ({}) and the state \
             directory {}, then rerunning — this re-extracts and re-embeds the whole corpus.{} \
             rebuild isolated with a new --state-dir, a new --prefix and, when graph detection \
             is enabled, a new --brain (or --no-graph), validate it, switch readers, then clean \
             the old target. `--fresh` picks up added and changed files in place but never \
             deletes documents for removed or excluded files, so it is refused here too",
            self.targets.indices_phrase(),
            self.targets.state_dir,
            self.targets.edges_note()
        )
    }
}

impl std::error::Error for UnsupportedInventoryDeltaError {}

/// Whether a plan file that vanished from the walk is still on disk (#439).
///
/// The disk probe uses the file's reversible raw-bytes identity (`path_id`:
/// `unix:<hex>` / `windows:<hex>`), NOT its `rel`. `rel` is a `to_string_lossy`
/// rendering, so a hidden non-UTF-8 name — the exact case #439 was filed for —
/// is stored with `U+FFFD` and no dirent matches it; probing through `rel` would
/// call every such file a deletion. `path_id` round-trips the real bytes. Legacy
/// plans that predate `path_id` fall back to `rel`. An unreadable path counts as
/// present, so a stat failure never produces a "restore the removed file"
/// instruction for a file that is merely inaccessible.
fn vanished_is_on_disk(root: &Path, assignment: &FileAssignment) -> bool {
    let rel = decode_stable_path_id(&assignment.path_id)
        .unwrap_or_else(|| std::path::PathBuf::from(&assignment.rel));
    root.join(rel).try_exists().unwrap_or(true)
}

/// Reverse of `walk::stable_path_id`: reconstruct the real relative path from its
/// `unix:<hex>` / `windows:<hex>` identity, preserving bytes a UTF-8 `rel` loses.
/// Returns `None` for an empty or unrecognised id so the caller falls back.
fn decode_stable_path_id(path_id: &str) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        if let Some(hex) = path_id.strip_prefix("unix:") {
            use std::os::unix::ffi::OsStrExt;
            if hex.len() % 2 != 0 {
                return None;
            }
            // `hex.get` (not `hex[..]`) so a non-ASCII byte in a corrupted or
            // hand-edited path_id returns None instead of panicking on a
            // non-char-boundary slice — the doc contract above.
            let bytes: Option<Vec<u8>> = (0..hex.len())
                .step_by(2)
                .map(|i| {
                    hex.get(i..i + 2)
                        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                })
                .collect();
            return Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
                &bytes?,
            )));
        }
    }
    #[cfg(windows)]
    {
        if let Some(hex) = path_id.strip_prefix("windows:") {
            use std::os::windows::ffi::OsStringExt;
            if hex.len() % 4 != 0 {
                return None;
            }
            let units: Option<Vec<u16>> = (0..hex.len())
                .step_by(4)
                .map(|i| {
                    hex.get(i..i + 4)
                        .and_then(|quad| u16::from_str_radix(quad, 16).ok())
                })
                .collect();
            return Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(
                &units?,
            )));
        }
    }
    let _ = path_id;
    None
}

impl UnsupportedInventoryDelta {
    fn between(root: &Path, files: &[walk::FileEntry], keys: &[String], plan: &Plan) -> Self {
        let current_keys: std::collections::HashSet<&str> = keys
            .iter()
            .filter(|key| !key.is_empty())
            .map(String::as_str)
            .collect();
        let durable_keys: std::collections::HashSet<&str> = plan
            .files
            .keys()
            .map(String::as_str)
            .chain(plan.junk_files.iter().map(|junk| junk.file_key.as_str()))
            .collect();
        // Path identity, not just content identity. An in-place EDIT gives the
        // same file a new content key, so a key-only comparison reads the
        // superseded key as vanished and the new one as added — the SAME path
        // listed as both removed and added. That is a replacement, and the file
        // is still there to be republished, so it must never be refused.
        // Ordinary resume already reaches this conclusion by mapping each file
        // onto its planned key (`select_resume_plan_keys`); doing it here makes
        // `--fresh`, which has no plan to map through, agree — without making
        // the gate more fatal than the open it precedes.
        let current_rels: std::collections::HashSet<&str> =
            files.iter().map(|file| file.rel.as_str()).collect();
        let current_path_ids: std::collections::HashSet<&str> = files
            .iter()
            .map(|file| file.rel_id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        let path_survives = |assignment: &FileAssignment| {
            current_rels.contains(assignment.rel.as_str())
                || (!assignment.path_id.is_empty()
                    && current_path_ids.contains(assignment.path_id.as_str()))
        };

        let mut added_content_groups: Vec<InventoryDeltaEntry> = files
            .iter()
            .zip(keys)
            .filter(|(_, key)| !key.is_empty() && !durable_keys.contains(key.as_str()))
            .map(|(file, key)| InventoryDeltaEntry {
                file_key: key.clone(),
                path: file.rel.clone(),
            })
            .collect();
        // #439: a plan file absent from the walk is a *deletion* only if it is
        // also absent from disk. If it is still on disk, the walk stopped
        // yielding it because an ignore/hidden rule widened — an exclusion, not
        // a removal — and the accurate recovery is different. `vanished_is_on_disk`
        // checks through the reversible raw-bytes identity, so a hidden NON-UTF-8
        // name (the case #439 was filed for) is matched on disk rather than read
        // as a phantom removal via its lossy `rel`.
        let mut deleted_content_groups: Vec<InventoryDeltaEntry> = Vec::new();
        let mut excluded_content_groups: Vec<InventoryDeltaEntry> = Vec::new();
        for (key, assignment) in &plan.files {
            if current_keys.contains(key.as_str()) || path_survives(assignment) {
                continue;
            }
            let entry = InventoryDeltaEntry {
                file_key: key.clone(),
                path: assignment.rel.clone(),
            };
            if vanished_is_on_disk(root, assignment) {
                excluded_content_groups.push(entry);
            } else {
                deleted_content_groups.push(entry);
            }
        }
        // Deliberately NOT extended with `plan.junk_files`. A junk/skipped file
        // published no documents, no aliases and no graph edges — its entire
        // live footprint is one `file:{key}` catalog row, and the stale
        // junk-catalog sweep below (#238) deletes that row before dropping the
        // plan entry. Removing it therefore strands nothing, so refusing the
        // rerun would block a case the pipeline now handles completely. Junk
        // keys stay in `durable_keys` above: an unchanged skipped file is still
        // not an addition.

        let stable_order = |left: &InventoryDeltaEntry, right: &InventoryDeltaEntry| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.file_key.cmp(&right.file_key))
        };
        added_content_groups.sort_by(stable_order);
        excluded_content_groups.sort_by(stable_order);
        deleted_content_groups.sort_by(stable_order);
        Self {
            added_content_groups,
            deleted_content_groups,
            excluded_content_groups,
        }
    }

    /// A rerun is refused only when a content group the plan published has
    /// vanished from the folder: those documents stay live and searchable with
    /// no source file behind them, and nothing in this pipeline removes them.
    /// Additions are not refused — they are skipped by the frozen plan exactly
    /// as before and `--fresh` rebuilds the plan in place to include them.
    fn refuses(&self) -> bool {
        // #589: only a GENUINE deletion (source file gone from disk) refuses the
        // rerun — its published documents would be left live with no file behind
        // them and nothing else removes them. An EXCLUSION (file still on disk,
        // newly matched by a widened ignore/hidden/`.xerjignore` rule) is data
        // the exclusion must REMOVE, so it is SWEPT by `sweep_excluded_groups`
        // and the run continues (#439's data-exposure headline). When a genuine
        // deletion co-occurs the whole rerun is refused, and `into_error` still
        // reports the excluded set too because it also stays live in that case.
        !self.deleted_content_groups.is_empty()
    }

    fn into_error(self, targets: RefusalTargets) -> anyhow::Error {
        UnsupportedInventoryDeltaError {
            delta: self,
            targets,
        }
        .into()
    }
}

fn alias_keys_to_reindex(
    previous: &[state::DuplicateFile],
    current: &[state::DuplicateFile],
    migration_keys: Option<&[String]>,
) -> std::collections::HashSet<String> {
    let paths_by_key = |aliases: &[state::DuplicateFile]| {
        let mut by_key: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
        for alias in aliases {
            by_key
                .entry(alias.file_key.clone())
                .or_default()
                .insert(alias.rel.clone());
        }
        by_key
    };
    let previous = paths_by_key(previous);
    let current = paths_by_key(current);
    let mut changed = std::collections::HashSet::new();
    if let Some(keys) = migration_keys {
        changed.extend(keys.iter().cloned());
    }
    for key in previous.keys().chain(current.keys()) {
        if previous.get(key) != current.get(key) {
            changed.insert(key.clone());
        }
    }
    changed
}

/// Default second-brain name for a corpus root: `sanitize_slug(basename)`
/// (SECOND_BRAIN_SPEC §6.1), falling back to `"brain"` when the basename
/// sanitizes to nothing (e.g. `/`). Public because `xerj brain` must know
/// the SAME name this pipeline will use — the console URL it prints and
/// opens embeds it — and two copies of this rule would drift.
pub fn derive_brain_name(root: &Path) -> String {
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let name = base
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = dataset::sanitize_slug(&name);
    if slug.is_empty() {
        "brain".into()
    } else {
        slug
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DurableDatasetStats {
    bytes: u64,
    junk: u64,
    dropped: u64,
}

/// Source metadata durably represented by each dataset.
///
/// `FileDone` is the commit record for a successfully published canonical
/// source. A source can feed more than one inferred dataset (for example, a
/// workbook or database with multiple tables), so each distinct assigned
/// dataset reports the complete source bytes it depends on. Repeated groups
/// within the same dataset count the source only once. Parser-level junk has
/// no group after extraction rejects it, so it retains the historical
/// attribution to the file's first assigned dataset. Coercion drops retain
/// their exact dataset captured in the completion record.
fn durable_dataset_stats(
    plan: &Plan,
    done: &HashMap<String, FileDone>,
) -> HashMap<String, DurableDatasetStats> {
    let mut stats_by_dataset: HashMap<String, DurableDatasetStats> = HashMap::new();
    for (file_key, completed) in done {
        let Some(assignment) = plan.files.get(file_key) else {
            continue;
        };
        let assigned_datasets: std::collections::HashSet<&str> = assignment
            .assignments
            .iter()
            .map(|(_, slug)| slug.as_str())
            .collect();
        for slug in assigned_datasets {
            let stats = stats_by_dataset.entry(slug.to_string()).or_default();
            stats.bytes = stats.bytes.saturating_add(completed.bytes);
        }
        if let Some((_, slug)) = assignment.assignments.first() {
            let stats = stats_by_dataset.entry(slug.clone()).or_default();
            stats.junk = stats.junk.saturating_add(completed.junk);
        }
        for (slug, dropped) in &completed.dropped_by_dataset {
            if assignment
                .assignments
                .iter()
                .any(|(_, assigned)| assigned == slug)
            {
                let stats = stats_by_dataset.entry(slug.clone()).or_default();
                stats.dropped = stats.dropped.saturating_add(*dropped);
            }
        }
    }
    stats_by_dataset
}

fn invocation_report_timestamps(
    started: chrono::DateTime<chrono::Utc>,
    summary_generated_at: chrono::DateTime<chrono::Utc>,
) -> (String, String) {
    (started.to_rfc3339(), summary_generated_at.to_rfc3339())
}

fn pin_pending_embedding_identity(
    es: &Es,
    journal: &mut state::Journal,
    pending: &sync::PendingSync,
) -> Result<()> {
    if !pending
        .desired
        .plan
        .datasets
        .iter()
        .any(|dataset| dataset.semantic_field.is_some())
    {
        return Ok(());
    }
    let expected = pending
        .desired
        .execution
        .as_ref()
        .context("pending semantic generation has no execution identity")?;
    let current = es.embedding_execution_identity().context(
        "pending semantic generation cannot verify the server embedding execution identity",
    )?;
    anyhow::ensure!(
        current.resumable,
        "pending semantic generation cannot resume because the current embedding backend is not \
         resumable: {}; restore the original backend, or rebuild with a new --state-dir and a new \
         --prefix",
        current
            .non_resumable_reason
            .as_deref()
            .unwrap_or("the server did not provide a stable execution identity")
    );
    anyhow::ensure!(
        current.identity_sha256 == expected.embedding_identity_sha256
            && current.backend == expected.embedding_backend
            && current.dimensions == expected.embedding_dimension
            && current.semantic_contract == expected.embedding_semantic_contract
            && current.resumable == expected.embedding_resumable,
        "pending semantic generation was prepared for a different embedding execution identity; \
         no remote mutation was attempted. Restore the original embedding backend, or rebuild with \
         a new --state-dir and a new --prefix. --fresh cannot discard this pending generation"
    );
    journal.pin_embedding_identity(
        &current.identity_sha256,
        current.resumable,
        current.non_resumable_reason.as_deref(),
    )
}

fn finish_generated_run(es: &Es, journal: &mut state::Journal, cfg: &IndexCfg) -> Result<Value> {
    let committed = journal
        .committed_manifest
        .as_ref()
        .context("generated run finished without committed generation authority")?;
    let execution = committed
        .execution
        .as_ref()
        .context("generated run finished without execution identity")?;
    let generation = committed.generation;
    let dataset_count = committed.plan.datasets.len();
    let sync::SourceExecutionPolicy::DurableSnapshot { reference, .. } = &execution.source_policy
    else {
        anyhow::bail!("generated run does not reference a durable snapshot");
    };
    let run_id = reference
        .strip_prefix("sync-snapshots/")
        .context("generated run snapshot reference is not state-relative")?
        .to_owned();
    let response = es.search(
        catalog::CATALOG_INDEX,
        &json!({
            "size": 2,
            "query": {"bool": {"filter": [
                {"term": {"run_id": &run_id}},
                {"term": {"doc_kind": "run"}}
            ]}}
        }),
    )?;
    let hits = response
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .context("generated run summary query has no hits")?;
    anyhow::ensure!(
        hits.len() == 1,
        "generated run summary query returned {} documents; expected exactly one",
        hits.len()
    );
    let summary = hits[0]
        .get("_source")
        .cloned()
        .context("generated run summary hit has no _source")?;
    anyhow::ensure!(
        summary.get("generation").and_then(Value::as_u64) == Some(generation),
        "generated run summary generation disagrees with committed authority"
    );
    journal.finish(&summary)?;
    if cfg.json {
        println!("{summary}");
    } else if !cfg.quiet {
        println!(
            "generation {} committed — {} datasets, {} records live",
            generation,
            dataset_count,
            summary
                .get("records_total")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        );
    }
    Ok(summary)
}

/// #589: sweep the documents an exclusion left behind. A file still on disk
/// that a widened ignore/hidden/`.xerjignore` rule now skips must have its
/// already-published documents removed — an exclusion that cannot remove data
/// already in the index is the #439 data-exposure hole. Deletes the indexed
/// records (by `ax_file`) across the corpus indices and the file's catalog
/// document(s) (by logical `path`).
///
/// The caller sweeps only when there is NO genuine deletion in the same delta
/// (a co-occurring deletion refuses the whole rerun, mutating nothing) and
/// never under `--dry-run`.
///
/// KNOWN GAP (#589, next hunk): graph edges taught by the file remain in the
/// edges index until it is swept too — the same limitation the refusal message
/// already documents in `edges_note`.
fn sweep_excluded_groups(es: &Es, plan: &Plan, excluded: &[InventoryDeltaEntry]) -> Result<()> {
    // Exact dataset indices from the plan — the same resolution the replacement
    // delete path uses (`ds_rt` → `rt.index`); a `{prefix}-*` wildcard would be
    // untestable and could reach indices this corpus does not own.
    let mut indices: Vec<&str> = plan.datasets.iter().map(|d| d.index.as_str()).collect();
    indices.sort_unstable();
    indices.dedup();
    for entry in excluded {
        for index in &indices {
            es.delete_by_query(index, &json!({"term": {"ax_file": entry.file_key}}))
                .with_context(|| {
                    format!(
                        "sweep indexed records for newly-excluded {} in {index}",
                        entry.path
                    )
                })?;
        }
        es.delete_by_query(
            catalog::CATALOG_INDEX,
            &json!({"term": {"path": entry.path}}),
        )
        .with_context(|| format!("sweep catalog entry for newly-excluded {}", entry.path))?;
    }
    Ok(())
}

/// Catalog doc id for a time correlation, corpus-prefix-scoped (#673): the
/// `autoindex-catalog` index is shared across corpora, so a time correlation
/// keyed only by dataset slugs would overwrite the same-slug correlation from
/// another corpus. Extracted (#689) so the prefix scoping is unit-testable, the
/// way `KeyCorr::id` is for `corr:`.
fn tcorr_id(prefix: &str, a_dataset: &str, b_dataset: &str) -> String {
    format!("tcorr:{prefix}:{a_dataset}:{b_dataset}")
}

#[cfg(test)]
mod tcorr_id_tests {
    use super::tcorr_id;

    /// #689 (coverage gap from #673): the shared `autoindex-catalog` index means
    /// a time correlation keyed by dataset slugs must carry the corpus prefix, or
    /// `(reports, orders)` in corpus A overwrites the same-slug correlation in
    /// corpus B. `corr:` is pinned by `correlate::tests`; this pins `tcorr:`.
    #[test]
    fn tcorr_id_is_prefix_scoped_across_corpora() {
        // Same slugs, different corpus prefix -> distinct ids (no overwrite).
        assert_ne!(
            tcorr_id("ax-a", "reports", "orders"),
            tcorr_id("ax-b", "reports", "orders"),
            "#689: same-slug time correlations from two corpora must not collide"
        );
        // Idempotent within one corpus.
        assert_eq!(
            tcorr_id("ax-a", "reports", "orders"),
            tcorr_id("ax-a", "reports", "orders"),
        );
        // The prefix is actually in the id.
        assert!(tcorr_id("ax-a", "reports", "orders").starts_with("tcorr:ax-a:"));
    }
}

fn run_index(cfg: IndexCfg) -> Result<i32> {
    run_index_report(cfg).map(|(code, _)| code)
}

/// `run_index` plus the machine-readable run summary — the same JSON the
/// run writes to the catalog as `run:{run_id}` (datasets, `records_total`
/// as *live* per-dataset counts, `graph.edges_written` etc.). `xerj brain`
/// composes autoindex through this so it can be honest about what actually
/// got indexed without re-querying or parsing stdout. The summary is
/// `None` when the run ended before a plan produced one (empty folder,
/// `--dry-run`).
pub fn run_index_report(cfg: IndexCfg) -> Result<(i32, Option<Value>)> {
    // The very first statement of the function, deliberately: `started` must
    // be when this invocation began, not when its summary was built.
    let invocation_started = chrono::Utc::now();
    // Fix the phase-A pool width BEFORE anything parallel starts: hashing and
    // sniffing are the CPU-bound phase, and they used to take every core no
    // matter what the caller asked for (#240 §2).
    pool::configure(cfg.scan_workers);
    // What phase A is *actually* running on. `pool::configure` is first-call-wins
    // (rayon pools cannot be resized), so in a process that already indexed once
    // — `xerj-server`'s brain endpoint does exactly that — the installed width
    // can differ from what this run's plan asked for. Progress must state the
    // number the policy got, not the one it requested (#240 + #241).
    let scan_threads = pool::scan_pool().current_num_threads();
    extract::pdf::configure_workers(cfg.pdf_workers);
    extract::pdf::configure_timeout(cfg.pdf_timeout_secs);
    let t0 = Instant::now();
    // The progress surface and its ticker are the FIRST things built: every
    // later phase reports through them, and the ticker guarantees the stream
    // closes with a terminal line even if this function bails (#241). The
    // resource plan is announced through the same surface rather than through a
    // bare `eprintln!`, so `--quiet` / `--progress none` still means silent and
    // `--progress json` still means one machine-readable stream (#240 + #241).
    let surface = progress::detect(cfg.progress);
    let pr = Progress::new(
        surface,
        cfg.progress_interval
            .unwrap_or_else(|| progress::default_interval(surface)),
    );
    let ticker = pr.spawn_ticker();
    // One throughput meter for the whole run: both phase-A routes (the legacy
    // scan and the generated route's `project_reconcile_plan`) feed it, so the
    // estimate is built from whatever this invocation actually parsed.
    let scan_meter = estimate::Meter::new();
    // What this run decided to take from the machine, before it takes it.
    pr.note(&format!(
        "autoindex: {} scan threads, {} index workers, {} pdf workers, --bulk-mb {} [{}]",
        scan_threads,
        cfg.workers,
        cfg.pdf_workers,
        cfg.bulk_mb,
        xerj_common::resource::describe(),
    ));
    for note in &cfg.resource_notes {
        pr.note(&format!("autoindex: {note}"));
    }
    pr.note(&format!(
        "autoindex: bulk HTTP request timeout: {}s",
        cfg.bulk_timeout_secs
    ));
    // The run's bulk load is admitted through one window `--workers` wide, so
    // a 429 can shrink what the run offers instead of only delaying it
    // (#240 §8). Enabled here and nowhere else: probes have nothing to
    // throttle. Its shrink/recover announcements go to stderr, which the
    // progress surface owns — so they are emitted exactly when that surface is
    // enabled, not merely when `--quiet` is absent.
    let es = Es::with_bulk_timeout(&cfg.url, cfg.api_key.clone(), cfg.bulk_timeout_secs)?
        .with_bulk_concurrency(cfg.workers, pr.enabled());
    es.ping()?;

    let stub_matcher = StubMatcher::compile(&cfg.stub_globs)?;
    let root_str = cfg
        .root
        .canonicalize()
        .unwrap_or_else(|_| cfg.root.clone())
        .to_string_lossy()
        .to_string();
    let state_dir = cfg
        .state_dir
        .clone()
        .unwrap_or_else(|| state::default_state_dir(&root_str, &cfg.url, &cfg.prefix));
    // Acquire state authority before discovery as well as hashing. A waiter
    // must never classify a path snapshot taken while another owner was
    // publishing or replacing the durable plan.
    let preflight =
        state::Journal::preflight(&state_dir, &root_str, &cfg.url, &cfg.prefix, cfg.fresh)?;
    let genesis_recovery = preflight
        .committed_manifest
        .as_ref()
        .is_some_and(|manifest| manifest.generation == 0 && manifest.groups.is_empty());
    // `--fresh` must be refused before the journal is opened, and only for a
    // durable *generation*. `open_after_preflight` deletes journal.ndjson
    // whenever `fresh` is set, and the `gc_snapshots` call that follows every
    // open then sees an empty protected set and removes every sealed snapshot
    // directory — so nothing evaluated later can save a generated corpus.
    //
    // Scope matters here. A legacy (non-generated) journal keeps `--fresh`
    // exactly as it has always behaved: it is a crash-resume boundary, not a
    // generation, and `xerj brain` documents and depends on `--fresh` to
    // re-index a folder whose server-side data was wiped (brain.rs). Only
    // generated state — a pending sync, or a committed manifest past genesis —
    // has authority that `--fresh` would silently destroy.
    let blocking_generation = if !cfg.fresh {
        None
    } else if preflight.pending_sync.is_some() {
        Some(None)
    } else {
        preflight
            .committed_manifest
            .as_ref()
            .filter(|_| !genesis_recovery)
            .map(|manifest| Some(manifest.generation))
    };
    if let Some(committed_generation) = blocking_generation {
        return Err(UnsafeFreshGenerationError {
            committed_generation,
        }
        .into());
    }
    if let Some(reason) = preflight.unreadable_plan.as_deref() {
        // Only reachable under --fresh; without it the preflight would have
        // returned this as an error. It goes through the progress surface, not
        // a raw eprintln!, so `--progress none` (which `--quiet` selects) stays
        // silent and `--progress json` stays one parseable stream (#241).
        pr.note(&format!(
            "autoindex: --fresh: the durable resume plan in {} could not be read ({reason}); \
             rebuilding it from the current folder. Documents published for files that are no \
             longer present cannot be identified from an unreadable plan and are not deleted.",
            state_dir.join("journal.ndjson").display()
        ));
    }
    // `--dry-run` has to be decided *before* the generated branches, not after
    // them. Both of those branches publish and commit, and both return before
    // control ever reaches the legacy `cfg.dry_run` check further down — so on
    // any already-generated state directory the flag was accepted, ignored, and
    // the destination mutated anyway. A projection-only flag that writes is
    // worse than one that errors, so this branch is placed where nothing has
    // been opened for write yet: the journal is still only preflighted, and
    // `gc_snapshots` (which deletes unprotected snapshot directories) has not
    // run.
    if cfg.dry_run {
        if let Some(pending) = &preflight.pending_sync {
            println!("{}", serde_json::to_string_pretty(&pending.desired.plan)?);
            // stdout is the RESULT, stderr is PROGRESS — so this explanation
            // goes through the progress surface, never a bare `eprintln!`
            // that `--progress none` could not silence and `--progress json`
            // could not parse (#241).
            pr.note(&format!(
                "(dry run — nothing indexed; corpus generation {} is already sealed and pending \
                 replay from its own durable snapshot. Re-run without --dry-run to finish it.)",
                pending.desired.generation
            ));
            pr.finish(true, 0, "dry-run", &[]);
            return Ok((0, None));
        }
    }
    // A durable sync_begin owns the desired generation. Never rediscover and
    // replan from a mutable source tree while that transaction is pending.
    // Operation handlers are deliberately not enabled by this foundation
    // slice; fail with the exact durable transaction rather than accidentally
    // executing a different folder snapshot.
    if preflight.pending_sync.is_some() {
        let mut journal = state::Journal::open_after_preflight(
            preflight,
            &root_str,
            &cfg.url,
            &cfg.prefix,
            cfg.bulk_timeout_secs,
            cfg.fresh,
        )?;
        sync_executor::gc_snapshots(&state_dir, &journal)?;
        let pending = journal
            .pending_sync
            .as_ref()
            .context("preflight reported a pending generation but authoritative replay did not")?
            .clone();
        pin_pending_embedding_identity(&es, &mut journal, &pending)?;
        pr.phase("replay", 0, 0);
        let mut backend = sync_executor::EsSyncBackend::new(&es, &state_dir, cfg.bulk_mb << 20);
        sync_executor::replay_pending_operations(&state_dir, &mut journal, &mut backend)?;
        // Through the progress surface, never a bare `eprintln!`: stderr
        // belongs to that surface, so `--progress none` stays silent and
        // `--progress json` stays one parseable stream (#241).
        pr.note("autoindex: resumed and committed pending corpus generation from durable source");
        let summary = finish_generated_run(&es, &mut journal, &cfg)?;
        let code = generated_exit_code(&summary);
        finish_generated_progress(&pr, code, &summary);
        return Ok((code, Some(summary)));
    }
    // Totals are unknown until the walk returns, so this phase honestly
    // reports `pct=unknown` and proves liveness with the clock alone.
    pr.phase("walk", 0, 0);
    let (discovered_files, ignore_report) = walk::walk_reporting_opts(
        &cfg.root,
        cfg.follow_symlinks,
        cfg.follow_symlinks_outside_root,
        cfg.ignore,
    )?;
    let discovered_bytes: u64 = discovered_files.iter().map(|f| f.size).sum();
    pr.note(&format!(
        "autoindex: {} files ({} MB) under {}",
        discovered_files.len(),
        discovered_bytes / (1 << 20),
        root_str
    ));
    // "Where did my files go?" is answered here, on every run — the rules are
    // named, not just the totals (#276).
    for line in ignore_report.summary_lines() {
        pr.note(&format!("autoindex: {line}"));
    }
    // An empty folder is only "nothing to do" when there is also no durable
    // state: with a journal present, zero files is a deletion of the whole
    // corpus and has to be reconciled, not shrugged off.
    if discovered_files.is_empty() && !preflight.journal_exists {
        println!("no files found under {}", cfg.root.display());
        pr.finish(true, 0, "no-files", &[]);
        return Ok((0, None));
    }
    // Full hashing on every run is deliberate: size/mtime/inode fingerprints
    // cannot prove byte identity across all supported local and network
    // filesystems. A metadata-only shortcut could leave stale live documents
    // forever after a same-size rewrite with restored or stale timestamps.
    // Hashing reads every byte of the corpus. On a large tree it is minutes of
    // real work, and before #241 it was minutes with no output at all.
    pr.phase("hash", discovered_files.len() as u64, discovered_bytes);
    let mut inventory = content::resolve_reporting(discovered_files, &|bytes| pr.item_done(bytes))?;
    if cfg.no_graph && preflight.committed_manifest.is_some() && !genesis_recovery {
        if cfg.dry_run {
            let base = preflight
                .committed_manifest
                .as_ref()
                .context("branch guard proved a committed manifest")?;
            let plan =
                project_reconcile_plan(&inventory, &base.plan, &cfg, &state_dir, &pr, &scan_meter)?;
            let unchanged = serde_json::to_value(&plan)? == serde_json::to_value(&base.plan)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
            // stdout is the RESULT, stderr is PROGRESS: the projection above is
            // the result, this explanation is progress and goes out through the
            // surface that `--progress none` silences (#241).
            pr.note(&format!(
                "(dry run — nothing indexed; {})",
                if unchanged {
                    format!(
                        "committed generation {} already describes this folder",
                        base.generation
                    )
                } else {
                    format!(
                        "this is the plan a real run would commit as generation {}",
                        base.generation + 1
                    )
                }
            ));
            // `ignored_files_in_pruned_dirs` is budget-capped, so the flag
            // saying whether it is a total or a floor travels with it (#279).
            pr.finish_with_flags(
                true,
                0,
                "dry-run",
                &[
                    ("files", inventory.files.len() as u64),
                    ("ignored_files", ignore_report.files_skipped),
                    ("ignored_dirs", ignore_report.dirs_pruned),
                    (
                        "ignored_files_in_pruned_dirs",
                        ignore_report.files_inside_pruned_dirs,
                    ),
                ],
                &[(
                    "ignored_files_in_pruned_dirs_exact",
                    ignore_report.files_inside_pruned_dirs_is_exact(),
                )],
            );
            return Ok((0, None));
        }
        let mut journal = state::Journal::open_after_preflight(
            preflight,
            &root_str,
            &cfg.url,
            &cfg.prefix,
            cfg.bulk_timeout_secs,
            cfg.fresh,
        )?;
        sync_executor::gc_snapshots(&state_dir, &journal)?;
        let base = journal
            .committed_manifest
            .as_ref()
            .context("generated journal lost its committed manifest")?
            .clone();
        let plan =
            project_reconcile_plan(&inventory, &base.plan, &cfg, &state_dir, &pr, &scan_meter)?;
        // Say out loud that the decision gate is not on this route. It is an
        // incremental reconcile of an already-committed generation, so the work
        // is the *changed* set and is published from a sealed snapshot rather
        // than through the phase-B queue the estimate prices. Accepting
        // `--max-minutes` here and quietly not applying it would be the
        // accepted-and-ignored class from #204 — so the run states the
        // exemption instead of leaving the caller to discover it.
        if cfg.max_minutes > 0 && cfg.approve.is_none() {
            pr.note(&format!(
                "gate: --max-minutes {} does not apply to an incremental reconcile of committed \
                 generation {} — only files that changed are processed, and they are published \
                 from a sealed snapshot rather than through the queue the estimate prices. Use \
                 --dry-run to see the projected plan before committing to it",
                cfg.max_minutes, base.generation
            ));
        }
        if serde_json::to_value(&plan)? == serde_json::to_value(&base.plan)? {
            if let Some(expected) = &base.execution {
                let (schema_identity, index_identity) = generation_contract_identities(&plan)?;
                anyhow::ensure!(
                    expected.root_identity == root_str
                        && expected.url == cfg.url
                        && expected.prefix == cfg.prefix
                        && expected.follow_symlinks == cfg.follow_symlinks
                        && expected.chunker_identity == prepared_records_identity(&cfg)?
                        && !expected.graph_enabled
                        && expected.brain == "disabled"
                        && expected.detector_identity == DETECTOR_DISABLED_IDENTITY
                        && expected.schema_identity == schema_identity
                        && expected.index_identity == index_identity,
                    "autoindex execution configuration changed since the committed generation; \
                     rebuild with a new --state-dir and a new --prefix"
                );
                if plan
                    .datasets
                    .iter()
                    .any(|dataset| dataset.semantic_field.is_some())
                {
                    let current = es.embedding_execution_identity()?;
                    ensure_embedding_execution_unchanged_and_resumable(
                        &current,
                        &expected.embedding_identity_sha256,
                        &expected.embedding_backend,
                        expected.embedding_dimension,
                        &expected.embedding_semantic_contract,
                    )?;
                }
            }
            let summary = finish_generated_run(&es, &mut journal, &cfg)?;
            let code = generated_exit_code(&summary);
            finish_generated_progress(&pr, code, &summary);
            return Ok((code, Some(summary)));
        }
        begin_non_graph_generation(
            &es,
            &mut journal,
            &state_dir,
            &cfg,
            &root_str,
            &inventory,
            plan,
        )?;
        let mut backend = sync_executor::EsSyncBackend::new(&es, &state_dir, cfg.bulk_mb << 20);
        sync_executor::replay_pending_operations(&state_dir, &mut journal, &mut backend)?;
        let summary = finish_generated_run(&es, &mut journal, &cfg)?;
        let code = generated_exit_code(&summary);
        finish_generated_progress(&pr, code, &summary);
        return Ok((code, Some(summary)));
    }
    if cfg.no_graph && preflight.plan.is_some() && !genesis_recovery {
        let replacement_state = state_dir.with_extension("generation-v1");
        let replacement_prefix = format!("{}-generation-v1", cfg.prefix);
        let reasons = if preflight.legacy_migration_reasons.is_empty() {
            "legacy journal has no complete generated-manifest authority".to_owned()
        } else {
            preflight.legacy_migration_reasons.join("; ")
        };
        let mut rebuild_argv = vec![
            "xerj".to_owned(),
            "autoindex".to_owned(),
            cfg.root.to_string_lossy().into_owned(),
            "--no-graph".to_owned(),
            "--url".to_owned(),
            cfg.url.clone(),
            "--state-dir".to_owned(),
            replacement_state.to_string_lossy().into_owned(),
            "--prefix".to_owned(),
            replacement_prefix,
            "--workers".to_owned(),
            cfg.workers.to_string(),
            "--pdf-workers".to_owned(),
            cfg.pdf_workers.to_string(),
            "--pdf-timeout-secs".to_owned(),
            cfg.pdf_timeout_secs.to_string(),
            "--bulk-mb".to_owned(),
            cfg.bulk_mb.to_string(),
            "--bulk-timeout-secs".to_owned(),
            cfg.bulk_timeout_secs.to_string(),
            "--snapshot-max-gb".to_owned(),
            (cfg.snapshot_max_bytes >> 30).to_string(),
            "--max-file-gb".to_owned(),
            cfg.max_file_gb.to_string(),
            "--sample".to_owned(),
            cfg.sample.to_string(),
        ];
        if cfg.no_semantic {
            rebuild_argv.push("--no-semantic".to_owned());
        }
        if cfg.follow_symlinks {
            rebuild_argv.push("--follow-symlinks".to_owned());
        }
        if cfg.follow_symlinks_outside_root {
            rebuild_argv.push("--follow-symlinks-outside-root".to_owned());
        }
        anyhow::bail!(
            "this state directory contains a legacy nonempty plan that cannot become generation \
             authority: {reasons}. Start an independent rebuild using this argv JSON (no shell \
             quoting required): {:?}. Keep XERJ_API_KEY set when the endpoint requires \
             authentication",
            rebuild_argv
        );
    }
    // #490: a committed generation manifest is graph-disabled by construction
    // (`sync::validate_manifest` requires `!graph_enabled` for every incremental
    // generation), and the `--no-graph`/committed re-run above already returned.
    // So reaching here with a committed manifest on the default *graph* path
    // means the corpus was built `--no-graph` and is being re-run with graph
    // detection on. Left to proceed it reconciles the no-graph plan on the graph
    // path and mutates the destination — publishing new documents and writing a
    // legacy plan record over the committed manifest — before the mismatch ever
    // surfaces (as an opaque `legacy plan write cannot follow a committed
    // generated manifest`), and then leaves `--fresh` refused. Refuse it here,
    // before the journal is opened for write and before `gc_snapshots`, exactly
    // the way the graph→no-graph direction above is refused: nothing is mutated.
    if !cfg.no_graph && !genesis_recovery {
        if let Some(committed) = preflight.committed_manifest.as_ref() {
            anyhow::bail!(
                "this corpus was indexed with --no-graph (committed generation {}); re-running \
                 it on the default graph path would reconcile two different authorities and \
                 mutate the destination. No remote mutation was attempted. Re-run with \
                 --no-graph to continue the committed corpus incrementally, or rebuild with a \
                 new --state-dir and a new --prefix.",
                committed.generation
            );
        }
    }
    if let Some(prior_plan) = preflight.plan.as_ref() {
        let comparison_keys = if cfg.fresh {
            inventory.keys.clone()
        } else {
            // Ordinary resume preserves planned-key identity for supported
            // same-path replacement and legacy plans.
            select_resume_plan_keys(
                &inventory.files,
                &inventory.keys,
                prior_plan,
                &state_dir.join("journal.ndjson"),
            )?
            .into_iter()
            .zip(inventory.keys.iter())
            .map(|(planned, current)| planned.unwrap_or_else(|| current.clone()))
            .collect()
        };
        // `--fresh` is checked here too: discarding the plan does not delete
        // the documents already published for a file that is now gone, so a
        // removal is unsafe in place whether or not the plan is kept.
        let delta = UnsupportedInventoryDelta::between(
            &cfg.root,
            &inventory.files,
            &comparison_keys,
            prior_plan,
        );
        if delta.refuses() {
            return Err(delta.into_error(RefusalTargets::describe(&cfg, &state_dir, prior_plan)));
        }
        // #589: a widened exclusion (file still on disk, newly ignored) is data
        // the exclusion must remove — sweep the excluded group's documents and
        // continue, rather than leaving them live in the destination. Genuine
        // deletions above still refuse; never mutate under --dry-run.
        if !delta.excluded_content_groups.is_empty() && !cfg.dry_run {
            sweep_excluded_groups(&es, prior_plan, &delta.excluded_content_groups)
                .context("sweep newly-excluded content groups")?;
        }
    }
    let mut journal = state::Journal::open_after_preflight(
        preflight,
        &root_str,
        &cfg.url,
        &cfg.prefix,
        cfg.bulk_timeout_secs,
        cfg.fresh,
    )?;
    sync_executor::gc_snapshots(&state_dir, &journal)?;
    let resumed_with_plan = journal.plan.is_some() && !genesis_recovery;
    if inventory.files.is_empty() && !resumed_with_plan {
        println!("no files found under {}", cfg.root.display());
        pr.finish(true, 0, "no-files", &[]);
        return Ok((0, None));
    }
    let run_id = journal.run_id.clone();
    if journal.resumed {
        pr.note(&format!(
            "resuming from journal {} ({} files already done)",
            journal.path().display(),
            journal.done.len()
        ));
    }
    let journal_path = journal.path().to_path_buf();
    let mut content_changed = std::collections::HashSet::new();
    let mut stale_alias_ids = Vec::new();
    let mut alias_paths_to_replace = std::collections::HashSet::new();
    let mut plan_changed = journal.plan.is_none();
    // Preserve legacy document IDs while upgrading old plans with full digests.
    // A later same-size/tail mutation is then detected and reindexed.
    if let Some(plan) = &mut journal.plan {
        let needs_alias_path_migration = !plan.alias_paths_indexed;
        let previous_aliases = plan.duplicate_files.clone();
        alias_paths_to_replace.extend(previous_aliases.iter().map(|alias| alias.rel.clone()));
        stale_alias_ids.extend(plan.duplicate_files.iter().map(|old| {
            catalog::duplicate_file_id(&cfg.prefix, &old.file_key, &old.rel, &old.path_id)
        }));
        let selected_plan_keys =
            select_resume_plan_keys(&inventory.files, &inventory.keys, plan, &journal_path)?;
        for (index, planned_key) in selected_plan_keys.into_iter().enumerate() {
            let file = &inventory.files[index];
            if let Some(planned_key) = planned_key {
                if !plan.files.contains_key(&planned_key) {
                    // The file was diverted off a planned key exclusively
                    // owned by another current file. Record the divergence in
                    // the durable plan so every resume deterministically skips
                    // this path instead of racing one ax_file key.
                    if !plan
                        .junk_files
                        .iter()
                        .any(|junk| junk.file_key == planned_key)
                    {
                        let owner = plan
                            .files
                            .get(&inventory.keys[index])
                            .map(|assignment| assignment.rel.as_str())
                            .unwrap_or("another file");
                        plan.junk_files.push(JunkFile {
                            file_key: planned_key.clone(),
                            rel: file.rel.clone(),
                            format: "unknown".into(),
                            status: "skipped".into(),
                            reason: format!(
                                "content resolves to planned key {} owned by {owner}; skipped to \
                                 keep key ownership exclusive (remove one of the two files and \
                                 rerun to index the survivor)",
                                inventory.keys[index]
                            ),
                            bytes: file.size,
                        });
                        plan_changed = true;
                    }
                    inventory.keys[index] = planned_key;
                    continue;
                }
                let assignment = plan.files.get_mut(&planned_key).expect("planned key");
                if assignment.rel != file.rel {
                    content_changed.insert(planned_key.clone());
                    plan_changed = true;
                }
                if assignment
                    .content_digest
                    .as_deref()
                    .is_some_and(|digest| digest != inventory.digests[index])
                {
                    content_changed.insert(planned_key.clone());
                    plan_changed = true;
                }
                if assignment.path_id != file.rel_id
                    || assignment.content_digest.as_deref()
                        != Some(inventory.digests[index].as_str())
                {
                    plan_changed = true;
                }
                assignment.rel = file.rel.clone();
                assignment.path_id = file.rel_id.clone();
                assignment.content_digest = Some(inventory.digests[index].clone());
                inventory.keys[index] = planned_key;
            }
        }
        let key_by_path: HashMap<&str, &str> = inventory
            .files
            .iter()
            .zip(inventory.keys.iter())
            .map(|(file, key)| (file.rel.as_str(), key.as_str()))
            .collect();
        for duplicate in &mut inventory.duplicates {
            if let Some(key) = key_by_path.get(duplicate.duplicate_of.as_str()) {
                duplicate.file_key = (*key).to_string();
            }
        }
        let current_alias_ids: std::collections::HashSet<String> = inventory
            .duplicates
            .iter()
            .map(|alias| {
                catalog::duplicate_file_id(&cfg.prefix, &alias.file_key, &alias.rel, &alias.path_id)
            })
            .collect();
        stale_alias_ids.retain(|id| !current_alias_ids.contains(id));
        // The historical global flag cannot identify which live documents
        // already carry ax_paths. Its one-time migration must rewrite every
        // canonical key; ordinary alias changes remain scoped per key.
        // A key whose entire duplicate group was deleted has no current file
        // to republish; scheduling it would strand a pending replacement that
        // every later run re-journals without ever committing.
        let current_keys: std::collections::HashSet<&str> =
            inventory.keys.iter().map(String::as_str).collect();
        content_changed.extend(
            alias_keys_to_reindex(
                &previous_aliases,
                &inventory.duplicates,
                needs_alias_path_migration.then_some(inventory.keys.as_slice()),
            )
            .into_iter()
            .filter(|key| current_keys.contains(key.as_str())),
        );
        if needs_alias_path_migration {
            plan.alias_paths_indexed = true;
            plan_changed = true;
        }
        if previous_aliases != inventory.duplicates {
            plan_changed = true;
        }
        plan.duplicate_files = inventory.duplicates.clone();
    }
    alias_paths_to_replace.extend(inventory.duplicates.iter().map(|alias| alias.rel.clone()));
    let files = inventory.files.clone();
    let keys = inventory.keys.clone();
    let digests = inventory.digests.clone();
    let duplicate_files = inventory.duplicates.clone();
    let paths_discovered = files.len() + duplicate_files.len();
    let planned_bytes: u64 = files.iter().map(|f| f.size).sum();
    if !duplicate_files.is_empty() {
        pr.note(&format!(
            "autoindex: {} byte-identical duplicate path(s) will reuse canonical content",
            duplicate_files.len()
        ));
        for duplicate in duplicate_files.iter().take(REFUSAL_LIST_CAP) {
            pr.note(&format!(
                "  duplicate: {} → {}",
                duplicate.rel, duplicate.duplicate_of
            ));
        }
        let remaining = duplicate_files.len().saturating_sub(REFUSAL_LIST_CAP);
        if remaining > 0 {
            pr.note(&format!("  … and {remaining} more"));
        }
    }

    // ── Phase A: inference (skipped when a frozen plan exists) ──────────
    let mut clusters_rt: Option<Vec<dataset::Cluster>> = None;
    let mut pdf_spools: Vec<Option<extract::pdf::ExtractionSpool>> =
        (0..files.len()).map(|_| None).collect();
    // Both worker widths come from the one resource policy (#240), and since
    // that policy may set them apart, the spool's headroom is sized for the
    // wider phase: phase-A scan threads are what hold artifact descriptors
    // open, phase-B workers are what hold bulk buffers. Reserving for the
    // larger of the two can only make this optional accelerator hand capacity
    // back — never take headroom the run still needs.
    //
    // `--no-graph` is the exception, and it is a decision this rebase had to
    // make between two landed changes. #248's artifact is a phase A→B
    // accelerator, but every `--no-graph` route below returns from the
    // generated path — which publishes from its own sealed snapshot and never
    // reaches the legacy phase B. A retained artifact could therefore never be
    // replayed, so admission is disabled instead of spooling gigabytes nothing
    // will read. Nothing observable changes: the generated run document does
    // not carry `pdf_extraction_reuse`.
    let (pdf_spool_budget, pdf_spool_capacity_warning) = if cfg.no_graph {
        (extract::pdf::ExtractionSpoolBudget::new(0, 0), None)
    } else {
        extract::pdf::ExtractionSpoolBudget::for_state_dir(
            &state_dir,
            cfg.workers.max(scan_threads),
            cfg.pdf_workers,
            cfg.bulk_mb,
        )
    };
    let phase_a_context = PhaseAContext {
        state_dir: &state_dir,
        budget: &pdf_spool_budget,
        capacity_warning: pdf_spool_capacity_warning.as_deref(),
        progress: &pr,
        meter: &scan_meter,
    };
    let mut plan: Plan = if let Some(p) = journal.plan.clone().filter(|_| !genesis_recovery) {
        p
    } else {
        pr.phase("scan", files.len() as u64, planned_bytes);
        // Named with the width phase A is really running at: `--workers` only
        // started governing this phase in #240, so "how wide is it right now"
        // is exactly the question the reader has.
        pr.note(&format!(
            "phase A: sniffing + sampling {} files with {scan_threads} threads…",
            files.len()
        ));
        let PhaseA {
            plan,
            clusters,
            pdf_spools: spools,
        } = build_phase_a(
            &cfg.root,
            &files,
            &keys,
            &digests,
            duplicate_files.clone(),
            &phase_a_context,
            &cfg,
        );
        pdf_spools = spools;
        pr.note(&format!(
            "phase A: {} datasets inferred, {} junk/skipped files",
            plan.datasets.len(),
            plan.junk_files.len()
        ));
        clusters_rt = Some(clusters);
        plan
    };

    // Second gate, after key selection has settled: fail before index
    // creation/mapping, replacement intent, graph invalidation,
    // delete-by-query, bulk publication, refresh, or catalog writes when a
    // canonical content group the plan published is gone from the folder.
    // Continuing would leave those documents searchable with no source file.
    // Additions, same-path replacement and crash repair remain supported.
    //
    // It runs before the #238 junk sweep below on purpose: a refused rerun
    // must compute nothing and mutate nothing.
    if resumed_with_plan {
        let delta = UnsupportedInventoryDelta::between(&cfg.root, &files, &keys, &plan);
        if delta.refuses() {
            return Err(delta.into_error(RefusalTargets::describe(&cfg, &state_dir, &plan)));
        }
        // #589: sweep documents left behind by a widened exclusion (see gate
        // above). Genuine deletions still refuse; never mutate under --dry-run.
        if !delta.excluded_content_groups.is_empty() && !cfg.dry_run {
            sweep_excluded_groups(&es, &plan, &delta.excluded_content_groups)
                .context("sweep newly-excluded content groups")?;
        }
    }

    // ── stale junk-catalog sweep (#238) ──────────────────────────────────
    //
    // A junk/skipped file is never indexed and never enters the graph corpus
    // — both walk `plan.files`, which by construction does not contain it. Its
    // ENTIRE live footprint is one `file:{key}` catalog document, so removing
    // that document is a complete removal, unlike an indexed file where
    // dropping the catalog entry while its records stay live would be a lie.
    //
    // The durable plan is the only thing that remembers the document exists:
    // nothing else in a run remembers a file it deliberately did not read. So
    // the sweep is driven from the plan, and the plan entry may be dropped
    // only after the delete has actually landed — the same order quickwit's
    // GC keeps, deleting split files first and removing metastore records
    // only for the deletes that succeeded (quickwit,
    // quickwit/quickwit-index-management/src/garbage_collection.rs:484-534,
    // Apache-2.0; approach only, no code taken). Dropping the record first
    // would strand the document exactly as before.
    //
    // A key that is somehow BOTH planned and junk is left alone: deleting
    // `file:{key}` would take out a live indexed file's catalog entry, and an
    // immortal junk row is the cheaper of those two failures.
    let live_keys: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
    let stale_junk_keys: std::collections::HashSet<String> = plan
        .junk_files
        .iter()
        .filter(|junk| {
            !live_keys.contains(junk.file_key.as_str()) && !plan.files.contains_key(&junk.file_key)
        })
        .map(|junk| junk.file_key.clone())
        .collect();

    // ── estimate → work order → decision gate ────────────────────────────
    //
    // Everything below happens BEFORE the first remote mutation: no index has
    // been created, no mapping upgraded, no plan persisted, no document
    // written. That placement is the whole point — a run that stops here has
    // cost the user a read of their folder and nothing else.
    let pending = pending_for_phase_b(&keys, &plan, &journal.done_keys(), &content_changed);
    let scan_rates = scan_meter.rates();
    let planned_for_estimate: Vec<estimate::PlannedFile> = pending
        .iter()
        .map(|&i| estimate::PlannedFile {
            family: plan.files[&keys[i]].family.clone(),
            bytes: files[i].size,
        })
        .collect();
    let run_estimate = estimate::Estimate::compute(&planned_for_estimate, &scan_rates, cfg.workers);
    let work_items: Vec<order::Item> = pending
        .iter()
        .map(|&i| order::Item {
            index: i,
            band: order::band_from_family_str(&files[i].rel, &plan.files[&keys[i]].family),
            bytes: files[i].size,
        })
        .collect();
    let bands = order::summarize(&work_items);
    let rel_bytes: Vec<(String, u64)> = pending
        .iter()
        .map(|&i| (files[i].rel.clone(), files[i].size))
        .collect();
    let rel_family_bytes: Vec<(String, String, u64)> = pending
        .iter()
        .map(|&i| {
            (
                files[i].rel.clone(),
                plan.files[&keys[i]].family.clone(),
                files[i].size,
            )
        })
        .collect();
    let heaviest = gate::heaviest_directories(&rel_bytes, 5);

    pr.note(&format!("estimate: {}", run_estimate.headline()));
    pr.note(&format!("estimate: basis — {}", run_estimate.basis));
    for family in &run_estimate.families {
        pr.note(&format!(
            "  estimate: {:<10} {:>5} files {:>9} at {}/s measured over {} file(s) / {} → {}",
            family.family,
            family.planned_files,
            estimate::human_bytes(family.planned_bytes),
            estimate::human_bytes(family.bytes_per_sec as u64),
            family.measured_files,
            estimate::human_bytes(family.measured_bytes),
            estimate::human_secs(family.seconds_of_work),
        ));
    }
    for family in &run_estimate.unmeasured_families {
        pr.note(&format!(
            "  estimate: {:<10} {:>5} files {:>9} NOT priced — {}",
            family.family,
            family.planned_files,
            estimate::human_bytes(family.planned_bytes),
            family.reason,
        ));
    }
    for exclude in &run_estimate.excludes {
        pr.note(&format!("  estimate excludes: {exclude}"));
    }
    pr.note(&format!(
        "work order: {} — {} inside each band at {} worker(s); a file that outlasts everything \
         above it starts first regardless of band",
        bands
            .iter()
            .map(|band| format!(
                "{} ({} files, {})",
                band.band,
                band.files,
                estimate::human_bytes(band.bytes)
            ))
            .collect::<Vec<_>>()
            .join(" → "),
        if cfg.workers == 1 {
            "smallest first"
        } else {
            "biggest first"
        },
        cfg.workers,
    ));
    for band in &bands {
        pr.note(&format!("  {:<16} {}", band.band, band.why));
    }

    if gate::over_threshold(&run_estimate, cfg.max_minutes) {
        let semantic_datasets: Vec<String> = plan
            .datasets
            .iter()
            .filter(|dataset| dataset.semantic_field.is_some())
            .map(|dataset| dataset.slug.clone())
            .collect();
        // Decided once, before anything is printed, and used for both halves:
        // whether to touch stdin at all, and what the payload says about why
        // nobody was asked. `pr.enabled()` is the missing third condition —
        // `--quiet` / `--progress none` routes every `pr.note` below to
        // nothing, so without it the gate printed no question and then waited
        // on stdin for the answer to it.
        let prompt_blocked = gate::detect_prompt_block(pr.enabled());
        let request = gate::DecisionRequest {
            root: root_str.clone(),
            estimate: &run_estimate,
            max_minutes: cfg.max_minutes,
            bands: bands.clone(),
            heaviest: heaviest.clone(),
            without_generated: gate::without_generated_directories(
                &run_estimate,
                &scan_rates,
                &heaviest,
                &rel_family_bytes,
            ),
            semantic_datasets,
            total_datasets: plan.datasets.len(),
            graph_files: if cfg.no_graph {
                0
            } else {
                pending.len() as u64
            },
            prompt_blocked,
        };
        match cfg.approve {
            Some(answer) => pr.note(&format!(
                "gate: {} — answered with --approve {}",
                request.reason(),
                answer.as_str()
            )),
            None if cfg.dry_run => pr.note(&format!(
                "gate: a real run would STOP here and exit {} — {}",
                gate::EXIT_NEEDS_DECISION,
                request.reason()
            )),
            None => {
                for line in request.prose() {
                    pr.note(&line);
                }
                // Ask only where an answer can arrive AND the question was
                // actually shown. A pipe, a CI job or an agent has no one at
                // the keyboard; a quiet run has someone at the keyboard who
                // was shown nothing. Blocking on stdin in either case is an
                // invisible deadlock — the failure this gate was added to
                // prevent, not to introduce. When it is blocked, stdin is not
                // opened at all.
                let answer = gate::answer_from_terminal(
                    prompt_blocked,
                    || std::io::stdin().lock(),
                    |line| pr.note(line),
                );
                match answer {
                    Some(gate::Approval::Proceed) => pr.note("gate: proceeding"),
                    Some(gate::Approval::Fast) => {
                        // Not honourable mid-run: --no-semantic decides the
                        // mappings this plan was inferred with. Saying so beats
                        // silently indexing semantically anyway (#204).
                        pr.note(
                            "gate: 'fast' changes the inferred mappings, so it cannot be applied \
                             to a plan that is already frozen. Nothing was indexed — re-run the \
                             same command with --approve fast",
                        );
                        println!("{}", request.to_json());
                        pr.finish(false, gate::EXIT_NEEDS_DECISION, "needs-decision", &[]);
                        return Ok((gate::EXIT_NEEDS_DECISION, None));
                    }
                    Some(gate::Approval::Cancel) => {
                        pr.note("gate: cancelled — nothing was indexed");
                        pr.finish(true, 0, "cancelled", &[]);
                        return Ok((0, None));
                    }
                    None => {
                        println!("{}", request.to_json());
                        pr.finish(false, gate::EXIT_NEEDS_DECISION, "needs-decision", &[]);
                        return Ok((gate::EXIT_NEEDS_DECISION, None));
                    }
                }
            }
        }
    } else if cfg.max_minutes > 0 && run_estimate.gate_seconds().is_some() {
        // Say what the silence means. "Under the threshold" is a statement
        // about the floor, not a promise about the run, and an agent relaying
        // it to a person needs the difference.
        pr.note(&format!(
            "gate: not triggered — the measured extraction floor {} is under --max-minutes {}. \
             That is NOT a promise the run finishes in {} minutes: server, network and embedding \
             time are not measured before indexing starts",
            run_estimate.range_text(),
            cfg.max_minutes,
            cfg.max_minutes
        ));
    }
    if cfg.approve == Some(gate::Approval::Cancel) {
        pr.note("gate: --approve cancel — nothing was indexed");
        pr.finish(true, 0, "cancelled", &[]);
        return Ok((0, None));
    }

    if cfg.dry_run {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        pr.note("(dry run — nothing indexed)");
        // As above: the count is capped, so the completeness flag ships with it.
        pr.finish_with_flags(
            true,
            0,
            "dry-run",
            &[
                ("files", files.len() as u64),
                ("ignored_files", ignore_report.files_skipped),
                ("ignored_dirs", ignore_report.dirs_pruned),
                (
                    "ignored_files_in_pruned_dirs",
                    ignore_report.files_inside_pruned_dirs,
                ),
            ],
            &[(
                "ignored_files_in_pruned_dirs_exact",
                ignore_report.files_inside_pruned_dirs_is_exact(),
            )],
        );
        return Ok((0, None));
    }

    if cfg.no_graph && !resumed_with_plan {
        begin_non_graph_generation(
            &es,
            &mut journal,
            &state_dir,
            &cfg,
            &root_str,
            &inventory,
            plan,
        )?;
        let mut backend = sync_executor::EsSyncBackend::new(&es, &state_dir, cfg.bulk_mb << 20);
        sync_executor::replay_pending_operations(&state_dir, &mut journal, &mut backend)?;
        let summary = finish_generated_run(&es, &mut journal, &cfg)?;
        let code = generated_exit_code(&summary);
        finish_generated_progress(&pr, code, &summary);
        return Ok((code, Some(summary)));
    }

    if plan
        .datasets
        .iter()
        .any(|dataset| dataset.semantic_field.is_some())
    {
        let identity = es
            .embedding_execution_identity()
            .context("semantic autoindex could not pin the server embedding execution identity")?;
        journal.pin_embedding_identity(
            &identity.identity_sha256,
            identity.resumable,
            identity.non_resumable_reason.as_deref(),
        )?;
    }

    // ── create indices with explicit mappings ────────────────────────────
    // Two round trips per dataset; a 135-dataset plan is a real wait.
    pr.phase("prepare", plan.datasets.len() as u64, 0);
    for d in &plan.datasets {
        es.ensure_index(&d.index, &build_mapping(&d.specs))
            .with_context(|| format!("create index {}", d.index))?;
        es.update_mapping(
            &d.index,
            &json!({"properties": {"ax_paths": {"type": "keyword"}}}),
        )
        .with_context(|| format!("upgrade alias-path mapping for {}", d.index))?;
        pr.item_done(0);
    }
    es.ensure_index(catalog::CATALOG_INDEX, &catalog::catalog_mapping())?;
    es.update_mapping(
        catalog::CATALOG_INDEX,
        &json!({"properties": {
            "duplicate_of": {"type": "keyword"},
            // `started` intentionally stays out of this additive upgrade. A
            // catalog written by v1.0.0-rc.4 has a dynamically inferred TEXT
            // `started`, and asking the engine to add it as `date` is refused
            // 400 — which `es.update_mapping` turns into an `Err`, aborting
            // the run before any document work. `catalog::catalog_mapping`
            // declares it `date` for a fresh catalog, so the field is
            // permanently bimodal across installs; its doc comment carries the
            // full tripwire and the measured refusal.
            "summary_generated_at": {"type": "date", "format": "strict_date_optional_time||epoch_millis"},
            "invocation_telemetry_scope": {"type": "keyword"},
            // Safe to add here, unlike `started`: no release ever wrote this
            // field, so no existing catalog has a conflicting inferred type.
            "junk_records_this_run": {"type": "long"}
        }}),
    )
    .context("upgrade autoindex catalog mapping")?;
    // A replacement transaction starts before the effective new plan is
    // persisted and before live visibility changes. If the process dies at
    // any later boundary, journal replay removes the older file_done and
    // deterministically schedules a delete-before-replace repair.
    let generation_by_key: HashMap<&str, &str> = keys
        .iter()
        .zip(digests.iter())
        .map(|(key, digest)| (key.as_str(), digest.as_str()))
        .collect();
    // Snapshot whether live records may already exist before this run starts
    // any new publication intents. Fresh first publications can skip the
    // delete/refresh round trip; replacements and crash repairs cannot.
    let mut cleanup_required: std::collections::HashSet<String> = journal
        .done
        .keys()
        .chain(journal.pending_replacements.keys())
        .cloned()
        .collect();
    let mut replacements: Vec<&String> = content_changed
        .iter()
        .filter(|key| {
            let desired = generation_by_key.get(key.as_str()).copied();
            journal.done.contains_key(key.as_str())
                || journal
                    .pending_replacements
                    .get(key.as_str())
                    .is_some_and(|pending| Some(pending.as_str()) != desired)
        })
        .collect();
    replacements.sort();
    for key in replacements {
        journal.file_replace_start(
            key,
            generation_by_key
                .get(key.as_str())
                .copied()
                .unwrap_or("unknown"),
        )?;
    }
    // Persist only an effective plan change. Repeating the full plan on every
    // no-op resume caused journal growth proportional to plan_size × runs.
    if plan_changed {
        journal.write_plan(&plan)?;
    }
    replacement_failpoint(1).context("after durable replacement plan")?;

    let unity_guid_map = build_unity_guid_map(&files, &plan, &pr);
    report_unity_guid_map(&unity_guid_map, &pr);

    // ── Phase B: full-stream extraction + bulk indexing ─────────────────
    struct DsRt {
        index: String,
        plan: HashMap<String, coerce::Coerce>,
        records: AtomicU64,
    }
    let mut ds_rt: HashMap<String, DsRt> = HashMap::new();
    for d in &plan.datasets {
        ds_rt.insert(
            d.slug.clone(),
            DsRt {
                index: d.index.clone(),
                plan: coerce::plan_from_specs(&d.specs),
                records: AtomicU64::new(0),
            },
        );
    }

    let done0 = journal.done_keys();
    let planned_junk: std::collections::HashSet<&str> = plan
        .junk_files
        .iter()
        .map(|j| j.file_key.as_str())
        .collect();
    let mut new_unplanned: Vec<JunkFile> = Vec::new();
    // The same predicate the estimate above priced — see `pending_for_phase_b`
    // for why the two snapshots of `journal.done` cannot disagree.
    let mut todo: Vec<usize> = pending_for_phase_b(&keys, &plan, &done0, &content_changed);
    for i in 0..files.len() {
        if keys[i].is_empty() || done0.contains(&keys[i]) && !content_changed.contains(&keys[i]) {
            continue;
        }
        if !plan.files.contains_key(&keys[i]) && !planned_junk.contains(keys[i].as_str()) {
            // file appeared after the plan was frozen — recorded, not fatal
            new_unplanned.push(JunkFile {
                file_key: keys[i].clone(),
                rel: files[i].rel.clone(),
                format: "unknown".into(),
                status: "skipped".into(),
                reason: "not in the frozen resume plan, so nothing was published for it. Re-run \
                         with --fresh to rebuild the plan in place and include it (ids stay \
                         idempotent), or index it under a new --state-dir and --prefix"
                    .into(),
                bytes: files[i].size,
            });
        }
    }
    if !new_unplanned.is_empty() {
        // The frozen plan cannot absorb files discovered after it was written.
        // Say so on stderr rather than leaving it to the catalog: a rerun that
        // quietly ignores new files is the bug this gate exists to surface.
        // Routed through the progress surface, which owns stderr (#241): a raw
        // eprintln! here would break `--progress json`'s single parseable
        // stream, and `--quiet` already selects `--progress none`.
        pr.note(&format!(
            "autoindex: {} file(s) appeared after the resume plan was frozen and were NOT \
             indexed:",
            new_unplanned.len()
        ));
        for jf in new_unplanned.iter().take(REFUSAL_LIST_CAP) {
            pr.note(&format!("  not in plan, skipped: {}", jf.rel));
        }
        let remaining = new_unplanned.len().saturating_sub(REFUSAL_LIST_CAP);
        if remaining > 0 {
            pr.note(&format!("  … and {remaining} more"));
        }
        pr.note(
            "autoindex: re-run with --fresh to rebuild the plan in place and index them (ids \
             stay idempotent)",
        );
    }
    if resumed_with_plan {
        // Legacy journals predate intent-before-publication and may have live
        // partial records without either file_done or file_replace_start.
        // Conservatively clean every resumed planned todo once. Only a plan
        // created in this process can prove that its first publication is
        // genuinely fresh and skip the delete round trip.
        cleanup_required.extend(todo.iter().map(|&i| keys[i].clone()));
    }
    let todo_set: std::collections::HashSet<usize> = todo.iter().copied().collect();
    for (index, spool) in pdf_spools.iter_mut().enumerate() {
        if spool.is_none() {
            continue;
        }
        if todo_set.contains(&index) {
            pdf_spool_budget.record_phase_b_eligible();
        } else {
            pdf_spool_budget.record_discarded_before_replay();
            spool.take();
        }
    }
    // Every publication, including a fresh one, receives durable intent
    // before its first bulk. A failed fresh publication therefore skips the
    // unnecessary delete now but is recognized as pending and cleaned on the
    // next run.
    let mut intent_keys: Vec<&str> = todo.iter().map(|&i| keys[i].as_str()).collect();
    intent_keys.sort_unstable();
    intent_keys.dedup();
    for key in intent_keys {
        let generation = generation_by_key.get(key).copied().unwrap_or("unknown");
        if journal
            .pending_replacements
            .get(key)
            .is_none_or(|pending| pending != generation)
        {
            journal.file_replace_start(key, generation)?;
        }
    }
    // ── second-brain graph: corpus table, structural edges, invalidation ──
    // (SECOND_BRAIN_SPEC §6.6.1/§6.6.3.) Runs after plan finalization so the
    // detectors see the whole corpus, and BEFORE any Phase B publication so
    // replacement invalidation can only ever see prior-generation edges —
    // running it later would invalidate this run's own fresh edges.
    let bulk_cut = cfg.bulk_mb << 20;
    // Parser junk this invocation read out of source files (durable: it is
    // journaled per file and replayed on resume).
    let junk_records = AtomicU64::new(0);
    // Records the backend refused in a bulk response (invocation-local: no
    // journal record exists for a document that was never accepted).
    let rejected_records = AtomicU64::new(0);
    let bulk_errors = Mutex::new(Vec::<String>::new());
    let graph: Option<GraphRt> = if cfg.no_graph {
        None
    } else {
        pr.phase("graph", files.len() as u64, 0);
        let brain = match &cfg.brain {
            Some(b) => b.clone(),
            None => derive_brain_name(&cfg.root),
        };
        if let Err(reason) = detect::validate_brain(&brain) {
            anyhow::bail!(
                "brain name '{brain}' is invalid: {reason}. Pass an explicit --brain <name> \
                 or disable relationship detection with --no-graph"
            );
        }
        // Corpus resolution table: every planned file's rel → identity +
        // anchor node. valid_at comes from the file mtime (§6.4) so an
        // unchanged corpus re-emits byte-identical edge_ids and re-runs
        // converge by overwrite, exactly like ids::doc_id does for nodes.
        let mut corpus_files = Vec::new();
        for (i, f) in files.iter().enumerate() {
            let key = &keys[i];
            if key.is_empty() {
                continue;
            }
            let Some(fa) = plan.files.get(key) else {
                continue; // junk or post-freeze files carry no node docs
            };
            let slug = fa
                .assignments
                .iter()
                .find(|(g, _)| g.is_none())
                .map(|(_, s)| s.clone())
                .or_else(|| fa.assignments.iter().map(|(_, s)| s.clone()).min());
            let Some(slug) = slug else { continue };
            let mtime_ms = std::fs::metadata(&f.path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            corpus_files.push(detect::corpus_file(
                &f.rel, key, &slug, &fa.family, mtime_ms,
            ));
            pr.item_done(0);
        }
        let corpus = detect::CorpusIndex::build(corpus_files);
        let detectors = detect::default_detectors();
        let edges_index = detect::edges_index_name(&brain);
        let created_at_ms = chrono::Utc::now().timestamp_millis();

        es.ensure_index(&edges_index, &detect::edge_index_mapping())
            .with_context(|| format!("create edges index {edges_index}"))?;
        let mut nodes_indices: Vec<&str> = plan.datasets.iter().map(|d| d.index.as_str()).collect();
        nodes_indices.sort_unstable();
        nodes_indices.dedup();
        detect::ensure_brain_meta(
            &es,
            &edges_index,
            &brain,
            &nodes_indices.join(","),
            created_at_ms,
        )?;

        // Replacement invalidation FIRST: soft-invalidate every live edge a
        // replaced file taught in earlier runs. The bi-temporal record stays
        // queryable (`as_of` time travel); nothing is deleted.
        let mut invalidated = 0u64;
        {
            let mut replaced_rels: Vec<&str> = todo
                .iter()
                .filter(|&&i| cleanup_required.contains(&keys[i]))
                .map(|&i| files[i].rel.as_str())
                .collect();
            replaced_rels.sort_unstable();
            replaced_rels.dedup();
            for rel in replaced_rels {
                invalidated +=
                    detect::invalidate_prior_edges(&es, &edges_index, rel, created_at_ms)
                        .with_context(|| format!("invalidate prior edges taught by {rel}"))?;
            }
        }

        // Structural detection (samedir chains) + bulk write, cut at the same
        // --bulk-mb threshold as node bulks.
        let mut structural = Vec::new();
        for det in &detectors {
            det.detect_structure(&corpus, &mut structural);
        }
        let assembled = detect::assemble(&structural, &edges_index, created_at_ms);
        let mut written: std::collections::BTreeMap<&'static str, u64> =
            std::collections::BTreeMap::new();
        {
            let mut send_err: Option<String> = None;
            let mut buf: Vec<u8> = Vec::new();
            for edge in &assembled.edges {
                buf.extend_from_slice(&edge.ndjson);
                *written.entry(edge.detector).or_default() += 1;
                if buf.len() >= bulk_cut
                    && record_bulk_outcome(
                        &es,
                        std::mem::take(&mut buf),
                        &rejected_records,
                        &bulk_errors,
                        &mut send_err,
                    )
                {
                    break;
                }
            }
            if send_err.is_none() && !buf.is_empty() {
                record_bulk_outcome(&es, buf, &rejected_records, &bulk_errors, &mut send_err);
            }
            if let Some(e) = send_err {
                anyhow::bail!("write structural graph edges to {edges_index}: {e}");
            }
        }
        pr.note(&format!(
            "graph: brain '{brain}' → {edges_index}; {} structural edges, {} prior edges \
             invalidated ({} detectors live)",
            assembled.edges.len(),
            invalidated,
            detectors.len()
        ));
        Some(GraphRt {
            corpus,
            detectors,
            href_raw: detect::href::Href::default(),
            edges_index,
            brain,
            created_at_ms,
            written: Mutex::new(written),
            self_dropped: AtomicU64::new(assembled.self_dropped),
            invalidated,
        })
    };

    // Value first, then biggest-first inside each band — `crate::order` owns
    // the rule and the reasoning, and the bands were already printed with the
    // estimate above so the order is explained rather than asserted. The queue
    // is drained with `pop()`, so this is the start order reversed.
    //
    // This replaces a plain ascending sort by size. That rule was right about
    // scheduling (a huge file must not be left to serialise the tail) and
    // silent about value: a user who stopped early got whatever was largest,
    // which on a source tree is `node_modules`. `order` keeps the scheduling
    // property inside each band and adds a critical-path exception so a file
    // that genuinely dominates the run still starts first.
    todo = {
        let items: Vec<order::Item> = todo
            .iter()
            .map(|&i| order::Item {
                index: i,
                band: order::band_from_family_str(&files[i].rel, &plan.files[&keys[i]].family),
                bytes: files[i].size,
            })
            .collect();
        order::start_order_as_pop_queue(&items, cfg.workers)
    };
    let n_todo = todo.len();
    // Percent and ETA are bytes-based, never file-count-based. The queue is
    // biggest-first, so a files-done percent races to ~100% and then sits
    // there for minutes on the one big file still in flight (#241 §5/§6).
    // A replayed PDF still costs its own bytes to stage and send, so a spooled
    // file counts toward the same denominator as a parsed one.
    let todo_bytes: u64 = todo.iter().map(|&i| files[i].size).sum();
    let n_pdf_spools = todo
        .iter()
        .filter(|&&index| pdf_spools[index].is_some())
        .count();
    pr.phase("index", n_todo as u64, todo_bytes);
    pr.note(&format!(
        "phase B: indexing {} files with {} workers → {}",
        n_todo, cfg.workers, cfg.url
    ));
    if n_pdf_spools > 0 {
        pr.note(&format!(
            "phase B: reusing {n_pdf_spools} run-local PDF extraction(s); these PDFs will not be \
             parsed a second time"
        ));
    }

    // Move each optional artifact into its sole Phase-B job. A replay cannot
    // be retried or retained accidentally after staging begins.
    let queue = Mutex::new(
        todo.into_iter()
            .map(|index| {
                let spool = pdf_spools[index].take();
                (index, spool)
            })
            .collect::<Vec<_>>(),
    );
    let mut paths_by_key: HashMap<String, Vec<String>> = files
        .iter()
        .zip(keys.iter())
        .map(|(file, key)| (key.clone(), vec![file.rel.clone()]))
        .collect();
    for duplicate in &plan.duplicate_files {
        paths_by_key
            .entry(duplicate.file_key.clone())
            .or_default()
            .push(duplicate.rel.clone());
    }
    for paths in paths_by_key.values_mut() {
        paths.sort();
        paths.dedup();
    }
    let journal_mx = Mutex::new(&mut journal);
    let files_done = AtomicU64::new(0);
    let records_total = AtomicU64::new(0);
    let extra_junk = Mutex::new(Vec::<JunkFile>::new());

    std::thread::scope(|scope| {
        for _ in 0..cfg.workers.min(n_todo.max(1)) {
            scope.spawn(|| {
                loop {
                    let (i, pdf_spool) = match queue.lock().unwrap().pop() {
                        Some(job) => job,
                        None => break,
                    };
                    let f = &files[i];
                    // Counts this file done on EVERY exit path below, junk
                    // included: progress measures work drained from the queue,
                    // and a `continue` that skipped the count would park the
                    // bar short of 100% forever.
                    let _in_flight = pr.file(&f.rel, f.size);
                    let key = &keys[i];
                    let expected_digest = &digests[i];
                    let fa = plan.files.get(key).unwrap();
                    let asg: HashMap<Option<String>, String> =
                        fa.assignments.iter().cloned().collect();
                    let sn = if stub_matcher.matches(&f.rel) {
                        Ok(stub_sniffed(Path::new(&f.rel)))
                    } else {
                        sniff::sniff(&f.path)
                    };
                    let sn = match sn {
                        Ok(s) => s,
                        Err(e) => {
                            extra_junk.lock().unwrap().push(JunkFile {
                                file_key: key.clone(),
                                rel: f.rel.clone(),
                                format: "unknown".into(),
                                status: "junk".into(),
                                reason: format!("unreadable at index time: {e}"),
                                bytes: f.size,
                            });
                            continue;
                        }
                    };
                    let mut file_records = 0u64;
                    let mut file_junk = 0u64;
                    // Groups phase B saw that phase A never sampled, so they
                    // have no dataset to route to. Silently counting these as
                    // junk is how a whole Unity class (or SQL table) can
                    // vanish from a run with nothing in the report to say so.
                    let mut unrouted_groups: std::collections::BTreeMap<String, u64> =
                        std::collections::BTreeMap::new();
                    // `script_guid`s this file referenced that no `.meta` in
                    // the tree defines. These are exactly the records that
                    // ship without script_path/script_class, which is what
                    // makes the headline query come back empty.
                    let mut unresolved_script_guids: std::collections::BTreeMap<String, u64> =
                        std::collections::BTreeMap::new();
                    let mut file_dropped_by_dataset: HashMap<String, u64> = HashMap::new();
                    let mut send_err: Option<String> = None;
                    // Edges this file teaches — buffered apart from the node
                    // staging file (different target index) and sent only
                    // after the node bulks are accepted (§6.7).
                    let mut edge_drafts: Vec<detect::EdgeDraft> = Vec::new();
                    // (doc id, label) of the last staged text section — the
                    // sequence detector's predecessor. Stream order is the
                    // only source that can name a PDF page boundary's
                    // predecessor (p2-s0 follows the LAST section of page 1).
                    let mut prev_section: Option<(String, String)> = None;
                    let mut staged = match tempfile::Builder::new()
                        .prefix(".autoindex-stage-")
                        .tempfile_in(&state_dir)
                    {
                        Ok(file) => file,
                        Err(error) => {
                            let mut errors = bulk_errors.lock().unwrap();
                            if errors.len() < 5 {
                                errors.push(format!(
                                    "create per-file staging area for {}: {error}",
                                    f.rel
                                ));
                            }
                            continue;
                        }
                    };
                    if let Err(error) = content::verify(&f.path, f.size, expected_digest) {
                        let mut errors = bulk_errors.lock().unwrap();
                        if errors.len() < 5 {
                            errors.push(format!("{error:#}"));
                        }
                        continue;
                    }
                    // File-card anchor node (§6.6.2a): one card doc per corpus
                    // file, staged BEFORE the file's records. Its
                    // deterministic id is `CorpusFile.anchor_doc_id` — the
                    // node every file-level edge (wikilink/mdlink/href/
                    // pathcite/cratecite/samedir dst, sequence opener src)
                    // terminates at. Row/line/page families have no `s0`
                    // section doc, so without the card those edges pointed at
                    // ghosts. Not counted as an extracted record: it is
                    // derived anchor infrastructure, not file content.
                    if let Some(gr) = graph.as_ref() {
                        if let Some(cf) = gr.corpus.files.get(&f.rel) {
                            if let Some(rt) = ds_rt.get(&cf.dataset_slug) {
                                let name = f.rel.rsplit('/').next().unwrap_or(&f.rel);
                                let mut fields = Map::new();
                                fields.insert("title".into(), Value::String(name.to_string()));
                                fields.insert("ax_path".into(), Value::String(f.rel.clone()));
                                fields.insert(
                                    "ax_paths".into(),
                                    Value::Array(
                                        paths_by_key
                                            .get(key)
                                            .into_iter()
                                            .flatten()
                                            .cloned()
                                            .map(Value::String)
                                            .collect(),
                                    ),
                                );
                                fields.insert("ax_file".into(), Value::String(key.clone()));
                                fields.insert(
                                    "ax_locator".into(),
                                    Value::String(detect::FILE_CARD_LOCATOR.into()),
                                );
                                fields.insert(
                                    "ax_dataset".into(),
                                    Value::String(cf.dataset_slug.clone()),
                                );
                                fields.insert("ax_run".into(), Value::String(run_id.clone()));
                                fields.insert(
                                    "ax_format".into(),
                                    Value::String(format_str(Some(&sn))),
                                );
                                let action = json!({"index": {
                                    "_index": rt.index, "_id": cf.anchor_doc_id}});
                                if let Err(error) = writeln!(
                                    staged.as_file_mut(),
                                    "{}\n{}",
                                    action,
                                    Value::Object(fields)
                                ) {
                                    send_err =
                                        Some(format!("stage file card for {}: {error}", f.rel));
                                }
                            }
                        }
                    }
                    {
                        let mut sink = |rec: extract::RawRecord| -> bool {
                            let Some(slug) = asg.get(&rec.group).or_else(|| asg.get(&None)) else {
                                file_junk += 1;
                                *unrouted_groups
                                    .entry(
                                        rec.group.clone().unwrap_or_else(|| "(ungrouped)".into()),
                                    )
                                    .or_insert(0) += 1;
                                return true;
                            };
                            let Some(rt) = ds_rt.get(slug) else {
                                file_junk += 1;
                                return true;
                            };
                            let mut fields = rec.fields;
                            // BEFORE coercion, not after: these are ordinary
                            // record fields once stamped, and a field that
                            // skips `coerce_record` is a field the dataset
                            // plan never validated.
                            if let Some(unresolved) = enrich_unity_fields(
                                sn.family,
                                &mut fields,
                                &unity_guid_map.map,
                                &f.rel,
                            ) {
                                *unresolved_script_guids.entry(unresolved).or_insert(0u64) += 1;
                            }
                            let dropped = coerce::coerce_record(&mut fields, &rt.plan);
                            if dropped > 0 {
                                let durable =
                                    file_dropped_by_dataset.entry(slug.clone()).or_default();
                                *durable = durable.saturating_add(dropped as u64);
                            }
                            fields.insert("ax_path".into(), Value::String(f.rel.clone()));
                            fields.insert(
                                "ax_paths".into(),
                                Value::Array(
                                    paths_by_key
                                        .get(key)
                                        .into_iter()
                                        .flatten()
                                        .cloned()
                                        .map(Value::String)
                                        .collect(),
                                ),
                            );
                            fields.insert("ax_file".into(), Value::String(key.clone()));
                            fields.insert("ax_locator".into(), Value::String(rec.locator.clone()));
                            fields.insert("ax_dataset".into(), Value::String(slug.clone()));
                            fields.insert("ax_run".into(), Value::String(run_id.clone()));
                            fields.insert("ax_format".into(), Value::String(format_str(Some(&sn))));
                            let id = ids::doc_id(slug, key, &rec.locator);
                            let action = json!({"index": {"_index": rt.index, "_id": id}});
                            let doc = Value::Object(fields);
                            if let Err(error) = writeln!(
                                staged.as_file_mut(),
                                "{}\n{}",
                                action,
                                serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into())
                            ) {
                                send_err =
                                    Some(format!("stage extracted records for {}: {error}", f.rel));
                                return false;
                            }
                            rt.records.fetch_add(1, Ordering::Relaxed);
                            file_records += 1;
                            // Textual edge detection (§6.6.2), after the node
                            // action is staged: `body` is the exact section
                            // string the node doc carries, and `id` is the
                            // section node the evidence lives in.
                            if let Some(gr) = graph.as_ref() {
                                if let Some(label) = section_label(&rec.locator) {
                                    if let (Some(cf), Some(body)) = (
                                        gr.corpus.files.get(&f.rel),
                                        doc.get("body").and_then(Value::as_str),
                                    ) {
                                        let ctx = detect::SectionCtx {
                                            corpus: &gr.corpus,
                                            file: cf,
                                            section_label: &label,
                                            prev_section: prev_section
                                                .as_ref()
                                                .map(|(pid, pl)| (pid.as_str(), pl.as_str())),
                                            section_doc_id: &id,
                                            text: body,
                                        };
                                        for det in &gr.detectors {
                                            det.detect_text(&ctx, &mut edge_drafts);
                                        }
                                        prev_section = Some((id.clone(), label));
                                    }
                                }
                            }
                            true
                        };
                        // Demoted one-off config files (#173) index as
                        // documents — their key sets are configuration, not a
                        // schema (see `dataset` module docs).
                        //
                        // `as_document` deliberately outranks artifact replay.
                        // It decides the record *shape*: phase A built this
                        // dataset's mapping by re-sampling the file through
                        // `extract_as_document`, whereas an artifact replays
                        // PDF-parser page records. Replaying here would publish
                        // records the frozen plan does not describe. Today no
                        // PDF can reach this branch (`demotable_family` is
                        // JSON/YAML/XML only), so this is a guard that keeps
                        // the precedence right if that set ever widens — the
                        // artifact is refunded rather than replayed.
                        let res = if fa.as_document {
                            if pdf_spool.is_some() {
                                pdf_spool_budget.record_discarded_before_replay();
                            }
                            drop(pdf_spool);
                            extract::extract_as_document(&f.path, sn.gzip, &mut sink)
                        } else if sn.family == Family::Pdf {
                            match pdf_spool {
                                Some(spool) => {
                                    match spool.replay(f.size, expected_digest, &mut sink) {
                                        Ok(stats) => Ok(stats),
                                        Err(replay_error) => {
                                            // Replay verifies the complete
                                            // artifact before `deliver`
                                            // invokes the sink, so falling
                                            // back here cannot duplicate a
                                            // partially staged record stream.
                                            pdf_spool_budget
                                                .record_replay_fallback(&f.rel, &replay_error);
                                            pdf_spool_budget.record_reparse();
                                            extract::extract(&f.path, &sn, None, &mut sink)
                                                .with_context(|| {
                                                    format!(
                                                        "reparse {} after run-local PDF artifact \
                                                         verification failed: {replay_error:#}",
                                                        f.rel
                                                    )
                                                })
                                        }
                                    }
                                }
                                None => {
                                    pdf_spool_budget.record_reparse();
                                    extract::extract(&f.path, &sn, None, &mut sink)
                                }
                            }
                        } else {
                            extract::extract(&f.path, &sn, None, &mut sink)
                        };
                        match res {
                            Ok(stats) => {
                                file_junk += stats.junk;
                            }
                            Err(e) => {
                                send_err = Some(format!("extract {}: {e}", f.rel));
                                extra_junk.lock().unwrap().push(JunkFile {
                                    file_key: key.clone(),
                                    rel: f.rel.clone(),
                                    format: format_str(Some(&sn)),
                                    status: "junk".into(),
                                    reason: format!("extract failed at index time: {e}"),
                                    bytes: f.size,
                                });
                            }
                        }
                        if !unresolved_script_guids.is_empty() {
                            let total: u64 = unresolved_script_guids.values().sum();
                            pr.note(&format!(
                                "unity: {}: {total} MonoBehaviour record(s) reference {} \
                                 script guid(s) no .meta defines — they ship without \
                                 script_path/script_class: {}",
                                f.rel,
                                unresolved_script_guids.len(),
                                unresolved_script_guids
                                    .keys()
                                    .take(REFUSAL_LIST_CAP)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        if !unrouted_groups.is_empty() {
                            let total: u64 = unrouted_groups.values().sum();
                            let named = unrouted_groups
                                .iter()
                                .take(REFUSAL_LIST_CAP)
                                .map(|(g, n)| format!("{g} ({n})"))
                                .collect::<Vec<_>>()
                                .join(", ");
                            let more = unrouted_groups.len().saturating_sub(REFUSAL_LIST_CAP);
                            let suffix = if more > 0 {
                                format!(" and {more} more")
                            } else {
                                String::new()
                            };
                            // A NOTE, never an `extra_junk` entry. This file
                            // still reaches `journal.file_done` — `send_err`
                            // is untouched — and every junk entry is turned
                            // into a catalog document under the same
                            // `file:{file_key}` id as that completion
                            // (`catalog::file_doc`, `catalog.rs`). Pushing one
                            // here put two documents with one id in the same
                            // bulk, and the later one wins: a file that
                            // indexed N records was reported as status "junk"
                            // with records 0, `junk_file_count` counted it
                            // twice, and `code_coverage.observe` ran for it
                            // twice. The dropped records are already carried
                            // on that file's own completion as `junk`
                            // (`file_junk` below), which is where a per-file
                            // drop count belongs.
                            pr.note(&format!(
                                "{}: {total} record(s) dropped: group(s) never sampled in \
                                 phase A, so they have no dataset — {named}{suffix}",
                                f.rel
                            ));
                        }
                    }
                    // Raw-source href pass: the HTML extractor strips markup
                    // before sectioning, so `<a href>` evidence exists only in
                    // the raw bytes (detect::href module docs). The second
                    // content::verify below still covers this re-read.
                    if send_err.is_none() {
                        if let Some(gr) = graph.as_ref() {
                            if let Some(cf) =
                                gr.corpus.files.get(&f.rel).filter(|cf| cf.family == "html")
                            {
                                if let Ok(Some(bytes)) =
                                    extract::read_whole(&f.path, sn.gzip, extract::MAX_WHOLE_FILE)
                                {
                                    let (raw, _) = sniff::decode_text(&bytes);
                                    gr.href_raw.detect_raw_html(
                                        &gr.corpus,
                                        cf,
                                        &raw,
                                        &mut edge_drafts,
                                    );
                                }
                            }
                        }
                    }
                    if send_err.is_none() {
                        if let Err(error) = content::verify(&f.path, f.size, expected_digest) {
                            send_err = Some(format!(
                                "{error:#}; no records from this changing file were made visible"
                            ));
                        }
                    }
                    // Visibility begins only after extraction and the second
                    // full-content verification. Delete-before-replace makes
                    // a retry clean up any partial prior attempt.
                    if send_err.is_none() && cleanup_required.contains(key) {
                        let mut indices: Vec<&str> = fa
                            .assignments
                            .iter()
                            .filter_map(|(_, slug)| ds_rt.get(slug).map(|rt| rt.index.as_str()))
                            .collect();
                        indices.sort_unstable();
                        indices.dedup();
                        for index in indices {
                            if let Err(error) =
                                es.delete_by_query(index, &json!({"term": {"ax_file": key}}))
                            {
                                send_err = Some(format!(
                                    "remove prior records for {} before replacement: {error:#}",
                                    f.rel
                                ));
                                break;
                            }
                        }
                        if send_err.is_none() {
                            if let Err(error) = replacement_failpoint(2) {
                                send_err = Some(format!("{error:#}"));
                            }
                        }
                    }
                    if send_err.is_none() {
                        if let Err(error) = staged.as_file_mut().rewind() {
                            send_err =
                                Some(format!("rewind staged records for {}: {error}", f.rel));
                        }
                    }
                    if send_err.is_none() {
                        let mut reader = BufReader::new(staged.as_file_mut());
                        let mut buf = Vec::with_capacity(bulk_cut + (1 << 20));
                        let mut docs = 0usize;
                        loop {
                            let mut action = Vec::new();
                            match reader.read_until(b'\n', &mut action) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(error) => {
                                    send_err =
                                        Some(format!("read staged action for {}: {error}", f.rel));
                                    break;
                                }
                            }
                            let mut document = Vec::new();
                            match reader.read_until(b'\n', &mut document) {
                                Ok(0) => {
                                    send_err = Some(format!(
                                        "staged record for {} ended without a document",
                                        f.rel
                                    ));
                                    break;
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    send_err = Some(format!(
                                        "read staged document for {}: {error}",
                                        f.rel
                                    ));
                                    break;
                                }
                            }
                            buf.extend_from_slice(&action);
                            buf.extend_from_slice(&document);
                            docs += 1;
                            if (buf.len() >= bulk_cut || docs >= 5000)
                                && record_bulk_outcome(
                                    &es,
                                    std::mem::take(&mut buf),
                                    &rejected_records,
                                    &bulk_errors,
                                    &mut send_err,
                                )
                            {
                                break;
                            }
                            if buf.is_empty() {
                                docs = 0;
                                buf.reserve(bulk_cut);
                            }
                        }
                        if !buf.is_empty() && send_err.is_none() {
                            record_bulk_outcome(
                                &es,
                                buf,
                                &rejected_records,
                                &bulk_errors,
                                &mut send_err,
                            );
                        }
                    }
                    // Second-brain edges for this file (§6.7): only after the
                    // node bulks were accepted, so an edge never precedes its
                    // own src doc. A failed edge send leaves the file
                    // un-journaled — the whole file (nodes AND edges) is
                    // republished on the next run, which converges because
                    // both sides overwrite by deterministic _id.
                    if send_err.is_none() && !edge_drafts.is_empty() {
                        if let Some(gr) = graph.as_ref() {
                            let out =
                                detect::assemble(&edge_drafts, &gr.edges_index, gr.created_at_ms);
                            gr.self_dropped
                                .fetch_add(out.self_dropped, Ordering::Relaxed);
                            let mut ebuf: Vec<u8> = Vec::new();
                            for edge in &out.edges {
                                ebuf.extend_from_slice(&edge.ndjson);
                                if ebuf.len() >= bulk_cut
                                    && record_bulk_outcome(
                                        &es,
                                        std::mem::take(&mut ebuf),
                                        &rejected_records,
                                        &bulk_errors,
                                        &mut send_err,
                                    )
                                {
                                    break;
                                }
                            }
                            if send_err.is_none() && !ebuf.is_empty() {
                                record_bulk_outcome(
                                    &es,
                                    ebuf,
                                    &rejected_records,
                                    &bulk_errors,
                                    &mut send_err,
                                );
                            }
                            if send_err.is_none() {
                                let mut written = gr.written.lock().unwrap();
                                for edge in &out.edges {
                                    *written.entry(edge.detector).or_default() += 1;
                                }
                            }
                        }
                    }
                    if let Some(e) = send_err {
                        // endpoint trouble: record, do NOT journal file_done
                        let mut be = bulk_errors.lock().unwrap();
                        if be.len() < 5 {
                            be.push(e);
                        }
                        continue;
                    }
                    if let Err(error) = replacement_failpoint(4) {
                        let mut errors = bulk_errors.lock().unwrap();
                        if errors.len() < 5 {
                            errors.push(format!("{error:#}"));
                        }
                        continue;
                    }
                    records_total.fetch_add(file_records, Ordering::Relaxed);
                    junk_records.fetch_add(file_junk, Ordering::Relaxed);
                    let (commit_result, journal_path) = {
                        let mut journal = journal_mx.lock().unwrap();
                        let path = journal.path().display().to_string();
                        let result = journal.file_done(&FileDone {
                            file_key: key.clone(),
                            path: f.rel.clone(),
                            records: file_records,
                            junk: file_junk,
                            bytes: f.size,
                            dropped_by_dataset: file_dropped_by_dataset,
                            generation: Some(expected_digest.clone()),
                        });
                        (result, path)
                    };
                    match commit_result {
                        Ok(()) => {}
                        Err(error) => {
                            let mut errors = bulk_errors.lock().unwrap();
                            if errors.len() < 5 {
                                errors.push(format!(
                                    "durably commit completed source {} to {}: {error:#}. \
                                     Live records may be present, but the file remains pending; \
                                     repair journal storage and rerun autoindex",
                                    f.rel, journal_path
                                ));
                            }
                            continue;
                        }
                    }
                    // Workers no longer print. A worker can block for minutes
                    // inside one file, so a worker-driven heartbeat cannot
                    // bound silence; the ticker thread can, and does.
                    files_done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    // Corpus-wide edges (§6.6.2, `EdgeDetector::detect_corpus`): the pass for
    // relationships that only exist once EVERY document has been read —
    // sharedterm cannot know which words are distinctive until it has seen the
    // whole run. It runs after Phase B for the same reason per-file edges are
    // written after their file's nodes: an edge must never precede the docs it
    // points at. Skipped when Phase B already failed — that run bails below,
    // and edges over a half-read corpus would be edges over a lie.
    if let Some(gr) = &graph {
        if bulk_errors.lock().unwrap().is_empty() {
            pr.phase("graph-corpus", gr.detectors.len() as u64, 0);
            let mut drafts = Vec::new();
            for det in &gr.detectors {
                det.detect_corpus(&gr.corpus, &mut drafts);
                pr.item_done(0);
            }
            if !drafts.is_empty() {
                let out = detect::assemble(&drafts, &gr.edges_index, gr.created_at_ms);
                gr.self_dropped
                    .fetch_add(out.self_dropped, Ordering::Relaxed);
                let mut send_err: Option<String> = None;
                let mut buf: Vec<u8> = Vec::new();
                for edge in &out.edges {
                    buf.extend_from_slice(&edge.ndjson);
                    if buf.len() >= bulk_cut
                        && record_bulk_outcome(
                            &es,
                            std::mem::take(&mut buf),
                            &rejected_records,
                            &bulk_errors,
                            &mut send_err,
                        )
                    {
                        break;
                    }
                }
                if send_err.is_none() && !buf.is_empty() {
                    record_bulk_outcome(&es, buf, &rejected_records, &bulk_errors, &mut send_err);
                }
                match send_err {
                    Some(e) => bulk_errors
                        .lock()
                        .unwrap()
                        .push(format!("write corpus-wide graph edges: {e}")),
                    None => {
                        let mut written = gr.written.lock().unwrap();
                        for edge in &out.edges {
                            *written.entry(edge.detector).or_default() += 1;
                        }
                    }
                }
            }
        }
    }

    let bulk_errs = bulk_errors.into_inner().unwrap();
    if !bulk_errs.is_empty() {
        // `bulk_errors` keeps only the first five distinct errors, so without
        // the counter the operator cannot tell one refused document from ten
        // thousand. That is the whole reason backend rejections are counted
        // apart from parser junk: this is the only place the number is ever
        // reported, because a run that gets here writes no run document and
        // no map.
        let rejected = rejected_records.load(Ordering::Relaxed);
        let scale = if rejected > 0 {
            format!(" The backend refused {rejected} record(s).")
        } else {
            String::new()
        };
        anyhow::bail!(
            "autoindex stopped with bulk/backend failures: {}.{} Failed source files were not \
             journaled complete; fix the reported server or embedding configuration and rerun \
             the same command to resume safely",
            bulk_errs.join(" | "),
            scale
        );
    }

    // ── finalize: refresh, verify, correlate, catalog ────────────────────
    //
    // This block was 47-64% of every measured run and emitted NOTHING between
    // the last phase-B line and the final summary (#241 §1). Its work is fully
    // countable before each loop starts, so it is reported like any other
    // phase — split into named sub-phases because the mix of one-shot refreshes
    // and per-dataset round trips has no single honest denominator.
    pr.phase("finalize-refresh", 1 + graph.is_some() as u64, 0);
    es.refresh(&format!("{}-*", cfg.prefix)).ok();
    pr.item_done(0);
    // The dot-prefixed edges index is outside the {prefix}-* pattern.
    if let Some(gr) = &graph {
        es.refresh(&gr.edges_index).ok();
        pr.item_done(0);
    }

    // live per-dataset counts + time ranges (every claim traces to a run)
    pr.phase("finalize-count", plan.datasets.len() as u64, 0);
    let mut ds_counts: HashMap<String, u64> = HashMap::new();
    let mut ds_timerange: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for d in &plan.datasets {
        let _counted = pr.file(&d.index, 0);
        let cnt = es.count(&d.index).unwrap_or(0);
        ds_counts.insert(d.slug.clone(), cnt);
        if let Some(t) = &d.time_field {
            let body = json!({"size":0,"aggs":{
                "mn":{"min":{"field":t}},"mx":{"max":{"field":t}}}});
            if let Ok(v) = es.search(&d.index, &body) {
                let get = |k: &str| -> Option<String> {
                    let a = v.pointer(&format!("/aggregations/{k}"))?;
                    a.get("value_as_string")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            a.get("value").and_then(|f| f.as_f64()).and_then(|ms| {
                                chrono::DateTime::from_timestamp_millis(ms as i64)
                                    .map(|d| infer::dates::to_rfc3339_millis(&d))
                            })
                        })
                };
                ds_timerange.insert(d.slug.clone(), (get("mn"), get("mx")));
            }
        }
    }

    // ── zero-live verification gate (#195) ───────────────────────────────
    //
    // The journal's completed files claim records were made visible; the
    // live counts above are what the server actually answers with. When the
    // journal says records landed but ZERO documents are live across every
    // dataset index, the run must not report success: the user (or agent)
    // would be left with green, mapped, empty indices and no way to tell
    // "the corpus has no match" from "nothing was ever written". This is
    // the last-resort catch for any rejection path the per-bulk
    // classification does not recognise.
    let (files_done_journaled, journal_records) = {
        let journal = journal_mx.lock().unwrap();
        (
            journal.done.len(),
            journal.done.values().map(|fd| fd.records).sum::<u64>(),
        )
    };
    let live_records: u64 = ds_counts.values().sum();
    if journal_records > 0 && live_records == 0 {
        // Typed, not `bail!`: a caller that composes this pipeline can only
        // tell "the journal outlived its destination" from "the server
        // rejected our writes" if the condition carries a type. `xerj brain`
        // downcasts it (brain.rs) to offer the in-place rebuild instead of
        // printing this text raw. Peer precedent for classifying a
        // recoverable condition rather than folding it into a generic error:
        // redb's `Database::do_repair` returns `DatabaseError::RepairAborted`
        // as its own variant (redb/src/db.rs:994, Apache-2.0/MIT) so the
        // caller can distinguish it from ordinary corruption.
        return Err(ZeroLiveVerificationError {
            journal_records,
            files_done_journaled,
            dataset_indices: plan.datasets.len(),
        }
        .into());
    }

    // correlations
    let mut key_corrs: Vec<correlate::KeyCorr> = Vec::new();
    if let Some(clusters) = &clusters_rt {
        let mut cands = Vec::new();
        for (c, d) in clusters.iter().zip(plan.datasets.iter()) {
            for spec in &d.specs {
                let Some(acc) = c.fields.get(&spec.name) else {
                    continue;
                };
                if correlate::is_candidate(
                    &spec.es_type,
                    spec.semantic.as_deref(),
                    acc.distinct.len(),
                    acc.n,
                    acc.distinct_overflow,
                    (acc.long_ok > 0).then_some((acc.int_min, acc.int_max)),
                ) {
                    cands.push(correlate::Candidate {
                        slug: d.slug.clone(),
                        index: d.index.clone(),
                        field: spec.name.clone(),
                        kind: spec.es_type.clone(),
                        values: acc.raw_values.clone(),
                        sampled_n: acc.n,
                    });
                }
            }
        }
        key_corrs = correlate::key_overlaps(&cands);
        // One live query per candidate overlap, and the bound is known before
        // the loop starts — `phase=finalize-correlate items=37/135` needs no
        // new bookkeeping at all (#241 §8).
        pr.phase("finalize-correlate", key_corrs.len() as u64, 0);
        for c in key_corrs.iter_mut() {
            let _query = pr.file(
                &format!("{}.{} ~ {}.{}", c.a_slug, c.a_field, c.b_slug, c.b_field),
                0,
            );
            correlate::confirm(&es, c, 20).ok();
        }
        // keep only live-confirmed overlaps in the report
        key_corrs.retain(|c| c.confirmed.map(|(n, _)| n > 0).unwrap_or(false));
    } else {
        pr.note("(resumed run: key-overlap correlations kept from the original run's catalog)");
    }

    let timed: Vec<&PlanDataset> = plan
        .datasets
        .iter()
        .filter(|d| d.time_field.is_some())
        .collect();
    pr.phase("finalize-histogram", timed.len() as u64, 0);
    let mut series = Vec::new();
    for d in timed {
        if let Some(t) = &d.time_field {
            let _query = pr.file(&d.index, 0);
            if let Ok(Some(s)) = correlate::fetch_histogram(&es, &d.slug, &d.index, t) {
                series.push(s);
            }
        }
    }
    let time_corrs = correlate::time_alignment(&series);

    // ── catalog write ────────────────────────────────────────────────────
    // Alias IDs changed as identity evolved. Remove by logical path first so
    // catalogs created by any previous identity scheme cannot survive beside
    // the one current alias document.
    pr.phase(
        "finalize-catalog",
        alias_paths_to_replace.len() as u64 + 1,
        0,
    );
    for path in &alias_paths_to_replace {
        let _delete = pr.file(path, 0);
        es.delete_by_query(
            catalog::CATALOG_INDEX,
            &json!({
                "bool": {
                    "filter": [
                        {"term": {"status": "duplicate"}},
                        {"term": {"path": path}}
                    ]
                }
            }),
        )
        .with_context(|| format!("replace catalog alias for {path}"))?;
    }
    let mut cat_buf: Vec<u8> = Vec::new();
    let push_doc = |id: &str, doc: &Value, buf: &mut Vec<u8>| {
        let action = json!({"index": {"_index": catalog::CATALOG_INDEX, "_id": id}});
        buf.extend_from_slice(action.to_string().as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(doc.to_string().as_bytes());
        buf.push(b'\n');
    };
    for id in &stale_alias_ids {
        let action = json!({"delete": {"_index": catalog::CATALOG_INDEX, "_id": id}});
        cat_buf.extend_from_slice(action.to_string().as_bytes());
        cat_buf.push(b'\n');
    }
    // Junk/skipped files that left the corpus (#238). Deletes lead the buffer
    // so a key that is re-reported in the same run — a junk file whose bytes
    // changed, a legacy plan key rewritten under the current scheme — is
    // deleted before its replacement document is indexed, never after.
    let mut swept_junk_keys: Vec<&str> = stale_junk_keys.iter().map(String::as_str).collect();
    swept_junk_keys.sort_unstable();
    for key in &swept_junk_keys {
        let action = json!({"delete": {
            "_index": catalog::CATALOG_INDEX,
            "_id": catalog::file_id(&cfg.prefix, key),
        }});
        cat_buf.extend_from_slice(action.to_string().as_bytes());
        cat_buf.push(b'\n');
    }

    // dataset docs
    let mut junk_records_by_run: u64 = junk_records.load(Ordering::Relaxed);
    let (dataset_stats, durable_junk_records) = {
        let journal = journal_mx.lock().unwrap();
        let stats = durable_dataset_stats(&plan, &journal.done);
        // One definition of "junk records" per map. The dataset docs below
        // report the durable per-file commits, so the run doc must too:
        // reporting the invocation-local counter there made a no-op resume
        // publish `junk_records_total: 0` next to dataset docs that showed
        // the real number, in the same `xerj autoindex map` output. The
        // per-dataset values are this same total attributed to each file's
        // first assigned dataset, so they sum back to it for every file the
        // frozen plan still describes.
        let durable_junk: u64 = journal.done.values().map(|done| done.junk).sum();
        (stats, durable_junk)
    };
    for d in &plan.datasets {
        let sample_queries = catalog::build_sample_queries(d, &key_corrs);
        let mut notes = Vec::new();
        let durable = dataset_stats.get(&d.slug).copied().unwrap_or_default();
        let dropped = durable.dropped;
        if dropped > 0 {
            notes.push(format!(
                "{dropped} field values could not be coerced to the inferred types and were dropped (records still indexed)"
            ));
        }
        if let Some(g) = &d.group {
            notes.push(format!("source table: {g}"));
        }
        for s in &d.specs {
            for n in &s.notes {
                notes.push(format!("{}: {}", s.name, n));
            }
        }
        // formats incl gz flag
        let mut formats: Vec<String> = plan
            .files
            .values()
            .filter(|fa| fa.assignments.iter().any(|(_, s)| s == &d.slug))
            .map(|fa| {
                if fa.gzip {
                    format!("{}(gzip)", fa.family)
                } else {
                    fa.family.clone()
                }
            })
            .collect();
        formats.sort();
        formats.dedup();
        let (tmin, tmax) = ds_timerange.get(&d.slug).cloned().unwrap_or((None, None));
        let (id, doc) = catalog::dataset_doc(&catalog::DatasetDocInput {
            prefix: &cfg.prefix,
            pd: d,
            record_count: *ds_counts.get(&d.slug).unwrap_or(&0),
            junk_records: durable.junk,
            bytes: durable.bytes,
            file_count: d.file_count,
            formats,
            time_min: tmin,
            time_max: tmax,
            sample_queries,
            notes,
            run_id: &run_id,
        });
        push_doc(&id, &doc, &mut cat_buf);
    }

    // file docs — indexed (from journal) + junk/skipped (from plan + this run)
    //
    // The same pass produces this path's code/AST coverage (`CodeCoverage`):
    // the journal's completions are the corpus, not just this invocation's
    // slice, so a resume reports what is live rather than what it re-parsed.
    let mut code_coverage = CodeCoverage::default();
    let mut completed_keys: HashSet<String> = HashSet::new();
    {
        let j = journal_mx.lock().unwrap();
        for fd in j.done.values() {
            completed_keys.insert(fd.file_key.clone());
            let current_path = plan
                .files
                .get(&fd.file_key)
                .map(|assignment| assignment.rel.as_str())
                .unwrap_or(&fd.path);
            let fmt = plan
                .files
                .get(&fd.file_key)
                .map(|fa| {
                    if fa.gzip {
                        format!("{}(gzip)", fa.family)
                    } else {
                        fa.family.clone()
                    }
                })
                .unwrap_or_else(|| "unknown".into());
            code_coverage.observe(&fmt, fd.records);
            let (id, doc) = catalog::file_doc(
                &cfg.prefix,
                &fd.file_key,
                current_path,
                &fmt,
                "indexed",
                None,
                fd.records,
                fd.junk,
                fd.bytes,
                &run_id,
            );
            push_doc(&id, &doc, &mut cat_buf);
        }
    }
    let extra = extra_junk.into_inner().unwrap();
    let mut all_junk: Vec<&JunkFile> = plan
        .junk_files
        .iter()
        .filter(|junk| !stale_junk_keys.contains(&junk.file_key))
        .collect();
    all_junk.extend(extra.iter());
    all_junk.extend(new_unplanned.iter());
    // Disjointness from the journal completions above is ENFORCED here, not
    // merely asserted in a comment: `catalog::file_doc` derives its `_id` from
    // the file key alone, so a junk entry for a file that also reached
    // `file_done` is a second document with an id the completion already used
    // — and the bulk applies them in order, so the junk one wins. A file that
    // indexed N records would be reported in the catalog as status "junk" with
    // records 0. See `shadowed_junk_entries`.
    let shadowed = shadowed_junk_entries(&all_junk, &completed_keys);
    // Debug builds stop on the producer's defect; release builds keep the
    // truthful document and say what they dropped.
    debug_assert!(
        shadowed.is_empty(),
        "junk entry for a file that reached file_done: {:?}",
        shadowed.iter().map(|jf| &jf.rel).collect::<Vec<_>>()
    );
    if !shadowed.is_empty() {
        // Not fatal: the completion document is the correct one and it is
        // already staged, so dropping the junk entry restores the truth. It is
        // still a defect in whichever producer emitted it, so say so.
        pr.note(&format!(
            "internal: {} junk entr(ies) named a file that also completed and \
             were dropped so the catalog keeps the indexed record: {}",
            shadowed.len(),
            shadowed
                .iter()
                .take(REFUSAL_LIST_CAP)
                .map(|jf| jf.rel.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        all_junk.retain(|jf| !completed_keys.contains(&jf.file_key));
    }
    // Counted now: every later reader of this number outlives the borrows
    // `all_junk` holds on `plan` and `new_unplanned`, which the durable
    // junk-plan update below mutates.
    let junk_file_count = all_junk.len();
    for jf in &all_junk {
        code_coverage.observe(&jf.format, 0);
        let (id, doc) = catalog::file_doc(
            &cfg.prefix,
            &jf.file_key,
            &jf.rel,
            &jf.format,
            &jf.status,
            Some(&jf.reason),
            0,
            0,
            jf.bytes,
            &run_id,
        );
        push_doc(&id, &doc, &mut cat_buf);
        junk_records_by_run += 0; // junk FILES tracked separately from junk records
    }
    for duplicate in &plan.duplicate_files {
        let (id, doc) = catalog::duplicate_file_doc(
            &cfg.prefix,
            &duplicate.file_key,
            &duplicate.rel,
            &duplicate.path_id,
            &duplicate.duplicate_of,
            duplicate.bytes,
            &run_id,
        );
        push_doc(&id, &doc, &mut cat_buf);
    }

    for c in &key_corrs {
        let mut v = c.to_value();
        v["run_id"] = json!(run_id);
        push_doc(&c.id(&cfg.prefix), &v, &mut cat_buf);
    }
    for (i, tc) in time_corrs.iter().enumerate() {
        // Prefix-scoped for the same cross-corpus reason as `corr:` above (#673):
        // time correlations are keyed by dataset slugs and share the catalog.
        let i_fallback = i.to_string();
        let id = tcorr_id(
            &cfg.prefix,
            tc.get("a_dataset").and_then(|v| v.as_str()).unwrap_or(""),
            tc.get("b_dataset")
                .and_then(|v| v.as_str())
                .unwrap_or(&i_fallback),
        );
        let mut v = tc.clone();
        v["run_id"] = json!(run_id);
        push_doc(&id, &v, &mut cat_buf);
    }

    let wall = t0.elapsed().as_secs_f64();
    let (started, summary_generated_at) =
        invocation_report_timestamps(invocation_started, chrono::Utc::now());
    let total_records: u64 = ds_counts.values().sum();
    // Run-summary honesty (§6.6.4): what the detectors wrote AND what they
    // could not resolve — a dangling [[link]] is a fact about the corpus, not
    // something to swallow.
    let graph_summary = graph.as_ref().map(|gr| {
        let written = gr.written.lock().unwrap();
        let mut counters = detect::DetectorCounters::default();
        for det in &gr.detectors {
            let c = det.counters();
            counters.unresolved += c.unresolved;
            counters.ambiguous += c.ambiguous;
            counters.capped += c.capped;
        }
        let raw = gr.href_raw.counters();
        counters.unresolved += raw.unresolved;
        counters.ambiguous += raw.ambiguous;
        let by_detector: Map<String, Value> = written
            .iter()
            .map(|(tag, n)| ((*tag).to_string(), json!(n)))
            .collect();
        json!({
            "brain": gr.brain,
            "edges_index": gr.edges_index,
            "edges_written": written.values().sum::<u64>(),
            "by_detector": by_detector,
            "edges_unresolved": counters.unresolved,
            "edges_ambiguous": counters.ambiguous,
            "edges_capped": counters.capped,
            "edges_self_dropped": gr.self_dropped.load(Ordering::Relaxed),
            "edges_invalidated": gr.invalidated,
        })
    });
    // A resume intentionally reuses and upserts the durable run id. Timing
    // and detector counters therefore describe this latest invocation,
    // while corpus descriptors describe the durable live run state.
    let mut run_doc = json!({
        "doc_kind": "run",
        "run_id": run_id,
        "root": root_str,
        "url": cfg.url,
        "prefix": cfg.prefix,
        "started": started,
        "summary_generated_at": summary_generated_at,
        "invocation_telemetry_scope": "latest_invocation_of_durable_run",
        "files_total": paths_discovered,
        "unique_content_files": files.len(),
        "files_indexed": journal_mx.lock().unwrap().done.len(),
        "duplicate_files": plan.duplicate_files.len(),
        "files_junk": junk_file_count,
        "records_total": total_records,
        // Two numbers, one definition each, neither of them overlapping.
        //
        // `junk_records_total` is the durable corpus descriptor: the same
        // definition as every dataset doc's `junk_records`, summed over every
        // `FileDone` in the journal. It survives a no-op resume.
        //
        // `junk_records_this_run` is the same *kind* of failure narrowed to
        // the files this invocation actually parsed. It is the NARROWER of the
        // two, not the wider: every file counted here also committed a
        // `FileDone` that the total sums. On an unchanged resume it reads 0
        // while the total stays non-zero — which is the whole defect this
        // change exists to fix, seen from the other side.
        //
        // Backend rejections are deliberately absent. They are a different
        // failure, and they can never be reported here anyway: a per-item
        // rejection populates `bulk_errors`, which aborts the run above,
        // before this document exists. Publishing a
        // `records_rejected_by_backend` field would ship a number that is
        // provably always 0 — see `record_bulk_outcome` and
        // `backend_rejected_records_abort_the_run_before_any_map_is_written`.
        "junk_records_total": durable_junk_records,
        "junk_records_this_run": junk_records_by_run,
        // This-run submission accounting (#195): what THIS invocation sent
        // and had accepted, vs `records_total` above which is the live
        // server-side count. A healthy run keeps these consistent; a
        // mismatch is visible without reading a server log.
        "files_submitted_this_run": files_done.load(Ordering::Relaxed),
        "records_submitted_this_run": records_total.load(Ordering::Relaxed),
        "wall_seconds": (wall * 10.0).round() / 10.0,
        "workers": cfg.workers,
        // The whole resource decision, so a run can be explained after the
        // fact from its own summary rather than from the machine it ran on.
        // `scan_workers` is the width phase A actually ran at — see the
        // first-call-wins note at the top of this function; a requested width
        // that lost that race would make the summary a record of an intention
        // rather than of a run.
        "scan_workers": scan_threads,
        "pdf_workers": cfg.pdf_workers,
        "bulk_mb": cfg.bulk_mb,
        "cores_available": xerj_common::resource::cores(),
        // `null` on a platform with no RAM probe — the summary reports what the
        // run actually knew, never a stand-in number (#240).
        "memory_safe_zone_mb": xerj_common::resource::memory_safe_zone_bytes()
            .map(|b| b / (1024 * 1024)),
        // What the server's backpressure did to the offered load. A final
        // limit below `workers` means this run met real congestion and
        // answered it, which is the difference between a slow run and a run
        // that was making the machine worse (#240 §8).
        "bulk_concurrency_final": es.bulk_concurrency_limit(),
        "bulk_congestion_events": es.bulk_congestion_events(),
        "resource_notes": cfg.resource_notes,
        "semantic": !cfg.no_semantic,
        "pdf_extraction_reuse": pdf_spool_budget.report(),
    });
    if let Some(g) = &graph_summary {
        run_doc["graph"] = g.clone();
    }
    // Appended rather than written into the literal above: `serde_json::json!`
    // recurses once per key and that literal is already 30 deep — the same
    // reason `catalog::catalog_mapping` inserts its tail fields.
    for (key, value) in code_coverage.fields() {
        run_doc[key] = json!(value);
    }
    push_doc(&format!("run:{run_id}"), &run_doc, &mut cat_buf);

    if !cat_buf.is_empty() {
        let outcome = es.bulk(cat_buf).context("write catalog")?;
        // The catalog is the data map every later `map`/`status`/agent query
        // reads; a rejected catalog bulk (e.g. a write block that engaged
        // mid-run) must not be swallowed into a "success" exit (#195).
        if outcome.server_errors > 0 {
            anyhow::bail!(
                "write catalog: bulk backend failed for {} item(s): {}. Fix the reported \
                 server condition and rerun the same command",
                outcome.server_errors,
                outcome
                    .first_server_error
                    .as_deref()
                    .unwrap_or("unknown server error")
            );
        }
    }
    es.refresh(catalog::CATALOG_INDEX).ok();
    pr.item_done(0);
    // ── durable junk record, written last (#238) ─────────────────────────
    //
    // Only now is the plan allowed to claim what the catalog holds: the
    // documents above are written and the swept ones deleted. A failed
    // catalog bulk bailed out before this point with the plan untouched, so
    // the next run recomputes the identical additions and removals and
    // retries them — losing a record here is what makes a document immortal.
    //
    // Post-freeze skipped files are re-derived on every rerun, so this
    // converges: once they are in the plan they are `planned_junk`, the
    // additions are empty, and no further plan record is appended.
    if !new_unplanned.is_empty() || !stale_junk_keys.is_empty() {
        plan.junk_files
            .retain(|junk| !stale_junk_keys.contains(&junk.file_key));
        plan.junk_files.append(&mut new_unplanned);
        journal_mx
            .lock()
            .unwrap()
            .write_plan(&plan)
            .context("record junk/skipped catalog entries in the resume plan")?;
    }
    journal_mx.lock().unwrap().finish(&run_doc)?;

    // ── summary ──────────────────────────────────────────────────────────
    let junk_total_records = junk_records.load(Ordering::Relaxed);
    if cfg.json {
        println!("{run_doc}");
    } else if !cfg.quiet {
        println!("\ndone in {wall:.1}s — {} datasets, {} records live, {} duplicate aliases, {} junk records, {} junk/skipped files",
            plan.datasets.len(), total_records, plan.duplicate_files.len(), junk_total_records, junk_file_count);
        // Indexed-vs-submitted honesty line (#195): the live count against
        // what this run actually submitted, so a silent-rejection mismatch
        // is visible in the client output alone. Units differ on purpose: a
        // source record (e.g. one prose file) can expand to several section
        // documents, but submitted records with ZERO live documents is
        // always a defect (and fails above, before this line prints).
        println!(
            "indexed: {} documents live; this run submitted {} source records from {} files",
            total_records,
            records_total.load(Ordering::Relaxed),
            files_done.load(Ordering::Relaxed),
        );
        let mut rows: Vec<(&String, u64)> = plan
            .datasets
            .iter()
            .map(|d| (&d.index, *ds_counts.get(&d.slug).unwrap_or(&0)))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        for (idx, cnt) in rows {
            println!("  {idx:<40} {cnt:>10} docs");
        }
        if let Some(g) = &graph_summary {
            let by: Vec<String> = g["by_detector"]
                .as_object()
                .map(|m| m.iter().map(|(tag, n)| format!("{tag} {n}")).collect())
                .unwrap_or_default();
            println!(
                "graph: {} edges → {} ({}); {} unresolved, {} ambiguous, {} capped, {} self-dropped, {} invalidated",
                g["edges_written"],
                g["edges_index"].as_str().unwrap_or(""),
                if by.is_empty() { "no detections".to_string() } else { by.join(", ") },
                g["edges_unresolved"],
                g["edges_ambiguous"],
                g["edges_capped"],
                g["edges_self_dropped"],
                g["edges_invalidated"],
            );
        }
        println!(
            "{}",
            next_hint(
                &cfg.url,
                &cfg.prefix,
                cfg.api_key.as_deref(),
                std::env::var("XERJ_API_KEY").ok().as_deref(),
                cfg.api_key_file.as_deref(),
            )
        );
        // This line is where an agent learns how to search, so it is where the
        // code-aware form belongs. A blind usability run showed the cost of
        // omitting it: the agent followed exactly this guidance, got whole
        // files back, and reinvented client-side slicing rather than
        // discovering `_passage` — which it never saw mentioned anywhere.
        // Only printed when source code was actually indexed; for a PDF or log
        // corpus it would be noise.
        let indexed_code = plan.files.values().any(|fa| fa.family == "code");
        if indexed_code {
            println!(
                "      code was indexed — for a function/class instead of a whole file:\n\
                 \x20       curl -s $URL/{}-*/_search -H 'Content-Type: application/json' \\\n\
                 \x20         -d '{{\"query\":{{\"bool\":{{\"should\":[{{\"multi_match\":{{\"query\":\"<symbol or phrase>\",\
                 \"fields\":[\"body\",\"defs\"],\"type\":\"most_fields\"}}}},{{\"match_phrase\":{{\"defs\":{{\"query\":\"<symbol or phrase>\",\"boost\":4}}}}}}]}}}},\
                 \"_source\":[\"ax_path\",\"title\"],\"fields\":[\"_passage\"]}}'\n\
                 \x20       (the `match_phrase` clause ranks the file that DEFINES a symbol above files that merely call it; `_passage` returns \
                 the enclosing block, not the file)",
                cfg.prefix
            );
        }
        // A mixed corpus needs one more thing said out loud. Code volume
        // swamps prose on shared vocabulary: in a blind run, a question about
        // documentation ("exit code 3") matched WordPress PHP on `exit` and
        // `code` and returned nothing but source, costing a wasted round trip.
        // The agent worked out the fix itself afterwards — scope the query —
        // which is exactly the sort of thing that should not have to be
        // rediscovered.
        let indexed_prose = plan
            .files
            .values()
            .any(|fa| fa.family.starts_with("txt") || fa.family == "html");
        if indexed_code && indexed_prose {
            println!(
                "      this corpus mixes code and prose — code volume can swamp prose on\n\
                 \x20     shared words. Scope the side you want:\n\
                 \x20       …,\"query\":{{\"bool\":{{\"must\":[{{\"match\":{{\"body\":\"<text>\"}}}}],\
                 \"filter\":[{{\"term\":{{\"ax_format\":\"code\"}}}}]}}}}\n\
                 \x20       (values for `ax_format` in this run: {})",
                {
                    let mut fams: Vec<&str> =
                        plan.files.values().map(|fa| fa.family.as_str()).collect();
                    fams.sort_unstable();
                    fams.dedup();
                    fams.join(", ")
                }
            );
        }
    }
    // Exit 3 means "completed, some input was unusable". Backend rejections
    // are not consulted and must not be: a rejected item aborts the run with
    // an error long before this line, so `rejected_records` is provably 0
    // here. Splitting them out of `junk_total_records` therefore changes no
    // exit code.
    let code = if junk_total_records > 0 || junk_file_count > 0 {
        3
    } else {
        0
    };
    // Terminal line, in every progress mode but `none` (which `--quiet`
    // selects, and which prints nothing by definition). Exit 3 means
    // "completed, some files were unparseable" — success — and an agent reading
    // a bare `3` off a silent stream reads failure (#241 §9). Say it in words.
    if let Some(warning) = code_coverage.warning() {
        pr.note(&warning);
    }
    let mut done_fields = vec![
        ("files", files_done.load(Ordering::Relaxed)),
        ("records", total_records),
        ("datasets", plan.datasets.len() as u64),
        ("junk_files", junk_file_count as u64),
    ];
    done_fields.extend(code_coverage.fields());
    pr.finish(
        true,
        code,
        if code == 3 {
            "completed-with-junk"
        } else {
            "completed"
        },
        &done_fields,
    );
    drop(ticker);
    Ok((code, Some(run_doc)))
}

fn format_str(sn: Option<&Sniffed>) -> String {
    match sn {
        Some(s) if s.gzip => format!("{}(gzip)", s.family.as_str()),
        Some(s) => s.family.as_str().to_string(),
        None => "unknown".into(),
    }
}

// ─── map subcommand ──────────────────────────────────────────────────────

fn run_map(cfg: MapCfg) -> Result<i32> {
    let es = Es::new(&cfg.url, cfg.api_key.clone())?;
    es.ping()?;
    let fetch = |query: Value, size: usize, sort: Option<Value>| -> Result<Vec<Value>> {
        let mut body = json!({"query": query, "size": size});
        if let Some(s) = sort {
            body["sort"] = s;
        }
        let v = es.search(catalog::CATALOG_INDEX, &body)?;
        Ok(v.pointer("/hits/hits")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| h.get("_source").cloned())
                    .collect()
            })
            .unwrap_or_default())
    };
    let mut ds_query = json!({"term": {"doc_kind": "dataset"}});
    if let Some(slug) = &cfg.dataset {
        ds_query = json!({"bool": {"must": [
            {"term": {"doc_kind": "dataset"}},
            {"term": {"slug": slug}}
        ]}});
    }
    let datasets = fetch(ds_query, 500, Some(json!([{"record_count": "desc"}])))?;
    if datasets.is_empty() {
        eprintln!(
            "no autoindex catalog found at {} (index {}) — run `xerj autoindex <folder>` first",
            cfg.url,
            catalog::CATALOG_INDEX
        );
        return Ok(1);
    }
    let mut runs = fetch(json!({"term": {"doc_kind": "run"}}), 50, None)?;
    runs.sort_by_key(|r| {
        std::cmp::Reverse(
            r.get("started")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        )
    });
    let correlations = {
        let mut all = fetch(json!({"term": {"doc_kind": "correlation"}}), 200, None)?;
        // stale-correlation hygiene: catalog docs upsert by deterministic id,
        // so older runs' correlations linger — show only the latest run that
        // produced each corr_kind.
        for kind in ["key_overlap", "time_alignment"] {
            let latest = all
                .iter()
                .filter(|c| c.get("corr_kind").and_then(|k| k.as_str()) == Some(kind))
                .filter_map(|c| c.get("run_id").and_then(|r| r.as_str()))
                .max()
                .map(|s| s.to_string());
            all.retain(|c| {
                c.get("corr_kind").and_then(|k| k.as_str()) != Some(kind)
                    || c.get("run_id")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                        == latest
            });
        }
        all
    };
    let latest_run_filter = runs
        .first()
        .and_then(|run| run.get("run_id"))
        .and_then(|value| value.as_str())
        .map(|run_id| json!({"term": {"run_id": run_id}}));
    let mut junk_must = vec![json!({"term": {"doc_kind": "file"}})];
    if let Some(filter) = latest_run_filter.clone() {
        junk_must.push(filter);
    }
    let junk_files = fetch(
        json!({"bool": {"must": junk_must,
            "must_not": [
                {"term": {"status": "indexed"}},
                {"term": {"status": "duplicate"}}
        ]}}),
        500,
        None,
    )?;
    let mut duplicate_must = vec![
        json!({"term": {"doc_kind": "file"}}),
        json!({"term": {"status": "duplicate"}}),
    ];
    if let Some(filter) = latest_run_filter {
        duplicate_must.push(filter);
    }
    let duplicate_files = fetch(json!({"bool": {"must": duplicate_must}}), 500, None)?;
    if cfg.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "run": runs.first(),
                "datasets": datasets,
                "correlations": correlations,
                "junk_files": junk_files,
                "duplicate_files": duplicate_files,
                "gotchas": catalog::GOTCHAS,
            }))?
        );
    } else {
        print!(
            "{}",
            catalog::render_map(
                runs.first(),
                &datasets,
                &correlations,
                &junk_files,
                &duplicate_files,
                junk_files.len() as u64
            )
        );
        // Second-brain summary (§6.1): live edge count straight from the
        // edges index, scoped by `exists src` so the meta doc never counts.
        if let Some(g) = runs.first().and_then(|r| r.get("graph")) {
            if let (Some(brain), Some(edges_index)) = (
                g.get("brain").and_then(Value::as_str),
                g.get("edges_index").and_then(Value::as_str),
            ) {
                let live = es
                    .search(
                        edges_index,
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
                    .and_then(|v| v.pointer("/hits/total/value").and_then(Value::as_u64));
                match live {
                    Some(n) => println!("\ngraph: {n} live edges in {edges_index} (brain {brain})"),
                    None => {
                        println!("\ngraph: brain {brain} — edges index {edges_index} unreachable")
                    }
                }
            }
        }
    }
    Ok(0)
}

// ─── catalog summary (reused by `xerj feedback`) ─────────────────────────

/// A compact, factual view of what the latest autoindex run put on a node.
///
/// `xerj feedback` uses it to auto-fill the "what was indexed" line of a field
/// report without re-deriving the catalog queries `run_map` already owns. It is
/// deliberately read-only and narrow: the latest run document and the dataset
/// documents, nothing else. Everything else the caller needs (correlations,
/// junk, gotchas) belongs to the full `autoindex map`, not to a one-line
/// summary.
pub struct CatalogSummary {
    /// The most recent `doc_kind: run` document, if any run has been recorded.
    pub run: Option<Value>,
    /// Every `doc_kind: dataset` document, most records first — the same order
    /// `run_map` renders them in.
    pub datasets: Vec<Value>,
}

impl CatalogSummary {
    /// A single honest sentence for a field report's "Pointed at" line, or
    /// `None` when nothing was indexed (so the caller emits a placeholder
    /// rather than a sentence that says "0 records"). Never fabricates: every
    /// number here comes straight from the catalog the server wrote.
    pub fn one_line(&self) -> Option<String> {
        if self.datasets.is_empty() {
            return None;
        }
        let get = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
        // Prefer the run document's own total; fall back to summing datasets so
        // a node whose run doc predates that field still reports a real count.
        let records = self
            .run
            .as_ref()
            .and_then(|r| get(r, "records_total").as_u64())
            .unwrap_or_else(|| {
                self.datasets
                    .iter()
                    .filter_map(|d| d.get("record_count").and_then(Value::as_u64))
                    .sum()
            });
        let mut sentence = format!(
            "{records} records across {} dataset(s)",
            self.datasets.len()
        );
        if let Some(root) = self
            .run
            .as_ref()
            .and_then(|r| r.get("root"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            sentence.push_str(&format!(" under {root}"));
        }
        if let Some(run_id) = self
            .run
            .as_ref()
            .and_then(|r| r.get("run_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            sentence.push_str(&format!(" (autoindex run {run_id})"));
        }
        Some(sentence)
    }
}

/// Fetch the [`CatalogSummary`] from a running node's `autoindex-catalog`.
///
/// Reuses the same catalog index and query shapes as `run_map`, minus the
/// correlation/junk/duplicate passes a one-line summary does not need. Any
/// failure — endpoint unreachable, auth rejected, no catalog yet — is returned
/// as an `Err`, so `xerj feedback` can degrade to a template placeholder
/// instead of inventing a number (the repo's honest-claims rule).
pub fn fetch_catalog_summary(url: &str, api_key: Option<String>) -> Result<CatalogSummary> {
    let es = Es::new(url, api_key)?;
    es.ping()?;
    let fetch = |query: Value, size: usize, sort: Option<Value>| -> Result<Vec<Value>> {
        let mut body = json!({"query": query, "size": size});
        if let Some(s) = sort {
            body["sort"] = s;
        }
        let v = es.search(catalog::CATALOG_INDEX, &body)?;
        Ok(v.pointer("/hits/hits")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| h.get("_source").cloned())
                    .collect()
            })
            .unwrap_or_default())
    };
    let datasets = fetch(
        json!({"term": {"doc_kind": "dataset"}}),
        500,
        Some(json!([{"record_count": "desc"}])),
    )?;
    let mut runs = fetch(json!({"term": {"doc_kind": "run"}}), 50, None)?;
    runs.sort_by_key(|r| {
        std::cmp::Reverse(
            r.get("started")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        )
    });
    Ok(CatalogSummary {
        run: runs.into_iter().next(),
        datasets,
    })
}

// ─── status subcommand ───────────────────────────────────────────────────

fn run_status(cfg: StatusCfg) -> Result<i32> {
    // journals
    let dirs: Vec<std::path::PathBuf> = match &cfg.state_dir {
        Some(d) => vec![d.clone()],
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let base = Path::new(&home).join(".xerj").join("autoindex");
            std::fs::read_dir(&base)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default()
        }
    };
    for d in dirs {
        let jp = d.join("journal.ndjson");
        if !jp.exists() {
            continue;
        }
        let mut root = String::new();
        let mut done = 0u64;
        let mut records = 0u64;
        let mut finished = false;
        let mut generation: Option<u64> = None;
        let mut generation_files: Option<u64> = None;
        let mut generation_records: Option<u64> = None;
        let mut graph_line: Option<String> = None;
        if let Ok(f) = std::fs::File::open(&jp) {
            use std::io::BufRead;
            for line in std::io::BufReader::new(f).lines().map_while(|l| l.ok()) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    match v.get("kind").and_then(|k| k.as_str()) {
                        Some("run") => {
                            root = v.get("root").and_then(|r| r.as_str()).unwrap_or("").into()
                        }
                        Some("file_done") => {
                            done += 1;
                            records += v.get("records").and_then(|r| r.as_u64()).unwrap_or(0);
                        }
                        Some("finish") => {
                            finished = true;
                            generation = v.pointer("/summary/generation").and_then(Value::as_u64);
                            generation_files =
                                v.pointer("/summary/files_indexed").and_then(Value::as_u64);
                            generation_records =
                                v.pointer("/summary/records_total").and_then(Value::as_u64);
                            // Latest finish wins — the summary embeds the run
                            // doc, whose `graph` block is the edge count of
                            // record for this journal.
                            if let Some(g) =
                                v.pointer("/summary/graph").filter(|graph| !graph.is_null())
                            {
                                graph_line = Some(format!(
                                    "graph: {} edges written to {} (brain {})",
                                    g.get("edges_written").and_then(Value::as_u64).unwrap_or(0),
                                    g.get("edges_index").and_then(Value::as_str).unwrap_or("?"),
                                    g.get("brain").and_then(Value::as_str).unwrap_or("?"),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        println!(
            "journal {} — root {} — {} files done, {} records, {}{}",
            jp.display(),
            root,
            generation_files.unwrap_or(done),
            generation_records.unwrap_or(records),
            if finished { "FINISHED" } else { "in progress" },
            generation
                .map(|generation| format!(" (generation {generation})"))
                .unwrap_or_default()
        );
        if let Some(line) = graph_line {
            println!("  {line}");
        }
    }
    // live indices
    if let Ok(es) = Es::new(&cfg.url, cfg.api_key.clone()) {
        if es.ping().is_ok() {
            let pat = format!("{}-", cfg.prefix);
            println!("\nlive indices at {}:", cfg.url);
            for (name, docs) in es.cat_indices().unwrap_or_default() {
                if name.starts_with(&pat) || name == catalog::CATALOG_INDEX {
                    println!("  {name:<40} {docs:>10} docs");
                }
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod section_label_tests {
    use super::section_label;

    /// The two text-section locator grammars (§6.6.2) and their labels; every
    /// other locator shape must be None so row/line/byte records never reach
    /// `detect_text`.
    #[test]
    fn labels_only_text_section_locators() {
        assert_eq!(section_label("s0").as_deref(), Some("section 0"));
        assert_eq!(section_label("s17").as_deref(), Some("section 17"));
        assert_eq!(section_label("p1-s0").as_deref(), Some("page 1 section 0"));
        assert_eq!(
            section_label("p12-s3").as_deref(),
            Some("page 12 section 3")
        );
        for not_a_section in [
            "s", "sx", "s1x", "p1", "p1-s", "p-s1", "px-s1", "b1024", "row7", "file", "line3",
            "p1-s2-x",
        ] {
            assert_eq!(section_label(not_a_section), None, "{not_a_section}");
        }
    }
}

#[cfg(test)]
mod unsafe_fresh_generation_tests {
    use super::*;

    #[test]
    fn cli_error_routing_separates_typed_json_from_unrelated_human_errors() {
        let typed: anyhow::Error = UnsafeFreshGenerationError {
            committed_generation: Some(7),
        }
        .into();
        let route = route_cli_error(&typed, true);
        assert_eq!(route.exit_code, 1);
        assert!(route.stderr.is_none());
        let stdout = route.stdout.unwrap();
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["schema"], "xerj.autoindex.unsafe_fresh_generation.v1");
        assert_eq!(value["error"], "unsafe_fresh_existing_generation");
        assert_eq!(value["blocking_state"]["kind"], "committed_generation");
        assert_eq!(value["blocking_state"]["generation"], 7);
        // The refusal must never print an empty inventory delta it never
        // computed: it names the durable state that is in the way instead.
        assert!(value.get("added_content_groups").is_none());
        assert!(format!("{typed:#}").contains("committed corpus generation 7"));

        let pending: anyhow::Error = UnsafeFreshGenerationError {
            committed_generation: None,
        }
        .into();
        let pending_value: Value =
            serde_json::from_str(&route_cli_error(&pending, true).stdout.unwrap()).unwrap();
        assert_eq!(
            pending_value["blocking_state"]["kind"],
            "pending_generation"
        );

        let unrelated = anyhow::anyhow!("endpoint unavailable");
        let route = route_cli_error(&unrelated, true);
        assert_eq!(route.exit_code, 1);
        assert!(route.stdout.is_none());
        assert_eq!(route.stderr.as_deref(), Some("error: endpoint unavailable"));
    }
}

#[cfg(test)]
mod inventory_delta_tests {
    use super::*;
    use std::path::PathBuf;

    fn file(path: &str) -> walk::FileEntry {
        walk::FileEntry {
            path: PathBuf::from(path),
            rel: path.to_owned(),
            rel_id: format!("id:{path}"),
            is_symlink: false,
            size: 1,
        }
    }

    fn assignment(path: &str) -> FileAssignment {
        FileAssignment {
            rel: path.to_owned(),
            path_id: format!("id:{path}"),
            family: "csv".into(),
            gzip: false,
            content_digest: Some(format!("digest:{path}")),
            assignments: vec![(None, "rows".into())],
            as_document: false,
            is_symlink: Some(false),
        }
    }

    fn targets() -> RefusalTargets {
        RefusalTargets {
            state_dir: "/state".into(),
            data_indices: vec!["ax-rows".into()],
            edges_index: Some(".xerj-memory-corpus-edges".into()),
        }
    }

    /// A root under which no plan file exists, so every vanished group is a
    /// genuine deletion (#439) — the behaviour these classifier tests assert.
    fn nowhere() -> &'static Path {
        Path::new("/xerj-439-no-such-root")
    }

    #[test]
    fn only_a_removed_content_group_refuses_a_rerun() {
        let mut plan = Plan::default();
        plan.files.insert("keep".into(), assignment("keep.csv"));
        assert!(!UnsupportedInventoryDelta::between(
            nowhere(),
            &[file("keep.csv")],
            &["keep".into()],
            &plan
        )
        .refuses());
        // An added file is skipped by the frozen plan, not a refusal: the
        // documented rerun-then---fresh workflow has to keep working.
        let added = UnsupportedInventoryDelta::between(
            nowhere(),
            &[file("keep.csv"), file("new.csv")],
            &["keep".into(), "new".into()],
            &plan,
        );
        assert_eq!(added.added_content_groups.len(), 1);
        assert!(!added.refuses(), "an addition alone must not fail the run");
        // A removal leaves live documents with no source file behind them.
        assert!(UnsupportedInventoryDelta::between(nowhere(), &[], &[], &plan).refuses());
    }

    #[test]
    fn classifier_sorts_added_and_vanished_groups() {
        let mut plan = Plan::default();
        plan.files.insert("keep".into(), assignment("keep.csv"));
        plan.junk_files.push(JunkFile {
            file_key: "junk".into(),
            rel: "broken.pdf".into(),
            format: "pdf".into(),
            status: "junk".into(),
            reason: "fixture".into(),
            bytes: 1,
        });
        assert!(
            !UnsupportedInventoryDelta::between(
                nowhere(),
                &[file("keep.csv"), file("broken.pdf")],
                &["keep".into(), "junk".into()],
                &plan,
            )
            .refuses(),
            "unchanged durable junk is neither added nor vanished"
        );

        plan.files.insert("old-z".into(), assignment("z-old.csv"));
        plan.files.insert("old-a".into(), assignment("a-old.csv"));
        let delta = UnsupportedInventoryDelta::between(
            nowhere(),
            &[
                file("keep.csv"),
                file("broken.pdf"),
                file("z-new.csv"),
                file("m-new.csv"),
            ],
            &["keep".into(), "junk".into(), "new-z".into(), "new-m".into()],
            &plan,
        );
        assert_eq!(
            delta
                .added_content_groups
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["m-new.csv", "z-new.csv"]
        );
        assert_eq!(
            delta
                .deleted_content_groups
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["a-old.csv", "z-old.csv"]
        );
    }

    #[test]
    fn refusal_names_the_removed_files_and_every_recovery_route() {
        let mut plan = Plan::default();
        plan.files.insert("gone".into(), assignment("gone.csv"));
        let error =
            UnsupportedInventoryDelta::between(nowhere(), &[], &[], &plan).into_error(targets());
        let message = format!("{error:#}");
        assert!(message.contains("gone.csv"), "{message}");
        assert!(
            message.contains("no longer exist in the folder"),
            "{message}"
        );
        assert!(message.contains("made no remote mutations"), "{message}");
        assert!(message.contains("restore the DELETED file(s)"), "{message}");
        assert!(message.contains("ax-rows"), "{message}");
        assert!(message.contains("/state"), "{message}");
        assert!(message.contains(".xerj-memory-corpus-edges"), "{message}");
        assert!(message.contains("new --state-dir"), "{message}");
        assert!(message.contains("`--fresh`"), "{message}");
    }

    /// #439: a plan file that vanished from the walk but is STILL ON DISK was
    /// excluded by a widened ignore/hidden rule, not removed. It must classify
    /// as excluded (not deleted), and the refusal must not tell an operator (or
    /// an agent parsing the JSON) to restore a file that is one `ls` away.
    #[test]
    fn an_excluded_still_on_disk_group_is_not_reported_as_a_deletion() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("secret.csv"), "id,value\n1,live\n").unwrap();
        let mut plan = Plan::default();
        plan.files.insert("secret".into(), assignment("secret.csv"));

        // The walk yields nothing (the file is now excluded) though it is on disk.
        let delta = UnsupportedInventoryDelta::between(root.path(), &[], &[], &plan);
        assert!(
            delta.deleted_content_groups.is_empty(),
            "an on-disk file must not be classified as a deletion"
        );
        assert_eq!(
            delta
                .excluded_content_groups
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["secret.csv"]
        );
        // #589: an excluded-only delta is now SWEPT, not refused — the rerun
        // continues after `sweep_excluded_groups` removes its documents, so
        // `refuses()` (deletion-only) is false here. `into_error`'s excluded
        // wording still applies when a genuine deletion co-occurs (both sets
        // stay live in that case), so it is still exercised below as a direct
        // message-builder check.
        assert!(
            !delta.refuses(),
            "#589: an excluded-only delta is swept, not refused"
        );
        let error = delta.into_error(targets());
        let message = format!("{error:#}");
        assert!(message.contains("secret.csv"), "{message}");
        assert!(
            message.contains("still on disk"),
            "the excluded message must say the file is still on disk: {message}"
        );
        assert!(
            !message.contains("restore the DELETED"),
            "an excluded (never-removed) file must not be offered as a deletion to restore: {message}"
        );
        // The machine-readable schema carries the split and the excluded route,
        // and omits the restore instruction when nothing was actually deleted.
        let json = error
            .downcast_ref::<UnsupportedInventoryDeltaError>()
            .expect("typed refusal")
            .to_json();
        assert_eq!(json["excluded_content_groups"].as_array().unwrap().len(), 1);
        assert!(json["recovery"].get("excluded_not_removed").is_some());
        assert!(
            json["recovery"].get("restore_removed_files").is_none(),
            "no deleted files, so no restore instruction: {json}"
        );
        // Back-compat: the v1 union field still lists it.
        assert_eq!(json["vanished_content_groups"].as_array().unwrap().len(), 1);
    }

    /// #589 (fail-before, WIP): the purge half of #439. An exclusion (a file
    /// still on disk that a widened ignore/hidden/`.xerjignore` rule now skips)
    /// is data the exclusion must REMOVE from the index, not a reason to refuse
    /// the whole rerun. With NO genuine deletion present, the reconcile must
    /// PROCEED — sweeping the excluded group's published documents — unlike a
    /// real deletion, which stays refused. Today `refuses()` conflates the two.
    ///
    /// Un-ignore when the sweep lands; that change also flips the
    /// `assert!(delta.refuses())` in
    /// `an_excluded_still_on_disk_group_is_not_reported_as_a_deletion` (the
    /// excluded-only case no longer produces a refusal).
    #[test]
    fn an_excluded_only_delta_is_swept_not_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("secret.csv"), "id,value\n1,live\n").unwrap();
        let mut plan = Plan::default();
        plan.files.insert("secret".into(), assignment("secret.csv"));

        // The walk yields nothing (the file is now excluded) though it is on disk.
        let delta = UnsupportedInventoryDelta::between(root.path(), &[], &[], &plan);
        assert!(
            delta.deleted_content_groups.is_empty(),
            "no genuine deletion — the file is still on disk"
        );
        assert_eq!(
            delta
                .excluded_content_groups
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            ["secret.csv"],
        );
        // #589: an excluded-only delta must NOT refuse the rerun — the excluded
        // group is swept from the destination and the run continues.
        assert!(
            !delta.refuses(),
            "#589: an excluded-only delta must be swept, not refused"
        );
    }

    /// #439's headline case (CHANGELOG): a hidden NON-UTF-8 name still on disk.
    /// Its `rel` is a lossy `U+FFFD` rendering that matches no dirent, so a probe
    /// through `rel` would call it a deletion; the reversible `path_id` matches
    /// the real bytes and classifies it excluded.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_excluded_name_is_matched_on_disk_via_path_id() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let raw = b".secret_\xff\xfe.csv";
        let name = std::ffi::OsStr::from_bytes(raw);
        std::fs::write(root.path().join(name), "id\n1\n").unwrap();
        let rel_lossy = std::path::Path::new(name).to_string_lossy().into_owned();
        let mut path_id = String::from("unix:");
        for byte in raw {
            use std::fmt::Write;
            write!(path_id, "{byte:02x}").unwrap();
        }
        let mut plan = Plan::default();
        plan.files.insert(
            "secret".into(),
            FileAssignment {
                rel: rel_lossy.clone(),
                path_id,
                family: "csv".into(),
                gzip: false,
                content_digest: Some("digest".into()),
                assignments: vec![(None, "rows".into())],
                as_document: false,
                is_symlink: Some(false),
            },
        );
        // Proof that path_id is load-bearing: the lossy rel does NOT resolve.
        assert!(
            !root.path().join(&rel_lossy).try_exists().unwrap_or(false),
            "the lossy rel must not resolve on disk — that is why path_id is needed"
        );
        let delta = UnsupportedInventoryDelta::between(root.path(), &[], &[], &plan);
        assert!(
            delta.deleted_content_groups.is_empty(),
            "a non-UTF-8 name still on disk must not be a deletion: {:?}",
            delta.deleted_content_groups
        );
        assert_eq!(delta.excluded_content_groups.len(), 1);
    }

    /// #439 robustness: a corrupted or hand-edited `path_id` must return None
    /// (so the caller falls back to `rel`), never panic — including a non-ASCII
    /// byte inside the hex region, which naive `hex[i..i + 2]` slicing panics on.
    #[test]
    fn decode_stable_path_id_rejects_malformed_ids_without_panicking() {
        assert!(decode_stable_path_id("").is_none());
        assert!(decode_stable_path_id("id:legacy").is_none());
        #[cfg(unix)]
        {
            assert_eq!(
                decode_stable_path_id("unix:2e63").unwrap(),
                std::path::PathBuf::from(".c")
            );
            assert!(decode_stable_path_id("unix:2").is_none(), "odd length");
            assert!(decode_stable_path_id("unix:zz").is_none(), "non-hex");
            assert!(
                decode_stable_path_id("unix:aéb").is_none(),
                "multibyte char in hex must not panic"
            );
        }
    }

    /// The prose refusal is bounded by REFUSAL_LIST_CAP, not by the corpus: an
    /// unmounted bind mount under an indexed root vanishes every group at once,
    /// and rendering 82k entries into one String is megabytes of stderr in the
    /// one path whose job is to be read. `--json` still carries all of them.
    #[test]
    fn refusal_prose_caps_its_listings_while_json_keeps_every_entry() {
        let mut plan = Plan::default();
        let total = REFUSAL_LIST_CAP * 3;
        for i in 0..total {
            plan.files.insert(
                format!("gone{i:03}"),
                assignment(&format!("gone{i:03}.csv")),
            );
        }
        let error =
            UnsupportedInventoryDelta::between(nowhere(), &[], &[], &plan).into_error(targets());
        let message = format!("{error:#}");
        assert!(message.contains("gone000.csv"), "{message}");
        assert!(
            message.contains(&format!(", … and {} more", total - REFUSAL_LIST_CAP)),
            "{message}"
        );
        // The tail past the cap must not be rendered at all, not merely elided.
        let last = format!("gone{:03}.csv", total - 1);
        assert!(!message.contains(&last), "{message}");
        assert_eq!(
            message.matches(".csv (").count(),
            REFUSAL_LIST_CAP,
            "{message}"
        );

        let stdout = route_cli_error(&error, true)
            .stdout
            .expect("the typed refusal renders as JSON under --json");
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            value["vanished_content_groups"].as_array().unwrap().len(),
            total
        );
    }

    #[test]
    fn refusal_prose_below_the_cap_has_no_more_tail() {
        let mut plan = Plan::default();
        plan.files.insert("gone".into(), assignment("gone.csv"));
        let error =
            UnsupportedInventoryDelta::between(nowhere(), &[], &[], &plan).into_error(targets());
        let message = format!("{error:#}");
        assert!(!message.contains("… and "), "{message}");
    }

    #[test]
    fn cli_error_routing_separates_typed_json_from_unrelated_human_errors() {
        let typed = UnsupportedInventoryDelta {
            added_content_groups: Vec::new(),
            deleted_content_groups: vec![InventoryDeltaEntry {
                file_key: "key".into(),
                path: "gone.csv".into(),
            }],
            excluded_content_groups: Vec::new(),
        }
        .into_error(targets());
        let route = route_cli_error(&typed, true);
        assert_eq!(route.exit_code, 1);
        assert!(route.stderr.is_none());
        let stdout = route.stdout.unwrap();
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["schema"], "xerj.autoindex.unsupported_sync_delta.v1");
        assert_eq!(value["error"], "unsupported_content_group_removal");
        assert_eq!(value["vanished_content_groups"][0]["path"], "gone.csv");
        assert!(value["recovery"]["rebuild_in_place"]
            .as_str()
            .unwrap()
            .contains("ax-rows"));

        let unrelated = anyhow::anyhow!("endpoint unavailable");
        let route = route_cli_error(&unrelated, true);
        assert_eq!(route.exit_code, 1);
        assert!(route.stdout.is_none());
        assert_eq!(route.stderr.as_deref(), Some("error: endpoint unavailable"));
    }
}

#[cfg(test)]
mod duplicate_integration_tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::io::Write;

    fn legacy_assignment(rel: &str) -> FileAssignment {
        FileAssignment {
            rel: rel.to_string(),
            path_id: String::new(),
            is_symlink: None,
            family: "txt".to_string(),
            gzip: false,
            content_digest: None,
            assignments: vec![(None, "text".to_string())],
            as_document: false,
        }
    }

    #[test]
    fn legacy_prefix_collision_has_one_deterministic_owner() {
        let corpus = tempfile::tempdir().unwrap();
        let mut a = vec![b'x'; 65_537];
        let mut b = a.clone();
        a[65_536] = b'a';
        b[65_536] = b'b';
        fs::write(corpus.path().join("a.txt"), a).unwrap();
        fs::write(corpus.path().join("b.txt"), b).unwrap();
        let files = walk::walk(corpus.path(), false).unwrap();
        let inventory = content::resolve_reporting(files.clone(), &|_| {}).unwrap();
        let legacy = ids::file_key(&files[0].path, files[0].size).unwrap();
        assert_eq!(
            legacy,
            ids::file_key(&files[1].path, files[1].size).unwrap()
        );

        // The exact historical owner sorts second. It must retain the legacy
        // key; the earlier collision sibling must never steal or share it.
        let mut plan = Plan::default();
        plan.files
            .insert(legacy.clone(), legacy_assignment("b.txt"));
        let error = select_resume_plan_keys(
            &inventory.files,
            &inventory.keys,
            &plan,
            Path::new("/state/journal.ndjson"),
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("collides with legacy resume key"));
        // Recovery advice must stay scoped to the two colliding files and
        // keep an exact rebuild isolated from the old destination.
        assert!(message.contains("remove or move one of these two files"));
        assert!(message.contains("/state/journal.ndjson"));
        assert!(message.contains("new --state-dir, new --prefix, and new --brain"));
    }

    #[test]
    fn planned_key_claimed_by_path_diverts_the_content_claimant_deterministically() {
        let corpus = tempfile::tempdir().unwrap();
        // a.txt was planned under its old digest; its content has since
        // changed, while b.txt now holds exactly the bytes a.txt was planned
        // with — so b.txt's content key IS the planned key a.txt claims by rel.
        fs::write(corpus.path().join("a.txt"), b"rewritten content\n").unwrap();
        fs::write(corpus.path().join("b.txt"), b"original planned content\n").unwrap();
        let inventory =
            content::resolve_reporting(walk::walk(corpus.path(), false).unwrap(), &|_| {}).unwrap();
        let planned_key = inventory.keys[1].clone();
        let mut plan = Plan::default();
        plan.files
            .insert(planned_key.clone(), legacy_assignment("a.txt"));

        let selected = select_resume_plan_keys(
            &inventory.files,
            &inventory.keys,
            &plan,
            Path::new("/state/journal.ndjson"),
        )
        .unwrap();
        assert_eq!(selected[0].as_deref(), Some(planned_key.as_str()));
        let diverted = selected[1].as_deref().expect("diverted key");
        assert_ne!(diverted, planned_key);
        assert!(diverted.starts_with(&format!("{planned_key}-claimed-")));
        // The divergence is a pure function of (digest, path identity):
        // resumes select the same exclusive owner and the same diverted key.
        let again = select_resume_plan_keys(
            &inventory.files,
            &inventory.keys,
            &plan,
            Path::new("/state/journal.ndjson"),
        )
        .unwrap();
        assert_eq!(selected, again);
    }

    #[test]
    fn one_alias_change_invalidates_only_its_content_key() {
        let alias = |file_key: &str, rel: &str| state::DuplicateFile {
            file_key: file_key.to_string(),
            rel: rel.to_string(),
            path_id: format!("id:{rel}"),
            is_symlink: Some(false),
            duplicate_of: format!("{file_key}.txt"),
            bytes: 10,
        };
        let previous = vec![alias("a", "a-copy.txt"), alias("c", "c-copy.txt")];
        let current = vec![
            alias("a", "a-copy.txt"),
            alias("b", "b-copy.txt"),
            alias("c", "c-copy.txt"),
        ];
        assert_eq!(
            alias_keys_to_reindex(&previous, &current, None),
            HashSet::from(["b".to_string()])
        );
        assert_eq!(
            alias_keys_to_reindex(&current, &previous, None),
            HashSet::from(["b".to_string()])
        );
        assert_eq!(
            alias_keys_to_reindex(
                &previous,
                &current,
                Some(&["a".to_string(), "b".to_string(), "c".to_string()])
            ),
            HashSet::from(["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    /// #487: an UNCHANGED re-index on a backend with `resumable == false`
    /// (`neural`/`proxy`) must report the resume LIMITATION, not a false
    /// "identity changed" — and must surface the backend's own reason rather
    /// than sending the user to a remedy (restore the identity) that cannot work.
    #[test]
    fn unchanged_reindex_on_non_resumable_backend_reports_resume_limit_not_identity_change() {
        let current = crate::esclient::EmbeddingExecutionIdentity {
            version: 1,
            backend: "neural".into(),
            identity_sha256: "6e51e5ce3e46".into(),
            dimensions: None,
            semantic_contract: "semantic_text-derived-vector.v1".into(),
            resumable: false,
            non_resumable_reason: Some("identity is derived from the model name".into()),
        };
        // All comparable fields match → the identity did NOT change.
        let err = ensure_embedding_execution_unchanged_and_resumable(
            &current,
            "6e51e5ce3e46",
            "neural",
            None,
            "semantic_text-derived-vector.v1",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("cannot resume") && !err.contains("identity changed"),
            "must report the resume limitation, not a false identity change: {err}"
        );
        assert!(
            err.contains("derived from the model name"),
            "should surface the backend's own non_resumable_reason: {err}"
        );
        // A GENUINE change still reports "identity changed".
        let changed = crate::esclient::EmbeddingExecutionIdentity {
            identity_sha256: "DIFFERENT".into(),
            resumable: true,
            ..current.clone()
        };
        let err2 = ensure_embedding_execution_unchanged_and_resumable(
            &changed,
            "6e51e5ce3e46",
            "neural",
            None,
            "semantic_text-derived-vector.v1",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err2.contains("identity changed"),
            "a real identity change must still report it: {err2}"
        );
    }

    #[test]
    fn duplicate_content_keeps_journal_and_live_id_cardinality_equal_on_resume() {
        let corpus = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let body = "quarterly revenue was 42\noperating income was 7\n";
        fs::write(corpus.path().join("report-original.txt"), body).unwrap();
        fs::write(corpus.path().join("report-copy.txt"), body).unwrap();

        let discovered = walk::walk(corpus.path(), false).unwrap();
        let inventory = content::resolve_reporting(discovered, &|_| {}).unwrap();
        assert_eq!(inventory.files.len(), 1);
        assert_eq!(inventory.duplicates.len(), 1);

        let mut live_ids = HashSet::new();
        let mut records = 0u64;
        let sniffed = sniff::sniff(&inventory.files[0].path).unwrap();
        extract::extract(&inventory.files[0].path, &sniffed, None, &mut |record| {
            live_ids.insert(ids::doc_id("text", &inventory.keys[0], &record.locator));
            records += 1;
            true
        })
        .unwrap();

        let mut journal = state::Journal::open(
            state_dir.path(),
            "corpus",
            "http://engine",
            "test",
            300,
            false,
        )
        .unwrap();
        journal
            .write_plan(&Plan {
                duplicate_files: inventory.duplicates.clone(),
                ..Plan::default()
            })
            .unwrap();
        journal
            .file_done(&FileDone {
                file_key: inventory.keys[0].clone(),
                path: inventory.files[0].rel.clone(),
                records,
                junk: 0,
                bytes: inventory.files[0].size,
                dropped_by_dataset: HashMap::new(),
                generation: Some(inventory.digests[0].clone()),
            })
            .unwrap();
        drop(journal);

        let resumed = state::Journal::open(
            state_dir.path(),
            "corpus",
            "http://engine",
            "test",
            300,
            false,
        )
        .unwrap();
        assert!(resumed.resumed);
        assert_eq!(
            resumed.done.values().map(|f| f.records).sum::<u64>(),
            records
        );
        assert_eq!(records as usize, live_ids.len());
        let done = resumed.done_keys();
        assert!(inventory.keys.iter().all(|key| done.contains(key)));
        let aliases = &resumed.plan.unwrap().duplicate_files;
        assert_eq!(aliases, &inventory.duplicates);
    }

    #[test]
    fn mutation_after_more_than_one_bulk_is_staged_and_retry_replaces_stale_locators() {
        let corpus = tempfile::tempdir().unwrap();
        let path = corpus.path().join("large.csv");
        let mut csv = String::from("id,value\n");
        for id in 0..6_001 {
            csv.push_str(&format!("{id},old-{id}\n"));
        }
        fs::write(&path, csv).unwrap();

        let inventory =
            content::resolve_reporting(walk::walk(corpus.path(), false).unwrap(), &|_| {}).unwrap();
        let expected_size = inventory.files[0].size;
        let expected_digest = inventory.digests[0].clone();
        let sniffed = sniff::sniff(&path).unwrap();
        let mut staged = tempfile::NamedTempFile::new().unwrap();
        let mut staged_docs = 0usize;
        extract::extract(&path, &sniffed, None, &mut |record| {
            writeln!(
                staged,
                "{}\n{}",
                record.locator,
                Value::Object(record.fields)
            )
            .unwrap();
            staged_docs += 1;
            if staged_docs == 5_001 {
                // A shorter source replaces the file while extraction is in
                // progress, after the production 5,000-document bulk cut.
                fs::write(&path, "id,value\n0,new-0\n1,new-1\n").unwrap();
            }
            true
        })
        .unwrap();
        assert!(staged_docs > 5_000);

        let mut live: HashSet<String> = (0..6_001).map(|id| format!("row:{id}")).collect();
        assert!(content::verify(&path, expected_size, &expected_digest).is_err());
        // Verification precedes delete/visibility, so a rejected attempt has
        // not mixed any staged records into the old live set.
        assert_eq!(live.len(), 6_001);

        // The retry's delete-before-replace removes every old locator before
        // the now-short source becomes visible.
        live.clear();
        let retry_sniffed = sniff::sniff(&path).unwrap();
        extract::extract(&path, &retry_sniffed, None, &mut |record| {
            live.insert(record.locator);
            true
        })
        .unwrap();
        assert_eq!(live, HashSet::from(["r0".into(), "r1".into()]));
    }
}

#[cfg(test)]
mod map_metadata_tests {
    use super::*;

    fn assignment(slugs: &[&str]) -> FileAssignment {
        FileAssignment {
            rel: "report.dat".into(),
            path_id: "path:report.dat".into(),
            is_symlink: Some(false),
            family: "json".into(),
            gzip: false,
            content_digest: Some("digest".into()),
            assignments: slugs
                .iter()
                .enumerate()
                .map(|(group, slug)| (Some(format!("group-{group}")), (*slug).to_string()))
                .collect(),
            as_document: false,
        }
    }

    #[test]
    fn unchanged_resume_keeps_durable_assignment_aware_dataset_bytes() {
        let _guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let state_dir = tempfile::tempdir().unwrap();
        let plan = Plan {
            files: HashMap::from([
                (
                    "shared".into(),
                    assignment(&["quarterly", "quarterly", "annual"]),
                ),
                ("quarterly-only".into(), assignment(&["quarterly"])),
            ]),
            ..Plan::default()
        };
        let mut initial = state::Journal::open(
            state_dir.path(),
            "corpus",
            "http://engine",
            "ax",
            300,
            false,
        )
        .unwrap();
        initial.write_plan(&plan).unwrap();
        initial
            .file_done(&FileDone {
                file_key: "shared".into(),
                path: "report.dat".into(),
                records: 10,
                junk: 5,
                bytes: 100,
                dropped_by_dataset: HashMap::from([("quarterly".into(), 7), ("annual".into(), 3)]),
                generation: Some("digest".into()),
            })
            .unwrap();
        initial
            .file_done(&FileDone {
                file_key: "quarterly-only".into(),
                path: "quarterly.json".into(),
                records: 2,
                junk: 2,
                bytes: 23,
                dropped_by_dataset: HashMap::from([("quarterly".into(), 11)]),
                generation: Some("digest-2".into()),
            })
            .unwrap();
        let before_resume = durable_dataset_stats(&plan, &initial.done);
        drop(initial);

        // Opening the same durable run appends only an invocation-level
        // resume record. No source is processed and no FileDone is appended.
        let resumed = state::Journal::open(
            state_dir.path(),
            "corpus",
            "http://engine",
            "ax",
            300,
            false,
        )
        .unwrap();
        let after_unchanged_resume =
            durable_dataset_stats(resumed.plan.as_ref().unwrap(), &resumed.done);

        assert_eq!(before_resume, after_unchanged_resume);
        assert_eq!(after_unchanged_resume["quarterly"].bytes, 123);
        assert_eq!(after_unchanged_resume["annual"].bytes, 100);
        assert_eq!(after_unchanged_resume["quarterly"].junk, 7);
        assert_eq!(after_unchanged_resume["annual"].junk, 0);
        assert_eq!(after_unchanged_resume["quarterly"].dropped, 18);
        assert_eq!(after_unchanged_resume["annual"].dropped, 3);
    }

    #[test]
    fn invocation_started_is_not_derived_from_summary_generation() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-08-04T00:29:05.673023686Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let summary_generated_at =
            chrono::DateTime::parse_from_rfc3339("2026-08-04T00:31:37.937589283Z")
                .unwrap()
                .with_timezone(&chrono::Utc);

        let (reported_started, reported_summary_generated_at) =
            invocation_report_timestamps(started, summary_generated_at);

        assert_eq!(reported_started, "2026-08-04T00:29:05.673023686+00:00");
        assert_eq!(
            reported_summary_generated_at,
            "2026-08-04T00:31:37.937589283+00:00"
        );
        assert_ne!(reported_started, reported_summary_generated_at);
    }

    #[test]
    fn catalog_mapping_declares_latest_invocation_telemetry_fields() {
        let mapping = catalog::catalog_mapping();
        assert_eq!(
            mapping.pointer("/mappings/properties/started/type"),
            Some(&json!("date"))
        );
        assert_eq!(
            mapping.pointer("/mappings/properties/summary_generated_at/type"),
            Some(&json!("date"))
        );
        assert_eq!(
            mapping.pointer("/mappings/properties/invocation_telemetry_scope/type"),
            Some(&json!("keyword"))
        );
        assert_eq!(
            mapping.pointer("/mappings/properties/started/format"),
            Some(&json!("strict_date_optional_time||epoch_millis"))
        );
        assert_eq!(
            mapping.pointer("/mappings/properties/summary_generated_at/format"),
            Some(&json!("strict_date_optional_time||epoch_millis"))
        );
    }
}

#[cfg(test)]
mod generation_contract_identity_tests {
    use super::*;

    fn dataset(slug: &str, index: &str, family: &str) -> PlanDataset {
        PlanDataset {
            slug: slug.into(),
            index: index.into(),
            family: family.into(),
            group: None,
            specs: Vec::new(),
            time_field: None,
            semantic_field: None,
            sampled_records: 0,
            file_count: 0,
        }
    }

    #[test]
    fn contract_identities_are_order_independent_and_change_with_contracts() {
        let mut first = Plan {
            datasets: vec![
                dataset("b", "prefix-b", "csv"),
                dataset("a", "prefix-a", "json"),
            ],
            ..Plan::default()
        };
        let expected = generation_contract_identities(&first).unwrap();
        first.datasets.reverse();
        assert_eq!(generation_contract_identities(&first).unwrap(), expected);

        first.datasets[0].family = "text".into();
        let schema_changed = generation_contract_identities(&first).unwrap();
        assert_ne!(schema_changed.0, expected.0);

        first.datasets[0].family = "json".into();
        first.datasets[0].index = "different-a".into();
        let index_changed = generation_contract_identities(&first).unwrap();
        assert_eq!(index_changed.0, expected.0);
        assert_ne!(index_changed.1, expected.1);
    }

    /// ONBOARDING-401-REPRO.md §3: a *successful* run signed off by printing
    /// `next:` commands that carried no credential, so against an auth-enabled
    /// server (the default) every one of them answered 401 — including the
    /// `xerj autoindex map` line `xerj brain` prints after "your second brain
    /// is ready". The run's own key and url must ride into the hints.
    #[test]
    fn next_hint_carries_the_runs_credential() {
        // Key came from `--api-key` (or a file), not the environment: print it
        // literally, because nothing else in the user's shell holds it.
        let hint = next_hint("http://localhost:9510", "ax", Some("s3cret"), None, None);
        assert!(
            hint.contains("--api-key \"s3cret\""),
            "map hint must carry the key, got: {hint}"
        );
        assert!(
            hint.contains("Authorization: ApiKey s3cret"),
            "search hint must carry the key, got: {hint}"
        );
        assert!(
            hint.contains("http://localhost:9510/ax-*/_search"),
            "search hint must target the run's url and prefix, got: {hint}"
        );
        // Nothing is left as an un-runnable `GET /…` sketch.
        assert!(
            !hint.contains("search via GET"),
            "hints must be runnable commands, got: {hint}"
        );

        // The key is already exported: reference the variable instead of
        // echoing the admin secret into a banner people paste into issues.
        let hint = next_hint(
            "http://localhost:9200",
            "ax",
            Some("s3cret"),
            Some("s3cret"),
            None,
        );
        assert!(
            !hint.contains("s3cret"),
            "must not echo a secret already in the environment, got: {hint}"
        );
        assert!(
            hint.contains("--api-key \"$XERJ_API_KEY\"")
                && hint.contains("Authorization: ApiKey $XERJ_API_KEY"),
            "must reference the exported variable, got: {hint}"
        );

        // A different value in the environment is not the run's credential.
        let hint = next_hint(
            "http://localhost:9200",
            "ax",
            Some("s3cret"),
            Some("other"),
            None,
        );
        assert!(
            hint.contains("--api-key \"s3cret\""),
            "a stale env var must not shadow the run's key, got: {hint}"
        );

        // Open server: no credential to carry, and none invented.
        let hint = next_hint("http://localhost:9200", "ax", None, Some("ignored"), None);
        assert!(
            !hint.contains("api-key") && !hint.contains("Authorization"),
            "an auth-free server must get auth-free hints, got: {hint}"
        );
        assert!(hint.contains("xerj autoindex map --url http://localhost:9200"));
    }

    /// A blind onboarding run found `autoindex` discovering `./data/admin.key`
    /// on its own and then printing that admin key verbatim in its completion
    /// banner. The reader never typed the secret, so echoing it into copyable
    /// output is a disclosure — and this banner is exactly what people paste
    /// into bug reports. Reference the file instead; `$(cat …)` still pastes.
    #[test]
    fn a_discovered_key_is_referenced_by_path_never_echoed() {
        let path = std::path::Path::new("./data/admin.key");
        let hint = next_hint(
            "http://localhost:9200",
            "ax",
            Some("s3cret"),
            None,
            Some(path),
        );
        assert!(
            !hint.contains("s3cret"),
            "a key discovered on disk must never be echoed, got: {hint}"
        );
        assert!(
            hint.contains("$(cat ./data/admin.key)"),
            "the hint must read the key back from its file, got: {hint}"
        );
    }

    #[test]
    fn internal_contract_versions_are_explicit() {
        assert_eq!(PREPARED_RECORDS_IDENTITY, "prepared-records-v1");
        assert_eq!(DOCUMENT_IDS_IDENTITY, "document-ids-v1");
        assert_eq!(DETECTOR_DISABLED_IDENTITY, "disabled");
    }
}

#[cfg(test)]
mod code_coverage_tests {
    use super::*;

    fn captured(buffer: &std::sync::Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().unwrap().clone()).unwrap()
    }

    /// The exact line that made #294 survivable for a whole release: over a
    /// corpus of three source files and one `.md`, with every source file
    /// junked, the run printed `files=4 records=1` — indistinguishable from a
    /// healthy one-file corpus. Coverage plus a warning is what makes those
    /// two runs print different lines.
    #[test]
    fn the_generated_terminal_line_carries_coverage_and_warns_when_no_code_indexed() {
        let (pr, buffer) = progress::Progress::capture(
            progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        finish_generated_progress(
            &pr,
            3,
            &json!({
                "files_indexed": 4,
                "records_total": 1,
                "generation": 1,
                "code_files": 3,
                "code_files_indexed": 0,
                "code_files_junked": 3,
            }),
        );
        let text = captured(&buffer);
        let done = text
            .lines()
            .find(|line| line.starts_with("xerj-done "))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            done.contains("code_files=3 code_files_indexed=0 code_files_junked=3"),
            "{done}"
        );
        assert!(
            text.lines()
                .any(|line| line.starts_with("warning:") && line.contains("NONE")),
            "a corpus that indexed no source code is warned about in words: {text}"
        );
        assert!(
            text.trim_end()
                .lines()
                .next_back()
                .unwrap()
                .starts_with("xerj-done "),
            "the terminal line stays terminal — the warning precedes it: {text}"
        );
    }

    #[test]
    fn a_healthy_corpus_reports_its_coverage_without_a_warning() {
        let (pr, buffer) = progress::Progress::capture(
            progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        finish_generated_progress(
            &pr,
            0,
            &json!({
                "files_indexed": 4,
                "records_total": 8,
                "generation": 1,
                "code_files": 3,
                "code_files_indexed": 3,
                "code_files_junked": 0,
            }),
        );
        let text = captured(&buffer);
        assert!(
            text.contains("code_files=3 code_files_indexed=3 code_files_junked=0"),
            "{text}"
        );
        assert!(!text.contains("warning:"), "{text}");
    }

    /// A corpus with no source code in it at all must not start warning, and a
    /// gzipped source file is still a source file.
    #[test]
    fn coverage_counts_families_not_compression_and_stays_quiet_without_code() {
        let mut coverage = CodeCoverage::default();
        coverage.observe("txt-prose", 4);
        coverage.observe("csv", 0);
        assert_eq!(coverage, CodeCoverage::default());
        assert!(coverage.warning().is_none());

        coverage.observe("code", 1);
        coverage.observe("code(gzip)", 0);
        assert_eq!(
            (coverage.files, coverage.indexed, coverage.junked),
            (2, 1, 1)
        );
        assert!(
            coverage.warning().is_none(),
            "partial loss is reported by the counters, not by the warning"
        );
        assert_eq!(
            coverage.fields(),
            [
                ("code_files", 2),
                ("code_files_indexed", 1),
                ("code_files_junked", 1)
            ]
        );
    }
}

#[cfg(test)]
mod failure_resume_http_tests;
#[cfg(test)]
mod incremental_reconcile_http_tests;

/// The Unity PIPELINE half — `build_unity_guid_map` + `enrich_unity_fields`
/// + the plan's field registration, driven through the real phase-A planner.
///
/// The PR that introduced Unity support tested `extract::unity::*` thoroughly
/// and this half not at all, which is why the sample-conditional mapping
/// registration survived review: no unit test of an extractor can see a bug
/// whose cause is what phase A sampled.
#[cfg(test)]
mod unity_pipeline_tests {
    use super::*;

    const GUID: &str = "abc123def4560000";

    /// A scene whose MonoBehaviour group's FIRST document has no `m_Script`
    /// and whose SECOND one does. With `sample: 1`, phase A therefore never
    /// sees `script_guid` — but phase B still stamps `script_path` from the
    /// second record. That gap is the bug.
    fn scene_with_late_script_ref() -> String {
        format!(
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n\
             --- !u!1 &1\nGameObject:\n  m_Name: Player\n\
             --- !u!114 &2\nMonoBehaviour:\n  m_Name: NoScriptYet\n  speed: 1\n\
             --- !u!114 &3\nMonoBehaviour:\n  m_Name: HasScript\n  \
             m_Script: {{fileID: 11500000, guid: {GUID}, type: 3}}\n"
        )
    }

    fn write_project(dir: &Path) {
        std::fs::create_dir_all(dir.join("Assets/Scripts")).unwrap();
        std::fs::create_dir_all(dir.join("Assets/Scenes")).unwrap();
        std::fs::write(
            dir.join("Assets/Scripts/PlayerController.cs.meta"),
            format!("fileFormatVersion: 2\nguid: {GUID}\nMonoImporter:\n  serializedVersion: 2\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("Assets/Scenes/Main.unity"),
            scene_with_late_script_ref(),
        )
        .unwrap();
    }

    fn plan_for(root: &Path, sample: usize) -> (Plan, Vec<walk::FileEntry>) {
        let files = walk::walk(root, false).unwrap();
        let keys: Vec<String> = files
            .iter()
            .map(|f| ids::file_key(&f.path, f.size).unwrap())
            .collect();
        let digests: Vec<String> = (0..files.len()).map(|i| format!("d{i}")).collect();
        let budget = extract::pdf::ExtractionSpoolBudget::new(0, 0);
        let progress = Progress::silent();
        let meter = estimate::Meter::new();
        let ctx = PhaseAContext {
            state_dir: root,
            budget: &budget,
            capacity_warning: None,
            progress: &progress,
            meter: &meter,
        };
        let mut cfg = super::phase_a_grouping_tests::cfg_for(root);
        cfg.sample = sample;
        let plan = build_phase_a(root, &files, &keys, &digests, Vec::new(), &ctx, &cfg).plan;
        (plan, files)
    }

    #[test]
    fn the_guid_map_resolves_a_meta_sidecar_to_its_asset_path() {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path());
        let (plan, files) = plan_for(dir.path(), 500);
        let g = build_unity_guid_map(&files, &plan, &Progress::silent());
        assert_eq!(
            g.map.get(GUID).map(String::as_str),
            Some("Assets/Scripts/PlayerController.cs"),
            "guid must resolve to the asset the .meta sits beside"
        );
        assert!(g.unreadable.is_empty(), "{:?}", g.unreadable);
        assert!(g.no_guid.is_empty(), "{:?}", g.no_guid);
    }

    /// Blocker: `script_path`/`script_class` were registered in the explicit
    /// mapping only when the SAMPLE happened to contain `script_guid`. Phase A
    /// reads a bounded window, so a cluster whose window held no `m_Script`
    /// got them dynamic-mapped at index time instead — the field-budget
    /// overshoot of #312.
    #[test]
    fn script_link_fields_are_mapped_even_when_the_sample_never_saw_a_script_guid() {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path());

        let (plan, _) = plan_for(dir.path(), 1);
        let mb = plan
            .datasets
            .iter()
            .find(|d| d.group.as_deref() == Some("MonoBehaviour"))
            .expect("a MonoBehaviour cluster must exist");
        assert!(
            !mb.specs.iter().any(|s| s.name == "script_guid"),
            "precondition: with sample=1 the sampled window must NOT contain \
             script_guid, or this test proves nothing"
        );
        for want in ["script_path", "script_class"] {
            assert!(
                mb.specs.iter().any(|s| s.name == want),
                "{want} must be in the explicit mapping regardless of what the \
                 sample saw; specs = {:?}",
                mb.specs.iter().map(|s| &s.name).collect::<Vec<_>>()
            );
        }
    }

    /// End to end over the two halves: the guid map built from `.meta` files
    /// resolves the `script_guid` an extractor emitted, and the enrichment
    /// stamps both denormalized fields. This is the feature's headline query
    /// ("which scenes use this script?") reduced to its mechanism.
    #[test]
    fn enrichment_resolves_a_script_guid_to_path_and_class() {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path());
        let (plan, files) = plan_for(dir.path(), 500);
        let g = build_unity_guid_map(&files, &plan, &Progress::silent());

        let mut fields = Map::new();
        fields.insert("script_guid".into(), Value::String(GUID.into()));
        let unresolved = enrich_unity_fields(
            Family::UnityYaml,
            &mut fields,
            &g.map,
            "Assets/Scenes/Main.unity",
        );
        assert_eq!(unresolved, None, "a defined guid must resolve");
        assert_eq!(fields["script_path"], "Assets/Scripts/PlayerController.cs");
        assert_eq!(fields["script_class"], "PlayerController");
    }

    /// Blocker: an unresolvable `script_guid` produced no counter, no warning
    /// and no report line — the record shipped without `script_path`, and
    /// "no users" and "broken link" became the same answer.
    #[test]
    fn an_unresolvable_script_guid_is_reported_not_swallowed() {
        let mut fields = Map::new();
        fields.insert("script_guid".into(), Value::String("deadbeef".into()));
        let unresolved = enrich_unity_fields(
            Family::UnityYaml,
            &mut fields,
            &std::collections::HashMap::new(),
            "Assets/Scenes/Main.unity",
        );
        assert_eq!(
            unresolved.as_deref(),
            Some("deadbeef"),
            "the caller must be told which guid failed"
        );
        assert!(!fields.contains_key("script_path"));
    }

    /// The enrichment was moved to run BEFORE `coerce_record` so its fields
    /// are validated like every other field instead of bypassing coercion.
    /// That only works if the plan actually carries them — this drives the
    /// real `coerce::plan_from_specs` over the real planned specs and asserts
    /// both fields survive the round trip with their values intact.
    #[test]
    fn enriched_fields_survive_the_coercion_they_now_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path());
        let (plan, files) = plan_for(dir.path(), 500);
        let g = build_unity_guid_map(&files, &plan, &Progress::silent());

        let mb = plan
            .datasets
            .iter()
            .find(|d| d.group.as_deref() == Some("MonoBehaviour"))
            .expect("a MonoBehaviour cluster must exist");
        let coerce_plan = coerce::plan_from_specs(&mb.specs);
        assert!(
            coerce_plan.contains_key("script_path"),
            "the coercion plan must know the field, else it would pass through \
             unvalidated exactly as it did before"
        );

        let mut fields = Map::new();
        fields.insert("script_guid".into(), Value::String(GUID.into()));
        let _ = enrich_unity_fields(
            Family::UnityYaml,
            &mut fields,
            &g.map,
            "Assets/Scenes/Main.unity",
        );
        let dropped = coerce::coerce_record(&mut fields, &coerce_plan);
        assert_eq!(dropped, 0, "nothing may be dropped: {fields:?}");
        assert_eq!(fields["script_path"], "Assets/Scripts/PlayerController.cs");
        assert_eq!(fields["script_class"], "PlayerController");
    }

    /// The failures the map collects have to reach a person. Counting them
    /// into a struct nobody prints is the same silence in a different place.
    #[test]
    fn the_guid_map_failures_are_printed_with_names_and_counts() {
        let (pr, buffer) = progress::Progress::capture(
            progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        let g = UnityGuidMap {
            map: std::collections::HashMap::new(),
            unreadable: vec![("Assets/A.cs.meta".into(), "permission denied".into())],
            no_guid: vec!["Assets/B.cs.meta".into()],
        };
        report_unity_guid_map(&g, &pr);
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(text.contains("Assets/A.cs.meta"), "{text}");
        assert!(text.contains("permission denied"), "{text}");
        assert!(text.contains("Assets/B.cs.meta"), "{text}");
        assert!(text.contains("no guid"), "{text}");

        // A healthy map must stay quiet — a warning on every clean run is a
        // warning nobody reads.
        let (pr2, buf2) = progress::Progress::capture(
            progress::Surface::Plain,
            std::time::Duration::from_secs(3600),
        );
        report_unity_guid_map(&UnityGuidMap::default(), &pr2);
        assert!(String::from_utf8(buf2.lock().unwrap().clone())
            .unwrap()
            .is_empty());
    }

    /// Blocker: `build_unity_guid_map` discarded `extract_meta`'s `Result`.
    /// An unparseable `.meta` therefore produced silence.
    #[test]
    fn a_meta_that_carries_no_guid_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Assets")).unwrap();
        // Sniffs as UnityMeta (first line is `fileFormatVersion:` and a line
        // starts `guid:`) but the value is a YAML integer, so `extract_meta`
        // recovers no guid STRING — the sidecar is real and still unusable.
        std::fs::write(
            dir.path().join("Assets/Broken.cs.meta"),
            "fileFormatVersion: 2\nguid: 12345\nMonoImporter:\n  serializedVersion: 2\n",
        )
        .unwrap();
        let (plan, files) = plan_for(dir.path(), 500);
        let g = build_unity_guid_map(&files, &plan, &Progress::silent());
        assert!(g.map.is_empty(), "no guid should have resolved");
        assert_eq!(
            g.no_guid,
            vec!["Assets/Broken.cs.meta".to_string()],
            "the .meta with no usable guid must be named, not silently skipped"
        );
    }
}
