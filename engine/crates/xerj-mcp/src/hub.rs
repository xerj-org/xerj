//! Corpus Hub v2 — the `xerj_hub_*` MCP tools (plan §7,
//! `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`; specs in
//! `docs/hub-v2-grpc-mcp-notes.md`).
//!
//! Thin adapters over the code the CLI runs — no hub logic lives here:
//!
//! | tool | runs | which is also |
//! |---|---|---|
//! | `xerj_hub_ask` | [`xerj_hub::ask::run_ask_with`] | `xerj ask "<q>" [--corpus] [--def\|--how\|--files] [--max-tokens]` |
//! | `xerj_hub_def` | [`xerj_hub::ask::run_ask_with`] with `--corpus <slug> --def <SYMBOL>` | `xerj ask --corpus <slug> --def <SYMBOL>` |
//! | `xerj_hub_find` | [`xerj_hub::ask::run_find`] | the routing half of `xerj ask` (what it would pick, and the runners-up) |
//!
//! The answer flow is the CLI's: local packs first, hosted second (this build
//! has no hosted client, so "hosted" is unavailable exactly as it is for the
//! CLI), the catalogue is the cached copy (below), and the node is the
//! one this server proxies to (`--url` / `XERJ_URL`), or the automatic hub node.
//! The tool result text is the §1 rendering with the flow's notices on top (MCP
//! has no stderr); the structured content is the [`xerj_hub::Answer`] JSON.
//!
//! One difference, on purpose: a tool call never downloads a pack. The flow
//! runs with `no_fetch`, so a corpus that is not on this machine is answered
//! with the `xerj hub pull <slug>` line instead — a tool call cannot start a
//! download, and so cannot trigger the pull's least-recently-used eviction (a
//! delete) behind an agent's back. Excluded corpora are never pulled or served
//! by the flow itself; approach-only answers carry the licence warning on every
//! hit.
//!
//! Nor does a tool call fetch the catalogue: the tools read the copy `xerj hub
//! sync` (or any CLI `xerj ask` / `xerj hub pull`) cached, however old, and
//! never block on the hub's 10 s/30 s fetch timeouts inside an agent's turn.
//! With no cached copy the tool says to run `xerj hub sync`.

use serde_json::{json, Value};
use xerj_common::hub_evidence::AnswerEvidence;
use xerj_hub::ask::{
    self, AskArgs, AskOutcome, ModeRequest, EXIT_NEEDS_DECISION, FIND_DEFAULT_K, FIND_MAX_K,
};
use xerj_hub::node::NodeLauncher;

use crate::{tool_text, Ctx};

/// The longest question or symbol a tool takes — the hosted routes' limit.
pub const MAX_INPUT_BYTES: usize = 512;
/// `k` for `xerj_hub_def` is clamped to this (the `def` maximum).
const MAX_K: u64 = 50;
/// The largest `max_tokens` accepted (the hosted `code` route's cap).
const MAX_TOKENS_CAP: u64 = 8192;

