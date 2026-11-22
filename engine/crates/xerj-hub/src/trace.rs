//! `xerj trace <session.jsonl>` (#1789): list every hub answer an agent
//! session was shown, with the evidence behind it.
//!
//! A session file is JSON Lines, one event per line, as agent harnesses write
//! them. Nothing here depends on one harness's schema: each line is walked
//! for three shapes, at any depth and inside JSON-encoded strings (a tool
//! result's text is often the answer's JSON):
//!
//! - a tool call: an object with `"type": "tool_use"`, an `id`, a `name` and
//!   an `input` (its `question`, `symbol` or `command` is the question);
//! - a hub [`Answer`]: an object with `corpus`, `pin_sha` and `hits` — what
//!   `xerj_hub_ask` / `xerj_hub_def` put in `structuredContent` and what
//!   `xerj ask --json` prints;
//! - an evidence receipt: a [`TraceEvidenceReport`], either inside the answer
//!   as `evidence_receipt` (the MCP opt-in, #2932) or on its own.
//!
//! An answer or receipt belongs to the tool call named by the nearest
//! enclosing `tool_use_id`, or, outside any, by the line's only tool result
//! (a harness that stores the structured result beside the message). Each receipt's digest is recomputed (RFC 8785
//! JCS, [`TraceEvidenceReport::to_jcs`]). With `--repo <dir>`, a git checkout
//! of the corpus, every hit and every pinned citation is also re-read at its
//! pinned commit (`git show <commit>:<file>`) and checked byte for byte.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use xerj_common::hub::{Answer, Hit};
use xerj_common::hub_evidence::{PinnedCitation, PinnedSource, TraceEvidenceReport};

pub const TRACE_USAGE: &str = "usage: xerj trace <session.jsonl> [options]

Lists each hub answer in an agent session (xerj_hub_ask / xerj_hub_def tool
results, and `xerj ask --json` output) with its corpus, pinned revision and
file:line evidence, and checks every evidence receipt's digest.

options:
  --repo <dir>   a git checkout of the corpus: re-read each hit and pinned
                 citation at its pinned commit and check the bytes
  --json         print the report as JSON

exit codes: 0 every receipt (and, with --repo, every checked hit and citation)
verified; 1 the session has no hub answers; 2 usage error or unreadable file;
3 a receipt, hit or citation failed verification.";

/// Exit code: something failed verification.
pub const EXIT_VERIFY_FAILED: i32 = 3;

/// Longest JSON-encoded string that is parsed for nested answers.
const MAX_NESTED_JSON: usize = 4 << 20;
/// Deepest nesting walked; deeper values are skipped.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceArgs {
    pub path: PathBuf,
    pub repo: Option<PathBuf>,
    pub json: bool,
}

/// `Ok(None)` is `--help`.
pub fn parse_trace_args(args: &[String]) -> Result<Option<TraceArgs>, String> {
    let mut path: Option<PathBuf> = None;
    let mut repo: Option<PathBuf> = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(None),
            "--json" => json = true,
            "--repo" => {
                i += 1;
                repo = Some(PathBuf::from(
                    args.get(i).ok_or("--repo needs a value")?.as_str(),
                ));
            }
            f if f.starts_with("--") => return Err(format!("unknown flag '{f}'")),
            p if path.is_none() => path = Some(PathBuf::from(p)),
            extra => return Err(format!("unexpected argument '{extra}'")),
        }
        i += 1;
    }
    match path {
        Some(path) => Ok(Some(TraceArgs { path, repo, json })),
        None => Err("a session file is required".to_string()),
    }
}

/// One hit as evidence, and what re-reading it at the pin found.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HitEvidence {
    pub file: String,
    pub line: u32,
    pub kind: String,
    /// `verified`, `not checked`, or why the bytes did not match.
    pub bytes: String,
}

/// One pinned citation in a receipt.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CitationEvidence {
    pub repository: String,
    pub commit: String,
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub span_sha256: String,
    pub bytes: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReceiptCheck {
    pub evidence_receipt_sha256: String,
    pub session_id: String,
    /// The digest recomputed over the RFC 8785 form matches, and the receipt
    /// is well formed.
    pub digest_ok: bool,
    pub error: Option<String>,
    pub abstain: bool,
    pub gaps: Vec<String>,
    pub citations: Vec<CitationEvidence>,
}

/// One hub answer the session was shown.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TracedAnswer {
    /// 1-based line of the session file the answer was read from.
    pub line: usize,
    pub tool_call_id: Option<String>,
    pub tool: Option<String>,
    pub question: Option<String>,
    pub corpus: Option<String>,
    pub pin_sha: Option<String>,
    pub pin_date: Option<String>,
    pub licence_spdx: Option<String>,
    pub answered_from: Option<String>,
    pub evidence: Vec<HitEvidence>,
    pub receipt: Option<ReceiptCheck>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionTrace {
    pub session: String,
    pub lines: usize,
    pub unreadable_lines: usize,
    pub tool_calls: usize,
    pub answers: Vec<TracedAnswer>,
}

