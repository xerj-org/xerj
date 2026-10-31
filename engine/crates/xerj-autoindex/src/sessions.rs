//! Explicit, offline session export before ordinary autoindex ingestion.
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use xerj_wasm::{builtins::PiiRedactionPlugin, ProcessAction, TransformPlugin};

const MAX_INPUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LINE_BYTES: u64 = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_RECORDS: usize = 100_000;

pub const HELP: &str = "xerj sessions import — offline session preparation for autoindex

USAGE: xerj sessions import --format <claude-code|codex> --input FILE --output NEW.jsonl

Read one explicit Claude Code transcript or Codex rollout JSONL; write user/assistant text
records through the existing native PII redactor before any output is stored.
Keep format-specific identity and source filename/line/byte offset metadata.
Claude Code preserves session/message/parent UUIDs and file paths explicitly
named by Read/Write/Edit/MultiEdit; Codex uses the archive event IDs below.
Skip tool results, tool inputs except file paths, thinking and metadata.
This exports all text-bearing branches in file order, not an active conversation
chain. Unsupported/malformed turns or repeated message UUIDs refuse the export.

The output is a new, private JSONL snapshot. Existing output is never replaced;
parse failures publish nothing. No node is started or contacted. Review the
export, then run: xerj autoindex <export-folder> --prefix <your-prefix>
The redactor handles email/IP/card/SSN/phone patterns, not every secret or name.
Codex reads response_item messages after session_meta, skips duplicate event_msg
text and tools/reasoning; uses line-and-byte-digest event IDs, no inferred parents
or files. exec --json and external history_base are unsupported.
Input <=512 MiB; line <=1 MiB; turn text <=256 KiB; <=100000 messages.
Exit 0: exported, JSON summary on stdout; 2: usage; 1: import/publication error.
";

#[derive(Debug, Serialize)]
pub struct ImportSummary {
    pub schema_version: u32,
    pub records: u64,
    pub source_lines: u64,
    pub skipped_records: u64,
    pub skipped_blocks: u64,
    pub source_sha256: String,
    pub output_sha256: String,
}

fn uuid_field(entry: &Value, field: &str, line: u64) -> Result<String> {
    let text = entry[field]
        .as_str()
        .filter(|s| s.len() == 36)
        .with_context(|| format!("line {line}: expected UUID in {field}"))?;
    uuid::Uuid::parse_str(text)
        .map(|id| id.to_string())
        .map_err(|_| anyhow::anyhow!("line {line}: invalid UUID in {field}"))
}

fn optional_text(entry: &Value, field: &str, line: u64) -> Result<Value> {
    match entry.get(field) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(s)) => Ok(json!(s)),
        _ => bail!("line {line}: expected text in {field}"),
    }
}

fn flag(entry: &Value, field: &str, line: u64) -> Result<bool> {
    match entry.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => bail!("line {line}: expected boolean in {field}"),
    }
}

fn turn_text(content: &Value, line: u64, skipped: &mut u64) -> Result<(String, Vec<String>)> {
    if let Some(text) = content.as_str() {
        ensure!(
            text.len() <= MAX_TEXT_BYTES,
            "line {line}: turn text exceeds 256 KiB"
        );
        return Ok((text.to_string(), Vec::new()));
    }
    let blocks = content
        .as_array()
        .with_context(|| format!("line {line}: unsupported message content"))?;
    let mut text = String::new();
    let mut files = Vec::new();
    for block in blocks {
        let kind = block["type"]
            .as_str()
            .with_context(|| format!("line {line}: missing content block type"))?;
        if kind == "text" {
            let part = block["text"]
                .as_str()
                .with_context(|| format!("line {line}: malformed text block"))?;
            let separator_bytes = usize::from(!text.is_empty());
            ensure!(
                text.len() + part.len() + separator_bytes <= MAX_TEXT_BYTES,
                "line {line}: turn text exceeds 256 KiB"
            );
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part);
        } else {
            ensure!(
                matches!(
                    kind,
                    "tool_use"
                        | "server_tool_use"
                        | "tool_result"
                        | "thinking"
                        | "redacted_thinking"
                        | "image"
                        | "document"
                        | "container_upload"
                ) || kind.ends_with("_tool_result"),
                "line {line}: unsupported content block type"
            );
            *skipped += 1;
            if kind == "tool_use"
                && matches!(
                    block["name"].as_str(),
                    Some("Read" | "Write" | "Edit" | "MultiEdit")
                )
            {
                if let Some(path) = block.pointer("/input/file_path") {
                    let path = path
                        .as_str()
                        .with_context(|| format!("line {line}: malformed tool file path"))?;
                    if !files.iter().any(|f| f == path) {
                        files.push(path.to_string());
                    }
                }
            }
        }
    }
    Ok((text, files))
}