/// The three tool specs, in the order `tools/list` serves them.
pub fn specs() -> Vec<Value> {
    vec![
        json!({
            "name": "xerj_hub_find",
            "description": concat!(
                "Discover which hub corpora can answer a question. Each candidate names the ",
                "upstream repo (owner/name) and a one-line description of what that pack is, plus ",
                "its pin, licence and whether the pack is already on this machine — not passages; ",
                "ask one with xerj_hub_ask. A query that names a different project of the same slug ",
                "(fabric.js vs the pack's repo) is reported as a different project instead of a ",
                "silent match. A question that names a project no corpus covers is no_coverage \
                 (index it locally with xerj autoindex, then xerj code or xerj search); that is not \
                 an ambiguous tie. Routed exactly as `xerj ask` routes: your project's dependencies, ",
                "the signed catalogue's term sketches, and the exact symbol a definition question ",
                "names (offline: reads the catalogue `xerj hub sync` cached and never fetches it; ",
                "this build has no hosted search). Excluded-licence corpora are never listed."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "What the agent is working on; a corpus name is never required here."
                    },
                    "k": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": FIND_MAX_K,
                        "description": format!(
                            "Max candidate corpora to return (default {FIND_DEFAULT_K}, at most {FIND_MAX_K})."
                        )
                    }
                },
                "required": ["question"]
            }
        }),
        json!({
            "name": "xerj_hub_def",
            "description": concat!(
                "Look up one definition in a named hub corpus: its kind, signature, file:line, and ",
                "the literal value for a const/macro/define. Returns an Answer in def mode (no ",
                "passages). Same as `xerj ask --corpus <slug> --def <SYMBOL>`: answered from the ",
                "local pack; a corpus not on this machine is never downloaded by this tool — the ",
                "answer says which `xerj hub pull` line fetches it. Approach-only corpora carry a ",
                "read-do-not-copy warning on every hit."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Corpus slug to resolve the symbol in." },
                    "symbol": { "type": "string", "description": "Exact definition name." },
                    "k": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_K,
                        "description": "Max hits to return (default 5)."
                    },
                    "evidence": evidence_schema()
                },
                "required": ["slug", "symbol"]
            }
        }),
        json!({
            "name": "xerj_hub_ask",
            "description": concat!(
                "Ask a hub corpus a question in one turn — no clone, no local indexing. Local packs ",
                "first, hosted second, exactly like `xerj ask`. The corpus is optional (routed by ",
                "the offline sketch when omitted) and is always named in the answer; when more than ",
                "one corpus fits, the candidates come back and you re-ask with `corpus`. When no \
                 corpus covers the topic, the result says so and points at local indexing instead of \
                 asking you to pick a corpus. Never ",
                "downloads a pack: a corpus not on this machine is answered with the `xerj hub ",
                "pull` line to run, and never fetches the catalogue: with none cached it says to run ",
                "`xerj hub sync`. Result text is the hub rendering (corpus, pin date, licence, ",
                "file:line); structured content is the Answer JSON."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "Natural-language question." },
                    "corpus": { "type": "string", "description": "Optional explicit corpus slug; omit to route by sketch." },
                    "mode": {
                        "type": "string",
                        "enum": ["def", "how", "files"],
                        "description": "Answer shape; omit for auto (a lone identifier or 'what is the value of X' is def)."
                    },
                    "max_tokens": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TOKENS_CAP,
                        "description": "Soft output budget for the rendered answer (about 4 characters a token)."
                    },
                    "evidence": evidence_schema()
                },
                "required": ["question"]
            }
        }),
    ]
}

fn evidence_schema() -> Value {
    json!({
        "type": "object",
        "description": "Opt in to structuredContent.evidence_receipt. Supply caller trace identities. Without original pinned bytes and an in-domain calibration assessment, the receipt abstains and escalates; search scores are never probabilities.",
        "properties": {
            "session_id": { "type": "string", "minLength": 1, "maxLength": MAX_INPUT_BYTES },
            "tool_call_id": { "type": "string", "minLength": 1, "maxLength": MAX_INPUT_BYTES },
            "p_min": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.8 }
        },
        "required": ["session_id", "tool_call_id"],
        "additionalProperties": false
    })
}

struct EvidenceRequest {
    session_id: String,
    tool_call_id: String,
    p_min: f64,
}

