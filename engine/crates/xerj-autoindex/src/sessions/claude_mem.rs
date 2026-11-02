//! Read-only claude-mem SQLite subset. Reference and licence are pinned in the import guide.
use super::*;
use rusqlite::{types::ValueRef, Connection, OpenFlags, Row};

/// One exported claude-mem table: which columns become text, which stay optional.
struct Table {
    name: &'static str,
    role: &'static str,
    /// Columns every supported claude-mem schema has.
    required: &'static [&'static str],
    /// Columns added by later claude-mem migrations; read as NULL when absent.
    optional: &'static [&'static str],
    /// Joins `sdk_sessions` on this column (unique there) for the platform session ID.
    session: Option<&'static str>,
}

const PROMPTS: Table = Table {
    name: "user_prompts",
    role: "user",
    required: &["id", "prompt_text", "created_at_epoch"],
    optional: &["prompt_number", "content_session_id"],
    // The row carries content_session_id itself; sdk_sessions repeats it once per platform.
    session: None,
};

const OBSERVATIONS: Table = Table {
    name: "observations",
    role: "observation",
    required: &[
        "id",
        "memory_session_id",
        "project",
        "type",
        "created_at_epoch",
    ],
    optional: &[
        "prompt_number",
        "title",
        "subtitle",
        "narrative",
        "text",
        "facts",
        "concepts",
        "files_read",
        "files_modified",
    ],
    session: Some("memory_session_id"),
};

const SUMMARIES: Table = Table {
    name: "session_summaries",
    role: "summary",
    required: &["id", "memory_session_id", "project", "created_at_epoch"],
    optional: &[
        "prompt_number",
        "request",
        "investigated",
        "learned",
        "completed",
        "next_steps",
        "notes",
        "files_read",
        "files_edited",
    ],
    session: Some("memory_session_id"),
};

const SUMMARY_SECTIONS: [(&str, &str); 6] = [
    ("request", "Request"),
    ("investigated", "Investigated"),
    ("learned", "Learned"),
    ("completed", "Completed"),
    ("next_steps", "Next steps"),
    ("notes", "Notes"),
];

fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let names = stmt
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(names)
}

/// A row as JSON: NULL, integer and text only; anything else refuses the export.
fn row_json(row: &Row, names: &[&str], at: &str) -> Result<serde_json::Map<String, Value>> {
    let mut out = serde_json::Map::new();
    for (i, name) in names.iter().enumerate() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => json!(n),
            ValueRef::Text(t) => json!(std::str::from_utf8(t)
                .map_err(|_| anyhow::anyhow!("{at}: {name} is not UTF-8 text"))?),
            _ => bail!("{at}: unsupported value type in {name}"),
        };
        out.insert((*name).to_string(), value);
    }
    Ok(out)
}

fn text<'a>(row: &'a serde_json::Map<String, Value>, field: &str, at: &str) -> Result<&'a str> {
    match &row[field] {
        Value::Null => Ok(""),
        Value::String(s) => Ok(s.trim()),
        _ => bail!("{at}: expected text in {field}"),
    }
}

/// claude-mem stores `facts`, `concepts` and `files_*` as JSON.stringify'd string arrays.
fn string_array(
    row: &serde_json::Map<String, Value>,
    field: &str,
    at: &str,
) -> Result<Vec<String>> {
    let raw = text(row, field, at)?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw)
        .map_err(|_| anyhow::anyhow!("{at}: {field} is not a JSON string array"))
}

fn push_line(body: &mut String, line: &str) {
    if line.is_empty() {
        return;
    }
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(line);
}