fn unchanged(file: &File, before: &Metadata, input: &Path, read: u64) -> Result<()> {
    let after = file.metadata()?;
    let named = fs::metadata(input)?;
    ensure!(
        before.len() == read
            && before.len() == after.len()
            && before.len() == named.len()
            && before.modified()? == after.modified()?
            && before.modified()? == named.modified()?,
        "session input changed during import; use a stopped or archived transcript"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            before.dev() == named.dev() && before.ino() == named.ino(),
            "session input was replaced during import; use an archived transcript"
        );
    }
    Ok(())
}

fn publish(file: tempfile::NamedTempFile, output: &Path) -> Result<()> {
    file.persist_noclobber(output)
        .map_err(|_| anyhow::anyhow!("cannot publish session export: output exists or is not writable; choose a new output path"))?;
    Ok(())
}

struct ParsedTurn {
    text_fields: Value,
    identity_fields: Value,
}

fn claude_turn(
    entry: &Value,
    line: u64,
    offset: u64,
    source_file: &str,
    summary: &mut ImportSummary,
    session: &mut Option<String>,
    seen: &mut HashSet<String>,
) -> Result<Option<ParsedTurn>> {
    let kind = entry["type"]
        .as_str()
        .with_context(|| format!("line {line}: missing session record type"))?;
    if !matches!(kind, "user" | "assistant") {
        summary.skipped_records += 1;
        return Ok(None);
    }
    let sid = uuid_field(entry, "sessionId", line)?;
    if let Some(expected) = session.as_ref() {
        ensure!(
            expected == &sid,
            "line {line}: mixed session IDs are unsupported"
        );
    } else {
        *session = Some(sid.clone());
    }
    let mid = uuid_field(entry, "uuid", line)?;
    ensure!(
        seen.len() < MAX_RECORDS,
        "session exceeds 100000 message UUIDs"
    );
    ensure!(
        seen.insert(mid.clone()),
        "line {line}: repeated message UUID; export a transcript without revisions"
    );
    if flag(entry, "isMeta", line)? {
        summary.skipped_records += 1;
        return Ok(None);
    }
    let message = entry["message"]
        .as_object()
        .with_context(|| format!("line {line}: missing message object"))?;
    ensure!(
        message.get("role").is_none_or(|r| r.as_str() == Some(kind)),
        "line {line}: message role disagrees with record type"
    );
    let (body, files) = turn_text(
        message.get("content").unwrap_or(&Value::Null),
        line,
        &mut summary.skipped_blocks,
    )?;
    if body.trim().is_empty() {
        summary.skipped_records += 1;
        return Ok(None);
    }
    let parent_id = if entry["parentUuid"].is_null() {
        Value::Null
    } else {
        json!(uuid_field(entry, "parentUuid", line)?)
    };
    let timestamp = optional_text(entry, "timestamp", line)?;
    if let Some(text) = timestamp.as_str() {
        ensure!(
            chrono::DateTime::parse_from_rfc3339(text).is_ok(),
            "line {line}: invalid timestamp"
        );
    }
    let text_fields = json!({"body":body, "files":files, "source_file":source_file,
            "cwd":optional_text(entry,"cwd",line)?, "git_branch":optional_text(entry,"gitBranch",line)?});
    let mut record = json!({});
    record["id"] = json!(format!("{sid}/{mid}"));
    record["session_schema_version"] = json!(1);
    record["session_format"] = json!("claude-code");
    record["session_id"] = json!(sid);
    record["message_id"] = json!(mid);
    record["parent_id"] = parent_id;
    record["role"] = json!(kind);
    record["timestamp"] = timestamp;
    record["source_line"] = json!(line);
    record["source_byte_offset"] = json!(offset);
    record["is_sidechain"] = json!(flag(entry, "isSidechain", line)?);
    record["is_summary"] = json!(flag(entry, "isCompactSummary", line)?);
    Ok(Some(ParsedTurn {
        text_fields,
        identity_fields: record,
    }))
}

