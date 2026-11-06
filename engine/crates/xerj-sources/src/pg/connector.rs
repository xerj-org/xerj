//! The Postgres connector: discover tables, take a consistent snapshot, then
//! follow every change through logical replication (or, when the server or
//! role does not allow replication, poll a watermark column).
//!
//! Consistency: the replication slot is created with an exported snapshot,
//! and the snapshot is read inside a `REPEATABLE READ` transaction pinned to
//! it (`SET TRANSACTION SNAPSHOT`). Streaming then starts at the slot's
//! consistent point, so every change is in exactly one of the two: the
//! snapshot or the stream. Document ids come from primary keys, so applying a
//! change twice (after a crash between "applied" and "acknowledged") rewrites
//! the same document: the apply is idempotent and nothing is lost or doubled.
//!
//! Acknowledgement: the LSN reported back to Postgres is advanced only after
//! the batch containing that commit was applied. The server keeps WAL from the
//! last acknowledged LSN, so a restart (or a lost connection) resumes there.

use super::pgoutput::{self, Cell, Msg, Relation};
use super::wire::{ident, literal, pg_time_to_unix_secs, standby_status, PgConn, PgError};
use crate::change::{
    copy_unescape, doc_id, pg_value, FactsObserver, Op, RowChange, RowChangeKind, Sink,
};
use crate::spec::{Mode, SourceSpec};
use crate::status::{lsn_str, parse_lsn, Rate, Shared, State, TableStatus, LAG_SECS};
use crate::store::{PersistedState, Phase, Store};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Ops per sink call during the snapshot.
const SNAPSHOT_BATCH: usize = 5_000;
/// Stream batching: apply when this many ops are buffered, or when the oldest
/// buffered op is this old (whichever first), always at a commit boundary.
const STREAM_BATCH: usize = 2_000;
const STREAM_MAX_WAIT: Duration = Duration::from_millis(20);
const FEEDBACK_EVERY: Duration = Duration::from_secs(5);
const SAVE_EVERY: Duration = Duration::from_secs(1);
const POLL_PAGE: usize = 10_000;

pub struct Ctx<'a> {
    pub spec: &'a SourceSpec,
    pub store: &'a Store,
    pub status: &'a Shared,
    pub sink: &'a dyn Sink,
    pub facts: Option<&'a dyn FactsObserver>,
    pub stop: &'a AtomicBool,
}

#[derive(Debug, Clone)]
struct Table {
    schema: String,
    name: String,
    columns: Vec<(String, u32)>,
    pk: Vec<String>,
    estimate: u64,
    index: String,
}

impl Ctx<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    fn with<R>(&self, f: impl FnOnce(&mut crate::status::Status) -> R) -> R {
        let mut g = self.status.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }
}

