//! Read-only MemPalace sqlite_exact subset. Reference and licence are pinned in the import guide.
use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// The sqlite_exact backend's file inside a palace directory.
const STORE: &str = "sqlite_exact.sqlite3";
/// MemPalace's default backend; its store is not read by this importer.
const CHROMA_STORE: &str = "chroma.sqlite3";
/// The collection holding verbatim drawers; closets and other collections are skipped.
const DRAWERS: &str = "mempalace_drawers";
/// Metadata flag on a record whose strings MemPalace stored escaped.
const ESCAPED: &str = "_mempalace_escaped";
const ESCAPE_PREFIX: &str = "\u{1}mpesc:";
/// Drawer metadata kept as redacted text when it is a string.
const TEXT_METADATA: [&str; 5] = ["wing", "room", "hall", "added_by", "content_date"];
/// Drawer metadata kept when it is an integer.
const NUMERIC_METADATA: [&str; 3] = ["chunk_index", "line_start", "line_end"];

/// Undo MemPalace's stored-text escape (`\\`, `\0`, `\uXXXX` after the prefix).
/// NUL becomes a space and an unpaired surrogate U+FFFD, as MemPalace's own search
/// index and MCP output render them; other text is returned as written.
fn unescape(value: &str) -> String {
    let Some(rest) = value.strip_prefix(ESCAPE_PREFIX) else {
        return value.to_string();
    };
    let mut out = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut ahead = chars.clone();
        match ahead.next() {
            Some('\\') => out.push('\\'),
            Some('0') => out.push(' '),
            Some('u') => {
                let hex: String = ahead.by_ref().take(4).collect();
                let valid = hex.len() == 4
                    && hex
                        .chars()
                        .all(|h| h.is_ascii_digit() || ('A'..='F').contains(&h));
                if !valid {
                    out.push('\\');
                    continue;
                }
                let code = u32::from_str_radix(&hex, 16).unwrap();
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
            }
            _ => {
                out.push('\\');
                continue;
            }
        }
        chars = ahead;
    }
    out
}

/// The values MemPalace's own reader treats as a set flag: 1, true, "1", "true", "True".
fn flagged(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => matches!(s.as_str(), "1" | "true" | "True"),
        _ => false,
    }
}

/// RFC 3339 instant normalised like the other formats; anything else is unknown.
fn timestamp(candidates: [Option<&str>; 3]) -> Value {
    candidates
        .into_iter()
        .flatten()
        .find_map(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| {
            json!(t
                .with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        })
        .unwrap_or(Value::Null)
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut path = db.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

/// What a writer starting mid-import changes: each file's presence, size and mtime.
type Signature = Vec<Option<(u64, Option<std::time::SystemTime>)>>;

fn signature(db: &Path) -> Signature {
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            fs::metadata(sidecar(db, suffix))
                .ok()
                .map(|m| (m.len(), m.modified().ok()))
        })
        .collect()
}

/// The store file for a palace directory or an explicit store path, and the
/// canonical path SQLite will open (it names sidecars after the symlink target).
fn store_path(input: &Path) -> Result<(PathBuf, PathBuf)> {
    let meta = fs::metadata(input).context("cannot open MemPalace palace or store")?;
    let chroma = |dir: &Path| dir.join(CHROMA_STORE).is_file();
    let store = if meta.is_dir() {
        let store = input.join(STORE);
        ensure!(
            store.is_file() || !chroma(input),
            "this palace uses MemPalace's default Chroma backend; only the sqlite_exact \
             backend is supported"
        );
        ensure!(
            !(store.is_file() && chroma(input)),
            "this palace holds both a Chroma and a sqlite_exact store, which MemPalace \
             itself refuses to open; pass the store file to read it anyway"
        );
        ensure!(
            store.is_file(),
            "no {STORE} in the palace directory; pass a MemPalace palace (~/.mempalace/palace)"
        );
        store
    } else {
        ensure!(
            meta.is_file(),
            "MemPalace input must be a directory or a regular file"
        );
        ensure!(
            input.file_name().and_then(|n| n.to_str()) != Some(CHROMA_STORE),
            "this is MemPalace's Chroma store; only the sqlite_exact backend is supported"
        );
        input.to_path_buf()
    };
    let canonical = fs::canonicalize(&store).context("cannot open MemPalace store")?;
    Ok((store, canonical))
}