mod codex;

#[derive(Clone, Copy)]
enum SessionFormat {
    ClaudeCode,
    Codex,
}

pub fn import_claude_code(input: &Path, output: &Path) -> Result<ImportSummary> {
    import_session(input, output, SessionFormat::ClaudeCode)
}

pub fn import_codex(input: &Path, output: &Path) -> Result<ImportSummary> {
    import_session(input, output, SessionFormat::Codex)
}

fn import_session(input: &Path, output: &Path, format: SessionFormat) -> Result<ImportSummary> {
    match fs::symlink_metadata(output) {
        Ok(_) => bail!("session output already exists; choose a new output path"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("cannot inspect session output"),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Opening a FIFO must not block before we can reject non-files.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(input).context("cannot open session input")?;
    let before = file.metadata()?;
    ensure!(before.is_file(), "session input must be a regular file");
    ensure!(
        before.len() <= MAX_INPUT_BYTES,
        "session input exceeds 512 MiB; split or archive a smaller transcript"
    );
    let source_file = input
        .file_name()
        .and_then(|s| s.to_str())
        .context("session input filename must be UTF-8")?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staging = tempfile::Builder::new()
        .prefix(".xerj-session-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .context("cannot create private export staging file; output parent must exist")?;
    let redactor = PiiRedactionPlugin::new(Vec::new());
    let mut summary = ImportSummary {
        schema_version: 1,
        records: 0,
        source_lines: 0,
        skipped_records: 0,
        skipped_blocks: 0,
        source_sha256: String::new(),
        output_sha256: String::new(),
    };
    let mut source_hash = Sha256::new();
    let mut output_hash = Sha256::new();
    let mut reader = BufReader::new((&file).take(before.len() + 1));
    let mut bytes = Vec::new();
    let mut read = 0;
    let mut written = 0u64;
    let mut session: Option<String> = None;
    let mut seen = HashSet::new();
    let mut codex = codex::CodexState::default();
    loop {
        bytes.clear();
        let count = (&mut reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut bytes)?;
        if count == 0 {
            break;
        }
        summary.source_lines += 1;
        let line = summary.source_lines;
        let offset = read;
        read += count as u64;
        ensure!(
            count as u64 <= MAX_LINE_BYTES,
            "line {line}: session line exceeds 1 MiB"
        );
        source_hash.update(&bytes);
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry: Value = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow::anyhow!("line {line}: invalid JSON; no session export published")
        })?;
        let parsed = match format {
            SessionFormat::ClaudeCode => claude_turn(
                &entry,
                line,
                offset,
                source_file,
                &mut summary,
                &mut session,
                &mut seen,
            )?,
            SessionFormat::Codex => {
                codex.turn(&entry, line, offset, source_file, &bytes, &mut summary)?
            }
        };
        let Some(ParsedTurn {
            text_fields: mut record,
            identity_fields,
        }) = parsed
        else {
            continue;
        };
        ensure!(
            redactor.process(&mut record) == ProcessAction::Pass,
            "PII redaction refused session record"
        );
        // Only validated enums/IDs and numeric provenance bypass text redaction.
        record
            .as_object_mut()
            .unwrap()
            .extend(identity_fields.as_object().unwrap().clone());
        let kind = record["role"].as_str().unwrap();
        let title: String = record["body"]
            .as_str()
            .unwrap()
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        record["title"] = json!(format!("{kind}: {title}"));
        let mut encoded = serde_json::to_vec(&record)?;
        encoded.push(b'\n');
        written += encoded.len() as u64;
        ensure!(
            written <= MAX_INPUT_BYTES,
            "session export exceeds 512 MiB; use a smaller transcript"
        );
        staging.write_all(&encoded)?;
        output_hash.update(&encoded);
        summary.records += 1;
    }
    ensure!(
        summary.records > 0,
        "no supported text-bearing user/assistant turns; no export published"
    );
    unchanged(&file, &before, input, read)?;
    summary.source_sha256 = format!("{:x}", source_hash.finalize());
    summary.output_sha256 = format!("{:x}", output_hash.finalize());
    staging.as_file_mut().sync_all()?;
    publish(staging, output)?;
    Ok(summary)
}

pub fn run_cli(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return 0;
    }
    let parsed = (|| -> Result<(PathBuf, PathBuf, SessionFormat)> {
        ensure!(args[0] == "import", "expected sessions import");
        let mut input = None;
        let mut output = None;
        let mut format = None;
        let mut it = args[1..].iter();
        while let Some(arg) = it.next() {
            let slot = match arg.as_str() {
                "--input" => &mut input,
                "--output" => &mut output,
                "--format" => &mut format,
                _ => bail!("unknown session import option; see xerj sessions --help"),
            };
            ensure!(slot.is_none(), "duplicate session import option");
            let value = it
                .next()
                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                .context("session import option requires a value")?;
            *slot = Some(value.clone());
        }
        let format = match format.as_deref() {
            Some("claude-code") => SessionFormat::ClaudeCode,
            Some("codex") => SessionFormat::Codex,
            _ => bail!("--format claude-code or --format codex is required"),
        };
        Ok((
            PathBuf::from(input.context("--input is required")?),
            PathBuf::from(output.context("--output is required")?),
            format,
        ))
    })();
    let (input, output, format) = match parsed {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    match import_session(&input, &output, format) {
        Ok(summary) => {
            let mut stdout = std::io::stdout().lock();
            if serde_json::to_writer(&mut stdout, &summary).is_err() || writeln!(stdout).is_err() {
                eprintln!("error: export was published, but its summary could not be written");
                return 1;
            }
            0
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const SESSION: &str = "550e8400-e29b-41d4-a716-446655440000";
    const USER: &str = "550e8400-e29b-41d4-a716-446655440001";
    const ASSISTANT: &str = "550e8400-e29b-41d4-a716-446655440002";

    fn message(role: &str, uuid: &str, content: Value) -> Value {
        json!({"type":role, "uuid":uuid, "sessionId":SESSION,
               "timestamp":"2026-10-25T00:00:00Z",
               "message":{"role":role,"content":content}})
    }

    pub(super) fn write_input(path: &Path, entries: &[Value]) {
        let mut text = entries
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn sessions_decision_text_is_redacted_and_points_to_the_original_turn() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("conversation.jsonl");
        let output = dir.path().join("safe.jsonl");
        let mut decision = message(
            "assistant",
            ASSISTANT,
            json!([
                {"type":"thinking","thinking":"NEVER_EXPORT_THOUGHTS"},
                {"type":"text","text":"We dropped polling because file events preserve recovery. Contact owner@example.com."},
                {"type":"tool_use","name":"Edit","input":{"file_path":"src/watch.rs","old_string":"NEVER_EXPORT_INPUT"}}
            ]),
        );
        decision["parentUuid"] = json!(USER);
        write_input(
            &input,
            &[
                json!({"type":"file-history-snapshot","payload":"NEVER_EXPORT_METADATA"}),
                message("user", USER, json!("Why did we drop polling?")),
                decision,
            ],
        );
        let summary = import_claude_code(&input, &output).unwrap();
        assert_eq!(summary.records, 2);
        let text = std::fs::read_to_string(&output).unwrap();
        assert!(text.contains("[REDACTED_EMAIL]"));
        for secret in [
            "owner@example.com",
            "NEVER_EXPORT_THOUGHTS",
            "NEVER_EXPORT_INPUT",
            "NEVER_EXPORT_METADATA",
        ] {
            assert!(!text.contains(secret), "export leaked {secret}");
        }
        let rows: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows[1]["source_line"], 3);
        assert_eq!(rows[1]["message_id"], ASSISTANT);
        assert_eq!(rows[1]["parent_id"], USER);
        assert_eq!(rows[1]["files"], json!(["src/watch.rs"]));
        assert_eq!(
            rows[0].as_object().unwrap().keys().collect::<Vec<_>>(),
            rows[1].as_object().unwrap().keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn sessions_a_malformed_later_line_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        write_input(&input, &[message("user", USER, json!("valid text"))]);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&input)
            .unwrap()
            .write_all(b"{\"secret\":\"NEVER_ECHO_THIS\"")
            .unwrap();
        let error = import_claude_code(&input, &output).unwrap_err().to_string();
        assert!(error.contains("line 2"), "{error}");
        assert!(!error.contains("NEVER_ECHO_THIS"));
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn sessions_existing_output_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        write_input(&input, &[message("user", USER, json!("hello"))]);
        std::fs::write(&output, "existing").unwrap();
        assert!(import_claude_code(&input, &output).is_err());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "existing");
    }

    #[test]
    fn sessions_redacts_the_five_native_pii_types_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("owner@example.com.jsonl");
        let output = dir.path().join("safe.jsonl");
        let mut entry = message("user", USER, json!("Email: owner@example.com. IP: 192.0.2.55. Card: 4111-1111-1111-1111. SSN: 123-45-6789. Phone: 202-555-0147."));
        entry["cwd"] = json!("/home/owner@example.com/work");
        entry["gitBranch"] = json!("contact/owner@example.com");
        write_input(&input, &[entry]);
        let summary = import_claude_code(&input, &output).unwrap();
        let bytes = std::fs::read(&output).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        for marker in ["EMAIL", "IP", "CC", "SSN", "PHONE"] {
            assert!(text.contains(&format!("[REDACTED_{marker}]")), "{text}");
        }
        assert!(!text.contains("owner@example.com"));
        let row: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(row["session_id"], SESSION);
        assert_eq!(row["message_id"], USER);
        assert_eq!(
            summary.source_sha256,
            format!("{:x}", Sha256::digest(std::fs::read(input).unwrap()))
        );
        assert_eq!(
            summary.output_sha256,
            format!("{:x}", Sha256::digest(bytes))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn sessions_duplicate_mixed_or_unknown_turns_fail_without_partial_output() {
        let first = message("user", USER, json!("first"));
        let second = message("assistant", ASSISTANT, json!("second"));
        let mut mixed = second.clone();
        mixed["sessionId"] = json!("550e8400-e29b-41d4-a716-446655440099");
        let mut unknown = second.clone();
        unknown["message"]["content"] = json!([{"type":"future_text","text":"unsupported"}]);
        let mut bad_role = second.clone();
        bad_role["message"]["role"] = json!("user");
        for (entry, needle) in [
            (first.clone(), "repeated message UUID"),
            (mixed, "mixed session IDs"),
            (unknown, "unsupported content block"),
            (bad_role, "role disagrees"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.jsonl");
            let output = dir.path().join("safe.jsonl");
            write_input(&input, &[first.clone(), entry]);
            let error = import_claude_code(&input, &output).unwrap_err().to_string();
            assert!(error.contains(needle), "{error}");
            assert!(!output.exists());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
            // Recovery uses the same never-published output after replacing
            // the synthetic malformed input, not weakening a parser guard.
            write_input(&input, &[first.clone(), second.clone()]);
            assert_eq!(import_claude_code(&input, &output).unwrap().records, 2);
        }
    }

    #[test]
    fn sessions_meta_and_tool_only_messages_do_not_become_searchable_text() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        let mut meta = message("assistant", ASSISTANT, json!("hidden metadata text"));
        meta["isMeta"] = json!(true);
        write_input(
            &input,
            &[
                message(
                    "user",
                    USER,
                    json!([{"type":"tool_result","content":"raw result"}]),
                ),
                meta,
            ],
        );
        assert!(import_claude_code(&input, &output)
            .unwrap_err()
            .to_string()
            .contains("no supported text-bearing"));
        assert!(!output.exists());
    }

    #[test]
    fn sessions_preserves_branch_flags_and_ids_without_choosing_a_live_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        let output = dir.path().join("safe.jsonl");
        let mut entry = message("assistant", ASSISTANT, json!("discarded alternative"));
        entry["isSidechain"] = json!(true);
        entry["isCompactSummary"] = json!(true);
        entry["parentUuid"] = json!(USER);
        write_input(&input, &[entry]);
        import_claude_code(&input, &output).unwrap();
        let row: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(row["is_sidechain"], true);
        assert_eq!(row["is_summary"], true);
        assert_eq!(row["parent_id"], USER);
    }

    #[test]
    fn sessions_input_change_or_output_creation_cannot_publish_over_existing_state() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.jsonl");
        std::fs::write(&input, "before").unwrap();
        let opened = File::open(&input).unwrap();
        let before = opened.metadata().unwrap();
        std::fs::write(&input, "a different length").unwrap();
        assert!(unchanged(&opened, &before, &input, before.len())
            .unwrap_err()
            .to_string()
            .contains("changed during import"));
        let output = dir.path().join("safe.jsonl");
        let mut staging = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        staging.write_all(b"safe new text").unwrap();
        // Simulate another writer winning after preflight, before publication.
        std::fs::write(&output, "winner").unwrap();
        assert!(publish(staging, &output).is_err());
        assert_eq!(std::fs::read_to_string(output).unwrap(), "winner");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn sessions_limits_refuse_instead_of_truncating_or_falling_back_to_raw_text() {
        for payload in [
            vec![b'x'; MAX_LINE_BYTES as usize + 1],
            message("user", USER, json!("x".repeat(MAX_TEXT_BYTES + 1)))
                .to_string()
                .into_bytes(),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.jsonl");
            let output = dir.path().join("safe.jsonl");
            std::fs::write(&input, payload).unwrap();
            let error = import_claude_code(&input, &output).unwrap_err().to_string();
            assert!(error.contains("exceeds"), "{error}");
            assert!(!output.exists());
        }
    }

    #[test]
    fn sessions_text_limit_is_inclusive_for_strings_and_block_arrays() {
        for content in [
            json!("x".repeat(MAX_TEXT_BYTES)),
            json!([
                {"type":"text","text":"x".repeat(MAX_TEXT_BYTES)}
            ]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.jsonl");
            let output = dir.path().join("safe.jsonl");
            write_input(&input, &[message("user", USER, content)]);
            assert_eq!(import_claude_code(&input, &output).unwrap().records, 1);
            let row: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
            assert_eq!(row["body"].as_str().unwrap().len(), MAX_TEXT_BYTES);
        }
    }

    #[cfg(unix)]
    #[test]
    fn sessions_named_pipe_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("pipe");
        let output = dir.path().join("safe.jsonl");
        let name = std::ffi::CString::new(input.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is a valid NUL-terminated temporary path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let error = import_claude_code(&input, &output).unwrap_err().to_string();
        assert!(error.contains("regular file"), "{error}");
        assert!(!output.exists());
    }
}
