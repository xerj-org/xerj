//! PDF text extraction backed by a real PDF parser.
//!
//! The old implementation searched raw streams and interpreted font character
//! codes as Latin-1. `pdf_oxide` resolves the object graph and font mappings
//! before returning Unicode text. Each document runs in a fresh worker process
//! so parser state or failure cannot contaminate the server. On Unix the worker
//! also gets a process group and an address-space limit. This is crash/resource
//! isolation, not a security sandbox: the worker retains the user's authority.

use super::{split_sections, ExtractStats, FieldOrigin, RawRecord, Sink};
use anyhow::{anyhow, Context, Result};
use pdf_oxide::PdfDocument;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use xxhash_rust::xxh3::Xxh3;

const PDF_CAP: u64 = 512 << 20;
const MAX_PAGES: usize = 100_000;
const MAX_PAGE_TEXT: usize = 16 << 20;
const MAX_EXTRACTED_TEXT: usize = 64 << 20;
const MAX_WORKER_OUTPUT: usize = 32 << 20;
const MAX_WORKER_STDERR: u64 = 64 << 10;
const WORKER_ADDRESS_SPACE: u64 = 1536 << 20;
// The spool is an optional accelerator on the same filesystem and descriptor
// table as correctness-critical state. The byte ceiling includes pessimistic
// 32 MiB reservations held by concurrently serializing PDF responses, not only
// completed artifact lengths. Hundreds of small retained artifacts plus four
// in-flight maximum-sized reservations fit while optional pressure stays below
// the fixed ceiling.
const MAX_SPOOL_BYTES: u64 = 384 << 20;
const MAX_SPOOL_HANDLES: u64 = 512;
// This is an admission-time floor, not a guarantee against other processes
// consuming filesystem space after the snapshot.
const MIN_FILESYSTEM_HEADROOM: u64 = 4 << 30;
const JOURNAL_FILESYSTEM_HEADROOM: u64 = 64 << 20;
// Preserve a base allowance for the journal/lock/model/runtime plus four
// descriptors per general and PDF worker for staging, sockets, and pipes.
const MIN_DESCRIPTOR_HEADROOM: u64 = 64;
const DESCRIPTOR_HEADROOM_PER_WORKER: u64 = 4;

