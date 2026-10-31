//! Explicit Codex rollout subset. Reference and licence are pinned in the import guide.
use super::*;

#[derive(Default)]
pub(super) struct CodexState {
    session: Option<String>,
    cwd: Value,
    branch: Value,
    messages: usize,
}

fn timestamp(entry: &Value, line: u64) -> Result<Value> {
    let timestamp = entry["timestamp"]
        .as_str()
        .with_context(|| format!("line {line}: missing rollout timestamp"))?;
    ensure!(
        chrono::DateTime::parse_from_rfc3339(timestamp).is_ok(),
        "line {line}: invalid rollout timestamp"
    );
    Ok(json!(timestamp))
}

impl CodexState {
    pub(super) fn turn(
        &mut self,
        entry: &Value,
        line: u64,
        offset: u64,
        source_file: &str,
        bytes: &[u8],
        summary: &mut ImportSummary,
    ) -> Result<Option<ParsedTurn>> {
        let kind = entry["type"]
            .as_str()
            .with_context(|| format!("line {line}: missing rollout record type"))?;
        let time = timestamp(entry, line)?;
        let payload = &entry["payload"];
        ensure!(
            payload.is_object(),
            "line {line}: expected rollout payload object"
        );
        if kind == "session_meta" {
            ensure!(
                self.session.is_none(),
                "line {line}: repeated session metadata; select one rollout file"
            );
            self.session = Some(uuid_field(payload, "id", line)?);
            // A single file cannot resolve a prefix inherited from another rollout.
            ensure!(payload["history_base"].is_null(), "line {line}: external history_base is unsupported; select a self-contained archived rollout");
            self.cwd = optional_text(payload, "cwd", line)?;
            ensure!(
                payload["git"].is_null() || payload["git"].is_object(),
                "line {line}: invalid git metadata"
            );
            self.branch = optional_text(&payload["git"], "branch", line)?;
            summary.skipped_records += 1;
            return Ok(None);
        }
        let sid = self.session.as_ref().with_context(|| format!("line {line}: Codex rollout must start with session_meta; exec --json is unsupported"))?;
        if kind == "turn_context" {
            // This is the explicit per-turn working directory; instructions stay out.
            self.cwd = optional_text(payload, "cwd", line)?;
            summary.skipped_records += 1;
            return Ok(None);
        }
        if kind != "response_item" {
            ensure!(
                matches!(
                    kind,
                    "event_msg"
                        | "compacted"
                        | "token_usage_record"
                        | "world_state"
                        | "security_risk_score"
                ),
                "line {line}: unsupported rollout record type"
            );
            summary.skipped_records += 1;
            return Ok(None);
        }
        let item = payload["type"]
            .as_str()
            .with_context(|| format!("line {line}: missing response item type"))?;
        if item != "message" {
            ensure!(
                matches!(
                    item,
                    "reasoning"
                        | "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output"
                        | "local_shell_call"
                        | "web_search_call"
                        | "image_generation_call"
                        | "tool_search_call"
                        | "tool_search_output"
                        | "additional_tools"
                        | "agent_message"
                        | "compaction"
                        | "configuration_update"
                        | "compaction_trigger"
                        | "context_compaction"
                ),
                "line {line}: unsupported response item type"
            );
            summary.skipped_records += 1;
            return Ok(None);
        }
        let role = payload["role"]
            .as_str()
            .with_context(|| format!("line {line}: missing message role"))?;
        if matches!(role, "system" | "developer") {
            summary.skipped_records += 1;
            return Ok(None);
        }
        ensure!(
            matches!(role, "user" | "assistant"),
            "line {line}: unsupported message role"
        );
        ensure!(
            payload["channel"].is_null(),
            "line {line}: channel-bearing messages are unsupported"
        );
        self.messages += 1;
        ensure!(
            self.messages <= MAX_RECORDS,
            "session exceeds 100000 response messages"
        );
        let phase = optional_text(payload, "phase", line)?;
        ensure!(
            phase.is_null()
                || matches!(
                    phase.as_str(),
                    Some("commentary" | "partial_answer" | "final_answer")
                ),
            "line {line}: unsupported message phase"
        );
        let blocks = payload["content"]
            .as_array()
            .with_context(|| format!("line {line}: unsupported message content"))?;
        let mut body = String::new();
        for block in blocks {
            match block["type"].as_str() {
                Some("input_text" | "output_text") => {
                    let text = block["text"]
                        .as_str()
                        .with_context(|| format!("line {line}: malformed text block"))?;
                    let separator = usize::from(!body.is_empty());
                    ensure!(
                        body.len() + separator + text.len() <= MAX_TEXT_BYTES,
                        "line {line}: turn text exceeds 256 KiB"
                    );
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(text);
                }
                Some("input_image" | "input_audio") => summary.skipped_blocks += 1,
                _ => bail!("line {line}: unsupported content block type"),
            }
        }
        if body.trim().is_empty() {
            summary.skipped_records += 1;
            return Ok(None);
        }
        // Rollout message IDs can be absent, arbitrary or repeated. This is an
        // archive event identity, not an invented provider ID or parent chain.
        let mid = format!("line-{line}-{:x}", Sha256::digest(bytes));
        Ok(Some(ParsedTurn {
            text_fields: json!({"body":body, "files":[], "source_file":source_file,
                "cwd":self.cwd, "git_branch":self.branch}),
            identity_fields: json!({"id":format!("codex/{sid}/{mid}"),
                "session_schema_version":1, "session_format":"codex", "session_id":sid,
                "message_id":mid, "parent_id":null, "role":role, "timestamp":time,
                "source_line":line, "source_byte_offset":offset, "is_sidechain":null,
                "is_summary":false, "message_phase":phase}),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::write_input;

    fn meta() -> Value {
        json!({"timestamp":"2026-10-25T00:00:00Z", "type":"session_meta",
            "payload":{"id":"550e8400-e29b-41d4-a716-446655440000", "cwd":"/first", "git":{"branch":"main"}}})
    }

    fn message(text: &str) -> Value {
        json!({"timestamp":"2026-10-25T00:00:01Z", "type":"response_item",
            "payload":{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]}})
    }

    #[test]
    fn codex_archive_identity_provenance_and_redaction_are_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("owner@example.com.jsonl");
        let output = dir.path().join("safe.jsonl");
        let original = include_bytes!("../../tests/fixtures/codex-session.jsonl");
        fs::write(&input, original).unwrap();
        let summary = import_codex(&input, &output).unwrap();
        assert_eq!(
            (
                summary.records,
                summary.source_lines,
                summary.skipped_records,
                summary.skipped_blocks
            ),
            (2, 10, 8, 1)
        );
        let safe = fs::read(&output).unwrap();
        let text = std::str::from_utf8(&safe).unwrap();
        assert!(!text.contains("DO_NOT_EXPORT"));
        assert!(!text.contains("owner@example.com"));
        assert!(text.contains("[REDACTED_EMAIL]"));
        let rows: Vec<Value> = text
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let offset: usize = original
            .split_inclusive(|b| *b == b'\n')
            .take(8)
            .map(<[u8]>::len)
            .sum();
        assert_eq!(rows[1]["source_line"], 9);
        assert_eq!(rows[1]["source_byte_offset"], offset);
        assert_eq!(rows[1]["message_phase"], "final_answer");
        assert_eq!(rows[1]["is_sidechain"], Value::Null);
        assert_eq!(rows[1]["parent_id"], Value::Null);
        assert_eq!(rows[1]["files"], json!([]));
        assert_eq!(
            summary.source_sha256,
            format!("{:x}", Sha256::digest(original))
        );
        assert_eq!(
            summary.output_sha256,
            format!("{:x}", Sha256::digest(&safe))
        );
        assert_eq!(fs::read(&input).unwrap(), original);
        assert_eq!(
            rows[0].as_object().unwrap().keys().collect::<Vec<_>>(),
            rows[1].as_object().unwrap().keys().collect::<Vec<_>>()
        );
        let again = dir.path().join("again.jsonl");
        import_codex(&input, &again).unwrap();
        assert_eq!(fs::read(&again).unwrap(), safe);
        assert!(import_codex(&input, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), safe);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn codex_ignores_provider_ids_and_keeps_identical_messages_as_distinct_archive_events() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        let mut turn = message("repeat");
        turn["payload"]["id"] = json!("not-a-uuid owner@example.com");
        write_input(&input, &[meta(), turn.clone(), turn]);
        import_codex(&input, &output).unwrap();
        let rows: Vec<Value> = fs::read_to_string(output)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_ne!(rows[0]["id"], rows[1]["id"]);
        assert!(rows[0]["message_id"]
            .as_str()
            .unwrap()
            .starts_with("line-2-"));
        assert!(!rows[0].to_string().contains("owner@example.com"));
    }

    #[test]
    fn codex_context_and_all_five_pii_types_use_the_shared_redactor() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        let mut header = meta();
        header["payload"]["git"]["branch"] = json!("owner@example.com");
        let context = json!({"timestamp":"2026-10-25T00:00:00Z", "type":"turn_context", "payload":{"cwd":"/owner@example.com/new", "instructions":"DO_NOT_EXPORT"}});
        write_input(
            &input,
            &[
                header,
                context,
                message(
                    "Email: owner@example.com. IP: 192.0.2.55. Card: 4111-1111-1111-1111. SSN: 123-45-6789. Phone: 202-555-0147.",
                ),
            ],
        );
        import_codex(&input, &output).unwrap();
        let text = fs::read_to_string(output).unwrap();
        for marker in ["EMAIL", "IP", "CC", "SSN", "PHONE"] {
            assert!(text.contains(&format!("[REDACTED_{marker}]")), "{text}");
        }
        assert!(!text.contains("DO_NOT_EXPORT"));
        assert!(!text.contains("owner@example.com"));
        let row: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(row["cwd"], "/[REDACTED_EMAIL]/new");
        assert_eq!(row["git_branch"], "[REDACTED_EMAIL]");
    }

    #[test]
    fn codex_unsupported_or_malformed_shapes_publish_nothing_and_can_recover() {
        let mut missing_text = message("valid");
        missing_text["payload"]["content"][0]["text"] = json!(7);
        let mut role = message("valid");
        role["payload"]["role"] = json!("future");
        let mut block = message("valid");
        block["payload"]["content"][0]["type"] = json!("future_text");
        let mut phase = message("valid");
        phase["payload"]["phase"] = json!("analysis");
        let mut channel = message("DO_NOT_EXPORT_ANALYSIS");
        channel["payload"]["channel"] = json!("analysis");
        let mut time = message("valid");
        time["timestamp"] = json!("invalid");
        let mut cwd = meta();
        cwd["type"] = json!("turn_context");
        cwd["payload"]["cwd"] = json!(5);
        let mut inherited = meta();
        inherited["payload"]["history_base"] = json!({"rollout_id":"external"});
        let mut bad_id = meta();
        bad_id["payload"]["id"] = json!("zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz");
        for (entries, needle) in [
            (vec![message("valid")], "start with session_meta"),
            (
                vec![meta(), message("valid"), meta()],
                "repeated session metadata",
            ),
            (vec![inherited, message("valid")], "external history_base"),
            (vec![bad_id], "invalid UUID"),
            (
                vec![meta(), message("valid"), missing_text],
                "malformed text",
            ),
            (vec![meta(), role], "unsupported message role"),
            (vec![meta(), block], "unsupported content block"),
            (vec![meta(), phase], "unsupported message phase"),
            (vec![meta(), channel], "channel-bearing"),
            (vec![meta(), time], "invalid rollout timestamp"),
            (vec![meta(), cwd], "expected text in cwd"),
            (
                vec![
                    meta(),
                    json!({"timestamp":"2026-10-25T00:00:00Z", "type":"future", "payload":{}}),
                ],
                "unsupported rollout",
            ),
            (vec![meta()], "no supported text-bearing"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.jsonl");
            let output = dir.path().join("safe.jsonl");
            write_input(&input, &entries);
            let before = fs::read(&input).unwrap();
            let error = import_codex(&input, &output).unwrap_err().to_string();
            assert!(error.contains(needle), "{needle}: {error}");
            assert!(!error.contains("DO_NOT_EXPORT"));
            assert!(!output.exists());
            assert_eq!(fs::read(&input).unwrap(), before);
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
            write_input(&input, &[meta(), message("fixed archive")]);
            assert_eq!(import_codex(&input, &output).unwrap().records, 1);
        }
    }

    #[test]
    fn codex_truncated_tail_and_text_limits_are_atomic() {
        for text_len in [MAX_TEXT_BYTES, MAX_TEXT_BYTES + 1] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.jsonl");
            let output = dir.path().join("safe.jsonl");
            write_input(&input, &[meta(), message(&"x".repeat(text_len))]);
            if text_len == MAX_TEXT_BYTES {
                assert_eq!(import_codex(&input, &output).unwrap().records, 1);
            } else {
                assert!(import_codex(&input, &output)
                    .unwrap_err()
                    .to_string()
                    .contains("exceeds 256 KiB"));
                assert!(!output.exists());
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        write_input(&input, &[meta(), message("valid")]);
        fs::OpenOptions::new()
            .append(true)
            .open(&input)
            .unwrap()
            .write_all(b"{\"DO_NOT_EXPORT\":")
            .unwrap();
        let error = import_codex(&input, &output).unwrap_err().to_string();
        assert!(error.contains("line 3: invalid JSON"), "{error}");
        assert!(!error.contains("DO_NOT_EXPORT"));
        assert!(!output.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
