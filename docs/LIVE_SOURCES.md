# Live database sources

Connect a Postgres database once. XERJ takes a consistent snapshot of the
tables you pick, then applies every insert, update, delete and truncate within
seconds through Postgres logical replication. The source lives inside the
node: it survives restarts and resumes where it stopped.

MySQL is not in this build yet; until it is, dump the database and run
`xerj autoindex` on the dump.

## Connect

```sh
export PG_SRC_PW='...'          # in the environment of the XERJ NODE, not your shell
xerj connect postgres://app@db.internal:5432/shop --password-env PG_SRC_PW
```

Verify:

```sh
xerj sources shop
# expect: shop  streaming  logical  0.0 ...
```

The same over HTTP (superuser key on a secured node):

```sh
curl -X PUT localhost:9200/_xerj/sources/shop -H 'Content-Type: application/json' \
  -d '{"url":"postgres://app@db.internal:5432/shop","password_env":"PG_SRC_PW"}'
curl localhost:9200/_xerj/sources/shop
# expect: "state":"streaming"
```

Agents use the MCP tool `xerj_sources` (`action`: list, status, connect, delete).

Each table becomes one index, `src-<name>-<schema>-<table>` (change the prefix
with `--index-prefix`). The document id is the primary key, so every change
lands on the same document. Tables without a primary key are listed under
`skipped` in the status and not followed.

## The password

Never in the URL (it is refused) and never stored. The source keeps a
*reference*: `--password-env VAR` (a variable in the node's environment) or
`--password-file PATH` (a file the node can read). The node reads it each time
it connects.

## What the database needs

| Setting | Why |
|---|---|
| `wal_level = logical` (restart Postgres after changing it) | logical replication |
| the role has `REPLICATION` (`ALTER ROLE app REPLICATION`) | to create and read the replication slot |
| `SELECT` on the tables | the snapshot |
| table ownership, or a publication made for XERJ | XERJ creates `xerj_<name>` itself when it can; otherwise a DBA runs `CREATE PUBLICATION xerj_shop FOR TABLE ...;` and you pass `--publication xerj_shop` |
| a free slot (`max_replication_slots`) | one per source |

Least privilege: a role with `LOGIN REPLICATION`, `SELECT` on the chosen tables,
and a DBA-made publication. No superuser.

Without replication rights, `--mode poll --watermark-column updated_at` re-reads
rows whose `updated_at` grew, every `--poll-secs`. Polling sees inserts and
updates, never deletes, and the status says so.

## The replication slot keeps WAL

A slot makes Postgres keep WAL until XERJ acknowledges it. XERJ acknowledges as
it applies, so a healthy source holds seconds of WAL. A source that is stopped,
erroring, or deleted from XERJ's disk without `xerj sources --delete` would make
the database keep WAL **forever** and eventually fill its disk. Remove a source
with:

```sh
xerj sources shop --delete            # drops the slot and the publication XERJ made
xerj sources shop --delete --delete-indices
```

If a node is gone for good, drop the slot on the database:
`SELECT pg_drop_replication_slot('xerj_shop');`

## Status

`GET /_xerj/sources/{name}` and `xerj sources <name>` report:

- `state`: `connecting`, `snapshotting` (with `snapshot.percent`, `eta_secs`,
  `rows_per_sec`), `streaming`, `lagging` (more than 30 s behind), `polling`,
  `error` (with `error.message` and `error.hint`, the fix), `stopped`.
- `stream`: `confirmed_lsn` (applied and acknowledged), `server_lsn`, `lag_secs`,
  `rows_per_sec`, insert/update/delete/truncate counts, and the node's own
  commit-to-applied latency p50/p95.
- `tables`, `skipped`, and `notes` (schema changes and anything else worth
  reading once).

An error is retried with backoff; the source never gives up on its own.

## Guarantees

- **Consistent start.** The slot is created with an exported snapshot and the
  snapshot is read in a `REPEATABLE READ` transaction pinned to it; streaming
  starts at the slot's consistent point. Every change is in the snapshot or in
  the stream, never both, never neither.
- **No loss, no duplicates across restarts.** The node acknowledges an LSN only
  after the changes up to it are applied. After a crash it resumes from the
  last acknowledged LSN; anything re-sent rewrites the same documents (ids are
  primary keys).
- **Schema changes.** A new column becomes a new field from its first change
  on; a dropped column stays on older documents until their row changes; a
  type change is noted in the status. `TRUNCATE` clears the index.
- **Large unchanged values.** An update that leaves a large (TOASTed) column
  untouched merges into the stored document instead of dropping the column.

## Rows to facts (XERJ Query-1)

`--facts-namespace <ns>` marks a source for the rows-to-facts bridge: every
applied row change is offered to a facts observer with its primary key (a
stable `subject_id`), the new values, and the commit LSN. The mapping from a
row to facts is a separate change; until it lands, the option is stored and
reported but no facts are written.
