//! Deterministic, evidence-gated source-to-sink code review.

use crate::hub_evidence::{sha256, PinnedCitation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const VERSION: &str = "xerj-cyber-code-review/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodeReviewError {
    #[error("invalid code review input: {0}")]
    Invalid(&'static str),
    #[error("code review JSON: {0}")]
    Json(#[from] serde_json::Error),
}

fn require(ok: bool, reason: &'static str) -> Result<(), CodeReviewError> {
    if ok {
        Ok(())
    } else {
        Err(CodeReviewError::Invalid(reason))
    }
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, CodeReviewError> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

fn valid_cwe(cwe: &str) -> bool {
    let Some(digits) = cwe.strip_prefix("CWE-") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeLanguage {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Sql,
}

impl CodeLanguage {
    pub fn from_path(path: &str) -> Option<Self> {
        match path.rsplit('.').next()? {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "go" => Some(Self::Go),
            "sql" => Some(Self::Sql),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSinkRule {
    pub language: CodeLanguage,
    pub name: String,
    pub sources: Vec<String>,
    pub sinks: Vec<String>,
    pub sanitizers: Vec<String>,
    pub cwe: String,
}

impl SourceSinkRule {
    pub fn builtin(language: CodeLanguage) -> Vec<Self> {
        use CodeLanguage::*;

        match language {
            Rust => vec![
                Self::new(
                    language,
                    "rust-command-injection",
                    &["std::env::args", "std::env::var", "request", "user_input"],
                    &["Command::new", "execute", "write_all"],
                    &["shell_escape", "validate", "allowlist"],
                    "CWE-78",
                ),
                Self::new(
                    language,
                    "rust-sql-injection",
                    &["std::env::args", "request", "user_input"],
                    &["query", "execute", "sqlx::query"],
                    &["bind", "query!", "quote"],
                    "CWE-89",
                ),
                Self::new(
                    language,
                    "rust-path-traversal",
                    &["request", "url", "user_input"],
                    &["File::open", "std::fs::read", "Path::join"],
                    &["canonicalize", "components", "allowlist"],
                    "CWE-22",
                ),
            ],
            Python => vec![
                Self::new(
                    language,
                    "python-command-injection",
                    &["request.args", "input", "sys.argv", "os.environ"],
                    &["os.system", "subprocess", "Popen"],
                    &["shlex.quote", "allowlist", "validate"],
                    "CWE-78",
                ),
                Self::new(
                    language,
                    "python-sql-injection",
                    &["request.args", "input", "sys.argv"],
                    &["execute", "executemany", "raw"],
                    &["parameter", "bind", "escape"],
                    "CWE-89",
                ),
                Self::new(
                    language,
                    "python-code-injection",
                    &["request", "input", "sys.argv"],
                    &["eval", "exec"],
                    &["literal_eval", "allowlist", "validate"],
                    "CWE-95",
                ),
            ],
            TypeScript | JavaScript => vec![
                Self::new(
                    language,
                    "javascript-sql-injection",
                    &["req.query", "req.body", "req.params", "process.env"],
                    &["query", "execute", "raw"],
                    &["parameter", "placeholder", "escapeId"],
                    "CWE-89",
                ),
                Self::new(
                    language,
                    "javascript-command-injection",
                    &["req.query", "req.body", "process.env"],
                    &["exec", "execSync", "spawn"],
                    &["allowlist", "validate", "shellEscape"],
                    "CWE-78",
                ),
                Self::new(
                    language,
                    "javascript-dom-xss",
                    &["location", "req.query", "req.body"],
                    &["innerHTML", "document.write", "dangerouslySetInnerHTML"],
                    &["sanitize", "textContent", "escape"],
                    "CWE-79",
                ),
            ],
            Go => vec![
                Self::new(
                    language,
                    "go-command-injection",
                    &["r.FormValue", "r.URL.Query", "os.Args", "os.Getenv"],
                    &["exec.Command", "exec.CommandContext"],
                    &["allowlist", "validation", "sanitize"],
                    "CWE-78",
                ),
                Self::new(
                    language,
                    "go-sql-injection",
                    &["r.FormValue", "r.URL.Query", "os.Args"],
                    &["db.Query", "db.Exec", "QueryRow"],
                    &["$1", "?", "Prepare"],
                    "CWE-89",
                ),
                Self::new(
                    language,
                    "go-path-traversal",
                    &["r.FormValue", "r.URL.Query"],
                    &["os.Open", "os.ReadFile", "filepath.Join"],
                    &["filepath.Clean", "filepath.Rel", "allowlist"],
                    "CWE-22",
                ),
            ],
            Sql => vec![Self::new(
                language,
                "sql-dynamic-injection",
                &["input", "parameter", "request"],
                &["EXECUTE", "sp_executesql", "PREPARE"],
                &["sp_executesql", "QUOTENAME", "bind"],
                "CWE-89",
            )],
        }
    }

    pub fn built_in(language: CodeLanguage) -> Vec<Self> {
        Self::builtin(language)
    }

    pub fn custom(
        language: CodeLanguage,
        name: impl Into<String>,
        sources: Vec<String>,
        sinks: Vec<String>,
        sanitizers: Vec<String>,
        cwe: impl Into<String>,
    ) -> Result<Self, CodeReviewError> {
        let rule = Self {
            language,
            name: name.into(),
            sources,
            sinks,
            sanitizers,
            cwe: cwe.into(),
        };
        rule.validate()?;
        Ok(rule)
    }

    fn new(
        language: CodeLanguage,
        name: &str,
        sources: &[&str],
        sinks: &[&str],
        sanitizers: &[&str],
        cwe: &str,
    ) -> Self {
        Self {
            language,
            name: name.into(),
            sources: sources.iter().map(|s| (*s).into()).collect(),
            sinks: sinks.iter().map(|s| (*s).into()).collect(),
            sanitizers: sanitizers.iter().map(|s| (*s).into()).collect(),
            cwe: cwe.into(),
        }
    }

    fn validate(&self) -> Result<(), CodeReviewError> {
        require(
            !self.name.trim().is_empty()
                && !self.sources.is_empty()
                && !self.sinks.is_empty()
                && valid_cwe(&self.cwe),
            "invalid source/sink rule",
        )
    }

    fn matches(&self, candidate: &TaintCandidate) -> bool {
        self.language == candidate.language
            && self.sinks.iter().any(|sink| {
                candidate.sink == *sink
                    || (candidate.code.contains(sink) && candidate.code.contains(&candidate.sink))
            })
            && self.sources.iter().any(|source| {
                candidate.code.contains(source)
                    || candidate
                        .source_path
                        .as_deref()
                        .is_some_and(|path| path.contains(source))
            })
    }

    fn has_sanitizer(&self, candidate: &TaintCandidate) -> bool {
        self.sanitizers
            .iter()
            .any(|guard| candidate.code.contains(guard))
            || candidate
                .guard
                .as_deref()
                .is_some_and(|guard| self.sanitizers.iter().any(|known| guard.contains(known)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaintCandidate {
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub language: CodeLanguage,
    pub sink: String,
    pub source_path: Option<String>,
    pub code: String,
    pub guard: Option<String>,
}

impl TaintCandidate {
    pub fn new(
        file: impl Into<String>,
        line_start: u64,
        line_end: u64,
        language: CodeLanguage,
        sink: impl Into<String>,
        source_path: Option<String>,
        code: impl Into<String>,
    ) -> Result<Self, CodeReviewError> {
        let candidate = Self {
            file: file.into(),
            line_start,
            line_end,
            language,
            sink: sink.into(),
            source_path,
            code: code.into(),
            guard: None,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    fn validate(&self) -> Result<(), CodeReviewError> {
        require(
            !self.file.trim().is_empty()
                && self.line_start > 0
                && self.line_start <= self.line_end
                && !self.sink.trim().is_empty()
                && !self.code.trim().is_empty(),
            "candidate location, sink, and code are required",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorVulnKind {
    Vulnerability,
    FixCommit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorVulnCitation {
    pub corpus: String,
    pub record_id: String,
    pub kind: PriorVulnKind,
    pub cwe: String,
    pub summary: String,
    pub pin_sha: String,
    pub fetched_at: String,
    pub pinned_citation: PinnedCitation,
}

impl PriorVulnCitation {
    fn validate(&self) -> Result<(), CodeReviewError> {
        require(
            !self.corpus.trim().is_empty()
                && !self.record_id.trim().is_empty()
                && !self.summary.trim().is_empty()
                && valid_cwe(&self.cwe)
                && self.pin_sha.len() == 40
                && self
                    .pin_sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && !self.fetched_at.trim().is_empty(),
            "invalid prior vulnerability citation",
        )?;
        require(
            self.pin_sha == self.pinned_citation.commit,
            "citation pin mismatch",
        )
    }

    pub fn reference(&self) -> String {
        format!(
            "[{}:{}@{}]",
            self.corpus,
            self.record_id,
            &self.pin_sha[..8]
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionReason {
    NoMatchingRule,
    Sanitized,
    NoCorpusEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeReviewAbstention {
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub sink: String,
    pub source_path: Option<String>,
    pub reason: AbstentionReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeReviewFinding {
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub sink: String,
    pub source_path: Option<String>,
    pub cwe: String,
    pub citations: Vec<PriorVulnCitation>,
    pub similarity: f64,
}

impl CodeReviewFinding {
    fn validate(&self) -> Result<(), CodeReviewError> {
        require(
            !self.file.trim().is_empty()
                && self.line_start > 0
                && self.line_start <= self.line_end
                && !self.sink.trim().is_empty()
                && valid_cwe(&self.cwe)
                && !self.citations.is_empty()
                && self.similarity.is_finite()
                && (0.0..=1.0).contains(&self.similarity),
            "finding evidence or location missing",
        )?;
        for citation in &self.citations {
            citation.validate()?;
            require(citation.cwe == self.cwe, "finding citation CWE mismatch")?;
        }
        Ok(())
    }
}

fn tokens(input: &str) -> BTreeSet<String> {
    input
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|part| part.len() >= 3)
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

fn similarity(
    candidate: &TaintCandidate,
    rule: &SourceSinkRule,
    citation: &PriorVulnCitation,
) -> f64 {
    if citation.cwe != rule.cwe {
        return 0.0;
    }
    let lhs = tokens(&format!(
        "{} {} {} {}",
        candidate.sink,
        candidate.code,
        rule.name,
        candidate.source_path.as_deref().unwrap_or("")
    ));
    let rhs = tokens(&citation.summary);
    let common = lhs.intersection(&rhs).count() as f64;
    if common == 0.0 {
        return 0.0;
    }
    let union = (lhs.len().max(1) + rhs.len().max(1) - common as usize) as f64;
    // Exact CWE plus the matched rule is the structural signal; token Dice is the lexical tie breaker.
    (0.75 + 0.25 * (2.0 * common / union)).min(1.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeReviewReport {
    pub report_version: String,
    pub repository: String,
    pub findings: Vec<CodeReviewFinding>,
    pub abstentions: Vec<CodeReviewAbstention>,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct ReportPayload<'a> {
    report_version: &'a str,
    repository: &'a str,
    findings: &'a [CodeReviewFinding],
    abstentions: &'a [CodeReviewAbstention],
}

impl CodeReviewReport {
    fn digest(&self) -> Result<String, CodeReviewError> {
        Ok(sha256(&canonical(&ReportPayload {
            report_version: &self.report_version,
            repository: &self.repository,
            findings: &self.findings,
            abstentions: &self.abstentions,
        })?))
    }

    pub fn evaluate(
        repository: impl Into<String>,
        candidates: &[TaintCandidate],
        citations: &[PriorVulnCitation],
        custom_rules: &[SourceSinkRule],
    ) -> Result<Self, CodeReviewError> {
        let repository = repository.into();
        require(!repository.trim().is_empty(), "repository required")?;
        for candidate in candidates {
            candidate.validate()?;
        }
        for citation in citations {
            citation.validate()?;
        }
        for rule in custom_rules {
            rule.validate()?;
        }
        let mut rules: Vec<_> = [
            CodeLanguage::Rust,
            CodeLanguage::Python,
            CodeLanguage::TypeScript,
            CodeLanguage::JavaScript,
            CodeLanguage::Go,
            CodeLanguage::Sql,
        ]
        .into_iter()
        .flat_map(SourceSinkRule::builtin)
        .collect();
        rules.extend_from_slice(custom_rules);
        let mut findings = Vec::new();
        let mut abstentions = Vec::new();
        for candidate in candidates {
            let matching_rules: Vec<_> = rules
                .iter()
                .filter(|rule| rule.matches(candidate))
                .collect();
            if matching_rules.is_empty() {
                abstentions.push(abstention(candidate, AbstentionReason::NoMatchingRule));
                continue;
            }
            let active_rules: Vec<_> = matching_rules
                .iter()
                .filter(|rule| !rule.has_sanitizer(candidate))
                .copied()
                .collect();
            if active_rules.is_empty() {
                abstentions.push(abstention(candidate, AbstentionReason::Sanitized));
                continue;
            }
            let mut matches: Vec<_> = active_rules
                .iter()
                .flat_map(|rule| {
                    citations.iter().filter_map(move |citation| {
                        let score = similarity(candidate, rule, citation);
                        (score > 0.0).then_some((score, citation, rule.cwe.as_str()))
                    })
                })
                .collect();
            matches.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then_with(|| a.2.cmp(b.2))
                    .then_with(|| a.1.corpus.cmp(&b.1.corpus))
                    .then_with(|| a.1.record_id.cmp(&b.1.record_id))
                    .then_with(|| a.1.pin_sha.cmp(&b.1.pin_sha))
            });
            if matches.is_empty() {
                abstentions.push(abstention(candidate, AbstentionReason::NoCorpusEvidence));
                continue;
            }
            let selected_cwe = matches[0].2;
            let mut selected_citations = Vec::new();
            for (_, citation, cwe) in &matches {
                if *cwe != selected_cwe {
                    continue;
                }
                if !selected_citations.iter().any(|known: &&PriorVulnCitation| {
                    known.corpus == citation.corpus
                        && known.record_id == citation.record_id
                        && known.pin_sha == citation.pin_sha
                }) {
                    selected_citations.push(*citation);
                }
            }
            findings.push(CodeReviewFinding {
                file: candidate.file.clone(),
                line_start: candidate.line_start,
                line_end: candidate.line_end,
                sink: candidate.sink.clone(),
                source_path: candidate.source_path.clone(),
                cwe: selected_cwe.into(),
                citations: selected_citations.into_iter().cloned().collect(),
                similarity: matches[0].0,
            });
        }

        findings.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line_start.cmp(&b.line_start))
                .then_with(|| a.sink.cmp(&b.sink))
        });
        let mut report = Self {
            report_version: VERSION.into(),
            repository,
            findings,
            abstentions,
            report_sha256: String::new(),
        };
        report.validate()?;
        report.report_sha256 = report.digest()?;
        report.to_jcs()?;
        Ok(report)
    }

    fn validate(&self) -> Result<(), CodeReviewError> {
        require(
            self.report_version == VERSION && !self.repository.trim().is_empty(),
            "report metadata",
        )?;
        for finding in &self.findings {
            finding.validate()?;
        }
        for abstention in &self.abstentions {
            require(
                abstention.line_start > 0
                    && abstention.line_start <= abstention.line_end
                    && !abstention.file.is_empty()
                    && !abstention.sink.is_empty(),
                "invalid abstention location",
            )?;
        }
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>, CodeReviewError> {
        self.validate()?;
        require(
            self.report_sha256 == self.digest()?,
            "report digest mismatch",
        )?;
        let bytes = canonical(self)?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    pub fn verify_json(json: &[u8]) -> Result<Self, CodeReviewError> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let report: Self = serde_json::from_slice(json)?;
        report.to_jcs()?;
        Ok(report)
    }
}

fn abstention(candidate: &TaintCandidate, reason: AbstentionReason) -> CodeReviewAbstention {
    CodeReviewAbstention {
        file: candidate.file.clone(),
        line_start: candidate.line_start,
        line_end: candidate.line_end,
        sink: candidate.sink.clone(),
        source_path: candidate.source_path.clone(),
        reason,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeededBenchmarkResult {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub true_positives: u64,
    pub false_positives: u64,
    pub false_negatives: u64,
}

pub fn evaluate_seeded_benchmark(
    candidates: &[TaintCandidate],
    expected_vulnerable: &[usize],
    citations: &[PriorVulnCitation],
    custom_rules: &[SourceSinkRule],
) -> Result<SeededBenchmarkResult, CodeReviewError> {
    let report =
        CodeReviewReport::evaluate("seeded-benchmark", candidates, citations, custom_rules)?;
    let expected: BTreeSet<String> = expected_vulnerable
        .iter()
        .filter_map(|i| candidates.get(*i))
        .map(|c| format!("{}:{}:{}", c.file, c.line_start, c.sink))
        .collect();
    let found: BTreeSet<String> = report
        .findings
        .iter()
        .map(|f| format!("{}:{}:{}", f.file, f.line_start, f.sink))
        .collect();
    let tp = expected.intersection(&found).count() as u64;
    let fp = found.difference(&expected).count() as u64;
    let fn_ = expected.difference(&found).count() as u64;
    let precision = if tp + fp == 0 {
        0.0
    } else {
        tp as f64 / (tp + fp) as f64
    };
    let recall = if tp + fn_ == 0 {
        0.0
    } else {
        tp as f64 / (tp + fn_) as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Ok(SeededBenchmarkResult {
        precision,
        recall,
        f1,
        true_positives: tp,
        false_positives: fp,
        false_negatives: fn_,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation(corpus: &str, id: &str, cwe: &str, summary: &str) -> PriorVulnCitation {
        let bytes = format!("{summary}\n").into_bytes();
        let pinned = PinnedCitation::from_bytes(
            corpus.into(),
            "fixtures/security-corpus".into(),
            "0123456789abcdef0123456789abcdef01234567".into(),
            "records.txt".into(),
            &bytes,
            0..summary.len(),
        )
        .unwrap();
        PriorVulnCitation {
            corpus: corpus.into(),
            record_id: id.into(),
            kind: PriorVulnKind::Vulnerability,
            cwe: cwe.into(),
            summary: summary.into(),
            pin_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            fetched_at: "2026-11-10T00:00:00Z".into(),
            pinned_citation: pinned,
        }
    }

    fn fixtures() -> Vec<TaintCandidate> {
        vec![
            TaintCandidate {
                file: "seed/python.rs".into(),
                line_start: 2,
                line_end: 2,
                language: CodeLanguage::Python,
                sink: "os.system".into(),
                source_path: Some("request.args".into()),
                code: "os.system(request.args['cmd'])".into(),
                guard: None,
            },
            TaintCandidate {
                file: "seed/node.ts".into(),
                line_start: 4,
                line_end: 5,
                language: CodeLanguage::TypeScript,
                sink: "query".into(),
                source_path: Some("req.body".into()),
                code: "db.query(req.body.sql)".into(),
                guard: None,
            },
            TaintCandidate {
                file: "seed/safe.py".into(),
                line_start: 7,
                line_end: 7,
                language: CodeLanguage::Python,
                sink: "os.system".into(),
                source_path: Some("request.args".into()),
                code: "os.system(shlex.quote(request.args['cmd']))".into(),
                guard: Some("shlex.quote".into()),
            },
            TaintCandidate {
                file: "seed/abstain.rs".into(),
                line_start: 9,
                line_end: 9,
                language: CodeLanguage::Rust,
                sink: "File::open".into(),
                source_path: Some("request".into()),
                code: "File::open(request.path)".into(),
                guard: None,
            },
        ]
    }

    #[test]
    fn seeded_benchmark_has_perfect_precision_recall_and_safe_abstentions() {
        let citations = vec![
            citation(
                "rust-vulns",
                "CMD-1",
                "CWE-78",
                "request argument reaches os.system command sink",
            ),
            citation(
                "cve",
                "SQL-1",
                "CWE-89",
                "untrusted request body reaches database query sink",
            ),
        ];
        let result = evaluate_seeded_benchmark(&fixtures(), &[0, 1], &citations, &[]).unwrap();
        assert_eq!(result.precision, 1.0);
        assert_eq!(result.recall, 1.0);
        assert_eq!(result.f1, 1.0);
        assert_eq!(result.false_positives, 0);
    }

    #[test]
    fn no_evidence_becomes_explicit_abstention() {
        let candidates = fixtures();
        let report = CodeReviewReport::evaluate("fixture", &candidates[..1], &[], &[]).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(
            report.abstentions[0].reason,
            AbstentionReason::NoCorpusEvidence
        );
    }

    #[test]
    fn builtins_and_custom_recipes_are_deterministic() {
        assert!(!SourceSinkRule::builtin(CodeLanguage::Rust).is_empty());
        let custom = SourceSinkRule::custom(
            CodeLanguage::Rust,
            "recipe",
            vec!["stdin".into()],
            vec!["danger".into()],
            vec!["guard".into()],
            "CWE-20",
        )
        .unwrap();
        let candidate = TaintCandidate::new(
            "custom.rs",
            1,
            1,
            CodeLanguage::Rust,
            "danger",
            Some("stdin".into()),
            "danger(stdin)",
        )
        .unwrap();
        let report = CodeReviewReport::evaluate(
            "fixture",
            &[candidate],
            &[citation("recipe", "one", "CWE-20", "stdin danger sink")],
            &[custom],
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn report_is_jcs_sealed_and_tamper_detected() {
        let report = CodeReviewReport::evaluate(
            "fixture",
            &fixtures()[..1],
            &[citation(
                "cve",
                "one",
                "CWE-78",
                "request reaches os.system",
            )],
            &[],
        )
        .unwrap();
        let json = report.to_jcs().unwrap();
        assert_eq!(
            CodeReviewReport::verify_json(&json)
                .unwrap()
                .to_jcs()
                .unwrap(),
            report.to_jcs().unwrap()
        );
        let mut value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        value["repository"] = "tampered".into();
        assert!(CodeReviewReport::verify_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
