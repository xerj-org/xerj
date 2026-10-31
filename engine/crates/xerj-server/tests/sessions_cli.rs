//! Offline CLI export: success, redaction and failure publication boundaries.
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn import(input: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["sessions", "import", "--format", "claude-code", "--input"])
        .arg(input)
        .arg("--output")
        .arg(output)
        .env("XERJ_DISABLE_FEEDBACK", "true")
        .output()
        .unwrap()
}

#[test]
fn sessions_cli_exports_only_redacted_conversation_and_refuses_overwrite() {
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("session.jsonl");
    std::fs::write(
        &input,
        include_str!("../../xerj-autoindex/tests/fixtures/claude-session.jsonl"),
    )
    .unwrap();
    let output = work.path().join("records.jsonl");
    let result = import(&input, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["records"], 2);
    assert_eq!(report["source_lines"], 4);
    assert_eq!(report["skipped_records"], 2);
    assert_eq!(report["skipped_blocks"], 3);
    let bytes = std::fs::read(&output).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("[REDACTED_EMAIL]"));
    assert!(!text.contains("owner@example.com"));
    assert!(!text.contains("DO_NOT_EXPORT"));
    let denied = import(&input, &output);
    assert_eq!(denied.status.code(), Some(1));
    assert_eq!(std::fs::read(output).unwrap(), bytes);
}

#[test]
fn sessions_cli_bad_format_is_usage_error_without_output() {
    let work = tempfile::tempdir().unwrap();
    let output = work.path().join("safe.jsonl");
    let result = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args([
            "sessions", "import", "--format", "unknown", "--input", "unused", "--output",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
}

#[test]
fn sessions_cli_codex_rollout_exports_decision_without_duplicate_events_or_private_payloads() {
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("rollout.jsonl");
    let output = work.path().join("records.jsonl");
    std::fs::write(
        &input,
        include_str!("../../xerj-autoindex/tests/fixtures/codex-session.jsonl"),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["sessions", "import", "--format", "codex", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["records"], 2);
    assert_eq!(report["source_lines"], 10);
    assert_eq!(report["skipped_records"], 8);
    assert_eq!(report["skipped_blocks"], 1);
    let text = std::fs::read_to_string(output).unwrap();
    assert!(!text.contains("DO_NOT_EXPORT"));
    assert!(!text.contains("owner@example.com"));
    assert!(text.contains("[REDACTED_EMAIL]"));
    let rows: Vec<Value> = text
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(rows[1]["session_format"], "codex");
    assert_eq!(rows[1]["source_line"], 9);
    assert_eq!(rows[1]["message_phase"], "final_answer");
    assert_eq!(rows[1]["parent_id"], Value::Null);
    assert_eq!(rows[1]["files"], serde_json::json!([]));
}

#[test]
fn sessions_cli_malformed_tail_keeps_input_and_publishes_no_partial_export() {
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("session.jsonl");
    let output = work.path().join("records.jsonl");
    let mut bytes =
        include_bytes!("../../xerj-autoindex/tests/fixtures/claude-session.jsonl").to_vec();
    bytes.extend_from_slice(b"{\"private\":\"NEVER_ECHO_PRIVATE_PAYLOAD\"");
    std::fs::write(&input, &bytes).unwrap();
    let result = import(&input, &output);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("invalid JSON"), "{error}");
    assert!(!error.contains("NEVER_ECHO_PRIVATE_PAYLOAD"));
    assert!(!output.exists());
    assert_eq!(std::fs::read(input).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 1);
}