fn evidence_arg(args: &Value) -> Result<Option<EvidenceRequest>, String> {
    let Some(value) = args.get("evidence") else {
        return Ok(None);
    };
    let object = value.as_object().ok_or("`evidence` must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "session_id" | "tool_call_id" | "p_min"))
    {
        return Err("unknown `evidence` field".into());
    }
    let p_min = match value.get("p_min") {
        None => 0.8,
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
            .ok_or("`p_min` must be a probability from 0 to 1")?,
    };
    Ok(Some(EvidenceRequest {
        session_id: text_arg(value, "session_id")?,
        tool_call_id: text_arg(value, "tool_call_id")?,
        p_min,
    }))
}

/// Dispatch one `xerj_hub_*` call; `None` when `name` is not a hub tool.
pub async fn call(ctx: &Ctx, name: &str, args: &Value) -> Option<Value> {
    let result = match name {
        "xerj_hub_find" => find(args).await,
        "xerj_hub_def" => def(ctx, args).await,
        "xerj_hub_ask" => ask_tool(ctx, args).await,
        _ => return None,
    };
    Some(result.unwrap_or_else(|msg| tool_text(format!("{name}: {msg}"), true)))
}

// ── argument checks ──────────────────────────────────────────────────────────

/// A required, non-blank string of at most [`MAX_INPUT_BYTES`].
fn text_arg(args: &Value, key: &str) -> Result<String, String> {
    let raw = match args.get(key) {
        Some(Value::String(s)) => s,
        Some(_) => return Err(format!("`{key}` must be a string")),
        None => return Err(format!("`{key}` is required")),
    };
    if raw.len() > MAX_INPUT_BYTES {
        return Err(format!(
            "`{key}` is limited to {MAX_INPUT_BYTES} bytes, got {}",
            raw.len()
        ));
    }
    let t = raw.trim();
    if t.is_empty() {
        return Err(format!("`{key}` must not be blank"));
    }
    Ok(t.to_string())
}

/// The longest corpus slug a tool accepts — the server's `valid_slug` cap.
const MAX_SLUG_BYTES: usize = 64;

/// A corpus slug, checked before anything is looked up or built from it.
fn slug_arg(args: &Value, key: &str, required: bool) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) if !required => Ok(None),
        None | Some(Value::Null) => Err(format!("`{key}` is required")),
        Some(Value::String(s)) if s.len() <= MAX_SLUG_BYTES && xerj_hub::cli::valid_slug(s) => {
            Ok(Some(s.clone()))
        }
        Some(Value::String(s)) if s.is_empty() && !required => Ok(None),
        Some(_) => Err(format!(
            "`{key}` must be a corpus slug (lower-case letters, digits, '.', '_', '-'; at most \
             {MAX_SLUG_BYTES} bytes)"
        )),
    }
}

/// An optional integer in `1..=max`.
fn int_arg(args: &Value, key: &str, max: u64) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64().filter(|n| (1..=max).contains(n)) {
            Some(n) => Ok(Some(n)),
            None => Err(format!("`{key}` must be an integer from 1 to {max}")),
        },
    }
}

// ── the tools ────────────────────────────────────────────────────────────────

async fn find(args: &Value) -> Result<Value, String> {
    let question = text_arg(args, "question")?;
    let k = int_arg(args, "k", FIND_MAX_K as u64)?.map_or(FIND_DEFAULT_K, |n| n as usize);
    let found = tokio::task::spawn_blocking(move || ask::run_find(&question, k))
        .await
        .map_err(|e| format!("join error: {e}"))??;
    Ok(json!({
        "content": [ { "type": "text", "text": ask::render_find(&found) } ],
        "structuredContent": found,
        "isError": false,
    }))
}

async fn def(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let slug = slug_arg(args, "slug", true)?.unwrap_or_default();
    let symbol = text_arg(args, "symbol")?;
    let k = int_arg(args, "k", MAX_K)?;
    let evidence = evidence_arg(args)?;
    let mut a = AskArgs::new(symbol.clone());
    a.corpus = Some(slug);
    a.mode = ModeRequest::Def(symbol);
    a.k = k.map(|n| n as usize);
    Ok(run(ctx, a, evidence).await)
}

async fn ask_tool(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let question = text_arg(args, "question")?;
    let corpus = slug_arg(args, "corpus", false)?;
    let mode = match args.get("mode") {
        None | Some(Value::Null) => ModeRequest::Auto,
        Some(Value::String(m)) => match m.as_str() {
            "how" => ModeRequest::How,
            "files" => ModeRequest::Files,
            // `def` names no symbol of its own: the one the question names
            // (`what is the value of X`), else the whole question — what
            // `xerj ask --def` would be given.
            "def" => ModeRequest::Def(
                ask::detect_mode(&question, &ModeRequest::Auto)
                    .1
                    .unwrap_or_else(|| question.clone()),
            ),
            other => return Err(format!("`mode` must be def, how or files, not '{other}'")),
        },
        Some(_) => return Err("`mode` must be a string".to_string()),
    };
    let max_tokens = int_arg(args, "max_tokens", MAX_TOKENS_CAP)?;
    let evidence = evidence_arg(args)?;
    let mut a = AskArgs::new(question);
    a.corpus = corpus;
    a.mode = mode;
    a.max_tokens = max_tokens.map(|n| n as usize);
    Ok(run(ctx, a, evidence).await)
}