/// Text body and file paths for one row, in the order claude-mem renders them.
fn body_and_files(
    table: &Table,
    row: &serde_json::Map<String, Value>,
    at: &str,
) -> Result<(String, Vec<String>)> {
    let mut body = String::new();
    let mut files = Vec::new();
    match table.role {
        "user" => push_line(&mut body, text(row, "prompt_text", at)?),
        "observation" => {
            for field in ["title", "subtitle", "narrative", "text"] {
                push_line(&mut body, text(row, field, at)?);
            }
            for fact in string_array(row, "facts", at)? {
                push_line(&mut body, &format!("- {}", fact.trim()));
            }
            files.extend(string_array(row, "files_modified", at)?);
            files.extend(string_array(row, "files_read", at)?);
        }
        _ => {
            for (field, label) in SUMMARY_SECTIONS {
                let value = text(row, field, at)?;
                if !value.is_empty() {
                    push_line(&mut body, &format!("{label}: {value}"));
                }
            }
            files.extend(string_array(row, "files_edited", at)?);
            files.extend(string_array(row, "files_read", at)?);
        }
    }
    ensure!(body.len() <= MAX_TEXT_BYTES, "{at}: text exceeds 256 KiB");
    let mut unique = HashSet::new();
    files.retain(|f| unique.insert(f.clone()));
    Ok((body, files))
}

fn timestamp(row: &serde_json::Map<String, Value>, at: &str) -> Result<Value> {
    let millis = row["created_at_epoch"]
        .as_i64()
        .with_context(|| format!("{at}: expected integer created_at_epoch"))?;
    let time = chrono::DateTime::from_timestamp_millis(millis)
        .with_context(|| format!("{at}: created_at_epoch out of range"))?;
    Ok(json!(
        time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    ))
}

