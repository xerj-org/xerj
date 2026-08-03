# Snapshot and restore

XERJ implements a subset of the Elasticsearch snapshot API. A snapshot is a
recursive filesystem copy of one or more index directories into a repository
directory, plus a `manifest.json` at the snapshot root. A restore copies those
directories back and reopens the indices.

Restore is destructive. It deletes the live index directory before copying the
snapshot back. Read [Restore replaces index
directories](#restore-replaces-index-directories) before you run one.

Everything below was read out of the source. The handlers are in
`engine/crates/xerj-api/src/es_compat.rs`, the engine work is
`Engine::create_snapshot` and `Engine::restore_snapshot` in
`engine/crates/xerj-engine/src/engine.rs`, the routes are in
`engine/crates/xerj-api/src/router.rs`, and the authorization rules are in
`engine/crates/xerj-api/src/authz.rs`.

## Routes

| Method | Path | Handler |
|---|---|---|
| `PUT` | `/_snapshot/{repo}` | register or replace a repository |
| `GET` | `/_snapshot/{repo}` | read a repository config (`*` or `_all` returns all of them) |
| `DELETE` | `/_snapshot/{repo}` | deregister a repository |
| `PUT` | `/_snapshot/{repo}/{snapshot}` | take a snapshot |
| `GET` | `/_snapshot/{repo}/{snapshot}` | read one snapshot's info |
| `POST` | `/_snapshot/{repo}/{snapshot}/_restore` | restore |
| `GET` | `/_cat/repositories` | plain-text `name type` lines, one per repository, no header row |

Those are all the snapshot routes the router registers (`router.rs:485-497`,
`router.rs:763`). There is no `DELETE /_snapshot/{repo}/{snapshot}`, no
`_status`, no `_verify`, no `_cleanup`, and no snapshot lifecycle management.
See [What is not supported](#what-is-not-supported).

## Register a repository

The body you send is stored verbatim under the repository name
(`es_compat.rs:23138`). Only one field is ever read back out: the string at
`settings.location`, which is the filesystem path the snapshot directory is
created under (`es_compat.rs:23197-23201`).

The repository name must not be empty and must not contain `..`, `/`, `\`, or a
NUL byte. Violations return HTTP 400 with `"type":
"mapper_parsing_exception"` (`es_compat.rs:23086-23098`).

The location is bounded. `validate_snapshot_path` rejects any location that
contains a `..` path component, and requires the location to canonicalize to a
path inside `server.data_dir` or inside one of the base directories listed in
`limits.snapshot_repo_allowlist` in the TOML config
(`engine.rs:1971-1997`, `xerj-common/src/config.rs:960`). The allowlist defaults
to empty, which means only `data_dir` is permitted. This is checked when a
snapshot is created or restored, not when the repository is registered, so a
bad location is accepted by `PUT /_snapshot/{repo}` and only fails later.

If `settings.location` is absent the handler falls back to
`/tmp/xerj-snapshots` (`es_compat.rs:23201`). That fallback is outside the
default `data_dir` and will therefore be refused by the location check, so
always set `settings.location` explicitly.

```bash
curl -sS -X PUT localhost:9200/_snapshot/backups \
  -H "Authorization: ApiKey $XERJ_ADMIN_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"type":"fs","settings":{"location":"./data/snapshots"}}'
```

Response:

```json
{"acknowledged": true}
```

The repository registry is an in-memory map built empty at engine start
(`engine.rs:257`, `engine.rs:405`). Nothing reloads it from disk, so
repositories have to be registered again after a restart.

## Take a snapshot

```bash
curl -sS -X PUT localhost:9200/_snapshot/backups/2026-08-04 \
  -H "Authorization: ApiKey $XERJ_ADMIN_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"indices":"logs-*"}'
```

The body is optional. The only field read is `indices`
(`es_compat.rs:23210-23217`).

### Both spellings of `indices`

Since rc.10 the create handler parses `indices` as an array of names or
patterns, or as a single string that may itself be comma-separated:

```json
{"indices": ["logs-2026-08", "metrics-*"]}
{"indices": "logs-2026-08,metrics-*"}
```

Before rc.10 only the array spelling was read. The string spelling fell through
to "absent", which means "every index on the node", so `{"indices":"logs-*"}`
captured everything. Both spellings now parse the same way.

### Patterns resolve against what you can see

`Engine::create_snapshot` takes concrete index names, so the handler expands
patterns before calling it. It expands them over `Engine::index_name_list()`
(`es_compat.rs:23225-23249`), which filters through
`xerj_engine::index_guard::visible` (`engine.rs:1250-1256`). The middleware
installs the request's principal as that visibility rule
(`authz.rs:1272-1273`), so the set of indices captured is the set the caller is
allowed to see. Before rc.10 the wildcard reached the engine verbatim, matched
nothing, and produced an empty snapshot, so `{"indices":["*"]}` used to back up
nothing at all.

The matcher used on this path is `glob_match_simple`
(`es_compat.rs:19590-19601`). It handles exactly three shapes: `*` on its own,
a trailing wildcard (`logs-*`), and a leading wildcard (`*-2026`). Anything
else is compared as an exact string, so a mid-pattern wildcard such as
`logs-*-2026` matches nothing here. `_all` and `*` both expand to every visible
index.

### With no `indices` at all

If `indices` is absent, the engine snapshots every open index except its own
internal ones. "Internal" here means a name starting with `.xerj_`
(`is_system_index`, `engine.rs:1925-1927`). The reserved agent-memory and
second-brain namespace uses the prefix `.xerj-memory-` with a hyphen
(`xerj-common/src/types.rs:190`), which is not `.xerj_`, so an unnamed snapshot
does include every brain on the node. That is exactly why an unnamed snapshot
is superuser-only; see [Authorization](#authorization).

### What gets copied, and when

For each selected index the engine flushes the memtable and then copies the
index directory recursively: WAL files, segment files, and the schema, settings
and `es_mapping.json` files (`engine.rs:1669-1690`). The flush happens
immediately before that index's own copy, inside the per-index loop, so indices
are captured at different instants. There is no cross-index point-in-time
consistency, and no lock stops writes arriving during the copy.

An index named in `indices` that does not exist on the node is skipped
(`engine.rs:1670-1673`), but it is still listed in the manifest, because the
manifest records the requested list rather than the copied list
(`engine.rs:1657-1668`, `engine.rs:1692-1697`).

### Response

The operation is synchronous. There is no `wait_for_completion` on create. The
response is `{"accepted": true, "snapshot": <manifest>}` where the manifest is
the same JSON written to `manifest.json` (`es_compat.rs:23256-23260`,
`engine.rs:1692-1712`):

```json
{
  "accepted": true,
  "snapshot": {
    "snapshot": "2026-08-04",
    "uuid": "…",
    "version": "8.13.0",
    "indices": ["logs-2026-08"],
    "state": "SUCCESS",
    "start_time_in_millis": 1754265600123,
    "end_time_in_millis": 1754265601047,
    "duration_in_millis": 924,
    "failures": [],
    "shards": {"total": 1, "failed": 0, "successful": 1}
  }
}
```

The timestamps are real. `start_time_in_millis` is sampled before the flush and
copy work begins and `end_time_in_millis` after it finishes, so
`duration_in_millis` is the elapsed wall-clock time of the copy
(`engine.rs:1655`, `engine.rs:1692`, `engine.rs:1702`).

The `shards` counts are index counts, not shard counts: all three are derived
from the length of the index list (`engine.rs:1704-1707`). `version` is the
hardcoded string `"8.13.0"`, which is the ES protocol version XERJ reports, not
a XERJ version.

The snapshot name is validated the same way the repository name is, and must
also not be `.` or `..` (`es_compat.rs:23105-23123`).

## Inspect

```bash
curl -sS localhost:9200/_snapshot/backups/2026-08-04 \
  -H "Authorization: ApiKey $XERJ_ADMIN_KEY"
```

Returns `{"snapshots": [<manifest>]}`, or 404
`index_not_found_exception` if either the repository or the snapshot is unknown
(`es_compat.rs:23265-23283`).

This handler reads an in-memory map keyed by `"{repo}/{snapshot}"`, populated
when the snapshot was taken in this process (`es_compat.rs:23257-23258`). After
a restart it returns 404 even though the files are still on disk. Restore does
not use that map: it reads `manifest.json` from the repository directory, so
restoring a snapshot taken before a restart still works.

There is no `GET /_snapshot/{repo}/_all` and no wildcard listing of snapshots;
the lookup is an exact key match.

## Restore replaces index directories

```bash
curl -sS -X POST 'localhost:9200/_snapshot/backups/2026-08-04/_restore?wait_for_completion=true' \
  -H "Authorization: ApiKey $XERJ_ADMIN_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"indices":"logs-2026-08"}'
```

For every index it selects, the restore loop (`engine.rs:1857-1892`):

1. removes the index from the live index map,
2. calls `remove_dir_all` on `<data_dir>/<index>`,
3. recreates the directory and copies the snapshot's copy back,
4. reopens the index and reloads its persisted ES mapping.

Every document written to that index after the snapshot was taken is gone. So
is every mapping change. There is no merge mode, no dry run, and no way to
restore under a different name (see the rejected options below). If the reopen
in step 4 fails, the index is recorded in the engine's `failed_indices` map and
the loop continues to the next index (`engine.rs:1895-1898`), so the old data
is already deleted at that point.

The list of index names in the response is the set the filter selected. An
index whose snapshot directory was missing is skipped with a warning
(`engine.rs:1836-1840`) and one whose reopen failed is also skipped, but both
still appear in that list. Treat it as "what was attempted", not as proof of
success.

### Selecting indices to restore

`indices` accepts the same two spellings as create: an array, or a single
string that may be comma-separated (`es_compat.rs:23345-23356`). Each entry is
split on commas again in the engine, and each resulting pattern is matched
against the index list in the snapshot's own `manifest.json`
(`engine.rs:1765-1806`).

The matcher on the restore path is `glob_match` (`engine.rs:2066-2087`), a full
wildcard matcher supporting `*` and `?` anywhere in the pattern. This is a
different matcher from the one the create path uses, so a pattern that selects
nothing when taking a snapshot may still select something when restoring one.

- A pattern with no `*` that matches nothing in the snapshot is an error: HTTP
  400, `"type": "search_phase_execution_exception"`, reason
  `[{snapshot}] no index matches [{pattern}] in snapshot` (`engine.rs:1795-1802`).
  A wildcard that matches nothing is not an error.
- A wildcard never selects a `.xerj_*` internal index unless the pattern itself
  starts with `.` (`engine.rs:1786-1793`).
- With no `indices` at all, every index in the manifest except `.xerj_*` ones is
  restored (`engine.rs:1775-1782`).

Index names coming out of the manifest are revalidated with `IndexName::new`
before any filesystem operation, and the destination directory is checked to be
inside `data_dir` both lexically and after canonicalization
(`engine.rs:1822-1834`, `engine.rs:1844-1855`,
`engine.rs:1866-1877`).

### Restore options that are rejected

These five options are refused with HTTP 400, `"type":
"illegal_argument_exception"`, reason `restore option [{name}] is not supported
by this XERJ version` (`es_compat.rs:23316-23343`):

`rename_pattern`, `rename_replacement`, `feature_states`, `index_settings`,
`ignore_index_settings`

They fail loud rather than being ignored, because ignoring a rename would
overwrite the source index instead of creating a copy.

Any other field in the restore body is ignored. The handler reads only
`indices` and the five names above, so `include_global_state`,
`include_aliases`, `partial` and friends have no effect and produce no error.

### `wait_for_completion`

`wait_for_completion` is a query-string parameter and is compared to the exact
string `"true"` (`es_compat.rs:23358-23361`). `wait_for_completion=1` and
`wait_for_completion=TRUE` both read as false.

The restore runs synchronously either way. The flag only picks the response
shape (`es_compat.rs:23368-23386`):

```json
{"accepted": true}
```

or, with `wait_for_completion=true`:

```json
{
  "snapshot": {
    "snapshot": "2026-08-04",
    "indices": ["logs-2026-08"],
    "shards": {"total": 1, "failed": 0, "successful": 1}
  }
}
```

Again, those counts are index counts.

## Authorization

Snapshot and restore are the only two routes where an index pattern in a
request body is decided by the authorization middleware itself rather than left
to the engine's visibility guard (`authz.rs:1143-1152`). The reason is in the
code: `create_snapshot` walks the index map and `restore_snapshot` expands
against the snapshot manifest and then removes and rewrites index directories,
so neither passes through `get_index` or `delete_index`, which is where every
other body-named target meets the guard.

The rules, as `decide` and `authorize_expression` apply them:

- **Superuser skips all of it** (`authz.rs:1291-1293`). A principal is
  superuser when it presents the configured admin key, and also when
  `auth.enabled` is false or `auth.admin_api_key` is empty, which is the
  point-at-a-folder local posture (`auth.rs:214-222`). A superuser can back up
  and restore everything.

- **Naming no indices at all is superuser-only.** For any non-GET, non-HEAD
  request under `/_snapshot/` with three or more path segments, an empty set of
  demanded indices is refused (`authz.rs:1333-1358`). That covers both `PUT
  /_snapshot/{repo}/{snap}` with no body and `POST
  /_snapshot/{repo}/{snap}/_restore` with no `indices`, because both cover
  every index on the node, brains included. The 403 names the resource as
  `<all indices>` and the action as `create_snapshot` or `restore_snapshot`.

- **A pattern that may reach the reserved namespace needs an explicit grant.**
  `may_reach_reserved` (`authz.rs:177-191`) judges a pattern by the literal
  text before its first `*`: the expression qualifies if that prefix starts
  with `.xerj-memory-` or is itself a prefix of `.xerj-memory-`. So `*`, `_all`,
  `.*`, `.xerj-*` and `.xerj-memory-alice*` all qualify. For those, only a
  superuser, or a scoped key whose `role_descriptors` grant that very pattern
  with the required privilege, is allowed through (`authz.rs:1175-1193`). An
  unscoped key is never allowed a reserved-reaching pattern on these two
  routes.

- **Narrower patterns work normally.** `logs-*` does not qualify under
  `may_reach_reserved`, so it is not refused and is not subject to the extra
  check. The ordinary per-tenant backup keeps working.

- **Concrete names go through the normal per-index check**, resolving aliases
  first (`authz.rs:1194-1202`).

- **Restore demands write, not read.** `body_targets` records snapshot
  `indices` as `ReadIndex`, and `decide` upgrades every demand to `WriteIndex`
  when the last path segment is `_restore` (`authz.rs:895-905`,
  `authz.rs:1342-1347`), because a restore overwrites what it names.

A refusal is an ES-shaped 403 naming the resource and the action, with the
grant that would fix it (`authz.rs:242-262`).

Note that "unscoped" is a real category here: a key with no usable
`role_descriptors` keeps its historical reach over ordinary indices but has no
privilege at all on `.xerj-memory-*` (`auth.rs:99-106`). And an explicit grant
is not second-guessed: a scoped key minted with `names: ["*"]` does satisfy the
check above, because `*` matches every index and that is what the operator
asked for. Only a superuser can mint such a key. To keep brains isolated, grant
concrete names or a prefix that excludes `.xerj-memory-`.

## The native backup route

`POST /v1/admin/backup` calls the same `Engine::create_snapshot`
(`native.rs:779-815`). Its body has three optional fields: `repo_path`, `name`
and `indices` (`native.rs:770-777`). `repo_path` defaults to
`<data_dir>/_backups` and `name` defaults to `backup-<uuid>`. It returns HTTP
201 with the manifest under a `manifest` key.

Two differences from the ES route are worth knowing:

- It passes `indices` to the engine unchanged, with no pattern expansion. The
  engine looks index names up exactly, so a wildcard here matches nothing.
- The authorization middleware classifies `/v1/...` as a cluster route, not as
  a snapshot route (`authz.rs:473-480`, `authz.rs:675-677`), so the
  snapshot-specific pattern rule above does not apply to it. It falls through
  to the general cluster-mutation rule: a scoped key is refused, an unscoped
  key is allowed (`authz.rs:1386-1396`).

The repository location check still applies, since it lives inside
`Engine::create_snapshot`.

## What is not supported

Confirmed absent from the code, not merely undocumented:

- **Deleting a snapshot.** The route table registers only `PUT` and `GET` on
  `/_snapshot/{repo}/{snapshot}` (`router.rs:491-493`). Removing a snapshot
  means deleting its directory yourself.
- **`_status`, `_verify`, `_cleanup`, `_slm`.** No routes exist for any of
  them.
- **`GET /_snapshot`** with no repository. The route is `/_snapshot/:repo`; use
  `GET /_snapshot/_all` or `GET /_snapshot/*` to list repositories.
- **Listing snapshots in a repository.** `GET /_snapshot/{repo}/{snapshot}` is
  an exact key lookup; `_all` and wildcards are not handled.
- **Anything but a local filesystem repository.** The `type` field you send is
  stored and echoed by `_cat/repositories` but is never acted on. There is no
  S3, GCS, Azure or HDFS implementation; the engine always does a local
  recursive directory copy.
- **Incremental or deduplicated snapshots.** Every snapshot copies the full
  index directory. There is no shared blob store and no reference counting
  between snapshots.
- **Global cluster state.** Only per-index directories and the manifest are
  written. Index templates, ILM policies, cluster settings, enrich policies,
  transform pipelines and security keys are not captured, and are not restored.
- **Restore rename, per-index settings overrides, and feature states.**
  Rejected with 400, as listed above.
- **`ignore_unavailable`, `include_global_state`, `partial`,
  `include_aliases`.** Not parsed on either verb.
- **Persistent repository and snapshot metadata.** Both maps are in-memory and
  are lost on restart.
- **Cluster-wide coordination.** Both operations act on the local node's
  `data_dir`.
- **Compression or encryption of the copy.** Files are copied as they are on
  disk.
- **Point-in-time consistency across indices,** and any guard against
  concurrent writes or a concurrent restore. See [What gets copied, and
  when](#what-gets-copied-and-when).

## Related

- [`docs/ARCHITECTURE.md`](./ARCHITECTURE.md) for where the engine, storage and
  API layers sit.
- `CHANGELOG.md` for the rc.10 entry describing the snapshot authorization fix
  and the `indices` parsing fix.