/// Run the CLI's flow for `a` (never fetching a pack or the catalogue) against
/// this server's node and turn the outcome into a tool result.
async fn run(ctx: &Ctx, mut a: AskArgs, evidence: Option<EvidenceRequest>) -> Value {
    a.no_fetch = true;
    a.cached_catalogue_only = true;
    a.json = false;
    a.url = Some(ctx.base_url.clone());
    let launcher = match launcher() {
        Ok(l) => l,
        Err(e) => return tool_text(e, true),
    };
    let question = a.question.clone();
    match tokio::task::spawn_blocking(move || ask::run_ask_with(&a, None, launcher)).await {
        Ok(outcome) => {
            if let Some(answer) = &outcome.answer {
                auto_review(answer, &question);
            }
            outcome_result(outcome, &question, evidence)
        }
        Err(e) => tool_text(format!("join error: {e}"), true),
    }
}

/// One automatic review per corpus per week after a hub answer
/// ([`xerj_hub::feedback::auto_review_for_answer`]: hits or not, the pack's age,
/// the question's shape only). Sent in the background, so the tool result is
/// never held; failures are silent. Off with the feedback opt-out. The agent's
/// own `xerj_corpus_feedback` replaces it on the server.
fn auto_review(answer: &xerj_common::hub::Answer, question: &str) {
    let (family, version) = crate::client_family();
    let Some(review) =
        xerj_hub::feedback::auto_review_for_answer(answer, question, &family, &version)
    else {
        return;
    };
    tokio::task::spawn_blocking(move || {
        let path = xerj_hub::feedback::state_path();
        if let Some(r) = xerj_hub::feedback::claim_auto(&path, "mcp", review) {
            let _ = xerj_hub::feedback::send_blocking(
                &r,
                &xerj_common::corpus_feedback::endpoint_from_env(),
                xerj_hub::feedback::AUTO_TIMEOUT,
            );
        }
    });
}

/// The re-ask hint as the CLI words it, and as a tool caller needs it.
const CLI_CORPUS_HINT: &str = "re-run with --corpus <slug>";
const MCP_CORPUS_HINT: &str = "call xerj_hub_ask again with the `corpus` argument";

/// An [`AskOutcome`] as a tool result. An answer — including one with no hits
/// (a miss is an answer, not an error) — carries the Answer JSON as structured
/// content. More than one candidate corpus is not an error either: the text
/// lists them for a re-ask with `corpus`. Anything else is `isError`.
fn outcome_result(o: AskOutcome, question: &str, evidence: Option<EvidenceRequest>) -> Value {
    let mut text = String::new();
    for line in &o.stderr {
        // The flow's hints name the CLI flag; a tool caller sets an argument.
        text.push_str(&line.replace(CLI_CORPUS_HINT, MCP_CORPUS_HINT));
        text.push('\n');
    }
    text.push_str(&o.stdout);
    match o.answer {
        Some(answer) => {
            let receipt = if let Some(request) = evidence {
                let context = AnswerEvidence {
                    session_id: request.session_id,
                    tool_call_id: request.tool_call_id,
                    question: question.into(),
                    shown_answer: text.clone(),
                    confidence: None,
                    p_min: request.p_min,
                    coverage_gaps: vec!["question-specific coverage not assessed".into()],
                };
                // Packs expose rendered index records, not original pinned files.
                // Never read mutable local files or accept caller-supplied evidence.
                match answer.evidence_report(context, |_| Ok(None)) {
                    Ok(report) => Some(report),
                    Err(e) => return tool_text(format!("evidence receipt: {e}"), true),
                }
            } else {
                None
            };
            let mut result = json!({
                "content": [ { "type": "text", "text": text } ],
                "structuredContent": answer,
                "isError": false,
            });
            if let Some(receipt) = receipt {
                result["structuredContent"]["evidence_receipt"] = json!(receipt);
            }
            result
        }
        None => tool_text(text, o.exit != EXIT_NEEDS_DECISION),
    }
}