/// Read-only without creating sidecars on a clean store, as MemPalace's own reader does.
/// Returns whether the store was opened immutable.
fn open(db: &Path) -> Result<(Connection, bool)> {
    let (wal, shm) = (sidecar(db, "-wal").is_file(), sidecar(db, "-shm").is_file());
    ensure!(
        wal == shm,
        "MemPalace store has an incomplete WAL sidecar set; let its writer exit cleanly, then rerun"
    );
    let len = fs::metadata(db)?.len()
        + fs::metadata(sidecar(db, "-wal"))
            .map(|m| m.len())
            .unwrap_or(0);
    ensure!(
        len <= MAX_INPUT_BYTES,
        "MemPalace store exceeds 512 MiB; export a smaller copy"
    );
    // A live writer's rows are in the WAL, which immutable mode would skip.
    let mode = if wal {
        "mode=ro"
    } else {
        "mode=ro&immutable=1"
    };
    let path = db.to_str().context("MemPalace store path must be UTF-8")?;
    let uri = format!(
        "file:{}?{mode}",
        path.replace('%', "%25")
            .replace('?', "%3f")
            .replace('#', "%23")
    );
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("cannot open MemPalace store read-only")?;
    conn.execute_batch("PRAGMA query_only = 1; BEGIN")
        .context("cannot read MemPalace store")?;
    Ok((conn, !wal))
}