impl SessionTrace {
    pub fn receipts(&self) -> usize {
        self.answers.iter().filter(|a| a.receipt.is_some()).count()
    }

    /// Receipts, hits and citations that were checked and did not verify.
    pub fn failures(&self) -> usize {
        let mut n = 0;
        for a in &self.answers {
            n += a.evidence.iter().filter(|h| failed(&h.bytes)).count();
            if let Some(r) = &a.receipt {
                n += usize::from(!r.digest_ok);
                n += r.citations.iter().filter(|c| failed(&c.bytes)).count();
            }
        }
        n
    }
}

const VERIFIED: &str = "verified";
const NOT_CHECKED: &str = "not checked";

fn failed(bytes: &str) -> bool {
    bytes != VERIFIED && bytes != NOT_CHECKED
}

/// What one walk of the file collected before answers are checked.
#[derive(Default)]
struct Found {
    /// tool_use id → (tool name, question).
    calls: HashMap<String, (String, Option<String>)>,
    call_count: usize,
    /// (line, enclosing tool_use_id, answer object without its receipt, receipt).
    answers: Vec<(usize, Option<String>, Value, Option<Value>)>,
    /// (line, enclosing tool_use_id, receipt) not inside an answer.
    receipts: Vec<(usize, Option<String>, Value)>,
    session: Option<String>,
}

fn is_answer(o: &Map<String, Value>) -> bool {
    o.get("corpus").is_some_and(Value::is_string)
        && o.get("pin_sha").is_some_and(Value::is_string)
        && o.get("hits").is_some_and(Value::is_array)
}

fn is_receipt(o: &Map<String, Value>) -> bool {
    o.get("report_version").is_some_and(Value::is_string)
        && o.get("entries").is_some_and(Value::is_array)
        && o.contains_key("evidence_receipt_sha256")
}