/// Run until stopped. Errors are reported in the status with a fix hint and
/// retried with backoff; they never end the worker.
pub fn run(ctx: &Ctx<'_>) {
    let mut backoff = Duration::from_secs(2);
    loop {
        if ctx.stopped() {
            ctx.with(|s| s.state = State::Stopped);
            return;
        }
        match run_once(ctx) {
            Ok(()) => {
                ctx.with(|s| s.state = State::Stopped);
                return;
            }
            Err(e) => {
                let (message, hint) = explain(&e);
                tracing::warn!(target: "xerj_sources", source = %ctx.spec.name, "{message}");
                ctx.with(|s| {
                    s.state = State::Error;
                    s.error = Some(crate::status::ErrorInfo {
                        message,
                        hint,
                        at: chrono::Utc::now().to_rfc3339(),
                    });
                });
                let until = Instant::now() + backoff;
                while Instant::now() < until {
                    if ctx.stopped() {
                        ctx.with(|s| s.state = State::Stopped);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// A human message and the fix, from an error chain.
pub fn explain(e: &anyhow::Error) -> (String, String) {
    let message = format!("{e:#}");
    let hint = if let Some(pg) = e.downcast_ref::<PgError>() {
        match pg.code.as_str() {
            "28P01" | "28000" => "The database refused the login. Check the user in the URL and the password the source's password_env / password_file points at.".into(),
            "3D000" => "The database in the URL does not exist.".into(),
            "42501" => "The role lacks a privilege. Grant SELECT on the tables, REPLICATION on the role (ALTER ROLE <user> REPLICATION), and either let XERJ create the publication (table owner) or create it yourself: CREATE PUBLICATION xerj_<name> FOR TABLE ...; then set \"publication\" on the source.".into(),
            "53400" => "The server has no free replication slot. Raise max_replication_slots (and max_wal_senders) in postgresql.conf and restart Postgres, or drop an unused slot.".into(),
            "55006" => "The replication slot is in use by another connection. Another XERJ node may be following this source; stop it, or give this source a different name.".into(),
            "42704" => "The replication slot or publication was dropped outside XERJ. The source takes a fresh snapshot.".into(),
            _ => pg.hint.clone().unwrap_or_default(),
        }
    } else if message.contains("cannot reach") || message.contains("resolve") {
        "The XERJ node cannot reach the database. Check host and port in the URL, firewalls, and that Postgres listens on that address (listen_addresses, pg_hba.conf).".into()
    } else {
        String::new()
    };
    (message, hint)
}

fn run_once(ctx: &Ctx<'_>) -> Result<()> {
    let spec = ctx.spec;
    let pw = spec.resolve_password()?;
    let cp = spec.conn_params()?;
    ctx.with(|s| {
        s.state = State::Connecting;
        s.error = None;
    });
    let mut admin = PgConn::connect(&cp, pw.as_deref(), false)
        .with_context(|| format!("connect to {}", spec.display_url()))?;
    let wal_level = admin
        .simple_query("SHOW wal_level")?
        .rows
        .first()
        .and_then(|r| r[0].clone())
        .unwrap_or_default();
    let role = admin.simple_query(
        "SELECT rolreplication, rolsuper FROM pg_roles WHERE rolname = current_user",
    )?;
    let can_replicate = role
        .rows
        .first()
        .map(|r| r.iter().any(|v| v.as_deref() == Some("t")))
        .unwrap_or(false);
    let (tables, skipped) = discover(&mut admin, spec)?;
    if tables.is_empty() {
        bail!(
            "no table to follow: the filter selected none that has a primary key{}",
            if skipped.is_empty() {
                String::new()
            } else {
                format!(" (skipped: {})", skipped.join("; "))
            }
        );
    }
    ctx.with(|s| {
        s.tables = tables
            .iter()
            .map(|t| TableStatus {
                table: format!("{}.{}", t.schema, t.name),
                index: t.index.clone(),
                rows_indexed: 0,
                rows_estimate: t.estimate,
                snapshot_done: false,
            })
            .collect();
        s.skipped = skipped.clone();
    });
    let logical_ok = wal_level == "logical" && can_replicate;
    let why_not = || {
        let mut v = Vec::new();
        if wal_level != "logical" {
            v.push(format!(
                "wal_level is {wal_level:?} (needs logical: ALTER SYSTEM SET wal_level = logical; then restart Postgres)"
            ));
        }
        if !can_replicate {
            v.push(format!(
                "role {} has no REPLICATION attribute (ALTER ROLE {} REPLICATION)",
                cp.user, cp.user
            ));
        }
        v.join("; ")
    };
    let use_logical = match spec.mode {
        Mode::Logical => {
            if !logical_ok {
                bail!("logical replication is not available: {}", why_not());
            }
            true
        }
        Mode::Poll => false,
        Mode::Auto => {
            if logical_ok {
                true
            } else if spec.watermark_column.is_some() {
                ctx.with(|s| {
                    s.note(format!(
                        "logical replication is not available ({}); polling {} instead. Polling sees inserts and updates, never deletes",
                        why_not(),
                        spec.watermark_column.as_deref().unwrap_or_default()
                    ))
                });
                false
            } else {
                bail!(
                    "real-time streaming needs logical replication, which is not available: {}. \
                     Fix that, or set watermark_column (for example updated_at) to poll instead",
                    why_not()
                );
            }
        }
    };
    if use_logical {
        run_logical(ctx, &cp, pw.as_deref(), admin, tables)
    } else {
        run_poll(ctx, admin, tables)
    }
}

fn discover(admin: &mut PgConn, spec: &SourceSpec) -> Result<(Vec<Table>, Vec<String>)> {
    // chr(31)/chr(30) as separators: column names may contain ',' or ':'.
    let q = admin.simple_query(
        "SELECT n.nspname, c.relname, c.reltuples::bigint, \
           (SELECT string_agg(a.attname || chr(31) || a.atttypid::text, chr(30) ORDER BY a.attnum) \
              FROM pg_attribute a WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped), \
           (SELECT string_agg(a.attname, chr(30) ORDER BY array_position(i.indkey::int2[], a.attnum)) \
              FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
             WHERE i.indrelid = c.oid AND i.indisprimary) \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r','p') AND NOT c.relispartition \
           AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' \
           AND has_table_privilege(c.oid, 'SELECT') \
         ORDER BY 1, 2",
    )?;
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for r in &q.rows {
        let schema = r[0].clone().unwrap_or_default();
        let name = r[1].clone().unwrap_or_default();
        if !spec.selects(&schema, &name) {
            continue;
        }
        let columns: Vec<(String, u32)> = r[3]
            .as_deref()
            .unwrap_or("")
            .split('\x1e')
            .filter(|s| !s.is_empty())
            .filter_map(|c| {
                let (n, t) = c.split_once('\x1f')?;
                Some((n.to_string(), t.parse().ok()?))
            })
            .collect();
        let pk: Vec<String> = r[4]
            .as_deref()
            .unwrap_or("")
            .split('\x1e')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if pk.is_empty() {
            skipped.push(format!(
                "{schema}.{name}: no primary key, so changes cannot be matched to documents (add one)"
            ));
            continue;
        }
        let estimate = r[2]
            .as_deref()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0) as u64;
        out.push(Table {
            index: spec.index_for(&schema, &name),
            schema,
            name,
            columns,
            pk,
            estimate,
        });
    }
    Ok((out, skipped))
}

fn ensure_publication(
    ctx: &Ctx<'_>,
    admin: &mut PgConn,
    tables: &[Table],
    st: &mut PersistedState,
) -> Result<()> {
    let name = ctx.spec.publication_name();
    let exists = !admin
        .simple_query(&format!(
            "SELECT 1 FROM pg_publication WHERE pubname = {}",
            literal(&name)
        ))?
        .rows
        .is_empty();
    let list = tables
        .iter()
        .map(|t| format!("{}.{}", ident(&t.schema), ident(&t.name)))
        .collect::<Vec<_>>()
        .join(", ");
    if ctx.spec.publication.is_some() {
        if !exists {
            bail!(
                "publication {name} does not exist. Create it (CREATE PUBLICATION {} FOR TABLE {list}) or drop \"publication\" from the source so XERJ creates its own",
                ident(&name)
            );
        }
        return Ok(());
    }
    let created_here = if exists {
        // Ours from an earlier run: keep its table list in step with the filter.
        admin
            .simple_query(&format!(
                "ALTER PUBLICATION {} SET TABLE {list}",
                ident(&name)
            ))
            .with_context(|| format!("update publication {name}"))?;
        st.publication_created
    } else {
        let with = "WITH (publish_via_partition_root = true)";
        admin
            .simple_query(&format!(
                "CREATE PUBLICATION {} FOR TABLE {list} {with}",
                ident(&name)
            ))
            .with_context(|| format!("create publication {name}"))?;
        true
    };
    st.publication_created = created_here;
    Ok(())
}

fn drop_slot_if_exists(admin: &mut PgConn, slot: &str) -> Result<()> {
    admin.simple_query(&format!(
        "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
          WHERE slot_name = {0} AND active_pid IS NOT NULL",
        literal(slot)
    ))?;
    admin.simple_query(&format!(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = {}",
        literal(slot)
    ))?;
    Ok(())
}

/// Drop what this source created on the database: its slot (which otherwise
/// keeps WAL forever) and, if XERJ created it, its publication. Called by
/// `DELETE /_xerj/sources/{name}` after the worker stopped.
pub fn cleanup(spec: &SourceSpec, st: &PersistedState) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if !st.slot_created && !st.publication_created {
        return Ok(done);
    }
    let pw = spec.resolve_password()?;
    let mut admin = PgConn::connect(&spec.conn_params()?, pw.as_deref(), false)?;
    if st.slot_created {
        drop_slot_if_exists(&mut admin, &spec.slot_name())?;
        done.push(format!("dropped replication slot {}", spec.slot_name()));
    }
    if st.publication_created {
        admin.simple_query(&format!(
            "DROP PUBLICATION IF EXISTS {}",
            ident(&spec.publication_name())
        ))?;
        done.push(format!("dropped publication {}", spec.publication_name()));
    }
    admin.close();
    Ok(done)
}

fn row_doc(columns: &[(String, u32)], values: &[Option<String>]) -> Map<String, Value> {
    let mut doc = Map::new();
    for ((name, oid), v) in columns.iter().zip(values) {
        if let Some(text) = v {
            if let Some(val) = pg_value(*oid, text) {
                doc.insert(name.clone(), val);
            }
        }
    }
    doc
}

fn key_of(
    columns: &[(String, u32)],
    pk: &[String],
    values: &[Option<String>],
) -> Option<Vec<(String, String)>> {
    pk.iter()
        .map(|k| {
            let i = columns.iter().position(|(n, _)| n == k)?;
            Some((k.clone(), values.get(i)?.clone()?))
        })
        .collect()
}

fn snapshot(
    ctx: &Ctx<'_>,
    admin: &mut PgConn,
    tables: &[Table],
    exported: Option<&str>,
) -> Result<()> {
    admin.simple_query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    if let Some(name) = exported {
        admin.simple_query(&format!("SET TRANSACTION SNAPSHOT {}", literal(name)))?;
    }
    let total: u64 = tables.iter().map(|t| t.estimate).sum::<u64>().max(1);
    let started = Instant::now();
    let mut done_all = 0u64;
    ctx.with(|s| {
        s.state = State::Snapshotting;
        s.snapshot = Default::default();
        s.snapshot.rows_estimate = total;
    });
    for (ti, t) in tables.iter().enumerate() {
        if ctx.stopped() {
            bail!("stopped during the snapshot");
        }
        let cols = t
            .columns
            .iter()
            .map(|(n, _)| ident(n))
            .collect::<Vec<_>>()
            .join(", ");
        let limit = ctx
            .spec
            .snapshot_limit
            .map(|n| format!(" LIMIT {n}"))
            .unwrap_or_default();
        let sql = format!(
            "COPY (SELECT {cols} FROM {}.{}{limit}) TO STDOUT",
            ident(&t.schema),
            ident(&t.name)
        );
        let mut batch: Vec<Op> = Vec::with_capacity(SNAPSHOT_BATCH);
        let mut facts: Vec<RowChange> = Vec::new();
        let mut table_rows = 0u64;
        let mut flush = |batch: &mut Vec<Op>, facts: &mut Vec<RowChange>, n: u64| -> Result<()> {
            ctx.sink.apply(batch)?;
            if let (Some(obs), Some(ns)) = (ctx.facts, ctx.spec.facts_namespace.as_deref()) {
                obs.on_changes(ns, facts)?;
            }
            batch.clear();
            facts.clear();
            done_all += n;
            let el = started.elapsed().as_secs_f64().max(1e-3);
            let rps = done_all as f64 / el;
            ctx.with(|s| {
                s.tables[ti].rows_indexed += n;
                s.snapshot.rows_done = done_all;
                s.snapshot.rows_estimate = total.max(done_all);
                s.snapshot.percent =
                    (100.0 * done_all as f64 / total.max(done_all) as f64).min(100.0);
                s.snapshot.rows_per_sec = rps;
                s.snapshot.eta_secs = if rps > 0.0 {
                    Some((total.saturating_sub(done_all) as f64 / rps) as u64)
                } else {
                    None
                };
            });
            Ok(())
        };
        let mut pending = 0u64;
        admin
            .copy_out(&sql, |line| {
                let values: Vec<Option<String>> = line
                    .split(|b| *b == b'\t')
                    .map(|f| {
                        if f == b"\\N" {
                            None
                        } else {
                            Some(copy_unescape(f))
                        }
                    })
                    .collect();
                let Some(key) = key_of(&t.columns, &t.pk, &values) else {
                    return Ok(());
                };
                let doc = row_doc(&t.columns, &values);
                if ctx.spec.facts_namespace.is_some() {
                    facts.push(RowChange {
                        source: ctx.spec.name.clone(),
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        kind: RowChangeKind::Insert,
                        key: key.clone(),
                        row: doc.clone(),
                        lsn: 0,
                    });
                }
                batch.push(Op::Index {
                    index: t.index.clone(),
                    id: doc_id(&key),
                    doc,
                });
                pending += 1;
                table_rows += 1;
                if batch.len() >= SNAPSHOT_BATCH {
                    flush(&mut batch, &mut facts, pending)?;
                    pending = 0;
                    if ctx.stopped() {
                        bail!("stopped during the snapshot");
                    }
                }
                Ok(())
            })
            .with_context(|| format!("snapshot {}.{}", t.schema, t.name))?;
        if !batch.is_empty() || pending > 0 {
            flush(&mut batch, &mut facts, pending)?;
        }
        ctx.with(|s| {
            s.tables[ti].snapshot_done = true;
            s.tables[ti].rows_estimate = table_rows;
        });
    }
    admin.simple_query("COMMIT")?;
    let secs = started.elapsed().as_secs_f64();
    ctx.with(|s| {
        s.snapshot.percent = 100.0;
        s.snapshot.eta_secs = Some(0);
        s.note(format!(
            "snapshot done: {done_all} rows from {} tables in {secs:.1} s ({:.0} rows/s)",
            tables.len(),
            done_all as f64 / secs.max(1e-3)
        ));
    });
    Ok(())
}

fn run_logical(
    ctx: &Ctx<'_>,
    cp: &crate::spec::ConnParams,
    pw: Option<&str>,
    mut admin: PgConn,
    tables: Vec<Table>,
) -> Result<()> {
    let spec = ctx.spec;
    let mut st = ctx.store.load_state();
    ctx.with(|s| s.mode = "logical".into());
    ensure_publication(ctx, &mut admin, &tables, &mut st)?;
    ctx.store.save_state(&st)?;
    let slot = spec.slot_name();
    let mut repl;
    if st.phase != Phase::Streaming {
        // A fresh, consistent snapshot. Anything a previous attempt indexed
        // is cleared first: rows deleted since then would otherwise linger.
        drop_slot_if_exists(&mut admin, &slot)?;
        for t in &tables {
            ctx.sink.clear_index(&t.index)?;
        }
        repl = PgConn::connect(cp, pw, true)?;
        let major: u32 = repl
            .server_version
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let create = if major >= 15 {
            format!(
                "CREATE_REPLICATION_SLOT {} LOGICAL pgoutput (SNAPSHOT 'export')",
                ident(&slot)
            )
        } else {
            format!(
                "CREATE_REPLICATION_SLOT {} LOGICAL pgoutput EXPORT_SNAPSHOT",
                ident(&slot)
            )
        };
        let r = repl
            .simple_query(&create)
            .context("create the replication slot")?;
        let point = r
            .col("consistent_point")
            .and_then(|i| r.rows.first()?.get(i)?.clone())
            .and_then(|s| parse_lsn(&s))
            .ok_or_else(|| anyhow!("CREATE_REPLICATION_SLOT returned no consistent point"))?;
        let snap_name = r
            .col("snapshot_name")
            .and_then(|i| r.rows.first()?.get(i)?.clone())
            .ok_or_else(|| anyhow!("CREATE_REPLICATION_SLOT exported no snapshot"))?;
        st.slot_created = true;
        st.phase = Phase::Snapshot;
        st.lsn = 0;
        ctx.store.save_state(&st)?;
        ctx.with(|s| {
            s.note(format!(
                "created replication slot {slot} at {}",
                lsn_str(point)
            ))
        });
        snapshot(ctx, &mut admin, &tables, Some(&snap_name))?;
        st.phase = Phase::Streaming;
        st.lsn = point;
        ctx.store.save_state(&st)?;
    } else {
        repl = PgConn::connect(cp, pw, true)?;
        ctx.with(|s| {
            for t in s.tables.iter_mut() {
                t.snapshot_done = true;
            }
            s.snapshot.percent = 100.0;
        });
    }
    admin.close();
    match stream(ctx, &mut repl, &tables, &mut st) {
        Err(e) => {
            if e.downcast_ref::<PgError>().map(|p| p.code.as_str()) == Some("42704") {
                st.phase = Phase::Snapshot;
                st.slot_created = false;
                ctx.store.save_state(&st)?;
            }
            Err(e)
        }
        ok => ok,
    }
}

struct Followed {
    index: String,
    columns: Vec<(String, u32)>,
    pk: Vec<String>,
    schema: String,
    name: String,
}

fn stream(
    ctx: &Ctx<'_>,
    repl: &mut PgConn,
    tables: &[Table],
    st: &mut PersistedState,
) -> Result<()> {
    let spec = ctx.spec;
    let pubname = spec.publication_name();
    let start = st.lsn;
    repl.start_copy_both(&format!(
        "START_REPLICATION SLOT {} LOGICAL {} (proto_version '1', publication_names {})",
        ident(&spec.slot_name()),
        lsn_str(start),
        literal(&ident(&pubname))
    ))?;
    repl.set_read_timeout(Duration::from_millis(10))?;
    ctx.with(|s| {
        s.state = State::Streaming;
        s.error = None;
        s.stream.confirmed_lsn = lsn_str(start);
        s.note(format!("streaming from {}", lsn_str(start)));
    });
    let selected: HashSet<(String, String)> = tables
        .iter()
        .map(|t| (t.schema.clone(), t.name.clone()))
        .collect();
    let mut rels: HashMap<u32, Relation> = HashMap::new();
    let mut followed: HashMap<u32, Followed> = HashMap::new();
    let mut warned: HashSet<u32> = HashSet::new();
    let mut buf: Vec<Op> = Vec::new();
    let mut facts: Vec<RowChange> = Vec::new();
    let mut buf_commits: Vec<i64> = Vec::new();
    let mut first_buf_at: Option<Instant> = None;
    let mut in_txn = false;
    let mut cur_commit_ts: i64 = 0;
    let mut pending_lsn = start;
    let mut confirmed = start;
    let mut server_lsn = start;
    let mut last_feedback = Instant::now();
    let mut last_save = Instant::now();
    let mut saved_lsn = start;
    let mut rate = Rate::default();
    let mut counts = [0u64; 4]; // inserts, updates, deletes, truncates

    loop {
        if ctx.stopped() {
            if !in_txn && !buf.is_empty() {
                apply(ctx, &mut buf, &mut facts)?;
                confirmed = pending_lsn;
            }
            repl.send_copy_data(&standby_status(confirmed, false))?;
            st.lsn = confirmed;
            ctx.store.save_state(st)?;
            return Ok(());
        }
        if let Some(d) = repl.read_copy_data()? {
            match d.first() {
                Some(b'w') if d.len() >= 25 => {
                    let wal_end = u64::from_be_bytes(d[9..17].try_into().unwrap());
                    server_lsn = server_lsn.max(wal_end);
                    match pgoutput::decode(&d[25..])? {
                        Msg::Begin { commit_ts, .. } => {
                            in_txn = true;
                            cur_commit_ts = commit_ts;
                        }
                        Msg::Commit { end_lsn, .. } => {
                            in_txn = false;
                            pending_lsn = end_lsn;
                            if first_buf_at.is_some() {
                                buf_commits.push(cur_commit_ts);
                            }
                        }
                        Msg::Relation(rel) => {
                            schema_change(ctx, rels.get(&rel.oid), &rel);
                            let key: Vec<String> = rel
                                .columns
                                .iter()
                                .filter(|c| c.key)
                                .map(|c| c.name.clone())
                                .collect();
                            if selected.contains(&(rel.schema.clone(), rel.name.clone()))
                                || spec.selects(&rel.schema, &rel.name)
                            {
                                if key.is_empty() {
                                    if warned.insert(rel.oid) {
                                        ctx.with(|s| s.note(format!("{}.{}: no replica identity key; its changes are skipped", rel.schema, rel.name)));
                                    }
                                    followed.remove(&rel.oid);
                                } else {
                                    followed.insert(
                                        rel.oid,
                                        Followed {
                                            index: spec.index_for(&rel.schema, &rel.name),
                                            columns: rel
                                                .columns
                                                .iter()
                                                .map(|c| (c.name.clone(), c.type_oid))
                                                .collect(),
                                            pk: key,
                                            schema: rel.schema.clone(),
                                            name: rel.name.clone(),
                                        },
                                    );
                                }
                            }
                            rels.insert(rel.oid, rel);
                        }
                        Msg::Insert { rel, new } => {
                            if let Some(f) = followed.get(&rel) {
                                let values = cells_text(&new);
                                if let Some(key) = key_of(&f.columns, &f.pk, &values) {
                                    let doc = row_doc(&f.columns, &values);
                                    push_fact(
                                        ctx,
                                        &mut facts,
                                        f,
                                        RowChangeKind::Insert,
                                        &key,
                                        &doc,
                                        pending_lsn,
                                    );
                                    buf.push(Op::Index {
                                        index: f.index.clone(),
                                        id: doc_id(&key),
                                        doc,
                                    });
                                    counts[0] += 1;
                                }
                            }
                        }
                        Msg::Update { rel, old, new } => {
                            if let Some(f) = followed.get(&rel) {
                                let values = cells_text(&new);
                                let Some(key) = key_of(&f.columns, &f.pk, &values) else {
                                    continue;
                                };
                                if let Some(old) = old {
                                    if let Some(old_key) =
                                        key_of(&f.columns, &f.pk, &cells_text(&old))
                                    {
                                        if old_key != key {
                                            buf.push(Op::Delete {
                                                index: f.index.clone(),
                                                id: doc_id(&old_key),
                                            });
                                        }
                                    }
                                }
                                let doc = row_doc(&f.columns, &values);
                                push_fact(
                                    ctx,
                                    &mut facts,
                                    f,
                                    RowChangeKind::Update,
                                    &key,
                                    &doc,
                                    pending_lsn,
                                );
                                if new.contains(&Cell::Unchanged) {
                                    buf.push(Op::Update {
                                        index: f.index.clone(),
                                        id: doc_id(&key),
                                        doc,
                                    });
                                } else {
                                    buf.push(Op::Index {
                                        index: f.index.clone(),
                                        id: doc_id(&key),
                                        doc,
                                    });
                                }
                                counts[1] += 1;
                            }
                        }
                        Msg::Delete { rel, old } => {
                            if let Some(f) = followed.get(&rel) {
                                if let Some(key) = key_of(&f.columns, &f.pk, &cells_text(&old)) {
                                    push_fact(
                                        ctx,
                                        &mut facts,
                                        f,
                                        RowChangeKind::Delete,
                                        &key,
                                        &Map::new(),
                                        pending_lsn,
                                    );
                                    buf.push(Op::Delete {
                                        index: f.index.clone(),
                                        id: doc_id(&key),
                                    });
                                    counts[2] += 1;
                                }
                            }
                        }
                        Msg::Truncate { rels: oids } => {
                            // Order matters: what is buffered happened before.
                            if !buf.is_empty() {
                                apply(ctx, &mut buf, &mut facts)?;
                            }
                            for oid in oids {
                                if let Some(f) = followed.get(&oid) {
                                    ctx.sink.clear_index(&f.index)?;
                                    counts[3] += 1;
                                    ctx.with(|s| {
                                        s.note(format!(
                                            "{}.{} was truncated; its index was cleared",
                                            f.schema, f.name
                                        ))
                                    });
                                }
                            }
                        }
                        Msg::Other => {}
                    }
                    if !buf.is_empty() && first_buf_at.is_none() {
                        first_buf_at = Some(Instant::now());
                    }
                }
                Some(b'k') if d.len() >= 18 => {
                    let wal_end = u64::from_be_bytes(d[1..9].try_into().unwrap());
                    server_lsn = server_lsn.max(wal_end);
                    if d[17] == 1 {
                        repl.send_copy_data(&standby_status(confirmed, false))?;
                        last_feedback = Instant::now();
                    }
                }
                _ => {}
            }
        }
        // Apply at commit boundaries only, so a batch never holds half a
        // transaction and the acknowledged LSN always ends one.
        if !in_txn && pending_lsn > confirmed {
            let due = buf.len() >= STREAM_BATCH
                || first_buf_at
                    .map(|t| t.elapsed() >= STREAM_MAX_WAIT)
                    .unwrap_or(true);
            if due {
                let n = buf.len() as u64;
                if !buf.is_empty() {
                    apply(ctx, &mut buf, &mut facts)?;
                }
                let now = unix_now();
                let r = rate.add(n);
                confirmed = pending_lsn;
                let commits = std::mem::take(&mut buf_commits);
                ctx.with(|s| {
                    for ts in commits {
                        s.record_latency((now - pg_time_to_unix_secs(ts)) * 1000.0);
                    }
                    s.stream.rows_per_sec = r;
                });
                first_buf_at = None;
            }
        }
        if last_feedback.elapsed() >= FEEDBACK_EVERY {
            repl.send_copy_data(&standby_status(confirmed, false))?;
            last_feedback = Instant::now();
        }
        if last_save.elapsed() >= SAVE_EVERY {
            if saved_lsn != confirmed {
                st.lsn = confirmed;
                ctx.store.save_state(st)?;
                saved_lsn = confirmed;
            }
            last_save = Instant::now();
            let lag = match first_buf_at {
                Some(_) if cur_commit_ts > 0 => {
                    (unix_now() - pg_time_to_unix_secs(cur_commit_ts)).max(0.0)
                }
                _ if in_txn && cur_commit_ts > 0 => {
                    (unix_now() - pg_time_to_unix_secs(cur_commit_ts)).max(0.0)
                }
                _ => 0.0,
            };
            let r = rate.decay();
            ctx.with(|s| {
                s.stream.confirmed_lsn = lsn_str(confirmed);
                s.stream.server_lsn = lsn_str(server_lsn);
                s.stream.lag_secs = lag;
                if first_buf_at.is_none() {
                    s.stream.rows_per_sec = r;
                }
                s.stream.inserts = counts[0];
                s.stream.updates = counts[1];
                s.stream.deletes = counts[2];
                s.stream.truncates = counts[3];
                s.state = if lag > LAG_SECS {
                    State::Lagging
                } else {
                    State::Streaming
                };
            });
        }
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn cells_text(cells: &[Cell]) -> Vec<Option<String>> {
    cells
        .iter()
        .map(|c| match c {
            Cell::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn push_fact(
    ctx: &Ctx<'_>,
    facts: &mut Vec<RowChange>,
    f: &Followed,
    kind: RowChangeKind,
    key: &[(String, String)],
    doc: &Map<String, Value>,
    lsn: u64,
) {
    if ctx.spec.facts_namespace.is_some() && ctx.facts.is_some() {
        facts.push(RowChange {
            source: ctx.spec.name.clone(),
            schema: f.schema.clone(),
            table: f.name.clone(),
            kind,
            key: key.to_vec(),
            row: doc.clone(),
            lsn,
        });
    }
}

fn apply(ctx: &Ctx<'_>, buf: &mut Vec<Op>, facts: &mut Vec<RowChange>) -> Result<()> {
    ctx.sink.apply(buf)?;
    if let (Some(obs), Some(ns)) = (ctx.facts, ctx.spec.facts_namespace.as_deref()) {
        if !facts.is_empty() {
            obs.on_changes(ns, facts)?;
        }
    }
    buf.clear();
    facts.clear();
    Ok(())
}

/// New columns, dropped columns and type changes, said once in the status.
fn schema_change(ctx: &Ctx<'_>, old: Option<&Relation>, new: &Relation) {
    let Some(old) = old else { return };
    let before: HashMap<&str, u32> = old
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_oid))
        .collect();
    let after: HashMap<&str, u32> = new
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_oid))
        .collect();
    let t = format!("{}.{}", new.schema, new.name);
    for (n, ty) in &after {
        match before.get(n) {
            None => ctx.with(|s| s.note(format!("{t}: new column {n}; it is indexed from now on (older documents do not have it until their row changes)"))),
            Some(b) if b != ty => ctx.with(|s| s.note(format!("{t}: column {n} changed type (oid {b} -> {ty}); new values are indexed with the new type, older documents keep the old one. If the index mapping refuses the new type, delete and re-add the source to re-snapshot"))),
            _ => {}
        }
    }
    for n in before.keys() {
        if !after.contains_key(n) {
            ctx.with(|s| {
                s.note(format!(
                    "{t}: column {n} was dropped; older documents keep it until their row changes"
                ))
            });
        }
    }
}

/// Watermark polling: re-read rows whose watermark is past the high-water
/// mark. No replication privileges needed; deletes are not seen.
fn run_poll(ctx: &Ctx<'_>, mut admin: PgConn, tables: Vec<Table>) -> Result<()> {
    let wm = ctx
        .spec
        .watermark_column
        .clone()
        .ok_or_else(|| anyhow!("polling needs watermark_column"))?;
    let mut st = ctx.store.load_state();
    if st.phase != Phase::Polling {
        st = PersistedState {
            phase: Phase::Polling,
            ..Default::default()
        };
        for t in &tables {
            ctx.sink.clear_index(&t.index)?;
        }
        ctx.store.save_state(&st)?;
    }
    let tables: Vec<Table> = tables
        .into_iter()
        .filter(|t| {
            let has = t.columns.iter().any(|(n, _)| *n == wm);
            if !has {
                ctx.with(|s| {
                    s.skipped
                        .push(format!("{}.{}: no column {wm} to poll", t.schema, t.name))
                });
            }
            has
        })
        .collect();
    ctx.with(|s| {
        s.mode = "poll".into();
        s.state = State::Polling;
        s.note(format!(
            "polling every {} s on {wm}; deletes are not detected in polling mode",
            ctx.spec.poll_secs
        ));
    });
    let mut rate = Rate::default();
    loop {
        for (ti, t) in tables.iter().enumerate() {
            loop {
                if ctx.stopped() {
                    return Ok(());
                }
                let cols = t
                    .columns
                    .iter()
                    .map(|(n, _)| ident(n))
                    .collect::<Vec<_>>()
                    .join(", ");
                let wcol = t.columns.iter().position(|(n, _)| *n == wm).unwrap();
                let filter = st
                    .watermarks
                    .get(&t.index)
                    .map(|v| format!(" WHERE {} > {}", ident(&wm), literal(v)))
                    .unwrap_or_default();
                let q = admin.simple_query(&format!(
                    "SELECT {cols} FROM {}.{}{filter} ORDER BY {} LIMIT {POLL_PAGE}",
                    ident(&t.schema),
                    ident(&t.name),
                    ident(&wm)
                ))?;
                let n = q.rows.len();
                let mut ops = Vec::with_capacity(n);
                let mut high = None;
                for row in &q.rows {
                    if let Some(key) = key_of(&t.columns, &t.pk, row) {
                        ops.push(Op::Index {
                            index: t.index.clone(),
                            id: doc_id(&key),
                            doc: row_doc(&t.columns, row),
                        });
                    }
                    if let Some(v) = &row[wcol] {
                        high = Some(v.clone());
                    }
                }
                if !ops.is_empty() {
                    ctx.sink.apply(&ops)?;
                }
                if let Some(h) = high {
                    st.watermarks.insert(t.index.clone(), h);
                    ctx.store.save_state(&st)?;
                }
                let r = rate.add(n as u64);
                ctx.with(|s| {
                    s.tables[ti].rows_indexed += n as u64;
                    s.stream.rows_per_sec = r;
                    s.stream.inserts += n as u64;
                    s.tables[ti].snapshot_done = true;
                });
                if n < POLL_PAGE {
                    break;
                }
            }
        }
        let until = Instant::now() + Duration::from_secs(ctx.spec.poll_secs);
        while Instant::now() < until {
            if ctx.stopped() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