fn export_table(
    conn: &Connection,
    table: &Table,
    sessions: bool,
    source_file: &str,
    summary: &mut ImportSummary,
    source_hash: &mut Sha256,
    export: &mut Export,
) -> Result<()> {
    let present = columns(conn, table.name)?;
    if present.is_empty() {
        return Ok(());
    }
    ensure!(
        !present.contains("sdk_session_id"),
        "claude-mem table {} predates claude-mem's session ID rename; start claude-mem once to \
         migrate the store, then rerun",
        table.name
    );
    for name in table.required {
        ensure!(
            present.contains(*name),
            "claude-mem table {} has no {name} column; unsupported claude-mem version",
            table.name
        );
    }
    let names: Vec<&str> = table
        .required
        .iter()
        .chain(table.optional)
        .copied()
        .collect();
    // Column and table names come only from the allowlists above.
    let mut select: Vec<String> = names
        .iter()
        .map(|c| {
            if present.contains(*c) {
                format!("t.{c}")
            } else {
                format!("NULL AS {c}")
            }
        })
        .collect();
    let join = if let Some(key) = table.session.filter(|_| sessions) {
        select.push("s.content_session_id".to_string());
        format!("LEFT JOIN sdk_sessions s ON s.{key} = t.{key}")
    } else {
        select.push("NULL".to_string());
        String::new()
    };
    let sql = format!(
        "SELECT {} FROM {} t {join} ORDER BY t.created_at_epoch, t.id",
        select.join(", "),
        table.name
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut all = names.clone();
    all.push("session");
    while let Some(row) = rows.next()? {
        summary.source_lines += 1;
        ensure!(
            summary.source_lines <= MAX_RECORDS as u64,
            "claude-mem store exceeds 100000 rows; export a smaller copy"
        );
        let at = format!("{} row {}", table.name, row.get::<_, i64>(0)?);
        let row = row_json(row, &all, &at)?;
        source_hash.update(table.name.as_bytes());
        source_hash.update(serde_json::to_vec(&row)?);
        let (body, files) = body_and_files(table, &row, &at)?;
        if body.is_empty() {
            summary.skipped_records += 1;
            continue;
        }
        let rowid = row["id"].as_i64().unwrap();
        let mut text_fields = json!({"body": body, "files": files, "source_file": source_file});
        for field in ["project", "memory_session_id"] {
            if let Some(value) = row.get(field).filter(|v| !v.is_null()) {
                text_fields[field] = value.clone();
            }
        }
        if table.role == "observation" {
            text_fields["observation_type"] = row["type"].clone();
            text_fields["concepts"] = json!(string_array(&row, "concepts", &at)?);
        }
        let mut record = json!({});
        record["id"] = json!(format!("claude-mem/{}/{rowid}", table.role));
        record["session_schema_version"] = json!(1);
        record["session_format"] = json!("claude-mem");
        // claude-mem keys sessions by its own ID; the platform's session ID is joined in.
        let session = row["session"]
            .as_str()
            .or_else(|| row.get("content_session_id").and_then(Value::as_str));
        match session.map(|s| (s, uuid::Uuid::parse_str(s))) {
            Some((s, Ok(id))) if s.len() == 36 => record["session_id"] = json!(id.to_string()),
            // Not a UUID (another claude-mem platform): keep it, but as redacted text.
            Some((s, _)) => text_fields["session_id"] = json!(s),
            None => record["session_id"] = Value::Null,
        }
        record["message_id"] = json!(format!("{}/{rowid}", table.role));
        record["parent_id"] = Value::Null;
        record["role"] = json!(table.role);
        record["timestamp"] = timestamp(&row, &at)?;
        record["source_table"] = json!(table.name);
        record["source_line"] = json!(rowid);
        record["prompt_number"] = row
            .get("prompt_number")
            .filter(|v| v.is_i64())
            .cloned()
            .unwrap_or(Value::Null);
        // Observations and summaries are claude-mem's model-written condensations.
        record["is_summary"] = json!(table.role != "user");
        export.push(
            ParsedTurn {
                text_fields,
                identity_fields: record,
            },
            summary,
        )?;
    }
    Ok(())
}

pub(super) fn import(input: &Path, output: &Path) -> Result<ImportSummary> {
    ensure_new_output(output)?;
    let meta = fs::metadata(input).context("cannot open claude-mem database")?;
    ensure!(meta.is_file(), "claude-mem input must be a regular file");
    // Rows not yet checkpointed live in the WAL, so it counts toward the input cap.
    // SQLite names the WAL after the symlink target, so resolve the input first.
    let mut wal = fs::canonicalize(input)
        .context("cannot open claude-mem database")?
        .into_os_string();
    wal.push("-wal");
    let wal_len = fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    ensure!(
        meta.len() + wal_len <= MAX_INPUT_BYTES,
        "claude-mem database exceeds 512 MiB; export a smaller copy"
    );
    let source_file = input
        .file_name()
        .and_then(|s| s.to_str())
        .context("claude-mem input filename must be UTF-8")?;
    // Read-only but not immutable: a running claude-mem worker keeps recent rows in the WAL.
    let conn = Connection::open_with_flags(
        input,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("cannot open claude-mem database read-only")?;
    conn.execute_batch("PRAGMA query_only = 1; BEGIN")
        .context("cannot read claude-mem database")?;
    let tables = columns(&conn, "observations")
        .context("cannot read the input as a SQLite database; pass ~/.claude-mem/claude-mem.db")?;
    ensure!(
        !tables.is_empty(),
        "not a claude-mem database: no observations table"
    );
    let sessions = !columns(&conn, "sdk_sessions")?.is_empty();
    let mut export = Export::create(output)?;
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
    // One read transaction: all three tables come from the same snapshot.
    for table in [&PROMPTS, &OBSERVATIONS, &SUMMARIES] {
        export_table(
            &conn,
            table,
            sessions,
            source_file,
            &mut summary,
            &mut source_hash,
            &mut export,
        )?;
    }
    conn.execute_batch("COMMIT")?;
    ensure!(
        summary.records > 0,
        "no text-bearing claude-mem prompts, observations or summaries; no export published"
    );
    summary.source_sha256 = format!("{:x}", source_hash.finalize());
    export.publish(output, &mut summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> PathBuf {
        let path = dir.join("claude-mem.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(include_str!("../../tests/fixtures/claude-mem.sql"))
            .unwrap();
        path
    }

    fn rows(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    #[test]
    fn claude_mem_prompts_observations_and_summaries_export_redacted_with_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let input = store(dir.path());
        let original = fs::read(&input).unwrap();
        let output = dir.path().join("safe.jsonl");
        let summary = import_claude_mem(&input, &output).unwrap();
        assert_eq!((summary.records, summary.source_lines), (3, 3));
        let safe = fs::read_to_string(&output).unwrap();
        assert!(!safe.contains("DO_NOT_EXPORT"));
        assert!(!safe.contains("owner@example.com"));
        assert!(safe.contains("[REDACTED_EMAIL]"));
        let rows = rows(&output);
        let roles: Vec<_> = rows.iter().map(|r| r["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "observation", "summary"]);
        for row in &rows {
            assert_eq!(row["session_format"], "claude-mem");
            assert_eq!(row["session_id"], "550e8400-e29b-41d4-a716-446655440000");
        }
        let observation = &rows[1];
        assert_eq!(observation["id"], "claude-mem/observation/1");
        assert_eq!(
            observation["title"],
            "observation: Dropped polling for file events"
        );
        assert!(observation["body"]
            .as_str()
            .unwrap()
            .contains("- The durable journal preserves recovery"));
        assert_eq!(observation["files"], json!(["src/watch.rs", "src/poll.rs"]));
        assert_eq!(observation["observation_type"], "decision");
        assert_eq!(observation["concepts"], json!(["watcher", "recovery"]));
        assert_eq!(observation["source_table"], "observations");
        assert_eq!(observation["source_line"], 1);
        assert_eq!(observation["timestamp"], "2026-10-25T00:00:01.000Z");
        assert_eq!(
            [
                &rows[0]["is_summary"],
                &rows[1]["is_summary"],
                &rows[2]["is_summary"]
            ],
            [false, true, true]
        );
        assert!(rows[2]["body"]
            .as_str()
            .unwrap()
            .starts_with("Request: Explain why polling was dropped\nInvestigated:"));
        assert_eq!(rows[2]["files"], json!(["src/watch.rs", "src/poll.rs"]));
        // Same snapshot, same bytes; the store itself is never written.
        let again = dir.path().join("again.jsonl");
        let second = import_claude_mem(&input, &again).unwrap();
        assert_eq!(fs::read_to_string(&again).unwrap(), safe);
        assert_eq!(second.source_sha256, summary.source_sha256);
        assert_eq!(fs::read(&input).unwrap(), original);
        assert!(import_claude_mem(&input, &output).is_err());
        assert_eq!(fs::read_to_string(&output).unwrap(), safe);
    }

    #[test]
    fn claude_mem_rows_still_in_a_live_workers_wal_are_exported() {
        let dir = tempfile::tempdir().unwrap();
        let input = store(dir.path());
        // A running worker: WAL mode, no checkpoint, connection held open.
        let worker = Connection::open(&input).unwrap();
        worker
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                 INSERT INTO observations (memory_session_id, project, type, created_at,
                   created_at_epoch, title) VALUES ('mem-0001', 'watcher', 'discovery',
                   '2026-10-25T00:00:03.000Z', 1792886403000, 'Written after the snapshot');",
            )
            .unwrap();
        assert!(dir.path().join("claude-mem.db-wal").exists());
        let output = dir.path().join("live.jsonl");
        let summary = import_claude_mem(&input, &output).unwrap();
        assert_eq!(summary.records, 4);
        assert!(fs::read_to_string(&output)
            .unwrap()
            .contains("Written after the snapshot"));
        drop(worker);
    }

    #[test]
    fn claude_mem_other_platform_sessions_keep_their_id_and_prompts_export_once() {
        let dir = tempfile::tempdir().unwrap();
        let input = store(dir.path());
        // sdk_sessions repeats a content_session_id once per platform, and a
        // non-Claude platform's session ID is not a UUID.
        Connection::open(&input)
            .unwrap()
            .execute_batch(
                "INSERT INTO sdk_sessions VALUES (2, '550e8400-e29b-41d4-a716-446655440000',
                   'mem-0002', 'watcher', 'codex', NULL, '2026-10-25T00:00:00.000Z',
                   1792886400000, 'completed', NULL, NULL);
                 INSERT INTO sdk_sessions VALUES (3, 'rollout-abc', 'mem-0003', 'watcher',
                   'codex', NULL, '2026-10-25T00:00:00.000Z', 1792886400000, 'completed',
                   NULL, NULL);
                 UPDATE observations SET memory_session_id = 'mem-0003';",
            )
            .unwrap();
        let output = dir.path().join("safe.jsonl");
        let summary = import_claude_mem(&input, &output).unwrap();
        assert_eq!((summary.records, summary.source_lines), (3, 3));
        let rows = rows(&output);
        let ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            [
                "claude-mem/user/1",
                "claude-mem/observation/1",
                "claude-mem/summary/1"
            ]
        );
        assert_eq!(
            rows[0]["session_id"],
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(rows[1]["session_id"], "rollout-abc");
    }

    #[test]
    fn claude_mem_older_schema_without_migrated_columns_still_exports() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("claude-mem.db");
        Connection::open(&input)
            .unwrap()
            .execute_batch(
                "CREATE TABLE observations (id INTEGER PRIMARY KEY, memory_session_id TEXT,
                   project TEXT, text TEXT, type TEXT, created_at TEXT, created_at_epoch INTEGER);
                 INSERT INTO observations VALUES (7, 'mem-1', 'p', 'Plain v1 text', 'change',
                   '2026-10-25T00:00:00.000Z', 1792886400000);",
            )
            .unwrap();
        let output = dir.path().join("safe.jsonl");
        let summary = import_claude_mem(&input, &output).unwrap();
        assert_eq!(summary.records, 1);
        let rows = rows(&output);
        assert_eq!(rows[0]["body"], "Plain v1 text");
        assert_eq!(rows[0]["session_id"], Value::Null);
        assert_eq!(rows[0]["files"], json!([]));
    }

    #[test]
    fn claude_mem_wrong_or_malformed_stores_publish_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("safe.jsonl");
        let not_sqlite = dir.path().join("notes.db");
        fs::write(&not_sqlite, "not a database").unwrap();
        let other = dir.path().join("other.db");
        Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        let pre_rename = dir.path().join("old.db");
        Connection::open(&pre_rename)
            .unwrap()
            .execute_batch(
                "CREATE TABLE observations (id INTEGER PRIMARY KEY, sdk_session_id TEXT,
                   project TEXT, text TEXT, type TEXT, created_at_epoch INTEGER);",
            )
            .unwrap();
        let malformed = store(dir.path());
        Connection::open(&malformed)
            .unwrap()
            .execute_batch("UPDATE observations SET facts = 'not json'")
            .unwrap();
        for (input, reason) in [
            (
                &not_sqlite,
                "cannot read the input as a SQLite database; pass ~/.claude-mem/claude-mem.db: file is not a database",
            ),
            (&pre_rename, "predates claude-mem's session ID rename"),
            (&other, "no observations table"),
            (
                &malformed,
                "observations row 1: facts is not a JSON string array",
            ),
        ] {
            let error = format!("{:#}", import_claude_mem(input, &output).unwrap_err());
            assert!(error.contains(reason), "{error}");
            assert!(!output.exists());
        }
        assert_eq!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_mem_input_cap_counts_the_wal_even_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let input = store(dir.path());
        // Sparse, so the test writes no real data.
        fs::File::create(dir.path().join("claude-mem.db-wal"))
            .unwrap()
            .set_len(MAX_INPUT_BYTES)
            .unwrap();
        let link = dir.path().join("link.db");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        let output = dir.path().join("safe.jsonl");
        for path in [&input, &link] {
            let error = format!("{:#}", import_claude_mem(path, &output).unwrap_err());
            assert!(error.contains("exceeds 512 MiB"), "{error}");
            assert!(!output.exists());
        }
    }

    #[test]
    fn claude_mem_is_a_cli_format() {
        let dir = tempfile::tempdir().unwrap();
        let input = store(dir.path());
        let output = dir.path().join("safe.jsonl");
        let args: Vec<String> = ["import", "--format", "claude-mem", "--input"]
            .iter()
            .map(|s| s.to_string())
            .chain([input.display().to_string(), "--output".into()])
            .chain([output.display().to_string()])
            .collect();
        assert_eq!(run_cli(&args), 0);
        assert_eq!(rows(&output).len(), 3);
    }
}
