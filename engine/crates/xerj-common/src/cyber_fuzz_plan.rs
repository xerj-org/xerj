//! Deterministic `cyber.fuzz_plan` (#2773, #2767, #2769).
//!
//! Ranks parser, decoder, and deserializer entry points from caller-supplied
//! surface signals and cited corpus bugs. Selects seed and dictionary metadata
//! only after a licence-class gate. Deduplicates crashes by a normalized
//! stack hash. Emits harness skeletons as text.
//!
//! This module does not fetch corpora, compile harnesses, run a fuzzer, or
//! send any traffic. Uncited candidates are abstained, never guessed. Hashes
//! are integrity checks, not exploitability claims.

use crate::hub::LicenceClass;
use crate::hub_evidence::{sha256, PinnedCitation};
use chrono::{DateTime, NaiveDate};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const REPORT_VERSION: &str = "xerj-cyber-fuzz-plan/v1";
pub const STACK_HASH_TOP_K: usize = 8;

const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEXT: usize = 4_096;
const MAX_STACK: usize = 256 * 1024;
const MAX_ITEMS: usize = 256;
const ABSTAIN_REASON: &str = "no cited corpus evidence; excluded rather than guessed";

#[derive(Debug, thiserror::Error)]
pub enum FuzzPlanError {
    #[error("fuzz plan refused: {0}")]
    InvalidInput(&'static str),
    #[error("fuzz plan refused: seed or dictionary drawn from an Excluded corpus licence")]
    ExcludedCorpusLicence,
    #[error("fuzz plan report digest mismatch")]
    DigestMismatch,
    #[error("fuzz plan JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, FuzzPlanError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Parser,
    Decoder,
    Deserializer,
    Other,
}

impl EntryKind {
    fn keyword(self) -> &'static str {
        match self {
            Self::Parser => "parser",
            Self::Decoder => "decoder",
            Self::Deserializer => "deserializer",
            Self::Other => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizerKind {
    Address,
    Memory,
    UndefinedBehavior,
    Thread,
    Crash,
    Timeout,
    Unknown,
}

impl SanitizerKind {
    fn as_token(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Memory => "memory",
            Self::UndefinedBehavior => "undefined_behavior",
            Self::Thread => "thread",
            Self::Crash => "crash",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityHint {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessEngine {
    CargoFuzz,
    LibFuzzer,
    AflPlusPlus,
}

/// Cited historical corpus bug. A ranked target must carry at least one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusBugRef {
    pub corpus: String,
    pub record_id: String,
    pub cwe: String,
    pub summary: String,
    pub pin_sha: String,
    pub fetched_at: String,
    pub pinned_citation: PinnedCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceSignals {
    pub untrusted_bytes_input: bool,
    pub unsafe_blocks: u32,
    pub arithmetic_length_parsing: bool,
    pub recursion: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuzzCandidate {
    pub id: String,
    pub function: String,
    pub file: String,
    pub line: u32,
    pub kind: EntryKind,
    pub signals: SurfaceSignals,
    pub cited_bugs: Vec<CorpusBugRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankedTarget {
    pub rank: u32,
    pub candidate_id: String,
    pub function: String,
    pub file: String,
    pub line: u32,
    pub kind: EntryKind,
    pub score: u32,
    pub rationale: String,
    pub cited_bugs: Vec<CorpusBugRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbstainedCandidate {
    pub candidate_id: String,
    pub function: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedCorpusEntry {
    pub name: String,
    pub corpus: String,
    pub record_id: String,
    pub pin_sha: String,
    pub fetched_at: String,
    pub licence_class: LicenceClass,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictionaryToken {
    pub token: String,
    pub corpus: String,
    pub record_id: String,
    pub pin_sha: String,
    pub fetched_at: String,
    pub licence_class: LicenceClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashInput {
    pub id: String,
    pub sanitizer: SanitizerKind,
    pub signal: String,
    pub stack: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashBucket {
    pub stack_hash: String,
    pub sanitizer: SanitizerKind,
    pub signal: String,
    pub normalized_frames: Vec<String>,
    pub crash_ids: Vec<String>,
    pub matched_bugs: Vec<CorpusBugRef>,
    pub severity_hint: SeverityHint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSkeleton {
    pub engine: HarnessEngine,
    pub target_id: String,
    pub filename: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuzzPlanReport {
    pub report_version: String,
    pub crate_name: String,
    pub ranked_targets: Vec<RankedTarget>,
    pub abstained: Vec<AbstainedCandidate>,
    pub seeds: Vec<SeedCorpusEntry>,
    pub dictionary: Vec<DictionaryToken>,
    pub crash_buckets: Vec<CrashBucket>,
    pub harnesses: Vec<HarnessSkeleton>,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct Preimage<'a> {
    report_version: &'a str,
    crate_name: &'a str,
    ranked_targets: &'a [RankedTarget],
    abstained: &'a [AbstainedCandidate],
    seeds: &'a [SeedCorpusEntry],
    dictionary: &'a [DictionaryToken],
    crash_buckets: &'a [CrashBucket],
    harnesses: &'a [HarnessSkeleton],
}

impl FuzzPlanReport {
    /// Build a sealed plan. Candidates without a cited bug are abstained.
    /// An `Excluded` seed or dictionary fails the whole plan.
    pub fn plan(
        crate_name: &str,
        candidates: &[FuzzCandidate],
        seeds: &[SeedCorpusEntry],
        dictionary: &[DictionaryToken],
        crashes: &[CrashInput],
        known_bugs: &[CorpusBugRef],
    ) -> Result<Self> {
        if !valid_slug(crate_name, 64) {
            return Err(FuzzPlanError::InvalidInput(
                "crate_name must be 1..=64 ASCII [A-Za-z0-9_-]",
            ));
        }
        if candidates.len() > MAX_ITEMS
            || seeds.len() > MAX_ITEMS
            || dictionary.len() > MAX_ITEMS
            || crashes.len() > MAX_ITEMS
            || known_bugs.len() > MAX_ITEMS
        {
            return Err(FuzzPlanError::InvalidInput(
                "fuzz plan input exceeds the item limit",
            ));
        }
        let (ranked_targets, abstained) = rank_candidates(candidates)?;
        let seeds = select_seeds(seeds)?;
        let dictionary = select_dictionary(dictionary)?;
        let crash_buckets = dedup_crashes(crashes, known_bugs)?;
        let harnesses = generate_harnesses(&ranked_targets)?;
        let mut report = Self {
            report_version: REPORT_VERSION.to_string(),
            crate_name: crate_name.to_string(),
            ranked_targets,
            abstained,
            seeds,
            dictionary,
            crash_buckets,
            harnesses,
            report_sha256: String::new(),
        };
        report.report_sha256 = report.compute_digest()?;
        let bytes = serde_json_canonicalizer::to_vec(&report)?;
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(FuzzPlanError::InvalidInput("report exceeds size limit"));
        }
        Ok(report)
    }

    fn compute_digest(&self) -> Result<String> {
        let pre = Preimage {
            report_version: &self.report_version,
            crate_name: &self.crate_name,
            ranked_targets: &self.ranked_targets,
            abstained: &self.abstained,
            seeds: &self.seeds,
            dictionary: &self.dictionary,
            crash_buckets: &self.crash_buckets,
            harnesses: &self.harnesses,
        };
        Ok(sha256(&serde_json_canonicalizer::to_vec(&pre)?))
    }

    pub fn verify(&self) -> Result<()> {
        if self.report_version != REPORT_VERSION {
            return Err(FuzzPlanError::InvalidInput("unsupported report_version"));
        }
        if self.report_sha256 != self.compute_digest()? {
            return Err(FuzzPlanError::DigestMismatch);
        }
        for target in &self.ranked_targets {
            if target.rationale.trim().is_empty() || target.cited_bugs.is_empty() {
                return Err(FuzzPlanError::InvalidInput(
                    "ranked target is missing a rationale or cited bug",
                ));
            }
        }
        for seed in &self.seeds {
            if seed.licence_class == LicenceClass::Excluded {
                return Err(FuzzPlanError::ExcludedCorpusLicence);
            }
        }
        for token in &self.dictionary {
            if token.licence_class == LicenceClass::Excluded {
                return Err(FuzzPlanError::ExcludedCorpusLicence);
            }
        }
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.verify()?;
        let out = serde_json_canonicalizer::to_vec(self)?;
        if out.len() > MAX_REPORT_BYTES {
            return Err(FuzzPlanError::InvalidInput("report exceeds size limit"));
        }
        Ok(out)
    }

    pub fn verify_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(FuzzPlanError::InvalidInput("report exceeds size limit"));
        }
        reject_duplicate_json_keys(bytes)?;
        let report: Self = serde_json::from_slice(bytes)?;
        report.verify()?;
        Ok(report)
    }
}

/// Rank candidates with cited bugs. Others are abstained, not guessed.
pub fn rank_candidates(
    candidates: &[FuzzCandidate],
) -> Result<(Vec<RankedTarget>, Vec<AbstainedCandidate>)> {
    let mut ranked = Vec::new();
    let mut abstained = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        validate_candidate(candidate)?;
        if !seen.insert(candidate.id.clone()) {
            return Err(FuzzPlanError::InvalidInput("duplicate candidate id"));
        }
        if candidate.cited_bugs.is_empty() {
            abstained.push(AbstainedCandidate {
                candidate_id: candidate.id.clone(),
                function: candidate.function.clone(),
                reason: ABSTAIN_REASON.to_string(),
            });
            continue;
        }
        let mut cited = candidate.cited_bugs.clone();
        cited.sort_by(|a, b| {
            a.corpus
                .cmp(&b.corpus)
                .then_with(|| a.record_id.cmp(&b.record_id))
        });
        let similarity = cited
            .iter()
            .map(|bug| bug_similarity(candidate, bug))
            .sum::<u32>()
            .min(40);
        let score = surface_score(&candidate.signals) + similarity;
        ranked.push((score, similarity, cited, candidate));
    }
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.3.function.cmp(&b.3.function))
            .then_with(|| a.3.file.cmp(&b.3.file))
            .then_with(|| a.3.line.cmp(&b.3.line))
            .then_with(|| a.3.id.cmp(&b.3.id))
    });
    abstained.sort_by(|a, b| {
        a.candidate_id
            .cmp(&b.candidate_id)
            .then_with(|| a.function.cmp(&b.function))
    });
    let ranked_targets = ranked
        .into_iter()
        .enumerate()
        .map(|(idx, (score, similarity, cited_bugs, candidate))| {
            let rank = u32::try_from(idx + 1).unwrap_or(u32::MAX);
            RankedTarget {
                rationale: rationale(rank, score, candidate, &cited_bugs, similarity),
                cited_bugs,
                rank,
                candidate_id: candidate.id.clone(),
                function: candidate.function.clone(),
                file: candidate.file.clone(),
                line: candidate.line,
                kind: candidate.kind,
                score,
            }
        })
        .collect();
    Ok((ranked_targets, abstained))
}

fn surface_score(signals: &SurfaceSignals) -> u32 {
    u32::from(signals.untrusted_bytes_input) * 40
        + signals.unsafe_blocks.min(3) * 10
        + u32::from(signals.arithmetic_length_parsing) * 20
        + u32::from(signals.recursion) * 15
}

fn bug_similarity(candidate: &FuzzCandidate, bug: &CorpusBugRef) -> u32 {
    let func = ident_tokens(&candidate.function);
    let summary = ident_tokens(&bug.summary);
    let shared = func.intersection(&summary).count() as u32;
    let mut score = shared.min(3) * 5;
    let kind = candidate.kind.keyword();
    if !kind.is_empty() && bug.summary.to_ascii_lowercase().contains(kind) {
        score += 10;
    }
    score
}

fn rationale(
    rank: u32,
    score: u32,
    candidate: &FuzzCandidate,
    cited_bugs: &[CorpusBugRef],
    similarity: u32,
) -> String {
    let cites = cited_bugs
        .iter()
        .map(|bug| format!("{}:{} ({})", bug.corpus, bug.record_id, bug.cwe))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "rank {rank}: score {score} from untrusted_input={} unsafe_blocks={} arithmetic_length={} recursion={} corpus_similarity={similarity}; cited {cites}",
        candidate.signals.untrusted_bytes_input,
        candidate.signals.unsafe_blocks,
        candidate.signals.arithmetic_length_parsing,
        candidate.signals.recursion,
    )
}

/// Refuse `Excluded` seeds. Provenance (`pin_sha`, `fetched_at`) is retained.
pub fn select_seeds(seeds: &[SeedCorpusEntry]) -> Result<Vec<SeedCorpusEntry>> {
    let mut out = Vec::with_capacity(seeds.len());
    let mut seen = BTreeSet::new();
    for seed in seeds {
        validate_seed(seed)?;
        if !seen.insert((
            seed.corpus.clone(),
            seed.record_id.clone(),
            seed.name.clone(),
        )) {
            return Err(FuzzPlanError::InvalidInput("duplicate seed identity"));
        }
        out.push(seed.clone());
    }
    out.sort_by(|a, b| {
        a.corpus
            .cmp(&b.corpus)
            .then_with(|| a.record_id.cmp(&b.record_id))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Refuse `Excluded` dictionary tokens. Provenance is retained.
pub fn select_dictionary(tokens: &[DictionaryToken]) -> Result<Vec<DictionaryToken>> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut seen = BTreeSet::new();
    for token in tokens {
        validate_dictionary(token)?;
        if !seen.insert((
            token.corpus.clone(),
            token.record_id.clone(),
            token.token.clone(),
        )) {
            return Err(FuzzPlanError::InvalidInput("duplicate dictionary token"));
        }
        out.push(token.clone());
    }
    out.sort_by(|a, b| {
        a.corpus
            .cmp(&b.corpus)
            .then_with(|| a.record_id.cmp(&b.record_id))
            .then_with(|| a.token.cmp(&b.token))
    });
    Ok(out)
}

type CrashKey = (SanitizerKind, String, String);

struct CrashGroup {
    frames: Vec<String>,
    ids: Vec<String>,
}

/// Same crash set, any input order, same dedup buckets.
pub fn dedup_crashes(
    crashes: &[CrashInput],
    known_bugs: &[CorpusBugRef],
) -> Result<Vec<CrashBucket>> {
    for bug in known_bugs {
        validate_bug(bug)?;
    }
    let mut groups: BTreeMap<CrashKey, CrashGroup> = BTreeMap::new();
    let mut seen_ids = BTreeSet::new();
    for crash in crashes {
        validate_crash(crash)?;
        if !seen_ids.insert(crash.id.clone()) {
            return Err(FuzzPlanError::InvalidInput("duplicate crash id"));
        }
        let frames = normalize_stack(&crash.stack);
        let signal = normalize_signal(&crash.signal);
        let hash = stack_hash(crash.sanitizer, &signal, &frames);
        let entry = groups
            .entry((crash.sanitizer, signal, hash))
            .or_insert_with(|| CrashGroup {
                frames,
                ids: Vec::new(),
            });
        entry.ids.push(crash.id.clone());
    }
    let mut buckets = Vec::with_capacity(groups.len());
    for ((sanitizer, signal, stack_hash), mut group) in groups {
        group.ids.sort();
        let crash_ids = group.ids;
        let frames = group.frames;
        let mut matched_bugs: Vec<CorpusBugRef> = known_bugs
            .iter()
            .filter(|bug| bug_matches_frames(bug, &frames))
            .cloned()
            .collect();
        matched_bugs.sort_by(|a, b| {
            a.corpus
                .cmp(&b.corpus)
                .then_with(|| a.record_id.cmp(&b.record_id))
        });
        matched_bugs.dedup_by(|a, b| a.corpus == b.corpus && a.record_id == b.record_id);
        let severity_hint = severity_hint(sanitizer, &signal, &matched_bugs);
        buckets.push(CrashBucket {
            stack_hash,
            sanitizer,
            signal,
            normalized_frames: frames,
            crash_ids,
            matched_bugs,
            severity_hint,
        });
    }
    Ok(buckets)
}

pub fn normalize_stack(raw: &str) -> Vec<String> {
    let mut frames = Vec::new();
    for line in raw.lines() {
        if let Some(frame) = normalize_frame_line(line) {
            frames.push(frame);
            if frames.len() == STACK_HASH_TOP_K {
                break;
            }
        }
    }
    frames
}

pub fn normalize_frame_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_TEXT {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("error:")
        || lower.starts_with("summary:")
        || lower.starts_with("warning:")
        || lower.starts_with("stack backtrace")
        || lower.starts_with("note:")
    {
        return None;
    }
    let stripped = strip_column_jitter(&strip_pid_markers(&strip_hex_addresses(&strip_build_ids(
        trimmed,
    ))));
    let mut tokens: Vec<&str> = stripped.split_whitespace().collect();
    while let Some(tok) = tokens.first().copied() {
        let bare = tok.trim_matches(|c: char| c == '#' || c == ':' || c == ',');
        if bare.is_empty() || bare.bytes().all(|b| b.is_ascii_digit()) {
            tokens.remove(0);
            continue;
        }
        if bare.eq_ignore_ascii_case("in") || bare.eq_ignore_ascii_case("at") {
            tokens.remove(0);
            continue;
        }
        break;
    }
    tokens.retain(|tok| {
        let bare = tok.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '+' | ':'));
        !(bare.len() >= 8 && bare.bytes().all(|b| b.is_ascii_hexdigit()))
    });
    if tokens.is_empty() {
        return None;
    }
    let joined = tokens.join(" ");
    if !joined.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(joined)
}

fn stack_hash(sanitizer: SanitizerKind, signal: &str, frames: &[String]) -> String {
    let mut pre = String::new();
    pre.push_str(sanitizer.as_token());
    pre.push('\n');
    pre.push_str(signal);
    pre.push('\n');
    for frame in frames.iter().take(STACK_HASH_TOP_K) {
        pre.push_str(frame);
        pre.push('\n');
    }
    sha256(pre.as_bytes())
}

fn normalize_signal(signal: &str) -> String {
    signal
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn severity_hint(sanitizer: SanitizerKind, signal: &str, matched: &[CorpusBugRef]) -> SeverityHint {
    let mut hint = match sanitizer {
        SanitizerKind::Address => {
            if contains_any(
                signal,
                &[
                    "heap-buffer-overflow",
                    "stack-buffer-overflow",
                    "use-after-free",
                    "double-free",
                    "heap-use-after-free",
                    "stack-use-after-return",
                ],
            ) {
                SeverityHint::Critical
            } else {
                SeverityHint::High
            }
        }
        SanitizerKind::Memory => SeverityHint::High,
        SanitizerKind::UndefinedBehavior => {
            if signal.contains("overflow") || signal.contains("shift") {
                SeverityHint::High
            } else {
                SeverityHint::Medium
            }
        }
        SanitizerKind::Thread | SanitizerKind::Timeout => SeverityHint::Medium,
        SanitizerKind::Crash => {
            if signal.contains("sigsegv") || signal.contains("sigbus") {
                SeverityHint::High
            } else if signal.contains("sigabrt") {
                SeverityHint::Medium
            } else {
                SeverityHint::Low
            }
        }
        SanitizerKind::Unknown => SeverityHint::Low,
    };
    for bug in matched {
        if memory_corruption_cwe(&bug.cwe) {
            hint = SeverityHint::Critical;
        }
    }
    hint
}

fn memory_corruption_cwe(cwe: &str) -> bool {
    matches!(
        cwe,
        "CWE-119"
            | "CWE-120"
            | "CWE-121"
            | "CWE-122"
            | "CWE-124"
            | "CWE-125"
            | "CWE-415"
            | "CWE-416"
            | "CWE-787"
    )
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| hay.contains(needle))
}

fn bug_matches_frames(bug: &CorpusBugRef, frames: &[String]) -> bool {
    if frames
        .iter()
        .any(|frame| contains_token(frame, &bug.record_id))
    {
        return true;
    }
    let symbols = match_symbols(bug);
    if symbols.is_empty() {
        return false;
    }
    frames.iter().any(|frame| {
        symbols
            .iter()
            .any(|symbol| frame_contains_symbol(frame, symbol))
    })
}

fn match_symbols(bug: &CorpusBugRef) -> Vec<String> {
    let mut symbols = Vec::new();
    let mut rest = bug.summary.as_str();
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('`') {
            let symbol = &rest[..end];
            if valid_symbol(symbol) {
                symbols.push(symbol.to_string());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    for token in bug
        .summary
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
    {
        if (token.contains('_') || token.contains("::")) && valid_symbol(token) {
            symbols.push(token.to_string());
        }
    }
    symbols.sort();
    symbols.dedup();
    symbols
}

fn frame_contains_symbol(frame: &str, symbol: &str) -> bool {
    frame
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '(' | ')' | ',' | '+'))
        .flat_map(|token| token.split("::"))
        .any(|token| token == symbol)
}

fn contains_token(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    hay.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')))
        .any(|token| token == needle)
}

/// Text skeletons only. This function does not compile or execute them.
pub fn generate_harnesses(targets: &[RankedTarget]) -> Result<Vec<HarnessSkeleton>> {
    let mut out = Vec::with_capacity(targets.len().saturating_mul(3));
    for target in targets {
        if !valid_function(&target.function) || !valid_id(&target.candidate_id) {
            return Err(FuzzPlanError::InvalidInput(
                "harness target has an unsafe function or id",
            ));
        }
        let c_symbol = target.function.replace("::", "_");
        out.push(HarnessSkeleton {
            engine: HarnessEngine::CargoFuzz,
            target_id: target.candidate_id.clone(),
            filename: format!("fuzz/cargo/{}.rs", target.candidate_id),
            source: format!(
                "#![no_main]\n\n// Target: {function} ({file}:{line})\n// Generated by {REPORT_VERSION}. Skeleton only; not compiled or executed here.\nlibfuzzer_sys::fuzz_target!(|data: &[u8]| {{\n    let _ = {function}(data);\n}});\n",
                function = target.function,
                file = target.file,
                line = target.line,
            ),
        });
        out.push(HarnessSkeleton {
            engine: HarnessEngine::LibFuzzer,
            target_id: target.candidate_id.clone(),
            filename: format!("fuzz/libfuzzer/{}.c", target.candidate_id),
            source: format!(
                "#include <stdint.h>\n#include <stddef.h>\n\n/* Target: {function} ({file}:{line}) */\n/* Generated by {REPORT_VERSION}. Skeleton only; not compiled or executed here. */\nextern int {c_symbol}(const uint8_t *data, size_t size);\n\nint LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {{\n    (void){c_symbol}(data, size);\n    return 0;\n}}\n",
                function = target.function,
                file = target.file,
                line = target.line,
            ),
        });
        out.push(HarnessSkeleton {
            engine: HarnessEngine::AflPlusPlus,
            target_id: target.candidate_id.clone(),
            filename: format!("fuzz/afl/{}.rs", target.candidate_id),
            source: format!(
                "// Target: {function} ({file}:{line})\n// Generated by {REPORT_VERSION}. Skeleton only; not compiled or executed here.\nfn main() {{\n    afl::fuzz!(|data: &[u8]| {{\n        let _ = {function}(data);\n    }});\n}}\n",
                function = target.function,
                file = target.file,
                line = target.line,
            ),
        });
    }
    Ok(out)
}

fn validate_candidate(candidate: &FuzzCandidate) -> Result<()> {
    if !valid_id(&candidate.id) {
        return Err(FuzzPlanError::InvalidInput(
            "candidate id must be 1..=128 ASCII [A-Za-z0-9_.:-]",
        ));
    }
    if !valid_function(&candidate.function)
        || !valid_rel_path(&candidate.file)
        || candidate.line == 0
    {
        return Err(FuzzPlanError::InvalidInput(
            "candidate function, file, or line is invalid",
        ));
    }
    if candidate.cited_bugs.len() > MAX_ITEMS {
        return Err(FuzzPlanError::InvalidInput("too many cited bugs"));
    }
    for bug in &candidate.cited_bugs {
        validate_bug(bug)?;
    }
    Ok(())
}

fn validate_bug(bug: &CorpusBugRef) -> Result<()> {
    if !valid_slug(&bug.corpus, 128) || !valid_id(&bug.record_id) || !valid_cwe(&bug.cwe) {
        return Err(FuzzPlanError::InvalidInput(
            "corpus bug corpus, record_id, or cwe is invalid",
        ));
    }
    if !valid_line(&bug.summary) {
        return Err(FuzzPlanError::InvalidInput(
            "corpus bug summary must be non-empty single-line text",
        ));
    }
    validate_pin(&bug.pin_sha, &bug.fetched_at)?;
    validate_citation(&bug.pinned_citation, &bug.corpus, &bug.pin_sha)?;
    Ok(())
}

fn validate_seed(seed: &SeedCorpusEntry) -> Result<()> {
    if seed.licence_class == LicenceClass::Excluded {
        return Err(FuzzPlanError::ExcludedCorpusLicence);
    }
    if !valid_slug(&seed.name, 128) || !valid_slug(&seed.corpus, 128) || !valid_id(&seed.record_id)
    {
        return Err(FuzzPlanError::InvalidInput("seed identity is invalid"));
    }
    if !hex_lower(&seed.content_sha256, 64) {
        return Err(FuzzPlanError::InvalidInput(
            "seed content_sha256 must be 64 lowercase hex characters",
        ));
    }
    validate_pin(&seed.pin_sha, &seed.fetched_at)
}

fn validate_dictionary(token: &DictionaryToken) -> Result<()> {
    if token.licence_class == LicenceClass::Excluded {
        return Err(FuzzPlanError::ExcludedCorpusLicence);
    }
    if !valid_token(&token.token) || !valid_slug(&token.corpus, 128) || !valid_id(&token.record_id)
    {
        return Err(FuzzPlanError::InvalidInput("dictionary token is invalid"));
    }
    validate_pin(&token.pin_sha, &token.fetched_at)
}

fn validate_crash(crash: &CrashInput) -> Result<()> {
    if !valid_id(&crash.id) || !valid_line(&crash.signal) || crash.stack.len() > MAX_STACK {
        return Err(FuzzPlanError::InvalidInput(
            "crash id, signal, or stack is invalid",
        ));
    }
    if crash
        .stack
        .chars()
        .any(|c| c != '\n' && c != '\r' && c != '\t' && (c < ' ' || c == '\u{7f}'))
    {
        return Err(FuzzPlanError::InvalidInput(
            "crash stack contains control characters",
        ));
    }
    Ok(())
}

fn validate_pin(pin_sha: &str, fetched_at: &str) -> Result<()> {
    if !hex_lower(pin_sha, 40) {
        return Err(FuzzPlanError::InvalidInput(
            "pin_sha must be a 40-character lowercase hex git SHA",
        ));
    }
    if !valid_fetched_at(fetched_at) {
        return Err(FuzzPlanError::InvalidInput(
            "fetched_at must be YYYY-MM-DD or RFC 3339",
        ));
    }
    Ok(())
}

fn validate_citation(citation: &PinnedCitation, corpus: &str, pin_sha: &str) -> Result<()> {
    if citation.corpus != corpus || citation.commit != pin_sha {
        return Err(FuzzPlanError::InvalidInput(
            "pinned_citation corpus/commit must match the corpus bug pin",
        ));
    }
    if !valid_repo(&citation.repository) || !valid_rel_path(&citation.file) {
        return Err(FuzzPlanError::InvalidInput(
            "pinned_citation path is invalid",
        ));
    }
    if !hex_lower(&citation.file_sha256, 64) || !hex_lower(&citation.span_sha256, 64) {
        return Err(FuzzPlanError::InvalidInput(
            "pinned_citation digests must be lowercase SHA-256",
        ));
    }
    if citation.byte_start >= citation.byte_end
        || citation.line_start == 0
        || citation.line_start > citation.line_end
    {
        return Err(FuzzPlanError::InvalidInput(
            "pinned_citation bounds are invalid",
        ));
    }
    Ok(())
}

fn valid_fetched_at(value: &str) -> bool {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
        || DateTime::parse_from_rfc3339(value).is_ok()
}

fn valid_repo(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/' | b'.'))
}

fn valid_slug(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

fn valid_cwe(value: &str) -> bool {
    let Some(number) = value.strip_prefix("CWE-") else {
        return false;
    };
    let digits = number.bytes().all(|b| b.is_ascii_digit());
    !number.is_empty() && number.len() <= 5 && digits && !number.starts_with('0')
}

fn valid_function(value: &str) -> bool {
    let mut parts = value.split("::");
    let Some(first) = parts.next() else {
        return false;
    };
    valid_ident(first) && parts.all(valid_ident) && value.len() <= 256
}

fn valid_ident(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {
            chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        }
        _ => false,
    }
}

fn valid_symbol(value: &str) -> bool {
    valid_function(value) && value.len() >= 3 && value.len() <= 128
}

fn valid_rel_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.contains(['\\', ':', '\0'])
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn valid_line(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_TEXT
        && !value.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(|c| c.is_control() || c == '\u{7f}')
}

fn hex_lower(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn ident_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 3 && !is_stopword(&token.to_ascii_lowercase()))
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn is_stopword(value: &str) -> bool {
    matches!(
        value,
        "the"
            | "and"
            | "for"
            | "with"
            | "from"
            | "into"
            | "bug"
            | "crash"
            | "overflow"
            | "buffer"
            | "heap"
            | "stack"
            | "in"
            | "of"
            | "to"
            | "on"
            | "by"
            | "a"
            | "an"
            | "via"
            | "historical"
    )
}

fn strip_build_ids(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if lower[i..].starts_with("(buildid:") {
            if let Some(end) = value[i..].find(')') {
                i += end + 1;
                continue;
            }
        }
        if lower[i..].starts_with("buildid:") {
            let mut j = i + "buildid:".len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > start {
                i = j;
                continue;
            }
        }
        let ch = value[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn strip_hex_addresses(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '+' && chars.peek() == Some(&'0') {
            let mut look = chars.clone();
            look.next();
            if matches!(look.next(), Some('x' | 'X'))
                && look.next().is_some_and(|c| c.is_ascii_hexdigit())
            {
                chars.next();
                chars.next();
                while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    chars.next();
                }
                continue;
            }
        }
        if ch == '0' && matches!(chars.peek(), Some('x' | 'X')) {
            let mut look = chars.clone();
            look.next();
            if look.next().is_some_and(|c| c.is_ascii_hexdigit()) {
                chars.next();
                while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    chars.next();
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn strip_pid_markers(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'=' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 && j + 1 < bytes.len() && bytes[j] == b'=' && bytes[j + 1] == b'=' {
                i = j + 2;
                continue;
            }
        }
        let ch = value[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn strip_column_jitter(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == b':' {
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 {
                    out.push_str(&value[i..j]);
                    i = k;
                    continue;
                }
            }
        }
        let ch = value[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<()> {
    struct DupCheck;
    impl<'de> Deserialize<'de> for DupCheck {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = DupCheck;
                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("any JSON value without duplicate object keys")
                }
                fn visit_bool<E>(self, _: bool) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_i64<E>(self, _: i64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_u64<E>(self, _: u64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_f64<E>(self, _: f64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_str<E>(self, _: &str) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_string<E>(self, _: String) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_unit<E>(self) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_none<E>(self) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> std::result::Result<DupCheck, A::Error> {
                    while seq.next_element::<DupCheck>()?.is_some() {}
                    Ok(DupCheck)
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<DupCheck, A::Error> {
                    let mut seen = HashSet::new();
                    while let Some((key, _)) = map.next_entry::<String, DupCheck>()? {
                        if !seen.insert(key) {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(DupCheck)
                }
            }
            deserializer.deserialize_any(V)
        }
    }
    let _: DupCheck = serde_json::from_slice(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_PIN: &str = "abcdef0123456789abcdef0123456789abcdef01";

    fn citation(corpus: &str, commit: &str) -> PinnedCitation {
        PinnedCitation::from_bytes(
            corpus.into(),
            "example/repo".into(),
            commit.into(),
            "src/parser.rs".into(),
            b"fn parse_length(data: &[u8]) -> usize { data.len() }\n",
            3..14,
        )
        .unwrap()
    }

    fn bug(corpus: &str, record_id: &str, summary: &str) -> CorpusBugRef {
        CorpusBugRef {
            corpus: corpus.into(),
            record_id: record_id.into(),
            cwe: "CWE-122".into(),
            summary: summary.into(),
            pin_sha: PIN.into(),
            fetched_at: "2026-01-15".into(),
            pinned_citation: citation(corpus, PIN),
        }
    }

    fn candidate(
        id: &str,
        function: &str,
        kind: EntryKind,
        signals: SurfaceSignals,
        cited: Vec<CorpusBugRef>,
    ) -> FuzzCandidate {
        FuzzCandidate {
            id: id.into(),
            function: function.into(),
            file: "src/parser.rs".into(),
            line: 42,
            kind,
            signals,
            cited_bugs: cited,
        }
    }

    fn signals(
        untrusted: bool,
        unsafe_blocks: u32,
        arithmetic: bool,
        recursion: bool,
    ) -> SurfaceSignals {
        SurfaceSignals {
            untrusted_bytes_input: untrusted,
            unsafe_blocks,
            arithmetic_length_parsing: arithmetic,
            recursion,
        }
    }

    fn seed(name: &str, licence: LicenceClass) -> SeedCorpusEntry {
        SeedCorpusEntry {
            name: name.into(),
            corpus: "oss-fuzz".into(),
            record_id: "seed-1".into(),
            pin_sha: PIN.into(),
            fetched_at: "2026-02-01T00:00:00Z".into(),
            licence_class: licence,
            content_sha256: "a".repeat(64),
        }
    }

    fn dict(token: &str, licence: LicenceClass) -> DictionaryToken {
        DictionaryToken {
            token: token.into(),
            corpus: "oss-fuzz".into(),
            record_id: "dict-1".into(),
            pin_sha: OTHER_PIN.into(),
            fetched_at: "2026-02-02".into(),
            licence_class: licence,
        }
    }

    #[test]
    fn ranking_cites_bugs_and_abstains_without_corpus_evidence() {
        let cited = bug(
            "oss-fuzz",
            "OSV-2020-1",
            "historical heap overflow in parser `parse_length`",
        );
        let strong = candidate(
            "parse-length",
            "parse_length",
            EntryKind::Parser,
            signals(true, 2, true, true),
            vec![cited],
        );
        let weak = candidate(
            "decode-header",
            "decode_header",
            EntryKind::Decoder,
            signals(true, 0, false, false),
            vec![bug(
                "oss-fuzz",
                "OSV-2021-9",
                "decoder length bug in `decode_header`",
            )],
        );
        let uncited = candidate(
            "helper",
            "helper_fn",
            EntryKind::Other,
            signals(true, 3, true, true),
            Vec::new(),
        );
        let forward = vec![strong.clone(), uncited.clone(), weak.clone()];
        let mut reverse = forward.clone();
        reverse.reverse();
        let (a, abs_a) = rank_candidates(&forward).unwrap();
        let (b, abs_b) = rank_candidates(&reverse).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].candidate_id, "parse-length");
        assert!(a[0].score > a[1].score);
        assert!(a
            .iter()
            .all(|t| !t.rationale.is_empty() && !t.cited_bugs.is_empty()));
        assert!(a[0].rationale.contains("oss-fuzz:OSV-2020-1"));
        assert_eq!(abs_a, abs_b);
        assert_eq!(abs_a[0].candidate_id, "helper");
        assert_eq!(abs_a[0].reason, ABSTAIN_REASON);
        assert_eq!(
            a.iter().map(|t| t.candidate_id.clone()).collect::<Vec<_>>(),
            b.iter().map(|t| t.candidate_id.clone()).collect::<Vec<_>>()
        );
        assert_eq!(a[0].score, b[0].score);
    }

    #[test]
    fn excluded_corpus_licence_refuses_seeds_and_dictionaries() {
        assert!(matches!(
            select_seeds(&[seed("bad", LicenceClass::Excluded)]),
            Err(FuzzPlanError::ExcludedCorpusLicence)
        ));
        assert!(matches!(
            select_dictionary(&[dict("AAA", LicenceClass::Excluded)]),
            Err(FuzzPlanError::ExcludedCorpusLicence)
        ));
        let seeds = select_seeds(&[
            seed("later", LicenceClass::ApproachOnly),
            SeedCorpusEntry {
                name: "early".into(),
                ..seed("early", LicenceClass::AdaptWithAttribution)
            },
        ])
        .unwrap();
        assert_eq!(seeds[0].name, "early");
        assert_eq!(seeds[0].pin_sha, PIN);
        assert_eq!(seeds[0].fetched_at, "2026-02-01T00:00:00Z");
        assert_eq!(seeds[1].licence_class, LicenceClass::ApproachOnly);
        let dictionary =
            select_dictionary(&[dict("\\n", LicenceClass::AdaptWithAttribution)]).unwrap();
        assert_eq!(dictionary[0].pin_sha, OTHER_PIN);
        assert_eq!(dictionary[0].fetched_at, "2026-02-02");
    }

    #[test]
    fn same_crash_set_has_the_same_dedup_buckets() {
        let stack_a = "\
==1423==ERROR: AddressSanitizer: heap-buffer-overflow on address 0x602000000010 at pc 0x0000555555555123\n\
    #0 0x0000555555555123 in parse_length /src/parser.rs:42:7\n\
    #1 0x0000555555555400 in decode (BuildId: a1b2c3d4e5f6789012345678901234567890abcd) /src/decoder.rs:10:3\n";
        let stack_b = "\
==9==ERROR: AddressSanitizer: heap-buffer-overflow on address 0x60200000ffff at pc 0x0000555555559999\n\
    #0 0x0000555555559999 in parse_length /src/parser.rs:42:18\n\
    #1 0x000055555555aaaa in decode (BuildId: ffeeddccbbaa99887766554433221100aabbccdd) /src/decoder.rs:10:1\n";
        let other = "\
    #0 0x1111 in other_fn /src/other.rs:3:1\n";
        let known = bug(
            "oss-fuzz",
            "OSV-2020-1",
            "historical heap overflow in parser `parse_length`",
        );
        let crashes = [
            CrashInput {
                id: "c2".into(),
                sanitizer: SanitizerKind::Address,
                signal: "heap-buffer-overflow".into(),
                stack: stack_b.into(),
            },
            CrashInput {
                id: "c-other".into(),
                sanitizer: SanitizerKind::Address,
                signal: "heap-buffer-overflow".into(),
                stack: other.into(),
            },
            CrashInput {
                id: "c1".into(),
                sanitizer: SanitizerKind::Address,
                signal: "heap-buffer-overflow".into(),
                stack: stack_a.into(),
            },
        ];
        let mut shuffled = crashes.clone();
        shuffled.rotate_left(1);
        let a = dedup_crashes(&crashes, std::slice::from_ref(&known)).unwrap();
        let b = dedup_crashes(&shuffled, &[known]).unwrap();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
        assert_eq!(a.len(), 2);
        let matched = a.iter().find(|bucket| bucket.crash_ids.len() == 2).unwrap();
        assert_eq!(matched.crash_ids, ["c1", "c2"]);
        assert_eq!(
            matched.normalized_frames,
            [
                "parse_length /src/parser.rs:42",
                "decode /src/decoder.rs:10"
            ]
        );
        assert!(matched.stack_hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(matched.stack_hash.len(), 64);
        assert_eq!(matched.matched_bugs.len(), 1);
        assert_eq!(matched.matched_bugs[0].record_id, "OSV-2020-1");
        assert_eq!(matched.severity_hint, SeverityHint::Critical);
        let unmatched = a
            .iter()
            .find(|bucket| bucket.crash_ids == ["c-other"])
            .unwrap();
        assert!(unmatched.matched_bugs.is_empty());
    }

    #[test]
    fn harness_skeletons_cover_cargo_fuzz_libfuzzer_and_afl() {
        let (ranked, _) = rank_candidates(&[candidate(
            "parse-length",
            "crate::parse_length",
            EntryKind::Parser,
            signals(true, 0, true, false),
            vec![bug("oss-fuzz", "OSV-1", "parser bug in `parse_length`")],
        )])
        .unwrap();
        let harnesses = generate_harnesses(&ranked).unwrap();
        assert_eq!(harnesses.len(), 3);
        assert!(harnesses[0].source.contains("fuzz_target!"));
        assert!(harnesses[0].source.contains("libfuzzer_sys::fuzz_target"));
        assert!(harnesses[1].source.contains("LLVMFuzzerTestOneInput"));
        assert!(harnesses[1].source.contains("crate_parse_length"));
        assert!(harnesses[2].source.contains("afl::fuzz!"));
        assert!(harnesses.iter().all(|h| h.source.contains(REPORT_VERSION)));
    }

    #[test]
    fn sealed_report_round_trips_and_rejects_duplicate_keys() {
        let report = FuzzPlanReport::plan(
            "sample-crate",
            &[candidate(
                "parse-length",
                "parse_length",
                EntryKind::Parser,
                signals(true, 1, true, false),
                vec![bug("oss-fuzz", "OSV-1", "parser bug in `parse_length`")],
            )],
            &[seed("minimal", LicenceClass::AdaptWithAttribution)],
            &[dict(":", LicenceClass::ApproachOnly)],
            &[CrashInput {
                id: "c1".into(),
                sanitizer: SanitizerKind::Address,
                signal: "heap-buffer-overflow".into(),
                stack: "#0 0x10 in parse_length src/parser.rs:42:1\n".into(),
            }],
            &[bug("oss-fuzz", "OSV-1", "parser bug in `parse_length`")],
        )
        .unwrap();
        assert_eq!(report.report_version, REPORT_VERSION);
        assert_eq!(report.seeds[0].pin_sha, PIN);
        assert!(!report.ranked_targets[0].cited_bugs.is_empty());
        let jcs = report.to_jcs().unwrap();
        let again = FuzzPlanReport::verify_json(&jcs).unwrap();
        assert_eq!(again.report_sha256, report.report_sha256);
        let text = String::from_utf8(jcs).unwrap();
        let mutated = text.replacen(
            "\"crate_name\":",
            "\"crate_name\":\"sample-crate\",\"crate_name\":",
            1,
        );
        assert!(FuzzPlanReport::verify_json(mutated.as_bytes()).is_err());
    }
}