/// What starts the automatic hub node: this executable when it is `xerj`
/// (`xerj mcp`); for the standalone `xerj-mcp` binary — which takes no node
/// flags — the `xerj` beside it, else `xerj` on `PATH`.
fn launcher() -> Result<NodeLauncher, String> {
    let current = NodeLauncher::current().map_err(|e| format!("{e:#}"))?;
    let standalone = current
        .exe
        .file_stem()
        .is_some_and(|s| s.to_string_lossy() == "xerj-mcp");
    if !standalone {
        return Ok(current);
    }
    let sibling = current
        .exe
        .with_file_name(format!("xerj{}", std::env::consts::EXE_SUFFIX));
    Ok(NodeLauncher {
        exe: if sibling.is_file() {
            sibling
        } else {
            std::path::PathBuf::from("xerj")
        },
        pre_args: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_refused_before_anything_runs() {
        let long = "x".repeat(MAX_INPUT_BYTES + 1);
        assert!(text_arg(&json!({ "question": long }), "question").is_err());
        assert!(text_arg(&json!({ "question": "  " }), "question").is_err());
        assert!(text_arg(&json!({ "question": 7 }), "question").is_err());
        assert_eq!(
            text_arg(&json!({ "question": " q " }), "question").unwrap(),
            "q"
        );
        for bad in ["../etc", "a/b", "", "UPPER", "-lead"] {
            assert!(
                slug_arg(&json!({ "slug": bad }), "slug", true).is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(
            slug_arg(&json!({ "slug": "mini-c" }), "slug", true).unwrap(),
            Some("mini-c".into())
        );
        assert_eq!(slug_arg(&json!({}), "corpus", false).unwrap(), None);
        assert!(int_arg(&json!({ "k": 0 }), "k", 50).is_err());
        assert!(int_arg(&json!({ "k": 51 }), "k", 50).is_err());
        assert!(int_arg(&json!({ "k": "5" }), "k", 50).is_err());
        assert_eq!(int_arg(&json!({ "k": 50 }), "k", 50).unwrap(), Some(50));
    }

    /// Review finding (rev7): `slug_arg` took a slug of any length (a 200 KB
    /// one was accepted and echoed back in "unknown corpus '…'"). The server's
    /// own `valid_slug` caps a slug at 64 bytes; the tool arguments do too.
    #[test]
    fn a_slug_longer_than_the_servers_cap_is_refused_and_never_echoed() {
        let at_cap = "a".repeat(MAX_SLUG_BYTES);
        assert_eq!(
            slug_arg(&json!({ "slug": at_cap }), "slug", true).unwrap(),
            Some(at_cap.clone())
        );
        for len in [MAX_SLUG_BYTES + 1, 200 * 1024] {
            let long = "a".repeat(len);
            for (key, required) in [("slug", true), ("corpus", false)] {
                let err = slug_arg(&json!({ key: long }), key, required)
                    .expect_err("over the cap is refused");
                assert!(
                    err.len() < 200,
                    "the slug is not echoed back: {}",
                    err.len()
                );
            }
        }
    }

    #[test]
    fn an_ambiguous_route_is_not_an_error_and_a_failure_is() {
        let ambiguous = AskOutcome {
            exit: EXIT_NEEDS_DECISION,
            stdout: "a — x\nb — y".into(),
            stderr: vec!["more than one corpus matches; re-run with --corpus <slug>".into()],
            answer: None,
        };
        let r = outcome_result(ambiguous, "question", None);
        assert_eq!(r["isError"], false);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("a — x"));
        let failed = AskOutcome {
            exit: 1,
            stdout: String::new(),
            stderr: vec!["no hub corpus matches; see xerj.org/llms.txt".into()],
            answer: None,
        };
        assert_eq!(outcome_result(failed, "question", None)["isError"], true);
    }

    /// FTX shell-less trial (2026-10-17): the "nothing matches" and "more than one
    /// corpus" hints told a tool caller to "re-run with --corpus <slug>", a CLI
    /// flag it has no way to pass. The tool result names the argument instead.
    #[test]
    fn the_reask_hint_names_the_tool_argument_not_the_cli_flag() {
        for (exit, line) in [
            (
                1,
                "closest corpora: lz4, snappy; re-run with --corpus <slug> to ask one of them",
            ),
            (
                EXIT_NEEDS_DECISION,
                "more than one corpus matches; re-run with --corpus <slug>",
            ),
        ] {
            let r = outcome_result(
                AskOutcome {
                    exit,
                    stdout: String::new(),
                    stderr: vec![line.into()],
                    answer: None,
                },
                "question",
                None,
            );
            let text = r["content"][0]["text"].as_str().unwrap();
            assert!(!text.contains("--corpus"), "{text}");
            assert!(
                text.contains("xerj_hub_ask again with the `corpus` argument"),
                "{text}"
            );
        }
    }

    #[test]
    fn receipt_arguments_are_strict_and_opt_in() {
        assert!(evidence_arg(&json!({})).unwrap().is_none());
        for value in [
            json!(true),
            json!(null),
            json!({}),
            json!({"session_id":"s","tool_call_id":"c","p_min":1.1}),
            json!({"session_id":"s","tool_call_id":"c","confidence":0.99}),
        ] {
            assert!(evidence_arg(&json!({"evidence":value})).is_err());
        }
        let request = evidence_arg(&json!({"evidence":{"session_id":"s","tool_call_id":"c"}}))
            .unwrap()
            .unwrap();
        assert_eq!(request.p_min, 0.8);
        for spec in specs().into_iter().skip(1) {
            assert_eq!(
                spec["inputSchema"]["properties"]["evidence"]["type"],
                "object"
            );
        }
    }

    #[test]
    fn receipt_is_additive_preserves_default_and_captures_exact_display() {
        use xerj_common::hub::{Answer, AnswerMode, AnsweredFrom, Hit, LicenceClass};
        use xerj_common::hub_evidence::TraceEvidenceReport;
        let mut answer = Answer {
            corpus: "sample".into(),
            pin_sha: "a".repeat(40),
            pin_date: "2026-11-04".into(),
            age_days: 0,
            licence_spdx: "MIT".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: AnsweredFrom::LocalPack,
            mode: AnswerMode::How,
            hits: vec![Hit {
                file: "src/lib.rs".into(),
                line: 1,
                kind: "passage".into(),
                text: "example".into(),
                value: None,
                score: 100.0,
                licence_warning: None,
            }],
            truncated: false,
            warnings: vec![],
        };
        for empty in [false, true] {
            if empty {
                answer.hits.clear();
            }
            let outcome = || AskOutcome {
                exit: 0,
                stdout: xerj_common::hub::render(&answer),
                stderr: vec!["notice".into()],
                answer: Some(answer.clone()),
            };
            let expected = json!({
                "content":[{"type":"text","text":format!("notice\n{}",xerj_common::hub::render(&answer))}],
                "structuredContent":answer,"isError":false,
            });
            let default = outcome_result(outcome(), "question", None);
            assert_eq!(default, expected);
            let request = EvidenceRequest {
                session_id: "s".into(),
                tool_call_id: "c".into(),
                p_min: 0.8,
            };
            let mut opted = outcome_result(outcome(), "question", Some(request));
            let receipt = opted["structuredContent"]
                .as_object_mut()
                .unwrap()
                .remove("evidence_receipt")
                .unwrap();
            assert_eq!(opted, default);
            let report: TraceEvidenceReport = serde_json::from_value(receipt).unwrap();
            let entry = &report.entries[0];
            assert!(entry.abstain && entry.escalate);
            assert!(entry.confidence.is_none());
            assert_eq!(
                entry.shown_answer,
                default["content"][0]["text"].as_str().unwrap()
            );
            TraceEvidenceReport::verify(&report.to_jcs().unwrap(), |_| panic!("no source bytes"))
                .unwrap();
        }
    }
}