pub(super) fn import(input: &Path, output: &Path) -> Result<ImportSummary> {
    ensure_new_output(output)?;
    let (store, db) = store_path(input)?;
    let before = signature(&db);
    let (conn, immutable) = open(&db)?;
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
             AND name IN ('collections', 'documents')",
            [],
            |row| row.get(0),
        )
        .context("cannot read the input as a SQLite database; pass ~/.mempalace/palace")?;
    ensure!(
        tables == 2,
        "not a MemPalace sqlite_exact store: no collections and documents tables"
    );
    let collection: Option<i64> = conn
        .query_row(
            "SELECT id FROM collections WHERE name = ?1",
            [DRAWERS],
            |row| row.get(0),
        )
        .optional()?;
    let collection =
        collection.with_context(|| format!("MemPalace store has no {DRAWERS} collection"))?;
    let source_file = store
        .file_name()
        .and_then(|s| s.to_str())
        .context("MemPalace store filename must be UTF-8")?;
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
    let mut stmt = conn.prepare(
        "SELECT rowid, id, document, metadata_json, created_at FROM documents \
         WHERE collection_id = ?1 ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([collection])?;
    while let Some(row) = rows.next()? {
        summary.source_lines += 1;
        ensure!(
            summary.source_lines <= MAX_RECORDS as u64,
            "MemPalace store exceeds 100000 drawers; export a smaller copy"
        );
        let rowid: i64 = row.get(0)?;
        let at = format!("documents row {rowid}");
        let read = |i| {
            row.get::<_, String>(i)
                .with_context(|| format!("{at}: expected text"))
        };
        let (drawer, document, metadata, created) = (read(1)?, read(2)?, read(3)?, read(4)?);
        source_hash.update(serde_json::to_vec(&json!([
            rowid, drawer, document, metadata, created
        ]))?);
        let metadata: serde_json::Map<String, Value> = serde_json::from_str(&metadata)
            .map_err(|_| anyhow::anyhow!("{at}: metadata_json is not a JSON object"))?;
        // Registry and sentinel rows are MemPalace bookkeeping, not memories.
        if metadata.get("ingest_mode").and_then(Value::as_str) == Some("registry")
            || flagged(metadata.get("is_sentinel"))
        {
            summary.skipped_records += 1;
            continue;
        }
        let escaped = flagged(metadata.get(ESCAPED));
        let decode = |s: &str| {
            if escaped {
                unescape(s)
            } else {
                s.to_string()
            }
        };
        let meta_text = |key: &str| metadata.get(key).and_then(Value::as_str).map(decode);
        let body = decode(&document).trim().to_string();
        if body.is_empty() {
            summary.skipped_records += 1;
            continue;
        }
        ensure!(body.len() <= MAX_TEXT_BYTES, "{at}: text exceeds 256 KiB");
        let origin = meta_text("source_file");
        // A project drawer is a chunk of that file; a conversation drawer's source is a transcript.
        let convo = metadata.get("ingest_mode").and_then(Value::as_str) == Some("convos");
        let files: Vec<&String> = origin.iter().filter(|_| !convo).collect();
        let mut text_fields = json!({
            "body": body,
            "files": files,
            "source_file": source_file,
            "drawer_id": drawer,
        });
        if let Some(origin) = &origin {
            text_fields["origin_file"] = json!(origin);
        }
        for key in TEXT_METADATA {
            if let Some(value) = meta_text(key) {
                text_fields[key] = json!(value);
            }
        }
        // The drawer ID is stable across `mempalace compact`, which can renumber
        // rowids, but is built from wing/room names, so only its digest is identity.
        let key = format!("{:x}", Sha256::digest(drawer.as_bytes()));
        let key = &key[..32];
        let mut record = json!({});
        record["id"] = json!(format!("mempalace/drawer/{key}"));
        record["session_schema_version"] = json!(1);
        record["session_format"] = json!("mempalace");
        // Drawers are keyed by palace location, not by agent session.
        record["session_id"] = Value::Null;
        record["message_id"] = json!(format!("drawer/{key}"));
        record["parent_id"] = Value::Null;
        record["role"] = json!("drawer");
        // When the content was written, else when it was mined, else when stored.
        let (authored, filed) = (meta_text("authored_at"), meta_text("filed_at"));
        record["timestamp"] = timestamp([
            authored.as_deref(),
            filed.as_deref(),
            Some(created.as_str()),
        ]);
        record["source_table"] = json!("documents");
        record["source_line"] = json!(rowid);
        for key in NUMERIC_METADATA {
            record[key] = metadata
                .get(key)
                .filter(|v| v.is_i64())
                .cloned()
                .unwrap_or(Value::Null);
        }
        // Drawers hold the mined text verbatim, not a model's condensation.
        record["is_summary"] = json!(false);
        export.push(
            ParsedTurn {
                text_fields,
                identity_fields: record,
            },
            &mut summary,
        )?;
    }
    drop(rows);
    drop(stmt);
    conn.execute_batch("COMMIT")?;
    // Immutable mode trusts the file not to change; a writer that started meanwhile
    // (its sidecars appear, or a checkpoint rewrites the file) voids that snapshot.
    ensure!(
        !immutable || signature(&db) == before,
        "MemPalace store changed during import; rerun when its writer is idle"
    );
    ensure!(
        summary.records > 0,
        "no text-bearing MemPalace drawers; no export published"
    );
    summary.source_sha256 = format!("{:x}", source_hash.finalize());
    export.publish(output, &mut summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palace(dir: &Path) -> PathBuf {
        let palace = dir.join("palace");
        fs::create_dir(&palace).unwrap();
        Connection::open(palace.join(STORE))
            .unwrap()
            .execute_batch(include_str!("../../tests/fixtures/mempalace.sql"))
            .unwrap();
        palace
    }

    fn fresh(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir(&path).unwrap();
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
    fn mempalace_drawers_export_redacted_without_bookkeeping_or_closets() {
        let dir = tempfile::tempdir().unwrap();
        let palace = palace(dir.path());
        let store = palace.join(STORE);
        let original = fs::read(&store).unwrap();
        let output = dir.path().join("safe.jsonl");
        let summary = import_mempalace(&store, &output).unwrap();
        // Registry and sentinel rows are read and skipped; the closet is never read.
        assert_eq!(
            (
                summary.records,
                summary.source_lines,
                summary.skipped_records
            ),
            (3, 5, 2)
        );
        let safe = fs::read_to_string(&output).unwrap();
        assert!(!safe.contains("DO_NOT_EXPORT"));
        assert!(!safe.contains("owner@example.com"));
        assert!(safe.contains("[REDACTED_EMAIL]"));
        assert!(!safe.contains(ESCAPE_PREFIX));
        let rows = rows(&output);
        let project = &rows[0];
        let key = format!("{:x}", Sha256::digest(b"drawer_watcher_design_0001"));
        assert_eq!(project["id"], format!("mempalace/drawer/{}", &key[..32]));
        assert_eq!(project["message_id"], format!("drawer/{}", &key[..32]));
        assert_eq!(project["source_line"], 1);
        assert_eq!(project["session_format"], "mempalace");
        assert_eq!(project["role"], "drawer");
        assert_eq!(project["is_summary"], false);
        assert_eq!(project["session_id"], Value::Null);
        assert_eq!(
            project["title"],
            "drawer: We dropped polling because file events avoid repeated scans while the \
             durable journal preserves recovery."
        );
        assert_eq!(project["files"], json!(["/work/watcher/docs/decisions.md"]));
        assert_eq!(project["origin_file"], "/work/watcher/docs/decisions.md");
        assert_eq!(project["source_file"], STORE);
        assert_eq!(project["drawer_id"], "drawer_watcher_design_0001");
        assert_eq!(
            (&project["wing"], &project["room"]),
            (&json!("watcher"), &json!("design"))
        );
        assert_eq!(
            (&project["line_start"], &project["line_end"]),
            (&json!(3), &json!(4))
        );
        assert_eq!(project["timestamp"], "2026-10-25T00:00:01.000Z");
        let convo = &rows[1];
        assert_eq!(convo["hall"], "hall_facts");
        // A transcript's own time wins over when MemPalace mined it.
        assert_eq!(convo["timestamp"], "2026-10-20T09:30:00.000Z");
        assert_eq!(convo["files"], json!([]));
        assert_eq!(
            convo["origin_file"],
            "/home/me/.claude/projects/watcher/session.jsonl"
        );
        let escaped = &rows[2];
        assert_eq!(escaped["body"], "Build log one two \\ path \u{FFFD} end");
        assert_eq!(escaped["files"], json!(["/work/watcher/build .log"]));
        assert!(escaped.get(ESCAPED).is_none());
        // Same snapshot, same bytes; the store is unchanged and gains no sidecars.
        let again = dir.path().join("again.jsonl");
        let rerun = import_mempalace(&palace, &again).unwrap();
        assert_eq!(rerun.output_sha256, summary.output_sha256);
        assert_eq!(rerun.source_sha256, summary.source_sha256);
        assert_eq!(fs::read(&store).unwrap(), original);
        assert!(!sidecar(&store, "-wal").exists() && !sidecar(&store, "-shm").exists());
        assert!(import_mempalace(&store, &output)
            .unwrap_err()
            .to_string()
            .contains("already exists"));
    }

    #[test]
    fn mempalace_drawers_still_in_a_live_writers_wal_are_exported() {
        let dir = tempfile::tempdir().unwrap();
        let palace = palace(dir.path());
        let writer = Connection::open(palace.join(STORE)).unwrap();
        writer
            .execute_batch(
                "PRAGMA wal_autocheckpoint = 0;
                 INSERT INTO documents VALUES (1, 'drawer_late', 'Written after the snapshot',
                   '{\"wing\":\"watcher\",\"room\":\"design\"}', x'00', 1,
                   '2026-10-25T00:00:09.000000+00:00', '2026-10-25T00:00:09.000000+00:00');",
            )
            .unwrap();
        let output = dir.path().join("safe.jsonl");
        assert_eq!(import_mempalace(&palace, &output).unwrap().records, 4);
        assert_eq!(rows(&output)[3]["body"], "Written after the snapshot");
        drop(writer);
    }

    #[test]
    fn mempalace_chroma_and_malformed_stores_publish_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let chroma = dir.path().join("chroma-palace");
        fs::create_dir(&chroma).unwrap();
        fs::write(chroma.join(CHROMA_STORE), b"").unwrap();
        let not_sqlite = dir.path().join("not-sqlite");
        fs::create_dir(&not_sqlite).unwrap();
        fs::write(not_sqlite.join(STORE), b"not a database").unwrap();
        let empty = dir.path().join("empty.sqlite3");
        Connection::open(&empty)
            .unwrap()
            .execute_batch("CREATE TABLE t(x)")
            .unwrap();
        let no_drawers = palace(&fresh(dir.path(), "no-drawers"));
        Connection::open(no_drawers.join(STORE))
            .unwrap()
            .execute_batch("UPDATE collections SET name = 'renamed' WHERE id = 1")
            .unwrap();
        let bad_metadata = palace(&fresh(dir.path(), "bad-metadata"));
        Connection::open(bad_metadata.join(STORE))
            .unwrap()
            .execute_batch("UPDATE documents SET metadata_json = '[1]' WHERE rowid = 2")
            .unwrap();
        let both = palace(&fresh(dir.path(), "both"));
        fs::write(both.join(CHROMA_STORE), b"").unwrap();
        let torn = palace(&fresh(dir.path(), "torn"));
        fs::write(sidecar(&torn.join(STORE), "-wal"), b"").unwrap();
        for (name, input, reason) in [
            ("chroma", chroma.clone(), "default Chroma backend"),
            ("chroma_file", chroma.join(CHROMA_STORE), "Chroma store"),
            (
                "missing",
                dir.path().join("missing"),
                "cannot open MemPalace",
            ),
            (
                "not_sqlite",
                not_sqlite,
                "pass ~/.mempalace/palace: file is not a database",
            ),
            ("empty", empty, "no collections and documents tables"),
            ("no_drawers", no_drawers, "no mempalace_drawers collection"),
            (
                "bad_metadata",
                bad_metadata,
                "row 2: metadata_json is not a JSON object",
            ),
            ("both", both, "both a Chroma and a sqlite_exact store"),
            ("torn", torn, "incomplete WAL sidecar set"),
        ] {
            let output = dir.path().join(format!("{name}.jsonl"));
            let error = format!("{:#}", import_mempalace(&input, &output).unwrap_err());
            assert!(error.contains(reason), "{name}: {error}");
            assert!(!output.exists(), "{name}");
        }
    }

    #[test]
    fn mempalace_ids_survive_a_compact_that_renumbers_rows() {
        let dir = tempfile::tempdir().unwrap();
        let palace = palace(dir.path());
        let before = dir.path().join("before.jsonl");
        import_mempalace(&palace, &before).unwrap();
        // Rewrite the drawers in reverse order so every rowid changes.
        Connection::open(palace.join(STORE))
            .unwrap()
            .execute_batch(
                "CREATE TABLE copy AS SELECT * FROM documents ORDER BY rowid DESC;
                 DELETE FROM documents;
                 INSERT INTO documents SELECT * FROM copy;
                 DROP TABLE copy;",
            )
            .unwrap();
        let after = dir.path().join("after.jsonl");
        import_mempalace(&palace, &after).unwrap();
        let ids = |path: &Path| -> Vec<(Value, Value)> {
            rows(path)
                .into_iter()
                .map(|r| (r["id"].clone(), r["source_line"].clone()))
                .collect()
        };
        let (before, after) = (ids(&before), ids(&after));
        for ((id, line), (id_after, line_after)) in before.iter().zip(&after) {
            assert_eq!(id, id_after);
            assert_ne!(line, line_after);
        }
    }

    #[test]
    fn mempalace_flags_match_its_own_reader() {
        for (value, set) in [
            (json!(true), true),
            (json!(1), true),
            (json!("1"), true),
            (json!("true"), true),
            (json!("True"), true),
            (json!(false), false),
            (json!(0), false),
            (json!(2), false),
            (json!("yes"), false),
            (json!("TRUE"), false),
            (Value::Null, false),
        ] {
            assert_eq!(flagged(Some(&value)), set, "{value}");
        }
        assert!(!flagged(None));
    }

    #[test]
    fn mempalace_writer_starting_mid_import_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let store = palace(dir.path()).join(STORE);
        let before = signature(&store);
        let (reader, immutable) = open(&store).unwrap();
        assert!(immutable);
        // A writer that opens the clean store now creates its sidecars, so the
        // check after COMMIT sees a different signature and refuses.
        let writer = Connection::open(&store).unwrap();
        writer
            .execute_batch(
                "INSERT INTO documents VALUES (1, 'drawer_late', 'Late', '{}', x'00', 1, \
                 '2026-10-25T00:00:09+00:00', '2026-10-25T00:00:09+00:00');",
            )
            .unwrap();
        reader.execute_batch("COMMIT").unwrap();
        assert_ne!(signature(&store), before);
    }

    #[test]
    fn mempalace_unescape_follows_stored_text_rules() {
        for (stored, read) in [
            ("plain \\0 text", "plain \\0 text"),
            ("\u{1}mpesc:a\\\\b", "a\\b"),
            ("\u{1}mpesc:nul\\0end", "nul end"),
            ("\u{1}mpesc:\\uDC80", "\u{FFFD}"),
            ("\u{1}mpesc:\\u00E9", "\u{e9}"),
            ("\u{1}mpesc:keep \\x and \\u12", "keep \\x and \\u12"),
            ("\u{1}mpesc:\\\\0", "\\0"),
        ] {
            assert_eq!(unescape(stored), read, "{stored:?}");
        }
    }

    #[test]
    fn mempalace_is_a_cli_format() {
        let dir = tempfile::tempdir().unwrap();
        let input = palace(dir.path());
        let output = dir.path().join("safe.jsonl");
        let args: Vec<String> = ["import", "--format", "mempalace", "--input"]
            .iter()
            .map(|s| s.to_string())
            .chain([input.display().to_string(), "--output".into()])
            .chain([output.display().to_string()])
            .collect();
        assert_eq!(run_cli(&args), 0);
        assert_eq!(rows(&output).len(), 3);
    }
}