/// Serializes every test that mutates the process-global
/// `XERJ_PDF_WORKER_BIN`/`XERJ_TEST_PDF_COUNT` pair. The variables are read
/// inside `spawn_worker`, so a test that sets them changes the behaviour of
/// every other test running concurrently in this binary — including tests in
/// sibling modules. Take this before the first `set_var` and hold it until the
/// restoring guard drops. Poison is ignored on purpose: a panicking test must
/// not cascade into unrelated failures here.
#[cfg(test)]
pub(crate) static WORKER_BIN_ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
static CORRUPT_REPLAY_SOURCE_SIZE: AtomicU64 = AtomicU64::new(u64::MAX);
#[cfg(test)]
static CORRUPTED_REPLAY_RESERVATION_DROPPED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn corrupt_replay_for_source_size(source_size: u64) {
    CORRUPTED_REPLAY_RESERVATION_DROPPED.store(false, Ordering::SeqCst);
    CORRUPT_REPLAY_SOURCE_SIZE.store(source_size, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn corrupted_replay_reservation_was_dropped() -> bool {
    CORRUPTED_REPLAY_RESERVATION_DROPPED.load(Ordering::SeqCst)
}

pub fn extract(path: &Path, sink: Sink) -> Result<ExtractStats> {
    let _permit = worker_gate().acquire();
    let response = spawn_worker(path)?;
    Ok(deliver(response, sink))
}

/// Run the isolated parser once and retain its bounded protocol response in an
/// anonymous, run-local file for Phase B.
///
/// The spool is deliberately not durable state. A process restart has a frozen
/// inference plan but no trusted open handle, so it parses the PDF again. This
/// avoids coupling extraction cache recovery to the publication journal.
pub(crate) fn extract_and_spool(
    path: &Path,
    state_dir: &Path,
    source_size: u64,
    source_digest: &str,
    budget: &Arc<ExtractionSpoolBudget>,
    sink: Sink,
) -> Result<(ExtractStats, Option<ExtractionSpool>, Option<SpoolFallback>)> {
    let _permit = worker_gate().acquire();
    let response = spawn_worker(path)?;
    // Counted only once the worker has produced a usable protocol response.
    // An invocation that spawned and then failed, timed out, or was killed is
    // deliberately excluded — hence `..._responses` and not `..._calls` in the
    // report. Total parser *invocations* are not a number this counter can
    // honestly supply.
    budget.record_phase_a_parse();
    let (spool, fallback) =
        try_spool_response(state_dir, source_size, source_digest, budget, &response);
    let stats = deliver(response, sink);
    Ok((stats, spool, fallback))
}

pub(crate) struct SpoolFallback {
    pub(crate) category: &'static str,
    pub(crate) message: String,
}

fn try_spool_response(
    state_dir: &Path,
    source_size: u64,
    source_digest: &str,
    budget: &Arc<ExtractionSpoolBudget>,
    response: &WorkerResponse,
) -> (Option<ExtractionSpool>, Option<SpoolFallback>) {
    let reservation = budget.try_reserve(MAX_WORKER_OUTPUT as u64);
    match reservation {
        Ok(reservation) => {
            match spool_response(state_dir, source_size, source_digest, response, reservation) {
                Ok(spool) => {
                    budget.record_artifact_accepted(spool.bytes);
                    (Some(spool), None)
                }
                Err(error) => {
                    budget.record_io_fallback();
                    budget.record_fallback_category("artifact_io");
                    budget.record_artifact_rejected();
                    (
                        None,
                        Some(SpoolFallback {
                            category: "artifact_io",
                            message: format!(
                                "could not retain the run-local extraction artifact: {error:#}"
                            ),
                        }),
                    )
                }
            }
        }
        Err(category) => (
            {
                budget.record_artifact_rejected();
                budget.record_fallback_category(category);
                None
            },
            Some(SpoolFallback {
                category,
                message: format!(
                    "admission refused ({category}); snapshot headroom or the bounded {} MiB/{}-artifact ceiling would be exceeded",
                    budget.limit >> 20, budget.max_spools
                ),
            }),
        ),
    }
}

fn spool_response(
    state_dir: &Path,
    source_size: u64,
    source_digest: &str,
    response: &WorkerResponse,
    mut reservation: ExtractionSpoolReservation,
) -> Result<ExtractionSpool> {
    validate_response_identity(response)?;
    let mut file = tempfile::tempfile_in(state_dir).with_context(|| {
        format!(
            "create anonymous PDF extraction spool under {}",
            state_dir.display()
        )
    })?;
    {
        let mut writer = BufWriter::new(&mut file);
        serde_json::to_writer(&mut writer, &response)
            .context("serialize validated PDF extraction spool")?;
        writer.flush().context("flush PDF extraction spool")?;
    }
    let bytes = file
        .stream_position()
        .context("measure PDF extraction spool")?;
    if bytes > MAX_WORKER_OUTPUT as u64 {
        return Err(anyhow!(
            "validated PDF extraction spool exceeded the {} MiB parent-memory safety limit",
            MAX_WORKER_OUTPUT >> 20
        ));
    }
    let artifact_digest = artifact_digest(&mut file, bytes)?;
    reservation.shrink_to(bytes);
    file.rewind().context("rewind PDF extraction spool")?;
    Ok(ExtractionSpool {
        file: Mutex::new(file),
        source_size,
        source_digest: source_digest.to_owned(),
        bytes,
        artifact_digest,
        _reservation: reservation,
    })
}

fn artifact_digest(file: &mut File, bytes: u64) -> Result<u128> {
    file.rewind().context("rewind PDF extraction spool")?;
    let mut hash = Xxh3::new();
    let mut remaining = bytes;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .context("size PDF extraction spool digest buffer")?;
        let read = file
            .read(&mut buffer[..limit])
            .context("read PDF extraction spool for digest")?;
        anyhow::ensure!(read > 0, "PDF extraction spool was truncated while hashing");
        hash.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hash.digest128())
}

pub(crate) struct ExtractionSpoolBudget {
    admission: Mutex<()>,
    used: AtomicU64,
    spools: AtomicU64,
    limit: u64,
    max_spools: u64,
    live_capacity: Option<LiveCapacity>,
    reservations_started: AtomicU64,
    cumulative_reserved_bytes: AtomicU64,
    artifacts_created: AtomicU64,
    artifacts_not_created: AtomicU64,
    phase_b_eligible_artifacts: AtomicU64,
    artifacts_discarded_before_replay: AtomicU64,
    exact_artifact_bytes: AtomicU64,
    peak_retained_or_reserved_bytes: AtomicU64,
    peak_live_artifacts: AtomicU64,
    phase_a_pdf_parser_responses: AtomicU64,
    capacity_status: &'static str,
    capacity_reason: &'static str,
    fallback_examples: Mutex<Vec<serde_json::Value>>,
    fallback_examples_total: AtomicU64,
    fallback_categories: Mutex<std::collections::BTreeMap<&'static str, u64>>,
    initial_available_bytes: Option<u64>,
    initial_descriptor_limit: Option<u64>,
    initial_open_descriptors: Option<u64>,
    filesystem_headroom: Option<u64>,
    descriptor_headroom: Option<u64>,
    capacity_fallbacks: AtomicU64,
    io_fallbacks: AtomicU64,
    replay_verified: AtomicU64,
    replay_integrity_failures: AtomicU64,
    phase_b_pdf_parses: AtomicU64,
}

struct LiveCapacity {
    state_dir: PathBuf,
    filesystem_headroom: u64,
    descriptor_headroom: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtractionSpoolCapacity {
    bytes: u64,
    handles: u64,
    filesystem_headroom: u64,
    descriptor_headroom: u64,
}

impl ExtractionSpoolBudget {
    pub(crate) fn new(limit: u64, max_spools: u64) -> Arc<Self> {
        Arc::new(Self {
            admission: Mutex::new(()),
            used: AtomicU64::new(0),
            spools: AtomicU64::new(0),
            limit,
            max_spools,
            live_capacity: None,
            reservations_started: AtomicU64::new(0),
            cumulative_reserved_bytes: AtomicU64::new(0),
            artifacts_created: AtomicU64::new(0),
            artifacts_not_created: AtomicU64::new(0),
            phase_b_eligible_artifacts: AtomicU64::new(0),
            artifacts_discarded_before_replay: AtomicU64::new(0),
            exact_artifact_bytes: AtomicU64::new(0),
            peak_retained_or_reserved_bytes: AtomicU64::new(0),
            peak_live_artifacts: AtomicU64::new(0),
            phase_a_pdf_parser_responses: AtomicU64::new(0),
            capacity_status: "enabled",
            capacity_reason: "explicit_budget",
            fallback_examples: Mutex::new(Vec::new()),
            fallback_examples_total: AtomicU64::new(0),
            fallback_categories: Mutex::new(std::collections::BTreeMap::new()),
            initial_available_bytes: None,
            initial_descriptor_limit: None,
            initial_open_descriptors: None,
            filesystem_headroom: None,
            descriptor_headroom: None,
            capacity_fallbacks: AtomicU64::new(0),
            io_fallbacks: AtomicU64::new(0),
            replay_verified: AtomicU64::new(0),
            replay_integrity_failures: AtomicU64::new(0),
            phase_b_pdf_parses: AtomicU64::new(0),
        })
    }

    /// Derive an optimization budget from the resources shared with the
    /// correctness-critical journal, Phase-B staging files, HTTP sockets, and
    /// worker pipes. If either resource cannot preserve explicit headroom,
    /// admission is disabled and Phase B uses the existing safe reparse path.
    pub(crate) fn for_state_dir(
        state_dir: &Path,
        workers: usize,
        pdf_workers: usize,
        bulk_mb: usize,
    ) -> (Arc<Self>, Option<String>) {
        let available_bytes = match available_space_for(state_dir) {
            Ok(bytes) => bytes,
            Err(error) => {
                let mut budget = Self::new(0, 0);
                let inner = Arc::get_mut(&mut budget).expect("new budget has one owner");
                inner.capacity_status = "disabled";
                inner.capacity_reason = "free_space_probe_unavailable";
                return (
                    budget,
                    Some(format!(
                        "disabled because free space under {} could not be measured: {error}",
                        state_dir.display()
                    )),
                );
            }
        };
        let (descriptor_limit, open_descriptors) = descriptor_snapshot_for(state_dir);
        let capacity = derive_spool_capacity(
            available_bytes,
            descriptor_limit,
            open_descriptors,
            workers,
            pdf_workers,
            bulk_mb,
        );
        let warning = if capacity.bytes < MAX_WORKER_OUTPUT as u64 || capacity.handles == 0 {
            Some(format!(
                "disabled to preserve {} MiB filesystem and {} descriptor headroom \
                 ({} MiB free, descriptor limit {})",
                capacity.filesystem_headroom >> 20,
                capacity.descriptor_headroom,
                available_bytes >> 20,
                descriptor_limit
                    .map(|limit| limit.to_string())
                    .unwrap_or_else(|| "unavailable".into())
            ))
        } else if capacity.bytes < MAX_SPOOL_BYTES || capacity.handles < MAX_SPOOL_HANDLES {
            Some(format!(
                "limited to {} MiB and {} artifacts to preserve {} MiB filesystem and {} \
                 descriptor headroom",
                capacity.bytes >> 20,
                capacity.handles,
                capacity.filesystem_headroom >> 20,
                capacity.descriptor_headroom
            ))
        } else {
            None
        };
        let capacity_status = if capacity.bytes < MAX_WORKER_OUTPUT as u64 || capacity.handles == 0
        {
            "disabled"
        } else if capacity.bytes < MAX_SPOOL_BYTES || capacity.handles < MAX_SPOOL_HANDLES {
            "limited"
        } else {
            "enabled"
        };
        let capacity_reason = if descriptor_limit.is_none() || open_descriptors.is_none() {
            "descriptor_probe_unavailable"
        } else if capacity.bytes < MAX_WORKER_OUTPUT as u64 {
            "filesystem_headroom"
        } else if capacity.handles == 0 {
            "descriptor_headroom"
        } else if capacity_status == "limited" {
            "resource_share_limited"
        } else {
            "full_bounded_capacity"
        };
        let budget = Arc::new(Self {
            admission: Mutex::new(()),
            used: AtomicU64::new(0),
            spools: AtomicU64::new(0),
            limit: capacity.bytes,
            max_spools: capacity.handles,
            live_capacity: Some(LiveCapacity {
                state_dir: state_dir.to_owned(),
                filesystem_headroom: capacity.filesystem_headroom,
                descriptor_headroom: capacity.descriptor_headroom,
            }),
            reservations_started: AtomicU64::new(0),
            cumulative_reserved_bytes: AtomicU64::new(0),
            artifacts_created: AtomicU64::new(0),
            artifacts_not_created: AtomicU64::new(0),
            phase_b_eligible_artifacts: AtomicU64::new(0),
            artifacts_discarded_before_replay: AtomicU64::new(0),
            exact_artifact_bytes: AtomicU64::new(0),
            peak_retained_or_reserved_bytes: AtomicU64::new(0),
            peak_live_artifacts: AtomicU64::new(0),
            phase_a_pdf_parser_responses: AtomicU64::new(0),
            capacity_status,
            capacity_reason,
            fallback_examples: Mutex::new(Vec::new()),
            fallback_examples_total: AtomicU64::new(0),
            fallback_categories: Mutex::new(std::collections::BTreeMap::new()),
            initial_available_bytes: Some(available_bytes),
            initial_descriptor_limit: descriptor_limit,
            initial_open_descriptors: open_descriptors,
            filesystem_headroom: Some(capacity.filesystem_headroom),
            descriptor_headroom: Some(capacity.descriptor_headroom),
            capacity_fallbacks: AtomicU64::new(0),
            io_fallbacks: AtomicU64::new(0),
            replay_verified: AtomicU64::new(0),
            replay_integrity_failures: AtomicU64::new(0),
            phase_b_pdf_parses: AtomicU64::new(0),
        });
        (budget, warning)
    }

    fn try_reserve(
        self: &Arc<Self>,
        bytes: u64,
    ) -> std::result::Result<ExtractionSpoolReservation, &'static str> {
        if let Some(live) = &self.live_capacity {
            let available = available_space_for(&live.state_dir).map_err(|_| {
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                "free_space_probe_failed"
            })?;
            // `available` already excludes bytes occupied by retained spool
            // files; adding `used` again would double-count them.
            if available < live.filesystem_headroom.saturating_add(bytes) {
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err("filesystem_admission_floor");
            }
            let (Some(limit), Some(open)) = descriptor_snapshot_for(&live.state_dir) else {
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err("descriptor_probe_failed");
            };
            if open
                .saturating_add(live.descriptor_headroom)
                .saturating_add(1)
                > limit
            {
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err("descriptor_admission_floor");
            }
        }
        // Serialize the tiny accounting transition so a handle claim that is
        // subsequently rejected by the byte ceiling cannot inflate the
        // advertised live-artifact peak.
        let _admission = self.admission.lock().unwrap();
        let previous_spools = self
            .spools
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |spools| {
                (spools < self.max_spools).then_some(spools + 1)
            })
            .map_err(|_| {
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                "artifact_count_ceiling"
            })?;
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(bytes) else {
                self.spools.fetch_sub(1, Ordering::AcqRel);
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err("byte_accounting_overflow");
            };
            if next > self.limit {
                self.spools.fetch_sub(1, Ordering::AcqRel);
                self.capacity_fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err("byte_ceiling");
            }
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.peak_live_artifacts
                        .fetch_max(previous_spools + 1, Ordering::Relaxed);
                    self.reservations_started.fetch_add(1, Ordering::Relaxed);
                    self.cumulative_reserved_bytes
                        .fetch_add(bytes, Ordering::Relaxed);
                    self.peak_retained_or_reserved_bytes
                        .fetch_max(next, Ordering::Relaxed);
                    return Ok(ExtractionSpoolReservation {
                        budget: Arc::clone(self),
                        bytes,
                        #[cfg(test)]
                        cleanup_probe: std::sync::atomic::AtomicBool::new(false),
                    });
                }
                Err(actual) => used = actual,
            }
        }
    }

    pub(crate) fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "reservations_started": self.reservations_started.load(Ordering::Relaxed),
            "cumulative_reserved_bytes": self.cumulative_reserved_bytes.load(Ordering::Relaxed),
            "artifacts_created": self.artifacts_created.load(Ordering::Relaxed),
            "artifacts_not_created": self.artifacts_not_created.load(Ordering::Relaxed),
            "phase_b_eligible_artifacts": self.phase_b_eligible_artifacts.load(Ordering::Relaxed),
            "artifacts_discarded_before_replay": self.artifacts_discarded_before_replay.load(Ordering::Relaxed),
            "exact_artifact_bytes": self.exact_artifact_bytes.load(Ordering::Relaxed),
            "current_retained_or_reserved_bytes": self.used.load(Ordering::Relaxed),
            "peak_retained_or_reserved_bytes": self.peak_retained_or_reserved_bytes.load(Ordering::Relaxed),
            "current_live_artifacts": self.spools.load(Ordering::Relaxed),
            "peak_live_artifacts": self.peak_live_artifacts.load(Ordering::Relaxed),
            "phase_a_pdf_parser_responses": self.phase_a_pdf_parser_responses.load(Ordering::Relaxed),
            "capacity_fallbacks": self.capacity_fallbacks.load(Ordering::Relaxed),
            "io_fallbacks": self.io_fallbacks.load(Ordering::Relaxed),
            "replay_verified": self.replay_verified.load(Ordering::Relaxed),
            "replay_integrity_failures": self.replay_integrity_failures.load(Ordering::Relaxed),
            "phase_b_pdf_parses": self.phase_b_pdf_parses.load(Ordering::Relaxed),
            "byte_ceiling": self.limit,
            "artifact_ceiling": self.max_spools,
            "capacity_status": self.capacity_status,
            "capacity_reason": self.capacity_reason,
            "fallback_examples": self.fallback_examples.lock().unwrap().clone(),
            "fallback_examples_limit": 3,
            "fallback_examples_truncated": self.fallback_examples_total.load(Ordering::Relaxed) > 3,
            "fallback_categories": self.fallback_categories.lock().unwrap().clone(),
            "capacity": {
                "initial_available_bytes": self.initial_available_bytes,
                "initial_descriptor_limit": self.initial_descriptor_limit,
                "initial_open_descriptors": self.initial_open_descriptors,
                "filesystem_headroom": self.filesystem_headroom,
                "descriptor_headroom": self.descriptor_headroom,
            },
        })
    }

    pub(crate) fn record_io_fallback(&self) {
        self.io_fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_reparse(&self) {
        self.phase_b_pdf_parses.fetch_add(1, Ordering::Relaxed);
    }

    /// A verified artifact that failed one of its pre-replay checks. This is
    /// an *integrity* outcome, already counted by `replay_integrity_failures`
    /// inside `replay_with_gate`; it deliberately does not touch
    /// `io_fallbacks`, which counts only the artifact-creation I/O failures in
    /// `try_spool_response`. A digest, JSON, or protocol-identity mismatch is
    /// not an I/O event and reporting it as one would overstate disk trouble.
    pub(crate) fn record_replay_fallback(&self, path: &str, error: &anyhow::Error) {
        self.record_fallback_category("replay_verification");
        self.record_fallback_example(
            path,
            "replay_verification",
            &format!(
                "run-local PDF extraction artifact could not be verified; reparsed source: \
                 {error:#}"
            ),
        );
    }

    pub(crate) fn platform_reuse_is_unavailable(&self) -> bool {
        self.capacity_status == "disabled" && self.capacity_reason == "descriptor_probe_unavailable"
    }

    pub(crate) fn record_fallback_example(
        &self,
        path: &str,
        category: &'static str,
        message: &str,
    ) {
        self.fallback_examples_total.fetch_add(1, Ordering::Relaxed);
        let mut examples = self.fallback_examples.lock().unwrap();
        if examples.len() < 3 {
            examples.push(serde_json::json!({
                "path": path,
                "category": category,
                "message": message,
            }));
        }
    }

    fn record_fallback_category(&self, category: &'static str) {
        *self
            .fallback_categories
            .lock()
            .unwrap()
            .entry(category)
            .or_default() += 1;
    }

    fn record_phase_a_parse(&self) {
        self.phase_a_pdf_parser_responses
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_artifact_accepted(&self, bytes: u64) {
        self.artifacts_created.fetch_add(1, Ordering::Relaxed);
        self.exact_artifact_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_artifact_rejected(&self) {
        self.artifacts_not_created.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_phase_b_eligible(&self) {
        self.phase_b_eligible_artifacts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_discarded_before_replay(&self) {
        self.artifacts_discarded_before_replay
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_source_generation_changed(&self) {
        self.record_fallback_category("source_generation_changed");
    }
}

fn available_space_for(state_dir: &Path) -> std::io::Result<u64> {
    #[cfg(test)]
    if let Some(value) = injected_probe_value(state_dir, "available-bytes") {
        return Ok(value);
    }
    fs2::available_space(state_dir)
}

fn descriptor_snapshot_for(state_dir: &Path) -> (Option<u64>, Option<u64>) {
    #[cfg(test)]
    {
        let limit = injected_probe_value(state_dir, "fd-limit");
        let open = injected_probe_value(state_dir, "fd-open");
        if limit.is_some() || open.is_some() {
            return (limit, open);
        }
    }
    #[cfg(not(test))]
    let _ = state_dir;
    (descriptor_soft_limit(), open_descriptor_count())
}

#[cfg(test)]
fn injected_probe_value(state_dir: &Path, name: &str) -> Option<u64> {
    std::fs::read_to_string(state_dir.join(format!(".autoindex-test-pdf-spool-{name}")))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn derive_spool_capacity(
    available_bytes: u64,
    descriptor_limit: Option<u64>,
    open_descriptors: Option<u64>,
    workers: usize,
    pdf_workers: usize,
    bulk_mb: usize,
) -> ExtractionSpoolCapacity {
    let workers = workers as u64;
    let pdf_workers = pdf_workers as u64;
    let bulk_bytes = (bulk_mb as u64).saturating_mul(1 << 20);
    let filesystem_headroom = MIN_FILESYSTEM_HEADROOM
        .max(available_bytes / 2)
        .max(JOURNAL_FILESYSTEM_HEADROOM.saturating_add(workers.saturating_mul(bulk_bytes)));
    let bytes = MAX_SPOOL_BYTES.min(available_bytes.saturating_sub(filesystem_headroom));

    let descriptor_headroom = MIN_DESCRIPTOR_HEADROOM
        .saturating_add(workers.saturating_mul(DESCRIPTOR_HEADROOM_PER_WORKER))
        .saturating_add(pdf_workers.saturating_mul(DESCRIPTOR_HEADROOM_PER_WORKER));
    let handles = descriptor_limit
        .zip(open_descriptors)
        .map(|(limit, open)| {
            let live_cap = limit.saturating_sub(open.saturating_add(descriptor_headroom));
            MAX_SPOOL_HANDLES.min(live_cap)
        })
        .unwrap_or(0);

    ExtractionSpoolCapacity {
        bytes,
        handles,
        filesystem_headroom,
        descriptor_headroom,
    }
}

#[cfg(unix)]
fn descriptor_soft_limit() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return None;
    }
    if limit.rlim_cur == libc::RLIM_INFINITY {
        Some(u64::MAX)
    } else {
        Some(limit.rlim_cur)
    }
}

#[cfg(not(unix))]
fn descriptor_soft_limit() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn open_descriptor_count() -> Option<u64> {
    // Reading this directory temporarily owns one descriptor, so the count is
    // conservatively one higher than the steady state after this function.
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .and_then(|entries| u64::try_from(entries.count()).ok())
}

#[cfg(not(target_os = "linux"))]
fn open_descriptor_count() -> Option<u64> {
    None
}

struct ExtractionSpoolReservation {
    budget: Arc<ExtractionSpoolBudget>,
    bytes: u64,
    #[cfg(test)]
    cleanup_probe: std::sync::atomic::AtomicBool,
}

impl ExtractionSpoolReservation {
    fn shrink_to(&mut self, bytes: u64) {
        debug_assert!(bytes <= self.bytes);
        let released = self.bytes - bytes;
        self.bytes = bytes;
        let previous = self.budget.used.fetch_sub(released, Ordering::AcqRel);
        debug_assert!(previous >= released);
    }
}

impl Drop for ExtractionSpoolReservation {
    fn drop(&mut self) {
        let previous = self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        debug_assert!(previous >= self.bytes);
        let previous_spools = self.budget.spools.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous_spools > 0);
        #[cfg(test)]
        if self.cleanup_probe.load(Ordering::SeqCst) {
            CORRUPTED_REPLAY_RESERVATION_DROPPED.store(true, Ordering::SeqCst);
        }
    }
}

/// A validated worker response bound to one inventory generation.
///
/// `File` is anonymous and the mutex owns its single seek cursor. Phase A and
/// Phase B are sequential today, but cursor ownership remains explicit rather
/// than relying on cloned Unix file descriptors with shared offsets.
pub(crate) struct ExtractionSpool {
    file: Mutex<File>,
    source_size: u64,
    source_digest: String,
    bytes: u64,
    artifact_digest: u128,
    _reservation: ExtractionSpoolReservation,
}

impl ExtractionSpool {
    /// Verify and deliver the retained response exactly once.
    ///
    /// Four checks can fail against a caller that routed the right artifact to
    /// the right file: the artifact's physical length, its content digest, its
    /// JSON decode, and the worker protocol identity inside it. The
    /// `source_size`/`source_digest` comparison below is a fifth check of a
    /// different kind — a binding assertion. Phase B derives both values from
    /// the same inventory entry that Phase A spooled under, so at the
    /// production call site it can only fire if an artifact were routed to the
    /// wrong file. Mutation of the *source* between phases is caught earlier
    /// and with full strength by the `content::verify` call that Phase B runs
    /// before extraction (`lib.rs`), not here.
    pub(crate) fn replay(
        self,
        source_size: u64,
        source_digest: &str,
        sink: Sink,
    ) -> Result<ExtractStats> {
        self.replay_with_gate(source_size, source_digest, sink, worker_gate())
    }

    fn replay_with_gate(
        self,
        source_size: u64,
        source_digest: &str,
        sink: Sink,
        gate: &WorkerGate,
    ) -> Result<ExtractStats> {
        // JSON decoding materializes a bounded WorkerResponse just like the
        // parser protocol. Share the PDF gate so Phase B cannot multiply that
        // 32 MiB response bound by the general autoindex worker count.
        let _permit = gate.acquire();
        let ExtractionSpool {
            file,
            source_size: expected_size,
            source_digest: expected_digest,
            bytes,
            artifact_digest: expected_artifact_digest,
            _reservation: reservation,
        } = self;
        let budget = Arc::clone(&reservation.budget);
        let decoded = (|| -> Result<WorkerResponse> {
            // Binding assertion, not a source-mutation check — see `replay`.
            anyhow::ensure!(
                source_size == expected_size && source_digest == expected_digest,
                "PDF extraction spool belongs to a different source generation; retry extraction"
            );
            let mut file = file
                .into_inner()
                .map_err(|_| anyhow!("PDF extraction spool lock was poisoned"))?;
            #[cfg(test)]
            if CORRUPT_REPLAY_SOURCE_SIZE
                .compare_exchange(source_size, u64::MAX, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                reservation.cleanup_probe.store(true, Ordering::SeqCst);
                file.set_len(8)
                    .context("inject PDF extraction spool truncation")?;
            }
            let actual_bytes = file
                .metadata()
                .context("measure PDF extraction spool before replay")?
                .len();
            anyhow::ensure!(
                actual_bytes == bytes,
                "PDF extraction spool length changed (expected {}, found {}); retry extraction",
                bytes,
                actual_bytes
            );
            let actual_digest = artifact_digest(&mut file, bytes)?;
            anyhow::ensure!(
                actual_digest == expected_artifact_digest,
                "PDF extraction spool content changed; retry extraction"
            );
            file.rewind().context("rewind PDF extraction spool")?;
            let response: WorkerResponse = serde_json::from_reader(BufReader::new(&mut file))
                .context("PDF extraction spool is malformed or truncated; retry extraction")?;
            validate_response_identity(&response)?;
            Ok(response)
        })();
        // The optional artifact and its reservation are gone before `deliver`
        // can expand records into the unbounded correctness-critical stage.
        // Replay is deliberately one-shot.
        drop(reservation);
        match decoded {
            Ok(response) => {
                budget.replay_verified.fetch_add(1, Ordering::Relaxed);
                Ok(deliver(response, sink))
            }
            Err(error) => {
                budget
                    .replay_integrity_failures
                    .fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }
}

fn deliver(response: WorkerResponse, sink: Sink) -> ExtractStats {
    let mut stats = ExtractStats::default();
    for record in response.records {
        stats.records += 1;
        if !sink(record) {
            break;
        }
    }
    stats
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerResponse {
    schema: u32,
    extractor: String,
    parser: String,
    containment: String,
    records: Vec<RawRecord>,
}

pub fn configure_workers(workers: usize) {
    worker_gate().set_limit(workers.clamp(1, 4));
}

pub fn configure_timeout(seconds: u64) {
    WORKER_TIMEOUT_SECS.store(seconds.clamp(1, 3600), Ordering::Relaxed);
}

/// Hidden same-binary worker entry point. One invocation parses one document.
pub fn run_worker_cli() -> i32 {
    let mut args = std::env::args_os().skip(2);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("PDF worker protocol error: missing input path");
        return 2;
    };
    if args.next().is_some() {
        eprintln!("PDF worker protocol error: unexpected arguments");
        return 2;
    }
    match extract_in_process(&path) {
        Ok(records) => {
            let response = WorkerResponse {
                schema: 1,
                extractor: format!("xerj-autoindex/{}", env!("CARGO_PKG_VERSION")),
                parser: format!("pdf_oxide/{}", pdf_oxide::VERSION),
                containment: containment_description().to_string(),
                records,
            };
            let result = (|| -> Result<()> {
                let mut stdout = std::io::stdout().lock();
                serde_json::to_writer(&mut stdout, &response)?;
                stdout.flush()?;
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("PDF worker could not write its bounded result: {error}");
                return 1;
            }
            0
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

fn spawn_worker(path: &Path) -> Result<WorkerResponse> {
    // Trusted developer/test hook. The selected executable runs with the
    // invoking user's authority and must implement this private protocol.
    let executable = std::env::var_os("XERJ_PDF_WORKER_BIN")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("cannot locate the xerj executable for isolated PDF extraction")?;
    let mut command = Command::new(executable);
    command
        .arg("__extract-pdf")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    set_worker_memory_limit(&mut command);
    let mut child = command.spawn().with_context(|| {
        format!(
            "could not start the isolated PDF parser for {}; check process/resource limits",
            path.display()
        )
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || read_capped(stdout, MAX_WORKER_OUTPUT));
    let err_reader = std::thread::spawn(move || read_capped(stderr, MAX_WORKER_STDERR as usize));

    let started = Instant::now();
    let timeout = Duration::from_secs(WORKER_TIMEOUT_SECS.load(Ordering::Relaxed));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_worker_tree(&mut child);
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(error).with_context(|| {
                    format!("could not monitor PDF parser for {}", path.display())
                });
            }
        }
        if started.elapsed() >= timeout {
            terminate_worker_tree(&mut child);
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(anyhow!(
                "PDF extraction timed out after {} seconds for {}; repair/split the PDF, or run OCR if it is image-only",
                timeout.as_secs(),
                path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    terminate_worker_descendants(child.id());
    let stdout = out_reader
        .join()
        .map_err(|_| anyhow!("PDF parser output reader panicked"))??;
    let stderr = err_reader
        .join()
        .map_err(|_| anyhow!("PDF parser error reader panicked"))??;
    if stdout.overflow {
        return Err(anyhow!(
            "PDF parser output exceeded the {} MiB parent-memory safety limit for {}; split the document before indexing",
            MAX_WORKER_OUTPUT >> 20,
            path.display()
        ));
    }
    if stderr.overflow {
        return Err(anyhow!(
            "PDF parser error output exceeded the {} KiB safety limit for {}; the worker was excessively noisy and its truncated diagnostic is: {}",
            MAX_WORKER_STDERR >> 10,
            path.display(),
            String::from_utf8_lossy(&stderr.prefix)
        ));
    }
    let stderr = String::from_utf8_lossy(&stderr.prefix);
    if !status.success() {
        return Err(anyhow!(
            "isolated PDF parser failed for {}{}{}; verify/decrypt the PDF, repair it, or run OCR for image-only input",
            path.display(),
            if stderr.trim().is_empty() { "" } else { ": " },
            stderr.trim()
        ));
    }
    let response: WorkerResponse = serde_json::from_slice(&stdout.prefix).with_context(|| {
        format!(
            "isolated PDF parser returned an invalid result for {}; this is an internal worker-protocol error",
            path.display()
        )
    })?;
    validate_response_identity(&response)?;
    Ok(response)
}

fn validate_response_identity(response: &WorkerResponse) -> Result<()> {
    if response.schema != 1 {
        return Err(anyhow!(
            "PDF parser returned unsupported extraction schema {}; update parent and worker together",
            response.schema
        ));
    }
    let expected_extractor = format!("xerj-autoindex/{}", env!("CARGO_PKG_VERSION"));
    if response.extractor != expected_extractor {
        return Err(anyhow!(
            "PDF extractor version mismatch: expected {expected_extractor}, worker reported {}; update parent and worker together",
            response.extractor
        ));
    }
    if response.parser != format!("pdf_oxide/{}", pdf_oxide::VERSION) {
        return Err(anyhow!(
            "PDF parser version mismatch: expected pdf_oxide/{}, worker reported {}; update parent and worker together",
            pdf_oxide::VERSION, response.parser
        ));
    }
    Ok(())
}

static WORKER_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(120);

struct CappedRead {
    prefix: Vec<u8>,
    overflow: bool,
}

fn read_capped(mut input: impl Read, cap: usize) -> Result<CappedRead> {
    let mut prefix = Vec::with_capacity(cap.min(64 << 10));
    let mut overflow = false;
    let mut chunk = [0u8; 64 << 10];
    loop {
        let read = input
            .read(&mut chunk)
            .context("could not read isolated PDF parser output")?;
        if read == 0 {
            break;
        }
        let remaining = cap.saturating_sub(prefix.len());
        let retain = remaining.min(read);
        prefix.extend_from_slice(&chunk[..retain]);
        overflow |= retain < read;
        // Keep draining after overflow so the child can exit instead of
        // blocking forever on a full pipe.
    }
    Ok(CappedRead { prefix, overflow })
}

#[cfg(unix)]
fn set_worker_memory_limit(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: WORKER_ADDRESS_SPACE,
                rlim_max: WORKER_ADDRESS_SPACE,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn set_worker_memory_limit(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_worker_tree(child: &mut std::process::Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_worker_descendants(process_group: u32) {
    unsafe {
        libc::kill(-(process_group as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_worker_tree(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn terminate_worker_descendants(_process_group: u32) {}

#[cfg(unix)]
fn containment_description() -> &'static str {
    "fresh process; Unix process-group cleanup; 1536 MiB RLIMIT_AS; not a security sandbox"
}

#[cfg(not(unix))]
fn containment_description() -> &'static str {
    "fresh process; no OS memory cap on this platform; not a security sandbox"
}

fn extract_in_process(path: &Path) -> Result<Vec<RawRecord>> {
    let size = std::fs::metadata(path)?.len();
    if size > PDF_CAP {
        return Err(anyhow!(
            "PDF is {:.1} MiB, above the 512 MiB safety limit; split or compress it before indexing",
            size as f64 / (1 << 20) as f64
        ));
    }

    pdf_oxide::fonts::global_cache::clear_global_font_cache();
    let doc = PdfDocument::open(path).with_context(|| {
        "PDF parser could not open the document; verify/repair damaged input, decrypt password-protected input, or run OCR for image-only input"
    })?;
    let pages = doc.page_count().with_context(|| {
        "PDF parser could not read the page tree; verify the file or decrypt it first"
    })?;
    if pages == 0 {
        return Err(anyhow!("PDF contains no pages"));
    }
    if pages > MAX_PAGES {
        return Err(anyhow!(
            "PDF declares {pages} pages, above the {MAX_PAGES}-page safety limit; split it before indexing"
        ));
    }

    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "untitled".to_string());
    let mut records = Vec::new();
    let mut extracted_bytes = 0usize;
    let mut failed_pages = Vec::new();
    let mut omitted_pages = Vec::new();
    let mut pages_with_text = 0usize;

    for page_index in 0..pages {
        let page = match doc.extract_text(page_index) {
            Ok(text) => text,
            Err(error) => {
                failed_pages.push(format!("{} ({error})", page_index + 1));
                continue;
            }
        };
        let page = normalize_legacy_symbol_glyphs(&page);
        let body = page.trim();
        if body.is_empty() {
            omitted_pages.push(page_index + 1);
            continue;
        }
        if body.len() > MAX_PAGE_TEXT {
            return Err(anyhow!(
                "PDF page {} produced more than 16 MiB of text; split or repair the document before indexing",
                page_index + 1
            ));
        }
        if body.chars().filter(|ch| !ch.is_whitespace()).count() < 8 {
            omitted_pages.push(page_index + 1);
            continue;
        }
        validate_text_quality(body).with_context(|| {
            format!(
                "PDF page {} produced untrusted text; refusing to index possible binary/font garbage",
                page_index + 1
            )
        })?;
        extracted_bytes = extracted_bytes.saturating_add(body.len());
        pages_with_text += 1;
        if extracted_bytes > MAX_EXTRACTED_TEXT {
            return Err(anyhow!(
                "PDF extraction exceeded the 64 MiB text safety limit; split the document before indexing"
            ));
        }

        for (section, text) in split_sections(body).into_iter().enumerate() {
            let mut fields = Map::new();
            fields.insert("title".into(), Value::String(title.clone()));
            fields.insert(
                "page".into(),
                Value::Number(((page_index + 1) as u64).into()),
            );
            if section > 0 {
                fields.insert("section".into(), Value::Number((section as u64).into()));
            }
            fields.insert("body".into(), Value::String(text));
            records.push(RawRecord {
                fields,
                locator: format!("p{}-s{}", page_index + 1, section),
                group: None,
                // title/page/section/body plus the pdf_* counters are this
                // extractor's vocabulary; `section` and `pdf_ocr_warning`
                // appear only sometimes.
                origin: FieldOrigin::Extractor,
            });
        }
    }

    if records.is_empty() {
        if failed_pages.is_empty() {
            return Err(anyhow!(
                "PDF has no extractable text; it may be an image-only scan. Run OCR first, then autoindex the searchable PDF"
            ));
        }
        return Err(anyhow!(
            "PDF text extraction failed on every non-empty page ({}); verify/decrypt the PDF or run OCR before indexing",
            failed_pages.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !failed_pages.is_empty() {
        return Err(anyhow!(
            "PDF extraction failed on {}/{} pages (first failures: {}); refusing a partial index. Repair/decrypt the PDF or run OCR first",
            failed_pages.len(),
            pages,
            failed_pages.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    for record in &mut records {
        record.fields.insert(
            "pdf_pages_total".into(),
            Value::Number((pages as u64).into()),
        );
        record.fields.insert(
            "pdf_pages_with_text".into(),
            Value::Number((pages_with_text as u64).into()),
        );
        record.fields.insert(
            "pdf_pages_omitted".into(),
            Value::Number((omitted_pages.len() as u64).into()),
        );
        if !omitted_pages.is_empty() {
            record.fields.insert(
                "pdf_ocr_warning".into(),
                Value::String(format!(
                    "{} page(s) had no usable text (first: {}); intentional blanks or scanned pages may need OCR",
                    omitted_pages.len(),
                    omitted_pages
                        .iter()
                        .take(8)
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            );
        }
    }
    Ok(records)
}

struct WorkerGate {
    state: Mutex<(usize, usize)>,
    ready: Condvar,
}

impl WorkerGate {
    /// `limit` is the width the resource policy decided
    /// (`crate::resources::plan`), already range-checked at the CLI — it is
    /// taken as given, not re-clamped against a second opinion. Only zero is
    /// corrected, because a gate of width 0 parks every parser forever.
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new((0, limit.max(1))),
            ready: Condvar::new(),
        }
    }

    fn set_limit(&self, limit: usize) {
        self.state.lock().expect("PDF worker gate poisoned").1 = limit;
        self.ready.notify_all();
    }

    fn acquire(&self) -> WorkerPermit<'_> {
        let mut state = self.state.lock().expect("PDF worker gate poisoned");
        while state.0 >= state.1 {
            state = self.ready.wait(state).expect("PDF worker gate poisoned");
        }
        state.0 += 1;
        WorkerPermit(self)
    }
}

struct WorkerPermit<'a>(&'a WorkerGate);

impl Drop for WorkerPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("PDF worker gate poisoned");
        state.0 -= 1;
        self.0.ready.notify_one();
    }
}

fn worker_gate() -> &'static WorkerGate {
    static GATE: OnceLock<WorkerGate> = OnceLock::new();
    GATE.get_or_init(|| {
        // Default only: `configure_workers` overwrites this with the run's
        // plan (`crate::resources::plan`) before any extraction starts.
        WorkerGate::new(xerj_common::resource::cores().min(crate::resources::MAX_PDF_WORKERS))
    })
}

fn normalize_legacy_symbol_glyphs(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\u{f0b7}' => '•',
            '\u{f02d}' => '-',
            '\u{f078}' | '\u{f0a8}' | '\u{f06f}' | '\u{f052}' | '\u{f0a3}' | '\u{f020}' => '□',
            '\u{f0d2}' | '\u{f0d4}' | '\u{f0be}' => ' ',
            other => other,
        })
        .collect()
}

fn validate_text_quality(text: &str) -> Result<()> {
    // Fail closed rather than embed broken font codes. On the pinned
    // 368-document FinanceBench PDF corpus this 0.5% threshold accepted 367
    // documents and rejected one; keep that false-rejection cost visible when
    // recalibrating against broader labeled corpora.
    let mut visible = 0usize;
    let mut semantic = 0usize;
    let mut suspicious = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        visible += 1;
        if ch.is_alphanumeric() {
            semantic += 1;
        }
        if ch == '\u{fffd}'
            || ch.is_control()
            || ('\u{e000}'..='\u{f8ff}').contains(&ch)
            || ('\u{f0000}'..='\u{ffffd}').contains(&ch)
            || ('\u{100000}'..='\u{10fffd}').contains(&ch)
        {
            suspicious += 1;
        }
    }
    if visible < 8 {
        return Err(anyhow!("too little visible text"));
    }
    if suspicious * 200 > visible {
        return Err(anyhow!(
            "more than 0.5% of visible characters are controls, replacement glyphs, or private-use codes"
        ));
    }
    if semantic * 20 < visible {
        return Err(anyhow!(
            "fewer than 5% of visible characters are letters or numbers"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        containment_description, derive_spool_capacity, extract_in_process,
        normalize_legacy_symbol_glyphs, spool_response, try_spool_response, validate_text_quality,
        WorkerResponse, JOURNAL_FILESYSTEM_HEADROOM, MAX_SPOOL_BYTES, MAX_SPOOL_HANDLES,
        MAX_WORKER_OUTPUT, MIN_DESCRIPTOR_HEADROOM, MIN_FILESYSTEM_HEADROOM,
    };
    use crate::infer::{infer_fields, FieldAcc};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::io::{Read, Seek, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    fn write_prose_pdf(path: &std::path::Path) {
        let prose = "Quarterly revenue increased because subscription demand remained strong. \
            Operating income improved as cloud infrastructure costs declined. Management expects \
            cash flow to support continued investment throughout the next fiscal year.";
        let stream = format!("BT /F1 12 Tf 72 720 Td ({prose}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        std::fs::write(path, pdf).unwrap();
    }

    #[test]
    fn quality_gate_accepts_financial_and_international_text() {
        validate_text_quality(
            "Consolidated revenue — Q4 2025\n€1,234.5 million (增长率 12%; прибыль 8%).",
        )
        .unwrap();
    }

    #[test]
    fn quality_gate_rejects_binary_and_font_garbage() {
        let garbage = "abc\u{0000}\u{0001}\u{fffd}\u{e123}\u{0002}xyz";
        assert!(validate_text_quality(garbage).is_err());
    }

    #[test]
    fn quality_gate_rejects_operator_noise() {
        assert!(validate_text_quality("//// <<>> [] () -- === +++").is_err());
    }

    #[test]
    fn known_legacy_symbol_glyphs_are_normalized_but_unknown_pua_is_rejected() {
        let normalized = normalize_legacy_symbol_glyphs(
            "Yes \u{f078} No \u{f0a8}\n\u{f0b7} Revenue\nrisk\u{f02d}adjusted",
        );
        assert_eq!(normalized, "Yes □ No □\n• Revenue\nrisk-adjusted");
        validate_text_quality(&normalized).unwrap();
        assert!(validate_text_quality("Revenue \u{e123} remains unknown").is_err());
    }

    #[test]
    fn reproducible_pdf_extracts_pages_and_elects_semantic_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quarterly-report.pdf");
        write_prose_pdf(&path);

        let first = extract_in_process(&path).unwrap();
        let second = extract_in_process(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        assert!(!first.is_empty());
        assert_eq!(first[0].fields["page"], 1);
        assert!(first[0].locator.starts_with("p1-s"));
        assert!(first[0].fields["body"]
            .as_str()
            .unwrap()
            .contains("Quarterly revenue increased"));

        let mut fields: HashMap<String, FieldAcc> = HashMap::new();
        for record in &first {
            for (name, value) in &record.fields {
                fields.entry(name.clone()).or_default().add(value);
            }
        }
        let specs = infer_fields(&fields, first.len() as u64, false);
        let body = specs.iter().find(|spec| spec.name == "body").unwrap();
        assert_eq!(body.es_type, "semantic_text");
        assert!(matches!(
            first[0].fields.get("body"),
            Some(Value::String(_))
        ));
    }

    fn response(records: Vec<crate::extract::RawRecord>) -> WorkerResponse {
        WorkerResponse {
            schema: 1,
            extractor: format!("xerj-autoindex/{}", env!("CARGO_PKG_VERSION")),
            parser: format!("pdf_oxide/{}", pdf_oxide::VERSION),
            containment: containment_description().to_string(),
            records,
        }
    }

    #[test]
    fn anonymous_spool_replays_exact_records_and_rejects_another_generation() {
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = source.path().join("quarterly-report.pdf");
        write_prose_pdf(&path);
        let expected = extract_in_process(&path).unwrap();
        let budget = super::ExtractionSpoolBudget::new(2 * super::MAX_WORKER_OUTPUT as u64, 2);
        let spool = spool_response(
            state.path(),
            123,
            "axf2-generation-a",
            &response(expected.clone()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        let wrong_generation = spool_response(
            state.path(),
            123,
            "axf2-generation-a",
            &response(expected.clone()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();

        // The artifact exists only as an open handle; it adds no recoverable
        // path or stale-cache state under the journal directory.
        assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);

        let mut replayed = Vec::new();
        let stats = spool
            .replay(123, "axf2-generation-a", &mut |record| {
                replayed.push(record);
                true
            })
            .unwrap();
        assert_eq!(stats.records as usize, expected.len());
        assert_eq!(
            serde_json::to_value(&replayed).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );

        let error = wrong_generation
            .replay(123, "axf2-generation-b", &mut |_| true)
            .unwrap_err();
        assert!(error.to_string().contains("different source generation"));
    }

    #[test]
    fn spool_replay_early_stop_matches_direct_delivery() {
        let state = tempfile::tempdir().unwrap();
        let records: Vec<_> = (0..5)
            .map(|index| crate::extract::RawRecord {
                fields: serde_json::Map::from_iter([(
                    "ordinal".into(),
                    serde_json::Value::from(index),
                )]),
                locator: format!("p1-s{index}"),
                group: None,
                origin: super::FieldOrigin::Extractor,
            })
            .collect();
        let expected = response(records.clone());
        let mut direct = Vec::new();
        let direct_stats = super::deliver(expected, &mut |record| {
            direct.push(record);
            direct.len() < 3
        });

        let budget = super::ExtractionSpoolBudget::new(super::MAX_WORKER_OUTPUT as u64, 1);
        let spool = spool_response(
            state.path(),
            7,
            "digest",
            &response(records),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        let mut replayed = Vec::new();
        let replay_stats = spool
            .replay(7, "digest", &mut |record| {
                assert_eq!(budget.used.load(Ordering::Acquire), 0);
                assert_eq!(budget.spools.load(Ordering::Acquire), 0);
                replayed.push(record);
                replayed.len() < 3
            })
            .unwrap();

        assert_eq!(replay_stats.records, direct_stats.records);
        assert_eq!(
            serde_json::to_value(replayed).unwrap(),
            serde_json::to_value(direct).unwrap()
        );
    }

    #[test]
    fn spool_budget_accepts_exact_limit_and_rejects_limit_plus_one() {
        let budget = super::ExtractionSpoolBudget::new(1024, 2);
        let exact = budget.try_reserve(1024).expect("exact limit must fit");
        assert!(budget.try_reserve(1).is_err());
        drop(exact);
        assert!(budget.try_reserve(1025).is_err());
        assert!(budget.try_reserve(1024).is_ok());

        let rejected = super::ExtractionSpoolBudget::new(0, 1);
        assert!(rejected.try_reserve(1).is_err());
        assert_eq!(
            rejected
                .peak_live_artifacts
                .load(std::sync::atomic::Ordering::Acquire),
            0,
            "a byte-refused provisional handle is not a live artifact"
        );
    }

    #[test]
    fn fallback_examples_are_exactly_bounded_and_report_truncation() {
        let budget = super::ExtractionSpoolBudget::new(0, 0);
        for index in 0..4 {
            budget.record_fallback_example(
                &format!("report-{index}.pdf"),
                "test_fallback",
                "injected",
            );
        }
        let report = budget.report();
        assert_eq!(report["fallback_examples"].as_array().unwrap().len(), 3);
        assert_eq!(report["fallback_examples_limit"], 3);
        assert_eq!(report["fallback_examples_truncated"], true);
        assert_eq!(report["fallback_examples"][0]["path"], "report-0.pdf");
        assert_eq!(report["fallback_examples"][2]["path"], "report-2.pdf");
    }

    #[test]
    fn failed_worker_protocol_call_is_not_reported_as_a_completed_phase_a_parse() {
        // `spawn_worker` reads the process-global `XERJ_PDF_WORKER_BIN`, which
        // the run_index tests in `failure_resume_http_tests` point at a stub
        // that answers successfully for *any* path. Without both the shared
        // lock and an explicitly failing worker of our own, this test's
        // outcome depends on which other test happens to be running — it only
        // passed in CI because of `--test-threads=2` plus name ordering.
        let _env_lock = super::WORKER_BIN_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let missing = source.path().join("missing.pdf");
        let previous = std::env::var_os("XERJ_PDF_WORKER_BIN");
        std::env::set_var(
            "XERJ_PDF_WORKER_BIN",
            source.path().join("no-such-pdf-worker"),
        );
        struct RestoreWorkerBin(Option<std::ffi::OsString>);
        impl Drop for RestoreWorkerBin {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("XERJ_PDF_WORKER_BIN", value),
                    None => std::env::remove_var("XERJ_PDF_WORKER_BIN"),
                }
            }
        }
        let _restore = RestoreWorkerBin(previous);
        let budget = super::ExtractionSpoolBudget::new(super::MAX_WORKER_OUTPUT as u64, 1);
        let error = match super::extract_and_spool(
            &missing,
            state.path(),
            0,
            "digest",
            &budget,
            &mut |_| true,
        ) {
            Err(error) => error,
            Ok(_) => panic!("a worker binary that does not exist cannot produce a response"),
        };
        assert!(!format!("{error:#}").is_empty());
        assert_eq!(budget.report()["phase_a_pdf_parser_responses"], 0);
        assert_eq!(budget.report()["reservations_started"], 0);
    }

    #[test]
    fn spool_handle_cap_retains_more_than_twelve_small_artifacts() {
        let budget = super::ExtractionSpoolBudget::new(MAX_SPOOL_BYTES, MAX_SPOOL_HANDLES);
        let mut retained = Vec::new();
        for _ in 0..20 {
            let mut reservation = budget.try_reserve(MAX_WORKER_OUTPUT as u64).unwrap();
            reservation.shrink_to(1 << 20);
            retained.push(reservation);
        }
        assert_eq!(budget.spools.load(Ordering::Acquire), 20);
        assert_eq!(budget.used.load(Ordering::Acquire), 20 << 20);
        drop(retained);
        assert_eq!(budget.spools.load(Ordering::Acquire), 0);
    }

    #[test]
    fn many_small_artifacts_plus_four_transient_reservations_fit_384_mib() {
        const ARTIFACTS: u64 = 360;
        const EXACT_ARTIFACT_BYTES: u64 = 220 << 20;
        let budget = super::ExtractionSpoolBudget::new(MAX_SPOOL_BYTES, MAX_SPOOL_HANDLES);
        let base = EXACT_ARTIFACT_BYTES / ARTIFACTS;
        let remainder = EXACT_ARTIFACT_BYTES % ARTIFACTS;
        let mut retained = Vec::new();
        for index in 0..ARTIFACTS {
            let mut reservation = budget.try_reserve(MAX_WORKER_OUTPUT as u64).unwrap();
            reservation.shrink_to(base + u64::from(index < remainder));
            retained.push(reservation);
        }
        let transient: Vec<_> = (0..4)
            .map(|_| budget.try_reserve(MAX_WORKER_OUTPUT as u64).unwrap())
            .collect();
        assert_eq!(
            budget.used.load(Ordering::Acquire),
            EXACT_ARTIFACT_BYTES + 4 * MAX_WORKER_OUTPUT as u64
        );
        assert!(budget.used.load(Ordering::Acquire) < MAX_SPOOL_BYTES);
        drop(transient);
        drop(retained);
    }

    #[test]
    fn shared_capacity_preserves_disk_and_descriptor_headroom() {
        let capacity = derive_spool_capacity(8 << 30, Some(4096), Some(32), 8, 4, 24);
        assert_eq!(capacity.bytes, MAX_SPOOL_BYTES);
        assert_eq!(capacity.handles, MAX_SPOOL_HANDLES);
        assert_eq!(capacity.filesystem_headroom, 4 << 30);
        assert_eq!(
            capacity.descriptor_headroom,
            MIN_DESCRIPTOR_HEADROOM + 8 * 4 + 4 * 4
        );

        let constrained = derive_spool_capacity(1536 << 20, Some(512), Some(32), 8, 4, 24);
        assert_eq!(constrained.filesystem_headroom, MIN_FILESYSTEM_HEADROOM);
        assert_eq!(constrained.bytes, 0);
        assert_eq!(
            constrained.handles,
            512 - 32 - (MIN_DESCRIPTOR_HEADROOM + 8 * 4 + 4 * 4)
        );
    }

    #[test]
    fn shared_capacity_refuses_low_disk_before_staging_headroom_is_spent() {
        let just_too_small = MIN_FILESYSTEM_HEADROOM + MAX_WORKER_OUTPUT as u64 - 1;
        let capacity = derive_spool_capacity(just_too_small, Some(4096), Some(16), 8, 4, 24);
        assert!(capacity.bytes < MAX_WORKER_OUTPUT as u64);

        let exact = derive_spool_capacity(
            MIN_FILESYSTEM_HEADROOM + MAX_WORKER_OUTPUT as u64,
            Some(4096),
            Some(16),
            8,
            4,
            24,
        );
        assert_eq!(exact.bytes, MAX_WORKER_OUTPUT as u64);

        let worker_bound = derive_spool_capacity(8 << 30, Some(4096), Some(16), 200, 4, 24);
        assert_eq!(
            worker_bound.filesystem_headroom,
            JOURNAL_FILESYSTEM_HEADROOM + 200 * (24 << 20)
        );
    }

    #[test]
    fn shared_capacity_refuses_low_or_unmeasurable_descriptor_capacity() {
        let low_limit = derive_spool_capacity(8 << 30, Some(128), Some(16), 8, 4, 24);
        assert_eq!(low_limit.handles, 0);

        let unknown_limit = derive_spool_capacity(8 << 30, None, Some(16), 8, 4, 24);
        assert_eq!(unknown_limit.handles, 0);

        let unknown_usage = derive_spool_capacity(8 << 30, Some(4096), None, 8, 4, 24);
        assert_eq!(unknown_usage.handles, 0);
    }

    #[test]
    fn live_admission_uses_injected_disk_and_descriptor_probe_values() {
        let state = tempfile::tempdir().unwrap();
        let write_probe = |name: &str, value: u64| {
            std::fs::write(
                state
                    .path()
                    .join(format!(".autoindex-test-pdf-spool-{name}")),
                value.to_string(),
            )
            .unwrap();
        };
        write_probe("available-bytes", 16 << 30);
        write_probe("fd-limit", 4096);
        write_probe("fd-open", 16);
        let (budget, warning) = super::ExtractionSpoolBudget::for_state_dir(state.path(), 8, 4, 24);
        assert!(warning.is_none());

        write_probe(
            "available-bytes",
            MIN_FILESYSTEM_HEADROOM + MAX_WORKER_OUTPUT as u64 - 1,
        );
        assert_eq!(
            budget.try_reserve(MAX_WORKER_OUTPUT as u64).err().unwrap(),
            "filesystem_admission_floor"
        );

        write_probe("available-bytes", 16 << 30);
        write_probe("fd-limit", 128);
        write_probe("fd-open", 16);
        assert_eq!(
            budget.try_reserve(MAX_WORKER_OUTPUT as u64).err().unwrap(),
            "descriptor_admission_floor"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn low_rlimit_subprocess_disables_optional_spooling() {
        const CHILD: &str = "XERJ_TEST_LOW_RLIMIT_PDF_SPOOL_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
                0
            );
            limit.rlim_cur = limit.rlim_max.min(96);
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
            let state = tempfile::tempdir().unwrap();
            std::fs::write(
                state
                    .path()
                    .join(".autoindex-test-pdf-spool-available-bytes"),
                (16_u64 << 30).to_string(),
            )
            .unwrap();
            let (budget, _) = super::ExtractionSpoolBudget::for_state_dir(state.path(), 8, 4, 24);
            assert_eq!(budget.report()["capacity_status"], "disabled");
            assert_eq!(budget.report()["capacity_reason"], "descriptor_headroom");
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "extract::pdf::tests::low_rlimit_subprocess_disables_optional_spooling",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "low-RLIMIT child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn multi_megabyte_spool_replays_with_exact_integrity() {
        const BODY_BYTES: usize = 2 * 1024 * 1024;
        let state = tempfile::tempdir().unwrap();
        let body = "quarterly-results ".repeat(BODY_BYTES / "quarterly-results ".len());
        let record = crate::extract::RawRecord {
            fields: serde_json::Map::from_iter([("body".into(), body.clone().into())]),
            locator: "p1-s0".into(),
            group: None,
            origin: super::FieldOrigin::Extractor,
        };
        let budget = super::ExtractionSpoolBudget::new(super::MAX_WORKER_OUTPUT as u64, 1);
        let spool = spool_response(
            state.path(),
            7,
            "digest",
            &response(vec![record]),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        assert!(spool.bytes >= BODY_BYTES as u64);
        assert!(spool.bytes < super::MAX_WORKER_OUTPUT as u64);

        let mut replayed_body = None;
        let stats = spool
            .replay(7, "digest", &mut |record| {
                replayed_body = record.fields["body"].as_str().map(ToOwned::to_owned);
                true
            })
            .unwrap();
        assert_eq!(stats.records, 1);
        assert_eq!(replayed_body.as_deref(), Some(body.as_str()));
    }

    #[test]
    fn spool_replay_verifies_exact_length_and_digest_before_decode() {
        let source = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = source.path().join("quarterly-report.pdf");
        write_prose_pdf(&path);
        let records = extract_in_process(&path).unwrap();

        let budget = super::ExtractionSpoolBudget::new(3 * super::MAX_WORKER_OUTPUT as u64, 3);
        let truncated = spool_response(
            state.path(),
            7,
            "digest",
            &response(records.clone()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        truncated.file.lock().unwrap().set_len(8).unwrap();
        let error = truncated.replay(7, "digest", &mut |_| true).unwrap_err();
        assert!(format!("{error:#}").contains("length changed"));

        let appended = spool_response(
            state.path(),
            7,
            "digest",
            &response(records.clone()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        {
            let mut file = appended.file.lock().unwrap();
            file.seek(std::io::SeekFrom::End(0)).unwrap();
            file.write_all(b" ").unwrap();
            file.flush().unwrap();
        }
        let error = appended.replay(7, "digest", &mut |_| true).unwrap_err();
        assert!(format!("{error:#}").contains("length changed"));

        let mutated = spool_response(
            state.path(),
            7,
            "digest",
            &response(records.clone()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        {
            let mut file = mutated.file.lock().unwrap();
            file.rewind().unwrap();
            let mut first = [0u8; 1];
            file.read_exact(&mut first).unwrap();
            file.rewind().unwrap();
            file.write_all(if first == *b"{" { b"[" } else { b"{" })
                .unwrap();
            file.flush().unwrap();
        }
        assert_eq!(
            mutated.file.lock().unwrap().metadata().unwrap().len(),
            mutated.bytes
        );
        let error = mutated.replay(7, "digest", &mut |_| true).unwrap_err();
        assert!(error.to_string().contains("content changed"));
    }

    fn rewrite_spool_and_reseal(
        spool: &mut super::ExtractionSpool,
        rewrite: impl FnOnce(&mut Vec<u8>),
    ) {
        let mut file = spool.file.lock().unwrap();
        file.rewind().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        rewrite(&mut bytes);
        file.set_len(0).unwrap();
        file.rewind().unwrap();
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();
        spool.bytes = bytes.len() as u64;
        spool.artifact_digest = super::artifact_digest(&mut file, spool.bytes).unwrap();
        file.rewind().unwrap();
    }

    #[test]
    fn replay_rejects_malformed_json_after_physical_integrity_passes() {
        let state = tempfile::tempdir().unwrap();
        let budget = super::ExtractionSpoolBudget::new(super::MAX_WORKER_OUTPUT as u64, 1);
        let mut spool = spool_response(
            state.path(),
            7,
            "digest",
            &response(Vec::new()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        rewrite_spool_and_reseal(&mut spool, |bytes| bytes[0] = b'[');

        let error = spool.replay(7, "digest", &mut |_| true).unwrap_err();
        assert!(
            format!("{error:#}").contains("malformed or truncated"),
            "{error:#}"
        );
        assert_eq!(budget.report()["replay_integrity_failures"], 1);
    }

    #[test]
    fn replay_rejects_worker_protocol_mismatch_after_integrity_passes() {
        let state = tempfile::tempdir().unwrap();
        let budget = super::ExtractionSpoolBudget::new(super::MAX_WORKER_OUTPUT as u64, 1);
        let mut spool = spool_response(
            state.path(),
            7,
            "digest",
            &response(Vec::new()),
            budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
        )
        .unwrap();
        rewrite_spool_and_reseal(&mut spool, |bytes| {
            let needle = b"xerj-autoindex/";
            let offset = bytes
                .windows(needle.len())
                .position(|window| window == needle)
                .unwrap();
            bytes[offset] = b'X';
        });

        let error = spool.replay(7, "digest", &mut |_| true).unwrap_err();
        assert!(
            format!("{error:#}").contains("extractor version mismatch"),
            "{error:#}"
        );
        assert_eq!(budget.report()["replay_integrity_failures"], 1);
    }

    #[test]
    fn concurrent_spool_replay_never_exceeds_configured_pdf_gate() {
        const REPLAYS: usize = 8;
        const LIMIT: usize = 2;
        let state = tempfile::tempdir().unwrap();
        let budget = super::ExtractionSpoolBudget::new(
            REPLAYS as u64 * super::MAX_WORKER_OUTPUT as u64,
            REPLAYS as u64,
        );
        let records = vec![crate::extract::RawRecord {
            fields: serde_json::Map::new(),
            locator: "page:1".into(),
            group: None,
            origin: super::FieldOrigin::Extractor,
        }];
        let spools: Vec<_> = (0..REPLAYS)
            .map(|_| {
                spool_response(
                    state.path(),
                    7,
                    "digest",
                    &response(records.clone()),
                    budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).unwrap(),
                )
                .unwrap()
            })
            .collect();
        let gate = Arc::new(super::WorkerGate::new(LIMIT));
        let start = Arc::new(Barrier::new(REPLAYS));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for spool in spools {
                let gate = Arc::clone(&gate);
                let start = Arc::clone(&start);
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                scope.spawn(move || {
                    start.wait();
                    spool
                        .replay_with_gate(
                            7,
                            "digest",
                            &mut |_| {
                                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                                maximum.fetch_max(now, Ordering::SeqCst);
                                std::thread::sleep(Duration::from_millis(20));
                                active.fetch_sub(1, Ordering::SeqCst);
                                true
                            },
                            &gate,
                        )
                        .unwrap();
                });
            }
        });
        assert_eq!(maximum.load(Ordering::SeqCst), LIMIT);
    }

    #[test]
    fn spool_budget_is_atomic_and_refunded_on_physical_drop() {
        let budget = super::ExtractionSpoolBudget::new(100, 2);
        let mut first = budget.try_reserve(80).unwrap();
        assert!(budget.try_reserve(21).is_err());
        first.shrink_to(40);
        let second = budget.try_reserve(60).unwrap();
        assert!(budget.try_reserve(1).is_err());
        drop(second);
        assert!(budget.try_reserve(60).is_ok());
        drop(first);

        let count_budget = super::ExtractionSpoolBudget::new(1_000, 1);
        let only = count_budget.try_reserve(1).unwrap();
        assert!(count_budget.try_reserve(1).is_err());
        drop(only);
        assert!(count_budget.try_reserve(1).is_ok());
    }

    #[test]
    fn concurrent_reservations_report_exact_peak_live_artifacts() {
        const RESERVATIONS: usize = 32;
        let budget = super::ExtractionSpoolBudget::new(RESERVATIONS as u64, RESERVATIONS as u64);
        let barrier = Arc::new(Barrier::new(RESERVATIONS + 1));
        std::thread::scope(|scope| {
            for _ in 0..RESERVATIONS {
                let budget = Arc::clone(&budget);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let reservation = budget.try_reserve(1).unwrap();
                    barrier.wait();
                    barrier.wait();
                    drop(reservation);
                });
            }
            barrier.wait();
            assert_eq!(
                budget.peak_live_artifacts.load(Ordering::Acquire),
                RESERVATIONS as u64
            );
            assert_eq!(budget.spools.load(Ordering::Acquire), RESERVATIONS as u64);
            barrier.wait();
        });
        assert_eq!(budget.spools.load(Ordering::Acquire), 0);
    }

    #[test]
    fn spool_io_failure_preserves_a_clean_fallback_and_refunds_capacity() {
        let state = tempfile::tempdir().unwrap();
        let not_a_directory = state.path().join("regular-file");
        std::fs::write(&not_a_directory, b"not a directory").unwrap();
        let budget = super::ExtractionSpoolBudget::new(super::MAX_WORKER_OUTPUT as u64, 1);
        let response = response(Vec::new());

        let (spool, fallback) =
            try_spool_response(&not_a_directory, 7, "digest", &budget, &response);
        assert!(spool.is_none());
        let fallback = fallback.unwrap();
        assert_eq!(fallback.category, "artifact_io");
        assert!(fallback.message.contains("could not retain"));
        assert!(fallback.message.contains("anonymous PDF extraction spool"));

        // The failed attempt must not strand either the byte or handle charge.
        assert!(budget.try_reserve(super::MAX_WORKER_OUTPUT as u64).is_ok());
    }
}