/// The `tool_use_id` of every tool result in `v` (JSON strings not parsed).
fn tool_result_ids<'a>(v: &'a Value, out: &mut Vec<&'a str>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("tool_result") {
                if let Some(id) = o.get("tool_use_id").and_then(Value::as_str) {
                    out.push(id);
                }
            }
            for child in o.values() {
                tool_result_ids(child, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                tool_result_ids(child, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn question_of(input: &Value) -> Option<String> {
    ["question", "symbol", "command"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

impl Found {
    fn walk(&mut self, v: &Value, line: usize, ctx: Option<&str>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match v {
            Value::Object(o) => {
                if self.session.is_none() {
                    if let Some(s) = ["sessionId", "session_id"]
                        .iter()
                        .find_map(|k| o.get(*k).and_then(Value::as_str))
                    {
                        self.session = Some(s.to_string());
                    }
                }
                if o.get("type").and_then(Value::as_str) == Some("tool_use") {
                    if let (Some(id), Some(name)) = (
                        o.get("id").and_then(Value::as_str),
                        o.get("name").and_then(Value::as_str),
                    ) {
                        let question = o.get("input").and_then(question_of);
                        if self
                            .calls
                            .insert(id.to_string(), (name.to_string(), question))
                            .is_none()
                        {
                            self.call_count += 1;
                        }
                    }
                }
                let ctx = o.get("tool_use_id").and_then(Value::as_str).or(ctx);
                if is_answer(o) {
                    let mut answer = o.clone();
                    let receipt = answer.remove("evidence_receipt");
                    self.answers.push((
                        line,
                        ctx.map(str::to_string),
                        Value::Object(answer),
                        receipt,
                    ));
                    return;
                }
                if is_receipt(o) {
                    self.receipts
                        .push((line, ctx.map(str::to_string), v.clone()));
                    return;
                }
                for child in o.values() {
                    self.walk(child, line, ctx, depth + 1);
                }
            }
            Value::Array(items) => {
                for child in items {
                    self.walk(child, line, ctx, depth + 1);
                }
            }
            Value::String(s) => {
                let t = s.trim_start();
                if (t.starts_with('{') || t.starts_with('[')) && s.len() <= MAX_NESTED_JSON {
                    if let Ok(nested) = serde_json::from_str::<Value>(s) {
                        self.walk(&nested, line, ctx, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Reads a file as it was at a commit of a git checkout.
pub trait PinnedReader {
    fn read(&self, commit: &str, file: &str) -> Result<Vec<u8>, String>;
}

/// `git -C <repo> show <commit>:<file>`.
pub struct GitReader(pub PathBuf);

impl PinnedReader for GitReader {
    fn read(&self, commit: &str, file: &str) -> Result<Vec<u8>, String> {
        if !commit.bytes().all(|b| b.is_ascii_hexdigit()) || commit.is_empty() {
            return Err(format!("pin '{commit}' is not a commit id"));
        }
        if file.starts_with('-') || file.contains('\0') {
            return Err(format!("refused path '{file}'"));
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .arg("show")
            .arg(format!("{commit}:{file}"))
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!(
                "not in the checkout at that commit ({})",
                err.lines().next().unwrap_or("git show failed").trim()
            ))
        }
    }
}

fn check_hit(hit: &Hit, answer: &Answer, reader: Option<&dyn PinnedReader>) -> String {
    let Some(reader) = reader else {
        return NOT_CHECKED.to_string();
    };
    match reader.read(&answer.pin_sha, &hit.file) {
        Ok(bytes) => {
            let source = PinnedSource {
                repository: answer.corpus.clone(),
                bytes,
            };
            match hit.pinned_citation(&answer.corpus, &answer.pin_sha, &source) {
                Ok(_) => VERIFIED.to_string(),
                Err(e) => format!("mismatch: {e}"),
            }
        }
        Err(e) => format!("unresolved: {e}"),
    }
}

fn check_receipt(receipt: &Value, reader: Option<&dyn PinnedReader>) -> ReceiptCheck {
    let str_at = |key: &str| {
        receipt
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut check = ReceiptCheck {
        evidence_receipt_sha256: str_at("evidence_receipt_sha256"),
        session_id: str_at("session_id"),
        digest_ok: false,
        error: None,
        abstain: true,
        gaps: Vec::new(),
        citations: Vec::new(),
    };
    let report: TraceEvidenceReport = match serde_json::from_value(receipt.clone()) {
        Ok(r) => r,
        Err(e) => {
            check.error = Some(format!("not a valid receipt: {e}"));
            return check;
        }
    };
    match report.to_jcs() {
        Ok(_) => check.digest_ok = true,
        Err(e) => check.error = Some(e.to_string()),
    }
    check.abstain = report.entries.iter().any(|e| e.abstain);
    for entry in &report.entries {
        for gap in entry.abstain_gaps() {
            if !check.gaps.contains(&gap) {
                check.gaps.push(gap);
            }
        }
        for c in &entry.citations {
            check.citations.push(CitationEvidence {
                repository: c.repository.clone(),
                commit: c.commit.clone(),
                file: c.file.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                byte_start: c.byte_start,
                byte_end: c.byte_end,
                span_sha256: c.span_sha256.clone(),
                bytes: check_citation(c, reader),
            });
        }
    }
    check
}

fn check_citation(c: &PinnedCitation, reader: Option<&dyn PinnedReader>) -> String {
    let Some(reader) = reader else {
        return NOT_CHECKED.to_string();
    };
    match reader.read(&c.commit, &c.file) {
        Ok(bytes) => match c.verify_bytes(&bytes) {
            Ok(()) => VERIFIED.to_string(),
            Err(e) => format!("mismatch: {e}"),
        },
        Err(e) => format!("unresolved: {e}"),
    }
}

/// Trace a session's JSON Lines text. `name` labels the session when the
/// file carries no session id.
pub fn trace_text(text: &str, name: &str, reader: Option<&dyn PinnedReader>) -> SessionTrace {
    let mut found = Found::default();
    let mut lines = 0;
    let mut unreadable = 0;
    for (i, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        lines += 1;
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => {
                let mut ids = Vec::new();
                tool_result_ids(&v, &mut ids, 0);
                ids.dedup();
                let line_ctx = match ids.as_slice() {
                    [only] => Some(only.to_string()),
                    _ => None,
                };
                found.walk(&v, i + 1, line_ctx.as_deref(), 0)
            }
            Err(_) => unreadable += 1,
        }
    }

    // One answer can be seen twice in one tool result (its text and its
    // structured content): keep the first, and take a receipt from either.
    let mut answers: Vec<TracedAnswer> = Vec::new();
    let mut seen: HashMap<(Option<String>, String), usize> = HashMap::new();
    let mut take_receipt: Vec<(usize, Value)> = Vec::new();
    for (line, ctx, answer_value, receipt) in &found.answers {
        let key = (ctx.clone(), answer_value.to_string());
        if let Some(&at) = seen.get(&key) {
            if let Some(r) = receipt {
                take_receipt.push((at, r.clone()));
            }
            continue;
        }
        let (tool, question) = match ctx.as_ref().and_then(|id| found.calls.get(id)) {
            Some((name, q)) => (Some(name.clone()), q.clone()),
            None => (None, None),
        };
        let parsed: Option<Answer> = serde_json::from_value(answer_value.clone()).ok();
        let str_at = |k: &str| {
            answer_value
                .get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let evidence = match &parsed {
            Some(a) => a
                .hits
                .iter()
                .map(|h| HitEvidence {
                    file: h.file.clone(),
                    line: h.line,
                    kind: h.kind.clone(),
                    bytes: check_hit(h, a, reader),
                })
                .collect(),
            None => answer_value
                .get("hits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|h| HitEvidence {
                    file: h["file"].as_str().unwrap_or_default().to_string(),
                    line: h["line"].as_u64().unwrap_or(0) as u32,
                    kind: h["kind"].as_str().unwrap_or_default().to_string(),
                    bytes: if reader.is_some() {
                        "unresolved: the answer does not parse as a hub answer".to_string()
                    } else {
                        NOT_CHECKED.to_string()
                    },
                })
                .collect(),
        };
        seen.insert(key, answers.len());
        if let Some(r) = receipt {
            take_receipt.push((answers.len(), r.clone()));
        }
        answers.push(TracedAnswer {
            line: *line,
            tool_call_id: ctx.clone(),
            tool,
            question,
            corpus: str_at("corpus"),
            pin_sha: str_at("pin_sha"),
            pin_date: str_at("pin_date"),
            licence_spdx: str_at("licence_spdx"),
            answered_from: parsed.as_ref().map(|a| a.answered_from.label().to_string()),
            evidence,
            receipt: None,
        });
    }
    // A receipt outside its answer joins the answer of the same tool call;
    // with none, it is listed on its own.
    for (line, ctx, receipt) in &found.receipts {
        let at = ctx.as_ref().and_then(|id| {
            answers
                .iter()
                .position(|a| a.tool_call_id.as_deref() == Some(id))
        });
        match at {
            Some(at) => take_receipt.push((at, receipt.clone())),
            None => {
                let (tool, question) = match ctx.as_ref().and_then(|id| found.calls.get(id)) {
                    Some((name, q)) => (Some(name.clone()), q.clone()),
                    None => (None, None),
                };
                take_receipt.push((answers.len(), receipt.clone()));
                answers.push(TracedAnswer {
                    line: *line,
                    tool_call_id: ctx.clone(),
                    tool,
                    question,
                    corpus: None,
                    pin_sha: None,
                    pin_date: None,
                    licence_spdx: None,
                    answered_from: None,
                    evidence: Vec::new(),
                    receipt: None,
                });
            }
        }
    }
    for (at, receipt) in take_receipt {
        if answers[at].receipt.is_none() {
            let check = check_receipt(&receipt, reader);
            if answers[at].question.is_none() {
                answers[at].question = serde_json::from_value::<TraceEvidenceReport>(receipt)
                    .ok()
                    .and_then(|r| r.entries.first().map(|e| e.question.clone()));
            }
            answers[at].receipt = Some(check);
        }
    }
    answers.sort_by_key(|a| a.line);

    let session = found
        .session
        .clone()
        .or_else(|| {
            answers
                .iter()
                .find_map(|a| a.receipt.as_ref().map(|r| r.session_id.clone()))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| name.to_string());
    SessionTrace {
        session,
        lines,
        unreadable_lines: unreadable,
        tool_calls: found.call_count,
        answers,
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// The human report.
pub fn render_trace(t: &SessionTrace, checked_bytes: bool) -> String {
    let mut out = String::new();
    let push = |out: &mut String, s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    push(
        &mut out,
        format!(
            "session {}: {} lines{}, {} tool calls, {} hub answers, {} with an evidence receipt",
            t.session,
            t.lines,
            if t.unreadable_lines > 0 {
                format!(" ({} not JSON, skipped)", t.unreadable_lines)
            } else {
                String::new()
            },
            t.tool_calls,
            t.answers.len(),
            t.receipts()
        ),
    );
    for (i, a) in t.answers.iter().enumerate() {
        push(
            &mut out,
            format!(
                "\n[{}] {}  call {}  (line {})",
                i + 1,
                a.tool.as_deref().unwrap_or("(no tool call)"),
                a.tool_call_id.as_deref().unwrap_or("-"),
                a.line
            ),
        );
        if let Some(q) = &a.question {
            push(&mut out, format!("    question: {q}"));
        }
        if let Some(corpus) = &a.corpus {
            let mut line = format!(
                "    corpus:   {corpus} @ {}",
                short(a.pin_sha.as_deref().unwrap_or("?"))
            );
            if let Some(d) = &a.pin_date {
                line.push_str(&format!(" ({d})"));
            }
            if let Some(l) = &a.licence_spdx {
                line.push_str(&format!(", {l}"));
            }
            if let Some(f) = &a.answered_from {
                line.push_str(&format!(", answered from {f}"));
            }
            push(&mut out, line);
        }
        if a.corpus.is_some() && a.evidence.is_empty() {
            push(&mut out, "    evidence: none (no hits)".to_string());
        }
        for (j, h) in a.evidence.iter().enumerate() {
            let label = if j == 0 { "evidence:" } else { "         " };
            let kind = if h.kind.is_empty() {
                String::new()
            } else {
                format!("  [{}]", h.kind)
            };
            let bytes = if checked_bytes {
                format!("  {}", h.bytes)
            } else {
                String::new()
            };
            push(
                &mut out,
                format!("    {label} {}:{}{kind}{bytes}", h.file, h.line),
            );
        }
        match &a.receipt {
            None => push(&mut out, "    receipt:  none".to_string()),
            Some(r) => {
                let digest = if r.digest_ok {
                    "digest ok".to_string()
                } else {
                    format!(
                        "FAILED ({})",
                        r.error.as_deref().unwrap_or("digest mismatch")
                    )
                };
                push(
                    &mut out,
                    format!(
                        "    receipt:  sha256 {} {digest}",
                        short(&r.evidence_receipt_sha256)
                    ),
                );
                if r.digest_ok {
                    let decision = if r.abstain {
                        format!("ABSTAIN ({})", r.gaps.join("; "))
                    } else {
                        "VERIFIED".to_string()
                    };
                    push(&mut out, format!("              {decision}"));
                }
                for c in &r.citations {
                    push(
                        &mut out,
                        format!(
                            "              cites {}:{}-{} @ {} bytes {}..{}  {}",
                            c.file,
                            c.line_start,
                            c.line_end,
                            short(&c.commit),
                            c.byte_start,
                            c.byte_end,
                            c.bytes
                        ),
                    );
                }
            }
        }
    }
    let failures = t.failures();
    push(
        &mut out,
        format!(
            "\n{} answers, {} receipts, {}{}",
            t.answers.len(),
            t.receipts(),
            if failures == 0 {
                "nothing failed verification".to_string()
            } else {
                format!("{failures} FAILED verification")
            },
            if checked_bytes {
                ""
            } else {
                " (bytes not checked: pass --repo <checkout>)"
            }
        ),
    );
    out
}

/// `xerj trace …`.
pub fn run_trace(args: &[String]) -> i32 {
    let parsed = match parse_trace_args(args) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{TRACE_USAGE}");
            return 0;
        }
        Err(msg) => {
            eprintln!("xerj trace: {msg}\n\n{TRACE_USAGE}");
            return 2;
        }
    };
    let text = match std::fs::read(&parsed.path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            eprintln!("xerj trace: cannot read {}: {e}", parsed.path.display());
            return 2;
        }
    };
    if let Some(repo) = &parsed.repo {
        if !Path::new(repo).is_dir() {
            eprintln!("xerj trace: --repo {} is not a directory", repo.display());
            return 2;
        }
    }
    let reader = parsed.repo.clone().map(GitReader);
    let name = parsed
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".to_string());
    let trace = trace_text(
        &text,
        &name,
        reader.as_ref().map(|r| r as &dyn PinnedReader),
    );
    if parsed.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&trace).unwrap_or_default()
        );
    } else {
        print!("{}", render_trace(&trace, reader.is_some()));
    }
    if trace.answers.is_empty() {
        if !parsed.json {
            eprintln!(
                "xerj trace: no hub answers in {} (looked for xerj_hub_ask / xerj_hub_def \
                 results and `xerj ask --json` output)",
                parsed.path.display()
            );
        }
        return 1;
    }
    if trace.failures() > 0 {
        return EXIT_VERIFY_FAILED;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use xerj_common::hub::{AnswerMode, AnsweredFrom, LicenceClass};
    use xerj_common::hub_evidence::AnswerEvidence;

    /// A synthetic session, generated by [`synthetic_session`]. No real
    /// transcript went into it: the corpus, file and questions are made up.
    /// Regenerate with `XERJ_REGEN_TRACE_FIXTURE=1`.
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trace/synthetic-session.jsonl"
    );
    const SESSION: &str = "synthetic-session-1789";
    const PIN: &str = "4f1c2a9e8b7d6c5f4e3d2c1b0a9f8e7d6c5b4a39";
    const FILE: &str = "src/frame.rs";
    const FRAME_RS: &str = "//! Frame format for the demo codec.\n\
                            pub const MAGIC: u32 = 0x184D2204;\n\
                            \n\
                            /// Checksum of one block: xxh32 with seed 0.\n\
                            pub fn checksum(data: &[u8]) -> u32 {\n\
                            \x20   xxh32(data, 0)\n\
                            }\n";

    /// The corpus at [`PIN`], held in memory.
    struct MemReader;
    impl PinnedReader for MemReader {
        fn read(&self, commit: &str, file: &str) -> Result<Vec<u8>, String> {
            if commit == PIN && file == FILE {
                Ok(FRAME_RS.as_bytes().to_vec())
            } else {
                Err(format!("no {file} at {commit}"))
            }
        }
    }

    fn hit(line: u32, kind: &str, text: &str) -> Hit {
        Hit {
            file: FILE.into(),
            line,
            kind: kind.into(),
            text: text.into(),
            value: None,
            score: 1.0,
            licence_warning: None,
        }
    }

    fn answer(mode: AnswerMode, hits: Vec<Hit>) -> Answer {
        Answer {
            corpus: "demo-codec".into(),
            pin_sha: PIN.into(),
            pin_date: "2026-11-01".into(),
            age_days: 21,
            licence_spdx: "MIT".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: AnsweredFrom::LocalPack,
            mode,
            hits,
            truncated: false,
            warnings: Vec::new(),
        }
    }

    /// The receipt the MCP opt-in builds (#2932). `with_bytes` resolves the
    /// pinned file, so the receipt carries byte-span citations; without it,
    /// as packs are served today, it carries none.
    fn receipt(a: &Answer, call: &str, question: &str, with_bytes: bool) -> Value {
        let context = AnswerEvidence {
            session_id: SESSION.into(),
            tool_call_id: call.into(),
            question: question.into(),
            shown_answer: xerj_common::hub::render(a),
            confidence: None,
            p_min: 0.8,
            coverage_gaps: vec!["question-specific coverage not assessed".into()],
        };
        let report = a
            .evidence_report(context, |_| {
                Ok(with_bytes.then(|| PinnedSource {
                    repository: "demo-codec".into(),
                    bytes: FRAME_RS.as_bytes().to_vec(),
                }))
            })
            .unwrap();
        serde_json::to_value(report).unwrap()
    }

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({
            "type": "assistant",
            "sessionId": SESSION,
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": name, "input": input}
            ]}
        })
    }

    /// An MCP tool result: the rendered text in the message, the structured
    /// content (answer plus optional receipt) beside it.
    fn mcp_result(id: &str, a: &Answer, receipt: Option<Value>) -> Value {
        let mut structured = serde_json::to_value(a).unwrap();
        if let Some(r) = receipt {
            structured["evidence_receipt"] = r;
        }
        let text = xerj_common::hub::render(a);
        json!({
            "type": "user",
            "sessionId": SESSION,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id,
                 "content": [{"type": "text", "text": text}]}
            ]},
            "toolUseResult": {
                "content": [{"type": "text", "text": text}],
                "structuredContent": structured,
                "isError": false
            }
        })
    }

    fn text_result(id: &str, text: &str) -> Value {
        json!({
            "type": "user",
            "sessionId": SESSION,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": text}
            ]}
        })
    }

    fn synthetic_session() -> String {
        let mut lines: Vec<String> = Vec::new();
        let push =
            |lines: &mut Vec<String>, v: Value| lines.push(serde_json::to_string(&v).unwrap());
        push(
            &mut lines,
            json!({
                "type": "user",
                "sessionId": SESSION,
                "message": {"role": "user", "content":
                    "In demo-codec, what is the frame magic number, and how is a block checksum computed?"}
            }),
        );

        // 1. A definition, with the receipt packs give today: no pinned bytes.
        let q1 = "MAGIC";
        let a1 = answer(
            AnswerMode::Def,
            vec![hit(2, "const", "pub const MAGIC: u32 = 0x184D2204;")],
        );
        push(
            &mut lines,
            tool_use(
                "toolu_synth_01",
                "mcp__xerj__xerj_hub_def",
                json!({"symbol": q1, "corpus": "demo-codec",
                       "evidence": {"session_id": SESSION, "tool_call_id": "toolu_synth_01"}}),
            ),
        );
        push(
            &mut lines,
            mcp_result(
                "toolu_synth_01",
                &a1,
                Some(receipt(&a1, "toolu_synth_01", q1, false)),
            ),
        );

        // 2. A how-question whose receipt pins the exact bytes it cites.
        let q2 = "how is a block checksum computed";
        let a2 = answer(
            AnswerMode::How,
            vec![
                hit(5, "function", "pub fn checksum(data: &[u8]) -> u32 {"),
                hit(
                    4,
                    "comment",
                    "/// Checksum of one block: xxh32 with seed 0.",
                ),
            ],
        );
        push(
            &mut lines,
            tool_use(
                "toolu_synth_02",
                "mcp__xerj__xerj_hub_ask",
                json!({"question": q2, "corpus": "demo-codec",
                       "evidence": {"session_id": SESSION, "tool_call_id": "toolu_synth_02"}}),
            ),
        );
        push(
            &mut lines,
            mcp_result(
                "toolu_synth_02",
                &a2,
                Some(receipt(&a2, "toolu_synth_02", q2, true)),
            ),
        );

        // 3. The CLI through a shell tool: the answer is JSON in the text. Its
        //    hit names line 5 for text that is on line 6, which --repo catches.
        let a3 = answer(AnswerMode::How, vec![hit(5, "call", "xxh32(data, 0)")]);
        push(
            &mut lines,
            tool_use(
                "toolu_synth_03",
                "Bash",
                json!({"command": "xerj ask --json --corpus demo-codec \"which hash does checksum call\""}),
            ),
        );
        push(
            &mut lines,
            text_result(
                "toolu_synth_03",
                &serde_json::to_string_pretty(&a3).unwrap(),
            ),
        );

        // 4. A receipt edited after it was issued: the digest no longer matches.
        let q4 = "what seed does the checksum use";
        let a4 = answer(
            AnswerMode::How,
            vec![hit(
                4,
                "comment",
                "/// Checksum of one block: xxh32 with seed 0.",
            )],
        );
        let mut r4 = receipt(&a4, "toolu_synth_04", q4, false);
        r4["entries"][0]["shown_answer"] = json!("the seed is 42");
        push(
            &mut lines,
            tool_use(
                "toolu_synth_04",
                "mcp__xerj__xerj_hub_ask",
                json!({"question": q4, "corpus": "demo-codec",
                       "evidence": {"session_id": SESSION, "tool_call_id": "toolu_synth_04"}}),
            ),
        );
        push(&mut lines, mcp_result("toolu_synth_04", &a4, Some(r4)));

        // 5. A tool call with no hub answer in its result.
        push(
            &mut lines,
            tool_use("toolu_synth_05", "Bash", json!({"command": "ls src"})),
        );
        push(
            &mut lines,
            text_result("toolu_synth_05", "frame.rs\nlib.rs\n"),
        );

        // A line cut short, as a crashed writer leaves one.
        lines.push("{\"type\":\"assistant\",\"message\":{\"role\":\"assist".to_string());
        push(
            &mut lines,
            json!({
                "type": "assistant",
                "sessionId": SESSION,
                "message": {"role": "assistant", "content": [{"type": "text", "text":
                    "The magic number is 0x184D2204 (src/frame.rs:2). A block checksum is xxh32 with seed 0 (src/frame.rs:4-6)."}]}
            }),
        );
        lines.join("\n") + "\n"
    }

    fn fixture() -> String {
        let generated = synthetic_session();
        if std::env::var_os("XERJ_REGEN_TRACE_FIXTURE").is_some() {
            std::fs::create_dir_all(Path::new(FIXTURE).parent().unwrap()).unwrap();
            std::fs::write(FIXTURE, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(FIXTURE).unwrap();
        assert_eq!(
            committed, generated,
            "the committed fixture drifted from synthetic_session(); rerun with \
             XERJ_REGEN_TRACE_FIXTURE=1"
        );
        committed
    }

    #[test]
    fn the_synthetic_session_lists_every_hub_answer_and_its_receipt() {
        let t = trace_text(&fixture(), "fallback", None);
        assert_eq!(t.session, SESSION);
        assert_eq!(t.lines, 13);
        assert_eq!(t.unreadable_lines, 1);
        assert_eq!(t.tool_calls, 5);
        assert_eq!(t.answers.len(), 4, "{t:#?}");
        assert_eq!(t.receipts(), 3);

        let calls: Vec<_> = t
            .answers
            .iter()
            .map(|a| a.tool_call_id.as_deref().unwrap())
            .collect();
        assert_eq!(
            calls,
            [
                "toolu_synth_01",
                "toolu_synth_02",
                "toolu_synth_03",
                "toolu_synth_04"
            ]
        );
        let first = &t.answers[0];
        assert_eq!(first.tool.as_deref(), Some("mcp__xerj__xerj_hub_def"));
        assert_eq!(first.question.as_deref(), Some("MAGIC"));
        assert_eq!(first.pin_sha.as_deref(), Some(PIN));
        assert_eq!(first.answered_from.as_deref(), Some("local pack"));
        assert_eq!(
            (first.evidence[0].file.as_str(), first.evidence[0].line),
            (FILE, 2)
        );
        let r1 = first.receipt.as_ref().unwrap();
        assert!(r1.digest_ok && r1.abstain && r1.citations.is_empty());
        assert!(r1
            .gaps
            .contains(&"no byte-exact pinned evidence".to_string()));

        let r2 = t.answers[1].receipt.as_ref().unwrap();
        assert!(r2.digest_ok);
        assert_eq!(r2.citations.len(), 2);
        assert_eq!(
            (r2.citations[0].line_start, r2.citations[0].bytes.as_str()),
            (5, NOT_CHECKED)
        );

        let shell = &t.answers[2];
        assert_eq!(shell.tool.as_deref(), Some("Bash"));
        assert!(shell
            .question
            .as_deref()
            .unwrap()
            .starts_with("xerj ask --json"));
        assert!(shell.receipt.is_none());

        let tampered = t.answers[3].receipt.as_ref().unwrap();
        assert!(!tampered.digest_ok);
        assert!(
            tampered
                .error
                .as_deref()
                .unwrap()
                .contains("digest mismatch"),
            "{tampered:?}"
        );
        assert_eq!(t.failures(), 1);
    }

    #[test]
    fn with_the_pinned_corpus_every_hit_and_citation_is_checked_byte_for_byte() {
        let t = trace_text(&fixture(), "fallback", Some(&MemReader));
        let bytes: Vec<Vec<&str>> = t
            .answers
            .iter()
            .map(|a| a.evidence.iter().map(|h| h.bytes.as_str()).collect())
            .collect();
        assert_eq!(bytes[0], [VERIFIED]);
        assert_eq!(bytes[1], [VERIFIED, VERIFIED]);
        assert!(bytes[2][0].starts_with("mismatch:"), "{:?}", bytes[2]);
        assert_eq!(bytes[3], [VERIFIED]);
        let r2 = t.answers[1].receipt.as_ref().unwrap();
        assert!(r2.citations.iter().all(|c| c.bytes == VERIFIED));
        // The tampered receipt and the shell answer's wrong line.
        assert_eq!(t.failures(), 2);

        let text = render_trace(&t, true);
        assert!(
            text.contains(
                "session synthetic-session-1789: 13 lines (1 not JSON, skipped), 5 tool calls, \
                 4 hub answers, 3 with an evidence receipt"
            ),
            "{text}"
        );
        assert!(
            text.contains("evidence: src/frame.rs:2  [const]  verified"),
            "{text}"
        );
        assert!(text.contains("2 FAILED verification"), "{text}");
    }

    #[test]
    fn a_receipt_outside_its_answer_joins_the_same_tool_call() {
        let a = answer(
            AnswerMode::Def,
            vec![hit(2, "const", "pub const MAGIC: u32 = 0x184D2204;")],
        );
        let r = receipt(&a, "call-1", "MAGIC", true);
        let line = |v: Value| serde_json::to_string(&v).unwrap();
        let session = [
            line(json!({"type": "tool_result", "tool_use_id": "call-1", "answer": a})),
            line(json!({"type": "tool_result", "tool_use_id": "call-1", "receipt": r})),
            line(json!({"type": "tool_result", "tool_use_id": "call-2", "receipt": r})),
        ]
        .join("\n");
        let t = trace_text(&session, "s", Some(&MemReader));
        assert_eq!(t.answers.len(), 2, "{t:#?}");
        assert!(t.answers[0].corpus.is_some() && t.answers[0].receipt.is_some());
        // The orphan receipt is listed on its own, its question from the receipt.
        assert!(t.answers[1].corpus.is_none());
        assert_eq!(t.answers[1].question.as_deref(), Some("MAGIC"));
        assert_eq!(t.failures(), 0);
    }

    #[test]
    fn a_session_with_no_hub_answers_is_empty_not_an_error() {
        let t = trace_text(
            "{\"type\":\"user\",\"message\":\"hi\"}\nnot json\n",
            "empty",
            None,
        );
        assert!(t.answers.is_empty());
        assert_eq!(
            (t.lines, t.unreadable_lines, t.session.as_str()),
            (2, 1, "empty")
        );
    }

    #[test]
    fn git_reader_reads_a_file_at_a_commit() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        run(&["init", "-q"]);
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join(FILE), FRAME_RS).unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "c",
        ]);
        let head = run(&["rev-parse", "HEAD"]).trim().to_string();
        std::fs::write(dir.path().join(FILE), "changed after the pin\n").unwrap();
        let reader = GitReader(dir.path().to_path_buf());
        assert_eq!(reader.read(&head, FILE).unwrap(), FRAME_RS.as_bytes());
        assert!(reader
            .read(&head, "src/missing.rs")
            .unwrap_err()
            .starts_with("not in the checkout"));
        assert!(reader.read("HEAD", FILE).is_err());
        assert!(reader.read(&head, "-x").is_err());
    }

    #[test]
    fn parse_trace_args_shapes() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_trace_args(&s(&["--help"])), Ok(None));
        assert!(parse_trace_args(&s(&[])).is_err());
        assert!(parse_trace_args(&s(&["a.jsonl", "b.jsonl"])).is_err());
        assert_eq!(
            parse_trace_args(&s(&["a.jsonl", "--json", "--repo", "r"])),
            Ok(Some(TraceArgs {
                path: "a.jsonl".into(),
                repo: Some("r".into()),
                json: true
            }))
        );
    }

    #[test]
    fn run_trace_exit_codes() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        fixture();
        assert_eq!(run_trace(&s(&[FIXTURE, "--json"])), EXIT_VERIFY_FAILED);
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "{}\n").unwrap();
        assert_eq!(run_trace(&s(&[empty.to_str().unwrap(), "--json"])), 1);
        assert_eq!(run_trace(&s(&["/no/such/session.jsonl"])), 2);
    }
}
